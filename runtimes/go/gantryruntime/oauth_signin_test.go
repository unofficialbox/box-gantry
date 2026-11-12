//go:build live

// One-time OAuth sign-in for the live smoke. It opens no browser and prints no
// token: it prints a Box consent URL, waits for the redirect on localhost, and
// writes the refresh token Box issues to BOX_OAUTH_REFRESH_TOKEN_FILE.
// scripts/oauth-signin.sh wraps it and stores the result as the
// BOX_OAUTH_REFRESH_TOKEN repo secret. It does nothing unless
// BOX_OAUTH_SIGNIN=1, so the live smoke's own run never starts it.
package gantryruntime_test

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"testing"
	"time"

	"boxgantry.invalid/boxsdk/auth"
	"boxgantry.invalid/boxsdk/gantryruntime"
)

func TestOAuthSignIn(t *testing.T) {
	loadDotenv(t)
	if os.Getenv("BOX_OAUTH_SIGNIN") != "1" {
		t.Skip("set BOX_OAUTH_SIGNIN=1 to sign in (scripts/oauth-signin.sh does)")
	}
	id, secret := os.Getenv("BOX_OAUTH_CLIENT_ID"), os.Getenv("BOX_OAUTH_CLIENT_SECRET")
	out := os.Getenv("BOX_OAUTH_REFRESH_TOKEN_FILE")
	if id == "" || secret == "" || out == "" {
		t.Fatal("BOX_OAUTH_CLIENT_ID, BOX_OAUTH_CLIENT_SECRET and BOX_OAUTH_REFRESH_TOKEN_FILE are required")
	}
	port := os.Getenv("BOX_OAUTH_REDIRECT_PORT")
	if port == "" {
		port = "8787"
	}
	redirect := "http://localhost:" + port + "/callback"

	raw := make([]byte, 16)
	if _, err := rand.Read(raw); err != nil {
		t.Fatal(err)
	}
	state := hex.EncodeToString(raw)

	listener, err := net.Listen("tcp", "127.0.0.1:"+port)
	if err != nil {
		t.Fatalf("listening on port %s: %v", port, err)
	}
	type result struct {
		code string
		err  error
	}
	done := make(chan result, 1)
	server := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/callback" {
			http.NotFound(w, r)
			return
		}
		q := r.URL.Query()
		switch {
		case q.Get("state") != state:
			http.Error(w, "state mismatch", http.StatusBadRequest)
			done <- result{err: errors.New("state mismatch on the redirect")}
		case q.Get("code") == "":
			http.Error(w, "no code", http.StatusBadRequest)
			done <- result{err: fmt.Errorf("no code on the redirect (error=%q)", q.Get("error"))}
		default:
			fmt.Fprintln(w, "Signed in. You can close this tab.")
			done <- result{code: q.Get("code")}
		}
	})}
	go server.Serve(listener)
	defer server.Close()

	cfg := auth.OAuthConfig{ClientID: id, ClientSecret: secret, Store: fileTokenStore{path: out}}
	fmt.Printf("\nOpen this URL, sign in as the Box test user, and grant access:\n\n  %s\n\nWaiting for the redirect to %s ...\n", cfg.AuthorizeURL(redirect, state), redirect)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	var res result
	select {
	case res = <-done:
	case <-ctx.Done():
		t.Fatal("timed out waiting for the sign-in")
	}
	if res.err != nil {
		t.Fatal(res.err)
	}
	src, err := cfg.ExchangeCode(ctx, res.code, redirect)
	if err != nil {
		t.Fatalf("exchanging the code: %v", err)
	}
	client := gantryruntime.New(src)
	me := getJSON(ctx, t, client, client.BaseUrl("api")+"/users/me")
	fmt.Printf("Signed in as %v (%v); refresh token written to %s\n", me["login"], me["id"], out)
}
