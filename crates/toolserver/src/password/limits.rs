//! The limits on guessing (`docs/password-sign-in.md`, "Limits").
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
//! - **Per account, every address together** ([`over_ceiling`]): at most [`ACCOUNT_CEILING`]
//!   failures before the count is forgotten (24 hours with none). Past it, only an address
//!   the person has fully signed in from in the last [`KNOWN_SOURCE_DAYS`] days may still try
//!   (each with its own wait above): this bounds guessing from many addresses at once.
//! - **Hashing across the tool server** ([`GlobalLimit`]): at most [`GLOBAL_PER_MINUTE`]
//!   password hashes in any rolling minute for anyone, plus a reserve of
//!   [`GLOBAL_RESERVE_PER_MINUTE`] that only an attempt on a real account, from an address that
//!   isn't waiting and is below its threshold, may use. Junk (unknown names) fills the open
//!   allowance only, so it can't shut out a real sign-in. Routes that compute no hash (the
//!   second step, a setup's first step and confirmation) take nothing from it; the token or code
//!   they need is checked first.
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
/// Failures on one account from every address together before only the person's known
/// addresses may try (until 24 hours pass with no failure).
pub const ACCOUNT_CEILING: i32 = 50;
/// How long an address the person fully signed in from stays known.
pub const KNOWN_SOURCE_DAYS: i64 = 30;
/// Password hashes for anyone, in any rolling minute.
pub const GLOBAL_PER_MINUTE: usize = 300;
/// More hashes in the same minute, only for an attempt on a real account from an address that
/// isn't waiting and is below its threshold (or a valid setup token).
pub const GLOBAL_RESERVE_PER_MINUTE: usize = 120;
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

/// May `source` try a password or a code for this account now? `Err(until)` when that address
/// waits, or the account is over its ceiling and the address isn't one the person has signed in
/// from.
pub fn may_try(
    account: &Counter,
    source: &SourceCounters,
    now: DateTime<Utc>,
) -> Result<(), DateTime<Utc>> {
    if let Some(until) = source.sign_in.locked(now) {
        return Err(until);
    }
    match over_ceiling(account, now) {
        Some(until) if !source.known(now) => Err(until),
        _ => Ok(()),
    }
}

/// [`may_try`] for a setup code: no address is "known" before a setup, so the ceiling holds for
/// every address (an admin's new code clears these counts).
pub fn may_try_setup(
    account: &Counter,
    source: &SourceCounters,
    now: DateTime<Utc>,
) -> Result<(), DateTime<Utc>> {
    if let Some(until) = source.setup.locked(now) {
        return Err(until);
    }
    match over_ceiling(account, now) {
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
/// a reserve for attempts on real accounts that are below their own threshold.
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
    fn an_address_waits_alone_and_the_ceiling_spares_known_addresses() {
        let mut account = Counter::default();
        let mut theirs = SourceCounters::default();
        let mut attacker = SourceCounters::default();
        for i in 0..10 {
            attacker.sign_in.fail(at(i));
            account.count_only(at(i));
        }
        assert!(may_try(&account, &attacker, at(10)).is_err());
        assert!(
            may_try(&account, &theirs, at(10)).is_ok(),
            "another address"
        );
        for i in 10..ACCOUNT_CEILING as i64 {
            account.count_only(at(i));
        }
        assert_eq!(
            may_try(&account, &theirs, at(60)),
            Err(at(ACCOUNT_CEILING as i64 - 1) + FORGET_AFTER)
        );
        theirs.last_success_at = Some(at(0) - Duration::days(KNOWN_SOURCE_DAYS - 1));
        assert!(
            may_try(&account, &theirs, at(60)).is_ok(),
            "a known address"
        );
        assert!(may_try_setup(&account, &theirs, at(60)).is_err());
        assert!(
            may_try(&account, &SourceCounters::default(), at(60) + FORGET_AFTER).is_ok(),
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
    }
}
