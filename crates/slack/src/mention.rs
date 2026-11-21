//! Tagging PM OS in a channel (`docs/slack-mentions-and-home-spec.md` §2).
//!
//! A PM writes `@PM OS …` in a channel thread. The app's token never reads the channel: it
//! only learns that it was tagged (`app_mention`), and the PM's personal bot reads the thread
//! as the PM (`slack.read_thread`) in a task. The answer is delivered in the PM's DM with
//! PM OS, in a thread that starts with one message saying where the tag was; the channel
//! thread gets one ephemeral line, for the asker only. A drafted reply is a `slack.post_reply`
//! approval like any other, so nothing posts without the PM's click.
//!
//! **Steps:** member check, personal-bot check, Slack-link check, the ask (the app's mention
//! stripped), the DM's root message, the task (`[tagged in Slack] channel=… thread_ts=…
//! message_ts=…` and the ask), then the ephemeral in the thread. Every refusal is one
//! ephemeral in the thread.
//!
//! Thread text is never logged, and the DM message never quotes the thread.

use pmos_contract::model::valid_slack_id;
use pmos_contract::onboarding::personal_bot_name;
use serde_json::Value;

use crate::bots::{plain_text, Bots};
use crate::cards;
use crate::commands::FollowUp;
use crate::member_cards::NOT_A_MEMBER;
use crate::personas::{self, Persona, Speaker};
use crate::runtime::RuntimeError;

/// The marker the `slack-reply` skill recognizes, first on the task's first line.
pub const MARKER: &str = "[tagged in Slack]";
/// The ask when the PM only tagged PM OS.
pub const DEFAULT_ASK: &str = "Draft a reply to this thread.";
/// The longest ask passed on, in characters.
pub const MAX_ASK: usize = 2000;

pub const NO_PERSONAL_BOT: &str =
    "Tagging needs your personal bot. DM me `setup` to finish setting up.";
pub const SLACK_NOT_LINKED: &str =
    "Connect your Slack account first, so I can read this thread as you: DM me `setup`.";
pub const COULD_NOT_START: &str = "I couldn't start on this. Try again in a minute, or DM me.";

/// One `app_mention`, its ids checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mention {
    pub(crate) user: String,
    pub(crate) channel: String,
    /// The tagged message.
    pub(crate) ts: String,
    /// The thread: the event's `thread_ts`, or its `ts` for a top-level message.
    pub(crate) thread_ts: String,
    pub(crate) ask: String,
}

/// A channel id a tag can come from: `^[CG][A-Z0-9]{2,}$` (DMs, `D…`, never).
pub fn channel_ok(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('C' | 'G'))
        && (3..=32).contains(&id.len())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// A message timestamp: `^\d{1,12}\.\d{1,9}$`.
pub fn ts_ok(ts: &str) -> bool {
    ts.split_once('.').is_some_and(|(a, b)| {
        (1..=12).contains(&a.len())
            && (1..=9).contains(&b.len())
            && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())
    })
}

/// The app's DM with a PM: `D` and capital letters and digits.
fn dm_ok(id: &str) -> bool {
    id.starts_with('D')
        && (3..=32).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The ask: the app's own mention (`<@BOT>` or `<@BOT|name>`) taken out, or, when the bot's
/// user id isn't known, the first `<@…>`; then as typed, trimmed and capped. An empty ask is
/// [`DEFAULT_ASK`].
pub fn ask_of(text: &str, bot_user: Option<&str>) -> String {
    let stripped = match bot_user.filter(|u| !u.is_empty()) {
        Some(bot) => strip_mentions_of(text, bot),
        None => strip_first_mention(text),
    };
    let ask = plain_text(&stripped);
    let ask = ask.trim();
    if ask.is_empty() {
        return DEFAULT_ASK.to_string();
    }
    ask.chars()
        .take(MAX_ASK)
        .collect::<String>()
        .trim()
        .to_string()
}

fn strip_mentions_of(text: &str, bot: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("<@") {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let inner = &rest[open + 2..open + close];
        let id = inner.split('|').next().unwrap_or_default();
        out.push_str(&rest[..open]);
        if id != bot {
            out.push_str(&rest[open..=open + close]);
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

fn strip_first_mention(text: &str) -> String {
    match text.find("<@") {
        Some(open) => match text[open..].find('>') {
            Some(close) => format!("{}{}", &text[..open], &text[open + close + 1..]),
            None => text.to_string(),
        },
        None => text.to_string(),
    }
}

/// The task's text: the marker and the three ids, then the ask.
pub fn task_text(channel: &str, thread_ts: &str, message_ts: &str, ask: &str) -> String {
    format!("{MARKER} channel={channel} thread_ts={thread_ts} message_ts={message_ts}\n{ask}")
}

/// The DM's root message: where the tag was, never what the thread says.
pub fn dm_root_text(channel: &str, permalink: Option<&str>) -> String {
    let place = match permalink {
        Some(url) => format!("{} in <#{channel}>", crate::blocks::link(url, "a thread")),
        None => format!("a thread in <#{channel}>"),
    };
    format!(
        "You tagged me in {place}. I'm drafting a reply here. Nothing posts there until you \
         say so."
    )
}

/// The ephemeral in the thread once the task is created.
pub fn on_it_text(dm_channel: &str) -> String {
    format!(
        "On it. The draft will be in <https://slack.com/app_redirect?channel={dm_channel}|our DM>. \
         Nothing posts here until you say so."
    )
}

impl Mention {
    /// The mention in an `events_api` payload, or `None` for one PM OS ignores: from a bot,
    /// with a subtype, in a DM, or with ids that aren't ids.
    pub(crate) fn from_payload(payload: &Value) -> Option<Self> {
        let event = &payload["event"];
        if event.get("subtype").is_some() || event.get("bot_id").is_some() {
            return None;
        }
        let channel = event["channel"].as_str().unwrap_or_default();
        if channel.starts_with('D') {
            return None;
        }
        let user = event["user"].as_str().unwrap_or_default();
        let ts = event["ts"].as_str().unwrap_or_default();
        let thread_ts = event["thread_ts"].as_str().unwrap_or(ts);
        if !channel_ok(channel) || !ts_ok(ts) || !ts_ok(thread_ts) || !valid_slack_id(user) {
            tracing::warn!("app_mention with ids that don't look like Slack's; dropped");
            return None;
        }
        let bot_user = payload["authorizations"][0]["user_id"].as_str();
        Some(Self {
            user: user.to_string(),
            channel: channel.to_string(),
            ts: ts.to_string(),
            thread_ts: thread_ts.to_string(),
            ask: ask_of(event["text"].as_str().unwrap_or_default(), bot_user),
        })
    }
}

impl Bots {
    /// An `app_mention`: checked and deduplicated here, worked on in the follow-up.
    pub(crate) fn mention(&self, payload: &Value) -> Option<FollowUp> {
        let mention = Mention::from_payload(payload)?;
        let fallback = format!("{}:{}", mention.channel, mention.ts);
        if !self.first_event(payload, fallback) {
            return None;
        }
        let this = self.clone();
        Some(Box::pin(async move { this.on_mention(mention).await }))
    }

    /// One ephemeral to the asker in the tagged thread; a failure is logged and ignored.
    async fn tell_thread(&self, m: &Mention, text: &str) {
        self.private(&m.channel, &m.user, Some(&m.thread_ts), text)
            .await;
    }

    async fn on_mention(&self, m: Mention) {
        let who = self.people().identify(&m.user);
        let Some(member) = who.member() else {
            tracing::info!(slack_id = %m.user, who = who.kind(), "tag from someone who may not use PM OS");
            return self
                .tell_thread(&m, crate::people::refusal(&who, NOT_A_MEMBER))
                .await;
        };
        let handle = member.handle.clone();
        let bot = personal_bot_name(&handle);
        let roster = match self.roster().await {
            Ok(roster) => roster,
            Err(e) => {
                tracing::warn!(error = %e, "bot roster unavailable for a tag");
                return self.tell_thread(&m, COULD_NOT_START).await;
            }
        };
        let Some(info) = roster.iter().find(|b| b.name == bot) else {
            tracing::info!(user = handle, "tag without a personal bot");
            return self.tell_thread(&m, NO_PERSONAL_BOT).await;
        };
        let shown = info.display_name();
        match self.toolserver().onboarding(&handle).await {
            Ok(state) if state["linked"]["slack"].as_bool() != Some(true) => {
                tracing::info!(user = handle, "tag without a linked Slack account");
                return self.tell_thread(&m, SLACK_NOT_LINKED).await;
            }
            Ok(_) => {}
            // The tool reports it later, if it is a problem.
            Err(e) => tracing::warn!(error = %e, "onboarding state unavailable for a tag"),
        }
        // `@PM OS Chief: …` asks Chief (`docs/bot-marketplace-spec.md` §9): the prefix is read
        // only from the PM's own tag, after the app's mention.
        let (persona, ask) = match self.persona_prefix(&m.ask).await {
            Some((p, ask)) if ask.is_empty() => (Some(p), DEFAULT_ASK.to_string()),
            Some((p, ask)) => (Some(p), ask),
            None => (None, m.ask.clone()),
        };
        let speaker = persona.as_ref().map(|p| Speaker {
            name: p.shown(),
            preview: None,
        });
        let Some((dm, root)) = self.open_tag_thread(&m, &shown, speaker.as_ref()).await else {
            return self.tell_thread(&m, COULD_NOT_START).await;
        };
        let text = task_text(&m.channel, &m.thread_ts, &m.ts, &ask);
        // Remembered first, so the PM's reply in the DM thread goes to the same bot.
        self.remember_thread(&dm, &root, &bot);
        match self
            .runtime()
            .create_task_as(
                &handle,
                &bot,
                &text,
                &dm,
                &root,
                persona.as_ref().map(|p| p.key.as_str()),
            )
            .await
        {
            Ok(task) => {
                tracing::info!(task = %task["id"], bot, user = handle, "task created from a tag");
                self.tell_thread(&m, &on_it_text(&dm)).await;
            }
            Err(RuntimeError::Persona(code)) => {
                self.forget_thread(&dm, &root);
                tracing::info!(code, user = handle, "persona tag refused by the runtime");
                let name = persona.as_ref().map(Persona::shown).unwrap_or_default();
                self.tell_thread(&m, &personas::refusal(&code, &name)).await;
            }
            Err(e) => {
                self.forget_thread(&dm, &root);
                match &e {
                    RuntimeError::Unavailable => {
                        tracing::warn!("creating a task from a tag failed: runtime unavailable")
                    }
                    _ => tracing::warn!(error = %e, "creating a task from a tag was refused"),
                }
                self.tell_thread(&m, COULD_NOT_START).await;
            }
        }
    }

    /// Open the PM's DM and post the thread's root message there, under the personal bot's
    /// name (starting with the marketplace bot's, when one was asked). Returns the DM channel
    /// and the root message's ts.
    async fn open_tag_thread(
        &self,
        m: &Mention,
        shown: &str,
        speaker: Option<&Speaker>,
    ) -> Option<(String, String)> {
        let dm = match self.slack().open_dm(&m.user).await {
            Ok(dm) if dm_ok(&dm) => dm,
            Ok(_) => {
                tracing::warn!("conversations.open returned a channel that isn't a DM");
                return None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "opening the DM for a tag failed");
                return None;
            }
        };
        let permalink = match self.slack().permalink(&m.channel, &m.ts).await {
            Ok(url) => Some(url),
            Err(e) => {
                tracing::info!(error = %e, "no permalink for the tagged message; naming the channel");
                None
            }
        };
        let mut message = cards::note(&dm_root_text(&m.channel, permalink.as_deref()));
        if let Some(speaker) = speaker {
            message = personas::attribute(message, speaker);
        }
        let root = self.post(&dm, None, Some(shown), message).await?;
        if !dm_ok(&root.channel) || !ts_ok(&root.ts) {
            tracing::warn!("the DM root message came back without usable ids");
            return None;
        }
        Some((root.channel, root.ts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_are_checked() {
        assert!(channel_ok("C0LAUNCH") && channel_ok("G01"));
        assert!(!channel_ok("D0ANA") && !channel_ok("C1") && !channel_ok("c0x"));
        assert!(!channel_ok("C0<!here>"));
        assert!(ts_ok("1731500000.000100") && ts_ok("1.2"));
        assert!(!ts_ok("1731500000") && !ts_ok("1.0000000001") && !ts_ok("../x.1"));
        assert!(!ts_ok("1234567890123.1"));
    }

    #[test]
    fn the_ask_loses_the_apps_mention() {
        assert_eq!(
            ask_of(
                "<@UPMOS> does Acme Vault support SAML &amp; SSO?",
                Some("UPMOS")
            ),
            "does Acme Vault support SAML & SSO?"
        );
        assert_eq!(
            ask_of("ask <@U0BOB> and <@UPMOS|pm-os> too", Some("UPMOS")),
            "ask <@U0BOB> and  too"
        );
        assert_eq!(ask_of("<@UX> hi <@U0BOB>", None), "hi <@U0BOB>");
        assert_eq!(ask_of("<@UPMOS>  ", Some("UPMOS")), DEFAULT_ASK);
        assert_eq!(
            ask_of(&format!("<@UPMOS> {}", "é".repeat(3000)), Some("UPMOS"))
                .chars()
                .count(),
            MAX_ASK
        );
    }

    #[test]
    fn mentions_from_payloads() {
        let payload =
            |event: Value| json!({"authorizations": [{"user_id": "UPMOS"}], "event": event});
        let m = Mention::from_payload(&payload(json!({
            "type": "app_mention", "user": "U0ANA", "channel": "C0LAUNCH",
            "ts": "1731500000.000200", "thread_ts": "1731500000.000100", "text": "<@UPMOS> x",
        })))
        .unwrap();
        assert_eq!(m.thread_ts, "1731500000.000100");
        assert_eq!(m.ask, "x");
        let top = Mention::from_payload(&payload(json!({
            "type": "app_mention", "user": "U0ANA", "channel": "C0LAUNCH",
            "ts": "1731500000.000200", "text": "<@UPMOS>",
        })))
        .unwrap();
        assert_eq!(top.thread_ts, "1731500000.000200");
        for bad in [
            json!({"user": "U0ANA", "channel": "D0ANA", "ts": "1.2"}),
            json!({"user": "U0ANA", "channel": "C0X", "ts": "1.2", "bot_id": "B1"}),
            json!({"user": "U0ANA", "channel": "C0X", "ts": "1.2", "subtype": "x"}),
            json!({"user": "U0ANA", "channel": "C0X", "ts": "nope"}),
            json!({"user": "U0ANA", "channel": "C0X", "ts": "1.2", "thread_ts": "a.b"}),
        ] {
            assert!(
                Mention::from_payload(&payload(bad.clone())).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn messages() {
        assert_eq!(
            task_text("C0L", "1.1", "1.2", "x"),
            "[tagged in Slack] channel=C0L thread_ts=1.1 message_ts=1.2\nx"
        );
        assert_eq!(
            dm_root_text("C0L", Some("https://acme.slack.com/archives/C0L/p12")),
            "You tagged me in <https://acme.slack.com/archives/C0L/p12|a thread> in <#C0L>. \
             I'm drafting a reply here. Nothing posts there until you say so."
        );
        assert!(dm_root_text("C0L", None).starts_with("You tagged me in a thread in <#C0L>."));
        assert!(
            on_it_text("D0ANA").contains("<https://slack.com/app_redirect?channel=D0ANA|our DM>")
        );
    }
}
