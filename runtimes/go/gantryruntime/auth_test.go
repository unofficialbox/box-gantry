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

// Box's box_config.json ships the RSA key as an encrypted PKCS#8 block (PBES2).
// These fixtures were produced with:
//
//	openssl genrsa 2048 | openssl pkcs8 -topk8 -v2 aes-256-cbc \
//	    -v2prf hmacWithSHA256 -passout pass:testpass123
//
// and the AES-128 / HMAC-SHA1 variant likewise, to cover both a modern and a
// default-PRF scheme. Each key is throwaway (generated for this test only).
const encPKCS8PBKDF2SHA256AES256 = `-----BEGIN ENCRYPTED PRIVATE KEY-----
MIIFNTBfBgkqhkiG9w0BBQ0wUjAxBgkqhkiG9w0BBQwwJAQQI8S3fhrpSQBHC7rb
XvjftQICCAAwDAYIKoZIhvcNAgkFADAdBglghkgBZQMEASoEEEmahLu16I3znudE
uU9xIukEggTQz95tfDXKaUWAMSoJESxtFfS98e5NhpeLM6v7g558b46JLB40IRzK
t0AvebAEyLF/WnGkVELYT2fczCOllYf/xo8j/xDcpxLf0zhHMJ/NwLOFfM8K3owZ
2GjUwqdBfmmiBkNUe5mSJPRvk4KRuHcqCMvzVK8W0RKQiE4CPAF1w7Tk0sYi70GZ
PQ4wdeXx5GVS3LsxlmtIMU9gaNhjp+aouAtyqiQ8yClNy1PVxeD8JLVn2xtKxXlp
OPdJIfiSBg0v2c05VGLluXEB3NXSQilhc9pQUGZs7Zh5YHXaUTTQ69/3ZY39+lV4
rI0dcv8qb7o/3bQUomQ9PDu+47fF6JhVSiT6CHVn9AT/D9nqd70X/wM4tM/ao+fe
OCdbH3GhgwO+HQJ75Ngi87dIrLg/2S3IdM9OQ0uQfF4n4/cq4nq9epVmE2mUD10e
ciuzSn3PadX2B3WzcjyuURU+51Ts2J913Fidga7yqmNzqq8H0hbgrxA7bVB2BM6k
yIuUDxAYkOASorsRvsWzeIPTc5xeqP9yc5ren9O17a9BMDN91xQKmhDJOGkHGcvz
cW0EW+gRBtimdWHd6NRarCDDQ7OvceA/iHgH1ndLVE4x3MulK8VKe9UcR05cJTxy
Ctd2lIqsF8CJ/ms/0V/fvEHt0xM9V6QweHYGw3zkBuC1UGhEMhi1otnut2URA6ZS
/ATBRTxS+/pYAbibmMKW9zOQ6PH2yudrMFHHqHt+/maeVgyEX3ZecpzJYoZJPzms
1q5cnWmKE6qhd+1mK7nKPOCTGs/1FRo1s8mb0jjNHyzFTAEnQUmKrOq5XKSOW6H8
NBAWS9i0sUP8oyWHc5qWxlJqSueSlWbajV0kqjs3+mkIlN8OzivazkxsSWrwqP/z
ITU07EMXF4k1n74ZnF9YrCQGPl+2z8Ekl23wWDPD45g1Uh+j78DcEI4yv3nc8cr9
nn7v4uMeR78xWVeKodTIMuLokRofwMGwExr8X+LyKibixUrp0Bu9OFhR9aTy9rV/
fpRX1i6zqD0kjviWa5dTUB0ENYowo1z9o6Sea4tnlZ+tENNV0+jFvJ4Nmdwicfls
OVRAkimj+rd65EJGPAhHzmYGpZb74CzSth+QZED9ExMJcVxwoa9aXNCZGeRiitbr
QUTXlIDpARay7onb/UJvMkcYuAIrz1c54ZbEJecJA/xkSUvbCg15QTGzcMsvaZkk
2S9I9Blp1WtpAUdtK5DKCEHC32PWRNrmY8d08lGMkZ9VoxRfAnw5wlyVoJQaU6n8
JGGe+6tYCFijcads4lACM+ZwbNSJihPOG8mqSIBVKV/DHR2JO+0n0qUcxzMt3+Dl
6h81lv5FLhqaiezitdLBveauxWzPlexcLxmBunzhuNUP2aMjVtx21Wk+/KGC6nZO
0F/BxgcqWy4JLgkdhVryZxY+P/EC5ZwFqi3boKHcxVFezJ+o2TuS7sh6nJezB3Pi
Kiu2hN6Ku1PPX+2o3tUs1ifB/f1s+vwVw4UfS/4w0OV8nokb62OHUgacbAlgeW7V
8oXazq45+DktBxhFWuiOkortIJOE1u/hSt1Vd2h1NoDMgxJuBT41dbanjDzDqbQE
GUzhGttW4BAmCYLtj0YNYCokoJ8wTHqC6/EjoNAQEpi8JQ3kT3/9kO8=
-----END ENCRYPTED PRIVATE KEY-----
`

const encPKCS8PBKDF2SHA1AES128 = `-----BEGIN ENCRYPTED PRIVATE KEY-----
MIIFJzBRBgkqhkiG9w0BBQ0wRDAjBgkqhkiG9w0BBQwwFgQQM9EGKtUPmjPhayfL
+TgqFwICCAAwHQYJYIZIAWUDBAECBBCnxFwHaMwbJCIUnQbGbbwcBIIE0Il9zsI3
O8seNFGH+bSGPwvG89w+7x4E94PodxsOQkCvR7qZmqTiJZxNl6VRCOlJB93G4CET
WNvekQT220v4OGBMCdTuLAyzXdLmg2fddBsR1xxLkYsaj1GoMfJkffjg6pF/Nc3T
j5rndthUT0j8CTaZOh0SXBdZU8T/sRab90TA6Tz2ZfFkxDJ3qnpCELKUK0bLC/6T
NF9xUAPBMMoWvK+5V2fr+lqHxwq9s1x5auP9yCHJUL8PWDmQf0mzLX75ynYFDR9a
reVJERNSvtFgIG0YqZf1J+DPZma6Hze11wywgVPIJeB5I13lY1ZhIXjWlYgj/Ev7
X0R0JDbfdX05iHcNGDSa/Q+hsZfZY/0vs2nHFNOO/rIIKYu0CtWDlSfWkmY+aU64
4NePsKmC5A+E56qRUiCPxlhkwefksoyx6iXO9hpVnLhW2nWkdAwsfS+V8j6euw1Z
Dcx6AqpSKurhEe50rGCxE2Iqgux9ygUgb3E2YgIg5Q7adkEwvuGuZUPpKXQbLhIl
sWLlL2HpiNnq3KAxtrYwJGpE9zA3pDo14DoWVNCL0eG+mZirGZP+6mLhgIiYWMmT
TUI2sNdpfRRHdBtnItfswarAlW9JuupLPENuU+8SG0vtFXIfJugAHOP80JXyPJE7
AULdQSC3ymYcwpYYH5ios5VdPAB3tjYHDpjURRaZit4OYx9D1eqvDZsXZuJcpjr2
o1viRvvNEwwNONu4NRcTvWDKXpQOItnEMWCwsGBt45D6u/N54NhsGNZaefOQQ+HG
VOfiFyml1NCie7vC7MtF7KLy/zQLFdx5ZpxJp0cnMv/JjmG0Je+TGMJ0N1KkzF1y
zcAfYP/OYUbnS4rahBzRxu1XbHHhnVJQf7qM6bkKNNfNlfFiJhV7bVk465JAau8a
/hL7xjOc+15VQxKvX+Hbq3se4NARYcSwf9PX1GjbppBHwEVMG0GrMbD/jdQNRbND
/qwz8ScxHXU2ZISKBq96zJlroBrE7b+8NwTionJhNBatHcGDyTWq+IFIBnBC6JeG
dZgIK1uSljaW8c0t3lKoBuGEulW2o+AfdhFtulIrRjtyWBZ13eA3tLS2PICtPfBW
RRBQB4C52RF6QVKmTrvEjk7+0gEj7/qJ3M6UHaejbd/3+/1Be1vlZ1AXcMJtiAvI
UnbPjfnCV/hkfu3AZIETHfgPVmO48v78QsbOHI/JReje7qLAhZ/qgltFfkmY+e2m
2ECg+xY4+Dt6pafQiBsYc0d/DMwRBK2dX+CeoeDQW2dRzijQgXw1tHA7kc9t0bN0
e2oh6vaz8x14IpW9WMetwFzI2O+nMK63HWTN/XBtKjymIyQvg0IRcoH2DV9pGHdX
HsfbdAcdq6BJggak/jAar3YQ2vtcoQB8hsiPNkUtrjyTS3I/w+amZy2ww3KSKixy
AKG0jGeNOYRtZ2hsnReekzev2Xrq33+ymboAH+20pOxDaFI3PRl2Lh+beDOIDPpJ
2jJnRClf1qkHXa+/qeNX6l6JvHsT3VlFLN/eFwfSOADJaQjiovcXuHQ+baUqBeIe
27MCG44tuSkuXPpxiwyv5RdQra+ivyiwj5Ygp7SxFxjyVfGt6ByWqzdLhV10dTTc
b5zg3K3BypaYvJbxkKA/2dnfqeZBWoqx9uRV
-----END ENCRYPTED PRIVATE KEY-----
`

// TestParseEncryptedPKCS8 covers Box's box_config.json key format: an encrypted
// PKCS#8 (PBES2) RSA key. Both the modern (PBKDF2-HMAC-SHA256 + AES-256-CBC) and
// a default-PRF (HMAC-SHA1 + AES-128-CBC) scheme decrypt to a usable RSA key.
func TestParseEncryptedPKCS8(t *testing.T) {
	cases := []struct {
		name, keyPEM, passphrase string
	}{
		{"pbkdf2-sha256-aes256", encPKCS8PBKDF2SHA256AES256, "testpass123"},
		{"pbkdf2-sha1-aes128", encPKCS8PBKDF2SHA1AES128, "pw2"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			key, err := parseRSAPrivateKey([]byte(tc.keyPEM), tc.passphrase)
			if err != nil {
				t.Fatalf("parsing encrypted PKCS#8 key: %v", err)
			}
			if err := key.Validate(); err != nil {
				t.Fatalf("decrypted RSA key is invalid: %v", err)
			}
			// The decrypted key actually signs (RS256), the JWT flow's real use.
			digest := sha256.Sum256([]byte("box-gantry"))
			sig, err := rsa.SignPKCS1v15(rand.Reader, key, crypto.SHA256, digest[:])
			if err != nil {
				t.Fatalf("signing with the decrypted key: %v", err)
			}
			if err := rsa.VerifyPKCS1v15(&key.PublicKey, crypto.SHA256, digest[:], sig); err != nil {
				t.Fatalf("signature does not verify: %v", err)
			}
		})
	}
}

// TestParseEncryptedPKCS8WrongPassphrase confirms a wrong passphrase fails
// loudly (bad PKCS#7 padding or a garbage PKCS#8 parse) rather than silently
// yielding a wrong key.
func TestParseEncryptedPKCS8WrongPassphrase(t *testing.T) {
	if _, err := parseRSAPrivateKey([]byte(encPKCS8PBKDF2SHA256AES256), "not-the-passphrase"); err == nil {
		t.Fatal("expected a wrong passphrase to fail")
	}
}

// TestParseEncryptedPKCS8NoPassphrase confirms an encrypted key with no
// passphrase is rejected at parse time, not mis-parsed as plaintext.
func TestParseEncryptedPKCS8NoPassphrase(t *testing.T) {
	if _, err := parseRSAPrivateKey([]byte(encPKCS8PBKDF2SHA256AES256), ""); err == nil {
		t.Fatal("expected an encrypted key with no passphrase to fail")
	}
}
