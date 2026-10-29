package gantryruntime

import (
	"context"
	"encoding/json"
	"errors"
	"iter"
	"net/url"
	"strings"
	"time"
)

// LongPollServer is one of Box's real-time servers, as `OPTIONS /events`
// lists them. A zero MaxRetries or RetryTimeout takes Box's documented
// default.
type LongPollServer struct {
	URL          string
	MaxRetries   int
	RetryTimeout time.Duration
}

// EventSource is what EventStream drives: the typed calls a generated events
// manager supplies. Server lists a real-time server; Page fetches the events
// after a stream position and returns the position to wait from next; ID
// names an event so a repeat can be dropped ("" when it has none).
type EventSource[E any] struct {
	Server func(ctx context.Context) (LongPollServer, error)
	Page   func(ctx context.Context, position string) (events []E, next string, err error)
	ID     func(event E) string
}

// ErrNoLongPollServer means `OPTIONS /events` listed no real-time server.
var ErrNoLongPollServer = errors.New("box: no real-time server to long-poll")

const (
	defaultLongPollRetries = 10
	defaultLongPollTimeout = 610 * time.Second
	// Box may deliver an event twice; this many recent event ids are kept to
	// drop the repeats (the bound Box's own SDKs use).
	maxSeenEvents = 1000
)

// EventStream follows Box's long-poll protocol from position ("now" for
// events from here on): it yields the events after position, each once,
// then waits on a real-time server until Box reports a change and yields
// those. A `reconnect` answer, or exhausting the server's retries, lists a
// fresh server. A request that times out or fails in transit is retried; an
// error response from Box, or ctx ending, is yielded and ends the stream.
func EventStream[E any](ctx context.Context, c *Client, position string, source EventSource[E]) iter.Seq2[E, error] {
	return func(yield func(E, error) bool) {
		var zero E
		seen := map[string]bool{}
		var server *LongPollServer
		retries := 0
		for {
			events, next, err := source.Page(ctx, position)
			if err != nil {
				yield(zero, err)
				return
			}
			for _, event := range events {
				if id := source.ID(event); id != "" {
					if seen[id] {
						continue
					}
					seen[id] = true
				}
				if !yield(event, nil) {
					return
				}
			}
			if len(seen) >= maxSeenEvents {
				// Repeats arrive close together: keep only this page's ids.
				seen = map[string]bool{}
				for _, event := range events {
					if id := source.ID(event); id != "" {
						seen[id] = true
					}
				}
			}
			if next != "" {
				position = next
			}

			// Wait for Box to report a change after position.
			for changed := false; !changed; {
				if server == nil || retries >= server.MaxRetries {
					listed, err := source.Server(ctx)
					if err != nil {
						yield(zero, err)
						return
					}
					if listed.MaxRetries <= 0 {
						listed.MaxRetries = defaultLongPollRetries
					}
					if listed.RetryTimeout <= 0 {
						listed.RetryTimeout = defaultLongPollTimeout
					}
					server, retries = &listed, 0
				}
				retries++
				message, err := c.longPoll(ctx, *server, position)
				var apiErr *APIError
				switch {
				case ctx.Err() != nil:
					yield(zero, ctx.Err())
					return
				case errors.As(err, &apiErr):
					yield(zero, err)
					return
				case err != nil:
					// Timed out or failed in transit: poll again.
				case message == "new_change":
					changed = true
				case message == "reconnect", message == "max_retries":
					server = nil
				}
			}
		}
	}
}

// longPoll waits on a real-time server for Box's next message about the
// stream after position: `new_change`, `reconnect`, or `max_retries`.
func (c *Client) longPoll(ctx context.Context, server LongPollServer, position string) (string, error) {
	separator := "?"
	if strings.Contains(server.URL, "?") {
		separator = "&"
	}
	request := c.NewRequest("GET", server.URL+separator+"stream_position="+url.QueryEscape(position))
	resp, err := c.Fetch(ctx, WithTimeout(request, server.RetryTimeout))
	if err != nil {
		return "", err
	}
	var answer struct {
		Message string `json:"message"`
	}
	if err := json.Unmarshal(resp.body, &answer); err != nil {
		return "", err
	}
	return answer.Message, nil
}

// StreamPosition renders a stream position Box sent as a JSON string or
// number as the string to send back.
func StreamPosition(raw json.RawMessage) string {
	var text string
	if json.Unmarshal(raw, &text) == nil {
		return text
	}
	return strings.TrimSpace(string(raw))
}
