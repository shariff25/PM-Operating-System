//! The one shape every connector maps a call to (spec §4.3), and the grants that carry it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::ids::{Digest, Id, ResourcePath, ResourcePattern};
use crate::jcs;

/// What an operation does to its target. From the signed connector map, never from the
/// tool's own description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Read,
    Write,
    Destructive,
    /// The connector map does not know the tool, or the tool has changed since it was mapped.
    /// It can be recorded where nothing is enforced, so that observe mode shows everything an
    /// agent does; it is never allowed, and an enforced receipt cannot carry it.
    Unknown,
}

/// Whether, and how, an operation can be put back (spec §4.7). From the signed connector map,
/// like [`Kind`]. Reserved in slice G1's formats; undo itself is slice G9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversal {
    /// The system keeps versions: undo writes the earlier values back.
    Restore,
    /// There is an opposite operation: create and archive, add and remove.
    Inverse,
    /// It cannot be put back: a sent message, a hard delete, a payment.
    None,
}

impl Reversal {
    pub fn reversible(self) -> bool {
        !matches!(self, Reversal::None)
    }
}

/// A random value, fresh for each action, from the system's random source. It is never in a
/// receipt. Whoever first forms the action draws it: the shim, which must sign the action's
/// digest before the gate has seen the call, sends it with the call, and the gate refuses a
/// salt it has already seen in that session and computes the digest again itself (review
/// finding GR-44; spec §4.3). For the values before a write, the gate draws it.
pub type Salt = [u8; 32];

/// The digest of an action's arguments: SHA-256 over a salt and their RFC 8785 form.
///
/// Salted because a plain digest of a small value (a status, a yes or no, an amount) gives the
/// value to anyone who can guess it, and receipts outlive the values (review finding GR-36).
/// Whoever must recompute the digest, such as the page an approver reads, is given the salt
/// with the readable arguments.
pub fn args_digest(args: &Value, salt: &Salt) -> Result<Digest> {
    Ok(Digest::of(&[
        b"gov.args.v1\0",
        salt,
        jcs::canonical(args)?.as_bytes(),
    ]))
}

/// The digest of arguments that have no canonical form (an integer beyond 2^53 is the usual
/// cause): over the salt and the arguments as the gate will send them. It cannot be
/// reproduced by another implementation, and is only ever on an action of kind `unknown`; it
/// exists so that no call goes without a receipt (review finding GR-61).
pub fn raw_args_digest(args: &Value, salt: &Salt) -> Digest {
    let bytes = serde_json::to_vec(args).unwrap_or_default();
    Digest::of(&[b"gov.args.raw.v1\0", salt, &bytes])
}

/// The digest of a target's values before a write, salted for the same reason (spec §4.7).
pub fn before_digest(values: &Value, salt: &Salt) -> Result<Digest> {
    Ok(Digest::of(&[
        b"gov.before.v1\0",
        salt,
        jcs::canonical(values)?.as_bytes(),
    ]))
}

/// System, operation, kind, resource, the target's version where the system has one, and the
/// digest of the arguments. Policy and mandates match on the first four; approvals bind
/// [`CanonicalAction::digest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawAction")]
pub struct CanonicalAction {
    pub system: Id,
    pub operation: Id,
    pub kind: Kind,
    pub resource: ResourcePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_version: Option<Id>,
    pub args_digest: Digest,
}

/// What is parsed, before the rule that a resource lies in its system is applied: an action
/// read from the wire keeps the rule too (review finding GR-19).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAction {
    system: Id,
    operation: Id,
    kind: Kind,
    resource: ResourcePath,
    #[serde(default)]
    resource_version: Option<Id>,
    args_digest: Digest,
}

impl TryFrom<RawAction> for CanonicalAction {
    type Error = Error;
    fn try_from(raw: RawAction) -> Result<Self> {
        if raw.resource.system() != raw.system.as_str() {
            return Err(Error::Malformed("resource is not in its system"));
        }
        Ok(CanonicalAction {
            system: raw.system,
            operation: raw.operation,
            kind: raw.kind,
            resource: raw.resource,
            resource_version: raw.resource_version,
            args_digest: raw.args_digest,
        })
    }
}

impl CanonicalAction {
    /// Refuses a resource that does not begin with its system.
    pub fn new(
        system: Id,
        operation: Id,
        kind: Kind,
        resource: ResourcePath,
        resource_version: Option<Id>,
        args: &Value,
        salt: &Salt,
    ) -> Result<Self> {
        if resource.system() != system.as_str() {
            return Err(Error::Malformed("resource is not in its system"));
        }
        Ok(CanonicalAction {
            system,
            operation,
            kind,
            resource,
            resource_version,
            args_digest: args_digest(args, salt)?,
        })
    }

    /// SHA-256 over the canonical form of the six fields. Any change to the arguments, the
    /// target or its version gives another digest.
    pub fn digest(&self) -> Digest {
        let value = serde_json::to_value(self).expect("an action serialises");
        let text = jcs::canonical(&value).expect("an action holds no numbers");
        Digest::of(&[b"gov.action.v1\0", text.as_bytes()])
    }
}

/// Part of a mandate: these operations of this system, on these resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub system: Id,
    /// Exact operation names from the connector map. There is no wildcard.
    pub operations: BTreeSet<Id>,
    pub resources: Vec<ResourcePattern>,
}

impl Grant {
    /// A grant must name at least one operation and one resource, all inside its system.
    pub fn validate(&self) -> Result<()> {
        if self.operations.is_empty() || self.resources.is_empty() {
            return Err(Error::Malformed("an empty grant"));
        }
        if self
            .resources
            .iter()
            .any(|r| r.system() != self.system.as_str())
        {
            return Err(Error::Malformed("a grant's resource is not in its system"));
        }
        Ok(())
    }

    fn carries(&self, action: &CanonicalAction) -> bool {
        self.system == action.system
            && self.operations.contains(&action.operation)
            && self.resources.iter().any(|r| r.covers(&action.resource))
    }
}

/// Do `grants` carry `action`?
pub fn carries(grants: &[Grant], action: &CanonicalAction) -> bool {
    grants.iter().any(|g| g.carries(action))
}

/// Is everything `child` grants also granted by `parent`? Each operation of each child grant,
/// on each of that grant's resource patterns, must lie inside one parent grant.
pub fn within(child: &[Grant], parent: &[Grant]) -> bool {
    child.iter().all(|c| {
        c.operations.iter().all(|op| {
            c.resources.iter().all(|res| {
                parent.iter().any(|p| {
                    p.system == c.system
                        && p.operations.contains(op)
                        && p.resources.iter().any(|pr| res.within(pr))
                })
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn action(args: Value, version: Option<&str>) -> CanonicalAction {
        CanonicalAction::new(
            Id::new("jira").unwrap(),
            Id::new("issue.update").unwrap(),
            Kind::Write,
            ResourcePath::new("jira/project/PAY/issue/PAY-12").unwrap(),
            version.map(|v| Id::new(v).unwrap()),
            &args,
            &[9; 32],
        )
        .unwrap()
    }

    #[test]
    fn the_digest_follows_arguments_target_and_version() {
        let a = action(json!({"summary": "x", "points": 3}), Some("7"));
        let same = action(json!({"points": 3.0, "summary": "x"}), Some("7"));
        assert_eq!(a.digest(), same.digest());
        assert_ne!(
            a.digest(),
            action(json!({"summary": "y", "points": 3}), Some("7")).digest()
        );
        assert_ne!(
            a.digest(),
            action(json!({"summary": "x", "points": 3}), Some("8")).digest()
        );
        assert_ne!(
            a.digest(),
            action(json!({"summary": "x", "points": 3}), None).digest()
        );
        let mut moved = a.clone();
        moved.resource = ResourcePath::new("jira/project/PAY/issue/PAY-13").unwrap();
        assert_ne!(a.digest(), moved.digest());
        let mut kind = a.clone();
        kind.kind = Kind::Destructive;
        assert_ne!(a.digest(), kind.digest());
    }

    /// Review finding GR-36: the same small value under two salts gives unrelated digests, so
    /// a digest cannot be matched against a list of guesses.
    #[test]
    fn a_digest_of_a_small_value_does_not_give_it_away() {
        let yes = json!({"approved": true});
        let a = args_digest(&yes, &[1; 32]).unwrap();
        assert_eq!(a, args_digest(&yes, &[1; 32]).unwrap());
        assert_ne!(a, args_digest(&yes, &[2; 32]).unwrap());
        assert_ne!(
            before_digest(&yes, &[1; 32]).unwrap(),
            args_digest(&yes, &[1; 32]).unwrap()
        );
    }

    #[test]
    fn a_resource_must_be_in_its_system() {
        assert!(CanonicalAction::new(
            Id::new("jira").unwrap(),
            Id::new("issue.update").unwrap(),
            Kind::Write,
            ResourcePath::new("github/repo/x").unwrap(),
            None,
            &json!({}),
            &[9; 32],
        )
        .is_err());
    }
}
