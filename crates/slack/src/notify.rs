//! The notification loop: one long-poll on the runtime's `GET /v1/notifications`, posting
//! each task's progress and results in its Slack thread, as the bot that did the work.
//!
//! - **Progress (D6):** a task's milestones share one message, posted at the first one and
//!   edited in place (`chat.update`) as the run goes, so a thread never gets more than one
//!   message per milestone.
//! - **Result:** the final answer is a new message, so the PM is notified.
//! - **Failure:** the stated reason, as a new message.
//! - **Approvals (D7):** a card per pending item, from the tool server. A new or closed
//!   approval also refreshes the PM's App Home if they have opened it (`home.rs`).
//! - **The daily brief's block:** a ` ```pmos-brief ` block is taken out of every answer
//!   before it is posted. On `"done"` it is sent to pmos-web in the background
//!   (`brief_feed.rs`), if that is configured, only when the task really is a brief: its
//!   `routine_id` names the requester's own brief routine, or it is a `brief now` task this
//!   service started (`brief::is_brief_task`), and it came from the requester's own personal
//!   bot. A block in any other answer is dropped, never sent: the block alone proves nothing.
//! - **Restarts:** the cursor is saved to `<state dir>/notifications.cursor` after each batch,
//!   so a restart doesn't post anything twice. With no saved cursor (the first start), items
//!   older than an hour are skipped rather than flooding the PMs' DMs.
//! - **A cursor the runtime can't continue from** (its database was reset or restored, so the
//!   saved cursor is ahead of it): the notifier starts again from the feed's present and logs
//!   a warning, rather than waiting forever for a cursor that will never come. See
//!   [`cursor_reset`] for the answers it recognises; each is optional.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::blocks::escape;
use crate::bots::{Bots, DEFAULT_BOT};
use crate::bounded::Bounded;
use crate::brief_feed::{self, Producer};
use crate::cards::{self, Stage};
use crate::personas::{self, Persona, Speaker};
use crate::slackapi::Posted;
use crate::toolserver::CallError;

pub const CURSOR_FILE: &str = "notifications.cursor";

/// Where one task's messages go.
struct TaskThread {
    channel: String,
    /// `None` until the first message of a task that started outside Slack (a routine's
    /// run); that message becomes the thread.
    thread_ts: Option<String>,
    progress_ts: Option<String>,
    milestones: Vec<String>,
    /// The marketplace bot the task runs as (`None`: Nudge), named on every message.
    speaker: Option<Speaker>,
    /// A Preview persona's key and line, shown once per thread.
    preview: Option<(String, String)>,
    /// The progress message carries the Preview line, so its edits keep it.
    progress_preview: bool,
}

pub struct Notifier {
    bots: Bots,
    cursor_file: PathBuf,
    /// How long each long-poll waits. Default 25 s (the runtime's maximum).
    pub wait: Duration,
    /// On the first start, items older than this are skipped. Default 1 hour.
    pub max_age_on_first_start: Duration,
    tasks: Bounded<String, TaskThread>,
    seen: Bounded<String, ()>,
    brief_feed: Option<Producer>,
}

impl Notifier {
    pub fn new(bots: Bots, state_dir: impl AsRef<Path>) -> Self {
        Self {
            bots,
            cursor_file: state_dir.as_ref().join(CURSOR_FILE),
            wait: Duration::from_secs(25),
            max_age_on_first_start: Duration::from_secs(3600),
            tasks: Bounded::new(1000),
            seen: Bounded::new(5000),
            brief_feed: None,
        }
    }

    /// Send each brief's block to pmos-web (`brief_feed.rs`). `None`, the default: the block
    /// is still taken out of the brief, and nothing is sent.
    pub fn with_brief_feed(mut self, producer: Option<Producer>) -> Self {
        self.brief_feed = producer;
        self
    }

    /// Poll until `shutdown`. Errors back off (1 s doubling to 30 s) and never end the loop.
    pub async fn run(mut self, shutdown: CancellationToken) {
        if let Some(dir) = self.cursor_file.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                tracing::error!(error = %e, dir = %dir.display(), "can't create the Slack state directory; the cursor won't survive a restart");
            }
        }
        let mut cursor = read_cursor(&self.cursor_file);
        let mut first_start = cursor.is_none();
        let epoch_file = self.cursor_file.with_file_name(EPOCH_FILE);
        let mut epoch = read_cursor(&epoch_file);
        if first_start {
            tracing::info!("no saved notification cursor; skipping anything older than an hour");
        }
        let mut backoff = Duration::from_secs(1);
        loop {
            let polled = tokio::select! {
                _ = shutdown.cancelled() => return,
                r = self.bots.runtime().notifications(cursor.as_deref(), self.wait) => r,
            };
            let batch = match polled {
                Ok(batch) => {
                    backoff = Duration::from_secs(1);
                    batch
                }
                Err(e) => {
                    tracing::warn!(error = %e, wait_s = backoff.as_secs(), "notifications poll failed; retrying");
                    tokio::select! {
                        _ = shutdown.cancelled() => return,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    continue;
                }
            };
            if let Some(reset) = cursor_reset(&batch, cursor.as_deref(), epoch.as_deref()) {
                // Ids only, never message text.
                tracing::warn!(
                    saved = cursor.as_deref().unwrap_or("none"),
                    restart_from = reset.as_deref().unwrap_or("the start, skipping old items"),
                    "the runtime can't continue from the saved notification cursor; starting \
                     again from now"
                );
                cursor = reset;
                match &cursor {
                    Some(c) => {
                        if let Err(e) = write_cursor(&self.cursor_file, c) {
                            tracing::error!(error = %e, "saving the notification cursor failed");
                        }
                    }
                    None => {
                        let _ = std::fs::remove_file(&self.cursor_file);
                        // From the feed's start: nothing older than an hour is posted.
                        first_start = true;
                    }
                }
                epoch = remember_epoch(&epoch_file, &batch, epoch);
                continue;
            }
            epoch = remember_epoch(&epoch_file, &batch, epoch);
            let items = batch["items"].as_array().cloned().unwrap_or_default();
            for item in &items {
                if first_start && older_than(item, self.max_age_on_first_start) {
                    tracing::info!(id = %item["id"], "old notification skipped on first start");
                    continue;
                }
                self.handle(item).await;
            }
            if let Some(next) = cursor_of(&batch["cursor"]) {
                if cursor.as_deref() != Some(next.as_str()) {
                    if let Err(e) = write_cursor(&self.cursor_file, &next) {
                        tracing::error!(error = %e, "saving the notification cursor failed");
                    }
                    cursor = Some(next);
                }
            }
            if items.is_empty() {
                // A runtime that answers at once with nothing must not make this spin.
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                }
            }
        }
    }

    async fn handle(&mut self, item: &Value) {
        if let Some(id) = item["id"].as_str() {
            if !self.seen.first_time(id.to_string()) {
                return;
            }
        }
        let Some(task_id) = item["task_id"].as_str().map(String::from) else {
            tracing::warn!(id = %item["id"], "notification without a task; ignored");
            return;
        };
        let bot = item["bot"].as_str().unwrap_or(DEFAULT_BOT).to_string();
        if let (Some(channel), Some(thread)) = (
            item["slack"]["channel"].as_str(),
            item["slack"]["thread_ts"].as_str(),
        ) {
            // Also rebuilds the thread → bot map after a restart.
            self.bots.remember_thread(channel, thread, &bot);
        }
        if self.tasks.get(&task_id).is_none() {
            let place = match item["slack"]["channel"].as_str() {
                Some(channel) => TaskThread {
                    channel: channel.to_string(),
                    thread_ts: item["slack"]["thread_ts"].as_str().map(String::from),
                    progress_ts: None,
                    milestones: Vec::new(),
                    speaker: None,
                    preview: None,
                    progress_preview: false,
                },
                // A task from outside Slack (a routine's run): DM the PM it is for.
                None => {
                    let requester = item["requested_by"].as_str().unwrap_or_default();
                    let who = self.bots.people().of_handle(requester);
                    let Some(slack_id) = who.member().and_then(|m| m.slack_id.clone()) else {
                        tracing::warn!(
                            requester,
                            who = who.kind(),
                            "notification for someone who may not use PM OS in Slack; dropped"
                        );
                        return;
                    };
                    TaskThread {
                        channel: slack_id,
                        thread_ts: None,
                        progress_ts: None,
                        milestones: Vec::new(),
                        speaker: None,
                        preview: None,
                        progress_preview: false,
                    }
                }
            };
            let mut place = place;
            if let Some((speaker, persona)) = self.speaker(item).await {
                place.preview = persona
                    .filter(|p| p.preview)
                    .map(|p| (p.key.clone(), Speaker::preview_line(&p)));
                place.speaker = Some(speaker);
            }
            self.tasks.insert(task_id.clone(), place);
        }
        let username = self.bots.display_name(&bot).await;
        // The brief's machine-readable block is for the web app, never for the PM to read.
        let split = brief_feed::split(
            item["text"].as_str().unwrap_or_default(),
            chrono::Utc::now(),
        );
        let text = split.text;
        match item["kind"].as_str().unwrap_or_default() {
            "milestone" => self.milestone(&task_id, &bot, &username, &text).await,
            "done" => {
                // A delivered daily brief is that day's one message from PM OS.
                crate::brief::note_delivery(&self.bots, item).await;
                if let Some(block) = split.block {
                    if crate::brief::is_brief_task(&self.bots, item).await {
                        self.feed_brief(item, &bot, block);
                    } else {
                        tracing::warn!(
                            task = %task_id,
                            "a brief block in a task that isn't a brief; not sent"
                        );
                    }
                }
                self.finish_progress(&task_id, Stage::Done).await;
                if !text.trim().is_empty() {
                    let answer = personas::mark_fallback(cards::answer(&text), item);
                    self.say(&task_id, &bot, &username, answer).await;
                    // The first answer a new member gets: one closing line, once.
                    if let (Some(handle), Some((channel, thread))) = (
                        item["requested_by"].as_str(),
                        self.tasks
                            .get(&task_id)
                            .map(|t| (t.channel.clone(), t.thread_ts.clone())),
                    ) {
                        self.bots
                            .onboarding_closing(handle, &channel, thread.as_deref())
                            .await;
                    }
                }
                self.tasks.remove(&task_id);
            }
            "needs_approval" => {
                self.finish_progress(&task_id, Stage::Waiting).await;
                if !text.trim().is_empty() {
                    let answer = personas::mark_fallback(cards::answer(&text), item);
                    self.say(&task_id, &bot, &username, answer).await;
                }
                let ids: Vec<String> = item["approvals"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                // The cards are read for the PM the task is for: the tool server shows the
                // bot an item only for the member it belongs to.
                let requester = item["requested_by"].as_str().unwrap_or_default();
                for id in ids {
                    // Each draft card of a task a fallback model answered says so (MP7).
                    let card = personas::mark_fallback(self.card(&id, requester).await, item);
                    self.say(&task_id, &bot, &username, card).await;
                }
                self.refresh_home(item);
            }
            "approval_closed" => {
                if !text.trim().is_empty() {
                    self.say(&task_id, &bot, &username, cards::answer(&text))
                        .await;
                }
                self.tasks.remove(&task_id);
                self.refresh_home(item);
            }
            "failed" => {
                self.finish_progress(&task_id, Stage::Failed).await;
                let reason = Some(text.as_str())
                    .filter(|t| !t.trim().is_empty())
                    .or(item["error"].as_str())
                    .unwrap_or("no reason was given");
                let message = format!("I couldn't finish this: {}", escape(reason));
                self.say(&task_id, &bot, &username, cards::note(&message))
                    .await;
                self.tasks.remove(&task_id);
            }
            other => tracing::warn!(kind = other, "unknown notification kind; ignored"),
        }
    }

    /// A finished brief's block, to pmos-web, in the background: only for a team member's
    /// own personal bot, and only when it is well formed. Never holds up the brief.
    fn feed_brief(&self, item: &Value, bot: &str, block: Result<brief_feed::Payload, String>) {
        let handle = item["requested_by"].as_str().unwrap_or_default();
        if handle.is_empty()
            || bot != pmos_contract::onboarding::personal_bot_name(handle)
            || self.bots.people().of_handle(handle).member().is_none()
        {
            tracing::warn!(bot, "a brief block not from a member's own bot; not sent");
            return;
        }
        let mut payload = match block {
            Ok(p) => p,
            Err(why) => {
                tracing::warn!(
                    user = handle,
                    why,
                    "the brief's block is malformed; not sent"
                );
                return;
            }
        };
        // Who wrote it, for Today's attribution (slice R2): the runtime's notification names
        // the persona when it isn't Nudge. A key not shaped like one is left out (Nudge),
        // never passed on.
        payload.persona = item["persona"]
            .as_str()
            .filter(|k| *k != pmos_contract::personas::ALWAYS_ON)
            .filter(|k| pmos_contract::bots::valid_name(k))
            .map(String::from);
        let Some(producer) = self.brief_feed.clone() else {
            return;
        };
        let handle = handle.to_string();
        tokio::spawn(async move { producer.deliver(&handle, &payload).await });
    }

    /// Republish the requester's App Home, if they have it open; best effort.
    fn refresh_home(&self, item: &Value) {
        if let Some(handle) = item["requested_by"].as_str() {
            self.bots.refresh_home(handle);
        }
    }

    /// Who a notification's task runs as: `None` for Nudge (a notification carries `persona`
    /// only when it isn't Nudge). The name comes from the runtime's catalog, never from the
    /// key; a key it doesn't list reads "A bot no longer offered". With no catalog at all (the
    /// runtime unreachable and nothing read before), the bot is "A marketplace bot" and its
    /// thread still gets the Preview line: nothing says it was benchmarked (gate-fix-4, M1).
    async fn speaker(&self, item: &Value) -> Option<(Speaker, Option<Persona>)> {
        let persona = &item["persona"];
        let key = persona.as_str().filter(|k| *k != "nudge")?;
        match self.bots.personas().await {
            Ok(catalog) => Speaker::of(persona, &catalog),
            Err(e) => {
                tracing::warn!(error = %e, "persona catalog unreadable: the bot is unnamed and shown as a Preview");
                let stand_in = personas::unnamed(key);
                Some((
                    Speaker {
                        name: stand_in.shown(),
                        preview: None,
                    },
                    Some(stand_in),
                ))
            }
        }
    }

    /// Post in the task's thread. The first message of a task with no thread starts one.
    async fn say(
        &mut self,
        task_id: &str,
        bot: &str,
        username: &str,
        message: (String, Value),
    ) -> Option<Posted> {
        self.say_with(task_id, bot, username, message)
            .await
            .map(|(posted, _)| posted)
    }

    /// [`Notifier::say`], and whether the message carried the Preview line. A persona's
    /// message starts with its name; a Preview persona's first message in a thread also says
    /// "Preview: not yet benchmarked".
    async fn say_with(
        &mut self,
        task_id: &str,
        bot: &str,
        username: &str,
        message: (String, Value),
    ) -> Option<(Posted, bool)> {
        let (channel, thread_ts, speaker, preview) = {
            let t = self.tasks.get(&task_id.to_string())?;
            (
                t.channel.clone(),
                t.thread_ts.clone(),
                t.speaker.clone(),
                t.preview.clone(),
            )
        };
        let mut previewed = false;
        let message = match speaker {
            Some(mut speaker) => {
                if let Some((key, line)) = &preview {
                    // A task with no thread yet starts one: its first message is the first.
                    let first = match &thread_ts {
                        Some(ts) => self.bots.first_preview(&channel, ts, key),
                        None => true,
                    };
                    if first {
                        speaker.preview = Some(line.clone());
                        previewed = true;
                    }
                }
                personas::attribute(message, &speaker)
            }
            None => message,
        };
        let posted = self
            .bots
            .post(&channel, thread_ts.as_deref(), Some(username), message)
            .await?;
        if thread_ts.is_none() {
            if let Some(t) = self.tasks.get_mut(&task_id.to_string()) {
                t.channel = posted.channel.clone();
                t.thread_ts = Some(posted.ts.clone());
            }
            self.bots.remember_thread(&posted.channel, &posted.ts, bot);
            if let Some((key, _)) = &preview {
                self.bots.first_preview(&posted.channel, &posted.ts, key);
            }
        }
        Some((posted, previewed))
    }

    /// A progress message as its task's persona says it, keeping the Preview line on a
    /// message that was posted with it.
    fn attributed(t: &TaskThread, message: (String, Value)) -> (String, Value) {
        match &t.speaker {
            Some(speaker) => {
                let mut speaker = speaker.clone();
                if t.progress_preview {
                    speaker.preview = t.preview.as_ref().map(|(_, line)| line.clone());
                }
                personas::attribute(message, &speaker)
            }
            None => message,
        }
    }

    async fn milestone(&mut self, task_id: &str, bot: &str, username: &str, text: &str) {
        let key = task_id.to_string();
        let Some(t) = self.tasks.get_mut(&key) else {
            return;
        };
        if !text.trim().is_empty() && !t.milestones.iter().any(|m| m == text) {
            t.milestones.push(text.to_string());
        }
        let message = cards::progress(&t.milestones, Stage::Running);
        match t.progress_ts.clone() {
            Some(ts) => {
                let channel = t.channel.clone();
                let message = Self::attributed(t, message);
                self.bots.update(&channel, &ts, message).await;
            }
            None => {
                if let Some((posted, previewed)) =
                    self.say_with(task_id, bot, username, message).await
                {
                    if let Some(t) = self.tasks.get_mut(&key) {
                        t.progress_ts = Some(posted.ts);
                        t.progress_preview = previewed;
                    }
                }
            }
        }
    }

    /// Mark the progress message finished, if there is one.
    async fn finish_progress(&mut self, task_id: &str, stage: Stage) {
        let Some(t) = self.tasks.get(&task_id.to_string()) else {
            return;
        };
        let (Some(ts), false) = (t.progress_ts.clone(), t.milestones.is_empty()) else {
            return;
        };
        let channel = t.channel.clone();
        let message = Self::attributed(t, cards::progress(&t.milestones, stage));
        self.bots.update(&channel, &ts, message).await;
    }

    /// The approval card for one item, read for `requested_by` (the task's PM), retried
    /// briefly if the tool server is unavailable.
    async fn card(&self, id: &str, requested_by: &str) -> (String, Value) {
        // "Drafted by Chief" needs the catalog: at most a minute old.
        let _ = self.bots.personas().await;
        for attempt in 0..3 {
            match self.bots.toolserver().approval(id, requested_by).await {
                Ok(item) if item["status"] == "pending" => {
                    return cards::approval_card(&item, self.bots.web_url());
                }
                Ok(item) => {
                    let line = cards::decision_line(&item, "", false);
                    return cards::approval_closed(&item, &line);
                }
                Err(CallError::Unavailable) if attempt < 2 => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, id, "approval item unavailable for its card");
                    break;
                }
            }
        }
        cards::note(&format!(
            "An approval is waiting (`{}`), but PM OS couldn't load it. It is in the console's \
             approval queue.",
            escape(id)
        ))
    }
}

/// `true` if the item says when it happened (`at` or `created_at`) and that is longer ago
/// than `max_age`. An item with no time is never skipped.
fn older_than(item: &Value, max_age: Duration) -> bool {
    let Some(at) = item["at"].as_str().or(item["created_at"].as_str()) else {
        return false;
    };
    let Ok(at) = chrono::DateTime::parse_from_rfc3339(at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(at);
    age.to_std().is_ok_and(|age| age > max_age)
}

/// Where the feed's `epoch` is kept beside the cursor, when the runtime sends one.
pub const EPOCH_FILE: &str = "notifications.epoch";

/// Should the notifier start again, and from where? `Some(Some(head))`: from `head`, the
/// feed's present; `Some(None)`: from the feed's start, skipping old items; `None`: carry on.
///
/// The runtime's notification feed may say its position can't be continued from in any of
/// these ways, each optional (an answer with none of them changes nothing):
/// - **Explicitly:** `{"code": "cursor_ahead" | "cursor_unknown", "cursor"?: <head>}`, as a
///   `4xx` (`runtime.rs` passes it on as a batch with no items) or in a `200` batch; or
///   `{"cursor_reset": true, "cursor"?: <head>}`. `head` may also come as `head`.
/// - **By epoch:** a batch's `epoch` (any string or number naming the feed's lifetime) that
///   differs from the one the notifier saw before: from the batch's `cursor`, if it has one.
/// - **By position (fallback):** a batch's `head` (the feed's latest cursor), when the saved
///   cursor and `head` are both whole numbers and the saved one is greater: from `head`.
pub fn cursor_reset(
    batch: &Value,
    saved: Option<&str>,
    epoch: Option<&str>,
) -> Option<Option<String>> {
    let head = cursor_of(&batch["head"]).or_else(|| cursor_of(&batch["cursor"]));
    let code = batch["code"].as_str().unwrap_or_default();
    if crate::runtime::CURSOR_CODES.contains(&code) || batch["cursor_reset"] == true {
        return Some(head);
    }
    if let (Some(seen), Some(now)) = (epoch, cursor_of(&batch["epoch"])) {
        if seen != now {
            return Some(cursor_of(&batch["cursor"]));
        }
    }
    let saved: u128 = saved?.parse().ok()?;
    let head_n: u128 = cursor_of(&batch["head"])?.parse().ok()?;
    (saved > head_n).then(|| Some(head_n.to_string()))
}

/// Keep the batch's `epoch`, if it sends one and it is new.
fn remember_epoch(path: &Path, batch: &Value, seen: Option<String>) -> Option<String> {
    match cursor_of(&batch["epoch"]) {
        Some(now) if seen.as_deref() != Some(now.as_str()) => {
            if let Err(e) = write_cursor(path, &now) {
                tracing::error!(error = %e, "saving the notification epoch failed");
            }
            Some(now)
        }
        _ => seen,
    }
}

fn read_cursor(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write then rename, so a crash never leaves half a cursor.
fn write_cursor(path: &Path, cursor: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, cursor)?;
    std::fs::rename(tmp, path)
}

fn cursor_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ages_and_cursors() {
        let hour = Duration::from_secs(3600);
        let old = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let new = chrono::Utc::now().to_rfc3339();
        assert!(older_than(&json!({"at": old}), hour));
        assert!(!older_than(&json!({"created_at": new}), hour));
        assert!(!older_than(&json!({}), hour), "no time: never skipped");
        assert_eq!(cursor_of(&json!("c-9")).as_deref(), Some("c-9"));
        assert_eq!(cursor_of(&json!(12)).as_deref(), Some("12"));
        assert_eq!(cursor_of(&json!(null)), None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CURSOR_FILE);
        assert_eq!(read_cursor(&path), None);
        write_cursor(&path, "c-3").unwrap();
        assert_eq!(read_cursor(&path).as_deref(), Some("c-3"));
    }

    #[test]
    fn a_cursor_the_runtime_cant_continue_from_starts_again_from_now() {
        // An ordinary batch, with or without the optional fields: carry on.
        let plain = json!({"cursor": "12", "items": []});
        assert_eq!(cursor_reset(&plain, Some("10"), None), None);
        assert_eq!(cursor_reset(&plain, None, None), None);
        assert_eq!(
            cursor_reset(&json!({"cursor": "12", "head": "12"}), Some("12"), None),
            None
        );
        // Explicit: from the head it names, or from the start when it names none.
        for code in ["cursor_ahead", "cursor_unknown"] {
            assert_eq!(
                cursor_reset(&json!({"code": code, "cursor": "40"}), Some("900"), None),
                Some(Some("40".into()))
            );
            assert_eq!(
                cursor_reset(&json!({"code": code, "head": 40}), Some("900"), None),
                Some(Some("40".into()))
            );
            assert_eq!(
                cursor_reset(&json!({"code": code}), Some("900"), None),
                Some(None)
            );
        }
        assert_eq!(
            cursor_reset(
                &json!({"cursor_reset": true, "cursor": "3"}),
                Some("9"),
                None
            ),
            Some(Some("3".into()))
        );
        // A new epoch: from the batch's cursor.
        let fresh = json!({"epoch": "e2", "cursor": "5", "items": []});
        assert_eq!(
            cursor_reset(&fresh, Some("900"), Some("e1")),
            Some(Some("5".into()))
        );
        assert_eq!(cursor_reset(&fresh, Some("900"), Some("e2")), None);
        assert_eq!(
            cursor_reset(&fresh, Some("900"), None),
            None,
            "the first sight of an epoch"
        );
        // Fallback: the saved cursor is past the feed's head.
        let behind = json!({"cursor": "900", "head": "41", "items": []});
        assert_eq!(
            cursor_reset(&behind, Some("900"), None),
            Some(Some("41".into()))
        );
        assert_eq!(cursor_reset(&behind, Some("41"), None), None);
        // Cursors that aren't numbers are never compared.
        let opaque = json!({"cursor": "c-9", "head": "c-1"});
        assert_eq!(cursor_reset(&opaque, Some("c-9"), None), None);
    }
}
