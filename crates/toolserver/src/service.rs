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
use crate::audit::{AuditEvent, Client, Outcome};
use crate::catalog::{Catalog, ToolSpec};
use crate::connectors::{untrusted, Caller, Connector, ConnectorError};
use crate::context::{load_docs, ContextDoc, ReindexReport};
use crate::memory::valid_key;
use crate::schemas::check_args;
use crate::session::{token_hash, valid_handle, Session};
use crate::store::{StoreError, Stores};
use crate::team::Team;

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
            ConnectorError::Forbidden(m) => ToolError::Forbidden(m),
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
    #[error("`{0}` is not in team.yaml")]
    NotAMember(String),
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
    #[error("the item's skill no longer declares `{0}`")]
    NotAllowed(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("the write task failed: {0}")]
    Internal(String),
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
    team: Arc<Team>,
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
            team: Team::open(),
            stale_after_days: pmos_contract::lint::DEFAULT_STALE_AFTER_DAYS,
        })
    }

    /// Only people in `team` may have sessions (when it is enforced).
    pub fn with_team(mut self, team: Arc<Team>) -> Self {
        self.team = team;
        self
    }

    pub fn team(&self) -> &Arc<Team> {
        &self.team
    }

    /// Readiness: storage answers within a second, and every connector's own dependency
    /// (such as the support docs server) answers. Each check is reported by name.
    pub async fn readiness(&self) -> Vec<(String, Result<(), String>)> {
        let storage = match tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.stores.approvals.get("apr_readiness_probe"),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("storage didn't answer within 1 s".into()),
        };
        let mut checks = vec![("storage".to_string(), storage)];
        let mut systems: Vec<_> = self.connectors.iter().collect();
        systems.sort_by_key(|(name, _)| name.as_str());
        for (name, connector) in systems {
            checks.push((name.clone(), connector.ready().await));
        }
        checks
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
        if let Err(e) = self.team.reload() {
            return Err(ReindexError::Lint(format!("  team.yaml: {e}")));
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
        if !self.team.allows(user) {
            return Err(SessionError::NotAMember(user.to_string()));
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

    /// A call from an agent sandbox over MCP.
    pub async fn call(
        &self,
        session: &Session,
        harness_name: &str,
        args: Value,
        now: DateTime<Utc>,
    ) -> Result<Value, ToolError> {
        self.call_as(Client::Mcp, session, harness_name, args, now)
            .await
    }

    /// A call by `client`, which is recorded in the audit log.
    pub async fn call_as(
        &self,
        client: Client,
        session: &Session,
        harness_name: &str,
        args: Value,
        now: DateTime<Utc>,
    ) -> Result<Value, ToolError> {
        let args = if args.is_null() { json!({}) } else { args };
        let mut event = AuditEvent {
            client,
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
                    run_id: &session.run_id,
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
            // Fail closed: the write runs only once the audit log has recorded it starting.
            let started = AuditEvent {
                outcome: Outcome::Started,
                detail: Some("approved in the harness; writing".into()),
                ..event.clone()
            };
            if let Err(e) = self.stores.audit.record(started).await {
                tracing::error!(error = %e, tool = %spec.name, "AUDIT UNAVAILABLE: approved write not run");
                // The grant was used up; put it back so the PM needn't approve again.
                let _ = self
                    .stores
                    .approvals
                    .add_grant(&key, &spec.name, args, now)
                    .await;
                return Err(ToolError::Unavailable(e.0));
            }
            let written = self
                .execute_write(
                    Caller {
                        user: &session.user,
                        run_id: &session.run_id,
                    },
                    spec,
                    args,
                )
                .await?;
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
        caller: Caller<'_>,
        spec: &ToolSpec,
        args: &Value,
    ) -> Result<Value, ToolError> {
        let connector = self.connector(&spec.system)?;
        let written = connector
            .write(caller, spec.action(), args)
            .await
            .map_err(|e| match e {
                // A timeout or a 5xx after sending: the write may have happened. Saying
                // "unavailable" would invite a retry and a duplicate.
                ConnectorError::Unavailable(detail) => ToolError::Connector(format!(
                    "{system} didn't confirm the write ({detail}). It may or may not have happened: check {system} before trying again, and don't retry automatically.",
                    system = spec.system
                )),
                other => other.into(),
            })?;
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
            client: Client::Orchestrator,
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

    /// Decide an inbox item as `decided_by`. Approve and edit run the write as the item's
    /// user; edited arguments are checked like any call's, and the skill must still declare
    /// the tool.
    ///
    /// The write runs in its own task, so a caller that disconnects can't cancel it halfway,
    /// and every way out of here is audited. The item is `executing` while the write runs;
    /// if the process dies then, [`ToolService::reconcile`] resolves it on the next start.
    pub async fn decide(
        self: &Arc<Self>,
        id: &str,
        decided_by: &str,
        decision: Decision,
        edited_args: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecisionError> {
        // Check the write is still allowed before changing anything. Ownership and status
        // are checked again, atomically, by the store.
        if decision != Decision::Reject {
            if let Some(current) = self.stores.approvals.get(id).await? {
                if current.user == decided_by && current.status == Status::Pending {
                    let spec = self.write_spec(&current)?;
                    if decision == Decision::Edit {
                        let args = edited_args.as_ref().ok_or(DecideError::MissingArgs)?;
                        check_args(&spec.input_schema, args).map_err(DecisionError::InvalidArgs)?;
                    }
                }
            }
        }

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
                            client: Client::Orchestrator,
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

        let event = AuditEvent {
            client: Client::Orchestrator,
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

        let this = self.clone();
        tokio::spawn(async move { this.run_approved(item, event).await })
            .await
            .map_err(|e| DecisionError::Internal(e.to_string()))?
    }

    /// The tool for an inbox item, if the item's skill may still run it.
    fn write_spec(&self, item: &ApprovalItem) -> Result<ToolSpec, DecisionError> {
        let spec = self
            .catalog
            .tools()
            .find(|t| t.name == item.tool)
            .cloned()
            .ok_or_else(|| DecisionError::UnknownTool(item.tool.clone()))?;
        let declared = self
            .catalog
            .skill(&item.skill)
            .is_some_and(|s| s.tools.contains(&spec.name));
        if !declared {
            return Err(DecisionError::NotAllowed(item.tool.clone()));
        }
        Ok(spec)
    }

    /// Run an approved (`executing`) item's write and store the outcome.
    async fn run_approved(
        &self,
        item: ApprovalItem,
        mut event: AuditEvent,
    ) -> Result<ApprovalItem, DecisionError> {
        let finish = |result: Result<Value, String>| {
            let id = item.id.clone();
            async move { self.stores.approvals.complete(&id, result).await }
        };
        // Checked before the decision; this catches a catalog that changed in between.
        let spec = match self.write_spec(&item) {
            Ok(spec) => spec,
            Err(e) => {
                event.outcome = Outcome::Error;
                event.detail = Some(e.to_string());
                self.audit(event).await;
                let _ = finish(Err(e.to_string())).await;
                return Err(e);
            }
        };
        // Fail closed: no write runs unless the audit log has recorded that it is starting.
        let started = AuditEvent {
            outcome: Outcome::Started,
            detail: Some("approved in the inbox; writing".into()),
            ..event.clone()
        };
        if let Err(e) = self.stores.audit.record(started).await {
            tracing::error!(error = %e, id = %item.id, "AUDIT UNAVAILABLE: approved write not run");
            let _ = finish(Err("PM OS couldn't record the write in the audit log, so it didn't run. Approve it again shortly.".into())).await;
            return Err(DecisionError::Decide(DecideError::Store(e)));
        }

        let result = self
            .execute_write(
                Caller {
                    user: &item.user,
                    run_id: &item.run_id,
                },
                &spec,
                &item.args,
            )
            .await;
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
        let stored = finish(outcome).await;
        if stored.is_err() {
            let note = "outcome not saved to the inbox; the reconciler will mark it";
            event.detail = Some(match event.detail.take() {
                Some(d) => format!("{d}; {note}"),
                None => note.to_string(),
            });
        }
        // The outcome is audited even when storing it failed.
        self.audit(event).await;
        Ok(stored?.unwrap_or(item))
    }

    /// Resolve writes left `executing` for longer than `older_than` (the process died
    /// mid-write). Their outcome is unknown, so they become `failed` with a note to check the
    /// target system before trying again; nothing is retried automatically. Returns how many.
    pub async fn reconcile(
        &self,
        older_than: Duration,
        now: DateTime<Utc>,
    ) -> Result<usize, StoreError> {
        let stuck = self.stores.approvals.stuck(now - older_than).await?;
        for item in &stuck {
            let system = item.tool.split('.').next().unwrap_or("the system");
            let message = format!(
                "PM OS stopped before confirming this write. It may or may not have happened: check {system} before trying again."
            );
            if self
                .stores
                .approvals
                .complete(&item.id, Err(message.clone()))
                .await?
                .is_some()
            {
                self.audit(AuditEvent {
                    client: Client::Orchestrator,
                    at: now,
                    run_id: item.run_id.clone(),
                    user: item.user.clone(),
                    skill: item.skill.clone(),
                    tool: item.tool.clone(),
                    outcome: Outcome::Error,
                    sources: Vec::new(),
                    args_hash: Some(item.args_hash.clone()),
                    output_ref: Some(item.id.clone()),
                    detail: Some(format!("outcome unknown: {message}")),
                })
                .await;
            }
        }
        Ok(stuck.len())
    }
}
