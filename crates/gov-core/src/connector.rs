//! The connector map: what each upstream tool is and does (spec §4.3, §4.7).
//!
//! It is part of the signed policy bundle. A gate takes an operation's name, its kind, whether
//! it can be put back and whether it reaches outside from here, never from what the tool says
//! about itself. Each tool is pinned by a digest of its definition, so a tool that changes is
//! no longer the tool that was mapped (threat T14).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::action::{CanonicalAction, Kind, Reversal, Salt};
use crate::error::{Error, Result};
use crate::ids::{Digest, Id, ResourcePath};
use crate::jcs;

/// The digest that pins a tool: SHA-256 over the canonical form of its `name`, `title`,
/// `description`, `inputSchema`, `outputSchema` and `annotations`, as the server lists them.
pub fn tool_digest(definition: &Value) -> Result<Digest> {
    let pinned = json!({
        "name": definition.get("name"),
        "title": definition.get("title"),
        "description": definition.get("description"),
        "inputSchema": definition.get("inputSchema"),
        "outputSchema": definition.get("outputSchema"),
        "annotations": definition.get("annotations"),
    });
    Ok(Digest::of(&[
        b"gov.tool.v1\0",
        jcs::canonical(&pinned)?.as_bytes(),
    ]))
}

/// How a call's resource is built from its arguments: identifiers joined by `/`, where a
/// segment written `{name}` is taken from the string argument of that name
/// (`jira/project/{project}/issue/{key}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResourceTemplate(Vec<Segment>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(Id),
    Arg(Id),
}

impl ResourceTemplate {
    pub fn new(s: &str) -> Result<Self> {
        let mut segments = Vec::new();
        for part in s.split('/') {
            let segment = match part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                Some(name) => Segment::Arg(Id::new(name)?),
                None => Segment::Literal(Id::new(part)?),
            };
            segments.push(segment);
        }
        if segments.len() > 16 {
            return Err(Error::Malformed("resource template"));
        }
        Ok(ResourceTemplate(segments))
    }

    /// The first segment, which must be a literal: the system.
    fn system(&self) -> Option<&Id> {
        match self.0.first() {
            Some(Segment::Literal(id)) => Some(id),
            _ => None,
        }
    }

    /// The resource for these arguments. Every `{name}` must be a string argument that is
    /// itself a valid identifier; anything else cannot be mapped, and is not guessed at.
    fn fill(&self, args: &Value, fold_case: bool) -> Result<ResourcePath> {
        let mut parts = Vec::with_capacity(self.0.len());
        for segment in &self.0 {
            match segment {
                Segment::Literal(id) => parts.push(id.as_str().to_string()),
                Segment::Arg(name) => {
                    let value = args
                        .get(name.as_str())
                        .and_then(Value::as_str)
                        .ok_or(Error::Malformed("an argument the resource is built from"))?;
                    let value = if fold_case {
                        value.to_ascii_lowercase()
                    } else {
                        value.to_string()
                    };
                    parts.push(Id::new(&value)?.as_str().to_string());
                }
            }
        }
        ResourcePath::new(&parts.join("/"))
    }
}

impl TryFrom<String> for ResourceTemplate {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        ResourceTemplate::new(&s)
    }
}

impl From<ResourceTemplate> for String {
    fn from(t: ResourceTemplate) -> String {
        t.0.iter()
            .map(|s| match s {
                Segment::Literal(id) => id.as_str().to_string(),
                Segment::Arg(id) => format!("{{{id}}}"),
            })
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// One upstream tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolMap {
    /// The tool's name as the server lists it.
    pub tool: Id,
    /// The digest of the definition that was reviewed. See [`tool_digest`].
    pub digest: Digest,
    pub operation: Id,
    pub kind: Kind,
    pub reversal: Reversal,
    /// Its effects leave the system or widen who can see something: a notification, a
    /// webhook, a share. Such an operation asks for a person even when it can be put back
    /// (spec §4.7; review finding GR-35).
    pub outward: bool,
    pub resource: ResourceTemplate,
}

/// One upstream server and the system it fronts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerMap {
    pub server: Id,
    pub system: Id,
    /// The system's identifiers do not tell upper case from lower (Jira's project keys), so
    /// the parts of a resource taken from arguments are lower-cased. Without it a refusal of
    /// `jira/project/pay/*` would be passed by asking for `PAY`.
    #[serde(default)]
    pub fold_case: bool,
    pub tools: Vec<ToolMap>,
}

/// A call, mapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    pub action: CanonicalAction,
    pub reversal: Reversal,
    pub outward: bool,
    /// False when the map does not know the tool, or the tool has changed since it was
    /// mapped. The action then has a kind of `unknown`, which nothing may enforce on.
    pub known: bool,
}

impl ServerMap {
    /// The lint for one server's map.
    pub fn validate(&self) -> Result<()> {
        let mut seen = BTreeSet::new();
        for tool in &self.tools {
            if !seen.insert(&tool.tool) {
                return Err(Error::Malformed("a tool is mapped twice"));
            }
            if tool.kind == Kind::Unknown {
                return Err(Error::Malformed("a mapped tool has a known kind"));
            }
            if tool.resource.system() != Some(&self.system) {
                return Err(Error::Malformed("a tool's resource is not in its system"));
            }
            // A read changes nothing, so there is nothing to put back or to reach outside.
            if tool.kind == Kind::Read && (tool.reversal.reversible() || tool.outward) {
                return Err(Error::Malformed("a read is neither reversible nor outward"));
            }
        }
        Ok(())
    }

    /// Map one call. `listed` is the digest of the tool's definition as the server listed it
    /// in this session, if it listed it.
    ///
    /// A tool the map knows, listed with the digest that was reviewed, gives the mapped
    /// action. Anything else (a tool not in the map, a tool whose definition changed, a tool
    /// never listed, arguments the resource cannot be built from) gives an action of kind
    /// `unknown`: it can be recorded in observe mode, so a customer sees everything its
    /// agents do, and it can never be allowed in enforce mode.
    pub fn map(
        &self,
        tool: &str,
        listed: Option<&Digest>,
        args: &Value,
        salt: &Salt,
    ) -> Result<Mapped> {
        let known = self
            .tools
            .iter()
            .find(|t| t.tool.as_str() == tool)
            .filter(|t| listed == Some(&t.digest));
        if let Some(t) = known {
            if let Ok(resource) = t.resource.fill(args, self.fold_case) {
                return Ok(Mapped {
                    action: CanonicalAction::new(
                        self.system.clone(),
                        t.operation.clone(),
                        t.kind,
                        resource,
                        None,
                        args,
                        salt,
                    )?,
                    reversal: t.reversal,
                    outward: t.outward,
                    known: true,
                });
            }
        }
        // The operation is named after the tool where the name can be an identifier; the
        // resource says only that it was not mapped. The worst is assumed of it.
        let operation = Id::new(tool).unwrap_or(Id::new("unnamed-tool")?);
        let resource = ResourcePath::new(&format!("{}/unmapped", self.system))?;
        Ok(Mapped {
            action: CanonicalAction::new(
                self.system.clone(),
                operation,
                Kind::Unknown,
                resource,
                None,
                args,
                salt,
            )?,
            reversal: Reversal::None,
            outward: true,
            known: false,
        })
    }
}
