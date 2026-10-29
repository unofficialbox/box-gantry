//go:build live

// VR-7 live smoke: exercise the runtime against a real Box account — one
// call per configured auth flow, plus paginate + upload + download +
// delete, with the etag flow (304 / 412) on the uploaded file. Build-tagged
// `live` so the standard CI gate never runs it (no credentials there) — CI
// only vets it (`go vet -tags live`) so it can't rot.
// Run it on demand with credentials:
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
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"boxgantry.invalid/boxsdk/auth"
	"boxgantry.invalid/boxsdk/gantryruntime"
)

// loadDotenv loads KEY=VALUE lines from a .env file into the process
// environment (for local runs — see .env.sample). It searches BOX_ENV_FILE,
// then walks up from the working directory so it works whether the test is
// run from the repo root or the package dir. Real environment variables
// always win; the .env only fills gaps. Dependency-free by design (the
// runtime ships zero external dependencies).
func loadDotenv(t *testing.T) {
	t.Helper()
	path := os.Getenv("BOX_ENV_FILE")
	if path == "" {
		dir, _ := os.Getwd()
		for range 5 {
			candidate := filepath.Join(dir, ".env")
			if _, err := os.Stat(candidate); err == nil {
				path = candidate
				break
			}
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			dir = parent
		}
	}
	if path == "" {
		return
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return
	}
	for _, line := range strings.Split(string(data), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		key, value, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}
		key = strings.TrimSpace(key)
		value = strings.Trim(strings.TrimSpace(value), `"'`)
		if _, set := os.LookupEnv(key); !set {
			t.Setenv(key, value)
		}
	}
}

// authSources builds every auth flow the environment configures. A flow is
// attempted only when its variables are present, so a token-only setup
// still runs the whole smoke.
func authSources(t *testing.T) map[string]gantryruntime.TokenSource {
	t.Helper()
	loadDotenv(t)
	sources := map[string]gantryruntime.TokenSource{}

	if token := os.Getenv("BOX_DEVELOPER_TOKEN"); token != "" {
		sources["developer"] = auth.DeveloperToken(token)
	}
	if id := os.Getenv("BOX_CLIENT_ID"); id != "" && os.Getenv("BOX_CLIENT_SECRET") != "" {
		if ent := os.Getenv("BOX_ENTERPRISE_ID"); ent != "" {
			sources["ccg"] = auth.ClientCredentials(auth.CCGConfig{
				ClientID:     id,
				ClientSecret: os.Getenv("BOX_CLIENT_SECRET"),
				EnterpriseID: ent,
			})
		}
		if rt := os.Getenv("BOX_OAUTH_REFRESH_TOKEN"); rt != "" {
			sources["oauth"] = auth.OAuth(auth.OAuthConfig{
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
	src, err := auth.JWTAuth(auth.JWTConfig{
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
	// current user (proving the flow yields a usable token). Each flow is its
	// own subtest, so one bad credential (an expired developer token) fails
	// that flow without hiding the others' results.
	var primary *gantryruntime.Client
	names := make([]string, 0, len(sources))
	for name := range sources {
		names = append(names, name)
	}
	slices.Sort(names)
	for _, name := range names {
		client := gantryruntime.New(sources[name])
		t.Run(name, func(t *testing.T) {
			me := getJSON(ctx, t, client, client.BaseUrl("api")+"/users/me")
			if me["id"] == nil {
				t.Fatalf("/users/me returned no id: %v", me)
			}
			t.Logf("authenticated as user %v", me["id"])
			if primary == nil {
				primary = client
			}
		})
	}
	if primary == nil {
		t.Fatal("no auth flow produced a usable token")
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
	content := []byte("box-gantry live smoke")
	// Unique per run: Box rejects a duplicate name in the same folder (409), so
	// a leftover from a prior run would otherwise wedge the smoke.
	name := fmt.Sprintf("box-gantry-smoke-%d.txt", time.Now().UnixNano())
	attributes, _ := json.Marshal(map[string]any{
		"name":   name,
		"parent": map[string]string{"id": "0"},
	})

	// Upload (multipart to the upload host).
	upURL := client.BaseUrl("upload") + "/files/content"
	upReq := gantryruntime.WithMultipartBody(client.NewRequest("POST", upURL), "attributes", attributes, "file", bytes.NewReader(content))
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

	// Delete unconditionally once we hold the id, even if the download below
	// fails, so a smoke run never leaves an artifact in the account.
	defer func() {
		delResp, err := client.Fetch(ctx, client.NewRequest("DELETE", client.BaseUrl("api")+"/files/"+fileID))
		if err != nil {
			t.Errorf("delete: %v", err)
			return
		}
		if code := gantryruntime.StatusCode(delResp); code != 204 {
			t.Errorf("delete: expected 204, got %d", code)
			return
		}
		t.Logf("delete: cleaned up file %s", fileID)
	}()

	// Download and compare.
	dlURL := client.BaseUrl("api") + "/files/" + fileID + "/content"
	dlBody := fetchOK(ctx, t, client, client.NewRequest("GET", dlURL))
	if !bytes.Equal(dlBody, content) {
		t.Fatalf("download: content mismatch (got %d bytes, want %d)", len(dlBody), len(content))
	}
	t.Logf("download: content round-tripped")

	smokeEtags(ctx, t, client, fileID)
	smokeEvents(t, client, fileID)
}

// smokeEvents follows Box's long-poll event stream from "now" while the
// uploaded file is renamed, and must see that rename (ITEM_RENAME) within a
// minute: the long poll reports the change, and the page after it carries it.
func smokeEvents(t *testing.T, client *gantryruntime.Client, fileID string) {
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	api := client.BaseUrl("api")
	type event struct {
		ID     string `json:"event_id"`
		Type   string `json:"event_type"`
		Source struct {
			ID string `json:"id"`
		} `json:"source"`
	}
	source := gantryruntime.EventSource[event]{
		Server: func(ctx context.Context) (gantryruntime.LongPollServer, error) {
			resp, err := client.Fetch(ctx, client.NewRequest("OPTIONS", api+"/events"))
			if err != nil {
				return gantryruntime.LongPollServer{}, err
			}
			var listed struct {
				Entries []struct {
					URL          string `json:"url"`
					RetryTimeout int64  `json:"retry_timeout"`
				} `json:"entries"`
			}
			body, _ := gantryruntime.ResponseBytes(resp)
			if err := json.Unmarshal(body, &listed); err != nil || len(listed.Entries) == 0 {
				return gantryruntime.LongPollServer{}, fmt.Errorf("no real-time server in %s", body)
			}
			return gantryruntime.LongPollServer{
				URL:          listed.Entries[0].URL,
				RetryTimeout: time.Duration(listed.Entries[0].RetryTimeout) * time.Second,
			}, nil
		},
		Page: func(ctx context.Context, position string) ([]event, string, error) {
			req := gantryruntime.WithQuery(client.NewRequest("GET", api+"/events"), "stream_position", position)
			resp, err := client.Fetch(ctx, req)
			if err != nil {
				return nil, "", err
			}
			var page struct {
				Entries []event         `json:"entries"`
				Next    json.RawMessage `json:"next_stream_position"`
			}
			body, _ := gantryruntime.ResponseBytes(resp)
			if err := json.Unmarshal(body, &page); err != nil {
				return nil, "", err
			}
			return page.Entries, gantryruntime.StreamPosition(page.Next), nil
		},
		ID: func(e event) string { return e.ID },
	}

	// Rename the file once the stream is waiting, so the long poll is what
	// reports it.
	rename := time.AfterFunc(3*time.Second, func() {
		name, _ := json.Marshal(map[string]string{"name": fmt.Sprintf("box-gantry-smoke-renamed-%d.txt", time.Now().UnixNano())})
		req := gantryruntime.WithJsonBody(client.NewRequest("PUT", api+"/files/"+fileID), name)
		if _, err := client.Fetch(ctx, req); err != nil {
			t.Errorf("events: renaming the file: %v", err)
		}
	})
	defer rename.Stop()
	for e, err := range gantryruntime.EventStream(ctx, client, "now", source) {
		if err != nil {
			t.Fatalf("events: no ITEM_RENAME for file %s from the long poll: %v", fileID, err)
		}
		if e.Type == "ITEM_RENAME" && e.Source.ID == fileID {
			t.Logf("events: the long poll reported %s for file %s", e.Type, fileID)
			return
		}
	}
}

// smokeEtags drives Box's etag flow on the uploaded file, as the generated
// …IfModified variants and the errors guide's re-read-and-retry loop expect
// it: If-None-Match with the current etag is a 304 and a stale one reads the
// file; If-Match with a stale etag is a 412 and the current one writes.
func smokeEtags(ctx context.Context, t *testing.T, client *gantryruntime.Client, fileID string) {
	url := client.BaseUrl("api") + "/files/" + fileID
	var file struct {
		Etag string `json:"etag"`
	}
	if err := json.Unmarshal(fetchOK(ctx, t, client, client.NewRequest("GET", url)), &file); err != nil || file.Etag == "" {
		t.Fatalf("etag: the file read yields no etag (%v)", err)
	}
	// Box's etags are version counters, so this one is well-formed but stale.
	stale := "9" + file.Etag

	read := func(etag string) error {
		_, err := client.Fetch(ctx, gantryruntime.WithHeader(client.NewRequest("GET", url), "If-None-Match", etag))
		return err
	}
	if err := read(file.Etag); !gantryruntime.IsNotModified(err) {
		t.Fatalf("etag: If-None-Match with the current etag: err = %v, want a 304", err)
	}
	if err := read(stale); err != nil {
		t.Fatalf("etag: If-None-Match with a stale etag: %v", err)
	}

	description, _ := json.Marshal(map[string]string{"description": "box-gantry etag smoke"})
	write := func(etag string) error {
		req := gantryruntime.WithJsonBody(client.NewRequest("PUT", url), description)
		_, err := client.Fetch(ctx, gantryruntime.WithHeader(req, "If-Match", etag))
		return err
	}
	if err := write(stale); !gantryruntime.IsPreconditionFailed(err) {
		t.Fatalf("etag: If-Match with a stale etag: err = %v, want a 412", err)
	}
	if err := write(file.Etag); err != nil {
		t.Fatalf("etag: If-Match with the current etag: %v", err)
	}
	t.Logf("etag: 304 for the current etag, 412 for a stale If-Match, the current one writes")
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
