//! The daily brief's DM commands (`docs/daily-brief.md` §1).
//!
//! **The commands** (typed alone in a DM, by members), through [`handle_dm`]:
//! - `brief me at 8`, `brief me at 8:30`, `brief me at 7am`, `daily brief at 09:00`: turn the
//!   brief on at that time. `brief on` is 08:30.
//! - `brief off`: turn it off. `brief` or `brief status`: say whether it is on and when it
//!   next runs. `brief now` (or `brief me now`): one brief now, in the thread.
//!
//! **The routine.** One routine on the PM's personal bot: `M H * * 1-5` (weekdays only) in
//! the PM's Slack time zone, else `PMOS_DEFAULT_TZ`, with output to the PM's DM. It is made
//! with `POST /v1/routines {draft}` (the model is not involved). It is found again by its
//! prompt starting with [`TAG`] (`[daily-brief]`), as `[reply-assistant]` and `[bets-review]`
//! are, so the runtime needs nothing new. Asking again never makes a second one: the same
//! time says so; a new time creates the new routine and then deletes the old one (the
//! runtime's routine PATCH is not used for schedules). `brief off` deletes only routines with
//! the tag. Times are 00:00 to 23:59 (`7am`, `7:30 pm` also work); anything else gets one
//! helpful line and changes nothing. The wording is in `brief_copy.rs`.
//!
//! **The two hooks** (one line each, in files this module does not edit):
//! - `bots.rs`, `on_dm`: [`handle_dm`], before the model-based handling.
//! - `notify.rs`, the `"done"` arm of `handle`: [`note_delivery`], so a delivered brief counts
//!   as that day's message for the nudge engine (`docs/nudges.md`).

use chrono::{DateTime, Utc};
use pmos_contract::onboarding::personal_bot_name;
use serde_json::{json, Value};

use crate::blocks::escape;
use crate::bots::Bots;
use crate::cards;
use crate::reply::{when, zone};
use crate::runtime::RuntimeError;

#[path = "brief_copy.rs"]
mod copy;

/// How Slack finds the routine: its prompt starts with this.
pub const TAG: &str = "[daily-brief]";
pub const PROMPT: &str = "[daily-brief] Write my daily brief with the daily-brief skill.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Turn it on at `(hour, minute)`.
    On(u32, u32),
    Off,
    Status,
    Now,
    /// `brief me at <something that is not a time>`.
    BadTime,
}

/// A time of day: `8`, `8:30`, `08:30`, `7am`, `7 pm`, `12:15am`. 24-hour unless am/pm.
pub fn parse_time(text: &str) -> Option<(u32, u32)> {
    let t: String = text
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let (body, meridiem) = match (t.strip_suffix("am"), t.strip_suffix("pm")) {
        (Some(b), _) => (b, Some(false)),
        (_, Some(b)) => (b, Some(true)),
        _ => (t.as_str(), None),
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h, m),
        Some(_) => return None,
        None => (body, "00"),
    };
    if h.is_empty() || h.len() > 2 || !h.chars().chain(m.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (mut h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    if m > 59 {
        return None;
    }
    match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            h %= 12;
            if pm {
                h += 12;
            }
        }
        None if h > 23 => return None,
        None => {}
    }
    Some((h, m))
}

/// A brief command, typed alone, in any case; `None` for anything else.
pub fn command(text: &str) -> Option<Command> {
    let t = text.trim().trim_end_matches(['?', '.', '!']).to_lowercase();
    let mut words: Vec<&str> = t.split_whitespace().collect();
    if words.starts_with(&["daily", "brief"]) {
        words.remove(0);
    }
    match words.as_slice() {
        ["brief"] | ["brief", "status"] => Some(Command::Status),
        ["brief", "on"] => Some(Command::On(copy::DEFAULT_TIME.0, copy::DEFAULT_TIME.1)),
        ["brief", "off"] => Some(Command::Off),
        ["brief", "now"] | ["brief", "me", "now"] => Some(Command::Now),
        ["brief", "me", "at", rest @ ..] | ["brief", "at", rest @ ..] => {
            Some(match parse_time(&rest.join(" ")) {
                Some((h, m)) => Command::On(h, m),
                None => Command::BadTime,
            })
        }
        _ => None,
    }
}

/// The cron for a time: weekdays only.
pub fn cron(hour: u32, minute: u32) -> String {
    format!("{minute} {hour} * * 1-5")
}

/// The routine `brief me at …` creates, exactly.
pub fn draft(handle: &str, hour: u32, minute: u32, tz: &str) -> Value {
    json!({
        "bot": personal_bot_name(handle),
        "owner": handle,
        "schedule": {"cron": cron(hour, minute), "tz": tz},
        "prompt": PROMPT,
        "output": {"kind": "dm"},
        "approval": "approve",
    })
}

/// A PM's brief routine, by its prompt's tag.
pub fn is_brief_routine(routine: &Value) -> bool {
    routine["prompt"]
        .as_str()
        .is_some_and(|p| p.trim_start().starts_with(TAG))
}

/// The `(hour, minute)` a routine's cron runs at.
fn time_of(routine: &Value) -> Option<(u32, u32)> {
    let cron = routine["schedule"]["cron"].as_str()?;
    let mut f = cron.split_whitespace();
    let (m, h) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?);
    Some((h, m))
}

fn clock(hm: (u32, u32)) -> String {
    format!("{:02}:{:02}", hm.0, hm.1)
}

fn next_run(routine: &Value, default_tz: &str) -> String {
    let tz = zone(routine["schedule"]["tz"].as_str(), default_tz);
    routine["next_run_at"]
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| when(t.with_timezone(&Utc), tz))
        .unwrap_or_else(|| "not scheduled yet".into())
}

fn failure(e: &RuntimeError) -> String {
    match e {
        RuntimeError::Unavailable => copy::UNAVAILABLE.into(),
        RuntimeError::Refused(why) | RuntimeError::Question(why) => copy::failed(&escape(why)),
    }
}

/// The DM hook. `slack_user` is the sender's Slack id. Returns `true` when the message was a
/// brief command (and has been answered in the thread), `false` for anything else, which
/// then goes on as before.
///
/// In `bots.rs`, `on_dm`, add it next to the other DM commands, before `level::command`:
/// `if crate::brief::handle_dm(self, &dm.channel, &dm.thread_ts, &dm.user, &text).await { return; }`
pub async fn handle_dm(
    bots: &Bots,
    channel: &str,
    thread_ts: &str,
    slack_user: &str,
    text: &str,
) -> bool {
    let Some(cmd) = command(text) else {
        return false;
    };
    let say = |line: String| async move {
        bots.post(channel, Some(thread_ts), None, cards::note(&line))
            .await;
    };
    let Some(handle) = bots
        .team()
        .by_slack_id(slack_user)
        .map(|m| m.handle.clone())
    else {
        say(copy::NOT_A_MEMBER.into()).await;
        return true;
    };
    let bot = personal_bot_name(&handle);
    match bots.roster().await {
        Ok(roster) if roster.iter().any(|b| b.name == bot) => {}
        Ok(_) => {
            say(copy::NO_BOT.into()).await;
            return true;
        }
        Err(_) => {
            say(copy::UNAVAILABLE.into()).await;
            return true;
        }
    }
    let line = match cmd {
        Command::BadTime => copy::BAD_TIME.into(),
        Command::Now => {
            bots.remember_thread(channel, thread_ts, &bot);
            match bots
                .runtime()
                .create_task(&handle, &bot, PROMPT, channel, thread_ts)
                .await
            {
                Ok(task) => {
                    tracing::info!(task = %task["id"], user = handle, "brief now");
                    // So its answer's block is known for a brief's (`is_brief_task`).
                    if let Some(id) = task["id"].as_str() {
                        mark_brief_now(id);
                    }
                    copy::NOW_STARTED.into()
                }
                Err(e) => failure(&e),
            }
        }
        cmd => routine_command(bots, &handle, slack_user, cmd).await,
    };
    say(line).await;
    true
}

async fn routine_command(bots: &Bots, handle: &str, slack_user: &str, cmd: Command) -> String {
    let runtime = bots.runtime();
    let routines = match runtime.routines(handle).await {
        Ok(r) => r,
        Err(e) => return failure(&e),
    };
    let found: Vec<&Value> = routines.iter().filter(|r| is_brief_routine(r)).collect();
    let default_tz = bots.default_tz().to_string();
    match cmd {
        Command::Status => match found.first() {
            None => copy::STATUS_OFF.into(),
            Some(r) => {
                let time = clock(time_of(r).unwrap_or(copy::DEFAULT_TIME));
                let tz = escape(r["schedule"]["tz"].as_str().unwrap_or("UTC"));
                if r["enabled"] == false {
                    copy::status_paused(&time, &tz)
                } else {
                    copy::status_on(&time, &tz, &escape(&next_run(r, &default_tz)))
                }
            }
        },
        Command::Off if found.is_empty() => copy::ALREADY_OFF.into(),
        Command::Off => {
            let mut failed = None;
            for r in &found {
                let id = r["id"].as_str().unwrap_or_default();
                if let Err(e) = runtime.delete_routine(handle, id).await {
                    failed = Some(e);
                }
            }
            match failed {
                None => {
                    tracing::info!(owner = handle, "brief off");
                    copy::OFF.into()
                }
                Some(e) => failure(&e),
            }
        }
        Command::On(h, m) => {
            let (tz, _) = bots.pm_tz(slack_user).await;
            let (time, shown_tz) = (clock((h, m)), escape(&tz));
            let wanted = cron(h, m);
            let same = found.iter().find(|r| {
                r["schedule"]["cron"] == wanted.as_str() && r["schedule"]["tz"] == tz.as_str()
            });
            if let Some(r) = same {
                if r["enabled"] == false {
                    let id = r["id"].as_str().unwrap_or_default();
                    return match runtime.set_routine_enabled(handle, id, true).await {
                        Ok(_) => copy::resumed(&time, &shown_tz),
                        Err(e) => failure(&e),
                    };
                }
                return copy::unchanged(&time, &shown_tz);
            }
            // A new time (or zone): make the new routine first, then remove the old ones, so
            // a failure never leaves the PM with none.
            if let Err(e) = runtime
                .create_routine(handle, &draft(handle, h, m, &tz))
                .await
            {
                return failure(&e);
            }
            tracing::info!(owner = handle, time = %time, "brief on");
            for r in found {
                let id = r["id"].as_str().unwrap_or_default();
                if let Err(e) = runtime.delete_routine(handle, id).await {
                    tracing::warn!(error = %e, "removing the old brief routine failed");
                }
            }
            copy::on(&time, &shown_tz)
        }
        Command::Now | Command::BadTime => unreachable!("handled before"),
    }
}

/// The notification hook, for the `"done"` arm of `notify.rs`'s `handle`: when a finished
/// task came from the PM's brief routine, today counts as their nudge day
/// (`nudges::record_daily_message`). Anything else does nothing.
///
/// The runtime's notification carries `routine_id` only for a task a routine created
/// (`docs/bot-runtime-spec.md` §5), so a notification without one costs no call. `brief now`
/// is a plain task the PM asked for, not the day's unprompted message, and is not counted.
///
/// In `notify.rs`, at the top of the `"done"` arm:
/// `crate::brief::note_delivery(&self.bots, item).await;`
pub async fn note_delivery(bots: &Bots, item: &Value) {
    let (Some(requester), Some(_)) = (item["requested_by"].as_str(), item["routine_id"].as_str())
    else {
        return;
    };
    if let Ok(routines) = bots.runtime().routines(requester).await {
        if let Some(handle) = delivered_brief_owner(item, &routines) {
            crate::nudges::record_daily_message(handle);
        }
    }
}

/// How many `brief now` tasks are remembered (each until its answer arrives).
const BRIEF_NOW_KEPT: usize = 1_000;

/// The `brief now` tasks this process started and hasn't seen finish, oldest first.
static BRIEF_NOW: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

/// Remember a `brief now` task at creation, so its answer is known for a brief's.
pub fn mark_brief_now(task_id: &str) {
    let mut kept = BRIEF_NOW.lock().expect("brief now lock");
    if kept.len() >= BRIEF_NOW_KEPT {
        kept.pop_front();
    }
    kept.push_back(task_id.to_string());
}

/// Was this task a `brief now` this process started? Forgets it: an answer comes once.
fn take_brief_now(task_id: &str) -> bool {
    let mut kept = BRIEF_NOW.lock().expect("brief now lock");
    match kept.iter().position(|t| t == task_id) {
        Some(i) => {
            kept.remove(i);
            true
        }
        None => false,
    }
}

/// Is the finished task of this `done` notification a daily brief, so its ` ```pmos-brief `
/// block may go to the web app (`notify.rs`)? Only when this service knows it is one: a
/// `brief now` it started (marked at creation; a restart in between forgets the mark, and
/// that one brief's block isn't sent), or a task its `routine_id` says came from the
/// requester's own brief routine (one runtime call, only for a notification that has both a
/// block and a `routine_id`). The block alone proves nothing: any answer can contain one.
pub async fn is_brief_task(bots: &Bots, item: &Value) -> bool {
    if item["task_id"].as_str().is_some_and(take_brief_now) {
        return true;
    }
    let (Some(requester), Some(_)) = (item["requested_by"].as_str(), item["routine_id"].as_str())
    else {
        return false;
    };
    match bots.runtime().routines(requester).await {
        Ok(routines) => delivered_brief_owner(item, &routines).is_some(),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't check the task's routine; its brief block isn't sent");
            false
        }
    }
}

/// The PM whose brief this `done` notification delivered, given their routines: the
/// notification names a routine, and that routine is one of theirs with the brief's tag.
fn delivered_brief_owner<'a>(item: &'a Value, routines: &[Value]) -> Option<&'a str> {
    let requester = item["requested_by"].as_str()?;
    let routine_id = item["routine_id"].as_str()?;
    routines
        .iter()
        .any(|r| r["id"] == routine_id && r["owner"] == requester && is_brief_routine(r))
        .then_some(requester)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("8"), Some((8, 0)));
        assert_eq!(parse_time("8:30"), Some((8, 30)));
        assert_eq!(parse_time("09:00"), Some((9, 0)));
        assert_eq!(parse_time("7am"), Some((7, 0)));
        assert_eq!(parse_time("7:15 PM"), Some((19, 15)));
        assert_eq!(parse_time("12am"), Some((0, 0)));
        assert_eq!(parse_time("12pm"), Some((12, 0)));
        assert_eq!(parse_time("23:59"), Some((23, 59)));
        for bad in [
            "24:00", "8:60", "25", "0pm", "13pm", "noon", "8:5", ":30", "", "-1",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn commands() {
        assert_eq!(command("brief me at 8"), Some(Command::On(8, 0)));
        assert_eq!(command("Brief me at 8:30."), Some(Command::On(8, 30)));
        assert_eq!(command("daily brief at 09:00"), Some(Command::On(9, 0)));
        assert_eq!(command("brief on"), Some(Command::On(8, 30)));
        assert_eq!(command("brief me at 7 am"), Some(Command::On(7, 0)));
        assert_eq!(command("brief me at 25"), Some(Command::BadTime));
        assert_eq!(command("brief off"), Some(Command::Off));
        assert_eq!(command("brief"), Some(Command::Status));
        assert_eq!(command("brief status?"), Some(Command::Status));
        assert_eq!(command("brief now"), Some(Command::Now));
        assert_eq!(command("brief me now"), Some(Command::Now));
        assert_eq!(command("write a brief for the board"), None);
        assert_eq!(command("brief the team on pricing"), None);
        assert_eq!(command("briefing"), None);
    }

    #[test]
    fn the_draft_is_weekdays_only_and_tagged() {
        let d = draft("ana", 8, 30, "Europe/London");
        assert_eq!(d["schedule"]["cron"], "30 8 * * 1-5");
        assert!(is_brief_routine(&d));
        assert!(!is_brief_routine(&crate::reply::draft("ana", "UTC")));
        assert_eq!(time_of(&d), Some((8, 30)));
    }

    #[test]
    fn brief_now_tasks_are_known_once() {
        mark_brief_now("tsk_brief_now_1");
        assert!(take_brief_now("tsk_brief_now_1"));
        assert!(!take_brief_now("tsk_brief_now_1"), "once only");
        assert!(!take_brief_now("tsk_some_other_task"));
    }

    #[test]
    fn a_delivered_brief_is_found_by_its_routine() {
        let brief = json!({"id": "rtn_b", "owner": "ana", "prompt": PROMPT});
        let other = json!({"id": "rtn_o", "owner": "ana", "prompt": "[reply-assistant] x"});
        let routines = [brief, other];
        let done = |routine: Value| {
            json!({"kind": "done", "requested_by": "ana", "task_id": "tsk_1",
                   "routine_id": routine})
        };
        assert_eq!(
            delivered_brief_owner(&done(json!("rtn_b")), &routines),
            Some("ana")
        );
        // Another routine, an unknown one, or none at all (an older runtime, `brief now`).
        assert_eq!(
            delivered_brief_owner(&done(json!("rtn_o")), &routines),
            None
        );
        assert_eq!(
            delivered_brief_owner(&done(json!("rtn_x")), &routines),
            None
        );
        assert_eq!(delivered_brief_owner(&done(Value::Null), &routines), None);
        let mut no_field = done(Value::Null);
        no_field.as_object_mut().unwrap().remove("routine_id");
        assert_eq!(delivered_brief_owner(&no_field, &routines), None);
        // Someone else's brief routine never counts for this PM.
        let theirs = [json!({"id": "rtn_b", "owner": "ben", "prompt": PROMPT})];
        assert_eq!(delivered_brief_owner(&done(json!("rtn_b")), &theirs), None);
    }
}
