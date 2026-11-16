//! The daily brief's feed to the web app (`docs/daily-brief.md`, "The web app's panels").
//!
//! The `daily-brief` skill ends each brief with one fenced block, ` ```pmos-brief `, holding
//! the same items as JSON. When the brief comes back in the notification feed, [`split`]
//! takes the block out of the text (the PM never sees it in Slack) and checks it; a
//! [`Producer`] then sends it to pmos-web's `PUT /internal/brief/{handle}` so the home page's
//! "What changed", "Waiting on you" and "You're waiting on" panels show the same day.
//!
//! **The block**, version 1 (every key but `version` optional; no other keys):
//!
//! ~~~text
//! ```pmos-brief
//! {"version": 1, "date": "2026-11-16",
//!  "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked",
//!               "summary": "waiting on the vendor sandbox", "url": "https://…"}],
//!  "waiting_on_you": [{"system": "slack", "who": "Ben", "summary": "asked about SAML SSO",
//!                      "url": "https://…", "asked_on": "2026-11-14"}],
//!  "you_wait_on": [{"system": "slack", "who": "Legal", "summary": "your DPA question",
//!                   "url": "https://…", "asked_on": "2026-11-13"}]}
//! ```
//! ~~~
//!
//! **Strict.** The block is all or nothing: bad JSON, an unknown key, a second block, a block
//! with no closing fence, more than [`MAX_ITEMS`] in a list, text over its limit or over one
//! line, Slack markup (`<@…>`, `<#…>`, `<!…>`), a `system` outside `a-z`, digits, `-` and `_`,
//! a url that is not `http`/`https` (or has credentials), or a body over [`MAX_BODY_BYTES`]
//! means nothing is sent. The block is still removed from what the PM sees, and the brief is
//! delivered as usual. The limits are pmos-web's own (`crates/web/src/followups.rs`), so a
//! payload that passes here is not refused there.
//!
//! **Privacy.** `summary` is a one-line paraphrase the skill wrote, never a quote of a Slack
//! message; pmos-web keeps Slack follow-ups for 24 hours and others for a week. Logs carry
//! the handle and counts, never the text, a url or the token.

use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

/// The fence's info string: the block opens with ` ```pmos-brief ` on a line of its own.
pub const FENCE: &str = "pmos-brief";
/// The block format this side reads.
pub const VERSION: u64 = 1;
/// pmos-web's limits (`crates/web/src/followups.rs`, `app/brief.rs`).
pub const MAX_ITEMS: usize = 50;
pub const MAX_SYSTEM: usize = 32;
pub const MAX_WHO: usize = 80;
/// For `title` and `summary`.
pub const MAX_TEXT: usize = 200;
pub const MAX_URL: usize = 2_000;
pub const MAX_BODY_BYTES: usize = 128 * 1024;

/// What `PUT /internal/brief/{handle}` takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Payload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub changed: Vec<Changed>,
    pub waiting_on_you: Vec<FollowUp>,
    pub you_wait_on: Vec<FollowUp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Changed {
    pub system: String,
    pub title: String,
    pub summary: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FollowUp {
    pub system: String,
    pub who: String,
    pub summary: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl Payload {
    /// `(changed, waiting_on_you, you_wait_on)`, for the logs.
    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.changed.len(),
            self.waiting_on_you.len(),
            self.you_wait_on.len(),
        )
    }
}

// What the block may hold, exactly.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockIn {
    version: u64,
    date: Option<String>,
    #[serde(default)]
    changed: Vec<ChangedIn>,
    #[serde(default)]
    waiting_on_you: Vec<FollowIn>,
    #[serde(default)]
    you_wait_on: Vec<FollowIn>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangedIn {
    system: String,
    title: String,
    #[serde(default)]
    summary: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowIn {
    system: String,
    who: String,
    summary: String,
    url: String,
    asked_on: Option<String>,
}

/// A brief's text with the block taken out, and what the block held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// What the PM sees.
    pub text: String,
    /// `None`: there was no block. `Some(Err(why))`: there was one and it is not sent; `why`
    /// names the rule it broke, never its content.
    pub block: Option<Result<Payload, String>>,
}

fn opens(line: &str) -> bool {
    line.trim()
        .strip_prefix("```")
        .is_some_and(|rest| rest.trim() == FENCE)
}

fn closes(line: &str) -> bool {
    line.trim() == "```"
}

/// Take every ` ```pmos-brief ` block out of `text` and read it. Text without one comes back
/// unchanged. `now` dates `asked_on` and refuses one in the future.
pub fn split(text: &str, now: DateTime<Utc>) -> Split {
    if !text.lines().any(opens) {
        return Split {
            text: text.to_string(),
            block: None,
        };
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    let mut skip_blank = false;
    for line in text.split('\n') {
        match current.as_mut() {
            Some(body) if closes(line) => {
                blocks.push(body.join("\n"));
                current = None;
                // A blank line on both sides of the block becomes one.
                skip_blank = kept.last().is_none_or(|l| l.trim().is_empty());
            }
            Some(body) => body.push(line),
            None if opens(line) => current = Some(Vec::new()),
            None if skip_blank && line.trim().is_empty() => skip_blank = false,
            None => {
                skip_blank = false;
                kept.push(line);
            }
        }
    }
    let shown = kept.join("\n").trim_end().to_string();
    let block = if current.is_some() {
        Err("the block has no closing fence".to_string())
    } else if blocks.len() > 1 {
        Err(format!("{} blocks, not one", blocks.len()))
    } else {
        parse(&blocks[0], now)
    };
    Split {
        text: shown,
        block: Some(block),
    }
}

/// Read and check one block's JSON.
pub fn parse(json: &str, now: DateTime<Utc>) -> Result<Payload, String> {
    // serde's message can quote the value it choked on; say only where and what kind.
    let block: BlockIn = serde_json::from_str(json).map_err(|e| {
        format!(
            "not a valid block ({:?} error at line {}, column {})",
            e.classify(),
            e.line(),
            e.column()
        )
    })?;
    if block.version != VERSION {
        return Err(format!("version {} is not {VERSION}", block.version));
    }
    for (name, n) in [
        ("changed", block.changed.len()),
        ("waiting_on_you", block.waiting_on_you.len()),
        ("you_wait_on", block.you_wait_on.len()),
    ] {
        if n > MAX_ITEMS {
            return Err(format!("{name}: {n} items, at most {MAX_ITEMS}"));
        }
    }
    let date = match block.date {
        None => None,
        Some(d) => Some(
            NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d")
                .map_err(|_| "date: not YYYY-MM-DD".to_string())?
                .to_string(),
        ),
    };
    let changed = block
        .changed
        .iter()
        .map(|c| {
            Ok(Changed {
                system: system("changed.system", &c.system)?,
                title: line("changed.title", &c.title, MAX_TEXT, false)?,
                summary: line("changed.summary", &c.summary, MAX_TEXT, true)?,
                url: url("changed.url", &c.url)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let follow = |name: &str, list: &[FollowIn]| -> Result<Vec<FollowUp>, String> {
        list.iter()
            .map(|f| {
                Ok(FollowUp {
                    system: system(&format!("{name}.system"), &f.system)?,
                    who: line(&format!("{name}.who"), &f.who, MAX_WHO, false)?,
                    summary: line(&format!("{name}.summary"), &f.summary, MAX_TEXT, false)?,
                    url: url(&format!("{name}.url"), &f.url)?,
                    created_at: asked_on(&format!("{name}.asked_on"), f.asked_on.as_deref(), now)?,
                })
            })
            .collect()
    };
    let payload = Payload {
        date,
        changed,
        waiting_on_you: follow("waiting_on_you", &block.waiting_on_you)?,
        you_wait_on: follow("you_wait_on", &block.you_wait_on)?,
    };
    let size = serde_json::to_vec(&payload).map(|b| b.len()).unwrap_or(0);
    if size > MAX_BODY_BYTES {
        return Err(format!("{size} bytes, at most {MAX_BODY_BYTES}"));
    }
    Ok(payload)
}

/// One line of plain text: trimmed, not empty (unless `allow_empty`), at most `max`
/// characters, no control characters (so no line breaks) and no Slack markup.
fn line(field: &str, text: &str, max: usize, allow_empty: bool) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() && !allow_empty {
        return Err(format!("{field}: empty"));
    }
    if text.chars().count() > max {
        return Err(format!("{field}: over {max} characters"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("{field}: not one line of plain text"));
    }
    if ["<@", "<#", "<!"].iter().any(|m| text.contains(m)) {
        return Err(format!("{field}: Slack markup, so likely message text"));
    }
    Ok(text.to_string())
}

fn system(field: &str, text: &str) -> Result<String, String> {
    let text = line(field, text, MAX_SYSTEM, false)?;
    let mut chars = text.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if ok {
        Ok(text)
    } else {
        Err(format!("{field}: not lower-case letters, digits, - and _"))
    }
}

fn url(field: &str, text: &str) -> Result<String, String> {
    let text = line(field, text, MAX_URL, false)?;
    let parsed = reqwest::Url::parse(&text).map_err(|_| format!("{field}: not a url"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || text.chars().any(char::is_whitespace)
    {
        return Err(format!(
            "{field}: not an http or https url without credentials"
        ));
    }
    Ok(text)
}

/// `asked_on` (`YYYY-MM-DD`) as the follow-up's `created_at`: that day's start in UTC, or now
/// if that is later (a PM ahead of UTC can be on tomorrow's date). More than a day ahead of
/// `now` is refused.
fn asked_on(field: &str, day: Option<&str>, now: DateTime<Utc>) -> Result<Option<String>, String> {
    let Some(day) = day else {
        return Ok(None);
    };
    let day = NaiveDate::parse_from_str(day.trim(), "%Y-%m-%d")
        .map_err(|_| format!("{field}: not YYYY-MM-DD"))?;
    if day > now.date_naive() + chrono::Days::new(1) {
        return Err(format!("{field}: in the future"));
    }
    let start = day.and_time(NaiveTime::MIN).and_utc().min(now);
    Ok(Some(
        start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    ))
}

/// Why a send failed. Never holds the token.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("pmos-web answered {0}")]
    Status(u16),
    #[error("pmos-web unreachable: {0}")]
    Unreachable(String),
}

/// Sends a PM's brief to pmos-web. Off (`None` from [`Producer::from_lookup`]) unless both
/// `PMOS_WEB_URL` and `PMOS_WEB_BRIEF_TOKEN` are set.
#[derive(Clone)]
pub struct Producer {
    base: reqwest::Url,
    token: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Producer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Producer")
            .field("base", &self.base.as_str())
            .field("token", &"<redacted>")
            .finish()
    }
}

impl Producer {
    /// `base` is pmos-web's base URL (`http://127.0.0.1:8770`): `http` or `https`, no
    /// credentials, query or fragment. Each send also has its own 10 s timeout.
    pub fn new(base: &str, token: &str, http: reqwest::Client) -> Result<Self, String> {
        let bad = || {
            "PMOS_WEB_URL must be pmos-web's http:// or https:// base URL, such as http://127.0.0.1:8770".to_string()
        };
        let mut parsed = reqwest::Url::parse(base.trim()).map_err(|_| bad())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(bad());
        }
        if !parsed.path().ends_with('/') {
            let path = format!("{}/", parsed.path());
            parsed.set_path(&path);
        }
        let token = token.trim();
        if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("PMOS_WEB_BRIEF_TOKEN must be one word".into());
        }
        Ok(Self {
            base: parsed,
            token: token.to_string(),
            http,
        })
    }

    /// From `PMOS_WEB_URL` and `PMOS_WEB_BRIEF_TOKEN`: `Ok(None)` (off) unless both are set.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
        http: reqwest::Client,
    ) -> Result<Option<Self>, String> {
        let set = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        match (set("PMOS_WEB_URL"), set("PMOS_WEB_BRIEF_TOKEN")) {
            (Some(url), Some(token)) => Self::new(&url, &token, http).map(Some),
            _ => Ok(None),
        }
    }

    /// pmos-web's base URL, for the start-up log.
    pub fn base(&self) -> &str {
        self.base.as_str()
    }

    /// `PUT /internal/brief/{handle}` with the payload.
    pub async fn send(&self, handle: &str, payload: &Payload) -> Result<(), SendError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|_| SendError::Unreachable("bad base url".into()))?
            .pop_if_empty()
            .extend(["internal", "brief", handle]);
        let res = self
            .http
            .put(url)
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(10))
            .json(payload)
            .send()
            .await
            .map_err(|e| SendError::Unreachable(e.without_url().to_string()))?;
        match res.status() {
            s if s.is_success() => Ok(()),
            s => Err(SendError::Status(s.as_u16())),
        }
    }

    /// [`Self::send`], logged: the handle and counts on success, the reason at warn on a
    /// failure. Never returns an error, so a brief is never held up by pmos-web.
    pub async fn deliver(&self, handle: &str, payload: &Payload) {
        let (changed, waiting_on_you, you_wait_on) = payload.counts();
        match self.send(handle, payload).await {
            Ok(()) => tracing::info!(
                user = handle,
                changed,
                waiting_on_you,
                you_wait_on,
                "brief sent to the web app"
            ),
            Err(e) => tracing::warn!(
                user = handle,
                error = %e,
                "sending the brief to the web app failed; the Slack brief is unaffected"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        "2026-11-16T09:00:00Z".parse().unwrap()
    }

    fn fenced(json: &str) -> String {
        format!("```pmos-brief\n{json}\n```")
    }

    const BRIEF: &str = "Morning, Ana. One thing moved.\n\n**What changed**\n- PAY-212 moved to Blocked. ([Jira](https://jira.example.com/browse/PAY-212))";

    fn valid() -> serde_json::Value {
        json!({
            "version": 1, "date": "2026-11-16",
            "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked",
                         "summary": "waiting on the vendor sandbox",
                         "url": "https://jira.example.com/browse/PAY-212"}],
            "waiting_on_you": [{"system": "slack", "who": "Ben",
                                "summary": "asked about SAML SSO for the console",
                                "url": "https://slack.com/archives/C0PM/p1731600000000100",
                                "asked_on": "2026-11-14"}],
            "you_wait_on": [{"system": "confluence", "who": "Legal",
                             "summary": "your question about the DPA",
                             "url": "https://wiki.example.com/pages/9001"}],
        })
    }

    fn with(edit: impl FnOnce(&mut serde_json::Value)) -> Result<Payload, String> {
        let mut v = valid();
        edit(&mut v);
        parse(&v.to_string(), now())
    }

    #[test]
    fn a_valid_block_is_taken_out_and_read() {
        let text = format!("{BRIEF}\n\n{}\n", fenced(&valid().to_string()));
        let s = split(&text, now());
        assert_eq!(s.text, BRIEF);
        let p = s.block.unwrap().unwrap();
        assert_eq!(p.date.as_deref(), Some("2026-11-16"));
        assert_eq!(p.counts(), (1, 1, 1));
        assert_eq!(p.changed[0].system, "jira");
        assert_eq!(
            p.waiting_on_you[0].created_at.as_deref(),
            Some("2026-11-14T00:00:00Z")
        );
        assert_eq!(p.you_wait_on[0].created_at, None);
        // The body is exactly the route's shape.
        let body = serde_json::to_value(&p).unwrap();
        assert_eq!(
            body["waiting_on_you"][0],
            json!({"system": "slack", "who": "Ben",
                   "summary": "asked about SAML SSO for the console",
                   "url": "https://slack.com/archives/C0PM/p1731600000000100",
                   "created_at": "2026-11-14T00:00:00Z"})
        );
        assert!(body["you_wait_on"][0].get("created_at").is_none());
    }

    #[test]
    fn no_block_leaves_the_text_alone() {
        let s = split(BRIEF, now());
        assert_eq!(s.text, BRIEF);
        assert_eq!(s.block, None);
        // Other code blocks are not the block.
        let other = "Here:\n```json\n{\"version\": 1}\n```";
        assert_eq!(split(other, now()).block, None);
        assert_eq!(split(other, now()).text, other);
    }

    #[test]
    fn a_quiet_day_block_is_empty_lists() {
        let text = format!(
            "Nothing moved overnight. Enjoy it while it lasts.\n\n{}",
            fenced(r#"{"version": 1}"#)
        );
        let s = split(&text, now());
        assert_eq!(s.text, "Nothing moved overnight. Enjoy it while it lasts.");
        assert_eq!(s.block.unwrap().unwrap().counts(), (0, 0, 0));
    }

    #[test]
    fn malformed_json_is_removed_and_not_sent() {
        for bad in [
            "{not json",
            "",
            "[]",
            r#"{"version": 1, "changed": {}}"#,
            r#"{"version": 2}"#,
            r#"{"changed": []}"#,
            r#"{"version": 1, "extra": true}"#,
            r#"{"version": 1, "changed": [{"system": "jira", "title": "x", "url": "https://a.b", "note": "?"}]}"#,
            r#"{"version": 1, "waiting_on_you": [{"system": "slack", "who": "Ben", "url": "https://a.b"}]}"#,
        ] {
            let s = split(&format!("{BRIEF}\n\n{}", fenced(bad)), now());
            assert_eq!(s.text, BRIEF, "{bad}");
            assert!(matches!(s.block, Some(Err(_))), "{bad}");
        }
        // The reason never quotes the content.
        let why = parse(r#"{"version": 1, "changed": "secret words"}"#, now()).unwrap_err();
        assert!(!why.contains("secret"), "{why}");
    }

    #[test]
    fn an_unclosed_or_second_block_is_refused_and_still_hidden() {
        let s = split(
            &format!("{BRIEF}\n\n```pmos-brief\n{{\"version\": 1}}"),
            now(),
        );
        assert_eq!(s.text, BRIEF);
        assert!(matches!(s.block, Some(Err(ref e)) if e.contains("closing")));
        let one = fenced(r#"{"version": 1}"#);
        let s = split(&format!("{BRIEF}\n\n{one}\n\n{one}"), now());
        assert_eq!(s.text, BRIEF);
        assert!(matches!(s.block, Some(Err(ref e)) if e.contains("2 blocks")));
    }

    #[test]
    fn a_block_in_the_middle_is_taken_out_cleanly() {
        let text = format!(
            "Morning.\n\n{}\n\nWant a reply drafted? Send \"draft a reply\".",
            fenced(r#"{"version": 1}"#)
        );
        let s = split(&text, now());
        assert_eq!(
            s.text,
            "Morning.\n\nWant a reply drafted? Send \"draft a reply\"."
        );
        assert!(matches!(s.block, Some(Ok(_))));
        // Indented fences count too.
        let s = split("Hi\n  ```pmos-brief\n  {\"version\": 1}\n  ```", now());
        assert_eq!(s.text, "Hi");
        assert!(matches!(s.block, Some(Ok(_))));
    }

    #[test]
    fn oversize_lists_are_refused() {
        let item = valid()["changed"][0].clone();
        assert!(with(|v| v["changed"] = json!(vec![item.clone(); MAX_ITEMS])).is_ok());
        let e = with(|v| v["changed"] = json!(vec![item; MAX_ITEMS + 1])).unwrap_err();
        assert!(e.contains("changed") && e.contains("51"), "{e}");
        let f = valid()["waiting_on_you"][0].clone();
        assert!(with(|v| v["you_wait_on"] = json!(vec![f; MAX_ITEMS + 1])).is_err());
    }

    #[test]
    fn a_large_body_is_refused() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL - 20));
        let item = json!({"system": "jira", "title": "t", "url": long});
        let follow = json!({"system": "jira", "who": "w", "summary": "s", "url": long});
        assert!(with(|v| v["changed"] = json!(vec![item.clone(); MAX_ITEMS])).is_ok());
        let e = with(|v| {
            v["changed"] = json!(vec![item; MAX_ITEMS]);
            v["you_wait_on"] = json!(vec![follow; MAX_ITEMS]);
        })
        .unwrap_err();
        assert!(e.contains("bytes"), "{e}");
    }

    #[test]
    fn bad_urls_are_refused() {
        for bad in [
            "javascript:alert(1)",
            "ftp://example.com/x",
            "https://user:pw@example.com/x",
            "https://exa mple.com",
            "/relative/path",
            "mailto:a@example.com",
            "",
        ] {
            assert!(
                with(|v| v["waiting_on_you"][0]["url"] = json!(bad)).is_err(),
                "{bad}"
            );
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL));
        assert!(with(|v| v["changed"][0]["url"] = json!(long)).is_err());
        assert!(with(|v| v["changed"][0]["url"] = json!("http://example.com/x")).is_ok());
    }

    #[test]
    fn text_must_be_one_short_line_with_no_slack_markup() {
        assert!(with(|v| v["waiting_on_you"][0]["summary"] = json!("asked about\nSSO")).is_err());
        assert!(with(|v| v["changed"][0]["title"] = json!("a\tb")).is_err());
        assert!(with(|v| v["waiting_on_you"][0]["summary"] = json!("  ")).is_err());
        assert!(with(|v| v["waiting_on_you"][0]["summary"] = json!("x".repeat(201))).is_err());
        assert!(with(|v| v["waiting_on_you"][0]["summary"] = json!("é".repeat(200))).is_ok());
        assert!(with(|v| v["you_wait_on"][0]["who"] = json!("w".repeat(81))).is_err());
        assert!(
            with(|v| v["waiting_on_you"][0]["summary"] = json!("<@U0ANA> can you check")).is_err()
        );
        assert!(with(|v| v["changed"][0]["summary"] = json!("ping <!here>")).is_err());
        // A `changed` summary may be left out.
        assert!(with(|v| {
            v["changed"][0].as_object_mut().unwrap().remove("summary");
        })
        .is_ok());
        // Trimmed.
        let p = with(|v| v["changed"][0]["title"] = json!("  PAY-212 moved  ")).unwrap();
        assert_eq!(p.changed[0].title, "PAY-212 moved");
    }

    #[test]
    fn systems_and_dates() {
        for bad in ["Slack", "1jira", "", "jira!", "a b", &"x".repeat(33)] {
            assert!(
                with(|v| v["changed"][0]["system"] = json!(bad)).is_err(),
                "{bad}"
            );
        }
        assert!(with(|v| v["changed"][0]["system"] = json!("google-docs_2")).is_ok());
        assert!(with(|v| v["date"] = json!("16/11/2026")).is_err());
        assert!(with(|v| v["waiting_on_you"][0]["asked_on"] = json!("yesterday")).is_err());
        assert!(with(|v| v["waiting_on_you"][0]["asked_on"] = json!("2026-11-20")).is_err());
        // Tomorrow (a PM ahead of UTC) is now, never in the future.
        let p = with(|v| v["waiting_on_you"][0]["asked_on"] = json!("2026-11-17")).unwrap();
        assert_eq!(
            p.waiting_on_you[0].created_at.as_deref(),
            Some("2026-11-16T09:00:00Z")
        );
    }

    #[test]
    fn the_producer_needs_both_settings_and_a_good_url() {
        let http = reqwest::Client::new();
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(Producer::from_lookup(env(&[]), http.clone())
            .unwrap()
            .is_none());
        assert!(Producer::from_lookup(
            env(&[("PMOS_WEB_URL", "http://127.0.0.1:8770")]),
            http.clone()
        )
        .unwrap()
        .is_none());
        assert!(Producer::from_lookup(
            env(&[("PMOS_WEB_BRIEF_TOKEN", "a-token-of-thirty-two-characters")]),
            http.clone()
        )
        .unwrap()
        .is_none());
        let p = Producer::from_lookup(
            env(&[
                ("PMOS_WEB_URL", "http://127.0.0.1:8770"),
                ("PMOS_WEB_BRIEF_TOKEN", "secret-token-value"),
            ]),
            http.clone(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.base(), "http://127.0.0.1:8770/");
        assert!(!format!("{p:?}").contains("secret-token-value"));
        for bad in ["127.0.0.1:8770", "ftp://x", "http://u:p@x", "http://x/?a=1"] {
            assert!(Producer::new(bad, "tok", http.clone()).is_err(), "{bad}");
        }
    }
}
