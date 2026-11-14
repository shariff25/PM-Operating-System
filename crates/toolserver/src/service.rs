//! The tool server's rules, independent of transport and storage. MCP (`mcp.rs`) and the
//! internal HTTP API (`app.rs`) are thin layers over [`ToolService`]; the stores behind it are
//! in memory or in Postgres (`store.rs`).
//!
//! Every call goes through these rules:
//! 1. The tool must be one the session's skill declared (PRD: Authorization).
//! 2. Context reads return nothing above the skill's `data-class`.
//! 3. Connector output is wrapped as untrusted data.
//! 4. A write runs only with a matching grant or an approval by its own user (OUT-4).
//! 5. Every call and decision is audited (ADM-2).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use pmos_contract::model::CONTEXT_DOMAINS;
use pmos_contract::{Skill, ToolKind};
use serde_json::{json, Value};

use crate::approvals::{args_hash, ApprovalItem, DecideError, Decision, NewItem, Status};
use crate::audit::{AuditEvent, Outcome};
use crate::catalog::{Catalog, ToolSpec};
use crate::connectors::{untrusted, Caller, Connector, ConnectorError};
use crate::context::{load_docs, ContextDoc, ReindexReport};
use crate::memory::valid_key;
use crate::schemas::check_args;
use crate::session::{token_hash, valid_handle, Session};
use crate::store::{StoreError, Stores};

pub const DEFAULT_SESSION_TTL_MINUTES: i64 = 60;
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    #[error("tool `{0}` is not declared by this skill")]
    NotAllowed(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Connector(String),
    /// Storage failed. The call did not complete; it is safe to retry.
    #[error("PM OS storage is unavailable; try again shortly")]
    Unavailable(String),
}

impl From<ConnectorError> for ToolError {
    fn from(e: ConnectorError) -> Self {
        match e {
            ConnectorError::NotFound(id) => ToolError::NotFound(id),
            ConnectorError::BadArgs(m) => ToolError::InvalidArgs(m),
            other => ToolError::Connector(other.to_string()),
        }
    }
}

impl From<StoreError> for ToolError {
    fn from(e: StoreError) -> Self {
        ToolError::Unavailable(e.0)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("unknown skill `{0}`")]
    UnknownSkill(String),
    #[error("invalid user handle `{0}`")]
    InvalidUser(String),
    #[error("run_id must be 1–128 characters")]
    InvalidRun,
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReindexError {
    #[error("content does not pass `pmos lint`:\n{0}")]
    Lint(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecisionError {
    #[error(transparent)]
    Decide(#[from] DecideError),
    #[error("tool `{0}` is no longer served")]
    UnknownTool(String),
}

impl From<StoreError> for DecisionError {
    fn from(e: StoreError) -> Self {
        DecisionError::Decide(DecideError::Store(e))
    }
}

pub struct ToolService {
    catalog: Catalog,
    stores: Stores,
    connectors: HashMap<String, Arc<dyn Connector>>,
    stale_after_days: i64,
}

impl ToolService {
    /// Fails if any registry tool belongs to a system with no handler.
    pub fn new(
        catalog: Catalog,
        stores: Stores,
        connectors: Vec<Arc<dyn Connector>>,
    ) -> Result<Self, String> {
        let connectors: HashMap<String, Arc<dyn Connector>> = connectors
            .into_iter()
            .map(|c| (c.system().to_string(), c))
            .collect();
        for tool in catalog.tools() {
            let served = matches!(tool.system.as_str(), "context" | "memory")
                || connectors.contains_key(&tool.system);
            if !served {
                return Err(format!(
                    "no connector for system `{}` (tool `{}`)",
                    tool.system, tool.name
                ));
            }
        }
        Ok(Self {
            catalog,
            stores,
            connectors,
            stale_after_days: pmos_contract::lint::DEFAULT_STALE_AFTER_DAYS,
        })
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Rebuild the context index from the content repo checkout at `content_root`, after a
    /// merge. Content that fails `pmos lint` is refused and the index is left as it was.
    /// Skill and tool registry changes take effect on the next restart.
    pub async fn reindex(
        &self,
        content_root: &std::path::Path,
        now: DateTime<Utc>,
    ) -> Result<ReindexReport, ReindexError> {
        let report = pmos_contract::lint(
            content_root,
            &pmos_contract::LintOptions::new(now.date_naive()),
        );
        if report.has_errors() {
            let lines: Vec<String> = report
                .errors()
                .map(|d| format!("  {}: {}: {}", d.code, d.path, d.message))
                .collect();
            return Err(ReindexError::Lint(lines.join("\n")));
        }
        Ok(self.stores.context.reindex(load_docs(content_root)).await?)
    }

    /// Record an audit event. A failure is logged at error level: the audit log is a
    /// control, so a platform owner needs to see it, but it never undoes the call itself.
    async fn audit(&self, event: AuditEvent) {
        if let Err(e) = self.stores.audit.record(event.clone()).await {
            tracing::error!(
                error = %e,
                run_id = %event.run_id,
                tool = %event.tool,
                outcome = ?event.outcome,
                "AUDIT EVENT NOT RECORDED"
            );
        }
    }

    // ----- sessions -------------------------------------------------------------------

    pub async fn create_session(
        &self,
        user: &str,
        skill: &str,
        run_id: &str,
        ttl: Option<Duration>,
        now: DateTime<Utc>,
    ) -> Result<Session, SessionError> {
        if !valid_handle(user) {
            return Err(SessionError::InvalidUser(user.to_string()));
        }
        if self.catalog.skill(skill).is_none() {
            return Err(SessionError::UnknownSkill(skill.to_string()));
        }
        if run_id.is_empty() || run_id.len() > 128 {
            return Err(SessionError::InvalidRun);
        }
        let ttl = ttl.unwrap_or_else(|| Duration::minutes(DEFAULT_SESSION_TTL_MINUTES));
        let session = Session::new(user, skill, run_id, ttl, now);
        self.stores.sessions.insert(&session, now).await?;
        Ok(session)
    }

    /// The live session for `token`. A session whose skill is no longer in the catalog
    /// (content changed since it was created) is treated as gone.
    pub async fn session(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Session>, StoreError> {
        Ok(self
            .stores
            .sessions
            .get(token, now)
            .await?
            .filter(|s| self.catalog.skill(&s.skill).is_some()))
    }

    pub async fn revoke_session(&self, token: &str) -> Result<bool, StoreError> {
        self.stores.sessions.revoke(token).await
    }

    fn skill_of(&self, session: &Session) -> Option<&Skill> {
        self.catalog.skill(&session.skill)
    }

    /// The tools this session may call.
    pub fn list_tools(&self, session: &Session) -> Vec<&ToolSpec> {
        self.skill_of(session)
            .map(|skill| self.catalog.tools_for(skill))
            .unwrap_or_default()
    }

    // ----- tool calls -----------------------------------------------------------------

    pub async fn call(
        &self,
        session: &Session,
        harness_name: &str,
        args: Value,
        now: DateTime<Utc>,
    ) -> Result<Value, ToolError> {
        let args = if args.is_null() { json!({}) } else { args };
        let mut event = AuditEvent {
            at: now,
            run_id: session.run_id.clone(),
            user: session.user.clone(),
            skill: session.skill.clone(),
            tool: harness_name.to_string(),
            outcome: Outcome::Ok,
            sources: Vec::new(),
            args_hash: Some(args_hash(&args)),
            output_ref: None,
            detail: None,
        };

        let result = self
            .dispatch(session, harness_name, &args, now, &mut event)
            .await;
        if let Err(e) = &result {
            event.outcome = match e {
                ToolError::UnknownTool(_) | ToolError::NotAllowed(_) | ToolError::Forbidden(_) => {
                    Outcome::Denied
                }
                _ => Outcome::Error,
            };
            event.detail = Some(e.to_string());
        }
        self.audit(event).await;
        result
    }

    async fn dispatch(
        &self,
        session: &Session,
        harness_name: &str,
        args: &Value,
        now: DateTime<Utc>,
        event: &mut AuditEvent,
    ) -> Result<Value, ToolError> {
        let spec = self
            .catalog
            .tool(harness_name)
            .ok_or_else(|| ToolError::UnknownTool(harness_name.to_string()))?;
        event.tool = spec.name.clone();
        let skill = self
            .skill_of(session)
            .ok_or_else(|| ToolError::NotAllowed(spec.harness_name.clone()))?;
        if !skill.tools.contains(&spec.name) {
            return Err(ToolError::NotAllowed(spec.harness_name.clone()));
        }
        check_args(&spec.input_schema, args).map_err(ToolError::InvalidArgs)?;

        match (spec.kind, spec.system.as_str()) {
            (ToolKind::Read, "context") => self.context_tool(skill, spec, args, now, event).await,
            (ToolKind::Read, "memory") => {
                let notes = self.stores.memory.read(&session.user).await?;
                Ok(json!({ "notes": notes }))
            }
            (ToolKind::Internal, "memory") => self.memory_update(session, args, now).await,
            (ToolKind::Read, system) => {
                let connector = self.connector(system)?;
                let caller = Caller {
                    user: &session.user,
                };
                let items = connector.read(caller, spec.action(), args).await?;
                event.sources = items
                    .iter()
                    .map(|i| format!("{}:{}", i.source.system, i.source.id))
                    .collect();
                Ok(json!({ "results": items.iter().map(untrusted).collect::<Vec<_>>() }))
            }
            (ToolKind::Write, _) => self.write(session, spec, args, now, event).await,
            (kind, system) => Err(ToolError::UnknownTool(format!(
                "{} ({kind:?} tool on {system})",
                spec.name
            ))),
        }
    }

    fn connector(&self, system: &str) -> Result<&Arc<dyn Connector>, ToolError> {
        self.connectors
            .get(system)
            .ok_or_else(|| ToolError::Connector(format!("no connector for `{system}`")))
    }

    async fn context_tool(
        &self,
        skill: &Skill,
        spec: &ToolSpec,
        args: &Value,
        now: DateTime<Utc>,
        event: &mut AuditEvent,
    ) -> Result<Value, ToolError> {
        let ceiling = skill.data_class;
        let today = now.date_naive();
        match spec.action() {
            "search" => {
                let query = args["query"].as_str().unwrap_or_default();
                let domain = args["domain"].as_str();
                if let Some(d) = domain {
                    if !CONTEXT_DOMAINS.contains(&d) {
                        return Err(ToolError::InvalidArgs(format!("unknown domain `{d}`")));
                    }
                }
                let limit = args["limit"]
                    .as_u64()
                    .map(|n| n as usize)
                    .unwrap_or(DEFAULT_LIMIT)
                    .min(MAX_LIMIT);
                let hits = self
                    .stores
                    .context
                    .search(query, domain, MAX_LIMIT * 2)
                    .await?;
                let (allowed, withheld): (Vec<_>, Vec<_>) =
                    hits.into_iter().partition(|h| h.doc.sensitivity <= ceiling);
                let allowed: Vec<_> = allowed.into_iter().take(limit).collect();
                event.sources = allowed.iter().map(|h| h.doc.path.clone()).collect();
                if !withheld.is_empty() {
                    // Audited, not shown to the model: the result list simply omits them.
                    event.detail = Some(format!(
                        "{} result(s) above data class {ceiling} withheld",
                        withheld.len()
                    ));
                }
                Ok(json!({
                    "results": allowed.iter().map(|h| {
                        let mut v = self.doc_meta(&h.doc, today);
                        v["snippet"] = json!(h.snippet);
                        v
                    }).collect::<Vec<_>>()
                }))
            }
            "read" => {
                let path = args["path"].as_str().unwrap_or_default();
                if path.contains("..") || path.starts_with('/') || !path.starts_with("context/") {
                    return Err(ToolError::InvalidArgs(
                        "path must be a context file path from a search result".into(),
                    ));
                }
                let doc = self
                    .stores
                    .context
                    .get(path)
                    .await?
                    .ok_or_else(|| ToolError::NotFound(path.to_string()))?;
                if doc.sensitivity > ceiling {
                    return Err(ToolError::Forbidden(format!(
                        "{path} is {} and this skill handles at most {ceiling}",
                        doc.sensitivity
                    )));
                }
                event.sources = vec![doc.path.clone()];
                let mut v = self.doc_meta(&doc, today);
                v["body"] = json!(doc.body);
                Ok(v)
            }
            other => Err(ToolError::UnknownTool(format!("context.{other}"))),
        }
    }

    fn doc_meta(&self, doc: &ContextDoc, today: chrono::NaiveDate) -> Value {
        let stale = doc.is_stale(today, self.stale_after_days);
        let mut v = json!({
            "path": doc.path,
            "cite": doc.path,
            "title": doc.title,
            "domain": doc.domain,
            "owner": doc.owner,
            "last_reviewed": doc.last_reviewed,
            "sensitivity": doc.sensitivity,
            "stale": stale,
        });
        if stale {
            v["stale_note"] = json!(format!(
                "Last reviewed {}; flag it to {}.",
                doc.last_reviewed, doc.owner
            ));
        }
        v
    }

    async fn memory_update(
        &self,
        session: &Session,
        args: &Value,
        now: DateTime<Utc>,
    ) -> Result<Value, ToolError> {
        let key = args["key"].as_str().unwrap_or_default();
        if !valid_key(key) {
            return Err(ToolError::InvalidArgs(
                "key must be 1–64 lowercase letters, digits or hyphens".into(),
            ));
        }
        let text = args["text"].as_str().unwrap_or_default();
        let note = self
            .stores
            .memory
            .update(&session.user, key, text, now)
            .await?;
        Ok(json!({ "note": note }))
    }

    async fn write(
        &self,
        session: &Session,
        spec: &ToolSpec,
        args: &Value,
        now: DateTime<Utc>,
        event: &mut AuditEvent,
    ) -> Result<Value, ToolError> {
        let key = token_hash(&session.token);
        if self
            .stores
            .approvals
            .take_grant(&key, &spec.name, args, now)
            .await?
        {
            let written = self.execute_write(&session.user, spec, args).await?;
            event.outcome = Outcome::Approved;
            event.detail = Some("approved in the harness".into());
            event.output_ref = written["url"].as_str().map(String::from);
            return Ok(written);
        }
        let item = self
            .stores
            .approvals
            .create(
                NewItem {
                    user: &session.user,
                    skill: &session.skill,
                    run_id: &session.run_id,
                    tool: &spec.name,
                    args,
                },
                now,
            )
            .await?;
        event.outcome = Outcome::Pending;
        event.output_ref = Some(item.id.clone());
        Ok(json!({
            "status": "pending_approval",
            "approval_id": item.id,
            "message": format!(
                "Not done yet: {} is waiting for {} to approve it in the PM OS inbox. Do not retry, and do not say it was done.",
                spec.name, session.user
            ),
        }))
    }

    async fn execute_write(
        &self,
        user: &str,
        spec: &ToolSpec,
        args: &Value,
    ) -> Result<Value, ToolError> {
        let connector = self.connector(&spec.system)?;
        let written = connector
            .write(Caller { user }, spec.action(), args)
            .await?;
        Ok(json!({
            "status": "done",
            "system": written.system,
            "id": written.id,
            "url": written.url,
        }))
    }

    // ----- approvals ------------------------------------------------------------------

    /// Record that the PM approved this exact write in the harness, so the call that follows
    /// runs without a second approval.
    pub async fn grant(
        &self,
        session: &Session,
        harness_name: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> Result<(), ToolError> {
        let spec = self
            .catalog
            .tool(harness_name)
            .ok_or_else(|| ToolError::UnknownTool(harness_name.to_string()))?;
        if spec.kind != ToolKind::Write {
            return Err(ToolError::InvalidArgs(format!(
                "{} is not a write tool",
                spec.name
            )));
        }
        let declared = self
            .skill_of(session)
            .is_some_and(|s| s.tools.contains(&spec.name));
        if !declared {
            return Err(ToolError::NotAllowed(spec.harness_name.clone()));
        }
        self.stores
            .approvals
            .add_grant(&token_hash(&session.token), &spec.name, args, now)
            .await?;
        self.audit(AuditEvent {
            at: now,
            run_id: session.run_id.clone(),
            user: session.user.clone(),
            skill: session.skill.clone(),
            tool: spec.name.clone(),
            outcome: Outcome::Granted,
            sources: Vec::new(),
            args_hash: Some(args_hash(args)),
            output_ref: None,
            detail: None,
        })
        .await;
        Ok(())
    }

    pub async fn approvals(
        &self,
        user: &str,
        status: Option<Status>,
    ) -> Result<Vec<ApprovalItem>, StoreError> {
        self.stores.approvals.list(user, status).await
    }

    pub async fn approval(&self, id: &str) -> Result<Option<ApprovalItem>, StoreError> {
        self.stores.approvals.get(id).await
    }

    /// A person's decision on an inbox item. Approve and edit run the write as the item's
    /// user; reject closes it.
    pub async fn decide(
        &self,
        id: &str,
        decided_by: &str,
        decision: Decision,
        edited_args: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecisionError> {
        let item = match self
            .stores
            .approvals
            .decide(id, decided_by, decision, edited_args, now)
            .await
        {
            Ok(item) => item,
            Err(e) => {
                if e == DecideError::Forbidden {
                    if let Ok(Some(item)) = self.stores.approvals.get(id).await {
                        self.audit(AuditEvent {
                            at: now,
                            run_id: item.run_id,
                            user: decided_by.to_string(),
                            skill: item.skill,
                            tool: item.tool,
                            outcome: Outcome::Denied,
                            sources: Vec::new(),
                            args_hash: Some(item.args_hash),
                            output_ref: Some(item.id),
                            detail: Some("decision by someone other than the item's user".into()),
                        })
                        .await;
                    }
                }
                return Err(e.into());
            }
        };

        let mut event = AuditEvent {
            at: now,
            run_id: item.run_id.clone(),
            user: item.user.clone(),
            skill: item.skill.clone(),
            tool: item.tool.clone(),
            outcome: Outcome::Rejected,
            sources: Vec::new(),
            args_hash: Some(item.args_hash.clone()),
            output_ref: Some(item.id.clone()),
            detail: (decision == Decision::Edit).then(|| "edited before approval".to_string()),
        };
        if item.status == Status::Rejected {
            self.audit(event).await;
            return Ok(item);
        }

        let spec = self
            .catalog
            .tools()
            .find(|t| t.name == item.tool)
            .cloned()
            .ok_or_else(|| DecisionError::UnknownTool(item.tool.clone()))?;
        let result = self.execute_write(&item.user, &spec, &item.args).await;
        let outcome = match &result {
            Ok(written) => {
                event.outcome = Outcome::Approved;
                event.output_ref = written["url"].as_str().map(String::from);
                Ok(written.clone())
            }
            Err(e) => {
                event.outcome = Outcome::Error;
                event.detail = Some(e.to_string());
                Err(e.to_string())
            }
        };
        let done = self.stores.approvals.complete(&item.id, outcome).await?;
        self.audit(event).await;
        Ok(done.unwrap_or(item))
    }
}
