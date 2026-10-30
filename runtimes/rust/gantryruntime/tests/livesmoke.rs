//! VR-7 live smoke: exercise the runtime against a real Box account — one
//! authenticated call per configured auth flow, plus paginate + upload +
//! download + delete, with the etag flow (304 / 412) on the uploaded file.
//! `#[ignore]`d so the standard `cargo test` gate never runs it (no credentials
//! there), yet it still *compiles* under the gate so it can't rot. Run it on
//! demand with credentials:
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
use std::sync::Arc;
use std::time::Duration;

use gantryruntime::{
    response_bytes, status_code, stream_position, with_form_body, with_header, with_json_body,
    with_multipart_body, with_query, Auth, CcgConfig, Client, Error, EventSource, EventStream,
    JwtConfig, LongPollServer, OAuthConfig, Stream,
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
    // current user, proving the flow yields a usable token. Every flow runs
    // before any failure is raised, so one bad credential (an expired
    // developer token) doesn't hide the others' results.
    let mut primary: Option<Client> = None;
    let mut failed = Vec::new();
    for (name, auth) in sources {
        let client = Client::new(auth);
        match whoami(&client).await {
            Ok(id) => {
                eprintln!("{name} auth: authenticated as user {id}");
                primary.get_or_insert(client);
            }
            Err(err) => {
                eprintln!("{name} auth: FAILED: {err}");
                failed.push(name);
            }
        }
    }

    let client = Arc::new(
        primary.unwrap_or_else(|| panic!("no auth flow yielded a usable token: {failed:?}")),
    );
    smoke_paginate(&client).await;
    smoke_upload_download_delete(&client).await;
    assert!(failed.is_empty(), "auth flow(s) failed: {failed:?}");
}

/// `GET /users/me`: the current user's id, or why the flow yielded no usable
/// token.
async fn whoami(client: &Client) -> Result<String, String> {
    let url = format!("{}/users/me", client.base_url("api"));
    let resp = client
        .fetch(client.new_request("GET", &url))
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    let code = status_code(&resp);
    let body = response_bytes(&resp).map_err(|err| format!("reading body: {err}"))?;
    if !(200..300).contains(&code) {
        return Err(format!("status {code}: {}", String::from_utf8_lossy(&body)));
    }
    let me: serde_json::Value =
        serde_json::from_slice(&body).map_err(|err| format!("decoding: {err}"))?;
    if me["id"].is_null() {
        return Err(format!("no id in {me}"));
    }
    Ok(me["id"].to_string())
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
async fn smoke_upload_download_delete(client: &Arc<Client>) {
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
        "attributes",
        &attributes,
        "file",
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
    let etags = smoke_etags(client, &file_id).await;
    let events = smoke_events(client, &file_id).await;
    let downscope = smoke_downscope(client, &file_id).await;

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

    etags.unwrap_or_else(|err| panic!("etag: {err}"));
    eprintln!("etag: 304 for the current etag, 412 for a stale If-Match, the current one writes");
    events.unwrap_or_else(|err| panic!("events: {err}"));
    eprintln!("events: the long poll reported ITEM_RENAME for file {file_id}");
    let refused = downscope.unwrap_or_else(|err| panic!("downscope: {err}"));
    eprintln!("downscope: the narrow token renamed the file; its delete was refused ({refused})");

    let resp = deleted.expect("delete request");
    assert_eq!(status_code(&resp), 204, "delete: expected 204");
    eprintln!("delete: cleaned up file {file_id}");
}

/// Box's event stream over raw requests: `OPTIONS /events` for a real-time
/// server, `GET /events` for a page.
struct RawEvents {
    client: Arc<Client>,
}

impl EventSource for RawEvents {
    type Event = serde_json::Value;

    async fn server(&self) -> Result<Option<LongPollServer>, Error> {
        let url = format!("{}/events", self.client.base_url("api"));
        let resp = self
            .client
            .fetch(self.client.new_request("OPTIONS", &url))
            .await?;
        let listed: serde_json::Value = serde_json::from_slice(&response_bytes(&resp)?)?;
        let server = &listed["entries"][0];
        Ok(server["url"].as_str().map(|url| LongPollServer {
            url: url.to_string(),
            max_retries: None,
            retry_timeout: server["retry_timeout"].as_u64().map(Duration::from_secs),
        }))
    }

    async fn page(
        &self,
        position: &str,
    ) -> Result<(Vec<serde_json::Value>, Option<String>), Error> {
        let url = format!("{}/events", self.client.base_url("api"));
        let req = with_query(
            self.client.new_request("GET", &url),
            "stream_position",
            position,
        );
        let resp = self.client.fetch(req).await?;
        let page: serde_json::Value = serde_json::from_slice(&response_bytes(&resp)?)?;
        let entries = page["entries"].as_array().cloned().unwrap_or_default();
        Ok((entries, stream_position(&page["next_stream_position"])))
    }

    fn id(event: &serde_json::Value) -> Option<&str> {
        event["event_id"].as_str()
    }
}

/// Follow Box's long-poll event stream from "now" while the uploaded file is
/// renamed, and see that rename (`ITEM_RENAME`) within a minute: the long poll
/// reports the change, and the page after it carries it. Returns why it didn't,
/// so the caller can still clean up first.
async fn smoke_events(client: &Arc<Client>, file_id: &str) -> Result<(), String> {
    // Rename the file once the stream is waiting, so the long poll reports it.
    let renamer = {
        let client = client.clone();
        let url = format!("{}/files/{file_id}", client.base_url("api"));
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let name = serde_json::to_vec(&serde_json::json!({
                "name": format!("box-gantry-smoke-renamed-{nanos}.txt"),
            }))
            .unwrap();
            client
                .fetch(with_json_body(client.new_request("PUT", &url), &name))
                .await
                .map(|_| ())
        })
    };
    let source = RawEvents {
        client: client.clone(),
    };
    let mut stream = EventStream::new(client.clone(), "now", source);
    let found = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match stream.next().await {
                Some(Ok(event))
                    if event["event_type"] == "ITEM_RENAME" && event["source"]["id"] == file_id =>
                {
                    return Ok(());
                }
                Some(Ok(_)) => {}
                Some(Err(err)) => return Err(format!("the stream failed: {err}")),
                None => return Err("the stream ended".to_string()),
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(format!("no ITEM_RENAME for file {file_id} within a minute")));
    renamer
        .await
        .map_err(|err| format!("the rename task: {err}"))?
        .map_err(|err| format!("renaming the file: {err}"))?;
    found
}

/// Drive Box's etag flow on the uploaded file, as the generated
/// `…_if_modified` variants and the errors guide's re-read-and-retry loop
/// expect it: `If-None-Match` with the current etag is a 304 and a stale one
/// reads the file; `If-Match` with a stale etag is a 412 and the current one
/// writes. Returns why it didn't, so the caller can still clean up first.
async fn smoke_etags(client: &Client, file_id: &str) -> Result<(), String> {
    let url = format!("{}/files/{file_id}", client.base_url("api"));
    let file = client
        .fetch(client.new_request("GET", &url))
        .await
        .map_err(|err| format!("reading the file: {err}"))?;
    let file: serde_json::Value =
        serde_json::from_slice(&response_bytes(&file).unwrap_or_default())
            .map_err(|err| format!("decoding the file: {err}"))?;
    let etag = file["etag"]
        .as_str()
        .ok_or("the file read yields no etag")?
        .to_string();
    // Box's etags are version counters, so this one is well-formed but stale.
    let stale = format!("9{etag}");

    let read = |etag: &str| {
        client.fetch(with_header(
            client.new_request("GET", &url),
            "If-None-Match",
            etag,
        ))
    };
    match read(&etag).await {
        Err(err) if err.status() == Some(304) => {}
        outcome => {
            return Err(format!(
                "If-None-Match with the current etag: {}, want a 304",
                describe(outcome)
            ))
        }
    }
    read(&stale)
        .await
        .map_err(|err| format!("If-None-Match with a stale etag: {err}"))?;

    let description = serde_json::to_vec(&serde_json::json!({
        "description": "box-gantry etag smoke",
    }))
    .unwrap();
    let write = |etag: &str| {
        let req = with_json_body(client.new_request("PUT", &url), &description);
        client.fetch(with_header(req, "If-Match", etag))
    };
    match write(&stale).await {
        Err(err) if err.is_precondition_failed() => {}
        outcome => {
            return Err(format!(
                "If-Match with a stale etag: {}, want a 412",
                describe(outcome)
            ))
        }
    }
    write(&etag)
        .await
        .map_err(|err| format!("If-Match with the current etag: {err}"))?;
    Ok(())
}

/// Exchange the session's token for one limited to renaming and previewing the
/// uploaded file — the form the generated `downscope_token` posts — then prove
/// the narrow token renames the file and Box refuses its delete. Returns the
/// refusal, or why it didn't happen, so the caller still deletes the file with
/// the original token first.
async fn smoke_downscope(client: &Client, file_id: &str) -> Result<String, String> {
    let subject = client
        .access_token()
        .await
        .map_err(|err| format!("the session's token: {err}"))?;
    let url = format!("{}/files/{file_id}", client.base_url("api"));
    let form = form_encode(&[
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        ),
        ("subject_token", &subject),
        (
            "subject_token_type",
            "urn:ietf:params:oauth:token-type:access_token",
        ),
        ("scope", "item_rename item_preview"),
        ("resource", &url),
    ]);
    let token_url = format!("{}/oauth2/token", client.base_url("api_root"));
    let resp = client
        .fetch(with_form_body(
            client.new_request("POST", &token_url),
            form.as_bytes(),
        ))
        .await
        .map_err(|err| format!("the exchange: {err}"))?;
    let token: serde_json::Value =
        serde_json::from_slice(&response_bytes(&resp).unwrap_or_default())
            .map_err(|err| format!("decoding the exchange: {err}"))?;
    let token = token["access_token"]
        .as_str()
        .ok_or("the exchange returned no access token")?;
    let narrow = Client::new(Auth::developer_token(token));

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = serde_json::to_vec(&serde_json::json!({
        "name": format!("box-gantry-smoke-downscoped-{nanos}.txt"),
    }))
    .unwrap();
    narrow
        .fetch(with_json_body(narrow.new_request("PUT", &url), &name))
        .await
        .map_err(|err| format!("renaming with the narrow token: {err}"))?;
    match narrow.fetch(narrow.new_request("DELETE", &url)).await {
        Err(err) if matches!(err.status(), Some(401 | 403)) => Ok(err.to_string()),
        outcome => Err(format!(
            "deleting with the narrow token: {}, want a 401/403 refusal",
            describe(outcome)
        )),
    }
}

/// `application/x-www-form-urlencoded` pairs, percent-encoding all but RFC
/// 3986's unreserved bytes (a space becomes `%20`, which Box accepts).
fn form_encode(pairs: &[(&str, &str)]) -> String {
    let encode = |text: &str| {
        text.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
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

/// A fetch outcome for a failure message: the error, or the status it got.
fn describe(outcome: Result<gantryruntime::Response, gantryruntime::Error>) -> String {
    match outcome {
        Ok(resp) => format!("status {}", status_code(&resp)),
        Err(err) => err.to_string(),
    }
}
