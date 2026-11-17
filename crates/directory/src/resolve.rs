//! The tool server's `resolve` and first-link routes, for `pmos-web` (`PMOS_WEB_TOKEN`) and
//! the console (`PMOS_CONSOLE_TS_TOKEN`): `docs/user-database-spec.md` §2 and §7,
//! `docs/toolserver.md` "The user database's read API".
//!
//! - **One user per call:** [`ResolveClient::resolve`] looks a person up by Entra key
//!   (`tid`, `oid`), by sign-in name, or by handle. The answer carries no address, no `oid`.
//! - **Cache:** an answer is fresh for [`FRESH_FOR`] (10 seconds) per lookup, so a busy page
//!   costs one call per person every 10 seconds, and a deactivation or a removed role is seen
//!   within 10 seconds while the tool server answers. A miss (`404`) is never cached.
//! - **Last-known-good:** when the tool server can't be reached (or fails), the error carries
//!   the last answer for that lookup and its age ([`LastKnown`]), and the caller decides what
//!   that may still allow. `pmos-web` serves its own non-Slack reads on one at most 5 minutes
//!   old (§2, "When the store is unavailable"); a sign-in never uses one. The client itself
//!   never treats a stale answer as fresh.
//! - **Testable:** the clock ([`Clock`]) and the transport ([`Transport`]) are injected;
//!   [`HttpTransport`] is the real one.
//!
//! Never logged: a token, an address, a sign-in name or an `oid`. Logs name the lookup's kind
//! (`entra`, `sign_in_name`, `handle`) and, for a handle, nothing more than the log line's
//! fixed text; `Debug` of a [`Lookup`] shows its kind only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

/// How long an answer is used without asking again (spec §2: web and console ≤ 10 s).
pub const FRESH_FOR: Duration = Duration::seconds(10);

/// How long an answer is kept at all, for last-known-good. Longer than any caller's allowance
/// (pmos-web's is 5 minutes); past it the entry is dropped.
pub const RETAIN: Duration = Duration::minutes(15);

/// The most answers kept; past it the oldest go first.
pub const MAX_ENTRIES: usize = 10_000;

/// How long one call to the tool server may take.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The largest answer read.
const MAX_REPLY: usize = 64 * 1024;

/// The time now. Injected, so tests can move it.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// What to look a person up by. `Debug` shows the kind only, never the value.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Lookup {
    /// The Entra key, `tid` and `oid` (GUIDs; compared lower-case).
    Entra {
        tid: String,
        oid: String,
    },
    /// The Entra sign-in name (the ID token's `preferred_username`), compared lower-case.
    SignInName(String),
    Handle(String),
}

impl Lookup {
    pub fn entra(tid: &str, oid: &str) -> Self {
        Self::Entra {
            tid: tid.trim().to_ascii_lowercase(),
            oid: oid.trim().to_ascii_lowercase(),
        }
    }

    pub fn sign_in_name(name: &str) -> Self {
        Self::SignInName(name.trim().to_lowercase())
    }

    pub fn handle(handle: &str) -> Self {
        Self::Handle(handle.trim().to_string())
    }

    /// `entra`, `sign_in_name` or `handle`: what logs may say.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Entra { .. } => "entra",
            Self::SignInName(_) => "sign_in_name",
            Self::Handle(_) => "handle",
        }
    }

    fn query(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Entra { tid, oid } => vec![
                ("by", "entra".into()),
                ("tid", tid.clone()),
                ("oid", oid.clone()),
            ],
            Self::SignInName(n) => vec![("by", "sign_in_name".into()), ("name", n.clone())],
            Self::Handle(h) => vec![("by", "handle".into()), ("handle", h.clone())],
        }
    }
}

impl std::fmt::Debug for Lookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lookup({})", self.kind())
    }
}

/// A user's status. Anything but `active` grants nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Active,
    Deactivated,
    /// A status a newer tool server sends: treated as not active.
    Other(String),
}

/// Where the user's Entra link stands (spec §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntraState {
    /// No Entra identity yet.
    None,
    /// Linked, and usable for this user's roles.
    Linked,
    /// An Admin or Auditor whose link the host hasn't confirmed: refused until it does.
    NeedsHost,
    /// A value a newer tool server sends: treated as not usable.
    Other(String),
}

/// One user as `resolve` answers them: no address, no `oid`. The display name is untrusted
/// text (render it as plain text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryUser {
    pub handle: String,
    pub display_name: String,
    pub status: Status,
    /// The stored role names (`admin`, `pm`, `reviewer`, `auditor`). A deactivated user's
    /// kept roles grant nothing: check [`Self::is_active`] first.
    pub roles: Vec<String>,
    pub entra: EntraState,
    pub sign_in_name_confirmed: bool,
    pub dual_role: bool,
}

impl DirectoryUser {
    pub fn is_active(&self) -> bool {
        self.status == Status::Active
    }

    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    /// Holds `admin` or `auditor`: their Entra link must be set on the host (spec §5, §7).
    pub fn needs_host_link(&self) -> bool {
        self.has_role("admin") || self.has_role("auditor")
    }
}

#[derive(Deserialize)]
struct UserWire {
    handle: String,
    #[serde(default)]
    display_name: String,
    status: String,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    entra: Option<String>,
    #[serde(default)]
    sign_in_name_confirmed: bool,
    #[serde(default)]
    dual_role: bool,
}

impl From<UserWire> for DirectoryUser {
    fn from(w: UserWire) -> Self {
        Self {
            handle: w.handle,
            display_name: w.display_name,
            status: match w.status.as_str() {
                "active" => Status::Active,
                "deactivated" => Status::Deactivated,
                _ => Status::Other(w.status),
            },
            roles: w.roles,
            entra: match w.entra.as_deref() {
                Some("none") | None => EntraState::None,
                Some("linked") => EntraState::Linked,
                Some("needs_host") => EntraState::NeedsHost,
                Some(other) => EntraState::Other(other.to_string()),
            },
            sign_in_name_confirmed: w.sign_in_name_confirmed,
            dual_role: w.dual_role,
        }
    }
}

/// The last answer for a lookup, kept for when the tool server can't be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastKnown {
    pub user: DirectoryUser,
    /// How long ago the tool server gave it.
    pub age: Duration,
}

/// Why a lookup has no fresh answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Nobody (`404`). Never cached.
    NotFound,
    /// The tool server called the lookup malformed (`422`): never a real person.
    Invalid,
    /// Over the tool server's miss limit (`429 resolve_misses`).
    RateLimited,
    /// The tool server refused this client (`401`/`403`): a configuration error.
    Refused { status: u16 },
    /// The tool server couldn't be reached, failed, or answered something unreadable. The
    /// last answer for this lookup, if there is one, with its age.
    Unavailable {
        why: &'static str,
        last_known_good: Option<LastKnown>,
    },
}

/// Why a first link was refused (the tool server's codes, `docs/toolserver.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRefusal {
    /// `404 not_found`: no such user with that sign-in name.
    NotFound,
    /// `403 not_active`.
    NotActive,
    /// `403 sign_in_unconfirmed`: an admin hasn't confirmed the sign-in name (§4).
    SignInUnconfirmed,
    /// `403 host_only`: an Admin or Auditor, linked on the host only.
    HostOnly,
    /// `403 evals_account`.
    EvalsAccount,
    /// `409 linked`: the user already has a different Entra identity (a conflict, audited
    /// and flagged by the tool server).
    Linked,
    /// `409 taken`: another user holds this Entra key (a conflict, likewise).
    Taken,
    /// `422 invalid`.
    Invalid,
    /// Another refusal a newer tool server sends.
    Other { status: u16, code: Option<String> },
}

impl LinkRefusal {
    /// A conflict (§7 step 3), which the tool server audited as `users.identity_conflict`.
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Linked | Self::Taken)
    }
}

/// A first link that didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    Refused(LinkRefusal),
    RateLimited,
    /// Couldn't be reached, failed, or answered something unreadable: nothing is known.
    Unavailable(&'static str),
}

/// A first link that did: the user, and whether this call stored the key (`false` when the
/// user already had exactly this key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    pub first_link: bool,
    pub user: DirectoryUser,
}

// The transport.

/// `GET` or `POST`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// One call to the tool server. No `Debug`: the query can hold an `oid` or a sign-in name.
#[derive(Clone)]
pub struct Request {
    pub method: Method,
    /// Path segments under the base, each encoded on its own (`["internal", "users", …]`).
    pub path: Vec<String>,
    pub query: Vec<(&'static str, String)>,
    pub body: Option<Value>,
}

/// The tool server's answer: status and body.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A call that got no answer: a fixed reason, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportError(pub &'static str);

/// How calls reach the tool server. [`HttpTransport`] in production; a fake in tests.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: Request) -> Result<Reply, TransportError>;
}

/// HTTP with a bearer token. `Debug` never shows the token.
pub struct HttpTransport {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport")
            .field("base", &self.base)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl HttpTransport {
    /// `base` is the tool server's URL (`PMOS_TOOLSERVER_URL`); `token` the caller's own
    /// tool server token.
    pub fn new(http: reqwest::Client, base: &str, token: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&self, request: Request) -> Result<Reply, TransportError> {
        let mut url =
            reqwest::Url::parse(&self.base).map_err(|_| TransportError("bad base URL"))?;
        url.path_segments_mut()
            .map_err(|_| TransportError("bad base URL"))?
            .pop_if_empty()
            .extend(&request.path);
        if !request.query.is_empty() {
            let mut q = url.query_pairs_mut();
            for (k, v) in &request.query {
                q.append_pair(k, v);
            }
        }
        let mut req = match request.method {
            Method::Get => self.http.get(url),
            Method::Post => self.http.post(url),
        };
        if let Some(body) = &request.body {
            req = req.json(body);
        }
        let res = req
            .bearer_auth(&self.token)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                TransportError(if e.is_timeout() {
                    "timed out"
                } else {
                    "didn't answer"
                })
            })?;
        let status = res.status().as_u16();
        let body = res
            .bytes()
            .await
            .map_err(|_| TransportError("dropped the connection"))?;
        if body.len() > MAX_REPLY {
            return Err(TransportError("answer too large"));
        }
        Ok(Reply {
            status,
            body: body.to_vec(),
        })
    }
}

// The client.

struct Entry {
    user: DirectoryUser,
    at: DateTime<Utc>,
}

/// The cached resolve client. Cheap to share behind an `Arc`.
pub struct ResolveClient {
    transport: Arc<dyn Transport>,
    clock: Clock,
    fresh_for: Duration,
    max_entries: usize,
    cache: Mutex<HashMap<Lookup, Entry>>,
}

impl std::fmt::Debug for ResolveClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveClient")
            .field("fresh_for", &self.fresh_for)
            .finish_non_exhaustive()
    }
}

fn code_of(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("code")?
        .as_str()
        .filter(|c| c.len() <= 64 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        .map(String::from)
}

impl ResolveClient {
    pub fn new(transport: Arc<dyn Transport>, clock: Clock) -> Self {
        Self {
            transport,
            clock,
            fresh_for: FRESH_FOR,
            max_entries: MAX_ENTRIES,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Over HTTP to `base` with `token`, on the system clock.
    pub fn http(http: reqwest::Client, base: &str, token: &str) -> Self {
        Self::new(
            Arc::new(HttpTransport::new(http, base, token)),
            Arc::new(Utc::now),
        )
    }

    /// The age past which an answer is fetched again (default [`FRESH_FOR`]; at most that).
    pub fn with_fresh_for(mut self, fresh_for: Duration) -> Self {
        self.fresh_for = fresh_for.min(FRESH_FOR).max(Duration::zero());
        self
    }

    /// The most answers kept (default [`MAX_ENTRIES`]; at least 1).
    pub fn with_max_entries(mut self, max: usize) -> Self {
        self.max_entries = max.max(1);
        self
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// The age of an entry at `now`, or `None` when the clock went back past it (its age is
    /// then unknown, so it is neither fresh nor last-known-good).
    fn age(at: DateTime<Utc>, now: DateTime<Utc>) -> Option<Duration> {
        let age = now - at;
        (age >= Duration::zero()).then_some(age)
    }

    /// The person `lookup` names: from the cache while fresh, else from the tool server.
    pub async fn resolve(&self, lookup: &Lookup) -> Result<DirectoryUser, ResolveError> {
        let now = self.now();
        if let Some(user) = self.fresh(lookup, now) {
            return Ok(user);
        }
        let request = Request {
            method: Method::Get,
            path: vec!["internal".into(), "users".into(), "resolve".into()],
            query: lookup.query(),
            body: None,
        };
        let unavailable = |why: &'static str| {
            tracing::warn!(kind = lookup.kind(), why, "user directory unavailable");
            ResolveError::Unavailable {
                why,
                last_known_good: self.last_known(lookup, now),
            }
        };
        let reply = match self.transport.send(request).await {
            Ok(r) => r,
            Err(TransportError(why)) => return Err(unavailable(why)),
        };
        match reply.status {
            200 => match serde_json::from_slice::<UserWire>(&reply.body) {
                Ok(w) => {
                    let user = DirectoryUser::from(w);
                    self.store(lookup, &user, now);
                    Ok(user)
                }
                Err(_) => Err(unavailable("unreadable answer")),
            },
            404 => {
                self.cache.lock().expect("directory cache").remove(lookup);
                Err(ResolveError::NotFound)
            }
            422 => Err(ResolveError::Invalid),
            429 => {
                tracing::warn!(kind = lookup.kind(), "user lookup over the miss limit");
                Err(ResolveError::RateLimited)
            }
            s @ (401 | 403) => {
                tracing::error!(
                    status = s,
                    "the tool server refused this client's user lookups: check its token"
                );
                Err(ResolveError::Refused { status: s })
            }
            _ => Err(unavailable("failed")),
        }
    }

    /// [`Self::resolve`] by handle.
    pub async fn by_handle(&self, handle: &str) -> Result<DirectoryUser, ResolveError> {
        self.resolve(&Lookup::handle(handle)).await
    }

    /// [`Self::resolve`] by Entra key.
    pub async fn by_entra(&self, tid: &str, oid: &str) -> Result<DirectoryUser, ResolveError> {
        self.resolve(&Lookup::entra(tid, oid)).await
    }

    /// [`Self::resolve`] by sign-in name.
    pub async fn by_sign_in_name(&self, name: &str) -> Result<DirectoryUser, ResolveError> {
        self.resolve(&Lookup::sign_in_name(name)).await
    }

    /// The first Entra link (`POST /internal/users/{handle}/identities/entra`, `pmos-web`'s
    /// token only): store `(tid, oid)` for `handle`, whose sign-in name must be
    /// `sign_in_name` (the ID token's `preferred_username`, never its `email`). The tool
    /// server re-checks every condition and audits the outcome. Never cached as a refusal;
    /// a link's user is cached under the handle and the key.
    pub async fn link_entra(
        &self,
        handle: &str,
        tid: &str,
        oid: &str,
        sign_in_name: &str,
    ) -> Result<Linked, LinkError> {
        let key = Lookup::entra(tid, oid);
        let Lookup::Entra { tid, oid } = &key else {
            unreachable!("Lookup::entra makes an Entra lookup")
        };
        let request = Request {
            method: Method::Post,
            path: vec![
                "internal".into(),
                "users".into(),
                handle.to_string(),
                "identities".into(),
                "entra".into(),
            ],
            query: Vec::new(),
            body: Some(json!({
                "tid": tid,
                "oid": oid,
                "sign_in_name": sign_in_name.trim().to_lowercase(),
            })),
        };
        let reply = match self.transport.send(request).await {
            Ok(r) => r,
            Err(TransportError(why)) => {
                tracing::warn!(why, "first Entra link: the tool server didn't answer");
                return Err(LinkError::Unavailable(why));
            }
        };
        #[derive(Deserialize)]
        struct LinkedWire {
            first_link: bool,
            user: UserWire,
        }
        let code = code_of(&reply.body);
        let refused = |r: LinkRefusal| Err(LinkError::Refused(r));
        match (reply.status, code.as_deref()) {
            (200, _) => match serde_json::from_slice::<LinkedWire>(&reply.body) {
                Ok(w) => {
                    let user = DirectoryUser::from(w.user);
                    let now = self.now();
                    self.store(&key, &user, now);
                    Ok(Linked {
                        first_link: w.first_link,
                        user,
                    })
                }
                Err(_) => Err(LinkError::Unavailable("unreadable answer")),
            },
            (404, _) => refused(LinkRefusal::NotFound),
            (403, Some("not_active")) => refused(LinkRefusal::NotActive),
            (403, Some("sign_in_unconfirmed")) => refused(LinkRefusal::SignInUnconfirmed),
            (403, Some("host_only")) => refused(LinkRefusal::HostOnly),
            (403, Some("evals_account")) => refused(LinkRefusal::EvalsAccount),
            (409, Some("linked")) => refused(LinkRefusal::Linked),
            (409, Some("taken")) => refused(LinkRefusal::Taken),
            (422, _) => refused(LinkRefusal::Invalid),
            (429, _) => Err(LinkError::RateLimited),
            (s, _) if (400..500).contains(&s) => refused(LinkRefusal::Other { status: s, code }),
            _ => Err(LinkError::Unavailable("failed")),
        }
    }

    /// The answer kept for `lookup`, however old (within [`RETAIN`]), without asking the tool
    /// server. **Never for a decision:** for informational uses only, such as the roles in a
    /// sign-out event's assertion.
    pub fn peek(&self, lookup: &Lookup) -> Option<DirectoryUser> {
        self.last_known(lookup, self.now()).map(|k| k.user)
    }

    /// Drop every cached answer about `handle` (after a change made through this service).
    pub fn forget(&self, handle: &str) {
        self.cache
            .lock()
            .expect("directory cache")
            .retain(|_, e| e.user.handle != handle);
    }

    fn fresh(&self, lookup: &Lookup, now: DateTime<Utc>) -> Option<DirectoryUser> {
        let cache = self.cache.lock().expect("directory cache");
        let e = cache.get(lookup)?;
        Self::age(e.at, now)
            .filter(|a| *a < self.fresh_for)
            .map(|_| e.user.clone())
    }

    fn last_known(&self, lookup: &Lookup, now: DateTime<Utc>) -> Option<LastKnown> {
        let cache = self.cache.lock().expect("directory cache");
        let e = cache.get(lookup)?;
        let age = Self::age(e.at, now).filter(|a| *a <= RETAIN)?;
        Some(LastKnown {
            user: e.user.clone(),
            age,
        })
    }

    /// Keep `user` as the answer for `lookup` (and for their handle), dropping what is past
    /// [`RETAIN`] and, over [`MAX_ENTRIES`], the oldest.
    fn store(&self, lookup: &Lookup, user: &DirectoryUser, now: DateTime<Utc>) {
        let mut cache = self.cache.lock().expect("directory cache");
        cache.retain(|_, e| Self::age(e.at, now).is_some_and(|a| a <= RETAIN));
        // An answer by another key says where this person stands now: a cached answer for
        // their handle is replaced, and an answer for another key that named them is dropped
        // (it may no longer be theirs).
        cache.retain(|k, e| e.user.handle != user.handle || k == lookup);
        let entry = |u: &DirectoryUser| Entry {
            user: u.clone(),
            at: now,
        };
        cache.insert(lookup.clone(), entry(user));
        cache.insert(Lookup::handle(&user.handle), entry(user));
        while cache.len() > self.max_entries {
            let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            cache.remove(&oldest);
        }
    }

    /// How many answers are kept (tests).
    pub fn cached(&self) -> usize {
        self.cache.lock().expect("directory cache").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// One request the fake saw: method, path, query and body.
    type Seen = (
        Method,
        Vec<String>,
        Vec<(&'static str, String)>,
        Option<Value>,
    );

    /// A fake tool server: answers queued in order, every request recorded.
    #[derive(Default)]
    struct Fake {
        answers: Mutex<VecDeque<Result<Reply, TransportError>>>,
        seen: Mutex<Vec<Seen>>,
    }

    impl Fake {
        fn push(&self, status: u16, body: Value) {
            self.answers.lock().unwrap().push_back(Ok(Reply {
                status,
                body: serde_json::to_vec(&body).unwrap(),
            }));
        }
        fn down(&self) {
            self.answers
                .lock()
                .unwrap()
                .push_back(Err(TransportError("didn't answer")));
        }
        fn calls(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl Transport for Fake {
        async fn send(&self, r: Request) -> Result<Reply, TransportError> {
            self.seen
                .lock()
                .unwrap()
                .push((r.method, r.path, r.query, r.body));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(TransportError("no answer queued")))
        }
    }

    struct T {
        fake: Arc<Fake>,
        clock: Arc<Mutex<DateTime<Utc>>>,
        client: ResolveClient,
    }

    fn setup() -> T {
        let fake = Arc::new(Fake::default());
        let clock = Arc::new(Mutex::new(
            DateTime::parse_from_rfc3339("2026-11-17T09:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ));
        let c = clock.clone();
        let client = ResolveClient::new(fake.clone(), Arc::new(move || *c.lock().unwrap()));
        T {
            fake,
            clock,
            client,
        }
    }

    impl T {
        fn advance(&self, by: Duration) {
            *self.clock.lock().unwrap() += by;
        }
    }

    fn ana(status: &str) -> Value {
        json!({"handle": "ana", "display_name": "Ana Silva", "status": status,
               "roles": ["pm"], "entra": "linked", "sign_in_name_confirmed": true,
               "dual_role": false})
    }

    #[tokio::test]
    async fn an_answer_is_used_for_ten_seconds_then_fetched_again() {
        let t = setup();
        t.fake.push(200, ana("active"));
        let u = t.client.by_handle("ana").await.unwrap();
        assert_eq!(u.display_name, "Ana Silva");
        assert!(u.is_active() && u.has_role("pm") && !u.needs_host_link());
        assert_eq!(u.entra, EntraState::Linked);
        t.advance(Duration::seconds(9));
        t.client.by_handle("ana").await.unwrap();
        assert_eq!(t.fake.calls(), 1, "fresh: from the cache");
        t.advance(Duration::seconds(1));
        t.fake.push(200, ana("deactivated"));
        let u = t.client.by_handle("ana").await.unwrap();
        assert_eq!(t.fake.calls(), 2, "10 seconds old: asked again");
        assert!(!u.is_active(), "a deactivation is seen within 10 seconds");
        let (method, path, query, _) = t.fake.seen.lock().unwrap()[0].clone();
        assert_eq!(method, Method::Get);
        assert_eq!(path, ["internal", "users", "resolve"]);
        assert_eq!(
            query,
            vec![("by", "handle".to_string()), ("handle", "ana".to_string())]
        );
    }

    #[tokio::test]
    async fn a_miss_is_never_cached_and_drops_the_old_answer() {
        let t = setup();
        t.fake.push(200, ana("active"));
        t.client.by_handle("ana").await.unwrap();
        t.advance(Duration::seconds(11));
        t.fake.push(404, json!({"code": "not_found"}));
        assert_eq!(t.client.by_handle("ana").await, Err(ResolveError::NotFound));
        t.fake.push(404, json!({"code": "not_found"}));
        assert_eq!(t.client.by_handle("ana").await, Err(ResolveError::NotFound));
        assert_eq!(t.fake.calls(), 3, "the second miss was asked again");
        // Gone: with the tool server down now there's nothing last-known.
        t.fake.down();
        assert_eq!(
            t.client.by_handle("ana").await,
            Err(ResolveError::Unavailable {
                why: "didn't answer",
                last_known_good: None
            })
        );
    }

    #[tokio::test]
    async fn unreachable_reports_the_last_answer_and_its_age() {
        let t = setup();
        t.fake.push(200, ana("active"));
        t.client.by_handle("ana").await.unwrap();
        t.advance(Duration::seconds(40));
        t.fake.down();
        match t.client.by_handle("ana").await {
            Err(ResolveError::Unavailable {
                last_known_good: Some(k),
                ..
            }) => {
                assert_eq!(k.age, Duration::seconds(40));
                assert_eq!(k.user.handle, "ana");
            }
            other => panic!("{other:?}"),
        }
        // A 5xx is the same as no answer.
        t.advance(Duration::minutes(4));
        t.fake.push(503, json!({"code": "unavailable"}));
        match t.client.by_handle("ana").await {
            Err(ResolveError::Unavailable {
                last_known_good: Some(k),
                ..
            }) => assert_eq!(k.age, Duration::seconds(280)),
            other => panic!("{other:?}"),
        }
        // Past what is kept at all: nothing.
        t.advance(RETAIN);
        t.fake.down();
        assert!(matches!(
            t.client.by_handle("ana").await,
            Err(ResolveError::Unavailable {
                last_known_good: None,
                ..
            })
        ));
        // An unreadable answer is unavailable too, never a user.
        t.fake.push(200, json!({"nope": true}));
        assert!(matches!(
            t.client.by_handle("ana").await,
            Err(ResolveError::Unavailable { .. })
        ));
    }

    #[tokio::test]
    async fn a_clock_that_went_back_makes_the_answer_neither_fresh_nor_known() {
        let t = setup();
        t.fake.push(200, ana("active"));
        t.client.by_handle("ana").await.unwrap();
        t.advance(Duration::seconds(-30));
        t.fake.down();
        assert!(matches!(
            t.client.by_handle("ana").await,
            Err(ResolveError::Unavailable {
                last_known_good: None,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn status_codes_map_to_their_errors() {
        let t = setup();
        t.fake.push(429, json!({"code": "resolve_misses"}));
        assert_eq!(
            t.client.by_sign_in_name("x@example.com").await,
            Err(ResolveError::RateLimited)
        );
        t.fake.push(422, json!({"code": "invalid"}));
        assert_eq!(
            t.client.by_entra("bad", "bad").await,
            Err(ResolveError::Invalid)
        );
        t.fake.push(403, json!({"code": "forbidden"}));
        assert_eq!(
            t.client.by_handle("ana").await,
            Err(ResolveError::Refused { status: 403 })
        );
    }

    #[tokio::test]
    async fn an_answer_by_entra_key_also_serves_the_handle() {
        let t = setup();
        t.fake.push(200, ana("active"));
        let tid = "11111111-2222-3333-4444-555555555555";
        let oid = "AAAAAAAA-0000-0000-0000-000000000001";
        t.client.by_entra(tid, oid).await.unwrap();
        // Lower-cased on the way out.
        let (_, _, q, _) = t.fake.seen.lock().unwrap()[0].clone();
        assert!(q.contains(&("oid", oid.to_lowercase())));
        t.client.by_handle("ana").await.unwrap();
        t.client.by_entra(tid, &oid.to_lowercase()).await.unwrap();
        assert_eq!(t.fake.calls(), 1);
        t.client.forget("ana");
        assert_eq!(t.client.cached(), 0);
    }

    #[tokio::test]
    async fn link_answers_and_refusals() {
        let t = setup();
        let tid = "11111111-2222-3333-4444-555555555555";
        let oid = "aaaaaaaa-0000-0000-0000-000000000001";
        t.fake
            .push(200, json!({"first_link": true, "user": ana("active")}));
        let l = t
            .client
            .link_entra("ana", tid, oid, " Ana@Example.com ")
            .await
            .unwrap();
        assert!(l.first_link);
        let (m, path, _, body) = t.fake.seen.lock().unwrap()[0].clone();
        assert_eq!(m, Method::Post);
        assert_eq!(path, ["internal", "users", "ana", "identities", "entra"]);
        assert_eq!(
            body.unwrap(),
            json!({"tid": tid, "oid": oid, "sign_in_name": "ana@example.com"})
        );
        // Cached by the key now.
        t.client.by_entra(tid, oid).await.unwrap();
        assert_eq!(t.fake.calls(), 1);
        for (status, code, want) in [
            (404, "not_found", LinkRefusal::NotFound),
            (403, "not_active", LinkRefusal::NotActive),
            (403, "sign_in_unconfirmed", LinkRefusal::SignInUnconfirmed),
            (403, "host_only", LinkRefusal::HostOnly),
            (403, "evals_account", LinkRefusal::EvalsAccount),
            (409, "linked", LinkRefusal::Linked),
            (409, "taken", LinkRefusal::Taken),
            (422, "invalid", LinkRefusal::Invalid),
        ] {
            t.fake.push(status, json!({"code": code}));
            assert_eq!(
                t.client.link_entra("ana", tid, oid, "a@b.c").await,
                Err(LinkError::Refused(want.clone())),
                "{code}"
            );
            assert_eq!(want.is_conflict(), matches!(code, "linked" | "taken"));
        }
        t.fake.push(429, json!({"code": "resolve_misses"}));
        assert_eq!(
            t.client.link_entra("ana", tid, oid, "a@b.c").await,
            Err(LinkError::RateLimited)
        );
        t.fake.down();
        assert!(matches!(
            t.client.link_entra("ana", tid, oid, "a@b.c").await,
            Err(LinkError::Unavailable(_))
        ));
    }

    #[test]
    fn debug_never_shows_a_lookup_value_or_the_token() {
        let l = Lookup::sign_in_name("secret.person@example.com");
        assert_eq!(format!("{l:?}"), "Lookup(sign_in_name)");
        let l = Lookup::entra("t", "the-oid");
        assert!(!format!("{l:?}").contains("the-oid"));
        let h = HttpTransport::new(reqwest::Client::new(), "http://x", "tok-123456789");
        assert!(!format!("{h:?}").contains("tok-123456789"));
    }

    #[tokio::test]
    async fn keeps_at_most_max_entries() {
        let mut t = setup();
        t.client = t.client.with_max_entries(50);
        for i in 0..55 {
            let mut u = ana("active");
            u["handle"] = json!(format!("u{i}"));
            t.fake.push(200, u);
            t.client.by_handle(&format!("u{i}")).await.unwrap();
            t.advance(Duration::milliseconds(1));
        }
        assert_eq!(t.client.cached(), 50);
        // The oldest went first.
        t.fake.push(200, ana("active"));
        t.client.by_handle("u54").await.unwrap();
        assert_eq!(t.fake.calls(), 55, "the newest is still cached");
    }
}
