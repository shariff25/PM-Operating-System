//! A registry system served by an MCP server, behind the existing [`Connector`] trait
//! (`docs/mcp-servers-spec.md` §2.1, §2.5). `dispatch` and `write` in `service.rs` call it as
//! they call a built-in connector, so the policy checks, the approval gate, the audit event and
//! the ledger happen before and after the MCP call exactly as for a connector.
//!
//! PM OS-side rules stay here, applied on the arguments before the call and on the result after
//! it: a team bot's read scope (`connectors/scope.rs`), and `jira.rollup` computed over the
//! mapped search; Confluence's version check, HTML escaping and footer (slice M8); Slack's
//! marker and, through the record's approved checks, the audience a post needs. Writes act as
//! the PM (never a team bot). A Slack record without both audience checks serves reads only:
//! its posts stay on the built-in connector ([`super::split::ReadsOnly`]).

use std::sync::Arc;

use async_trait::async_trait;
use pmos_contract::mcp::{Adapter, CheckRole, ContentMap, ToolClass, ToolMapping};
use pmos_contract::Sensitivity;
use serde_json::{json, Map, Value};

use super::mapping;
use super::server::{CallKind, McpServer};
use crate::connectors::http::{arg, issue_key};
use crate::connectors::{
    rollup, Audience, Caller, Connector, ConnectorError, Conversation, Item, Route, SourceRef,
    TeamBot, MAX_AUDIENCE_MEMBERS,
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
            // As the lint: PM OS's own rules for a Confluence or Slack write run only through
            // their adapter, so a write without it is never served.
            for a in [Adapter::ConfluenceStorage, Adapter::SlackText] {
                if a.tools().contains(&t.registry.as_str()) && t.adapter != Some(a) {
                    return Err(format!(
                        "`{}`: {} needs `adapter: {}`",
                        record.id,
                        t.registry,
                        a.as_str()
                    ));
                }
            }
            if t.registry == "slack.post_reply" && !t.args.drop.iter().any(|d| d == "message_ts") {
                return Err(format!(
                    "`{}`: slack.post_reply must drop `message_ts`",
                    record.id
                ));
            }
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
        self.server
            .record()
            .tool(&name)
            // A Slack post this server doesn't serve isn't routed here.
            .filter(|t| t.class() == ToolClass::Read || self.posts_on_mcp())
            .map(|t| Route {
                server: self.server.record().id.clone(),
                digest: t.digest.clone(),
            })
    }

    fn upstream_args(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Option<Map<String, Value>> {
        if !self.posts_on_mcp() {
            return None;
        }
        upstream_args_for(self, action, args, caller.run_id).ok()
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
        let mut items = mapping::items(&mapping.result, &self.system, &result)?;
        if (self.system.as_str(), action) == ("slack", "read_thread") {
            // Each message as the built-in connector returns it: at most 2,000 characters
            // (slice M8; the server's answer is never trusted to be short).
            for item in &mut items {
                clip_thread_messages(item);
            }
        }
        // A search answers at most the limit asked for (default 10, as the built-in
        // connectors), whatever the server sent (slice M8). A team bot's widened Slack search
        // is cut back to it after dropping what is outside its scope.
        let asked = crate::connectors::http::limit(original, 10, 50) as usize;
        let Some(bot) = caller.team_bot else {
            if action == "search" {
                items.truncate(asked);
            }
            return Ok(items);
        };
        if action == "search" {
            Ok(items
                .into_iter()
                .filter(|i| self.item_in_scope(bot, i).is_ok())
                .take(asked)
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
        if self.system == "slack" && !self.posts_on_mcp() {
            return Err(ConnectorError::Unsupported(format!(
                "slack.{action} (Slack posts stay on the built-in connector without the audience checks)"
            )));
        }
        // PM OS's own rules on the arguments, before anything is sent: the shapes the built-in
        // connector checks, and a Confluence page's version.
        self.check_write(caller, action, args).await?;
        let up = mapping::write_args(mapping, args, caller.run_id)?;
        // Writes always act as the PM, with the PM's own token.
        let result = self
            .server
            .call(caller.user, mapping, up, CallKind::Write)
            .await?;
        mapping::written_ref(
            &mapping.result,
            &self.system,
            &result,
            self.mode == "fake-mcp",
        )
    }

    async fn audience(
        &self,
        caller: Caller<'_>,
        channel: &str,
    ) -> Result<Audience, ConnectorError> {
        let (conversation, flags) = self.conversation_check(caller, channel).await?;
        let members = self.members_check(caller, channel).await?;
        Ok(Audience {
            external: flags.external,
            shared: flags.shared,
            im: flags.im,
            members: members.0,
            truncated: members.1,
            conversation,
        })
    }

    async fn conversation(
        &self,
        caller: Caller<'_>,
        channel: &str,
    ) -> Result<Conversation, ConnectorError> {
        Ok(self.conversation_check(caller, channel).await?.0)
    }

    async fn ready(&self) -> Result<(), String> {
        self.server.check().await
    }
}

/// A Slack thread's messages clipped as the built-in connector clips them
/// ([`crate::connectors::slack::MAX_MESSAGE_CHARS`] each); a clipped thread says `truncated`.
fn clip_thread_messages(item: &mut Item) {
    let mut cut = false;
    if let Some(messages) = item.content["messages"].as_array_mut() {
        for m in messages {
            if let Some(text) = m.get("text").and_then(Value::as_str) {
                let (text, was_cut) =
                    crate::connectors::clip(text, crate::connectors::slack::MAX_MESSAGE_CHARS);
                cut |= was_cut;
                m["text"] = Value::String(text);
            }
        }
    }
    if cut {
        item.content["truncated"] = Value::Bool(true);
    }
}

/// A Slack conversation's sharing, as a conversation check read it.
struct Sharing {
    external: bool,
    shared: bool,
    im: bool,
}

/// A Slack conversation id (`C…`, `G…`, `D…`), as the built-in connector accepts one.
fn slack_conversation_id(value: &str) -> Result<&str, ConnectorError> {
    if !value.is_empty()
        && value.len() <= 32
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        Ok(value)
    } else {
        Err(ConnectorError::BadArgs(format!(
            "`{value}` is not a Slack id"
        )))
    }
}

/// A Slack message timestamp, such as `1731500000.000100`.
fn slack_ts(value: &str) -> Result<&str, ConnectorError> {
    let ok = value.split_once('.').is_some_and(|(a, b)| {
        !a.is_empty() && !b.is_empty() && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())
    });
    if ok {
        Ok(value)
    } else {
        Err(ConnectorError::BadArgs(format!(
            "`{value}` is not a Slack message timestamp"
        )))
    }
}

impl McpConnector {
    /// Do Slack posts go to this server? Only when its record approves both audience checks
    /// (slice M8, `docs/mcp-servers-spec.md` §2.6); otherwise they stay on the built-in
    /// connector ([`super::split::ReadsOnly`]).
    pub fn posts_on_mcp(&self) -> bool {
        self.system != "slack" || self.server.record().slack_audience()
    }

    /// The checks the built-in connector makes before a write, made here before anything is
    /// sent; and for `confluence.update_page`, the page's version, read with the PM's own
    /// token through the approved `confluence.read`: a page someone saved since PM OS read it
    /// is refused with nothing written.
    async fn check_write(
        &self,
        caller: Caller<'_>,
        action: &str,
        args: &Value,
    ) -> Result<(), ConnectorError> {
        use crate::connectors::http::{key_like, numeric_id};
        match (self.system.as_str(), action) {
            ("confluence", "create_draft") => {
                key_like(arg(args, "space")?, "Confluence space key")?;
                if let Some(parent) = args.get("parent_id").and_then(Value::as_str) {
                    numeric_id(parent, "Confluence page id")?;
                }
            }
            ("confluence", "update_page") => {
                let id = numeric_id(arg(args, "page_id")?, "Confluence page id")?;
                let named = args["version"]
                    .as_u64()
                    .filter(|v| *v >= 1)
                    .ok_or_else(|| {
                        ConnectorError::BadArgs(
                            "`version` is required: the page's version as confluence_read returned it"
                                .into(),
                        )
                    })?;
                let read = self.mapping("read", ToolClass::Read)?;
                let up =
                    mapping::upstream_args(&read.args, &json!({"page_id": id}), caller.run_id)?;
                let result = self
                    .server
                    .call(caller.user, read, up, CallKind::Read)
                    .await?;
                let current = mapping::items(&read.result, "confluence", &result)?
                    .into_iter()
                    .find(|i| i.source.id == id)
                    .and_then(|i| i.content["version"].as_u64())
                    .ok_or_else(|| {
                        ConnectorError::Unavailable(format!(
                            "Confluence page {id}'s version couldn't be read; nothing was written"
                        ))
                    })?;
                if current != named {
                    return Err(ConnectorError::BadArgs(
                        crate::connectors::PAGE_CHANGED.into(),
                    ));
                }
            }
            ("slack", "post_reply") => {
                slack_conversation_id(arg(args, "channel")?)?;
                slack_ts(arg(args, "thread_ts")?)?;
            }
            ("slack", _) => {
                slack_conversation_id(arg(args, "channel")?)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// A conversation's kind, name and sharing, through the approved conversation check, as
    /// the PM (never a team bot). A sharing flag the answer doesn't give is never guessed.
    async fn conversation_check(
        &self,
        caller: Caller<'_>,
        channel: &str,
    ) -> Result<(Conversation, Sharing), ConnectorError> {
        let check = self
            .server
            .record()
            .approved_check(CheckRole::SlackConversation)
            .filter(|_| self.system == "slack")
            .ok_or_else(|| ConnectorError::Unsupported(format!("{}.conversation", self.system)))?;
        let channel = slack_conversation_id(channel)?;
        let mapping = check.as_mapping();
        let up = mapping::upstream_args(&check.args, &json!({"channel": channel}), caller.run_id)?;
        let result = self
            .server
            .call(caller.user, &mapping, up, CallKind::Read)
            .await?;
        let info = mapping::raw_items(&check.result, &result)?
            .into_iter()
            .next()
            .cloned()
            .ok_or_else(|| {
                ConnectorError::Unavailable("the server described no conversation".into())
            })?;
        let fields = match &check.result.content {
            Some(ContentMap::Fields(m)) => m,
            _ => {
                return Err(ConnectorError::Unavailable(
                    "the conversation check names no fields".into(),
                ))
            }
        };
        let at = |k: &str| fields.get(k).and_then(|p| mapping::get_path(&info, p));
        let flag = |k: &str| -> Result<bool, ConnectorError> {
            match at(k) {
                Some(Value::Bool(b)) => Ok(*b),
                None | Some(Value::Null) if !CheckRole::CONVERSATION_REQUIRED.contains(&k) => {
                    Ok(false)
                }
                _ => Err(ConnectorError::Unavailable(format!(
                    "the server's answer doesn't say `{k}` for channel {channel}; nothing is posted"
                ))),
            }
        };
        let sharing = Sharing {
            external: flag("external")?,
            shared: flag("shared")?,
            im: flag("im")?,
        };
        // Read as the built-in connector reads `conversations.info`: only a name shaped like a
        // channel name, only a member id.
        let slack_like = json!({
            "is_im": sharing.im,
            "is_mpim": flag("mpim")?,
            "is_private": flag("private")?,
            "name": at("name").cloned().unwrap_or(Value::Null),
            "user": at("user").cloned().unwrap_or(Value::Null),
        });
        Ok((Conversation::from_info(channel, &slack_like), sharing))
    }

    /// A conversation's members, through the approved members check, as the PM, page by page
    /// up to [`MAX_AUDIENCE_MEMBERS`]; and whether the list stopped there. An entry not shaped
    /// like a member id fails the check: an outsider is never dropped from the count.
    async fn members_check(
        &self,
        caller: Caller<'_>,
        channel: &str,
    ) -> Result<(Vec<String>, bool), ConnectorError> {
        let check = self
            .server
            .record()
            .approved_check(CheckRole::SlackMembers)
            .filter(|_| self.system == "slack")
            .ok_or_else(|| ConnectorError::Unsupported(format!("{}.audience", self.system)))?;
        let channel = slack_conversation_id(channel)?;
        let mapping = check.as_mapping();
        let mut members: Vec<String> = Vec::new();
        let mut cursor = String::new();
        let truncated = loop {
            let page = (MAX_AUDIENCE_MEMBERS - members.len()).min(200);
            let mut given = json!({"channel": channel, "limit": page});
            if !cursor.is_empty() {
                given["cursor"] = json!(cursor);
            }
            let up = mapping::upstream_args(&check.args, &given, caller.run_id)?;
            let result = self
                .server
                .call(caller.user, &mapping, up, CallKind::Read)
                .await?;
            let before = members.len();
            for v in mapping::raw_items(&check.result, &result)? {
                let id = v
                    .as_str()
                    .and_then(crate::connectors::slack_user_id)
                    .ok_or_else(|| {
                        ConnectorError::Unavailable(format!(
                            "the server listed a member of {channel} that isn't a member id; nothing is posted"
                        ))
                    })?;
                members.push(id);
            }
            cursor = check
                .result
                .next_token
                .as_deref()
                .and_then(|p| mapping::get_path(&result, p))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if cursor.is_empty() {
                break false;
            }
            // At the cap, or a page that adds nobody but names a cursor: there may be members
            // not read.
            if members.len() >= MAX_AUDIENCE_MEMBERS || members.len() == before {
                break true;
            }
        };
        // A server that answered more than it was asked for: the list stops at the cap, and
        // says so (more refusing than trusting the page size).
        let truncated = truncated || members.len() > MAX_AUDIENCE_MEMBERS;
        members.truncate(MAX_AUDIENCE_MEMBERS);
        Ok((members, truncated))
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
    mapping::write_args(mapping, args, run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::slack::MAX_MESSAGE_CHARS;

    #[test]
    fn a_threads_messages_are_clipped_as_the_built_in_connector_clips_them() {
        let long = "x".repeat(MAX_MESSAGE_CHARS + 5);
        let mut item = Item {
            source: SourceRef {
                system: "slack".into(),
                id: "C0PMTEAM:1731600000.000100".into(),
                url: String::new(),
            },
            title: "Thread in C0PMTEAM".into(),
            content: json!({"messages": [{"user": "U1", "ts": "1.2", "text": long}, {"user": "U2", "ts": "1.3", "text": "short"}]}),
        };
        clip_thread_messages(&mut item);
        let messages = item.content["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["text"].as_str().unwrap().chars().count(),
            MAX_MESSAGE_CHARS
        );
        assert_eq!(messages[1]["text"], "short");
        assert_eq!(item.content["truncated"], true);
        let mut short = item.clone();
        short.content = json!({"messages": [{"text": "short"}]});
        clip_thread_messages(&mut short);
        assert!(short.content.get("truncated").is_none());
    }
}
