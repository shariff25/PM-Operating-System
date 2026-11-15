//! The Slack reply assistant, Slack's half (`docs/slack-reply-spec.md` §1 and §5).
//!
//! **The commands** (typed alone in a DM): `reply assistant on` creates the PM's
//! `[reply-assistant]` routine on their personal bot with `POST /v1/routines {draft}` (the
//! model isn't involved), or says it's already on; `reply assistant off` deletes it;
//! `reply assistant` says whether it's on and when it next runs. The routine is found by its
//! prompt's `[reply-assistant]` prefix. A member with no personal bot is told so, and nothing
//! changes. Its schedule is in the PM's Slack time zone, else `PMOS_DEFAULT_TZ`.
//!
//! **Snooze** on a reply card offers 1 hour, 4 hours, or tomorrow at 09:00 in the PM's time
//! zone, leaving out any that would pass the draft's expiry (created + 24 hours). The card
//! becomes "Snoozed until …", and at that time [`Snoozer`] posts it again in the PM's DM if
//! the item is still `pending`. Snoozes are kept in `<state dir>/snoozes.json` (ids and times
//! only, never message text), so they survive a restart.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Days, NaiveTime, Utc};
use chrono_tz::Tz;
use pmos_contract::onboarding::personal_bot_name;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::blocks::escape;
use crate::bots::{Bots, Click, Dm};
use crate::cards::{self, action};
use crate::runtime::RuntimeError;
use crate::toolserver::CallError;
use crate::triage::local_time;

/// How Slack finds the routine: its prompt starts with this.
pub const TAG: &str = "[reply-assistant]";
pub const PROMPT: &str = "[reply-assistant] Check Slack for messages since the last run that ask me something or need my answer, and draft a reply to each with the slack-reply skill. Skip anything I already answered.";
/// Every 30 minutes from 08:00 to 17:30, on weekdays.
pub const CRON: &str = "*/30 8-17 * * 1-5";
const SCHEDULE_WORDS: &str = "every 30 minutes from 08:00 to 17:30, Monday to Friday";

pub const SNOOZES_FILE: &str = "snoozes.json";
/// A reply draft expires this long after it was made (the tool server's default
/// `PMOS_SLACK_DRAFT_TTL_HOURS`), unless the item says when.
pub const DRAFT_TTL: chrono::Duration = chrono::Duration::hours(24);

pub(crate) const UNAVAILABLE: &str =
    "PM OS couldn't reach its bots right now. Try again in a minute.";

// ---- The commands ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    On,
    Off,
    Status,
}

/// `<words> on`, `<words> off` and `<words>` (or `… status`), typed alone, in any case.
pub fn routine_command_of(text: &str, words: &str) -> Option<Command> {
    let t = text.trim().trim_end_matches(['?', '.', '!']).to_lowercase();
    let typed: Vec<&str> = t.split_whitespace().collect();
    let name: Vec<&str> = words.split_whitespace().collect();
    let rest = typed.strip_prefix(name.as_slice())?;
    match rest {
        ["on"] => Some(Command::On),
        ["off"] => Some(Command::Off),
        [] | ["status"] => Some(Command::Status),
        _ => None,
    }
}

/// `reply assistant on`, `reply assistant off` and `reply assistant` (or `… status`), typed
/// alone, in any case.
pub fn command(text: &str) -> Option<Command> {
    routine_command_of(text, REPLY_ASSISTANT.words)
}

/// A routine a DM command turns on and off on the PM's personal bot, found again by the tag
/// its prompt starts with.
pub struct RoutineKind {
    /// The command's words: `reply assistant`.
    pub words: &'static str,
    /// How the answers name it: "the reply assistant".
    pub the: &'static str,
    pub tag: &'static str,
    pub cron: &'static str,
    pub prompt: &'static str,
    /// The schedule for a person: "every 30 minutes from 08:00 to 17:30, Monday to Friday".
    pub schedule_words: &'static str,
    /// The answer when it is turned on: (the bot's name, the time zone, the next run).
    pub on_text: fn(&str, &str, &str) -> String,
    /// The answer when it is turned off.
    pub off_text: &'static str,
    /// The status answer while it is off: (the bot's name).
    pub status_off_text: fn(&str) -> String,
}

impl RoutineKind {
    /// The routine `<words> on` creates, exactly.
    pub fn draft(&self, handle: &str, tz: &str) -> Value {
        json!({
            "bot": personal_bot_name(handle),
            "owner": handle,
            "schedule": {"cron": self.cron, "tz": tz},
            "prompt": self.prompt,
            "output": {"kind": "dm"},
            "approval": "approve",
        })
    }

    /// One of the PM's routines of this kind, by its prompt's tag.
    pub fn is_routine(&self, routine: &Value) -> bool {
        routine["prompt"]
            .as_str()
            .is_some_and(|p| p.trim_start().starts_with(self.tag))
    }
}

pub const REPLY_ASSISTANT: RoutineKind = RoutineKind {
    words: "reply assistant",
    the: "the reply assistant",
    tag: TAG,
    cron: CRON,
    prompt: PROMPT,
    schedule_words: SCHEDULE_WORDS,
    on_text: |shown, tz, next| {
        format!(
            "*The reply assistant is on.* {shown} checks Slack \
             {SCHEDULE_WORDS} ({tz} time) for messages that ask you \
             something, and drafts a reply to each here for you to post, \
             edit, snooze or reject. Nothing posts without your tap.\n\
             Next run: {next}."
        )
    },
    off_text: "*The reply assistant is off.* Drafts already waiting stay until you \
               decide them or they expire.",
    status_off_text: |shown| {
        format!(
            "*The reply assistant is off.* Say `reply assistant on` and {shown} checks \
             Slack {SCHEDULE_WORDS} for messages that need your answer, and drafts \
             replies for you to post."
        )
    },
};

/// The routine `reply assistant on` creates (spec §1), exactly.
pub fn draft(handle: &str, tz: &str) -> Value {
    REPLY_ASSISTANT.draft(handle, tz)
}

/// A PM's `[reply-assistant]` routine, by its prompt.
pub fn is_reply_routine(routine: &Value) -> bool {
    REPLY_ASSISTANT.is_routine(routine)
}

/// "the reply assistant" → "The reply assistant".
fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// `name` as a time zone, else `fallback`, else UTC.
pub fn zone(name: Option<&str>, fallback: &str) -> Tz {
    name.and_then(|n| n.parse().ok())
        .or_else(|| fallback.parse().ok())
        .unwrap_or(Tz::UTC)
}

/// A time for a person: "Mon 16 Nov 09:00 GMT".
pub fn when(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz)
        .format("%a %-d %b %H:%M %Z")
        .to_string()
}

/// When a routine next runs, in its own time zone.
fn next_run(routine: &Value, default_tz: &str) -> String {
    let tz = zone(routine["schedule"]["tz"].as_str(), default_tz);
    routine["next_run_at"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| when(t.with_timezone(&Utc), tz))
        .unwrap_or_else(|| "not scheduled yet".into())
}

// ---- Snooze ----

/// A snooze choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Hour,
    FourHours,
    Tomorrow,
}

impl Choice {
    pub const ALL: [Choice; 3] = [Choice::Hour, Choice::FourHours, Choice::Tomorrow];

    pub fn code(self) -> &'static str {
        match self {
            Choice::Hour => "1h",
            Choice::FourHours => "4h",
            Choice::Tomorrow => "tomorrow",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Choice::Hour => "1 hour",
            Choice::FourHours => "4 hours",
            Choice::Tomorrow => "Tomorrow 09:00",
        }
    }

    /// The choice a button's action id carries: `apr_snooze_4h` → 4 hours.
    pub fn of_action(action_id: &str) -> Option<Self> {
        let code = action_id.strip_prefix(action::SNOOZE_PREFIX)?;
        Self::ALL.into_iter().find(|c| c.code() == code)
    }

    /// When a snooze chosen at `now` ends. Tomorrow is the PM's tomorrow, at 09:00 there.
    pub fn until(self, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
        match self {
            Choice::Hour => Some(now + chrono::Duration::hours(1)),
            Choice::FourHours => Some(now + chrono::Duration::hours(4)),
            Choice::Tomorrow => {
                let tomorrow = now
                    .with_timezone(&tz)
                    .date_naive()
                    .checked_add_days(Days::new(1))?;
                local_time(tomorrow, NaiveTime::from_hms_opt(9, 0, 0)?, tz)
            }
        }
    }
}

/// When a draft expires: the item's `expires_at` if it has one, else `created_at` plus
/// [`DRAFT_TTL`].
pub fn expiry(item: &Value) -> Option<DateTime<Utc>> {
    let parse = |v: &Value| {
        v.as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    parse(&item["expires_at"]).or_else(|| parse(&item["created_at"]).map(|c| c + DRAFT_TTL))
}

/// The choices offered at `now`, with when each ends: those that end before the draft
/// expires.
pub fn offered(
    now: DateTime<Utc>,
    expires: Option<DateTime<Utc>>,
    tz: Tz,
) -> Vec<(Choice, DateTime<Utc>)> {
    Choice::ALL
        .into_iter()
        .filter_map(|c| c.until(now, tz).map(|t| (c, t)))
        .filter(|(_, t)| expires.is_none_or(|e| *t < e))
        .collect()
}

/// One snoozed card: which item, the PM's DM and the card's message, and when it comes back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snooze {
    pub approval_id: String,
    /// The PM's Slack id.
    pub user: String,
    pub channel: String,
    /// The card's message, now "Snoozed until …".
    pub ts: String,
    pub until: DateTime<Utc>,
}

/// The snoozes, kept in `<state dir>/snoozes.json` after every change.
pub struct SnoozeStore {
    path: PathBuf,
    entries: Mutex<Vec<Snooze>>,
}

impl SnoozeStore {
    /// The snoozes saved in `state_dir`, if any.
    pub fn open(state_dir: &Path) -> Self {
        let path = state_dir.join(SNOOZES_FILE);
        let entries = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::error!(error = %e, path = %path.display(), "the saved snoozes can't be read; starting with none");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Self {
            path,
            entries: Mutex::new(entries),
        }
    }

    pub fn all(&self) -> Vec<Snooze> {
        self.entries.lock().expect("snoozes lock").clone()
    }

    /// Add a snooze, replacing any for the same item.
    pub fn put(&self, snooze: Snooze) {
        let mut entries = self.entries.lock().expect("snoozes lock");
        entries.retain(|s| s.approval_id != snooze.approval_id);
        entries.push(snooze);
        self.save(&entries);
    }

    pub fn remove(&self, approval_id: &str) {
        let mut entries = self.entries.lock().expect("snoozes lock");
        let before = entries.len();
        entries.retain(|s| s.approval_id != approval_id);
        if entries.len() != before {
            self.save(&entries);
        }
    }

    /// The snoozes that end at or before `now`.
    pub fn due(&self, now: DateTime<Utc>) -> Vec<Snooze> {
        self.all().into_iter().filter(|s| s.until <= now).collect()
    }

    /// Write then rename, so a crash never leaves half a file.
    fn save(&self, entries: &[Snooze]) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("tmp");
        let body = serde_json::to_string_pretty(entries).unwrap_or_default();
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &self.path)) {
            tracing::error!(error = %e, "saving the snoozes failed; they won't survive a restart");
        }
    }
}

/// The clock the [`Snoozer`] reads; tests move it on.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Posts snoozed cards again when their time comes.
pub struct Snoozer {
    bots: Bots,
    /// How often it looks. Default 30 seconds.
    pub tick: Duration,
    pub clock: Clock,
}

impl Snoozer {
    pub fn new(bots: Bots) -> Self {
        Self {
            bots,
            tick: Duration::from_secs(30),
            clock: Arc::new(Utc::now),
        }
    }

    /// Look every `tick` until `shutdown`.
    pub async fn run(self, shutdown: CancellationToken) {
        loop {
            self.wake_due().await;
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(self.tick) => {}
            }
        }
    }

    /// Handle every snooze that is due. A card still `pending` (and not past its expiry) is
    /// posted again in the PM's DM and its snoozed message says so; one decided or expired
    /// meanwhile isn't, and its snoozed message shows the outcome. A tool server that
    /// doesn't answer keeps the snooze for the next look. Returns how many were posted.
    pub async fn wake_due(&self) -> usize {
        let now = (self.clock)();
        let store = self.bots.snoozes();
        let mut posted = 0;
        for s in store.due(now) {
            match self.bots.toolserver().approval(&s.approval_id).await {
                Ok(item)
                    if item["status"] == "pending" && expiry(&item).is_none_or(|e| e > now) =>
                {
                    let card = cards::approval_card(&item);
                    if self.bots.post(&s.channel, None, None, card).await.is_some() {
                        let note =
                            cards::reply_note(&item, ":zzz: Snoozed; the card is back, below.");
                        self.bots.update(&s.channel, &s.ts, note).await;
                        store.remove(&s.approval_id);
                        posted += 1;
                        tracing::info!(approval = %s.approval_id, "snoozed card posted again");
                    } else if now - s.until > DRAFT_TTL {
                        store.remove(&s.approval_id);
                    }
                }
                Ok(mut item) => {
                    let line = if item["status"] == "pending" {
                        item["status"] = json!("expired");
                        cards::EXPIRED_LINE.to_string()
                    } else {
                        cards::decision_line(&item, "", false)
                    };
                    self.bots
                        .update(&s.channel, &s.ts, cards::approval_closed(&item, &line))
                        .await;
                    store.remove(&s.approval_id);
                    tracing::info!(approval = %s.approval_id, status = %item["status"], "snoozed card closed meanwhile; not posted again");
                }
                Err(CallError::Unavailable) => {
                    tracing::warn!(approval = %s.approval_id, "tool server unavailable; the snoozed card waits for the next look");
                }
                Err(e) => {
                    tracing::warn!(approval = %s.approval_id, error = %e, "snoozed item can't be loaded; dropped");
                    store.remove(&s.approval_id);
                }
            }
        }
        posted
    }
}

// ---- In Slack ----

impl Bots {
    /// The PM's time zone from Slack, else `PMOS_DEFAULT_TZ`.
    pub(crate) async fn pm_tz(&self, slack_id: &str) -> (String, Tz) {
        let name = self
            .slack()
            .user_tz(slack_id)
            .await
            .filter(|n| n.parse::<Tz>().is_ok())
            .unwrap_or_else(|| self.default_tz().to_string());
        let tz = zone(Some(&name), "UTC");
        (name, tz)
    }

    /// `reply assistant on`, `off` and status.
    pub(crate) async fn reply_assistant(&self, dm: &Dm, handle: &str, cmd: Command) {
        self.routine_command(dm, handle, cmd, &REPLY_ASSISTANT)
            .await;
    }

    /// A routine command on the PM's personal bot (`reply assistant`, `bets review`): `on`
    /// creates the routine from `kind`'s draft (or resumes a paused one), `off` deletes it,
    /// and status says whether it is on and when it next runs. The routine is found by its
    /// prompt's tag. A member with no personal bot is told so, and nothing changes.
    pub(crate) async fn routine_command(
        &self,
        dm: &Dm,
        handle: &str,
        cmd: Command,
        kind: &RoutineKind,
    ) {
        let say = |text: String| async move {
            self.post(&dm.channel, Some(&dm.thread_ts), None, cards::note(&text))
                .await;
        };
        let the = kind.the;
        let title = capitalized(the);
        let words = kind.words;
        let bot = personal_bot_name(handle);
        let roster = match self.roster().await {
            Ok(roster) => roster,
            Err(_) => return say(UNAVAILABLE.into()).await,
        };
        let Some(info) = roster.iter().find(|b| b.name == bot) else {
            return say(format!(
                "{title} runs on your personal bot, `{}`, and you don't have one \
                 yet, so nothing was changed. Say `setup` to see where it stands.",
                escape(&bot)
            ))
            .await;
        };
        let shown = escape(&info.display_name());
        let routines = match self.runtime().routines(handle).await {
            Ok(r) => r,
            Err(e) => return say(runtime_note(&e)).await,
        };
        let found: Vec<&Value> = routines.iter().filter(|r| kind.is_routine(r)).collect();
        let default_tz = self.default_tz().to_string();
        let schedule_words = kind.schedule_words;
        let text = match cmd {
            Command::On => match found.first() {
                Some(r) if r["enabled"] == false => {
                    let id = r["id"].as_str().unwrap_or_default();
                    match self.runtime().set_routine_enabled(handle, id, true).await {
                        Ok(resumed) => format!(
                            "*{title} was paused; it's back on.* Next run: {}.",
                            escape(&next_run(&resumed, &default_tz))
                        ),
                        Err(e) => runtime_note(&e),
                    }
                }
                Some(r) => format!(
                    "*{title} is already on.* Next run: {}. Say `{words} \
                     off` to stop it.",
                    escape(&next_run(r, &default_tz))
                ),
                None => {
                    let (tz, _) = self.pm_tz(&dm.user).await;
                    match self
                        .runtime()
                        .create_routine(handle, &kind.draft(handle, &tz))
                        .await
                    {
                        Ok(created) => {
                            tracing::info!(routine = %created["id"], owner = handle, "{the} on");
                            (kind.on_text)(
                                &shown,
                                &escape(&tz),
                                &escape(&next_run(&created, &default_tz)),
                            )
                        }
                        Err(RuntimeError::Refused(why) | RuntimeError::Question(why)) => {
                            format!("I couldn't turn {the} on: {}", escape(&why))
                        }
                        Err(RuntimeError::Unavailable) => UNAVAILABLE.into(),
                    }
                }
            },
            Command::Off if found.is_empty() => {
                format!("{title} is already off. Say `{words} on` to start it.")
            }
            Command::Off => {
                let mut failed = None;
                for r in &found {
                    let id = r["id"].as_str().unwrap_or_default();
                    if let Err(e) = self.runtime().delete_routine(handle, id).await {
                        failed = Some(e);
                    }
                }
                match failed {
                    None => {
                        tracing::info!(owner = handle, "{the} off");
                        kind.off_text.into()
                    }
                    Some(e) => format!("I couldn't turn it off: {}", runtime_note(&e)),
                }
            }
            Command::Status => match found.first() {
                None => (kind.status_off_text)(&shown),
                Some(r) if r["enabled"] == false => format!(
                    "*{title} is paused.* Say `{words} on` to resume it, \
                     or `{words} off` to remove it."
                ),
                Some(r) => format!(
                    "*{title} is on:* {schedule_words}, {} time.\nNext run: {}. \
                     Say `{words} off` to stop it.",
                    escape(r["schedule"]["tz"].as_str().unwrap_or("UTC")),
                    escape(&next_run(r, &default_tz))
                ),
            },
        };
        say(text).await;
    }

    /// The item behind a reply-card click, if the clicker may act on it and it is still
    /// waiting; otherwise the card shows why and `None`.
    async fn pending_reply(&self, click: &Click, id: &str) -> Option<(String, Value)> {
        let handle = self.clicker(click).await?;
        let item = self.load_item(&click.channel, &click.user, id).await?;
        if !self.owns(&click.channel, &click.user, &handle, &item).await {
            return None;
        }
        if item["status"] != "pending" {
            let line = cards::decision_line(&item, &handle, false);
            self.update(
                &click.channel,
                &click.message_ts,
                cards::approval_closed(&item, &line),
            )
            .await;
            return None;
        }
        Some((handle, item))
    }

    /// **Snooze**: the choices that end before the draft expires, in place.
    pub(crate) async fn snooze_menu(&self, click: Click) {
        let Some((_, item)) = self.pending_reply(&click, &click.value).await else {
            return;
        };
        let (_, tz) = self.pm_tz(&click.user).await;
        let card = snooze_card(&item, Utc::now(), tz);
        self.update(&click.channel, &click.message_ts, card).await;
    }

    /// A snooze choice: kept, and the card becomes "Snoozed until …". A choice that no
    /// longer fits before the expiry shows the choices again.
    pub(crate) async fn snooze_pick(&self, click: Click, choice: Choice) {
        let Some((handle, item)) = self.pending_reply(&click, &click.value).await else {
            return;
        };
        let (_, tz) = self.pm_tz(&click.user).await;
        let now = Utc::now();
        let Some((_, until)) = offered(now, expiry(&item), tz)
            .into_iter()
            .find(|(c, _)| *c == choice)
        else {
            let card = snooze_card(&item, now, tz);
            self.update(&click.channel, &click.message_ts, card).await;
            return;
        };
        self.snoozes().put(Snooze {
            approval_id: click.value.clone(),
            user: click.user.clone(),
            channel: click.channel.clone(),
            ts: click.message_ts.clone(),
            until,
        });
        tracing::info!(approval = %click.value, user = handle, until = %until, "reply card snoozed");
        let line = format!(
            ":zzz: Snoozed until {}. The card comes back here then, if the draft is still \
             waiting.",
            escape(&when(until, tz))
        );
        self.update(
            &click.channel,
            &click.message_ts,
            cards::reply_note(&item, &line),
        )
        .await;
    }

    /// **Post to external channel**: the decision again with `confirm_external: true`. After
    /// an edit (`<id>:edit`) it is the edit again, so the edited text is what posts.
    pub(crate) async fn post_external(&self, click: Click) {
        let (id, edited) = match click.value.strip_suffix(":edit") {
            Some(id) => (id.to_string(), true),
            None => (click.value.clone(), false),
        };
        let Some((handle, item)) = self.pending_reply(&click, &id).await else {
            return;
        };
        let args = if edited {
            match self.pending_edit(&id) {
                Some(args) => Some(args),
                None => {
                    self.private(
                        &click.channel,
                        &click.user,
                        None,
                        "PM OS restarted and no longer has your edit, so nothing was posted. \
                         Tap *Edit* to make it again.",
                    )
                    .await;
                    return;
                }
            }
        } else {
            None
        };
        let mut shown = item.clone();
        if let Some(args) = &args {
            shown["args"] = args.clone();
        }
        self.update(
            &click.channel,
            &click.message_ts,
            cards::approval_closed(&shown, ":hourglass_flowing_sand: Posting…"),
        )
        .await;
        let decision = if edited { "edit" } else { "approve" };
        let outcome = self
            .toolserver()
            .decide(&id, &handle, decision, args, None, true)
            .await;
        self.show_decision(
            &click.channel,
            &click.message_ts,
            &shown,
            &handle,
            outcome,
            edited,
        )
        .await;
    }
}

/// The snooze choices for `item` at `now`.
fn snooze_card(item: &Value, now: DateTime<Utc>, tz: Tz) -> (String, Value) {
    let expires = expiry(item);
    let choices: Vec<(&str, &str)> = offered(now, expires, tz)
        .into_iter()
        .map(|(c, _)| (c.code(), c.label()))
        .collect();
    let expires = expires
        .map(|e| when(e, tz))
        .unwrap_or_else(|| "24 hours after it was drafted".into());
    cards::snooze_choices(item, &choices, &expires)
}

pub(crate) fn runtime_note(e: &RuntimeError) -> String {
    match e {
        RuntimeError::Unavailable => UNAVAILABLE.into(),
        other => escape(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn commands() {
        assert_eq!(command("reply assistant on"), Some(Command::On));
        assert_eq!(command("  Reply  Assistant OFF. "), Some(Command::Off));
        assert_eq!(command("reply assistant?"), Some(Command::Status));
        assert_eq!(command("reply assistant status"), Some(Command::Status));
        assert_eq!(command("reply assistant on please"), None);
        assert_eq!(command("docs: reply assistant on"), None);
        assert_eq!(command("reply"), None);
    }

    #[test]
    fn the_draft_is_the_specs() {
        let d = draft("ana", "Europe/London");
        assert_eq!(
            d,
            json!({"bot": "pm-ana", "owner": "ana",
             "schedule": {"cron": "*/30 8-17 * * 1-5", "tz": "Europe/London"},
             "prompt": "[reply-assistant] Check Slack for messages since the last run that ask me something or need my answer, and draft a reply to each with the slack-reply skill. Skip anything I already answered.",
             "output": {"kind": "dm"},
             "approval": "approve"})
        );
        assert!(is_reply_routine(&d));
        assert!(!is_reply_routine(&json!({"prompt": "Summarize #launch"})));
    }

    #[test]
    fn snooze_choices_stop_at_the_expiry() {
        let london: Tz = "Europe/London".parse().unwrap();
        // Sunday 15 Nov 2026, 10:00 in London; drafted at 09:00, so it expires Monday 09:00.
        let now = utc("2026-11-15T10:00:00Z");
        let made = json!({"created_at": "2026-11-15T09:00:00Z"});
        let exp = expiry(&made);
        assert_eq!(exp, Some(utc("2026-11-16T09:00:00Z")));
        let all = offered(now, exp, london);
        assert_eq!(
            all,
            vec![
                (Choice::Hour, utc("2026-11-15T11:00:00Z")),
                (Choice::FourHours, utc("2026-11-15T14:00:00Z")),
            ],
            "tomorrow 09:00 is the expiry itself: not offered"
        );
        // In Los Angeles, tomorrow 09:00 is Monday 17:00 UTC: past it.
        let la: Tz = "America/Los_Angeles".parse().unwrap();
        assert_eq!(
            Choice::Tomorrow.until(now, la),
            Some(utc("2026-11-16T17:00:00Z"))
        );
        // Drafted late: only an hour fits, then nothing.
        let late = Some(now + chrono::Duration::minutes(90));
        assert_eq!(offered(now, late, london).len(), 1);
        let later = Some(now + chrono::Duration::minutes(30));
        assert!(offered(now, later, london).is_empty());
        // Drafted just now: all three.
        let fresh = Some(now + DRAFT_TTL);
        assert_eq!(offered(now, fresh, london).len(), 3);
        // An item's own expires_at wins.
        assert_eq!(
            expiry(
                &json!({"created_at": "2026-11-15T09:00:00Z", "expires_at": "2026-11-15T12:00:00Z"})
            ),
            Some(utc("2026-11-15T12:00:00Z"))
        );
        assert_eq!(Choice::of_action("apr_snooze_4h"), Some(Choice::FourHours));
        assert_eq!(Choice::of_action("apr_snooze"), None);
        assert_eq!(when(now, london), "Sun 15 Nov 10:00 GMT");
    }

    #[test]
    fn snoozes_are_kept_in_the_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = SnoozeStore::open(dir.path());
        assert!(store.all().is_empty());
        let s = |id: &str, until: &str| Snooze {
            approval_id: id.into(),
            user: "U0ANA".into(),
            channel: "D0ANA".into(),
            ts: "1.0".into(),
            until: utc(until),
        };
        store.put(s("apr_1", "2026-11-15T11:00:00Z"));
        store.put(s("apr_2", "2026-11-15T14:00:00Z"));
        store.put(s("apr_1", "2026-11-15T12:00:00Z"));
        let reopened = SnoozeStore::open(dir.path());
        assert_eq!(reopened.all().len(), 2, "one per item");
        assert_eq!(
            reopened.due(utc("2026-11-15T12:00:00Z")),
            vec![s("apr_1", "2026-11-15T12:00:00Z")]
        );
        reopened.remove("apr_1");
        assert_eq!(SnoozeStore::open(dir.path()).all().len(), 1);
    }
}
