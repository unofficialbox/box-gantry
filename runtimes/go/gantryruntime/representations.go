package gantryruntime

import (
	"context"
	"encoding/json"
	"io"
	"strings"
	"time"
)

// DefaultRepresentationWait bounds how long RepresentationContent waits for
// Box to generate a representation when ctx has no earlier deadline.
const DefaultRepresentationWait = 5 * time.Minute

// representation is one of Box's representation entries.
type representation struct {
	Representation string `json:"representation"`
	Info           struct {
		URL string `json:"url"`
	} `json:"info"`
	Status struct {
		State string `json:"state"`
		Code  string `json:"code"`
	} `json:"status"`
	Content struct {
		URLTemplate string `json:"url_template"`
	} `json:"content"`
}

// RepresentationContent runs Box's representation protocol for the item req
// fetches, which already asks for field and carries the X-Rep-Hints header:
// it takes the first representation Box offers under field, polls its info
// URL while Box generates it (`pending`, `none`), then downloads assetPath
// from its content URL template ("" for a single-file representation such as
// a PDF or extracted text). A representation Box fails to generate (`error`)
// is an error. ctx bounds the wait, as does DefaultRepresentationWait.
func (c *Client) RepresentationContent(ctx context.Context, req *Request, field string, assetPath string) (io.Reader, error) {
	ctx, cancel := context.WithTimeout(ctx, DefaultRepresentationWait)
	defer cancel()
	resp, err := c.Fetch(ctx, req)
	if err != nil {
		return nil, err
	}
	var item map[string]json.RawMessage
	if err := json.Unmarshal(resp.body, &item); err != nil {
		return nil, err
	}
	var set struct {
		Entries []representation `json:"entries"`
	}
	if raw, ok := item[field]; ok {
		if err := json.Unmarshal(raw, &set); err != nil {
			return nil, err
		}
	}
	entries := set.Entries
	if len(entries) == 0 {
		return nil, runtimeError("box: no representation matches the X-Rep-Hints requested")
	}
	rep := entries[0]
	delay := 250 * time.Millisecond
	for rep.Status.State != "success" && rep.Status.State != "viewable" {
		switch rep.Status.State {
		case "pending", "none":
		case "error":
			return nil, runtimeError("box: Box could not generate the " + rep.Representation + " representation (" + rep.Status.Code + ")")
		default:
			return nil, runtimeError("box: unknown representation state " + rep.Status.State)
		}
		if rep.Info.URL == "" {
			return nil, runtimeError("box: the " + rep.Representation + " representation has no info URL to poll")
		}
		if rep.Status.State == "pending" && !sleep(ctx, delay) {
			return nil, ctx.Err()
		}
		delay = min(delay*2, 5*time.Second)
		resp, err := c.Fetch(ctx, c.NewRequest("GET", rep.Info.URL))
		if err != nil {
			return nil, err
		}
		var polled representation
		if err := json.Unmarshal(resp.body, &polled); err != nil {
			return nil, err
		}
		rep.Status = polled.Status
		if polled.Content.URLTemplate != "" {
			rep.Content = polled.Content
		}
	}
	if rep.Content.URLTemplate == "" {
		return nil, runtimeError("box: the " + rep.Representation + " representation has no content URL")
	}
	url := strings.Replace(rep.Content.URLTemplate, "{+asset_path}", assetPath, 1)
	content, err := c.Fetch(ctx, c.NewRequest("GET", url))
	if err != nil {
		return nil, err
	}
	return ResponseStream(content), nil
}
