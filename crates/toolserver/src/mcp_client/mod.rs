//! The tool server as an MCP **client** of external MCP servers (`docs/mcp-servers-spec.md`).
//!
//! A sandboxed bot still reaches only the tool server; the tool server reaches a server only
//! through its reviewed record (`content/mcp/<id>.yaml`, `pmos_contract::mcp`), and only for the
//! tools the record approves by exact name and definition digest. Every guarantee of the tool
//! server stays in the tool server: an MCP server is a backend that executes calls
//! ([`connector::McpConnector`] behind the `Connector` trait), never something that decides.
//!
//! - [`digest`]: a tool definition's digest.
//! - [`mapping`]: registry arguments to upstream arguments, results to items, errors.
//! - [`transport`]: HTTPS only for remote servers, pinned identity, no redirects.
//! - [`handler`]: no client capabilities; every server request refused and audited.
//! - [`server`]: discovery, the allowlist, re-lists, caps, deadlines, retries.
//! - [`connector`]: a registry system on a server, with PM OS's own rules around it.
//! - [`fake`]: the fake server and its records.
//! - [`oauth`]: per-PM OAuth to a server, tokens in the vault.
//! - [`compare`]: the built-in and MCP backends side by side, reads only.

pub mod compare;
pub mod connector;
pub mod digest;
pub mod fake;
pub mod handler;
pub mod host;
pub mod mapping;
pub mod oauth;
pub mod server;
pub mod split;
pub mod transport;

use std::sync::Arc;

use pmos_contract::mcp::{AuthKind, Operator, ServerRecord};
use pmos_contract::Sensitivity;

use crate::audit::AuditSink;
use crate::connectors::Connector;
use crate::users::{Directory, UserRole};
use crate::vault::Secret;

pub use connector::McpConnector;
pub use server::{McpServer, ServerAuth, UserTokens};

/// The systems a deployment may switch onto MCP in this build (`PMOS_MCP_SYSTEMS`): Jira
/// (slice M6), the public docs (slice M3), Confluence and Slack (slice M8; Slack's posts only
/// when its record approves the audience checks).
pub const SWITCHABLE_SYSTEMS: [&str; 4] = ["jira", "docs", "confluence", "slack"];

/// The systems the fake MCP server serves (`fake_records`): `PMOS_MCP_SYSTEMS` and
/// `PMOS_MCP_COMPARE` with fake connectors.
pub const FAKE_SYSTEMS: [&str; 3] = ["jira", "confluence", "slack"];

/// Policy Admin's view of `servers` (`GET /internal/admin/mcp`).
pub fn policy_view(servers: &[Arc<McpServer>]) -> serde_json::Value {
    let out: Vec<serde_json::Value> = servers
        .iter()
        .map(|s| {
            let r = s.record();
            let st = s.status();
            let state_of = |upstream: &str| {
                st.tools
                    .iter()
                    .find(|t| t.upstream == upstream)
                    .map(|t| t.state)
                    .unwrap_or("unchecked")
            };
            serde_json::json!({
                "id": r.id,
                "operator": r.operator.as_str(),
                "host": pmos_contract::mcp::parse_url(&r.url).map(|u| u.host).unwrap_or_default(),
                "auth": r.auth.as_str(),
                "scopes": r.oauth.as_ref().map(|o| o.scopes.clone()).unwrap_or_default(),
                "data_class": r.data_class,
                "systems": r.systems,
                "hosts": r.hosts(),
                "egress": r.egress,
                "approved_by": r.approved_by,
                "pull": r.pull,
                "tools": r.tools.iter().map(|t| serde_json::json!({
                    "registry": t.registry,
                    "upstream": t.upstream,
                    "class": match t.class() { pmos_contract::mcp::ToolClass::Read => "read", pmos_contract::mcp::ToolClass::Write => "write" },
                    "digest": t.digest,
                    "state": state_of(&t.upstream),
                    "approved_by": t.approved_by,
                })).collect::<Vec<_>>(),
                // Slice M8: the check tools PM OS calls itself (never listed to a bot).
                "checks": r.checks.iter().map(|c| serde_json::json!({
                    "role": c.role.as_str(),
                    "upstream": c.upstream,
                    "class": if c.class == Some(pmos_contract::mcp::ToolClass::Read) { "read" } else { "write" },
                    "digest": c.digest,
                    "state": state_of(&c.upstream),
                    "approved_by": c.approved_by,
                })).collect::<Vec<_>>(),
                "slack_posts": if r.systems.iter().any(|s| s == "slack") {
                    serde_json::Value::String(if r.slack_audience() { "mcp" } else { "builtin" }.into())
                } else {
                    serde_json::Value::Null
                },
                "last_check": {
                    "checked_at": st.checked_at,
                    "outcome": st.outcome,
                    "server_name": st.server_name,
                    "server_version": st.server_version,
                },
                "refused_requests": st.refused_requests,
                "dropped_notifications": st.dropped_notifications,
            })
        })
        .collect();
    serde_json::Value::Array(out)
}

/// `/readyz`'s `mcp` section: per server `ok`, `changed` (an approved tool changed or went
/// away), `down` (no connection), or `per_user` (checked at each PM's link), and its hosts.
pub async fn readiness(servers: &[Arc<McpServer>]) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for s in servers {
        let r = s.record();
        let status = if r.auth == AuthKind::OauthPerUser {
            "per_user"
        } else {
            match s.check().await {
                Err(_) => "down",
                Ok(()) if s.status().tools.iter().any(|t| t.state != "approved") => "changed",
                Ok(()) => "ok",
            }
        };
        out.insert(
            r.id.clone(),
            serde_json::json!({"status": status, "hosts": r.hosts()}),
        );
    }
    serde_json::Value::Object(out)
}

/// Each record's `approved_by`, and each of its tools', must be an active user with the Admin
/// role (the owner's decision of 2026-11-24).
pub fn check_approvals(records: &[ServerRecord], dir: &Directory) -> Result<(), Vec<String>> {
    let admin = |h: &str| {
        dir.get(h.trim())
            .is_some_and(|u| u.active() && u.has(UserRole::Admin))
    };
    let mut out = Vec::new();
    for r in records {
        if !admin(&r.approved_by) {
            out.push(format!(
                "mcp/{}.yaml: approved_by `{}` is not an active Admin in the user database",
                r.id, r.approved_by
            ));
        }
        for t in &r.tools {
            if !admin(&t.approved_by) {
                out.push(format!(
                    "mcp/{}.yaml: tool `{}` approved_by `{}` is not an active Admin in the user database",
                    r.id, t.registry, t.approved_by
                ));
            }
        }
        // Slice M8: a check tool is approved like a mapped one.
        for c in &r.checks {
            if !admin(&c.approved_by) {
                out.push(format!(
                    "mcp/{}.yaml: check `{}` approved_by `{}` is not an active Admin in the user database",
                    r.id,
                    c.role.as_str(),
                    c.approved_by
                ));
            }
        }
    }
    if out.is_empty() {
        Ok(())
    } else {
        Err(out)
    }
}

/// Records the tool server refuses to start with, whatever the mode: every `third_party`
/// record, until the owner decides where an approval of one is kept (§3, threat 8).
pub fn startup_refusals(records: &[ServerRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|r| r.operator == Operator::ThirdParty)
        .map(|r| {
            format!(
                "mcp/{}.yaml: a third_party server is refused until the owner's recorded approval can be checked (docs/mcp-servers-spec.md §3, threat 8)",
                r.id
            )
        })
        .collect()
}

/// Fake-data mode (`PMOS_MODEL_SOURCE=hosted-test`) allows no MCP server but the built-in
/// fakes and a `pmos-docs`-shaped record: run by PM OS, the public docs only, no per-user
/// OAuth. Every other record refuses the start (§4).
pub fn fake_data_refusals(records: &[ServerRecord]) -> Vec<String> {
    records
        .iter()
        .filter(|r| {
            !(r.operator == Operator::Pmos
                && r.auth != AuthKind::OauthPerUser
                && r.internal()
                && r.data_class == Sensitivity::Public
                && r.systems.iter().all(|s| s == "docs"))
        })
        .map(|r| {
            pmos_contract::model_source::fake_data_refusal(&format!(
                "mcp/{}.yaml must not be loaded: only fake MCP servers and pmos-docs are allowed with fake data",
                r.id
            ))
        })
        .collect()
}

/// Would `records` connect to a server holding real data? (The `held_real_data` marker is
/// written when one is enabled.)
pub fn real_data(records: &[ServerRecord], enabled: &[String]) -> bool {
    records.iter().any(|r| {
        r.systems.iter().any(|s| enabled.contains(s))
            && (r.operator != Operator::Pmos || r.auth == AuthKind::OauthPerUser)
    })
}

/// The connectors for `systems`, each from the one record that serves it. `secret` reads a
/// record's `secret_env`; `tokens` gives per-PM tokens for `oauth_per_user` records.
pub fn connectors_for(
    records: &[ServerRecord],
    systems: &[String],
    secret: impl Fn(&str) -> Option<String>,
    tokens: Option<Arc<dyn UserTokens>>,
    audit: Option<Arc<dyn AuditSink>>,
    routes: &transport::TestRoutes,
) -> Result<Served, String> {
    let mut out = Served {
        connectors: Vec::new(),
        servers: Vec::new(),
        reads_only: Vec::new(),
    };
    for system in systems {
        let record = records
            .iter()
            .find(|r| r.systems.contains(system))
            .ok_or_else(|| format!("PMOS_MCP_SYSTEMS names `{system}`, but no approved server record in mcp/ serves it"))?;
        let server = match out.servers.iter().find(|s| s.record().id == record.id) {
            Some(s) => s.clone(),
            None => {
                let auth = match record.auth {
                    AuthKind::None => ServerAuth::None,
                    AuthKind::ServiceToken => {
                        let env = record.secret_env.as_deref().unwrap_or_default();
                        let token = secret(env)
                            .filter(|t| !t.trim().is_empty())
                            .ok_or_else(|| format!("mcp/{}.yaml needs {env} set", record.id))?;
                        ServerAuth::Token(Secret::new(token))
                    }
                    AuthKind::OauthPerUser => ServerAuth::PerUser(tokens.clone().ok_or_else(|| {
                        format!("mcp/{}.yaml uses per-user OAuth, which needs live connectors and the vault", record.id)
                    })?),
                };
                let s = Arc::new(McpServer::new(record.clone(), auth, audit.clone(), routes)?);
                out.servers.push(s.clone());
                s
            }
        };
        out.push(McpConnector::new(system, server, "mcp")?);
    }
    Ok(out)
}

/// Connectors served over MCP, and the servers behind them (for Policy Admin's view).
pub struct Served {
    /// One per system. A Slack whose record lacks the audience checks is here as its MCP
    /// connector, which refuses posts (`Unsupported`); [`Served::over`] pairs it with the
    /// built-in Slack connector.
    pub connectors: Vec<Arc<dyn Connector>>,
    pub servers: Vec<Arc<McpServer>>,
    /// The systems served for reads only (Slack without both audience checks, slice M8).
    pub reads_only: Vec<Arc<McpConnector>>,
}

impl Served {
    fn push(&mut self, c: McpConnector) {
        let c = Arc::new(c);
        if !c.posts_on_mcp() {
            self.reads_only.push(c.clone());
        }
        self.connectors.push(c);
    }

    /// The served connectors, to take the place of `builtin`'s for their systems. A
    /// reads-only Slack is paired with the built-in Slack connector, which keeps its posts,
    /// the audience check and a draft's target label ([`split::ReadsOnly`]).
    pub fn over(&self, builtin: &[Arc<dyn Connector>]) -> Result<Vec<Arc<dyn Connector>>, String> {
        self.connectors
            .iter()
            .map(|c| {
                let Some(mcp) = self.reads_only.iter().find(|r| r.system() == c.system()) else {
                    return Ok(c.clone());
                };
                let old = builtin
                    .iter()
                    .find(|b| b.system() == c.system())
                    .cloned()
                    .ok_or_else(|| format!("{}'s posts need the built-in connector", c.system()))?;
                Ok(Arc::new(split::ReadsOnly::new(mcp.clone(), old)?) as Arc<dyn Connector>)
            })
            .collect()
    }
}

/// `PMOS_CONNECTORS=fake-mcp`: the fake server in this process on a loopback port, with a fresh
/// random token, and Jira served through it (slice M6).
pub async fn start_fake(
    audit: Option<Arc<dyn AuditSink>>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(Served, fake::FakeMcp), String> {
    start_fake_for(&["jira".to_string()], fake::FakeMcp::new(), audit, cancel).await
}

/// [`start_fake`] for `systems` (any of [`FAKE_SYSTEMS`]), served by `fake` (slice M8).
pub async fn start_fake_for(
    systems: &[String],
    fake: fake::FakeMcp,
    audit: Option<Arc<dyn AuditSink>>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(Served, fake::FakeMcp), String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("random token: {e}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let addr = fake::serve(
        fake.clone(),
        vec![fake::FAKE_HOST.to_string()],
        Some(token.clone()),
        cancel,
    )
    .await
    .map_err(|e| format!("the fake MCP server: {e}"))?;
    let records = fake::fake_records(addr);
    let mut served = Served {
        connectors: Vec::new(),
        servers: Vec::new(),
        reads_only: Vec::new(),
    };
    for system in systems {
        if !FAKE_SYSTEMS.contains(&system.as_str()) {
            return Err(format!("the fake MCP server doesn't serve `{system}`"));
        }
        let record = records
            .iter()
            .find(|r| r.id == format!("fake-{system}"))
            .cloned()
            .ok_or_else(|| format!("no fake-{system} record"))?;
        let server = Arc::new(McpServer::new(
            record,
            ServerAuth::Token(Secret::new(token.clone())),
            audit.clone(),
            &transport::TestRoutes::default(),
        )?);
        served.push(McpConnector::new(system, server.clone(), "fake-mcp")?);
        served.servers.push(server);
    }
    Ok((served, fake))
}
