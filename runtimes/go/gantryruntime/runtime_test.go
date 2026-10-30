// An internal test package (no self-import of the module path) so this file
// stays compilable when the compile-verification gate copies every .go file
// in this directory into a fixture under a different module path (see
// crates/gantry-backend-go/tests/compile_output.rs).
package gantryruntime

import (
	"bytes"
	"context"
	"crypto/tls"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"strings"
	"testing"
	"time"
)

type fixedToken string

func (t fixedToken) AccessToken(context.Context) (string, error) { return string(t), nil }

func TestDefaultHeaderIsSentAndOverridable(t *testing.T) {
	var got []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		got = append(got, r.Header.Get("X-Trace-Id"))
	}))
	defer server.Close()

	client := New(fixedToken("t"), WithDefaultHeader("X-Trace-Id", "default"))
	if _, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL)); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	overriding := WithHeader(client.NewRequest("GET", server.URL), "X-Trace-Id", "override")
	if _, err := client.Fetch(context.Background(), overriding); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	if want := []string{"default", "override"}; len(got) != 2 || got[0] != want[0] || got[1] != want[1] {
		t.Fatalf("got headers %v, want %v", got, want)
	}
}

func TestFetchSurfacesA401InsteadOfSwallowingItWhenRetriesAreDisabled(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer server.Close()

	client := New(fixedToken("t"), WithMaxRetries(0))
	_, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL))
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.Status != http.StatusUnauthorized {
		t.Fatalf("err = %v, want an *APIError with status %d", err, http.StatusUnauthorized)
	}
}

// A non-2xx response is an error carrying Box's error fields, never a
// "successful" response whose error body a caller would decode as a result.
func TestFetchReturnsAnAPIErrorForANon2xxResponse(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusNotFound)
		io.WriteString(w, `{"type":"error","status":404,"code":"not_found","message":"Not Found","request_id":"abc123"}`)
	}))
	defer server.Close()

	client := New(fixedToken("t"))
	resp, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL))
	if resp != nil {
		t.Fatalf("resp = %v, want nil alongside the error", resp)
	}
	var apiErr *APIError
	if !errors.As(err, &apiErr) {
		t.Fatalf("err = %v, want an *APIError", err)
	}
	if apiErr.Status != 404 || apiErr.Code != "not_found" || apiErr.Message != "Not Found" || apiErr.RequestID != "abc123" {
		t.Fatalf("APIError = %+v", apiErr)
	}
	if want := "box: status 404 not_found: Not Found (request_id abc123)"; err.Error() != want {
		t.Fatalf("Error() = %q, want %q", err.Error(), want)
	}
}

// The token endpoint answers with an OAuth 2.0 error body; its error and
// error_description fill Code and Message.
func TestFetchReadsAnOAuthErrorIntoTheAPIError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusBadRequest)
		io.WriteString(w, `{"error":"invalid_grant","error_description":"Grant credentials are invalid"}`)
	}))
	defer server.Close()

	client := New(fixedToken("t"))
	_, err := client.Fetch(context.Background(), client.NewRequest("POST", server.URL))
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.Code != "invalid_grant" || apiErr.Message != "Grant credentials are invalid" {
		t.Fatalf("err = %v (%+v), want invalid_grant with its description", err, apiErr)
	}
}

// A non-JSON error body still surfaces as an APIError, with the raw body.
func TestFetchReturnsAnAPIErrorForANonJSONErrorBody(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadGateway)
		io.WriteString(w, "upstream down")
	}))
	defer server.Close()

	client := New(fixedToken("t"), WithMaxRetries(0))
	_, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL))
	var apiErr *APIError
	if !errors.As(err, &apiErr) || apiErr.Status != 502 || string(apiErr.Body) != "upstream down" || apiErr.Code != "" {
		t.Fatalf("err = %v (%+v), want a 502 APIError carrying the raw body", err, apiErr)
	}
}

func TestWithMultipartBodySendsBareJSONAndTheFileBytes(t *testing.T) {
	var hadJSONPart bool
	var gotAttributes, gotFileName string
	var gotFileBytes []byte
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := r.ParseMultipartForm(1 << 20); err != nil {
			t.Fatalf("parse multipart form: %v", err)
		}
		if v, ok := r.MultipartForm.Value["attributes"]; ok {
			hadJSONPart = true
			gotAttributes = v[0]
		}
		file, header, err := r.FormFile("file")
		if err != nil {
			t.Fatalf("form file: %v", err)
		}
		defer file.Close()
		gotFileName = header.Filename
		gotFileBytes, _ = io.ReadAll(file)
	}))
	defer server.Close()

	client := New(fixedToken("t"))
	req := WithMultipartBody(
		client.NewRequest("POST", server.URL),
		"attributes", []byte(`{"name":"f.txt"}`),
		"file", strings.NewReader("file bytes"),
	)
	if _, err := client.Fetch(context.Background(), req); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	if !hadJSONPart {
		t.Fatal("expected an attributes JSON part")
	}
	// The bare attributes object, not wrapped in another JSON layer.
	if gotAttributes != `{"name":"f.txt"}` {
		t.Fatalf("attributes part = %q, want the bare JSON object", gotAttributes)
	}
	if gotFileName != "file" {
		t.Fatalf("file part name = %q, want %q", gotFileName, "file")
	}
	if string(gotFileBytes) != "file bytes" {
		t.Fatalf("file bytes = %q, want %q", gotFileBytes, "file bytes")
	}
}

// The avatar-upload shape has no attributes field at all (G-7); the bug this
// guards against sent a bogus empty attributes part regardless.
func TestWithMultipartBodyOmitsAnAbsentJSONPart(t *testing.T) {
	var hadJSONPart bool
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := r.ParseMultipartForm(1 << 20); err != nil {
			t.Fatalf("parse multipart form: %v", err)
		}
		_, hadJSONPart = r.MultipartForm.Value["attributes"]
	}))
	defer server.Close()

	client := New(fixedToken("t"))
	req := WithMultipartBody(
		client.NewRequest("POST", server.URL),
		"", nil,
		"pic", strings.NewReader("avatar bytes"),
	)
	if _, err := client.Fetch(context.Background(), req); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	if hadJSONPart {
		t.Fatal("expected no JSON part for a body with no JSON field")
	}
}

// WithHTTPVersion picks the protocol actually negotiated with a TLS server that
// offers HTTP/2: HTTP2 (the default) gets it, HTTP1 stays on HTTP/1.1.
func TestWithHTTPVersionSelectsTheNegotiatedProtocol(t *testing.T) {
	server := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-Proto-Major", strconv.Itoa(r.ProtoMajor))
	}))
	server.EnableHTTP2 = true
	server.StartTLS()
	defer server.Close()
	trusted := server.Client().Transport.(*http.Transport).TLSClientConfig

	for _, tc := range []struct {
		name    string
		version HTTPVersion
		want    string
	}{{"HTTP2", HTTP2, "2"}, {"HTTP1", HTTP1, "1"}} {
		t.Run(tc.name, func(t *testing.T) {
			client := New(fixedToken("t"), WithHTTPVersion(tc.version), WithMaxRetries(0))
			// Trust the test server's certificate without touching how the
			// option configured the protocol.
			transport, ok := client.http.Transport.(*http.Transport)
			if !ok {
				transport = http.DefaultTransport.(*http.Transport).Clone()
				client.http.Transport = transport
			}
			transport.TLSClientConfig = &tls.Config{RootCAs: trusted.RootCAs}
			response, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL))
			if err != nil {
				t.Fatalf("fetch: %v", err)
			}
			if got := response.header.Get("X-Proto-Major"); got != tc.want {
				t.Fatalf("negotiated HTTP/%s, want HTTP/%s", got, tc.want)
			}
		})
	}
}

// WithProxy sends API requests through the given proxy, alone or alongside
// WithHTTPVersion; a later WithHTTPClient replaces the built-in client.
func TestWithProxyRoutesRequestsThroughTheProxy(t *testing.T) {
	var proxied []string
	proxy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// A forward proxy sees the absolute target URL, not just its path.
		proxied = append(proxied, r.URL.String())
	}))
	defer proxy.Close()
	proxyURL, _ := url.Parse(proxy.URL)

	for _, opts := range [][]Option{
		{WithProxy(proxyURL)},
		{WithHTTPVersion(HTTP1), WithProxy(proxyURL)},
		{WithProxy(proxyURL), WithHTTPVersion(HTTP1)},
	} {
		client := New(fixedToken("t"), append(opts, WithMaxRetries(0))...)
		if _, err := client.Fetch(context.Background(), client.NewRequest("GET", "http://box.invalid/2.0/users/me")); err != nil {
			t.Fatalf("fetch through the proxy: %v", err)
		}
	}
	if len(proxied) != 3 || proxied[0] != "http://box.invalid/2.0/users/me" {
		t.Fatalf("proxy saw %q, want the target URL three times", proxied)
	}

	injected := &http.Client{}
	if client := New(fixedToken("t"), WithProxy(proxyURL), WithHTTPClient(injected)); client.http != injected {
		t.Fatal("WithHTTPClient after WithProxy should win")
	}
}

// WithAsUserHeader derives a client that acts as another user on every
// request, including the retry after a 401 refresh, and leaves the original
// client and its other default headers as they were.
func TestWithAsUserHeaderDerivesAClientActingAsTheUser(t *testing.T) {
	var seen []string
	unauthorizedOnce := true
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = append(seen, r.Header.Get("As-User")+"|"+r.Header.Get("X-Trace-Id"))
		if r.URL.Path == "/refresh" && unauthorizedOnce {
			unauthorizedOnce = false
			w.WriteHeader(http.StatusUnauthorized)
		}
	}))
	defer server.Close()

	client := New(fixedToken("t"), WithDefaultHeader("X-Trace-Id", "trace"))
	user := client.WithAsUserHeader("12345")
	for _, c := range []*Client{user, client} {
		if _, err := c.Fetch(context.Background(), c.NewRequest("GET", server.URL)); err != nil {
			t.Fatalf("fetch: %v", err)
		}
	}
	if _, err := user.Fetch(context.Background(), user.NewRequest("GET", server.URL+"/refresh")); err != nil {
		t.Fatalf("fetch after a 401: %v", err)
	}

	want := []string{"12345|trace", "|trace", "12345|trace", "12345|trace"}
	if strings.Join(seen, ",") != strings.Join(want, ",") {
		t.Fatalf("As-User|X-Trace-Id seen %v, want %v", seen, want)
	}
}

// flakyServer answers a 429 (Retry-After: 0), then a 401, then 200, and
// records the Authorization header of each request it receives.
func flakyServer(t *testing.T) (*httptest.Server, *[]string) {
	t.Helper()
	var auths []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		auths = append(auths, r.Header.Get("Authorization"))
		switch len(auths) {
		case 1:
			w.Header().Set("Retry-After", "0")
			w.WriteHeader(http.StatusTooManyRequests)
		case 2:
			w.WriteHeader(http.StatusUnauthorized)
		default:
			io.WriteString(w, `{"id":"1"}`)
		}
	}))
	t.Cleanup(server.Close)
	return server, &auths
}

// The request and response hooks see every attempt — the retried 429 and the
// resend after a 401 refresh included — with the token withheld from them.
func TestHooksObserveEveryAttempt(t *testing.T) {
	server, auths := flakyServer(t)
	var requests []RequestInfo
	var responses []ResponseInfo
	client := New(fixedToken("secret-token"),
		WithRequestHook(func(_ context.Context, r RequestInfo) { requests = append(requests, r) }),
		WithResponseHook(func(_ context.Context, r ResponseInfo) { responses = append(responses, r) }),
	)
	request := WithHeader(client.NewRequest("GET", server.URL+"/files/1"), "X-Trace-Id", "abc")
	if _, err := client.Fetch(context.Background(), WithQuery(request, "fields", "id")); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	if len(*auths) != 3 || (*auths)[2] != "Bearer secret-token" {
		t.Fatalf("server saw %q, want three bearer-authenticated attempts", *auths)
	}
	if len(requests) != 3 || len(responses) != 3 {
		t.Fatalf("hooks saw %d requests and %d responses, want 3 of each", len(requests), len(responses))
	}
	for i, want := range []int{429, 401, 200} {
		req, resp := requests[i], responses[i]
		if req.Attempt != i+1 || resp.Request.Attempt != i+1 || resp.Status != want || resp.Err != nil {
			t.Fatalf("attempt %d: request %+v, response status %d err %v", i+1, req, resp.Status, resp.Err)
		}
		if req.Method != "GET" || req.URL != server.URL+"/files/1?fields=id" || req.Header.Get("X-Trace-Id") != "abc" {
			t.Fatalf("attempt %d: request %+v", i+1, req)
		}
		if got := req.Header.Get("Authorization"); got != "REDACTED" {
			t.Fatalf("attempt %d: hook saw Authorization %q, want it redacted", i+1, got)
		}
		if resp.Elapsed <= 0 {
			t.Fatalf("attempt %d: elapsed %v", i+1, resp.Elapsed)
		}
	}
	if responses[0].Header.Get("Retry-After") != "0" {
		t.Fatalf("429 response headers %v", responses[0].Header)
	}
}

// The debug log has a line per attempt and never the token, a secret query
// value, or a body.
func TestDebugLoggingRedactsSecrets(t *testing.T) {
	server, _ := flakyServer(t)
	var out bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&out, &slog.HandlerOptions{Level: slog.LevelDebug}))
	client := New(fixedToken("secret-token"), WithDebugLogging(logger))
	request := client.NewRequest("POST", server.URL+"/files/1")
	request = WithQuery(WithQuery(request, "fields", "id"), "access_token", "query-token")
	request = WithJsonBody(WithQuery(request, "client_secret", "query-secret"), []byte(`{"password":"body-secret"}`))
	if _, err := client.Fetch(context.Background(), request); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	logged := out.String()
	for _, secret := range []string{"secret-token", "query-token", "query-secret", "body-secret"} {
		if strings.Contains(logged, secret) {
			t.Fatalf("debug log leaked %q:\n%s", secret, logged)
		}
	}
	lines := strings.Split(strings.TrimSpace(logged), "\n")
	if len(lines) != 3 {
		t.Fatalf("want a line per attempt, got:\n%s", logged)
	}
	for i, status := range []string{"429", "401", "200"} {
		for _, want := range []string{"method=POST", "access_token=REDACTED", "client_secret=REDACTED", "fields=id", "status=" + status, "attempt=" + strconv.Itoa(i+1)} {
			if !strings.Contains(lines[i], want) {
				t.Fatalf("line %d lacks %q: %s", i+1, want, lines[i])
			}
		}
	}

	// A transport error's message carries the URL; the log redacts it there too.
	server.Close()
	failing := WithQuery(client.NewRequest("GET", server.URL), "access_token", "query-token")
	if _, err := New(fixedToken("t"), WithMaxRetries(0), WithDebugLogging(logger)).Fetch(context.Background(), failing); err == nil {
		t.Fatal("fetch against a closed server should fail")
	}
	if logged := out.String(); strings.Contains(logged, "query-token") || !strings.Contains(logged, "error=") {
		t.Fatalf("transport error line:\n%s", logged)
	}
}

// The backoff ceiling doubles from the base up to the max; full jitter keeps
// each wait at or under it, and over many samples close to it.
func TestBackoffFollowsTheConfiguredBaseAndMax(t *testing.T) {
	ms := time.Millisecond
	for _, tc := range []struct {
		opts    []Option
		attempt int
		ceiling time.Duration
	}{
		// The defaults, unchanged: 500ms doubling to a 30s cap.
		{nil, 0, 500 * ms},
		{nil, 1, time.Second},
		{nil, 5, 16 * time.Second},
		{nil, 6, 30 * time.Second},
		{nil, 100, 30 * time.Second},
		{[]Option{WithRetryBackoff(10*ms, 35*ms)}, 0, 10 * ms},
		{[]Option{WithRetryBackoff(10*ms, 35*ms)}, 1, 20 * ms},
		{[]Option{WithRetryBackoff(10*ms, 35*ms)}, 2, 35 * ms},
		{[]Option{WithRetryBackoff(-ms, 0)}, 3, 0},
	} {
		client := New(fixedToken("t"), tc.opts...)
		var largest time.Duration
		for range 200 {
			wait := client.backoff(tc.attempt)
			if wait < 0 || wait > tc.ceiling {
				t.Fatalf("backoff(%d) = %v, want within [0, %v]", tc.attempt, wait, tc.ceiling)
			}
			largest = max(largest, wait)
		}
		if largest < tc.ceiling/2 {
			t.Fatalf("backoff(%d) never passed %v in 200 samples (largest %v)", tc.attempt, tc.ceiling/2, largest)
		}
	}
}

// A small backoff shortens the waits between retried 503s.
func TestWithRetryBackoffShortensTheWaits(t *testing.T) {
	hits := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if hits++; hits <= 4 {
			w.WriteHeader(http.StatusServiceUnavailable)
		}
	}))
	defer server.Close()

	client := New(fixedToken("t"), WithRetryBackoff(time.Millisecond, 4*time.Millisecond))
	started := time.Now()
	if _, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL)); err != nil {
		t.Fatalf("fetch: %v", err)
	}
	// Four waits of at most 1, 2, 4, and 4ms, where the defaults allow 7.5s.
	if elapsed := time.Since(started); hits != 5 || elapsed > 2*time.Second {
		t.Fatalf("%d attempts in %v, want 5 well under 2s", hits, elapsed)
	}
}

func TestRedactURL(t *testing.T) {
	for raw, want := range map[string]string{
		"https://api.box.com/2.0/files/1":                                  "https://api.box.com/2.0/files/1",
		"https://api.box.com/2.0/files?fields=id&limit=5":                  "https://api.box.com/2.0/files?fields=id&limit=5",
		"https://a.box.com/x?Access_Token=t&code=c&refresh%5Ftoken=r#frag": "https://a.box.com/x?Access_Token=REDACTED&code=REDACTED&refresh%5Ftoken=REDACTED",
		"https://user:pass@box.example/x?api_key=k&q=a@b":                  "https://box.example/x?api_key=REDACTED&q=a@b",
		"https://box.example/x?password":                                   "https://box.example/x?password=REDACTED",
	} {
		if got := redactURL(raw); got != want {
			t.Errorf("redactURL(%q) = %q, want %q", raw, got, want)
		}
	}
}

// An attempt that fails in transport still reaches the response hook, with
// the error and no status.
func TestResponseHookSeesATransportError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	server.Close()
	var responses []ResponseInfo
	client := New(fixedToken("t"), WithMaxRetries(0),
		WithResponseHook(func(_ context.Context, r ResponseInfo) { responses = append(responses, r) }))
	if _, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL)); err == nil {
		t.Fatal("fetch against a closed server should fail")
	}
	if len(responses) != 1 || responses[0].Err == nil || responses[0].Status != 0 || responses[0].Request.Attempt != 1 {
		t.Fatalf("responses %+v, want one transport error", responses)
	}
}
