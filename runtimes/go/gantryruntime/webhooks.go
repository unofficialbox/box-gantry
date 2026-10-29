package gantryruntime

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"net/http"
	"strings"
	"time"
	"unicode/utf16"
	"unicode/utf8"
)

// DefaultWebhookMaxAge is how old a delivery may be before a WebhookVerifier
// rejects it as a possible replay: Box's ten minutes.
const DefaultWebhookMaxAge = 10 * time.Minute

// WebhookVerifier checks that a webhook delivery came from Box, unaltered and
// recent, by Box's v2 signature scheme: each BOX-SIGNATURE-* header is the
// base64 HMAC-SHA256, under that signature key, of the body followed by the
// BOX-DELIVERY-TIMESTAMP header.
type WebhookVerifier struct {
	// PrimaryKey and SecondaryKey are the app's signature keys from the
	// Developer Console. Either may be empty: a delivery is genuine when the
	// signature of any configured key matches, so a key can be rotated while
	// the other keeps verifying.
	PrimaryKey   string
	SecondaryKey string
	// MaxAge is how old a delivery may be; zero means DefaultWebhookMaxAge.
	MaxAge time.Duration
	// Now is the clock; nil means time.Now.
	Now func() time.Time
}

// Verify reports whether body and header are a genuine, recent delivery from
// Box. Pass the body exactly as received. It is false for an unknown signature
// version or algorithm, a delivery older than MaxAge or timestamped in the
// future, and a signature no configured key produces.
func (v WebhookVerifier) Verify(body []byte, header http.Header) bool {
	if header.Get("Box-Signature-Version") != "1" || header.Get("Box-Signature-Algorithm") != "HmacSHA256" {
		return false
	}
	timestamp := header.Get("Box-Delivery-Timestamp")
	delivered, err := time.Parse(time.RFC3339, timestamp)
	if err != nil {
		return false
	}
	now, maxAge := time.Now, v.MaxAge
	if v.Now != nil {
		now = v.Now
	}
	if maxAge <= 0 {
		maxAge = DefaultWebhookMaxAge
	}
	// Whole seconds, as Box's own SDKs compare them.
	at, sent := now().Unix(), delivered.Unix()
	if sent > at || at-sent > int64(maxAge/time.Second) {
		return false
	}

	bodies := [][]byte{body}
	if escaped, ok := escapeNonASCII(body); ok {
		bodies = append(bodies, escaped)
	}
	for _, signed := range [...]struct{ key, header string }{
		{v.PrimaryKey, "Box-Signature-Primary"},
		{v.SecondaryKey, "Box-Signature-Secondary"},
	} {
		received, err := base64.StdEncoding.DecodeString(header.Get(signed.header))
		if signed.key == "" || err != nil || len(received) == 0 {
			continue
		}
		for _, b := range bodies {
			mac := hmac.New(sha256.New, []byte(signed.key))
			mac.Write(b)
			mac.Write([]byte(timestamp))
			if hmac.Equal(mac.Sum(nil), received) {
				return true
			}
		}
	}
	return false
}

// escapeNonASCII is body with every character from U+007F up written as a
// JSON \uXXXX escape (UTF-16, lowercase hex) — the form Box signs. A body
// re-encoded as raw UTF-8 on its way to the caller verifies through it. It is
// false when body is already all ASCII, or is not UTF-8.
func escapeNonASCII(body []byte) ([]byte, bool) {
	if !utf8.Valid(body) {
		return nil, false
	}
	var out strings.Builder
	escaped := false
	for _, r := range string(body) {
		if r < 0x7f {
			out.WriteRune(r)
			continue
		}
		escaped = true
		if r1, r2 := utf16.EncodeRune(r); r1 != utf8.RuneError {
			fmt.Fprintf(&out, `\u%04x\u%04x`, r1, r2)
		} else {
			fmt.Fprintf(&out, `\u%04x`, r)
		}
	}
	return []byte(out.String()), escaped
}
