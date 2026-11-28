//! A gate's renewal and what it does with the answer (spec §7.6.3, points 1 to 3).
//!
//! One signed request brings three things back: a lease, the revocation feed after the
//! number the gate has read through, and the policy bundle when the gate does not hold the
//! one the lease names. A gate takes the whole answer or none of it. Nothing here does any
//! input or output: [`Standing`] is what a gate holds, and [`Standing::take`] is every check.

use serde::{Deserialize, Serialize};

use crate::envelope::{document, open, seal, Envelope, VERSION};
use crate::error::{Error, Result};
use crate::ids::{Id, Scope};
use crate::keys::{random_id, KeySet, PublicKey, SigningKey};
use crate::lease::{HeldLease, Lease, LeaseRequest};
use crate::policy::{may_replace, open_bundle, HeldBundle, OpenedBundle};
use crate::revocation::{RevocationEntry, RevocationFeed, RevocationSet, Revocations};

/// A gate's request to renew, signed with a key enrolled for that gate. It carries no
/// time: a gate may not have one yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Renew {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub gate: Id,
    /// The Authority asked. A request for another is refused there.
    pub authority: Id,
    /// Fresh and random. The lease that answers carries it.
    pub nonce: Id,
    /// The bundle the gate holds, if it holds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<HeldBundle>,
    /// The revocation number the gate has read through. The feed that answers starts here.
    pub revocations_through: u64,
}

document!(Renew, "gov.renew.v1");

/// The Authority's answer. It is not signed as a whole: each part is, and the gate checks
/// that they belong together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Renewed {
    pub lease: Envelope,
    /// Always there, even when it holds no entry: the gate checks where it starts.
    pub feed: Envelope,
    /// Only when the gate did not say it holds the bundle the lease names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<Envelope>,
}

/// Why an answer was not taken. A closed list, for the gate's operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewRefused {
    /// The bundle does not open under the pinned root key, or is another Authority's.
    Bundle,
    /// The bundle has a lower number than the one held.
    BundleOlder,
    /// Another bundle is held under the same number.
    BundleForked,
    /// The lease names a bundle other than the one in hand, or there is none in hand.
    WrongBundle,
    /// The lease does not open under a leases key of the bundle in hand, or is not from
    /// this gate's Authority.
    Lease,
    /// The lease is for another gate, or answers a request this gate is not waiting on.
    NotThisRequest,
    /// The lease's time or revocation number is below what the gate holds: the Authority's
    /// clock was set back, or its database put back from an older copy (GR-168).
    AuthorityBehind,
    /// The feed does not open under the same keys, is another issuer's, does not start
    /// where the gate asked, stops short of the lease's number, or leaves a number out.
    Feed,
}

impl RenewRefused {
    pub fn code(self) -> &'static str {
        match self {
            RenewRefused::Bundle => "bundle",
            RenewRefused::BundleOlder => "bundle_older",
            RenewRefused::BundleForked => "bundle_forked",
            RenewRefused::WrongBundle => "wrong_bundle",
            RenewRefused::Lease => "lease",
            RenewRefused::NotThisRequest => "not_this_request",
            RenewRefused::AuthorityBehind => "authority_behind",
            RenewRefused::Feed => "feed",
        }
    }
}

/// What an answer changed, for the gate to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// The gate now holds another bundle than before.
    pub bundle_changed: bool,
    /// What the feed revoked that the gate did not already hold as revoked.
    pub revoked: Vec<RevocationEntry>,
}

/// What a gate holds from its Authority: the bundle, the lease, and what is revoked.
#[derive(Debug, Clone)]
pub struct Standing {
    scope: Scope,
    gate: Id,
    authority: Id,
    root: PublicKey,
    bundle: Option<OpenedBundle>,
    /// The bundle as it was signed, to hand on to a shim that follows this gate.
    signed: Option<Envelope>,
    lease: Option<HeldLease>,
    revoked: RevocationSet,
    through: u64,
}

impl Standing {
    /// A gate that has heard nothing from its Authority yet. `bundle` is one it was started
    /// with, as signed: it is opened here under `root`, must be this Authority's, and gives
    /// the gate a map at once. No lease is complete until a lease names that very bundle
    /// (GR-132).
    pub fn new(
        scope: Scope,
        gate: Id,
        authority: Id,
        root: PublicKey,
        bundle: Option<Envelope>,
    ) -> Result<Self> {
        let opened = match &bundle {
            None => None,
            Some(envelope) => {
                let opened = open_bundle(envelope, &root, &scope)?;
                if opened.bundle.issuer != authority {
                    return Err(Error::Malformed("a bundle of another Authority"));
                }
                Some(opened)
            }
        };
        Ok(Standing {
            scope,
            gate,
            authority,
            root,
            bundle: opened,
            signed: bundle,
            lease: None,
            revoked: RevocationSet::default(),
            through: 0,
        })
    }

    /// The bundle held, as it was signed by the root key.
    pub fn signed_bundle(&self) -> Option<&Envelope> {
        self.signed.as_ref()
    }

    pub fn bundle(&self) -> Option<&OpenedBundle> {
        self.bundle.as_ref()
    }

    pub fn revoked(&self) -> &RevocationSet {
        &self.revoked
    }

    pub fn revocations_through(&self) -> u64 {
        self.through
    }

    pub fn lease(&self) -> Option<&Lease> {
        self.lease.as_ref().map(HeldLease::lease)
    }

    /// Now, by the Authority's clock: the held lease's time plus the gate's own count since
    /// that lease was asked for. `None` before the first lease.
    pub fn now(&self, mono_ms: u64) -> Option<i64> {
        self.lease.as_ref().and_then(|l| l.now(mono_ms).ok())
    }

    /// Is there a lease, complete and no older than it says it is good for? Only then is
    /// a proof recorded as proven (spec §7.6.3, point 5).
    ///
    /// Complete means the gate holds the bundle the lease names and has read the feed
    /// through its number. That is true of every lease held here: [`Standing::take`] puts
    /// in a lease only together with that bundle and that feed, or puts in nothing.
    pub fn fresh(&self, mono_ms: u64) -> bool {
        self.lease
            .as_ref()
            .is_some_and(|lease| lease.within_its_life(mono_ms))
    }

    /// A request to renew, and the gate's memory of having sent it. `mono_ms` is the gate's
    /// own count of time, which must go on while the machine is suspended.
    pub fn ask(&self, key: &SigningKey, mono_ms: u64) -> Result<(Envelope, LeaseRequest)> {
        let nonce = random_id("n")?;
        let renew = Renew {
            v: VERSION,
            tenant: self.scope.tenant.clone(),
            env: self.scope.env.clone(),
            gate: self.gate.clone(),
            authority: self.authority.clone(),
            nonce: nonce.clone(),
            bundle: self.bundle.as_ref().map(OpenedBundle::held),
            revocations_through: self.through,
        };
        let request = LeaseRequest {
            gate: self.gate.clone(),
            nonce,
            sent_mono_ms: mono_ms,
        };
        Ok((seal(&renew, key)?, request))
    }

    /// Take `answer` as the answer to `request`: all of it, or nothing. On a refusal the
    /// gate holds exactly what it held.
    pub fn take(
        &mut self,
        answer: &Renewed,
        request: LeaseRequest,
    ) -> std::result::Result<Taken, RenewRefused> {
        // The bundle, if one came: under the pinned root key, from this Authority, and
        // never one that would take the gate back.
        let offered = match &answer.bundle {
            None => None,
            Some(envelope) => {
                let opened = open_bundle(envelope, &self.root, &self.scope)
                    .map_err(|_| RenewRefused::Bundle)?;
                if opened.bundle.issuer != self.authority {
                    return Err(RenewRefused::Bundle);
                }
                let held = self.bundle.as_ref().map(OpenedBundle::held);
                may_replace(held.as_ref(), &opened.held()).map_err(|e| match e {
                    Error::BundleOlder => RenewRefused::BundleOlder,
                    Error::BundleForked => RenewRefused::BundleForked,
                    _ => RenewRefused::Bundle,
                })?;
                Some(opened)
            }
        };
        let in_hand = offered
            .as_ref()
            .or(self.bundle.as_ref())
            .ok_or(RenewRefused::WrongBundle)?;

        // The lease: under a leases key of the bundle in hand, which is the one it names.
        let leases = KeySet::new(in_hand.bundle.keys.leases.iter().cloned());
        let lease = open::<Lease>(&answer.lease, &leases, &self.scope)
            .map_err(|_| RenewRefused::Lease)?
            .doc;
        if lease.issuer != self.authority {
            return Err(RenewRefused::Lease);
        }
        if lease.policy_version != in_hand.policy_version() {
            return Err(RenewRefused::WrongBundle);
        }
        let revocation_seq = lease.revocation_seq;
        let issuer = lease.issuer.clone();
        let held = HeldLease::accept(self.lease.as_ref(), lease, request).map_err(|e| match e {
            Error::AuthorityBehind => RenewRefused::AuthorityBehind,
            _ => RenewRefused::NotThisRequest,
        })?;

        // The feed: under the same keys, from the same issuer, starting where this gate has
        // read to, reaching the lease's number, and accounting for every number between.
        let feed = open::<RevocationFeed>(&answer.feed, &leases, &self.scope)
            .map_err(|_| RenewRefused::Feed)?
            .doc;
        if feed.issuer != issuer
            || feed.after != self.through
            || feed.through < revocation_seq
            || feed.validate().is_err()
        {
            return Err(RenewRefused::Feed);
        }

        // Every check has passed: now, and only now, anything changes.
        let revoked: Vec<RevocationEntry> = feed
            .entries
            .iter()
            .filter(|e| !self.revoked.is_revoked(e.kind, &e.id))
            .cloned()
            .collect();
        feed.apply(&mut self.revoked);
        self.through = feed.through;
        let bundle_changed = match (&offered, &self.bundle) {
            (Some(new), Some(old)) => new.digest != old.digest,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if let Some(bundle) = offered {
            self.bundle = Some(bundle);
            self.signed.clone_from(&answer.bundle);
        }
        self.lease = Some(held);
        Ok(Taken {
            bundle_changed,
            revoked,
        })
    }
}
