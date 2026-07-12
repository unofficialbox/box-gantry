package gantryruntime

import (
	"context"
	"crypto"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

// tokenServer is a stand-in Box token endpoint that records the last form
// it received and replies with a scripted token response.
type tokenServer struct {
	server   *httptest.Server
	lastForm url.Values
	hits     int
	reply    func(form url.Values) string
}

func newTokenServer(t *testing.T, reply func(url.Values) string) *tokenServer {
	t.Helper()
	ts := &tokenServer{reply: reply}
	ts.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := r.ParseForm(); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		ts.hits++
		ts.lastForm = r.PostForm
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(ts.reply(r.PostForm)))
	}))
	t.Cleanup(ts.server.Close)
	return ts
}

func TestDeveloperTokenSource(t *testing.T) {
	src := DeveloperToken("dev-123")
	got, err := src.AccessToken(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got != "dev-123" {
		t.Fatalf("developer token: got %q", got)
	}
}

func TestClientCredentialsGrant(t *testing.T) {
	srv := newTokenServer(t, func(url.Values) string {
		return `{"access_token":"ccg-tok","expires_in":3600}`
	})
	src := ClientCredentials(CCGConfig{
		ClientID:     "cid",
		ClientSecret: "secret",
		EnterpriseID: "12345",
		TokenURL:     srv.server.URL,
	})

	tok, err := src.AccessToken(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if tok != "ccg-tok" {
		t.Fatalf("ccg token: got %q", tok)
	}
	if srv.lastForm.Get("grant_type") != "client_credentials" {
		t.Fatalf("grant_type: got %q", srv.lastForm.Get("grant_type"))
	}
	if srv.lastForm.Get("box_subject_type") != "enterprise" ||
		srv.lastForm.Get("box_subject_id") != "12345" {
		t.Fatalf("subject: %q/%q", srv.lastForm.Get("box_subject_type"), srv.lastForm.Get("box_subject_id"))
	}

	// A second call within the token's lifetime is served from cache.
	if _, err := src.AccessToken(context.Background()); err != nil {
		t.Fatal(err)
	}
	if srv.hits != 1 {
		t.Fatalf("expected the token to be cached (1 hit), got %d", srv.hits)
	}
}

func TestClientCredentialsUserSubject(t *testing.T) {
	srv := newTokenServer(t, func(url.Values) string {
		return `{"access_token":"u","expires_in":3600}`
	})
	src := ClientCredentials(CCGConfig{ClientID: "c", ClientSecret: "s", UserID: "99", TokenURL: srv.server.URL})
	if _, err := src.AccessToken(context.Background()); err != nil {
		t.Fatal(err)
	}
	if srv.lastForm.Get("box_subject_type") != "user" || srv.lastForm.Get("box_subject_id") != "99" {
		t.Fatalf("user subject not set: %q/%q", srv.lastForm.Get("box_subject_type"), srv.lastForm.Get("box_subject_id"))
	}
}

func TestCachedTokenRefreshesOnExpiry(t *testing.T) {
	srv := newTokenServer(t, func(url.Values) string {
		// expires_in less than the refresh margin forces a refresh each call.
		return `{"access_token":"short","expires_in":1}`
	})
	src := ClientCredentials(CCGConfig{ClientID: "c", ClientSecret: "s", EnterpriseID: "1", TokenURL: srv.server.URL})
	for i := 0; i < 3; i++ {
		if _, err := src.AccessToken(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if srv.hits != 3 {
		t.Fatalf("expected a refresh on every call (3 hits), got %d", srv.hits)
	}
}

func TestOAuthRefreshRotatesToken(t *testing.T) {
	srv := newTokenServer(t, func(form url.Values) string {
		if form.Get("refresh_token") == "rt-1" {
			return `{"access_token":"at-1","refresh_token":"rt-2","expires_in":1}`
		}
		return `{"access_token":"at-2","refresh_token":"rt-3","expires_in":3600}`
	})
	src := OAuth(OAuthConfig{ClientID: "c", ClientSecret: "s", TokenURL: srv.server.URL}, "rt-1")

	first, err := src.AccessToken(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if first != "at-1" || srv.lastForm.Get("grant_type") != "refresh_token" {
		t.Fatalf("first exchange wrong: %q / %q", first, srv.lastForm.Get("grant_type"))
	}
	// The short TTL forces a second exchange, which must present the rotated
	// refresh token rt-2, not the original.
	second, err := src.AccessToken(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if second != "at-2" {
		t.Fatalf("second exchange token: got %q", second)
	}
	if srv.lastForm.Get("refresh_token") != "rt-2" {
		t.Fatalf("refresh token was not rotated: got %q", srv.lastForm.Get("refresh_token"))
	}
}

func TestOAuthExchangeCode(t *testing.T) {
	srv := newTokenServer(t, func(url.Values) string {
		return `{"access_token":"at","refresh_token":"rt","expires_in":3600}`
	})
	cfg := OAuthConfig{ClientID: "c", ClientSecret: "s", TokenURL: srv.server.URL}
	src, err := cfg.ExchangeCode(context.Background(), "the-code", "https://app/redirect")
	if err != nil {
		t.Fatal(err)
	}
	if srv.lastForm.Get("grant_type") != "authorization_code" || srv.lastForm.Get("code") != "the-code" {
		t.Fatalf("code exchange form wrong: %v", srv.lastForm)
	}
	// The access token from the exchange is used before any refresh.
	tok, err := src.AccessToken(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if tok != "at" {
		t.Fatalf("exchange token: got %q", tok)
	}
	if srv.hits != 1 {
		t.Fatalf("exchange result should be cached, got %d hits", srv.hits)
	}
}

func TestAuthorizeURL(t *testing.T) {
	cfg := OAuthConfig{ClientID: "abc"}
	got := cfg.AuthorizeURL("https://app/cb", "xyz")
	if !strings.HasPrefix(got, authorizeURL+"?") {
		t.Fatalf("authorize URL prefix: %q", got)
	}
	u, err := url.Parse(got)
	if err != nil {
		t.Fatal(err)
	}
	q := u.Query()
	if q.Get("response_type") != "code" || q.Get("client_id") != "abc" ||
		q.Get("redirect_uri") != "https://app/cb" || q.Get("state") != "xyz" {
		t.Fatalf("authorize URL query wrong: %v", q)
	}
}

func TestJWTAuthSignsAndExchanges(t *testing.T) {
	key, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	keyPEM := pem.EncodeToMemory(&pem.Block{
		Type:  "RSA PRIVATE KEY",
		Bytes: x509.MarshalPKCS1PrivateKey(key),
	})

	var gotAssertion string
	srv := newTokenServer(t, func(form url.Values) string {
		gotAssertion = form.Get("assertion")
		return `{"access_token":"jwt-tok","expires_in":3600}`
	})

	src, err := JWTAuth(JWTConfig{
		ClientID:      "client",
		ClientSecret:  "secret",
		PublicKeyID:   "kid-1",
		PrivateKeyPEM: keyPEM,
		EnterpriseID:  "ent-1",
		TokenURL:      srv.server.URL,
	})
	if err != nil {
		t.Fatal(err)
	}
	tok, err := src.AccessToken(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if tok != "jwt-tok" {
		t.Fatalf("jwt token: got %q", tok)
	}
	if srv.lastForm.Get("grant_type") != "urn:ietf:params:oauth:grant-type:jwt-bearer" {
		t.Fatalf("jwt grant_type: got %q", srv.lastForm.Get("grant_type"))
	}

	// The assertion is a real RS256 JWT the paired public key verifies, with
	// the enterprise subject and configured kid.
	parts := strings.Split(gotAssertion, ".")
	if len(parts) != 3 {
		t.Fatalf("assertion is not a three-part JWT: %q", gotAssertion)
	}
	signingInput := parts[0] + "." + parts[1]
	digest := sha256.Sum256([]byte(signingInput))
	sig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		t.Fatal(err)
	}
	if err := rsa.VerifyPKCS1v15(&key.PublicKey, crypto.SHA256, digest[:], sig); err != nil {
		t.Fatalf("assertion signature does not verify: %v", err)
	}
	header := decodeSegment(t, parts[0])
	if header["alg"] != "RS256" || header["kid"] != "kid-1" {
		t.Fatalf("header wrong: %v", header)
	}
	claims := decodeSegment(t, parts[1])
	if claims["sub"] != "ent-1" || claims["box_sub_type"] != "enterprise" {
		t.Fatalf("claims subject wrong: %v", claims)
	}
	if claims["aud"] != srv.server.URL {
		t.Fatalf("claims aud: got %v", claims["aud"])
	}
}

func TestJWTAuthRejectsBadKey(t *testing.T) {
	_, err := JWTAuth(JWTConfig{PrivateKeyPEM: []byte("not a pem")})
	if err == nil {
		t.Fatal("expected a bad key to fail at construction")
	}
}

func decodeSegment(t *testing.T, seg string) map[string]any {
	t.Helper()
	raw, err := base64.RawURLEncoding.DecodeString(seg)
	if err != nil {
		t.Fatal(err)
	}
	var m map[string]any
	if err := json.Unmarshal(raw, &m); err != nil {
		t.Fatal(err)
	}
	return m
}
