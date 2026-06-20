use anyhow::Result;
use chrono::Utc;
use cron::Schedule;
use std::str::FromStr;
use tracing::{error, info};

use crate::state::State;
use crate::sync::Engine;

/// Run the daemon: one reconcile at startup, then on every cron tick until a
/// shutdown signal arrives. Reconcile errors are logged, not fatal.
pub async fn run_daemon(engine: &Engine<'_>, state: &mut State, cron_expr: &str) -> Result<()> {
    let schedule = Schedule::from_str(cron_expr)
        .map_err(|e| anyhow::anyhow!("invalid cron expression '{cron_expr}': {e}"))?;

    if let Err(e) = engine.reconcile(state, true).await {
        error!("startup reconcile failed: {e:#}");
    }

    info!("daemon started; schedule = '{cron_expr}'");
    loop {
        let Some(next) = schedule.upcoming(Utc).next() else {
            error!("cron schedule yields no upcoming times; stopping");
            break;
        };
        let wait = (next - Utc::now())
            .to_std()
            .unwrap_or(std::time::Duration::from_secs(1));
        info!("next run at {next} (in {wait:?})");

        tokio::select! {
            _ = tokio::time::sleep(wait) => {
                if let Err(e) = engine.reconcile(state, false).await {
                    error!("scheduled reconcile failed: {e:#}");
                }
            }
            _ = shutdown_signal() => {
                info!("shutdown signal received; stopping");
                break;
            }
        }
    }
    Ok(())
}

/// Resolve when either Ctrl-C (SIGINT) or SIGTERM is received.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };

    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
