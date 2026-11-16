//! Nudges: at most one short, gentle DM a day to a PM about their own work (`docs/nudges.md`).
//!
//! **Off unless `PMOS_NUDGES=1`.** The rules, all enforced here:
//! - one nudge per PM per day, in their DM, never in a channel;
//! - only inside the PM's working hours (default 09:00 to 17:00, Monday to Friday) in the
//!   time zone Slack has for them, else `PMOS_DEFAULT_TZ`;
//! - every nudge has a **Not now** button; three ignored or dismissed in a row and the PM is
//!   left alone for 14 days;
//! - `nudges off` and `nudges on` in a DM stop and resume them;
//! - a wording line is shown to a PM at most once ([`crate::nudge_copy`]);
//! - a day on which another daily message went to the PM counts as that day's nudge
//!   ([`record_daily_message`]);
//! - state lives in `<state dir>/nudges.json`, written before a nudge is posted, so a restart
//!   never sends a second one the same day.
//!
//! **Adding a kind** (a skill not tried yet, a welcome back): implement [`NudgeKind`], put its
//! lines in `nudge_copy.rs` under its id, and add it to [`default_kinds`].

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::bots::Bots;
use crate::commands::FollowUp;
use crate::nudge_copy::{self, Line};

pub const STATE_FILE: &str = "nudges.json";
/// The **Not now** button's action id.
pub const ACTION_NOT_NOW: &str = "nudge_not_now";
/// What a PM is told after **Not now**.
pub const NOT_NOW_REPLY: &str = "Okay, not now.";
/// How long the engine stays quiet after three nudges in a row went unanswered.
pub const QUIET_DAYS: u64 = 14;
/// How many nudges in a row may be ignored or dismissed.
pub const STRIKES: u32 = 3;
/// A pending draft must be at least this old before a nudge.
pub const OLDEST_MIN_HOURS: i64 = 4;
/// Slack drafts expire after this many hours (the tool server's default draft TTL).
const SLACK_DRAFT_TTL_HOURS: i64 = 24;
/// The tools whose drafts expire (`crates/toolserver` `SLACK_DRAFT_TOOLS`).
const EXPIRING_TOOLS: [&str; 2] = ["slack.post_reply", "slack.send_message"];

pub const ANSWER_OFF: &str = "Nudges are off. Say `nudges on` whenever you want them back.";
pub const ANSWER_ON: &str = "Nudges are on: at most one a day, in your working hours.";

/// `PMOS_NUDGES=1` turns the engine on; anything else leaves it off.
pub fn enabled(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
}

// ---- Time ----

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NudgeConfig {
    /// For a PM Slack has no time zone for: `PMOS_DEFAULT_TZ`.
    pub default_tz: Tz,
    /// Working hours, local: from `start` up to (not including) `end`.
    pub start: NaiveTime,
    pub end: NaiveTime,
    /// How often the loop looks.
    pub tick: Duration,
}

impl NudgeConfig {
    /// `default_tz` is `PMOS_DEFAULT_TZ`; `hours` is `PMOS_NUDGES_HOURS`, `HH:MM-HH:MM`.
    pub fn from_settings(default_tz: &str, hours: Option<&str>) -> Result<Self, String> {
        let default_tz = default_tz
            .parse()
            .map_err(|_| "PMOS_DEFAULT_TZ must be an IANA time zone for nudges".to_string())?;
        let bad = || "PMOS_NUDGES_HOURS must look like 09:00-17:00".to_string();
        let (start, end) = match hours.map(str::trim).filter(|h| !h.is_empty()) {
            None => ("09:00", "17:00"),
            Some(h) => h.split_once('-').ok_or_else(bad)?,
        };
        let parse = |t: &str| NaiveTime::parse_from_str(t.trim(), "%H:%M").map_err(|_| bad());
        let (start, end) = (parse(start)?, parse(end)?);
        if start >= end {
            return Err(bad());
        }
        Ok(Self {
            default_tz,
            start,
            end,
            tick: Duration::from_secs(60),
        })
    }
}

// ---- Kinds ----

/// A pending draft, as the tool server lists it.
#[derive(Debug, Clone)]
pub struct Draft {
    pub id: String,
    pub created_at: Option<DateTime<Utc>>,
    /// When it expires, if it does.
    pub expires_at: Option<DateTime<Utc>>,
}

/// What a kind may look at.
pub struct Context {
    pub now: DateTime<Utc>,
    /// The PM's pending drafts, oldest first (at most the first 50).
    pub drafts: Vec<Draft>,
    /// How many are pending in all.
    pub total: usize,
}

/// A nudge worth sending, before its wording is chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub kind: &'static str,
    pub count: usize,
    /// How long until the soonest expires; `None` unless everything involved expires.
    pub expires_in: Option<chrono::Duration>,
    /// What the nudge is about. When one of these is gone by the next nudge, the PM acted.
    pub refs: Vec<String>,
}

pub trait NudgeKind: Send + Sync {
    /// The id its lines carry in `nudge_copy`.
    fn id(&self) -> &'static str;
    fn evaluate(&self, ctx: &Context) -> Option<Candidate>;
}

/// The PM has pending drafts and the oldest is over four hours old.
pub struct DraftsWaiting;

impl NudgeKind for DraftsWaiting {
    fn id(&self) -> &'static str {
        nudge_copy::DRAFTS_WAITING
    }

    fn evaluate(&self, ctx: &Context) -> Option<Candidate> {
        let oldest = ctx.drafts.iter().filter_map(|d| d.created_at).min()?;
        if ctx.now - oldest < chrono::Duration::hours(OLDEST_MIN_HOURS) {
            return None;
        }
        // Only claim an expiry when every draft in the count has one.
        let all_listed = ctx.total <= ctx.drafts.len();
        let soonest = ctx
            .drafts
            .iter()
            .map(|d| d.expires_at)
            .collect::<Option<Vec<_>>>()
            .and_then(|all| all.into_iter().min())
            .filter(|_| all_listed);
        Some(Candidate {
            kind: self.id(),
            count: ctx.total.max(ctx.drafts.len()),
            expires_in: soonest.map(|at| (at - ctx.now).max(chrono::Duration::zero())),
            refs: ctx.drafts.iter().map(|d| d.id.clone()).collect(),
        })
    }
}

pub fn default_kinds() -> Vec<Box<dyn NudgeKind>> {
    vec![Box::new(DraftsWaiting)]
}

/// A draft from `GET /internal/approvals`: Slack drafts expire `created_at` + 24 hours unless
/// the item says when; other tools' drafts don't expire.
pub fn draft_of(item: &Value) -> Option<Draft> {
    let time = |v: &Value| {
        v.as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    let created_at = time(&item["created_at"]);
    let expires_at = time(&item["expires_at"]).or_else(|| {
        let tool = item["tool"].as_str()?;
        EXPIRING_TOOLS
            .contains(&tool)
            .then(|| created_at.map(|c| c + chrono::Duration::hours(SLACK_DRAFT_TTL_HOURS)))?
    });
    Some(Draft {
        id: item["id"].as_str()?.to_string(),
        created_at,
        expires_at,
    })
}

// ---- State ----

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Open {
    /// The local date it was sent.
    day: String,
    channel: String,
    ts: String,
    refs: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Pm {
    off: bool,
    /// The local date of the last nudge, or of another daily message.
    last_day: Option<String>,
    /// Nudges in a row ignored or dismissed.
    streak: u32,
    /// Quiet until this local date (the first day nudges may return).
    quiet_until: Option<String>,
    /// Ids of the wording lines already shown.
    used: Vec<String>,
    /// The nudge not yet answered.
    open: Option<Open>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    pms: BTreeMap<String, Pm>,
}

fn load(path: &Path) -> State {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::error!(error = %e, path = %path.display(), "nudge state unreadable; starting empty");
            State::default()
        }),
        Err(_) => State::default(),
    }
}

/// Write then rename, so a crash never leaves half a file.
fn save(path: &Path, state: &State) {
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_string_pretty(state).unwrap_or_default();
    if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
        tracing::error!(error = %e, "saving the nudge state failed");
    }
}

/// One more nudge went unanswered: after [`STRIKES`] in a row, go quiet.
fn strike(pm: &mut Pm, today: NaiveDate) {
    pm.streak += 1;
    if pm.streak >= STRIKES {
        pm.streak = 0;
        pm.quiet_until = today
            .checked_add_days(Days::new(QUIET_DAYS))
            .map(|d| d.to_string());
    }
}

fn is_quiet(pm: &Pm, today: &str) -> bool {
    pm.quiet_until.as_deref().is_some_and(|q| q > today)
}

// ---- Commands ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    On,
    Off,
}

/// `nudges on` or `nudges off`, typed alone, in any case.
pub fn command(text: &str) -> Option<Command> {
    let t = text.trim().trim_end_matches(['.', '!']).to_lowercase();
    let words: Vec<&str> = t.split_whitespace().collect();
    match words.as_slice() {
        ["nudges", "on"] => Some(Command::On),
        ["nudges", "off"] => Some(Command::Off),
        _ => None,
    }
}

// ---- The engine ----

#[derive(Clone)]
pub struct Nudges {
    inner: Arc<Inner>,
}

struct Inner {
    bots: Bots,
    cfg: NudgeConfig,
    path: PathBuf,
    clock: Arc<dyn Clock>,
    lines: Vec<Line>,
    kinds: Vec<Box<dyn NudgeKind>>,
    state: Mutex<State>,
    /// Slack's time zone per PM, with when it was fetched.
    tz: Mutex<HashMap<String, (DateTime<Utc>, Tz)>>,
}

const TZ_TTL: chrono::Duration = chrono::Duration::hours(6);

static INSTALLED: Mutex<Option<Nudges>> = Mutex::new(None);

/// The running engine, if there is one: what the DM and button hooks use.
pub fn installed() -> Option<Nudges> {
    INSTALLED.lock().ok().and_then(|g| g.clone())
}

/// Other modules call this when they send a PM a daily message of their own (the triage
/// digest, a routine's summary): that day then counts as the PM's nudge. A no-op when the
/// engine is off.
pub fn record_daily_message(handle: &str) {
    if let Some(n) = installed() {
        n.record_daily_message(handle);
    }
}

/// The DM hook: handles `nudges on` and `nudges off`. Returns whether it did (always `false`
/// with the engine off, so the text goes to the bot as before).
pub async fn handle_dm(channel: &str, thread_ts: &str, handle: &str, text: &str) -> bool {
    match installed() {
        Some(n) => n.dm_command(channel, thread_ts, handle, text).await,
        None => false,
    }
}

/// The button hook: the work for a **Not now** click, or `None` if `payload` is not one.
pub fn action(payload: &Value) -> Option<FollowUp> {
    if payload["type"] != "block_actions" || payload["actions"][0]["action_id"] != ACTION_NOT_NOW {
        return None;
    }
    let n = installed()?;
    let payload = payload.clone();
    Some(Box::pin(async move {
        n.handle_action(&payload).await;
    }))
}

/// Start the engine when `PMOS_NUDGES=1` (`env` reads settings): `Ok(None)` when it is off.
pub fn spawn(
    bots: &Bots,
    default_tz: &str,
    state_dir: &Path,
    env: impl Fn(&str) -> Option<String>,
    shutdown: CancellationToken,
) -> Result<Option<tokio::task::JoinHandle<()>>, String> {
    if !enabled(env("PMOS_NUDGES").as_deref()) {
        return Ok(None);
    }
    let cfg = NudgeConfig::from_settings(default_tz, env("PMOS_NUDGES_HOURS").as_deref())?;
    tracing::info!(start = %cfg.start, end = %cfg.end, tz = %cfg.default_tz, "nudges on");
    let engine = Nudges::new(bots.clone(), cfg, state_dir);
    engine.install();
    Ok(Some(tokio::spawn(engine.run(shutdown))))
}

impl Nudges {
    pub fn new(bots: Bots, cfg: NudgeConfig, state_dir: impl AsRef<Path>) -> Self {
        Self::with_parts(
            bots,
            cfg,
            state_dir,
            Arc::new(SystemClock),
            nudge_copy::LINES.to_vec(),
            default_kinds(),
        )
    }

    /// Everything replaceable, for tests.
    pub fn with_parts(
        bots: Bots,
        cfg: NudgeConfig,
        state_dir: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        lines: Vec<Line>,
        kinds: Vec<Box<dyn NudgeKind>>,
    ) -> Self {
        let dir = state_dir.as_ref();
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::error!(error = %e, dir = %dir.display(), "can't create the Slack state directory; a restart could send a second nudge");
        }
        let path = dir.join(STATE_FILE);
        let state = load(&path);
        Self {
            inner: Arc::new(Inner {
                bots,
                cfg,
                path,
                clock,
                lines,
                kinds,
                state: Mutex::new(state),
                tz: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Make this the engine the hooks talk to.
    pub fn install(&self) {
        if let Ok(mut g) = INSTALLED.lock() {
            *g = Some(self.clone());
        }
    }

    pub async fn run(self, shutdown: CancellationToken) {
        loop {
            let sent = self.tick().await;
            if sent > 0 {
                tracing::info!(sent, "nudges sent");
            }
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(self.inner.cfg.tick) => {}
            }
        }
    }

    /// Change one PM's state under the lock and save it.
    fn update<R>(&self, handle: &str, f: impl FnOnce(&mut Pm) -> R) -> R {
        let mut state = self.inner.state.lock().expect("nudge state");
        let out = f(state.pms.entry(handle.to_string()).or_default());
        save(&self.inner.path, &state);
        out
    }

    fn pm(&self, handle: &str) -> Pm {
        let state = self.inner.state.lock().expect("nudge state");
        state.pms.get(handle).cloned().unwrap_or_default()
    }

    /// The PM's time zone from Slack (cached for hours), else the service default.
    async fn tz_of(&self, handle: &str, slack_id: &str) -> Tz {
        let now = self.inner.clock.now();
        let cached = self
            .inner
            .tz
            .lock()
            .expect("tz cache")
            .get(handle)
            .filter(|(at, _)| now - *at < TZ_TTL)
            .map(|(_, tz)| *tz);
        if let Some(tz) = cached {
            return tz;
        }
        let tz = self
            .inner
            .bots
            .slack()
            .user_tz(slack_id)
            .await
            .and_then(|name| name.parse::<Tz>().ok())
            .unwrap_or(self.inner.cfg.default_tz);
        self.inner
            .tz
            .lock()
            .expect("tz cache")
            .insert(handle.to_string(), (now, tz));
        tz
    }

    /// The zone to use without asking Slack: the cached one, else the default.
    fn known_tz(&self, handle: &str) -> Tz {
        self.inner
            .tz
            .lock()
            .expect("tz cache")
            .get(handle)
            .map_or(self.inner.cfg.default_tz, |(_, tz)| *tz)
    }

    fn working(&self, now: DateTime<Utc>, tz: Tz) -> bool {
        let local = now.with_timezone(&tz);
        let t = local.time();
        !matches!(local.weekday(), Weekday::Sat | Weekday::Sun)
            && t >= self.inner.cfg.start
            && t < self.inner.cfg.end
    }

    /// Another daily message went to this PM today: today is their nudge day.
    pub fn record_daily_message(&self, handle: &str) {
        let today = self
            .inner
            .clock
            .now()
            .with_timezone(&self.known_tz(handle))
            .date_naive()
            .to_string();
        self.update(handle, |pm| pm.last_day = Some(today));
    }

    /// A DM that may be `nudges on` or `nudges off`: answers it in the thread with one plain
    /// line and returns `true`; any other text returns `false`.
    pub async fn dm_command(
        &self,
        channel: &str,
        thread_ts: &str,
        handle: &str,
        text: &str,
    ) -> bool {
        let Some(cmd) = command(text) else {
            return false;
        };
        let line = self.set_enabled(handle, cmd == Command::On);
        self.inner
            .bots
            .post(channel, Some(thread_ts), None, crate::cards::note(line))
            .await;
        true
    }

    /// `nudges on` / `nudges off`; returns the one-line answer.
    pub fn set_enabled(&self, handle: &str, on: bool) -> &'static str {
        self.update(handle, |pm| {
            pm.off = !on;
            if on {
                // Asking for them back ends any quiet period.
                pm.quiet_until = None;
                pm.streak = 0;
            }
        });
        if on {
            ANSWER_ON
        } else {
            ANSWER_OFF
        }
    }

    /// Look at every member once; returns how many nudges were sent.
    pub async fn tick(&self) -> usize {
        let team = self.inner.bots.team();
        let mut sent = 0;
        for member in &team.members {
            if self.consider(&member.handle, &member.slack_id).await {
                sent += 1;
            }
        }
        sent
    }

    async fn consider(&self, handle: &str, slack_id: &str) -> bool {
        let before = self.pm(handle);
        if before.off {
            return false;
        }
        let now = self.inner.clock.now();
        // Cheap checks first: no Slack or tool server call outside the day's window.
        let tz = self.tz_of(handle, slack_id).await;
        if !self.working(now, tz) {
            return false;
        }
        let today_date = now.with_timezone(&tz).date_naive();
        let today = today_date.to_string();
        if before.last_day.as_deref() == Some(&today) || is_quiet(&before, &today) {
            return false;
        }
        let (items, total) = match self.inner.bots.toolserver().pending_approvals(handle).await {
            Ok(found) => found,
            Err(e) => {
                tracing::info!(handle, error = %e, "nudges: can't list drafts; skipped");
                return false;
            }
        };
        let drafts: Vec<Draft> = items.iter().filter_map(draft_of).collect();

        // Did the PM answer the last nudge? Any of its drafts gone means yes.
        if let Some(open) = before.open.as_ref().filter(|o| o.day != today) {
            let acted = open.refs.iter().any(|r| !drafts.iter().any(|d| &d.id == r));
            self.update(handle, |pm| {
                pm.open = None;
                if acted {
                    pm.streak = 0;
                } else {
                    strike(pm, today_date);
                }
            });
            if is_quiet(&self.pm(handle), &today) {
                return false;
            }
        }

        let ctx = Context { now, drafts, total };
        let used = self.pm(handle).used;
        let Some((candidate, line)) = self.inner.kinds.iter().find_map(|kind| {
            let c = kind.evaluate(&ctx)?;
            let line = self.inner.lines.iter().find(|l| {
                l.kind == kind.id()
                    && !used.iter().any(|u| u == l.id)
                    && (!l.expires || c.expires_in.is_some())
            })?;
            Some((c, *line))
        }) else {
            return false;
        };
        let text = nudge_copy::render(&line, candidate.count, candidate.expires_in);

        // Recorded before the post, so a crash after it can't send a second one.
        let reserved = self.update(handle, |pm| {
            if pm.off || pm.last_day.as_deref() == Some(&today) {
                return false;
            }
            pm.last_day = Some(today.clone());
            pm.used.push(line.id.to_string());
            pm.open = Some(Open {
                day: today.clone(),
                channel: slack_id.to_string(),
                ts: String::new(),
                refs: candidate.refs.clone(),
            });
            true
        });
        if !reserved {
            return false;
        }
        let posted = self
            .inner
            .bots
            .post(slack_id, None, None, message(&text, line.id))
            .await;
        match posted {
            Some(p) => {
                self.update(handle, |pm| {
                    if let Some(open) = pm.open.as_mut() {
                        open.channel = p.channel.clone();
                        open.ts = p.ts.clone();
                    }
                });
                tracing::info!(handle, line = line.id, "nudge sent");
                true
            }
            None => {
                self.update(handle, |pm| {
                    pm.last_day = before.last_day.clone();
                    pm.used.retain(|u| u != line.id);
                    pm.open = None;
                });
                false
            }
        }
    }

    /// A **Not now** click: counts as a dismissal and takes the button away.
    pub async fn handle_action(&self, payload: &Value) {
        let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(String::from);
        let (Some(user), Some(channel), Some(ts)) = (
            s(&payload["user"]["id"]),
            s(&payload["container"]["channel_id"]).or_else(|| s(&payload["channel"]["id"])),
            s(&payload["container"]["message_ts"]).or_else(|| s(&payload["message"]["ts"])),
        ) else {
            return;
        };
        let team = self.inner.bots.team();
        let Some(member) = team.by_slack_id(&user) else {
            return;
        };
        let handle = member.handle.clone();
        let tz = self.tz_of(&handle, &user).await;
        let today = self.inner.clock.now().with_timezone(&tz).date_naive();
        self.update(&handle, |pm| {
            if pm.open.as_ref().is_some_and(|o| o.ts == ts) {
                pm.open = None;
                strike(pm, today);
            }
        });
        self.inner
            .bots
            .update(&channel, &ts, crate::cards::note(NOT_NOW_REPLY))
            .await;
    }
}

/// The nudge as Slack shows it: the line, a **Not now** button and how to stop them.
pub fn message(text: &str, line_id: &str) -> (String, Value) {
    let blocks = json!([
        {"type": "section", "text": {"type": "mrkdwn", "text": text}},
        {"type": "actions", "block_id": "nudge", "elements": [{
            "type": "button",
            "action_id": ACTION_NOT_NOW,
            "text": {"type": "plain_text", "text": "Not now"},
            "value": line_id,
        }]},
        {"type": "context", "elements": [{"type": "mrkdwn", "text": "Say `nudges off` to stop these."}]},
    ]);
    (text.to_string(), blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn switch_and_commands() {
        assert!(enabled(Some("1")) && enabled(Some(" 1 ")));
        assert!(!enabled(None) && !enabled(Some("0")) && !enabled(Some("true")));
        assert_eq!(command("nudges off"), Some(Command::Off));
        assert_eq!(command(" Nudges ON. "), Some(Command::On));
        assert_eq!(command("nudges"), None);
        assert_eq!(command("nudges off please"), None);
    }

    #[test]
    fn hours_setting() {
        let c = NudgeConfig::from_settings("Europe/London", None).unwrap();
        assert_eq!(c.start, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(c.end, NaiveTime::from_hms_opt(17, 0, 0).unwrap());
        let c = NudgeConfig::from_settings("UTC", Some("08:30-16:00")).unwrap();
        assert_eq!(c.start, NaiveTime::from_hms_opt(8, 30, 0).unwrap());
        assert!(NudgeConfig::from_settings("UTC", Some("17:00-09:00")).is_err());
        assert!(NudgeConfig::from_settings("UTC", Some("nine")).is_err());
        assert!(NudgeConfig::from_settings("Nowhere/Land", None).is_err());
    }

    #[test]
    fn drafts_waiting_rules() {
        let now = t("2026-11-17T18:00:00Z");
        let item = |id: &str, tool: &str, hours_ago: i64| {
            draft_of(&json!({
                "id": id, "tool": tool,
                "created_at": (now - chrono::Duration::hours(hours_ago)).to_rfc3339(),
            }))
            .unwrap()
        };
        let ctx = |drafts: Vec<Draft>, total| Context { now, drafts, total };
        let kind = DraftsWaiting;
        // Nothing, or all young: no nudge.
        assert!(kind.evaluate(&ctx(vec![], 0)).is_none());
        assert!(kind
            .evaluate(&ctx(vec![item("a", "slack.post_reply", 3)], 1))
            .is_none());
        // Slack drafts: the expiry is the soonest.
        let c = kind
            .evaluate(&ctx(
                vec![
                    item("a", "slack.post_reply", 5),
                    item("b", "slack.send_message", 1),
                ],
                2,
            ))
            .unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(c.expires_in, Some(chrono::Duration::hours(19)));
        // A draft that doesn't expire: no expiry claim.
        let c = kind
            .evaluate(&ctx(
                vec![
                    item("a", "slack.post_reply", 5),
                    item("b", "jira.create", 6),
                ],
                2,
            ))
            .unwrap();
        assert_eq!(c.expires_in, None);
        // More pending than listed: unknown, so no claim; the count is the total.
        let c = kind
            .evaluate(&ctx(vec![item("a", "slack.post_reply", 5)], 70))
            .unwrap();
        assert_eq!((c.count, c.expires_in), (70, None));
    }

    #[test]
    fn strikes_make_quiet() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let mut pm = Pm::default();
        strike(&mut pm, day);
        strike(&mut pm, day);
        assert!(pm.quiet_until.is_none());
        strike(&mut pm, day);
        assert_eq!(pm.quiet_until.as_deref(), Some("2026-12-04"));
        assert_eq!(pm.streak, 0);
        assert!(is_quiet(&pm, "2026-12-03"));
        assert!(!is_quiet(&pm, "2026-12-04"));
    }
}
