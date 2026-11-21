//! Routines in plain language (D8): the prompt that asks the model for a routine, the strict
//! checks its answer must pass, the next run time, and the sentence Slack reads back.
//!
//! The model only proposes. Everything it returns is checked in code: the cron has exactly
//! five fields and parses, the time zone is an IANA name, the bot and owner come from the
//! request (never the model), and `approval` is always `approve` in v1.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::parser::{CronParser, Seconds, Year};
use croner::Cron;
use serde::Deserialize;
use serde_json::Value;

use crate::model::{Draft, Output, Schedule};

/// The only approval setting a routine may have in v1.
pub const APPROVAL: &str = "approve";

/// What the model answered, once checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Draft(Draft),
    /// The request was unclear; ask the PM this.
    Question(String),
}

pub const ASK_AGAIN: &str =
    "I couldn't turn that into a routine. When should it run, and what should the bot do each time?";
pub const ASK_WHEN: &str =
    "When should it run? For example: every weekday at 8am, or every Monday at 9:30.";
pub const ASK_WHAT: &str = "What should the bot do each time it runs?";
pub const ASK_TZ: &str = "Which time zone should it follow? For example: London, or New York.";
pub const NEVER_RUNS: &str =
    "That schedule never comes round (no such date). When should it run instead?";

/// Parse a cron of exactly five fields: minute, hour, day of month, month, day of week.
/// Nicknames (`@daily`) and seconds or year fields are refused, so what is stored is what
/// the console shows.
pub fn parse_cron(cron: &str) -> Result<Cron, String> {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 5 || cron.contains('@') {
        return Err(format!(
            "the schedule `{cron}` must have five fields: minute hour day-of-month month day-of-week"
        ));
    }
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(&fields.join(" "))
        .map_err(|e| format!("the schedule `{cron}` isn't a valid cron: {e}"))
}

/// An IANA time zone name, exactly as the database spells it (`Europe/London`).
pub fn parse_tz(tz: &str) -> Result<Tz, String> {
    tz.parse::<Tz>()
        .map_err(|_| format!("`{tz}` isn't an IANA time zone, such as Europe/London"))
}

/// The first run strictly after `after`, in the schedule's own time zone (so 8:00 London
/// stays 8:00 London across a clock change).
pub fn next_run(schedule: &Schedule, after: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    let cron = parse_cron(&schedule.cron)?;
    let tz = parse_tz(&schedule.tz)?;
    cron.find_next_occurrence(&after.with_timezone(&tz), false)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("the schedule `{}` never runs", schedule.cron))
}

/// The system prompt for `pmos/extraction`.
pub fn system_prompt(bot: &str, tz: &str, now: DateTime<Utc>) -> String {
    format!(
        "You turn a product manager's request for a recurring task into a routine for the bot `{bot}`. \
Reply with one JSON object and nothing else.\n\
\n\
If the request says clearly when to run and what to do, reply:\n\
{{\"schedule\": {{\"cron\": \"<minute> <hour> <day-of-month> <month> <day-of-week>\", \"tz\": \"<IANA time zone>\"}}, \
\"prompt\": \"<what the bot does on each run>\", \"output\": {{\"kind\": \"dm\"}}, \"approval\": \"approve\"}}\n\
- `cron` has exactly five fields. Day of week: 1-5 is Monday to Friday, 0 is Sunday. 8am is `0 8 * * *`.\n\
- `tz` is the PM's time zone, {tz}, unless the request names another place; always an IANA name such as Europe/London.\n\
- `prompt` is the task in the PM's own words, as an instruction, without the schedule.\n\
\n\
If when to run or what to do is unclear, reply: {{\"question\": \"<one short question for the PM>\"}}\n\
\n\
Now is {now} UTC.",
        now = now.format("%Y-%m-%d %H:%M")
    )
}

#[derive(Debug, Deserialize)]
struct ModelSchedule {
    #[serde(default)]
    cron: Option<String>,
    #[serde(default)]
    tz: Option<String>,
}

/// The fields read from the model's reply. Anything else it says (a bot, an owner, another
/// approval setting) is ignored: those come from the request and from v1 policy.
#[derive(Debug, Deserialize)]
struct ModelReply {
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    schedule: Option<ModelSchedule>,
    #[serde(default)]
    prompt: Option<String>,
}

/// Strip one Markdown code fence, which models add even when asked not to.
fn unfence(reply: &str) -> &str {
    let t = reply.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// Check the model's reply and build the draft, or the question to ask.
pub fn read_reply(
    reply: &str,
    bot: &str,
    owner: &str,
    default_tz: &str,
    now: DateTime<Utc>,
) -> Parsed {
    let value: Value = match serde_json::from_str(unfence(reply)) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            tracing::warn!("the model's routine reply wasn't a JSON object");
            return Parsed::Question(ASK_AGAIN.into());
        }
    };
    let Ok(reply) = serde_json::from_value::<ModelReply>(value) else {
        tracing::warn!("the model's routine reply had the wrong field types");
        return Parsed::Question(ASK_AGAIN.into());
    };
    if let Some(q) = reply.question.map(|q| q.trim().to_string()) {
        if !q.is_empty() {
            return Parsed::Question(q);
        }
    }
    let Some(cron) = reply
        .schedule
        .as_ref()
        .and_then(|s| s.cron.as_deref())
        .map(str::trim)
        .filter(|c| !c.is_empty())
    else {
        return Parsed::Question(ASK_WHEN.into());
    };
    let tz = reply
        .schedule
        .as_ref()
        .and_then(|s| s.tz.as_deref())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(default_tz);
    let prompt = reply.prompt.as_deref().map(str::trim).unwrap_or_default();
    if prompt.is_empty() {
        return Parsed::Question(ASK_WHAT.into());
    }
    if parse_cron(cron).is_err() {
        return Parsed::Question(ASK_WHEN.into());
    }
    if parse_tz(tz).is_err() {
        return Parsed::Question(ASK_TZ.into());
    }
    let schedule = Schedule {
        cron: cron.split_whitespace().collect::<Vec<_>>().join(" "),
        tz: tz.to_string(),
    };
    if next_run(&schedule, now).is_err() {
        return Parsed::Question(NEVER_RUNS.into());
    }
    Parsed::Draft(Draft {
        bot: bot.to_string(),
        owner: owner.to_string(),
        schedule,
        prompt: prompt.to_string(),
        output: Output::dm(),
        approval: APPROVAL.into(),
        persona: None,
    })
}

// ----- the read-back sentence ----------------------------------------------------------------

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

fn day_name(field: &str) -> Option<&'static str> {
    let n = match field.to_ascii_uppercase().as_str() {
        "0" | "7" | "SUN" => 0,
        "1" | "MON" => 1,
        "2" | "TUE" => 2,
        "3" | "WED" => 3,
        "4" | "THU" => 4,
        "5" | "FRI" => 5,
        "6" | "SAT" => 6,
        _ => return None,
    };
    Some(DAYS[n])
}

fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// "Every weekday", "Every Monday and Thursday", "On day 1 of every month", if the fields
/// are that simple.
fn days_phrase(dom: &str, month: &str, dow: &str) -> Option<String> {
    if month != "*" {
        return None;
    }
    match (dom, dow.to_ascii_uppercase().as_str()) {
        ("*", "*") => Some("Every day".into()),
        ("*", "1-5" | "MON-FRI") => Some("Every weekday".into()),
        ("*", list) => {
            let names: Option<Vec<&str>> = list.split(',').map(day_name).collect();
            names.map(|n| format!("Every {}", and_list(&n)))
        }
        (d, "*") if d.parse::<u8>().is_ok_and(|d| (1..=31).contains(&d)) => {
            Some(format!("On day {d} of every month"))
        }
        _ => None,
    }
}

fn time_phrase(minute: &str, hour: &str) -> Option<String> {
    let m: u8 = minute.parse().ok().filter(|m| *m < 60)?;
    let h: u8 = hour.parse().ok().filter(|h| *h < 24)?;
    Some(format!("{h}:{m:02}"))
}

/// "London time" for `Europe/London`, "UTC" for UTC.
pub fn tz_label(tz: &str) -> String {
    if matches!(tz, "UTC" | "Etc/UTC" | "Etc/GMT" | "GMT") {
        return "UTC".into();
    }
    let place = tz.rsplit('/').next().unwrap_or(tz).replace('_', " ");
    format!("{place} time")
}

/// How the summary names the bot: "Docs bot" for the team bot `docs`, a personal bot by its
/// name.
fn bot_label(bot: &str, personal: bool) -> String {
    if personal {
        return bot.to_string();
    }
    let mut chars = bot.chars();
    match chars.next() {
        Some(first) => format!("{}{} bot", first.to_uppercase(), chars.as_str()),
        None => bot.to_string(),
    }
}

/// The task as a clause after "will": first letter lowercased unless it starts an acronym,
/// no trailing full stop.
fn task_clause(prompt: &str) -> String {
    let p = prompt.trim().trim_end_matches('.');
    let mut chars = p.chars();
    match (chars.next(), chars.clone().next()) {
        (Some(first), Some(second)) if !second.is_uppercase() => {
            format!("{}{}", first.to_lowercase(), chars.as_str())
        }
        _ => p.to_string(),
    }
}

/// The plain sentence Slack reads back for a one-tap confirm: "Every weekday at 8:00 London
/// time, Docs bot will summarize overnight #launch."
pub fn summary(draft: &Draft, personal_bot: bool) -> String {
    let fields: Vec<&str> = draft.schedule.cron.split_whitespace().collect();
    let bot = bot_label(&draft.bot, personal_bot);
    let task = task_clause(&draft.prompt);
    let tz = tz_label(&draft.schedule.tz);
    if let [minute, hour, dom, month, dow] = fields[..] {
        if let (Some(days), Some(time)) = (days_phrase(dom, month, dow), time_phrase(minute, hour))
        {
            return format!("{days} at {time} {tz}, {bot} will {task}.");
        }
    }
    let described = parse_cron(&draft.schedule.cron)
        .map(|c| c.describe())
        .unwrap_or_else(|_| draft.schedule.cron.clone());
    format!(
        "{} ({tz}), {bot} will {task}.",
        described.trim_end_matches('.')
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn sched(cron: &str, tz: &str) -> Schedule {
        Schedule {
            cron: cron.into(),
            tz: tz.into(),
        }
    }

    #[test]
    fn crons_have_exactly_five_valid_fields() {
        assert!(parse_cron("0 8 * * 1-5").is_ok());
        assert!(parse_cron("*/15 9-17 * * MON-FRI").is_ok());
        for bad in [
            "0 8 * *",
            "0 0 8 * * 1-5",
            "@daily",
            "61 8 * * *",
            "",
            "0 8 * * 1-5 2027",
        ] {
            assert!(parse_cron(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn time_zones_are_iana_names() {
        assert!(parse_tz("Europe/London").is_ok());
        assert!(parse_tz("America/New_York").is_ok());
        assert!(parse_tz("London").is_err());
        assert!(parse_tz("europe/london").is_err());
        assert!(parse_tz("+01:00").is_err());
    }

    #[test]
    fn next_run_follows_the_local_clock() {
        // 8:00 London is 7:00 UTC in summer and 8:00 UTC in winter.
        let s = sched("0 8 * * 1-5", "Europe/London");
        // Friday 2026-10-23 09:00 UTC: next is Monday 26th, after the clocks went back.
        assert_eq!(
            next_run(&s, at(2026, 10, 23, 9, 0)).unwrap(),
            at(2026, 10, 26, 8, 0)
        );
        assert_eq!(
            next_run(&s, at(2026, 6, 1, 6, 0)).unwrap(),
            at(2026, 6, 1, 7, 0)
        );
        // Strictly after: a run at exactly the slot moves on to the next one.
        assert_eq!(
            next_run(&s, at(2026, 6, 1, 7, 0)).unwrap(),
            at(2026, 6, 2, 7, 0)
        );
        assert!(next_run(&sched("0 8 30 2 *", "UTC"), at(2026, 1, 1, 0, 0)).is_err());
    }

    #[test]
    fn reads_a_good_reply_and_forces_approve() {
        let now = at(2026, 11, 14, 12, 0);
        let reply = r#"```json
{"schedule": {"cron": "0  8 * * 1-5", "tz": "Europe/London"}, "prompt": "Summarize overnight #launch",
 "output": {"kind": "channel"}, "approval": "auto", "bot": "someone-else", "owner": "eve"}
```"#;
        let Parsed::Draft(d) = read_reply(reply, "docs", "ana", "UTC", now) else {
            panic!("expected a draft");
        };
        assert_eq!(d.schedule, sched("0 8 * * 1-5", "Europe/London"));
        assert_eq!(d.approval, "approve");
        assert_eq!(d.output, Output::dm());
        assert_eq!((d.bot.as_str(), d.owner.as_str()), ("docs", "ana"));
    }

    #[test]
    fn unclear_or_invalid_replies_become_questions() {
        let now = at(2026, 11, 14, 12, 0);
        let q = |reply: &str| match read_reply(reply, "docs", "ana", "Europe/London", now) {
            Parsed::Question(q) => q,
            Parsed::Draft(d) => panic!("expected a question, got {d:?}"),
        };
        assert_eq!(q("not json at all"), ASK_AGAIN);
        assert_eq!(q("[1, 2]"), ASK_AGAIN);
        assert_eq!(q(r#"{"prompt": 5}"#), ASK_AGAIN);
        assert_eq!(q(r#"{"question": "Which channel?"}"#), "Which channel?");
        assert_eq!(q(r#"{"prompt": "Summarize #launch"}"#), ASK_WHEN);
        assert_eq!(
            q(r#"{"schedule": {"cron": "0 8 * *"}, "prompt": "Summarize"}"#),
            ASK_WHEN
        );
        assert_eq!(
            q(r#"{"schedule": {"cron": "0 8 * * *"}, "prompt": " "}"#),
            ASK_WHAT
        );
        assert_eq!(
            q(r#"{"schedule": {"cron": "0 8 * * *", "tz": "London"}, "prompt": "Summarize"}"#),
            ASK_TZ
        );
        assert_eq!(
            q(r#"{"schedule": {"cron": "0 8 30 2 *"}, "prompt": "Summarize"}"#),
            NEVER_RUNS
        );
        // The request's time zone is the default.
        let Parsed::Draft(d) = read_reply(
            r#"{"schedule": {"cron": "0 8 * * *"}, "prompt": "Summarize"}"#,
            "docs",
            "ana",
            "Europe/Paris",
            now,
        ) else {
            panic!("expected a draft");
        };
        assert_eq!(d.schedule.tz, "Europe/Paris");
    }

    fn draft(cron: &str, tz: &str, bot: &str, prompt: &str) -> Draft {
        Draft {
            bot: bot.into(),
            owner: "ana".into(),
            schedule: sched(cron, tz),
            prompt: prompt.into(),
            output: Output::dm(),
            approval: APPROVAL.into(),
            persona: None,
        }
    }

    #[test]
    fn summaries_read_as_a_sentence() {
        assert_eq!(
            summary(
                &draft(
                    "0 8 * * 1-5",
                    "Europe/London",
                    "docs",
                    "Summarize overnight #launch."
                ),
                false
            ),
            "Every weekday at 8:00 London time, Docs bot will summarize overnight #launch."
        );
        assert_eq!(
            summary(
                &draft(
                    "30 9 * * 1,4",
                    "America/New_York",
                    "pm-ana",
                    "PRD review prep"
                ),
                true
            ),
            "Every Monday and Thursday at 9:30 New York time, pm-ana will PRD review prep."
        );
        assert_eq!(
            summary(&draft("0 7 1 * *", "UTC", "docs", "List new pages"), false),
            "On day 1 of every month at 7:00 UTC, Docs bot will list new pages."
        );
        assert_eq!(
            summary(&draft("*/15 * * * *", "UTC", "docs", "Check"), false),
            "At every 15 minutes (UTC), Docs bot will check."
        );
    }
}
