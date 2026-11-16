//! Bounded retry of upstream answers that are safe to repeat: `429` and `503`, and a failure to
//! connect (nothing was sent, so nothing was started). Nothing else is retried: a `400`-class
//! error other than `429` would fail the same way again, and a stream that has begun cannot be
//! taken back. Streamed requests are therefore retried only before the first byte reaches the
//! caller, which is the only time the upstream answers with such a status.
//!
//! A retry waits for the upstream's `Retry-After` (seconds or an HTTP date) when it sent one,
//! otherwise for an exponential backoff with jitter. It stops at the attempt limit, when the
//! wait would run past the total time budget, or at once when the upstream's error says the
//! quota will not come back soon (a daily quota, no credit left).

use std::time::Duration;

use axum::http::{header, HeaderMap};
use chrono::{DateTime, Utc};

/// Defaults: three attempts within 20 seconds.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(20);
pub const DEFAULT_BASE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts in all, the first included. `1` turns retrying off.
    pub max_attempts: u32,
    /// Longest the whole call may spend retrying, counted from the first attempt.
    pub budget: Duration,
    /// The first backoff; each later one doubles.
    pub base: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            budget: DEFAULT_BUDGET,
            base: DEFAULT_BASE,
        }
    }
}

impl RetryPolicy {
    /// `PMOS_GATEWAY_RETRY_MAX_ATTEMPTS` (1 to 10), `PMOS_GATEWAY_RETRY_BUDGET_SECS` (0 to 300)
    /// and `PMOS_GATEWAY_RETRY_BASE_MS` (0 to 60000).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut p = Self::default();
        let num = |name: &str| -> Result<Option<f64>, String> {
            match get(name).filter(|v| !v.trim().is_empty()) {
                None => Ok(None),
                Some(v) => v
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .map(Some)
                    .ok_or_else(|| format!("{name} must be a non-negative number, not `{v}`")),
            }
        };
        if let Some(n) = num("PMOS_GATEWAY_RETRY_MAX_ATTEMPTS")? {
            if !(1.0..=10.0).contains(&n) || n.fract() != 0.0 {
                return Err(
                    "PMOS_GATEWAY_RETRY_MAX_ATTEMPTS must be a whole number, 1 to 10".into(),
                );
            }
            p.max_attempts = n as u32;
        }
        if let Some(n) = num("PMOS_GATEWAY_RETRY_BUDGET_SECS")? {
            if n > 300.0 {
                return Err("PMOS_GATEWAY_RETRY_BUDGET_SECS must be at most 300".into());
            }
            p.budget = Duration::from_secs_f64(n);
        }
        if let Some(n) = num("PMOS_GATEWAY_RETRY_BASE_MS")? {
            if n > 60_000.0 {
                return Err("PMOS_GATEWAY_RETRY_BASE_MS must be at most 60000".into());
            }
            p.base = Duration::from_secs_f64(n / 1000.0);
        }
        Ok(p)
    }

    /// How long to wait before attempt `attempt + 1`, or `None` to give up and return what the
    /// upstream said. `status` is `0` for a failure to connect; `elapsed` is the time since the
    /// first attempt began.
    pub fn next_delay(
        &self,
        attempt: u32,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
        elapsed: Duration,
    ) -> Option<Duration> {
        if attempt >= self.max_attempts || !(status == 0 || is_retryable_status(status)) {
            return None;
        }
        if status != 0 && unrecoverable(body) {
            return None;
        }
        let delay = retry_after(headers, Utc::now()).unwrap_or_else(|| self.backoff(attempt));
        (elapsed + delay <= self.budget).then_some(delay)
    }

    /// Exponential backoff with jitter: half to all of `base * 2^(attempt-1)`.
    fn backoff(&self, attempt: u32) -> Duration {
        let full = self.base.saturating_mul(1 << (attempt - 1).min(16));
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let jitter = 0.5 + 0.5 * f64::from(nanos % 1000) / 1000.0;
        full.mul_f64(jitter)
    }
}

pub fn is_retryable_status(status: u16) -> bool {
    status == 429 || status == 503
}

/// The `Retry-After` header: a number of seconds, or an HTTP date.
pub fn retry_after(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
    let v = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = v.parse::<f64>() {
        return (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs));
    }
    let at = DateTime::parse_from_rfc2822(v).ok()?.with_timezone(&Utc);
    Some((at - now).to_std().unwrap_or(Duration::ZERO))
}

/// Whether the provider's error body says waiting will not help: no credit left, or a daily
/// quota (Gemini names `...PerDay...` quotas; OpenAI answers `insufficient_quota`). Only what
/// the body states; anything unclear is left to the retry budget.
pub fn unrecoverable(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    let lower = text.to_ascii_lowercase();
    [
        "insufficient_quota",
        "billing_not_active",
        "perday",
        "per_day",
        "per day",
        "daily",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn with_retry_after(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::RETRY_AFTER, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        let now = DateTime::parse_from_rfc2822("Sun, 06 Nov 1994 08:49:37 GMT")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            retry_after(&with_retry_after("3"), now),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            retry_after(&with_retry_after("Sun, 06 Nov 1994 08:49:42 GMT"), now),
            Some(Duration::from_secs(5))
        );
        // A date in the past means now.
        assert_eq!(
            retry_after(&with_retry_after("Sun, 06 Nov 1994 08:00:00 GMT"), now),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after(&with_retry_after("soon"), now), None);
        assert_eq!(retry_after(&HeaderMap::new(), now), None);
    }

    #[test]
    fn decisions_follow_status_attempts_and_budget() {
        let p = RetryPolicy::default();
        let none = HeaderMap::new();
        let s = Duration::ZERO;
        assert!(p.next_delay(1, 429, &none, b"{}", s).is_some());
        assert!(p.next_delay(1, 503, &none, b"{}", s).is_some());
        assert!(p.next_delay(1, 0, &none, b"", s).is_some());
        for st in [400, 401, 404, 422, 500, 502] {
            assert!(p.next_delay(1, st, &none, b"{}", s).is_none(), "{st}");
        }
        // The last attempt is final.
        assert!(p.next_delay(3, 429, &none, b"{}", s).is_none());
        // A wait that would pass the budget is not started.
        assert!(p
            .next_delay(1, 429, &with_retry_after("30"), b"{}", s)
            .is_none());
        assert!(p
            .next_delay(
                1,
                429,
                &with_retry_after("5"),
                b"{}",
                Duration::from_secs(16)
            )
            .is_none());
        assert_eq!(
            p.next_delay(
                1,
                429,
                &with_retry_after("5"),
                b"{}",
                Duration::from_secs(15)
            ),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn backoff_doubles_with_jitter() {
        let p = RetryPolicy::default();
        for _ in 0..20 {
            let a = p.backoff(1);
            let b = p.backoff(2);
            assert!(a >= Duration::from_millis(500) && a <= Duration::from_secs(1));
            assert!(b >= Duration::from_secs(1) && b <= Duration::from_secs(2));
        }
    }

    #[test]
    fn a_quota_that_will_not_recover_fails_fast() {
        assert!(unrecoverable(
            br#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#
        ));
        assert!(unrecoverable(
            br#"[{"error":{"status":"RESOURCE_EXHAUSTED","details":[{"violations":[{"quotaId":"GenerateRequestsPerDayPerProjectPerModel-FreeTier"}]}]}}]"#
        ));
        assert!(!unrecoverable(
            br#"{"error":{"code":"rate_limit_exceeded","message":"Rate limit reached"}}"#
        ));
        assert!(!unrecoverable(
            br#"[{"error":{"status":"RESOURCE_EXHAUSTED","details":[{"violations":[{"quotaId":"GenerateRequestsPerMinutePerProjectPerModel"}]}]}}]"#
        ));
    }

    #[test]
    fn env_settings_are_checked() {
        let get = |pairs: &'static [(&'static str, &'static str)]| {
            RetryPolicy::from_lookup(move |k| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert_eq!(get(&[]).unwrap(), RetryPolicy::default());
        let p = get(&[
            ("PMOS_GATEWAY_RETRY_MAX_ATTEMPTS", "5"),
            ("PMOS_GATEWAY_RETRY_BUDGET_SECS", "7.5"),
            ("PMOS_GATEWAY_RETRY_BASE_MS", "250"),
        ])
        .unwrap();
        assert_eq!(p.max_attempts, 5);
        assert_eq!(p.budget, Duration::from_millis(7500));
        assert_eq!(p.base, Duration::from_millis(250));
        assert!(get(&[("PMOS_GATEWAY_RETRY_MAX_ATTEMPTS", "0")]).is_err());
        assert!(get(&[("PMOS_GATEWAY_RETRY_MAX_ATTEMPTS", "x")]).is_err());
        assert!(get(&[("PMOS_GATEWAY_RETRY_BUDGET_SECS", "-1")]).is_err());
    }
}
