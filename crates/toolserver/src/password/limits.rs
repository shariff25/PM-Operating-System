//! The limits on guessing (`docs/password-sign-in.md`, "Limits").
//!
//! - **Per account** ([`Counter`]): failed passwords and failed second-step codes count
//!   together; setup-code guesses have a counter of their own with the same schedule. From the
//!   [`THRESHOLD`]th failure on, the account waits [`delay`]: 30 seconds, doubling with each
//!   further failure, at most [`MAX_DELAY`] (15 minutes). Never a permanent lock: the delay
//!   ends on its own, an attempt made during it is refused without being checked or counted,
//!   and failures older than [`FORGET_AFTER`] (24 hours) are forgotten. A full sign-in, a
//!   finished setup and a password change set the counter back to zero.
//! - **Across the tool server** ([`GlobalLimit`]): at most [`GLOBAL_PER_MINUTE`] attempts on the
//!   unauthenticated password routes in any rolling minute, from every caller together.
//! - The hashing itself is capped too (`super::PasswordService`, [`HASH_PERMITS`]).
//!
//! Every limit takes the time as an argument, so the tests drive them with a fake clock.

use std::collections::VecDeque;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

/// The failure from which an account waits.
pub const THRESHOLD: i32 = 5;
/// The first wait.
pub const BASE_DELAY_SECS: i64 = 30;
/// The longest wait.
pub const MAX_DELAY_SECS: i64 = 15 * 60;
/// [`MAX_DELAY_SECS`] as a duration.
pub const MAX_DELAY: Duration = Duration::seconds(MAX_DELAY_SECS);
/// Failures older than this are forgotten at the next one.
pub const FORGET_AFTER: Duration = Duration::hours(24);
/// Attempts on the unauthenticated password routes, from every caller together, in any
/// rolling minute.
pub const GLOBAL_PER_MINUTE: usize = 300;
/// Hashes computed at once; more wait (at most [`HASH_WAIT_SECS`]).
pub const HASH_PERMITS: usize = 2;
/// How long a request waits for a hashing slot before `503 sign_in_busy`.
pub const HASH_WAIT_SECS: u64 = 10;

/// How long an account waits after its `failures`th failure: nothing below [`THRESHOLD`],
/// then 30 s, 1 min, 2 min, 4 min, 8 min, and 15 min from the 10th on.
pub fn delay(failures: i32) -> Option<Duration> {
    if failures < THRESHOLD {
        return None;
    }
    let doublings = (failures - THRESHOLD).min(10) as u32;
    let secs = BASE_DELAY_SECS.saturating_mul(1i64 << doublings);
    Some(Duration::seconds(secs.min(MAX_DELAY_SECS)))
}

/// One account's failures of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counter {
    pub count: i32,
    pub last_at: Option<DateTime<Utc>>,
    pub locked_until: Option<DateTime<Utc>>,
}

impl Counter {
    /// Is the account waiting at `now`? Until when.
    pub fn locked(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.locked_until.filter(|until| *until > now)
    }

    /// Count one failure at `now`. True when this failure started (or extended) a wait.
    pub fn fail(&mut self, now: DateTime<Utc>) -> bool {
        let forgotten = self.last_at.is_some_and(|at| now - at > FORGET_AFTER);
        self.count = if forgotten {
            1
        } else {
            self.count.saturating_add(1)
        };
        self.last_at = Some(now);
        self.locked_until = delay(self.count).map(|d| now + d);
        self.locked_until.is_some()
    }

    /// Take back one failure counted ahead of a check that then passed (the password step
    /// counts a failure before it hashes, so parallel guesses can't slip past the threshold).
    /// The failure was counted while the account wasn't waiting, so the wait it started ends.
    pub fn take_back(&mut self) {
        self.count = (self.count - 1).max(0);
        self.locked_until = None;
    }

    /// Back to zero (a full sign-in; a finished setup; a password change).
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Seconds until `until`, at least 1: a `Retry-After`.
pub fn retry_after(until: DateTime<Utc>, now: DateTime<Utc>) -> u64 {
    u64::try_from((until - now).num_seconds())
        .unwrap_or(0)
        .max(1)
}

/// The rolling-minute limit across the tool server's unauthenticated password routes.
#[derive(Debug)]
pub struct GlobalLimit {
    per_minute: usize,
    seen: Mutex<VecDeque<DateTime<Utc>>>,
}

impl Default for GlobalLimit {
    fn default() -> Self {
        Self::new(GLOBAL_PER_MINUTE)
    }
}

impl GlobalLimit {
    pub fn new(per_minute: usize) -> Self {
        Self {
            per_minute: per_minute.max(1),
            seen: Mutex::new(VecDeque::new()),
        }
    }

    /// Take one attempt at `now`: `Err(retry_after_secs)` when the minute is full.
    pub fn take(&self, now: DateTime<Utc>) -> Result<(), u64> {
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let window = Duration::minutes(1);
        while seen.front().is_some_and(|t| now - *t >= window || *t > now) {
            seen.pop_front();
        }
        if seen.len() >= self.per_minute {
            let oldest = seen.front().copied().unwrap_or(now);
            return Err(retry_after(oldest + window, now));
        }
        seen.push_back(now);
        Ok(())
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
    }

    #[test]
    fn the_global_limit_is_a_rolling_minute() {
        let g = GlobalLimit::new(3);
        assert!(g.take(at(0)).is_ok());
        assert!(g.take(at(10)).is_ok());
        assert!(g.take(at(20)).is_ok());
        assert_eq!(g.take(at(30)), Err(30));
        assert!(g.take(at(60)).is_ok(), "the first left the window");
        assert!(g.take(at(61)).is_err());
        assert_eq!(GlobalLimit::default().per_minute, GLOBAL_PER_MINUTE);
    }
}
