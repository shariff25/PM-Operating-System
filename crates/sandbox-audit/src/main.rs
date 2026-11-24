use anyhow::{Context, Result};
use pmos_sandbox_audit::app::{Collector, Config};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let config = Config::from_lookup(|k| std::env::var(k).ok()).map_err(anyhow::Error::msg)?;
    let bind = config.bind;
    let collector = Collector::new(config).context("opening the spool")?;
    {
        let inner = collector.inner.lock().expect("collector lock");
        tracing::info!(
            run = %inner.run,
            spooled = inner.spool.len().unwrap_or(0),
            "sandbox-audit collector starting"
        );
    }
    let cancel = CancellationToken::new();
    let tasks = collector.spawn_tasks(cancel.clone());
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    let ct = cancel.clone();
    axum::serve(listener, collector.router())
        .with_graceful_shutdown(async move {
            // SIGTERM (docker stop) or Ctrl-C: flush the open summaries into the spool.
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("a SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            ct.cancel();
        })
        .await?;
    cancel.cancel();
    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}
