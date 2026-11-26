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
}

/// The digest of an action's arguments: SHA-256 over their RFC 8785 form.
pub fn args_digest(args: &Value) -> Result<Digest> {
    Ok(Digest::of(&[
        b"gov.args.v1\0",
        jcs::canonical(args)?.as_bytes(),
    ]))
}

/// System, operation, kind, resource, the target's version where the system has one, and the
/// digest of the arguments. Policy and mandates match on the first four; approvals bind
/// [`CanonicalAction::digest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalAction {
    pub system: Id,
    pub operation: Id,
    pub kind: Kind,
    pub resource: ResourcePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_version: Option<Id>,
    pub args_digest: Digest,
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
            args_digest: args_digest(args)?,
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

    #[test]
    fn a_resource_must_be_in_its_system() {
        assert!(CanonicalAction::new(
            Id::new("jira").unwrap(),
            Id::new("issue.update").unwrap(),
            Kind::Write,
            ResourcePath::new("github/repo/x").unwrap(),
            None,
            &json!({}),
        )
        .is_err());
    }
}
