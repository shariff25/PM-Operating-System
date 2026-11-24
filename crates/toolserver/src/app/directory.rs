//! The user database's read API (`docs/user-database-spec.md` §2; slice U2).
//!
//! | Route | Token | Answers |
//! | --- | --- | --- |
//! | `GET /internal/directory` | orchestrator (`PMOS_SERVICE_TOKEN`), Slack bot (`PMOS_BOT_TOKEN`) | Every user: `handle`, `display_name`, `status`, `roles`; the Slack bot also `slack_id`. `ETag: "<directory_version>"`; `304` on a matching `If-None-Match`. |
//! | `GET /internal/users/resolve?by=entra&tid=&oid=` / `?by=sign_in_name&name=` / `?by=handle&handle=` | `pmos-web` (`PMOS_WEB_TOKEN`), console (`PMOS_CONSOLE_TS_TOKEN`) | One user: `handle`, `display_name`, `status`, `roles`, `entra`, `sign_in_name_confirmed`, `dual_role`; `404` if none. |
//! | `POST /internal/users/{handle}/identities/entra` `{id_token, nonce}` | `pmos-web` | The first Entra link (§7 step 2; `crate::users::link`), from an ID token this route validates itself. |
//! | `GET /internal/users/entra-link` | `pmos-web` | `{configured, tenant_id, client_id}`: what first-link tokens are validated for (`pmos-web`'s `/readyz` compares). |
//!
//! Any other token, or no token, is `403` or `401`; the gate runs before anything reads the
//! request. **Projections are fixed here:** no route ever serves an address (sign-in name,
//! Atlassian email), a GitHub login, an `oid`, a `tid` or an Atlassian or GitHub id; only the
//! Slack bot's directory carries a Slack id. Web and the console can't list.
//!
//! **The first link never takes `pmos-web`'s word** for who someone is (the review of
//! `b469d5b`, U1): the body is the raw ID token Microsoft issued and the sign-in's `nonce`.
//! The route validates the token with the tenant's own keys (`pmos_entra::Verifier`: the same
//! rules `pmos-web` applies, from one shared crate) for the tenant and client id in its own
//! settings (`PMOS_ENTRA_TENANT_ID`, `PMOS_ENTRA_CLIENT_ID`; without them `501
//! not_configured`), uses each token for one first link only (`token_used` after), and links
//! the `(tid, oid)` the token names to `{handle}` only if the token's `preferred_username` is
//! that user's confirmed sign-in name. Every outcome is audited; a link is on the admin
//! overview.
//!
//! **No user assertion** on these routes, by design: `resolve` and the first link are how a
//! person *gets* a session, so there is no signed-in subject yet (§2). An
//! `X-Pmos-Assertion` header is not read. What limits a stolen web or console token instead:
//! one user per call, no addresses in any answer, and two limits per client. The **miss
//! limit**: at most [`MISSES_PER_MINUTE`] misses in any rolling minute (`404`s from `resolve`,
//! and for `pmos-web` every refusal of the first-link route); past that a miss answers
//! `429 resolve_misses`. The **hit limit**: at most [`HITS_PER_MINUTE`] answers that found
//! someone in any rolling minute; past that `429 resolve_rate`. Every miss is logged by kind,
//! never the value; more than [`HOURLY_WARNING`] in an hour from one client logs a warning
//! ([`MissLimiter::misses_last_hour`] is what U5's overview reads).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, OriginalUri, Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{bearer, client_for, error, unavailable, AppState};
use crate::audit::Client;
use crate::users::link::{EntraLink, LinkError, LinkRefusal};
use crate::users::{rules, Directory, EntraKey, IdentityKind, User};

/// At most this many misses (`404`s) per client in any rolling minute (§2).
pub const MISSES_PER_MINUTE: usize = 20;

/// At most this many `resolve` answers that found someone, per client, in any rolling minute
/// (UM2). Generous on purpose: `pmos-web` and the console cache each answer for 10 seconds per
/// person, so a signed-in person costs at most 6 a minute; 600 covers 100 people active at
/// once (the pilot team is a few dozen) with room for sign-ins, while a stolen token can't
/// confirm more than 600 names, handles or `oid`s a minute.
pub const HITS_PER_MINUTE: usize = 600;

/// More misses than this in an hour from one client raises a warning (§2).
pub const HOURLY_WARNING: u32 = 100;

/// The largest first-link body: an ID token (a few kilobytes) and a nonce.
const MAX_LINK_BODY: usize = 20 * 1024;

const MINUTE: Duration = Duration::from_secs(60);

/// Which clients reach each route.
const DIRECTORY: &[Client] = &[Client::Orchestrator, Client::Bot];
const RESOLVE: &[Client] = &[Client::Web, Client::Console];
const FIRST_LINK: &[Client] = &[Client::Web];

pub(super) fn routes(state: AppState) -> Router<AppState> {
    let limiter = Arc::new(MissLimiter::default());
    let hits = Arc::new(HitLimiter::default());
    let gate = |allowed: &'static [Client]| {
        middleware::from_fn_with_state(
            Gate {
                state: state.clone(),
                allowed,
            },
            gate,
        )
    };
    Router::new()
        .route("/directory", get(directory).route_layer(gate(DIRECTORY)))
        .route("/users/resolve", get(resolve).route_layer(gate(RESOLVE)))
        .route(
            "/users/entra-link",
            get(entra_link_settings).route_layer(gate(FIRST_LINK)),
        )
        .route(
            "/users/{handle}/identities/entra",
            post(first_link)
                .route_layer(gate(FIRST_LINK))
                .layer(DefaultBodyLimit::max(MAX_LINK_BODY)),
        )
        .layer(Extension(limiter))
        .layer(Extension(hits))
}

#[derive(Clone)]
struct Gate {
    state: AppState,
    allowed: &'static [Client],
}

/// The token decides the client; the route decides which clients it serves.
async fn gate(State(g): State<Gate>, mut req: Request, next: Next) -> Response {
    let Some(client) = client_for(&g.state, bearer(&req)) else {
        return error(StatusCode::UNAUTHORIZED, "service token required");
    };
    if !g.allowed.contains(&client) {
        return error(
            StatusCode::FORBIDDEN,
            "this token may not use this endpoint",
        );
    }
    req.extensions_mut().insert(client);
    next.run(req).await
}

// --- The miss limit ------------------------------------------------------------------------

/// Per client: the misses answered `404` in the last minute (at most [`MISSES_PER_MINUTE`]
/// instants), and every miss (limited or not) counted per minute for the last hour.
#[derive(Default)]
struct Window {
    answered: VecDeque<Instant>,
    hour: VecDeque<(Instant, u32)>,
}

/// Whether a miss may be answered `404`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Miss {
    Answer,
    /// Over the limit: `429`, retry after this many seconds.
    Limited(u64),
}

/// The miss-only rate limit (§2): a rolling minute per client. Hits never touch it.
#[derive(Default)]
pub struct MissLimiter {
    windows: Mutex<HashMap<&'static str, Window>>,
}

impl MissLimiter {
    /// Count a miss by `client` at `now`.
    pub fn miss(&self, client: Client, now: Instant) -> Miss {
        let mut windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        let w = windows.entry(client.as_str()).or_default();
        while w
            .answered
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= MINUTE)
        {
            w.answered.pop_front();
        }
        while w
            .hour
            .front()
            .is_some_and(|(t, _)| now.saturating_duration_since(*t) >= 60 * MINUTE)
        {
            w.hour.pop_front();
        }
        let before: u32 = w.hour.iter().map(|(_, n)| n).sum();
        match w.hour.back_mut() {
            Some((start, n)) if now.saturating_duration_since(*start) < MINUTE => *n += 1,
            _ => w.hour.push_back((now, 1)),
        }
        if before == HOURLY_WARNING {
            tracing::warn!(
                client = client.as_str(),
                "more than {HOURLY_WARNING} user lookups that found nobody in an hour from one client"
            );
        }
        if w.answered.len() >= MISSES_PER_MINUTE {
            let oldest = w.answered.front().copied().unwrap_or(now);
            let wait = MINUTE.saturating_sub(now.saturating_duration_since(oldest));
            return Miss::Limited(wait.as_secs().max(1));
        }
        w.answered.push_back(now);
        Miss::Answer
    }

    /// Misses by `client` in the last hour, limited or not (the admin overview's warning).
    pub fn misses_last_hour(&self, client: Client, now: Instant) -> u32 {
        let windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        windows.get(client.as_str()).map_or(0, |w| {
            w.hour
                .iter()
                .filter(|(t, _)| now.saturating_duration_since(*t) < 60 * MINUTE)
                .map(|(_, n)| n)
                .sum()
        })
    }
}

/// The hit limit (UM2): at most [`HITS_PER_MINUTE`] `resolve` answers that found someone,
/// per client, in any rolling minute.
#[derive(Default)]
pub struct HitLimiter {
    windows: Mutex<HashMap<&'static str, VecDeque<Instant>>>,
}

impl HitLimiter {
    /// Count a hit by `client` at `now`: [`Miss::Answer`] to answer it, or
    /// [`Miss::Limited`] past the limit (not counted).
    pub fn hit(&self, client: Client, now: Instant) -> Miss {
        let mut windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        let w = windows.entry(client.as_str()).or_default();
        while w
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= MINUTE)
        {
            w.pop_front();
        }
        if w.len() >= HITS_PER_MINUTE {
            let oldest = w.front().copied().unwrap_or(now);
            let wait = MINUTE.saturating_sub(now.saturating_duration_since(oldest));
            return Miss::Limited(wait.as_secs().max(1));
        }
        w.push_back(now);
        Miss::Answer
    }
}

/// `429` with `code` and `Retry-After`.
fn limited(code: &str, message: &str, retry: u64) -> Response {
    let mut res = coded(StatusCode::TOO_MANY_REQUESTS, code, message);
    if let Ok(v) = HeaderValue::from_str(&retry.to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res
}

/// A miss: `404` (code `not_found`), or `429 resolve_misses` past the limit. `kind` is what
/// was looked up by, for the log; never the value.
fn miss(limiter: &MissLimiter, client: Client, kind: &str, not_found: Response) -> Response {
    match limiter.miss(client, Instant::now()) {
        Miss::Answer => {
            tracing::info!(client = client.as_str(), kind, "user lookup found nobody");
            not_found
        }
        Miss::Limited(retry) => {
            tracing::warn!(
                client = client.as_str(),
                kind,
                "user lookup found nobody, over the miss limit"
            );
            limited(
                "resolve_misses",
                "too many lookups that found nobody; try again shortly",
                retry,
            )
        }
    }
}

fn coded(status: StatusCode, code: &str, message: &str) -> Response {
    let mut res = (status, Json(json!({ "code": code, "error": message }))).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

// --- Projections -----------------------------------------------------------------------------

fn roles(u: &User) -> Vec<&'static str> {
    u.role_set().into_iter().map(|r| r.as_str()).collect()
}

/// One user in the directory: the orchestrator's fields, plus `slack_id` for the Slack bot.
pub fn directory_entry(u: &User, client: Client) -> Value {
    let mut v = json!({
        "handle": u.handle,
        "display_name": u.display_name,
        "status": u.status.as_str(),
        "roles": roles(u),
    });
    if client == Client::Bot {
        v["slack_id"] = json!(u.identity(IdentityKind::Slack).map(|i| i.subject.as_str()));
    }
    v
}

/// One user as `resolve` and the first link answer them (web and the console).
pub fn resolve_entry(u: &User) -> Value {
    json!({
        "handle": u.handle,
        "display_name": u.display_name,
        "status": u.status.as_str(),
        "roles": roles(u),
        "entra": u.entra_state(),
        "sign_in_name_confirmed": u.sign_in_name_confirmed_at.is_some(),
        "dual_role": u.dual_role(),
    })
}

/// The directory's strong ETag.
pub fn etag(version: i64) -> String {
    format!("\"{version}\"")
}

/// Does `If-None-Match` match `etag`? (Weak comparison, as RFC 9110 says for this header.)
fn none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
}

// --- Handlers ------------------------------------------------------------------------------

async fn directory(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    headers: HeaderMap,
) -> Response {
    let dir: Directory = match state.svc.stores().users.snapshot(Utc::now()).await {
        Ok(d) => d,
        Err(e) => return unavailable(e),
    };
    let tag = etag(dir.version);
    let mut res = if none_match(&headers, &tag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let users: Vec<Value> = dir
            .users
            .values()
            .map(|u| directory_entry(u, client))
            .collect();
        Json(json!({ "version": dir.version, "users": users })).into_response()
    };
    if let Ok(v) = HeaderValue::from_str(&tag) {
        res.headers_mut().insert(header::ETAG, v);
    }
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    res
}

#[derive(Deserialize)]
struct ResolveParams {
    by: Option<String>,
    tid: Option<String>,
    oid: Option<String>,
    name: Option<String>,
    handle: Option<String>,
}

/// What `resolve` looks up by, normalized as stored.
enum Lookup {
    Entra(EntraKey),
    SignInName(String),
    Handle(String),
}

impl Lookup {
    fn kind(&self) -> &'static str {
        match self {
            Self::Entra(_) => "entra",
            Self::SignInName(_) => "sign_in_name",
            Self::Handle(_) => "handle",
        }
    }

    fn parse(p: &ResolveParams) -> Option<Self> {
        fn given(v: &Option<String>) -> Option<&str> {
            v.as_deref().filter(|s| !s.is_empty())
        }
        match p.by.as_deref()? {
            "entra" => Some(Self::Entra(EntraKey {
                tid: rules::guid(given(&p.tid)?, "tenant id").ok()?,
                oid: rules::guid(given(&p.oid)?, "object id").ok()?,
            })),
            "sign_in_name" => Some(Self::SignInName(
                rules::address(given(&p.name)?, "sign-in name").ok()?,
            )),
            "handle" => {
                let h = given(&p.handle)?;
                rules::valid_handle(h).then(|| Self::Handle(h.to_string()))
            }
            _ => None,
        }
    }

    fn find<'a>(&self, dir: &'a Directory) -> Option<&'a User> {
        match self {
            Self::Handle(h) => dir.get(h),
            Self::SignInName(n) => dir
                .users
                .values()
                .find(|u| u.sign_in_name.as_deref() == Some(n.as_str())),
            Self::Entra(k) => dir.users.values().find(|u| {
                u.identity(IdentityKind::Entra)
                    .is_some_and(|e| e.issuer == k.tid && e.subject == k.oid)
            }),
        }
    }
}

async fn resolve(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Extension(limiter): Extension<Arc<MissLimiter>>,
    Extension(hits): Extension<Arc<HitLimiter>>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let lookup = axum::extract::Query::<ResolveParams>::try_from_uri(&uri)
        .ok()
        .and_then(|q| Lookup::parse(&q.0));
    let Some(lookup) = lookup else {
        return coded(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid",
            "use by=entra&tid=&oid= (GUIDs), by=sign_in_name&name= (an email address) or by=handle&handle=",
        );
    };
    let dir = match state.svc.stores().users.snapshot(Utc::now()).await {
        Ok(d) => d,
        Err(e) => return unavailable(e),
    };
    match lookup.find(&dir) {
        Some(u) => {
            if let Miss::Limited(retry) = hits.hit(client, Instant::now()) {
                tracing::warn!(
                    client = client.as_str(),
                    kind = lookup.kind(),
                    "user lookups over the hit limit"
                );
                return limited(
                    "resolve_rate",
                    "too many user lookups; try again shortly",
                    retry,
                );
            }
            let mut v = resolve_entry(u);
            if client == Client::Web {
                // A password session holds the credential's epoch and ends when it moves
                // (docs/password-sign-in.md): pmos-web's answer carries it; `null` without one.
                match state
                    .svc
                    .stores()
                    .users
                    .passwords()
                    .password_status(&u.handle, Utc::now())
                    .await
                {
                    Ok(s) => v["password_epoch"] = json!(s.password_set.then_some(s.epoch)),
                    Err(e) => return unavailable(e),
                }
            }
            let mut res = Json(v).into_response();
            res.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
        None => {
            // For the admin overview's warning (slice U3).
            state.svc.people().resolve_misses().record(client.as_str());
            miss(
                &limiter,
                client,
                lookup.kind(),
                coded(StatusCode::NOT_FOUND, "not_found", "no such user"),
            )
        }
    }
}

/// The first link's body: the raw ID token Microsoft issued to `pmos-web`, and the `nonce`
/// `pmos-web` put in that sign-in's authorize request. Nothing else is accepted: no `tid`,
/// `oid` or name the route would have to take on `pmos-web`'s word.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkBody {
    id_token: String,
    nonce: String,
}

fn link_status(why: LinkRefusal) -> StatusCode {
    match why {
        LinkRefusal::Invalid | LinkRefusal::InvalidToken | LinkRefusal::EntraMisconfigured => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        LinkRefusal::NotFound => StatusCode::NOT_FOUND,
        LinkRefusal::NotActive
        | LinkRefusal::Unconfirmed
        | LinkRefusal::HostOnly
        | LinkRefusal::EvalsAccount
        | LinkRefusal::TokenUsed
        | LinkRefusal::NotAMember => StatusCode::FORBIDDEN,
        LinkRefusal::Linked | LinkRefusal::Taken => StatusCode::CONFLICT,
        LinkRefusal::EntraUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        LinkRefusal::NotConfigured => StatusCode::NOT_IMPLEMENTED,
    }
}

/// A first-link refusal's answer. Every refusal counts as a miss for the caller (UM2), so the
/// route is no better an oracle than `resolve`.
fn link_refused(
    state: &AppState,
    limiter: &MissLimiter,
    client: Client,
    why: LinkRefusal,
) -> Response {
    tracing::info!(code = why.code(), "first Entra link refused");
    state.svc.people().resolve_misses().record(client.as_str());
    miss(
        limiter,
        client,
        "first_link",
        coded(link_status(why), why.code(), why.message()),
    )
}

/// A refusal decided before the store saw the link (no settings, the tenant's keys unreadable,
/// the body, the token, a replay): counted as a miss **first**, so past the miss limit it
/// answers `429` with no directory snapshot and no audit row (the re-check of eb24d28, LB:
/// a web-token holder can't flood the audit log); otherwise audited here as the store audits
/// its own (`users.identity_link`, denied, `anonymous`, detail `entra; <code>`, the target
/// handle only when that user exists).
async fn refuse_before_store(
    state: &AppState,
    limiter: &MissLimiter,
    client: Client,
    handle: &str,
    why: LinkRefusal,
    now: chrono::DateTime<Utc>,
) -> Response {
    state.svc.people().resolve_misses().record(client.as_str());
    if let Miss::Limited(retry) = limiter.miss(client, Instant::now()) {
        tracing::warn!(
            client = client.as_str(),
            code = why.code(),
            "first Entra link refused, over the miss limit"
        );
        return limited(
            "resolve_misses",
            "too many lookups that found nobody; try again shortly",
            retry,
        );
    }
    tracing::info!(code = why.code(), "first Entra link refused");
    let dir = state.svc.stores().users.snapshot(now).await.ok();
    let (actor, event) = crate::users::link::refused_event(dir.as_ref(), handle, why);
    if let Err(e) = state
        .svc
        .stores()
        .audit
        .record(crate::users::link::audit(&actor, &event, now))
        .await
    {
        return unavailable(e);
    }
    coded(link_status(why), why.code(), why.message())
}

/// `GET /internal/users/entra-link`: the tenant and client id first-link tokens are validated
/// for, or `configured: false`. Not secret (both are in every ID token); `pmos-web`'s
/// `/readyz` compares them with its own.
async fn entra_link_settings(State(state): State<AppState>) -> Response {
    let tenant = state.people.entra.as_ref().map(|c| c.verifier.tenant());
    let mut res = Json(json!({
        "configured": tenant.is_some(),
        "tenant_id": tenant.map(|t| t.tenant_id.as_str()),
        "client_id": tenant.map(|t| t.client_id.as_str()),
    }))
    .into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

async fn first_link(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Extension(limiter): Extension<Arc<MissLimiter>>,
    Path(handle): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let now = Utc::now();
    let Some(check) = state.people.entra.clone() else {
        tracing::error!(
            "first Entra link refused: no PMOS_ENTRA_TENANT_ID and PMOS_ENTRA_CLIENT_ID here"
        );
        // Counted and audited like every refusal (LD), the limit first (LB).
        return refuse_before_store(
            &state,
            &limiter,
            client,
            &handle,
            LinkRefusal::NotConfigured,
            now,
        )
        .await;
    };
    let Ok(b) = serde_json::from_slice::<LinkBody>(&body) else {
        return refuse_before_store(&state, &limiter, client, &handle, LinkRefusal::Invalid, now)
            .await;
    };
    // The identity comes from the token, checked here with the tenant's own keys: never from
    // pmos-web's word.
    let identity = match check.verifier.validate(&b.id_token, &b.nonce, now).await {
        Ok(identity) => identity,
        Err(pmos_entra::Failure::Unavailable(why)) => {
            tracing::warn!(why, "first Entra link: the tenant's keys couldn't be read");
            return refuse_before_store(
                &state,
                &limiter,
                client,
                &handle,
                LinkRefusal::EntraUnavailable,
                now,
            )
            .await;
        }
        Err(f) => {
            tracing::warn!(why = f.why(), "first Entra link: the ID token was refused");
            let why = LinkRefusal::from_token(&f);
            return refuse_before_store(&state, &limiter, client, &handle, why, now).await;
        }
    };
    // One first link per token: a captured token can't be replayed here, before or after a
    // restart (M4: the store keeps the used tokens, in Postgres in a deployment).
    let first_use = match state
        .svc
        .stores()
        .users
        .first_token_use(&identity.replay_key(), identity.replay_until(), now)
        .await
    {
        Ok(first) => first,
        Err(e) => return unavailable(e),
    };
    if !first_use {
        tracing::warn!("first Entra link: an ID token presented again");
        return refuse_before_store(
            &state,
            &limiter,
            client,
            &handle,
            LinkRefusal::TokenUsed,
            now,
        )
        .await;
    }
    let Some(sign_in_name) = identity.preferred_username.clone() else {
        return refuse_before_store(
            &state,
            &limiter,
            client,
            &handle,
            LinkRefusal::NotFound,
            now,
        )
        .await;
    };
    let link = EntraLink {
        handle,
        sign_in_name,
        key: EntraKey {
            tid: identity.tid.clone(),
            oid: identity.oid.clone(),
        },
    };
    match state.svc.stores().users.link_entra(&link, now).await {
        Ok(done) => {
            if done.first_link {
                tracing::info!("first Entra link stored");
            }
            // As `resolve` answers pmos-web (the only client here): with the password epoch,
            // since pmos-web caches this answer under the person's handle too.
            let mut user = resolve_entry(&done.user);
            match state
                .svc
                .stores()
                .users
                .passwords()
                .password_status(&done.user.handle, Utc::now())
                .await
            {
                Ok(s) => user["password_epoch"] = json!(s.password_set.then_some(s.epoch)),
                Err(e) => return unavailable(e),
            }
            let mut res = Json(json!({
                "first_link": done.first_link,
                "user": user,
            }))
            .into_response();
            res.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
        // The store audited it.
        Err(LinkError::Refused(why)) => link_refused(&state, &limiter, client, why),
        Err(LinkError::Store(e)) => unavailable(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_21st_miss_in_a_rolling_minute_is_limited_per_client() {
        let l = MissLimiter::default();
        let t0 = Instant::now();
        for i in 0..MISSES_PER_MINUTE {
            let at = t0 + Duration::from_millis(i as u64 * 100);
            assert_eq!(l.miss(Client::Web, at), Miss::Answer, "miss {i}");
        }
        let at = t0 + Duration::from_secs(10);
        assert!(matches!(l.miss(Client::Web, at), Miss::Limited(s) if s >= 49));
        // Another client has its own window.
        assert_eq!(l.miss(Client::Console, at), Miss::Answer);
        // A minute after the first miss, one slot is free again; then limited again.
        let later = t0 + MINUTE;
        assert_eq!(l.miss(Client::Web, later), Miss::Answer);
        assert!(matches!(l.miss(Client::Web, later), Miss::Limited(_)));
        // Every miss counts for the hour, limited or not.
        assert_eq!(l.misses_last_hour(Client::Web, later), 23);
        assert_eq!(l.misses_last_hour(Client::Web, later + 61 * MINUTE), 0);
    }

    #[test]
    fn if_none_match_matches_strong_weak_lists_and_star() {
        let tag = etag(7);
        assert_eq!(tag, "\"7\"");
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::IF_NONE_MATCH, HeaderValue::from_str(v).unwrap());
            m
        };
        for yes in ["\"7\"", "W/\"7\"", "\"3\", \"7\"", "*"] {
            assert!(none_match(&h(yes), &tag), "{yes}");
        }
        for no in ["\"6\"", "7", "\"77\""] {
            assert!(!none_match(&h(no), &tag), "{no}");
        }
        assert!(!none_match(&HeaderMap::new(), &tag));
    }
}
