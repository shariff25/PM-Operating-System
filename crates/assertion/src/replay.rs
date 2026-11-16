//! Replay protection: each nonce is accepted once while its assertion could still verify.
//!
//! [`MemoryReplayCache`] lives in one process. With several replicas of a verifying service,
//! it stops a captured assertion being replayed against the same replica, but not against a
//! sibling: each replica keeps its own set. A restart also empties it. A request binding
//! narrows what such a replay can do (the same method, path and body, within the assertion's
//! at most 120 seconds plus the clock skew), and that is all. To close the gap, implement
//! [`ReplayGuard`] over a store every replica shares, with an atomic insert-if-absent that
//! expires the entry at `retain_until` (for example a Postgres table with a unique nonce, or
//! Redis `SET NX` with an expiry).

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use crate::wire::{Nonce, NONCE_LEN};

/// Why a replay guard refused a nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayRejection {
    /// Seen before, inside its validity window.
    Seen,
    /// The store could not record it (full, or unreachable). Verification fails closed.
    Unavailable,
}

/// Records nonces. Called only after every other check has passed, so only validly signed,
/// current assertions reach it.
pub trait ReplayGuard: Send + Sync {
    /// Record `nonce` until `retain_until` (unix seconds), or refuse it if it is already
    /// recorded. `now` is the verifier's clock, for pruning. Must be atomic: two concurrent
    /// calls with the same nonce must not both succeed.
    fn check_and_record(
        &self,
        nonce: &Nonce,
        retain_until: u64,
        now: u64,
    ) -> Result<(), ReplayRejection>;
}

/// A bounded in-process replay cache. When it holds `capacity` live nonces it refuses new
/// ones (fail closed) until some expire; since only verified assertions are recorded, only a
/// holder of a signing key can fill it.
pub struct MemoryReplayCache {
    capacity: usize,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    until: HashMap<[u8; NONCE_LEN], u64>,
    by_expiry: BTreeSet<(u64, [u8; NONCE_LEN])>,
}

impl MemoryReplayCache {
    /// The default bound: room for over 800 assertions a second sustained for 120 seconds.
    pub const DEFAULT_CAPACITY: usize = 100_000;

    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Live entries (after pruning at the last call).
    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.until.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for MemoryReplayCache {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

impl std::fmt::Debug for MemoryReplayCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryReplayCache")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .finish()
    }
}

impl ReplayGuard for MemoryReplayCache {
    fn check_and_record(
        &self,
        nonce: &Nonce,
        retain_until: u64,
        now: u64,
    ) -> Result<(), ReplayRejection> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ReplayRejection::Unavailable)?;
        // Drop everything that can no longer verify.
        while let Some(&(until, n)) = inner.by_expiry.first() {
            if until > now {
                break;
            }
            inner.by_expiry.pop_first();
            inner.until.remove(&n);
        }
        if inner.until.contains_key(nonce.as_bytes()) {
            return Err(ReplayRejection::Seen);
        }
        if inner.until.len() >= self.capacity {
            return Err(ReplayRejection::Unavailable);
        }
        inner.until.insert(*nonce.as_bytes(), retain_until);
        inner.by_expiry.insert((retain_until, *nonce.as_bytes()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(b: u8) -> Nonce {
        Nonce([b; NONCE_LEN])
    }

    #[test]
    fn a_nonce_is_accepted_once_while_live() {
        let c = MemoryReplayCache::new(10);
        assert_eq!(c.check_and_record(&n(1), 200, 100), Ok(()));
        assert_eq!(
            c.check_and_record(&n(1), 200, 150),
            Err(ReplayRejection::Seen)
        );
        assert_eq!(c.check_and_record(&n(2), 200, 150), Ok(()));
        assert_eq!(c.len(), 2);
        // Once past its window it is pruned (and could not verify anyway).
        assert_eq!(c.check_and_record(&n(3), 400, 200), Ok(()));
        assert_eq!(c.len(), 1);
        assert_eq!(c.check_and_record(&n(1), 400, 200), Ok(()));
    }

    #[test]
    fn it_is_bounded_and_fails_closed() {
        let c = MemoryReplayCache::new(2);
        assert_eq!(c.check_and_record(&n(1), 200, 100), Ok(()));
        assert_eq!(c.check_and_record(&n(2), 210, 100), Ok(()));
        assert_eq!(
            c.check_and_record(&n(3), 220, 100),
            Err(ReplayRejection::Unavailable)
        );
        assert_eq!(c.len(), 2);
        // Room again once the oldest expires.
        assert_eq!(c.check_and_record(&n(3), 220, 200), Ok(()));
        assert_eq!(
            c.check_and_record(&n(2), 220, 200),
            Err(ReplayRejection::Seen)
        );
    }

    #[test]
    fn a_zero_capacity_still_holds_one() {
        let c = MemoryReplayCache::new(0);
        assert_eq!(c.check_and_record(&n(1), 200, 100), Ok(()));
        assert_eq!(
            c.check_and_record(&n(2), 200, 100),
            Err(ReplayRejection::Unavailable)
        );
    }

    #[test]
    fn concurrent_callers_cannot_both_record_a_nonce() {
        let c = std::sync::Arc::new(MemoryReplayCache::new(100));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (c, barrier) = (c.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    c.check_and_record(&n(9), 200, 100).is_ok()
                })
            })
            .collect();
        let wins: usize = handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum();
        assert_eq!(wins, 1);
    }
}
