//! The hand-written runtime the generated Box Rust SDK ships against
//! (TR-Rust.5). It implements the machine-readable runtime contract
//! (`gantry-contract` V1): generated code calls only these declarations, and
//! this crate supplies the behavior — a retrying async network layer (jittered
//! backoff, 401 refresh, Retry-After), auth-token threading, request builders,
//! and response accessors.
//!
//! It is the real implementation the compilable stubs stand in for during
//! generation-time verification (FR-5.3). Because it satisfies the same
//! signatures — `async fn` network entry points returning `Result<T, Error>`,
//! per the Rust manifest axes — the generated SDK compiles against it unchanged
//! (FR-5.2), which `crates/gantry-backend-rust/tests` enforces.
//!
//! Async threads cancellation through the future itself (no context parameter);
//! dropping a `fetch` future cancels the in-flight request.

mod auth;
mod jwt;

pub use auth::{Auth, CcgConfig, OAuthConfig, RefreshTokenStore, TokenSource};
pub use jwt::JwtConfig;

use std::collections::HashMap;
use std::time::Duration;

/// The safety ceiling on any single retry sleep, so a pathological server-sent
/// `Retry-After` (hours, years) can never suspend `fetch` far past the caller's
/// intent.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);

/// A runtime error: a failed request, auth acquisition, or body decode. Opaque
/// by design — the message carries the detail, the type stays stable.
#[derive(Debug)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Error {
        Error(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error(err.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error(err.to_string())
    }
}

/// A body stream. Buffered by construction (the Rust manifest's streaming axis
/// is satisfied by full buffering here): keeping the bytes in hand makes both
/// request retries and response replay safe.
pub struct Stream(Vec<u8>);

impl Stream {
    /// An empty stream (e.g. an absent multipart file part).
    pub fn empty() -> Stream {
        Stream(Vec::new())
    }

    /// A stream over already-buffered bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Stream {
        Stream(bytes)
    }

    /// The buffered bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

/// The body assembled by the `with_*` builders before `fetch` executes it.
enum Body {
    /// A byte body with a fixed content type (JSON, form, buffered stream).
    Bytes { content_type: String, data: Vec<u8> },
    /// A Box-style multipart body: an `attributes` JSON part plus a file part.
    Multipart {
        attributes: Vec<u8>,
        file_name: String,
        file: Vec<u8>,
    },
}

/// The runtime-owned HTTP request envelope, assembled by the `with_*` builders
/// before `fetch` executes it.
pub struct Request {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    query: Vec<(String, String)>,
    body: Option<Body>,
}

/// The runtime-owned HTTP response envelope. The body is read fully so it can
/// be replayed as bytes or a stream and so retries stay safe.
pub struct Response {
    status: i64,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// The runtime session: it holds the auth flow, HTTP client, base-URL
/// configuration, and retry policy shared by every manager.
pub struct Client {
    auth: Auth,
    http: reqwest::Client,
    base_urls: HashMap<String, String>,
    max_retries: u32,
}

impl Client {
    /// Build a runtime session for an authentication flow, with the default
    /// Box base URLs, a 60s HTTP timeout, and five retries.
    pub fn new(auth: Auth) -> Client {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Client {
            auth,
            http,
            max_retries: 5,
            base_urls: default_base_urls(),
        }
    }

    /// Override how many times a retriable failure is retried (fluent).
    pub fn with_max_retries(mut self, n: u32) -> Client {
        self.max_retries = n;
        self
    }

    /// Override the `reqwest::Client` used for API requests (fluent) — e.g. to
    /// inject an instrumented transport instead of the internally built default.
    pub fn with_http_client(mut self, http: reqwest::Client) -> Client {
        self.http = http;
        self
    }

    /// Override one base-URL class for custom deployments (fluent).
    pub fn with_base_url(mut self, name: &str, base: &str) -> Client {
        self.base_urls
            .insert(name.to_string(), base.trim_end_matches('/').to_string());
        self
    }

    /// The configured base URL for a D-106 class, without a trailing slash.
    pub fn base_url(&self, name: &str) -> String {
        self.base_urls.get(name).cloned().unwrap_or_default()
    }

    /// A valid access token for the configured auth flow.
    pub async fn access_token(&self) -> Result<String, Error> {
        self.auth.access_token().await
    }

    /// Create a request envelope for a method and fully built URL.
    pub fn new_request(&self, method: &str, url: &str) -> Request {
        Request {
            method: method.to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            query: Vec::new(),
            body: None,
        }
    }

    /// Execute the request with retries: exponential backoff + full jitter, a
    /// single 401 token refresh, and Retry-After (as a floor) on 429/5xx.
    ///
    /// Retries are gated on idempotency: a 429 means the request was rate-limited
    /// and never processed, so it retries for every method; a transport error or
    /// 5xx may have already committed a write, so those retry only for idempotent
    /// methods (GET/HEAD/PUT/DELETE/…). The 401 refresh applies to every method.
    pub async fn fetch(&self, request: Request) -> Result<Response, Error> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| {
            Error::new(format!(
                "gantryruntime: invalid method {:?}",
                request.method
            ))
        })?;
        let idempotent = method.is_idempotent();
        let mut token = self.access_token().await?;
        let mut refreshed = false;

        for attempt in 0..=self.max_retries {
            let mut builder = self
                .http
                .request(method.clone(), &request.url)
                .query(&request.query)
                .header("Authorization", format!("Bearer {token}"));
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            builder = apply_body(builder, request.body.as_ref());

            let response = match builder.send().await {
                Ok(response) => response,
                Err(err) => {
                    // A transport error gives no response, so a non-idempotent
                    // request may or may not have committed — don't replay it.
                    if attempt == self.max_retries || !idempotent {
                        return Err(err.into());
                    }
                    sleep(backoff(attempt)).await;
                    continue;
                }
            };
            let response = read_response(response).await?;

            // A single force-refresh on 401: re-acquire past the token cache so
            // the retry doesn't just resend the same rejected token.
            if response.status == 401 && !refreshed {
                refreshed = true;
                token = self.auth.force_refresh(&token).await?;
                continue;
            }
            // Back off exponentially on rate-limit / server errors.
            if should_retry(response.status, idempotent) && attempt < self.max_retries {
                sleep(retry_delay(&response, attempt)).await;
                continue;
            }
            return Ok(response);
        }
        Err(Error::new("gantryruntime: retries exhausted"))
    }
}

/// The default Box base URLs by D-106 class (custom deployments override any
/// via `with_base_url`).
fn default_base_urls() -> HashMap<String, String> {
    [
        ("api", "https://api.box.com/2.0"),
        ("api_root", "https://api.box.com"),
        ("upload", "https://upload.box.com/api/2.0"),
        ("upload_session", "https://upload.box.com/api/2.0"),
        ("oauth_authorize", "https://account.box.com/api/oauth2"),
        ("download", "https://api.box.com/2.0"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Attach the assembled body (and its content type) to the request builder.
fn apply_body(builder: reqwest::RequestBuilder, body: Option<&Body>) -> reqwest::RequestBuilder {
    match body {
        None => builder,
        Some(Body::Bytes { content_type, data }) => builder
            .header("Content-Type", content_type)
            .body(data.clone()),
        Some(Body::Multipart {
            attributes,
            file_name,
            file,
        }) => {
            // A boundary that provably does not occur in either part, so the
            // payload's own bytes can never split the framing.
            let boundary = multipart_boundary(attributes, file);
            let payload = multipart_body(&boundary, attributes, file_name, file);
            builder
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(payload)
        }
    }
}

/// Build a Box-style multipart/form-data body: an `attributes` JSON field plus
/// a `file` part (G-7). `file_name` is escaped for the quoted header value.
fn multipart_body(boundary: &str, attributes: &[u8], file_name: &str, file: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(b"Content-Disposition: form-data; name=\"attributes\"\r\n");
    out.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    out.extend_from_slice(attributes);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            escape_filename(file_name)
        )
        .as_bytes(),
    );
    out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    out.extend_from_slice(file);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// A multipart boundary guaranteed not to appear in either part — so no file
/// content can forge a delimiter and split the framing. Derived from a
/// nanosecond-seeded counter, re-rolled until it is collision-free (`mime/
/// multipart` gets this from a random boundary; we verify explicitly).
fn multipart_boundary(attributes: &[u8], file: &[u8]) -> String {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    loop {
        let candidate = format!("gantryruntimeXboundaryX{seed:016x}");
        let needle = candidate.as_bytes();
        if !contains_subslice(attributes, needle) && !contains_subslice(file, needle) {
            return candidate;
        }
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
    }
}

/// Whether `haystack` contains `needle` as a contiguous subslice.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Escape a filename for a quoted `Content-Disposition` value (RFC 6266): strip
/// CR/LF so it can never inject header lines or a boundary, and backslash-escape
/// `\` and `"` so a quote can't close the value early.
fn escape_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '\r' | '\n' => {}
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Read a response fully into the owned envelope.
async fn read_response(response: reqwest::Response) -> Result<Response, Error> {
    let status = response.status().as_u16() as i64;
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = response.bytes().await?.to_vec();
    Ok(Response {
        status,
        headers,
        body,
    })
}

/// Whether a response status warrants a retry for a request of this idempotency.
/// A 429 (rate-limited, never processed) retries for any method; a 5xx may have
/// committed a write, so it retries only for idempotent methods.
fn should_retry(status: i64, idempotent: bool) -> bool {
    status == 429 || (status >= 500 && idempotent)
}

/// Exponential backoff capped at 30s, with full jitter.
fn backoff(attempt: u32) -> Duration {
    let base = (500u64.saturating_mul(1u64 << attempt.min(20))).min(30_000);
    Duration::from_millis(jitter(base))
}

/// The delay before the next retry: exponential backoff (so repeated 429s/5xx
/// escalate), raised to a server-sent `Retry-After` as a floor when present, and
/// clamped to [`MAX_RETRY_DELAY`] so a hostile header can't stall the client.
fn retry_delay(response: &Response, attempt: u32) -> Duration {
    let mut delay = backoff(attempt);
    if let Some(value) = header_value(&response.headers, "retry-after") {
        if let Ok(secs) = value.trim().parse::<u64>() {
            delay = delay.max(Duration::from_secs(secs));
        }
    }
    delay.min(MAX_RETRY_DELAY)
}

/// A cheap, dependency-free full-jitter source: a uniform value in `[0, max]`
/// from a nanosecond-seeded xorshift (jitter only needs to decorrelate retries,
/// not be cryptographic).
fn jitter(max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x % (max + 1)
}

async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Case-insensitive header lookup (HTTP header names are case-insensitive).
fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Return the request with the header set (replacing any prior value).
pub fn with_header(mut request: Request, name: &str, value: &str) -> Request {
    request
        .headers
        .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
    request.headers.push((name.to_string(), value.to_string()));
    request
}

/// Return the request with the query parameter appended, encoded at send time.
pub fn with_query(mut request: Request, name: &str, value: &str) -> Request {
    request.query.push((name.to_string(), value.to_string()));
    request
}

/// Return the request with the serialized JSON body and content type set.
pub fn with_json_body(mut request: Request, body: &[u8]) -> Request {
    request.body = Some(Body::Bytes {
        content_type: "application/json".to_string(),
        data: body.to_vec(),
    });
    request
}

/// Return the request with an application/x-www-form-urlencoded body (the
/// OAuth2 token endpoints).
pub fn with_form_body(mut request: Request, form: &[u8]) -> Request {
    request.body = Some(Body::Bytes {
        content_type: "application/x-www-form-urlencoded".to_string(),
        data: form.to_vec(),
    });
    request
}

/// Return the request with a streaming body (buffered here).
pub fn with_stream_body(mut request: Request, body: Stream, content_type: &str) -> Request {
    request.body = Some(Body::Bytes {
        content_type: content_type.to_string(),
        data: body.into_bytes(),
    });
    request
}

/// Return the request with a Box-style multipart body: an `attributes` JSON
/// part plus a file part (G-7).
pub fn with_multipart_body(
    mut request: Request,
    attributes: &[u8],
    file_name: &str,
    file: Stream,
) -> Request {
    request.body = Some(Body::Multipart {
        attributes: attributes.to_vec(),
        file_name: file_name.to_string(),
        file: file.into_bytes(),
    });
    request
}

/// Read the whole response body.
pub fn response_bytes(response: &Response) -> Result<Vec<u8>, Error> {
    Ok(response.body.clone())
}

/// The response body as a stream, for binary downloads (FR-7.4).
pub fn response_stream(response: &Response) -> Stream {
    Stream::from_bytes(response.body.clone())
}

/// A response header value, empty when absent (redirect Location, Retry-After
/// surfacing).
pub fn response_header(response: &Response, name: &str) -> String {
    header_value(&response.headers, name)
        .unwrap_or_default()
        .to_string()
}

/// The response status code.
pub fn status_code(response: &Response) -> i64 {
    response.status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: i64, headers: &[(&str, &str)], body: &[u8]) -> Response {
        Response {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn base_urls_default_and_override() {
        let client = Client::new(Auth::developer_token("t"));
        assert_eq!(client.base_url("api"), "https://api.box.com/2.0");
        assert_eq!(client.base_url("upload"), "https://upload.box.com/api/2.0");
        assert_eq!(client.base_url("nope"), "");
        let client = client.with_base_url("api", "https://custom.example.com/2.0/");
        // The trailing slash is trimmed (contract: no trailing slash).
        assert_eq!(client.base_url("api"), "https://custom.example.com/2.0");
    }

    #[test]
    fn with_header_replaces_case_insensitively() {
        let client = Client::new(Auth::developer_token("t"));
        let mut req = client.new_request("GET", "https://api.box.com/2.0/files/1");
        req = with_header(req, "X-Box", "a");
        req = with_header(req, "x-box", "b");
        assert_eq!(req.headers, vec![("x-box".to_string(), "b".to_string())]);
    }

    #[test]
    fn with_query_appends_in_order() {
        let client = Client::new(Auth::developer_token("t"));
        let mut req = client.new_request("GET", "https://api.box.com/2.0/files");
        req = with_query(req, "fields", "id");
        req = with_query(req, "fields", "name");
        assert_eq!(
            req.query,
            vec![
                ("fields".to_string(), "id".to_string()),
                ("fields".to_string(), "name".to_string())
            ]
        );
    }

    #[test]
    fn response_accessors() {
        let resp = response(201, &[("Location", "/folders/9")], b"hello");
        assert_eq!(status_code(&resp), 201);
        assert_eq!(response_bytes(&resp).unwrap(), b"hello");
        // Header lookup is case-insensitive; absent headers read empty.
        assert_eq!(response_header(&resp, "location"), "/folders/9");
        assert_eq!(response_header(&resp, "X-Missing"), "");
        assert_eq!(response_stream(&resp).into_bytes(), b"hello");
    }

    #[test]
    fn retry_gating_respects_idempotency() {
        // 429 (never processed) retries for any method.
        assert!(should_retry(429, true));
        assert!(should_retry(429, false));
        // 5xx retries only for idempotent methods (a write may have committed).
        assert!(should_retry(500, true));
        assert!(should_retry(503, true));
        assert!(!should_retry(500, false));
        // Success / client errors never retry.
        assert!(!should_retry(200, true));
        assert!(!should_retry(404, true));
    }

    #[test]
    fn backoff_is_bounded_and_grows() {
        // Full jitter keeps every sample within [0, cap]; the cap climbs with
        // the attempt and saturates at 30s.
        for attempt in 0..8u32 {
            let cap = (500u64 << attempt.min(20)).min(30_000);
            for _ in 0..64 {
                assert!(backoff(attempt) <= Duration::from_millis(cap));
            }
        }
        // At high attempts the cap saturates at 30s, never higher.
        for _ in 0..64 {
            assert!(backoff(20) <= Duration::from_millis(30_000));
        }
    }

    #[test]
    fn retry_delay_uses_backoff_floored_by_retry_after_and_capped() {
        // At attempt 0 backoff is <= 500ms, so a 2s Retry-After raises the floor.
        let resp = response(429, &[("Retry-After", "2")], b"");
        assert_eq!(retry_delay(&resp, 0), Duration::from_secs(2));
        // A pathological Retry-After is clamped to the ceiling, never honored raw.
        let resp = response(429, &[("Retry-After", "100000")], b"");
        assert_eq!(retry_delay(&resp, 0), MAX_RETRY_DELAY);
        // A non-numeric Retry-After falls back to jittered backoff (<= cap).
        let resp = response(503, &[("Retry-After", "soon")], b"");
        assert!(retry_delay(&resp, 0) <= Duration::from_millis(500));
    }

    #[test]
    fn multipart_body_frames_both_parts() {
        let body = multipart_body("BOUND", br#"{"name":"f"}"#, "f.txt", b"data");
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("--BOUND\r\n"));
        assert!(text.contains("name=\"attributes\""));
        assert!(text.contains(r#"{"name":"f"}"#));
        assert!(text.contains("name=\"file\"; filename=\"f.txt\""));
        assert!(text.contains("data"));
        assert!(text.ends_with("--BOUND--\r\n"));
    }

    #[test]
    fn multipart_boundary_avoids_payload_collision() {
        // Seed the payload with what the first candidate would be, forcing a
        // re-roll; the returned boundary must not occur in either part.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap();
        let planted = format!("gantryruntimeXboundaryX{seed:016x}");
        let file = format!("prefix{planted}suffix").into_bytes();
        let boundary = multipart_boundary(b"{}", &file);
        assert!(!contains_subslice(&file, boundary.as_bytes()));
        assert!(!contains_subslice(b"{}", boundary.as_bytes()));
    }

    #[test]
    fn escape_filename_neutralizes_injection() {
        // CRLF is stripped (no header/boundary injection); quotes and
        // backslashes are escaped so they can't close the quoted value.
        assert_eq!(escape_filename("a\r\nb"), "ab");
        assert_eq!(escape_filename(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_filename(r"a\b"), r"a\\b");
        assert_eq!(escape_filename("plain.txt"), "plain.txt");
    }

    #[tokio::test]
    async fn developer_token_returns_the_fixed_token() {
        let client = Client::new(Auth::developer_token("dev-123"));
        assert_eq!(client.access_token().await.unwrap(), "dev-123");
    }

    /// A listener that accepts a connection and then never responds, so a
    /// request against it only returns once its client's own timeout fires.
    async fn hang_forever() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
            std::future::pending::<()>().await;
        });
        format!("http://{addr}/hang")
    }

    #[tokio::test]
    async fn with_http_client_is_used_for_requests() {
        // The default client's timeout is 60s; a request through it against a
        // server that never responds would hang this test for a minute. An
        // injected client with a much shorter timeout proves `fetch` actually
        // uses it rather than the internally built default.
        let url = hang_forever().await;
        let injected = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        // Retries would otherwise chain several timed-out attempts with
        // backoff sleeps between them; disable them so the bound below tests
        // only the injected client's own timeout.
        let client = Client::new(Auth::developer_token("t"))
            .with_http_client(injected)
            .with_max_retries(0);
        let request = client.new_request("GET", &url);
        let elapsed = {
            let start = std::time::Instant::now();
            let result = client.fetch(request).await;
            assert!(result.is_err(), "expected the short timeout to fire");
            start.elapsed()
        };
        assert!(
            elapsed < Duration::from_secs(5),
            "fetch took {elapsed:?}, expected it to fail fast via the injected client's timeout"
        );
    }
}
