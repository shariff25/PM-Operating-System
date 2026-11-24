//! A registry system served by an MCP server, behind the existing [`Connector`] trait
//! (`docs/mcp-servers-spec.md` §2.1, §2.5). `dispatch` and `write` in `service.rs` call it as
//! they call a built-in connector, so the policy checks, the approval gate, the audit event and
//! the ledger happen before and after the MCP call exactly as for a connector.
//!
//! PM OS-side rules stay here, applied on the arguments before the call and on the result after
//! it: a team bot's read scope (`connectors/scope.rs`), and `jira.rollup` computed over the
//! mapped search. Writes act as the PM (never a team bot); Slack's audience check is not
//! offered, so Slack writes stay on the built-in connector.

use std::sync::Arc;

use async_trait::async_trait;
use pmos_contract::mcp::{Adapter, ToolClass, ToolMapping};
use pmos_contract::Sensitivity;
use serde_json::{json, Map, Value};

use super::mapping;
use super::server::{CallKind, McpServer};
use crate::connectors::http::{arg, issue_key};
use crate::connectors::{
    rollup, Caller, Connector, ConnectorError, Item, Route, SourceRef, TeamBot,
};

/// What a team bot's Slack search asks the server for: results outside its channels are
/// dropped afterwards, then the search is cut to the limit the bot asked for.
const SCOPED_SEARCH_LIMIT: u64 = 50;

/// One system, served by one server's record.
pub struct McpConnector {
    system: String,
    server: Arc<McpServer>,
    mode: &'static str,
}

impl McpConnector {
    /// Fails unless the record serves `system` and every mapped tool's arguments cover its
    /// registry schema exactly.
    pub fn new(system: &str, server: Arc<McpServer>, mode: &'static str) -> Result<Self, String> {
        let record = server.record();
        if !record.systems.iter().any(|s| s == system) {
            return Err(format!("`{}` doesn't serve `{system}`", record.id));
        }
        for t in record
            .tools
            .iter()
            .filter(|t| t.registry.starts_with(&format!("{system}.")))
        {
            let schema = crate::schemas::input_schema(&t.registry).ok_or_else(|| {
                format!("`{}` maps `{}`, which has no schema", record.id, t.registry)
            })?;
            mapping::check_coverage(t, &schema).map_err(|e| format!("`{}`: {e}", record.id))?;
        }
        Ok(Self {
            system: system.to_string(),
            server,
            mode,
        })
    }

    pub fn server(&self) -> &Arc<McpServer> {
        &self.server
    }

    fn mapping(&self, action: &str, class: ToolClass) -> Result<&ToolMapping, ConnectorError> {
        let name = format!("{}.{action}", self.system);
        self.server
            .record()
            .tool(&name)
            .filter(|t| t.class() == class)
            .ok_or(ConnectorError::Unsupported(format!(
                "{name} (not approved on {})",
                self.server.record().id
            )))
    }

    /// A team bot's scope on the arguments, before anything is sent (`scope.rs`).
    fn scope_args(
        &self,
        bot: TeamBot<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Value, ConnectorError> {
        let mut args = args.clone();
        match (self.system.as_str(), action) {
            ("jira", "search" | "rollup") => {
                args["jql"] = json!(bot.jql(arg(&args, "jql")?)?);
            }
            ("jira", "read") => bot.check_issue(issue_key(arg(&args, "key")?)?)?,
            ("jira", _) => {
                bot.jira_projects()?;
            }
            ("confluence", "search") => {
                args["cql"] = json!(bot.cql(arg(&args, "cql")?)?);
            }
            ("confluence", _) => {
                bot.confluence_spaces()?;
            }
            ("slack", "read_thread") => bot.check_channel(arg(&args, "channel")?)?,
            ("slack", "search") => {
                bot.slack_channels()?;
                // Results outside the scope are dropped after the call, so ask for the most
                // and cut to the limit asked for after dropping them (`read`).
                args["limit"] = json!(SCOPED_SEARCH_LIMIT);
            }
            ("slack", _) => {
                bot.slack_channels()?;
            }
            _ => {}
        }
        Ok(args)
    }

    /// A team bot's scope on one item: `Err` if it is outside.
    fn item_in_scope(&self, bot: TeamBot<'_>, item: &Item) -> Result<(), ConnectorError> {
        let id = item.source.id.as_str();
        match self.system.as_str() {
            "jira" => bot.check_issue(id),
            "confluence" => bot.check_space(id, item.content["space"].as_str().unwrap_or_default()),
            "slack" => bot.check_channel(id.split(':').next().unwrap_or_default()),
            _ => Ok(()),
        }
    }

    /// `jira.rollup` over the mapped search: page after page (the token sent back under the
    /// name its result path ends with), up to `max_issues` and the rollup's time budget.
    async fn rollup(
        &self,
        caller: Caller<'_>,
        mapping: &ToolMapping,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        let (group_by, max) = rollup::options(args)?;
        let deadline = tokio::time::Instant::now() + rollup::DEFAULT_BUDGET;
        let mut issues = Vec::new();
        let mut token: Option<String> = None;
        let mut truncated = None;
        let mut url = String::new();
        loop {
            let mut up = mapping::upstream_args(&mapping.args, args, caller.run_id)?;
            if let (Some(t), Some(path)) = (&token, mapping.result.next_token.as_deref()) {
                let name = path.rsplit('.').next().unwrap_or(path);
                up.insert(name.to_string(), json!(t));
            }
            let call = self
                .server
                .call(caller.reader(), mapping, up, CallKind::Read);
            let page = match tokio::time::timeout_at(deadline, call).await {
                Ok(page) => page?,
                Err(_) => {
                    truncated = Some(rollup::Truncated::Time);
                    break;
                }
            };
            if url.is_empty() {
                url = mapping
                    .result
                    .url
                    .as_deref()
                    .and_then(|p| mapping::get_path(&page, p))
                    .and_then(Value::as_str)
                    .filter(|u| u.starts_with("https://"))
                    .unwrap_or_default()
                    .to_string();
            }
            let raw = mapping::raw_items(&mapping.result, &page)?;
            let room = max - issues.len();
            issues.extend(
                raw.iter()
                    .take(room)
                    .filter_map(|v| rollup::RollupIssue::from_jira(v))
                    // Results outside a team bot's scope are dropped after the call.
                    .filter(|i| {
                        caller
                            .team_bot
                            .is_none_or(|b| b.check_issue(&i.key).is_ok())
                    }),
            );
            let next = mapping
                .result
                .next_token
                .as_deref()
                .and_then(|p| mapping::get_path(&page, p))
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(String::from);
            let last = mapping
                .result
                .last
                .as_deref()
                .and_then(|p| mapping::get_path(&page, p))
                .and_then(Value::as_bool)
                .unwrap_or(next.is_none());
            if raw.len() >= room {
                if raw.len() > room || !last {
                    truncated = Some(rollup::Truncated::Cap);
                }
                break;
            }
            match next {
                Some(t) if !last && !raw.is_empty() => token = Some(t),
                _ => break,
            }
        }
        let content = rollup::rollup(
            &issues,
            group_by,
            chrono::Utc::now().date_naive(),
            truncated,
        );
        Ok(vec![rollup::item(content, url)])
    }
}

#[async_trait]
impl Connector for McpConnector {
    fn system(&self) -> &str {
        &self.system
    }

    fn mode(&self) -> &'static str {
        self.mode
    }

    fn data_class(&self) -> Option<Sensitivity> {
        Some(self.server.record().data_class)
    }

    fn route(&self, action: &str) -> Option<Route> {
        let name = format!("{}.{action}", self.system);
        self.server.record().tool(&name).map(|t| Route {
            server: self.server.record().id.clone(),
            digest: t.digest.clone(),
        })
    }

    async fn read(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<Vec<Item>, ConnectorError> {
        let mapping = self.mapping(action, ToolClass::Read)?;
        let original = args;
        let args = match caller.team_bot {
            Some(bot) => self.scope_args(bot, action, args)?,
            None => args.clone(),
        };
        if mapping.adapter == Some(Adapter::JiraRollup) {
            return self.rollup(caller, mapping, &args).await;
        }
        let up = mapping::upstream_args(&mapping.args, &args, caller.run_id)?;
        let result = self
            .server
            .call(caller.reader(), mapping, up, CallKind::Read)
            .await?;
        let items = mapping::items(&mapping.result, &self.system, &result)?;
        let Some(bot) = caller.team_bot else {
            return Ok(items);
        };
        if action == "search" {
            // Results outside the scope are dropped after the call; a widened Slack search is
            // cut back to the limit asked for (default 10, as the built-in connector).
            let limit = match self.system.as_str() {
                "slack" => crate::connectors::http::limit(original, 10, 50) as usize,
                _ => usize::MAX,
            };
            Ok(items
                .into_iter()
                .filter(|i| self.item_in_scope(bot, i).is_ok())
                .take(limit)
                .collect())
        } else {
            // A read outside the scope is refused, checked again on what came back.
            for i in &items {
                self.item_in_scope(bot, i)?;
            }
            Ok(items)
        }
    }

    async fn write(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<SourceRef, ConnectorError> {
        let mapping = self.mapping(action, ToolClass::Write)?;
        let up = mapping::upstream_args(&mapping.args, args, caller.run_id)?;
        // Writes always act as the PM, with the PM's own token.
        let result = self
            .server
            .call(caller.user, mapping, up, CallKind::Write)
            .await?;
        mapping::source_ref(&mapping.result, &self.system, &result)
    }

    async fn ready(&self) -> Result<(), String> {
        self.server.check().await
    }
}

/// The upstream arguments a write would send for `args`: for the audit's
/// `upstream_args_hash` and the mapping tests.
pub fn upstream_args_for(
    connector: &McpConnector,
    action: &str,
    args: &Value,
    run_id: &str,
) -> Result<Map<String, Value>, ConnectorError> {
    let mapping = connector.mapping(action, ToolClass::Write)?;
    mapping::upstream_args(&mapping.args, args, run_id)
}
