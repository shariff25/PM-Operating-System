//! Old and new side by side, for reads, on fake data only (`docs/mcp-servers-spec.md` slice M7,
//! and the owner's decision of 2026-11-24).
//!
//! [`SideBySide`] serves a system from its built-in backend (the in-process fake) and sends each
//! read to the MCP backend too, at the same time. The caller always gets the built-in answer;
//! the two answers are compared and the comparison recorded as an audit event `mcp.compare`:
//! counts, the ids only one side returned, and the names of the fields that differ. Never a
//! value, a title or a body. Writes, audience checks and readiness go to the built-in backend
//! only: a write is never sent twice.
//!
//! `config.rs` allows it only with fake connectors, so only fixtures are compared.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;

use crate::audit::{AuditEvent, AuditSink, Client, Outcome};
use crate::connectors::{
    Audience, Caller, Connector, ConnectorError, Conversation, Item, SourceRef,
};

/// The audit event of one comparison.
pub const COMPARE_EVENT: &str = "mcp.compare";

/// The most ids or field names one comparison lists.
const MAX_LISTED: usize = 20;

/// How two answers to one read compare: metadata only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub same: bool,
    /// Items each side returned (`None` for an error).
    pub builtin: Option<usize>,
    pub mcp: Option<usize>,
    /// The error kinds, when either side failed.
    pub builtin_error: Option<&'static str>,
    pub mcp_error: Option<&'static str>,
    pub only_builtin: Vec<String>,
    pub only_mcp: Vec<String>,
    /// `<id>.<field path>` for each field whose value differs on an item both returned.
    pub fields: Vec<String>,
}

fn kind(e: &ConnectorError) -> &'static str {
    match e {
        ConnectorError::NotFound(_) => "not_found",
        ConnectorError::BadArgs(_) => "bad_args",
        ConnectorError::Unsupported(_) => "unsupported",
        ConnectorError::Unavailable(_) => "unavailable",
        ConnectorError::NotConnected(_) => "not_connected",
        ConnectorError::Reconnect(_) => "reconnect",
        ConnectorError::Forbidden(_) => "forbidden",
        ConnectorError::RateLimited(_) => "rate_limited",
        ConnectorError::NotConfigured(_) => "not_configured",
    }
}

/// The paths at which two values differ (objects by key, everything else whole).
fn diff(prefix: &str, a: &Value, b: &Value, out: &mut BTreeSet<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()) {
                let p = format!("{prefix}.{k}");
                diff(
                    &p,
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        _ if a != b => {
            out.insert(prefix.to_string());
        }
        _ => {}
    }
}

/// Compare the built-in answer with the MCP one.
pub fn compare(
    builtin: &Result<Vec<Item>, ConnectorError>,
    mcp: &Result<Vec<Item>, ConnectorError>,
) -> Comparison {
    let mut c = Comparison {
        same: true,
        builtin: builtin.as_ref().ok().map(Vec::len),
        mcp: mcp.as_ref().ok().map(Vec::len),
        builtin_error: builtin.as_ref().err().map(kind),
        mcp_error: mcp.as_ref().err().map(kind),
        only_builtin: Vec::new(),
        only_mcp: Vec::new(),
        fields: Vec::new(),
    };
    match (builtin, mcp) {
        (Ok(a), Ok(b)) => {
            let by_id = |items: &[Item]| -> BTreeMap<String, Item> {
                items
                    .iter()
                    .map(|i| (format!("{}:{}", i.source.system, i.source.id), i.clone()))
                    .collect()
            };
            let (a_ids, b_ids) = (by_id(a), by_id(b));
            c.only_builtin = a_ids
                .keys()
                .filter(|k| !b_ids.contains_key(*k))
                .take(MAX_LISTED)
                .cloned()
                .collect();
            c.only_mcp = b_ids
                .keys()
                .filter(|k| !a_ids.contains_key(*k))
                .take(MAX_LISTED)
                .cloned()
                .collect();
            let mut fields = BTreeSet::new();
            for (id, x) in &a_ids {
                if let Some(y) = b_ids.get(id) {
                    let xv = serde_json::to_value(x).unwrap_or_default();
                    let yv = serde_json::to_value(y).unwrap_or_default();
                    diff(id, &xv, &yv, &mut fields);
                }
            }
            c.fields = fields.into_iter().take(MAX_LISTED).collect();
            let order_same = a
                .iter()
                .map(|i| &i.source.id)
                .eq(b.iter().map(|i| &i.source.id));
            c.same = a.len() == b.len()
                && c.only_builtin.is_empty()
                && c.only_mcp.is_empty()
                && c.fields.is_empty()
                && order_same;
        }
        (Err(x), Err(y)) => c.same = kind(x) == kind(y),
        _ => c.same = false,
    }
    c
}

impl Comparison {
    /// The audit detail: fixed words, counts, ids and field names.
    pub fn detail(&self, tool: &str) -> String {
        let side = |n: Option<usize>, e: Option<&str>| match (n, e) {
            (Some(n), _) => format!("{n} items"),
            (None, Some(e)) => format!("error {e}"),
            _ => "nothing".into(),
        };
        let mut out = format!(
            "{tool}: {}; builtin {}, mcp {}",
            if self.same { "same" } else { "differ" },
            side(self.builtin, self.builtin_error),
            side(self.mcp, self.mcp_error)
        );
        if !self.only_builtin.is_empty() {
            out.push_str(&format!("; only builtin: {}", self.only_builtin.join(", ")));
        }
        if !self.only_mcp.is_empty() {
            out.push_str(&format!("; only mcp: {}", self.only_mcp.join(", ")));
        }
        if !self.fields.is_empty() {
            out.push_str(&format!("; fields: {}", self.fields.join(", ")));
        }
        out
    }
}

/// A system served by `builtin`, with each read compared against `mcp`.
pub struct SideBySide {
    builtin: Arc<dyn Connector>,
    mcp: Arc<dyn Connector>,
    audit: Arc<dyn AuditSink>,
}

impl SideBySide {
    pub fn new(
        builtin: Arc<dyn Connector>,
        mcp: Arc<dyn Connector>,
        audit: Arc<dyn AuditSink>,
    ) -> Self {
        assert_eq!(builtin.system(), mcp.system(), "one system on both sides");
        Self {
            builtin,
            mcp,
            audit,
        }
    }
}

#[async_trait]
impl Connector for SideBySide {
    fn system(&self) -> &str {
        self.builtin.system()
    }

    fn mode(&self) -> &'static str {
        self.builtin.mode()
    }

    async fn read(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        let (builtin, mcp) = tokio::join!(
            self.builtin.read(caller, action, args),
            self.mcp.read(caller, action, args)
        );
        let tool = format!("{}.{action}", self.system());
        let c = compare(&builtin, &mcp);
        let event = AuditEvent {
            at: Utc::now(),
            client: Client::System,
            run_id: caller.run_id.to_string(),
            user: caller.user.to_string(),
            skill: None,
            bot: caller.team_bot.map(|b| b.name.to_string()),
            persona: None,
            tool: COMPARE_EVENT.into(),
            outcome: if c.same { Outcome::Ok } else { Outcome::Error },
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: Some(crate::approvals::args_hash(args)),
            output_ref: None,
            detail: Some(c.detail(&tool)),
        };
        if let Err(e) = self.audit.record(event).await {
            tracing::error!(error = %e, "a side-by-side comparison could not be audited");
        }
        builtin
    }

    async fn write(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<SourceRef, ConnectorError> {
        // One backend only, never both.
        self.builtin.write(caller, action, args).await
    }

    async fn audience(
        &self,
        caller: Caller<'_>,
        channel: &str,
    ) -> Result<Audience, ConnectorError> {
        self.builtin.audience(caller, channel).await
    }

    async fn conversation(
        &self,
        caller: Caller<'_>,
        channel: &str,
    ) -> Result<Conversation, ConnectorError> {
        self.builtin.conversation(caller, channel).await
    }

    async fn ready(&self) -> Result<(), String> {
        self.builtin.ready().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(id: &str, status: &str) -> Item {
        Item {
            source: SourceRef {
                system: "jira".into(),
                id: id.into(),
                url: String::new(),
            },
            title: "secret title".into(),
            content: json!({"status": status, "description": "secret body"}),
        }
    }

    #[test]
    fn comparisons_name_ids_and_fields_never_values() {
        let a = Ok(vec![item("PM-1", "To Do"), item("PM-2", "Done")]);
        assert!(compare(&a, &a).same);
        let b = Ok(vec![item("PM-1", "In Progress"), item("PM-3", "Done")]);
        let c = compare(&a, &b);
        assert!(!c.same);
        assert_eq!(c.only_builtin, vec!["jira:PM-2"]);
        assert_eq!(c.only_mcp, vec!["jira:PM-3"]);
        assert_eq!(c.fields, vec!["jira:PM-1.content.status"]);
        let detail = c.detail("jira.search");
        assert_eq!(
            detail,
            "jira.search: differ; builtin 2 items, mcp 2 items; only builtin: jira:PM-2; only mcp: jira:PM-3; fields: jira:PM-1.content.status"
        );
        assert!(!detail.contains("secret") && !detail.contains("In Progress"));
        let e = compare(
            &Err(ConnectorError::NotFound("x".into())),
            &Err(ConnectorError::NotFound("y".into())),
        );
        assert!(e.same, "the same error kind; the texts may differ");
        assert!(!compare(&a, &Err(ConnectorError::Unavailable("down".into()))).same);
    }
}
