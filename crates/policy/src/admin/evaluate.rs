//! The Policy Admin's logic (`docs/policy-admin.md`): what each bot's NVIDIA OpenShell sandbox
//! is allowed, where that comes from, whether the checked-in files are current, and what a
//! proposed change would widen or narrow.
//!
//! **What a policy is made from.** `pmos policy generate` (`crates/policy`) derives every bot's
//! policy from the content tree, never from a hand-written file:
//!
//! - a bot (`content/bots/<bot>/BOT.md`): its `role`, `scope` and `owner` (a personal bot also
//!   reads its owner's `master/people/<owner>/AGENTS.md`, when there is one);
//! - a role (`content/roles/<role>/ROLE.md`): its `skills`, each uploaded read-only;
//! - a skill (`content/skills/<skill>/SKILL.md`): its `metadata.tools`, the only tools
//!   `tools/call` may name.
//!
//! Those inputs are the [`PolicySpec`] the admin edits. Everything else (the three hosts and
//! ports, the REST routes, the MCP methods and revisions, the worker binary, the system paths,
//! the non-root identity, Landlock) is fixed in `crates/policy/src/lib.rs` and changes only
//! with code; [`fixed`] shows it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::generate::{from_report, BotContext, Generated};
use crate::openshell::{self, Policy};
use crate::spec::{self, SandboxSpec};
use crate::{check_policy, check_spec, Service};
use chrono::NaiveDate;
use pmos_contract::bots::{valid_name, BotScope};
use pmos_contract::{lint, LintOptions, Report};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::proposals::{self, repo_path, Change, Issue};

/// The policy inputs, by name: what the admin may change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PolicySpec {
    pub bots: BTreeMap<String, BotInputs>,
    pub roles: BTreeMap<String, RoleInputs>,
    pub skills: BTreeMap<String, SkillInputs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BotInputs {
    pub role: String,
    /// `team` or `personal`.
    pub scope: String,
    /// A handle, without `@` (`TODO` while unassigned).
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleInputs {
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillInputs {
    /// Registry names (`docs.search`).
    pub tools: Vec<String>,
}

/// A proposed spec: any subset of [`PolicySpec`]; what is left out stays as it is.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecPatch {
    #[serde(default)]
    pub bots: BTreeMap<String, BotPatch>,
    #[serde(default)]
    pub roles: BTreeMap<String, RolePatch>,
    #[serde(default)]
    pub skills: BTreeMap<String, SkillPatch>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotPatch {
    pub role: Option<String>,
    pub scope: Option<String>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolePatch {
    pub skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPatch {
    pub tools: Option<Vec<String>>,
}

/// The most names one list in a spec may hold.
pub const MAX_LIST: usize = 100;

fn scope_name(s: BotScope) -> &'static str {
    match s {
        BotScope::Team => "team",
        BotScope::Personal => "personal",
    }
}

/// The spec as the content has it now.
pub fn current_spec(report: &Report) -> PolicySpec {
    PolicySpec {
        bots: report
            .bots
            .iter()
            .map(|b| {
                (
                    b.name.clone(),
                    BotInputs {
                        role: b.role.clone(),
                        scope: scope_name(b.scope).into(),
                        owner: b.owner.clone(),
                    },
                )
            })
            .collect(),
        roles: report
            .roles
            .iter()
            .map(|r| {
                (
                    r.name.clone(),
                    RoleInputs {
                        skills: r.skills.clone(),
                    },
                )
            })
            .collect(),
        skills: report
            .skills
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    SkillInputs {
                        tools: s.tools.clone(),
                    },
                )
            })
            .collect(),
    }
}

/// The fixed part of every policy, from `crates/policy/src/lib.rs`: not editable here.
pub fn fixed() -> Value {
    let services: Vec<Value> = Service::ALL
        .into_iter()
        .map(|s| {
            json!({
                "service": service_name(s),
                "rule": s.rule_name(),
                "host": s.host(),
                "port": s.port(),
                "protocol": s.protocol(),
                "routes": s.routes().map(|r| r.iter().map(|(m, p)| format!("{m} {p}")).collect::<Vec<_>>()),
                "credential_profile": s.credential().map(|c| c.profile),
            })
        })
        .collect();
    json!({
        "services": services,
        "mcp_path": crate::TOOLSERVER_MCP_PATH,
        "mcp_methods": crate::MCP_METHODS,
        "mcp_versions": crate::MCP_VERSIONS,
        "worker_binary": crate::WORKER_BINARY,
        "workspace": crate::WORKSPACE,
        "content_mount": crate::CONTENT_MOUNT,
        "shared_content": crate::SHARED_CONTENT_DIRS,
        "system_read_only": crate::SYSTEM_READ_ONLY,
        "system_read_write": crate::SYSTEM_READ_WRITE,
        "run_as": crate::RUN_AS,
        "landlock": "hard_requirement",
        "source": "crates/policy/src/lib.rs",
    })
}

fn service_name(s: Service) -> &'static str {
    match s {
        Service::ToolServer => "toolserver",
        Service::Gateway => "gateway",
        Service::Runtime => "runtime",
    }
}

// Allowances.

/// What one network rule lets the sandbox do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkAllowance {
    /// The `network_policies` key (`pmos_gateway`).
    pub rule: String,
    /// `toolserver`, `gateway`, `runtime`, or `null` for a destination PM OS doesn't know.
    pub service: Option<&'static str>,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// Extra ports, and IP ranges: always empty in a generated policy.
    pub ports: Vec<u16>,
    pub allowed_ips: Vec<String>,
    pub protocol: Option<String>,
    pub enforcement: Option<String>,
    pub tls: Option<String>,
    pub access: Option<String>,
    pub binaries: Vec<String>,
    /// The provider instance bound to this endpoint, if any.
    pub credential: Option<String>,
    /// REST: `METHOD path` rules.
    pub routes: Vec<String>,
    /// MCP: the methods allowed, and the tools `tools/call` may name.
    pub mcp_methods: Vec<String>,
    pub mcp_tools: Vec<String>,
    pub mcp_versions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Allowances {
    pub network: Vec<NetworkAllowance>,
    pub network_middlewares: Vec<String>,
    pub filesystem: FsAllowance,
    pub landlock: Option<String>,
    pub process: ProcessAllowance,
    /// The launch spec's `--provider` attachments (`null` fields when there is no launch spec).
    pub providers: Option<Vec<String>>,
    /// The launch spec's read-only uploads, `from -> to`.
    pub uploads: Option<Vec<String>>,
    /// The same, in sentences.
    pub summary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsAllowance {
    pub include_workdir: bool,
    pub read_only: Vec<String>,
    pub read_write: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProcessAllowance {
    pub run_as_user: Option<String>,
    pub run_as_group: Option<String>,
}

/// What `policy` (and its launch spec) allow, read field by field from the files.
pub fn allowances(policy: &Policy, launch: Option<&SandboxSpec>) -> Allowances {
    let mut network = Vec::new();
    for (key, rule) in &policy.network_policies {
        let binaries: Vec<String> = rule.binaries.iter().map(|b| b.path.clone()).collect();
        for e in &rule.endpoints {
            let mut routes = Vec::new();
            let mut methods = Vec::new();
            let mut tools = Vec::new();
            let mcp = e.protocol.as_deref() == Some("mcp");
            for r in &e.rules {
                let a = &r.allow;
                if mcp {
                    if let Some(m) = &a.method {
                        methods.push(m.clone());
                    }
                    if let Some(t) = &a.tool {
                        tools.extend(t.patterns().into_iter().map(String::from));
                    }
                } else {
                    routes.push(format!(
                        "{} {}",
                        a.method.as_deref().unwrap_or("*"),
                        a.path.as_deref().unwrap_or("*")
                    ));
                }
            }
            tools.sort();
            tools.dedup();
            network.push(NetworkAllowance {
                rule: key.clone(),
                service: match (&e.host, e.port) {
                    (Some(h), Some(p)) => Service::at(h, p).map(service_name),
                    _ => None,
                },
                host: e.host.clone(),
                port: e.port,
                ports: e.ports.clone(),
                allowed_ips: e.allowed_ips.clone(),
                protocol: e.protocol.clone(),
                enforcement: e.enforcement.clone(),
                tls: e.tls.clone(),
                access: e.access.clone(),
                binaries: binaries.clone(),
                credential: e.credential_binding.as_ref().map(|b| b.provider.clone()),
                routes,
                mcp_methods: methods,
                mcp_tools: tools,
                mcp_versions: e
                    .mcp
                    .as_ref()
                    .and_then(|m| m.versions.clone())
                    .unwrap_or_default(),
            });
        }
    }
    let fs = policy.filesystem_policy.clone().unwrap_or_default();
    let process = policy.process.clone().unwrap_or_default();
    let mut a = Allowances {
        network,
        network_middlewares: policy.network_middlewares.keys().cloned().collect(),
        filesystem: FsAllowance {
            include_workdir: fs.include_workdir,
            read_only: fs.read_only,
            read_write: fs.read_write,
        },
        landlock: policy.landlock.as_ref().map(|l| l.compatibility.clone()),
        process: ProcessAllowance {
            run_as_user: process.run_as_user,
            run_as_group: process.run_as_group,
        },
        providers: launch.map(|s| s.providers.iter().map(|p| p.name.clone()).collect()),
        uploads: launch.map(|s| {
            s.uploads
                .iter()
                .map(|u| format!("{} -> {}", u.from, u.to))
                .collect()
        }),
        summary: Vec::new(),
    };
    a.summary = summary(&a);
    a
}

fn summary(a: &Allowances) -> Vec<String> {
    let mut out = Vec::new();
    for n in &a.network {
        let dest = format!(
            "{}:{}",
            n.host.as_deref().unwrap_or("?"),
            n.port.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
        );
        let what = match n.service {
            Some("toolserver") => "the tool server",
            Some("gateway") => "the model gateway",
            Some("runtime") => "the bot runtime",
            _ => "an unknown destination",
        };
        let mut line = format!(
            "May connect to {what} at {dest} ({}, {}) from {}",
            n.protocol.as_deref().unwrap_or("no inspection"),
            n.enforcement.as_deref().unwrap_or("audit only"),
            if n.binaries.is_empty() {
                "no binary".to_string()
            } else {
                n.binaries.join(", ")
            }
        );
        if !n.routes.is_empty() {
            line.push_str(&format!(": {}", n.routes.join(", ")));
        }
        if !n.mcp_methods.is_empty() {
            line.push_str(&format!(": MCP {}", n.mcp_methods.join(", ")));
        }
        line.push('.');
        out.push(line);
        if n.protocol.as_deref() == Some("mcp") {
            out.push(if n.mcp_tools.is_empty() {
                "May call no tools.".to_string()
            } else {
                format!("May call these tools: {}.", n.mcp_tools.join(", "))
            });
        }
        if let Some(c) = &n.credential {
            out.push(format!("Uses the credential {c} on {dest}."));
        }
        if !n.allowed_ips.is_empty() || !n.ports.is_empty() {
            out.push(format!("Also allows extra ports or IP ranges on {dest}."));
        }
    }
    if !a.network_middlewares.is_empty() {
        out.push(format!(
            "Network middleware: {}.",
            a.network_middlewares.join(", ")
        ));
    }
    let content: Vec<&str> = a
        .filesystem
        .read_only
        .iter()
        .filter(|p| p.starts_with(crate::CONTENT_MOUNT))
        .map(String::as_str)
        .collect();
    out.push(format!(
        "May read this content: {}.",
        if content.is_empty() {
            "none".to_string()
        } else {
            content.join(", ")
        }
    ));
    out.push(format!(
        "May write only: {}{}.",
        a.filesystem.read_write.join(", "),
        if a.filesystem.include_workdir {
            ", and its working directory"
        } else {
            ""
        }
    ));
    out.push(format!(
        "Runs as {}:{}; Landlock {}.",
        a.process.run_as_user.as_deref().unwrap_or("(default user)"),
        a.process
            .run_as_group
            .as_deref()
            .unwrap_or("(default group)"),
        a.landlock.as_deref().unwrap_or("best_effort (default)")
    ));
    out
}

/// One allowance as a comparable item: `(kind, key, value)`. Sets compare by presence
/// (added: wider; removed: narrower); `value`s of scalar kinds compare by equality (changed).
type Atom = (&'static str, String, String);

/// Scalar kinds: any change is flagged as widening for review.
const SCALAR_KINDS: [&str; 8] = [
    "network.protocol",
    "network.enforcement",
    "network.tls",
    "network.access",
    "network.credential",
    "filesystem.include_workdir",
    "landlock",
    "process",
];

fn atoms(a: &Allowances) -> BTreeSet<Atom> {
    let mut out = BTreeSet::new();
    let mut put = |kind: &'static str, key: String, value: String| {
        out.insert((kind, key, value));
    };
    for n in &a.network {
        let dest = format!(
            "{}:{}",
            n.host.as_deref().unwrap_or("*"),
            n.port.map(|p| p.to_string()).unwrap_or_else(|| "*".into())
        );
        put("network.destination", dest.clone(), String::new());
        for p in &n.ports {
            put(
                "network.destination",
                format!("{dest} port {p}"),
                String::new(),
            );
        }
        for ip in &n.allowed_ips {
            put(
                "network.destination",
                format!("{dest} ip {ip}"),
                String::new(),
            );
        }
        for b in &n.binaries {
            put("network.binary", format!("{dest} {b}"), String::new());
        }
        for r in &n.routes {
            put("network.route", format!("{dest} {r}"), String::new());
        }
        for m in &n.mcp_methods {
            put("network.mcp_method", format!("{dest} {m}"), String::new());
        }
        for t in &n.mcp_tools {
            put("network.mcp_tool", t.clone(), String::new());
        }
        for v in &n.mcp_versions {
            put("network.mcp_version", format!("{dest} {v}"), String::new());
        }
        let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "(none)".into());
        put("network.protocol", dest.clone(), opt(&n.protocol));
        put("network.enforcement", dest.clone(), opt(&n.enforcement));
        put("network.tls", dest.clone(), opt(&n.tls));
        put("network.access", dest.clone(), opt(&n.access));
        put("network.credential", dest, opt(&n.credential));
    }
    for m in &a.network_middlewares {
        put("network.middleware", m.clone(), String::new());
    }
    for p in &a.filesystem.read_only {
        put("filesystem.read_only", p.clone(), String::new());
    }
    for p in &a.filesystem.read_write {
        put("filesystem.read_write", p.clone(), String::new());
    }
    put(
        "filesystem.include_workdir",
        String::new(),
        a.filesystem.include_workdir.to_string(),
    );
    put(
        "landlock",
        String::new(),
        a.landlock.clone().unwrap_or_else(|| "best_effort".into()),
    );
    put(
        "process",
        String::new(),
        format!(
            "{}:{}",
            a.process.run_as_user.as_deref().unwrap_or("(default)"),
            a.process.run_as_group.as_deref().unwrap_or("(default)")
        ),
    );
    for p in a.providers.iter().flatten() {
        put("credential.provider", p.clone(), String::new());
    }
    for u in a.uploads.iter().flatten() {
        put("upload", u.clone(), String::new());
    }
    out
}

/// One difference in what a bot may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AllowanceChange {
    pub bot: String,
    /// `network.destination`, `network.route`, `network.mcp_tool`, `filesystem.read_only`, …
    pub kind: &'static str,
    /// What it is about (a destination, a path, a tool); empty for a bot-wide setting.
    pub item: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// `wider` (something added), `narrower` (removed) or `changed` (a setting changed).
    pub direction: &'static str,
    /// `true` for `wider` and `changed`: review it as a widening.
    pub widening: bool,
    pub description: String,
}

/// Each bot's allowances before and after; a bot on one side only was added or removed.
pub fn diff_allowances(
    before: &BTreeMap<String, Allowances>,
    after: &BTreeMap<String, Allowances>,
) -> Vec<AllowanceChange> {
    let empty = BTreeSet::new();
    let mut out = Vec::new();
    let bots: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for bot in bots {
        let a = before.get(bot).map(atoms);
        let b = after.get(bot).map(atoms);
        let (a, b) = (a.as_ref().unwrap_or(&empty), b.as_ref().unwrap_or(&empty));
        let scalar = |k: &str| SCALAR_KINDS.contains(&k);
        // Scalars by (kind, key).
        let a_sc: BTreeMap<(&str, &String), &String> = a
            .iter()
            .filter(|x| scalar(x.0))
            .map(|(k, key, v)| ((*k, key), v))
            .collect();
        let b_sc: BTreeMap<(&str, &String), &String> = b
            .iter()
            .filter(|x| scalar(x.0))
            .map(|(k, key, v)| ((*k, key), v))
            .collect();
        let keys: BTreeSet<&(&str, &String)> = a_sc.keys().chain(b_sc.keys()).collect();
        for key in keys {
            let (old, new) = (a_sc.get(key), b_sc.get(key));
            if old == new {
                continue;
            }
            // A setting that appears or goes with its destination is covered by the
            // destination's own change.
            let (Some(old), Some(new)) = (old, new) else {
                continue;
            };
            out.push(AllowanceChange {
                bot: bot.clone(),
                kind: key.0,
                item: key.1.clone(),
                before: Some((*old).clone()),
                after: Some((*new).clone()),
                direction: "changed",
                widening: true,
                description: format!(
                    "{} {}changes from {old} to {new}",
                    key.0,
                    if key.1.is_empty() {
                        String::new()
                    } else {
                        format!("on {} ", key.1)
                    }
                ),
            });
        }
        for (kind, item, _) in b.difference(a).filter(|x| !scalar(x.0)) {
            out.push(set_change(bot, kind, item, true));
        }
        for (kind, item, _) in a.difference(b).filter(|x| !scalar(x.0)) {
            out.push(set_change(bot, kind, item, false));
        }
    }
    out
}

fn set_change(bot: &str, kind: &'static str, item: &str, added: bool) -> AllowanceChange {
    let verb = if added { "may now" } else { "may no longer" };
    let what = match kind {
        "network.destination" => format!("connect to {item}"),
        "network.binary" => format!("open connections with {item}"),
        "network.route" => format!("call {item}"),
        "network.mcp_method" => format!("send the MCP method {item}"),
        "network.mcp_tool" => format!("call the tool {item}"),
        "network.mcp_version" => format!("use MCP revision {item}"),
        "network.middleware" => format!("go through the middleware {item}"),
        "filesystem.read_only" => format!("read {item}"),
        "filesystem.read_write" => format!("write {item}"),
        "credential.provider" => format!("have the provider {item} attached"),
        "upload" => format!("get {item} uploaded"),
        other => format!("{other} {item}"),
    };
    AllowanceChange {
        bot: bot.to_string(),
        kind,
        item: item.to_string(),
        before: (!added).then(|| item.to_string()),
        after: added.then(|| item.to_string()),
        direction: if added { "wider" } else { "narrower" },
        widening: added,
        description: format!("{bot} {verb} {what}"),
    }
}

// Reading the current state.

/// One generated file and its checked-in copy.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyFile {
    /// Repository path (`policies/bots/docs.yaml`).
    pub path: String,
    /// `same`, `differs`, `missing` (generated, not checked in), `orphan` (checked in, no
    /// longer generated) or `unknown` (no checked-in policies to compare with).
    pub state: &'static str,
    pub generated: Option<String>,
    pub checked_in: Option<String>,
}

/// The content and policies as they are now: the lint report, what generation gives, and the
/// checked-in files (relative to the policies root).
pub struct Current {
    pub report: Report,
    pub generated: Result<Generated, Vec<pmos_contract::Diagnostic>>,
    /// `None` when the server has no policies root.
    pub checked_in: Option<BTreeMap<String, String>>,
}

pub fn today_report(root: &Path, today: NaiveDate) -> Report {
    lint(root, &LintOptions::new(today))
}

/// Generate from a lint report, refusing a report with errors as `pmos policy generate` does.
pub fn generate(report: &Report, root: &Path) -> Result<Generated, Vec<pmos_contract::Diagnostic>> {
    let errors: Vec<_> = report.errors().cloned().collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    from_report(report, root)
}

/// The `.yaml` files in `bots/` and `providers/` under `root`, with their text (symlinks
/// and unreadable files left out).
pub fn checked_in(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for dir in ["bots", "providers"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_file = e.file_type().map(|t| t.is_file()).unwrap_or(false);
            if !is_file || !name.ends_with(".yaml") || name.starts_with('.') {
                continue;
            }
            let rel = format!("{dir}/{name}");
            if let Ok(Some(text)) = proposals::read_text(root, &rel) {
                out.insert(rel, text);
            }
        }
    }
    out
}

pub fn current(content_root: &Path, policies_root: Option<&Path>, today: NaiveDate) -> Current {
    let report = today_report(content_root, today);
    let generated = generate(&report, content_root);
    Current {
        report,
        generated,
        checked_in: policies_root.map(checked_in),
    }
}

/// Every generated or checked-in file, with its state.
pub fn files(
    generated: Option<&Generated>,
    checked: Option<&BTreeMap<String, String>>,
    policies_dir: &str,
) -> Vec<PolicyFile> {
    let empty = BTreeMap::new();
    let gen = generated.map(|g| &g.files).unwrap_or(&empty);
    let mut paths: BTreeSet<&String> = gen.keys().collect();
    if let Some(c) = checked {
        paths.extend(c.keys());
    }
    paths
        .into_iter()
        .map(|rel| {
            let g = gen.get(rel);
            let c = checked.and_then(|c| c.get(rel));
            let state = match (checked.is_some(), g, c) {
                (false, _, _) => "unknown",
                (true, Some(g), Some(c)) if g == c => "same",
                (true, Some(_), Some(_)) => "differs",
                (true, Some(_), None) => "missing",
                (true, None, _) => "orphan",
            };
            PolicyFile {
                path: repo_path(policies_dir, rel),
                state,
                generated: g.cloned(),
                checked_in: c.cloned(),
            }
        })
        .collect()
}

/// Each bot's allowances from `files` (relative path to text): the policy and launch spec that
/// parse. A bot whose policy doesn't parse is left out.
pub fn allowances_of(files: &BTreeMap<String, String>) -> BTreeMap<String, Allowances> {
    let mut out = BTreeMap::new();
    for (rel, text) in files {
        let Some(bot) = rel
            .strip_prefix("bots/")
            .and_then(|r| r.strip_suffix(".yaml"))
            .filter(|b| !b.ends_with(".sandbox"))
        else {
            continue;
        };
        let Ok(policy) = openshell::parse(text) else {
            continue;
        };
        let launch = files
            .get(&format!("bots/{bot}.sandbox.yaml"))
            .and_then(|t| spec::parse(t).ok());
        out.insert(bot.to_string(), allowances(&policy, launch.as_ref()));
    }
    out
}

/// The files a reader should take as "now": the checked-in ones when the server has them,
/// else what the current content generates.
pub fn now_files(cur: &Current) -> BTreeMap<String, String> {
    match (&cur.checked_in, &cur.generated) {
        (Some(c), _) => c.clone(),
        (None, Ok(g)) => g.files.clone(),
        (None, Err(_)) => BTreeMap::new(),
    }
}

/// Every PM OS rule checked on `files` for each bot in `generated` (the crate's
/// `check_policy` and `check_spec`), at repository paths.
pub fn rule_violations(
    generated: &Generated,
    files: &BTreeMap<String, String>,
    policies_dir: &str,
) -> Vec<Issue> {
    let mut out = Vec::new();
    for ctx in &generated.bots {
        let rel = format!("bots/{}.yaml", ctx.bot);
        let spec_rel = format!("bots/{}.sandbox.yaml", ctx.bot);
        let label = repo_path(policies_dir, &rel);
        let spec_label = repo_path(policies_dir, &spec_rel);
        let parsed = files.get(&rel).and_then(|t| openshell::parse(t).ok());
        if let Some(text) = files.get(&rel) {
            out.extend(check_policy(ctx, &label, text).into_iter().map(violation));
        }
        if let Some(text) = files.get(&spec_rel) {
            out.extend(
                check_spec(ctx, &spec_label, text, parsed.as_ref())
                    .into_iter()
                    .map(violation),
            );
        }
    }
    out
}

fn violation(v: crate::Violation) -> Issue {
    Issue::error(v.rule, v.path, v.message)
}

/// A bot's facts for the policy screen.
pub fn bot_facts(ctx: &BotContext) -> Value {
    json!({
        "bot": ctx.bot,
        "role": ctx.role,
        "scope": ctx.scope_name(),
        "sandbox": crate::sandbox_name(&ctx.bot),
        "skills": ctx.skills,
        "tools": ctx.mcp_tools,
        "master": ctx.master,
    })
}

// Turning a proposed spec into content changes.

fn valid_owner(o: &str) -> bool {
    let o = o.trim_start_matches('@');
    !o.is_empty()
        && o.len() <= 64
        && o.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_skill_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_tool_name(t: &str) -> bool {
    t.len() <= 64
        && t.split_once('.').is_some_and(|(sys, rest)| {
            !sys.is_empty()
                && !rest.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.'))
        })
}

fn check_list(
    issues: &mut Vec<Issue>,
    path: &str,
    items: &[String],
    valid: fn(&str) -> bool,
    what: &str,
) {
    if items.len() > MAX_LIST {
        issues.push(Issue::error(
            "spec-too-long",
            path,
            format!("At most {MAX_LIST} {what}s."),
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, item) in items.iter().enumerate() {
        if !valid(item) {
            issues.push(Issue::error(
                "spec-bad-name",
                format!("{path}[{i}]"),
                format!("`{item}` is not a valid {what} name."),
            ));
        } else if !seen.insert(item.as_str()) {
            issues.push(Issue::error(
                "spec-duplicate",
                format!("{path}[{i}]"),
                format!("`{item}` is listed twice."),
            ));
        }
    }
}

/// The content changes `patch` makes, or every problem with it. Values equal to the current
/// ones change nothing.
pub fn spec_changes(
    content_root: &Path,
    report: &Report,
    patch: &SpecPatch,
) -> Result<Vec<Change>, Vec<Issue>> {
    let mut issues = Vec::new();
    // (content-relative file, [(key path, value)])
    let mut edits: BTreeMap<String, Vec<(Vec<&'static str>, String)>> = BTreeMap::new();
    for (name, p) in &patch.bots {
        let at = format!("spec.bots.{name}");
        let Some(bot) = report.bots.iter().find(|b| &b.name == name) else {
            issues.push(Issue::error(
                "spec-unknown-bot",
                &at,
                format!("There's no bot `{name}`. New bots are added as a content proposal."),
            ));
            continue;
        };
        let file = format!("bots/{name}/BOT.md");
        if let Some(role) = &p.role {
            if !valid_name(role) {
                issues.push(Issue::error(
                    "spec-bad-name",
                    format!("{at}.role"),
                    "A role name is lowercase letters, digits and single hyphens.",
                ));
            } else if role != &bot.role {
                edits
                    .entry(file.clone())
                    .or_default()
                    .push((vec!["role"], role.clone()));
            }
        }
        if let Some(scope) = &p.scope {
            if scope != "team" && scope != "personal" {
                issues.push(Issue::error(
                    "spec-bad-value",
                    format!("{at}.scope"),
                    "`scope` is `team` or `personal`.",
                ));
            } else if scope != scope_name(bot.scope) {
                edits
                    .entry(file.clone())
                    .or_default()
                    .push((vec!["scope"], scope.clone()));
            }
        }
        if let Some(owner) = &p.owner {
            if !valid_owner(owner) {
                issues.push(Issue::error(
                    "spec-bad-value",
                    format!("{at}.owner"),
                    "`owner` is a handle (letters, digits, `.`, `_`, `-`).",
                ));
            } else if owner.trim_start_matches('@') != bot.owner {
                edits
                    .entry(file.clone())
                    .or_default()
                    .push((vec!["owner"], owner.trim_start_matches('@').to_string()));
            }
        }
    }
    for (name, p) in &patch.roles {
        let at = format!("spec.roles.{name}");
        let Some(role) = report.roles.iter().find(|r| &r.name == name) else {
            issues.push(Issue::error(
                "spec-unknown-role",
                &at,
                format!("There's no role `{name}`. New roles are added as a content proposal."),
            ));
            continue;
        };
        if let Some(skills) = &p.skills {
            let before = issues.len();
            check_list(
                &mut issues,
                &format!("{at}.skills"),
                skills,
                valid_skill_name,
                "skill",
            );
            if issues.len() == before && skills != &role.skills {
                edits
                    .entry(format!("roles/{name}/ROLE.md"))
                    .or_default()
                    .push((vec!["skills"], skills.join(" ")));
            }
        }
    }
    for (name, p) in &patch.skills {
        let at = format!("spec.skills.{name}");
        let Some(skill) = report.skills.iter().find(|s| &s.name == name) else {
            issues.push(Issue::error(
                "spec-unknown-skill",
                &at,
                format!("There's no skill `{name}`. New skills are added as a content proposal."),
            ));
            continue;
        };
        if let Some(tools) = &p.tools {
            let before = issues.len();
            check_list(
                &mut issues,
                &format!("{at}.tools"),
                tools,
                valid_tool_name,
                "tool",
            );
            if issues.len() == before && tools != &skill.tools {
                edits
                    .entry(format!("skills/{name}/SKILL.md"))
                    .or_default()
                    .push((vec!["metadata", "tools"], tools.join(" ")));
            }
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    let mut changes = Vec::new();
    for (rel, sets) in edits {
        let text = match proposals::read_text(content_root, &rel) {
            Ok(Some(t)) => t,
            Ok(None) => {
                issues.push(Issue::error(
                    "spec-file-missing",
                    rel.clone(),
                    "This file isn't in the deployed content.",
                ));
                continue;
            }
            Err(r) => {
                issues.push(Issue::error(r.code, rel.clone(), r.message));
                continue;
            }
        };
        let mut new = text;
        for (key, value) in sets {
            match set_front_matter(&new, &key, &value) {
                Ok(t) => new = t,
                Err(why) => issues.push(Issue::error("spec-unsupported-format", rel.clone(), why)),
            }
        }
        changes.push(Change {
            rel,
            text: Some(new),
        });
    }
    if issues.is_empty() {
        Ok(changes)
    } else {
        Err(issues)
    }
}

/// `text` with front matter key `key` (one key, or `metadata.<key>`) set to the string
/// `value`, every other line as it was. Refused when the result doesn't parse to exactly the
/// old front matter with that one value changed (a multi-line value, an anchor, a layout this
/// line editor can't change safely).
pub fn set_front_matter(text: &str, key: &[&str], value: &str) -> Result<String, String> {
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text),
    };
    let mut lines: Vec<String> = body.split_inclusive('\n').map(String::from).collect();
    let bare = |l: &str| l.trim_end_matches(['\r', '\n']).to_string();
    if lines.first().map(|l| bare(l)) != Some("---".into()) {
        return Err("the file has no front matter".into());
    }
    let close = lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, l)| bare(l) == "---")
        .map(|(i, _)| i)
        .ok_or("the front matter isn't closed")?;
    let nl = if lines[0].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let quoted = serde_json::to_string(value).map_err(|e| e.to_string())?;
    match key {
        [k] => {
            let prefix = format!("{k}:");
            match (1..close).find(|&i| lines[i].starts_with(&prefix)) {
                Some(i) => lines[i] = format!("{k}: {quoted}{nl}"),
                None => lines.insert(close, format!("{k}: {quoted}{nl}")),
            }
        }
        [parent, k] => {
            let p = (1..close)
                .find(|&i| bare(&lines[i]).trim_end() == format!("{parent}:"))
                .ok_or(format!("the front matter has no `{parent}` block"))?;
            let mut i = p + 1;
            let mut found = None;
            while i < close && (lines[i].starts_with(' ') || bare(&lines[i]).trim().is_empty()) {
                let t = lines[i].trim_start();
                if t.starts_with(&format!("{k}:")) && found.is_none() {
                    found = Some((i, lines[i].len() - t.len()));
                }
                i += 1;
            }
            match found {
                Some((at, indent)) => {
                    lines[at] = format!("{}{k}: {quoted}{nl}", " ".repeat(indent));
                }
                None => return Err(format!("the front matter has no `{parent}.{k}`")),
            }
        }
        _ => return Err("unsupported key".into()),
    }
    let new = format!("{bom}{}", lines.concat());
    // Check: the same front matter, with only that value changed.
    let parse = |t: &str| -> Result<serde_norway::Value, String> {
        let doc = pmos_contract::frontmatter::split(t).map_err(|e| e.to_string())?;
        serde_norway::from_str(doc.yaml).map_err(|e| e.to_string())
    };
    let mut want = parse(text)?;
    let got = parse(&new).map_err(|_| "the edited front matter doesn't parse".to_string())?;
    let mut at = &mut want;
    for (n, k) in key.iter().enumerate() {
        let map = at.as_mapping_mut().ok_or("the front matter isn't a map")?;
        let k = serde_norway::Value::String(k.to_string());
        if n + 1 == key.len() {
            map.insert(k, serde_norway::Value::String(value.to_string()));
            break;
        }
        at = map.get_mut(&k).ok_or("missing parent")?;
    }
    if got != want {
        return Err(
            "this file's front matter has a layout the policy editor can't change safely; change it as a content proposal".into(),
        );
    }
    Ok(new)
}

// Evaluating a change.

/// One file a proposal changes, as a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    /// Repository path.
    pub path: String,
    /// `added`, `modified` or `deleted`.
    pub change: &'static str,
    /// A unified diff (`--- a/… +++ b/…`, three lines of context).
    pub diff: String,
}

/// Everything a validation finds, and what a pull request would write.
#[derive(Debug, Clone, Serialize)]
pub struct Evaluation {
    /// No errors: the change can be proposed.
    pub valid: bool,
    /// Every error in the changed tree, and the warnings that are new or on a changed file.
    pub issues: Vec<Issue>,
    pub content_diffs: Vec<FileDiff>,
    /// The regenerated policy files that change.
    pub policy_diffs: Vec<FileDiff>,
    pub allowance_changes: Vec<AllowanceChange>,
    /// Any allowance change marked widening.
    pub widening: bool,
    /// Policy files to write (`None`: delete), relative to the policies root. Not served.
    #[serde(skip)]
    pub policy_writes: Vec<(String, Option<String>)>,
    /// The text each changed file had before (`None`: absent), by repository path: what the
    /// stale-base check compares the default branch with. Not served.
    #[serde(skip)]
    pub base_texts: BTreeMap<String, Option<String>>,
}

/// Where things are, for one evaluation.
pub struct Roots<'a> {
    pub content: &'a Path,
    pub policies: Option<&'a Path>,
    pub content_dir: &'a str,
    pub policies_dir: &'a str,
}

fn change_kind(old: Option<&String>, new: Option<&String>) -> &'static str {
    match (old, new) {
        (None, _) => "added",
        (_, None) => "deleted",
        _ => "modified",
    }
}

/// Apply `changes` to a scratch copy of the content, lint it, regenerate every policy, check
/// them against the PM OS rules, and compare with what is there now.
pub fn evaluate(roots: &Roots, changes: &[Change], today: NaiveDate) -> Result<Evaluation, Issue> {
    let cur = current(roots.content, roots.policies, today);
    let overlay = proposals::overlay(roots.content, changes)
        .map_err(|r| Issue::error(r.code, roots.content_dir, r.message))?;
    let report = today_report(&overlay.root, today);
    let changed: BTreeSet<&str> = changes.iter().map(|c| c.rel.as_str()).collect();
    let baseline: BTreeSet<(&str, &str, &str)> = cur
        .report
        .warnings()
        .map(|d| (d.code, d.path.as_str(), d.message.as_str()))
        .collect();
    let mut issues: Vec<Issue> = report
        .diagnostics
        .iter()
        .filter(|d| {
            d.severity == pmos_contract::Severity::Error
                || changed.contains(d.path.as_str())
                || !baseline.contains(&(d.code, d.path.as_str(), d.message.as_str()))
        })
        .map(|d| Issue::from_lint(d, roots.content_dir))
        .collect();

    let mut base_texts = BTreeMap::new();
    let mut content_diffs = Vec::new();
    for c in changes {
        let path = repo_path(roots.content_dir, &c.rel);
        let old = proposals::read_text(roots.content, &c.rel).ok().flatten();
        let diff = proposals::unified_diff(&path, old.as_deref(), c.text.as_deref());
        content_diffs.push(FileDiff {
            path: path.clone(),
            change: change_kind(old.as_ref(), c.text.as_ref()),
            diff,
        });
        base_texts.insert(path, old);
    }

    let before_files = now_files(&cur);
    let mut policy_diffs = Vec::new();
    let mut policy_writes = Vec::new();
    let mut allowance_changes = Vec::new();
    if let Ok(new) = generate(&report, &overlay.root) {
        issues.extend(rule_violations(&new, &new.files, roots.policies_dir));
        let mut paths: BTreeSet<&String> = new.files.keys().collect();
        // Generated files of bots that no longer exist go, as `pmos policy generate` removes
        // them; hand-written files in those folders stay.
        paths.extend(
            before_files
                .iter()
                .filter(|(_, t)| t.starts_with(crate::GENERATED_MARKER))
                .map(|(k, _)| k),
        );
        for rel in paths {
            let (old, now) = (before_files.get(rel), new.files.get(rel));
            if old == now {
                continue;
            }
            let path = repo_path(roots.policies_dir, rel);
            policy_diffs.push(FileDiff {
                path: path.clone(),
                change: change_kind(old, now),
                diff: proposals::unified_diff(
                    &path,
                    old.map(String::as_str),
                    now.map(String::as_str),
                ),
            });
            policy_writes.push((rel.clone(), now.cloned()));
            base_texts.insert(path, old.cloned());
        }
        allowance_changes =
            diff_allowances(&allowances_of(&before_files), &allowances_of(&new.files));
        if policy_writes
            .iter()
            .any(|(rel, _)| rel.starts_with("providers/"))
        {
            allowance_changes.push(AllowanceChange {
                bot: String::new(),
                kind: "provider_profile",
                item: "providers/".into(),
                before: None,
                after: None,
                direction: "changed",
                widening: true,
                description: "A provider profile every bot's credentials come from changes".into(),
            });
        }
    }
    let valid = !issues.iter().any(|i| i.severity == "error");
    let widening = allowance_changes.iter().any(|c| c.widening);
    Ok(Evaluation {
        valid,
        issues,
        content_diffs,
        policy_diffs,
        allowance_changes,
        widening,
        policy_writes,
        base_texts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT: &str = "---\nname: docs\nrole: docs-helper\nscope: team\nowner: \"TODO\"\ndescription: x\n---\n\n# Docs\n";
    const SKILL: &str = "---\nname: docs\ndescription: d\nmetadata:\n  owner: \"TODO\"\n  tools: \"docs.search docs.read\"\n  track: \"Everyday\"\n---\n\nBody\n";

    #[test]
    fn front_matter_values_are_set_in_place() {
        let t = set_front_matter(BOT, &["role"], "pm-assistant").unwrap();
        assert_eq!(
            t,
            BOT.replace("role: docs-helper", "role: \"pm-assistant\"")
        );
        let t = set_front_matter(SKILL, &["metadata", "tools"], "docs.search").unwrap();
        assert_eq!(
            t,
            SKILL.replace("tools: \"docs.search docs.read\"", "tools: \"docs.search\"")
        );
        // CRLF and a BOM are kept.
        let crlf = format!("\u{feff}{}", BOT.replace('\n', "\r\n"));
        let t = set_front_matter(&crlf, &["scope"], "personal").unwrap();
        assert!(t.starts_with('\u{feff}') && t.contains("scope: \"personal\"\r\n"));
        // A value that would need quoting is quoted; injection is just text.
        let t = set_front_matter(BOT, &["owner"], "a\"\nrole: evil").unwrap();
        let doc = pmos_contract::frontmatter::split(&t).unwrap();
        let v: serde_norway::Value = serde_norway::from_str(doc.yaml).unwrap();
        assert_eq!(v["role"], serde_norway::Value::String("docs-helper".into()));
    }

    #[test]
    fn layouts_it_cant_edit_safely_are_refused() {
        let folded = BOT.replace("role: docs-helper", "role: >\n  docs-helper");
        assert!(set_front_matter(&folded, &["role"], "pm-assistant").is_err());
        assert!(set_front_matter("# no front matter", &["role"], "x").is_err());
        let no_meta = BOT;
        assert!(set_front_matter(no_meta, &["metadata", "tools"], "x.y").is_err());
    }

    #[test]
    fn names_are_checked() {
        assert!(valid_tool_name("docs.search") && valid_tool_name("jira.create_issue"));
        for bad in ["docs", ".x", "x.", "Docs.search", "a b.c", "x.y;z"] {
            assert!(!valid_tool_name(bad), "{bad}");
        }
        assert!(valid_owner("@ana") && valid_owner("TODO"));
        assert!(!valid_owner("a b") && !valid_owner(""));
    }
}
