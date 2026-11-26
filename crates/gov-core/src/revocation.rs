//! What has been withdrawn (spec §4.6). The feed that fills this is the Authority's (G4).

use std::collections::BTreeSet;

use crate::ids::Id;

/// The kinds of thing a revocation can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
