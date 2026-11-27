//! What has been withdrawn (spec §4.6). The feed that fills this is the Authority's (G4).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::envelope::document;
use crate::error::{Error, Result};
use crate::ids::Id;

/// The kinds of thing a revocation can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Revocable {
    Agent,
    Mandate,
    Countersignature,
    Key,
}

/// The revocations a verifier holds.
pub trait Revocations {
    fn is_revoked(&self, what: Revocable, id: &Id) -> bool;
}

/// A set in memory.
#[derive(Debug, Clone, Default)]
pub struct RevocationSet(BTreeSet<(Revocable, Id)>);

impl RevocationSet {
    pub fn revoke(&mut self, what: Revocable, id: Id) {
        self.0.insert((what, id));
    }
}

impl Revocations for RevocationSet {
    fn is_revoked(&self, what: Revocable, id: &Id) -> bool {
        self.0.contains(&(what, id.clone()))
    }
}

/// One revocation, as the Authority numbers it. Numbers rise by one from 1 and are never
/// used again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevocationEntry {
    pub seq: u64,
    pub kind: Revocable,
    pub id: Id,
    /// When the thing named would have expired anyway. After this, and the allowance for
    /// clocks, the entry is left out of the feed. A thing with no expiry carries `i64::MAX`.
    pub until: i64,
}

/// Everything revoked after number `after` (spec §7.6.2, point 7). Signed with the leases
/// key, never the admin-record key. The feed only adds: nothing in it is ever undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevocationFeed {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub issuer: Id,
    /// The number the asker said it had read through.
    pub after: u64,
    /// The newest number there is.
    pub through: u64,
    /// How many numbers between `after` and `through` are left out because what they named
    /// has expired. With the entries, they account for every number in between, so a gate
    /// sees that the range it missed was dropped and is not shown a shorter feed as whole.
    pub dropped: u64,
    /// Seconds since the epoch, by the Authority's clock.
    pub at: i64,
    pub entries: Vec<RevocationEntry>,
}

document!(RevocationFeed, "gov.revocations.v1");

impl RevocationFeed {
    /// The entries are in order, lie after `after` and no later than `through`, and with
    /// the dropped ones account for every number in between.
    pub fn validate(&self) -> Result<()> {
        if self.through < self.after {
            return Err(Error::Malformed("a feed ends before it starts"));
        }
        let mut last = self.after;
        for e in &self.entries {
            if e.seq <= last || e.seq > self.through {
                return Err(Error::Malformed("a feed's entries are out of order"));
            }
            last = e.seq;
        }
        let between = self.through - self.after;
        if (self.entries.len() as u64).checked_add(self.dropped) != Some(between) {
            return Err(Error::Malformed("a feed does not account for every number"));
        }
        Ok(())
    }

    /// Add what the feed names to `set`.
    pub fn apply(&self, set: &mut RevocationSet) {
        for e in &self.entries {
            set.revoke(e.kind, e.id.clone());
        }
    }
}
