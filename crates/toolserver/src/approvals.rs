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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pmos_contract::Sensitivity;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ledger::{args_edit_distance, LedgerOutcome, LedgerQuery, LedgerRow, Reason};
use crate::reviews::{NewRecommendation, RecommendError, Recommendation};
use crate::store::{StoreError, StoreResult};

/// How long a grant stays valid after the PM approves.
pub const GRANT_TTL_MINUTES: i64 = 15;
/// The most items one listing of a PM's approvals returns (`GET /internal/approvals`): the
/// newest. Every caller shows a PM's inbox and recent history, which is far below this; an
/// older item is still read by its id.
pub const LIST_LIMIT: usize = 200;
/// The safety cap on a PM's *pending* items in one inbox listing ([`ApprovalRepo::inbox`]).
/// Pending items are never cut at [`LIST_LIMIT`]: only past this much higher cap, and then
/// the answer says so (`truncated: true`, with `total_pending`).
pub const PENDING_CAP: usize = 1000;
/// The shortest `expected_args_hash` a decision may name: the start of the item's
/// `args_hash` (a Slack card's buttons carry 16 hex characters of it).
pub const MIN_EXPECTED_HASH: usize = 16;

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

/// Is `expected` (a decision's `expected_args_hash`) a well-formed start of an args hash:
/// [`MIN_EXPECTED_HASH`] to 64 lowercase hex characters?
pub fn valid_expected_hash(expected: &str) -> bool {
    (MIN_EXPECTED_HASH..=64).contains(&expected.len())
        && expected
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Does `item` still hold the arguments whose hash starts with `expected`? `None` (nothing
/// named) always matches; a malformed value never does.
pub fn still_as_shown(item: &ApprovalItem, expected: Option<&str>) -> bool {
    expected.is_none_or(|e| valid_expected_hash(e) && item.args_hash.starts_with(e))
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
    /// The persona (marketplace bot) whose session drafted it, copied from the session when
    /// the item is created and never changed (`docs/bot-marketplace-spec.md` §5). `None` (served
    /// as `null`) is Nudge, and every item from before personas.
    pub persona: Option<String>,
    /// The line its persona's pages carry (`draft-notice`, Echo's internal-review line), copied
    /// from the persona when the item is drafted and never changed: a saved edit and the
    /// approved write put it back at the top of the page from here, even once the persona's
    /// file is gone or says something else (gate-fix-4). Never served.
    #[serde(skip)]
    pub draft_notice: Option<String>,
    pub run_id: String,
    /// Registry name, such as `confluence.create_draft`.
    pub tool: String,
    pub args: Value,
    pub args_hash: String,
    /// The version of the draft's text: 1 as drafted, bumped each time a person changes its
    /// arguments to others (a saved edit, or an edit decision). A reviewer's recommendation
    /// names the revision it read (`crate::reviews`).
    pub revision: i64,
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
    /// The arguments as the bot drafted them, kept once a person changed `args` (a saved
    /// edit, [`ApprovalRepo::save_draft`], or an edit decision) so the ledger can measure the
    /// edit. Never served; `None` while `args` are the bot's own. Removed with the text.
    #[serde(skip)]
    pub drafted_args: Option<Value>,
    /// When a person last saved an edit of this pending draft ([`ApprovalRepo::save_draft`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<DateTime<Utc>>,
    /// A Slack draft's channel as `conversations.info` last described it
    /// ([`ApprovalRepo::set_target_conversation`]). Served as `target_channel` by
    /// `ToolService::approval_json`, only while it is for the channel the item names.
    #[serde(skip)]
    pub target_conversation: Option<TargetConversation>,
    /// For a write sent to an MCP server: the hash of the upstream arguments, what left PM OS
    /// (`docs/mcp-servers-spec.md` §2.6, slice M8). `None` for a built-in connector's write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_args_hash: Option<String>,
}

/// What the tool server last read of a Slack draft's channel: metadata only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetConversation {
    /// The channel id this describes.
    pub channel: String,
    #[serde(flatten)]
    pub conversation: crate::connectors::Conversation,
    pub checked_at: DateTime<Utc>,
}

impl ApprovalItem {
    /// Has a person changed the arguments the bot drafted?
    pub fn edited(&self) -> bool {
        self.drafted_args.is_some()
    }

    /// The arguments as the bot drafted them.
    pub fn drafted(&self) -> &Value {
        self.drafted_args.as_ref().unwrap_or(&self.args)
    }
}

/// What a person's change of `item`'s arguments to `args` keeps of the bot's own: the
/// drafted arguments, or `None` when `args` are exactly those again.
pub fn drafted_after(item: &ApprovalItem, args: &Value) -> Option<Value> {
    let drafted = item.drafted();
    (args_hash(drafted) != args_hash(args)).then(|| drafted.clone())
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
    /// The start of the `args_hash` the deciding person was shown. When set and the item now
    /// holds other arguments, the decision is refused as [`DecideError::Stale`] and nothing
    /// changes; the store checks it under the same lock as the decision.
    pub expected_args_hash: Option<String>,
    /// The personas offered now, when the decision must respect holds: an approve or an edit
    /// of an item drafted by a persona not in it, that no Admin released, is refused as
    /// [`DecideError::Held`] and nothing changes. The store checks it under the same lock as
    /// the decision (and the release), so a release or a withdrawal can't slip between the
    /// check and the decision (gate-fix-4). `None`: not checked (the store's own tests).
    pub hold: Option<Arc<BTreeSet<String>>>,
}

impl Verdict {
    pub fn new(decision: Decision, args: Option<Value>) -> Self {
        Self {
            decision,
            args,
            reason: None,
            confirm_external: false,
            expected_args_hash: None,
            hold: None,
        }
    }

    /// The same verdict, checked against holds: `offered` are the personas offered now.
    pub fn holding(mut self, offered: Arc<BTreeSet<String>>) -> Self {
        self.hold = Some(offered);
        self
    }

    /// The same verdict, naming the args hash the person deciding was shown.
    pub fn shown(mut self, expected_args_hash: Option<String>) -> Self {
        self.expected_args_hash = expected_args_hash;
        self
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
    /// The item's arguments changed since the person deciding was shown them (a saved edit
    /// in between): nothing was decided.
    #[error("this draft changed since it was shown; nothing was decided")]
    Stale,
    /// Drafted by a persona that is no longer offered and not released by an Admin: it can be
    /// rejected, not approved, edited or saved ([`Verdict::hold`]).
    #[error("this draft is held: the bot that drafted it isn't offered any more")]
    Held,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Is `item` held, given the personas `offered` now and whether it was `released`? A pending
/// item of a persona not offered, not released. The stores call it under their lock.
pub fn held_by(item: &ApprovalItem, offered: &BTreeSet<String>, released: bool) -> bool {
    item.status == Status::Pending
        && !released
        && item.persona.as_ref().is_some_and(|p| !offered.contains(p))
}

pub struct NewItem<'a> {
    pub user: &'a str,
    /// The session's skill, or `None` for a bot session.
    pub skill: Option<&'a str>,
    /// The session's bot, for a bot session.
    pub bot: Option<&'a str>,
    /// The session's persona; `None` for Nudge and for a skill session.
    pub persona: Option<&'a str>,
    /// The persona's draft notice when the item is drafted ([`ApprovalItem::draft_notice`]).
    pub draft_notice: Option<&'a str>,
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
            served_by: None,
        }
    }

    pub fn into_item(&self, now: DateTime<Utc>) -> ApprovalItem {
        ApprovalItem {
            id: format!("apr_{}", uuid::Uuid::new_v4().simple()),
            user: self.user.to_string(),
            skill: self.skill.map(String::from),
            bot: self.bot.map(String::from),
            persona: self.persona.map(String::from),
            draft_notice: self.draft_notice.map(String::from),
            run_id: self.run_id.to_string(),
            tool: self.tool.to_string(),
            args: self.args.clone(),
            args_hash: args_hash(self.args),
            revision: 1,
            status: Status::Pending,
            created_at: now,
            decided_at: None,
            decided_by: None,
            result: None,
            sources_sensitivity: Some(self.sources_sensitivity),
            redacted_at: None,
            drafted_args: None,
            edited_at: None,
            target_conversation: None,
            upstream_args_hash: None,
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

    /// Record which backend serves item `id`'s write (`served_by`: `builtin`, `fake` or an MCP
    /// server's id; slice M8). Kept on the item; its ledger row reads it from there.
    async fn note_served_by(&self, id: &str, served_by: &str) -> StoreResult<()>;

    /// Record on item `id` the hash of the upstream arguments its write sends to an MCP server
    /// (slice M8): written before the call.
    async fn note_upstream(&self, id: &str, upstream_args_hash: &str) -> StoreResult<()>;

    /// `user`'s items, optionally only those with `status`: newest first (ties by id), at
    /// most [`LIST_LIMIT`]. An inbox that must show every pending item reads
    /// [`ApprovalRepo::inbox`] instead.
    async fn list(&self, user: &str, status: Option<Status>) -> StoreResult<Vec<ApprovalItem>>;

    /// `user`'s inbox: every pending item, newest first (ties by id), up to `pending_cap`,
    /// with how many are pending in all; and the newest `decided_limit` items that aren't
    /// pending.
    async fn inbox(
        &self,
        user: &str,
        pending_cap: usize,
        decided_limit: usize,
    ) -> StoreResult<Inbox>;

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

    /// Save a person's edit of a pending item: `args` replace its arguments (and their hash),
    /// so every surface shows them and an approve anywhere writes them. The bot's own
    /// arguments are kept in `drafted_args` (saving them back clears it, and `edited_at`);
    /// `edited_at` is `now`. Only the item's own user, only while it is pending: the same
    /// refusals as a decision. The ledger row is unchanged until the decision.
    ///
    /// `raise_to` can only raise the item's `sources_sensitivity`, never lower it: the stored
    /// level becomes [`raised`] (the higher of the item's own, unrecorded counting as
    /// `restricted`, and `raise_to`), under the same lock as the save.
    async fn save_draft(
        &self,
        id: &str,
        user: &str,
        args: Value,
        raise_to: Option<Sensitivity>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        self.save_draft_held(id, user, args, raise_to, None, now)
            .await
    }

    /// [`ApprovalRepo::save_draft`], refused as [`DecideError::Held`] when `hold` (the
    /// personas offered now) is given and the item is held, checked under the same lock as the
    /// save.
    async fn save_draft_held(
        &self,
        id: &str,
        user: &str,
        args: Value,
        raise_to: Option<Sensitivity>,
        hold: Option<&BTreeSet<String>>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError>;

    /// Record what `conversations.info` said of a Slack draft's channel, replacing what was
    /// there (a renamed channel reads its new name). Changes nothing else; `false` when there
    /// is no such item.
    async fn set_target_conversation(
        &self,
        id: &str,
        target: &TargetConversation,
    ) -> StoreResult<bool>;

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

    /// Close `user`'s pending items as the system ([`SYSTEM_DECIDER`]): each becomes
    /// `rejected` with the ledger reason [`crate::ledger::Reason::OwnerDeactivated`], and no
    /// write runs. The user was deactivated and not reactivated within the grace
    /// (`docs/user-database-spec.md` §6). Returns the ids closed.
    async fn close_for_owner(&self, user: &str, now: DateTime<Utc>) -> StoreResult<Vec<String>>;

    /// The pending items of any of `users` (a reviewer's assigned PMs), newest first (ties by
    /// id), at most `cap`, with how many are pending in all.
    async fn pending_of(
        &self,
        users: &[String],
        cap: usize,
    ) -> StoreResult<(Vec<ApprovalItem>, usize)>;

    /// Store a reviewer's recommendation (`crate::reviews`), under the item's own lock, so a
    /// saved edit or a decision can't land between the check and the insert. Refused, with
    /// nothing stored ([`recommend_refusal`]): no such item; the reviewer is its owner; not
    /// pending (`Expired` for an expired Slack draft); the item's `revision` or `args_hash`
    /// isn't the one named (`DraftChanged`, with what it holds now). The reviewer's live
    /// recommendation on the item, if any, is withdrawn (and its note cleared) in the same
    /// operation. Never changes the item, its ledger row or anything else.
    async fn recommend(
        &self,
        new: NewRecommendation<'_>,
        now: DateTime<Utc>,
    ) -> Result<Recommendation, RecommendError>;

    /// The live recommendations on `approval_ids`, oldest first (ties by id).
    async fn recommendations(&self, approval_ids: &[String]) -> StoreResult<Vec<Recommendation>>;

    /// How many items each persona drafted since `since` (created at or after it), for `user`
    /// alone or, with `None`, for everyone: keyed by persona, with Nudge's (`persona` NULL)
    /// under [`pmos_contract::personas::ALWAYS_ON`]. Keys with no item are left out. Counts
    /// only: no item's content (`docs/bot-marketplace-spec.md` §6).
    async fn persona_counts(
        &self,
        user: Option<&str>,
        since: DateTime<Utc>,
    ) -> StoreResult<BTreeMap<String, u64>>;

    /// Release item `id` from the hold its withdrawn persona puts on it
    /// (`docs/bot-marketplace-spec.md` §6, "Removal"): an Admin's decision, recorded once with
    /// who and when. `false` when there is no such item or it was already released. Changes
    /// nothing else on the item.
    async fn release_hold(&self, id: &str, by: &str, now: DateTime<Utc>) -> StoreResult<bool>;

    /// Whether an Admin released item `id` from its persona's hold ([`Self::release_hold`]).
    async fn hold_released(&self, id: &str) -> StoreResult<bool>;

    /// The pending items drafted by a persona **not** in `offered` that no Admin released,
    /// newest first (ties by id), at most `cap`, with how many there are in all: with `offered`
    /// the keys offered now, these are the held drafts ([`crate::service::ToolService::held`];
    /// the admin view, slice A1). Nudge's items (`persona` NULL) are never among them.
    async fn unreleased_pending_outside(
        &self,
        offered: &[String],
        cap: usize,
    ) -> StoreResult<(Vec<ApprovalItem>, usize)>;
}

/// Why `new` can't be stored on `item` as it is now, checked in this order by both stores:
/// the owner, the status, then the version the reviewer read.
pub fn recommend_refusal(
    item: &ApprovalItem,
    new: &NewRecommendation<'_>,
) -> Result<(), RecommendError> {
    if item.user == new.reviewer {
        return Err(RecommendError::OwnDraft);
    }
    if item.status == Status::Expired {
        return Err(RecommendError::Expired);
    }
    if item.status != Status::Pending {
        return Err(RecommendError::NotPending);
    }
    if item.revision != new.draft_revision || item.args_hash != new.draft_sha256 {
        return Err(RecommendError::DraftChanged {
            revision: item.revision,
            args_hash: item.args_hash.clone(),
        });
    }
    Ok(())
}

/// Who closes a deactivated owner's held drafts, in `decided_by`.
pub const SYSTEM_DECIDER: &str = "system";

/// A PM's inbox as [`ApprovalRepo::inbox`] reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inbox {
    /// Pending items, newest first, at most the cap asked for.
    pub pending: Vec<ApprovalItem>,
    /// How many items are pending in all: more than `pending.len()` only past the cap.
    pub total_pending: usize,
    /// The newest items that aren't pending.
    pub decided: Vec<ApprovalItem>,
}

impl Inbox {
    /// Were pending items left out (past the safety cap)?
    pub fn truncated(&self) -> bool {
        self.total_pending > self.pending.len()
    }
}

/// The level a saved edit leaves an item at: the higher of its own (unrecorded counts as
/// `restricted`) and `raise_to`. Never lower than before.
pub fn raised(current: Option<Sensitivity>, raise_to: Option<Sensitivity>) -> Option<Sensitivity> {
    match raise_to {
        None => current,
        Some(r) => Some(current.unwrap_or(Sensitivity::Restricted).max(r)),
    }
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
    /// Every recommendation, live and withdrawn, in the order stored.
    recommendations: Vec<Recommendation>,
    /// Items an Admin released from their withdrawn persona's hold: who, and when.
    released: BTreeMap<String, (String, DateTime<Utc>)>,
}

impl State {
    /// Remove the notes on `id`'s recommendations: its Slack text is gone.
    fn remove_notes(&mut self, id: &str, now: DateTime<Utc>) {
        for r in self
            .recommendations
            .iter_mut()
            .filter(|r| r.approval_id == id && r.note.is_some())
        {
            r.note = None;
            r.note_removed_at = Some(now);
        }
    }
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

    async fn note_served_by(&self, id: &str, served_by: &str) -> StoreResult<()> {
        let mut state = self.state.lock().expect("approvals lock");
        if let Some(l) = state.ledger.get_mut(id) {
            l.row.served_by = Some(served_by.to_string());
        }
        Ok(())
    }

    async fn note_upstream(&self, id: &str, upstream_args_hash: &str) -> StoreResult<()> {
        let mut state = self.state.lock().expect("approvals lock");
        if let Some(item) = state.items.get_mut(id) {
            item.upstream_args_hash = Some(upstream_args_hash.to_string());
        }
        Ok(())
    }

    async fn list(&self, user: &str, status: Option<Status>) -> StoreResult<Vec<ApprovalItem>> {
        let state = self.state.lock().expect("approvals lock");
        let mut items: Vec<_> = state
            .items
            .values()
            .filter(|i| i.user == user && status.is_none_or(|s| i.status == s))
            .cloned()
            .collect();
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        items.truncate(LIST_LIMIT);
        Ok(items)
    }

    async fn inbox(
        &self,
        user: &str,
        pending_cap: usize,
        decided_limit: usize,
    ) -> StoreResult<Inbox> {
        let state = self.state.lock().expect("approvals lock");
        let mut mine: Vec<_> = state
            .items
            .values()
            .filter(|i| i.user == user)
            .cloned()
            .collect();
        mine.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let (mut pending, mut decided): (Vec<_>, Vec<_>) =
            mine.into_iter().partition(|i| i.status == Status::Pending);
        let total_pending = pending.len();
        pending.truncate(pending_cap);
        decided.truncate(decided_limit);
        Ok(Inbox {
            pending,
            total_pending,
            decided,
        })
    }

    async fn decide_with(
        &self,
        id: &str,
        decided_by: &str,
        verdict: Verdict,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        let mut state = self.state.lock().expect("approvals lock");
        let released = state.released.contains_key(id);
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
        if !still_as_shown(item, verdict.expected_args_hash.as_deref()) {
            return Err(DecideError::Stale);
        }
        if let (Some(offered), true) = (&verdict.hold, verdict.decision != Decision::Reject) {
            if held_by(item, offered, released) {
                return Err(DecideError::Held);
            }
        }
        let reason = verdict.stored_reason();
        // Measured from the bot's own arguments, so a saved edit approved as it stands
        // counts as edited.
        let (distance, edited) = match verdict.decision {
            Decision::Reject => {
                item.status = Status::Rejected;
                (None, false)
            }
            Decision::Approve => {
                item.status = Status::Executing;
                match &item.drafted_args {
                    Some(drafted) => (Some(args_edit_distance(drafted, &item.args)), true),
                    None => (Some(0.0), false),
                }
            }
            Decision::Edit => {
                let args = verdict.args.ok_or(DecideError::MissingArgs)?;
                let distance = args_edit_distance(item.drafted(), &args);
                let drafted = drafted_after(item, &args);
                let edited = drafted.is_some();
                let hash = args_hash(&args);
                if hash != item.args_hash {
                    item.revision += 1;
                }
                item.args_hash = hash;
                item.args = args;
                item.drafted_args = drafted;
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

    async fn save_draft_held(
        &self,
        id: &str,
        user: &str,
        args: Value,
        raise_to: Option<Sensitivity>,
        hold: Option<&BTreeSet<String>>,
        now: DateTime<Utc>,
    ) -> Result<ApprovalItem, DecideError> {
        let mut state = self.state.lock().expect("approvals lock");
        let released = state.released.contains_key(id);
        let item = state.items.get_mut(id).ok_or(DecideError::NotFound)?;
        if item.user != user {
            return Err(DecideError::Forbidden);
        }
        if item.status == Status::Expired {
            return Err(DecideError::Expired);
        }
        if item.status != Status::Pending {
            return Err(DecideError::NotPending);
        }
        if hold.is_some_and(|offered| held_by(item, offered, released)) {
            return Err(DecideError::Held);
        }
        let drafted = drafted_after(item, &args);
        item.edited_at = drafted.as_ref().map(|_| now);
        item.drafted_args = drafted;
        let hash = args_hash(&args);
        if hash != item.args_hash {
            item.revision += 1;
        }
        item.args_hash = hash;
        item.args = args;
        item.sources_sensitivity = raised(item.sources_sensitivity, raise_to);
        Ok(item.clone())
    }

    async fn set_target_conversation(
        &self,
        id: &str,
        target: &TargetConversation,
    ) -> StoreResult<bool> {
        let mut state = self.state.lock().expect("approvals lock");
        Ok(match state.items.get_mut(id) {
            Some(item) => {
                item.target_conversation = Some(target.clone());
                true
            }
            None => false,
        })
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
            item.drafted_args = None;
            item.redacted_at = Some(now);
            expired.push(item.clone());
        }
        for item in &expired {
            if let Some(entry) = state.ledger.get_mut(&item.id) {
                entry.row.outcome = LedgerOutcome::Expired;
            }
            state.remove_notes(&item.id, now);
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
        let mut ids = Vec::new();
        for item in state.items.values_mut().filter(|i| {
            matches!(
                i.status,
                Status::Approved | Status::Rejected | Status::Failed
            ) && tools.contains(&i.tool.as_str())
                && i.redacted_at.is_none()
                && i.decided_at.is_some_and(|d| d <= before)
        }) {
            item.args = redact_args(&item.args);
            item.drafted_args = None;
            item.redacted_at = Some(now);
            ids.push(item.id.clone());
        }
        for id in &ids {
            state.remove_notes(id, now);
        }
        Ok(ids.len())
    }

    async fn close_for_owner(&self, user: &str, now: DateTime<Utc>) -> StoreResult<Vec<String>> {
        let mut state = self.state.lock().expect("approvals lock");
        let mut ids = Vec::new();
        for item in state
            .items
            .values_mut()
            .filter(|i| i.user == user && i.status == Status::Pending)
        {
            item.status = Status::Rejected;
            item.decided_at = Some(now);
            item.decided_by = Some(SYSTEM_DECIDER.to_string());
            ids.push(item.id.clone());
        }
        for id in &ids {
            if let Some(entry) = state.ledger.get_mut(id) {
                entry.row.outcome = LedgerOutcome::Rejected;
                entry.row.reason = Some(crate::ledger::Reason::OwnerDeactivated);
                entry.row.decided_at = Some(now);
                entry.row.decided_by = Some(SYSTEM_DECIDER.to_string());
            }
        }
        Ok(ids)
    }

    async fn persona_counts(
        &self,
        user: Option<&str>,
        since: DateTime<Utc>,
    ) -> StoreResult<BTreeMap<String, u64>> {
        let state = self.state.lock().expect("approvals lock");
        let mut out = BTreeMap::new();
        for item in state.items.values() {
            if item.created_at < since || user.is_some_and(|u| u != item.user) {
                continue;
            }
            let key = item
                .persona
                .clone()
                .unwrap_or_else(|| pmos_contract::personas::ALWAYS_ON.to_string());
            *out.entry(key).or_insert(0) += 1;
        }
        Ok(out)
    }

    async fn release_hold(&self, id: &str, by: &str, now: DateTime<Utc>) -> StoreResult<bool> {
        let mut state = self.state.lock().expect("approvals lock");
        if !state.items.contains_key(id) || state.released.contains_key(id) {
            return Ok(false);
        }
        state.released.insert(id.to_string(), (by.to_string(), now));
        Ok(true)
    }

    async fn hold_released(&self, id: &str) -> StoreResult<bool> {
        let state = self.state.lock().expect("approvals lock");
        Ok(state.released.contains_key(id))
    }

    async fn unreleased_pending_outside(
        &self,
        offered: &[String],
        cap: usize,
    ) -> StoreResult<(Vec<ApprovalItem>, usize)> {
        let state = self.state.lock().expect("approvals lock");
        let mut found: Vec<_> = state
            .items
            .values()
            .filter(|i| {
                i.status == Status::Pending
                    && i.persona.as_ref().is_some_and(|p| !offered.contains(p))
                    && !state.released.contains_key(&i.id)
            })
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let total = found.len();
        found.truncate(cap);
        Ok((found, total))
    }

    async fn pending_of(
        &self,
        users: &[String],
        cap: usize,
    ) -> StoreResult<(Vec<ApprovalItem>, usize)> {
        let state = self.state.lock().expect("approvals lock");
        let mut pending: Vec<_> = state
            .items
            .values()
            .filter(|i| i.status == Status::Pending && users.contains(&i.user))
            .cloned()
            .collect();
        pending.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let total = pending.len();
        pending.truncate(cap);
        Ok((pending, total))
    }

    async fn recommend(
        &self,
        new: NewRecommendation<'_>,
        now: DateTime<Utc>,
    ) -> Result<Recommendation, RecommendError> {
        let mut state = self.state.lock().expect("approvals lock");
        let item = state
            .items
            .get(new.approval_id)
            .ok_or(RecommendError::NotFound)?;
        recommend_refusal(item, &new)?;
        for old in state.recommendations.iter_mut().filter(|r| {
            r.approval_id == new.approval_id
                && r.reviewer == new.reviewer
                && r.withdrawn_at.is_none()
        }) {
            old.withdrawn_at = Some(now);
            old.note = None;
        }
        let stored = new.into_stored(now);
        state.recommendations.push(stored.clone());
        Ok(stored)
    }

    async fn recommendations(&self, approval_ids: &[String]) -> StoreResult<Vec<Recommendation>> {
        let state = self.state.lock().expect("approvals lock");
        let mut live: Vec<_> = state
            .recommendations
            .iter()
            .filter(|r| r.withdrawn_at.is_none() && approval_ids.contains(&r.approval_id))
            .cloned()
            .collect();
        live.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(live)
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
