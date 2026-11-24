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
pub mod mapping;
pub mod oauth;
pub mod server;
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
/// (slice M6) and the public docs (slice M3). Confluence and Slack wait for slice M8.
pub const SWITCHABLE_SYSTEMS: [&str; 2] = ["jira", "docs"];

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
) -> Result<Vec<Arc<dyn Connector>>, String> {
    let mut out: Vec<Arc<dyn Connector>> = Vec::new();
    let mut servers: Vec<Arc<McpServer>> = Vec::new();
    for system in systems {
        let record = records
            .iter()
            .find(|r| r.systems.contains(system))
            .ok_or_else(|| format!("PMOS_MCP_SYSTEMS names `{system}`, but no approved server record in mcp/ serves it"))?;
        let server = match servers.iter().find(|s| s.record().id == record.id) {
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
                servers.push(s.clone());
                s
            }
        };
        out.push(Arc::new(McpConnector::new(system, server, "mcp")?));
    }
    Ok(out)
}

/// `PMOS_CONNECTORS=fake-mcp`: the fake server in this process on a loopback port, with a fresh
/// random token, and Jira served through it. Confluence, Slack and the docs stay in-process
/// fakes (slice M8 moves the rest).
pub async fn start_fake(
    audit: Option<Arc<dyn AuditSink>>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<(Vec<Arc<dyn Connector>>, fake::FakeMcp), String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("random token: {e}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let fake = fake::FakeMcp::new();
    let addr = fake::serve(
        fake.clone(),
        vec![fake::FAKE_HOST.to_string()],
        Some(token.clone()),
        cancel,
    )
    .await
    .map_err(|e| format!("the fake MCP server: {e}"))?;
    let records = fake::fake_records(addr);
    let jira = records
        .iter()
        .find(|r| r.id == "fake-jira")
        .cloned()
        .ok_or("no fake-jira record")?;
    let server = Arc::new(McpServer::new(
        jira,
        ServerAuth::Token(Secret::new(token)),
        audit,
        &transport::TestRoutes::default(),
    )?);
    let connector: Arc<dyn Connector> = Arc::new(McpConnector::new("jira", server, "fake-mcp")?);
    Ok((vec![connector], fake))
}
