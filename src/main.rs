mod config;
mod filter;
mod http;
mod scheduler;
mod source;
mod state;
mod sync;
mod target;

use anyhow::Result;
use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use config::Config;
use filter::Filters;
use source::Source;
use state::State;
use sync::Engine;
use target::Target;

#[derive(Parser, Debug)]
#[command(
    name = "gitea-mirror-sync",
    version,
    about = "Keep mirror repos on a Gitea/Forgejo instance in sync with a source forge"
)]
struct Cli {
    /// Run a single reconcile (including the token-rotation check) then exit.
    /// Use this with an external scheduler (systemd timer, k8s CronJob, ...).
    #[arg(long)]
    once: bool,

    /// Validate the config, print the detected target capabilities, then exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let cfg = Config::load()?;

    let http = http::build_client()?;
    let target = Target::new(
        http.clone(),
        &cfg.target_url,
        &cfg.target_token,
        &cfg.target_owner,
    );

    info!("detecting target capabilities…");
    let caps = target.detect(cfg.target_owner_type).await;
    info!(
        version = %caps.version,
        forgejo = caps.is_forgejo,
        pull_mirror_patch = caps.supports_pull_mirror_patch,
        owner_is_org = caps.owner_is_org,
        "target capabilities detected"
    );

    let source = Source::from_config(&cfg, http.clone())?;
    let filters = Filters::from_config(&cfg)?;

    if cli.check {
        println!("--- gitea-mirror-sync --check ---");
        println!(
            "Source : {:?}  owner='{}'",
            cfg.source_type, cfg.source_owner
        );
        println!(
            "Target : '{}'  owner='{}'",
            cfg.target_url, cfg.target_owner
        );
        println!("Capabilities:\n{caps:#?}");
        if cfg.rotation_mode == config::RotationMode::Auto && !caps.supports_pull_mirror_patch {
            println!(
                "Note: token rotation will use delete+recreate (no PATCH mirror_token support detected)."
            );
        }
        return Ok(());
    }

    let mut st = State::load(&cfg.state_file)?;
    let engine = Engine {
        cfg: &cfg,
        target: &target,
        source: &source,
        filters: &filters,
        caps: &caps,
    };

    if cli.once {
        engine.reconcile(&mut st, true).await?;
    } else {
        scheduler::run_daemon(&engine, &mut st, &cfg.cron).await?;
    }
    Ok(())
}
