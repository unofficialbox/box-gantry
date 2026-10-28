// An internal test package (no self-import of the module path) so this file
// stays compilable when the compile-verification gate copies every .go file
// in this directory into a fixture under a different module path (see
// crates/gantry-backend-go/tests/compile_output.rs).
package gantryruntime

import (
	"context"
	"crypto/tls"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
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

func TestFetchReturnsA401InsteadOfSwallowingItWhenRetriesAreDisabled(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
	}))
	defer server.Close()

	client := New(fixedToken("t"), WithMaxRetries(0))
	resp, err := client.Fetch(context.Background(), client.NewRequest("GET", server.URL))
	if err != nil {
		t.Fatalf("fetch: %v", err)
	}
	if got := StatusCode(resp); got != http.StatusUnauthorized {
		t.Fatalf("status = %d, want %d", got, http.StatusUnauthorized)
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
