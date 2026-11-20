//! The router and every `/api` route in `docs/web-api.md`.
//!
//! Every request but sign-in, `/healthz` and the static app needs a live session; the
//! session's person must still be an active user in the user database (the tool server's
//! `resolve`, cached 10 seconds per person: `pmos_directory::resolve`), and every `POST`,
//! `PUT` and `DELETE` must come from the app's own origin and carry the session's CSRF token.
//! When the tool server can't be reached, only `GET /api/me` and `GET /api/brief` may answer,
//! on the person's last-known-good answer for at most 5 minutes, with Slack items withheld
//! ([`SignedRead`]); everything else answers `503 users_unavailable`. Drafts and tasks also need the PM abilities of the permission
//! matrix; someone else's draft or task answers `404 not_found`.

use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use pmos_contract::model::Role;
use pmos_contract::onboarding::personal_bot_name;
use pmos_contract::permissions::Permission;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use pmos_directory::resolve::{
    DirectoryUser, EntraState, HttpTransport, LinkError, LinkRefusal, Lookup, ResolveClient,
    ResolveError,
};

use crate::auth::{
    self, AuthProvider, DevLoginProvider, PasswordProvider, SignInError, SignInInput,
};
use crate::config::{Config, NO_PROVIDER};
use crate::drafts::{self, Draft, Suggestion};
use crate::entra::{self, Entra, Failure};
use crate::followups::{BriefConfig, FollowUpStore, MemoryFollowUpStore};
use crate::progress::{self, EventOut, Progress};
use crate::session::{
    self, IdHash, MemorySessionStore, Session, SessionStore, StoreError, CSRF_HEADER,
};
use crate::settings::{DraftEdit, MemorySettingsStore, SettingsStore, StoredSuggestion, UserState};
use crate::upstream::{
    Approval, DecisionBody, RefusedAudience, Runtime, Task, TaskEvent, ToolServer, UpstreamError,
};

mod admin;
mod brief;
mod content;
mod password;
mod policy;
mod proposal;
mod reviews;
mod users;

/// How many tasks `GET /api/tasks` lists.
pub const RECENT_TASKS: usize = 20;
/// The longest ask, instruction, title and body accepted.
pub const MAX_ASK: usize = 8_000;
pub const MAX_INSTRUCTION: usize = 2_000;
pub const MAX_TITLE: usize = 500;
pub const MAX_BODY: usize = 200_000;

/// The reasons a rejection may give (the tool server's ledger reasons).
pub const REJECT_REASONS: [&str; 6] = [
    "wrong_facts",
    "wrong_target",
    "tone",
    "incomplete",
    "should_not_draft",
    "other",
];

/// What the app can mark seen: the three one-time hints, then the guided start's own marks
/// (started, each of its three steps, skipped, and the one "Approved!" moment). `DELETE
/// /api/me/hints` clears them all, which is how "Show hints again" restarts the guided start.
pub const HINTS: [&str; 9] = [
    "sources",
    "ask",
    "approve",
    "tour_started",
    "tour_asked",
    "tour_opened",
    "tour_decided",
    "tour_skipped",
    "tour_cheered",
];

/// The tool server's `409` codes passed through on approve.
pub const PASSED_THROUGH: [&str; 3] = ["needs_external_confirm", "audience_blocked", "expired"];

/// The policy for `/api` responses: nothing to load, never framed.
pub const API_CSP: &str = "default-src 'none'; frame-ancestors 'none'; base-uri 'none'";
/// The policy for the web app's own files: only this origin's scripts, styles, images and
/// fonts (no inline script or style, no `data:` images, no remote pixels), forms post only
/// here, never framed, no `<base>`.
pub const APP_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
     img-src 'self'; font-src 'self'; connect-src 'self'; object-src 'none'; \
     base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// What a state-changing request with a body must say it is.
pub const JSON: &str = "application/json";

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// What the server keeps state in, and its clock. [`Deps::in_memory`] is for tests and
/// development; `store_config::deps_from_env` picks Postgres when `PMOS_WEB_DATABASE_URL` is set.
pub struct Deps {
    pub sessions: Arc<dyn SessionStore>,
    pub settings: Arc<dyn SettingsStore>,
    pub followups: Arc<dyn FollowUpStore>,
    pub brief: BriefConfig,
    pub clock: Clock,
    /// The admin policy and content screens: policies root, gateway config, GitHub.
    pub admin: crate::admin_config::AdminConfig,
}

impl Deps {
    pub fn in_memory() -> Self {
        Self {
            sessions: Arc::new(MemorySessionStore::new()),
            settings: Arc::new(MemorySettingsStore::new()),
            followups: Arc::new(MemoryFollowUpStore::new()),
            brief: BriefConfig::default(),
            clock: Arc::new(Utc::now),
            admin: crate::admin_config::AdminConfig::default(),
        }
    }
}

struct Inner {
    /// Users and roles: the tool server's `resolve` and first-link routes, cached 10 seconds
    /// per lookup, with the last-known-good answer kept for when it can't be reached.
    directory: Arc<ResolveClient>,
    /// The deployed content tree the admin content and policy screens read.
    content_root: PathBuf,
    public_url: Option<String>,
    static_root: Option<PathBuf>,
    idle: chrono::Duration,
    max: chrono::Duration,
    runtime: Runtime,
    toolserver: ToolServer,
    /// Entra first, then development sign-in.
    providers: Vec<Arc<dyn AuthProvider>>,
    entra: Option<Arc<Entra>>,
    sessions: Arc<dyn SessionStore>,
    settings: Arc<dyn SettingsStore>,
    followups: Arc<dyn FollowUpStore>,
    brief: BriefConfig,
    clock: Clock,
    proposals: proposal::Proposals,
    /// What refusals said of drafts' Slack audiences, by draft id ([`remember_audience`]).
    audiences: std::sync::Mutex<std::collections::HashMap<String, RefusedAudience>>,
    /// Sign-in events the tool server didn't record ([`record_sign_in`]), since start-up:
    /// `/readyz` reports it.
    sign_in_record_failures: std::sync::atomic::AtomicU64,
    /// Password sign-in (`PMOS_WEB_PASSWORD_LOGIN=1`; `password.rs`).
    password_login: bool,
    /// `PMOS_WEB_TRUSTED_PROXIES`: whose `X-Forwarded-For` names the client's address.
    trusted_proxies: Vec<std::net::IpAddr>,
    /// Password sign-in's per-address limit.
    sources: password::SourceLimiter,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

/// What `pmos-web` serves ([`build_server`]): the browser-facing router; the
/// server-to-server router when `PMOS_WEB_INTERNAL_BIND` gives it its own listener; and the
/// periodic purge.
pub struct Server {
    pub public: Router,
    /// `Some` only with `internal_bind`: `PUT /internal/brief/{handle}`, `/healthz` and
    /// `/readyz`. Without it, the internal route is on `public`.
    pub internal: Option<Router>,
    pub housekeeping: Housekeeping,
}

/// How often expired sessions, follow-ups, `changed` items, edits and suggestions are
/// deleted ([`Housekeeping::run`]).
pub const HOUSEKEEPING_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The periodic purge: what has expired is deleted every [`HOUSEKEEPING_EVERY`] whether or
/// not anyone signs in (it also runs at each sign-in). Expired rows are never served before
/// it runs: every read filters on expiry.
#[derive(Clone)]
pub struct Housekeeping(AppState);

impl Housekeeping {
    /// One purge now. A failure is logged, never fatal.
    pub async fn run_once(&self) {
        brief::housekeeping(&self.0).await;
    }

    /// Purge at once and then every `every`, until `shutdown`.
    pub async fn run(self, every: std::time::Duration, shutdown: CancellationToken) {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tick.tick() => self.run_once().await,
            }
        }
    }
}

type Providers = (Vec<Arc<dyn AuthProvider>>, Option<Arc<Entra>>);

fn make_providers(config: &Config) -> Result<Providers, String> {
    let mut out: Vec<Arc<dyn AuthProvider>> = Vec::new();
    let entra = match &config.entra {
        Some(c) => Some(Arc::new(Entra::new(c.clone())?)),
        None => None,
    };
    if let Some(e) = &entra {
        out.push(e.clone());
    }
    if config.password_login {
        out.push(Arc::new(PasswordProvider));
    }
    if config.dev_login {
        out.push(Arc::new(DevLoginProvider));
    }
    Ok((out, entra))
}

/// The sign-in providers `config` turns on, Entra first. Never empty for a config that
/// loaded.
pub fn providers(config: &Config) -> Result<Vec<Arc<dyn AuthProvider>>, String> {
    Ok(make_providers(config)?.0)
}

/// The browser-facing app ([`build_server`]'s `public`). Without `internal_bind`, the
/// server-to-server route is on it too.
pub fn build(config: Config, deps: Deps) -> Result<Router, String> {
    Ok(build_server(config, deps)?.public)
}

/// The app. Refuses (`Err`) with no sign-in provider, development sign-in on an address
/// that isn't loopback, or a static directory without an `index.html`.
pub fn build_server(config: Config, deps: Deps) -> Result<Server, String> {
    let (providers, entra) = make_providers(&config)?;
    if providers.is_empty() {
        return Err(NO_PROVIDER.into());
    }
    crate::config::check_dev_bind(config.dev_login, config.bind)?;
    let hsts = config
        .public_url
        .as_deref()
        .is_some_and(|u| u.starts_with("https://"));
    let static_root = match &config.static_dir {
        None => None,
        Some(dir) => {
            let root = dir
                .canonicalize()
                .map_err(|e| format!("PMOS_WEB_STATIC_DIR {}: {e}", dir.display()))?;
            if !root.join("index.html").is_file() {
                return Err(format!(
                    "PMOS_WEB_STATIC_DIR {} has no index.html",
                    dir.display()
                ));
            }
            Some(root)
        }
    };
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let directory = Arc::new(ResolveClient::new(
        Arc::new(HttpTransport::new(
            http.clone(),
            &config.toolserver_url,
            config.toolserver_token.token(),
        )),
        deps.clock.clone(),
    ));
    let state = AppState(Arc::new(Inner {
        directory,
        runtime: Runtime::new(http.clone(), &config.bots_url, &config.bots_token),
        toolserver: ToolServer::new(http, &config.toolserver_url, &config.toolserver_token),
        content_root: config.content_root,
        public_url: config.public_url,
        static_root,
        idle: config.session_idle,
        max: config.session_max,
        providers,
        entra,
        sessions: deps.sessions,
        settings: deps.settings,
        followups: deps.followups,
        brief: deps.brief,
        clock: deps.clock,
        proposals: proposal::Proposals::new(deps.admin)?,
        audiences: Default::default(),
        sign_in_record_failures: Default::default(),
        password_login: config.password_login,
        trusted_proxies: config.trusted_proxies,
        sources: Default::default(),
    }));
    let api = Router::new()
        .route("/auth/providers", get(auth_providers))
        .route("/auth/login", get(entra_login))
        .route("/auth/callback", get(entra_callback))
        .route("/auth/dev-login", post(dev_login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/me/settings", put(put_settings))
        .route("/me/hints", post(post_hint).delete(delete_hints))
        .route("/drafts", get(list_drafts))
        .route("/drafts/practice", post(create_practice))
        .route(
            "/drafts/{id}",
            get(get_draft).put(put_draft).delete(delete_draft),
        )
        .route("/drafts/{id}/changes", post(post_changes))
        .route("/drafts/{id}/suggestion", get(get_suggestion))
        .route("/drafts/{id}/suggestion/accept", post(accept_suggestion))
        .route("/drafts/{id}/suggestion/reject", post(reject_suggestion))
        .route("/drafts/{id}/approve", post(approve))
        .route("/drafts/{id}/reject", post(reject))
        .route("/tasks", get(list_tasks).post(create_task))
        .route("/tasks/{id}", get(get_task))
        .merge(brief::routes())
        .merge(admin::routes())
        .merge(users::routes())
        .merge(reviews::routes())
        .merge(policy::routes())
        .merge(content::routes())
        .merge(password::routes())
        .fallback(|| async { ApiError::not_found() })
        // A known path with another method: JSON too, never an empty body or the app.
        .method_not_allowed_fallback(|| async {
            ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "This route doesn't take that method.",
            )
        })
        .layer(axum::middleware::from_fn(require_json));
    let health = || {
        Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route("/readyz", get(readyz))
    };
    let mut public = health()
        .nest("/api", api)
        // Links from before the app's own routes (Slack cards in history): to the app's
        // route for the draft.
        .route("/drafts/{id}", get(old_draft_link))
        .route("/d/{id}", get(old_draft_link));
    let internal = match config.internal_bind {
        None => {
            public = public.route("/internal/brief/{handle}", brief::internal());
            None
        }
        Some(_) => Some(
            health()
                .route("/internal/brief/{handle}", brief::internal())
                .fallback(|| async { ApiError::not_found() })
                .layer(axum::middleware::from_fn_with_state(hsts, security_headers))
                .with_state(state.clone()),
        ),
    };
    let public = public
        .fallback(static_files)
        .layer(axum::middleware::from_fn_with_state(hsts, security_headers))
        .with_state(state.clone());
    Ok(Server {
        public,
        internal,
        housekeeping: Housekeeping(state),
    })
}

/// `GET /drafts/{id}` and `GET /d/{id}`: a `303` to the app's own route for the draft,
/// `/#/d/{id}`, so links Slack posted before cards used that route still open the draft.
/// The id must be shaped like an approval id (`apr_` and up to 64 letters or digits);
/// anything else goes to the app's home. Only ever a same-origin path: never an open
/// redirect.
async fn old_draft_link(Path(id): Path<String>) -> Response {
    let ok = id.strip_prefix("apr_").is_some_and(|rest| {
        !rest.is_empty() && rest.len() <= 64 && rest.chars().all(|c| c.is_ascii_alphanumeric())
    });
    if ok {
        redirect(&format!("/#/d/{id}"), &[])
    } else {
        redirect("/", &[])
    }
}

/// With Entra sign-in on: does the tool server check first-link tokens for this server's tenant
/// and client id? `None` with Entra off. The reason goes to the log, never the ids.
async fn entra_link_ready(state: &AppState) -> Option<bool> {
    let entra = state.0.entra.as_ref()?;
    let ours = entra.config();
    let ok = match state.0.directory.entra_link_settings().await {
        Ok(Some((tenant, client)))
            if tenant.eq_ignore_ascii_case(&ours.tenant_id)
                && client.eq_ignore_ascii_case(&ours.client_id) =>
        {
            true
        }
        Ok(Some(_)) => {
            tracing::error!("not ready: the tool server's Entra tenant or client id differs from PMOS_WEB_ENTRA_*: no first Microsoft sign-in can link (docs/entra-sign-in.md)");
            false
        }
        Ok(None) => {
            tracing::error!("not ready: the tool server has no Entra settings (PMOS_ENTRA_TENANT_ID, PMOS_ENTRA_CLIENT_ID): no first Microsoft sign-in can link (docs/entra-sign-in.md)");
            false
        }
        Err(e) => {
            tracing::warn!(
                why = e.0,
                "not ready: the tool server's Entra settings couldn't be read"
            );
            false
        }
    };
    Some(ok)
}

/// `GET /readyz`: `200` when this server can serve, `503` otherwise, with each check's
/// result (`ok` or `down`) and no detail: its store answers, and the bot runtime and the tool
/// server (which answers who may sign in) answer their own `/healthz` within two seconds. No sign-in
/// and no secrets: the details go to the log. With Entra sign-in on, also `entra_link`: the
/// tool server validates first-link ID tokens for the same tenant and client id as this
/// server (`GET /internal/users/entra-link`); without that, a first Microsoft sign-in can't
/// link, so the server isn't ready.
async fn readyz(State(state): State<AppState>) -> Response {
    let (store, runtime, toolserver, entra_link) = tokio::join!(
        state.0.sessions.ping(),
        state.0.runtime.healthy(),
        state.0.toolserver.healthy(),
        entra_link_ready(&state),
    );
    if let Err(e) = &store {
        tracing::warn!(error = %e, "not ready: store");
    }
    let mut checks = vec![
        ("store", store.is_ok()),
        ("bot_runtime", runtime),
        ("tool_server", toolserver),
    ];
    if let Some(ok) = entra_link {
        checks.push(("entra_link", ok));
    }
    let ready = checks.iter().all(|(_, ok)| *ok);
    let body: serde_json::Map<String, serde_json::Value> = checks
        .iter()
        .map(|(name, ok)| (name.to_string(), json!(if *ok { "ok" } else { "down" })))
        .collect();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    // Not a readiness check: sign-in works without it (see `record_sign_in`). A number above
    // zero means some sign-ins or sign-outs are missing from the audit log; the log says when.
    let unrecorded = state
        .0
        .sign_in_record_failures
        .load(std::sync::atomic::Ordering::Relaxed);
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "ready": ready,
            "checks": body,
            "unrecorded_sign_in_events": unrecorded,
        })),
    )
        .into_response()
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// What browsers are told to use HTTPS for, once they reach an `https` `PMOS_WEB_PUBLIC_URL`:
/// a year, this host only (another host under the same domain may not serve HTTPS).
pub const HSTS: &str = "max-age=31536000";

/// Every response: no sniffing, no framing, no referrer at all, its own browsing context,
/// and with an `https` public URL, `Strict-Transport-Security`. Every `/api` answer is
/// `no-store` with a policy that loads nothing, whatever the handler set; the app's files
/// keep the policy and caching `static_files` gives them.
async fn security_headers(
    State(hsts): State<bool>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let api = is_api(req.uri().path());
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    if hsts {
        h.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(HSTS),
        );
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    if api {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(API_CSP),
        );
    } else {
        if !h.contains_key(header::CACHE_CONTROL) {
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
        if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
            h.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(API_CSP),
            );
        }
    }
    res
}

/// Is the request's `Content-Type` JSON (`application/json`, with any parameters)?
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case(JSON))
}

/// Does the request carry a body?
fn has_body(headers: &HeaderMap) -> bool {
    let length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    length.is_some_and(|n| n > 0) || headers.contains_key(header::TRANSFER_ENCODING)
}

fn bad_content_type() -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "bad_content_type",
        "Send this as JSON (Content-Type: application/json).",
    )
}

/// A state-changing request with a body must say it is JSON, so a cross-site HTML form
/// (which can only send `text/plain`, form or multipart bodies without a preflight) can't
/// reach a route. Checked before anything else, sign-in included.
async fn require_json(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    if is_write(req.method()) && has_body(req.headers()) && !is_json(req.headers()) {
        tracing::warn!(path = %req.uri().path(), "refused a write that isn't JSON");
        return bad_content_type().into_response();
    }
    next.run(req).await
}

// Errors.

/// `{"code", "error"}` with a status.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: String,
    error: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &str, error: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            error: error.into(),
        }
    }

    /// The answer's `code`.
    pub fn code(&self) -> &str {
        &self.code
    }

    fn signed_out() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "signed_out",
            "You're signed out. Sign in again.",
        )
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "There's nothing here.")
    }

    fn not_allowed() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "not_allowed",
            "Your role in PM OS doesn't allow this.",
        )
    }

    fn not_pending() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "not_pending",
            "This draft has already been decided.",
        )
    }

    fn bad_request(error: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", error)
    }

    fn store(e: StoreError) -> Self {
        tracing::error!(error = %e, "store unavailable");
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "PM OS can't save this right now. Try again in a minute.",
        )
    }

    /// An upstream failure. Never carries the upstream's body or a token: only which
    /// service and a plain reason.
    fn upstream(e: UpstreamError) -> Self {
        match e {
            UpstreamError::NotFound => Self::not_found(),
            UpstreamError::Refused {
                service, status, ..
            } => {
                tracing::warn!(service, status, "upstream refused");
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    "upstream_unavailable",
                    format!(
                        "The {service} refused this. If it keeps happening, tell a platform owner."
                    ),
                )
            }
            UpstreamError::Failed { service, why } | UpstreamError::Unknown { service, why } => {
                tracing::warn!(service, why = %why, "upstream call failed");
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    "upstream_unavailable",
                    format!("The {service} {why}. Try again in a minute."),
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "code": self.code, "error": self.error })),
        )
            .into_response()
    }
}

type ApiResult = Result<Response, ApiError>;

fn parse<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(bytes)
        .map_err(|_| ApiError::bad_request("The request body isn't the JSON this route takes."))
}

/// As [`parse`], with an empty body or `null` read as `{}`.
fn parse_optional<T: DeserializeOwned + Default>(bytes: &Bytes) -> Result<T, ApiError> {
    if bytes.iter().all(u8::is_ascii_whitespace) || bytes.trim_ascii() == b"null" {
        return Ok(T::default());
    }
    parse(bytes)
}

/// Ids from URLs before they go upstream: letters, digits, `_` and `-`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_write(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    )
}

/// Same origin, and the session's CSRF token in `X-Pmos-Csrf`.
fn check_write(state: &AppState, headers: &HeaderMap, session: &Session) -> Result<(), ApiError> {
    if !auth::same_origin(headers, state.0.public_url.as_deref()) {
        tracing::warn!(user = %session.handle, "refused a write from another origin");
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "bad_origin",
            "That request didn't come from PM OS.",
        ));
    }
    let presented = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !pmos_runtime::token_eq(presented, &session.csrf_token) {
        tracing::warn!(user = %session.handle, "refused a write with a bad CSRF token");
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "bad_csrf",
            "This page is out of date. Reload it and try again.",
        ));
    }
    Ok(())
}

fn now(state: &AppState) -> DateTime<Utc> {
    (state.0.clock)()
}

// The signed-in PM.

/// How long a session may keep reading pmos-web's own store on its last-known-good answer
/// while the tool server can't be reached (spec §2, "Stated bounds").
pub const LAST_KNOWN_GOOD: chrono::Duration = chrono::Duration::minutes(5);

/// What the app says where it withheld Slack items on the last-known-good path.
pub const SLACK_WITHHELD_NOTE: &str = "Slack items are hidden until PM OS can check your access";

/// The signed-in person, as the user database answered for this request.
#[derive(Debug, Clone)]
pub struct Member {
    pub handle: String,
    /// Untrusted text (often from Slack): the app renders it as plain text.
    pub display_name: String,
    /// Known roles only; an unknown one grants nothing.
    pub roles: Vec<Role>,
}

impl Member {
    fn from_user(u: &DirectoryUser) -> Self {
        Self {
            handle: u.handle.clone(),
            display_name: u.display_name.clone(),
            roles: u.roles.iter().filter_map(|r| Role::parse(r)).collect(),
        }
    }

    pub fn allows(&self, permission: Permission) -> bool {
        pmos_contract::permissions::allows(&self.roles, permission)
    }

    /// The display name, or the handle when there is none.
    fn name(&self) -> &str {
        if self.display_name.trim().is_empty() {
            &self.handle
        } else {
            &self.display_name
        }
    }
}

/// A live session whose person is an active user now (a fresh answer from the user database,
/// at most 10 seconds old), and for a write, a checked origin and CSRF token.
pub struct Signed {
    pub member: Member,
    pub session: Session,
    /// The session's reference for user assertions (`session::assertion_ref`): never the
    /// session id, and never logged.
    session_ref: String,
    /// Answered from the last-known-good answer (the tool server unreachable): only through
    /// [`SignedRead`], on a `GET` served from pmos-web's own store.
    pub stale: bool,
}

/// [`Signed`] for the two `GET` routes served from pmos-web's own store (`/api/me`,
/// `/api/brief`): when the tool server can't be reached, the session's last-known-good
/// answer will do for at most [`LAST_KNOWN_GOOD`], and `stale` is then set (the route
/// withholds Slack items). Every other route takes [`Signed`], which fails closed.
pub struct SignedRead(pub Signed);

impl Signed {
    fn handle(&self) -> &str {
        &self.member.handle
    }

    /// The bot runtime, signing as this PM's session, with their roles.
    fn runtime(&self, state: &AppState) -> Runtime {
        state
            .0
            .runtime
            .for_user(&self.session_ref, &self.member.roles)
    }

    /// The tool server, signing as this PM's session, with their roles.
    fn toolserver(&self, state: &AppState) -> ToolServer {
        state
            .0
            .toolserver
            .for_user(&self.session_ref, &self.member.roles)
    }

    fn need(&self, permission: Permission) -> Result<(), ApiError> {
        if self.member.allows(permission) {
            Ok(())
        } else {
            Err(ApiError::not_allowed())
        }
    }

    /// Drafts: the PM's own approvals.
    fn drafts(&self) -> Result<(), ApiError> {
        self.need(Permission::DecideOwnApproval)
    }

    fn bot(&self) -> String {
        personal_bot_name(self.handle())
    }
}

async fn live_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<(IdHash, Session)>, ApiError> {
    let Some(cookie) = session::cookie_value(headers) else {
        return Ok(None);
    };
    let id = session::hash_id(&cookie);
    let found = state
        .0
        .sessions
        .touch(&id, now(state), state.0.idle, state.0.max)
        .await
        .map_err(ApiError::store)?;
    Ok(found.map(|s| (id, s)))
}

fn users_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "users_unavailable",
        "PM OS can't check who you are right now. Try again in a minute.",
    )
}

fn entra_needs_host() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "entra_needs_host",
        "Someone with host access needs to confirm your Microsoft account before you can use \
         PM OS with your admin or auditor role.",
    )
}

fn no_longer_a_member() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "not_a_member",
        "You're not on the PM OS team any more. Ask a platform owner.",
    )
}

/// The session's person, from the user database. `allow_stale`: a `GET` may use the
/// last-known-good answer when the tool server can't be reached ([`SignedRead`]).
async fn signed_in(parts: &Parts, state: &AppState, allow_stale: bool) -> Result<Signed, ApiError> {
    let Some((id, session)) = live_session(state, &parts.headers).await? else {
        return Err(ApiError::signed_out());
    };
    let end_this = || async { state.0.sessions.remove(&id).await.map_err(ApiError::store) };
    // An Entra session is looked up by the key it signed in with, so an unlinked or
    // re-linked Microsoft account ends it; a development session by its handle.
    let entra = session.entra_tid.is_some();
    let lookup = match (&session.entra_tid, &session.entra_oid) {
        (Some(tid), Some(oid)) => Lookup::entra(tid, oid),
        (Some(_), None) => {
            // From before sign-in was keyed on `(tid, oid)`: sign in again.
            end_this().await?;
            return Err(ApiError::signed_out());
        }
        _ => Lookup::handle(&session.handle),
    };
    let (user, stale) = match state.0.directory.resolve(&lookup).await {
        Ok(u) => (u, false),
        Err(ResolveError::NotFound) if entra => {
            tracing::info!(user = %session.handle, "the session's Microsoft account is no longer linked: signed out");
            end_this().await?;
            return Err(ApiError::signed_out());
        }
        Err(ResolveError::NotFound) => {
            // Not in the user database: end every session they have, at once.
            tracing::info!(user = %session.handle, "not in the user database: signed out");
            state
                .0
                .sessions
                .remove_user(&session.handle)
                .await
                .map_err(ApiError::store)?;
            return Err(no_longer_a_member());
        }
        Err(ResolveError::Unavailable {
            last_known_good: Some(known),
            ..
        }) if allow_stale && parts.method == Method::GET && known.age <= LAST_KNOWN_GOOD => {
            tracing::warn!(user = %session.handle, age_secs = known.age.num_seconds(), "user directory unavailable: a read on the last-known-good answer");
            (known.user, true)
        }
        Err(e) => {
            let why = match e {
                ResolveError::Unavailable { why, .. } => why,
                ResolveError::RateLimited => "over the miss limit",
                ResolveError::Refused { .. } => "refused this client",
                _ => "failed",
            };
            tracing::warn!(user = %session.handle, why, "user directory unavailable: refused");
            return Err(users_unavailable());
        }
    };
    if user.handle != session.handle {
        // The Microsoft account now belongs to someone else in PM OS.
        tracing::warn!(user = %session.handle, "the session's Microsoft account is linked to another user: signed out");
        end_this().await?;
        return Err(ApiError::signed_out());
    }
    if !user.is_active() {
        tracing::info!(user = %session.handle, "deactivated: signed out");
        state
            .0
            .sessions
            .remove_user(&session.handle)
            .await
            .map_err(ApiError::store)?;
        return Err(no_longer_a_member());
    }
    if session.password_epoch.is_some() && user.password_epoch != session.password_epoch {
        // A password session from before a reset or a password change (or one whose
        // credential is gone): it ends (docs/password-sign-in.md).
        tracing::info!(user = %session.handle, "the password credential changed: signed out");
        end_this().await?;
        return Err(ApiError::signed_out());
    }
    if entra && user.entra != EntraState::Linked {
        // An Admin or Auditor whose Microsoft account the host hasn't confirmed (spec §7).
        tracing::warn!(user = %session.handle, "Entra link needs the host: signed out");
        end_this().await?;
        return Err(entra_needs_host());
    }
    if is_write(&parts.method) {
        check_write(state, &parts.headers, &session)?;
    }
    Ok(Signed {
        member: Member::from_user(&user),
        session,
        session_ref: session::assertion_ref(&id),
        stale,
    })
}

impl FromRequestParts<AppState> for Signed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        signed_in(parts, state, false).await
    }
}

impl FromRequestParts<AppState> for SignedRead {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        signed_in(parts, state, true).await.map(SignedRead)
    }
}

// Sign-in.

#[derive(Deserialize)]
struct DevLogin {
    handle: String,
}

/// `POST /api/auth/dev-login {handle}`: development sign-in (loopback only), resolved in the
/// user database by handle. Fails closed: with the tool server unreachable it answers
/// `503 users_unavailable`.
async fn dev_login(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> ApiResult {
    let Some(provider) = state.0.providers.iter().find(|p| p.name() == "dev") else {
        return Err(ApiError::not_found());
    };
    if !auth::same_origin(&headers, state.0.public_url.as_deref()) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "bad_origin",
            "That request didn't come from PM OS.",
        ));
    }
    // Login CSRF: a cross-site form can't send JSON, so sign-in always needs it.
    if !is_json(&headers) {
        return Err(bad_content_type());
    }
    let req: DevLogin = parse(&body)?;
    let claimed = match provider
        .authenticate(&SignInInput::Dev { handle: req.handle })
        .await
    {
        Ok(c) => c,
        Err(SignInError::Invalid(why)) => return Err(ApiError::bad_request(why)),
        Err(SignInError::Unsupported | SignInError::Rejected(_)) => {
            return Err(ApiError::not_found())
        }
    };
    let refuse = |reason: &'static str, attempted: bool| {
        let state = state.clone();
        let name = claimed.0.clone();
        async move {
            tracing::info!(provider = "dev", reason, "sign-in refused");
            record_sign_in(
                &state,
                None,
                SignInEvent::Refused {
                    provider: "dev",
                    reason,
                    attempted: attempted.then_some(name.as_str()),
                    member: None,
                },
            )
            .await;
        }
    };
    let user = match state.0.directory.by_handle(&claimed.0).await {
        Ok(u) if u.is_active() => u,
        Ok(_) => {
            refuse("deactivated", false).await;
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "not_a_member",
                "That person's access to PM OS is turned off.",
            ));
        }
        Err(ResolveError::NotFound | ResolveError::Invalid) => {
            refuse("not_a_member", true).await;
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "not_a_member",
                "That person isn't on the PM OS team.",
            ));
        }
        Err(_) => {
            refuse("users_unavailable", false).await;
            return Err(users_unavailable());
        }
    };
    let id = start_session(&state, &headers, &user, provider.name(), None, None).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session::set_cookie(&id))],
    )
        .into_response())
}

/// Why an Entra sign-in was refused: the signed-out page's `reason`, and whether the name
/// tried goes into the audit event (as its keyed hash).
struct EntraRefusal {
    reason: &'static str,
    attempted: bool,
}

impl EntraRefusal {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            attempted: false,
        }
    }

    fn not_a_member() -> Self {
        Self {
            reason: "not_a_member",
            attempted: true,
        }
    }
}

/// Who a checked ID token is in PM OS (`docs/user-database-spec.md` §7): by `(tid, oid)`;
/// failing that, a first link by `preferred_username` only (never `email`). The first link
/// sends the tool server the raw ID token and this sign-in's `nonce`: it validates the token
/// itself, links only the identity the token names, and audits it. Fails closed when the tool
/// server can't be reached.
async fn entra_person(
    state: &AppState,
    signed: &entra::SignedIn,
    nonce: &str,
) -> Result<DirectoryUser, EntraRefusal> {
    let identity = &signed.identity;
    let dir = &state.0.directory;
    match dir.by_entra(&identity.tid, &identity.oid).await {
        Ok(user) => {
            // 1. A known identity: names in the token are ignored from here on.
            if !user.is_active() {
                return Err(EntraRefusal::new("deactivated"));
            }
            if user.entra != EntraState::Linked {
                return Err(EntraRefusal::new("entra_needs_host"));
            }
            // The token's name is another user's sign-in name: flagged for admins (§7 step
            // 3), but this person is who their `oid` says.
            if let Some(name) = &identity.preferred_username {
                if let Ok(other) = dir.by_sign_in_name(name).await {
                    if other.handle != user.handle {
                        tracing::warn!(user = %user.handle, other = %other.handle, "Microsoft sign-in name is another user's: flagged");
                        record_identity_conflict(state, &other.handle).await;
                    }
                }
            }
            return Ok(user);
        }
        Err(ResolveError::NotFound) => {}
        Err(_) => return Err(EntraRefusal::new("users_unavailable")),
    }
    // 2. A first link, by `preferred_username` only. No fallback to `email`.
    let Some(name) = identity.preferred_username.as_deref() else {
        return Err(EntraRefusal::not_a_member());
    };
    let user = match dir.by_sign_in_name(name).await {
        Ok(u) => u,
        Err(ResolveError::NotFound | ResolveError::Invalid) => {
            return Err(EntraRefusal::not_a_member())
        }
        Err(_) => return Err(EntraRefusal::new("users_unavailable")),
    };
    match dir
        .link_entra(
            &user.handle,
            &identity.tid,
            &identity.oid,
            &signed.id_token,
            nonce,
        )
        .await
    {
        Ok(linked) => {
            if linked.first_link {
                tracing::info!(user = %linked.user.handle, "first Microsoft sign-in: linked");
            }
            if !linked.user.is_active() {
                return Err(EntraRefusal::new("deactivated"));
            }
            if linked.user.entra != EntraState::Linked {
                return Err(EntraRefusal::new("entra_needs_host"));
            }
            Ok(linked.user)
        }
        Err(LinkError::Refused(why)) => Err(match why {
            LinkRefusal::NotActive => EntraRefusal::new("deactivated"),
            LinkRefusal::SignInUnconfirmed => EntraRefusal::new("sign_in_unconfirmed"),
            LinkRefusal::HostOnly => EntraRefusal::new("entra_needs_host"),
            // 3. A conflict: the tool server audited `users.identity_conflict` and flags it.
            LinkRefusal::Linked | LinkRefusal::Taken => {
                tracing::warn!(user = %user.handle, "Microsoft sign-in conflicts with an existing link: refused");
                EntraRefusal::not_a_member()
            }
            // The tool server's own check of the token disagreed with ours (a configuration
            // mismatch between the two, or a token pmos-web shouldn't have accepted): refused,
            // and the log says so.
            LinkRefusal::InvalidToken => {
                tracing::error!(user = %user.handle, "the tool server refused an ID token pmos-web accepted: check that both have the same tenant and client id (docs/entra-sign-in.md)");
                EntraRefusal::new("failed")
            }
            // 4. Anything else.
            _ => EntraRefusal::not_a_member(),
        }),
        Err(LinkError::RateLimited | LinkError::Unavailable(_)) => {
            Err(EntraRefusal::new("users_unavailable"))
        }
    }
}

/// A new session for `user`, whichever provider signed them in: the cookie value. Ends
/// whatever session this browser had (rotation). `entra` is the token's `(oid, tid)` for an
/// Entra ID sign-in: the key the session is checked by on every request. `password_epoch`
/// is the password credential's epoch for a password sign-in, checked on every request too.
async fn start_session(
    state: &AppState,
    headers: &HeaderMap,
    user: &DirectoryUser,
    provider: &str,
    entra: Option<(String, String)>,
    password_epoch: Option<i64>,
) -> Result<String, ApiError> {
    if let Some(old) = session::cookie_value(headers) {
        state
            .0
            .sessions
            .remove(&session::hash_id(&old))
            .await
            .map_err(ApiError::store)?;
    }
    brief::housekeeping(state).await;
    let id = session::random_token();
    let at = now(state);
    let hashed = session::hash_id(&id);
    state
        .0
        .sessions
        .insert(
            hashed,
            Session {
                handle: user.handle.clone(),
                csrf_token: session::random_token(),
                created_at: at,
                last_seen: at,
                entra_oid: entra.as_ref().map(|(oid, _)| oid.clone()),
                entra_tid: entra.map(|(_, tid)| tid),
                password_epoch,
            },
        )
        .await
        .map_err(ApiError::store)?;
    tracing::info!(user = %user.handle, provider, "signed in");
    let roles = Member::from_user(user).roles;
    record_sign_in(
        state,
        Some((&user.handle, &session::assertion_ref(&hashed), &roles)),
        SignInEvent::SignIn { provider },
    )
    .await;
    Ok(id)
}

/// Flag a known Microsoft account signing in with a name that is `other`'s sign-in name
/// (spec §7 step 3): `users.identity_conflict` at the tool server, through the sign-in events
/// route. Best effort, like every sign-in event.
async fn record_identity_conflict(state: &AppState, other: &str) {
    let body = json!({
        "action": "users.identity_conflict",
        "provider": "entra",
        "member": other,
    });
    let recorded = tokio::time::timeout(
        SIGN_IN_RECORD_TIMEOUT,
        state.0.toolserver.record_sign_in(None, &body),
    )
    .await;
    if !matches!(recorded, Ok(Ok(()))) {
        state
            .0
            .sign_in_record_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::error!(
            action = "users.identity_conflict",
            "AUDIT: an identity conflict wasn't recorded"
        );
    }
}

/// How long recording a sign-in event may take before sign-in goes on without it.
pub const SIGN_IN_RECORD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A sign-in event for the audit log (`docs/web-api.md`, "Sign-in events"): fixed action
/// names and codes, never free text, no IP address, no token, and for a refusal never the
/// name tried, only a short keyed hash of it.
enum SignInEvent<'a> {
    SignIn {
        provider: &'a str,
    },
    SignOut {
        provider: &'a str,
    },
    Refused {
        provider: &'a str,
        /// The signed-out page's `reason`: `not_a_member`, `deactivated`, `failed`, ….
        reason: &'a str,
        /// The handle or email tried, hashed before it leaves this server.
        attempted: Option<&'a str>,
        /// For `wrong_account` (no longer sent): the member whose email the account used.
        member: Option<&'a str>,
    },
}

/// Record `event` at the tool server (`POST /internal/admin/events`), for `member` (its
/// handle, session reference and roles, for the user assertion) on a sign-in or sign-out.
///
/// **Fail-open, on purpose.** A sign-in or sign-out goes ahead when this fails (after at most
/// [`SIGN_IN_RECORD_TIMEOUT`]): the failure is logged as an error and counted on `/readyz`
/// (`unrecorded_sign_in_events`). Signing out must never be blocked. Signing in isn't either,
/// because a session alone reaches nothing: everything a signed-in person does goes through
/// the tool server or the bot runtime, which audit each call themselves, so a missing
/// sign-in event hides no action. Failing closed would turn a tool server blip into a
/// lockout of everyone, the admins who would look into it included.
async fn record_sign_in(
    state: &AppState,
    member: Option<(&str, &str, &[Role])>,
    event: SignInEvent<'_>,
) {
    let (action, body) = match &event {
        SignInEvent::SignIn { provider } => (
            "web.sign_in",
            json!({"action": "web.sign_in", "provider": provider}),
        ),
        SignInEvent::SignOut { provider } => (
            "web.sign_out",
            json!({"action": "web.sign_out", "provider": provider}),
        ),
        SignInEvent::Refused {
            provider,
            reason,
            attempted,
            member,
        } => {
            let mut body = json!({
                "action": "web.sign_in_refused",
                "provider": provider,
                "reason": reason,
            });
            if let Some(name) = attempted.filter(|n| !n.trim().is_empty()) {
                body["attempt"] = json!(state.0.toolserver.attempt_hash(name));
            }
            if let Some(m) = member {
                body["member"] = json!(m);
            }
            ("web.sign_in_refused", body)
        }
    };
    let (toolserver, handle) = match member {
        Some((handle, session_ref, roles)) => (
            state.0.toolserver.for_user(session_ref, roles),
            Some(handle),
        ),
        None => (state.0.toolserver.clone(), None),
    };
    let recorded = tokio::time::timeout(
        SIGN_IN_RECORD_TIMEOUT,
        toolserver.record_sign_in(handle, &body),
    )
    .await;
    let why = match recorded {
        Ok(Ok(())) => return,
        Ok(Err(UpstreamError::Refused { status, code, .. })) => {
            format!("refused ({status} {})", code.unwrap_or_default())
        }
        Ok(Err(UpstreamError::NotFound)) => "no such route (an older tool server)".to_string(),
        Ok(Err(UpstreamError::Failed { why, .. } | UpstreamError::Unknown { why, .. })) => why,
        Err(_) => "timed out".to_string(),
    };
    state
        .0
        .sign_in_record_failures
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::error!(action, error = %why, "AUDIT: a sign-in event wasn't recorded");
}

/// `GET /api/auth/providers` (no session): which sign-in buttons the app shows.
async fn auth_providers(State(state): State<AppState>) -> ApiResult {
    let names: Vec<&str> = state.0.providers.iter().map(|p| p.name()).collect();
    Ok(Json(json!({ "providers": names })).into_response())
}

/// Where the app explains why sign-in didn't finish.
fn signed_out_at(reason: &str) -> String {
    format!("/#/signed-out?reason={reason}")
}

/// A `303` to `location`, setting `cookies`. Only the authorize URL is absolute; every other
/// location is a same-origin path.
fn redirect(location: &str, cookies: &[String]) -> Response {
    let mut res = StatusCode::SEE_OTHER.into_response();
    let h = res.headers_mut();
    h.insert(
        header::LOCATION,
        HeaderValue::from_str(location).unwrap_or(HeaderValue::from_static("/")),
    );
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            h.append(header::SET_COOKIE, v);
        }
    }
    res
}

#[derive(Deserialize)]
struct LoginQuery {
    return_to: Option<String>,
}

/// `GET /api/auth/login[?return_to=/path]`: off to Entra ID. A top-level navigation, so a
/// failure redirects to the app's signed-out page rather than answering JSON.
async fn entra_login(
    State(state): State<AppState>,
    query: Result<axum::extract::Query<LoginQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult {
    let Some(entra) = state.0.entra.clone() else {
        return Err(ApiError::not_found());
    };
    let return_to = query.ok().and_then(|q| q.0.return_to);
    match entra.begin(now(&state), return_to.as_deref()).await {
        Ok((authorize, cookie)) => Ok(redirect(&authorize, &[cookie])),
        Err(f) => {
            tracing::warn!(provider = "entra", why = f.why(), "sign-in couldn't start");
            Ok(redirect(&signed_out_at(f.reason()), &[]))
        }
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// `GET /api/auth/callback?code&state`: Microsoft sends the browser back here. A cross-site
/// GET: no origin check applies, and `state`, matched once against the signed flow cookie
/// this browser got at `/api/auth/login`, is its CSRF defence. Always clears that cookie;
/// always redirects within this origin.
async fn entra_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<axum::extract::Query<CallbackQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult {
    let Some(entra) = state.0.entra.clone() else {
        return Err(ApiError::not_found());
    };
    let clear = entra::clear_flow_cookie();
    let fail = |f: Failure| {
        tracing::warn!(provider = "entra", why = f.why(), "sign-in refused");
        Ok(redirect(
            &signed_out_at(f.reason()),
            std::slice::from_ref(&clear),
        ))
    };
    let Ok(axum::extract::Query(q)) = query else {
        return fail(Failure::Failed("malformed callback"));
    };
    if let Some(error) = q.error.as_deref() {
        return fail(if error == "access_denied" {
            Failure::Cancelled
        } else {
            Failure::Failed("the identity provider answered an error")
        });
    }
    let (Some(code), Some(flow_state)) = (q.code, q.state) else {
        return fail(Failure::Failed("callback without code or state"));
    };
    let at = now(&state);
    let started = match entra.finish(&headers, &flow_state, at) {
        Ok(s) => s,
        Err(f) => return fail(f),
    };
    let input = SignInInput::Entra {
        code,
        nonce: started.nonce,
        verifier: started.verifier,
        now: at,
    };
    let SignInInput::Entra {
        code,
        nonce,
        verifier,
        now,
    } = &input
    else {
        return fail(Failure::Failed("unsupported input"));
    };
    // From here on the browser had started a sign-in with this server (its `state` matched),
    // so a refusal is recorded. Earlier failures (no flow, a forged or replayed callback) are
    // only logged: anyone can send those, and the audit log isn't theirs to fill.
    let signed = match entra.sign_in_identity(code, nonce, verifier, *now).await {
        Ok(signed) => signed,
        Err(f) => {
            record_sign_in(
                &state,
                None,
                SignInEvent::Refused {
                    provider: "entra",
                    reason: f.reason(),
                    attempted: None,
                    member: None,
                },
            )
            .await;
            return fail(f);
        }
    };
    let user = match entra_person(&state, &signed, nonce).await {
        Ok(user) => user,
        Err(refusal) => {
            tracing::info!(
                provider = "entra",
                reason = refusal.reason,
                "sign-in refused"
            );
            record_sign_in(
                &state,
                None,
                SignInEvent::Refused {
                    provider: "entra",
                    reason: refusal.reason,
                    attempted: signed
                        .identity
                        .preferred_username
                        .as_deref()
                        .filter(|_| refusal.attempted),
                    member: None,
                },
            )
            .await;
            return Ok(redirect(
                &signed_out_at(refusal.reason),
                std::slice::from_ref(&clear),
            ));
        }
    };
    let entra_id = (signed.identity.oid.clone(), signed.identity.tid.clone());
    let id = start_session(&state, &headers, &user, entra.name(), Some(entra_id), None).await?;
    Ok(redirect(
        &entra::safe_return(Some(&started.return_to)),
        &[clear, session::set_cookie(&id)],
    ))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult {
    if let Some((id, session)) = live_session(&state, &headers).await? {
        check_write(&state, &headers, &session)?;
        state
            .0
            .sessions
            .remove(&id)
            .await
            .map_err(ApiError::store)?;
        tracing::info!(user = %session.handle, "signed out");
        // Never blocks the sign-out: a failure is logged and counted.
        let provider = if session.entra_tid.is_some() {
            "entra"
        } else if session.password_epoch.is_some() {
            "password"
        } else {
            "dev"
        };
        // The roles in the event's assertion are informational: whatever answer is kept, with
        // no call to the tool server (a sign-out never waits on it).
        let roles = state
            .0
            .directory
            .peek(&Lookup::handle(&session.handle))
            .map(|u| Member::from_user(&u).roles)
            .unwrap_or_default();
        record_sign_in(
            &state,
            Some((&session.handle, &session::assertion_ref(&id), &roles)),
            SignInEvent::SignOut { provider },
        )
        .await;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session::clear_cookie())],
    )
        .into_response())
}

// Me.

fn me_json(signed: &Signed, st: &UserState) -> Response {
    Json(json!({
        "handle": signed.member.handle,
        // Untrusted text from the user database, rendered as plain text by the app.
        "display_name": signed.member.display_name,
        "name": signed.member.name(),
        "roles": signed.member.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        "permissions": pmos_contract::permissions::granted(&signed.member.roles),
        // Holds the `reviewer` role: show the Reviews entry (`/api/reviews`). Not the
        // `recommend-on-approval` permission, which an admin holds too without reviewing.
        "reviewer": signed.member.roles.contains(&Role::Reviewer),
        "csrf_token": signed.session.csrf_token,
        "first_visit": !st.opened_a_draft,
        "hints_seen": st.hints_seen.iter().collect::<Vec<_>>(),
        "nudges": st.nudges,
        "mascot": st.mascot,
    }))
    .into_response()
}

/// `GET /api/me`: from the user database (a fresh answer, or on the last-known-good path at
/// most 5 minutes old) and pmos-web's own store.
async fn me(State(state): State<AppState>, SignedRead(signed): SignedRead) -> ApiResult {
    let st = state
        .0
        .settings
        .get(signed.handle())
        .await
        .map_err(ApiError::store)?;
    Ok(me_json(&signed, &st))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SettingsBody {
    nudges: Option<bool>,
    mascot: Option<bool>,
}

async fn put_settings(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    let req: SettingsBody = parse_optional(&body)?;
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                if let Some(n) = req.nudges {
                    s.nudges = n;
                }
                if let Some(m) = req.mascot {
                    s.mascot = m;
                }
            }),
        )
        .await
        .map_err(ApiError::store)?;
    Ok(me_json(&signed, &st))
}

#[derive(Deserialize)]
struct HintBody {
    seen: String,
}

async fn post_hint(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    let req: HintBody = parse(&body)?;
    if !HINTS.contains(&req.seen.as_str()) {
        return Err(ApiError::bad_request(format!(
            "`seen` is one of {}.",
            HINTS.join(", ")
        )));
    }
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                s.hints_seen.insert(req.seen);
            }),
        )
        .await
        .map_err(ApiError::store)?;
    Ok(me_json(&signed, &st))
}

async fn delete_hints(State(state): State<AppState>, signed: Signed) -> ApiResult {
    let st = state
        .0
        .settings
        .update(signed.handle(), Box::new(|s| s.hints_seen.clear()))
        .await
        .map_err(ApiError::store)?;
    Ok(me_json(&signed, &st))
}

// Drafts.

/// A draft the PM may see: their practice draft or one of their approval items.
enum Found {
    Practice(crate::settings::PracticeDraft),
    Item(Box<Approval>),
}

async fn settings_of(state: &AppState, handle: &str) -> Result<UserState, ApiError> {
    state.0.settings.get(handle).await.map_err(ApiError::store)
}

/// The draft `id` if it is this PM's; otherwise `404`, whether or not it exists.
async fn find(state: &AppState, signed: &Signed, id: &str) -> Result<Found, ApiError> {
    if !valid_id(id) {
        return Err(ApiError::not_found());
    }
    if drafts::is_practice_id(id) {
        let st = settings_of(state, signed.handle()).await?;
        return match st.practice {
            Some(p) if p.id == id => Ok(Found::Practice(p)),
            _ => Err(ApiError::not_found()),
        };
    }
    let item = signed
        .toolserver(state)
        .approval_for(signed.handle(), id)
        .await
        .map_err(ApiError::upstream)?;
    if item.user != signed.handle() || drafts::kind(&item.tool).is_none() {
        return Err(ApiError::not_found());
    }
    Ok(Found::Item(Box::new(item)))
}

/// The draft without its sources (no upstream call).
fn base_draft(found: &Found, signed: &Signed, st: &UserState) -> Result<Draft, ApiError> {
    match found {
        Found::Practice(p) => Ok(drafts::practice_draft(p, &signed.bot(), st)),
        Found::Item(item) => drafts::draft(item, st).ok_or_else(ApiError::not_found),
    }
}

/// The most Slack audiences remembered from refusals (`needs_external_confirm`,
/// `audience_blocked`). Past it the memory starts again: it is only for showing.
const MAX_AUDIENCES: usize = 5_000;

/// Remember what a tool server refusal said of a draft's Slack audience, for the draft's
/// `audience` (the tool server keeps only `external`). In memory: a restart forgets it, as
/// the tool server forgets its own audience checks.
fn remember_audience(state: &AppState, id: &str, seen: RefusedAudience) {
    let mut all = state.0.audiences.lock().expect("audiences lock");
    if all.len() >= MAX_AUDIENCES && !all.contains_key(id) {
        all.clear();
    }
    all.insert(id.to_string(), seen);
}

/// A Slack draft's `audience` with what the last refusal for it counted.
fn with_audience(state: &AppState, mut draft: Draft) -> Draft {
    if let Some(audience) = draft.audience.as_mut() {
        let seen = state
            .0
            .audiences
            .lock()
            .expect("audiences lock")
            .get(&draft.summary.id)
            .copied();
        if let Some(seen) = seen {
            audience.members = audience.members.or(seen.members);
            audience.external = audience.external.or(seen.external);
            draft.external = audience.external.unwrap_or(draft.external);
        }
    }
    draft
}

/// The whole draft, with what the bot read for it. A practice draft has no sources and
/// never reaches the tool server.
async fn render(
    state: &AppState,
    found: &Found,
    signed: &Signed,
    st: &UserState,
) -> Result<Draft, ApiError> {
    match found {
        Found::Practice(_) => base_draft(found, signed, st),
        Found::Item(item) => item_draft(state, signed, item, st).await,
    }
}

async fn item_draft(
    state: &AppState,
    signed: &Signed,
    item: &Approval,
    st: &UserState,
) -> Result<Draft, ApiError> {
    let draft = drafts::draft(item, st).ok_or_else(ApiError::not_found)?;
    let found = match signed
        .toolserver(state)
        .sources(&item.id, signed.handle())
        .await
    {
        Ok(found) => found,
        // The draft is still shown, without citations; the app says it can't tell what the
        // bot read.
        Err(e) => {
            tracing::warn!(error = ?e, draft = %item.id, "sources unavailable");
            Vec::new()
        }
    };
    let (draft, citations) = drafts::with_sources(draft, item, found);
    let draft = with_audience(state, draft);
    if citations.dropped > 0 {
        // Counts only: never the draft's text or the refs.
        tracing::info!(
            draft = %item.id,
            cited = citations.kept,
            dropped = citations.dropped,
            "source markers dropped: not a source this task read"
        );
    }
    Ok(draft)
}

fn is_pending(found: &Found) -> bool {
    match found {
        Found::Practice(p) => p.status == "pending",
        Found::Item(item) => item.status == "pending",
    }
}

fn draft_response(draft: &Draft) -> Response {
    Json(draft).into_response()
}

async fn list_drafts(State(state): State<AppState>, signed: Signed) -> ApiResult {
    signed.drafts()?;
    let inbox = signed
        .toolserver(&state)
        .approvals(signed.handle())
        .await
        .map_err(ApiError::upstream)?;
    if inbox.truncated {
        tracing::warn!(user = %signed.handle(), total = ?inbox.total_pending, "the tool server cut this inbox at its safety cap");
    }
    let items = inbox.items;
    let st = settings_of(&state, signed.handle()).await?;
    let mut pending = Vec::new();
    let mut decided = Vec::new();
    for item in items.iter().filter(|i| i.user == signed.handle()) {
        let Some(summary) = drafts::summary(item) else {
            continue;
        };
        if item.status == "pending" {
            pending.push((item.created_at.clone(), summary));
        } else {
            let when = item
                .decided_at
                .clone()
                .unwrap_or_else(|| item.created_at.clone());
            decided.push((when, summary));
        }
    }
    if let Some(p) = &st.practice {
        let d = drafts::practice_draft(p, &signed.bot(), &st);
        if p.status == "pending" {
            pending.push((p.created_at.clone(), d.summary));
        } else {
            let when = p.decided_at.clone().unwrap_or_else(|| p.created_at.clone());
            decided.push((when, d.summary));
        }
    }
    pending.sort_by(|a, b| b.0.cmp(&a.0));
    decided.sort_by(|a, b| b.0.cmp(&a.0));
    decided.truncate(drafts::RECENT_DECIDED);
    // Suggestions (and any edit kept from before edits were saved at the tool server) of
    // drafts decided elsewhere, in Slack say, are no longer needed.
    let stale: Vec<String> = items
        .iter()
        .filter(|i| i.status != "pending")
        .map(|i| i.id.clone())
        .filter(|id| st.edits.contains_key(id) || st.suggestions.contains_key(id))
        .collect();
    if !stale.is_empty() {
        state
            .0
            .settings
            .update(
                signed.handle(),
                Box::new(move |s| {
                    for id in &stale {
                        s.forget_draft(id);
                    }
                }),
            )
            .await
            .map_err(ApiError::store)?;
    }
    let all: Vec<_> = pending.into_iter().chain(decided).map(|(_, s)| s).collect();
    // `truncated`: the tool server left pending drafts out past its safety cap, and
    // `total_pending` says how many there are (R12). Never true below that cap.
    let mut body = json!({ "drafts": all, "truncated": inbox.truncated });
    if let Some(total) = inbox.total_pending {
        body["total_pending"] = json!(total);
    }
    Ok(Json(body).into_response())
}

async fn get_draft(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    signed.drafts()?;
    let found = find(&state, &signed, &id).await?;
    let st = state
        .0
        .settings
        .update(signed.handle(), Box::new(|s| s.opened_a_draft = true))
        .await
        .map_err(ApiError::store)?;
    Ok(draft_response(&render(&state, &found, &signed, &st).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditBody {
    #[serde(default)]
    title: Option<String>,
    body: String,
}

async fn put_draft(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    signed.drafts()?;
    let req: EditBody = parse(&body)?;
    // The PM's text has no source markers (`[[src-…]]` or `[[sN]]`): they are removed
    // before anything is checked or saved, so an edited draft has no citations.
    let req = EditBody {
        title: req.title.as_deref().map(pmos_contract::cite::strip),
        body: pmos_contract::cite::strip(&req.body),
    };
    if req.body.chars().count() > MAX_BODY {
        return Err(ApiError::bad_request("The text is too long."));
    }
    if req
        .title
        .as_ref()
        .is_some_and(|t| t.trim().is_empty() || t.chars().count() > MAX_TITLE)
    {
        return Err(ApiError::bad_request(
            "The title must be 1 to 500 characters.",
        ));
    }
    let found = find(&state, &signed, &id).await?;
    if !is_pending(&found) {
        return Err(ApiError::not_pending());
    }
    // A real draft's edit is saved on the item at the tool server, so the Slack card and the
    // console show it, and an approve anywhere writes it. Only a practice draft's stays here.
    if let Found::Item(item) = &found {
        let title = req.title.filter(|_| drafts::has_title(&item.tool));
        let saved = save_edit(
            &state,
            &signed,
            item,
            DraftEdit {
                title,
                body: req.body,
            },
            None,
        )
        .await?;
        let st = settings_of(&state, signed.handle()).await?;
        let found = Found::Item(Box::new(saved));
        return Ok(draft_response(&render(&state, &found, &signed, &st).await?));
    }
    let key = id.clone();
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                let title = req
                    .title
                    .or_else(|| s.edits.get(&key).and_then(|e| e.title.clone()));
                s.edits.insert(
                    key,
                    DraftEdit {
                        title,
                        body: req.body,
                    },
                );
            }),
        )
        .await
        .map_err(ApiError::store)?;
    Ok(draft_response(&render(&state, &found, &signed, &st).await?))
}

/// Save the PM's edit of `item` at the tool server (`PUT /internal/approvals/{id}/draft`):
/// the item's arguments with the new body (and title, for a draft that has one), where it
/// goes unchanged. Answers the item as the tool server now has it.
///
/// `revision` names the bot's revision task when the text is an accepted suggestion: the tool
/// server then raises the draft's level to what that task read (R2). Text the PM typed has
/// no level a machine knows; the draft keeps its own, which a save never lowers.
async fn save_edit(
    state: &AppState,
    signed: &Signed,
    item: &Approval,
    edit: DraftEdit,
    revision: Option<&crate::upstream::Revision>,
) -> Result<Approval, ApiError> {
    let args = drafts::edited_args(&item.tool, &item.args, &edit);
    let saved = signed
        .toolserver(state)
        .save_draft(signed.handle(), &item.id, &args, revision)
        .await
        .map_err(|e| match e {
            UpstreamError::Refused {
                status: 409,
                code: Some(code),
                ..
            } if code == "expired" => ApiError::new(
                StatusCode::CONFLICT,
                "expired",
                "This draft expired before it was decided. Nothing was sent.",
            ),
            UpstreamError::Refused { status: 409, .. } => ApiError::not_pending(),
            UpstreamError::Refused {
                status: 400 | 422,
                code,
                ..
            } => {
                tracing::warn!(user = %signed.handle(), draft = %item.id, code = ?code, "edit refused");
                ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "bad_edit",
                    "This edit isn't valid for where the draft goes. Nothing was saved.",
                )
            }
            e => ApiError::upstream(e),
        })?;
    if saved.user != signed.handle() || saved.id != item.id {
        return Err(ApiError::upstream(UpstreamError::Failed {
            service: "tool server",
            why: "answered with another item".into(),
        }));
    }
    tracing::info!(user = %signed.handle(), draft = %item.id, "edit saved");
    Ok(saved)
}

#[derive(Deserialize)]
struct ChangeBody {
    instruction: String,
}

async fn post_changes(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    signed.drafts()?;
    signed.need(Permission::AskBot)?;
    let req: ChangeBody = parse(&body)?;
    let instruction = req.instruction.trim().to_string();
    if instruction.is_empty() || instruction.chars().count() > MAX_INSTRUCTION {
        return Err(ApiError::bad_request(
            "Say what to change, in at most 2,000 characters.",
        ));
    }
    let found = find(&state, &signed, &id).await?;
    if !is_pending(&found) {
        return Err(ApiError::not_pending());
    }
    let st = settings_of(&state, signed.handle()).await?;
    let current = base_draft(&found, &signed, &st)?;

    // Reserve the slot first, so two requests can't both start a change.
    let practice = matches!(found, Found::Practice(_));
    let reserved = StoredSuggestion {
        task_id: None,
        instruction: instruction.clone(),
        status: if practice { "ready" } else { "working" }.into(),
        body: practice.then(|| format!("{}{}", current.body, drafts::PRACTICE_SUGGESTION_NOTE)),
        error: None,
    };
    let mut won = false;
    let key = id.clone();
    let slot = reserved.clone();
    let won_ref = &mut won;
    state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                if s.suggestions
                    .get(&key)
                    .is_some_and(|x| x.status == "working")
                {
                    return;
                }
                s.suggestions.insert(key, slot);
                *won_ref = true;
            }),
        )
        .await
        .map_err(ApiError::store)?;
    if !won {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "change_in_progress",
            "Your bot is already working on a change to this draft.",
        ));
    }
    if practice {
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "suggestion": Suggestion::from(&reserved) })),
        )
            .into_response());
    }

    let prompt = drafts::revision_prompt(
        &current.summary.kind.replace('_', " "),
        &current.summary.title,
        &current.body,
        &instruction,
    );
    // The prompt carries the draft's text, so the revision's session starts at the draft's
    // level (unrecorded counts as restricted): the W6 carry-forward, for revisions (R2).
    let floor = match &found {
        Found::Item(item) => item.sources_sensitivity.as_deref().unwrap_or("restricted"),
        Found::Practice(_) => "public",
    };
    let created = signed
        .runtime(&state)
        .create_task_at(signed.handle(), &signed.bot(), &prompt, Some(floor))
        .await;
    let key = id.clone();
    match created {
        Ok(task) => {
            let task_id = task.id.clone();
            let st = state
                .0
                .settings
                .update(
                    signed.handle(),
                    Box::new(move |s| {
                        s.revision_tasks.insert(task_id.clone());
                        if let Some(x) = s.suggestions.get_mut(&key) {
                            x.task_id = Some(task_id);
                        }
                    }),
                )
                .await
                .map_err(ApiError::store)?;
            tracing::info!(user = %signed.handle(), draft = %id, task = %task.id, "change requested");
            let suggestion = st.suggestions.get(&id).map(Suggestion::from);
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({ "suggestion": suggestion })),
            )
                .into_response())
        }
        Err(e) => {
            state
                .0
                .settings
                .update(
                    signed.handle(),
                    Box::new(move |s| {
                        s.suggestions.remove(&key);
                    }),
                )
                .await
                .map_err(ApiError::store)?;
            Err(ApiError::upstream(e))
        }
    }
}

/// The task's result without a code fence around it, if the bot added one.
fn unfence(text: &str) -> String {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("```") {
        if let Some(inner) = rest.strip_suffix("```") {
            if let Some((_, body)) = inner.split_once('\n') {
                return body.trim_end().to_string();
            }
        }
    }
    t.to_string()
}

/// What a finished revision task means for the suggestion, or `None` while it runs.
fn settle(task: &Task) -> Option<(String, Option<String>, Option<String>)> {
    let failed = |why: &str| Some(("failed".into(), None, Some(why.to_string())));
    match task.status.as_str() {
        "queued" | "working" => None,
        "done" => match task.result.as_deref().map(unfence) {
            Some(body) if !body.is_empty() => Some(("ready".into(), Some(body), None)),
            _ => failed("Your bot finished without revised text. Try asking again."),
        },
        "needs_approval" => failed(
            "Your bot drafted a new write instead of answering with the text. Reject that \
             draft, then ask again.",
        ),
        _ => match task.error.as_deref().filter(|e| !e.is_empty()) {
            Some(e) => failed(e),
            None => failed("Your bot couldn't make the change. Try asking again."),
        },
    }
}

/// The PM's suggestion for `id`, brought up to date with its task.
async fn current_suggestion(
    state: &AppState,
    signed: &Signed,
    id: &str,
) -> Result<Option<StoredSuggestion>, ApiError> {
    let st = settings_of(state, signed.handle()).await?;
    let Some(sug) = st.suggestions.get(id).cloned() else {
        return Ok(None);
    };
    let (true, Some(task_id)) = (sug.status == "working", sug.task_id.clone()) else {
        return Ok(Some(sug));
    };
    let task = match signed.runtime(state).task(signed.handle(), &task_id).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = ?e, task = %task_id, "revision task unavailable");
            return Ok(Some(sug));
        }
    };
    let Some((status, body, error)) = settle(&task) else {
        return Ok(Some(sug));
    };
    let key = id.to_string();
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                if let Some(x) = s
                    .suggestions
                    .get_mut(&key)
                    .filter(|x| x.task_id.as_deref() == Some(task_id.as_str()))
                {
                    x.status = status;
                    x.body = body;
                    x.error = error;
                }
            }),
        )
        .await
        .map_err(ApiError::store)?;
    Ok(st.suggestions.get(id).cloned())
}

async fn get_suggestion(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    signed.drafts()?;
    find(&state, &signed, &id).await?;
    let sug = current_suggestion(&state, &signed, &id).await?;
    Ok(Json(json!({ "suggestion": sug.as_ref().map(Suggestion::from) })).into_response())
}

async fn accept_suggestion(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    signed.drafts()?;
    let found = find(&state, &signed, &id).await?;
    if !is_pending(&found) {
        return Err(ApiError::not_pending());
    }
    let ready = current_suggestion(&state, &signed, &id)
        .await?
        .filter(|s| s.status == "ready");
    let task_id = ready.as_ref().and_then(|s| s.task_id.clone());
    let Some(body) = ready.and_then(|s| s.body) else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_suggestion",
            "There's no suggested text to accept yet.",
        ));
    };
    let body = pmos_contract::cite::strip(&body);
    // A real draft: the accepted text is saved at the tool server, as an edit is, and then
    // the suggestion (which stays here) is gone.
    let found = match found {
        Found::Item(item) => {
            if body.chars().count() > MAX_BODY {
                return Err(ApiError::bad_request("The text is too long."));
            }
            // The text is the revision task's answer: name the task, so the tool server
            // raises the draft to what it read (R2). Its recorded level goes along as a floor
            // only; the tool server's own sessions decide. A suggestion without a task can't
            // be accepted: nothing would say what its text drew on.
            let Some(run_id) = task_id else {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "no_suggestion",
                    "There's no suggested text to accept yet.",
                ));
            };
            let sensitivity = signed
                .runtime(&state)
                .task(signed.handle(), &run_id)
                .await
                .ok()
                .and_then(|t| t.sources_sensitivity);
            let revision = crate::upstream::Revision {
                run_id,
                sensitivity,
            };
            Found::Item(Box::new(
                save_edit(
                    &state,
                    &signed,
                    &item,
                    DraftEdit {
                        title: None,
                        body: body.clone(),
                    },
                    Some(&revision),
                )
                .await?,
            ))
        }
        practice => practice,
    };
    let practice = matches!(found, Found::Practice(_));
    let key = id.clone();
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                s.suggestions.remove(&key);
                if practice {
                    let title = s.edits.get(&key).and_then(|e| e.title.clone());
                    s.edits.insert(key, DraftEdit { title, body });
                }
            }),
        )
        .await
        .map_err(ApiError::store)?;
    Ok(draft_response(&render(&state, &found, &signed, &st).await?))
}

async fn reject_suggestion(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    signed.drafts()?;
    let found = find(&state, &signed, &id).await?;
    let key = id.clone();
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                s.suggestions.remove(&key);
            }),
        )
        .await
        .map_err(ApiError::store)?;
    Ok(draft_response(&render(&state, &found, &signed, &st).await?))
}

#[derive(Deserialize, Default)]
struct ApproveBody {
    #[serde(default)]
    confirm_external: bool,
    /// The draft's `version` as the PM saw it. Optional; when sent, it must still be the
    /// draft's version (`409 stale` otherwise).
    #[serde(default)]
    version: Option<String>,
}

fn stale() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "stale",
        "This draft changed since you opened it. Reload it and check the text before \
         approving.",
    )
}

/// Decide a practice draft: only here, nothing upstream. Approving one already approved
/// answers it as it is (a retry), as for a real draft.
async fn decide_practice(
    state: &AppState,
    signed: &Signed,
    p: &crate::settings::PracticeDraft,
    to: &'static str,
    version: Option<&str>,
) -> ApiResult {
    let st = settings_of(state, signed.handle()).await?;
    if version.is_some_and(|v| v != drafts::practice_version(p, &st)) {
        return Err(stale());
    }
    if p.status == to && to == "approved" {
        return Ok(draft_response(&drafts::practice_draft(
            p,
            &signed.bot(),
            &st,
        )));
    }
    if p.status != "pending" {
        return Err(ApiError::not_pending());
    }
    let at = drafts::rfc3339(now(state));
    let id = p.id.clone();
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                if let Some(p) = s.practice.as_mut().filter(|p| p.id == id) {
                    p.status = to.into();
                    p.decided_at = Some(at);
                }
                s.suggestions.remove(&id);
            }),
        )
        .await
        .map_err(ApiError::store)?;
    let p = st.practice.clone().ok_or_else(ApiError::not_found)?;
    Ok(draft_response(&drafts::practice_draft(
        &p,
        &signed.bot(),
        &st,
    )))
}

/// After a decision: forget the edit and suggestion and show the item as it now is.
async fn decided(state: &AppState, signed: &Signed, item: Approval) -> ApiResult {
    let key = item.id.clone();
    let st = state
        .0
        .settings
        .update(signed.handle(), Box::new(move |s| s.forget_draft(&key)))
        .await
        .map_err(ApiError::store)?;
    Ok(draft_response(
        &item_draft(state, signed, &item, &st).await?,
    ))
}

/// Is `item` already approved by this PM with exactly the arguments of `version` (when
/// given)? Then an approve is a retry: it answers the draft as it is and sends nothing.
/// `executing` counts: the write is running.
fn approved_as(item: &Approval, handle: &str, version: Option<&str>) -> bool {
    matches!(item.status.as_str(), "executing" | "approved")
        && item.decided_by.as_deref() == Some(handle)
        && version.is_none_or(|v| v == drafts::args_version(&item.tool, &item.args))
}

/// A refused decision as the web app's error. The tool server's own `409` codes pass
/// through; a `409` without one means the item is no longer pending, or can't be decided
/// as it stands.
async fn refused_decision(
    state: &AppState,
    signed: &Signed,
    id: &str,
    e: UpstreamError,
) -> ApiError {
    let UpstreamError::Refused {
        status,
        code,
        audience,
        ..
    } = e
    else {
        return ApiError::upstream(e);
    };
    // The tool server's own sentence can quote the draft's arguments, so it is never passed
    // on, nor logged: the browser gets a fixed code and sentence, the log the status and
    // code.
    tracing::warn!(
        user = %signed.handle(),
        draft = %id,
        status,
        code = code.as_deref().unwrap_or("none"),
        "decision refused by the tool server"
    );
    if let (Some(seen), Some("needs_external_confirm" | "audience_blocked")) =
        (audience, code.as_deref())
    {
        remember_audience(state, id, seen);
    }
    match (status, code.as_deref()) {
        (409, Some("needs_external_confirm")) => ApiError::new(
            StatusCode::CONFLICT,
            "needs_external_confirm",
            "This channel is shared with people outside the company. Confirm to post there. \
             Nothing was sent.",
        ),
        (409, Some("audience_blocked")) => ApiError::new(
            StatusCode::CONFLICT,
            "audience_blocked",
            "This draft was built from restricted sources, and people outside the product team \
             can see that channel, so PM OS won't post it there. Nothing was sent.",
        ),
        (409, Some("expired")) => ApiError::new(
            StatusCode::CONFLICT,
            "expired",
            "This draft expired before it was decided. Nothing was sent.",
        ),
        // The draft changed between this read and the decision (R4): nothing was decided.
        (409, Some("stale")) => stale(),
        (503, Some("audience_unavailable")) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "audience_unavailable",
            "PM OS couldn't check who can see that channel. Nothing was sent; try again.",
        ),
        (409, _) => match signed
            .toolserver(state)
            .approval_for(signed.handle(), id)
            .await
        {
            Ok(item) if item.status != "pending" => ApiError::not_pending(),
            _ => ApiError::new(
                StatusCode::CONFLICT,
                "not_approvable",
                "This draft can't be approved as it stands. Nothing was sent. Reject it and \
                 ask again.",
            ),
        },
        // An edit that doesn't fit the tool, or (never from `edited_args`) one that changes
        // where the write goes.
        (400 | 422, _) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_edit",
            "The edited draft isn't valid for where it goes. Nothing was sent.",
        ),
        (403, _) => ApiError::new(
            StatusCode::FORBIDDEN,
            "not_a_member",
            "The tool server doesn't count you as a team member. Nothing was sent.",
        ),
        (503, _) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "PM OS can't decide this right now. Nothing was sent; try again in a minute.",
        ),
        _ => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "The tool server refused this. Nothing was sent. If it keeps happening, tell a \
             platform owner.",
        ),
    }
}

/// A decision whose outcome isn't known: the call was sent, but no usable answer came back.
/// `504` when it timed out, else `502`; the app must not say nothing was sent.
fn unknown_outcome(why: &str) -> ApiError {
    let status = if why == "timed out" {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::BAD_GATEWAY
    };
    ApiError::new(
        status,
        "unknown_outcome",
        "PM OS lost touch with the tool server during this decision, so it may or may not \
         have gone through. Reload the draft to see where it stands before trying again.",
    )
}

/// After a decision call whose answer was lost: read the item again. If it now shows this
/// PM's decision (`settled`), that is the answer; otherwise the outcome is unknown.
async fn after_lost_answer(
    state: &AppState,
    signed: &Signed,
    id: &str,
    why: &str,
    settled: impl Fn(&Approval) -> bool,
) -> ApiResult {
    tracing::warn!(user = %signed.handle(), draft = %id, why, "decision answer lost; reading the draft again");
    match signed
        .toolserver(state)
        .approval_for(signed.handle(), id)
        .await
    {
        Ok(item) if item.user == signed.handle() && settled(&item) => {
            decided(state, signed, item).await
        }
        _ => Err(unknown_outcome(why)),
    }
}

async fn approve(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    signed.drafts()?;
    let req: ApproveBody = parse_optional(&body)?;
    let version = req.version.as_deref();
    let item = match find(&state, &signed, &id).await? {
        // Practice drafts are decided here and never reach the tool server.
        Found::Practice(p) => {
            return decide_practice(&state, &signed, &p, "approved", version).await
        }
        Found::Item(item) => *item,
    };
    if drafts::is_practice_id(&item.id) {
        // Never: practice ids are this server's own. Refused rather than sent upstream.
        return Err(ApiError::not_found());
    }
    if item.status != "pending" {
        if approved_as(&item, signed.handle(), version) {
            return decided(&state, &signed, item).await;
        }
        return Err(ApiError::not_pending());
    }
    // Exactly what the PM saw: the version is computed from the item's arguments, which hold
    // their saved edit. An edited draft is approved with those arguments (`edit`), so a save
    // from another tab a moment later can't change what this approve writes.
    let current = drafts::version(&item);
    if version.is_some_and(|v| v != current) {
        return Err(stale());
    }
    // What the PM was shown, in the tool server's terms (R4): the args hash of the item read
    // just now, whose version the PM's matched. A save between that read and the decision is
    // refused there as `stale`, atomically, and answered here as `409 stale`.
    let decision = DecisionBody {
        decided_by: signed.handle().to_string(),
        decision: if item.edited { "edit" } else { "approve" },
        args: item.edited.then(|| item.args.clone()),
        reason: None,
        confirm_external: req.confirm_external,
        expected_args_hash: Some(item.args_hash.clone()).filter(|h| !h.is_empty()),
    };
    match signed.toolserver(&state).decide(&id, &decision).await {
        Ok(item) => {
            tracing::info!(user = %signed.handle(), draft = %id, decision = decision.decision, "draft approved");
            decided(&state, &signed, item).await
        }
        Err(UpstreamError::Unknown { why, .. }) => {
            let handle = signed.handle().to_string();
            after_lost_answer(&state, &signed, &id, &why, |now| {
                approved_as(now, &handle, Some(&current))
                    || (now.status == "failed" && now.decided_by.as_deref() == Some(&handle))
            })
            .await
        }
        Err(e) => {
            // Two approves at once (a double click, a retry after a lost answer): the tool
            // server lets exactly one decide. If that one approved what this one would have,
            // this is the same approval, not a failure.
            if matches!(
                e,
                UpstreamError::Refused {
                    status: 409,
                    code: None,
                    ..
                }
            ) {
                if let Ok(now_item) = signed
                    .toolserver(&state)
                    .approval_for(signed.handle(), &id)
                    .await
                {
                    if now_item.user == signed.handle()
                        && approved_as(&now_item, signed.handle(), Some(&current))
                    {
                        return decided(&state, &signed, now_item).await;
                    }
                }
            }
            let err = refused_decision(&state, &signed, &id, e).await;
            if err.code == "needs_external_confirm" {
                let key = id.clone();
                state
                    .0
                    .settings
                    .update(
                        signed.handle(),
                        Box::new(move |s| {
                            s.external.insert(key);
                        }),
                    )
                    .await
                    .map_err(ApiError::store)?;
            }
            Err(err)
        }
    }
}

#[derive(Deserialize, Default)]
struct RejectBody {
    #[serde(default)]
    reason: Option<String>,
}

async fn reject(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult {
    signed.drafts()?;
    let req: RejectBody = parse_optional(&body)?;
    if let Some(r) = &req.reason {
        if !REJECT_REASONS.contains(&r.as_str()) {
            return Err(ApiError::bad_request(format!(
                "`reason` is one of {}.",
                REJECT_REASONS.join(", ")
            )));
        }
    }
    let item = match find(&state, &signed, &id).await? {
        Found::Practice(p) => return decide_practice(&state, &signed, &p, "rejected", None).await,
        Found::Item(item) => *item,
    };
    if drafts::is_practice_id(&item.id) {
        return Err(ApiError::not_found());
    }
    if item.status != "pending" {
        return Err(ApiError::not_pending());
    }
    let decision = DecisionBody {
        decided_by: signed.handle().to_string(),
        decision: "reject",
        args: None,
        reason: req.reason,
        confirm_external: false,
        expected_args_hash: None,
    };
    match signed.toolserver(&state).decide(&id, &decision).await {
        Ok(item) => {
            tracing::info!(user = %signed.handle(), draft = %id, "draft rejected");
            decided(&state, &signed, item).await
        }
        Err(UpstreamError::Unknown { why, .. }) => {
            let handle = signed.handle().to_string();
            after_lost_answer(&state, &signed, &id, &why, |now| {
                now.status == "rejected" && now.decided_by.as_deref() == Some(&handle)
            })
            .await
        }
        Err(e) => Err(refused_decision(&state, &signed, &id, e).await),
    }
}

async fn create_practice(State(state): State<AppState>, signed: Signed) -> ApiResult {
    signed.drafts()?;
    let fresh = drafts::new_practice(now(&state));
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                if s.practice.is_none() {
                    s.practice = Some(fresh);
                }
            }),
        )
        .await
        .map_err(ApiError::store)?;
    let p = st.practice.as_ref().ok_or_else(ApiError::not_found)?;
    Ok(draft_response(&drafts::practice_draft(
        p,
        &signed.bot(),
        &st,
    )))
}

async fn delete_draft(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    signed.drafts()?;
    match find(&state, &signed, &id).await? {
        Found::Item(_) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_practice",
            "Only a practice draft can be deleted. Reject this one instead.",
        )),
        Found::Practice(p) => {
            state
                .0
                .settings
                .update(
                    signed.handle(),
                    Box::new(move |s| {
                        s.practice = None;
                        s.forget_draft(&p.id);
                    }),
                )
                .await
                .map_err(ApiError::store)?;
            Ok(StatusCode::NO_CONTENT.into_response())
        }
    }
}

// Tasks.

/// A task as the app shows it. `events` (only on `GET /api/tasks/{id}`), `progress` and
/// `retrying` are worked out in `progress.rs`: no worker or host name ever reaches the app.
#[derive(Serialize)]
struct TaskOut {
    id: String,
    bot: String,
    text: String,
    status: String,
    result: Option<String>,
    error: Option<String>,
    draft_ids: Vec<String>,
    created_at: String,
    updated_at: String,
    progress: Option<Progress>,
    retrying: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<Vec<EventOut>>,
}

/// `events` is `None` for a list (no events are read) and `Some` for a single task.
fn task_out(t: Task, events: Option<&[TaskEvent]>) -> TaskOut {
    let seen = events.unwrap_or_default();
    let progress = progress::progress(&t, seen);
    let retrying = progress::retrying(&t, seen);
    TaskOut {
        progress,
        retrying,
        events: events.map(progress::milestones),
        id: t.id,
        bot: t.bot,
        text: t.text,
        status: t.status,
        // Source markers are for drafts; an answer that has some shows without them.
        result: t.result.as_deref().map(pmos_contract::cite::strip),
        error: t.error,
        draft_ids: t.approvals,
        created_at: t.created_at,
        updated_at: t.updated_at,
    }
}

#[derive(Deserialize)]
struct AskBody {
    text: String,
}

async fn create_task(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    signed.need(Permission::AskBot)?;
    let req: AskBody = parse(&body)?;
    let text = req.text.trim();
    if text.is_empty() || text.chars().count() > MAX_ASK {
        return Err(ApiError::bad_request(
            "Say what you need, in at most 8,000 characters.",
        ));
    }
    let task = signed
        .runtime(&state)
        .create_task(signed.handle(), &signed.bot(), text)
        .await
        .map_err(|e| match e {
            UpstreamError::Refused {
                status: 403 | 404 | 422,
                ..
            } => ApiError::new(
                StatusCode::CONFLICT,
                "task_refused",
                "Your bot can't take this. If you're new to PM OS, finish onboarding in Slack \
                 first.",
            ),
            e => ApiError::upstream(e),
        })?;
    if task.requested_by != signed.handle() {
        return Err(ApiError::upstream(UpstreamError::Failed {
            service: "bot runtime",
            why: "made the task for someone else".into(),
        }));
    }
    tracing::info!(user = %signed.handle(), task = %task.id, "task created");
    Ok((StatusCode::CREATED, Json(task_out(task, Some(&[])))).into_response())
}

async fn get_task(
    State(state): State<AppState>,
    signed: Signed,
    Path(id): Path<String>,
) -> ApiResult {
    signed.need(Permission::ViewOwnTasks)?;
    if !valid_id(&id) {
        return Err(ApiError::not_found());
    }
    let task = signed
        .runtime(&state)
        .task(signed.handle(), &id)
        .await
        .map_err(|e| match e {
            UpstreamError::Refused { status: 403, .. } => ApiError::not_found(),
            e => ApiError::upstream(e),
        })?;
    if task.requested_by != signed.handle() {
        return Err(ApiError::not_found());
    }
    let events = match signed
        .runtime(&state)
        .task_events(signed.handle(), &id)
        .await
    {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(error = ?e, task = %id, "task events unavailable");
            Vec::new()
        }
    };
    Ok(Json(task_out(task, Some(&events))).into_response())
}

async fn list_tasks(State(state): State<AppState>, signed: Signed) -> ApiResult {
    signed.need(Permission::ViewOwnTasks)?;
    let st = settings_of(&state, signed.handle()).await?;
    let tasks = signed
        .runtime(&state)
        .tasks(
            signed.handle(),
            RECENT_TASKS + st.revision_tasks.len().min(30),
        )
        .await
        .map_err(ApiError::upstream)?;
    let tasks: Vec<TaskOut> = tasks
        .into_iter()
        .filter(|t| t.requested_by == signed.handle() && !st.revision_tasks.contains(&t.id))
        .take(RECENT_TASKS)
        .map(|t| task_out(t, None))
        .collect();
    Ok(Json(json!({ "tasks": tasks })).into_response())
}

// The web app's own files.

fn content_type(path: &FsPath) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// How long a browser may keep one of the app's files: `index.html` is checked every time
/// (it names the current build's scripts); the icon, whose name never changes and which
/// browsers fetch on every page, a week; anything else five minutes.
fn cache_control(file: &FsPath) -> &'static str {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if file.extension().is_some_and(|e| e == "html") {
        "no-cache"
    } else if FAVICONS.contains(&name) {
        "public, max-age=604800"
    } else {
        "public, max-age=300"
    }
}

/// The app's icon files, served from the static directory: the CSP allows no `data:` image,
/// so the icon must be a file of this origin.
pub const FAVICONS: [&str; 2] = ["favicon.svg", "favicon.ico"];

/// Does this URL path name a file of a type the app serves (anything but a page)?
fn is_asset_path(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or_default();
    match last.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            content_type(FsPath::new(last)) != "application/octet-stream" && ext != "html"
        }
        _ => false,
    }
}

/// A file under `root` for this URL path, or `None`: no `..`, no hidden names, and after
/// resolving links still inside `root`.
fn resolve(root: &FsPath, url_path: &str) -> Option<PathBuf> {
    let rel = FsPath::new(url_path.trim_start_matches('/'));
    for c in rel.components() {
        match c {
            Component::Normal(name) if !name.to_string_lossy().starts_with('.') => {}
            _ => return None,
        }
    }
    if url_path.contains('\\') || url_path.contains('\0') {
        return None;
    }
    let path = root.join(rel).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

async fn static_files(State(state): State<AppState>, method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::not_found().into_response();
    }
    let Some(root) = state.0.static_root.as_deref() else {
        return ApiError::not_found().into_response();
    };
    if !matches!(method, Method::GET | Method::HEAD) {
        return ApiError::not_found().into_response();
    }
    // A real file, or else the app's index.html, which routes on the client. A missing
    // file that is plainly not a page (`/favicon.svg`, `/assets/x.js`) is a 404: the app's
    // HTML served as an image or script helps nobody.
    let file = match resolve(root, path) {
        Some(file) => file,
        None if is_asset_path(path) => return ApiError::not_found().into_response(),
        None => root.join("index.html"),
    };
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let cache = cache_control(&file);
            (
                [
                    (header::CONTENT_TYPE, content_type(&file)),
                    (header::CACHE_CONTROL, cache),
                    (header::CONTENT_SECURITY_POLICY, APP_CSP),
                ],
                bytes,
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "static file unreadable");
            ApiError::not_found().into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn strips_a_fence_around_the_revised_text() {
        assert_eq!(unfence("```markdown\n# Hi\nthere\n```"), "# Hi\nthere");
        assert_eq!(unfence("  # Hi  "), "# Hi");
        assert_eq!(unfence("text with ``` inside"), "text with ``` inside");
    }

    #[test]
    fn resolves_static_paths_safely() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("index.html"), "<html>").unwrap();
        std::fs::create_dir(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/app.js"), "x").unwrap();
        std::fs::write(root.join(".env"), "SECRET").unwrap();
        assert!(resolve(&root, "/assets/app.js").is_some());
        assert!(resolve(&root, "/../etc/passwd").is_none());
        assert!(resolve(&root, "/.env").is_none());
        assert!(resolve(&root, "/assets/missing.js").is_none());
        assert!(resolve(&root, "/assets").is_none());
    }

    #[test]
    fn icons_cache_long_and_missing_assets_are_not_the_app() {
        assert_eq!(
            cache_control(FsPath::new("/d/favicon.svg")),
            "public, max-age=604800"
        );
        assert_eq!(cache_control(FsPath::new("/d/index.html")), "no-cache");
        assert_eq!(
            cache_control(FsPath::new("/d/assets/app.js")),
            "public, max-age=300"
        );
        assert_eq!(content_type(FsPath::new("favicon.svg")), "image/svg+xml");
        assert_eq!(content_type(FsPath::new("favicon.ico")), "image/x-icon");
        assert!(is_asset_path("/favicon.svg"));
        assert!(is_asset_path("/assets/app.js"));
        assert!(!is_asset_path("/drafts/apr_1"));
        assert!(!is_asset_path("/drafts/apr_1.2"));
        assert!(!is_asset_path("/index.html"));
        assert!(!is_asset_path("/.env"));
    }

    #[test]
    fn settles_revision_tasks() {
        let task = |status: &str, result: Option<&str>| Task {
            id: "tsk_1".into(),
            bot: "pm-ana".into(),
            requested_by: "ana".into(),
            source: "web".into(),
            text: String::new(),
            status: status.into(),
            result: result.map(String::from),
            error: None,
            approvals: vec![],
            checklist: Value::Null,
            attempts: 1,
            created_at: String::new(),
            updated_at: String::new(),
            sources_sensitivity: None,
        };
        assert!(settle(&task("working", None)).is_none());
        assert_eq!(
            settle(&task("done", Some("New text")))
                .unwrap()
                .1
                .as_deref(),
            Some("New text")
        );
        assert_eq!(settle(&task("done", None)).unwrap().0, "failed");
        assert_eq!(
            settle(&task("needs_approval", Some("x"))).unwrap().0,
            "failed"
        );
        assert_eq!(settle(&task("cancelled", None)).unwrap().0, "failed");
    }
}
