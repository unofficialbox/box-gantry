//go:build live

// VR-7 live smoke: exercise the runtime against a real Box account — one
// call per configured auth flow, plus paginate + upload + download +
// delete. Build-tagged `live` so the standard CI gate never compiles or
// runs it (no credentials there); run it on demand with credentials:
//
//	go test -tags live -run TestLiveSmoke ./gantryruntime/...
//
// It drives only the stable runtime contract (New / NewRequest / Fetch /
// the With* builders / response accessors), so it is independent of any
// generated method names — it verifies the hand-written runtime, which is
// the part a compile check cannot exercise.
package gantryruntime_test

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"strings"
	"testing"
	"time"

	"boxgantry.invalid/boxsdk/gantryruntime"
)

// authSources builds every auth flow the environment configures. A flow is
// attempted only when its variables are present, so a token-only setup
// still runs the whole smoke.
func authSources(t *testing.T) map[string]gantryruntime.TokenSource {
	t.Helper()
	sources := map[string]gantryruntime.TokenSource{}

	if token := os.Getenv("BOX_DEVELOPER_TOKEN"); token != "" {
		sources["developer"] = gantryruntime.DeveloperToken(token)
	}
	if id := os.Getenv("BOX_CLIENT_ID"); id != "" && os.Getenv("BOX_CLIENT_SECRET") != "" {
		if ent := os.Getenv("BOX_ENTERPRISE_ID"); ent != "" {
			sources["ccg"] = gantryruntime.ClientCredentials(gantryruntime.CCGConfig{
				ClientID:     id,
				ClientSecret: os.Getenv("BOX_CLIENT_SECRET"),
				EnterpriseID: ent,
			})
		}
		if rt := os.Getenv("BOX_OAUTH_REFRESH_TOKEN"); rt != "" {
			sources["oauth"] = gantryruntime.OAuth(gantryruntime.OAuthConfig{
				ClientID:     id,
				ClientSecret: os.Getenv("BOX_CLIENT_SECRET"),
			}, rt)
		}
	}
	if path := os.Getenv("BOX_JWT_CONFIG"); path != "" {
		sources["jwt"] = jwtSource(t, path)
	}
	return sources
}

// jwtSource builds a JWT flow from a Box box_config.json file.
func jwtSource(t *testing.T, path string) gantryruntime.TokenSource {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("reading BOX_JWT_CONFIG: %v", err)
	}
	var cfg struct {
		BoxAppSettings struct {
			ClientID     string `json:"clientID"`
			ClientSecret string `json:"clientSecret"`
			AppAuth      struct {
				PublicKeyID string `json:"publicKeyID"`
				PrivateKey  string `json:"privateKey"`
				Passphrase  string `json:"passphrase"`
			} `json:"appAuth"`
		} `json:"boxAppSettings"`
		EnterpriseID string `json:"enterpriseID"`
	}
	if err := json.Unmarshal(raw, &cfg); err != nil {
		t.Fatalf("parsing BOX_JWT_CONFIG: %v", err)
	}
	src, err := gantryruntime.JWTAuth(gantryruntime.JWTConfig{
		ClientID:      cfg.BoxAppSettings.ClientID,
		ClientSecret:  cfg.BoxAppSettings.ClientSecret,
		PublicKeyID:   cfg.BoxAppSettings.AppAuth.PublicKeyID,
		PrivateKeyPEM: []byte(cfg.BoxAppSettings.AppAuth.PrivateKey),
		Passphrase:    cfg.BoxAppSettings.AppAuth.Passphrase,
		EnterpriseID:  cfg.EnterpriseID,
	})
	if err != nil {
		t.Fatalf("building JWT auth: %v", err)
	}
	return src
}

func TestLiveSmoke(t *testing.T) {
	sources := authSources(t)
	if len(sources) == 0 {
		t.Skip("VR-7: no Box credentials in the environment; skipping live smoke")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	// One authenticated call per auth flow: GET /users/me must return the
	// current user (proving the flow yields a usable token).
	var primary *gantryruntime.Client
	for name, source := range sources {
		client := gantryruntime.New(source)
		me := getJSON(ctx, t, client, client.BaseUrl("api")+"/users/me")
		if me["id"] == nil {
			t.Fatalf("%s: /users/me returned no id: %v", name, me)
		}
		t.Logf("%s auth: authenticated as user %v", name, me["id"])
		if primary == nil {
			primary = client
		}
	}

	// The remaining flows exercise one shared client end to end.
	smokePaginate(ctx, t, primary)
	smokeUploadDownloadDelete(ctx, t, primary)
}

// smokePaginate walks the root folder's items, following the marker cursor
// across pages just like the generated iterators do.
func smokePaginate(ctx context.Context, t *testing.T, client *gantryruntime.Client) {
	seen := 0
	marker := ""
	for pages := 0; pages < 100; pages++ {
		url := client.BaseUrl("api") + "/folders/0/items"
		req := gantryruntime.WithQuery(client.NewRequest("GET", url), "limit", "100")
		if marker != "" {
			req = gantryruntime.WithQuery(req, "marker", marker)
		}
		body := fetchOK(ctx, t, client, req)
		var page struct {
			Entries    []json.RawMessage `json:"entries"`
			NextMarker string            `json:"next_marker"`
		}
		if err := json.Unmarshal(body, &page); err != nil {
			t.Fatalf("paginate: decoding page: %v", err)
		}
		seen += len(page.Entries)
		if page.NextMarker == "" {
			break
		}
		marker = page.NextMarker
	}
	t.Logf("paginate: walked the root folder, %d item(s)", seen)
}

// smokeUploadDownloadDelete uploads a small file to the root folder,
// downloads it back byte-for-byte, then deletes it.
func smokeUploadDownloadDelete(ctx context.Context, t *testing.T, client *gantryruntime.Client) {
	content := []byte("box-gantry live smoke " + time.Now().UTC().Format(time.RFC3339Nano))
	name := "box-gantry-smoke.txt"
	attributes, _ := json.Marshal(map[string]any{
		"name":   name,
		"parent": map[string]string{"id": "0"},
	})

	// Upload (multipart to the upload host).
	upURL := client.BaseUrl("upload") + "/files/content"
	upReq := gantryruntime.WithMultipartBody(client.NewRequest("POST", upURL), attributes, name, bytes.NewReader(content))
	upBody := fetchOK(ctx, t, client, upReq)
	var uploaded struct {
		Entries []struct {
			ID string `json:"id"`
		} `json:"entries"`
	}
	if err := json.Unmarshal(upBody, &uploaded); err != nil || len(uploaded.Entries) == 0 {
		t.Fatalf("upload: unexpected response: %s", upBody)
	}
	fileID := uploaded.Entries[0].ID
	t.Logf("upload: created file %s", fileID)

	// Download and compare.
	dlURL := client.BaseUrl("api") + "/files/" + fileID + "/content"
	dlBody := fetchOK(ctx, t, client, client.NewRequest("GET", dlURL))
	if !bytes.Equal(dlBody, content) {
		t.Fatalf("download: content mismatch (got %d bytes, want %d)", len(dlBody), len(content))
	}
	t.Logf("download: content round-tripped")

	// Delete (best effort — always attempted so smoke runs leave no trail).
	delResp, err := client.Fetch(ctx, client.NewRequest("DELETE", client.BaseUrl("api")+"/files/"+fileID))
	if err != nil {
		t.Fatalf("delete: %v", err)
	}
	if code := gantryruntime.StatusCode(delResp); code != 204 {
		t.Fatalf("delete: expected 204, got %d", code)
	}
	t.Logf("delete: cleaned up file %s", fileID)
}

// getJSON fetches a URL and decodes a JSON object, failing on non-2xx.
func getJSON(ctx context.Context, t *testing.T, client *gantryruntime.Client, url string) map[string]any {
	t.Helper()
	body := fetchOK(ctx, t, client, client.NewRequest("GET", url))
	var out map[string]any
	if err := json.Unmarshal(body, &out); err != nil {
		t.Fatalf("decoding %s: %v", url, err)
	}
	return out
}

// fetchOK runs a request and returns the body, failing on transport error
// or a non-2xx status.
func fetchOK(ctx context.Context, t *testing.T, client *gantryruntime.Client, req *gantryruntime.Request) []byte {
	t.Helper()
	resp, err := client.Fetch(ctx, req)
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	code := gantryruntime.StatusCode(resp)
	body, err := gantryruntime.ResponseBytes(resp)
	if err != nil {
		t.Fatalf("reading body: %v", err)
	}
	if code < 200 || code >= 300 {
		t.Fatalf("unexpected status %d: %s", code, strings.TrimSpace(string(body)))
	}
	return body
}
