//! Password sign-in's routes (`docs/web-api.md`, "Password sign-in"; `docs/password-sign-in.md`):
//! on with `PMOS_WEB_PASSWORD_LOGIN=1`, `404` otherwise.
//!
//! **The tool server decides; `pmos-web` relays.** It never stores or sees a password hash, an
//! authenticator secret or a recovery code's hash: it forwards what the person typed to the
//! tool server's `/internal/password/…` routes with its own token (and, for a signed-in
//! person, their user assertion) and acts on the decision. What it keeps itself:
//!
//! - **The tokens between steps** in two cookies the page can't read: `__Host-pmos-pending`
//!   (after a correct password, for the second step; five minutes) and `__Host-pmos-setup` (a
//!   setup in progress; fifteen minutes). `Secure; HttpOnly; SameSite=Strict; Path=/`.
//! - **The session**, exactly as every other sign-in makes it ([`super::start_session`]), with
//!   the credential's epoch: every request checks it against the user database, so a reset or
//!   a password change ends it within 10 seconds (at once in this server).
//! - **A per-address limit** ([`SourceLimiter`]): at most [`SOURCE_REFUSALS`] refused guesses
//!   (a wrong password, code or setup code) from one address in any [`SOURCE_WINDOW_MINUTES`]
//!   minutes; past it every unauthenticated password route answers `429 too_many_attempts`.
//!
//! Every write takes the origin check and JSON (a login CSRF defence, as development sign-in
//! does); the signed-in ones take the session's CSRF token too. Refusals keep the tool server's
//! code with `pmos-web`'s own sentence ([`PASSWORD_CODES`]). Nothing here logs a password, a
//! code, a token, a secret or a sign-in name.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use pmos_contract::permissions::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{
    auth, bad_content_type, is_json, now, parse, parse_optional, start_session, ApiError,
    ApiResult, AppState, Signed,
};
use crate::upstream::UpstreamError;

/// Refused guesses from one address before it waits.
pub const SOURCE_REFUSALS: usize = 20;
/// The window those are counted in.
pub const SOURCE_WINDOW_MINUTES: i64 = 15;
/// Addresses tracked at once; past it the one seen longest ago goes.
pub const MAX_SOURCES: usize = 10_000;

/// The cookie that carries the pending token after a correct password.
pub const PENDING_COOKIE: &str = "__Host-pmos-pending";
/// The cookie that carries a setup in progress.
pub const SETUP_COOKIE: &str = "__Host-pmos-setup";
/// How long each cookie lives (the tool server's token lives as long, or less).
const PENDING_SECS: i64 = 5 * 60;
const SETUP_SECS: i64 = 15 * 60;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/password/sign-in", post(sign_in))
        .route("/auth/password/second-step", post(second_step))
        .route("/auth/password/setup/begin", post(setup_begin))
        .route("/auth/password/setup/password", post(setup_password))
        .route(
            "/auth/password/setup/authenticator",
            post(setup_authenticator),
        )
        .route("/auth/password/setup/confirm", post(setup_confirm))
        .route("/auth/password/change", post(change))
        .route("/auth/password/recovery-codes", post(recovery_codes))
        .route("/auth/password/status", get(status))
        .route(
            "/admin/users/{handle}/password-setup-code",
            post(issue_setup_code),
        )
}

/// The tool server's codes on these routes, the status `pmos-web` answers with, and its own
/// sentence. Any other code is `502 upstream_unavailable`.
pub const PASSWORD_CODES: [(&str, u16, &str); 23] = [
    ("bad_request", 400, "That request wasn't in a shape PM OS takes."),
    ("sign_in_failed", 401, "That sign-in name and password didn't work. Check them and try again, or wait a few minutes."),
    ("code_failed", 401, "That code didn't work. Try the newest code from your authenticator app."),
    ("sign_in_expired", 401, "That sign-in took too long or was used up. Start again with your password."),
    ("setup_failed", 401, "That sign-in name and setup code didn't work. Check them, or ask an admin for a new code."),
    ("setup_expired", 401, "That setup took too long. Start again with your setup code."),
    ("reauth_failed", 403, "Your current password or the authenticator code wasn't right. Nothing changed."),
    ("not_a_member", 403, "Your access to PM OS is turned off. A platform owner can turn it back on."),
    ("forbidden", 403, "Only an active admin can make setup codes. Nothing changed."),
    ("host_only", 403, "An Admin's or Auditor's setup code comes from someone with host access (pmos-admin users password-setup). Nothing changed."),
    ("self_change", 403, "Nobody makes a setup code for themselves. Ask another admin. Nothing changed."),
    ("not_found", 404, "There's no such person in PM OS. Nothing changed."),
    ("wrong_step", 409, "Do the set-up steps in order: password, then authenticator, then the code."),
    ("no_password", 409, "You don't have a PM OS password yet. Nothing changed."),
    ("sign_in_unconfirmed", 409, "An admin needs to confirm this person's sign-in name first. Nothing changed."),
    ("evals_account", 409, "The evals account never has a password. Nothing changed."),
    ("not_active", 409, "That person is deactivated. Nothing changed."),
    ("weak_password", 422, "Choose another password: at least 12 characters, not your name or sign-in name, and not a common password."),
    ("too_many_attempts", 429, "Too many tries. Wait a few minutes, then try again."),
    ("not_configured", 501, "Password sign-in isn't set up on this PM OS. Nothing changed."),
    ("sign_in_busy", 503, "Sign-in is busy right now. Try again in a moment."),
    ("assertion_required", 503, "Password sign-in needs signed requests, and this PM OS isn't running with them. Nothing changed."),
    ("unavailable", 503, "PM OS storage isn't answering. Try again in a minute."),
];

/// The codes counted against an address: a wrong guess.
const GUESSES: [&str; 3] = ["sign_in_failed", "code_failed", "setup_failed"];

// --- The per-address limit ---------------------------------------------------------------------

/// Refused guesses per address in a rolling window. In this process only.
#[derive(Default)]
pub struct SourceLimiter {
    seen: Mutex<HashMap<Option<IpAddr>, VecDeque<DateTime<Utc>>>>,
}

impl SourceLimiter {
    fn window() -> Duration {
        Duration::minutes(SOURCE_WINDOW_MINUTES)
    }

    /// May `source` try at `now`? `Err(retry_after_secs)` when it has used its refusals.
    pub fn check(&self, source: Option<IpAddr>, now: DateTime<Utc>) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let Some(times) = seen.get_mut(&source) else {
            return Ok(());
        };
        times.retain(|t| now - *t < Self::window() && *t <= now);
        if times.len() >= SOURCE_REFUSALS {
            let oldest = times.front().copied().unwrap_or(now);
            let wait = (oldest + Self::window() - now).num_seconds().max(1);
            return Err(u64::try_from(wait).unwrap_or(1));
        }
        Ok(())
    }

    /// Count a refused guess from `source` at `now`.
    pub fn refused(&self, source: Option<IpAddr>, now: DateTime<Utc>) {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if !seen.contains_key(&source) && seen.len() >= MAX_SOURCES {
            let stalest = seen
                .iter()
                .min_by_key(|(_, t)| t.back().copied())
                .map(|(k, _)| *k);
            if let Some(k) = stalest {
                seen.remove(&k);
            }
        }
        let times = seen.entry(source).or_default();
        times.retain(|t| now - *t < Self::window());
        times.push_back(now);
    }
}

/// Where a request came from: the peer, or behind a trusted proxy the right-most
/// `X-Forwarded-For` address that isn't one. `None` when the server has no peer address (tests
/// that serve without it): every such request shares one count.
pub fn source(
    parts_peer: Option<SocketAddr>,
    headers: &HeaderMap,
    trusted: &[IpAddr],
) -> Option<IpAddr> {
    let peer = parts_peer.map(|p| p.ip())?;
    if !trusted.contains(&peer) {
        return Some(peer);
    }
    let forwarded: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|a| a.trim().parse::<IpAddr>().ok())
        .collect();
    Some(
        forwarded
            .into_iter()
            .rev()
            .find(|a| !trusted.contains(a))
            .unwrap_or(peer),
    )
}

// --- Cookies -----------------------------------------------------------------------------------

fn cookie(name: &str, value: &str, secs: i64) -> String {
    format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={secs}")
}

fn clear(name: &str) -> String {
    format!("{name}=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// A token cookie's value: 43 base64url characters, or nothing.
fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| {
            v.len() == 43
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn with_cookies(mut res: Response, cookies: &[String]) -> Response {
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}

// --- Answers -----------------------------------------------------------------------------------

fn off() -> ApiError {
    ApiError::not_found()
}

/// The tool server's refusal, as `pmos-web` answers it, with `attempts_left`, `reason` or
/// `Retry-After` carried over.
fn refused(status: u16, body: &Value, retry_after: Option<HeaderValue>) -> Response {
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    let Some((code, status, sentence)) = PASSWORD_CODES.iter().find(|(c, ..)| *c == code) else {
        tracing::warn!(
            status,
            "the tool server refused a password call with an unknown code"
        );
        return ApiError::upstream(UpstreamError::Refused {
            service: "tool server",
            status,
            code: None,
            error: None,
            audience: None,
        })
        .into_response();
    };
    let mut out = json!({ "code": code, "error": sentence });
    if let Some(n) = body.get("attempts_left").and_then(Value::as_i64) {
        out["attempts_left"] = json!(n);
    }
    if let Some(r) = body
        .get("reason")
        .and_then(Value::as_str)
        .filter(|r| ["too_short", "too_long", "matches_name", "common"].contains(r))
    {
        out["reason"] = json!(r);
    }
    let mut res = (
        StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST),
        Json(out),
    )
        .into_response();
    if let Some(v) = retry_after {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res
}

fn too_many(retry_after: u64) -> Response {
    let mut res = refused(
        429,
        &json!({ "code": "too_many_attempts" }),
        HeaderValue::from_str(&retry_after.to_string()).ok(),
    );
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// What the tool server answered: its status, body and `Retry-After`.
struct Answer {
    status: u16,
    body: Value,
    retry_after: Option<HeaderValue>,
}

/// An unauthenticated call (no one is signed in yet). The tool server unreachable: fail
/// closed, `503 users_unavailable`.
async fn call(state: &AppState, segments: &[&str], body: &Value) -> Result<Answer, ApiError> {
    match state
        .0
        .toolserver
        .password_call(reqwest::Method::POST, segments, None, Some(body))
        .await
    {
        Ok((status, body, retry_after)) => Ok(Answer {
            status,
            body,
            retry_after,
        }),
        Err(e) => {
            tracing::warn!(error = ?e, "password sign-in: the tool server didn't answer");
            Err(super::users_unavailable())
        }
    }
}

/// An unauthenticated password route's checks: on; same origin; JSON; the address not over
/// its limit. Gives back the address.
fn pre(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<SocketAddr>,
) -> Result<Option<IpAddr>, Box<Response>> {
    if !state.0.password_login {
        return Err(Box::new(off().into_response()));
    }
    if !auth::same_origin(headers, state.0.public_url.as_deref()) {
        return Err(Box::new(
            ApiError::new(
                StatusCode::FORBIDDEN,
                "bad_origin",
                "That request didn't come from PM OS.",
            )
            .into_response(),
        ));
    }
    // Login CSRF: a cross-site form can't send JSON.
    if !is_json(headers) {
        return Err(Box::new(bad_content_type().into_response()));
    }
    let from = source(peer, headers, &state.0.trusted_proxies);
    if let Err(wait) = state.0.sources.check(from, now(state)) {
        tracing::warn!("password sign-in: an address over its limit");
        return Err(Box::new(too_many(wait)));
    }
    Ok(from)
}

/// After a refusal: count a guess against the address.
fn count(state: &AppState, from: Option<IpAddr>, a: &Answer) {
    let code = a
        .body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if GUESSES.contains(&code) {
        state.0.sources.refused(from, now(state));
    }
}

/// The peer's address, when the server was started with it (`main` serves with connect info;
/// a test that doesn't has none).
pub struct Peer(Option<SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

// --- Sign-in -----------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInBody {
    sign_in_name: String,
    password: String,
}

/// `POST /api/auth/password/sign-in {sign_in_name, password}`: the first step.
async fn sign_in(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let from = match pre(&state, &headers, peer) {
        Ok(f) => f,
        Err(r) => return *r,
    };
    let b: SignInBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let a = match call(
        &state,
        &["sign-in"],
        &json!({"sign_in_name": b.sign_in_name, "password": b.password}),
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    if a.status != 200 {
        count(&state, from, &a);
        return refused(a.status, &a.body, a.retry_after);
    }
    let Some(token) = a.body.get("pending_token").and_then(Value::as_str) else {
        return bad_reply();
    };
    let res = Json(json!({
        "next": "second_step",
        "expires_at": a.body.get("expires_at"),
    }))
    .into_response();
    with_cookies(res, &[cookie(PENDING_COOKIE, token, PENDING_SECS)])
}

fn bad_reply() -> Response {
    ApiError::upstream(UpstreamError::Failed {
        service: "tool server",
        why: "sent a reply pmos-web can't read".into(),
    })
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecondStepBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    recovery_code: Option<String>,
}

/// Start the session for the tool server's `user` with `epoch`, ending `others` first when
/// asked (a setup or reset ends every session of theirs).
async fn session_for(
    state: &AppState,
    headers: &HeaderMap,
    user: &Value,
    epoch: Option<i64>,
    end_others: bool,
) -> Result<String, Box<Response>> {
    let Some(user) = pmos_directory::resolve::parse_user(user) else {
        return Err(Box::new(bad_reply()));
    };
    if !user.is_active() {
        return Err(Box::new(super::no_longer_a_member().into_response()));
    }
    if end_others {
        state
            .0
            .sessions
            .remove_user(&user.handle)
            .await
            .map_err(|e| Box::new(ApiError::store(e).into_response()))?;
    }
    state.0.directory.forget(&user.handle);
    start_session(state, headers, &user, "password", None, epoch)
        .await
        .map_err(|e| Box::new(e.into_response()))
}

/// `POST /api/auth/password/second-step {code} | {recovery_code}`, with the pending cookie:
/// the session.
async fn second_step(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let from = match pre(&state, &headers, peer) {
        Ok(f) => f,
        Err(r) => return *r,
    };
    let b: SecondStepBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let mut req = match (b.code, b.recovery_code) {
        (Some(c), None) => json!({ "code": c }),
        (None, Some(r)) => json!({ "recovery_code": r }),
        _ => {
            return ApiError::bad_request("Send `code` or `recovery_code`, one of them.")
                .into_response()
        }
    };
    let Some(token) = read_cookie(&headers, PENDING_COOKIE) else {
        return with_cookies(
            refused(401, &json!({"code": "sign_in_expired"}), None),
            &[clear(PENDING_COOKIE)],
        );
    };
    req["pending_token"] = json!(token);
    let a = match call(&state, &["second-step"], &req).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    if a.status != 200 {
        count(&state, from, &a);
        let code = a
            .body
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let res = refused(a.status, &a.body, a.retry_after);
        let used_up = code == "sign_in_expired"
            || code == "not_a_member"
            || (code == "code_failed"
                && a.body.get("attempts_left").and_then(Value::as_i64) == Some(0));
        return if used_up {
            with_cookies(res, &[clear(PENDING_COOKIE)])
        } else {
            res
        };
    }
    let epoch = a.body.get("password_epoch").and_then(Value::as_i64);
    let id = match session_for(&state, &headers, &a.body["user"], epoch, false).await {
        Ok(id) => id,
        Err(r) => return *r,
    };
    let res = Json(json!({
        "recovery_code_used": a.body.get("recovery_code_used").and_then(Value::as_bool).unwrap_or(false),
        "recovery_codes_left": a.body.get("recovery_codes_left").and_then(Value::as_u64).unwrap_or(0),
    }))
    .into_response();
    with_cookies(
        res,
        &[clear(PENDING_COOKIE), crate::session::set_cookie(&id)],
    )
}

// --- Setup -------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginBody {
    sign_in_name: String,
    setup_code: String,
}

/// `POST /api/auth/password/setup/begin {sign_in_name, setup_code}`.
async fn setup_begin(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let from = match pre(&state, &headers, peer) {
        Ok(f) => f,
        Err(r) => return *r,
    };
    let b: BeginBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let a = match call(
        &state,
        &["setup", "begin"],
        &json!({"sign_in_name": b.sign_in_name, "setup_code": b.setup_code}),
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    if a.status != 200 {
        count(&state, from, &a);
        return refused(a.status, &a.body, a.retry_after);
    }
    let Some(token) = a.body.get("setup_token").and_then(Value::as_str) else {
        return bad_reply();
    };
    let res = Json(json!({
        "handle": a.body.get("handle"),
        "kind": a.body.get("kind"),
        "expires_at": a.body.get("expires_at"),
        "password": {"min_length": 12, "max_length": 128},
    }))
    .into_response();
    with_cookies(res, &[cookie(SETUP_COOKIE, token, SETUP_SECS)])
}

/// The setup cookie, or `401 setup_expired`.
fn setup_token(headers: &HeaderMap) -> Result<String, Box<Response>> {
    read_cookie(headers, SETUP_COOKIE).ok_or_else(|| {
        Box::new(with_cookies(
            refused(401, &json!({"code": "setup_expired"}), None),
            &[clear(SETUP_COOKIE)],
        ))
    })
}

/// A setup step's refusal: `setup_expired` clears the cookie.
fn setup_refused(a: &Answer) -> Response {
    let res = refused(a.status, &a.body, a.retry_after.clone());
    let code = a
        .body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let used_up = matches!(
        code,
        "setup_expired" | "not_a_member" | "host_only" | "sign_in_unconfirmed"
    ) || (code == "code_failed"
        && a.body.get("attempts_left").and_then(Value::as_i64) == Some(0));
    if used_up {
        with_cookies(res, &[clear(SETUP_COOKIE)])
    } else {
        res
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    password: String,
}

/// `POST /api/auth/password/setup/password {password}`: `204`.
async fn setup_password(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = pre(&state, &headers, peer) {
        return *r;
    }
    let b: PasswordBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let token = match setup_token(&headers) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match call(
        &state,
        &["setup", "password"],
        &json!({"setup_token": token, "password": b.password}),
    )
    .await
    {
        Ok(a) if a.status == 200 => StatusCode::NO_CONTENT.into_response(),
        Ok(a) => setup_refused(&a),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// `POST /api/auth/password/setup/authenticator {}`: the enrolment, once.
async fn setup_authenticator(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = pre(&state, &headers, peer) {
        return *r;
    }
    if let Err(e) = parse_optional::<Empty>(&body) {
        return e.into_response();
    }
    let token = match setup_token(&headers) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    match call(
        &state,
        &["setup", "authenticator"],
        &json!({ "setup_token": token }),
    )
    .await
    {
        Ok(a) if a.status == 200 => {
            let pick = |k: &str| a.body.get(k).cloned().unwrap_or(Value::Null);
            Json(json!({
                "otpauth_uri": pick("otpauth_uri"),
                "secret": pick("secret"),
                "account": pick("account"),
                "issuer": pick("issuer"),
                "algorithm": pick("algorithm"),
                "digits": pick("digits"),
                "period": pick("period"),
            }))
            .into_response()
        }
        Ok(a) => setup_refused(&a),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBody {
    code: String,
}

/// `POST /api/auth/password/setup/confirm {code}`: the recovery codes, once, and the session;
/// every other session of the person ends.
async fn setup_confirm(
    State(state): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let from = match pre(&state, &headers, peer) {
        Ok(f) => f,
        Err(r) => return *r,
    };
    let b: CodeBody = match parse(&body) {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let token = match setup_token(&headers) {
        Ok(t) => t,
        Err(r) => return *r,
    };
    let a = match call(
        &state,
        &["setup", "confirm"],
        &json!({"setup_token": token, "code": b.code}),
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    if a.status != 200 {
        count(&state, from, &a);
        return setup_refused(&a);
    }
    let Some(codes) = a
        .body
        .get("recovery_codes")
        .and_then(Value::as_array)
        .cloned()
    else {
        return bad_reply();
    };
    let epoch = a.body.get("password_epoch").and_then(Value::as_i64);
    let id = match session_for(&state, &headers, &a.body["user"], epoch, true).await {
        Ok(id) => id,
        Err(r) => return *r,
    };
    let res = Json(json!({
        "kind": a.body.get("kind"),
        "recovery_codes": codes,
    }))
    .into_response();
    with_cookies(res, &[clear(SETUP_COOKIE), crate::session::set_cookie(&id)])
}

// --- Signed in ---------------------------------------------------------------------------------

/// A signed-in person's call, for them, with their assertion. A write whose answer was lost
/// is `502 unknown_outcome`.
async fn signed_call(
    state: &AppState,
    signed: &Signed,
    method: reqwest::Method,
    segments: &[&str],
    body: Option<&Value>,
) -> Result<Answer, Box<Response>> {
    let write = method != reqwest::Method::GET;
    match signed
        .toolserver(state)
        .password_call(method, segments, Some(signed.handle()), body)
        .await
    {
        Ok((status, body, retry_after)) => Ok(Answer {
            status,
            body,
            retry_after,
        }),
        Err(UpstreamError::Unknown { why, .. }) if write => {
            tracing::warn!(why = %why, "a password change's answer was lost");
            Err(Box::new(
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "unknown_outcome",
                    "PM OS can't tell whether that change was made. Sign in again to see.",
                )
                .into_response(),
            ))
        }
        Err(e) => Err(Box::new(ApiError::upstream(e).into_response())),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeBody {
    current_password: String,
    code: String,
    new_password: String,
}

/// `POST /api/auth/password/change {current_password, code, new_password}`: `204` with a new
/// session cookie; every other session of the person ends.
async fn change(
    State(state): State<AppState>,
    signed: Signed,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    let b: ChangeBody = parse(&body)?;
    let req = json!({
        "current_password": b.current_password,
        "code": b.code,
        "new_password": b.new_password,
    });
    let a = match signed_call(
        &state,
        &signed,
        reqwest::Method::POST,
        &["change"],
        Some(&req),
    )
    .await
    {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 200 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    let epoch = a.body.get("password_epoch").and_then(Value::as_i64);
    state
        .0
        .sessions
        .remove_user(signed.handle())
        .await
        .map_err(ApiError::store)?;
    // The cached answer holds the old epoch: ask afresh, so the new session isn't ended by it.
    state.0.directory.forget(signed.handle());
    let user = match state
        .0
        .directory
        .resolve(&pmos_directory::resolve::Lookup::handle(signed.handle()))
        .await
    {
        Ok(u) => u,
        Err(_) => return Err(super::users_unavailable()),
    };
    let id = start_session(&state, &headers, &user, "password", None, epoch).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, crate::session::set_cookie(&id))],
    )
        .into_response())
}

/// `POST /api/auth/password/recovery-codes {code}`: ten new recovery codes, once.
async fn recovery_codes(State(state): State<AppState>, signed: Signed, body: Bytes) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    let b: CodeBody = parse(&body)?;
    let a = match signed_call(
        &state,
        &signed,
        reqwest::Method::POST,
        &["recovery-codes"],
        Some(&json!({ "code": b.code })),
    )
    .await
    {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 200 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    Ok(Json(json!({ "recovery_codes": a.body.get("recovery_codes") })).into_response())
}

/// `GET /api/auth/password/status`: the signed-in person's own.
async fn status(State(state): State<AppState>, signed: Signed) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    let a = match signed_call(&state, &signed, reqwest::Method::GET, &["status"], None).await {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 200 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    let pick = |k: &str| a.body.get(k).and_then(Value::as_bool).unwrap_or(false);
    Ok(Json(json!({
        "enabled": pick("enabled"),
        "password_set": pick("password_set"),
        "second_step_enrolled": pick("second_step_enrolled"),
        "recovery_codes_left": a.body.get("recovery_codes_left").and_then(Value::as_u64).unwrap_or(0),
    }))
    .into_response())
}

/// `POST /api/admin/users/{handle}/password-setup-code`: an admin makes a one-time setup (or
/// reset) code, shown once.
async fn issue_setup_code(
    State(state): State<AppState>,
    signed: Signed,
    Path(handle): Path<String>,
) -> ApiResult {
    if !state.0.password_login {
        return Err(off());
    }
    signed.need(Permission::ManageMembers).map_err(|_| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Only an active admin can make setup codes. Nothing changed.",
        )
    })?;
    if !pmos_contract::model::valid_handle(&handle) {
        return Err(ApiError::not_found());
    }
    let a = match signed_call(
        &state,
        &signed,
        reqwest::Method::POST,
        &["setup-codes"],
        Some(&json!({ "handle": handle })),
    )
    .await
    {
        Ok(a) => a,
        Err(r) => return Ok(*r),
    };
    if a.status != 201 {
        return Ok(refused(a.status, &a.body, a.retry_after));
    }
    tracing::info!(admin = %signed.handle(), target = %handle, "password setup code made");
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "setup_code": a.body.get("setup_code"),
            "handle": a.body.get("handle"),
            "kind": a.body.get("kind"),
            "expires_at": a.body.get("expires_at"),
        })),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-20T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::seconds(secs)
    }

    #[test]
    fn an_address_waits_after_twenty_refusals_in_fifteen_minutes() {
        let l = SourceLimiter::default();
        let a: Option<IpAddr> = Some("203.0.113.7".parse().unwrap());
        let b: Option<IpAddr> = Some("203.0.113.8".parse().unwrap());
        for i in 0..SOURCE_REFUSALS as i64 {
            assert!(l.check(a, at(i)).is_ok());
            l.refused(a, at(i));
        }
        assert_eq!(l.check(a, at(30)), Err(15 * 60 - 30));
        assert!(
            l.check(b, at(30)).is_ok(),
            "another address has its own count"
        );
        // The first refusal leaves the window after 15 minutes: one more try.
        assert!(l.check(a, at(15 * 60)).is_ok());
        l.refused(a, at(15 * 60));
        assert!(l.check(a, at(15 * 60)).is_err());
        assert!(l.check(a, at(15 * 60 + 20 * 60)).is_ok());
    }

    #[test]
    fn the_address_behind_a_trusted_proxy_is_the_forwarded_one() {
        let peer: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let proxy: IpAddr = "10.0.0.1".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.9, 203.0.113.7, 10.0.0.1"),
        );
        // Untrusted peer: its own address, whatever it forwards.
        assert_eq!(source(Some(peer), &h, &[]), Some(proxy));
        // Trusted: the right-most address that isn't a proxy (the client's own claim to the
        // left of it is not believed).
        assert_eq!(source(Some(peer), &h, &[proxy]), "203.0.113.7".parse().ok());
        assert_eq!(source(Some(peer), &HeaderMap::new(), &[proxy]), Some(proxy));
        assert_eq!(source(None, &h, &[proxy]), None);
    }

    #[test]
    fn the_cookies_are_host_only_http_only_and_strict() {
        assert_eq!(
            cookie(PENDING_COOKIE, "t", 300),
            "__Host-pmos-pending=t; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=300"
        );
        assert!(clear(SETUP_COOKIE).ends_with("Max-Age=0"));
        let mut h = HeaderMap::new();
        let v = "a".repeat(43);
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("x=1; {SETUP_COOKIE}={v}")).unwrap(),
        );
        assert_eq!(read_cookie(&h, SETUP_COOKIE), Some(v));
        assert_eq!(read_cookie(&h, PENDING_COOKIE), None);
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-pmos-setup=short"),
        );
        assert_eq!(read_cookie(&h, SETUP_COOKIE), None);
    }
}
