//! The limits on guessing and on work (`docs/password-sign-in.md`, "Limits").
//!
//! - **Per account and address** ([`SourceCounters`]): failed passwords and failed
//!   second-step codes count together, setup-code guesses separately, for one person **from
//!   one address** (pmos-web's view of the browser: an IPv4 address or an IPv6 /64,
//!   [`source_key`]). From the [`THRESHOLD`]th failure on, that address waits [`delay`] for that
//!   person: 30 seconds, doubling with each further failure, at most [`MAX_DELAY`] (15 minutes).
//!   An attempt from that address during its wait is refused without being checked or
//!   counted; failures older than [`FORGET_AFTER`] (24 hours) are forgotten. **Another
//!   address is not held up**: the person signs in from theirs while someone else's address
//!   waits (the review of 2026-11-20, PM1).
//! - **Per account, every address together** ([`over_ceiling`]): past [`ACCOUNT_CEILING`]
//!   failures (until 24 hours pass with none), an address the person hasn't fully signed in
//!   from in the last [`KNOWN_SOURCE_DAYS`] days is **rationed**: after each failure it waits
//!   [`MAX_DELAY`] at once ([`fail_sign_in`]). It is never refused outright, so the owner,
//!   from a new address, with the right password and code, always gets in (the re-check of
//!   eb24d28, M2), and a correct password is always checked.
//! - **Hashing across the tool server** ([`GlobalLimit`]): at most [`GLOBAL_PER_MINUTE`]
//!   password hashes in any rolling minute for anyone, plus a reserve of
//!   [`GLOBAL_RESERVE_PER_MINUTE`] that only a password step **from an address someone fully
//!   signed in from** in the last [`KNOWN_SOURCE_DAYS`] days may use. Whether an attempt may use
//!   the reserve is decided from the address alone, before the sign-in name is looked up, so an
//!   unknown name and a real account get the same answer under load (M3). Setup and the
//!   signed-in routes use the open allowance only, never the reserve (H1, LA). Routes that
//!   compute no hash (the second step, a setup's first step and confirmation) take nothing.
//! - **Work per account** ([`AccountWork`]): at most [`ACCOUNT_WORK`] pieces of hashing work
//!   that follow an authentication of some kind (a correct password step, a setup's password,
//!   a password change, new recovery codes) per account in any [`ACCOUNT_WORK_MINUTES`]
//!   minutes, so one credential holder can't spend what everyone shares (H1, LA).
//! - **Work per setup token and per setup code** ([`SETUP_PASSWORD_SETS_PER_TOKEN`],
//!   [`SETUP_ENROLMENTS_PER_TOKEN`], [`SETUP_BEGINS_PER_CODE`],
//!   [`SETUP_PASSWORD_SETS_PER_CODE`]), and wrong guesses per code
//!   ([`SETUP_WRONG_GUESSES_PER_CODE`]), kept in the database with the token and the code.
//! - The hashing itself is capped too (`super::PasswordService`, [`HASH_PERMITS`]).
//!
//! Every limit takes the time as an argument, so the tests drive them with a fake clock.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

/// The failure from which an address waits for an account.
pub const THRESHOLD: i32 = 5;
/// The first wait.
pub const BASE_DELAY_SECS: i64 = 30;
/// The longest wait.
pub const MAX_DELAY_SECS: i64 = 15 * 60;
/// [`MAX_DELAY_SECS`] as a duration.
pub const MAX_DELAY: Duration = Duration::seconds(MAX_DELAY_SECS);
/// Failures older than this are forgotten at the next one.
pub const FORGET_AFTER: Duration = Duration::hours(24);
/// Failures on one account from every address together before an address the person hasn't
/// signed in from is rationed to one attempt per [`MAX_DELAY`] (until 24 hours pass with no
/// failure).
pub const ACCOUNT_CEILING: i32 = 50;
/// How long an address the person fully signed in from stays known.
pub const KNOWN_SOURCE_DAYS: i64 = 30;
/// Password hashes for anyone, in any rolling minute.
pub const GLOBAL_PER_MINUTE: usize = 300;
/// More hashes in the same minute, only for a password step from an address someone fully
/// signed in from in the last [`KNOWN_SOURCE_DAYS`] days.
pub const GLOBAL_RESERVE_PER_MINUTE: usize = 120;
/// Hashing work that follows an authentication, per account, in any [`ACCOUNT_WORK_MINUTES`].
pub const ACCOUNT_WORK: usize = 10;
/// The window [`ACCOUNT_WORK`] is counted in.
pub const ACCOUNT_WORK_MINUTES: i64 = 15;
/// Passwords set with one setup token; past it the token is spent (start again with the code).
pub const SETUP_PASSWORD_SETS_PER_TOKEN: i32 = 3;
/// New authenticator secrets made with one setup token; past it the token is spent.
pub const SETUP_ENROLMENTS_PER_TOKEN: i32 = 5;
/// Setup tokens one setup code gives (its renewals); past it the code is spent.
pub const SETUP_BEGINS_PER_CODE: i32 = 5;
/// Passwords set with one setup code across its renewals; past it the code is spent. Two
/// tokens' worth: someone who mistypes, abandons a setup and starts again with the same code
/// still finishes; more is not a person setting up, and an admin's new code is a minute's work.
pub const SETUP_PASSWORD_SETS_PER_CODE: i32 = 6;
/// Wrong setup codes tried against a person's live code before it is void. A 100-bit code
/// can't be guessed: this stops the work, it doesn't protect the secret.
pub const SETUP_WRONG_GUESSES_PER_CODE: i32 = 20;
/// Hashes computed at once; more wait (at most [`HASH_WAIT_SECS`]).
pub const HASH_PERMITS: usize = 2;
/// How long a request waits for a hashing slot before `503 sign_in_busy`.
pub const HASH_WAIT_SECS: u64 = 10;
/// What a signed-in person's own re-authentication (change the password, new recovery codes)
/// is counted under: one bucket per person, apart from every address ([`session_key`]).
pub const SESSION_SOURCE: &str = "session";

/// How long an address waits after its `failures`th failure: nothing below [`THRESHOLD`],
/// then 30 s, 1 min, 2 min, 4 min, 8 min, and 15 min from the 10th on.
pub fn delay(failures: i32) -> Option<Duration> {
    if failures < THRESHOLD {
        return None;
    }
    let doublings = (failures - THRESHOLD).min(10) as u32;
    let secs = BASE_DELAY_SECS.saturating_mul(1i64 << doublings);
    Some(Duration::seconds(secs.min(MAX_DELAY_SECS)))
}

/// Failures of one kind: for one account from one address (with a wait), or for one account
/// from every address (a count only, [`Counter::count_only`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counter {
    pub count: i32,
    pub last_at: Option<DateTime<Utc>>,
    pub locked_until: Option<DateTime<Utc>>,
}

impl Counter {
    /// Is the address waiting at `now`? Until when.
    pub fn locked(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.locked_until.filter(|until| *until > now)
    }

    fn bump(&mut self, now: DateTime<Utc>) {
        let forgotten = self.last_at.is_some_and(|at| now - at > FORGET_AFTER);
        self.count = if forgotten {
            1
        } else {
            self.count.saturating_add(1)
        };
        self.last_at = Some(now);
    }

    /// Count one failure at `now`. True when this failure started (or extended) a wait.
    pub fn fail(&mut self, now: DateTime<Utc>) -> bool {
        self.bump(now);
        self.locked_until = delay(self.count).map(|d| now + d);
        self.locked_until.is_some()
    }

    /// Count one failure at `now` with no wait (the account-wide count).
    pub fn count_only(&mut self, now: DateTime<Utc>) {
        self.bump(now);
        self.locked_until = None;
    }

    /// The count at `now`: nothing once 24 hours passed since the last failure.
    pub fn live_count(&self, now: DateTime<Utc>) -> i32 {
        match self.last_at {
            Some(at) if now - at <= FORGET_AFTER => self.count,
            _ => 0,
        }
    }

    /// Take back one failure counted ahead of a check that then passed (the password step
    /// counts a failure before it hashes, so parallel guesses can't slip past the threshold).
    /// The failure was counted while the address wasn't waiting, so the wait it started ends.
    pub fn take_back(&mut self) {
        self.count = (self.count - 1).max(0);
        self.locked_until = None;
    }

    /// Back to zero (a full sign-in from that address; a finished setup; a password change).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// One person's counters for one address ([`source_key`]), and when they last fully signed in
/// from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceCounters {
    /// Failed passwords and second-step codes (and re-authentication, for [`SESSION_SOURCE`]).
    pub sign_in: Counter,
    /// Failed setup-code guesses and setup confirmations.
    pub setup: Counter,
    /// The last full sign-in (or finished setup) from this address.
    pub last_success_at: Option<DateTime<Utc>>,
}

impl SourceCounters {
    /// Did the person fully sign in from here in the last [`KNOWN_SOURCE_DAYS`] days?
    pub fn known(&self, now: DateTime<Utc>) -> bool {
        self.last_success_at
            .is_some_and(|at| now - at <= Duration::days(KNOWN_SOURCE_DAYS))
    }

    /// Nothing worth keeping: no live failure, no wait, and not known.
    pub fn is_idle(&self, now: DateTime<Utc>) -> bool {
        self.sign_in.live_count(now) == 0
            && self.setup.live_count(now) == 0
            && self.sign_in.locked(now).is_none()
            && self.setup.locked(now).is_none()
            && !self.known(now)
    }
}

/// Has the account's count from every address reached [`ACCOUNT_CEILING`]? Until when (the
/// count is forgotten 24 hours after its last failure).
pub fn over_ceiling(account: &Counter, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    (account.live_count(now) >= ACCOUNT_CEILING)
        .then(|| account.last_at.map(|at| at + FORGET_AFTER))
        .flatten()
}

/// May `source` try a password or a code for this account now? `Err(until)` while that
/// address waits. Never refused for the account's ceiling alone (M2): past it, an address the
/// person hasn't signed in from is rationed by its own wait ([`fail_sign_in`]).
pub fn may_try(source: &SourceCounters, now: DateTime<Utc>) -> Result<(), DateTime<Utc>> {
    match source.sign_in.locked(now) {
        Some(until) => Err(until),
        None => Ok(()),
    }
}

/// What a counted sign-in failure did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed {
    /// This failure started (or extended) the address's wait.
    pub started_wait: bool,
    /// This failure took the account to its ceiling.
    pub reached_ceiling: bool,
}

/// Count one failed password or second-step code at `now`: on the account (a count only) and
/// on the address. Past the account's ceiling, an address the person hasn't signed in from
/// waits [`MAX_DELAY`] after each failure: one attempt per 15 minutes per address (M2).
pub fn fail_sign_in(
    account: &mut Counter,
    source: &mut SourceCounters,
    now: DateTime<Utc>,
) -> Failed {
    let before = account.live_count(now);
    account.count_only(now);
    let mut started_wait = source.sign_in.fail(now);
    if over_ceiling(account, now).is_some() && !source.known(now) {
        source.sign_in.locked_until = Some(now + MAX_DELAY);
        started_wait = true;
    }
    Failed {
        started_wait,
        reached_ceiling: before < ACCOUNT_CEILING && account.live_count(now) >= ACCOUNT_CEILING,
    }
}

/// [`may_try`] for a setup code: that address's own wait only. There is no account-wide
/// ceiling for setup codes (M2): guesses are counted against the live code itself, which is
/// void after [`SETUP_WRONG_GUESSES_PER_CODE`], and a new code starts from zero.
pub fn may_try_setup(source: &SourceCounters, now: DateTime<Utc>) -> Result<(), DateTime<Utc>> {
    match source.setup.locked(now) {
        Some(until) => Err(until),
        None => Ok(()),
    }
}

fn keyed(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"pmos-password-source\0");
    h.update(text.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The address an attempt comes from, as stored: the SHA-256 of pmos-web's address text,
/// normalized (an IPv4 address; an IPv6 address as its /64; an IPv4-mapped IPv6 address as
/// the IPv4 one). Anything else, or nothing, is the one key `unknown` shares. The address
/// itself is never stored or logged (a hash of an IPv4 address can be reversed by trying
/// them all: it is pseudonymous, not secret).
pub fn source_key(source: Option<&str>) -> String {
    let normal = source.and_then(normalize_source);
    keyed(normal.as_deref().unwrap_or("unknown"))
}

/// Is `source` an address (not the shared key `unknown`)? Only an address can become a
/// person's known address: attempts with no address all share one key, and that key is never
/// anyone's (M1).
pub fn is_address(source: Option<&str>) -> bool {
    source.and_then(normalize_source).is_some()
}

/// The key a signed-in person's own re-authentication is counted under.
pub fn session_key() -> String {
    keyed(SESSION_SOURCE)
}

/// An address as pmos-web counts it: IPv4 whole, IPv6 by its /64 (`2001:db8:1:2::/64`), an
/// IPv4-mapped IPv6 address as the IPv4 one.
pub fn normalize_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                let net = std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0);
                format!("{net}/64")
            }
        },
    }
}

fn normalize_source(text: &str) -> Option<String> {
    let t = text.trim();
    if let Ok(ip) = t.parse::<IpAddr>() {
        return Some(normalize_ip(ip));
    }
    let ip = t.strip_suffix("/64")?.parse::<std::net::Ipv6Addr>().ok()?;
    Some(normalize_ip(IpAddr::V6(ip)))
}

/// Seconds until `until`, at least 1: a `Retry-After`.
pub fn retry_after(until: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    u64::try_from((until - now).num_seconds())
        .unwrap_or(0)
        .max(1)
}

/// A rolling minute of timestamps.
#[derive(Debug, Default)]
struct Window(VecDeque<DateTime<Utc>>);

impl Window {
    fn trim(&mut self, now: DateTime<Utc>) {
        let window = Duration::minutes(1);
        while self
            .0
            .front()
            .is_some_and(|t| now - *t >= window || *t > now)
        {
            self.0.pop_front();
        }
    }

    fn retry(&self, now: DateTime<Utc>) -> u64 {
        let oldest = self.0.front().copied().unwrap_or(now);
        retry_after(oldest + Duration::minutes(1), now)
    }
}

/// The rolling-minute limit on password hashes across the tool server: an open allowance and
/// a reserve for password steps from known addresses.
#[derive(Debug)]
pub struct GlobalLimit {
    per_minute: usize,
    reserve: usize,
    seen: Mutex<(Window, Window)>,
}

impl Default for GlobalLimit {
    fn default() -> Self {
        Self::new(GLOBAL_PER_MINUTE, GLOBAL_RESERVE_PER_MINUTE)
    }
}

impl GlobalLimit {
    pub fn new(per_minute: usize, reserve: usize) -> Self {
        Self {
            per_minute: per_minute.max(1),
            reserve,
            seen: Mutex::new((Window::default(), Window::default())),
        }
    }

    /// Take one hash at `now`: from the open allowance, or, for an `eligible` attempt, from the
    /// reserve. `Err(retry_after_secs)` when neither has room.
    pub fn take(&self, now: DateTime<Utc>, eligible: bool) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let (open, reserve) = &mut *seen;
        open.trim(now);
        reserve.trim(now);
        if open.0.len() < self.per_minute {
            open.0.push_back(now);
            return Ok(());
        }
        if eligible && reserve.0.len() < self.reserve {
            reserve.0.push_back(now);
            return Ok(());
        }
        Err(if eligible && self.reserve > 0 {
            reserve.retry(now).min(open.retry(now))
        } else {
            open.retry(now)
        })
    }
}

/// Hashing work that follows an authentication, per account, in a rolling
/// [`ACCOUNT_WORK_MINUTES`] (in this process, as the global limit is): a correct password
/// step, a setup's password, a password change, new recovery codes. Past [`ACCOUNT_WORK`], more
/// of that work for the account is refused (`429`) until the window moves on. Only someone who
/// already holds one of the account's credentials (its password, a live setup token, a
/// session) can fill it, and filling it shuts out only that account.
#[derive(Debug, Default)]
pub struct AccountWork {
    seen: Mutex<std::collections::HashMap<String, VecDeque<DateTime<Utc>>>>,
}

/// Accounts [`AccountWork`] tracks before it forgets the idle ones.
const MAX_WORK_ACCOUNTS: usize = 10_000;

impl AccountWork {
    fn window() -> Duration {
        Duration::minutes(ACCOUNT_WORK_MINUTES)
    }

    fn trim(times: &mut VecDeque<DateTime<Utc>>, now: DateTime<Utc>) {
        while times
            .front()
            .is_some_and(|t| now - *t >= Self::window() || *t > now)
        {
            times.pop_front();
        }
    }

    fn full(times: &VecDeque<DateTime<Utc>>, now: DateTime<Utc>) -> Result<(), u64> {
        if times.len() >= ACCOUNT_WORK {
            let oldest = times.front().copied().unwrap_or(now);
            return Err(retry_after(oldest + Self::window(), now));
        }
        Ok(())
    }

    /// Has `handle` room for one more piece of work at `now`? `Err(retry_after_secs)` if not.
    pub fn check(&self, handle: &str, now: DateTime<Utc>) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        match seen.get_mut(handle) {
            Some(times) => {
                Self::trim(times, now);
                Self::full(times, now)
            }
            None => Ok(()),
        }
    }

    /// Check and count one piece of work for `handle` at `now`, in one step.
    pub fn take(&self, handle: &str, now: DateTime<Utc>) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        if seen.len() >= MAX_WORK_ACCOUNTS && !seen.contains_key(handle) {
            seen.retain(|_, t| t.back().is_some_and(|at| now - *at < Self::window()));
        }
        let times = seen.entry(handle.to_string()).or_default();
        Self::trim(times, now);
        Self::full(times, now)?;
        times.push_back(now);
        Ok(())
    }

    /// Count one piece of work that was done: a correct password step, checked with
    /// [`Self::check`] before its hash and counted once it passed (a wrong guess by someone
    /// else must never use the owner's room).
    pub fn count(&self, handle: &str, now: DateTime<Utc>) {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        seen.entry(handle.to_string()).or_default().push_back(now);
    }
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
    fn the_delay_grows_from_the_fifth_failure_and_is_capped() {
        let want = [
            (1, None),
            (4, None),
            (5, Some(30)),
            (6, Some(60)),
            (7, Some(120)),
            (8, Some(240)),
            (9, Some(480)),
            (10, Some(900)),
            (11, Some(900)),
            (1000, Some(900)),
            (i32::MAX, Some(900)),
        ];
        for (n, secs) in want {
            assert_eq!(delay(n), secs.map(Duration::seconds), "{n}");
        }
    }

    #[test]
    fn a_counter_waits_takes_back_and_forgets() {
        let mut c = Counter::default();
        for i in 0..4 {
            assert!(!c.fail(at(i)));
            assert_eq!(c.locked(at(i)), None);
        }
        assert!(c.fail(at(10)));
        assert_eq!(c.locked(at(10)), Some(at(40)));
        assert_eq!(c.locked(at(40)), None, "the wait ends on its own");
        // A correct password counted ahead takes its failure back, and the wait it started.
        c.take_back();
        assert_eq!((c.count, c.locked(at(11))), (4, None));
        // Old failures are forgotten.
        c.fail(at(10));
        c.fail(at(10) + FORGET_AFTER + Duration::seconds(1));
        assert_eq!(c.count, 1);
        c.clear();
        assert_eq!(c, Counter::default());
        assert_eq!(retry_after(at(40), at(10)), 30);
        assert_eq!(retry_after(at(10), at(40)), 1);
        // The account-wide count never waits.
        let mut a = Counter::default();
        for i in 0..20 {
            a.count_only(at(i));
        }
        assert_eq!((a.live_count(at(20)), a.locked(at(20))), (20, None));
        assert_eq!(
            a.live_count(at(19) + FORGET_AFTER + Duration::seconds(1)),
            0
        );
    }

    #[test]
    fn the_global_limit_is_a_rolling_minute_with_a_reserve() {
        let g = GlobalLimit::new(3, 2);
        assert!(g.take(at(0), false).is_ok());
        assert!(g.take(at(10), false).is_ok());
        assert!(g.take(at(20), false).is_ok());
        assert_eq!(g.take(at(30), false), Err(30));
        // Junk filled the open allowance: an attempt on a real account still gets a hash.
        assert!(g.take(at(30), true).is_ok());
        assert!(g.take(at(31), true).is_ok());
        assert!(g.take(at(32), true).is_err(), "the reserve is bounded too");
        assert!(g.take(at(60), false).is_ok(), "the first left the window");
        assert!(g.take(at(61), false).is_err());
        let d = GlobalLimit::default();
        assert_eq!(
            (d.per_minute, d.reserve),
            (GLOBAL_PER_MINUTE, GLOBAL_RESERVE_PER_MINUTE)
        );
    }

    #[test]
    fn an_address_waits_alone_and_past_the_ceiling_new_addresses_are_rationed() {
        let mut account = Counter::default();
        let mut theirs = SourceCounters::default();
        let mut attacker = SourceCounters::default();
        for i in 0..10 {
            fail_sign_in(&mut account, &mut attacker, at(i));
        }
        assert!(may_try(&attacker, at(10)).is_err());
        assert!(may_try(&theirs, at(10)).is_ok(), "another address");
        let mut reached = false;
        for i in 10..ACCOUNT_CEILING as i64 {
            let mut other = SourceCounters::default();
            reached |= fail_sign_in(&mut account, &mut other, at(i)).reached_ceiling;
        }
        assert!(reached, "the ceiling is reported once it is reached");
        // M2: past the ceiling a new address may still try: never refused for the ceiling.
        assert!(over_ceiling(&account, at(60)).is_some());
        assert!(may_try(&theirs, at(60)).is_ok(), "a new address may try");
        // But each failure from it rations it to one attempt per 15 minutes.
        let f = fail_sign_in(&mut account, &mut theirs, at(60));
        assert!(f.started_wait && !f.reached_ceiling);
        assert_eq!(may_try(&theirs, at(61)), Err(at(60) + MAX_DELAY));
        assert!(may_try(&theirs, at(60) + MAX_DELAY).is_ok());
        // A known address keeps its own schedule (no wait until its 5th failure).
        let mut known = SourceCounters {
            last_success_at: Some(at(0) - Duration::days(KNOWN_SOURCE_DAYS - 1)),
            ..Default::default()
        };
        assert!(!fail_sign_in(&mut account, &mut known, at(70)).started_wait);
        assert!(may_try(&known, at(71)).is_ok(), "a known address");
        assert!(may_try_setup(&theirs, at(60)).is_ok(), "no setup ceiling");
        let mut fresh = SourceCounters::default();
        assert!(
            !fail_sign_in(
                &mut account,
                &mut fresh,
                at(71) + FORGET_AFTER + Duration::seconds(1)
            )
            .started_wait,
            "the ceiling is forgotten after a quiet day"
        );
        let mut idle = SourceCounters::default();
        assert!(idle.is_idle(at(0)));
        idle.sign_in.fail(at(0));
        assert!(!idle.is_idle(at(1)));
        assert!(idle.is_idle(at(0) + FORGET_AFTER + Duration::seconds(1)));
    }

    #[test]
    fn sources_are_keyed_by_address_and_ipv6_by_its_64() {
        let k = |s: &str| source_key(Some(s));
        assert_eq!(k("203.0.113.7"), k(" 203.0.113.7 "));
        assert_ne!(k("203.0.113.7"), k("203.0.113.8"));
        assert_eq!(k("2001:db8:1:2::1"), k("2001:db8:1:2:ffff::9"));
        assert_eq!(k("2001:db8:1:2::1"), k("2001:db8:1:2::/64"));
        assert_ne!(k("2001:db8:1:2::1"), k("2001:db8:1:3::1"));
        assert_eq!(k("::ffff:203.0.113.7"), k("203.0.113.7"));
        assert_eq!(k("not an address"), source_key(None));
        assert_ne!(session_key(), source_key(None));
        assert_eq!(k("1.2.3.4").len(), 64);
        assert!(is_address(Some("203.0.113.7")) && is_address(Some("2001:db8:1:2::/64")));
        assert!(!is_address(None) && !is_address(Some("not an address")));
    }

    #[test]
    fn work_per_account_is_bounded_in_a_rolling_window() {
        let w = AccountWork::default();
        for i in 0..ACCOUNT_WORK as i64 {
            assert!(w.take("ana", at(i)).is_ok());
        }
        assert_eq!(w.take("ana", at(30)), Err(15 * 60 - 30));
        assert!(w.check("ana", at(30)).is_err());
        assert!(w.take("ben", at(30)).is_ok(), "another account has its own");
        assert!(
            w.take("ana", at(15 * 60)).is_ok(),
            "the first left the window"
        );
        assert!(w.check("cy", at(0)).is_ok());
        w.count("cy", at(0));
        assert!(w.check("cy", at(0)).is_ok());
    }
}
