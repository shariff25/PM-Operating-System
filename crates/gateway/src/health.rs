//! Health: a circuit per model and per upstream (`docs/model-providers-spec.md` §4.6, slice
//! MP4), for routed roles only (a one-hop role keeps its behaviour from before routes).
//!
//! - **Closed** (normal) → **open** after `circuit.failures` failures in a row, or at once
//!   where the fallback rules say (`fallback::Hit`). An open circuit skips its hop for its
//!   cool-down: `cool-down-secs`, doubled on each reopening up to `max-cool-down-secs`, or the
//!   `Retry-After` that opened it.
//! - **Half-open** once the cool-down ends: the next real call may use the hop, one call at a
//!   time. Its success closes the circuit and resets the cool-down; its failure reopens it,
//!   doubled. A call that ends without an outcome (the caller hung up) gives the trial back.
//! - **No synthetic traffic:** nothing here sends a request. State is in memory; a restart
//!   closes every circuit. Each change of state is one warning line with the label(s) and the
//!   reason's kind, never a URL, a body or an upstream's key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::fallback::Hit;
use crate::route::CircuitSettings;

/// Which circuit: a model's (by label) or an upstream's (by its config-local key, which is
/// never logged; its models' labels are logged in its place).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Model(String),
    Upstream(String),
}

#[derive(Debug, Clone)]
struct Circuit {
    failures: u32,
    open_until: Option<Instant>,
    cool_down: Duration,
    /// Opened since the last success: the next opening doubles the cool-down.
    reopening: bool,
    /// Half-open, with its one trial call in flight.
    trial: bool,
}

/// A circuit's state as `GET /v1/pmos/routes` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Open { until: Instant },
    HalfOpen,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open { .. } => "open",
            Self::HalfOpen => "half-open",
        }
    }
}

pub struct Health {
    settings: CircuitSettings,
    circuits: Mutex<HashMap<Key, Circuit>>,
    /// What a circuit's log line names: a model's label, or an upstream's models' labels.
    names: HashMap<Key, String>,
}

impl Health {
    pub fn new(settings: CircuitSettings, names: HashMap<Key, String>) -> Arc<Self> {
        Arc::new(Self {
            settings,
            circuits: Mutex::default(),
            names,
        })
    }

    fn name(&self, key: &Key) -> &str {
        self.names.get(key).map_or("", String::as_str)
    }

    fn fresh(&self) -> Circuit {
        Circuit {
            failures: 0,
            open_until: None,
            cool_down: self.settings.cool_down(),
            reopening: false,
            trial: false,
        }
    }

    /// A circuit's state now.
    pub fn state(&self, key: &Key) -> State {
        let map = self.circuits.lock().expect("health lock");
        match map.get(key).and_then(|c| c.open_until) {
            None => State::Closed,
            Some(until) if until > Instant::now() => State::Open { until },
            Some(_) => State::HalfOpen,
        }
    }

    /// May a call use a hop guarded by `keys`? `Ok`: yes, with the half-open trials it took
    /// (give them back with [`Trial`]); `Err`: no, until about then (the latest of its open
    /// circuits' ends; a half-open circuit whose trial is in flight reads as a second away).
    pub fn admit(self: &Arc<Self>, keys: &[Key]) -> Result<Trial, Instant> {
        let now = Instant::now();
        let mut map = self.circuits.lock().expect("health lock");
        let mut blocked: Option<Instant> = None;
        for k in keys {
            if let Some(c) = map.get(k) {
                match c.open_until {
                    Some(until) if until > now => {
                        blocked = Some(blocked.map_or(until, |b| b.max(until)))
                    }
                    Some(_) if c.trial => {
                        let soon = now + Duration::from_secs(1);
                        blocked = Some(blocked.map_or(soon, |b| b.max(soon)));
                    }
                    _ => {}
                }
            }
        }
        if let Some(b) = blocked {
            return Err(b);
        }
        let mut taken = Vec::new();
        for k in keys {
            if let Some(c) = map.get_mut(k) {
                if c.open_until.is_some() {
                    c.trial = true;
                    taken.push(k.clone());
                }
            }
        }
        Ok(Trial {
            health: self.clone(),
            keys: taken,
        })
    }

    /// A hop answered: its circuits close.
    pub fn success(&self, keys: &[Key]) {
        let mut map = self.circuits.lock().expect("health lock");
        for k in keys {
            if let Some(c) = map.get_mut(k) {
                if c.open_until.is_some() {
                    tracing::warn!(model = %self.name(k), "circuit closed: the model answered again");
                }
                *c = self.fresh();
            }
        }
    }

    /// A failure on the upstream's side, counted against `key` as `hit` says.
    pub fn failure(&self, key: &Key, hit: Hit, reason: &'static str) {
        if hit == Hit::None {
            self.release(std::slice::from_ref(key));
            return;
        }
        let now = Instant::now();
        let fresh = self.fresh();
        let mut map = self.circuits.lock().expect("health lock");
        let c = map.entry(key.clone()).or_insert(fresh);
        let half_open = c.open_until.is_some_and(|u| u <= now);
        c.trial = false;
        c.failures = c.failures.saturating_add(1);
        let open_for = match hit {
            Hit::None => return,
            Hit::Count if half_open || c.failures >= self.settings.failures => {
                Some(self.next_cool_down(c))
            }
            Hit::Count => None,
            Hit::OpenNow => Some(self.next_cool_down(c)),
            Hit::OpenFor(d) => Some(d),
            Hit::OpenMax => Some(self.settings.max_cool_down()),
        };
        if let Some(d) = open_for {
            c.open_until = Some(now + d);
            c.reopening = true;
            tracing::warn!(
                model = %self.name(key), reason, cool_down_secs = d.as_secs(),
                "circuit open: this model is skipped until its cool-down ends"
            );
        }
    }

    fn next_cool_down(&self, c: &mut Circuit) -> Duration {
        if c.reopening {
            c.cool_down = (c.cool_down * 2).min(self.settings.max_cool_down());
        } else {
            c.cool_down = self.settings.cool_down();
        }
        c.cool_down
    }

    /// Gives back half-open trials taken by a call that ended without an outcome.
    pub fn release(&self, keys: &[Key]) {
        let mut map = self.circuits.lock().expect("health lock");
        for k in keys {
            if let Some(c) = map.get_mut(k) {
                c.trial = false;
            }
        }
    }
}

/// The half-open trials one call took; given back when dropped without an outcome.
pub struct Trial {
    health: Arc<Health>,
    keys: Vec<Key>,
}

impl Drop for Trial {
    fn drop(&mut self) {
        if !self.keys.is_empty() {
            self.health.release(&self.keys);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health() -> Arc<Health> {
        Health::new(
            CircuitSettings {
                failures: 3,
                cool_down_secs: 1,
                max_cool_down_secs: 4,
            },
            HashMap::new(),
        )
    }

    #[test]
    fn opens_after_failures_in_a_row_half_opens_once_and_doubles_on_reopening() {
        let h = health();
        let k = Key::Model("m".into());
        for _ in 0..2 {
            h.failure(&k, Hit::Count, "server_error");
            assert_eq!(h.state(&k), State::Closed);
        }
        h.failure(&k, Hit::Count, "server_error");
        assert!(matches!(h.state(&k), State::Open { .. }));
        assert!(h.admit(std::slice::from_ref(&k)).is_err());
        std::thread::sleep(Duration::from_millis(1050));
        assert_eq!(h.state(&k), State::HalfOpen);
        // One trial at a time.
        let trial = h.admit(std::slice::from_ref(&k)).unwrap();
        assert!(h.admit(std::slice::from_ref(&k)).is_err());
        // A trial given back unanswered lets the next call try.
        drop(trial);
        let _trial = h.admit(std::slice::from_ref(&k)).unwrap();
        // It fails: reopened, for twice the cool-down.
        h.failure(&k, Hit::Count, "server_error");
        let State::Open { until } = h.state(&k) else {
            panic!("reopened")
        };
        assert!(until > Instant::now() + Duration::from_millis(1500));
        // Success closes it and resets.
        h.success(std::slice::from_ref(&k));
        assert_eq!(h.state(&k), State::Closed);
    }

    #[test]
    fn open_at_once_for_retry_after_or_the_longest_cool_down_and_short_waits_dont_count() {
        let h = health();
        let k = Key::Upstream("u".into());
        h.failure(&k, Hit::None, "rate_limit");
        h.failure(&k, Hit::None, "rate_limit");
        h.failure(&k, Hit::None, "rate_limit");
        assert_eq!(h.state(&k), State::Closed);
        h.failure(&k, Hit::OpenMax, "auth");
        let State::Open { until } = h.state(&k) else {
            panic!()
        };
        assert!(until > Instant::now() + Duration::from_secs(3));
        let k2 = Key::Model("m2".into());
        h.failure(&k2, Hit::OpenFor(Duration::from_secs(2)), "rate_limit");
        let State::Open { until } = h.state(&k2) else {
            panic!()
        };
        assert!(until > Instant::now() + Duration::from_millis(1500));
        // A hop is blocked while either of its circuits is.
        assert!(h.admit(&[Key::Model("free".into()), k.clone()]).is_err());
        assert!(h.admit(&[Key::Model("free".into())]).is_ok());
    }
}
