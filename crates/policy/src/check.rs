//! `pmos policy check`: every PM OS rule on any policy file, and generated files current.
//!
//! The rules are checked on the file's contents, not by comparing it with the generated one,
//! so a hand edit fails with the rule it breaks (`net-egress`, `fs-writable`, `cred-binding`)
//! and not only with `policy-stale`. Both are reported.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use chrono::NaiveDate;
use serde::Serialize;

use crate::generate::{generate, generated_dir_files, BotContext};
use crate::openshell::{self, Endpoint, Policy};
use crate::spec;
use crate::{
    provider_name, provider_owner, sandbox_name, Service, CONTENT_MOUNT, MCP_METHODS,
    MIN_NUMERIC_ID, RUN_AS, WORKER_BINARY, WORKSPACE,
};

/// Every rule, with what it requires. Ids are stable: CI output and docs refer to them.
pub const RULES: [(&str, &str); 23] = [
    (POLICY_PARSE, "The file is a valid OpenShell policy: YAML, known fields only, right types."),
    (POLICY_VERSION, "`version` is 1."),
    (POLICY_MISSING, "Every bot has a policy and a launch spec, and every provider profile exists."),
    (POLICY_STALE, "Each generated file is exactly what `pmos policy generate` writes."),
    (POLICY_ORPHAN, "No generated file is left for a bot that no longer exists."),
    (CONTENT_INVALID, "The content passes `pmos lint`, so policies can be generated."),
    (NET_EGRESS, "Egress only to toolserver.pmos.internal:8710, gateway.pmos.internal:8740 and bots.pmos.internal:8750: no other host, port, wildcard or IP range."),
    (NET_INSPECTION, "Every endpoint is inspected (`mcp` on /mcp for the tool server, `rest` for the gateway and runtime) and enforced; no `tls: skip`, no access preset."),
    (NET_TOOLS, "MCP: only initialize, notifications/initialized, tools/list and tools/call, and tools/call only for the role's tools, by exact name."),
    (NET_GATEWAY_ROUTES, "Gateway: only POST /v1/chat/completions."),
    (NET_RUNTIME_ROUTES, "Runtime: only POST /v1/worker/claim and POST /v1/worker/tasks/*/{heartbeat,events,trace,complete,fail}, exactly; no task, routine or notification routes."),
    (NET_BINARIES, "Only the worker binary may open connections."),
    (NET_MIDDLEWARE, "No network middleware."),
    (FS_WORKDIR, "`filesystem_policy` is present and `include_workdir` is false."),
    (FS_WRITABLE, "Writable: the bot's workspace, /tmp and /dev/null (or paths inside them) only. No content is writable."),
    (FS_READABLE, "Readable: OpenShell's system paths, the role's skills and the bot's master context (or paths inside them) only. No `..`."),
    (FS_LANDLOCK, "`landlock.compatibility` is `hard_requirement`, so a sandbox never runs without its filesystem rules."),
    (FS_UPLOADS, "The launch spec uploads only the role's skills and the bot's master context, each to a read-only path."),
    (CRED_BINDING, "The gateway and runtime endpoints bind exactly this bot's own provider (`pmos-<bot>-gateway`, `pmos-<bot>-worker`); the tool-server endpoint binds none; no credential rewriting, signing or uninspected credentials."),
    (CRED_PROVIDERS, "The launch spec attaches this bot's gateway and worker providers and nothing else."),
    (PROC_IDENTITY, "`run_as_user` and `run_as_group` are `sandbox` (or a numeric id of at least 1000); never root."),
    (SPEC_PARSE, "The launch spec is valid: known fields only."),
    (SPEC_IDENTITY, "The launch spec names its own bot, role, scope, sandbox, policy and the workspace as workdir."),
];

pub const POLICY_PARSE: &str = "policy-parse";
pub const POLICY_VERSION: &str = "policy-version";
pub const POLICY_MISSING: &str = "policy-missing";
pub const POLICY_STALE: &str = "policy-stale";
pub const POLICY_ORPHAN: &str = "policy-orphan";
pub const CONTENT_INVALID: &str = "content-invalid";
pub const NET_EGRESS: &str = "net-egress";
pub const NET_INSPECTION: &str = "net-inspection";
pub const NET_TOOLS: &str = "net-tools";
pub const NET_GATEWAY_ROUTES: &str = "net-gateway-routes";
pub const NET_RUNTIME_ROUTES: &str = "net-runtime-routes";
pub const NET_BINARIES: &str = "net-binaries";
pub const NET_MIDDLEWARE: &str = "net-middleware";
pub const FS_WORKDIR: &str = "fs-workdir";
pub const FS_WRITABLE: &str = "fs-writable";
pub const FS_READABLE: &str = "fs-readable";
pub const FS_LANDLOCK: &str = "fs-landlock";
pub const FS_UPLOADS: &str = "fs-uploads";
pub const CRED_BINDING: &str = "cred-binding";
pub const CRED_PROVIDERS: &str = "cred-providers";
pub const PROC_IDENTITY: &str = "proc-identity";
pub const SPEC_PARSE: &str = "spec-parse";
pub const SPEC_IDENTITY: &str = "spec-identity";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Violation {
    /// The file, as given (`policies/bots/docs.yaml`).
    pub path: String,
    /// A rule id from [`RULES`].
    pub rule: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CheckReport {
    pub files_checked: usize,
    pub bots: Vec<String>,
    pub violations: Vec<Violation>,
}

impl CheckReport {
    pub fn passed(&self) -> bool {
        self.violations.is_empty()
    }

    /// The rule ids that failed, for tests.
    pub fn rules(&self) -> Vec<&'static str> {
        self.violations.iter().map(|v| v.rule).collect()
    }
}

struct Out<'a> {
    path: &'a str,
    violations: Vec<Violation>,
}

impl Out<'_> {
    fn add(&mut self, rule: &'static str, message: impl Into<String>) {
        self.violations.push(Violation {
            path: self.path.to_string(),
            rule,
            message: message.into(),
        });
    }
}

fn allowed_destinations() -> String {
    Service::ALL
        .iter()
        .map(|s| format!("{}:{}", s.host(), s.port()))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Check one OpenShell policy file's text against every PM OS rule for bot `ctx`.
pub fn check_policy(ctx: &BotContext, path: &str, text: &str) -> Vec<Violation> {
    let mut out = Out {
        path,
        violations: Vec::new(),
    };
    match openshell::parse(text) {
        Ok(policy) => check_parsed(ctx, &policy, &mut out),
        Err(e) => out.add(POLICY_PARSE, format!("not a valid OpenShell policy: {e}")),
    }
    out.violations
}

fn check_parsed(ctx: &BotContext, policy: &Policy, out: &mut Out) {
    if policy.version != 1 {
        out.add(
            POLICY_VERSION,
            format!(
                "version is {}; OpenShell policies are version 1",
                policy.version
            ),
        );
    }
    check_filesystem(ctx, policy, out);
    check_process(policy, out);
    check_network(ctx, policy, out);
}

fn check_filesystem(ctx: &BotContext, policy: &Policy, out: &mut Out) {
    match &policy.landlock {
        None => out.add(
            FS_LANDLOCK,
            "landlock is missing, so OpenShell uses best_effort: if Landlock can't be applied, the sandbox runs with no filesystem rules at all",
        ),
        Some(l) if l.compatibility != "hard_requirement" => out.add(
            FS_LANDLOCK,
            format!(
                "landlock.compatibility is `{}`; it must be `hard_requirement`, or a sandbox whose Landlock rules fail runs with none",
                l.compatibility
            ),
        ),
        Some(_) => {}
    }

    let Some(fs) = &policy.filesystem_policy else {
        out.add(
            FS_WORKDIR,
            "filesystem_policy is missing, so OpenShell makes the working directory writable; list the bot's paths explicitly",
        );
        return;
    };
    if fs.include_workdir {
        out.add(
            FS_WORKDIR,
            "include_workdir is true: the whole working directory, and any content uploaded under it, would be writable; only the workspace may be",
        );
    }

    let writable = ctx.read_write();
    let content: Vec<String> = ctx.uploads().into_iter().map(|u| u.to).collect();
    for p in &fs.read_write {
        if !clean(p) {
            out.add(
                FS_WRITABLE,
                format!("`{p}` is not a clean absolute path (OpenShell refuses `..`)"),
            );
            continue;
        }
        // A path inside an allowed one only narrows access.
        if writable.iter().any(|w| covers(w, p)) {
            continue;
        }
        let exposes_content = is_content(p)
            || content.iter().any(|c| covers(p, c))
            || covers(p, &format!("{CONTENT_MOUNT}/skills"));
        if exposes_content {
            out.add(
                FS_WRITABLE,
                format!("`{p}` is writable, which makes PM OS content writable; skills and master context are read-only"),
            );
        } else {
            out.add(
                FS_WRITABLE,
                format!("`{p}` is writable; only the bot's workspace ({WORKSPACE}), /tmp and /dev/null may be"),
            );
        }
    }

    let readable = ctx.read_only();
    for p in &fs.read_only {
        if !clean(p) {
            out.add(
                FS_READABLE,
                format!("`{p}` is not a clean absolute path (OpenShell refuses `..`)"),
            );
            continue;
        }
        if readable.iter().chain(&writable).any(|r| covers(r, p)) {
            continue;
        }
        out.add(FS_READABLE, readable_problem(ctx, p));
    }
}

/// Absolute, with no `.` or `..` components, so "inside an allowed path" means what it says.
fn clean(p: &str) -> bool {
    p.starts_with('/') && !p.split('/').any(|c| c == ".." || c == ".")
}

/// Why `p` may not be read by this bot, in words.
fn readable_problem(ctx: &BotContext, p: &str) -> String {
    let skills = format!("{CONTENT_MOUNT}/skills/");
    let people = format!("{CONTENT_MOUNT}/master/people/");
    if let Some(rest) = p.strip_prefix(&people) {
        let handle = rest.split('/').next().unwrap_or(rest);
        return format!(
            "`{p}` is {handle}'s personal master context; a bot reads only its own master context"
        );
    }
    if let Some(rest) = p.strip_prefix(&skills) {
        let skill = rest.split('/').next().unwrap_or(rest);
        if !ctx.skills.iter().any(|s| s == skill) {
            return format!(
                "`{p}` is skill `{skill}`, which role `{}` doesn't have",
                ctx.role
            );
        }
    }
    if p.starts_with(&format!("{CONTENT_MOUNT}/")) && p != WORKSPACE {
        return format!(
            "`{p}` exposes content this bot isn't given; it gets only its role's skills and its master context"
        );
    }
    if ctx
        .read_only()
        .iter()
        .any(|allowed| covers(p, allowed) && p != allowed)
    {
        return format!("`{p}` is broader than the paths this bot may read");
    }
    format!("`{p}` is not a path this sandbox may read (only OpenShell's system paths, the role's skills and the bot's master context)")
}

/// Under the content mount, and not the workspace.
fn is_content(p: &str) -> bool {
    let p = p.trim_end_matches('/');
    let ws = WORKSPACE;
    p.starts_with(&format!("{CONTENT_MOUNT}/")) && p != ws && !p.starts_with(&format!("{ws}/"))
}

/// `ancestor` is `path` or a directory above it.
fn covers(ancestor: &str, path: &str) -> bool {
    let a = ancestor.trim_end_matches('/');
    a.is_empty() || path == a || path.starts_with(&format!("{a}/"))
}

fn check_process(policy: &Policy, out: &mut Out) {
    let Some(process) = &policy.process else {
        out.add(
            PROC_IDENTITY,
            format!("process is missing; set run_as_user and run_as_group to `{RUN_AS}` so the identity never falls back to the image's USER"),
        );
        return;
    };
    for (field, value) in [
        ("run_as_user", &process.run_as_user),
        ("run_as_group", &process.run_as_group),
    ] {
        match value.as_deref() {
            None => out.add(
                PROC_IDENTITY,
                format!("process.{field} is not set; set it to `{RUN_AS}`"),
            ),
            Some(v) => {
                if let Some(problem) = identity_problem(v) {
                    out.add(PROC_IDENTITY, format!("process.{field} `{v}` {problem}"));
                }
            }
        }
    }
}

fn identity_problem(v: &str) -> Option<String> {
    if v == RUN_AS {
        return None;
    }
    if v == "root" || v.parse::<u64>() == Ok(0) {
        return Some("runs the bot as root".into());
    }
    match v.parse::<u64>() {
        Ok(n) if n < MIN_NUMERIC_ID => Some(format!(
            "is a system account; use `{RUN_AS}` or an id of at least {MIN_NUMERIC_ID}"
        )),
        Ok(n) if n > 4_294_967_294 => Some("is not a valid id".into()),
        Ok(_) => None,
        Err(_) => Some(format!("is neither `{RUN_AS}` nor a numeric id")),
    }
}

fn check_network(ctx: &BotContext, policy: &Policy, out: &mut Out) {
    if !policy.network_middlewares.is_empty() {
        let names: Vec<_> = policy.network_middlewares.keys().cloned().collect();
        out.add(
            NET_MIDDLEWARE,
            format!(
                "network_middlewares sends the bot's traffic through middleware ({}); PM OS sandboxes use none",
                names.join(", ")
            ),
        );
    }
    for (key, rule) in &policy.network_policies {
        for b in &rule.binaries {
            if b.path != WORKER_BINARY {
                out.add(
                    NET_BINARIES,
                    format!(
                        "rule `{key}` lets `{}` open connections; only the worker ({WORKER_BINARY}) may",
                        b.path
                    ),
                );
            }
        }
        for ep in &rule.endpoints {
            let Some(service) = destination(key, ep, out) else {
                continue;
            };
            check_inspection(key, service, ep, out);
            check_binding(ctx, key, service, ep, out);
            match service {
                Service::ToolServer => check_mcp(ctx, key, ep, out),
                Service::Gateway => {
                    check_routes(NET_GATEWAY_ROUTES, "gateway", key, service, ep, out)
                }
                Service::Runtime => {
                    check_routes(NET_RUNTIME_ROUTES, "bot runtime", key, service, ep, out)
                }
            }
        }
    }
}

/// The PM OS service an endpoint reaches, or a `net-egress` violation.
fn destination(key: &str, ep: &Endpoint, out: &mut Out) -> Option<Service> {
    let host = ep.host.as_deref().unwrap_or("");
    let allowed = allowed_destinations();
    if host.is_empty() {
        out.add(
            NET_EGRESS,
            format!(
                "rule `{key}` has an endpoint with no host (allowed_ips {:?}); sandboxes may reach only {allowed}",
                ep.allowed_ips
            ),
        );
        return None;
    }
    if host.contains('*') {
        out.add(
            NET_EGRESS,
            format!(
                "rule `{key}` allows wildcard host `{host}`; sandboxes may reach only {allowed}"
            ),
        );
        return None;
    }
    if !ep.ports.is_empty() {
        out.add(
            NET_EGRESS,
            format!(
                "rule `{key}` allows `{host}` on ports {:?}; sandboxes may reach only {allowed}, each on its one port",
                ep.ports
            ),
        );
        return None;
    }
    let port = ep.port.unwrap_or(0);
    if let Some(service) = Service::at(host, port) {
        return Some(service);
    }
    let lower = host.to_ascii_lowercase();
    let hint = if ["slack", "atlassian", "jira", "confluence", "fortanix"]
        .iter()
        .any(|s| lower.contains(s))
    {
        " (Slack, Jira, Confluence and the support docs are reached only through the tool server)"
    } else {
        ""
    };
    out.add(
        NET_EGRESS,
        format!(
            "rule `{key}` allows egress to {host}:{port}{hint}; sandboxes may reach only {allowed}"
        ),
    );
    None
}

fn check_inspection(key: &str, service: Service, ep: &Endpoint, out: &mut Out) {
    let dest = format!("{}:{}", service.host(), service.port());
    let protocol = ep.protocol.as_deref().unwrap_or("");
    if protocol != service.protocol() {
        out.add(
            NET_INSPECTION,
            format!(
                "rule `{key}`: {dest} must be inspected as `protocol: {}` (found `{}`), so OpenShell checks every request",
                service.protocol(),
                if protocol.is_empty() { "none" } else { protocol }
            ),
        );
    }
    let enforcement = ep.enforcement.as_deref().unwrap_or("audit");
    if enforcement != "enforce" {
        out.add(
            NET_INSPECTION,
            format!("rule `{key}`: {dest} has enforcement `{enforcement}`, which only logs a request the rules don't allow and sends it anyway; use `enforce`"),
        );
    }
    if ep.tls.as_deref() == Some("skip") {
        out.add(
            NET_INSPECTION,
            format!("rule `{key}`: `tls: skip` relays {dest} traffic without inspecting it"),
        );
    }
    if let Some(access) = ep.access.as_deref().filter(|a| !a.is_empty()) {
        out.add(
            NET_INSPECTION,
            format!("rule `{key}`: access preset `{access}` allows whole classes of requests to {dest}; list the allowed calls in `rules`"),
        );
    }
    if service == Service::ToolServer && ep.path.as_deref() != Some(crate::TOOLSERVER_MCP_PATH) {
        out.add(
            NET_INSPECTION,
            format!(
                "rule `{key}`: {dest} must be limited to path {}, the MCP route; found `{}`",
                crate::TOOLSERVER_MCP_PATH,
                ep.path.as_deref().unwrap_or("any path")
            ),
        );
    }
}

fn check_binding(ctx: &BotContext, key: &str, service: Service, ep: &Endpoint, out: &mut Out) {
    let Some(expected) = provider_name(&ctx.bot, service) else {
        // The tool server: the per-task session token comes in the claim response, so no
        // provider may be bound here (a static one would outlive the task).
        if let Some(b) = &ep.credential_binding {
            out.add(
                CRED_BINDING,
                format!(
                    "rule `{key}` binds provider `{}` to {}:{}; the tool server takes only the per-task session token from the claim, so it binds no provider",
                    b.provider,
                    service.host(),
                    service.port()
                ),
            );
        }
        check_loosened(key, ep, out);
        return;
    };
    match &ep.credential_binding {
        None => out.add(
            CRED_BINDING,
            format!("rule `{key}` binds no credentials; bind this bot's own provider `{expected}`"),
        ),
        Some(b) if b.provider == expected => {}
        Some(b) => match provider_owner(&b.provider) {
            Some((other, _)) if other != ctx.bot => out.add(
                CRED_BINDING,
                format!(
                    "rule `{key}` binds `{}`, bot `{other}`'s credentials; a sandbox may use only its own (`{expected}`)",
                    b.provider
                ),
            ),
            _ => out.add(
                CRED_BINDING,
                format!(
                    "rule `{key}` binds provider `{}`; {}:{} takes only this bot's `{expected}`",
                    b.provider,
                    service.host(),
                    service.port()
                ),
            ),
        },
    }
    check_loosened(key, ep, out);
}

/// Credential handling beyond the default `Authorization` placement.
fn check_loosened(key: &str, ep: &Endpoint, out: &mut Out) {
    let loosened = [
        (
            ep.request_body_credential_rewrite,
            "request_body_credential_rewrite resolves credentials in request bodies",
        ),
        (
            ep.websocket_credential_rewrite,
            "websocket_credential_rewrite resolves credentials in WebSocket messages",
        ),
        (
            ep.allow_uninspected_credentials,
            "allow_uninspected_credentials lets credentials travel uninspected",
        ),
        (
            ep.credential_signing.is_some(),
            "credential_signing signs requests with provider credentials",
        ),
    ];
    for (set, what) in loosened {
        if set {
            out.add(
                CRED_BINDING,
                format!("rule `{key}`: {what}; credentials go only in the Authorization header"),
            );
        }
    }
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

fn check_mcp(ctx: &BotContext, key: &str, ep: &Endpoint, out: &mut Out) {
    if let Some(mcp) = &ep.mcp {
        if mcp.allow_all_known_mcp_methods == Some(true) {
            out.add(
                NET_TOOLS,
                format!("rule `{key}`: mcp.allow_all_known_mcp_methods allows every MCP method, and every tool unless rules narrow it"),
            );
        }
        if mcp.strict_tool_names == Some(false) {
            out.add(
                NET_TOOLS,
                format!("rule `{key}`: mcp.strict_tool_names is false; tool names must be checked strictly"),
            );
        }
    }
    let tools: Vec<&str> = ctx.mcp_tools.iter().map(String::as_str).collect();
    for rule in &ep.rules {
        let allow = &rule.allow;
        let method = allow.method.as_deref().unwrap_or("");
        if !MCP_METHODS.contains(&method) {
            out.add(
                NET_TOOLS,
                format!(
                    "rule `{key}` allows MCP method `{}`; only {} are allowed",
                    if method.is_empty() { "(none)" } else { method },
                    MCP_METHODS.join(", ")
                ),
            );
            continue;
        }
        if allow.params.is_some() {
            out.add(
                NET_TOOLS,
                format!(
                    "rule `{key}` matches tools by `params`; name the role's tools with `tool`"
                ),
            );
        }
        if method != "tools/call" {
            continue;
        }
        let Some(tool) = &allow.tool else {
            if allow.params.is_none() {
                out.add(
                    NET_TOOLS,
                    format!("rule `{key}` allows tools/call for every tool; list the role's tools by name"),
                );
            }
            continue;
        };
        for name in tool.patterns() {
            if is_glob(name) {
                out.add(
                    NET_TOOLS,
                    format!("rule `{key}` allows tool pattern `{name}`; name each of the role's tools exactly"),
                );
            } else if !tools.contains(&name) {
                out.add(
                    NET_TOOLS,
                    format!(
                        "rule `{key}` allows tool `{name}`, which role `{}` doesn't have (its tools: {})",
                        ctx.role,
                        if tools.is_empty() { "none".to_string() } else { tools.join(", ") }
                    ),
                );
            }
        }
    }
}

/// A REST endpoint's allow rules must each be one of the service's routes, exactly: same
/// method, same path pattern (so `/v1/worker/**` or `*` instead of a route fails).
fn check_routes(
    rule_id: &'static str,
    what: &str,
    key: &str,
    service: Service,
    ep: &Endpoint,
    out: &mut Out,
) {
    let routes = service.routes().unwrap_or_default();
    let allowed: Vec<String> = routes.iter().map(|(m, p)| format!("{m} {p}")).collect();
    for rule in &ep.rules {
        let route = (
            rule.allow.method.as_deref().unwrap_or(""),
            rule.allow.path.as_deref().unwrap_or(""),
        );
        if !routes.contains(&route) {
            out.add(
                rule_id,
                format!(
                    "rule `{key}` allows `{} {}` on the {what}; only {} {} allowed",
                    route.0,
                    route.1,
                    allowed.join(", "),
                    if allowed.len() == 1 { "is" } else { "are" }
                ),
            );
        }
    }
}

/// Check a launch spec's text for bot `ctx`; `policy` is the bot's parsed policy, if it parsed,
/// so uploads can be checked against its read-only and writable paths.
pub fn check_spec(
    ctx: &BotContext,
    path: &str,
    text: &str,
    policy: Option<&Policy>,
) -> Vec<Violation> {
    let mut out = Out {
        path,
        violations: Vec::new(),
    };
    let spec = match spec::parse(text) {
        Ok(s) => s,
        Err(e) => {
            out.add(SPEC_PARSE, format!("not a valid PM OS launch spec: {e}"));
            return out.violations;
        }
    };

    let expected_policy = format!("{}.yaml", ctx.bot);
    let expected_sandbox = sandbox_name(&ctx.bot);
    let identity = [
        ("bot", spec.bot.as_str(), ctx.bot.as_str()),
        ("role", spec.role.as_str(), ctx.role.as_str()),
        ("scope", spec.scope.as_str(), ctx.scope_name()),
        ("sandbox", spec.sandbox.as_str(), expected_sandbox.as_str()),
        ("policy", spec.policy.as_str(), expected_policy.as_str()),
        ("workdir", spec.workdir.as_str(), WORKSPACE),
    ];
    for (field, found, expected) in identity {
        if found != expected {
            out.add(
                SPEC_IDENTITY,
                format!(
                    "{field} is `{found}`; for bot `{}` it must be `{expected}`",
                    ctx.bot
                ),
            );
        }
    }

    for p in &spec.providers {
        match Service::credentialed()
            .find(|(s, _)| provider_name(&ctx.bot, *s).as_deref() == Some(p.name.as_str()))
        {
            Some((_, c)) => {
                if p.profile != c.profile || p.credential != c.env {
                    out.add(
                        CRED_PROVIDERS,
                        format!(
                            "provider `{}` must use profile `{}` and credential {}",
                            p.name, c.profile, c.env
                        ),
                    );
                }
            }
            None => match provider_owner(&p.name) {
                Some((other, _)) if other != ctx.bot => out.add(
                    CRED_PROVIDERS,
                    format!(
                        "attaches `{}`, bot `{other}`'s credentials; a sandbox gets only its own",
                        p.name
                    ),
                ),
                _ => out.add(
                    CRED_PROVIDERS,
                    format!(
                        "attaches provider `{}`; a bot's sandbox gets only its own gateway and worker providers",
                        p.name
                    ),
                ),
            },
        }
    }

    let allowed: BTreeSet<spec::Upload> = ctx.uploads().into_iter().collect();
    for u in &spec.uploads {
        if allowed.contains(u) {
            continue;
        }
        match allowed.iter().find(|a| a.from == u.from) {
            Some(a) => out.add(
                FS_UPLOADS,
                format!("uploads `{}` to `{}`; it belongs at `{}`", u.from, u.to, a.to),
            ),
            None => out.add(
                FS_UPLOADS,
                format!(
                    "uploads `{}`, which is neither a skill of role `{}` nor the bot's master context",
                    u.from, ctx.role
                ),
            ),
        }
    }
    if let Some(fs) = policy.and_then(|p| p.filesystem_policy.as_ref()) {
        for u in &spec.uploads {
            if let Some(w) = fs.read_write.iter().find(|w| covers(w, &u.to)) {
                out.add(
                    FS_UPLOADS,
                    format!(
                        "`{}` is uploaded under writable `{w}`; content must stay read-only",
                        u.to
                    ),
                );
            } else if !fs.read_only.iter().any(|r| r == &u.to) {
                out.add(
                    FS_UPLOADS,
                    format!(
                        "`{}` is uploaded but not listed read-only in the policy",
                        u.to
                    ),
                );
            }
        }
    }
    out.violations
}

fn display(root: &Path, rel: &str) -> String {
    let root = root.to_string_lossy();
    let root = root.trim_end_matches('/');
    if root.is_empty() || root == "." {
        rel.to_string()
    } else {
        format!("{root}/{rel}")
    }
}

/// Read a file, or record why it couldn't be read.
fn read(path: &Path, label: &str, missing: &str, report: &mut CheckReport) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(text) => {
            report.files_checked += 1;
            Some(text)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            report.violations.push(Violation {
                path: label.to_string(),
                rule: POLICY_MISSING,
                message: format!("{missing}; run `pmos policy generate`"),
            });
            None
        }
        Err(e) => {
            report.violations.push(Violation {
                path: label.to_string(),
                rule: POLICY_PARSE,
                message: format!("can't read the file: {e}"),
            });
            None
        }
    }
}

fn stale(label: &str, what: &str) -> Violation {
    Violation {
        path: label.to_string(),
        rule: POLICY_STALE,
        message: format!(
            "differs from what `pmos policy generate` writes for {what}; regenerate, and make lasting changes in the bot or role, not here"
        ),
    }
}

/// Check every file under `policies_root` against the content at `content_root`.
pub fn check(content_root: &Path, policies_root: &Path, today: NaiveDate) -> CheckReport {
    let mut report = CheckReport::default();
    let generated = match generate(content_root, today) {
        Ok(g) => g,
        Err(errors) => {
            for d in errors {
                report.violations.push(Violation {
                    path: display(content_root, &d.path),
                    rule: CONTENT_INVALID,
                    message: format!("[{}] {}", d.code, d.message),
                });
            }
            return report;
        }
    };
    report.bots = generated.bots.iter().map(|b| b.bot.clone()).collect();

    for ctx in &generated.bots {
        let policy_rel = format!("bots/{}.yaml", ctx.bot);
        let spec_rel = format!("bots/{}.sandbox.yaml", ctx.bot);
        let policy_label = display(policies_root, &policy_rel);
        let spec_label = display(policies_root, &spec_rel);

        let mut parsed = None;
        if let Some(text) = read(
            &policies_root.join(&policy_rel),
            &policy_label,
            &format!("bot `{}` has no sandbox policy", ctx.bot),
            &mut report,
        ) {
            report
                .violations
                .extend(check_policy(ctx, &policy_label, &text));
            if generated.files.get(&policy_rel) != Some(&text) {
                report
                    .violations
                    .push(stale(&policy_label, &format!("bot `{}`", ctx.bot)));
            }
            parsed = openshell::parse(&text).ok();
        }
        if let Some(text) = read(
            &policies_root.join(&spec_rel),
            &spec_label,
            &format!("bot `{}` has no launch spec", ctx.bot),
            &mut report,
        ) {
            report
                .violations
                .extend(check_spec(ctx, &spec_label, &text, parsed.as_ref()));
            if generated.files.get(&spec_rel) != Some(&text) {
                report
                    .violations
                    .push(stale(&spec_label, &format!("bot `{}`", ctx.bot)));
            }
        }
    }

    for (rel, expected) in generated
        .files
        .iter()
        .filter(|(rel, _)| rel.starts_with("providers/"))
    {
        let label = display(policies_root, rel);
        if let Some(text) = read(
            &policies_root.join(rel),
            &label,
            "the provider profile is missing",
            &mut report,
        ) {
            if &text != expected {
                report
                    .violations
                    .push(stale(&label, "the provider profile"));
            }
        }
    }

    let existing = generated_dir_files(policies_root).unwrap_or_default();
    for rel in existing
        .iter()
        .filter(|rel| !generated.files.contains_key(*rel))
    {
        let label = display(policies_root, rel);
        let Ok(text) = fs::read_to_string(policies_root.join(rel)) else {
            continue;
        };
        report.files_checked += 1;
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let bot = name
            .strip_suffix(".sandbox.yaml")
            .or_else(|| name.strip_suffix(".yaml"))
            .unwrap_or(name);
        report.violations.push(Violation {
            path: label.clone(),
            rule: POLICY_ORPHAN,
            message: format!(
                "no bot `{bot}` in the content, so nothing generates this file; delete it, or add the bot"
            ),
        });
        // Still check it as a bot with no grants, so a planted file fails on its rules too.
        if rel.starts_with("bots/") {
            let ctx = BotContext::unknown(bot);
            if name.ends_with(".sandbox.yaml") {
                report
                    .violations
                    .extend(check_spec(&ctx, &label, &text, None));
            } else {
                report.violations.extend(check_policy(&ctx, &label, &text));
            }
        }
    }

    report.violations.sort();
    report.violations.dedup();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::tests::{docs, pm_ana};
    use crate::openshell::{Allow, AllowRule, AnyOf, Binary, CredentialBinding, Matcher};
    use crate::spec::{ProviderAttachment, Upload};
    use crate::yaml::to_yaml;

    const PATH: &str = "policies/bots/docs.yaml";

    fn rules_of(v: &[Violation]) -> Vec<&'static str> {
        let mut r: Vec<_> = v.iter().map(|v| v.rule).collect();
        r.dedup();
        r
    }

    /// Check a policy changed by `edit`, and return the violations.
    fn with(edit: impl FnOnce(&mut Policy)) -> Vec<Violation> {
        let mut p = docs().policy();
        edit(&mut p);
        check_policy(&docs(), PATH, &to_yaml(&p).unwrap())
    }

    fn only(v: &[Violation], rule: &str) {
        assert!(!v.is_empty(), "expected a {rule} violation, got none");
        assert!(
            v.iter().all(|x| x.rule == rule),
            "expected only {rule}: {v:#?}"
        );
    }

    fn ts(p: &mut Policy) -> &mut Endpoint {
        &mut p
            .network_policies
            .get_mut("pmos_toolserver")
            .unwrap()
            .endpoints[0]
    }

    fn gw(p: &mut Policy) -> &mut Endpoint {
        &mut p
            .network_policies
            .get_mut("pmos_gateway")
            .unwrap()
            .endpoints[0]
    }

    fn rt(p: &mut Policy) -> &mut Endpoint {
        &mut p
            .network_policies
            .get_mut("pmos_runtime")
            .unwrap()
            .endpoints[0]
    }

    fn fs(p: &mut Policy) -> &mut crate::openshell::FilesystemPolicy {
        p.filesystem_policy.as_mut().unwrap()
    }

    #[test]
    fn every_rule_is_listed_once() {
        let ids: BTreeSet<&str> = RULES.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), RULES.len());
    }

    #[test]
    fn generated_policies_and_specs_pass_every_rule() {
        for ctx in [docs(), pm_ana()] {
            let policy = ctx.render_policy();
            assert_eq!(check_policy(&ctx, PATH, &policy), vec![]);
            let parsed = openshell::parse(&policy).unwrap();
            assert_eq!(
                check_spec(&ctx, PATH, &ctx.render_spec(), Some(&parsed)),
                vec![]
            );
        }
    }

    // --- hand edits, as text, for the cases the spec names ---

    const SLACK_RULE: &str = "  slack:\n    endpoints:\n      - host: slack.com\n        port: 443\n    binaries:\n      - path: /usr/local/bin/node\n";

    fn add_rule(text: &str, rule: &str) -> String {
        text.replacen(
            "network_policies:\n",
            &format!("network_policies:\n{rule}"),
            1,
        )
    }

    #[test]
    fn a_hand_edit_adding_slack_egress_fails_net_egress() {
        let text = add_rule(&docs().render_policy(), SLACK_RULE);
        let v = check_policy(&docs(), PATH, &text);
        only(&v, NET_EGRESS);
        assert!(v[0].message.contains("slack.com:443"), "{}", v[0].message);
        assert!(v[0].message.contains("through the tool server"));
    }

    #[test]
    fn a_hand_edit_adding_jira_hosts_fails_net_egress() {
        let rule = "  jira:\n    endpoints:\n      - host: \"*.atlassian.net\"\n        port: 443\n      - host: api.atlassian.com\n        port: 443\n    binaries:\n      - path: /usr/local/bin/node\n";
        let v = check_policy(&docs(), PATH, &add_rule(&docs().render_policy(), rule));
        only(&v, NET_EGRESS);
        assert_eq!(v.len(), 2);
        assert!(v
            .iter()
            .any(|x| x.message.contains("wildcard host `*.atlassian.net`")));
        assert!(v
            .iter()
            .any(|x| x.message.contains("api.atlassian.com:443")));
    }

    #[test]
    fn a_hand_edit_making_content_writable_fails_fs_writable() {
        let text = docs().render_policy().replacen(
            "  read_write:\n",
            "  read_write:\n    - /sandbox/skills/docs\n",
            1,
        );
        let v = check_policy(&docs(), PATH, &text);
        only(&v, FS_WRITABLE);
        assert!(v[0].message.contains("makes PM OS content writable"));
        // A parent of the content is just as bad.
        let v = with(|p| fs(p).read_write.push("/sandbox".into()));
        only(&v, FS_WRITABLE);
        assert!(v[0].message.contains("content writable"));
    }

    #[test]
    fn a_hand_edit_binding_another_bots_credentials_fails_cred_binding() {
        let text = docs().render_policy().replace(
            "provider: pmos-docs-gateway",
            "provider: pmos-pm-ana-gateway",
        );
        let v = check_policy(&docs(), PATH, &text);
        only(&v, CRED_BINDING);
        assert!(
            v[0].message.contains("bot `pm-ana`'s credentials"),
            "{}",
            v[0].message
        );
    }

    // --- policy-parse and policy-version ---

    #[test]
    fn invalid_yaml_and_unknown_fields_fail_policy_parse() {
        only(&check_policy(&docs(), PATH, "version: [\n"), POLICY_PARSE);
        let text = docs().render_policy() + "mounts:\n  - /content\n";
        only(&check_policy(&docs(), PATH, &text), POLICY_PARSE);
    }

    #[test]
    fn a_version_other_than_1_fails_policy_version() {
        only(&with(|p| p.version = 2), POLICY_VERSION);
    }

    // --- network ---

    #[test]
    fn net_egress_refuses_other_ports_port_lists_and_hostless_endpoints() {
        only(&with(|p| ts(p).port = Some(8711)), NET_EGRESS);
        only(&with(|p| ts(p).port = None), NET_EGRESS);
        only(
            &with(|p| {
                ts(p).port = None;
                ts(p).ports = vec![8710, 22];
            }),
            NET_EGRESS,
        );
        only(
            &with(|p| {
                gw(p).host = None;
                gw(p).allowed_ips = vec!["10.0.0.0/8".into()];
            }),
            NET_EGRESS,
        );
        // The docs server is internal, but only the tool server may talk to it.
        only(
            &with(|p| ts(p).host = Some("docs.pmos.internal".into())),
            NET_EGRESS,
        );
    }

    #[test]
    fn net_egress_allows_both_services_and_nothing_else() {
        assert_eq!(with(|_| {}), vec![]);
        // Dropping a service is not a security violation (only stale).
        assert_eq!(
            with(|p| {
                p.network_policies.remove("pmos_gateway");
            }),
            vec![]
        );
    }

    #[test]
    fn net_inspection_requires_enforced_inspection_on_the_mcp_route() {
        only(&with(|p| ts(p).protocol = None), NET_INSPECTION);
        only(
            &with(|p| gw(p).protocol = Some("tcp".into())),
            NET_INSPECTION,
        );
        only(
            &with(|p| ts(p).enforcement = Some("audit".into())),
            NET_INSPECTION,
        );
        only(&with(|p| gw(p).enforcement = None), NET_INSPECTION);
        only(&with(|p| gw(p).tls = Some("skip".into())), NET_INSPECTION);
        only(
            &with(|p| gw(p).access = Some("full".into())),
            NET_INSPECTION,
        );
        only(
            &with(|p| ts(p).path = Some("/internal/**".into())),
            NET_INSPECTION,
        );
        only(&with(|p| ts(p).path = None), NET_INSPECTION);
        // Automatic TLS (OpenShell's empty default) is fine.
        assert_eq!(with(|p| gw(p).tls = Some(String::new())), vec![]);
    }

    #[test]
    fn net_tools_limits_tool_calls_to_the_roles_tools() {
        let call = |tool: Matcher| AllowRule {
            allow: Allow {
                tool: Some(tool),
                ..Allow::method("tools/call")
            },
        };
        let v = with(|p| ts(p).rules.push(call(Matcher::One("jira_search".into()))));
        only(&v, NET_TOOLS);
        assert!(v[0]
            .message
            .contains("`jira_search`, which role `docs-helper` doesn't have"));
        only(
            &with(|p| ts(p).rules.push(call(Matcher::One("docs_*".into())))),
            NET_TOOLS,
        );
        only(
            &with(|p| {
                ts(p).rules.push(call(Matcher::Any(AnyOf {
                    any: vec!["**".into()],
                })))
            }),
            NET_TOOLS,
        );
        only(
            &with(|p| {
                ts(p).rules.push(AllowRule {
                    allow: Allow::method("tools/call"),
                })
            }),
            NET_TOOLS,
        );
        only(
            &with(|p| {
                ts(p).rules.push(AllowRule {
                    allow: Allow::method("resources/read"),
                })
            }),
            NET_TOOLS,
        );
        only(
            &with(|p| ts(p).mcp.as_mut().unwrap().allow_all_known_mcp_methods = Some(true)),
            NET_TOOLS,
        );
        only(
            &with(|p| ts(p).mcp.as_mut().unwrap().strict_tool_names = Some(false)),
            NET_TOOLS,
        );
        // Naming one of the role's own tools again, or a deny rule, is fine.
        assert_eq!(
            with(|p| ts(p).rules.push(call(Matcher::One("docs_read".into())))),
            vec![]
        );
        assert_eq!(
            with(|p| ts(p)
                .deny_rules
                .push(serde_norway::from_str("method: tools/call\ntool: docs_read\n").unwrap())),
            vec![]
        );
    }

    #[test]
    fn net_gateway_routes_allows_chat_only() {
        let route = |m: &str, path: &str| AllowRule {
            allow: Allow {
                path: Some(path.into()),
                ..Allow::method(m)
            },
        };
        only(
            &with(|p| gw(p).rules.push(route("POST", "/v1/embeddings"))),
            NET_GATEWAY_ROUTES,
        );
        only(
            &with(|p| gw(p).rules.push(route("*", "/**"))),
            NET_GATEWAY_ROUTES,
        );
        only(
            &with(|p| gw(p).rules.push(route("GET", "/v1/pmos/workarounds"))),
            NET_GATEWAY_ROUTES,
        );
    }

    #[test]
    fn net_runtime_routes_allows_only_the_worker_routes() {
        let route = |m: &str, path: &str| AllowRule {
            allow: Allow {
                path: Some(path.into()),
                ..Allow::method(m)
            },
        };
        // A hand edit reading tasks or routines on 8750 fails.
        for (m, path) in [
            ("GET", "/v1/tasks"),
            ("GET", "/v1/tasks/*"),
            ("GET", "/v1/routines"),
            ("POST", "/v1/routines"),
            ("GET", "/v1/notifications"),
            ("POST", "/v1/tasks"),
            // Wider patterns than the exact routes.
            ("POST", "/v1/worker/**"),
            ("POST", "/v1/worker/tasks/**"),
            ("*", "/v1/worker/claim"),
            // A route's method changed.
            ("GET", "/v1/worker/claim"),
        ] {
            let v = with(|p| rt(p).rules.push(route(m, path)));
            only(&v, NET_RUNTIME_ROUTES);
            assert!(v[0]
                .message
                .contains(&format!("`{m} {path}` on the bot runtime")));
        }
        // An access preset instead of rules is an inspection failure.
        only(
            &with(|p| rt(p).access = Some("read-write".into())),
            NET_INSPECTION,
        );
        // The runtime only on 8750, and only inspected and enforced.
        only(&with(|p| rt(p).port = Some(8751)), NET_EGRESS);
        only(
            &with(|p| rt(p).enforcement = Some("audit".into())),
            NET_INSPECTION,
        );
        // Each generated route alone is fine; dropping routes is only stale.
        assert_eq!(with(|p| rt(p).rules.truncate(1)), vec![]);
    }

    #[test]
    fn nothing_else_may_be_reached_on_the_runtime_host() {
        let v = with(|p| {
            let mut extra = rt(p).clone();
            extra.port = Some(8760);
            p.network_policies
                .get_mut("pmos_runtime")
                .unwrap()
                .endpoints
                .push(extra);
        });
        only(&v, NET_EGRESS);
        assert!(v[0].message.contains("bots.pmos.internal:8760"));
    }

    #[test]
    fn net_binaries_allows_only_the_worker() {
        let v = with(|p| {
            p.network_policies
                .get_mut("pmos_gateway")
                .unwrap()
                .binaries
                .push(Binary {
                    path: "/usr/bin/curl".into(),
                })
        });
        only(&v, NET_BINARIES);
        only(
            &with(|p| {
                p.network_policies
                    .get_mut("pmos_toolserver")
                    .unwrap()
                    .binaries = vec![Binary { path: "/**".into() }]
            }),
            NET_BINARIES,
        );
    }

    #[test]
    fn net_middleware_refuses_any_middleware() {
        let v = with(|p| {
            p.network_middlewares.insert(
                "tap".into(),
                serde_norway::from_str(
                    "middleware: external-tap\nendpoints:\n  include: [\"**.pmos.internal\"]\n",
                )
                .unwrap(),
            );
        });
        only(&v, NET_MIDDLEWARE);
    }

    // --- filesystem ---

    #[test]
    fn fs_workdir_requires_an_explicit_filesystem_without_the_workdir() {
        only(&with(|p| fs(p).include_workdir = true), FS_WORKDIR);
        only(&with(|p| p.filesystem_policy = None), FS_WORKDIR);
    }

    #[test]
    fn fs_writable_allows_only_the_workspace_and_scratch() {
        let v = with(|p| fs(p).read_write.push("/home".into()));
        only(&v, FS_WRITABLE);
        assert!(v[0].message.contains("only the bot's workspace"));
        only(&with(|p| fs(p).read_write.push("/".into())), FS_WRITABLE);
        only(
            &with(|p| fs(p).read_write.push("/sandbox/master/AGENTS.md".into())),
            FS_WRITABLE,
        );
        // `..` never counts as inside the workspace.
        let v = with(|p| fs(p).read_write.push("/sandbox/workspace/../skills".into()));
        only(&v, FS_WRITABLE);
        assert!(v[0].message.contains("not a clean absolute path"));
        // A directory inside the workspace is fine.
        assert_eq!(
            with(|p| fs(p).read_write.push("/sandbox/workspace/out".into())),
            vec![]
        );
        // Moving a system path from writable to read-only is fine.
        assert_eq!(
            with(|p| {
                fs(p).read_write.retain(|x| x != "/tmp");
                fs(p).read_only.push("/tmp".into());
            }),
            vec![]
        );
    }

    #[test]
    fn fs_readable_refuses_other_skills_other_peoples_context_and_wider_paths() {
        let v = with(|p| fs(p).read_only.push("/sandbox/skills/prd-draft".into()));
        only(&v, FS_READABLE);
        assert!(v[0]
            .message
            .contains("skill `prd-draft`, which role `docs-helper` doesn't have"));
        let v = with(|p| {
            fs(p)
                .read_only
                .push("/sandbox/master/people/ana/AGENTS.md".into())
        });
        only(&v, FS_READABLE);
        assert!(v[0].message.contains("ana's personal master context"));
        let v = with(|p| fs(p).read_only.push("/sandbox/context".into()));
        only(&v, FS_READABLE);
        only(
            &with(|p| fs(p).read_only.push("/sandbox".into())),
            FS_READABLE,
        );
        only(&with(|p| fs(p).read_only.push("/".into())), FS_READABLE);
        only(&with(|p| fs(p).read_only.push("/home".into())), FS_READABLE);
        only(
            &with(|p| {
                fs(p)
                    .read_only
                    .push("/sandbox/skills/docs/../../master".into())
            }),
            FS_READABLE,
        );
        only(&with(|p| fs(p).read_only.push("usr".into())), FS_READABLE);
        // Listing the workspace as read-only, or a path inside an allowed one, only narrows.
        assert_eq!(with(|p| fs(p).read_only.push(WORKSPACE.into())), vec![]);
        assert_eq!(
            with(|p| {
                fs(p).read_only.push("/sandbox/skills/docs/SKILL.md".into());
                fs(p).read_only.push("/usr/share".into());
            }),
            vec![]
        );
    }

    #[test]
    fn fs_landlock_requires_hard_requirement() {
        only(
            &with(|p| p.landlock.as_mut().unwrap().compatibility = "best_effort".into()),
            FS_LANDLOCK,
        );
        only(&with(|p| p.landlock = None), FS_LANDLOCK);
    }

    // --- credentials ---

    #[test]
    fn cred_binding_requires_this_bots_provider_for_each_service() {
        let bind = |provider: &str| {
            Some(CredentialBinding {
                provider: provider.into(),
            })
        };
        only(&with(|p| gw(p).credential_binding = None), CRED_BINDING);
        only(&with(|p| rt(p).credential_binding = None), CRED_BINDING);
        // This bot's own worker token on the gateway is still the wrong credential.
        let v = with(|p| gw(p).credential_binding = bind("pmos-docs-worker"));
        only(&v, CRED_BINDING);
        assert!(v[0]
            .message
            .contains("takes only this bot's `pmos-docs-gateway`"));
        // Another bot's worker token on the runtime.
        let v = with(|p| rt(p).credential_binding = bind("pmos-pm-ana-worker"));
        only(&v, CRED_BINDING);
        assert!(v[0].message.contains("bot `pm-ana`'s credentials"));
        // The tool server binds nothing: not a session provider, not this bot's own gateway
        // token, nothing else.
        for provider in ["pmos-docs-session", "pmos-docs-gateway", "atlassian"] {
            let v = with(|p| ts(p).credential_binding = bind(provider));
            only(&v, CRED_BINDING);
            assert!(
                v[0].message.contains("binds no provider"),
                "{}",
                v[0].message
            );
        }
        only(
            &with(|p| ts(p).request_body_credential_rewrite = true),
            CRED_BINDING,
        );
        only(
            &with(|p| rt(p).credential_signing = Some("sigv4".into())),
            CRED_BINDING,
        );
        only(
            &with(|p| gw(p).request_body_credential_rewrite = true),
            CRED_BINDING,
        );
        only(
            &with(|p| gw(p).websocket_credential_rewrite = true),
            CRED_BINDING,
        );
        only(
            &with(|p| gw(p).allow_uninspected_credentials = true),
            CRED_BINDING,
        );
        only(
            &with(|p| gw(p).credential_signing = Some("sigv4".into())),
            CRED_BINDING,
        );
    }

    // --- process ---

    #[test]
    fn proc_identity_refuses_root_system_accounts_and_missing_ids() {
        let set = |u: &str| with(|p| p.process.as_mut().unwrap().run_as_user = Some(u.to_string()));
        only(&set("root"), PROC_IDENTITY);
        only(&set("0"), PROC_IDENTITY);
        only(&set("33"), PROC_IDENTITY);
        only(&set("admin"), PROC_IDENTITY);
        only(&set("4294967295"), PROC_IDENTITY);
        only(
            &with(|p| p.process.as_mut().unwrap().run_as_group = None),
            PROC_IDENTITY,
        );
        only(&with(|p| p.process = None), PROC_IDENTITY);
        // A dedicated numeric identity is fine.
        assert_eq!(set("1500"), vec![]);
    }

    // --- launch spec ---

    fn spec_with(edit: impl FnOnce(&mut spec::SandboxSpec)) -> Vec<Violation> {
        let mut s = docs().spec();
        edit(&mut s);
        let policy = docs().policy();
        check_spec(&docs(), "s", &to_yaml(&s).unwrap(), Some(&policy))
    }

    #[test]
    fn spec_parse_refuses_unknown_fields() {
        let text = docs().render_spec() + "env:\n  SLACK_TOKEN: x\n";
        only(&check_spec(&docs(), "s", &text, None), SPEC_PARSE);
    }

    #[test]
    fn spec_identity_must_name_this_bot() {
        only(&spec_with(|s| s.bot = "pm-ana".into()), SPEC_IDENTITY);
        only(
            &spec_with(|s| s.role = "pm-assistant".into()),
            SPEC_IDENTITY,
        );
        only(&spec_with(|s| s.scope = "personal".into()), SPEC_IDENTITY);
        only(
            &spec_with(|s| s.sandbox = "pmos-pm-ana".into()),
            SPEC_IDENTITY,
        );
        only(
            &spec_with(|s| s.policy = "pm-ana.yaml".into()),
            SPEC_IDENTITY,
        );
        only(&spec_with(|s| s.workdir = "/sandbox".into()), SPEC_IDENTITY);
    }

    #[test]
    fn cred_providers_attaches_only_this_bots_providers() {
        let attach = |name: &str, profile: &str, cred: &str| ProviderAttachment {
            name: name.into(),
            profile: profile.into(),
            credential: cred.into(),
        };
        let v = spec_with(|s| {
            s.providers.push(attach(
                "pmos-pm-ana-worker",
                "pmos-worker",
                "PMOS_WORKER_TOKEN",
            ))
        });
        only(&v, CRED_PROVIDERS);
        assert!(v[0].message.contains("bot `pm-ana`'s credentials"));
        // There is no session provider any more: the session token comes in the claim.
        only(
            &spec_with(|s| {
                s.providers.push(attach(
                    "pmos-docs-session",
                    "pmos-session",
                    "PMOS_SESSION_TOKEN",
                ))
            }),
            CRED_PROVIDERS,
        );
        only(
            &spec_with(|s| s.providers.push(attach("my-slack", "slack", "SLACK_TOKEN"))),
            CRED_PROVIDERS,
        );
        only(
            &spec_with(|s| s.providers[0].credential = "PMOS_SERVICE_TOKEN".into()),
            CRED_PROVIDERS,
        );
        // Attaching fewer providers is not a leak.
        assert_eq!(spec_with(|s| s.providers.clear()), vec![]);
    }

    #[test]
    fn fs_uploads_allows_only_the_roles_skills_and_master_context_read_only() {
        let up = |from: &str, to: &str| Upload {
            from: from.into(),
            to: to.into(),
        };
        let v = spec_with(|s| {
            s.uploads
                .push(up("context/products", "/sandbox/context/products"))
        });
        assert_eq!(rules_of(&v), vec![FS_UPLOADS]);
        assert!(v.iter().any(|x| x.message.contains("neither a skill")));
        let v = spec_with(|s| s.uploads.push(up("skills/ask", "/sandbox/skills/ask")));
        assert_eq!(rules_of(&v), vec![FS_UPLOADS]);
        // The right content in a writable place is still refused.
        let v = spec_with(|s| s.uploads[0].to = "/sandbox/workspace/AGENTS.md".into());
        assert_eq!(rules_of(&v), vec![FS_UPLOADS]);
        assert!(v
            .iter()
            .any(|x| x.message.contains("under writable `/sandbox/workspace`")));
        assert!(v
            .iter()
            .any(|x| x.message.contains("belongs at `/sandbox/master/AGENTS.md`")));
    }

    #[test]
    fn a_policy_for_an_unknown_bot_grants_nothing() {
        let ctx = BotContext::unknown("evil");
        let v = check_policy(&ctx, PATH, &docs().render_policy());
        let rules = rules_of(&v);
        assert!(rules.contains(&FS_READABLE), "{v:#?}");
        assert!(rules.contains(&CRED_BINDING));
        assert!(rules.contains(&NET_TOOLS));
    }
}
