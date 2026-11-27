//! The connector map: what each upstream tool is and does (spec §4.3, §4.7).
//!
//! It is part of the signed policy bundle. A gate takes an operation's name, its kind, whether
//! it can be put back and whether it reaches outside from here, never from what the tool says
//! about itself. Each tool is pinned by a digest of its definition, so a tool that changes is
//! no longer the tool that was mapped (threat T14).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::action::{CanonicalAction, Kind, Reversal, Salt};
use crate::error::{Error, Result};
use crate::ids::{Digest, Id, ResourcePath};
use crate::jcs;

/// The digest that pins a tool: SHA-256 over the canonical form of its whole definition, as
/// the server lists it. Every field is covered, so text added anywhere in a definition makes
/// it another tool (review finding GR-64). A definition with no canonical form has no digest,
/// and so can never be the tool that was reviewed.
pub fn tool_digest(definition: &Value) -> Result<Digest> {
    Ok(Digest::of(&[
        b"gov.tool.v2\0",
        jcs::canonical(definition)?.as_bytes(),
    ]))
}

/// How a call's resource is built from its arguments: identifiers joined by `/`, where
/// `{name}` is the string argument of that name and `{name:before(-)}` is the part of it
/// before the first `-` (`jira/project/{key:before(-)}/issue/{key}`).
///
/// **The rule for writing one** (review finding GR-65): use only the arguments the server
/// itself uses to pick its target. Jira acts on the issue key, whatever a `project` argument
/// beside it says; a template built from both would let a call name one project and act on
/// another. Where one part of the resource follows from another, derive it here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ResourceTemplate(Vec<Segment>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(Id),
    Arg(Id),
    /// The part of the argument before the first of this character; the argument must hold it.
    Before(Id, char),
}

impl ResourceTemplate {
    pub fn new(s: &str) -> Result<Self> {
        let mut segments = Vec::new();
        for part in s.split('/') {
            let segment = match part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                Some(inner) => match inner.split_once(":before(") {
                    Some((name, rest)) => {
                        let mut chars = rest.chars();
                        match (chars.next(), chars.as_str()) {
                            (Some(c), ")") if c == '-' || c == '.' || c == '_' || c == ':' => {
                                Segment::Before(Id::new(name)?, c)
                            }
                            _ => return Err(Error::Malformed("resource template")),
                        }
                    }
                    None => Segment::Arg(Id::new(inner)?),
                },
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

    fn literals(&self) -> impl Iterator<Item = &Id> {
        self.0.iter().filter_map(|s| match s {
            Segment::Literal(id) => Some(id),
            _ => None,
        })
    }

    /// The resource for these arguments. Every argument used must be a string that is itself
    /// a valid identifier; anything else cannot be mapped, and is not guessed at.
    fn fill(&self, args: &Value, fold_case: bool) -> Result<ResourcePath> {
        let arg = |name: &Id| -> Result<String> {
            let value = args
                .get(name.as_str())
                .and_then(Value::as_str)
                .ok_or(Error::Malformed("an argument the resource is built from"))?;
            let value = if fold_case {
                value.to_ascii_lowercase()
            } else {
                value.to_string()
            };
            Ok(Id::new(&value)?.as_str().to_string())
        };
        let mut parts = Vec::with_capacity(self.0.len());
        for segment in &self.0 {
            match segment {
                Segment::Literal(id) => parts.push(id.as_str().to_string()),
                Segment::Arg(name) => parts.push(arg(name)?),
                Segment::Before(name, c) => {
                    let whole = arg(name)?;
                    let (before, _) = whole
                        .split_once(*c)
                        .ok_or(Error::Malformed("an argument the resource is built from"))?;
                    parts.push(Id::new(before)?.as_str().to_string());
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
                Segment::Before(id, c) => format!("{{{id}:before({c})}}"),
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

/// The operation a call is recorded under when the map cannot read it: a digest of the
/// tool's name, never the name. Whoever holds a server's listing can work out which tool
/// a recorded operation stands for; nobody else can read a name out of a receipt.
pub fn unmapped_operation(tool: &str) -> Id {
    let name = Digest::of(&[b"gov.toolname.v1\0", tool.as_bytes()]);
    Id::new(&format!("unmapped-{}", &name.as_str()[..16])).expect("hex digits are an identifier")
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
            // In a system that does not tell upper case from lower, everything is written
            // in lower case, so that nothing fails to match by its case (finding GR-66).
            if self.fold_case
                && tool
                    .resource
                    .literals()
                    .any(|l| l.as_str() != l.as_str().to_ascii_lowercase())
            {
                return Err(Error::Malformed("a folded system's template is lower case"));
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
    /// never listed, arguments the resource cannot be built from, arguments with no canonical
    /// form) gives an action of kind `unknown`: it can be recorded in observe mode, so a
    /// customer sees everything its agents do, and it can never be allowed in enforce mode.
    ///
    /// **It never fails.** A call that could make mapping fail would be a call with no
    /// receipt (review finding GR-61).
    pub fn map(&self, tool: &str, listed: Option<&Digest>, args: &Value, salt: &Salt) -> Mapped {
        let known = self
            .tools
            .iter()
            .find(|t| t.tool.as_str() == tool)
            .filter(|t| listed == Some(&t.digest));
        if let Some(t) = known {
            let action = t.resource.fill(args, self.fold_case).and_then(|resource| {
                CanonicalAction::new(
                    self.system.clone(),
                    t.operation.clone(),
                    t.kind,
                    resource,
                    None,
                    args,
                    salt,
                )
            });
            if let Ok(action) = action {
                return Mapped {
                    action,
                    reversal: t.reversal,
                    outward: t.outward,
                    known: true,
                };
            }
        }
        // The operation carries a digest of the tool's name, not the name: a name is up to
        // the caller or the server, and a receipt holds no text either could choose (T17,
        // review finding GR-71). The resource says only that it was not mapped. The worst is
        // assumed of it.
        let operation = unmapped_operation(tool);
        let resource = ResourcePath::new(&format!("{}/unmapped", self.system))
            .expect("a system and a word are a resource");
        Mapped {
            action: CanonicalAction {
                system: self.system.clone(),
                operation,
                kind: Kind::Unknown,
                resource,
                resource_version: None,
                args_digest: crate::action::args_digest(args, salt)
                    .unwrap_or_else(|_| crate::action::raw_args_digest(args, salt)),
            },
            reversal: Reversal::None,
            outward: true,
            known: false,
        }
    }
}
