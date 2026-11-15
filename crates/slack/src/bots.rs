//! The bots in Slack (spec §5): a PM's DMs become tasks for a bot, approval cards are decided
//! with a tap, and routines are set up and changed in plain language.
//!
//! **Which bot a DM goes to:**
//! 1. a `<bot>:` prefix (`docs: what ports does Acme Vault need?`);
//! 2. else the bot already working in that thread;
//! 3. else the PM's personal bot `pm-<handle>`, if it exists;
//! 4. else `docs`.
//!
//! **Commands first:** typed alone in a DM, `setup` (or `status`, `onboarding`), and for
//! platform owners `requests` and `remove <handle>`, are handled before any of that
//! (onboarding, in `members.rs`), and so are `reply assistant on`, `off` and status (the reply
//! assistant, in `reply.rs`). `docs: setup` still goes to the `docs` bot. Someone not in
//! `team.yaml` gets a note with **Request access**.
//!
//! A `slack.post_reply` draft gets the reply card (`docs/slack-reply-spec.md` §5): **Post as
//! you**, **Edit**, **Snooze** and **Reject**, and the audience check's refusals each in
//! their own words.
//!
//! Every envelope is acknowledged before any of this runs (Slack wants the ack within
//! 3 seconds); the work happens in follow-ups. The one exception is the memory-repo modal,
//! whose answer must be in the ack to show as a field error: [`Bots::late_ack`] waits for the
//! tool server, never longer than the ack allows. The PM is always the Slack user mapped
//! through `team.yaml`: the runtime and the tool server apply that PM's access, never the
//! bot's.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pmos_contract::model::TeamFile;
use pmos_contract::onboarding::personal_bot_name;
use serde_json::{json, Value};

use crate::blocks::escape;
use crate::bounded::Bounded;
use crate::cards::{self, action};
use crate::commands::{load_team, FollowUp, NOT_A_MEMBER};
use crate::member_cards::{self, action as member_action};
use crate::members::{self, DmCommand, Onboarding};
use crate::reply::{self, SnoozeStore};
use crate::runtime::{display_name, BotInfo, Runtime, RuntimeError};
use crate::slackapi::{Posted, SlackApi};
use crate::toolserver::{ApiError, ToolServer};

/// The bot a DM goes to when nothing else picks one.
pub const DEFAULT_BOT: &str = "docs";

const UNAVAILABLE: &str = "PM OS couldn't reach its bots right now. Try again in a minute.";
/// How long the bot roster from `GET /v1/bots` is reused.
const ROSTER_TTL: Duration = Duration::from_secs(60);

pub struct BotsConfig {
    pub slack: SlackApi,
    pub runtime: Runtime,
    pub toolserver: ToolServer,
    /// `content/team.yaml`, read on each message so a merged change applies at once.
    pub team_file: PathBuf,
    /// The time zone for routines when Slack has none for the PM (`PMOS_DEFAULT_TZ`).
    pub default_tz: String,
    /// The GitHub org memory repos live in (`PMOS_GITHUB_ORG`), to prefill the memory-repo
    /// modal. Without it the PM types the org.
    pub github_org: Option<String>,
    /// Where an interaction's `response_url` may point. Slack's is always
    /// `https://hooks.slack.com/`; tests set a local mock.
    pub response_url_prefix: String,
    /// `PMOS_SLACK_STATE_DIR`: the reply cards' snoozes are kept here (`snoozes.json`).
    pub state_dir: PathBuf,
}

/// An acknowledgement that has to wait for an answer, and the work after it. It resolves
/// within the time Slack allows for the ack.
pub type LateAck = Pin<Box<dyn Future<Output = (Option<Value>, Option<FollowUp>)> + Send>>;

/// A thread: `(channel, thread_ts)`.
type ThreadKey = (String, String);

/// A routine request the runtime asked a question about; the PM's next message in the
/// thread answers it.
struct Question {
    bot: String,
    owner: String,
    text: String,
}

/// A parsed routine waiting for **Create** or **Cancel**.
struct Draft {
    owner: String,
    draft: Value,
    summary: String,
}

struct Inner {
    cfg: BotsConfig,
    /// Which bot works in which thread.
    threads: Mutex<Bounded<ThreadKey, String>>,
    /// `event_id`s already handled: Slack retries an event it thinks was missed.
    seen: Mutex<Bounded<String, ()>>,
    questions: Mutex<Bounded<ThreadKey, Question>>,
    drafts: Mutex<Bounded<String, Draft>>,
    roster: Mutex<Option<(Instant, Vec<BotInfo>)>>,
    onboarding: Onboarding,
    /// Snoozed reply cards, kept in the state directory.
    snoozes: SnoozeStore,
    /// Edited arguments a `needs_external_confirm` answer held back, by approval id, for
    /// **Post to external channel**.
    held_edits: Mutex<Bounded<String, Value>>,
}

#[derive(Clone)]
pub struct Bots {
    inner: Arc<Inner>,
}

/// One DM from a PM.
pub(crate) struct Dm {
    pub(crate) user: String,
    pub(crate) channel: String,
    /// The thread to reply in: the message's thread, or the message itself.
    pub(crate) thread_ts: String,
    /// The message was a reply in a thread (not a new message in the DM).
    pub(crate) in_thread: bool,
    pub(crate) text: String,
}

/// One button press.
pub(crate) struct Click {
    pub(crate) user: String,
    pub(crate) channel: String,
    pub(crate) message_ts: String,
    pub(crate) thread_ts: Option<String>,
    pub(crate) trigger_id: String,
    pub(crate) value: String,
    /// Where to answer a click on an ephemeral message, which `chat.update` can't change.
    pub(crate) response_url: Option<String>,
}

impl Click {
    fn from_payload(payload: &Value) -> Option<Self> {
        let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(String::from);
        Some(Self {
            user: s(&payload["user"]["id"])?,
            channel: s(&payload["container"]["channel_id"])
                .or_else(|| s(&payload["channel"]["id"]))?,
            message_ts: s(&payload["container"]["message_ts"])
                .or_else(|| s(&payload["message"]["ts"]))?,
            thread_ts: s(&payload["message"]["thread_ts"]),
            trigger_id: s(&payload["trigger_id"]).unwrap_or_default(),
            value: s(&payload["actions"][0]["value"]).unwrap_or_default(),
            response_url: s(&payload["response_url"]),
        })
    }
}

/// Which way an approval card turns without deciding anything.
#[derive(Clone, Copy)]
enum Flip {
    /// **Reject**: the six reasons and **Back**.
    Reasons,
    /// **Back**: the card's own buttons again.
    Back,
}

#[derive(Clone, Copy)]
enum Change {
    Pause,
    Resume,
    Delete,
}

impl Bots {
    pub fn new(cfg: BotsConfig) -> Self {
        let onboarding = Onboarding::new(cfg.github_org.clone(), cfg.response_url_prefix.clone());
        let snoozes = SnoozeStore::open(&cfg.state_dir);
        Self {
            inner: Arc::new(Inner {
                cfg,
                snoozes,
                held_edits: Mutex::new(Bounded::new(500)),
                onboarding,
                threads: Mutex::new(Bounded::new(5000)),
                seen: Mutex::new(Bounded::new(5000)),
                questions: Mutex::new(Bounded::new(500)),
                drafts: Mutex::new(Bounded::new(500)),
                roster: Mutex::new(None),
            }),
        }
    }

    pub fn slack(&self) -> &SlackApi {
        &self.inner.cfg.slack
    }

    pub fn runtime(&self) -> &Runtime {
        &self.inner.cfg.runtime
    }

    pub fn toolserver(&self) -> &ToolServer {
        &self.inner.cfg.toolserver
    }

    pub fn team(&self) -> TeamFile {
        load_team(&self.inner.cfg.team_file)
    }

    pub(crate) fn onboarding(&self) -> &Onboarding {
        &self.inner.onboarding
    }

    /// The snoozed reply cards.
    pub fn snoozes(&self) -> &SnoozeStore {
        &self.inner.snoozes
    }

    /// `PMOS_DEFAULT_TZ`.
    pub(crate) fn default_tz(&self) -> &str {
        &self.inner.cfg.default_tz
    }

    /// The edit a `needs_external_confirm` answer held back for this item, if any.
    pub(crate) fn pending_edit(&self, id: &str) -> Option<Value> {
        self.inner
            .held_edits
            .lock()
            .expect("held edits lock")
            .get(&id.to_string())
            .cloned()
    }

    /// Remember that `bot` works in this thread, so the PM's next message there goes to it.
    pub fn remember_thread(&self, channel: &str, thread_ts: &str, bot: &str) {
        self.inner.threads.lock().expect("threads lock").insert(
            (channel.to_string(), thread_ts.to_string()),
            bot.to_string(),
        );
    }

    pub fn thread_bot(&self, channel: &str, thread_ts: &str) -> Option<String> {
        self.inner
            .threads
            .lock()
            .expect("threads lock")
            .get(&(channel.to_string(), thread_ts.to_string()))
            .cloned()
    }

    /// The bots (`GET /v1/bots`), reused for a minute; a stale copy if the runtime is down.
    pub async fn roster(&self) -> Result<Vec<BotInfo>, RuntimeError> {
        if let Some((at, bots)) = &*self.inner.roster.lock().expect("roster lock") {
            if at.elapsed() < ROSTER_TTL {
                return Ok(bots.clone());
            }
        }
        match self.runtime().bots().await {
            Ok(bots) => {
                *self.inner.roster.lock().expect("roster lock") =
                    Some((Instant::now(), bots.clone()));
                Ok(bots)
            }
            Err(e) => match &*self.inner.roster.lock().expect("roster lock") {
                Some((_, bots)) => Ok(bots.clone()),
                None => Err(e),
            },
        }
    }

    /// The bots straight from the runtime (and kept for the next minute), for the setup
    /// checklist, which has to see a new member's bot as soon as it is listed.
    pub async fn fresh_roster(&self) -> Result<Vec<BotInfo>, RuntimeError> {
        let bots = self.runtime().bots().await?;
        *self.inner.roster.lock().expect("roster lock") = Some((Instant::now(), bots.clone()));
        Ok(bots)
    }

    /// The name `bot` posts under.
    pub async fn display_name(&self, bot: &str) -> String {
        let roster = self.roster().await.unwrap_or_default();
        named(&roster, bot)
    }

    /// Handle an `events_api` payload. Only a PM's own DMs (`message.im`) do anything; the
    /// bot's own messages, edits and other subtypes, and retries of an event already handled,
    /// are ignored.
    pub fn event(&self, payload: &Value) -> Option<FollowUp> {
        let event = &payload["event"];
        if event["type"] != "message" || event["channel_type"] != "im" {
            return None;
        }
        if event.get("subtype").is_some() || event.get("bot_id").is_some() {
            return None;
        }
        let user = event["user"].as_str()?.to_string();
        let channel = event["channel"].as_str()?.to_string();
        let ts = event["ts"].as_str()?.to_string();
        let event_id = payload["event_id"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("{channel}:{ts}"));
        if !self
            .inner
            .seen
            .lock()
            .expect("seen lock")
            .first_time(event_id.clone())
        {
            tracing::info!(event_id, "event already handled; ignoring the retry");
            return None;
        }
        let thread_ts = event["thread_ts"].as_str().map(String::from);
        let dm = Dm {
            user,
            channel,
            in_thread: thread_ts.is_some(),
            thread_ts: thread_ts.unwrap_or(ts),
            text: plain_text(event["text"].as_str().unwrap_or_default()),
        };
        let this = self.clone();
        Some(Box::pin(async move { this.on_dm(dm).await }))
    }

    /// Handle an `interactive` payload. Returns the acknowledgement's payload, if any, and the
    /// follow-up work.
    pub fn interactive(&self, payload: &Value) -> (Option<Value>, Option<FollowUp>) {
        let this = self.clone();
        match payload["type"].as_str() {
            Some("block_actions") => {
                let Some(click) = Click::from_payload(payload) else {
                    return (None, None);
                };
                let work: FollowUp = match payload["actions"][0]["action_id"]
                    .as_str()
                    .unwrap_or_default()
                {
                    action::APPROVE => {
                        Box::pin(async move { this.decide(click, "approve", None).await })
                    }
                    action::REJECT => {
                        Box::pin(async move { this.flip_card(click, Flip::Reasons).await })
                    }
                    action::BACK => {
                        Box::pin(async move { this.flip_card(click, Flip::Back).await })
                    }
                    a if cards::reason_of_action(a).is_some() => {
                        let reason = cards::reason_of_action(a);
                        Box::pin(async move { this.decide(click, "reject", reason).await })
                    }
                    action::EDIT => Box::pin(async move { this.open_editor(click).await }),
                    action::SNOOZE => Box::pin(async move { this.snooze_menu(click).await }),
                    a if reply::Choice::of_action(a).is_some() => {
                        let choice = reply::Choice::of_action(a).expect("checked");
                        Box::pin(async move { this.snooze_pick(click, choice).await })
                    }
                    action::POST_EXTERNAL => {
                        Box::pin(async move { this.post_external(click).await })
                    }
                    action::ROUTINE_CREATE => {
                        Box::pin(async move { this.confirm_routine(click, true).await })
                    }
                    action::ROUTINE_CANCEL => {
                        Box::pin(async move { this.confirm_routine(click, false).await })
                    }
                    action::ROUTINE_PAUSE => {
                        Box::pin(async move { this.change_routine(click, Change::Pause).await })
                    }
                    action::ROUTINE_RESUME => {
                        Box::pin(async move { this.change_routine(click, Change::Resume).await })
                    }
                    action::ROUTINE_DELETE => {
                        Box::pin(async move { this.change_routine(click, Change::Delete).await })
                    }
                    member_action::REQUEST_ACCESS => {
                        Box::pin(async move { this.request_access(click).await })
                    }
                    member_action::APPROVE => {
                        Box::pin(async move { this.decide_member(click, "approve", None).await })
                    }
                    member_action::REJECT => {
                        Box::pin(async move { this.decide_member(click, "reject", None).await })
                    }
                    member_action::EDIT => {
                        Box::pin(async move { this.open_handle_modal(click).await })
                    }
                    member_action::REMOVE_CONFIRM => {
                        Box::pin(async move { this.remove_member(click, true).await })
                    }
                    member_action::REMOVE_CANCEL => {
                        Box::pin(async move { this.remove_member(click, false).await })
                    }
                    member_action::SETUP_REFRESH => {
                        Box::pin(async move { this.refresh_setup(click).await })
                    }
                    member_action::SETUP_MEMORY => {
                        Box::pin(async move { this.open_memory_modal(click).await })
                    }
                    _ => return (None, None),
                };
                (None, Some(work))
            }
            Some("view_submission") if payload["view"]["callback_id"] == action::EDIT_MODAL => {
                let values = payload["view"]["state"]["values"].clone();
                let errors = cards::empty_fields(&values);
                if !errors.is_empty() {
                    // Keeps the modal open, with the message under each empty field.
                    return (
                        Some(json!({"response_action": "errors", "errors": errors})),
                        None,
                    );
                }
                let meta: Value = payload["view"]["private_metadata"]
                    .as_str()
                    .and_then(|m| serde_json::from_str(m).ok())
                    .unwrap_or(Value::Null);
                let user = payload["user"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                (
                    None,
                    Some(Box::pin(async move {
                        this.submit_edit(user, meta, values).await
                    })),
                )
            }
            Some("view_submission")
                if payload["view"]["callback_id"] == member_action::HANDLE_MODAL =>
            {
                self.submit_handle(payload)
            }
            _ => (None, None),
        }
    }

    /// An interaction whose acknowledgement has to wait for an answer: the memory-repo
    /// modal, so the tool server's refusal shows as a field error. `None` for everything
    /// else, which [`Bots::interactive`] acknowledges at once.
    pub fn late_ack(&self, payload: &Value) -> Option<LateAck> {
        if payload["type"] == "view_submission"
            && payload["view"]["callback_id"] == member_action::MEMORY_MODAL
        {
            let this = self.clone();
            let payload = payload.clone();
            return Some(Box::pin(
                async move { this.submit_memory_repo(payload).await },
            ));
        }
        None
    }

    // ---- DMs ----

    async fn on_dm(&self, dm: Dm) {
        let team = self.team();
        let Some(member) = team.by_slack_id(&dm.user) else {
            tracing::info!(slack_id = %dm.user, "DM from someone not in team.yaml");
            let (text, blocks) = member_cards::request_access_note();
            if let Err(e) = self
                .slack()
                .ephemeral_blocks(&dm.channel, &dm.user, None, &text, blocks)
                .await
            {
                tracing::warn!(error = %e, "private note failed");
            }
            return;
        };
        let handle = member.handle.clone();
        let text = dm.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        match members::dm_command(&text, member.is_admin(), &team) {
            Some(DmCommand::Setup) => return self.post_setup(&dm, &handle).await,
            Some(DmCommand::Requests) => return self.list_requests(&dm).await,
            Some(DmCommand::Remove(who)) => {
                return self.confirm_remove(&dm, &handle, &who).await;
            }
            None => {}
        }
        if let Some(cmd) = reply::command(&text) {
            return self.reply_assistant(&dm, &handle, cmd).await;
        }
        if is_routine_list(&text) {
            let message = self
                .routines_message(&handle)
                .await
                .unwrap_or_else(|e| cards::note(&escape(&unavailable_or(&e))));
            self.post(&dm.channel, Some(&dm.thread_ts), None, message)
                .await;
            return;
        }
        let roster = match self.roster().await {
            Ok(roster) => roster,
            Err(_) => {
                self.post(
                    &dm.channel,
                    Some(&dm.thread_ts),
                    None,
                    cards::note(UNAVAILABLE),
                )
                .await;
                return;
            }
        };
        let (prefix, rest) = split_prefix(&text, &roster);
        let key = (dm.channel.clone(), dm.thread_ts.clone());
        if prefix.is_none() {
            let pending = self
                .inner
                .questions
                .lock()
                .expect("questions lock")
                .remove(&key);
            if let Some(q) = pending.filter(|q| q.owner == handle) {
                let combined = format!("{}\n{rest}", q.text);
                self.start_routine(&dm, &handle, &q.bot, &combined).await;
                return;
            }
        }
        let thread_bot = self.thread_bot(&dm.channel, &dm.thread_ts);
        let bot = choose_bot(prefix.as_deref(), thread_bot.as_deref(), &handle, &roster);
        if rest.is_empty() {
            let hint = format!(
                "Ask {} something after `{}:`.",
                escape(&named(&roster, &bot)),
                escape(&bot)
            );
            self.post(&dm.channel, Some(&dm.thread_ts), None, cards::note(&hint))
                .await;
            return;
        }
        if let Some(routine) = routine_request(rest) {
            self.start_routine(&dm, &handle, &bot, &routine).await;
            return;
        }
        self.start_task(&dm, &handle, &bot, &named(&roster, &bot), rest)
            .await;
    }

    async fn start_task(&self, dm: &Dm, handle: &str, bot: &str, shown: &str, text: &str) {
        // Remembered first, so a second message sent quickly in the thread goes to the same
        // bot; put back if the task isn't created.
        let previous = self.thread_bot(&dm.channel, &dm.thread_ts);
        self.remember_thread(&dm.channel, &dm.thread_ts, bot);
        let created = self
            .runtime()
            .create_task(handle, bot, text, &dm.channel, &dm.thread_ts)
            .await;
        if created.is_err() {
            let key = (dm.channel.clone(), dm.thread_ts.clone());
            let mut threads = self.inner.threads.lock().expect("threads lock");
            match previous {
                Some(previous) => threads.insert(key, previous),
                None => {
                    threads.remove(&key);
                }
            }
        }
        match created {
            Ok(task) => {
                tracing::info!(task = %task["id"], bot, user = handle, "task created from a DM");
            }
            Err(RuntimeError::Refused(why)) => {
                let message = format!("{} can't take that: {}", escape(shown), escape(&why));
                self.post(
                    &dm.channel,
                    Some(&dm.thread_ts),
                    None,
                    cards::note(&message),
                )
                .await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "creating a task failed");
                self.post(
                    &dm.channel,
                    Some(&dm.thread_ts),
                    None,
                    cards::note(UNAVAILABLE),
                )
                .await;
            }
        }
    }

    async fn start_routine(&self, dm: &Dm, handle: &str, bot: &str, text: &str) {
        let tz = match self.slack().user_tz(&dm.user).await {
            Some(tz) => tz,
            None => self.inner.cfg.default_tz.clone(),
        };
        let shown = self.display_name(bot).await;
        let thread = Some(dm.thread_ts.as_str());
        match self.runtime().parse_routine(handle, bot, text, &tz).await {
            Ok(parsed) if parsed["draft"].is_object() => {
                let summary = parsed["summary"]
                    .as_str()
                    .unwrap_or("Create this routine?")
                    .to_string();
                let key = uuid::Uuid::new_v4().simple().to_string();
                self.inner.drafts.lock().expect("drafts lock").insert(
                    key.clone(),
                    Draft {
                        owner: handle.to_string(),
                        draft: parsed["draft"].clone(),
                        summary: summary.clone(),
                    },
                );
                self.remember_thread(&dm.channel, &dm.thread_ts, bot);
                self.post(
                    &dm.channel,
                    thread,
                    Some(&shown),
                    cards::routine_readback(&summary, &key),
                )
                .await;
            }
            Ok(_) => {
                tracing::warn!("routine parse returned no draft");
                self.post(&dm.channel, thread, None, cards::note(UNAVAILABLE))
                    .await;
            }
            Err(RuntimeError::Question(question)) => {
                self.inner.questions.lock().expect("questions lock").insert(
                    (dm.channel.clone(), dm.thread_ts.clone()),
                    Question {
                        bot: bot.to_string(),
                        owner: handle.to_string(),
                        text: text.to_string(),
                    },
                );
                self.remember_thread(&dm.channel, &dm.thread_ts, bot);
                self.post(
                    &dm.channel,
                    thread,
                    Some(&shown),
                    cards::note(&escape(&question)),
                )
                .await;
            }
            Err(RuntimeError::Refused(why)) => {
                let message = format!("I couldn't set that routine up: {}", escape(&why));
                self.post(&dm.channel, thread, Some(&shown), cards::note(&message))
                    .await;
            }
            Err(RuntimeError::Unavailable) => {
                self.post(&dm.channel, thread, None, cards::note(UNAVAILABLE))
                    .await;
            }
        }
    }

    async fn routines_message(&self, handle: &str) -> Result<(String, Value), RuntimeError> {
        let routines = self.runtime().routines(handle).await?;
        let roster = self.roster().await.unwrap_or_default();
        Ok(cards::routine_list(&routines, |b| named(&roster, b)))
    }

    // ---- Buttons ----

    /// The clicking PM's handle; anyone outside `team.yaml` gets the private note.
    pub(crate) async fn clicker(&self, click: &Click) -> Option<String> {
        let handle = self
            .team()
            .by_slack_id(&click.user)
            .map(|m| m.handle.clone());
        if handle.is_none() {
            self.private(
                &click.channel,
                &click.user,
                click.thread_ts.as_deref(),
                NOT_A_MEMBER,
            )
            .await;
        }
        handle
    }

    /// Only the item's own PM may decide it. Anyone else gets a private note, and nothing
    /// is sent to the tool server.
    pub(crate) async fn owns(&self, channel: &str, user: &str, handle: &str, item: &Value) -> bool {
        let owner = item["user"].as_str().unwrap_or_default();
        if owner == handle {
            return true;
        }
        tracing::info!(
            clicker = handle,
            owner,
            "approval click by someone else; refused"
        );
        let note = format!(
            "Only {} can decide this one. Nothing was changed.",
            escape(&cards::person(owner))
        );
        self.private(channel, user, None, &note).await;
        false
    }

    /// The approval item for a click, or a private note saying why not.
    pub(crate) async fn load_item(&self, channel: &str, user: &str, id: &str) -> Option<Value> {
        match self.toolserver().approval(id).await {
            Ok(item) => Some(item),
            Err(e) => {
                let note = format!(
                    "PM OS couldn't load this approval ({}). Try again in a minute.",
                    escape(&e.to_string())
                );
                self.private(channel, user, None, &note).await;
                None
            }
        }
    }

    /// **Reject** and **Back** change the card in place and decide nothing. Only the item's
    /// own PM may turn it, and a card decided meanwhile shows its outcome instead.
    async fn flip_card(&self, click: Click, flip: Flip) {
        let Some(handle) = self.clicker(&click).await else {
            return;
        };
        let Some(item) = self
            .load_item(&click.channel, &click.user, &click.value)
            .await
        else {
            return;
        };
        if !self.owns(&click.channel, &click.user, &handle, &item).await {
            return;
        }
        let card = if item["status"] != "pending" {
            let line = cards::decision_line(&item, &handle, false);
            cards::approval_closed(&item, &line)
        } else {
            match flip {
                Flip::Reasons => cards::approval_reasons(&item),
                Flip::Back => cards::approval_card(&item),
            }
        };
        self.update(&click.channel, &click.message_ts, card).await;
    }

    /// Approve, or reject with the reason chosen on the card.
    async fn decide(&self, click: Click, decision: &'static str, reason: Option<&'static str>) {
        let Some(handle) = self.clicker(&click).await else {
            return;
        };
        let Some(item) = self
            .load_item(&click.channel, &click.user, &click.value)
            .await
        else {
            return;
        };
        if !self.owns(&click.channel, &click.user, &handle, &item).await {
            return;
        }
        if item["status"] != "pending" {
            let line = cards::decision_line(&item, &handle, false);
            self.update(
                &click.channel,
                &click.message_ts,
                cards::approval_closed(&item, &line),
            )
            .await;
            return;
        }
        // Take the buttons away at once, so a second tap can't decide twice.
        let doing = if decision == "approve" && cards::is_reply(&item) {
            ":hourglass_flowing_sand: Posting…"
        } else if decision == "approve" {
            ":hourglass_flowing_sand: Approving…"
        } else {
            ":hourglass_flowing_sand: Rejecting…"
        };
        self.update(
            &click.channel,
            &click.message_ts,
            cards::approval_closed(&item, doing),
        )
        .await;
        let outcome = self
            .toolserver()
            .decide(&click.value, &handle, decision, None, reason, false)
            .await
            .map(|mut decided| {
                // The card says why, whether or not the item echoes the reason back.
                if let (Some(reason), true) = (reason, decided["reason"].is_null()) {
                    decided["reason"] = json!(reason);
                }
                decided
            });
        self.show_decision(
            &click.channel,
            &click.message_ts,
            &item,
            &handle,
            outcome,
            false,
        )
        .await;
    }

    async fn open_editor(&self, click: Click) {
        let Some(handle) = self.clicker(&click).await else {
            return;
        };
        let Some(item) = self
            .load_item(&click.channel, &click.user, &click.value)
            .await
        else {
            return;
        };
        if !self.owns(&click.channel, &click.user, &handle, &item).await {
            return;
        }
        if item["status"] != "pending" {
            let line = cards::decision_line(&item, &handle, false);
            self.update(
                &click.channel,
                &click.message_ts,
                cards::approval_closed(&item, &line),
            )
            .await;
            return;
        }
        match cards::edit_modal(&item, &click.channel, &click.message_ts) {
            Some(view) => {
                if let Err(e) = self.slack().open_view(&click.trigger_id, view).await {
                    tracing::warn!(error = %e, "views.open failed");
                    self.private(
                        &click.channel,
                        &click.user,
                        None,
                        "Slack couldn't open the editor. Tap *Edit* again.",
                    )
                    .await;
                }
            }
            None => {
                self.private(
                    &click.channel,
                    &click.user,
                    None,
                    "Nothing in this one can be edited in Slack; approve or reject it.",
                )
                .await;
            }
        }
    }

    async fn submit_edit(&self, user: String, meta: Value, values: Value) {
        let (Some(id), Some(channel), Some(ts)) = (
            meta["id"].as_str(),
            meta["channel"].as_str(),
            meta["ts"].as_str(),
        ) else {
            tracing::warn!("edit submitted without its card's details; ignored");
            return;
        };
        let Some(handle) = self.team().by_slack_id(&user).map(|m| m.handle.clone()) else {
            self.private(channel, &user, None, NOT_A_MEMBER).await;
            return;
        };
        let Some(item) = self.load_item(channel, &user, id).await else {
            return;
        };
        if !self.owns(channel, &user, &handle, &item).await {
            return;
        }
        if item["status"] != "pending" {
            let line = cards::decision_line(&item, &handle, false);
            self.update(channel, ts, cards::approval_closed(&item, &line))
                .await;
            return;
        }
        let args = cards::edited_args(&item["args"], &values);
        let mut shown = item.clone();
        shown["args"] = args.clone();
        self.update(
            channel,
            ts,
            cards::approval_closed(&shown, ":hourglass_flowing_sand: Saving your edit…"),
        )
        .await;
        let outcome = self
            .toolserver()
            .decide(id, &handle, "edit", Some(args), None, false)
            .await;
        self.show_decision(channel, ts, &shown, &handle, outcome, true)
            .await;
    }

    /// Put a decision's outcome on the card: who decided and the link that was written, or
    /// the buttons back with what went wrong. The Slack audience check's answers
    /// (`docs/slack-reply-spec.md` §3) each get their own card.
    pub(crate) async fn show_decision(
        &self,
        channel: &str,
        ts: &str,
        item: &Value,
        handle: &str,
        outcome: Result<Value, ApiError>,
        edited: bool,
    ) {
        let card = match outcome {
            Ok(decided) => {
                let line = cards::decision_line(&decided, handle, edited);
                cards::approval_closed(&decided, &line)
            }
            Err(ApiError::Unavailable) => cards::approval_retry(
                item,
                "PM OS couldn't reach the tool server, so nothing was decided. Try again.",
            ),
            Err(ApiError::Refused { code, body, .. }) if code == "audience_blocked" => {
                tracing::info!(approval = %item["id"], "reply blocked by the audience check");
                cards::reply_blocked(&with_audience(item, &body), body["outsiders"].as_u64())
            }
            Err(ApiError::Refused { code, body, .. }) if code == "needs_external_confirm" => {
                if edited {
                    let id = item["id"].as_str().unwrap_or_default().to_string();
                    self.inner
                        .held_edits
                        .lock()
                        .expect("held edits lock")
                        .insert(id, item["args"].clone());
                }
                cards::reply_confirm_external(&with_audience(item, &body), edited)
            }
            Err(ApiError::Refused { code, .. }) if code == "expired" => {
                let mut shown = item.clone();
                shown["status"] = json!("expired");
                cards::approval_closed(&shown, cards::EXPIRED_LINE)
            }
            Err(ApiError::Refused { code, .. }) if code == "audience_unavailable" => {
                cards::approval_retry(
                    item,
                    "PM OS couldn't check who is in the channel (Slack didn't answer), so \
                     nothing was posted. Try again in a minute.",
                )
            }
            Err(ApiError::Refused { error, .. }) => {
                let why = if error.is_empty() {
                    "refused".to_string()
                } else {
                    error
                };
                let id = item["id"].as_str().unwrap_or_default();
                match self.toolserver().approval(id).await {
                    Ok(now) if now["status"] != "pending" => {
                        let line = format!(
                            "{}\n_{}_",
                            cards::decision_line(&now, handle, false),
                            escape(&why)
                        );
                        cards::approval_closed(&now, &line)
                    }
                    _ => cards::approval_retry(item, &format!("That didn't go through: {why}")),
                }
            }
        };
        self.update(channel, ts, card).await;
    }

    async fn confirm_routine(&self, click: Click, create: bool) {
        let Some(handle) = self.clicker(&click).await else {
            return;
        };
        let taken = self
            .inner
            .drafts
            .lock()
            .expect("drafts lock")
            .remove(&click.value);
        let Some(draft) = taken else {
            self.update(
                &click.channel,
                &click.message_ts,
                cards::note("This draft has expired. Send the routine again."),
            )
            .await;
            return;
        };
        if draft.owner != handle {
            let note = format!(
                "Only {} can confirm this routine.",
                escape(&cards::person(&draft.owner))
            );
            self.inner
                .drafts
                .lock()
                .expect("drafts lock")
                .insert(click.value.clone(), draft);
            self.private(&click.channel, &click.user, None, &note).await;
            return;
        }
        if !create {
            self.update(
                &click.channel,
                &click.message_ts,
                cards::note(":no_entry_sign: Cancelled; nothing was scheduled."),
            )
            .await;
            return;
        }
        match self.runtime().create_routine(&handle, &draft.draft).await {
            Ok(routine) => {
                tracing::info!(routine = %routine["id"], owner = handle, "routine created");
                let line = format!(
                    ":white_check_mark: Routine created: {}\nSay “my routines” to see, pause \
                     or delete it.",
                    escape(&draft.summary)
                );
                self.update(&click.channel, &click.message_ts, cards::note(&line))
                    .await;
            }
            Err(RuntimeError::Unavailable) => {
                self.inner
                    .drafts
                    .lock()
                    .expect("drafts lock")
                    .insert(click.value.clone(), draft);
                self.private(
                    &click.channel,
                    &click.user,
                    None,
                    "PM OS couldn't reach its bots. Tap *Create* again in a minute.",
                )
                .await;
            }
            Err(RuntimeError::Refused(why) | RuntimeError::Question(why)) => {
                let line = format!("I couldn't create that routine: {}", escape(&why));
                self.update(&click.channel, &click.message_ts, cards::note(&line))
                    .await;
            }
        }
    }

    async fn change_routine(&self, click: Click, change: Change) {
        let Some(handle) = self.clicker(&click).await else {
            return;
        };
        let id = click.value.as_str();
        let done = match change {
            Change::Pause => self
                .runtime()
                .set_routine_enabled(&handle, id, false)
                .await
                .map(|_| ()),
            Change::Resume => self
                .runtime()
                .set_routine_enabled(&handle, id, true)
                .await
                .map(|_| ()),
            Change::Delete => self.runtime().delete_routine(&handle, id).await,
        };
        if let Err(e) = done {
            let note = format!("That didn't work: {}", escape(&unavailable_or(&e)));
            self.private(&click.channel, &click.user, None, &note).await;
            return;
        }
        match self.routines_message(&handle).await {
            Ok(list) => self.update(&click.channel, &click.message_ts, list).await,
            Err(e) => {
                let note = format!(
                    "Done, but the list couldn't be refreshed: {}",
                    escape(&unavailable_or(&e))
                );
                self.private(&click.channel, &click.user, None, &note).await;
            }
        }
    }

    // ---- Slack ----

    /// Post a message; a failure is logged, never retried into a duplicate.
    pub(crate) async fn post(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        username: Option<&str>,
        (text, blocks): (String, Value),
    ) -> Option<Posted> {
        let mut message = json!({"channel": channel, "text": text, "blocks": blocks});
        if let Some(ts) = thread_ts {
            message["thread_ts"] = json!(ts);
        }
        if let Some(name) = username {
            message["username"] = json!(name);
        }
        match self.slack().post_message(message).await {
            Ok(posted) => Some(posted),
            Err(e) => {
                tracing::warn!(error = %e, "posting to Slack failed");
                None
            }
        }
    }

    pub(crate) async fn update(&self, channel: &str, ts: &str, (text, blocks): (String, Value)) {
        if let Err(e) = self.slack().update(channel, ts, &text, blocks).await {
            tracing::warn!(error = %e, "updating a Slack message failed");
        }
    }

    pub(crate) async fn private(
        &self,
        channel: &str,
        user: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) {
        if let Err(e) = self.slack().ephemeral(channel, user, thread_ts, text).await {
            tracing::warn!(error = %e, "private note failed");
        }
    }
}

/// The item with what a refusal says of the audience (`audience: {members, external}`), so
/// the card can show it.
fn with_audience(item: &Value, body: &Value) -> Value {
    let mut shown = item.clone();
    if let Some(audience) = body["audience"].as_object() {
        for (k, v) in audience {
            shown["audience"][k] = v.clone();
        }
    }
    shown
}

fn unavailable_or(e: &RuntimeError) -> String {
    match e {
        RuntimeError::Unavailable => UNAVAILABLE.to_string(),
        other => other.to_string(),
    }
}

/// The display name of `bot` from the roster, or one made from its name.
fn named(roster: &[BotInfo], bot: &str) -> String {
    roster
        .iter()
        .find(|b| b.name == bot)
        .map(BotInfo::display_name)
        .unwrap_or_else(|| display_name(bot))
}

/// The bot for a message, in the spec's order: the prefix, the thread's bot, the PM's
/// personal bot, then `docs`.
pub fn choose_bot(
    prefix: Option<&str>,
    thread_bot: Option<&str>,
    handle: &str,
    roster: &[BotInfo],
) -> String {
    if let Some(bot) = prefix.or(thread_bot) {
        return bot.to_string();
    }
    let personal = personal_bot_name(handle);
    if roster.iter().any(|b| b.name == personal) {
        personal
    } else {
        DEFAULT_BOT.to_string()
    }
}

/// `docs: what ports…` → (`docs`, `what ports…`), when `docs` is a bot. Anything else is
/// left as it is.
pub fn split_prefix<'a>(text: &'a str, roster: &[BotInfo]) -> (Option<String>, &'a str) {
    if let Some((head, tail)) = text.split_once(':') {
        let name = head.trim().to_lowercase();
        if !name.is_empty()
            && !name.contains(char::is_whitespace)
            && roster.iter().any(|b| b.name == name)
        {
            return (Some(name), tail.trim());
        }
    }
    (None, text.trim())
}

/// Words that make "every …" / "each …" a schedule rather than a question.
const SCHEDULE_WORDS: &[&str] = &[
    "day",
    "days",
    "weekday",
    "weekdays",
    "weekend",
    "weekends",
    "week",
    "weeks",
    "month",
    "months",
    "quarter",
    "year",
    "morning",
    "mornings",
    "afternoon",
    "afternoons",
    "evening",
    "evenings",
    "night",
    "nights",
    "hour",
    "hours",
    "minute",
    "minutes",
    "other",
    "business",
    "working",
    "workday",
    "workdays",
    "first",
    "last",
    "monday",
    "mondays",
    "tuesday",
    "tuesdays",
    "wednesday",
    "wednesdays",
    "thursday",
    "thursdays",
    "friday",
    "fridays",
    "saturday",
    "saturdays",
    "sunday",
    "sundays",
    "mon",
    "tue",
    "tues",
    "wed",
    "thu",
    "thur",
    "thurs",
    "fri",
    "sat",
    "sun",
];

/// The routine text if the message asks for one: `routine: …`, or a schedule such as
/// "every weekday at 8am …" or "each Monday …".
pub fn routine_request(text: &str) -> Option<String> {
    let t = text.trim();
    if t.get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("routine:"))
    {
        let rest = t[8..].trim();
        return (!rest.is_empty()).then(|| rest.to_string());
    }
    let mut words = t.split_whitespace();
    let first = words.next()?.to_lowercase();
    if first != "every" && first != "each" {
        return None;
    }
    let second = words
        .next()?
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    let schedule = second.starts_with(|c: char| c.is_ascii_digit())
        || SCHEDULE_WORDS.contains(&second.as_str());
    schedule.then(|| t.to_string())
}

/// "my routines" and close variants.
pub fn is_routine_list(text: &str) -> bool {
    let t = text
        .trim()
        .trim_end_matches(['?', '.', '!'])
        .trim()
        .to_lowercase();
    matches!(
        t.as_str(),
        "my routines"
            | "routines"
            | "list routines"
            | "list my routines"
            | "show routines"
            | "show my routines"
    )
}

/// A Slack message's text as the PM typed it: entities decoded, `<https://…|label>` as
/// "label (https://…)", `<#C…|name>` as "#name". User mentions stay as `<@U…>`.
pub fn plain_text(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let inner = &rest[open + 1..open + close];
        let (target, label) = match inner.split_once('|') {
            Some((t, l)) => (t, Some(l)),
            None => (inner, None),
        };
        let shown = if let Some(channel) = target.strip_prefix('#') {
            format!("#{}", label.unwrap_or(channel))
        } else if target.starts_with('@') || target.starts_with('!') {
            format!("<{target}>")
        } else {
            match label {
                Some(l) if l != target => format!("{l} ({target})"),
                _ => target.to_string(),
            }
        };
        out.push_str(&shown);
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bot(name: &str) -> BotInfo {
        BotInfo {
            name: name.into(),
            role: String::new(),
            scope: String::new(),
            owner: None,
            description: String::new(),
            display_name: None,
        }
    }

    #[test]
    fn routing_order() {
        let roster = vec![bot("docs"), bot("pm-ana")];
        // 1. the prefix wins over everything.
        let (prefix, rest) = split_prefix("docs: what ports does Acme Vault need?", &roster);
        assert_eq!(prefix.as_deref(), Some("docs"));
        assert_eq!(rest, "what ports does Acme Vault need?");
        assert_eq!(
            choose_bot(prefix.as_deref(), Some("pm-ana"), "ana", &roster),
            "docs"
        );
        // 2. then the thread's bot.
        assert_eq!(choose_bot(None, Some("docs"), "ana", &roster), "docs");
        // 3. then the PM's personal bot.
        assert_eq!(choose_bot(None, None, "ana", &roster), "pm-ana");
        // 4. then docs.
        assert_eq!(choose_bot(None, None, "bob", &roster), "docs");
        // An unknown prefix is part of the question.
        assert_eq!(
            split_prefix("Note: the ports", &roster),
            (None, "Note: the ports")
        );
        assert_eq!(split_prefix("DOCS:x", &roster).0.as_deref(), Some("docs"));
    }

    #[test]
    fn routines_are_recognized() {
        assert_eq!(
            routine_request("every weekday at 8am summarize #launch").as_deref(),
            Some("every weekday at 8am summarize #launch")
        );
        assert!(routine_request("Each Monday, list open bugs").is_some());
        assert!(routine_request("every 2 hours check the queue").is_some());
        assert_eq!(
            routine_request("routine: friday digest").as_deref(),
            Some("friday digest")
        );
        assert!(routine_request("every ticket in Acme Vault, what's the status?").is_none());
        assert!(routine_request("each of these").is_none());
        assert!(routine_request("routine:").is_none());
        assert!(is_routine_list("My routines?"));
        assert!(!is_routine_list("my routines are broken, help"));
    }

    #[test]
    fn slack_text_as_typed() {
        assert_eq!(
            plain_text("a &lt;b&gt; &amp; <https://x.com|site> <#C1|launch> <@U1> <https://y.com>"),
            "a <b> & site (https://x.com) #launch <@U1> https://y.com"
        );
        assert_eq!(plain_text("x < y"), "x < y");
    }
}
