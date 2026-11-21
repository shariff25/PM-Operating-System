//! A task's audit events, read from the tool server's JSON-lines audit file
//! (`PMOS_AUDIT_LOG` there, mounted read-only here as `PMOS_CONSOLE_AUDIT_LOG`).
//!
//! The tool server has no endpoint that reads its audit log back, so the console reads the
//! file it already writes for shipping off-box. Events are metadata only (no message
//! bodies), and a task's run id is its task id.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The most events one task page shows.
pub const MAX_EVENTS: usize = 500;

#[derive(Debug, Clone, Deserialize)]
pub struct AuditEvent {
    pub at: String,
    #[serde(default)]
    pub client: String,
    pub run_id: String,
    #[serde(default)]
    pub user: String,
    /// `null` for a bot session, which names its bot instead.
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub bot: Option<String>,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub output_ref: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}

/// The audit events for `run_id`, oldest first, at most [`MAX_EVENTS`]; and whether more
/// were left out. Lines that aren't events are skipped.
pub async fn events_for_run(
    path: PathBuf,
    run_id: String,
) -> Result<(Vec<AuditEvent>, bool), String> {
    tokio::task::spawn_blocking(move || scan(&path, &run_id))
        .await
        .map_err(|e| format!("reading the audit log: {e}"))?
}

fn scan(path: &Path, run_id: &str) -> Result<(Vec<AuditEvent>, bool), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut events = Vec::new();
    let mut more = false;
    // A cheap substring check first: most lines belong to other runs.
    let needle = format!("\"{run_id}\"");
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
        if !line.contains(&needle) {
            continue;
        }
        let Ok(event) = serde_json::from_str::<AuditEvent>(&line) else {
            continue;
        };
        if event.run_id != run_id {
            continue;
        }
        if events.len() == MAX_EVENTS {
            more = true;
            break;
        }
        events.push(event);
    }
    Ok((events, more))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn keeps_only_the_runs_events() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"at":"2026-11-14T08:00:00Z","client":"mcp","run_id":"tsk_a","user":"ana","skill":"docs","tool":"docs.search","outcome":"ok"}}"#
        )
        .unwrap();
        writeln!(file, "not json").unwrap();
        writeln!(
            file,
            r#"{{"at":"2026-11-14T08:00:01Z","client":"mcp","run_id":"tsk_b","user":"ben","skill":"docs","tool":"docs.search","outcome":"ok","detail":"mentions \"tsk_a\""}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"at":"2026-11-14T08:00:02Z","client":"orchestrator","run_id":"tsk_a","user":"ana","skill":"docs","tool":"jira.create_issue","outcome":"pending","output_ref":"apr_1"}}"#
        )
        .unwrap();
        let (events, more) = events_for_run(file.path().into(), "tsk_a".into())
            .await
            .unwrap();
        assert!(!more);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].outcome, "pending");
        assert_eq!(events[1].output_ref.as_deref(), Some("apr_1"));
        assert!(events_for_run("/no/such/file".into(), "tsk_a".into())
            .await
            .is_err());
    }

    /// A bot task's audit line, as the tool server writes it (`skill: null`, `bot` set), must
    /// not be skipped.
    #[tokio::test]
    async fn reads_bot_session_lines_as_the_tool_server_writes_them() {
        use pmos_toolserver::audit::{AuditEvent as Written, Client, Outcome};
        let written = Written {
            at: chrono::Utc::now(),
            client: Client::Mcp,
            run_id: "tsk_bot".into(),
            user: "ana".into(),
            skill: None,
            bot: Some("docs".into()),
            // A persona session's line carries `persona`; the console reads past it.
            persona: Some("chief".into()),
            tool: "docs.search".into(),
            outcome: Outcome::Ok,
            sources: vec![],
            source_titles: Default::default(),
            args_hash: None,
            output_ref: None,
            detail: None,
        };
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", serde_json::to_string(&written).unwrap()).unwrap();
        let (events, _) = events_for_run(file.path().into(), "tsk_bot".into())
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].bot.as_deref(), Some("docs"));
        assert_eq!(events[0].skill, None);
    }
}
