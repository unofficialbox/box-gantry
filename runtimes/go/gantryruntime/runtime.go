// Package gantryruntime is the hand-written runtime the generated Box SDK
// ships against (TR-Go.7). It implements the machine-readable runtime
// contract (gantry-contract V1): generated code calls only these
// declarations, and this package supplies the behavior — a retrying
// network layer (backoff + jitter, 401 refresh, Retry-After), auth token
// threading, request builders, and response accessors.
//
// It is the real implementation the compilable stubs stand in for during
// generation-time verification (FR-5.3). Because it satisfies the same
// signatures, the generated SDK compiles against it unchanged.
package gantryruntime

import (
	"bytes"
	"context"
	"crypto/tls"
	"errors"
	"io"
	"log/slog"
	"math/rand"
	"mime/multipart"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

// TokenSource yields an access token for the configured auth flow.
type TokenSource interface {
	AccessToken(ctx context.Context) (string, error)
}

// Request is the runtime-owned HTTP request envelope, assembled by the
// With* builders before Fetch executes it.
type Request struct {
	method  string
	rawURL  string
	header  http.Header
	query   url.Values
	getBody func() (io.Reader, error)
	timeout time.Duration
}

// Response is the runtime-owned HTTP response envelope. The body is read
// fully so it can be replayed as bytes or a stream and so retries are safe.
type Response struct {
	status int
	header http.Header
	body   []byte
}

// Client is the runtime session: it holds the auth source, HTTP client,
// base-URL configuration, and retry policy shared by every manager.
type Client struct {
	ts             TokenSource
	http           *http.Client
	baseURLs       map[string]string
	maxRetries     int
	retryBase      time.Duration
	retryMax       time.Duration
	defaultHeaders http.Header
	requestHooks   []func(context.Context, RequestInfo)
	responseHooks  []func(context.Context, ResponseInfo)

	// The built-in API client's settings (WithHTTPVersion, WithProxy); New
	// builds it from them unless WithHTTPClient injected one.
	httpVersion HTTPVersion
	proxy       *url.URL
	injected    bool
}

// Option configures a Client at construction (the With* surface, G-3).
type Option func(*Client)

// WithHTTPClient overrides the HTTP client (timeouts, transport, proxies).
func WithHTTPClient(h *http.Client) Option {
	return func(c *Client) { c.http, c.injected = h, true }
}

// HTTPVersion selects the HTTP protocol the built-in client uses for API
// requests (see WithHTTPVersion). The standard library has no HTTP/3.
type HTTPVersion int

const (
	// HTTP2 negotiates HTTP/2 over TLS when the server offers it, else
	// HTTP/1.1. The default.
	HTTP2 HTTPVersion = iota
	// HTTP1 uses HTTP/1.1 only.
	HTTP1
)

// WithHTTPVersion chooses the HTTP protocol for API requests. It configures
// the built-in HTTP client, so it and WithHTTPClient are alternatives:
// whichever option comes last wins. To pick the protocol of an injected
// client, configure its Transport directly. Token requests keep their own
// client.
func WithHTTPVersion(v HTTPVersion) Option {
	return func(c *Client) { c.httpVersion, c.injected = v, false }
}

// WithProxy sends API requests through the proxy at u (http://, https://, or
// socks5://, with any credentials in its userinfo) instead of the one the
// HTTPS_PROXY / NO_PROXY environment selects, which is the default. Like
// WithHTTPVersion it configures the built-in HTTP client, and the two
// combine; WithHTTPClient replaces both. Token requests take their proxy from
// the auth config's Proxy field.
func WithProxy(u *url.URL) Option {
	return func(c *Client) { c.proxy, c.injected = u, false }
}

// apiHTTPClient is the built-in API client: a 60s timeout, the requested
// protocol, and the proxy, if one is set. HTTP2 with no proxy keeps the
// shared DefaultTransport, which already negotiates HTTP/2 and reads the
// proxy from the environment.
func apiHTTPClient(v HTTPVersion, proxy *url.URL) *http.Client {
	client := &http.Client{Timeout: 60 * time.Second}
	if v == HTTP2 && proxy == nil {
		return client
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	if v == HTTP1 {
		// A non-nil, empty TLSNextProto disables HTTP/2 (net/http docs).
		transport.ForceAttemptHTTP2 = false
		transport.TLSNextProto = map[string]func(string, *tls.Conn) http.RoundTripper{}
	}
	if proxy != nil {
		transport.Proxy = http.ProxyURL(proxy)
	}
	client.Transport = transport
	return client
}

// WithMaxRetries sets how many times a retriable failure is retried.
func WithMaxRetries(n int) Option { return func(c *Client) { c.maxRetries = n } }

// The default exponential backoff: the wait before retry n is uniform in
// [0, min(base·2ⁿ, max)].
const (
	DefaultRetryBaseDelay = 500 * time.Millisecond
	DefaultRetryMaxDelay  = 30 * time.Second
)

// WithRetryBackoff sets the exponential backoff between retries: the wait
// before retry n is uniform (full jitter) in [0, min(base·2ⁿ, max)], from
// DefaultRetryBaseDelay and DefaultRetryMaxDelay. A negative value counts as
// zero. A Retry-After from Box still sets the wait for that retry.
func WithRetryBackoff(base, max time.Duration) Option {
	return func(c *Client) { c.retryBase, c.retryMax = clampNonNegative(base), clampNonNegative(max) }
}

func clampNonNegative(d time.Duration) time.Duration {
	if d < 0 {
		return 0
	}
	return d
}

// RequestInfo describes one HTTP attempt as Fetch is about to send it. The
// Header is a copy whose Authorization value reads "REDACTED", so a hook that
// records every header never captures the access token.
type RequestInfo struct {
	Method string
	URL    string
	Header http.Header
	// Attempt counts from 1: each retry, and the resend after a 401 token
	// refresh, is its own attempt.
	Attempt int
}

// ResponseInfo describes how one HTTP attempt ended: the response's status
// and headers, or Err when no response arrived (Status is then 0). Elapsed
// runs from sending the request to reading the whole body.
type ResponseInfo struct {
	Request RequestInfo
	Status  int
	Header  http.Header
	Elapsed time.Duration
	Err     error
}

// WithRequestHook calls hook before every HTTP attempt Fetch makes, retries
// included. Hooks observe; they can't change the request. Give the option
// more than once to add more hooks, which run in order. Token requests are
// not observed. To change requests in flight, wrap the transport of a client
// passed to WithHTTPClient instead.
func WithRequestHook(hook func(ctx context.Context, request RequestInfo)) Option {
	return func(c *Client) { c.requestHooks = append(c.requestHooks, hook) }
}

// WithResponseHook calls hook after every HTTP attempt Fetch makes — a
// retried 429 or 5xx, the 401 before a token refresh, and the final answer
// alike — and after an attempt that failed in transport. Like
// WithRequestHook it observes only, and may be given more than once.
func WithResponseHook(hook func(ctx context.Context, response ResponseInfo)) Option {
	return func(c *Client) { c.responseHooks = append(c.responseHooks, hook) }
}

// WithDebugLogging logs one line per HTTP attempt to logger (slog.Default()
// when nil) at debug level: the method, the URL with secret-looking query
// values redacted, the status or transport error, the elapsed time, and the
// attempt number. It never logs the Authorization header, a token, or a body.
// Like WithResponseHook it adds to any hooks already set.
func WithDebugLogging(logger *slog.Logger) Option {
	return WithResponseHook(func(ctx context.Context, r ResponseInfo) {
		log := logger
		if log == nil {
			log = slog.Default()
		}
		redacted := redactURL(r.Request.URL)
		attrs := []slog.Attr{slog.String("method", r.Request.Method), slog.String("url", redacted)}
		if r.Err != nil {
			attrs = append(attrs, slog.String("error", redactError(r.Err, r.Request.URL, redacted)))
		} else {
			attrs = append(attrs, slog.Int("status", r.Status))
		}
		attrs = append(attrs, slog.Duration("elapsed", r.Elapsed), slog.Int("attempt", r.Request.Attempt))
		log.LogAttrs(ctx, slog.LevelDebug, "box: http attempt", attrs...)
	})
}

// secretParamHints mark a query parameter whose value the debug log redacts:
// any name containing one of them, case-insensitively.
var secretParamHints = []string{"token", "secret", "password", "code", "assertion", "key", "signature", "auth"}

// redactURL is raw for a log line: any userinfo dropped, the fragment cut,
// and the value of every secret-looking query parameter replaced by REDACTED.
func redactURL(raw string) string {
	raw, _, _ = strings.Cut(raw, "#")
	base, query, hasQuery := strings.Cut(raw, "?")
	if scheme := strings.Index(base, "://"); scheme >= 0 {
		start := scheme + 3
		end := len(base)
		if slash := strings.IndexByte(base[start:], '/'); slash >= 0 {
			end = start + slash
		}
		if at := strings.LastIndexByte(base[start:end], '@'); at >= 0 {
			base = base[:start] + base[start+at+1:]
		}
	}
	if !hasQuery {
		return base
	}
	pairs := strings.Split(query, "&")
	for i, pair := range pairs {
		name, _, _ := strings.Cut(pair, "=")
		decoded, err := url.QueryUnescape(name)
		if err != nil {
			decoded = name
		}
		decoded = strings.ToLower(decoded)
		for _, hint := range secretParamHints {
			if strings.Contains(decoded, hint) {
				pairs[i] = name + "=REDACTED"
				break
			}
		}
	}
	return base + "?" + strings.Join(pairs, "&")
}

// redactError is err's message for a log line, with the raw request URL in it
// (a *url.Error carries one) replaced by its redacted form.
func redactError(err error, rawURL, redacted string) string {
	var urlErr *url.Error
	if errors.As(err, &urlErr) {
		return urlErr.Op + " " + redacted + ": " + urlErr.Err.Error()
	}
	return strings.ReplaceAll(err.Error(), rawURL, redacted)
}

// WithBaseURL overrides one base-URL class (custom deployments).
func WithBaseURL(name, base string) Option {
	return func(c *Client) { c.baseURLs[name] = strings.TrimRight(base, "/") }
}

// WithDefaultHeader adds a header sent with every request from this client —
// e.g. a tracing or User-Agent header the embedding application wants on
// every call. A header set on an individual Request (via the package-level
// WithHeader) takes precedence over a same-named default.
func WithDefaultHeader(name, value string) Option {
	return func(c *Client) { c.defaultHeaders.Set(name, value) }
}

// WithAsUserHeader returns a new Client that sends As-User: userID with every
// API request, so Box acts as that user (an admin or a service account whose
// app may "make API calls using the as-user header"). It shares this client's
// token source, HTTP client, base URLs, and retry policy; this client is
// unchanged. Token requests never carry the header.
func (c *Client) WithAsUserHeader(userID string) *Client {
	derived := *c
	derived.defaultHeaders = c.defaultHeaders.Clone()
	derived.defaultHeaders.Set("As-User", userID)
	return &derived
}

// New builds a runtime Client for a token source and options.
func New(ts TokenSource, opts ...Option) *Client {
	c := &Client{
		ts:             ts,
		maxRetries:     5,
		retryBase:      DefaultRetryBaseDelay,
		retryMax:       DefaultRetryMaxDelay,
		defaultHeaders: http.Header{},
		baseURLs: map[string]string{
			"api":             "https://api.box.com/2.0",
			"api_root":        "https://api.box.com",
			"upload":          "https://upload.box.com/api/2.0",
			"upload_session":  "https://upload.box.com/api/2.0",
			"oauth_authorize": "https://account.box.com/api/oauth2",
			"download":        "https://api.box.com/2.0",
		},
	}
	for _, o := range opts {
		o(c)
	}
	if !c.injected {
		c.http = apiHTTPClient(c.httpVersion, c.proxy)
	}
	return c
}

// BaseUrl returns the configured base URL for a D-106 class.
func (c *Client) BaseUrl(name string) string { return c.baseURLs[name] }

// AccessToken returns a valid access token for the configured auth flow.
func (c *Client) AccessToken(ctx context.Context) (string, error) {
	return c.ts.AccessToken(ctx)
}

// NewRequest creates a request envelope for a method and fully built URL.
func (c *Client) NewRequest(method string, rawURL string) *Request {
	return &Request{method: method, rawURL: rawURL, header: http.Header{}, query: url.Values{}}
}

// Fetch executes the request with retries: exponential backoff + jitter,
// a single 401 token refresh, and Retry-After on 429/503.
func (c *Client) Fetch(ctx context.Context, request *Request) (*Response, error) {
	token, err := c.AccessToken(ctx)
	if err != nil {
		return nil, err
	}
	u := request.rawURL
	if len(request.query) > 0 {
		u += "?" + request.query.Encode()
	}
	client := c.http
	if request.timeout > 0 {
		perRequest := *c.http
		perRequest.Timeout = request.timeout
		client = &perRequest
	}
	refreshed := false
	var lastErr error
	for attempt := 0; attempt <= c.maxRetries; attempt++ {
		var body io.Reader
		if request.getBody != nil {
			if body, err = request.getBody(); err != nil {
				return nil, err
			}
		}
		httpReq, err := http.NewRequestWithContext(ctx, request.method, u, body)
		if err != nil {
			return nil, err
		}
		for key, values := range c.defaultHeaders {
			if _, overridden := request.header[http.CanonicalHeaderKey(key)]; overridden {
				continue
			}
			for _, v := range values {
				httpReq.Header.Add(key, v)
			}
		}
		for key, values := range request.header {
			for _, v := range values {
				httpReq.Header.Add(key, v)
			}
		}
		httpReq.Header.Set("Authorization", "Bearer "+token)

		info := c.observeRequest(ctx, httpReq, attempt+1)
		started := time.Now()
		httpResp, err := client.Do(httpReq)
		if err != nil {
			c.observeResponse(ctx, info, nil, started, err)
			lastErr = err
			if attempt == c.maxRetries || !sleep(ctx, c.backoff(attempt)) {
				return nil, err
			}
			continue
		}
		resp, err := readResponse(httpResp)
		c.observeResponse(ctx, info, resp, started, err)
		if err != nil {
			return nil, err
		}

		// A single token refresh on 401, only when another attempt remains.
		if resp.status == http.StatusUnauthorized && !refreshed && attempt < c.maxRetries {
			refreshed = true
			if token, err = c.AccessToken(ctx); err != nil {
				return nil, err
			}
			continue
		}
		// Backoff on rate-limit / server errors, honoring Retry-After.
		if retriable(resp.status) && attempt < c.maxRetries {
			if sleep(ctx, c.retryAfter(resp, attempt)) {
				continue
			}
		}
		if resp.status < 200 || resp.status >= 300 {
			return nil, newAPIError(resp)
		}
		return resp, nil
	}
	if lastErr != nil {
		return nil, lastErr
	}
	return nil, context.DeadlineExceeded
}

// observeRequest runs the request hooks for one attempt and returns what they
// saw, for the response hooks to carry along.
func (c *Client) observeRequest(ctx context.Context, httpReq *http.Request, attempt int) RequestInfo {
	if len(c.requestHooks) == 0 && len(c.responseHooks) == 0 {
		return RequestInfo{}
	}
	header := httpReq.Header.Clone()
	header.Set("Authorization", "REDACTED")
	info := RequestInfo{Method: httpReq.Method, URL: httpReq.URL.String(), Header: header, Attempt: attempt}
	for _, hook := range c.requestHooks {
		hook(ctx, info)
	}
	return info
}

// observeResponse runs the response hooks for one attempt: resp when it
// arrived, err when it didn't.
func (c *Client) observeResponse(ctx context.Context, request RequestInfo, resp *Response, started time.Time, err error) {
	if len(c.responseHooks) == 0 {
		return
	}
	info := ResponseInfo{Request: request, Elapsed: time.Since(started), Err: err}
	if resp != nil {
		info.Status, info.Header = resp.status, resp.header.Clone()
	}
	for _, hook := range c.responseHooks {
		hook(ctx, info)
	}
}

func readResponse(httpResp *http.Response) (*Response, error) {
	defer httpResp.Body.Close()
	body, err := io.ReadAll(httpResp.Body)
	if err != nil {
		return nil, err
	}
	return &Response{status: httpResp.StatusCode, header: httpResp.Header, body: body}, nil
}

func retriable(status int) bool {
	return status == http.StatusTooManyRequests || status >= 500
}

// backoff is the wait before retrying after the given attempt: full jitter
// over min(retryBase·2^attempt, retryMax).
func (c *Client) backoff(attempt int) time.Duration {
	ceiling := min(c.retryBase, c.retryMax)
	for i := 0; i < attempt && ceiling < c.retryMax; i++ {
		ceiling = min(ceiling*2, c.retryMax)
		if ceiling < 0 { // overflowed
			ceiling = c.retryMax
		}
	}
	// Full jitter.
	return time.Duration(rand.Int63n(int64(ceiling) + 1))
}

func (c *Client) retryAfter(resp *Response, attempt int) time.Duration {
	if v := resp.header.Get("Retry-After"); v != "" {
		if secs, err := strconv.Atoi(v); err == nil {
			return time.Duration(secs) * time.Second
		}
	}
	return c.backoff(attempt)
}

func sleep(ctx context.Context, d time.Duration) bool {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

// WithHeader returns the request with the header set.
func WithHeader(request *Request, name string, value string) *Request {
	request.header.Set(name, value)
	return request
}

// WithTimeout returns the request with its own timeout, in place of the
// client's, for a call meant to wait longer (a long poll).
func WithTimeout(request *Request, timeout time.Duration) *Request {
	request.timeout = timeout
	return request
}

// WithQuery returns the request with the query parameter appended, encoded.
func WithQuery(request *Request, name string, value string) *Request {
	request.query.Add(name, value)
	return request
}

// WithJsonBody returns the request with the serialized JSON body.
func WithJsonBody(request *Request, body []byte) *Request {
	request.header.Set("Content-Type", "application/json")
	request.getBody = func() (io.Reader, error) { return bytes.NewReader(body), nil }
	return request
}

// WithFormBody returns the request with a form-urlencoded body.
func WithFormBody(request *Request, form []byte) *Request {
	request.header.Set("Content-Type", "application/x-www-form-urlencoded")
	request.getBody = func() (io.Reader, error) { return bytes.NewReader(form), nil }
	return request
}

// WithStreamBody returns the request with a streaming body. The stream is
// not replayable, so such requests are not retried after they begin.
func WithStreamBody(request *Request, body io.Reader, contentType string) *Request {
	request.header.Set("Content-Type", contentType)
	used := false
	request.getBody = func() (io.Reader, error) {
		if used {
			return nil, errStreamConsumed
		}
		used = true
		return body, nil
	}
	return request
}

// WithMultipartBody writes a JSON part named jsonPartName iff jsonBytes is
// non-empty, and a binary part named filePartName (used as both the form
// field name and the Content-Disposition filename — the real filename
// isn't structurally knowable) iff file is non-nil (G-7).
func WithMultipartBody(request *Request, jsonPartName string, jsonBytes []byte, filePartName string, file io.Reader) *Request {
	var buf bytes.Buffer
	writer := multipart.NewWriter(&buf)
	if len(jsonBytes) > 0 {
		_ = writeField(writer, jsonPartName, jsonBytes)
	}
	if file != nil {
		if part, err := writer.CreateFormFile(filePartName, filePartName); err == nil {
			_, _ = io.Copy(part, file)
		}
	}
	_ = writer.Close()
	payload := buf.Bytes()
	request.header.Set("Content-Type", writer.FormDataContentType())
	request.getBody = func() (io.Reader, error) { return bytes.NewReader(payload), nil }
	return request
}

func writeField(w *multipart.Writer, name string, value []byte) error {
	field, err := w.CreateFormField(name)
	if err != nil {
		return err
	}
	_, err = field.Write(value)
	return err
}

// ResponseBytes reads the whole response body.
func ResponseBytes(response *Response) ([]byte, error) { return response.body, nil }

// ResponseStream returns the response body as a stream.
func ResponseStream(response *Response) io.Reader { return bytes.NewReader(response.body) }

// ResponseHeader returns a response header value, empty when absent.
func ResponseHeader(response *Response, name string) string { return response.header.Get(name) }

// StatusCode returns the response status code.
func StatusCode(response *Response) int64 { return int64(response.status) }

type runtimeError string

func (e runtimeError) Error() string { return string(e) }

const errStreamConsumed runtimeError = "gantryruntime: streaming body cannot be retried"
