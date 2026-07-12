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
	"io"
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
	ts         TokenSource
	http       *http.Client
	baseURLs   map[string]string
	maxRetries int
}

// Option configures a Client at construction (the With* surface, G-3).
type Option func(*Client)

// WithHTTPClient overrides the HTTP client (timeouts, transport, proxies).
func WithHTTPClient(h *http.Client) Option { return func(c *Client) { c.http = h } }

// WithMaxRetries sets how many times a retriable failure is retried.
func WithMaxRetries(n int) Option { return func(c *Client) { c.maxRetries = n } }

// WithBaseURL overrides one base-URL class (custom deployments).
func WithBaseURL(name, base string) Option {
	return func(c *Client) { c.baseURLs[name] = strings.TrimRight(base, "/") }
}

// New builds a runtime Client for a token source and options.
func New(ts TokenSource, opts ...Option) *Client {
	c := &Client{
		ts:         ts,
		http:       &http.Client{Timeout: 60 * time.Second},
		maxRetries: 5,
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
		for key, values := range request.header {
			for _, v := range values {
				httpReq.Header.Add(key, v)
			}
		}
		httpReq.Header.Set("Authorization", "Bearer "+token)

		httpResp, err := c.http.Do(httpReq)
		if err != nil {
			lastErr = err
			if attempt == c.maxRetries || !sleep(ctx, backoff(attempt)) {
				return nil, err
			}
			continue
		}
		resp, err := readResponse(httpResp)
		if err != nil {
			return nil, err
		}

		// A single token refresh on 401.
		if resp.status == http.StatusUnauthorized && !refreshed {
			refreshed = true
			if token, err = c.AccessToken(ctx); err != nil {
				return nil, err
			}
			continue
		}
		// Backoff on rate-limit / server errors, honoring Retry-After.
		if retriable(resp.status) && attempt < c.maxRetries {
			if sleep(ctx, retryAfter(resp, attempt)) {
				continue
			}
		}
		return resp, nil
	}
	if lastErr != nil {
		return nil, lastErr
	}
	return nil, context.DeadlineExceeded
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

func backoff(attempt int) time.Duration {
	base := time.Duration(1<<attempt) * 500 * time.Millisecond
	if base > 30*time.Second {
		base = 30 * time.Second
	}
	// Full jitter.
	return time.Duration(rand.Int63n(int64(base) + 1))
}

func retryAfter(resp *Response, attempt int) time.Duration {
	if v := resp.header.Get("Retry-After"); v != "" {
		if secs, err := strconv.Atoi(v); err == nil {
			return time.Duration(secs) * time.Second
		}
	}
	return backoff(attempt)
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

// WithMultipartBody returns the request with a Box-style multipart body:
// an "attributes" JSON part plus a file part (G-7).
func WithMultipartBody(request *Request, attributes []byte, fileName string, file io.Reader) *Request {
	var buf bytes.Buffer
	writer := multipart.NewWriter(&buf)
	_ = writeField(writer, "attributes", attributes)
	if file != nil {
		if part, err := writer.CreateFormFile("file", fileName); err == nil {
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
