package gantryruntime

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
)

// APIError is a non-2xx response from Box. Fetch returns it once the retry
// budget and the single 401 refresh are spent, so a caller never mistakes an
// error body for a result. Code, Message, and RequestID come from Box's error
// body when it has one; Body is the raw payload either way. Match it with
// errors.As:
//
//	var apiErr *gantryruntime.APIError
//	if errors.As(err, &apiErr) && apiErr.Status == http.StatusNotFound { … }
type APIError struct {
	Status    int
	Code      string
	Message   string
	RequestID string
	Header    http.Header
	Body      []byte
}

func (e *APIError) Error() string {
	var b strings.Builder
	fmt.Fprintf(&b, "box: status %d", e.Status)
	if e.Code != "" {
		b.WriteString(" " + e.Code)
	}
	if e.Message != "" {
		b.WriteString(": " + e.Message)
	}
	if e.RequestID != "" {
		b.WriteString(" (request_id " + e.RequestID + ")")
	}
	return b.String()
}

// newAPIError builds the error for a non-2xx response, reading Box's error
// fields from the body when it is a Box error object.
func newAPIError(resp *Response) *APIError {
	apiErr := &APIError{Status: resp.status, Header: resp.header, Body: resp.body}
	var payload struct {
		Code      string `json:"code"`
		Message   string `json:"message"`
		RequestID string `json:"request_id"`
	}
	if json.Unmarshal(resp.body, &payload) == nil {
		apiErr.Code, apiErr.Message, apiErr.RequestID = payload.Code, payload.Message, payload.RequestID
	}
	return apiErr
}

// IsNotModified reports whether err is Box's 304 Not Modified: the answer to
// an If-None-Match read whose etag still matches the item.
func IsNotModified(err error) bool {
	var apiErr *APIError
	return errors.As(err, &apiErr) && apiErr.Status == http.StatusNotModified
}
