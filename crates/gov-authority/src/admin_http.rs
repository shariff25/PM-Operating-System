//! The Authority's listener for people (spec §7.6.4): sign-in against the tenant, a
//! session in a cookie, and the admin's and owner's calls. Served on loopback behind a
//! proxy that speaks TLS; this program does not.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use gov_core::enrol::EnrolKind;
use gov_core::{Digest, Envelope, Id};
use gov_signin::Verifier;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use sha2::Digest as _;

use crate::{Account, Authority, AuthorityError, MandateAsk, Role, Session, Store};

const MAX_BODY: usize = 256 * 1024;
const MAX_TOKEN_ANSWER: usize = 64 * 1024;
/// How long a sign-in may take from its start to the code coming back.
const FLOW_SECS: i64 = 600;
const MAX_FLOWS: usize = 10_000;

const CSRF_HEADER: &str = "x-gov-csrf";

pub struct AdminConfig {
    /// This listener's own public origin, as people's browsers see it.
    pub origin: String,
    pub client_secret: String,
    /// The header the operator's proxy puts each caller's address in.
    pub client_header: Option<String>,
    pub sign_ins_per_second: u32,
    pub sign_ins_per_client_per_second: u32,
    /// Fake data only: allows a plain-HTTP origin or identity provider on loopback.
    pub fake_data: bool,
}

struct Flow {
    state: String,
    nonce: String,
    verifier: String,
    started: i64,
}

#[derive(Default)]
struct Rate {
    second: i64,
    total: u32,
    by_client: HashMap<[u8; 16], u32>,
}

#[derive(Default)]
pub struct Counters {
    pub signed_in: AtomicU64,
    pub limited: AtomicU64,
    /// Why sign-ins failed. The caller is told none of it.
    pub failed: Mutex<BTreeMap<&'static str, u64>>,
}

pub struct Admin<S> {
    authority: Arc<Authority<S>>,
    verifier: Verifier,
    config: AdminConfig,
    /// The origin's host and port, as a `Host` header carries them.
    host: String,
    secure: bool,
    http: reqwest::Client,
    clock: fn() -> i64,
    flows: Mutex<HashMap<Digest, Flow>>,
    rate: Mutex<Rate>,
    pub counters: Counters,
}

fn random() -> Result<String, AuthorityError> {
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(gov_core::keys::random_salt()?))
}

fn pre_digest(value: &str) -> Digest {
    Digest::of(&[b"gov.signin.pre.v1\0", value.as_bytes()])
}

fn same(a: &str, b: &str) -> bool {
    Digest::of(&[a.as_bytes()]) == Digest::of(&[b.as_bytes()])
}

/// The one cookie of that name. Sent twice, it is not judged by whichever comes first.
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut found = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string());
    match (found.next(), found.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

impl<S: Store + 'static> Admin<S> {
    pub fn new(
        authority: Arc<Authority<S>>,
        verifier: Verifier,
        config: AdminConfig,
        clock: fn() -> i64,
    ) -> Result<Self, String> {
        let origin =
            reqwest::Url::parse(&config.origin).map_err(|_| "the admin origin is not a URL")?;
        if origin.origin().ascii_serialization() != config.origin {
            return Err("the admin origin is an origin: a scheme, a host and no path".into());
        }
        let loopback = gov_signin::is_loopback_host(&origin);
        let secure = origin.scheme() == "https";
        if !secure && !(loopback && config.fake_data) {
            return Err(
                "the admin origin is https, or a literal loopback address with fake data".into(),
            );
        }
        if verifier.tenant().plain_http() && !config.fake_data {
            return Err(
                "the identity provider is reached over https unless the data is fake".into(),
            );
        }
        // Behind the proxy every caller has one address: without the header a stranger
        // meets only the overall limit and uses it up for everyone (GR-196).
        if !loopback && config.client_header.is_none() {
            return Err(
                "name the header the proxy puts each caller's address in (the client header)"
                    .into(),
            );
        }
        let host = match origin.port() {
            Some(port) => format!("{}:{port}", origin.host_str().unwrap_or_default()),
            None => origin.host_str().unwrap_or_default().to_string(),
        };
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(gov_signin::FETCH_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Admin {
            authority,
            verifier,
            config,
            host,
            secure,
            http,
            clock,
            flows: Mutex::default(),
            rate: Mutex::default(),
            counters: Counters::default(),
        })
    }

    fn session_cookie(&self) -> &'static str {
        if self.secure {
            "__Host-gov_session"
        } else {
            "gov_session"
        }
    }

    fn set_cookie(&self, name: &str, value: &str, rest: &str) -> HeaderValue {
        let secure = if self.secure { "; Secure" } else { "" };
        HeaderValue::from_str(&format!("{name}={value}; HttpOnly{secure}; {rest}"))
            .unwrap_or(HeaderValue::from_static(""))
    }

    /// The pre-sign-in cookie. Host-only on https, so a sibling host cannot plant one
    /// (review finding GR-207).
    fn pre_cookie(&self) -> (&'static str, &'static str) {
        if self.secure {
            ("__Host-gov_pre", "/")
        } else {
            ("gov_pre", "/v1/signin")
        }
    }

    /// The caller's address as the proxy wrote it: the last entry of the last line of
    /// the named header. What comes before it is the caller's own text (GR-206).
    fn client(&self, headers: &HeaderMap) -> Option<Option<[u8; 16]>> {
        let name = self.config.client_header.as_ref()?;
        Some(
            headers
                .get_all(name)
                .iter()
                .next_back()
                .and_then(|v| v.to_str().ok())
                .and_then(|line| line.rsplit(',').next())
                .and_then(|last| last.trim().parse::<std::net::IpAddr>().ok())
                .map(crate::http::source_of),
        )
    }

    fn within_limit(&self, headers: &HeaderMap, now: i64) -> bool {
        let client = self.client(headers);
        let mut rate = self.rate.lock().unwrap_or_else(|p| p.into_inner());
        if rate.second != now {
            *rate = Rate {
                second: now,
                ..Rate::default()
            };
        }
        // A client is held to its own limit first, and only what that lets through
        // counts toward everyone's: one caller cannot use the whole up (GR-205).
        let own = match client {
            None => true,
            // The header is named and this request has none the proxy could have set.
            Some(None) => false,
            Some(Some(client)) => {
                let known =
                    rate.by_client.len() < MAX_FLOWS || rate.by_client.contains_key(&client);
                let count = rate.by_client.entry(client).or_insert(0);
                *count += 1;
                known && *count <= self.config.sign_ins_per_client_per_second
            }
        };
        let ok = own && {
            rate.total += 1;
            rate.total <= self.config.sign_ins_per_second
        };
        if !ok {
            self.counters.limited.fetch_add(1, Ordering::Relaxed);
        }
        ok
    }

    fn failed(&self, why: &'static str) -> Response {
        *self
            .counters
            .failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(why)
            .or_insert(0) += 1;
        StatusCode::FORBIDDEN.into_response()
    }

    async fn signed_in(&self, headers: &HeaderMap) -> Result<Session, StatusCode> {
        let Some(value) = cookie(headers, self.session_cookie()) else {
            return Err(StatusCode::UNAUTHORIZED);
        };
        self.authority
            .session(&value, (self.clock)())
            .await
            .map_err(|_| StatusCode::UNAUTHORIZED)
    }
}

pub fn router<S: Store + 'static>(admin: Arc<Admin<S>>) -> Router {
    Router::new()
        .route("/v1/signin/start", get(start::<S>))
        .route("/v1/signin/callback", get(callback::<S>))
        .route("/v1/signout", post(sign_out::<S>))
        .route("/v1/me", get(me::<S>))
        .route("/v1/people", post(add_person::<S>))
        .route("/v1/people/{id}/link", post(link::<S>))
        .route("/v1/people/{id}/offboard", post(offboard::<S>))
        .route("/v1/agents", post(register_agent::<S>))
        .route("/v1/agents/{id}/{change}", post(change_agent::<S>))
        .route("/v1/gates", post(register_gate::<S>))
        .route("/v1/gates/{id}/disable", post(disable_gate::<S>))
        .route("/v1/enrolments", post(offer::<S>))
        .route("/v1/bundles", post(publish::<S>))
        .route("/v1/keys/{kid}/revoke", post(revoke_key::<S>))
        .route("/v1/mandates", post(issue::<S>).get(mandates::<S>))
        .route("/v1/mandates/{id}/revoke", post(revoke_mandate::<S>))
        .layer(axum::middleware::from_fn_with_state(
            admin.clone(),
            guard::<S>,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(admin)
}

/// What every request meets before its route: the right host, and for anything but a
/// read, this origin, a JSON body and the session's second value (point 5).
async fn guard<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    // A header sent twice is not judged by whichever comes first.
    let text = |name: header::HeaderName| {
        let mut values = headers.get_all(name).iter();
        match (values.next(), values.next()) {
            (Some(only), None) => only.to_str().ok(),
            _ => None,
        }
    };
    let mut answer = if text(header::HOST) != Some(a.host.as_str()) {
        StatusCode::FORBIDDEN.into_response()
    } else if request.method() == Method::GET {
        next.run(request).await
    } else if text(header::ORIGIN) != Some(a.config.origin.as_str()) {
        StatusCode::FORBIDDEN.into_response()
    } else if !text(header::CONTENT_TYPE).is_some_and(|t| t.starts_with("application/json")) {
        StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response()
    } else {
        let presented = text(header::HeaderName::from_static(CSRF_HEADER)).map(str::to_string);
        match a.signed_in(headers).await {
            Ok(session) if presented.is_some_and(|p| session.carries(&p)) => {
                next.run(request).await
            }
            Ok(_) => StatusCode::FORBIDDEN.into_response(),
            Err(refused) => refused.into_response(),
        }
    };
    answer
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    answer
}

#[derive(Deserialize)]
struct Start {
    /// Ask the tenant for a fresh sign-in: for the acts that need a recent one.
    #[serde(default)]
    recent: bool,
}

async fn start<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Query(asked): Query<Start>,
) -> Response {
    let now = (a.clock)();
    if !a.within_limit(&headers, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(meta) = a.verifier.discovery().await else {
        return a.failed("unavailable");
    };
    let (Ok(pre), Ok(state), Ok(nonce), Ok(verifier)) = (random(), random(), random(), random())
    else {
        return a.failed("random");
    };
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    let redirect = format!("{}/v1/signin/callback", a.config.origin);
    let mut params = vec![
        ("client_id", a.verifier.tenant().client_id.clone()),
        ("response_type", "code".into()),
        ("response_mode", "query".into()),
        ("scope", "openid".into()),
        ("redirect_uri", redirect),
        ("state", state.clone()),
        ("nonce", nonce.clone()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".into()),
    ];
    // Always bounded: a tenant sign-in older than a session may last opens none here.
    let max_age = if asked.recent {
        0
    } else {
        a.authority.session_limits().absolute_secs
    };
    params.push(("max_age", max_age.to_string()));
    let Ok(to) = reqwest::Url::parse_with_params(&meta.authorization_endpoint, &params) else {
        return a.failed("unavailable");
    };
    {
        let mut flows = a.flows.lock().unwrap_or_else(|p| p.into_inner());
        flows.retain(|_, f| now.saturating_sub(f.started) <= FLOW_SECS);
        if flows.len() >= MAX_FLOWS {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        flows.insert(
            pre_digest(&pre),
            Flow {
                state,
                nonce,
                verifier,
                started: now,
            },
        );
    }
    // Lax, not Strict: it must come back on the navigation from the tenant.
    let (pre_name, pre_path) = a.pre_cookie();
    let kept = a.set_cookie(
        pre_name,
        &pre,
        &format!("SameSite=Lax; Path={pre_path}; Max-Age={FLOW_SECS}"),
    );
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, HeaderValue::from_str(to.as_str()).ok()),
            (header::SET_COOKIE, Some(kept)),
        ]
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?)))
        .collect::<HeaderMap>(),
    )
        .into_response()
}

#[derive(Deserialize)]
struct Back {
    code: Option<String>,
    state: Option<String>,
}

async fn callback<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Query(back): Query<Back>,
) -> Response {
    let now = (a.clock)();
    if !a.within_limit(&headers, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    // The flow is used once, whatever comes of it.
    let (pre_name, pre_path) = a.pre_cookie();
    let flow = cookie(&headers, pre_name).and_then(|pre| {
        a.flows
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&pre_digest(&pre))
    });
    let Some(flow) = flow.filter(|f| now.saturating_sub(f.started) <= FLOW_SECS) else {
        return a.failed("no_flow");
    };
    let (Some(code), Some(state)) = (back.code, back.state) else {
        return a.failed("no_code");
    };
    if !same(&state, &flow.state) {
        return a.failed("state");
    }
    let Ok(meta) = a.verifier.discovery().await else {
        return a.failed("unavailable");
    };
    let form = [
        ("grant_type", "authorization_code"),
        ("code", &code),
        (
            "redirect_uri",
            &format!("{}/v1/signin/callback", a.config.origin),
        ),
        ("client_id", &a.verifier.tenant().client_id),
        ("client_secret", &a.config.client_secret),
        ("code_verifier", &flow.verifier),
    ];
    let token = async {
        let mut answer = a
            .http
            .post(&meta.token_endpoint)
            .form(&form)
            .send()
            .await
            .ok()?;
        if !answer.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = answer.chunk().await.ok()? {
            if bytes.len() + chunk.len() > MAX_TOKEN_ANSWER {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        body.get("id_token")?.as_str().map(str::to_string)
    }
    .await;
    let Some(token) = token else {
        return a.failed("code");
    };
    let Some(at) = chrono::DateTime::from_timestamp(now, 0) else {
        return a.failed("clock");
    };
    let who = match a.verifier.validate(&token, &flow.nonce, at).await {
        Ok(who) => who,
        Err(failure) => return a.failed(failure.reason()),
    };
    let account = Account {
        tid: who.tid,
        oid: who.oid,
    };
    if now
        >= who
            .auth_time
            .saturating_add(a.authority.session_limits().absolute_secs)
    {
        return a.failed("signed_in_too_long_ago");
    }
    let signed = match a.authority.sign_in(&account, who.auth_time, now).await {
        Ok(signed) => signed,
        Err(_) => return a.failed("not_linked"),
    };
    a.counters.signed_in.fetch_add(1, Ordering::Relaxed);
    let mut answer = Json(json!({"csrf": signed.csrf})).into_response();
    let headers = answer.headers_mut();
    headers.append(
        header::SET_COOKIE,
        a.set_cookie(a.session_cookie(), &signed.value, "SameSite=Strict; Path=/"),
    );
    headers.append(
        header::SET_COOKIE,
        a.set_cookie(
            pre_name,
            "",
            &format!("SameSite=Lax; Path={pre_path}; Max-Age=0"),
        ),
    );
    // This page's address holds the spent code.
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    answer
}

fn refusal(e: AuthorityError) -> Response {
    match e {
        AuthorityError::SignInAgain => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"sign_in_again": true})),
        )
            .into_response(),
        AuthorityError::NotAllowed | AuthorityError::Refused => {
            StatusCode::FORBIDDEN.into_response()
        }
        AuthorityError::Unknown(_) => StatusCode::NOT_FOUND.into_response(),
        AuthorityError::Exists(_) | AuthorityError::NotNext(_) => {
            StatusCode::CONFLICT.into_response()
        }
        AuthorityError::Core(_) => StatusCode::BAD_REQUEST.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn done<T: serde::Serialize>(result: Result<T, AuthorityError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => refusal(e),
    }
}

fn read<T: DeserializeOwned>(body: &Bytes) -> Result<T, StatusCode> {
    gov_core::strict::parse(body)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or(StatusCode::BAD_REQUEST)
}

fn name(text: &str) -> Result<Id, StatusCode> {
    Id::new(text).map_err(|_| StatusCode::BAD_REQUEST)
}

/// Run one call as the signed-in person: read the session, the body, then act.
macro_rules! call {
    ($a:ident, $headers:ident, |$session:ident, $now:ident| $act:expr) => {{
        let $session = match $a.signed_in(&$headers).await {
            Ok(session) => session,
            Err(refused) => return refused.into_response(),
        };
        let $now = ($a.clock)();
        done($act.await)
    }};
}

async fn me<S: Store + 'static>(State(a): State<Arc<Admin<S>>>, headers: HeaderMap) -> Response {
    match a.signed_in(&headers).await {
        Ok(s) => Json(json!({"person": s.person(), "role": s.role, "session": s.reference()}))
            .into_response(),
        Err(refused) => refused.into_response(),
    }
}

async fn sign_out<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
) -> Response {
    let Some(value) = cookie(&headers, a.session_cookie()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut answer = done(a.authority.sign_out(&value, (a.clock)()).await);
    answer.headers_mut().append(
        header::SET_COOKIE,
        a.set_cookie(a.session_cookie(), "", "SameSite=Strict; Path=/; Max-Age=0"),
    );
    answer
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewPerson {
    id: Id,
    role: Role,
}

async fn add_person<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let new: NewPerson = match read(&body) {
        Ok(new) => new,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a
        .authority
        .add_person(&s, &new.id, new.role, now))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    /// The account's object id in the tenant this listener signs people in from.
    oid: String,
}

async fn link<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Path(person): Path<String>,
    body: Bytes,
) -> Response {
    let (person, link) = match (name(&person), read::<Link>(&body)) {
        (Ok(person), Ok(link)) => (person, link),
        (Err(bad), _) | (_, Err(bad)) => return bad.into_response(),
    };
    let oid = link.oid.trim().to_ascii_lowercase();
    if !gov_signin::is_guid(&oid) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let account = Account {
        tid: a.verifier.tenant().tenant_id.clone(),
        oid,
    };
    call!(a, headers, |s, now| a
        .authority
        .link_person(&s, &person, &account, now))
}

async fn offboard<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Path(person): Path<String>,
) -> Response {
    let person = match name(&person) {
        Ok(person) => person,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a
        .authority
        .offboard_person(&s, &person, now))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAgent {
    id: Id,
    owner: Id,
    #[serde(default)]
    labels: BTreeSet<Id>,
}

async fn register_agent<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let new: NewAgent = match read(&body) {
        Ok(new) => new,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a.authority.register_agent(
        &s,
        &new.id,
        &new.owner,
        new.labels.clone(),
        now
    ))
}

async fn change_agent<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Path((agent, change)): Path<(String, String)>,
) -> Response {
    let agent = match name(&agent) {
        Ok(agent) => agent,
        Err(bad) => return bad.into_response(),
    };
    let session = match a.signed_in(&headers).await {
        Ok(session) => session,
        Err(refused) => return refused.into_response(),
    };
    let now = (a.clock)();
    done(match change.as_str() {
        "suspend" => a.authority.suspend_agent(&session, &agent, now).await,
        "resume" => a.authority.resume_agent(&session, &agent, now).await,
        "disable" => a.authority.disable_agent(&session, &agent, now).await,
        _ => return StatusCode::NOT_FOUND.into_response(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    id: Id,
}

async fn register_gate<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let new: Named = match read(&body) {
        Ok(new) => new,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a
        .authority
        .register_gate(&s, &new.id, now))
}

async fn disable_gate<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Path(gate): Path<String>,
) -> Response {
    let gate = match name(&gate) {
        Ok(gate) => gate,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a
        .authority
        .disable_gate(&s, &gate, now))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    kind: EnrolKind,
    name: Id,
}

async fn offer<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let asked: Offer = match read(&body) {
        Ok(asked) => asked,
        Err(bad) => return bad.into_response(),
    };
    let session = match a.signed_in(&headers).await {
        Ok(session) => session,
        Err(refused) => return refused.into_response(),
    };
    let offered = a
        .authority
        .offer_enrolment(&session, asked.kind, &asked.name, (a.clock)())
        .await;
    done(offered.map(|code| json!({"code": code})))
}

async fn publish<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let signed: Envelope = match read(&body) {
        Ok(signed) => signed,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a
        .authority
        .publish_bundle(&s, &signed, now))
}

async fn revoke_key<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Path(kid): Path<String>,
) -> Response {
    let kid = match name(&kid) {
        Ok(kid) => kid,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a.authority.revoke_key(&s, &kid, now))
}

async fn issue<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ask: MandateAsk = match read(&body) {
        Ok(ask) => ask,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a
        .authority
        .issue_mandate(&s, &ask, now))
}

async fn mandates<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
) -> Response {
    let session = match a.signed_in(&headers).await {
        Ok(session) => session,
        Err(refused) => return refused.into_response(),
    };
    done(a.authority.list_mandates(session.person()).await)
}

async fn revoke_mandate<S: Store + 'static>(
    State(a): State<Arc<Admin<S>>>,
    headers: HeaderMap,
    Path(mandate): Path<String>,
) -> Response {
    let mandate = match name(&mandate) {
        Ok(mandate) => mandate,
        Err(bad) => return bad.into_response(),
    };
    call!(a, headers, |s, now| a
        .authority
        .revoke_mandate(&s, &mandate, now))
}
