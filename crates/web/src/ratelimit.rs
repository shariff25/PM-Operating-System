//! The per-client-IP limit on sign-in attempts. Account lockout stops guessing one account's
//! password; this slows one client guessing across many accounts (password spraying), and
//! caps the Argon2 work one client can make the server do.
//!
//! [`RateLimiter`] is the hook: the in-memory [`FixedWindowLimiter`] suits one replica; a
//! shared one (Postgres or the reverse proxy) can replace it for several.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

pub trait RateLimiter: Send + Sync {
    /// Count one attempt from `key` at `now`; false when it is over the limit.
    fn allow(&self, key: &str, now: DateTime<Utc>) -> bool;
}

/// Allows nothing to be limited. For tests that aren't about rate limits.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unlimited;

impl RateLimiter for Unlimited {
    fn allow(&self, _key: &str, _now: DateTime<Utc>) -> bool {
        true
    }
}

/// At most `limit` attempts per key in each `window`.
#[derive(Debug)]
pub struct FixedWindowLimiter {
    limit: u32,
    window: Duration,
    buckets: Mutex<HashMap<String, (DateTime<Utc>, u32)>>,
}

/// Above this many keys, expired windows are dropped before adding another.
const SWEEP_AT: usize = 10_000;

impl FixedWindowLimiter {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            buckets: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for FixedWindowLimiter {
    /// 20 sign-in attempts a minute per IP: plenty for a person, an office behind one NAT
    /// included, and slow for a script.
    fn default() -> Self {
        Self::new(20, Duration::minutes(1))
    }
}

impl RateLimiter for FixedWindowLimiter {
    fn allow(&self, key: &str, now: DateTime<Utc>) -> bool {
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() >= SWEEP_AT && !buckets.contains_key(key) {
            let window = self.window;
            buckets.retain(|_, (start, _)| now - *start < window);
        }
        let entry = buckets.entry(key.to_string()).or_insert((now, 0));
        if now - entry.0 >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_key_and_resets() {
        let l = FixedWindowLimiter::new(3, Duration::minutes(1));
        let t = Utc::now();
        assert!(l.allow("1.2.3.4", t));
        assert!(l.allow("1.2.3.4", t));
        assert!(l.allow("1.2.3.4", t));
        assert!(!l.allow("1.2.3.4", t));
        assert!(l.allow("5.6.7.8", t), "other clients are unaffected");
        assert!(l.allow("1.2.3.4", t + Duration::minutes(1)));
    }
}
