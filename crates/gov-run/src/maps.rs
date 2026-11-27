//! Connector maps as a library (spec §10, slices CM1 and CM2).
//!
//! A map is made from two things. The **listing** is the server's own answer to
//! `tools/list`, kept as it was given: it fixes each tool's digest. The **review** is what a
//! person decided about each tool after reading it: the operation, its kind, whether and
//! how it can be put back, whether it reaches outside the system, and the resource it acts
//! on. `build` joins them, and refuses unless every listed tool is either reviewed or set
//! aside by name with a reason: a tool nobody looked at never slips into a map, and a map
//! never claims a tool the server does not list.
//!
//! `drift` compares a later listing with a map: tools the server has added, removed, or
//! changed in any field of their definition since the map was reviewed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, ensure, Context};
use gov_core::action::{Kind, Reversal};
use gov_core::connector::{tool_digest, ResourceTemplate, ServerMap, ToolMap};
use gov_core::Id;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The largest listing or review read.
const MAX_FILE: u64 = 16 * 1024 * 1024;

/// What a person decided about one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reviewed {
    pub operation: Id,
    pub kind: Kind,
    pub reversal: Reversal,
    pub outward: bool,
    pub resource: ResourceTemplate,
    /// Whether what the tool returns can be trusted to steer an agent. Left out, it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results: Option<gov_core::label::Integrity>,
    /// Why, where the choice is not the obvious one. For the reader; it is not in the map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The review of one server at one version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub server: Id,
    pub system: Id,
    #[serde(default)]
    pub fold_case: bool,
    /// What was reviewed: the server's package and version, and how its listing was got.
    pub source: String,
    pub tools: BTreeMap<String, Reviewed>,
    /// How confidential the system's resources are, and what may be sent where (access
    /// governance spec, CG-4). Which project is confidential is the deployment's to say,
    /// so a map in the library carries none; a resource nobody labelled is taken for the
    /// most closely held.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<gov_core::connector::ResourceLabel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clearances: Vec<gov_core::connector::ResourceClearance>,
    /// Tools left out of the map on purpose, each with the reason. A call to one is
    /// recorded as unknown and can never be allowed where anything is enforced.
    #[serde(default)]
    pub unmapped: BTreeMap<String, String>,
}

fn strict<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("{} cannot be read", path.display()))?;
    ensure!(
        meta.is_file() && meta.len() <= MAX_FILE,
        "{} is not a file of a sane size",
        path.display()
    );
    let bytes =
        std::fs::read(path).with_context(|| format!("{} cannot be read", path.display()))?;
    let value = gov_core::strict::parse(&bytes)
        .map_err(|_| anyhow::anyhow!("{} is not strict JSON", path.display()))?;
    serde_json::from_value(value)
        .with_context(|| format!("{} is not in the form expected", path.display()))
}

pub fn read_review(path: &Path) -> anyhow::Result<Review> {
    strict(path)
}

pub fn read_map(path: &Path) -> anyhow::Result<ServerMap> {
    strict(path)
}

/// A server's tool listing from a file: the `tools` array of a `tools/list` result, in an
/// object that holds it under `tools`, under `result.tools`, or bare.
pub fn read_listing(path: &Path) -> anyhow::Result<Vec<Value>> {
    let value: Value = strict(path)?;
    let tools = match &value {
        Value::Array(_) => &value,
        Value::Object(o) => o
            .get("tools")
            .or_else(|| o.get("result").and_then(|r| r.get("tools")))
            .with_context(|| format!("{} holds no tools", path.display()))?,
        _ => bail!("{} holds no tools", path.display()),
    };
    let tools = tools
        .as_array()
        .with_context(|| format!("{}: tools is not a list", path.display()))?;
    Ok(tools.clone())
}

/// The names a listing gives its tools, each once.
fn names(listing: &[Value]) -> anyhow::Result<BTreeMap<String, &Value>> {
    let mut out = BTreeMap::new();
    for tool in listing {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .context("a listed tool has no name")?;
        ensure!(
            out.insert(name.to_string(), tool).is_none(),
            "the listing has the tool {name} twice"
        );
    }
    Ok(out)
}

/// Join a listing and its review into a map.
pub fn build(listing: &[Value], review: &Review) -> anyhow::Result<ServerMap> {
    let listed = names(listing)?;
    for name in review.tools.keys() {
        ensure!(
            !review.unmapped.contains_key(name),
            "{name} is both reviewed and set aside"
        );
        ensure!(
            listed.contains_key(name),
            "{name} is reviewed, but the server does not list it"
        );
    }
    for name in review.unmapped.keys() {
        ensure!(
            listed.contains_key(name),
            "{name} is set aside, but the server does not list it"
        );
    }
    let mut tools = Vec::new();
    for (name, definition) in &listed {
        let Some(reviewed) = review.tools.get(name) else {
            ensure!(
                review.unmapped.contains_key(name),
                "the server lists {name}, which nobody has reviewed or set aside"
            );
            continue;
        };
        tools.push(ToolMap {
            results: reviewed.results,
            tool: Id::new(name).map_err(|_| anyhow::anyhow!("{name} is not an identifier"))?,
            digest: tool_digest(definition)
                .map_err(|_| anyhow::anyhow!("the definition of {name} has no canonical form"))?,
            operation: reviewed.operation.clone(),
            kind: reviewed.kind,
            reversal: reviewed.reversal,
            outward: reviewed.outward,
            resource: reviewed.resource.clone(),
        });
    }
    for (name, reason) in &review.unmapped {
        ensure!(
            !reason.trim().is_empty(),
            "{name} is set aside with no reason"
        );
    }
    let map = ServerMap {
        labels: review.labels.clone(),
        clearances: review.clearances.clone(),
        server: review.server.clone(),
        system: review.system.clone(),
        fold_case: review.fold_case,
        tools,
    };
    map.validate()
        .map_err(|e| anyhow::anyhow!("the map does not pass its lint: {e}"))?;
    Ok(map)
}

/// What differs between a server's listing now and a map made from an earlier one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drift {
    /// Listed now, and neither in the map nor set aside.
    pub added: Vec<String>,
    /// In the map, and no longer listed.
    pub removed: Vec<String>,
    /// In both, with a definition that differs in any field.
    pub changed: Vec<String>,
    pub unchanged: usize,
}

impl Drift {
    pub fn none(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Compare `listing` with `map`. `set_aside` are tools the review left out on purpose: they
/// are not new.
pub fn drift(
    listing: &[Value],
    map: &ServerMap,
    set_aside: &BTreeSet<String>,
) -> anyhow::Result<Drift> {
    let listed = names(listing)?;
    let mut out = Drift::default();
    for tool in &map.tools {
        match listed.get(tool.tool.as_str()) {
            None => out.removed.push(tool.tool.to_string()),
            // A definition with no canonical form is not the one that was reviewed.
            Some(definition) => match tool_digest(definition) {
                Ok(digest) if digest == tool.digest => out.unchanged += 1,
                _ => out.changed.push(tool.tool.to_string()),
            },
        }
    }
    for name in listed.keys() {
        let mapped = map.tools.iter().any(|t| t.tool.as_str() == name);
        if !mapped && !set_aside.contains(name) {
            out.added.push(name.clone());
        }
    }
    Ok(out)
}
