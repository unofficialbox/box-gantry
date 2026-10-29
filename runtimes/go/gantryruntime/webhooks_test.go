// An internal test package, like runtime_test.go, so the compile gate can copy
// it under another module path.
package gantryruntime

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"net/http"
	"testing"
	"time"
)

// The vectors from Box's own SDK test suite (testWebhookValidation): keys,
// bodies, and the signatures Box computed for them, all delivered at
// 2020-01-01T00:00:00-07:00.
const (
	vectorPrimaryKey   = "SamplePrimaryKey"
	vectorSecondaryKey = "SampleSecondaryKey"
	vectorTimestamp    = "2020-01-01T00:00:00-07:00"
	vectorBody         = `{"type":"webhook_event","webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":"Test.txt"}}`
	vectorPrimary      = "6TfeAW3A1PASkgboxxA5yqHNKOwFyMWuEXny/FPD5hI="
	vectorSecondary    = "v+1CD1Jdo3muIcbpv5lxxgPglOqMfsNHPV899xWYydo="
	vectorFilePrefix   = `{"webhook":{"id":"1234567890"},"trigger":"FILE.UPLOADED","source":{"id":"1234567890","type":"file","name":"`
)

func vectorHeader(primary string) http.Header {
	h := http.Header{}
	// Lowercase on purpose: lookups must not depend on the sender's casing.
	h.Set("box-delivery-id", "f96bb54b-ee16-4fc5-aa65-8c2d9e5b546f")
	h.Set("box-delivery-timestamp", vectorTimestamp)
	h.Set("box-signature-algorithm", "HmacSHA256")
	h.Set("box-signature-primary", primary)
	h.Set("box-signature-secondary", vectorSecondary)
	h.Set("box-signature-version", "1")
	return h
}

func at(timestamp string) func() time.Time {
	return func() time.Time {
		t, _ := time.Parse(time.RFC3339, timestamp)
		return t
	}
}

func TestWebhookVerifierAcceptsBoxSignatures(t *testing.T) {
	both := WebhookVerifier{PrimaryKey: vectorPrimaryKey, SecondaryKey: vectorSecondaryKey, Now: at(vectorTimestamp)}
	if !both.Verify([]byte(vectorBody), vectorHeader(vectorPrimary)) {
		t.Error("a delivery signed with both keys should verify")
	}
	secondaryOnly := WebhookVerifier{SecondaryKey: vectorSecondaryKey, Now: at(vectorTimestamp)}
	if !secondaryOnly.Verify([]byte(vectorBody), vectorHeader("not-the-primary")) {
		t.Error("the secondary key alone should verify, as during a primary rotation")
	}

	// Box signs non-ASCII characters as \uXXXX escapes; a body that arrives
	// as raw UTF-8 verifies through that form. Escapes already in the JSON
	// (\r, \/, \\) are signed as they are.
	for name, c := range map[string]struct{ name, signature string }{
		"japanese":        {"スクリーンショット 2020-08-05.txt", "LV2uCu+5NJtIHrCXDYgZ0v/PP5THGRuegw3RtdnEyuE="},
		"emoji":           {"\U0001F600 2020-08-05.txt", "xF/SDZosX4le+v4A0Qn59sZhuD1RqY5KRUKzVMSbh0E="},
		"carriage return": {`test \r`, "SVkbKgy3dEEf2PbbzpNu2lDZS7zZ/aboU7HOZgBGrJk="},
		"forward slash":   {`\/`, "t41PWT5ZB6OcysnD6SDy9Ud+p9hdXxIdXqcdweyZv/Q="},
		"backslash":       {`\\`, "ERpMZwUQsGDTfj82ehdX6VvDZfvOhK5ULNfVmwVAGe0="},
	} {
		body := []byte(vectorFilePrefix + c.name + `"}}`)
		v := WebhookVerifier{PrimaryKey: vectorPrimaryKey, Now: at(vectorTimestamp)}
		if !v.Verify(body, vectorHeader(c.signature)) {
			t.Errorf("%s: Box's signature should verify", name)
		}
	}
}

func TestWebhookVerifierRejects(t *testing.T) {
	v := WebhookVerifier{PrimaryKey: vectorPrimaryKey, SecondaryKey: vectorSecondaryKey, Now: at(vectorTimestamp)}
	body := []byte(vectorBody)
	without := func(names ...string) http.Header {
		h := vectorHeader(vectorPrimary)
		for _, name := range names {
			h.Del(name)
		}
		return h
	}
	with := func(name, value string) http.Header {
		h := vectorHeader(vectorPrimary)
		h.Set(name, value)
		return h
	}
	for name, c := range map[string]struct {
		v      WebhookVerifier
		body   []byte
		header http.Header
	}{
		"wrong keys":            {WebhookVerifier{PrimaryKey: "IncorrectKey", SecondaryKey: "IncorrectKey", Now: v.Now}, body, vectorHeader(vectorPrimary)},
		"no keys":               {WebhookVerifier{Now: v.Now}, body, vectorHeader(vectorPrimary)},
		"tampered body":         {v, []byte(vectorBody[:len(vectorBody)-2] + " }}"), vectorHeader(vectorPrimary)},
		"stale":                 {WebhookVerifier{PrimaryKey: vectorPrimaryKey, Now: at("2020-01-01T07:10:01Z")}, body, vectorHeader(vectorPrimary)},
		"future":                {WebhookVerifier{PrimaryKey: vectorPrimaryKey, Now: at("2020-01-01T06:59:59Z")}, body, vectorHeader(vectorPrimary)},
		"short max age":         {WebhookVerifier{PrimaryKey: vectorPrimaryKey, MaxAge: time.Minute, Now: at("2020-01-01T07:01:01Z")}, body, vectorHeader(vectorPrimary)},
		"no timestamp":          {v, body, without("Box-Delivery-Timestamp")},
		"no signatures":         {v, body, without("Box-Signature-Primary", "Box-Signature-Secondary")},
		"no version":            {v, body, without("Box-Signature-Version")},
		"version 2":             {v, body, with("Box-Signature-Version", "2")},
		"HmacSHA1":              {v, body, with("Box-Signature-Algorithm", "HmacSHA1")},
		"bad timestamp":         {v, body, with("Box-Delivery-Timestamp", "yesterday")},
		"not base64":            {WebhookVerifier{PrimaryKey: vectorPrimaryKey, Now: v.Now}, body, with("Box-Signature-Primary", "%%%")},
		"secondary for primary": {WebhookVerifier{PrimaryKey: vectorSecondaryKey, Now: v.Now}, body, vectorHeader(vectorPrimary)},
	} {
		if c.v.Verify(c.body, c.header) {
			t.Errorf("%s: should not verify", name)
		}
	}
	// Ten minutes old exactly is still in time.
	edge := WebhookVerifier{PrimaryKey: vectorPrimaryKey, Now: at("2020-01-01T07:10:00Z")}
	if !edge.Verify(body, vectorHeader(vectorPrimary)) {
		t.Error("a delivery exactly MaxAge old should verify")
	}
}

// A signature computed here, independently of Verify, for a fresh delivery
// under the real clock.
func TestWebhookVerifierUsesTheRealClock(t *testing.T) {
	timestamp := time.Now().UTC().Format(time.RFC3339)
	mac := hmac.New(sha256.New, []byte("live-key"))
	mac.Write([]byte(vectorBody + timestamp))
	h := vectorHeader(base64.StdEncoding.EncodeToString(mac.Sum(nil)))
	h.Set("Box-Delivery-Timestamp", timestamp)
	if !(WebhookVerifier{PrimaryKey: "live-key"}).Verify([]byte(vectorBody), h) {
		t.Error("a fresh delivery should verify against time.Now")
	}
}
