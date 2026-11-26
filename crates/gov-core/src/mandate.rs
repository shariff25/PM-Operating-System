//! A mandate: what one agent may do, on which resources, until a set time (spec §4.2).

use serde::{Deserialize, Serialize};

use crate::action::{self, Grant};
use crate::envelope::{document, open, Envelope};
use crate::error::{Error, Result};
use crate::ids::{Id, Scope};
use crate::keys::{KeySet, PublicKey};
use crate::revocation::{Revocable, Revocations};

/// The longest chain a gate accepts: a root mandate and three delegations below it.
pub const MAX_CHAIN: usize = 4;

/// The agent a mandate is for. Only a request signed by `key` can use the mandate (T2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRef {
    pub id: Id,
    pub key: PublicKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuerKind {
    Person,
    Agent,
}

/// Who asked the Authority for the mandate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuedBy {
    pub kind: IssuerKind,
    pub id: Id,
    /// The person's signed-in session, for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
}

/// Ceilings the gate counts against. Absent means no ceiling of that kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_actions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_writes: Option<u64>,
}

impl Limits {
    fn within(&self, parent: &Limits) -> bool {
        fn one(child: Option<u64>, parent: Option<u64>) -> bool {
            match (child, parent) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(c), Some(p)) => c <= p,
            }
        }
        one(self.max_actions, parent.max_actions) && one(self.max_writes, parent.max_writes)
    }
}

/// Conditions on where the agent runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requires {
    /// Honoured only on a runtime adapter's word. None exists yet, so a mandate that sets
    /// this is refused rather than believed.
    #[serde(default)]
    pub sandbox: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mandate {
    pub v: u32,
    pub tenant: Id,
    pub env: Id,
    pub id: Id,
    pub issuer: Id,
    /// Seconds since the epoch, by the Authority's clock.
    pub issued_at: i64,
    pub expires_at: i64,
    pub agent: AgentRef,
    pub issued_by: IssuedBy,
    /// `<name>@<policy version>`.
    pub template: Id,
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub requires: Requires,
    /// The parent's whole signed mandate, for a delegated one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Box<Envelope>>,
}

document!(Mandate, "gov.mandate.v1");

/// What a gate checks a chain against.
pub struct ChainCheck<'a> {
    pub scope: &'a Scope,
    /// The Authority's keys. Every link must be signed by one of them.
    pub authority: &'a KeySet,
    /// Now, in seconds, from the lease's Authority time and the gate's monotonic clock.
    pub now: i64,
    pub revoked: &'a dyn Revocations,
}

/// A chain every link of which has been checked. The first link is the one presented.
#[derive(Debug, Clone)]
pub struct VerifiedChain {
    links: Vec<Mandate>,
}

impl VerifiedChain {
    /// The mandate that was presented: the one whose agent must sign the request.
    pub fn leaf(&self) -> &Mandate {
        &self.links[0]
    }
    /// Mandate ids, the presented one first.
    pub fn ids(&self) -> Vec<Id> {
        self.links.iter().map(|m| m.id.clone()).collect()
    }
    pub fn links(&self) -> &[Mandate] {
        &self.links
    }
}

/// Check a mandate and every ancestor it embeds (threats T4, T11, T19).
///
/// Each link: signed by the Authority; this tenant and environment; in its validity window;
/// neither it, its agent nor its signing key revoked; no sandbox condition. Each child, against
/// its parent: delegated by the parent's agent; its window, grants and limits inside the
/// parent's. The root must have been issued to a person's request.
///
/// The Authority already checked all of this when it signed. The gate checks again so that a
/// fault in the Authority's arithmetic cannot widen a mandate.
pub fn verify_chain(envelope: &Envelope, check: &ChainCheck<'_>) -> Result<VerifiedChain> {
    let mut links: Vec<Mandate> = Vec::new();
    let mut next = Some(envelope.clone());
    while let Some(env) = next {
        if links.len() == MAX_CHAIN {
            return Err(Error::ChainTooDeep);
        }
        let opened = open::<Mandate>(&env, check.authority, check.scope)?;
        let m = opened.doc;
        if m.issued_at >= m.expires_at {
            return Err(Error::Malformed("a mandate's window is empty"));
        }
        if check.now < m.issued_at {
            return Err(Error::NotYetValid);
        }
        if check.now >= m.expires_at {
            return Err(Error::Expired);
        }
        if check.revoked.is_revoked(Revocable::Key, &opened.kid)
            || check.revoked.is_revoked(Revocable::Mandate, &m.id)
            || check.revoked.is_revoked(Revocable::Agent, &m.agent.id)
            // A stolen agent key, revoked and replaced, must not keep its old mandates alive.
            || check.revoked.is_revoked(Revocable::Key, &m.agent.key.kid)
        {
            return Err(Error::Revoked);
        }
        if m.requires.sandbox {
            return Err(Error::SandboxUnverifiable);
        }
        if m.grants.is_empty() {
            return Err(Error::Malformed("a mandate grants nothing"));
        }
        for g in &m.grants {
            g.validate()?;
        }
        next = m.parent.as_deref().cloned();
        links.push(m);
    }
    for pair in links.windows(2) {
        let (child, parent) = (&pair[0], &pair[1]);
        if child.issued_by.kind != IssuerKind::Agent || child.issued_by.id != parent.agent.id {
            return Err(Error::NotDelegatedByParent);
        }
        if child.expires_at > parent.expires_at || child.issued_at < parent.issued_at {
            return Err(Error::WiderThanParent("time"));
        }
        if !action::within(&child.grants, &parent.grants) {
            return Err(Error::WiderThanParent("grants"));
        }
        if !child.limits.within(&parent.limits) {
            return Err(Error::WiderThanParent("limits"));
        }
    }
    let root = links.last().expect("the loop pushes at least one link");
    if root.issued_by.kind != IssuerKind::Person {
        return Err(Error::NotDelegatedByParent);
    }
    Ok(VerifiedChain { links })
}
