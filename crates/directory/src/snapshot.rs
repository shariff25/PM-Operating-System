//! The user directory as the bot runtime and the Slack service read it: a snapshot of
//! `GET /internal/directory` on the tool server (`docs/user-database-spec.md` §2), polled every
//! [`POLL_EVERY`] with `If-None-Match`.
//!
//! **Age-aware reads.** The client keeps the last good snapshot and when it was last
//! confirmed (a `200` or a `304`). What a caller may decide from it depends on that age, and
//! each allowance is its own method, so no caller can ask for "stale is fine" in general:
//!
//! | Read | Snapshot at most | Spec §2 |
//! | --- | --- | --- |
//! | [`SnapshotClient::fresh`]: any decision that *allows* | [`ALLOW_AGE`] (30 s) | Caching and invalidation |
//! | [`SnapshotClient::for_queueing_ask`]: queue a bot ask (the task still needs a tool server session, which fails closed) | [`QUEUE_ASK_AGE`] (5 min) | When the store is unavailable, "Ask a bot" |
//! | [`SnapshotClient::for_due_routine`]: run or pause a routine that fell due; older means skip this run | [`DUE_ROUTINE_AGE`] (5 min) | "A routine falls due" |
//! | [`SnapshotClient::for_slack_identity`]: Slack id → handle (the Slack service) | [`SLACK_IDENTITY_AGE`] (5 min) | "Slack id → handle" |
//!
//! Past its bound a read is `None`, which the caller treats as "can't check", never as
//! "allowed". A failed poll keeps the old snapshot and its old fetch time, so an outage only
//! ever narrows what a caller may do.
//!
//! **Projection.** One [`DirectoryUser`] type serves both callers: the orchestrator token
//! (the runtime) gets `handle`, `display_name`, `status`, `roles`; the Slack bot token also
//! gets `slack_id`, which is `None` for the runtime. A field the tool server doesn't send is
//! never invented.
//!
//! **Tests** inject a [`Fetcher`] and a [`Clock`] ([`FakeClock`]); the service uses
//! [`HttpFetcher`] and [`SystemClock`]. Nothing here logs or formats a token.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pmos_contract::model::{valid_handle, Role};
use pmos_contract::permissions::{self, Permission};
use serde::Deserialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// How often the snapshot is polled.
pub const POLL_EVERY: Duration = Duration::from_secs(10);
/// The oldest snapshot used to allow anything (§2 "Caching and invalidation").
pub const ALLOW_AGE: Duration = Duration::from_secs(30);
/// The oldest snapshot an ask may be queued on (§2 "When the store is unavailable").
pub const QUEUE_ASK_AGE: Duration = Duration::from_secs(5 * 60);
/// The oldest snapshot a due routine is run or paused on; older, the run is skipped.
pub const DUE_ROUTINE_AGE: Duration = Duration::from_secs(5 * 60);
/// The oldest snapshot the Slack service maps a Slack id to a handle on.
pub const SLACK_IDENTITY_AGE: Duration = Duration::from_secs(5 * 60);
/// The longest one poll may take.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// A user's status in the directory. A status this build doesn't know grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    Active,
    Deactivated,
    #[serde(other)]
    Unknown,
}

/// One user as `GET /internal/directory` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DirectoryUser {
    pub handle: String,
    #[serde(default)]
    pub display_name: String,
    pub status: UserStatus,
    /// The stored role rows (`admin`, `pm`, `reviewer`, `auditor`). A deactivated user's
    /// kept roles grant nothing: use [`DirectoryUser::allows`], which checks the status.
    #[serde(default)]
    pub roles: Vec<String>,
    /// The Slack member id: the Slack bot token's projection only; `None` for the runtime.
    #[serde(default)]
    pub slack_id: Option<String>,
}

impl DirectoryUser {
    pub fn is_active(&self) -> bool {
        self.status == UserStatus::Active
    }

    /// The roles this build knows. An unknown name grants nothing.
    pub fn known_roles(&self) -> Vec<Role> {
        self.roles.iter().filter_map(|r| Role::parse(r)).collect()
    }

    /// Active, and one of the roles allows `permission` (`pmos_contract::permissions`).
    pub fn allows(&self, permission: Permission) -> bool {
        self.is_active() && permissions::allows(&self.known_roles(), permission)
    }

    /// Active and holding `admin`.
    pub fn is_admin(&self) -> bool {
        self.is_active() && self.roles.iter().any(|r| r == "admin")
    }
}

/// One directory as the tool server answered it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The tool server's `directory_version`.
    pub version: i64,
    /// The `ETag` to send back in `If-None-Match`.
    pub etag: String,
    pub users: BTreeMap<String, DirectoryUser>,
}

impl Snapshot {
    pub fn user(&self, handle: &str) -> Option<&DirectoryUser> {
        self.users.get(handle)
    }

    /// The user with this Slack id (the Slack bot's projection; the runtime's has none).
    pub fn by_slack_id(&self, slack_id: &str) -> Option<&DirectoryUser> {
        self.users
            .values()
            .find(|u| u.slack_id.as_deref() == Some(slack_id))
    }

    /// Is `handle` an active user whose roles allow `permission`?
    pub fn allows(&self, handle: &str, permission: Permission) -> bool {
        self.user(handle).is_some_and(|u| u.allows(permission))
    }

    /// Is `handle` an active admin?
    pub fn is_admin(&self, handle: &str) -> bool {
        self.user(handle).is_some_and(DirectoryUser::is_admin)
    }

    /// Parse a `200` body (`{version, users: [...]}`). Every handle must be one; a body that
    /// doesn't fit is refused whole, so the last good snapshot stays.
    pub fn from_body(body: &[u8], etag: Option<String>) -> Result<Self, FetchError> {
        #[derive(Deserialize)]
        struct Body {
            version: i64,
            users: Vec<DirectoryUser>,
        }
        let b: Body = serde_json::from_slice(body)
            .map_err(|e| FetchError::Invalid(format!("the directory doesn't parse: {e}")))?;
        let mut users = BTreeMap::new();
        for u in b.users {
            if !valid_handle(&u.handle) {
                return Err(FetchError::Invalid(
                    "the directory lists a handle that isn't one".into(),
                ));
            }
            users.insert(u.handle.clone(), u);
        }
        Ok(Self {
            version: b.version,
            etag: etag.unwrap_or_else(|| format!("\"{}\"", b.version)),
            users,
        })
    }
}

/// Why a poll got no answer to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Unreachable, timed out, or a `5xx`/`429`: worth trying again.
    Unavailable(String),
    /// `401`/`403`: the token isn't one the directory serves (a configuration error).
    Refused(u16),
    /// An answer that doesn't fit.
    Invalid(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => {
                write!(f, "the tool server's directory is unavailable: {why}")
            }
            Self::Refused(status) => write!(
                f,
                "the tool server refused the directory ({status}): check this service's token"
            ),
            Self::Invalid(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// What one fetch answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// `304`: the snapshot held is current.
    NotModified,
    /// `200`: a new snapshot.
    Changed(Snapshot),
}

/// Reads the directory. `etag` is the held snapshot's, for `If-None-Match`.
#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn fetch(&self, etag: Option<&str>) -> Result<Fetched, FetchError>;
}

/// `GET {base}/internal/directory` with a bearer token: the runtime's `PMOS_SERVICE_TOKEN`
/// or the Slack service's `PMOS_BOT_TOKEN`.
pub struct HttpFetcher {
    url: String,
    token: String,
    http: reqwest::Client,
}

/// The token never reaches a log through `Debug`.
impl std::fmt::Debug for HttpFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetcher")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl HttpFetcher {
    /// `base` is the tool server's URL, such as `http://127.0.0.1:8710`.
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            url: format!("{}/internal/directory", base.trim_end_matches('/')),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(FETCH_TIMEOUT)
                .build()
                .expect("http client builds"),
        }
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, etag: Option<&str>) -> Result<Fetched, FetchError> {
        let mut req = self.http.get(&self.url).bearer_auth(&self.token);
        if let Some(tag) = etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, tag);
        }
        // A transport error names the URL at most; the token is in a header it never shows.
        let res = req
            .send()
            .await
            .map_err(|e| FetchError::Unavailable(e.without_url().to_string()))?;
        let status = res.status().as_u16();
        match status {
            304 => Ok(Fetched::NotModified),
            200 => {
                let tag = res
                    .headers()
                    .get(reqwest::header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let body = res
                    .bytes()
                    .await
                    .map_err(|e| FetchError::Unavailable(e.without_url().to_string()))?;
                Snapshot::from_body(&body, tag).map(Fetched::Changed)
            }
            401 | 403 => Err(FetchError::Refused(status)),
            s if s == 429 || s >= 500 => Err(FetchError::Unavailable(format!("status {s}"))),
            s => Err(FetchError::Invalid(format!(
                "the directory answered status {s}"
            ))),
        }
    }
}

/// Where "now" comes from: a monotonic clock, so a wall-clock change can't make an old
/// snapshot look new.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The real monotonic clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock that moves only when told (tests).
#[derive(Debug)]
pub struct FakeClock {
    start: Instant,
    offset: Mutex<Duration>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }
}

impl FakeClock {
    pub fn advance(&self, by: Duration) {
        *self.offset.lock().unwrap_or_else(|p| p.into_inner()) += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.start + *self.offset.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// What one [`SnapshotClient::refresh`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// A new snapshot at this version.
    Changed(i64),
    /// `304`: the held snapshot is confirmed current.
    NotModified,
}

struct Held {
    snapshot: Arc<Snapshot>,
    /// When the request that last confirmed it was sent (a `200` or a `304`). Taken before
    /// the request, so the age never understates.
    confirmed_at: Instant,
}

/// The last good snapshot and its age. Share one per service (`Arc`).
pub struct SnapshotClient {
    fetcher: Arc<dyn Fetcher>,
    clock: Arc<dyn Clock>,
    held: RwLock<Option<Held>>,
    /// The version of each new snapshot (0 until the first), for callers that act on a change.
    changed: watch::Sender<i64>,
    /// Whether the last poll failed, so an outage is logged once, not every 10 seconds.
    failing: Mutex<bool>,
}

impl SnapshotClient {
    pub fn new(fetcher: Arc<dyn Fetcher>, clock: Arc<dyn Clock>) -> Self {
        Self {
            fetcher,
            clock,
            held: RwLock::new(None),
            changed: watch::channel(0).0,
            failing: Mutex::new(false),
        }
    }

    /// Over HTTP with `token`, on the real clock.
    pub fn http(base: &str, token: &str) -> Self {
        Self::new(
            Arc::new(HttpFetcher::new(base, token)),
            Arc::new(SystemClock),
        )
    }

    /// Poll once. A failure keeps the snapshot held, and its fetch time, as they were.
    pub async fn refresh(&self) -> Result<Refresh, FetchError> {
        let etag = self.read().map(|(s, _)| s.etag.clone());
        let sent_at = self.clock.now();
        let fetched = match tokio::time::timeout(
            FETCH_TIMEOUT + Duration::from_secs(1),
            self.fetcher.fetch(etag.as_deref()),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(FetchError::Unavailable("the poll timed out".into())),
        };
        let out = {
            let mut held = self.held.write().unwrap_or_else(|p| p.into_inner());
            match fetched {
                Ok(Fetched::Changed(snapshot)) => {
                    let version = snapshot.version;
                    // A slower, earlier poll never replaces a later one.
                    if held.as_ref().is_none_or(|h| h.confirmed_at <= sent_at) {
                        *held = Some(Held {
                            snapshot: Arc::new(snapshot),
                            confirmed_at: sent_at,
                        });
                    }
                    Ok(Refresh::Changed(version))
                }
                Ok(Fetched::NotModified) => match held.as_mut() {
                    // Only a snapshot we sent the ETag of can be confirmed.
                    Some(h) if Some(&h.snapshot.etag) == etag.as_ref() => {
                        h.confirmed_at = h.confirmed_at.max(sent_at);
                        Ok(Refresh::NotModified)
                    }
                    _ => Err(FetchError::Invalid(
                        "the directory answered 304 for a snapshot this service doesn't hold"
                            .into(),
                    )),
                },
                Err(e) => Err(e),
            }
        };
        let mut failing = self.failing.lock().unwrap_or_else(|p| p.into_inner());
        match &out {
            Ok(r) => {
                if *failing {
                    tracing::info!("the user directory answers again");
                }
                *failing = false;
                if let Refresh::Changed(version) = r {
                    tracing::info!(version, "user directory updated");
                    self.changed.send_replace(*version);
                }
            }
            Err(e) => {
                if !*failing {
                    tracing::warn!(error = %e, "couldn't refresh the user directory; keeping the last snapshot");
                } else {
                    tracing::debug!(error = %e, "the user directory is still unavailable");
                }
                *failing = true;
            }
        }
        out
    }

    /// Poll now and then every `every` until `stop`.
    pub fn spawn(
        self: &Arc<Self>,
        every: Duration,
        stop: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tick.tick() => {
                        let _ = me.refresh().await;
                    }
                }
            }
        })
    }

    /// The version of each new snapshot, as it arrives.
    pub fn changes(&self) -> watch::Receiver<i64> {
        self.changed.subscribe()
    }

    fn read(&self) -> Option<(Arc<Snapshot>, Duration)> {
        let held = self.held.read().unwrap_or_else(|p| p.into_inner());
        held.as_ref().map(|h| {
            (
                h.snapshot.clone(),
                self.clock.now().saturating_duration_since(h.confirmed_at),
            )
        })
    }

    fn within(&self, bound: Duration) -> Option<Arc<Snapshot>> {
        self.read().filter(|(_, age)| *age <= bound).map(|(s, _)| s)
    }

    /// How old the held snapshot is, if there is one (for logs and readiness only).
    pub fn age(&self) -> Option<Duration> {
        self.read().map(|(_, age)| age)
    }

    /// The snapshot, if at most [`ALLOW_AGE`] old: the only one any decision that allows
    /// may use.
    pub fn fresh(&self) -> Option<Arc<Snapshot>> {
        self.within(ALLOW_AGE)
    }

    /// The snapshot for queueing a bot ask, if at most [`QUEUE_ASK_AGE`] old. The queued
    /// task still needs a tool server session to run, which fails closed.
    pub fn for_queueing_ask(&self) -> Option<Arc<Snapshot>> {
        self.within(QUEUE_ASK_AGE)
    }

    /// The snapshot for a routine that fell due, if at most [`DUE_ROUTINE_AGE`] old.
    /// `None` means skip this run (don't pause the routine) and log it.
    pub fn for_due_routine(&self) -> Option<Arc<Snapshot>> {
        self.within(DUE_ROUTINE_AGE)
    }

    /// The snapshot for mapping a Slack id to a handle (the Slack service), if at most
    /// [`SLACK_IDENTITY_AGE`] old. What that person may then do is checked on
    /// [`SnapshotClient::fresh`] or the bound for that action.
    pub fn for_slack_identity(&self) -> Option<Arc<Snapshot>> {
        self.within(SLACK_IDENTITY_AGE)
    }
}

#[cfg(any(test, feature = "test-support"))]
/// A fetcher that answers from memory, as the tool server would (tests): a `304` when the
/// ETag matches, the users as a new snapshot otherwise, or an outage.
#[derive(Default)]
pub struct StaticFetcher {
    state: Mutex<StaticState>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct StaticState {
    version: i64,
    users: Vec<DirectoryUser>,
    down: bool,
    fetches: usize,
}

#[cfg(any(test, feature = "test-support"))]
impl StaticFetcher {
    pub fn new(users: Vec<DirectoryUser>) -> Self {
        let f = Self::default();
        f.set_users(users);
        f
    }

    /// Replace every user; the version moves.
    pub fn set_users(&self, users: Vec<DirectoryUser>) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.users = users;
        s.version += 1;
    }

    /// Change one user (or add them); the version moves.
    pub fn upsert(&self, user: DirectoryUser) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.users.retain(|u| u.handle != user.handle);
        s.users.push(user);
        s.version += 1;
    }

    /// Answer as unreachable, or again as normal.
    pub fn set_down(&self, down: bool) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).down = down;
    }

    /// How many fetches were made.
    pub fn fetches(&self) -> usize {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).fetches
    }
}

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl Fetcher for StaticFetcher {
    async fn fetch(&self, etag: Option<&str>) -> Result<Fetched, FetchError> {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.fetches += 1;
        if s.down {
            return Err(FetchError::Unavailable("down (test)".into()));
        }
        let tag = format!("\"{}\"", s.version);
        if etag == Some(tag.as_str()) {
            return Ok(Fetched::NotModified);
        }
        Ok(Fetched::Changed(Snapshot {
            version: s.version,
            etag: tag,
            users: s
                .users
                .iter()
                .map(|u| (u.handle.clone(), u.clone()))
                .collect(),
        }))
    }
}

#[cfg(any(test, feature = "test-support"))]
/// A user for tests: `handle`, active, with `roles`.
pub fn user(handle: &str, roles: &[&str]) -> DirectoryUser {
    DirectoryUser {
        handle: handle.into(),
        display_name: handle.into(),
        status: UserStatus::Active,
        roles: roles.iter().map(|r| r.to_string()).collect(),
        slack_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(f: &Arc<StaticFetcher>) -> (SnapshotClient, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock::default());
        (SnapshotClient::new(f.clone(), clock.clone()), clock)
    }

    fn deactivated(handle: &str, roles: &[&str]) -> DirectoryUser {
        DirectoryUser {
            status: UserStatus::Deactivated,
            ..user(handle, roles)
        }
    }

    #[tokio::test]
    async fn nothing_before_the_first_answer() {
        let f = Arc::new(StaticFetcher::new(vec![user("ana", &["pm"])]));
        f.set_down(true);
        let (c, _) = client(&f);
        assert!(c.refresh().await.is_err());
        assert!(c.fresh().is_none());
        assert!(c.for_queueing_ask().is_none());
        assert!(c.for_due_routine().is_none());
        assert!(c.for_slack_identity().is_none());
        assert_eq!(c.age(), None);
    }

    #[tokio::test]
    async fn each_read_has_its_own_bound_inclusive() {
        let f = Arc::new(StaticFetcher::new(vec![user("ana", &["pm"])]));
        let (c, clock) = client(&f);
        assert_eq!(c.refresh().await.unwrap(), Refresh::Changed(1));
        f.set_down(true);
        // At exactly 30 s the snapshot still allows; a millisecond later it doesn't.
        clock.advance(ALLOW_AGE);
        assert!(c.fresh().is_some());
        clock.advance(Duration::from_millis(1));
        assert!(c.fresh().is_none());
        assert!(c.for_queueing_ask().is_some());
        assert!(c.for_due_routine().is_some());
        assert!(c.for_slack_identity().is_some());
        // At exactly 5 minutes the longer allowances hold; past it none does.
        clock.advance(QUEUE_ASK_AGE - ALLOW_AGE - Duration::from_millis(1));
        assert_eq!(c.age(), Some(QUEUE_ASK_AGE));
        assert!(c.for_queueing_ask().is_some());
        assert!(c.for_due_routine().is_some());
        assert!(c.for_slack_identity().is_some());
        clock.advance(Duration::from_millis(1));
        assert!(c.for_queueing_ask().is_none());
        assert!(c.for_due_routine().is_none());
        assert!(c.for_slack_identity().is_none());
        assert!(c.fresh().is_none());
    }

    #[tokio::test]
    async fn a_304_keeps_the_snapshot_fresh() {
        let f = Arc::new(StaticFetcher::new(vec![user("ana", &["pm"])]));
        let (c, clock) = client(&f);
        c.refresh().await.unwrap();
        let first = c.fresh().unwrap();
        for _ in 0..10 {
            clock.advance(POLL_EVERY);
            assert_eq!(c.refresh().await.unwrap(), Refresh::NotModified);
        }
        // 100 seconds on, confirmed every 10: still fresh, and the very same snapshot.
        let now = c.fresh().expect("fresh after 304s");
        assert!(Arc::ptr_eq(&first, &now));
        assert_eq!(c.age(), Some(Duration::ZERO));
        // A change answers 200 with the new users.
        f.upsert(deactivated("ana", &["pm"]));
        clock.advance(POLL_EVERY);
        assert_eq!(c.refresh().await.unwrap(), Refresh::Changed(2));
        assert!(!c.fresh().unwrap().user("ana").unwrap().is_active());
    }

    #[tokio::test]
    async fn an_outage_keeps_the_last_snapshot_and_its_age() {
        let f = Arc::new(StaticFetcher::new(vec![user("ana", &["pm"])]));
        let (c, clock) = client(&f);
        c.refresh().await.unwrap();
        f.set_down(true);
        for _ in 0..4 {
            clock.advance(POLL_EVERY);
            assert!(c.refresh().await.is_err());
        }
        // A failed poll never makes the snapshot younger.
        assert_eq!(c.age(), Some(POLL_EVERY * 4));
        assert!(c.fresh().is_none(), "40 s old: no longer allows");
        assert!(c
            .for_queueing_ask()
            .unwrap()
            .allows("ana", Permission::AskBot));
        f.set_down(false);
        assert_eq!(c.refresh().await.unwrap(), Refresh::NotModified);
        assert!(c.fresh().is_some());
    }

    #[tokio::test]
    async fn changes_are_announced() {
        let f = Arc::new(StaticFetcher::new(vec![user("ana", &["pm"])]));
        let (c, _) = client(&f);
        let mut rx = c.changes();
        c.refresh().await.unwrap();
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), 1);
        c.refresh().await.unwrap();
        assert!(!rx.has_changed().unwrap(), "a 304 is no change");
    }

    #[test]
    fn rights_follow_status_and_roles() {
        let mut s = Snapshot::default();
        for u in [
            user("ana", &["pm"]),
            user("aud", &["auditor"]),
            user("adm", &["admin"]),
            user("rev", &["reviewer"]),
            deactivated("gone", &["pm", "admin"]),
            user("odd", &["policy-steward"]),
        ] {
            s.users.insert(u.handle.clone(), u);
        }
        assert!(s.allows("ana", Permission::AskBot));
        assert!(s.allows("rev", Permission::AskBot));
        assert!(s.allows("adm", Permission::AskBot));
        assert!(!s.allows("aud", Permission::AskBot), "an auditor can't ask");
        assert!(
            !s.allows("gone", Permission::AskBot),
            "kept roles grant nothing"
        );
        assert!(
            !s.allows("odd", Permission::AskBot),
            "an unknown role grants nothing"
        );
        assert!(!s.allows("nobody", Permission::AskBot));
        assert!(s.is_admin("adm"));
        assert!(!s.is_admin("gone"));
        assert!(!s.is_admin("ana"));
    }

    #[test]
    fn bodies_parse_for_both_projections() {
        let runtime = br#"{"version": 7, "users": [
            {"handle": "ana", "display_name": "Ana", "status": "active", "roles": ["pm"]},
            {"handle": "bo", "display_name": "Bo", "status": "deactivated", "roles": []}]}"#;
        let s = Snapshot::from_body(runtime, Some("\"7\"".into())).unwrap();
        assert_eq!(s.version, 7);
        assert_eq!(s.etag, "\"7\"");
        assert_eq!(s.user("ana").unwrap().slack_id, None);
        assert_eq!(s.user("bo").unwrap().status, UserStatus::Deactivated);
        let slack = br#"{"version": 8, "users": [
            {"handle": "ana", "display_name": "Ana", "status": "active", "roles": ["pm"], "slack_id": "U0ANA"},
            {"handle": "cy", "display_name": "Cy", "status": "suspended", "roles": ["pm"], "slack_id": null}]}"#;
        let s = Snapshot::from_body(slack, None).unwrap();
        assert_eq!(s.etag, "\"8\"");
        assert_eq!(s.by_slack_id("U0ANA").unwrap().handle, "ana");
        assert_eq!(s.user("cy").unwrap().status, UserStatus::Unknown);
        assert!(!s.allows("cy", Permission::AskBot));
        // A bad handle or a body that doesn't fit is refused whole.
        let bad = br#"{"version": 9, "users": [{"handle": "Not A Handle", "status": "active"}]}"#;
        assert!(Snapshot::from_body(bad, None).is_err());
        assert!(Snapshot::from_body(b"{}", None).is_err());
    }

    #[test]
    fn the_token_never_prints() {
        let f = HttpFetcher::new("http://127.0.0.1:1", "secret-token-0123456789abcdef");
        assert!(!format!("{f:?}").contains("secret-token"));
    }

    // ----- over HTTP --------------------------------------------------------------------

    mod http {
        use super::*;
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::IntoResponse;
        use axum::routing::get;

        const TOKEN: &str = "service-token-0123456789abcdef0123";

        #[derive(Default)]
        struct Ts {
            seen: Mutex<Vec<Option<String>>>,
            status: Mutex<Option<u16>>,
        }

        async fn serve(ts: Arc<Ts>) -> String {
            let app = axum::Router::new().route(
                "/internal/directory",
                get(move |headers: HeaderMap| {
                    let ts = ts.clone();
                    async move {
                        if headers.get("authorization").and_then(|v| v.to_str().ok())
                            != Some(&format!("Bearer {TOKEN}"))
                        {
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        if let Some(s) = *ts.status.lock().unwrap() {
                            return StatusCode::from_u16(s).unwrap().into_response();
                        }
                        let inm = headers
                            .get("if-none-match")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        ts.seen.lock().unwrap().push(inm.clone());
                        if inm.as_deref() == Some("\"3\"") {
                            return (StatusCode::NOT_MODIFIED, [("etag", "\"3\"")]).into_response();
                        }
                        (
                            [("etag", "\"3\"")],
                            axum::Json(serde_json::json!({"version": 3, "users": [
                                {"handle": "ana", "display_name": "Ana", "status": "active", "roles": ["pm", "admin"]}
                            ]})),
                        )
                            .into_response()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            base
        }

        #[tokio::test]
        async fn polls_with_if_none_match_and_reads_304() {
            let ts = Arc::new(Ts::default());
            let base = serve(ts.clone()).await;
            let c = SnapshotClient::http(&format!("{base}/"), TOKEN);
            assert_eq!(c.refresh().await.unwrap(), Refresh::Changed(3));
            assert_eq!(c.refresh().await.unwrap(), Refresh::NotModified);
            assert_eq!(
                *ts.seen.lock().unwrap(),
                vec![None, Some("\"3\"".to_string())]
            );
            assert!(c.fresh().unwrap().is_admin("ana"));
            // An error status keeps the snapshot; a wrong token is a refusal, not an outage.
            *ts.status.lock().unwrap() = Some(503);
            assert!(matches!(c.refresh().await, Err(FetchError::Unavailable(_))));
            assert!(c.fresh().is_some());
            let wrong = SnapshotClient::http(&base, "another-token-0123456789abcdef0123");
            *ts.status.lock().unwrap() = None;
            let err = wrong.refresh().await.unwrap_err();
            assert_eq!(err, FetchError::Refused(401));
            assert!(!err.to_string().contains("another-token"));
            // Nothing listening: unavailable, and the message carries no token.
            let gone = SnapshotClient::http("http://127.0.0.1:9", TOKEN);
            let err = gone.refresh().await.unwrap_err();
            assert!(matches!(err, FetchError::Unavailable(_)), "{err}");
            assert!(!err.to_string().contains(TOKEN));
        }
    }
}
