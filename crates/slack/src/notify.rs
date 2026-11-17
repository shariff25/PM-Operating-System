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
//!   before it is posted, and on `"done"`, when it came from the requester's own personal bot,
//!   it is sent to pmos-web in the background (`brief_feed.rs`), if that is configured.
//! - **Restarts:** the cursor is saved to `<state dir>/notifications.cursor` after each batch,
//!   so a restart doesn't post anything twice. With no saved cursor (the first start), items
//!   older than an hour are skipped rather than flooding the PMs' DMs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::blocks::escape;
use crate::bots::{Bots, DEFAULT_BOT};
use crate::bounded::Bounded;
use crate::brief_feed::{self, Producer};
use crate::cards::{self, Stage};
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
        let first_start = cursor.is_none();
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
                },
                // A task from outside Slack (a routine's run): DM the PM it is for.
                None => {
                    let requester = item["requested_by"].as_str().unwrap_or_default();
                    let Some(member) = self.bots.team().by_handle(requester).cloned() else {
                        tracing::warn!(
                            requester,
                            "notification for someone not in team.yaml; dropped"
                        );
                        return;
                    };
                    TaskThread {
                        channel: member.slack_id,
                        thread_ts: None,
                        progress_ts: None,
                        milestones: Vec::new(),
                    }
                }
            };
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
                    self.feed_brief(item, &bot, block);
                }
                self.finish_progress(&task_id, Stage::Done).await;
                if !text.trim().is_empty() {
                    self.say(&task_id, &bot, &username, cards::answer(&text))
                        .await;
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
                    self.say(&task_id, &bot, &username, cards::answer(&text))
                        .await;
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
                    let card = self.card(&id, requester).await;
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
            || self.bots.team().by_handle(handle).is_none()
        {
            tracing::warn!(bot, "a brief block not from a member's own bot; not sent");
            return;
        }
        let payload = match block {
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

    /// Post in the task's thread. The first message of a task with no thread starts one.
    async fn say(
        &mut self,
        task_id: &str,
        bot: &str,
        username: &str,
        message: (String, Value),
    ) -> Option<Posted> {
        let (channel, thread_ts) = {
            let t = self.tasks.get(&task_id.to_string())?;
            (t.channel.clone(), t.thread_ts.clone())
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
        }
        Some(posted)
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
                self.bots.update(&channel, &ts, message).await;
            }
            None => {
                if let Some(posted) = self.say(task_id, bot, username, message).await {
                    if let Some(t) = self.tasks.get_mut(&key) {
                        t.progress_ts = Some(posted.ts);
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
        let message = cards::progress(&t.milestones, stage);
        self.bots.update(&channel, &ts, message).await;
    }

    /// The approval card for one item, read for `requested_by` (the task's PM), retried
    /// briefly if the tool server is unavailable.
    async fn card(&self, id: &str, requested_by: &str) -> (String, Value) {
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
}
