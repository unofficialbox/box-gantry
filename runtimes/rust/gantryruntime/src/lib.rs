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
mod events;
mod jwt;
mod representations;
mod webhooks;

pub use auth::{Auth, CcgConfig, OAuthConfig, RefreshTokenStore, TokenSource};
pub use events::{stream_position, EventSource, EventStream, LongPollServer};
pub use jwt::JwtConfig;
pub use representations::DEFAULT_REPRESENTATION_WAIT;
pub use webhooks::{WebhookVerifier, DEFAULT_WEBHOOK_MAX_AGE};

use std::collections::HashMap;
use std::time::Duration;

/// The safety ceiling on a server-sent `Retry-After`, so a pathological one
/// (hours, years) can never suspend `fetch` far past the caller's intent.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);

/// The default exponential backoff (see [`Client::with_retry_backoff`]): the
/// wait before retry `n` is uniform in `[0, min(base·2ⁿ, max)]`.
pub const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
/// The default cap on one backoff wait (see [`DEFAULT_RETRY_BASE_DELAY`]).
pub const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// A runtime error: a failed request, auth acquisition, body decode, or a
/// non-2xx response from Box. The message carries the detail; a non-2xx
/// response also carries its status and Box's error fields ([`Error::api`]).
#[derive(Debug)]
pub struct Error {
    message: String,
    // Boxed so the common `Result<_, Error>` stays small.
    api: Option<Box<ApiError>>,
}

/// A non-2xx response from Box: [`Client::fetch`] returns it (inside an
/// [`Error`]) once the retry budget and the single 401 refresh are spent, so a
/// caller never mistakes an error body for a result. `code`, `message`, and
/// `request_id` come from Box's error body when it has one; `body` is the raw
/// payload either way.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: i64,
    pub code: Option<String>,
    pub message: Option<String>,
    pub request_id: Option<String>,
    pub body: Vec<u8>,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Error {
        Error {
            message: message.into(),
            api: None,
        }
    }

    /// The non-2xx response behind this error, if that's what it is.
    pub fn api(&self) -> Option<&ApiError> {
        self.api.as_deref()
    }

    /// The HTTP status of a non-2xx response; `None` for any other failure.
    pub fn status(&self) -> Option<i64> {
        self.api.as_ref().map(|api| api.status)
    }

    /// Whether this is Box's 412 Precondition Failed: an `If-Match` write whose
    /// etag no longer matches, because the item changed since it was read.
    /// Re-read the item and retry with its new etag.
    pub fn is_precondition_failed(&self) -> bool {
        self.status() == Some(412)
    }

    /// The error for a non-2xx response, reading Box's error fields from the
    /// body when it is a Box error object — or, from the token endpoint, an
    /// OAuth 2.0 error (`error`, `error_description`).
    fn from_response(response: &Response) -> Error {
        let payload: serde_json::Value =
            serde_json::from_slice(&response.body).unwrap_or(serde_json::Value::Null);
        let field = |key: &str| {
            payload
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let api = ApiError {
            status: response.status,
            code: field("code").or_else(|| field("error")),
            message: field("message").or_else(|| field("error_description")),
            request_id: field("request_id"),
            body: response.body.clone(),
        };
        let mut message = format!("box: status {}", api.status);
        if let Some(code) = &api.code {
            message.push_str(&format!(" {code}"));
        }
        if let Some(detail) = &api.message {
            message.push_str(&format!(": {detail}"));
        }
        if let Some(id) = &api.request_id {
            message.push_str(&format!(" (request_id {id})"));
        }
        Error {
            message,
            api: Some(Box::new(api)),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::new(err.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::new(err.to_string())
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
    /// A Box-style multipart body (G-7): a JSON part and/or a binary part,
    /// each present iff its bytes are non-empty — never by a hardcoded
    /// `"attributes"`/`"file"` part name.
    Multipart {
        json_part_name: String,
        json_bytes: Vec<u8>,
        file_part_name: String,
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
    timeout: Option<Duration>,
}

/// The runtime-owned HTTP response envelope. The body is read fully so it can
/// be replayed as bytes or a stream and so retries stay safe.
pub struct Response {
    status: i64,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Which HTTP protocol the built-in client uses for API requests (see
/// [`Client::with_http_version`]). Non-exhaustive so HTTP/3 can join once
/// reqwest's support is stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HttpVersion {
    /// HTTP/1.1 only.
    Http1,
    /// HTTP/2 when the server offers it over TLS (ALPN), else HTTP/1.1. The
    /// default.
    Http2,
}

/// The built-in API client: a 60s timeout, the requested protocol, and the
/// proxy, if one is set (otherwise reqwest reads `HTTPS_PROXY` / `NO_PROXY`).
fn api_http_client(version: HttpVersion, proxy: Option<&reqwest::Proxy>) -> reqwest::Client {
    let builder = reqwest::Client::builder().timeout(Duration::from_secs(60));
    let builder = match version {
        HttpVersion::Http1 => builder.http1_only(),
        HttpVersion::Http2 => builder,
    };
    let builder = match proxy {
        Some(proxy) => builder.proxy(proxy.clone()),
        None => builder,
    };
    builder.build().unwrap_or_default()
}

/// One HTTP attempt as [`Client::fetch`] is about to send it, for a request
/// hook (see [`Client::with_request_hook`]). The headers are those sent, except
/// that the `authorization` value reads `REDACTED`, so a hook that records
/// every header never captures the access token.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RequestInfo {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Counts from 1: each retry, and the resend after a 401 token refresh, is
    /// its own attempt.
    pub attempt: u32,
}

/// How one HTTP attempt ended, for a response hook (see
/// [`Client::with_response_hook`]): the response's status and headers, or the
/// transport `error` when none arrived. `elapsed` runs from sending the
/// request to reading the whole body.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResponseInfo {
    pub request: RequestInfo,
    pub status: Option<i64>,
    pub headers: Vec<(String, String)>,
    pub elapsed: Duration,
    pub error: Option<String>,
}

type RequestHook = std::sync::Arc<dyn Fn(&RequestInfo) + Send + Sync>;
type ResponseHook = std::sync::Arc<dyn Fn(&ResponseInfo) + Send + Sync>;

/// The runtime session: it holds the auth flow, HTTP client, base-URL
/// configuration, and retry policy shared by every manager.
pub struct Client {
    // Shared, so a derived session (see `with_as_user_header`) reuses the
    // token cache instead of acquiring its own token.
    auth: std::sync::Arc<Auth>,
    http: reqwest::Client,
    base_urls: HashMap<String, String>,
    max_retries: u32,
    retry_base: Duration,
    retry_max: Duration,
    default_headers: Vec<(String, String)>,
    // The built-in HTTP client's settings, kept so `with_http_version` and
    // `with_proxy` combine.
    http_version: HttpVersion,
    proxy: Option<reqwest::Proxy>,
    request_hooks: Vec<RequestHook>,
    response_hooks: Vec<ResponseHook>,
}

impl Client {
    /// Build a runtime session for an authentication flow, with the default
    /// Box base URLs, a 60s HTTP timeout, and five retries.
    pub fn new(auth: Auth) -> Client {
        Client {
            auth: std::sync::Arc::new(auth),
            http: api_http_client(HttpVersion::Http2, None),
            max_retries: 5,
            retry_base: DEFAULT_RETRY_BASE_DELAY,
            retry_max: DEFAULT_RETRY_MAX_DELAY,
            base_urls: default_base_urls(),
            default_headers: Vec::new(),
            http_version: HttpVersion::Http2,
            proxy: None,
            request_hooks: Vec::new(),
            response_hooks: Vec::new(),
        }
    }

    /// Override how many times a retriable failure is retried (fluent).
    pub fn with_max_retries(mut self, n: u32) -> Client {
        self.max_retries = n;
        self
    }

    /// Set the exponential backoff between retries (fluent): the wait before
    /// retry `n` is uniform (full jitter) in `[0, min(base·2ⁿ, max)]`, from
    /// [`DEFAULT_RETRY_BASE_DELAY`] and [`DEFAULT_RETRY_MAX_DELAY`]. A
    /// `Retry-After` from Box still raises the wait, up to its own ceiling.
    pub fn with_retry_backoff(mut self, base: Duration, max: Duration) -> Client {
        self.retry_base = base;
        self.retry_max = max;
        self
    }

    /// Override the `reqwest::Client` used for API requests (fluent) — e.g. to
    /// inject an instrumented transport instead of the internally built default.
    pub fn with_http_client(mut self, http: reqwest::Client) -> Client {
        self.http = http;
        self
    }

    /// Choose the HTTP protocol for API requests (fluent). Replaces the HTTP
    /// client with a freshly built default for `version`, so it and
    /// [`Client::with_http_client`] are alternatives — whichever is called
    /// last wins. To pick the protocol of an injected client, configure that
    /// client directly. Token requests keep their own client.
    pub fn with_http_version(mut self, version: HttpVersion) -> Client {
        self.http_version = version;
        self.http = api_http_client(version, self.proxy.as_ref());
        self
    }

    /// Send API requests through `proxy` (fluent), e.g.
    /// `reqwest::Proxy::all("http://proxy.internal:3128")?`, instead of the
    /// proxy `HTTPS_PROXY` / `NO_PROXY` select, which is the default. Like
    /// [`Client::with_http_version`] it rebuilds the default HTTP client, and
    /// the two combine; [`Client::with_http_client`] replaces both. Token
    /// requests take their proxy from [`Auth::with_proxy`].
    pub fn with_proxy(mut self, proxy: reqwest::Proxy) -> Client {
        self.http = api_http_client(self.http_version, Some(&proxy));
        self.proxy = Some(proxy);
        self
    }

    /// Call `hook` before every HTTP attempt `fetch` makes, retries included
    /// (fluent). Hooks observe; they can't change the request. Call again to
    /// add more hooks, which run in order. Token requests are not observed.
    /// This is the way in for instrumentation: a `reqwest::Client` has no
    /// middleware to wrap.
    pub fn with_request_hook(
        mut self,
        hook: impl Fn(&RequestInfo) + Send + Sync + 'static,
    ) -> Client {
        self.request_hooks.push(std::sync::Arc::new(hook));
        self
    }

    /// Call `hook` after every HTTP attempt `fetch` makes (fluent) — a retried
    /// 429 or 5xx, the 401 before a token refresh, and the final answer alike —
    /// and after an attempt that failed in transport. Like
    /// [`Client::with_request_hook`] it observes only, and adds to any hooks
    /// already set.
    pub fn with_response_hook(
        mut self,
        hook: impl Fn(&ResponseInfo) + Send + Sync + 'static,
    ) -> Client {
        self.response_hooks.push(std::sync::Arc::new(hook));
        self
    }

    /// Log one line per HTTP attempt to `sink` (fluent): the method, the URL
    /// with secret-looking query values redacted, the status or transport
    /// error, the elapsed time, and the attempt number. It never logs the
    /// `authorization` header, a token, or a body. The runtime has no logging
    /// dependency, so the sink decides where lines go —
    /// `|line| eprintln!("{line}")`, or a `log`/`tracing` macro. Like
    /// [`Client::with_response_hook`] it adds to any hooks already set.
    pub fn with_debug_logging(self, sink: impl Fn(&str) + Send + Sync + 'static) -> Client {
        self.with_response_hook(move |response| sink(&debug_log_line(response)))
    }

    /// Add a header sent with every request from this client (fluent) — e.g.
    /// a tracing or `User-Agent` header an embedding application wants on
    /// every call. Replaces any prior default with the same name
    /// (case-insensitive). A header set on an individual `Request` (via the
    /// free [`with_header`]) takes precedence over a same-named default.
    pub fn with_header(mut self, name: &str, value: &str) -> Client {
        self.default_headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self.default_headers
            .push((name.to_string(), value.to_string()));
        self
    }

    /// A new session that sends `As-User: user_id` with every API request, so
    /// Box acts as that user (an admin, or a service account whose app may
    /// "make API calls using the as-user header"). It shares this session's
    /// auth flow and token cache, HTTP client, base URLs, and retry policy;
    /// this session is unchanged. Token requests never carry the header.
    pub fn with_as_user_header(&self, user_id: &str) -> Client {
        Client {
            auth: self.auth.clone(),
            http: self.http.clone(),
            base_urls: self.base_urls.clone(),
            max_retries: self.max_retries,
            retry_base: self.retry_base,
            retry_max: self.retry_max,
            default_headers: self.default_headers.clone(),
            http_version: self.http_version,
            proxy: self.proxy.clone(),
            request_hooks: self.request_hooks.clone(),
            response_hooks: self.response_hooks.clone(),
        }
        .with_header("As-User", user_id)
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
            timeout: None,
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
            for (name, value) in &self.default_headers {
                if !request
                    .headers
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case(name))
                {
                    builder = builder.header(name, value);
                }
            }
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            builder = apply_body(builder, request.body.as_ref());
            if let Some(timeout) = request.timeout {
                builder = builder.timeout(timeout);
            }

            let mut info = None;
            let started = std::time::Instant::now();
            let sent = match builder.build() {
                Ok(http_request) => {
                    info = self.observe_request(&http_request, attempt + 1);
                    self.http.execute(http_request).await
                }
                Err(err) => Err(err),
            };
            let response = match sent {
                Ok(response) => response,
                Err(err) => {
                    self.observe_response(info, started, Err(&err.to_string()));
                    // A transport error gives no response, so a non-idempotent
                    // request may or may not have committed — don't replay it.
                    if attempt == self.max_retries || !idempotent {
                        return Err(err.into());
                    }
                    sleep(backoff(attempt, self.retry_base, self.retry_max)).await;
                    continue;
                }
            };
            let response = read_response(response).await;
            match &response {
                Ok(response) => self.observe_response(info, started, Ok(response)),
                Err(err) => self.observe_response(info, started, Err(&err.to_string())),
            }
            let response = response?;

            // A single force-refresh on 401: re-acquire past the token cache so
            // the retry doesn't just resend the same rejected token.
            if response.status == 401 && !refreshed && attempt < self.max_retries {
                refreshed = true;
                token = self.auth.force_refresh(&token).await?;
                continue;
            }
            // Back off exponentially on rate-limit / server errors.
            if should_retry(response.status, idempotent) && attempt < self.max_retries {
                let wait = backoff(attempt, self.retry_base, self.retry_max);
                sleep(retry_delay(&response, wait)).await;
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(Error::from_response(&response));
            }
            return Ok(response);
        }
        Err(Error::new("gantryruntime: retries exhausted"))
    }
}

impl Client {
    /// Run the request hooks for one attempt, returning what they saw for the
    /// response hooks to carry along (`None` when there are no hooks at all).
    fn observe_request(&self, request: &reqwest::Request, attempt: u32) -> Option<RequestInfo> {
        if self.request_hooks.is_empty() && self.response_hooks.is_empty() {
            return None;
        }
        let headers = request
            .headers()
            .iter()
            .map(|(name, value)| {
                let value = if name == reqwest::header::AUTHORIZATION {
                    "REDACTED"
                } else {
                    value.to_str().unwrap_or_default()
                };
                (name.as_str().to_string(), value.to_string())
            })
            .collect();
        let info = RequestInfo {
            method: request.method().as_str().to_string(),
            url: request.url().as_str().to_string(),
            headers,
            attempt,
        };
        for hook in &self.request_hooks {
            hook(&info);
        }
        Some(info)
    }

    /// Run the response hooks for one attempt: the response when it arrived,
    /// the error's message when it didn't.
    fn observe_response(
        &self,
        request: Option<RequestInfo>,
        started: std::time::Instant,
        outcome: Result<&Response, &str>,
    ) {
        let Some(request) = request else { return };
        if self.response_hooks.is_empty() {
            return;
        }
        let (status, headers, error) = match outcome {
            Ok(response) => (Some(response.status), response.headers.clone(), None),
            Err(message) => (None, Vec::new(), Some(message.to_string())),
        };
        let info = ResponseInfo {
            request,
            status,
            headers,
            elapsed: started.elapsed(),
            error,
        };
        for hook in &self.response_hooks {
            hook(&info);
        }
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
            json_part_name,
            json_bytes,
            file_part_name,
            file,
        }) => {
            // A boundary that provably does not occur in either part, so the
            // payload's own bytes can never split the framing.
            let boundary = multipart_boundary(json_bytes, file);
            let payload =
                multipart_body(&boundary, json_part_name, json_bytes, file_part_name, file);
            builder
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(payload)
        }
    }
}

/// Build a Box-style multipart/form-data body (G-7): a JSON part named
/// `json_part_name` iff `json_bytes` is non-empty, and a binary part named
/// `file_part_name` — used as both the form field name and the
/// Content-Disposition filename, since the real filename isn't
/// structurally knowable — iff `file` is non-empty.
fn multipart_body(
    boundary: &str,
    json_part_name: &str,
    json_bytes: &[u8],
    file_part_name: &str,
    file: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    if !json_bytes.is_empty() {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"\r\n",
                escape_filename(json_part_name)
            )
            .as_bytes(),
        );
        out.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
        out.extend_from_slice(json_bytes);
        out.extend_from_slice(b"\r\n");
    }
    if !file.is_empty() {
        let name = escape_filename(file_part_name);
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\n")
                .as_bytes(),
        );
        out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        out.extend_from_slice(file);
        out.extend_from_slice(b"\r\n");
    }
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

/// Exponential backoff from `base`, doubling per attempt up to `max`, with full
/// jitter.
fn backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    let mut ceiling = base.min(max);
    for _ in 0..attempt {
        if ceiling >= max {
            break;
        }
        ceiling = ceiling.saturating_mul(2).min(max);
    }
    let nanos = u64::try_from(ceiling.as_nanos()).unwrap_or(u64::MAX);
    Duration::from_nanos(jitter(nanos))
}

/// The delay before the next retry: the exponential `backoff` (so repeated
/// 429s/5xx escalate), raised to a server-sent `Retry-After` as a floor when
/// present — that header clamped to [`MAX_RETRY_DELAY`] so a hostile one
/// can't stall the client.
fn retry_delay(response: &Response, backoff: Duration) -> Duration {
    let mut delay = backoff;
    if let Some(value) = header_value(&response.headers, "retry-after") {
        if let Ok(secs) = value.trim().parse::<u64>() {
            delay = delay.max(Duration::from_secs(secs).min(MAX_RETRY_DELAY));
        }
    }
    delay
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
    x % max.saturating_add(1)
}

async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// The debug log's line for one attempt (see [`Client::with_debug_logging`]).
fn debug_log_line(response: &ResponseInfo) -> String {
    let request = &response.request;
    let url = redact_url(&request.url);
    let outcome = match (&response.error, response.status) {
        (Some(error), _) => format!("error: {}", error.replace(&request.url, &url)),
        (None, Some(status)) => status.to_string(),
        (None, None) => "no response".to_string(),
    };
    format!(
        "box: {} {url} -> {outcome} in {}ms (attempt {})",
        request.method,
        response.elapsed.as_millis(),
        request.attempt
    )
}

/// Query parameters whose value the debug log redacts: any name containing
/// one of these, case-insensitively.
const SECRET_PARAM_HINTS: [&str; 8] = [
    "token",
    "secret",
    "password",
    "code",
    "assertion",
    "key",
    "signature",
    "auth",
];

/// `raw` for a log line: any userinfo dropped, the fragment cut, and the value
/// of every secret-looking query parameter replaced by `REDACTED`.
fn redact_url(raw: &str) -> String {
    let raw = raw.split_once('#').map_or(raw, |(url, _)| url);
    let (base, query) = match raw.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (raw, None),
    };
    let mut out = base.to_string();
    if let Some(scheme) = base.find("://") {
        let start = scheme + 3;
        let end = base[start..]
            .find('/')
            .map_or(base.len(), |slash| start + slash);
        if let Some(at) = base[start..end].rfind('@') {
            out = format!("{}{}", &base[..start], &base[start + at + 1..]);
        }
    }
    let Some(query) = query else { return out };
    let pairs: Vec<String> = query
        .split('&')
        .map(|pair| {
            let name = pair.split_once('=').map_or(pair, |(name, _)| name);
            let decoded = percent_decode(name).to_lowercase();
            if SECRET_PARAM_HINTS.iter().any(|hint| decoded.contains(hint)) {
                format!("{name}=REDACTED")
            } else {
                pair.to_string()
            }
        })
        .collect();
    format!("{out}?{}", pairs.join("&"))
}

/// Decode `%XX` escapes and `+` (lossily), enough to read a parameter name.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Return the request with its own timeout, in place of the client's, for a
/// call meant to wait longer (a long poll).
pub fn with_timeout(mut request: Request, timeout: Duration) -> Request {
    request.timeout = Some(timeout);
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

/// Return the request with a Box-style multipart body. Writes a JSON part
/// named `json_part_name` iff `json_bytes` is non-empty, and a binary part
/// named `file_part_name` iff `file` is non-empty (G-7). A caller signals a
/// part's absence with an empty value, never by omitting the call.
pub fn with_multipart_body(
    mut request: Request,
    json_part_name: &str,
    json_bytes: &[u8],
    file_part_name: &str,
    file: Stream,
) -> Request {
    request.body = Some(Body::Multipart {
        json_part_name: json_part_name.to_string(),
        json_bytes: json_bytes.to_vec(),
        file_part_name: file_part_name.to_string(),
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

    /// The default backoff: 500ms doubling to a 30s cap.
    fn default_backoff(attempt: u32) -> Duration {
        backoff(attempt, DEFAULT_RETRY_BASE_DELAY, DEFAULT_RETRY_MAX_DELAY)
    }

    #[test]
    fn backoff_is_bounded_and_grows() {
        // Full jitter keeps every sample within [0, cap]; the cap climbs with
        // the attempt and saturates at 30s.
        for attempt in 0..8u32 {
            let cap = (500u64 << attempt.min(20)).min(30_000);
            for _ in 0..64 {
                assert!(default_backoff(attempt) <= Duration::from_millis(cap));
            }
        }
        // At high attempts the cap saturates at 30s, never higher.
        for _ in 0..64 {
            assert!(default_backoff(20) <= Duration::from_millis(30_000));
            assert!(default_backoff(u32::MAX) <= Duration::from_millis(30_000));
        }
    }

    #[test]
    fn backoff_follows_the_configured_base_and_max() {
        let ms = Duration::from_millis;
        for (attempt, base, max, cap) in [
            (0, ms(10), ms(35), ms(10)),
            (1, ms(10), ms(35), ms(20)),
            (2, ms(10), ms(35), ms(35)),
            (9, ms(10), ms(35), ms(35)),
            (3, ms(0), ms(0), ms(0)),
            (40, Duration::MAX, Duration::MAX, Duration::MAX),
        ] {
            // Every sample is at or under the cap, and over 200 of them full
            // jitter comes close to it.
            let samples: Vec<Duration> = (0..200).map(|_| backoff(attempt, base, max)).collect();
            assert!(
                samples.iter().all(|wait| *wait <= cap),
                "{attempt}: {samples:?}"
            );
            let largest = samples.iter().max().copied().unwrap_or_default();
            assert!(
                largest >= cap / 2 || cap == Duration::MAX,
                "{attempt}: {largest:?}"
            );
        }
    }

    #[test]
    fn retry_delay_uses_backoff_floored_by_retry_after_and_capped() {
        // A 2s Retry-After raises a shorter backoff to the floor.
        let resp = response(429, &[("Retry-After", "2")], b"");
        assert_eq!(
            retry_delay(&resp, Duration::from_millis(400)),
            Duration::from_secs(2)
        );
        // A pathological Retry-After is clamped to the ceiling, never honored raw.
        let resp = response(429, &[("Retry-After", "100000")], b"");
        assert_eq!(retry_delay(&resp, Duration::ZERO), MAX_RETRY_DELAY);
        // A non-numeric Retry-After falls back to the backoff.
        let resp = response(503, &[("Retry-After", "soon")], b"");
        assert_eq!(
            retry_delay(&resp, Duration::from_millis(400)),
            Duration::from_millis(400)
        );
    }

    #[tokio::test]
    async fn with_retry_backoff_shortens_the_waits() {
        const UNAVAILABLE: &[u8] =
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        const OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}";
        let (url, heads) =
            serve_sequence(&[UNAVAILABLE, UNAVAILABLE, UNAVAILABLE, UNAVAILABLE, OK]).await;
        let client = Client::new(Auth::developer_token("t"))
            .with_retry_backoff(Duration::from_millis(1), Duration::from_millis(4));
        let started = std::time::Instant::now();
        client.fetch(client.new_request("GET", &url)).await.unwrap();
        // Four waits of at most 1, 2, 4, and 4ms, where the defaults allow 7.5s.
        let elapsed = started.elapsed();
        assert_eq!(heads.lock().unwrap().len(), 5);
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }

    #[test]
    fn multipart_body_frames_both_parts() {
        let body = multipart_body("BOUND", "attributes", br#"{"name":"f"}"#, "file", b"data");
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("--BOUND\r\n"));
        assert!(text.contains("name=\"attributes\""));
        assert!(text.contains(r#"{"name":"f"}"#));
        assert!(text.contains("name=\"file\"; filename=\"file\""));
        assert!(text.contains("data"));
        assert!(text.ends_with("--BOUND--\r\n"));
    }

    #[test]
    fn multipart_body_omits_an_absent_part() {
        let body = multipart_body("BOUND", "", b"", "pic", b"data");
        let text = String::from_utf8(body).unwrap();
        assert!(!text.contains("application/json"));
        assert!(text.contains("name=\"pic\"; filename=\"pic\""));
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

    #[test]
    fn reqwest_is_built_with_http2() {
        // `http2_prior_knowledge` only exists with reqwest's `http2` feature;
        // without that feature ALPN never offers h2 and the SDK silently
        // speaks HTTP/1.1 only. This stops compiling if the feature is lost.
        let _ = reqwest::Client::builder().http2_prior_knowledge();
    }

    #[tokio::test]
    async fn with_http_version_http1_sends_http11() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let mut read = 0;
            while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buf[read..]).await.unwrap();
                if n == 0 {
                    break;
                }
                read += n;
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await
                .unwrap();
            let head = String::from_utf8_lossy(&buf[..read]).to_string();
            head.lines().next().unwrap_or_default().to_string()
        });
        let client = Client::new(Auth::developer_token("t"))
            .with_http_version(HttpVersion::Http1)
            .with_max_retries(0);
        let request = client.new_request("GET", &format!("http://{addr}/ping"));
        client
            .fetch(request)
            .await
            .expect("the local server answers");
        let request_line = server.await.unwrap();
        assert_eq!(request_line, "GET /ping HTTP/1.1");
    }

    #[tokio::test]
    async fn with_proxy_sends_requests_through_the_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let mut read = 0;
            while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buf[read..]).await.unwrap();
                if n == 0 {
                    break;
                }
                read += n;
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await
                .unwrap();
            let head = String::from_utf8_lossy(&buf[..read]).to_string();
            head.lines().next().unwrap_or_default().to_string()
        });
        // Set before the HTTP version: the version rebuild keeps the proxy.
        let client = Client::new(Auth::developer_token("t"))
            .with_proxy(reqwest::Proxy::all(format!("http://{addr}")).unwrap())
            .with_http_version(HttpVersion::Http1)
            .with_max_retries(0);
        let request = client.new_request("GET", "http://box.invalid/2.0/users/me");
        client.fetch(request).await.expect("the proxy answers");
        // A forward proxy sees the absolute target URL.
        let request_line = proxy.await.unwrap();
        assert_eq!(request_line, "GET http://box.invalid/2.0/users/me HTTP/1.1");
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

    /// A server that captures the raw bytes of the first request it receives,
    /// answers 200, then hands the captured text back over the returned
    /// channel — enough to assert on the headers a `fetch` call actually sent.
    async fn capture_request() -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // A single `read` may return only part of the request (TCP makes
            // no framing guarantee), which would flake a header assertion on
            // a split read. Keep reading until the header terminator shows up,
            // bounded so a malformed request can't hang the test.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = socket.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        (format!("http://{addr}/"), rx)
    }

    #[tokio::test]
    async fn with_header_sends_a_default_on_every_request() {
        let (url, rx) = capture_request().await;
        let client = Client::new(Auth::developer_token("t")).with_header("X-Trace-Id", "abc");
        client.fetch(client.new_request("GET", &url)).await.unwrap();
        let sent = rx.await.unwrap();
        assert!(sent.contains("x-trace-id: abc") || sent.contains("X-Trace-Id: abc"));
    }

    /// Like `capture_request`, but reads the full request (headers + body,
    /// bounded by `Content-Length`) — enough to assert on an outgoing
    /// multipart body's exact bytes end to end through `fetch`.
    async fn capture_request_with_body() -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let n = socket.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
            let content_length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < header_end + content_length {
                let n = socket.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let _ = tx.send(buf[header_end..header_end + content_length].to_vec());
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        (format!("http://{addr}/"), rx)
    }

    #[tokio::test]
    async fn with_multipart_body_sends_bare_json_and_the_file_bytes_end_to_end() {
        let (url, rx) = capture_request_with_body().await;
        let client = Client::new(Auth::developer_token("t"));
        let req = with_multipart_body(
            client.new_request("POST", &url),
            "attributes",
            br#"{"name":"f.txt"}"#,
            "file",
            Stream::from_bytes(b"file bytes".to_vec()),
        );
        client.fetch(req).await.unwrap();
        let body = rx.await.unwrap();
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"attributes\""));
        // The bare attributes object, not wrapped in another JSON layer.
        assert!(text.contains(r#"{"name":"f.txt"}"#));
        assert!(text.contains("name=\"file\"; filename=\"file\""));
        assert!(text.contains("file bytes"));
    }

    /// Serve one canned HTTP response on a local port; returns its URL.
    async fn serve_once(raw: &'static [u8]) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = socket.read(&mut buf).await.unwrap();
            let _ = socket.write_all(raw).await;
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn a_401_with_no_retries_left_surfaces_as_an_api_error() {
        let url = serve_once(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n").await;
        let client = Client::new(Auth::developer_token("t")).with_max_retries(0);
        let err = client
            .fetch(client.new_request("GET", &url))
            .await
            .err()
            .expect("a 401 is an error");
        assert_eq!(err.status(), Some(401));
    }

    /// A non-2xx response is an error carrying Box's error fields, never an
    /// `Ok` whose error body a caller would decode as a result.
    #[tokio::test]
    async fn a_non_2xx_response_is_an_api_error_with_the_box_error_fields() {
        let url = serve_once(
            b"HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 92\r\n\r\n\
              {\"type\":\"error\",\"status\":404,\"code\":\"not_found\",\"message\":\"Not Found\",\"request_id\":\"abc123\"}",
        )
        .await;
        let client = Client::new(Auth::developer_token("t"));
        let err = client
            .fetch(client.new_request("GET", &url))
            .await
            .err()
            .expect("a 404 is an error");
        let api = err.api().expect("API error details");
        assert_eq!(api.status, 404);
        assert_eq!(api.code.as_deref(), Some("not_found"));
        assert_eq!(api.message.as_deref(), Some("Not Found"));
        assert_eq!(api.request_id.as_deref(), Some("abc123"));
        assert_eq!(
            err.to_string(),
            "box: status 404 not_found: Not Found (request_id abc123)"
        );
    }

    /// The token endpoint answers with an OAuth 2.0 error body; its `error`
    /// and `error_description` fill the code and message.
    #[tokio::test]
    async fn an_oauth_error_body_fills_the_code_and_message() {
        let url = serve_once(
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: 73\r\n\r\n\
              {\"error\":\"invalid_grant\",\"error_description\":\"Grant credentials invalid\"}",
        )
        .await;
        let client = Client::new(Auth::developer_token("t"));
        let err = client
            .fetch(client.new_request("POST", &url))
            .await
            .err()
            .expect("a 400 is an error");
        let api = err.api().expect("API error details");
        assert_eq!(api.code.as_deref(), Some("invalid_grant"));
        assert_eq!(api.message.as_deref(), Some("Grant credentials invalid"));
    }

    #[tokio::test]
    async fn a_non_json_error_body_is_kept_raw() {
        let url =
            serve_once(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 13\r\n\r\nupstream down")
                .await;
        let client = Client::new(Auth::developer_token("t")).with_max_retries(0);
        let err = client
            .fetch(client.new_request("GET", &url))
            .await
            .err()
            .expect("a 502 is an error");
        let api = err.api().expect("API error details");
        assert_eq!((api.status, api.code.as_deref()), (502, None));
        assert_eq!(api.body, b"upstream down");
    }

    #[tokio::test]
    async fn request_header_overrides_a_same_named_default() {
        let (url, rx) = capture_request().await;
        let client = Client::new(Auth::developer_token("t")).with_header("X-Trace-Id", "default");
        let request = with_header(client.new_request("GET", &url), "X-Trace-Id", "override");
        client.fetch(request).await.unwrap();
        let sent = rx.await.unwrap();
        assert!(sent.to_lowercase().contains("x-trace-id: override"));
        assert!(!sent.to_lowercase().contains("x-trace-id: default"));
    }

    #[tokio::test]
    async fn as_user_header_rides_a_derived_session_only() {
        let client = Client::new(Auth::developer_token("t")).with_header("X-Trace-Id", "trace");
        let user = client.with_as_user_header("12345");

        let (url, rx) = capture_request().await;
        user.fetch(user.new_request("GET", &url)).await.unwrap();
        let sent = rx.await.unwrap().to_lowercase();
        assert!(sent.contains("as-user: 12345"), "{sent}");
        assert!(sent.contains("x-trace-id: trace"), "{sent}");

        let (url, rx) = capture_request().await;
        client.fetch(client.new_request("GET", &url)).await.unwrap();
        let sent = rx.await.unwrap().to_lowercase();
        assert!(!sent.contains("as-user"), "{sent}");
        assert!(sent.contains("x-trace-id: trace"), "{sent}");
    }

    /// Serve the canned responses in order, one connection each (every one
    /// says `connection: close`); returns the URL and the request heads seen.
    async fn serve_sequence(
        responses: &'static [&'static [u8]],
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let heads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = heads.clone();
        tokio::spawn(async move {
            for raw in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).to_lowercase());
                let _ = socket.write_all(raw).await;
            }
        });
        (format!("http://{addr}/files/1"), heads)
    }

    /// A 429 (`Retry-After: 0`), then a 401, then a 200.
    const FLAKY: &[&[u8]] = &[
        b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 0\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
    ];

    #[tokio::test]
    async fn hooks_observe_every_attempt_with_the_token_withheld() {
        let (url, heads) = serve_sequence(FLAKY).await;
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let responses = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (seen_requests, seen_responses) = (requests.clone(), responses.clone());
        let client = Client::new(Auth::developer_token("secret-token"))
            .with_retry_backoff(Duration::ZERO, Duration::ZERO)
            .with_request_hook(move |r| seen_requests.lock().unwrap().push(r.clone()))
            .with_response_hook(move |r| seen_responses.lock().unwrap().push(r.clone()));
        let request = with_query(client.new_request("GET", &url), "fields", "id");
        client.fetch(request).await.unwrap();

        let heads = heads.lock().unwrap();
        assert_eq!(heads.len(), 3);
        assert!(heads[2].contains("authorization: bearer secret-token"));
        let requests = requests.lock().unwrap();
        let responses = responses.lock().unwrap();
        assert_eq!((requests.len(), responses.len()), (3, 3));
        for (i, status) in [429, 401, 200].into_iter().enumerate() {
            let attempt = i as u32 + 1;
            let (request, response) = (&requests[i], &responses[i]);
            assert_eq!(
                (request.attempt, response.request.attempt),
                (attempt, attempt)
            );
            assert_eq!(request.method, "GET");
            assert_eq!(request.url, format!("{url}?fields=id"));
            assert_eq!(
                header_value(&request.headers, "authorization"),
                Some("REDACTED")
            );
            assert_eq!(response.status, Some(status));
            assert!(response.error.is_none());
        }
        assert_eq!(
            header_value(&responses[0].headers, "retry-after"),
            Some("0")
        );
    }

    #[tokio::test]
    async fn the_debug_log_never_holds_a_secret() {
        let (url, _) = serve_sequence(FLAKY).await;
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        let client = Client::new(Auth::developer_token("secret-token"))
            .with_retry_backoff(Duration::ZERO, Duration::ZERO)
            .with_debug_logging(move |line| sink.lock().unwrap().push(line.to_string()));
        let mut request = with_query(client.new_request("POST", &url), "fields", "id");
        request = with_query(request, "access_token", "query-token");
        request = with_query(request, "client_secret", "query-secret");
        request = with_json_body(request, br#"{"password":"body-secret"}"#);
        client.fetch(request).await.unwrap();

        // A transport error's message carries the URL; it's redacted there too.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let sink = lines.clone();
        let refused = Client::new(Auth::developer_token("t"))
            .with_max_retries(0)
            .with_debug_logging(move |line| sink.lock().unwrap().push(line.to_string()));
        let request = with_query(
            refused.new_request("GET", &format!("http://{addr}/")),
            "access_token",
            "query-token",
        );
        assert!(refused.fetch(request).await.is_err());

        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 4, "{lines:#?}");
        for line in lines.iter() {
            for secret in ["secret-token", "query-token", "query-secret", "body-secret"] {
                assert!(!line.contains(secret), "leaked {secret}: {line}");
            }
        }
        for (i, status) in ["429", "401", "200"].into_iter().enumerate() {
            let want = format!(
                "box: POST {url}?fields=id&access_token=REDACTED&client_secret=REDACTED -> {status} in "
            );
            assert!(lines[i].starts_with(&want), "{}", lines[i]);
            assert!(
                lines[i].ends_with(&format!("(attempt {})", i + 1)),
                "{}",
                lines[i]
            );
        }
        assert!(lines[3].contains("-> error: "), "{}", lines[3]);
    }

    #[test]
    fn redact_url_hides_secret_query_values_and_userinfo() {
        for (raw, want) in [
            (
                "https://api.box.com/2.0/files/1",
                "https://api.box.com/2.0/files/1",
            ),
            (
                "https://api.box.com/2.0/files?fields=id&limit=5",
                "https://api.box.com/2.0/files?fields=id&limit=5",
            ),
            (
                "https://a.box.com/x?Access_Token=t&code=c&refresh%5Ftoken=r#frag",
                "https://a.box.com/x?Access_Token=REDACTED&code=REDACTED&refresh%5Ftoken=REDACTED",
            ),
            (
                "https://user:pass@box.example/x?api_key=k&q=a@b",
                "https://box.example/x?api_key=REDACTED&q=a@b",
            ),
            (
                "https://box.example/x?password",
                "https://box.example/x?password=REDACTED",
            ),
        ] {
            assert_eq!(redact_url(raw), want, "{raw}");
        }
    }

    #[tokio::test]
    async fn a_response_hook_sees_a_transport_error() {
        // Bind and drop a listener, so its port refuses connections.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let responses = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = responses.clone();
        let client = Client::new(Auth::developer_token("t"))
            .with_max_retries(0)
            .with_response_hook(move |r| seen.lock().unwrap().push(r.clone()));
        let request = client.new_request("GET", &format!("http://{addr}/"));
        assert!(client.fetch(request).await.is_err());
        let responses = responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(
            (responses[0].status, responses[0].request.attempt),
            (None, 1)
        );
        assert!(responses[0].error.is_some());
    }
}
