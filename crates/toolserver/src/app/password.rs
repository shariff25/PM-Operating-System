//! Password sign-in's routes (`docs/password-sign-in.md`; `docs/toolserver.md`, "Password
//! sign-in"). `pmos-web`'s token only: the orchestrator's, the console's and the Slack bot's
//! get `403`, and no token `401`, before anything reads the request.
//!
//! | Route | Assertion | Does |
//! | --- | --- | --- |
//! | `POST /internal/password/sign-in` | none (nobody is signed in yet) | `{sign_in_name, password}` → a pending token, or the uniform `401 sign_in_failed` |
//! | `POST /internal/password/second-step` | none | `{pending_token, code}` or `{pending_token, recovery_code}` → the user and the credential's epoch |
//! | `POST /internal/password/setup/begin` | none | `{sign_in_name, setup_code}` → a setup token, or the uniform `401 setup_failed` |
//! | `POST /internal/password/setup/password` | none | `{setup_token, password}` |
//! | `POST /internal/password/setup/authenticator` | none | `{setup_token}` → the enrolment secret and URI, once |
//! | `POST /internal/password/setup/confirm` | none | `{setup_token, code}` → recovery codes, the user, the epoch |
//! | `POST /internal/password/change?user=` | the person's | `{current_password, code, new_password}` → the new epoch |
//! | `POST /internal/password/recovery-codes?user=` | the person's | `{code}` → ten new recovery codes |
//! | `GET /internal/password/status?user=` | the person's | their own password status |
//! | `POST /internal/password/setup-codes?user=<admin>` | the admin's | `{handle}` → a setup or reset code, once |
//!
//! **Every route runs only in assertion `require`** with no development override
//! (`503 assertion_required` otherwise), so password sign-in never runs on a stack that
//! doesn't sign. The signed-in routes need `pmos-web`'s assertion for the `user=` they name.
//! Bodies are at most 4 KiB. Answers are `no-store`. Nothing here logs a password, a code, a
//! token, a secret or a sign-in name.

use axum::extract::{OriginalUri, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use super::admin::{coded, require_assertion};
use super::directory::resolve_entry;
use super::users::writes_allowed;
use super::{bearer, client_for, error, AppState};
use crate::audit::Client;
use crate::password::{self, PwError, SecondStep, Secret};
use crate::users::Actor;

/// The largest body a password route reads.
pub const MAX_PASSWORD_BODY: usize = 4 * 1024;

pub(super) fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/password/sign-in", post(sign_in))
        .route("/password/second-step", post(second_step))
        .route("/password/setup/begin", post(setup_begin))
        .route("/password/setup/password", post(setup_password))
        .route("/password/setup/authenticator", post(setup_authenticator))
        .route("/password/setup/confirm", post(setup_confirm))
        .route("/password/change", post(change))
        .route("/password/recovery-codes", post(regenerate))
        .route("/password/status", get(status))
        .route("/password/setup-codes", post(issue))
        .layer(middleware::from_fn_with_state(state, identify))
}

/// The routes a signed-in person calls, with their assertion.
fn signed_in_route(path: &str) -> bool {
    let path = path.strip_prefix("/internal").unwrap_or(path);
    matches!(
        path.trim_end_matches('/'),
        "/password/change" | "/password/recovery-codes" | "/password/status" | "/password/setup-codes"
    )
}

/// `pmos-web` only, in `require`; a signed-in route with its assertion.
async fn identify(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let client = match client_for(&state, bearer(&req)) {
        Some(Client::Web) => Client::Web,
        Some(_) => {
            return error(
                StatusCode::FORBIDDEN,
                "only pmos-web may use the password routes",
            )
        }
        None => return error(StatusCode::UNAUTHORIZED, "service token required"),
    };
    let assertions = &state.people.assertions;
    if !writes_allowed(assertions) {
        tracing::warn!("password route refused: the tool server isn't running in assertion require");
        return no_store(coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "assertion_required",
            "password sign-in needs signed user assertions (PMOS_ASSERTION_MODE=require); this tool server runs without them",
        ));
    }
    let path = req
        .extensions()
        .get::<OriginalUri>()
        .map(|u| u.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let mut req = req;
    if signed_in_route(&path) {
        match require_assertion(assertions, client, req, MAX_PASSWORD_BODY).await {
            Ok(checked) => req = checked,
            Err(refused) => return *refused,
        }
    } else if req.method() != Method::POST {
        return error(StatusCode::METHOD_NOT_ALLOWED, "POST only");
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn ok(status: StatusCode, body: Value) -> Response {
    no_store((status, Json(body)).into_response())
}

/// A refusal: `{code, error}`, with `attempts_left` for `code_failed`, `reason` for
/// `weak_password` and `Retry-After` for `too_many_attempts`. Never a value anyone sent.
fn refused(e: PwError) -> Response {
    if let PwError::Unavailable(why) = &e {
        tracing::error!(error = %why, "password sign-in: storage unavailable");
    } else {
        tracing::info!(code = e.code(), "password route refused");
    }
    let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_REQUEST);
    let mut body = json!({ "code": e.code(), "error": e.message() });
    match &e {
        PwError::CodeFailed { attempts_left } => body["attempts_left"] = json!(attempts_left),
        PwError::Weak(w) => body["reason"] = json!(w.code()),
        _ => {}
    }
    let mut res = ok(status, body);
    if let PwError::TooMany { retry_after } = e {
        if let Ok(v) = HeaderValue::from_str(&retry_after.to_string()) {
            res.headers_mut().insert(header::RETRY_AFTER, v);
        }
    }
    res
}

fn bad_request() -> Response {
    no_store(coded(
        StatusCode::BAD_REQUEST,
        "bad_request",
        "the body isn't the JSON this route takes (see docs/toolserver.md)",
    ))
}

fn not_configured() -> Response {
    no_store(coded(
        StatusCode::NOT_IMPLEMENTED,
        "not_configured",
        "password sign-in is off at the tool server (it needs PMOS_VAULT_KEY)",
    ))
}

/// The body as `T`, or `400`. Unknown fields are refused.
fn body<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Box<Response>> {
    if bytes.len() > MAX_PASSWORD_BODY {
        return Err(Box::new(bad_request()));
    }
    serde_json::from_slice(bytes).map_err(|_| Box::new(bad_request()))
}

macro_rules! service {
    ($state:expr) => {
        match $state.svc.passwords() {
            Some(p) => p.clone(),
            None => return not_configured(),
        }
    };
}

macro_rules! parse {
    ($bytes:expr) => {
        match body(&$bytes) {
            Ok(b) => b,
            Err(r) => return *r,
        }
    };
}

/// The user as `resolve` answers for `pmos-web`, with the credential's epoch.
async fn user_answer(
    state: &AppState,
    handle: &str,
    epoch: i64,
) -> Result<Value, Box<Response>> {
    let dir = match state.svc.people().directory().await {
        Ok(d) => d,
        Err(e) => return Err(Box::new(refused(PwError::from(e)))),
    };
    match dir.get(handle) {
        Some(u) => {
            let mut v = resolve_entry(u);
            v["password_epoch"] = json!(epoch);
            Ok(v)
        }
        None => Err(Box::new(refused(PwError::SignInExpired))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInBody {
    sign_in_name: String,
    password: String,
}

async fn sign_in(State(state): State<AppState>, bytes: axum::body::Bytes) -> Response {
    let svc = service!(state);
    let b: SignInBody = parse!(bytes);
    let password = Secret::new(b.password);
    let store = state.svc.stores().users.clone();
    match svc
        .sign_in(store.passwords(), &b.sign_in_name, &password, Utc::now())
        .await
    {
        Ok(started) => ok(
            StatusCode::OK,
            json!({
                "pending_token": started.token.expose(),
                "expires_at": started.expires_at,
            }),
        ),
        Err(e) => refused(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecondStepBody {
    pending_token: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    recovery_code: Option<String>,
}

async fn second_step(State(state): State<AppState>, bytes: axum::body::Bytes) -> Response {
    let svc = service!(state);
    let b: SecondStepBody = parse!(bytes);
    let step = match (b.code, b.recovery_code) {
        (Some(c), None) => SecondStep::Code(Secret::new(c)),
        (None, Some(r)) => SecondStep::Recovery(Secret::new(r)),
        _ => return bad_request(),
    };
    let store = state.svc.stores().users.clone();
    match svc
        .second_step(store.passwords(), &b.pending_token, &step, Utc::now())
        .await
    {
        Ok(done) => match user_answer(&state, &done.handle, done.epoch).await {
            Ok(user) => ok(
                StatusCode::OK,
                json!({
                    "user": user,
                    "password_epoch": done.epoch,
                    "recovery_code_used": done.recovery_code_used,
                    "recovery_codes_left": done.recovery_codes_left,
                }),
            ),
            Err(r) => *r,
        },
        Err(e) => refused(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupBeginBody {
    sign_in_name: String,
    setup_code: String,
}

async fn setup_begin(State(state): State<AppState>, bytes: axum::body::Bytes) -> Response {
    let svc = service!(state);
    let b: SetupBeginBody = parse!(bytes);
    let code = Secret::new(b.setup_code);
    let store = state.svc.stores().users.clone();
    match svc
        .setup_begin(store.passwords(), &b.sign_in_name, &code, Utc::now())
        .await
    {
        Ok(s) => ok(
            StatusCode::OK,
            json!({
                "setup_token": s.token.expose(),
                "handle": s.handle,
                "kind": s.kind.as_str(),
                "expires_at": s.expires_at,
            }),
        ),
        Err(e) => refused(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupPasswordBody {
    setup_token: String,
    password: String,
}

async fn setup_password(State(state): State<AppState>, bytes: axum::body::Bytes) -> Response {
    let svc = service!(state);
    let b: SetupPasswordBody = parse!(bytes);
    let password = Secret::new(b.password);
    let store = state.svc.stores().users.clone();
    match svc
        .setup_password(store.passwords(), &b.setup_token, &password, Utc::now())
        .await
    {
        Ok(()) => ok(StatusCode::OK, json!({ "next": "authenticator" })),
        Err(e) => refused(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenBody {
    setup_token: String,
}

async fn setup_authenticator(State(state): State<AppState>, bytes: axum::body::Bytes) -> Response {
    let svc = service!(state);
    let b: TokenBody = parse!(bytes);
    let store = state.svc.stores().users.clone();
    match svc
        .setup_authenticator(store.passwords(), &b.setup_token, Utc::now())
        .await
    {
        Ok(e) => ok(
            StatusCode::OK,
            json!({
                "otpauth_uri": e.uri.expose(),
                "secret": e.secret.expose(),
                "account": e.account,
                "issuer": password::totp::ISSUER,
                "algorithm": "SHA1",
                "digits": password::totp::DIGITS,
                "period": password::totp::PERIOD,
            }),
        ),
        Err(e) => refused(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmBody {
    setup_token: String,
    code: String,
}

async fn setup_confirm(State(state): State<AppState>, bytes: axum::body::Bytes) -> Response {
    let svc = service!(state);
    let b: ConfirmBody = parse!(bytes);
    let code = Secret::new(b.code);
    let store = state.svc.stores().users.clone();
    match svc
        .setup_confirm(store.passwords(), &b.setup_token, &code, Utc::now())
        .await
    {
        Ok(done) => match user_answer(&state, &done.handle, done.epoch).await {
            Ok(user) => {
                let codes: Vec<&str> = done.recovery_codes.iter().map(|c| c.expose()).collect();
                ok(
                    StatusCode::OK,
                    json!({
                        "user": user,
                        "password_epoch": done.epoch,
                        "kind": done.kind.as_str(),
                        "recovery_codes": codes,
                    }),
                )
            }
            Err(r) => *r,
        },
        Err(e) => refused(e),
    }
}

/// The `user=` a signed-in route names (its assertion's subject, already checked).
#[derive(Deserialize)]
struct Named {
    user: Option<String>,
}

fn named(q: &Named) -> Option<String> {
    q.user
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(String::from)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeBody {
    current_password: String,
    code: String,
    new_password: String,
}

async fn change(
    State(state): State<AppState>,
    Query(q): Query<Named>,
    bytes: axum::body::Bytes,
) -> Response {
    let svc = service!(state);
    let Some(user) = named(&q) else {
        return bad_request();
    };
    let b: ChangeBody = parse!(bytes);
    let (current, code, new) = (
        Secret::new(b.current_password),
        Secret::new(b.code),
        Secret::new(b.new_password),
    );
    let store = state.svc.stores().users.clone();
    match svc
        .change(store.passwords(), &user, &current, &code, &new, Utc::now())
        .await
    {
        Ok(epoch) => ok(StatusCode::OK, json!({ "password_epoch": epoch })),
        Err(e) => refused(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBody {
    code: String,
}

async fn regenerate(
    State(state): State<AppState>,
    Query(q): Query<Named>,
    bytes: axum::body::Bytes,
) -> Response {
    let svc = service!(state);
    let Some(user) = named(&q) else {
        return bad_request();
    };
    let b: CodeBody = parse!(bytes);
    let code = Secret::new(b.code);
    let store = state.svc.stores().users.clone();
    match svc
        .regenerate_recovery_codes(store.passwords(), &user, &code, Utc::now())
        .await
    {
        Ok(codes) => {
            let codes: Vec<&str> = codes.iter().map(|c| c.expose()).collect();
            ok(StatusCode::OK, json!({ "recovery_codes": codes }))
        }
        Err(e) => refused(e),
    }
}

async fn status(State(state): State<AppState>, Query(q): Query<Named>) -> Response {
    let Some(user) = named(&q) else {
        return bad_request();
    };
    let store = state.svc.stores().users.clone();
    match store.passwords().password_status(&user, Utc::now()).await {
        Ok(s) => ok(
            StatusCode::OK,
            json!({
                "enabled": state.svc.passwords().is_some(),
                "password_set": s.password_set,
                "second_step_enrolled": s.second_step_enrolled,
                "setup_code_pending": s.setup_code_pending,
                "recovery_codes_left": s.recovery_codes_left,
            }),
        ),
        Err(e) => refused(e.into()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueBody {
    handle: String,
}

async fn issue(
    State(state): State<AppState>,
    Extension(_client): Extension<Client>,
    Query(q): Query<Named>,
    bytes: axum::body::Bytes,
) -> Response {
    if state.svc.passwords().is_none() {
        return not_configured();
    }
    let Some(admin) = named(&q) else {
        return bad_request();
    };
    let b: IssueBody = parse!(bytes);
    let store = state.svc.stores().users.clone();
    match password::issue_setup_code(
        store.passwords(),
        &Actor::web(&admin),
        b.handle.trim(),
        Utc::now(),
    )
    .await
    {
        Ok(issued) => {
            tracing::info!(admin = %admin, target = %issued.handle, kind = issued.kind.as_str(), "password setup code issued");
            ok(
                StatusCode::CREATED,
                json!({
                    "setup_code": issued.code.expose(),
                    "handle": issued.handle,
                    "kind": issued.kind.as_str(),
                    "expires_at": issued.expires_at,
                }),
            )
        }
        Err(e) => refused(e),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_audit_views_show_password_details() {
        for d in [
            "password; rehashed",
            "recovery_code; recovery_left 9",
            "failures 5; setup",
            "host_user ops; reset",
            "begin; setup",
            "wrong_password",
        ] {
            assert_eq!(super::super::admin::safe_detail(d).as_deref(), Some(d), "{d}");
        }
        assert_eq!(super::super::admin::safe_detail("hunter2hunter2"), None);
    }
}
