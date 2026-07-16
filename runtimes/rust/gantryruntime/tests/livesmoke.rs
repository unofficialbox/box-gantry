//! VR-7 live smoke: exercise the runtime against a real Box account — one
//! authenticated call per configured auth flow, plus paginate + upload +
//! download + delete. `#[ignore]`d so the standard `cargo test` gate never runs
//! it (no credentials there), yet it still *compiles* under the gate so it can't
//! rot. Run it on demand with credentials:
//!
//! ```sh
//! cargo test -p gantryruntime --test livesmoke -- --ignored --nocapture
//! ```
//!
//! It drives only the stable runtime contract (`Client::new` / `new_request` /
//! `fetch` / the `with_*` builders / response accessors), so it is independent
//! of any generated method names — it verifies the hand-written runtime, which
//! is the part a compile check cannot exercise. With no credentials set it
//! returns early (a clean no-op), like the Go smoke's `t.Skip`.

use std::collections::HashMap;
use std::path::PathBuf;

use gantryruntime::{
    response_bytes, status_code, with_multipart_body, with_query, Auth, CcgConfig, Client,
    JwtConfig, OAuthConfig, Stream,
};

/// The process environment, with any `.env` file filling gaps (real env vars
/// win). Dependency-free by design — the runtime ships zero external deps for
/// this. Searches `BOX_ENV_FILE`, then walks up from the crate dir.
fn env_map() -> HashMap<String, String> {
    let mut map: HashMap<String, String> = std::env::vars().collect();
    let path = std::env::var("BOX_ENV_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            for _ in 0..6 {
                let candidate = dir.join(".env");
                if candidate.is_file() {
                    return Some(candidate);
                }
                if !dir.pop() {
                    break;
                }
            }
            None
        });
    if let Some(path) = path {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim().to_string();
                    let value = value.trim().trim_matches(['"', '\'']).to_string();
                    map.entry(key).or_insert(value);
                }
            }
        }
    }
    map
}

fn nonempty(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key).filter(|v| !v.is_empty()).cloned()
}

/// Every auth flow the environment configures, by name. A flow is built only
/// when its variables are present, so a token-only setup still runs the smoke.
fn auth_sources(env: &HashMap<String, String>) -> Vec<(&'static str, Auth)> {
    let mut sources = Vec::new();

    if let Some(token) = nonempty(env, "BOX_DEVELOPER_TOKEN") {
        sources.push(("developer", Auth::developer_token(token)));
    }
    if let (Some(id), Some(secret)) = (
        nonempty(env, "BOX_CLIENT_ID"),
        nonempty(env, "BOX_CLIENT_SECRET"),
    ) {
        if let Some(enterprise_id) = nonempty(env, "BOX_ENTERPRISE_ID") {
            sources.push((
                "ccg",
                Auth::client_credentials(CcgConfig {
                    client_id: id.clone(),
                    client_secret: secret.clone(),
                    enterprise_id,
                    user_id: None,
                    token_url: None,
                }),
            ));
        }
        if let Some(refresh) = nonempty(env, "BOX_OAUTH_REFRESH_TOKEN") {
            sources.push((
                "oauth",
                Auth::oauth(
                    OAuthConfig {
                        client_id: id,
                        client_secret: secret,
                        token_url: None,
                    },
                    refresh,
                ),
            ));
        }
    }
    if let Some(path) = nonempty(env, "BOX_JWT_CONFIG") {
        sources.push(("jwt", jwt_source(&path)));
    }
    sources
}

/// Build a JWT flow from a Box `box_config.json` file.
fn jwt_source(path: &str) -> Auth {
    let raw = std::fs::read_to_string(path).expect("reading BOX_JWT_CONFIG");
    let cfg: serde_json::Value = serde_json::from_str(&raw).expect("parsing BOX_JWT_CONFIG");
    let app = &cfg["boxAppSettings"];
    let app_auth = &app["appAuth"];
    let field = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    Auth::jwt(JwtConfig {
        client_id: field(&app["clientID"]),
        client_secret: field(&app["clientSecret"]),
        public_key_id: field(&app_auth["publicKeyID"]),
        private_key_pem: field(&app_auth["privateKey"]).into_bytes(),
        passphrase: app_auth["passphrase"].as_str().map(|s| s.to_string()),
        enterprise_id: field(&cfg["enterpriseID"]),
        user_id: None,
        token_url: None,
    })
    .expect("building JWT auth (bad box_config key?)")
}

#[tokio::test]
#[ignore = "VR-7: needs real Box credentials; run with --ignored"]
async fn live_smoke() {
    let env = env_map();
    let sources = auth_sources(&env);
    if sources.is_empty() {
        eprintln!("VR-7: no Box credentials in the environment; skipping live smoke");
        return;
    }

    // One authenticated call per auth flow: GET /users/me must return the
    // current user, proving the flow yields a usable token.
    let mut primary: Option<Client> = None;
    for (name, auth) in sources {
        let client = Client::new(auth);
        let me = get_json(&client, &format!("{}/users/me", client.base_url("api"))).await;
        assert!(
            !me["id"].is_null(),
            "{name}: /users/me returned no id: {me}"
        );
        eprintln!("{name} auth: authenticated as user {}", me["id"]);
        primary.get_or_insert(client);
    }

    let client = primary.expect("at least one flow");
    smoke_paginate(&client).await;
    smoke_upload_download_delete(&client).await;
}

/// Walk the root folder's items, following the marker cursor across pages just
/// like the generated iterators do.
async fn smoke_paginate(client: &Client) {
    let mut seen = 0usize;
    let mut marker = String::new();
    for _ in 0..100 {
        let url = format!("{}/folders/0/items", client.base_url("api"));
        let mut req = with_query(client.new_request("GET", &url), "limit", "100");
        if !marker.is_empty() {
            req = with_query(req, "marker", &marker);
        }
        let body = fetch_ok(client, req).await;
        let page: serde_json::Value = serde_json::from_slice(&body).expect("decoding page");
        seen += page["entries"].as_array().map(|e| e.len()).unwrap_or(0);
        match page["next_marker"].as_str() {
            Some(next) if !next.is_empty() => marker = next.to_string(),
            _ => break,
        }
    }
    eprintln!("paginate: walked the root folder, {seen} item(s)");
}

/// Upload a small file to the root folder, download it back byte-for-byte, then
/// delete it.
async fn smoke_upload_download_delete(client: &Client) {
    let content = b"box-gantry live smoke".to_vec();
    // Unique per run: Box rejects a duplicate name in the same folder (409), so
    // a leftover from a prior run would otherwise wedge the smoke.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("box-gantry-smoke-{nanos}.txt");
    let attributes = serde_json::to_vec(&serde_json::json!({
        "name": name,
        "parent": { "id": "0" },
    }))
    .unwrap();

    // Upload (multipart to the upload host).
    let up_url = format!("{}/files/content", client.base_url("upload"));
    let up_req = with_multipart_body(
        client.new_request("POST", &up_url),
        &attributes,
        &name,
        Stream::from_bytes(content.clone()),
    );
    let up_body = fetch_ok(client, up_req).await;
    let uploaded: serde_json::Value = serde_json::from_slice(&up_body).expect("upload response");
    let file_id = uploaded["entries"][0]["id"]
        .as_str()
        .expect("upload: no file id")
        .to_string();
    eprintln!("upload: created file {file_id}");

    // Download, capturing the result *without* asserting yet — the file must be
    // deleted even if the download fails, so a smoke run never leaves an
    // artifact in the account.
    let dl_url = format!("{}/files/{file_id}/content", client.base_url("api"));
    let download = client.fetch(client.new_request("GET", &dl_url)).await;

    // Delete unconditionally now that we hold the id.
    let del_url = format!("{}/files/{file_id}", client.base_url("api"));
    let deleted = client.fetch(client.new_request("DELETE", &del_url)).await;

    // Now surface any download failure and compare bytes.
    let dl_resp = download.expect("download request");
    assert!(
        (200..300).contains(&status_code(&dl_resp)),
        "download: unexpected status {}",
        status_code(&dl_resp)
    );
    assert_eq!(
        response_bytes(&dl_resp).expect("download body"),
        content,
        "download: content mismatch"
    );
    eprintln!("download: content round-tripped");

    let resp = deleted.expect("delete request");
    assert_eq!(status_code(&resp), 204, "delete: expected 204");
    eprintln!("delete: cleaned up file {file_id}");
}

/// Fetch a URL and decode a JSON object, failing on non-2xx.
async fn get_json(client: &Client, url: &str) -> serde_json::Value {
    let body = fetch_ok(client, client.new_request("GET", url)).await;
    serde_json::from_slice(&body).expect("decoding JSON")
}

/// Run a request and return the body, failing on transport error or non-2xx.
async fn fetch_ok(client: &Client, req: gantryruntime::Request) -> Vec<u8> {
    let resp = client.fetch(req).await.expect("request failed");
    let code = status_code(&resp);
    let body = response_bytes(&resp).expect("reading body");
    assert!(
        (200..300).contains(&code),
        "unexpected status {code}: {}",
        String::from_utf8_lossy(&body)
    );
    body
}
