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

pub use auth::{Auth, CcgConfig, OAuthConfig};

use std::collections::HashMap;
use std::time::Duration;

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
    /// single 401 token refresh, and Retry-After on 429/503.
    pub async fn fetch(&self, request: Request) -> Result<Response, Error> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| {
            Error::new(format!(
                "gantryruntime: invalid method {:?}",
                request.method
            ))
        })?;
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
                    if attempt == self.max_retries {
                        return Err(err.into());
                    }
                    sleep(backoff(attempt)).await;
                    continue;
                }
            };
            let response = read_response(response).await?;

            // A single token refresh on 401.
            if response.status == 401 && !refreshed {
                refreshed = true;
                token = self.access_token().await?;
                continue;
            }
            // Back off on rate-limit / server errors, honoring Retry-After.
            if retriable(response.status) && attempt < self.max_retries {
                sleep(retry_after(&response, attempt)).await;
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
            let boundary = "gantryruntimeXboundary";
            let payload = multipart_body(boundary, attributes, file_name, file);
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
/// a `file` part (G-7).
fn multipart_body(boundary: &str, attributes: &[u8], file_name: &str, file: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(b"Content-Disposition: form-data; name=\"attributes\"\r\n");
    out.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    out.extend_from_slice(attributes);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n")
            .as_bytes(),
    );
    out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    out.extend_from_slice(file);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
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

/// 429 or any 5xx is worth retrying.
fn retriable(status: i64) -> bool {
    status == 429 || status >= 500
}

/// Exponential backoff capped at 30s, with full jitter.
fn backoff(attempt: u32) -> Duration {
    let base = (500u64.saturating_mul(1u64 << attempt.min(20))).min(30_000);
    Duration::from_millis(jitter(base))
}

/// Retry-After (seconds) when the server sets it, else plain backoff.
fn retry_after(response: &Response, attempt: u32) -> Duration {
    if let Some(value) = header_value(&response.headers, "retry-after") {
        if let Ok(secs) = value.trim().parse::<u64>() {
            return Duration::from_secs(secs);
        }
    }
    backoff(attempt)
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
    fn retriable_statuses() {
        assert!(retriable(429));
        assert!(retriable(500));
        assert!(retriable(503));
        assert!(!retriable(200));
        assert!(!retriable(404));
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
    fn retry_after_prefers_header_seconds() {
        let resp = response(429, &[("Retry-After", "2")], b"");
        assert_eq!(retry_after(&resp, 0), Duration::from_secs(2));
        // A non-numeric Retry-After falls back to jittered backoff (<= cap).
        let resp = response(503, &[("Retry-After", "soon")], b"");
        assert!(retry_after(&resp, 0) <= Duration::from_millis(500));
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

    #[tokio::test]
    async fn developer_token_returns_the_fixed_token() {
        let client = Client::new(Auth::developer_token("dev-123"));
        assert_eq!(client.access_token().await.unwrap(), "dev-123");
    }
}
