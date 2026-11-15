//! Generate each bot's sandbox policy, launch spec and the shared provider profiles.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use chrono::NaiveDate;
use pmos_contract::bots::{role_tools, Bot, BotScope};
use pmos_contract::model::harness_tool_name;
use pmos_contract::{lint, Diagnostic, LintOptions, Report, Severity};
use serde_json::json;

use crate::openshell::{
    Allow, AllowRule, AnyOf, Binary, CredentialBinding, Endpoint, FilesystemPolicy, Landlock,
    Matcher, McpOptions, NetworkRule, Policy, Process,
};
use crate::spec::{ProviderAttachment, SandboxSpec, Upload};
use crate::{
    provider_name, sandbox_name, yaml, Service, CONTENT_MOUNT, GENERATED_MARKER, MCP_VERSIONS,
    RUN_AS, SYSTEM_READ_ONLY, SYSTEM_READ_WRITE, TOOLSERVER_MCP_PATH, TOOL_REGISTRY, WORKER_BINARY,
    WORKSPACE,
};

/// Everything about one bot that its sandbox policy depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotContext {
    pub bot: String,
    pub role: String,
    /// `None` for a policy file whose bot isn't in the content (checked with no grants).
    pub scope: Option<BotScope>,
    /// The role's skills, sorted.
    pub skills: Vec<String>,
    /// The role's tools as MCP tool names (`docs_search`), which is what `tools/call` carries.
    pub mcp_tools: BTreeSet<String>,
    /// The bot's master context files, relative to the content root.
    pub master: Vec<String>,
}

impl BotContext {
    /// The context for `bot`, from a lint report of the content at `content_root`. `None` if
    /// the bot's role isn't in the report.
    pub fn from_report(report: &Report, bot: &Bot, content_root: &Path) -> Option<Self> {
        let role = report.roles.iter().find(|r| r.name == bot.role)?;
        let skills: BTreeSet<String> = role.skills.iter().cloned().collect();
        let mcp_tools = role_tools(role, &report.skills)
            .iter()
            .map(|t| harness_tool_name(t))
            .collect();
        let mut master = vec!["master/AGENTS.md".to_string()];
        // A personal bot also reads its owner's own master file (CTX-3), if they wrote one.
        if bot.scope == BotScope::Personal {
            let personal = format!("master/people/{}/AGENTS.md", bot.owner);
            if content_root.join(&personal).is_file() {
                master.push(personal);
            }
        }
        Some(Self {
            bot: bot.name.clone(),
            role: role.name.clone(),
            scope: Some(bot.scope),
            skills: skills.into_iter().collect(),
            mcp_tools,
            master,
        })
    }

    /// A bot the content doesn't define: no skills, no tools, no master context.
    pub fn unknown(bot: &str) -> Self {
        Self {
            bot: bot.to_string(),
            role: String::new(),
            scope: None,
            skills: Vec::new(),
            mcp_tools: BTreeSet::new(),
            master: Vec::new(),
        }
    }

    pub fn scope_name(&self) -> &'static str {
        match self.scope {
            Some(BotScope::Team) => "team",
            Some(BotScope::Personal) => "personal",
            None => "unknown",
        }
    }

    /// The content uploaded into the sandbox, read-only: the role's skills, the bot's master
    /// context and the tool registry (`tools.yaml`, which the worker reads to tell reads from
    /// writes), each at the same relative path under [`CONTENT_MOUNT`].
    pub fn uploads(&self) -> Vec<Upload> {
        let mut uploads: Vec<Upload> = self
            .skills
            .iter()
            .map(|s| format!("skills/{s}"))
            .chain(self.master.iter().cloned())
            .chain(std::iter::once(TOOL_REGISTRY.to_string()))
            .map(|from| Upload {
                to: format!("{CONTENT_MOUNT}/{from}"),
                from,
            })
            .collect();
        uploads.sort();
        uploads
    }

    /// Every path the policy may list as read-only.
    pub fn read_only(&self) -> BTreeSet<String> {
        SYSTEM_READ_ONLY
            .iter()
            .map(|p| p.to_string())
            .chain(self.uploads().into_iter().map(|u| u.to))
            .collect()
    }

    /// Every path the policy may list as writable.
    pub fn read_write(&self) -> BTreeSet<String> {
        SYSTEM_READ_WRITE
            .iter()
            .chain([WORKSPACE].iter())
            .map(|p| p.to_string())
            .collect()
    }

    /// The bot's OpenShell policy.
    pub fn policy(&self) -> Policy {
        let network_policies = Service::ALL
            .into_iter()
            .map(|s| {
                (
                    s.rule_name().to_string(),
                    NetworkRule {
                        name: Some(s.rule_name().to_string()),
                        endpoints: vec![self.endpoint(s)],
                        binaries: vec![Binary {
                            path: WORKER_BINARY.into(),
                        }],
                    },
                )
            })
            .collect();
        Policy {
            version: 1,
            filesystem_policy: Some(FilesystemPolicy {
                include_workdir: false,
                read_only: self.read_only().into_iter().collect(),
                read_write: self.read_write().into_iter().collect(),
            }),
            landlock: Some(Landlock {
                compatibility: "hard_requirement".into(),
            }),
            process: Some(Process {
                run_as_user: Some(RUN_AS.into()),
                run_as_group: Some(RUN_AS.into()),
            }),
            network_policies,
            network_middlewares: BTreeMap::new(),
        }
    }

    fn endpoint(&self, service: Service) -> Endpoint {
        let base = Endpoint {
            host: Some(service.host().into()),
            port: Some(service.port()),
            protocol: Some(service.protocol().into()),
            enforcement: Some("enforce".into()),
            credential_binding: provider_name(&self.bot, service)
                .map(|provider| CredentialBinding { provider }),
            ..Endpoint::default()
        };
        if let Some(routes) = service.routes() {
            return Endpoint {
                rules: routes
                    .iter()
                    .map(|(method, path)| AllowRule {
                        allow: Allow {
                            path: Some(path.to_string()),
                            ..Allow::method(method)
                        },
                    })
                    .collect(),
                ..base
            };
        }
        match service {
            Service::ToolServer => {
                let mut rules: Vec<AllowRule> =
                    ["initialize", "notifications/initialized", "tools/list"]
                        .into_iter()
                        .map(|m| AllowRule {
                            allow: Allow::method(m),
                        })
                        .collect();
                if !self.mcp_tools.is_empty() {
                    rules.push(AllowRule {
                        allow: Allow {
                            tool: Some(Matcher::Any(AnyOf {
                                any: self.mcp_tools.iter().cloned().collect(),
                            })),
                            ..Allow::method("tools/call")
                        },
                    });
                }
                Endpoint {
                    path: Some(TOOLSERVER_MCP_PATH.into()),
                    mcp: Some(McpOptions {
                        versions: Some(MCP_VERSIONS.iter().map(|v| v.to_string()).collect()),
                        ..McpOptions::default()
                    }),
                    rules,
                    ..base
                }
            }
            // REST services returned above.
            Service::Gateway | Service::Runtime => base,
        }
    }

    /// The bot's launch spec.
    pub fn spec(&self) -> SandboxSpec {
        let mut providers: Vec<ProviderAttachment> = Service::credentialed()
            .map(|(_, c)| ProviderAttachment {
                name: format!("pmos-{}-{}", self.bot, c.suffix),
                profile: c.profile.into(),
                credential: c.env.into(),
            })
            .collect();
        providers.sort();
        SandboxSpec {
            bot: self.bot.clone(),
            role: self.role.clone(),
            scope: self.scope_name().into(),
            sandbox: sandbox_name(&self.bot),
            policy: format!("{}.yaml", self.bot),
            workdir: WORKSPACE.into(),
            providers,
            uploads: self.uploads(),
        }
    }

    fn sources(&self) -> String {
        format!("bots/{}/BOT.md and roles/{}/ROLE.md", self.bot, self.role)
    }

    /// The policy file's text, header included.
    pub fn render_policy(&self) -> String {
        let header = format!(
            "{GENERATED_MARKER} from {sources}.\n\
             # Do not edit by hand: change the bot or its role, regenerate, and commit both.\n\
             # `pmos policy check` fails on any difference. A network change needs a second\n\
             # platform-owner PM's review (POL-2); see policies/README.md.\n\
             # Format: NVIDIA OpenShell sandbox policy, version 1. Values still to verify on the\n\
             # Spark (worker binary path, /sandbox layout, MCP revisions) are listed in\n\
             # docs/sandbox-policies.md. Launch flags are in {bot}.sandbox.yaml.\n",
            sources = self.sources(),
            bot = self.bot,
        );
        header + &render(&self.policy())
    }

    /// The launch spec's text, header included.
    pub fn render_spec(&self) -> String {
        let header = format!(
            "{GENERATED_MARKER} from {sources}.\n\
             # PM OS launch spec for sandbox {sandbox}: the only providers to attach\n\
             # (`--provider`) and the only content to upload (`--upload <from>:<to>`, from the\n\
             # content root). Do not edit by hand; `pmos policy check` fails on any difference.\n",
            sources = self.sources(),
            sandbox = sandbox_name(&self.bot),
        );
        header + &render(&self.spec())
    }
}

fn render<T: serde::Serialize>(value: &T) -> String {
    // The policy types hold only string-keyed maps, which JSON can always represent.
    yaml::to_yaml(value).expect("policy types serialize to JSON")
}

/// The provider profiles every bot's providers are created from, one per credentialed
/// service. All are endpointless, so a credential resolves only where a policy binds that exact
/// provider instance.
pub fn provider_profiles() -> BTreeMap<String, String> {
    let profile = |service: Service| {
        let c = service
            .credential()
            .expect("profiles are made only for credentialed services");
        let (display_name, description, category, inference_capable, name, what) = match service {
            Service::Gateway => (
                "PM OS gateway",
                "One bot's client token for the PM OS model gateway.",
                "inference",
                true,
                "gateway_token",
                "The bot's own entry in the gateway's PMOS_GATEWAY_TOKENS (bot-<bot>=...)",
            ),
            _ => (
                "PM OS bot runtime worker",
                "One bot's worker client token for the PM OS bot runtime; it may claim only that bot's tasks.",
                "other",
                false,
                "worker_token",
                "The bot's own entry in the runtime's PMOS_BOTS_TOKENS (worker-<bot>=...)",
            ),
        };
        json!({
            "id": c.profile,
            "display_name": display_name,
            "description": format!(
                "{description} Endpointless: each bot's policy binds its own instance to {}:{}.",
                service.host(),
                service.port()
            ),
            "category": category,
            "inference_capable": inference_capable,
            "credentials": [{
                "name": name,
                "description": what,
                "env_vars": [c.env],
                "required": true,
                "auth_style": "bearer",
            }],
        })
    };
    Service::credentialed()
        .map(|(s, _)| profile(s))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|profile| {
            let id = profile["id"].as_str().unwrap_or_default().to_string();
            let header = format!(
                "{GENERATED_MARKER}. NVIDIA OpenShell provider profile.\n\
                 # Endpointless on purpose: a credential resolves only where a bot's own policy\n\
                 # binds its own instance (`credential_binding.provider: pmos-<bot>-...`).\n\
                 # Import once on the Spark (verify the command there):\n\
                 #   openshell provider profile lint -f policies/providers/{id}.yaml\n\
                 #   openshell provider profile import -f policies/providers/{id}.yaml --global\n"
            );
            (format!("providers/{id}.yaml"), header + &render(&profile))
        })
        .collect()
}

/// Everything `pmos policy generate` writes, keyed by path relative to the policies root.
#[derive(Debug, Clone, Default)]
pub struct Generated {
    pub bots: Vec<BotContext>,
    pub files: BTreeMap<String, String>,
}

/// Generate every bot's files from the content at `content_root`. Fails with the lint's
/// errors if the content doesn't pass `pmos lint`.
pub fn generate(content_root: &Path, today: NaiveDate) -> Result<Generated, Vec<Diagnostic>> {
    let report = lint(content_root, &LintOptions::new(today));
    let errors: Vec<Diagnostic> = report.errors().cloned().collect();
    if !errors.is_empty() {
        return Err(errors);
    }
    from_report(&report, content_root)
}

/// [`generate`] for a lint report already in hand.
pub fn from_report(report: &Report, content_root: &Path) -> Result<Generated, Vec<Diagnostic>> {
    let mut generated = Generated::default();
    let mut errors = Vec::new();
    let mut bots: Vec<&Bot> = report.bots.iter().collect();
    bots.sort_by(|a, b| a.name.cmp(&b.name));
    for bot in bots {
        let Some(ctx) = BotContext::from_report(report, bot, content_root) else {
            errors.push(Diagnostic {
                severity: Severity::Error,
                code: "bot-role-unknown",
                path: format!("bots/{}/BOT.md", bot.name),
                message: format!("role `{}` is not in roles/", bot.role),
            });
            continue;
        };
        generated
            .files
            .insert(format!("bots/{}.yaml", ctx.bot), ctx.render_policy());
        generated
            .files
            .insert(format!("bots/{}.sandbox.yaml", ctx.bot), ctx.render_spec());
        generated.bots.push(ctx);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    generated.files.extend(provider_profiles());
    Ok(generated)
}

/// What [`write`] did, by path relative to the policies root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteSummary {
    pub written: Vec<String>,
    pub unchanged: Vec<String>,
    /// Generated files for bots that no longer exist.
    pub removed: Vec<String>,
}

/// Write the generated files under `out_root`, and remove generated files (those starting
/// with [`GENERATED_MARKER`]) that are no longer generated. Hand-written files are never
/// touched.
pub fn write(out_root: &Path, generated: &Generated) -> io::Result<WriteSummary> {
    let mut summary = WriteSummary::default();
    for (rel, text) in &generated.files {
        let path = out_root.join(rel);
        if fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            summary.unchanged.push(rel.clone());
            continue;
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, text)?;
        summary.written.push(rel.clone());
    }
    for rel in generated_dir_files(out_root)? {
        if generated.files.contains_key(&rel) {
            continue;
        }
        let path = out_root.join(&rel);
        let text = fs::read_to_string(&path).unwrap_or_default();
        if text.starts_with(GENERATED_MARKER) {
            fs::remove_file(&path)?;
            summary.removed.push(rel);
        }
    }
    Ok(summary)
}

/// The `.yaml` files in `bots/` and `providers/` under `root`, relative to it.
pub(crate) fn generated_dir_files(root: &Path) -> io::Result<Vec<String>> {
    let mut files = Vec::new();
    for dir in ["bots", "providers"] {
        let entries = match fs::read_dir(root.join(dir)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_file() && name.ends_with(".yaml") {
                files.push(format!("{dir}/{name}"));
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::openshell;

    /// The seeded Docs bot, as the content defines it.
    pub(crate) fn docs() -> BotContext {
        BotContext {
            bot: "docs".into(),
            role: "docs-helper".into(),
            scope: Some(BotScope::Team),
            skills: vec!["docs".into()],
            mcp_tools: ["docs_read", "docs_search"].map(String::from).into(),
            master: vec!["master/AGENTS.md".into()],
        }
    }

    /// A personal bot with its owner's own master file.
    pub(crate) fn pm_ana() -> BotContext {
        BotContext {
            bot: "pm-ana".into(),
            role: "pm-assistant".into(),
            scope: Some(BotScope::Personal),
            skills: vec!["ask".into(), "meeting-notes".into(), "prd-draft".into()],
            mcp_tools: ["context_search", "jira_create_issue", "jira_search"]
                .map(String::from)
                .into(),
            master: vec![
                "master/AGENTS.md".into(),
                "master/people/ana/AGENTS.md".into(),
            ],
        }
    }

    #[test]
    fn the_policy_allows_only_the_three_services() {
        let p = docs().policy();
        let dests: Vec<(String, u16)> = p
            .network_policies
            .values()
            .flat_map(|r| &r.endpoints)
            .map(|e| (e.host.clone().unwrap(), e.port.unwrap()))
            .collect();
        assert_eq!(
            dests,
            vec![
                ("gateway.pmos.internal".to_string(), 8740),
                ("bots.pmos.internal".to_string(), 8750),
                ("toolserver.pmos.internal".to_string(), 8710)
            ]
        );
        assert!(p.network_middlewares.is_empty());
        for rule in p.network_policies.values() {
            assert_eq!(
                rule.binaries,
                vec![Binary {
                    path: WORKER_BINARY.into()
                }]
            );
            for e in &rule.endpoints {
                assert_eq!(e.enforcement.as_deref(), Some("enforce"));
                assert!(e.tls.is_none() && e.access.is_none() && e.allowed_ips.is_empty());
            }
        }
    }

    #[test]
    fn tool_calls_are_limited_to_the_roles_tools() {
        let p = docs().policy();
        let ts = &p.network_policies["pmos_toolserver"].endpoints[0];
        assert_eq!(ts.path.as_deref(), Some("/mcp"));
        assert_eq!(ts.protocol.as_deref(), Some("mcp"));
        let call = ts
            .rules
            .iter()
            .find(|r| r.allow.method.as_deref() == Some("tools/call"))
            .unwrap();
        assert_eq!(
            call.allow.tool.as_ref().unwrap().patterns(),
            vec!["docs_read", "docs_search"]
        );
        // A role with no tools gets no tools/call rule at all, not an open one.
        let mut none = docs();
        none.mcp_tools.clear();
        let p = none.policy();
        assert!(p.network_policies["pmos_toolserver"].endpoints[0]
            .rules
            .iter()
            .all(|r| r.allow.method.as_deref() != Some("tools/call")));
    }

    #[test]
    fn the_gateway_allows_chat_only_and_the_runtime_worker_routes_only() {
        let p = docs().policy();
        let routes = |rule: &str| -> Vec<String> {
            let e = &p.network_policies[rule].endpoints[0];
            assert_eq!(e.protocol.as_deref(), Some("rest"));
            e.rules
                .iter()
                .map(|r| {
                    format!(
                        "{} {}",
                        r.allow.method.as_deref().unwrap(),
                        r.allow.path.as_deref().unwrap()
                    )
                })
                .collect()
        };
        assert_eq!(routes("pmos_gateway"), ["POST /v1/chat/completions"]);
        assert_eq!(
            routes("pmos_runtime"),
            [
                "POST /v1/worker/claim",
                "POST /v1/worker/tasks/*/complete",
                "POST /v1/worker/tasks/*/events",
                "POST /v1/worker/tasks/*/fail",
                "POST /v1/worker/tasks/*/heartbeat",
                "POST /v1/worker/tasks/*/trace",
            ]
        );
    }

    #[test]
    fn filesystem_is_workspace_plus_read_only_content() {
        let fs = docs().policy().filesystem_policy.unwrap();
        assert!(!fs.include_workdir);
        assert_eq!(fs.read_write, ["/dev/null", "/sandbox/workspace", "/tmp"]);
        assert!(fs.read_only.contains(&"/sandbox/skills/docs".to_string()));
        assert!(fs
            .read_only
            .contains(&"/sandbox/master/AGENTS.md".to_string()));
        assert!(!fs
            .read_only
            .iter()
            .any(|p| p.starts_with("/sandbox/master/people")));
        // Nothing content-related is writable, and nothing beyond the system set and the
        // bot's content is readable.
        for p in &fs.read_write {
            assert!(!p.starts_with("/sandbox/skills") && !p.starts_with("/sandbox/master"));
        }
        let content: Vec<_> = fs
            .read_only
            .iter()
            .filter(|p| p.starts_with("/sandbox/"))
            .collect();
        assert_eq!(
            content,
            [
                "/sandbox/master/AGENTS.md",
                "/sandbox/skills/docs",
                "/sandbox/tools.yaml"
            ]
        );
    }

    #[test]
    fn a_personal_bot_reads_its_owners_master_file_and_its_skills() {
        let fs = pm_ana().policy().filesystem_policy.unwrap();
        for p in [
            "/sandbox/master/people/ana/AGENTS.md",
            "/sandbox/skills/ask",
            "/sandbox/skills/meeting-notes",
            "/sandbox/skills/prd-draft",
        ] {
            assert!(fs.read_only.contains(&p.to_string()), "{p}");
        }
    }

    #[test]
    fn credentials_are_the_bots_own_providers() {
        let p = docs().policy();
        let bound = |rule: &str| {
            p.network_policies[rule].endpoints[0]
                .credential_binding
                .as_ref()
                .map(|b| b.provider.clone())
        };
        assert_eq!(bound("pmos_gateway").as_deref(), Some("pmos-docs-gateway"));
        assert_eq!(bound("pmos_runtime").as_deref(), Some("pmos-docs-worker"));
        // The session token is per task and comes in the claim response: no provider.
        assert_eq!(bound("pmos_toolserver"), None);
        let spec = docs().spec();
        let names: Vec<_> = spec.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["pmos-docs-gateway", "pmos-docs-worker"]);
        let envs: Vec<_> = spec
            .providers
            .iter()
            .map(|p| p.credential.as_str())
            .collect();
        assert_eq!(envs, ["PMOS_GATEWAY_TOKEN", "PMOS_WORKER_TOKEN"]);
        // Two bots never share a provider.
        let other = pm_ana().spec();
        assert!(other
            .providers
            .iter()
            .all(|p| !names.contains(&p.name.as_str())));
    }

    #[test]
    fn the_process_is_non_root_and_landlock_is_required() {
        let p = docs().policy();
        let proc = p.process.unwrap();
        assert_eq!(proc.run_as_user.as_deref(), Some("sandbox"));
        assert_eq!(proc.run_as_group.as_deref(), Some("sandbox"));
        assert_eq!(p.landlock.unwrap().compatibility, "hard_requirement");
    }

    #[test]
    fn rendering_is_deterministic_and_reads_back_as_the_same_policy() {
        let text = docs().render_policy();
        assert_eq!(text, docs().render_policy());
        assert!(text.starts_with(GENERATED_MARKER));
        assert_eq!(openshell::parse(&text).unwrap(), docs().policy());
        let spec = docs().render_spec();
        assert_eq!(crate::spec::parse(&spec).unwrap(), docs().spec());
        // Keys are sorted at every level.
        let body: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        let top: Vec<&str> = body
            .iter()
            .filter(|l| !l.starts_with(' '))
            .map(|l| l.split(':').next().unwrap())
            .collect();
        let mut sorted = top.clone();
        sorted.sort();
        assert_eq!(top, sorted);
        // MCP revisions are quoted, so no YAML 1.1 reader turns them into dates.
        assert!(text.contains("\"2025-11-25\""));
    }

    #[test]
    fn provider_profiles_are_endpointless_bearer_credentials() {
        let profiles = provider_profiles();
        assert_eq!(
            profiles.keys().collect::<Vec<_>>(),
            ["providers/pmos-gateway.yaml", "providers/pmos-worker.yaml"]
        );
        for text in profiles.values() {
            let v: serde_json::Value = serde_norway::from_str(text).unwrap();
            assert!(v.get("endpoints").is_none() && v.get("binaries").is_none());
            assert_eq!(v["credentials"][0]["auth_style"], "bearer");
        }
        let worker: serde_json::Value =
            serde_norway::from_str(&profiles["providers/pmos-worker.yaml"]).unwrap();
        assert_eq!(worker["credentials"][0]["env_vars"][0], "PMOS_WORKER_TOKEN");
    }

    #[test]
    fn write_updates_changed_files_and_removes_only_generated_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let mut generated = Generated {
            bots: vec![docs()],
            files: BTreeMap::new(),
        };
        generated
            .files
            .insert("bots/docs.yaml".into(), docs().render_policy());
        let s = write(dir.path(), &generated).unwrap();
        assert_eq!(s.written, ["bots/docs.yaml"]);
        let s = write(dir.path(), &generated).unwrap();
        assert_eq!(s.unchanged, ["bots/docs.yaml"]);

        fs::write(dir.path().join("bots/gone.yaml"), pm_ana().render_policy()).unwrap();
        fs::write(dir.path().join("bots/notes.yaml"), "# hand-written\n").unwrap();
        let s = write(dir.path(), &generated).unwrap();
        assert_eq!(s.removed, ["bots/gone.yaml"]);
        assert!(dir.path().join("bots/notes.yaml").exists());
    }
}
