//! `glass-billing`: the billing relay (ADR 0040). One Stripe account serves
//! every org, and Stripe allows only a few webhook endpoints, so Stripe
//! sends every event here. The relay checks Stripe's signature and pushes
//! each paid checkout to the admin console of the org it names, signed with
//! its own key, queued on disk and tried again until that console takes it.
//! Consoles ask it for checkouts too, so Stripe's secret key is here alone.
//! It runs on our servers, never on a device.
//!
//! ```text
//! glass-billing --dev --console acme=http://127.0.0.1:7400
//! glass-billing --config /usr/local/etc/glass-billing.toml --listen 127.0.0.1:7500
//! glass-billing public-key --state /var/db/glass-billing
//! ```
//!
//! When it is ready it prints one line on stdout, for test harnesses:
//! `glass-billing ready http=<ip:port> key=<hex>`, the key being the public
//! half consoles check pushes with. Logs go to stderr (`GLASS_LOG`).

mod config;
mod outbox;
mod relay;
#[cfg(test)]
mod tests;

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use glass_crypto::SecretKey;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::prelude::*;

use crate::config::{Checkouts, Config};
use crate::outbox::{Outbox, Timing};
use crate::relay::{DevCheckouts, Relay};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "The billing relay: one Stripe endpoint, payments pushed to each org's console"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Development: loopback only, consoles named with --console, Stripe's
    /// webhooks signed with a development secret, checkouts that pay at once.
    #[arg(long)]
    dev: bool,
    /// The config: Stripe's keys, our operators' token, the consoles.
    #[arg(long, conflicts_with = "dev")]
    config: Option<PathBuf>,
    /// A development console, as org=url (repeat for more).
    #[arg(long = "console", requires = "dev")]
    consoles: Vec<String>,
    /// Where to listen. TLS is our web servers', in front.
    #[arg(long, default_value = "127.0.0.1:7500")]
    listen: SocketAddr,
    /// The relay's key and its queue of deliveries.
    #[arg(long, default_value = "build/billing")]
    state: PathBuf,
    /// The wait after a delivery first fails, in seconds; each one after
    /// waits three times as long, up to an hour.
    #[arg(long, default_value_t = 5)]
    first_retry_secs: u64,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the public key consoles check pushes with.
    PublicKey,
}

fn main() -> ExitCode {
    let filter = std::env::var("GLASS_LOG")
        .ok()
        .and_then(|spec| spec.parse::<Targets>().ok())
        .unwrap_or_else(|| Targets::new().with_default(LevelFilter::INFO));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_writer(std::io::stderr),
        )
        .with(filter)
        .init();
    let args = Args::parse();
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")
        .and_then(|rt| rt.block_on(run(args)));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The relay's key, kept in `state` for its owner alone.
fn key(state: &std::path::Path) -> anyhow::Result<SecretKey> {
    std::fs::create_dir_all(state).with_context(|| format!("{}", state.display()))?;
    glass_managed::device::kept_secret(&state.join("relay.key"), "the relay key")
        .map_err(anyhow::Error::msg)
}

fn public_hex(key: &SecretKey) -> anyhow::Result<String> {
    Ok(hex::encode(key.public()?.to_bytes()))
}

async fn run(args: Args) -> anyhow::Result<()> {
    let key = key(&args.state)?;
    if let Some(Command::PublicKey) = args.command {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{}", public_hex(&key)?)?;
        return Ok(());
    }
    let config = match (&args.config, args.dev) {
        (Some(path), false) => Config::load(path)?,
        (None, true) => {
            if !args.listen.ip().is_loopback() {
                bail!("--dev listens on a loopback address only");
            }
            Config::development(&args.consoles)?
        }
        _ => bail!("give --config, or --dev for development"),
    };
    let checkouts: Arc<dyn relay::Checkouts> = match &config.checkouts {
        Checkouts::Stripe {
            endpoint,
            secret_key,
        } => Arc::new(glass_gateway_providers::stripe::Stripe::new(
            endpoint.as_deref(),
            secret_key,
        )),
        Checkouts::Development => Arc::new(DevCheckouts::default()),
    };
    let timing = Timing {
        first: Duration::from_secs(args.first_retry_secs.max(1)),
        ..Timing::default()
    };
    let outbox = Outbox::open(&args.state, timing)?;
    let public = public_hex(&key)?;
    for c in &config.consoles {
        tracing::info!(org = %c.org, url = %c.url, "pushing to this console");
    }
    let relay = Relay::new(config, key, outbox, checkouts);
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("listening on {}", args.listen))?;
    let addr = listener.local_addr()?;
    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "glass-billing ready http={addr} key={public}")?;
        out.flush()?;
    }
    let mut terminate = signal(SignalKind::terminate()).context("listening for SIGTERM")?;
    tokio::select! {
        served = relay.serve(listener) => served?,
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("stopping");
    Ok(())
}
