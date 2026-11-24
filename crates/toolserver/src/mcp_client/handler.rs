//! The client side of one connection: PM OS declares no client capabilities and honours no
//! request a server makes (`docs/mcp-servers-spec.md` §3, threats 9 and 10).
//!
//! - `sampling/createMessage`, `elicitation/create`, `roots/list` and any custom request are
//!   answered "method not found" and audited `mcp.server_request_refused` with the method name
//!   only. (rmcp's defaults would answer `roots/list` with an empty list and decline an
//!   elicitation; both are overridden here.)
//! - Resource, prompt and log notifications are dropped and only counted: they may hold data.
//! - `notifications/tools/list_changed` marks the server for a re-list before its next call.
//! - `ping` is answered.

// Sampling, roots and logging are deprecated in MCP (SEP-2577); PM OS still refuses them.
#![allow(deprecated)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use chrono::Utc;
use rmcp::model::{
    CancelledNotificationParam, ClientCapabilities, ClientConfig, CreateMessageRequestParams,
    CreateMessageResult, CustomNotification, CustomRequest, CustomResult, ElicitRequestParams,
    ElicitResult, ErrorCode, Implementation, ListRootsResult, LoggingMessageNotificationParam,
    ProgressNotificationParam, ResourceUpdatedNotificationParam,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient};
use rmcp::{ClientHandler, ErrorData as McpError};

use crate::audit::{AuditEvent, AuditSink, Client, Outcome};

/// The audit event of a refused server request.
pub const REFUSED_EVENT: &str = "mcp.server_request_refused";
/// The audit event of each connection.
pub const CONNECT_EVENT: &str = "mcp.connect";
/// The audit event of an approved tool that changed or went away.
pub const CHANGED_EVENT: &str = "mcp.tool_changed";
/// The `user` of the MCP client's own events.
pub const EVENT_ACTOR: &str = "pmos-mcp-client";

/// What one server's connections saw, shared by them.
pub struct Events {
    server: String,
    audit: Option<Arc<dyn AuditSink>>,
    /// Requests refused.
    pub refused: AtomicU64,
    /// Notifications dropped (resources, prompts, logs, progress, custom).
    pub dropped: AtomicU64,
    /// A `tools/list_changed` arrived: re-list before the next call.
    pub list_changed: AtomicBool,
}

impl Events {
    pub fn new(server: &str, audit: Option<Arc<dyn AuditSink>>) -> Arc<Self> {
        Arc::new(Self {
            server: server.to_string(),
            audit,
            refused: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            list_changed: AtomicBool::new(false),
        })
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Record an MCP client event: fixed text plus ids, never content.
    pub async fn record(&self, tool: &str, outcome: Outcome, detail: String) {
        let Some(audit) = &self.audit else {
            return;
        };
        let event = AuditEvent {
            at: Utc::now(),
            client: Client::System,
            run_id: format!("mcp:{}", self.server),
            user: EVENT_ACTOR.into(),
            skill: None,
            bot: None,
            persona: None,
            tool: tool.into(),
            outcome,
            sources: Vec::new(),
            source_titles: Default::default(),
            args_hash: None,
            output_ref: None,
            detail: Some(detail),
        };
        if let Err(e) = audit.record(event).await {
            tracing::error!(error = %e, server = %self.server, "an MCP client event could not be audited");
        }
    }

    pub async fn refuse(&self, method: &str) -> McpError {
        self.refused.fetch_add(1, Ordering::Relaxed);
        // The method name only; never the request's parameters.
        let method: String = method
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
            .take(64)
            .collect();
        self.record(
            REFUSED_EVENT,
            Outcome::Denied,
            format!("server {} sent {method}; refused", self.server),
        )
        .await;
        McpError::new(ErrorCode::METHOD_NOT_FOUND, method, None)
    }

    fn drop_one(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// The handler of one connection.
#[derive(Clone)]
pub struct RefusingHandler {
    events: Arc<Events>,
}

impl RefusingHandler {
    pub fn new(events: Arc<Events>) -> Self {
        Self { events }
    }
}

/// What PM OS says about itself in `initialize`: no capabilities at all.
pub fn client_config() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("pmos-toolserver", env!("CARGO_PKG_VERSION")),
    )
}

#[allow(deprecated)]
impl ClientHandler for RefusingHandler {
    async fn create_message(
        &self,
        _params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, McpError> {
        Err(self.events.refuse("sampling/createMessage").await)
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, McpError> {
        Err(self.events.refuse("roots/list").await)
    }

    async fn create_elicitation(
        &self,
        _request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        Err(self.events.refuse("elicitation/create").await)
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleClient>,
    ) -> Result<CustomResult, McpError> {
        Err(self.events.refuse(&request.method).await)
    }

    async fn on_cancelled(
        &self,
        _params: CancelledNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
    }

    async fn on_progress(
        &self,
        _params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.events.drop_one();
    }

    async fn on_logging_message(
        &self,
        _params: LoggingMessageNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.events.drop_one();
    }

    async fn on_resource_updated(
        &self,
        _params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.events.drop_one();
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.events.drop_one();
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.events.drop_one();
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.events.list_changed.store(true, Ordering::SeqCst);
    }

    async fn on_custom_notification(
        &self,
        _notification: CustomNotification,
        _context: NotificationContext<RoleClient>,
    ) {
        self.events.drop_one();
    }

    fn get_info(&self) -> ClientConfig {
        client_config()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pmos_declares_no_client_capabilities() {
        let caps = serde_json::to_value(&super::client_config().capabilities).unwrap();
        assert_eq!(
            caps,
            serde_json::json!({}),
            "no sampling, roots or elicitation"
        );
    }
}
