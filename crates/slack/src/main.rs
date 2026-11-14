use std::path::PathBuf;

use anyhow::{anyhow, Result};
use pmos_slack::commands::{Commands, SLACK_RESPONSE_URL_PREFIX};
use pmos_slack::socket::Socket;
use pmos_slack::toolserver::ToolServer;
use tracing_subscriber::EnvFilter;

fn need(key: &str) -> Result<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("{key} is required"))
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
    let http = reqwest::Client::new();
    let socket = Socket {
        api_base: std::env::var("PMOS_SLACK_URL").unwrap_or_else(|_| "https://slack.com".into()),
        app_token,
        commands: Commands {
            toolserver,
            team_file,
            http: http.clone(),
            response_url_prefix: SLACK_RESPONSE_URL_PREFIX.into(),
        },
        http,
    };
    tracing::info!("pmos-slack starting (Socket Mode)");
    socket.run().await;
    Ok(())
}
