package gantryruntime

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

// realtime answers long polls from a script, one answer per request; "hang"
// holds the request past the client's timeout.
type realtime struct {
	mu        sync.Mutex
	answers   []string
	positions []string
}

func (rt *realtime) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	rt.mu.Lock()
	rt.positions = append(rt.positions, r.URL.Query().Get("stream_position"))
	answer := "reconnect"
	if len(rt.answers) > 0 {
		answer, rt.answers = rt.answers[0], rt.answers[1:]
	}
	rt.mu.Unlock()
	switch answer {
	case "hang":
		time.Sleep(300 * time.Millisecond)
	case "error":
		w.WriteHeader(http.StatusBadRequest)
		io.WriteString(w, `{"type":"error","status":400,"code":"bad_request"}`)
	default:
		fmt.Fprintf(w, `{"version":1,"message":%q}`, answer)
	}
}

// pages scripts EventSource.Page: the events and next position per call.
func source(t *testing.T, server string, servers *int, pages [][]string) EventSource[string] {
	t.Helper()
	call := 0
	return EventSource[string]{
		Server: func(context.Context) (LongPollServer, error) {
			*servers++
			return LongPollServer{URL: server + "/subscribe?channel=c", RetryTimeout: 100 * time.Millisecond}, nil
		},
		Page: func(_ context.Context, position string) ([]string, string, error) {
			if want := fmt.Sprint(call); call > 0 && position != want {
				t.Errorf("page %d fetched from %q, want %q", call, position, want)
			}
			if call >= len(pages) {
				return nil, fmt.Sprint(call + 1), nil
			}
			events := pages[call]
			call++
			return events, fmt.Sprint(call), nil
		},
		ID: func(event string) string { return strings.TrimSuffix(event, "'") },
	}
}

func TestEventStreamFollowsTheLongPollProtocol(t *testing.T) {
	rt := &realtime{answers: []string{"reconnect", "new_change", "hang", "new_change"}}
	server := httptest.NewServer(rt)
	defer server.Close()
	client := New(fixedToken("t"), WithMaxRetries(0))

	servers := 0
	// "now" yields nothing; then a and b; then b again (a repeat, as Box may
	// send) with c.
	pages := [][]string{nil, {"a", "b"}, {"b'", "c"}}
	var got []string
	for event, err := range EventStream(context.Background(), client, "now", source(t, server.URL, &servers, pages)) {
		if err != nil {
			t.Fatalf("stream: %v", err)
		}
		got = append(got, event)
		if len(got) == 3 {
			break
		}
	}
	if strings.Join(got, ",") != "a,b,c" {
		t.Fatalf("events %v, want a,b,c (the repeated b dropped)", got)
	}
	// reconnect lists a fresh server; a timed-out poll is retried on the same one.
	if servers != 2 {
		t.Fatalf("listed %d servers, want 2 (initial + after reconnect)", servers)
	}
	if want := "1,1,2,2"; strings.Join(rt.positions, ",") != want {
		t.Fatalf("long polls at positions %v, want %s", rt.positions, want)
	}
}

func TestEventStreamEndsOnAnErrorResponseOrCancellation(t *testing.T) {
	server := httptest.NewServer(&realtime{answers: []string{"error"}})
	defer server.Close()
	client := New(fixedToken("t"), WithMaxRetries(0))

	servers := 0
	for _, err := range EventStream(context.Background(), client, "now", source(t, server.URL, &servers, nil)) {
		var apiErr *APIError
		if !errors.As(err, &apiErr) || apiErr.Status != http.StatusBadRequest {
			t.Fatalf("err = %v, want the long poll's 400", err)
		}
		break
	}

	ctx, cancel := context.WithCancel(context.Background())
	hanging := httptest.NewServer(&realtime{answers: []string{"hang", "hang", "hang"}})
	defer hanging.Close()
	time.AfterFunc(50*time.Millisecond, cancel)
	for _, err := range EventStream(ctx, client, "now", source(t, hanging.URL, &servers, nil)) {
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("err = %v, want context.Canceled", err)
		}
	}
}

func TestStreamPositionReadsAStringOrANumber(t *testing.T) {
	for raw, want := range map[string]string{`"1152922976252290886"`: "1152922976252290886", `1152922976252290886`: "1152922976252290886"} {
		if got := StreamPosition([]byte(raw)); got != want {
			t.Fatalf("StreamPosition(%s) = %q, want %q", raw, got, want)
		}
	}
}
