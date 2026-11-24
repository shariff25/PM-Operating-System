//! `pmos-toolserver mcp digests <record.yaml>`: for an operator helping an Admin prepare or
//! review a server record. It connects to the record's server exactly as the tool server would
//! (same transport rules, same refusals), lists its tools, and prints each tool's name and
//! definition digest, then how each tool the record approves compares. It changes nothing and
//! approves nothing: an approval is a record merged by pull request.
//!
//! Only `service_token` and `none` servers: a per-user server needs a PM's own token, and its
//! last check is in Policy Admin's view instead. The token is read from the record's
//! `secret_env` and never printed.

use std::io::Write;

use pmos_contract::mcp::AuthKind;

use super::server::{McpServer, ServerAuth};
use super::transport::TestRoutes;
use crate::vault::Secret;

const USAGE: &str = "usage: pmos-toolserver mcp digests <record.yaml>";

/// Run the command with `args` (after `mcp`), reading settings from `env`; the exit code.
pub async fn run(
    args: &[String],
    env: impl Fn(&str) -> Option<String>,
    out: &mut dyn Write,
) -> i32 {
    let [cmd, path] = args else {
        let _ = writeln!(out, "{USAGE}");
        return 2;
    };
    if cmd != "digests" {
        let _ = writeln!(out, "{USAGE}");
        return 2;
    }
    let record = match std::fs::read_to_string(path)
        .map_err(|e| format!("{path}: {e}"))
        .and_then(|t| pmos_contract::mcp::parse(&t))
    {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(out, "{e}");
            return 1;
        }
    };
    let auth = match record.auth {
        AuthKind::None => ServerAuth::None,
        AuthKind::ServiceToken => {
            let name = record.secret_env.clone().unwrap_or_default();
            match env(&name).filter(|t| !t.trim().is_empty()) {
                Some(t) => ServerAuth::Token(Secret::new(t)),
                None => {
                    let _ = writeln!(out, "set {name} (the record's secret_env) first");
                    return 1;
                }
            }
        }
        AuthKind::OauthPerUser => {
            let _ = writeln!(
                out,
                "`{}` takes each PM's own token: see its last check in Policy Admin instead",
                record.id
            );
            return 1;
        }
    };
    let server = match McpServer::new(record, auth, None, &TestRoutes::default()) {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(out, "{e}");
            return 1;
        }
    };
    if let Err(e) = server.check().await {
        let _ = writeln!(out, "{}: {e}", server.record().id);
        return 1;
    }
    let status = server.status();
    let _ = writeln!(
        out,
        "{} lists {} tools (it calls itself {} {}; that is its own claim):",
        server.record().id,
        status.listed.len(),
        status.server_name.as_deref().unwrap_or("?"),
        status.server_version.as_deref().unwrap_or("?")
    );
    for (name, digest) in &status.listed {
        let _ = writeln!(out, "  {name} {digest}");
    }
    let _ = writeln!(out, "approved in the record:");
    let mut all_approved = true;
    for t in &status.tools {
        all_approved &= t.state == "approved";
        let _ = writeln!(out, "  {} {}", t.upstream, t.state);
    }
    if all_approved {
        0
    } else {
        3
    }
}

/// `main`'s entry: arguments after `mcp`.
pub async fn main(args: Vec<String>) -> i32 {
    let mut stdout = std::io::stdout();
    run(&args, |k| std::env::var(k).ok(), &mut stdout).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_client::fake::{self, FakeMcp, FAKE_HOST};

    #[tokio::test]
    async fn digests_are_listed_and_compared_without_printing_the_token() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = "host-command-token-0123456789abcdef01234";
        let fake = FakeMcp::new();
        let addr = fake::serve(
            fake.clone(),
            vec![FAKE_HOST.into()],
            Some(token.into()),
            cancel.clone(),
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake-jira.yaml");
        let record = fake::fake_records(addr).remove(0);
        std::fs::write(&path, serde_norway::to_string(&record).unwrap()).unwrap();
        let args = vec!["digests".to_string(), path.display().to_string()];
        let env = |k: &str| (k == "PMOS_MCP_TOKEN_FAKE").then(|| token.to_string());
        let mut out = Vec::new();
        assert_eq!(run(&args, env, &mut out).await, 0);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(&format!(
            "jira_delete_project {}",
            FakeMcp::digest("jira_delete_project")
        )));
        assert!(text.contains("jira_search_issues approved"), "{text}");
        assert!(!text.contains(token));
        fake.tricks
            .change_search
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut out = Vec::new();
        assert_eq!(run(&args, env, &mut out).await, 3, "a changed tool");
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("jira_search_issues changed"));
        let mut out = Vec::new();
        assert_eq!(run(&args, |_| None, &mut out).await, 1, "no token");
        cancel.cancel();
    }
}
