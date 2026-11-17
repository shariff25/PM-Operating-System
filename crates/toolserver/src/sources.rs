//! What a draft was built from, and links to where it will go, worked out from metadata the
//! tool server already keeps: the audit log's `sources` (context paths and `system:id`
//! references) and `source_titles` (a short title per item, recorded at the read), and an
//! approval item's target arguments. Nothing here stores or returns message text, page
//! bodies or snippets, and nothing makes a network call.
//!
//! | `sources` entry | `system` | `title` | `url` |
//! | --- | --- | --- | --- |
//! | `context/…/x.md` (context tools) | `context` | the file's title in the context index, else the recorded one | none (an internal file) |
//! | `jira:PAY-212` | `jira` | "PAY-212: {recorded summary}", else the issue key | `{site}/browse/PAY-212` |
//! | `jira:rollup:status` | `jira` | "Jira rollup by status" | none |
//! | `confluence:12345` | `confluence` | the recorded page title, else "Confluence page 12345" | `{site}/wiki/pages/viewpage.action?pageId=12345` |
//! | `slack:C01:1731.0001` | `slack` | "Slack thread in C01" (`read_thread`) or "Slack message in C01"; never a recorded title | `https://slack.com/archives/C01/p17310001` |
//! | `docs:https://…/page.md` | `docs` | the recorded page title, else the page's slug ("page") | the docs URL itself |
//!
//! Events recorded before titles were have none, so their sources read as ids, as before.
//!
//! Each source also has a **`cite`** ref, [`cite_ref`] of its `sources` entry, such as
//! `src-3f9a1c`. Read tools give every item they return the same ref (`ref` in the result),
//! so a bot can mark a sentence of its draft with the source it came from (`[[src-3f9a1c]]`,
//! `crates/contract/src/cite.rs`), and the ref is matched here to what the run really read.
//! It is a hash of ids only, the same for an item in every task, and needs nothing stored.
//!
//! `{site}` is the PM's linked Atlassian site, read from the stored credential (never the
//! token); with no linked site, Jira and Confluence sources have no `url`. A reference whose
//! shape isn't recognised is left out rather than guessed at.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::audit::source_title;
use crate::connectors::slack::archive_url;

/// The most sources one approval item lists.
pub const MAX_SOURCES: usize = 20;
/// The most audit events read for one item.
pub const MAX_READ_EVENTS: usize = 500;

/// The hex characters of a ref after `src-`.
pub const CITE_REF_HEX: usize = 6;

/// The ref a draft's source marker names an item by: `src-` and the first
/// [`CITE_REF_HEX`] hex characters of the SHA-256 of its audit `sources` entry
/// (`jira:PAY-212`, `context/product/x.md`). Only ids go in, so it carries no text.
pub fn cite_ref(entry: &str) -> String {
    let digest = Sha256::digest(entry.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}{}",
        pmos_contract::cite::REF_PREFIX,
        &hex[..CITE_REF_HEX]
    )
}

/// One thing the drafting run read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalSource {
    /// `context`, `jira`, `confluence`, `slack` or `docs`.
    pub system: String,
    pub title: String,
    pub url: Option<String>,
    /// The identifier in its system: a context path, an issue key, a page id, `channel:ts`,
    /// a docs URL.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The ref the bot was given for it ([`cite_ref`]), which its draft's markers name.
    pub cite: String,
    /// The read tool, such as `jira.read`.
    pub tool: String,
    /// The latest read of it in the run.
    pub read_at: DateTime<Utc>,
}

/// `GET /internal/approvals/{id}/sources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourcesReport {
    /// Newest read first, each source once, at most [`MAX_SOURCES`].
    pub sources: Vec<ApprovalSource>,
    /// False when the audit log can't be read back (only a JSON-lines file is configured):
    /// `sources` is then empty because nothing could be looked up, not because nothing was
    /// read.
    pub available: bool,
    /// More sources were read than are listed.
    pub truncated: bool,
}

/// Letters, digits and `extra`, 1 to `max` characters.
fn id_like(s: &str, extra: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// A Jira issue key, such as `PAY-212`.
pub fn jira_key(s: &str) -> bool {
    match s.split_once('-') {
        Some((project, n)) => {
            jira_project(project) && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// A Jira project key, such as `PAY`.
pub fn jira_project(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && id_like(s, "_", 32)
        && !s.chars().any(|c| c.is_ascii_lowercase())
}

/// A Confluence page id: digits.
pub fn page_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_digit())
}

/// A Confluence space key, such as `PM` or `~ana`.
pub fn space_key(s: &str) -> bool {
    id_like(s.strip_prefix('~').unwrap_or(s), "_", 64)
}

/// A Slack conversation id, such as `C01` or `D0EVALSBEN`.
pub fn slack_channel(s: &str) -> bool {
    id_like(s, "", 24) && !s.chars().any(|c| c.is_ascii_lowercase())
}

/// A Slack message timestamp, such as `1731600000.000100`.
pub fn slack_ts(s: &str) -> bool {
    match s.split_once('.') {
        Some((a, b)) => {
            !a.is_empty()
                && !b.is_empty()
                && a.len() + b.len() <= 24
                && (a.chars().chain(b.chars())).all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// An `https` URL with nothing that could break out of a link.
fn safe_https(s: &str) -> bool {
    s.starts_with("https://")
        && s.len() > "https://".len()
        && s.len() <= 2048
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\\'))
}

/// An Atlassian site's own URL, from what its credential stores: `site_url`, or else the
/// `{cloud_id}.atlassian.net` the connector also falls back to.
pub fn site_url(site_url: Option<&str>, cloud_id: Option<&str>) -> Option<String> {
    let url = site_url
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| safe_https(s) && !s["https://".len()..].contains('/'))
        .or_else(|| {
            cloud_id
                .filter(|c| id_like(c, "-", 64))
                .map(|c| format!("https://{c}.atlassian.net"))
        })?;
    Some(url)
}

/// What a connector read's `system:id` audit entry says, or `None` for a shape this doesn't
/// recognise. `recorded` is the title the audit event recorded for it, if any; it is cleaned
/// again here and never used for Slack. Returns `(ref, title, url)`.
pub fn describe(
    system: &str,
    action: &str,
    id: &str,
    site: Option<&str>,
    recorded: Option<&str>,
) -> Option<(String, String, Option<String>)> {
    let recorded = recorded.and_then(source_title);
    match system {
        "jira" => {
            if let Some(group) = id.strip_prefix("rollup:") {
                return id_like(group, "_", 32)
                    .then(|| (id.to_string(), format!("Jira rollup by {group}"), None));
            }
            jira_key(id).then(|| {
                let url = site.map(|s| format!("{s}/browse/{id}"));
                let title = match recorded {
                    Some(summary) => format!("{id}: {summary}"),
                    None => id.to_string(),
                };
                (id.to_string(), title, url)
            })
        }
        "confluence" => page_id(id).then(|| {
            let url = site.map(|s| format!("{s}/wiki/pages/viewpage.action?pageId={id}"));
            let title = recorded.unwrap_or_else(|| format!("Confluence page {id}"));
            (id.to_string(), title, url)
        }),
        "slack" => {
            let (channel, ts) = id.split_once(':')?;
            if !(slack_channel(channel) && slack_ts(ts)) {
                return None;
            }
            let what = if action == "read_thread" {
                "thread"
            } else {
                "message"
            };
            Some((
                id.to_string(),
                format!("Slack {what} in {channel}"),
                Some(archive_url(channel, ts)),
            ))
        }
        "docs" => {
            if !safe_https(id) {
                return None;
            }
            let slug = id
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default();
            let slug = slug.strip_suffix(".md").unwrap_or(slug);
            let title = slug.replace(['-', '_'], " ");
            let title = match recorded {
                Some(t) => t,
                None if title.trim().is_empty() => "Docs page".to_string(),
                None => title,
            };
            Some((id.to_string(), title, Some(id.to_string())))
        }
        _ => None,
    }
}

/// A link to where an approval item's write goes, when one can be built from its target
/// arguments (ids only, each checked) and the PM's Atlassian site, with no network call.
pub fn target_url(tool: &str, args: &Value, site: Option<&str>) -> Option<String> {
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default();
    match tool {
        "confluence.create_draft" => {
            let site = site?;
            let parent = arg("parent_id");
            if page_id(parent) {
                return Some(format!("{site}/wiki/pages/viewpage.action?pageId={parent}"));
            }
            let space = arg("space");
            space_key(space).then(|| format!("{site}/wiki/spaces/{space}"))
        }
        "confluence.update_page" => {
            let site = site?;
            let page = arg("page_id");
            page_id(page).then(|| format!("{site}/wiki/pages/viewpage.action?pageId={page}"))
        }
        "jira.create_issue" => {
            let site = site?;
            let epic = arg("epic");
            if jira_key(epic) {
                return Some(format!("{site}/browse/{epic}"));
            }
            let project = arg("project");
            jira_project(project).then(|| format!("{site}/browse/{project}"))
        }
        "slack.post_reply" => {
            let (channel, ts) = (arg("channel"), arg("thread_ts"));
            (slack_channel(channel) && slack_ts(ts)).then(|| archive_url(channel, ts))
        }
        "slack.send_message" => {
            let channel = arg("channel");
            slack_channel(channel).then(|| format!("https://slack.com/archives/{channel}"))
        }
        _ => None,
    }
}

/// Does building a target link for `tool` need the Atlassian site?
pub fn needs_site(tool: &str) -> bool {
    tool.starts_with("confluence.") || tool.starts_with("jira.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SITE: Option<&str> = Some("https://acme.atlassian.net");

    #[test]
    fn describes_each_system_from_ids_only() {
        assert_eq!(
            describe("jira", "read", "PAY-212", SITE, None),
            Some((
                "PAY-212".into(),
                "PAY-212".into(),
                Some("https://acme.atlassian.net/browse/PAY-212".into())
            ))
        );
        assert_eq!(
            describe("jira", "read", "PAY-212", None, None).unwrap().2,
            None,
            "no site, no link"
        );
        assert_eq!(
            describe("jira", "rollup", "rollup:status", SITE, None)
                .unwrap()
                .1,
            "Jira rollup by status"
        );
        assert_eq!(
            describe("confluence", "read", "9001", SITE, None)
                .unwrap()
                .2
                .as_deref(),
            Some("https://acme.atlassian.net/wiki/pages/viewpage.action?pageId=9001")
        );
        assert_eq!(
            describe(
                "slack",
                "read_thread",
                "C0FIELD:1731500000.000100",
                None,
                None
            ),
            Some((
                "C0FIELD:1731500000.000100".into(),
                "Slack thread in C0FIELD".into(),
                Some("https://slack.com/archives/C0FIELD/p1731500000000100".into())
            ))
        );
        assert_eq!(
            describe(
                "docs",
                "read",
                "https://docs.example.com/docs/vault-saml-sso.md",
                None,
                None
            )
            .unwrap()
            .1,
            "vault saml sso"
        );
        // Shapes that could carry anything else are left out.
        for (system, id) in [
            ("jira", "pay 212"),
            ("jira", "<b>x</b>"),
            ("confluence", "12a"),
            ("slack", "C01"),
            ("slack", "c01:1.2"),
            ("docs", "http://docs.example.com/x"),
            ("docs", "https://docs.example.com/\"onclick"),
            ("github", "x"),
        ] {
            assert_eq!(
                describe(system, "read", id, SITE, Some("A title")),
                None,
                "{system}:{id}"
            );
        }
    }

    #[test]
    fn recorded_titles_name_items_but_never_slack() {
        assert_eq!(
            describe(
                "jira",
                "read",
                "PAY-212",
                SITE,
                Some("Grace period setting")
            )
            .unwrap()
            .1,
            "PAY-212: Grace period setting"
        );
        assert_eq!(
            describe(
                "confluence",
                "read",
                "9001",
                None,
                Some("PRD: Key export v2")
            )
            .unwrap()
            .1,
            "PRD: Key export v2"
        );
        assert_eq!(
            describe(
                "docs",
                "read",
                "https://docs.example.com/docs/vault-saml-sso.md",
                None,
                Some("Configure SAML SSO")
            )
            .unwrap()
            .1,
            "Configure SAML SSO"
        );
        // Cleaned again on the way out, and blank reads as none.
        assert_eq!(
            describe("confluence", "read", "9001", None, Some("A\n<b>B</b>"))
                .unwrap()
                .1,
            "A <b>B</b>"
        );
        assert_eq!(
            describe("confluence", "read", "9001", None, Some(" \t"))
                .unwrap()
                .1,
            "Confluence page 9001"
        );
        // Slack stays channel and message ids, whatever was recorded.
        assert_eq!(
            describe(
                "slack",
                "read_thread",
                "C0FIELD:1731500000.000100",
                None,
                Some("the launch slipped")
            )
            .unwrap()
            .1,
            "Slack thread in C0FIELD"
        );
        assert_eq!(
            describe("jira", "rollup", "rollup:status", SITE, Some("x"))
                .unwrap()
                .1,
            "Jira rollup by status"
        );
    }

    #[test]
    fn target_links_come_from_checked_ids() {
        let args = json!({"space": "PM", "parent_id": "77", "title": "x", "body": "y"});
        assert_eq!(
            target_url("confluence.create_draft", &args, SITE).as_deref(),
            Some("https://acme.atlassian.net/wiki/pages/viewpage.action?pageId=77")
        );
        assert_eq!(
            target_url("confluence.create_draft", &json!({"space": "PM"}), SITE).as_deref(),
            Some("https://acme.atlassian.net/wiki/spaces/PM")
        );
        assert_eq!(
            target_url("confluence.create_draft", &args, None),
            None,
            "no site, no link"
        );
        assert_eq!(
            target_url(
                "jira.create_issue",
                &json!({"project": "PAY", "epic": "PAY-1"}),
                SITE
            )
            .as_deref(),
            Some("https://acme.atlassian.net/browse/PAY-1")
        );
        assert_eq!(
            target_url(
                "slack.post_reply",
                &json!({"channel": "C01", "thread_ts": "1731.0002", "text": "x"}),
                None
            )
            .as_deref(),
            Some("https://slack.com/archives/C01/p17310002")
        );
        assert_eq!(
            target_url(
                "slack.send_message",
                &json!({"channel": "https://evil.example"}),
                None
            ),
            None
        );
        assert_eq!(target_url("bets.record", &json!({}), SITE), None);
    }

    #[test]
    fn cite_refs_are_short_stable_and_made_from_ids() {
        let r = cite_ref("jira:PAY-212");
        assert_eq!(r, cite_ref("jira:PAY-212"), "stable");
        assert!(pmos_contract::cite::is_ref(&r), "{r}");
        assert_eq!(r.len(), "src-".len() + CITE_REF_HEX);
        assert_ne!(r, cite_ref("jira:PAY-213"));
        assert_ne!(cite_ref("context/a.md"), cite_ref("context/b.md"));
    }

    #[test]
    fn site_urls_are_https_origins() {
        assert_eq!(
            site_url(Some("https://acme.atlassian.net/"), None).as_deref(),
            Some("https://acme.atlassian.net")
        );
        assert_eq!(
            site_url(Some("javascript:alert(1)"), Some("abc-123")).as_deref(),
            Some("https://abc-123.atlassian.net")
        );
        assert_eq!(site_url(Some("https://a.net/x"), None), None);
        assert_eq!(site_url(None, Some("a b")), None);
    }
}
