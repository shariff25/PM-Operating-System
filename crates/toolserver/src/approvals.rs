//! Approvals for writes outside PM OS (OUT-4).
//!
//! A write tool call runs only if it matches a **grant**: the PM approved exactly this call
//! (same session, same tool, same arguments) in the harness, and the orchestrator recorded
//! it here. Grants are single-use and short-lived. A write with no grant never runs; it
//! becomes a pending **approval item** in the PM's inbox, and runs only when its own user
//! approves it there.
//!
//! Slack drafts keep their text a limited time (`docs/slack-reply-spec.md` §3): an undecided
//! one becomes `expired` after `PMOS_SLACK_DRAFT_TTL_HOURS`, and a decided one loses its text
//! that long after its decision. Either way only `channel`, `thread_ts`, `message_ts` and the
//! args hash stay.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pmos_contract::Sensitivity;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ledger::{args_edit_distance, LedgerOutcome, LedgerQuery, LedgerRow, Reason};
use crate::store::{StoreError, StoreResult};

/// How long a grant stays valid after the PM approves.
pub const GRANT_TTL_MINUTES: i64 = 15;

/// The Slack drafts that expire and lose their text after `PMOS_SLACK_DRAFT_TTL_HOURS`
/// (`docs/slack-reply-spec.md` §3).
pub const SLACK_DRAFT_TOOLS: [&str; 2] = ["slack.post_reply", "slack.send_message"];
/// The one tool that drafts at most once per thread, or once per message with `message_ts`.
pub const REPLY_TOOL: &str = "slack.post_reply";
/// What a removed string argument reads.
pub const EXPIRED_TEXT: &str = "[expired]";
/// The arguments an expired or decided Slack draft keeps: where it was for, not what it said.
/// `message_ts` is the tagged message a reply answers (`docs/slack-mentions-and-home-spec.md`
/// §3): an id, not text.
pub const KEPT_ARGS: [&str; 3] = ["channel", "thread_ts", "message_ts"];

/// `args` with every string replaced by [`EXPIRED_TEXT`], at any depth, except a top-level
/// `channel`, `thread_ts` and `message_ts`.
pub fn redact_args(args: &Value) -> Value {
    fn all(value: &Value) -> Value {
        match value {
            Value::String(_) => Value::String(EXPIRED_TEXT.into()),
            Value::Array(items) => Value::Array(items.iter().map(all).collect()),
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), all(v))).collect())
            }
            other => other.clone(),
        }
    }
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let kept = KEPT_ARGS.contains(&k.as_str()) && v.is_string();
                    (k.clone(), if kept { v.clone() } else { all(v) })
                })
                .collect(),
        ),
        other => all(other),
    }
}

/// The thread a `slack.post_reply` call is for: its `channel` and `thread_ts`.
pub fn reply_thread(args: &Value) -> Option<(&str, &str)> {
    Some((args["channel"].as_str()?, args["thread_ts"].as_str()?))
}

/// The message a `slack.post_reply` call answers, if it names one: its `message_ts`. A PM
/// who tags PM OS twice in one thread gets a draft for each tag.
pub fn reply_message(args: &Value) -> Option<&str> {
    args["message_ts"].as_str()
}

/// SHA-256 of the canonical JSON form of `args` (object keys sorted at every level).
pub fn args_hash(args: &Value) -> String {
    let canonical = serde_json::to_string(&canonicalize(args)).expect("json value serializes");
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, Value> =
                map.iter().map(|(k, v)| (k, canonicalize(v))).collect();
            let mut out = serde_json::Map::new();
            for (k, v) in sorted {
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for its user to decide.
    Pending,
    /// Approved; the write is running. An item left here (a crash mid-write) is resolved
    /// by the start-up reconciler as `failed`, outcome unknown.
    Executing,
    /// Approved, and the write succeeded; `result` holds what was written.
    Approved,
    Rejected,
    /// Approved, but the write failed or its outcome is unknown; `result` says which.
    Failed,
    /// A Slack draft nobody decided within `PMOS_SLACK_DRAFT_TTL_HOURS`. Nothing was posted,
    /// and its text is gone.
    Expired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "executing" => Some(Self::Executing),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "failed" => Some(Self::Failed),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalItem {
    pub id: String,
    pub user: String,
    /// The skill whose session asked for the write; `None` when a bot's session did.
    pub skill: Option<String>,
    /// The bot whose session asked for the write, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
    pub run_id: String,
    /// Registry name, such as `confluence.create_draft`.
    pub tool: String,
    pub args: Value,
    pub args_hash: String,
    pub status: Status,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The highest data class the drafting session had read when the item was created.
    /// `None` for items made before it was recorded, which count as `restricted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources_sensitivity: Option<Sensitivity>,
    /// When a Slack draft's text was removed: on expiry, or a day after its decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Edit,
    Reject,
}

/// A decision with what came with it: the edited arguments for `edit`, optionally a reason
/// for `reject` (the ledger keeps it), and for a Slack write, whether the PM confirmed
/// posting to an externally shared channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub decision: Decision,
    pub args: Option<Value>,
    pub reason: Option<Reason>,
    pub confirm_external: bool,
}

impl Verdict {
    pub fn new(decision: Decision, args: Option<Value>) -> Self {
        Self {
            decision,
            args,
            reason: None,
            confirm_external: false,
        }
    }

    /// The reason to store: only a rejection keeps one.
    fn stored_reason(&self) -> Option<Reason> {
        self.reason.filter(|_| self.decision == Decision::Reject)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecideError {
    #[error("approval item not found")]
    NotFound,
    #[error("only the item's own user can decide it")]
    Forbidden,
    #[error("approval item is already decided")]
    NotPending,
    /// A Slack draft nobody decided in time: nothing was posted.
    #[error("this draft expired before it was decided; nothing was posted")]
    Expired,
    #[error("an edit decision needs the edited arguments")]
    MissingArgs,
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct NewItem<'a> {
    pub user: &'a str,
    /// The session's skill, or `None` for a bot session.
    pub skill: Option<&'a str>,
    /// The session's bot, for a bot session.
    pub bot: Option<&'a str>,
    pub run_id: &'a str,
    pub tool: &'a str,
    pub args: &'a Value,
    /// For the ledger: the content repo's `HEAD` when the item was drafted.
    pub content_sha: Option<&'a str>,
    /// For the ledger: the drafting role's gateway alias, such as `pmos/default`.
    pub model_alias: Option<&'a str>,
    /// The highest data class the drafting session had read.
    pub sources_sensitivity: Sensitivity,
}

impl NewItem<'_> {
    /// The item's ledger row, as created.
    pub fn ledger_row(&self, item: &ApprovalItem) -> LedgerRow {
        LedgerRow {
            id: item.id.clone(),
            run_id: item.run_id.clone(),
            user: item.user.clone(),
            bot: item.bot.clone(),
            skill: item.skill.clone(),
            tool: item.tool.clone(),
            content_sha: self.content_sha.map(String::from),
            model_alias: self.model_alias.map(String::from),
            outcome: LedgerOutcome::Pending,
            edit_distance: None,
            reason: None,
            created_at: item.created_at,
            decided_at: None,
            decided_by: None,
        }
    }

    pub fn into_item(&self, now: DateTime<Utc>) -> ApprovalItem {
        ApprovalItem {
            id: format!("apr_{}", uuid::Uuid::new_v4().simple()),
            user: self.user.to_string(),
            skill: self.skill.map(String::from),
            bot: self.bot.map(String::from),
            run_id: self.run_id.to_string(),
            tool: self.tool.to_string(),
            args: self.args.clone(),
            args_hash: args_hash(self.args),
            status: Status::Pending,
            created_at: now,
            decided_at: None,
            decided_by: None,
            result: None,
            sources_sensitivity: Some(self.sources_sensitivity),
            redacted_at: None,
        }
    }
}

/// What [`ApprovalRepo::create_reply`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Drafted {
    New(ApprovalItem),
    /// A reply to the same thread was already waiting, being posted, or posted.
    Existing(ApprovalItem),
}

/// Does `item` stop `user` drafting another reply to `thread`? It does if it is their reply
/// to that thread, created at or after `since`, and waiting, being posted or posted. With
/// `message` (the new call's `message_ts`), only a reply to that same message does.
pub fn blocks_reply(
    item: &ApprovalItem,
    user: &str,
    thread: (&str, &str),
    message: Option<&str>,
    since: DateTime<Utc>,
) -> bool {
    item.user == user
        && item.tool == REPLY_TOOL
        && item.created_at >= since
        && matches!(
            item.status,
            Status::Pending | Status::Executing | Status::Approved
        )
        && reply_thread(&item.args) == Some(thread)
        && message.is_none_or(|m| reply_message(&item.args) == Some(m))
}

/// Grants, approval items and the feedback ledger (`ledger.rs`). `session_key` is the
/// session token's hash ([`crate::session::token_hash`]), never the token itself.
///
/// Creating, deciding and completing an item update its ledger row in the same operation.
#[async_trait]
pub trait ApprovalRepo: Send + Sync {
    /// Record that the PM approved this exact call; valid for [`GRANT_TTL_MINUTES`].
    async fn add_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<()>;

    /// Consume one live grant matching this exact call. Returns whether one existed. Two
    /// concurrent calls with one grant: exactly one gets `true`.
    async fn take_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<bool>;

    /// Create a pending item and its ledger row.
    async fn create(&self, new: NewItem<'_>, now: DateTime<Utc>) -> StoreResult<ApprovalItem>;

    /// Create a pending reply item, unless the same user has a reply to the same thread
    /// (`channel` and `thread_ts`) created at or after `since` that is pending, executing or
    /// approved: then return that one and create nothing. When the new call names a
    /// `message_ts`, only a reply to that same message counts. Of two concurrent calls for
    /// one thread, exactly one creates. Arguments that name no thread are created as usual.
    async fn create_reply(
        &self,
        new: NewItem<'_>,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Drafted>;

    async fn get(&self, id: &str) -> StoreResult<Option<ApprovalItem>>;

    /// Items for one user, newest first, optionally filtered by status.
    async fn list(&self, user: &str, status: Option<Status>) -> StoreResult<Vec<ApprovalItem>>;

    /// Record a decision with no reason. See [`ApprovalRepo::decide_with`].
    async fn decide(
        &self,
        id: &str,
        decided_by: &str,
        decision: Decision,
        edited_args: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        self.decide_with(id, decided_by, Verdict::new(decision, edited_args), now)
            .await
    }

    /// Record a decision. Only the item's own user may decide, and only once: of two
    /// concurrent decisions, exactly one succeeds. Approve and edit move the item to
    /// `executing` and return it with the arguments to run; the caller runs the write and then
    /// calls `complete`.
    ///
    /// The ledger row takes the decision: a rejection with its reason, or an approval with
    /// the edit distance between the original and the final arguments (`0.0` unchanged).
    async fn decide_with(
        &self,
        id: &str,
        decided_by: &str,
        verdict: Verdict,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError>;

    /// Store the outcome of an executing write: `approved` with the result, or `failed`
    /// with the error. Only an `executing` item changes; `None` otherwise. The ledger row
    /// becomes `approved`, `edited` (approved after a change to its arguments) or `failed`.
    async fn complete(
        &self,
        id: &str,
        result: Result<Value, String>,
    ) -> StoreResult<Option<ApprovalItem>>;

    /// Items still `executing` that were decided at or before `before`: writes interrupted
    /// by a crash or restart.
    async fn stuck(&self, before: DateTime<Utc>) -> StoreResult<Vec<ApprovalItem>>;

    /// Ledger rows matching `query`, as reported (with `expired` worked out), newest
    /// activity first.
    async fn ledger(&self, query: &LedgerQuery) -> StoreResult<Vec<LedgerRow>>;

    /// Expire `pending` items for `tools` created at or before `before`: each becomes
    /// `expired`, its arguments go through [`redact_args`] (the args hash is kept),
    /// `redacted_at` is `now`, and its ledger row is stored as `expired`. Returns them,
    /// oldest first.
    async fn expire(
        &self,
        tools: &[&str],
        before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Vec<ApprovalItem>>;

    /// Remove the text of decided (`approved`, `rejected` or `failed`) items for `tools`,
    /// decided at or before `before`, that still have it, the same way. Returns how many.
    async fn redact_decided(
        &self,
        tools: &[&str],
        before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<usize>;
}

#[derive(Debug, Clone)]
struct Grant {
    session_key: String,
    tool: String,
    args_hash: String,
    expires_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct MemoryApprovals {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    grants: Vec<Grant>,
    items: BTreeMap<String, ApprovalItem>,
    ledger: BTreeMap<String, Ledgered>,
}

/// A stored ledger row, and whether the decision changed the item's arguments.
struct Ledgered {
    row: LedgerRow,
    edited: bool,
}

#[async_trait]
impl ApprovalRepo for MemoryApprovals {
    async fn add_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<()> {
        let mut state = self.state.lock().expect("approvals lock");
        state.grants.retain(|g| g.expires_at > now);
        state.grants.push(Grant {
            session_key: session_key.to_string(),
            tool: tool.to_string(),
            args_hash: args_hash(args),
            expires_at: now + Duration::minutes(GRANT_TTL_MINUTES),
        });
        Ok(())
    }

    async fn take_grant(
        &self,
        session_key: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let hash = args_hash(args);
        let mut state = self.state.lock().expect("approvals lock");
        state.grants.retain(|g| g.expires_at > now);
        let found = state
            .grants
            .iter()
            .position(|g| g.session_key == session_key && g.tool == tool && g.args_hash == hash);
        Ok(match found {
            Some(i) => {
                state.grants.swap_remove(i);
                true
            }
            None => false,
        })
    }

    async fn create(&self, new: NewItem<'_>, now: DateTime<Utc>) -> StoreResult<ApprovalItem> {
        let item = new.into_item(now);
        let row = new.ledger_row(&item);
        let mut state = self.state.lock().expect("approvals lock");
        state.items.insert(item.id.clone(), item.clone());
        state
            .ledger
            .insert(item.id.clone(), Ledgered { row, edited: false });
        Ok(item)
    }

    async fn create_reply(
        &self,
        new: NewItem<'_>,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Drafted> {
        let item = new.into_item(now);
        let row = new.ledger_row(&item);
        let mut state = self.state.lock().expect("approvals lock");
        if let Some(thread) = reply_thread(new.args) {
            let existing = state
                .items
                .values()
                .filter(|i| blocks_reply(i, new.user, thread, reply_message(new.args), since))
                .max_by_key(|i| i.created_at);
            if let Some(existing) = existing {
                return Ok(Drafted::Existing(existing.clone()));
            }
        }
        state.items.insert(item.id.clone(), item.clone());
        state
            .ledger
            .insert(item.id.clone(), Ledgered { row, edited: false });
        Ok(Drafted::New(item))
    }

    async fn get(&self, id: &str) -> StoreResult<Option<ApprovalItem>> {
        Ok(self
            .state
            .lock()
            .expect("approvals lock")
            .items
            .get(id)
            .cloned())
    }

    async fn list(&self, user: &str, status: Option<Status>) -> StoreResult<Vec<ApprovalItem>> {
        let state = self.state.lock().expect("approvals lock");
        let mut items: Vec<_> = state
            .items
            .values()
            .filter(|i| i.user == user && status.is_none_or(|s| i.status == s))
            .cloned()
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.created_at));
        Ok(items)
    }

    async fn decide_with(
        &self,
        id: &str,
        decided_by: &str,
        verdict: Verdict,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        let mut state = self.state.lock().expect("approvals lock");
        let item = state.items.get_mut(id).ok_or(DecideError::NotFound)?;
        if item.user != decided_by {
            return Err(DecideError::Forbidden);
        }
        if item.status == Status::Expired {
            return Err(DecideError::Expired);
        }
        if item.status != Status::Pending {
            return Err(DecideError::NotPending);
        }
        let reason = verdict.stored_reason();
        let (distance, edited) = match verdict.decision {
            Decision::Reject => {
                item.status = Status::Rejected;
                (None, false)
            }
            Decision::Approve => {
                item.status = Status::Executing;
                (Some(0.0), false)
            }
            Decision::Edit => {
                let args = verdict.args.ok_or(DecideError::MissingArgs)?;
                let distance = args_edit_distance(&item.args, &args);
                let new_hash = args_hash(&args);
                let edited = new_hash != item.args_hash;
                item.args_hash = new_hash;
                item.args = args;
                item.status = Status::Executing;
                (Some(distance), edited)
            }
        };
        item.decided_at = Some(now);
        item.decided_by = Some(decided_by.to_string());
        let item = item.clone();
        if let Some(entry) = state.ledger.get_mut(id) {
            if verdict.decision == Decision::Reject {
                entry.row.outcome = LedgerOutcome::Rejected;
            }
            entry.row.edit_distance = distance;
            entry.row.reason = reason;
            entry.row.decided_at = Some(now);
            entry.row.decided_by = Some(decided_by.to_string());
            entry.edited = edited;
        }
        Ok(item)
    }

    async fn complete(
        &self,
        id: &str,
        result: Result<Value, String>,
    ) -> StoreResult<Option<ApprovalItem>> {
        let mut state = self.state.lock().expect("approvals lock");
        let Some(item) = state
            .items
            .get_mut(id)
            .filter(|i| i.status == Status::Executing)
        else {
            return Ok(None);
        };
        let failed = result.is_err();
        match result {
            Ok(value) => {
                item.status = Status::Approved;
                item.result = Some(value);
            }
            Err(message) => {
                item.status = Status::Failed;
                item.result = Some(serde_json::json!({ "error": message }));
            }
        }
        let item = item.clone();
        if let Some(entry) = state.ledger.get_mut(id) {
            entry.row.outcome = match (failed, entry.edited) {
                (true, _) => LedgerOutcome::Failed,
                (false, true) => LedgerOutcome::Edited,
                (false, false) => LedgerOutcome::Approved,
            };
        }
        Ok(Some(item))
    }

    async fn stuck(&self, before: DateTime<Utc>) -> StoreResult<Vec<ApprovalItem>> {
        let state = self.state.lock().expect("approvals lock");
        Ok(state
            .items
            .values()
            .filter(|i| i.status == Status::Executing && i.decided_at.is_some_and(|d| d <= before))
            .cloned()
            .collect())
    }

    async fn ledger(&self, query: &LedgerQuery) -> StoreResult<Vec<LedgerRow>> {
        let state = self.state.lock().expect("approvals lock");
        let mut rows: Vec<LedgerRow> = state
            .ledger
            .values()
            .map(|e| e.row.clone().reported(query.expire_before))
            .filter(|r| query.matches(r))
            .collect();
        rows.sort_by(|a, b| {
            b.activity_at()
                .cmp(&a.activity_at())
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(limit) = query.limit {
            rows.truncate(limit);
        }
        Ok(rows)
    }

    async fn expire(
        &self,
        tools: &[&str],
        before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<Vec<ApprovalItem>> {
        let mut state = self.state.lock().expect("approvals lock");
        let mut expired = Vec::new();
        for item in state.items.values_mut().filter(|i| {
            i.status == Status::Pending
                && tools.contains(&i.tool.as_str())
                && i.created_at <= before
        }) {
            item.status = Status::Expired;
            item.args = redact_args(&item.args);
            item.redacted_at = Some(now);
            expired.push(item.clone());
        }
        for item in &expired {
            if let Some(entry) = state.ledger.get_mut(&item.id) {
                entry.row.outcome = LedgerOutcome::Expired;
            }
        }
        expired.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(expired)
    }

    async fn redact_decided(
        &self,
        tools: &[&str],
        before: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> StoreResult<usize> {
        let mut state = self.state.lock().expect("approvals lock");
        let mut n = 0;
        for item in state.items.values_mut().filter(|i| {
            matches!(
                i.status,
                Status::Approved | Status::Rejected | Status::Failed
            ) && tools.contains(&i.tool.as_str())
                && i.redacted_at.is_none()
                && i.decided_at.is_some_and(|d| d <= before)
        }) {
            item.args = redact_args(&item.args);
            item.redacted_at = Some(now);
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hash_ignores_key_order() {
        let a = json!({"title": "PRD", "space": {"key": "PM", "id": 1}});
        let b = json!({"space": {"id": 1, "key": "PM"}, "title": "PRD"});
        assert_eq!(args_hash(&a), args_hash(&b));
        assert_ne!(args_hash(&a), args_hash(&json!({"title": "PRD2"})));
    }

    #[test]
    fn status_round_trips() {
        for s in [
            Status::Pending,
            Status::Executing,
            Status::Approved,
            Status::Rejected,
            Status::Failed,
            Status::Expired,
        ] {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        assert_eq!(Status::parse("maybe"), None);
    }

    #[test]
    fn redaction_keeps_only_where_the_draft_was_for() {
        let args = json!({
            "channel": "C0PMTEAM", "thread_ts": "1731600000.000100",
            "text": "Yes, SSO ships in 4.2.", "blocks": [{"text": "x", "n": 2}], "n": 1,
        });
        assert_eq!(
            redact_args(&args),
            json!({
                "channel": "C0PMTEAM", "thread_ts": "1731600000.000100",
                "text": EXPIRED_TEXT, "blocks": [{"text": EXPIRED_TEXT, "n": 2}], "n": 1,
            })
        );
        // Kept only at the top level, and only as strings.
        assert_eq!(
            redact_args(&json!({"nested": {"channel": "C1"}, "channel": ["C1"]})),
            json!({"nested": {"channel": EXPIRED_TEXT}, "channel": [EXPIRED_TEXT]})
        );
        assert_eq!(reply_thread(&args), Some(("C0PMTEAM", "1731600000.000100")));
        assert_eq!(reply_thread(&json!({"channel": "C1"})), None);
        assert_eq!(reply_message(&args), None);
        // The tagged message is kept too: it is an id.
        let tagged = json!({
            "channel": "C0PMTEAM", "thread_ts": "1731600000.000100",
            "message_ts": "1731600300.000200", "text": "Yes.",
        });
        assert_eq!(
            redact_args(&tagged),
            json!({
                "channel": "C0PMTEAM", "thread_ts": "1731600000.000100",
                "message_ts": "1731600300.000200", "text": EXPIRED_TEXT,
            })
        );
        assert_eq!(reply_message(&tagged), Some("1731600300.000200"));
    }
}
