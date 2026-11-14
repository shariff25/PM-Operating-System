//! Slash commands. Each returns what to put in the acknowledgement (Slack needs it within
//! 3 seconds) and, for slow work, a follow-up that replies through `response_url`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use pmos_contract::model::TeamFile;
use serde_json::{json, Value};

use crate::blocks;
use crate::toolserver::{CallError, ToolServer};

pub type FollowUp = Pin<Box<dyn Future<Output = ()> + Send>>;

pub struct Reply {
    /// The acknowledgement payload: shown at once, only to the user.
    pub ack: Value,
    /// Work to finish after the acknowledgement.
    pub follow_up: Option<FollowUp>,
}

/// Product filters `/docs` accepts as `product:<name>`.
const PRODUCTS: [&str; 6] = [
    "dsm",
    "ccm",
    "confidential-ai",
    "key-insight",
    "armor",
    "general",
];

#[derive(Clone)]
pub struct Commands {
    pub toolserver: ToolServer,
    /// `content/team.yaml`, read on each command so a merged change applies without a
    /// restart.
    pub team_file: PathBuf,
    pub http: reqwest::Client,
    /// Where `response_url` may point. Slack's is always `https://hooks.slack.com/`; tests
    /// set a local mock.
    pub response_url_prefix: String,
}

pub const SLACK_RESPONSE_URL_PREFIX: &str = "https://hooks.slack.com/";

impl Commands {
    fn team(&self) -> TeamFile {
        match std::fs::read_to_string(&self.team_file) {
            Ok(text) => TeamFile::from_yaml(&text).unwrap_or_else(|e| {
                tracing::error!(error = %e, "team.yaml does not parse; nobody can use the bot");
                TeamFile::default()
            }),
            Err(e) => {
                tracing::error!(error = %e, path = %self.team_file.display(), "team.yaml unreadable");
                TeamFile::default()
            }
        }
    }

    /// Handle one `slash_commands` payload.
    pub fn handle(&self, payload: &Value, envelope_id: &str) -> Reply {
        let command = payload["command"].as_str().unwrap_or_default();
        match command {
            "/docs" => self.docs(payload, envelope_id),
            other => Reply {
                ack: blocks::text(&format!("`{other}` isn't available yet.")),
                follow_up: None,
            },
        }
    }

    fn docs(&self, payload: &Value, envelope_id: &str) -> Reply {
        let slack_id = payload["user_id"].as_str().unwrap_or_default();
        let team = self.team();
        let Some(member) = team.by_slack_id(slack_id) else {
            tracing::info!(slack_id, "/docs from someone not in team.yaml");
            return Reply {
                ack: blocks::text(
                    "PM OS is for the Acme product team. If you need access, ask a \
                     platform-owner PM to add you.",
                ),
                follow_up: None,
            };
        };
        let text = payload["text"].as_str().unwrap_or_default().trim();
        if text.is_empty() || text == "help" {
            return Reply {
                ack: blocks::text(blocks::USAGE),
                follow_up: None,
            };
        }
        let mut product = None;
        let words: Vec<&str> = text
            .split_whitespace()
            .filter(|w| match w.strip_prefix("product:") {
                Some(p) if PRODUCTS.contains(&p) => {
                    product = Some(p.to_string());
                    false
                }
                _ => true,
            })
            .collect();
        let query = words.join(" ");
        if query.is_empty() {
            return Reply {
                ack: blocks::text(blocks::USAGE),
                follow_up: None,
            };
        }
        let Some(response_url) = payload["response_url"]
            .as_str()
            .filter(|u| u.starts_with(&self.response_url_prefix))
            .map(String::from)
        else {
            return Reply {
                ack: blocks::text("Slack sent no reply address; try again."),
                follow_up: None,
            };
        };

        let mut args = json!({"query": query, "limit": 5});
        if let Some(p) = &product {
            args["product"] = json!(p);
        }
        let this = self.clone();
        let user = member.handle.clone();
        let run_id = format!("slack-docs-{envelope_id}");
        let shown_query = query.clone();
        let follow_up = Box::pin(async move {
            let message = match this
                .toolserver
                .call_once(&user, "docs", &run_id, "docs_search", args)
                .await
            {
                Ok(result) => blocks::results(&shown_query, &result),
                Err(CallError::Unavailable) => {
                    blocks::text("PM OS couldn't search the docs right now. Try again in a minute.")
                }
                Err(CallError::Refused(why)) => blocks::text(&format!(
                    "PM OS couldn't run that search: {}",
                    blocks::escape(&why)
                )),
            };
            post_reply(&this.http, &response_url, &message).await;
        });
        Reply {
            ack: blocks::searching(&query),
            follow_up: Some(follow_up),
        }
    }
}

/// Send a reply to Slack's `response_url`, so the PM never stares at "Searching…" forever:
/// retry once on a 5xx or a network error, and if Slack refuses the message (a 4xx such as
/// `invalid_blocks`), send its plain-text version instead.
pub async fn post_reply(http: &reqwest::Client, response_url: &str, message: &Value) {
    let mut message = message.clone();
    for attempt in 0..3 {
        let res = http.post(response_url).json(&message).send().await;
        match res {
            Ok(r) if r.status().is_success() => return,
            Ok(r) if r.status().is_client_error() && message.get("blocks").is_some() => {
                let body = r.text().await.unwrap_or_default();
                tracing::warn!(%body, "Slack refused the /docs reply; sending plain text");
                if let Some(obj) = message.as_object_mut() {
                    obj.remove("blocks");
                }
            }
            Ok(r) if r.status().is_client_error() => {
                tracing::warn!(status = %r.status(), "Slack refused the /docs reply");
                return;
            }
            Ok(r) => {
                tracing::warn!(status = %r.status(), attempt, "replying to /docs failed");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, attempt, "replying to /docs failed");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
    tracing::error!("could not reply to /docs after retrying");
}
