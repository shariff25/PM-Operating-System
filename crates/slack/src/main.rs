use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use pmos_slack::bots::{Bots, BotsConfig};
use pmos_slack::commands::{Commands, SLACK_RESPONSE_URL_PREFIX};
use pmos_slack::notify::Notifier;
use pmos_slack::runtime::Runtime;
use pmos_slack::slackapi::SlackApi;
use pmos_slack::socket::{Ended, Socket};
use pmos_slack::toolserver::ToolServer;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

fn need(key: &str) -> Result<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("{key} is required"))
}

fn optional(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // rustls 0.23 needs a process-wide crypto provider before the first wss:// connection.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let app_token = need("PMOS_SLACK_APP_TOKEN")?;
    if !app_token.starts_with("xapp-") {
        return Err(anyhow!(
            "PMOS_SLACK_APP_TOKEN must be an app-level token (xapp-…) with connections:write"
        ));
    }
    let toolserver = ToolServer::new(need("PMOS_TOOLSERVER_URL")?, need("PMOS_BOT_TOKEN")?);
    let team_file =
        PathBuf::from(std::env::var("PMOS_CONTENT_ROOT").unwrap_or_else(|_| "content".into()))
            .join("team.yaml");
    if !team_file.is_file() {
        return Err(anyhow!("{} not found", team_file.display()));
    }
    let slack_url = optional("PMOS_SLACK_URL").unwrap_or_else(|| "https://slack.com".into());
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()?;

    // The bots (DMs, approval cards, routines) need the runtime; without it, `/docs` only.
    let bots = match optional("PMOS_BOTS_URL") {
        Some(bots_url) => {
            let bot_token = need("PMOS_SLACK_BOT_TOKEN")?;
            if !bot_token.starts_with("xoxb-") {
                return Err(anyhow!(
                    "PMOS_SLACK_BOT_TOKEN must be the app's bot token (xoxb-…)"
                ));
            }
            Some(Bots::new(BotsConfig {
                slack: SlackApi::new(slack_url.clone(), bot_token, http.clone()),
                runtime: Runtime::new(bots_url, need("PMOS_BOTS_TOKEN")?, http.clone()),
                toolserver: toolserver.clone(),
                team_file: team_file.clone(),
                default_tz: optional("PMOS_DEFAULT_TZ").unwrap_or_else(|| "UTC".into()),
            }))
        }
        None => {
            tracing::warn!("PMOS_BOTS_URL is not set: serving /docs only, no DMs or approvals");
            None
        }
    };

    let mut socket = Socket::new(
        slack_url,
        app_token,
        Commands {
            toolserver,
            team_file,
            http: http.clone(),
            response_url_prefix: SLACK_RESPONSE_URL_PREFIX.into(),
        },
        http,
    );
    let shutdown = CancellationToken::new();
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            pmos_runtime::shutdown_signal().await;
            shutdown.cancel();
        });
    }
    let mut notifier = None;
    if let Some(bots) = bots {
        let state_dir =
            PathBuf::from(optional("PMOS_SLACK_STATE_DIR").unwrap_or_else(|| "state/slack".into()));
        tracing::info!(state_dir = %state_dir.display(), "notification cursor kept here");
        socket = socket.with_bots(bots.clone());
        notifier = Some(tokio::spawn(
            Notifier::new(bots, state_dir).run(shutdown.child_token()),
        ));
    }
    tracing::info!("pmos-slack starting (Socket Mode)");
    let ended = socket.run(shutdown.clone()).await;
    // Socket Mode turned off ends the process too: stop the notifications loop.
    shutdown.cancel();
    // Let replies already being worked on reach Slack.
    let cut_off = socket.drain(Duration::from_secs(10)).await;
    if cut_off > 0 {
        tracing::warn!(cut_off, "shut down before every reply was sent");
    }
    if let Some(notifier) = notifier {
        let _ = tokio::time::timeout(Duration::from_secs(5), notifier).await;
    }
    match ended {
        // A non-zero exit, so a supervisor shows it rather than treating it as a clean stop.
        Ended::Disabled => Err(anyhow!(
            "Socket Mode is turned off for this Slack app; turn it on in the app's settings"
        )),
        _ => Ok(()),
    }
}
