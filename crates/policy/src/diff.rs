//! Structural diff between two generated sandbox policies (policy admin, step 3D).
//!
//! Pure and deterministic: the result depends only on the two [`Policy`] values, never on key
//! order, list order or duplicates in a list that OpenShell treats as a set. Each difference is
//! a typed [`Change`]; [`PolicyDiff::render`] writes them as stable text for review.
//!
//! Absent sections are compared as OpenShell reads them: no `filesystem_policy` means
//! `include_workdir: true` and no paths, no `landlock` means `best_effort`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::openshell::{Allow, AllowRule, Endpoint, Policy};
use crate::CONTENT_MOUNT;

/// Which part of the policy a change touches. Declaration order is the order of the rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Version,
    /// A read-only or read-write path that is not a skill.
    Filesystem,
    /// A path under `<CONTENT_MOUNT>/skills/`.
    SkillPath,
    Landlock,
    Process,
    /// A whole `network_policies` entry, its binaries.
    NetworkRule,
    /// An endpoint, or one of its fields or non-tool rules.
    Endpoint,
    /// One tool in the MCP `tools/call` allowlist.
    ToolAllowlist,
    Middleware,
    /// A rule's display `name`, which OpenShell uses only in logs.
    RuleName,
}

impl Area {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Filesystem => "filesystem",
            Self::SkillPath => "skill_path",
            Self::Landlock => "landlock",
            Self::Process => "process",
            Self::NetworkRule => "network_rule",
            Self::Endpoint => "endpoint",
            Self::ToolAllowlist => "tool_allowlist",
            Self::Middleware => "middleware",
            Self::RuleName => "rule_name",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Added,
    Removed,
    Changed,
}

impl Op {
    fn symbol(self) -> char {
        match self {
            Self::Added => '+',
            Self::Removed => '-',
            Self::Changed => '~',
        }
    }

    fn inverse(self) -> Self {
        match self {
            Self::Added => Self::Removed,
            Self::Removed => Self::Added,
            Self::Changed => Self::Changed,
        }
    }
}

/// One difference between two policies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Change {
    pub area: Area,
    pub op: Op,
    /// What changed, as a stable dotted path, such as
    /// `pmos_toolserver:toolserver.pmos.internal:8710/mcp tool docs_read`.
    pub subject: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// The differences between an old and a new policy, sorted and free of duplicates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDiff {
    pub changes: Vec<Change>,
}

impl PolicyDiff {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The diff that turns the new policy back into the old one.
    pub fn inverse(&self) -> PolicyDiff {
        let mut changes: Vec<Change> = self
            .changes
            .iter()
            .map(|c| Change {
                area: c.area,
                op: c.op.inverse(),
                subject: c.subject.clone(),
                before: c.after.clone(),
                after: c.before.clone(),
            })
            .collect();
        changes.sort();
        PolicyDiff { changes }
    }

    /// Stable text for review, one line per change:
    /// `+ [tool_allowlist] <subject>`, `- ...`, or `~ [area] <subject>: <before> -> <after>`.
    /// An empty diff renders as `no changes`.
    pub fn render(&self) -> String {
        if self.changes.is_empty() {
            return "no changes\n".into();
        }
        let mut out = String::new();
        for c in &self.changes {
            let _ = write!(out, "{} [{}] {}", c.op.symbol(), c.area.as_str(), c.subject);
            match (&c.before, &c.after) {
                (Some(b), Some(a)) => {
                    let _ = write!(out, ": {b} -> {a}");
                }
                (None, Some(a)) => {
                    let _ = write!(out, ": {a}");
                }
                (Some(b), None) => {
                    let _ = write!(out, ": {b}");
                }
                (None, None) => {}
            }
            out.push('\n');
        }
        out
    }
}

/// Compare two policies.
pub fn diff(old: &Policy, new: &Policy) -> PolicyDiff {
    let mut out = Vec::new();
    if old.version != new.version {
        out.push(changed(
            Area::Version,
            "version",
            old.version.to_string(),
            new.version.to_string(),
        ));
    }
    diff_filesystem(old, new, &mut out);
    diff_process(old, new, &mut out);
    diff_network(old, new, &mut out);
    diff_middlewares(old, new, &mut out);
    out.sort();
    out.dedup();
    PolicyDiff { changes: out }
}

fn changed(area: Area, subject: &str, before: String, after: String) -> Change {
    Change {
        area,
        op: Op::Changed,
        subject: subject.into(),
        before: Some(before),
        after: Some(after),
    }
}

fn set_changes(
    area_of: impl Fn(&str) -> Area,
    prefix: &str,
    old: &BTreeSet<String>,
    new: &BTreeSet<String>,
    out: &mut Vec<Change>,
) {
    for v in new.difference(old) {
        out.push(Change {
            area: area_of(v),
            op: Op::Added,
            subject: format!("{prefix}{v}"),
            before: None,
            after: None,
        });
    }
    for v in old.difference(new) {
        out.push(Change {
            area: area_of(v),
            op: Op::Removed,
            subject: format!("{prefix}{v}"),
            before: None,
            after: None,
        });
    }
}

fn path_area(path: &str) -> Area {
    let skills = format!("{CONTENT_MOUNT}/skills/");
    if path.starts_with(&skills) {
        Area::SkillPath
    } else {
        Area::Filesystem
    }
}

fn diff_filesystem(old: &Policy, new: &Policy, out: &mut Vec<Change>) {
    let view = |p: &Policy| {
        let fs = p.filesystem_policy.clone();
        let (workdir, ro, rw) = match fs {
            Some(f) => (f.include_workdir, f.read_only, f.read_write),
            None => (true, Vec::new(), Vec::new()),
        };
        let landlock = p
            .landlock
            .as_ref()
            .map_or("best_effort".to_string(), |l| l.compatibility.clone());
        (
            workdir,
            ro.into_iter().collect::<BTreeSet<_>>(),
            rw.into_iter().collect::<BTreeSet<_>>(),
            landlock,
        )
    };
    let (ow, oro, orw, ol) = view(old);
    let (nw, nro, nrw, nl) = view(new);
    if ow != nw {
        out.push(changed(
            Area::Filesystem,
            "filesystem_policy.include_workdir",
            ow.to_string(),
            nw.to_string(),
        ));
    }
    set_changes(path_area, "read_only ", &oro, &nro, out);
    set_changes(path_area, "read_write ", &orw, &nrw, out);
    if ol != nl {
        out.push(changed(Area::Landlock, "landlock.compatibility", ol, nl));
    }
}

fn diff_process(old: &Policy, new: &Policy, out: &mut Vec<Change>) {
    let view = |p: &Policy| {
        p.process
            .as_ref()
            .map(|x| (x.run_as_user.clone(), x.run_as_group.clone()))
            .unwrap_or((None, None))
    };
    let show = |v: Option<String>| v.unwrap_or_else(|| "(unset)".into());
    let (ou, og) = view(old);
    let (nu, ng) = view(new);
    if ou != nu {
        out.push(changed(Area::Process, "process.run_as_user", show(ou), show(nu)));
    }
    if og != ng {
        out.push(changed(Area::Process, "process.run_as_group", show(og), show(ng)));
    }
}

fn diff_middlewares(old: &Policy, new: &Policy, out: &mut Vec<Change>) {
    let keys: BTreeSet<&String> = old
        .network_middlewares
        .keys()
        .chain(new.network_middlewares.keys())
        .collect();
    for k in keys {
        let subject = format!("network_middlewares.{k}");
        match (old.network_middlewares.get(k), new.network_middlewares.get(k)) {
            (None, Some(_)) => out.push(Change {
                area: Area::Middleware,
                op: Op::Added,
                subject,
                before: None,
                after: None,
            }),
            (Some(_), None) => out.push(Change {
                area: Area::Middleware,
                op: Op::Removed,
                subject,
                before: None,
                after: None,
            }),
            (Some(a), Some(b)) if a != b => out.push(Change {
                area: Area::Middleware,
                op: Op::Changed,
                subject,
                before: None,
                after: None,
            }),
            _ => {}
        }
    }
}

/// An endpoint's identity: where it points. Duplicates get a `#n` suffix so none is hidden.
fn endpoint_keys(endpoints: &[Endpoint]) -> BTreeMap<String, &Endpoint> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for e in endpoints {
        let host = e.host.as_deref().unwrap_or("(any)");
        let mut ports: Vec<u16> = e.port.into_iter().chain(e.ports.iter().copied()).collect();
        ports.sort_unstable();
        ports.dedup();
        let ports = ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let base = format!("{host}:{ports}{}", e.path.as_deref().unwrap_or(""));
        let n = seen.entry(base.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 { base } else { format!("{base}#{n}") };
        out.insert(key, e);
    }
    out
}

fn compact(v: &impl Serialize) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "(unserializable)".into())
}

/// The tool names of a plain `tools/call` rule: only `method` and `tool` set.
fn tool_rule(rule: &AllowRule) -> Option<Vec<String>> {
    let a: &Allow = &rule.allow;
    if a.method.as_deref() != Some("tools/call") {
        return None;
    }
    let tool = a.tool.as_ref()?;
    let plain = Allow {
        method: a.method.clone(),
        tool: a.tool.clone(),
        ..Allow::default()
    };
    (plain == *a).then(|| tool.patterns().iter().map(|s| s.to_string()).collect())
}

/// Split an endpoint's rules into the tool allowlist and everything else (as JSON text).
fn split_rules(e: &Endpoint) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut tools = BTreeSet::new();
    let mut other = BTreeSet::new();
    for r in &e.rules {
        match tool_rule(r) {
            Some(names) => tools.extend(names),
            None => {
                other.insert(compact(r));
            }
        }
    }
    (tools, other)
}

fn summary(e: &Endpoint) -> String {
    let mut j = serde_json::to_value(e).unwrap_or(Json::Null);
    if let Some(m) = j.as_object_mut() {
        m.remove("host");
        m.remove("port");
        m.remove("ports");
        m.remove("path");
    }
    compact(&j)
}

fn diff_network(old: &Policy, new: &Policy, out: &mut Vec<Change>) {
    let keys: BTreeSet<&String> = old
        .network_policies
        .keys()
        .chain(new.network_policies.keys())
        .collect();
    for k in keys {
        match (old.network_policies.get(k), new.network_policies.get(k)) {
            (None, Some(r)) => out.push(Change {
                area: Area::NetworkRule,
                op: Op::Added,
                subject: k.clone(),
                before: None,
                after: Some(format!(
                    "endpoints [{}]",
                    endpoint_keys(&r.endpoints)
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }),
            (Some(r), None) => out.push(Change {
                area: Area::NetworkRule,
                op: Op::Removed,
                subject: k.clone(),
                before: Some(format!(
                    "endpoints [{}]",
                    endpoint_keys(&r.endpoints)
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                after: None,
            }),
            (Some(a), Some(b)) => {
                if a.name != b.name {
                    out.push(changed(
                        Area::RuleName,
                        &format!("{k}.name"),
                        a.name.clone().unwrap_or_else(|| "(unset)".into()),
                        b.name.clone().unwrap_or_else(|| "(unset)".into()),
                    ));
                }
                let bins = |r: &crate::openshell::NetworkRule| {
                    r.binaries.iter().map(|x| x.path.clone()).collect()
                };
                set_changes(
                    |_| Area::NetworkRule,
                    &format!("{k}.binaries "),
                    &bins(a),
                    &bins(b),
                    out,
                );
                diff_endpoints(k, a, b, out);
            }
            (None, None) => {}
        }
    }
}

fn diff_endpoints(
    rule: &str,
    a: &crate::openshell::NetworkRule,
    b: &crate::openshell::NetworkRule,
    out: &mut Vec<Change>,
) {
    let (ea, eb) = (endpoint_keys(&a.endpoints), endpoint_keys(&b.endpoints));
    let keys: BTreeSet<&String> = ea.keys().chain(eb.keys()).collect();
    for k in keys {
        let subject = format!("{rule}:{k}");
        match (ea.get(k), eb.get(k)) {
            (None, Some(e)) => out.push(Change {
                area: Area::Endpoint,
                op: Op::Added,
                subject,
                before: None,
                after: Some(summary(e)),
            }),
            (Some(e), None) => out.push(Change {
                area: Area::Endpoint,
                op: Op::Removed,
                subject,
                before: Some(summary(e)),
                after: None,
            }),
            (Some(x), Some(y)) => diff_one_endpoint(&subject, x, y, out),
            (None, None) => {}
        }
    }
}

fn diff_one_endpoint(subject: &str, x: &Endpoint, y: &Endpoint, out: &mut Vec<Change>) {
    // Field by field, except `rules`, which are split into tools and other rules below.
    let field_map = |e: &Endpoint| match serde_json::to_value(e) {
        Ok(Json::Object(m)) => m,
        _ => serde_json::Map::new(),
    };
    let (mx, my) = (field_map(x), field_map(y));
    let names: BTreeSet<&String> = mx.keys().chain(my.keys()).collect();
    for name in names {
        if matches!(name.as_str(), "rules" | "host" | "port" | "ports" | "path") {
            continue;
        }
        let (vx, vy) = (mx.get(name), my.get(name));
        if vx != vy {
            out.push(Change {
                area: Area::Endpoint,
                op: Op::Changed,
                subject: format!("{subject}.{name}"),
                before: Some(vx.map_or("(unset)".into(), compact)),
                after: Some(vy.map_or("(unset)".into(), compact)),
            });
        }
    }
    let (tx, ox) = split_rules(x);
    let (ty, oy) = split_rules(y);
    set_changes(
        |_| Area::ToolAllowlist,
        &format!("{subject} tool "),
        &tx,
        &ty,
        out,
    );
    set_changes(
        |_| Area::Endpoint,
        &format!("{subject}.rules "),
        &ox,
        &oy,
        out,
    );
}
