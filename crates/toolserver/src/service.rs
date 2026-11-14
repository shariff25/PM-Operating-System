//! The tool server's rules, independent of transport. MCP (`mcp.rs`) and the internal HTTP
//! API (`api.rs`) are thin layers over [`ToolService`].
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

use crate::approvals::{
    args_hash, ApprovalItem, ApprovalStore, DecideError, Decision, NewItem, Status,
};
use crate::audit::{AuditEvent, AuditSink, Outcome};
use crate::catalog::{Catalog, ToolSpec};
use crate::connectors::{untrusted, Caller, Connector, ConnectorError};
use crate::context::{ContextDoc, ContextIndex};
use crate::memory::{valid_key, MemoryStore};
use crate::schemas::check_args;
use crate::session::{valid_handle, Session, SessionStore};

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

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("unknown skill `{0}`")]
    UnknownSkill(String),
    #[error("invalid user handle `{0}`")]
    InvalidUser(String),
    #[error("run_id must be 1–128 characters")]
    InvalidRun,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecisionError {
    #[error(transparent)]
    Decide(#[from] DecideError),
    #[error("tool `{0}` is no longer served")]
    UnknownTool(String),
}

pub struct ToolService {
    catalog: Catalog,
    sessions: SessionStore,
    approvals: ApprovalStore,
    context: Arc<dyn ContextIndex>,
    memory: Arc<dyn MemoryStore>,
    connectors: HashMap<String, Arc<dyn Connector>>,
    audit: Arc<dyn AuditSink>,
    stale_after_days: i64,
}

impl ToolService {
    /// Fails if any registry tool belongs to a system with no handler.
    pub fn new(
        catalog: Catalog,
        context: Arc<dyn ContextIndex>,
        memory: Arc<dyn MemoryStore>,
        connectors: Vec<Arc<dyn Connector>>,
        audit: Arc<dyn AuditSink>,
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
            sessions: SessionStore::default(),
            approvals: ApprovalStore::default(),
            context,
            memory,
            connectors,
            audit,
            stale_after_days: pmos_contract::lint::DEFAULT_STALE_AFTER_DAYS,
        })
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    // ----- sessions -------------------------------------------------------------------

    pub fn create_session(
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
        Ok(self.sessions.create(user, skill, run_id, ttl, now))
    }

    pub fn session(&self, token: &str, now: DateTime<Utc>) -> Option<Session> {
        self.sessions.get(token, now)
    }

    pub fn revoke_session(&self, token: &str) -> bool {
        self.sessions.revoke(token)
    }

    fn skill_of(&self, session: &Session) -> &Skill {
        // Sessions are only created for skills in the catalog, and the catalog is fixed for
        // the life of the process.
        self.catalog
            .skill(&session.skill)
            .expect("session skill is in the catalog")
    }

    /// The tools this session may call.
    pub fn list_tools(&self, session: &Session) -> Vec<&ToolSpec> {
        self.catalog.tools_for(self.skill_of(session))
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
        self.audit.record(event);
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
        let skill = self.skill_of(session);
        if !skill.tools.contains(&spec.name) {
            return Err(ToolError::NotAllowed(spec.harness_name.clone()));
        }
        check_args(&spec.input_schema, args).map_err(ToolError::InvalidArgs)?;

        match (spec.kind, spec.system.as_str()) {
            (ToolKind::Read, "context") => self.context_tool(skill, spec, args, now, event),
            (ToolKind::Read, "memory") => Ok(json!({ "notes": self.memory.read(&session.user) })),
            (ToolKind::Internal, "memory") => self.memory_update(session, args, now),
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

    fn context_tool(
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
                let hits = self.context.search(query, domain, MAX_LIMIT * 2);
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
                    .context
                    .get(path)
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

    fn memory_update(
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
        Ok(json!({ "note": self.memory.update(&session.user, key, text, now) }))
    }

    async fn write(
        &self,
        session: &Session,
        spec: &ToolSpec,
        args: &Value,
        now: DateTime<Utc>,
        event: &mut AuditEvent,
    ) -> Result<Value, ToolError> {
        if self
            .approvals
            .take_grant(&session.token, &spec.name, args, now)
        {
            let written = self.execute_write(&session.user, spec, args).await?;
            event.outcome = Outcome::Approved;
            event.detail = Some("approved in the harness".into());
            event.output_ref = written["url"].as_str().map(String::from);
            return Ok(written);
        }
        let item = self.approvals.create(
            NewItem {
                user: &session.user,
                skill: &session.skill,
                run_id: &session.run_id,
                tool: &spec.name,
                args,
            },
            now,
        );
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
    pub fn grant(
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
        if !self.skill_of(session).tools.contains(&spec.name) {
            return Err(ToolError::NotAllowed(spec.harness_name.clone()));
        }
        self.approvals
            .add_grant(&session.token, &spec.name, args, now);
        self.audit.record(AuditEvent {
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
        });
        Ok(())
    }

    pub fn approvals(&self, user: &str, status: Option<Status>) -> Vec<ApprovalItem> {
        self.approvals.list(user, status)
    }

    pub fn approval(&self, id: &str) -> Option<ApprovalItem> {
        self.approvals.get(id)
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
            .approvals
            .decide(id, decided_by, decision, edited_args, now)
        {
            Ok(item) => item,
            Err(e) => {
                if let (DecideError::Forbidden, Some(item)) = (&e, self.approvals.get(id)) {
                    self.audit.record(AuditEvent {
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
                    });
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
            self.audit.record(event);
            return Ok(item);
        }

        let spec = self
            .catalog
            .tools()
            .find(|t| t.name == item.tool)
            .cloned()
            .ok_or_else(|| DecisionError::UnknownTool(item.tool.clone()))?;
        let result = self.execute_write(&item.user, &spec, &item.args).await;
        let done = match result {
            Ok(written) => {
                event.outcome = Outcome::Approved;
                event.output_ref = written["url"].as_str().map(String::from);
                self.approvals.complete(&item.id, Ok(written))
            }
            Err(e) => {
                event.outcome = Outcome::Error;
                event.detail = Some(e.to_string());
                self.approvals.complete(&item.id, Err(e.to_string()))
            }
        };
        self.audit.record(event);
        Ok(done.unwrap_or(item))
    }
}
