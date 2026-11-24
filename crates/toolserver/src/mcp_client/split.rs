//! Slack's split (slice M8, `docs/mcp-servers-spec.md` §2.6): a post needs the audience check,
//! which needs the server to describe a channel and list its members. When the Slack record
//! doesn't approve both checks, [`ReadsOnly`] serves Slack's reads from the MCP server and
//! everything else (posts, the audience check, a draft's target label, readiness) from the
//! built-in connector, as before the switch.
//!
//! The rules around a post stay in the tool server on both routes: the approval gate, the
//! audience check (`service.rs`), one draft per thread and per tagged message, and the 24-hour
//! text rule (SLK-8, the approvals store's sweep). None of them is the server's to keep.

use std::sync::Arc;

use async_trait::async_trait;
use pmos_contract::mcp::ToolClass;
use pmos_contract::Sensitivity;
use serde_json::Value;

use super::McpConnector;
use crate::connectors::{
    Audience, Caller, Connector, ConnectorError, Conversation, Item, Route, SourceRef,
};

/// Reads on the MCP server; posts and the audience check on the built-in connector.
pub struct ReadsOnly {
    mcp: Arc<McpConnector>,
    builtin: Arc<dyn Connector>,
}

impl ReadsOnly {
    pub fn new(mcp: Arc<McpConnector>, builtin: Arc<dyn Connector>) -> Result<Self, String> {
        if mcp.system() != builtin.system() {
            return Err(format!(
                "`{}` and `{}` are different systems",
                mcp.system(),
                builtin.system()
            ));
        }
        Ok(Self { mcp, builtin })
    }
}

#[async_trait]
impl Connector for ReadsOnly {
    fn system(&self) -> &str {
        self.mcp.system()
    }

    fn mode(&self) -> &'static str {
        self.mcp.mode()
    }

    fn data_class(&self) -> Option<Sensitivity> {
        // Reads come from the server.
        self.mcp.data_class()
    }

    fn route(&self, action: &str) -> Option<Route> {
        let name = format!("{}.{action}", self.system());
        let read = self
            .mcp
            .server()
            .record()
            .tool(&name)
            .is_some_and(|t| t.class() == ToolClass::Read);
        if read {
            self.mcp.route(action)
        } else {
            None
        }
    }

    fn served_by(&self, action: &str) -> String {
        match self.route(action) {
            Some(r) => r.server,
            None => self.builtin.served_by(action),
        }
    }

    async fn read(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        self.mcp.read(caller, action, args).await
    }

    async fn write(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<SourceRef, ConnectorError> {
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
        self.mcp.ready().await?;
        self.builtin.ready().await
    }
}
