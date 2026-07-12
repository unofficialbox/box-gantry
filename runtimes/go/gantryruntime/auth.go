// Package-level auth: the four Box auth flows, each a TokenSource passed
// to New. DeveloperToken is a fixed token; ClientCredentials (CCG), JWT,
// and OAuth exchange credentials at Box's token endpoint and cache the
// resulting access token until shortly before it expires (TR-Go.7).
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
	"errors"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"
)

// defaultTokenURL is Box's OAuth 2.0 token endpoint, shared by every
// exchange flow. Override per config for custom deployments.
const defaultTokenURL = "https://api.box.com/oauth2/token"

// authorizeURL is where a user is sent to grant an OAuth 2.0 app access.
const authorizeURL = "https://account.box.com/api/oauth2/authorize"

// refreshMargin refreshes a cached token this long before it expires, so
// in-flight requests never race an expiry.
const refreshMargin = 60 * time.Second

// DeveloperToken is the simplest auth flow: a fixed access token from the
// Box developer console. The other three flows (CCG, JWT, OAuth 2.0
// authorization code) implement the same TokenSource interface and can be
// passed to New in its place.
func DeveloperToken(token string) TokenSource { return developerToken(token) }

type developerToken string

func (t developerToken) AccessToken(context.Context) (string, error) { return string(t), nil }

// cachedToken caches an access token behind a mutex and refreshes it via
// refresh when it is missing or within refreshMargin of expiry. The three
// exchange flows embed one and supply the flow-specific refresh.
type cachedToken struct {
	mu      sync.Mutex
	token   string
	expiry  time.Time
	refresh func(ctx context.Context) (token string, ttl time.Duration, err error)
}

func (c *cachedToken) AccessToken(ctx context.Context) (string, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.token != "" && time.Until(c.expiry) > refreshMargin {
		return c.token, nil
	}
	token, ttl, err := c.refresh(ctx)
	if err != nil {
		return "", err
	}
	c.token, c.expiry = token, time.Now().Add(ttl)
	return c.token, nil
}

// tokenResponse is the subset of the token endpoint's JSON we consume.
type tokenResponse struct {
	AccessToken  string `json:"access_token"`
	RefreshToken string `json:"refresh_token"`
	ExpiresIn    int64  `json:"expires_in"`
}

func (r tokenResponse) ttl() time.Duration { return time.Duration(r.ExpiresIn) * time.Second }

// postTokenForm posts a form-encoded grant to the token endpoint and
// decodes the response, surfacing a non-2xx body as the error.
func postTokenForm(ctx context.Context, httpc *http.Client, tokenURL string, form url.Values) (tokenResponse, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, tokenURL, strings.NewReader(form.Encode()))
	if err != nil {
		return tokenResponse{}, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Accept", "application/json")
	resp, err := httpc.Do(req)
	if err != nil {
		return tokenResponse{}, err
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return tokenResponse{}, err
	}
	if resp.StatusCode != http.StatusOK {
		return tokenResponse{}, fmt.Errorf("gantryruntime: token endpoint returned %d: %s",
			resp.StatusCode, strings.TrimSpace(string(body)))
	}
	var tr tokenResponse
	if err := json.Unmarshal(body, &tr); err != nil {
		return tokenResponse{}, fmt.Errorf("gantryruntime: decoding token response: %w", err)
	}
	if tr.AccessToken == "" {
		return tokenResponse{}, errors.New("gantryruntime: token endpoint returned no access_token")
	}
	return tr, nil
}

func authHTTPClient(c *http.Client) *http.Client {
	if c != nil {
		return c
	}
	return &http.Client{Timeout: 30 * time.Second}
}

func orDefault(v, fallback string) string {
	if v == "" {
		return fallback
	}
	return v
}

// CCGConfig configures the Client Credentials Grant: server-to-server auth
// with no signing key. Set exactly one subject — EnterpriseID for the
// service account, or UserID to act as a managed user.
type CCGConfig struct {
	ClientID     string
	ClientSecret string
	EnterpriseID string
	UserID       string
	TokenURL     string       // optional; defaults to Box's token endpoint
	HTTPClient   *http.Client // optional
}

// ClientCredentials builds a CCG TokenSource.
func ClientCredentials(cfg CCGConfig) TokenSource {
	httpc := authHTTPClient(cfg.HTTPClient)
	tokenURL := orDefault(cfg.TokenURL, defaultTokenURL)
	subjectType, subjectID := "enterprise", cfg.EnterpriseID
	if cfg.UserID != "" {
		subjectType, subjectID = "user", cfg.UserID
	}
	ct := &cachedToken{}
	ct.refresh = func(ctx context.Context) (string, time.Duration, error) {
		tr, err := postTokenForm(ctx, httpc, tokenURL, url.Values{
			"grant_type":       {"client_credentials"},
			"client_id":        {cfg.ClientID},
			"client_secret":    {cfg.ClientSecret},
			"box_subject_type": {subjectType},
			"box_subject_id":   {subjectID},
		})
		if err != nil {
			return "", 0, err
		}
		return tr.AccessToken, tr.ttl(), nil
	}
	return ct
}

// OAuthConfig configures the OAuth 2.0 authorization-code flow (a user
// grants a Box app access). Use AuthorizeURL to build the redirect,
// ExchangeCode to turn the returned code into a TokenSource, or OAuth to
// resume from a stored refresh token.
type OAuthConfig struct {
	ClientID     string
	ClientSecret string
	TokenURL     string       // optional; defaults to Box's token endpoint
	HTTPClient   *http.Client // optional
}

// AuthorizeURL builds the URL to redirect a user to so they can grant
// access. state is echoed back to the redirect URI for CSRF protection.
func (cfg OAuthConfig) AuthorizeURL(redirectURI, state string) string {
	q := url.Values{
		"response_type": {"code"},
		"client_id":     {cfg.ClientID},
		"redirect_uri":  {redirectURI},
		"state":         {state},
	}
	return authorizeURL + "?" + q.Encode()
}

// ExchangeCode exchanges an authorization code for a TokenSource that
// refreshes itself thereafter.
func (cfg OAuthConfig) ExchangeCode(ctx context.Context, code, redirectURI string) (TokenSource, error) {
	httpc := authHTTPClient(cfg.HTTPClient)
	tokenURL := orDefault(cfg.TokenURL, defaultTokenURL)
	tr, err := postTokenForm(ctx, httpc, tokenURL, url.Values{
		"grant_type":    {"authorization_code"},
		"code":          {code},
		"client_id":     {cfg.ClientID},
		"client_secret": {cfg.ClientSecret},
		"redirect_uri":  {redirectURI},
	})
	if err != nil {
		return nil, err
	}
	if tr.RefreshToken == "" {
		return nil, errors.New("gantryruntime: authorization-code exchange returned no refresh_token")
	}
	s := newOAuthSource(cfg, httpc, tokenURL, tr.RefreshToken)
	s.token, s.expiry = tr.AccessToken, time.Now().Add(tr.ttl())
	return s, nil
}

// OAuth resumes the authorization-code flow from a previously stored
// refresh token, exchanging it for access tokens as needed.
func OAuth(cfg OAuthConfig, refreshToken string) TokenSource {
	return newOAuthSource(cfg, authHTTPClient(cfg.HTTPClient), orDefault(cfg.TokenURL, defaultTokenURL), refreshToken)
}

// oauthSource refreshes access tokens with a refresh token and rotates the
// refresh token Box returns (Box invalidates the old one each exchange).
type oauthSource struct {
	mu           sync.Mutex
	httpc        *http.Client
	tokenURL     string
	clientID     string
	clientSecret string
	token        string
	expiry       time.Time
	refreshToken string
}

func newOAuthSource(cfg OAuthConfig, httpc *http.Client, tokenURL, refreshToken string) *oauthSource {
	return &oauthSource{
		httpc:        httpc,
		tokenURL:     tokenURL,
		clientID:     cfg.ClientID,
		clientSecret: cfg.ClientSecret,
		refreshToken: refreshToken,
	}
}

func (s *oauthSource) AccessToken(ctx context.Context) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.token != "" && time.Until(s.expiry) > refreshMargin {
		return s.token, nil
	}
	tr, err := postTokenForm(ctx, s.httpc, s.tokenURL, url.Values{
		"grant_type":    {"refresh_token"},
		"refresh_token": {s.refreshToken},
		"client_id":     {s.clientID},
		"client_secret": {s.clientSecret},
	})
	if err != nil {
		return "", err
	}
	s.token, s.expiry = tr.AccessToken, time.Now().Add(tr.ttl())
	if tr.RefreshToken != "" {
		s.refreshToken = tr.RefreshToken
	}
	return s.token, nil
}

// JWTConfig configures server auth with a signing key (Box's box_config.json
// fields). PrivateKeyPEM is the RSA private key, optionally encrypted with
// Passphrase. Set exactly one subject — EnterpriseID or UserID.
type JWTConfig struct {
	ClientID      string
	ClientSecret  string
	PublicKeyID   string
	PrivateKeyPEM []byte
	Passphrase    string
	EnterpriseID  string
	UserID        string
	TokenURL      string       // optional; defaults to Box's token endpoint
	HTTPClient    *http.Client // optional
}

// JWTAuth builds a JWT-assertion TokenSource, parsing (and if needed
// decrypting) the RSA private key up front so a bad key fails loudly at
// construction rather than on the first request.
func JWTAuth(cfg JWTConfig) (TokenSource, error) {
	key, err := parseRSAPrivateKey(cfg.PrivateKeyPEM, cfg.Passphrase)
	if err != nil {
		return nil, err
	}
	httpc := authHTTPClient(cfg.HTTPClient)
	tokenURL := orDefault(cfg.TokenURL, defaultTokenURL)
	subjectType, subjectID := "enterprise", cfg.EnterpriseID
	if cfg.UserID != "" {
		subjectType, subjectID = "user", cfg.UserID
	}
	ct := &cachedToken{}
	ct.refresh = func(ctx context.Context) (string, time.Duration, error) {
		assertion, err := signAssertion(cfg, key, subjectType, subjectID, tokenURL)
		if err != nil {
			return "", 0, err
		}
		tr, err := postTokenForm(ctx, httpc, tokenURL, url.Values{
			"grant_type":    {"urn:ietf:params:oauth:grant-type:jwt-bearer"},
			"assertion":     {assertion},
			"client_id":     {cfg.ClientID},
			"client_secret": {cfg.ClientSecret},
		})
		if err != nil {
			return "", 0, err
		}
		return tr.AccessToken, tr.ttl(), nil
	}
	return ct, nil
}

// parseRSAPrivateKey decodes a PEM RSA key, decrypting a legacy
// passphrase-protected block (Box's box_config keys) when present, and
// accepts either PKCS#1 or PKCS#8 encoding.
func parseRSAPrivateKey(pemBytes []byte, passphrase string) (*rsa.PrivateKey, error) {
	block, _ := pem.Decode(pemBytes)
	if block == nil {
		return nil, errors.New("gantryruntime: no PEM block in the private key")
	}
	der := block.Bytes
	//nolint:staticcheck // legacy encrypted PEM is exactly Box's key format
	if x509.IsEncryptedPEMBlock(block) {
		if passphrase == "" {
			return nil, errors.New("gantryruntime: private key is encrypted but no passphrase was given")
		}
		decrypted, err := x509.DecryptPEMBlock(block, []byte(passphrase))
		if err != nil {
			return nil, fmt.Errorf("gantryruntime: decrypting private key: %w", err)
		}
		der = decrypted
	}
	if key, err := x509.ParsePKCS1PrivateKey(der); err == nil {
		return key, nil
	}
	parsed, err := x509.ParsePKCS8PrivateKey(der)
	if err != nil {
		return nil, fmt.Errorf("gantryruntime: parsing private key: %w", err)
	}
	key, ok := parsed.(*rsa.PrivateKey)
	if !ok {
		return nil, errors.New("gantryruntime: private key is not RSA")
	}
	return key, nil
}

// signAssertion builds and RS256-signs the JWT bearer assertion Box's
// jwt-bearer grant expects. The claim set is single-use: a random jti and
// a short (45s) expiry.
func signAssertion(cfg JWTConfig, key *rsa.PrivateKey, subjectType, subjectID, audience string) (string, error) {
	jti, err := randomID()
	if err != nil {
		return "", err
	}
	header := map[string]string{"alg": "RS256", "typ": "JWT", "kid": cfg.PublicKeyID}
	claims := map[string]any{
		"iss":          cfg.ClientID,
		"sub":          subjectID,
		"box_sub_type": subjectType,
		"aud":          audience,
		"jti":          jti,
		"exp":          time.Now().Add(45 * time.Second).Unix(),
	}
	headerJSON, err := json.Marshal(header)
	if err != nil {
		return "", err
	}
	claimsJSON, err := json.Marshal(claims)
	if err != nil {
		return "", err
	}
	signingInput := b64(headerJSON) + "." + b64(claimsJSON)
	digest := sha256.Sum256([]byte(signingInput))
	sig, err := rsa.SignPKCS1v15(rand.Reader, key, crypto.SHA256, digest[:])
	if err != nil {
		return "", fmt.Errorf("gantryruntime: signing assertion: %w", err)
	}
	return signingInput + "." + b64(sig), nil
}

func b64(b []byte) string { return base64.RawURLEncoding.EncodeToString(b) }

// randomID returns a 128-bit random hex string for the assertion jti.
func randomID() (string, error) {
	n, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 128))
	if err != nil {
		return "", err
	}
	return n.Text(16), nil
}
