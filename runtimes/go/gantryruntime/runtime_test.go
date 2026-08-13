// An internal test package (no self-import of the module path) so this file
// stays compilable when the compile-verification gate copies every .go file
// in this directory into a fixture under a different module path (see
// crates/gantry-backend-go/tests/compile_output.rs).
package gantryruntime

import (
	"context"
	"net/http"
	"net/http/httptest"
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
