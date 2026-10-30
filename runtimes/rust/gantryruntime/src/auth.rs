//! The Box auth flows, each producing an [`Auth`] passed to
//! [`Client::new`](crate::Client::new). `developer_token` is a fixed token;
//! `client_credentials` (CCG) and `oauth` exchange credentials at Box's token
//! endpoint and cache the resulting access token until shortly before it
//! expires (TR-Rust.5). Those flows also revoke their token on logout
//! ([`Auth::revoke_token`]).
//!
//! JWT server auth (signing-key assertions) lands in the next runtime slice;
//! the three flows here cover fixed-token and both refresh-based exchanges.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::jwt::{JwtConfig, Signer};
use crate::Error;

/// Box's OAuth 2.0 token endpoint, shared by every exchange flow.
const DEFAULT_TOKEN_URL: &str = "https://api.box.com/oauth2/token";

/// Where a user is sent to grant an OAuth 2.0 app access.
const AUTHORIZE_URL: &str = "https://account.box.com/api/oauth2/authorize";

/// Refresh a cached token this long before expiry so in-flight requests never
/// race an expiry.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// The configured authentication flow. Build one with [`Auth::developer_token`],
/// [`Auth::client_credentials`], or [`Auth::oauth`], then pass it to
/// [`Client::new`](crate::Client::new).
pub struct Auth {
    source: Source,
}

/// One of the supported token sources.
enum Source {
    /// A fixed developer-console token.
    Developer(String),
    /// A cached token refreshed by re-posting a fixed grant form (CCG).
    Form(FormSource),
    /// A cached token refreshed with a rotating refresh token (OAuth 2.0).
    OAuth(OAuthSource),
    /// A cached token refreshed by signing a fresh JWT bearer assertion. Boxed:
    /// the parsed RSA key makes this variant far larger than the others.
    Jwt(Box<JwtSource>),
    /// A caller-supplied [`TokenSource`] (see [`Auth::custom`]).
    Custom(Arc<dyn TokenSource>),
}

/// A custom access-token source, for callers that need to own token
/// acquisition and refresh outside the built-in flows — e.g. an
/// engine-owned token cache with proactive refresh and per-identity fan-out.
/// Build an [`Auth`] from one with [`Auth::custom`].
#[async_trait::async_trait]
pub trait TokenSource: Send + Sync {
    /// A valid access token, refreshing through the implementation's own
    /// cache as needed.
    async fn access_token(&self) -> Result<String, Error>;

    /// Re-acquire a token after `stale` was rejected by a 401, bypassing any
    /// cache. If another caller already refreshed past `stale`, return the
    /// newer token instead of re-acquiring again — single-flight behavior is
    /// the implementation's responsibility, mirroring the built-in sources.
    async fn force_refresh(&self, stale: &str) -> Result<String, Error>;
}

impl Auth {
    /// The simplest flow: a fixed access token from the Box developer console.
    pub fn developer_token(token: impl Into<String>) -> Auth {
        Auth {
            source: Source::Developer(token.into()),
        }
    }

    /// Client Credentials Grant: server-to-server auth with no signing key. Set
    /// exactly one subject on the config — `enterprise_id` for the service
    /// account, or `user_id` to act as a managed user.
    pub fn client_credentials(config: CcgConfig) -> Auth {
        let (subject_type, subject_id) = match &config.user_id {
            Some(user) => ("user", user.clone()),
            None => ("enterprise", config.enterprise_id.clone()),
        };
        let form = vec![
            ("grant_type".to_string(), "client_credentials".to_string()),
            ("client_id".to_string(), config.client_id.clone()),
            ("client_secret".to_string(), config.client_secret.clone()),
            ("box_subject_type".to_string(), subject_type.to_string()),
            ("box_subject_id".to_string(), subject_id),
        ];
        Auth {
            source: Source::Form(FormSource {
                http: auth_http_client(),
                token_url: config.token_url.unwrap_or_else(default_token_url),
                form,
                client_id: config.client_id,
                client_secret: config.client_secret,
                cached: Mutex::new(Cached::empty()),
            }),
        }
    }

    /// JWT server auth: sign a short-lived RSA assertion (from the app's
    /// `box_config.json`) and exchange it for an access token. Set exactly one
    /// subject on the config — `enterprise_id` or `user_id`.
    ///
    /// Fallible: the RSA private key is parsed (and if needed decrypted) up
    /// front, so a bad key fails here rather than on the first request.
    pub fn jwt(config: JwtConfig) -> Result<Auth, Error> {
        let signer = Signer::new(&config)?;
        Ok(Auth {
            source: Source::Jwt(Box::new(JwtSource {
                http: auth_http_client(),
                token_url: config.token_url.unwrap_or_else(default_token_url),
                client_id: config.client_id,
                client_secret: config.client_secret,
                signer,
                cached: Mutex::new(Cached::empty()),
            })),
        })
    }

    /// Resume the OAuth 2.0 authorization-code flow from a previously stored
    /// refresh token, exchanging it for access tokens as needed. Box rotates
    /// the refresh token on each exchange, so the newest one is retained.
    ///
    /// The rotated token is held in memory only; use [`Auth::oauth_with_store`]
    /// to persist each rotation durably across restarts.
    pub fn oauth(config: OAuthConfig, refresh_token: impl Into<String>) -> Auth {
        Self::oauth_source(config, refresh_token.into(), None)
    }

    /// Like [`Auth::oauth`], but persists each rotated refresh token through a
    /// [`RefreshTokenStore`] before returning — so an app restart reloads the
    /// live token instead of a refresh token Box has already invalidated.
    pub fn oauth_with_store(
        config: OAuthConfig,
        refresh_token: impl Into<String>,
        store: Arc<dyn RefreshTokenStore>,
    ) -> Auth {
        Self::oauth_source(config, refresh_token.into(), Some(store))
    }

    /// Build an `Auth` from a custom [`TokenSource`] — for callers that need
    /// to own token acquisition and refresh outside the built-in flows (e.g.
    /// an engine-owned token cache with proactive refresh and per-identity
    /// fan-out) instead of one of the flows above.
    pub fn custom(source: Arc<dyn TokenSource>) -> Auth {
        Auth {
            source: Source::Custom(source),
        }
    }

    fn oauth_source(
        config: OAuthConfig,
        refresh_token: String,
        store: Option<Arc<dyn RefreshTokenStore>>,
    ) -> Auth {
        Auth {
            source: Source::OAuth(OAuthSource {
                http: auth_http_client(),
                token_url: config.token_url.clone().unwrap_or_else(default_token_url),
                client_id: config.client_id,
                client_secret: config.client_secret,
                store,
                state: Mutex::new(OAuthState {
                    token: String::new(),
                    expiry: Instant::now(),
                    refresh_token,
                    // The caller-supplied token is already durable.
                    refresh_token_persisted: true,
                    revoked: false,
                }),
            }),
        }
    }

    /// Send token requests through `proxy` (fluent), e.g.
    /// `reqwest::Proxy::all("http://proxy.internal:3128")?`, instead of the
    /// proxy `HTTPS_PROXY` / `NO_PROXY` select, which is the default. A
    /// developer token or custom source makes no token requests, so it is
    /// unchanged. API requests take theirs from
    /// [`Client::with_proxy`](crate::Client::with_proxy).
    pub fn with_proxy(mut self, proxy: reqwest::Proxy) -> Auth {
        let http = auth_http_client_via(Some(proxy));
        match &mut self.source {
            Source::Form(source) => source.http = http,
            Source::OAuth(source) => source.http = http,
            Source::Jwt(source) => source.http = http,
            Source::Developer(_) | Source::Custom(_) => {}
        }
        self
    }

    /// A valid access token for the configured flow.
    pub(crate) async fn access_token(&self) -> Result<String, Error> {
        match &self.source {
            Source::Developer(token) => Ok(token.clone()),
            Source::Form(source) => source.access_token().await,
            Source::OAuth(source) => source.access_token().await,
            Source::Jwt(source) => source.access_token().await,
            Source::Custom(source) => source.access_token().await,
        }
    }

    /// Revoke the token this flow holds at Box's revoke endpoint, with the
    /// flow's own client credentials, and clear it from the cache — for logout
    /// and uninstall. The endpoint sits beside the token URL (`…/oauth2/token`
    /// → `…/oauth2/revoke`); token requests' proxy applies.
    ///
    /// CCG and JWT revoke the cached access token (if any) and acquire a fresh
    /// one on their next use. OAuth revokes the whole grant through its refresh
    /// token, so every later call fails with an error rather than refreshing a
    /// dead grant — run the authorization-code flow again, and discard the
    /// refresh token you stored. A developer token has no client credentials to
    /// revoke with, and a custom source owns its own tokens, so both are errors
    /// here; revoke those through the generated `authorization` manager.
    pub async fn revoke_token(&self) -> Result<(), Error> {
        match &self.source {
            Source::Developer(_) => Err(Error::new(
                "gantryruntime: a developer token has no client credentials to revoke it with",
            )),
            Source::Form(s) => {
                let mut cached = s.cached.lock().await;
                cached
                    .revoke(&s.http, &s.token_url, &s.client_id, &s.client_secret)
                    .await
            }
            Source::OAuth(source) => source.revoke().await,
            Source::Jwt(s) => {
                let mut cached = s.cached.lock().await;
                cached
                    .revoke(&s.http, &s.token_url, &s.client_id, &s.client_secret)
                    .await
            }
            Source::Custom(_) => Err(Error::new(
                "gantryruntime: a custom token source revokes its own tokens",
            )),
        }
    }

    /// Force-acquire a token after the current one was rejected (a 401),
    /// bypassing the freshness cache. Single-flight: if another task already
    /// replaced the rejected `stale` token, that new token is returned instead
    /// of refreshing again. A fixed developer token has nothing to refresh, so
    /// it is returned unchanged (the retry then surfaces the 401).
    pub(crate) async fn force_refresh(&self, stale: &str) -> Result<String, Error> {
        match &self.source {
            Source::Developer(token) => Ok(token.clone()),
            Source::Form(source) => source.force_refresh(stale).await,
            Source::OAuth(source) => source.force_refresh(stale).await,
            Source::Jwt(source) => source.force_refresh(stale).await,
            Source::Custom(source) => source.force_refresh(stale).await,
        }
    }
}

/// A durable store for the rotating OAuth refresh token. Box invalidates the
/// previous refresh token on each exchange, so an app that restarts must reload
/// the newest one — implement this to persist each rotation (a file, a DB row,
/// a secret manager).
///
/// `save` is `async` so a store can do real I/O without blocking the executor
/// (do the I/O with async APIs, or offload sync work via
/// `tokio::task::spawn_blocking`). It runs before a freshly rotated token is
/// returned; its failure propagates on the rotating call, and the runtime keeps
/// retrying persistence on later calls until it succeeds, so a rotation is never
/// silently treated as durable when it is not.
#[async_trait::async_trait]
pub trait RefreshTokenStore: Send + Sync {
    /// Persist the newly rotated refresh token durably.
    async fn save(&self, refresh_token: &str) -> Result<(), Error>;
}

/// The cached token if it is present and not within the refresh margin of
/// expiry (shared by both caching sources).
fn fresh_token(token: &str, expiry: Instant) -> Option<String> {
    if !token.is_empty() && expiry.saturating_duration_since(Instant::now()) > REFRESH_MARGIN {
        Some(token.to_string())
    } else {
        None
    }
}

/// A cached access token and its expiry instant.
struct Cached {
    token: String,
    expiry: Instant,
}

impl Cached {
    fn empty() -> Cached {
        Cached {
            token: String::new(),
            expiry: Instant::now(),
        }
    }

    fn fresh(&self) -> Option<String> {
        fresh_token(&self.token, self.expiry)
    }

    fn store(&mut self, token: String, ttl: Duration) {
        self.expiry = Instant::now() + ttl;
        self.token = token;
    }

    /// Revoke the cached token and clear it (CCG and JWT). A missing or
    /// already-expired token is simply dropped; on a failed revoke it stays.
    async fn revoke(
        &mut self,
        http: &reqwest::Client,
        token_url: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<(), Error> {
        if !self.token.is_empty() && Instant::now() < self.expiry {
            post_revoke_form(http, token_url, client_id, client_secret, &self.token).await?;
        }
        *self = Cached::empty();
        Ok(())
    }
}

/// A token source that refreshes by re-posting a fixed grant form (CCG).
struct FormSource {
    http: reqwest::Client,
    token_url: String,
    form: Vec<(String, String)>,
    /// The form's client credentials again, for a revoke.
    client_id: String,
    client_secret: String,
    cached: Mutex<Cached>,
}

impl FormSource {
    async fn access_token(&self) -> Result<String, Error> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.fresh() {
            return Ok(token);
        }
        self.refresh_locked(&mut cached).await
    }

    async fn force_refresh(&self, stale: &str) -> Result<String, Error> {
        let mut cached = self.cached.lock().await;
        // Single-flight: a concurrent 401 may already have replaced the token.
        if let Some(token) = cached.fresh() {
            if token != stale {
                return Ok(token);
            }
        }
        self.refresh_locked(&mut cached).await
    }

    async fn refresh_locked(&self, cached: &mut Cached) -> Result<String, Error> {
        let response = post_token_form(&self.http, &self.token_url, &self.form).await?;
        cached.store(response.access_token.clone(), response.ttl());
        Ok(response.access_token)
    }
}

/// A token source that refreshes by signing a fresh JWT bearer assertion and
/// exchanging it (server auth with a signing key). Like [`FormSource`], but the
/// grant form is re-minted each refresh — each assertion is single-use.
struct JwtSource {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    client_secret: String,
    signer: Signer,
    cached: Mutex<Cached>,
}

impl JwtSource {
    async fn access_token(&self) -> Result<String, Error> {
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.fresh() {
            return Ok(token);
        }
        self.refresh_locked(&mut cached).await
    }

    async fn force_refresh(&self, stale: &str) -> Result<String, Error> {
        let mut cached = self.cached.lock().await;
        // Single-flight: a concurrent 401 may already have replaced the token.
        if let Some(token) = cached.fresh() {
            if token != stale {
                return Ok(token);
            }
        }
        self.refresh_locked(&mut cached).await
    }

    async fn refresh_locked(&self, cached: &mut Cached) -> Result<String, Error> {
        let assertion = self.signer.assertion(&self.token_url)?;
        let form = vec![
            (
                "grant_type".to_string(),
                "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string(),
            ),
            ("assertion".to_string(), assertion),
            ("client_id".to_string(), self.client_id.clone()),
            ("client_secret".to_string(), self.client_secret.clone()),
        ];
        let response = post_token_form(&self.http, &self.token_url, &form).await?;
        cached.store(response.access_token.clone(), response.ttl());
        Ok(response.access_token)
    }
}

/// The OAuth 2.0 refresh-token source: it rotates the refresh token Box returns
/// (Box invalidates the old one each exchange) and, when configured, persists
/// each rotation through a [`RefreshTokenStore`].
struct OAuthSource {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    client_secret: String,
    store: Option<Arc<dyn RefreshTokenStore>>,
    state: Mutex<OAuthState>,
}

struct OAuthState {
    token: String,
    expiry: Instant,
    refresh_token: String,
    /// Whether `refresh_token` is known durable in the store. The initial token
    /// came from the caller (already stored); only later rotations start `false`
    /// until their `save` succeeds.
    refresh_token_persisted: bool,
    /// Set once the grant is revoked: there is nothing left to refresh.
    revoked: bool,
}

impl OAuthSource {
    async fn access_token(&self) -> Result<String, Error> {
        let mut state = self.state.lock().await;
        if state.revoked {
            return Err(revoked_grant());
        }
        self.retry_persist(&mut state).await;
        if let Some(token) = fresh_token(&state.token, state.expiry) {
            return Ok(token);
        }
        self.refresh_locked(&mut state).await
    }

    async fn force_refresh(&self, stale: &str) -> Result<String, Error> {
        let mut state = self.state.lock().await;
        if state.revoked {
            return Err(revoked_grant());
        }
        self.retry_persist(&mut state).await;
        // Single-flight: a concurrent 401 may already have replaced the token.
        if let Some(token) = fresh_token(&state.token, state.expiry) {
            if token != stale {
                return Ok(token);
            }
        }
        self.refresh_locked(&mut state).await
    }

    /// Revoke the grant through its refresh token (or, without one, the cached
    /// access token): Box accepts either and revokes the whole grant, so no
    /// fresh access token is minted first. Revoking twice is a no-op.
    async fn revoke(&self) -> Result<(), Error> {
        let mut state = self.state.lock().await;
        if state.revoked {
            return Ok(());
        }
        let token = if state.refresh_token.is_empty() {
            state.token.clone()
        } else {
            state.refresh_token.clone()
        };
        if !token.is_empty() {
            post_revoke_form(
                &self.http,
                &self.token_url,
                &self.client_id,
                &self.client_secret,
                &token,
            )
            .await?;
        }
        state.revoked = true;
        state.token.clear();
        state.refresh_token.clear();
        Ok(())
    }

    /// Best-effort retry of a rotation whose earlier `save` failed, so a
    /// transient store outage doesn't leave the durable copy behind the live
    /// token forever. The first failure was already surfaced by `refresh_locked`;
    /// these retries stay silent so a still-down store can't wedge every call.
    async fn retry_persist(&self, state: &mut OAuthState) {
        if state.refresh_token_persisted {
            return;
        }
        match &self.store {
            Some(store) => {
                if store.save(&state.refresh_token).await.is_ok() {
                    state.refresh_token_persisted = true;
                }
            }
            None => state.refresh_token_persisted = true,
        }
    }

    async fn refresh_locked(&self, state: &mut OAuthState) -> Result<String, Error> {
        let form = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), state.refresh_token.clone()),
            ("client_id".to_string(), self.client_id.clone()),
            ("client_secret".to_string(), self.client_secret.clone()),
        ];
        let response = post_token_form(&self.http, &self.token_url, &form).await?;
        state.token = response.access_token.clone();
        state.expiry = Instant::now() + response.ttl();
        if let Some(refresh) = &response.refresh_token {
            state.refresh_token = refresh.clone();
            // The rotation isn't durable until the store confirms it. Box has
            // already invalidated the previous token, so on a save failure we
            // keep the new token in memory (marked unpersisted, retried on later
            // calls) but surface the error now.
            state.refresh_token_persisted = self.store.is_none();
            if let Some(store) = &self.store {
                store.save(refresh).await?;
                state.refresh_token_persisted = true;
            }
        }
        Ok(response.access_token)
    }
}

/// The Client Credentials Grant config: server-to-server auth with no signing
/// key. Set exactly one subject — `enterprise_id` for the service account, or
/// `user_id` to act as a managed user. Derives `Default` so the optional
/// `user_id`/`token_url` can be elided with `..Default::default()`.
#[derive(Clone, Default)]
pub struct CcgConfig {
    pub client_id: String,
    pub client_secret: String,
    pub enterprise_id: String,
    /// Optional: act as a managed user instead of the enterprise service account.
    pub user_id: Option<String>,
    /// Optional: defaults to Box's token endpoint (custom deployments).
    pub token_url: Option<String>,
}

/// The OAuth 2.0 authorization-code config. Use [`OAuthConfig::authorize_url`]
/// to build the redirect, [`OAuthConfig::exchange_code`] to turn the returned
/// code into an [`Auth`], or [`Auth::oauth`] to resume from a stored refresh
/// token.
#[derive(Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// Optional: defaults to Box's token endpoint (custom deployments).
    pub token_url: Option<String>,
}

impl OAuthConfig {
    /// Build the URL to redirect a user to so they can grant access. `state` is
    /// echoed back to the redirect URI for CSRF protection.
    pub fn authorize_url(&self, redirect_uri: &str, state: &str) -> String {
        let query = form_urlencode(&[
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", redirect_uri),
            ("state", state),
        ]);
        format!("{AUTHORIZE_URL}?{query}")
    }

    /// Exchange an authorization code for an [`Auth`] that refreshes itself
    /// thereafter. The refresh token stays in memory; use
    /// [`OAuthConfig::exchange_code_with_store`] to persist it and every
    /// rotation, so a restarted app can resume with [`Auth::oauth_with_store`].
    pub async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<Auth, Error> {
        self.exchange(code, redirect_uri, None).await
    }

    /// Like [`OAuthConfig::exchange_code`], but saves the refresh token Box
    /// issues through `store` before returning, then each rotation as
    /// [`Auth::oauth_with_store`] does. If that first save fails, this returns
    /// the error and no `Auth`, and the user goes through consent again.
    pub async fn exchange_code_with_store(
        &self,
        code: &str,
        redirect_uri: &str,
        store: Arc<dyn RefreshTokenStore>,
    ) -> Result<Auth, Error> {
        self.exchange(code, redirect_uri, Some(store)).await
    }

    async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        store: Option<Arc<dyn RefreshTokenStore>>,
    ) -> Result<Auth, Error> {
        let http = auth_http_client();
        let token_url = self.token_url.clone().unwrap_or_else(default_token_url);
        let form = vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".to_string(), code.to_string()),
            ("client_id".to_string(), self.client_id.clone()),
            ("client_secret".to_string(), self.client_secret.clone()),
            ("redirect_uri".to_string(), redirect_uri.to_string()),
        ];
        let response = post_token_form(&http, &token_url, &form).await?;
        let refresh_token = response.refresh_token.clone().ok_or_else(|| {
            Error::new("gantryruntime: authorization-code exchange returned no refresh_token")
        })?;
        if let Some(store) = &store {
            store.save(&refresh_token).await?;
        }
        let ttl = response.ttl();
        let source = OAuthSource {
            http,
            token_url,
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            store,
            state: Mutex::new(OAuthState {
                token: response.access_token,
                expiry: Instant::now() + ttl,
                refresh_token,
                refresh_token_persisted: true,
                revoked: false,
            }),
        };
        Ok(Auth {
            source: Source::OAuth(source),
        })
    }
}

/// The subset of the token endpoint's JSON response we consume.
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
}

impl TokenResponse {
    fn ttl(&self) -> Duration {
        Duration::from_secs(self.expires_in)
    }
}

/// POST a form-encoded grant to the token endpoint and decode the response,
/// surfacing a non-2xx body as the error.
async fn post_token_form(
    http: &reqwest::Client,
    token_url: &str,
    form: &[(String, String)],
) -> Result<TokenResponse, Error> {
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let body = form_urlencode(&pairs);
    let response = http
        .post(token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        return Err(Error::new(format!(
            "gantryruntime: token endpoint returned {}: {}",
            status.as_u16(),
            detail.trim()
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new("gantryruntime: token endpoint returned no access_token"))?;
    Ok(TokenResponse {
        access_token,
        refresh_token: json
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        expires_in: json.get("expires_in").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

/// The error every call on a revoked OAuth grant fails with.
fn revoked_grant() -> Error {
    Error::new("gantryruntime: the OAuth grant was revoked; run the authorization-code flow again")
}

/// The revoke endpoint beside `token_url`: Box serves both under `/oauth2`, so
/// a custom token URL ending in `/token` gets its sibling `/revoke`. Any other
/// token URL is an error, not a guess.
fn revoke_url(token_url: &str) -> Result<String, Error> {
    token_url
        .strip_suffix("/token")
        .map(|base| format!("{base}/revoke"))
        .ok_or_else(|| {
            Error::new(format!(
                "gantryruntime: can't derive a revoke endpoint from token URL {token_url:?} \
                 (it doesn't end in /token)"
            ))
        })
}

/// POST `token` with the client credentials to the revoke endpoint beside
/// `token_url`, surfacing a non-2xx body as the error. Box answers an empty 200.
async fn post_revoke_form(
    http: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    token: &str,
) -> Result<(), Error> {
    let endpoint = revoke_url(token_url)?;
    let body = form_urlencode(&[
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("token", token),
    ]);
    let response = http
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let bytes = response.bytes().await?;
        let detail = String::from_utf8_lossy(&bytes);
        return Err(Error::new(format!(
            "gantryruntime: revoke endpoint returned {}: {}",
            status.as_u16(),
            detail.trim()
        )));
    }
    Ok(())
}

/// The dedicated auth HTTP client (a shorter timeout than the API client).
fn auth_http_client() -> reqwest::Client {
    auth_http_client_via(None)
}

/// The auth HTTP client through `proxy`, or through the proxy the environment
/// selects when `None`.
fn auth_http_client_via(proxy: Option<reqwest::Proxy>) -> reqwest::Client {
    let builder = reqwest::Client::builder().timeout(Duration::from_secs(30));
    let builder = match proxy {
        Some(proxy) => builder.proxy(proxy),
        None => builder,
    };
    builder.build().unwrap_or_default()
}

fn default_token_url() -> String {
    DEFAULT_TOKEN_URL.to_string()
}

/// Encode key/value pairs as `application/x-www-form-urlencoded`.
fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (name, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        percent_encode_into(&mut out, name);
        out.push('=');
        percent_encode_into(&mut out, value);
    }
    out
}

/// Percent-encode into `out` per the `application/x-www-form-urlencoded`
/// unreserved set (spaces become `+`).
fn percent_encode_into(out: &mut String, value: &str) {
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_urlencode_escapes_reserved_bytes() {
        let encoded = form_urlencode(&[("grant_type", "client_credentials"), ("q", "a b&c=d")]);
        assert_eq!(encoded, "grant_type=client_credentials&q=a+b%26c%3Dd");
    }

    #[test]
    fn authorize_url_carries_state_and_client() {
        let cfg = OAuthConfig {
            client_id: "cid".to_string(),
            client_secret: "secret".to_string(),
            token_url: None,
        };
        let url = cfg.authorize_url("https://app.example.com/cb", "xyz");
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=cid"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fapp.example.com%2Fcb"));
        assert!(url.contains("state=xyz"));
    }

    #[test]
    fn ccg_defaults_to_enterprise_subject() {
        // A config with no user_id targets the enterprise service account; the
        // token URL falls back to Box's endpoint.
        let auth = Auth::client_credentials(CcgConfig {
            client_id: "cid".to_string(),
            client_secret: "sec".to_string(),
            enterprise_id: "ent-1".to_string(),
            user_id: None,
            token_url: None,
        });
        match auth.source {
            Source::Form(source) => {
                assert_eq!(source.token_url, DEFAULT_TOKEN_URL);
                assert!(source
                    .form
                    .contains(&("box_subject_type".to_string(), "enterprise".to_string())));
                assert!(source
                    .form
                    .contains(&("box_subject_id".to_string(), "ent-1".to_string())));
            }
            Source::Developer(_) | Source::OAuth(_) | Source::Jwt(_) | Source::Custom(_) => {
                panic!("expected a form source")
            }
        }
    }

    #[test]
    fn cached_token_freshness_respects_margin() {
        let mut cached = Cached::empty();
        assert!(cached.fresh().is_none(), "empty cache is never fresh");
        cached.store("tok".to_string(), Duration::from_secs(3600));
        assert_eq!(cached.fresh().as_deref(), Some("tok"));
        // Inside the refresh margin, the cache reports stale.
        cached.store("tok".to_string(), Duration::from_secs(10));
        assert!(cached.fresh().is_none());
    }

    #[tokio::test]
    async fn force_refresh_reuses_a_concurrently_refreshed_token() {
        // A source whose cache already holds a fresh token *different* from the
        // rejected one: force_refresh returns it without any network round-trip
        // (single-flight — the token_url is unreachable, so a POST would fail).
        let source = FormSource {
            http: auth_http_client(),
            token_url: "http://127.0.0.1:1/never".to_string(),
            form: Vec::new(),
            client_id: String::new(),
            client_secret: String::new(),
            cached: Mutex::new(Cached::empty()),
        };
        source
            .cached
            .lock()
            .await
            .store("new".to_string(), Duration::from_secs(3600));
        assert_eq!(source.force_refresh("old").await.unwrap(), "new");
    }

    /// A recording store that fails its first `save` `fail_first` times, then
    /// succeeds — to exercise both the propagate-on-failure and
    /// retry-until-persisted paths.
    struct Recorder {
        saved: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        remaining_failures: std::sync::Mutex<u32>,
    }

    #[async_trait::async_trait]
    impl RefreshTokenStore for Recorder {
        async fn save(&self, refresh_token: &str) -> Result<(), Error> {
            {
                let mut left = self.remaining_failures.lock().unwrap();
                if *left > 0 {
                    *left -= 1;
                    return Err(Error::new("store down"));
                }
            }
            self.saved.lock().unwrap().push(refresh_token.to_string());
            Ok(())
        }
    }

    /// A one-shot local HTTP endpoint that answers the next request with a fixed
    /// token JSON, returning its URL. Lets the OAuth refresh path run for real.
    async fn serve_one_token(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await; // drain the request (small form)
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        });
        format!("http://{addr}/token")
    }

    fn oauth_with(token_url: String, store: Arc<Recorder>) -> Auth {
        Auth::oauth_with_store(
            OAuthConfig {
                client_id: "c".to_string(),
                client_secret: "s".to_string(),
                token_url: Some(token_url),
            },
            "initial-refresh",
            store,
        )
    }

    #[tokio::test]
    async fn oauth_refresh_persists_rotation_through_store() {
        // A real refresh (initial access token is empty → refresh_locked runs)
        // must drive the rotated refresh token into the store.
        let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let store = Arc::new(Recorder {
            saved: saved.clone(),
            remaining_failures: std::sync::Mutex::new(0),
        });
        let url =
            serve_one_token(r#"{"access_token":"at","refresh_token":"rotated","expires_in":3600}"#)
                .await;
        let auth = oauth_with(url, store);
        assert_eq!(auth.access_token().await.unwrap(), "at");
        assert_eq!(saved.lock().unwrap().as_slice(), ["rotated"]);
    }

    #[tokio::test]
    async fn with_proxy_sends_token_requests_through_the_proxy() {
        // The token URL can't resolve; only the proxy can answer it.
        let proxy = serve_one_token(r#"{"access_token":"proxied","expires_in":3600}"#).await;
        let auth = Auth::client_credentials(CcgConfig {
            client_id: "c".to_string(),
            client_secret: "s".to_string(),
            enterprise_id: "1".to_string(),
            token_url: Some("http://box.invalid/oauth2/token".to_string()),
            ..Default::default()
        })
        .with_proxy(reqwest::Proxy::all(proxy).unwrap());
        assert_eq!(auth.access_token().await.unwrap(), "proxied");
    }

    #[tokio::test]
    async fn exchange_code_with_store_saves_the_issued_refresh_token() {
        let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let store = Arc::new(Recorder {
            saved: saved.clone(),
            remaining_failures: std::sync::Mutex::new(0),
        });
        let url =
            serve_one_token(r#"{"access_token":"at","refresh_token":"issued","expires_in":3600}"#)
                .await;
        let config = OAuthConfig {
            client_id: "c".to_string(),
            client_secret: "s".to_string(),
            token_url: Some(url),
        };
        let auth = config
            .exchange_code_with_store("code", "https://app.example/callback", store)
            .await
            .unwrap();
        assert_eq!(auth.access_token().await.unwrap(), "at");
        assert_eq!(saved.lock().unwrap().as_slice(), ["issued"]);
    }

    /// A `TokenSource` that hands out a fixed token and records `force_refresh`
    /// calls, so a custom `Auth` can be verified without a network round-trip.
    struct FixedTokenSource {
        token: String,
        force_refresh_calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TokenSource for FixedTokenSource {
        async fn access_token(&self) -> Result<String, Error> {
            Ok(self.token.clone())
        }

        async fn force_refresh(&self, _stale: &str) -> Result<String, Error> {
            self.force_refresh_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.token.clone())
        }
    }

    #[tokio::test]
    async fn custom_token_source_serves_access_and_force_refresh() {
        let source = Arc::new(FixedTokenSource {
            token: "custom-token".to_string(),
            force_refresh_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let auth = Auth::custom(source.clone());
        assert_eq!(auth.access_token().await.unwrap(), "custom-token");
        assert_eq!(auth.force_refresh("stale").await.unwrap(), "custom-token");
        assert_eq!(
            source
                .force_refresh_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    /// A local endpoint that answers one request per scripted `(status, body)`
    /// in turn and records each as `"<request target> <form body>"`.
    async fn serve_script(
        script: Vec<(u16, &'static str)>,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            for (status, body) in script {
                let (mut sock, _) = listener.accept().await.unwrap();
                // Read the head, then as much body as Content-Length declares.
                let mut raw = Vec::new();
                let mut buf = [0u8; 2048];
                loop {
                    let n = sock.read(&mut buf).await.unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + length || n == 0 {
                            let target = text.split(' ').nth(1).unwrap_or("").to_string();
                            record
                                .lock()
                                .unwrap()
                                .push(format!("{target} {}", &text[end + 4..]));
                            break;
                        }
                    }
                }
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn ccg(token_url: String) -> Auth {
        Auth::client_credentials(CcgConfig {
            client_id: "c".to_string(),
            client_secret: "s".to_string(),
            enterprise_id: "1".to_string(),
            token_url: Some(token_url),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn ccg_revoke_posts_the_token_then_reacquires() {
        let (base, seen) = serve_script(vec![
            (200, r#"{"access_token":"tok-1","expires_in":3600}"#),
            (200, ""),
            (200, r#"{"access_token":"tok-2","expires_in":3600}"#),
        ])
        .await;
        let auth = ccg(format!("{base}/oauth2/token"));
        assert_eq!(auth.access_token().await.unwrap(), "tok-1");
        auth.revoke_token().await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[1],
            "/oauth2/revoke client_id=c&client_secret=s&token=tok-1"
        );
        assert_eq!(auth.access_token().await.unwrap(), "tok-2");
        // A fresh flow has nothing cached to revoke: no request at all.
        ccg(format!("{base}/oauth2/token"))
            .revoke_token()
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_failed_revoke_surfaces_and_keeps_the_token() {
        let (base, _) = serve_script(vec![
            (200, r#"{"access_token":"kept","expires_in":3600}"#),
            (400, r#"{"error":"invalid_client"}"#),
        ])
        .await;
        let auth = ccg(format!("{base}/oauth2/token"));
        auth.access_token().await.unwrap();
        let err = auth.revoke_token().await.unwrap_err().to_string();
        assert!(
            err.contains("400") && err.contains("invalid_client"),
            "{err}"
        );
        assert_eq!(auth.access_token().await.unwrap(), "kept");
    }

    #[tokio::test]
    async fn oauth_revoke_revokes_the_grant_then_fails_loudly() {
        let (base, seen) = serve_script(vec![
            (
                200,
                r#"{"access_token":"at","refresh_token":"rt-1","expires_in":3600}"#,
            ),
            (200, ""),
        ])
        .await;
        let auth = Auth::oauth(
            OAuthConfig {
                client_id: "c".to_string(),
                client_secret: "s".to_string(),
                token_url: Some(format!("{base}/oauth2/token")),
            },
            "rt-0",
        );
        assert_eq!(auth.access_token().await.unwrap(), "at");
        auth.revoke_token().await.unwrap();
        // The rotated refresh token is what gets revoked.
        assert_eq!(
            seen.lock().unwrap()[1],
            "/oauth2/revoke client_id=c&client_secret=s&token=rt-1"
        );
        let err = auth.access_token().await.unwrap_err().to_string();
        assert!(err.contains("revoked"), "{err}");
        assert!(auth.force_refresh("at").await.is_err());
        // Revoking again is a no-op: the script has no third answer.
        auth.revoke_token().await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn jwt_revoke_posts_the_token() {
        let (base, seen) = serve_script(vec![
            (200, r#"{"access_token":"jwt-1","expires_in":3600}"#),
            (200, ""),
        ])
        .await;
        let auth = Auth::jwt(JwtConfig {
            client_id: "c".to_string(),
            client_secret: "s".to_string(),
            public_key_id: "kid".to_string(),
            private_key_pem: crate::jwt::tests::TEST_KEY.as_bytes().to_vec(),
            passphrase: None,
            enterprise_id: "1".to_string(),
            user_id: None,
            token_url: Some(format!("{base}/oauth2/token")),
        })
        .unwrap();
        assert_eq!(auth.access_token().await.unwrap(), "jwt-1");
        auth.revoke_token().await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[1],
            "/oauth2/revoke client_id=c&client_secret=s&token=jwt-1"
        );
    }

    #[tokio::test]
    async fn revoke_goes_through_the_token_proxy() {
        // The token URL can't resolve; only the proxy can answer it.
        let (proxy, seen) = serve_script(vec![
            (200, r#"{"access_token":"proxied","expires_in":3600}"#),
            (200, ""),
        ])
        .await;
        let auth = ccg("http://box.invalid/oauth2/token".to_string())
            .with_proxy(reqwest::Proxy::all(proxy).unwrap());
        auth.access_token().await.unwrap();
        auth.revoke_token().await.unwrap();
        assert!(
            seen.lock().unwrap()[1].starts_with("http://box.invalid/oauth2/revoke "),
            "{:?}",
            seen.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn revoke_refuses_what_it_cannot_revoke() {
        assert!(Auth::developer_token("t").revoke_token().await.is_err());
        // A token URL without a /token suffix has no derivable revoke URL.
        let auth = ccg("http://127.0.0.1:1/custom".to_string());
        if let Source::Form(source) = &auth.source {
            source
                .cached
                .lock()
                .await
                .store("tok".to_string(), Duration::from_secs(3600));
        }
        let err = auth.revoke_token().await.unwrap_err().to_string();
        assert!(err.contains("revoke endpoint"), "{err}");
    }

    #[tokio::test]
    async fn oauth_refresh_surfaces_then_retries_a_failed_persist() {
        // First save fails: the rotating call returns Err, but the new token is
        // kept in memory marked unpersisted...
        let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let store = Arc::new(Recorder {
            saved: saved.clone(),
            remaining_failures: std::sync::Mutex::new(1),
        });
        let url =
            serve_one_token(r#"{"access_token":"at","refresh_token":"rotated","expires_in":3600}"#)
                .await;
        let auth = oauth_with(url, store);
        assert!(auth.access_token().await.is_err(), "first save failed");
        assert!(saved.lock().unwrap().is_empty());
        // ...and the next call retries persistence (no network needed — the token
        // is still fresh) and succeeds.
        assert_eq!(auth.access_token().await.unwrap(), "at");
        assert_eq!(saved.lock().unwrap().as_slice(), ["rotated"]);
    }
}
