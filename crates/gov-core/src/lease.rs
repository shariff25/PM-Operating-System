//! The lease that lets a gate decide alone, and the checkpoint the Ledger publishes
//! (spec §4.6, §6.2).

use serde::{Deserialize, Serialize};

use crate::envelope::document;
use crate::error::{Error, Result};
use crate::ids::{Digest, Id};

/// How fresh a lease an action needs. `Strict` is the stricter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Standard,
    Strict,
}

/// The oldest lease each class accepts, in seconds. The defaults are first figures, not
/// measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshnessLimits {
    pub strict_secs: u32,
    pub standard_secs: u32,
}

impl Default for FreshnessLimits {
    fn default() -> Self {
        FreshnessLimits {
            strict_secs: 10,
            standard_secs: 120,
        }
    }
}

impl FreshnessLimits {
    pub fn validate(&self) -> Result<()> {
        if self.strict_secs == 0 || self.strict_secs > self.standard_secs {
            return Err(Error::Malformed("freshness limits"));
        }
        Ok(())
    }
    fn secs(&self, class: Freshness) -> u64 {
        match class {
            Freshness::Strict => self.strict_secs.into(),
            Freshness::Standard => self.standard_secs.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub issuer: Id,
    pub gate: Id,
    /// The Authority's clock when it signed, in seconds since the epoch.
    pub authority_time: i64,
    /// The policy bundle the gate must be deciding from.
    pub policy_version: Id,
    /// The gate must hold every revocation up to this number.
    pub revocation_seq: u64,
    /// No action of any class is decided on a lease older than this.
    pub valid_for_secs: u32,
}

document!(Lease, "gov.lease.v1");

/// A lease and the gate's monotonic clock when it arrived.
///
/// A gate's "now" is the lease's Authority time plus its own monotonic clock since. No wall
/// clock is read, so a wrong one cannot keep an expired document alive (T20).
#[derive(Debug, Clone)]
pub struct HeldLease {
    lease: Lease,
    received_mono_ms: u64,
}

impl HeldLease {
    pub fn new(lease: Lease, received_mono_ms: u64) -> Self {
        HeldLease {
            lease,
            received_mono_ms,
        }
    }

    pub fn lease(&self) -> &Lease {
        &self.lease
    }

    fn age_ms(&self, mono_ms: u64) -> Result<u64> {
        // A monotonic clock that went backwards is not one; refuse.
        mono_ms
            .checked_sub(self.received_mono_ms)
            .ok_or(Error::LeaseStale)
    }

    /// Now, in seconds since the epoch.
    pub fn now(&self, mono_ms: u64) -> Result<i64> {
        let age = i64::try_from(self.age_ms(mono_ms)? / 1000).map_err(|_| Error::LeaseStale)?;
        self.lease
            .authority_time
            .checked_add(age)
            .ok_or(Error::LeaseStale)
    }

    /// May an action of `class` be decided now? Only if the gate is deciding from the bundle
    /// the lease names, holds every revocation it names, and the lease is young enough for the
    /// class (T11). No class is exempt.
    pub fn permits(
        &self,
        mono_ms: u64,
        class: Freshness,
        limits: &FreshnessLimits,
        policy_held: &Id,
        revocations_held_through: u64,
    ) -> Result<()> {
        if policy_held != &self.lease.policy_version
            || revocations_held_through < self.lease.revocation_seq
        {
            return Err(Error::LeaseIncomplete);
        }
        let age = self.age_ms(mono_ms)?;
        let limit = limits
            .secs(class)
            .min(self.lease.valid_for_secs.into())
            .saturating_mul(1000);
        if age > limit {
            return Err(Error::LeaseStale);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateHead {
    pub gate: Id,
    pub seq: u64,
    pub hash: Digest,
}

/// The Ledger's signed statement of every gate's chain head, written to the witness on a timer
/// whether or not anything changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub issuer: Id,
    /// Rises by one, so a missing checkpoint shows.
    pub seq: u64,
    pub at: i64,
    /// One entry per gate, in order of gate id.
    pub heads: Vec<GateHead>,
}

document!(Checkpoint, "gov.checkpoint.v1");

impl Checkpoint {
    pub fn validate(&self) -> Result<()> {
        if self.seq == 0 || !self.heads.windows(2).all(|w| w[0].gate < w[1].gate) {
            return Err(Error::Malformed("checkpoint"));
        }
        Ok(())
    }
}
