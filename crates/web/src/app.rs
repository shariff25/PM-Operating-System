//! The router and every `/api` route in `docs/web-api.md`.
//!
//! Every request but sign-in, `/healthz` and the static app needs a live session; the
//! session's handle must still be a member in `team.yaml` (re-read on each request), and
//! every `POST`, `PUT` and `DELETE` must come from the app's own origin and carry the
//! session's CSRF token. Drafts and tasks also need the PM abilities of the permission
//! matrix; someone else's draft or task answers `404 not_found`.

use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use pmos_contract::model::{TeamFile, TeamMember};
use pmos_contract::onboarding::personal_bot_name;
use pmos_contract::permissions::Permission;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{self, AuthProvider, DevLoginProvider, SignInError, SignInInput};
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
    Approval, DecisionBody, Runtime, Task, TaskEvent, ToolServer, UpstreamError,
};

mod admin;
mod brief;

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

/// The hints the app can mark seen.
pub const HINTS: [&str; 3] = ["sources", "ask", "approve"];

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
}

impl Deps {
    pub fn in_memory() -> Self {
        Self {
            sessions: Arc::new(MemorySessionStore::new()),
            settings: Arc::new(MemorySettingsStore::new()),
            followups: Arc::new(MemoryFollowUpStore::new()),
            brief: BriefConfig::default(),
            clock: Arc::new(Utc::now),
        }
    }
}

struct Inner {
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
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

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

/// The app. Refuses (`Err`) with no sign-in provider, or a static directory without an
/// `index.html`.
pub fn build(config: Config, deps: Deps) -> Result<Router, String> {
    let (providers, entra) = make_providers(&config)?;
    if providers.is_empty() {
        return Err(NO_PROVIDER.into());
    }
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
    let state = AppState(Arc::new(Inner {
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
    Ok(Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api", api)
        .route("/internal/brief/{handle}", brief::internal())
        .fallback(static_files)
        .layer(axum::middleware::from_fn(security_headers))
        .with_state(state))
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// Every response: no sniffing, no framing, no referrer at all, its own browsing context.
/// Every `/api` answer is `no-store` with a policy that loads nothing, whatever the handler
/// set; the app's files keep the policy and caching `static_files` gives them.
async fn security_headers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let api = is_api(req.uri().path());
    let mut res = next.run(req).await;
    let h = res.headers_mut();
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
            UpstreamError::Failed { service, why } => {
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

/// A live session whose handle is a member now, and for a write, a checked origin and CSRF
/// token.
pub struct Signed {
    pub member: TeamMember,
    pub session: Session,
    /// The session's reference for user assertions (`session::assertion_ref`): never the
    /// session id, and never logged.
    session_ref: String,
}

impl Signed {
    fn handle(&self) -> &str {
        &self.member.handle
    }

    /// The bot runtime, signing as this PM's session.
    fn runtime(&self, state: &AppState) -> Runtime {
        state.0.runtime.for_session(&self.session_ref)
    }

    /// The tool server, signing as this PM's session.
    fn toolserver(&self, state: &AppState) -> ToolServer {
        state.0.toolserver.for_session(&self.session_ref)
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

fn load_team(state: &AppState) -> Result<TeamFile, ApiError> {
    TeamFile::load(&state.0.content_root).map_err(|e| {
        tracing::error!(error = %e, "team.yaml can't be read");
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "team_unavailable",
            "PM OS can't read the team list right now. Tell a platform owner.",
        )
    })
}

impl FromRequestParts<AppState> for Signed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let Some((id, session)) = live_session(state, &parts.headers).await? else {
            return Err(ApiError::signed_out());
        };
        let team = load_team(state)?;
        let Some(member) = team.by_handle(&session.handle).cloned() else {
            // Removed from the team: end every session they have, at once.
            tracing::info!(user = %session.handle, "no longer in team.yaml: signed out");
            state
                .0
                .sessions
                .remove_user(&session.handle)
                .await
                .map_err(ApiError::store)?;
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "not_a_member",
                "You're not on the PM OS team any more. Ask a platform owner.",
            ));
        };
        if is_write(&parts.method) {
            check_write(state, &parts.headers, &session)?;
        }
        Ok(Self {
            member,
            session,
            session_ref: session::assertion_ref(&id),
        })
    }
}

// Sign-in.

#[derive(Deserialize)]
struct DevLogin {
    handle: String,
}

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
    let team = load_team(&state)?;
    let Some(member) = auth::find_member(&team, &claimed.0) else {
        tracing::info!(
            provider = provider.name(),
            "sign-in refused: not in team.yaml"
        );
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_a_member",
            "That person isn't on the PM OS team.",
        ));
    };
    let id = start_session(&state, &headers, member, provider.name()).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, session::set_cookie(&id))],
    )
        .into_response())
}

/// A new session for `member`, whichever provider signed them in: the cookie value. Ends
/// whatever session this browser had (rotation).
async fn start_session(
    state: &AppState,
    headers: &HeaderMap,
    member: &TeamMember,
    provider: &str,
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
    state
        .0
        .sessions
        .insert(
            session::hash_id(&id),
            Session {
                handle: member.handle.clone(),
                csrf_token: session::random_token(),
                created_at: at,
                last_seen: at,
            },
        )
        .await
        .map_err(ApiError::store)?;
    tracing::info!(user = %member.handle, provider, "signed in");
    Ok(id)
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
    let email = match entra.authenticate(&input).await {
        Ok(claimed) => claimed.0,
        Err(SignInError::Rejected(f)) => return fail(f),
        Err(_) => return fail(Failure::Failed("unsupported input")),
    };
    let team = load_team(&state)?;
    let Some(member) = auth::find_member_by_email(&team, &email) else {
        tracing::info!(provider = "entra", "sign-in refused: not in team.yaml");
        return Ok(redirect(
            &signed_out_at("not_a_member"),
            std::slice::from_ref(&clear),
        ));
    };
    let id = start_session(&state, &headers, member, entra.name()).await?;
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
        // The handle: a team.yaml member (`pmos_contract::model::TeamMember`) has no display
        // name, and the name onboarding takes from Slack stays on the join request, which
        // is not written to team.yaml. The user database will have one.
        "name": signed.member.handle,
        "roles": signed.member.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        "permissions": pmos_contract::permissions::granted(&signed.member.roles),
        "csrf_token": signed.session.csrf_token,
        "first_visit": !st.opened_a_draft,
        "hints_seen": st.hints_seen.iter().collect::<Vec<_>>(),
        "nudges": st.nudges,
    }))
    .into_response()
}

async fn me(State(state): State<AppState>, signed: Signed) -> ApiResult {
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
        .approval(id)
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
    let mut draft = drafts::draft(item, st).ok_or_else(ApiError::not_found)?;
    draft.sources = match signed
        .toolserver(state)
        .sources(&item.id, signed.handle())
        .await
    {
        Ok(found) => drafts::sources(found),
        // The draft is still shown; the app says it can't tell what the bot read.
        Err(e) => {
            tracing::warn!(error = ?e, draft = %item.id, "sources unavailable");
            Vec::new()
        }
    };
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
    let items = signed
        .toolserver(&state)
        .approvals(signed.handle())
        .await
        .map_err(ApiError::upstream)?;
    let st = settings_of(&state, signed.handle()).await?;
    let mut pending = Vec::new();
    let mut decided = Vec::new();
    for item in items.iter().filter(|i| i.user == signed.handle()) {
        let Some(summary) = drafts::summary(item, &st) else {
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
    // Edits and suggestions of drafts decided elsewhere (in Slack) are no longer needed.
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
    Ok(Json(json!({ "drafts": all })).into_response())
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
    let titled = match &found {
        Found::Practice(_) => true,
        Found::Item(item) => drafts::has_title(&item.tool),
    };
    let key = id.clone();
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                let title = if titled {
                    req.title
                        .or_else(|| s.edits.get(&key).and_then(|e| e.title.clone()))
                } else {
                    None
                };
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
    let created = signed
        .runtime(&state)
        .create_task(signed.handle(), &signed.bot(), &prompt)
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
        .filter(|s| s.status == "ready")
        .and_then(|s| s.body);
    let Some(body) = ready else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_suggestion",
            "There's no suggested text to accept yet.",
        ));
    };
    let key = id.clone();
    let st = state
        .0
        .settings
        .update(
            signed.handle(),
            Box::new(move |s| {
                s.suggestions.remove(&key);
                let title = s.edits.get(&key).and_then(|e| e.title.clone());
                s.edits.insert(key, DraftEdit { title, body });
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
        error,
        ..
    } = e
    else {
        return ApiError::upstream(e);
    };
    let say = |fallback: &str| error.clone().unwrap_or_else(|| fallback.to_string());
    match (status, code.as_deref()) {
        (409, Some(c)) if PASSED_THROUGH.contains(&c) => ApiError::new(
            StatusCode::CONFLICT,
            c,
            say("The tool server refused this decision."),
        ),
        (503, Some("audience_unavailable")) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "audience_unavailable",
            say("PM OS couldn't check who can see that channel. Nothing was sent; try again."),
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
                say("This draft can't be approved as it stands. Reject it and ask again."),
            ),
        },
        (400, _) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_edit",
            say("The edited draft isn't valid for its target."),
        ),
        (403, _) => ApiError::new(
            StatusCode::FORBIDDEN,
            "not_a_member",
            say("The tool server doesn't count you as a team member."),
        ),
        _ => ApiError::upstream(UpstreamError::Refused {
            service: "tool server",
            status,
            code,
            error,
        }),
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
    let st = settings_of(&state, signed.handle()).await?;
    // Exactly what the PM saw: the version is computed from the same arguments sent below.
    let args = drafts::effective_args(&item, &st);
    let current = drafts::args_version(&item.tool, &args);
    if version.is_some_and(|v| v != current) {
        return Err(stale());
    }
    let edited = st.edits.contains_key(&id);
    let decision = DecisionBody {
        decided_by: signed.handle().to_string(),
        decision: if edited { "edit" } else { "approve" },
        args: edited.then_some(args),
        reason: None,
        confirm_external: req.confirm_external,
    };
    match signed.toolserver(&state).decide(&id, &decision).await {
        Ok(item) => {
            tracing::info!(user = %signed.handle(), draft = %id, decision = decision.decision, "draft approved");
            decided(&state, &signed, item).await
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
    };
    match signed.toolserver(&state).decide(&id, &decision).await {
        Ok(item) => {
            tracing::info!(user = %signed.handle(), draft = %id, "draft rejected");
            decided(&state, &signed, item).await
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
        result: t.result,
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
