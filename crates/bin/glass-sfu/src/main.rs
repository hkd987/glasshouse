//! `glass-sfu`: the meetings relay (CF-21, ADR 0030). It forwards sealed
//! media between the people in a meeting and never records it. It runs on
//! our servers, never on a device; the admin console admits participants
//! over its private API. The relay itself is `glass-media-relay`.
//!
//! ```text
//! glass-sfu --udp 127.0.0.1:0 --public 127.0.0.1 --api 127.0.0.1:0
//! ```
//!
//! When it is ready it prints one line on stdout, for test harnesses:
//! `glass-sfu ready udp=<ip:port> api=<ip:port>`, where `udp` is the
//! address pages connect to (the public address and the socket's port).
//! Logs go to stderr (`GLASS_LOG` sets the filter).

use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use glass_media_relay::{Config, Relay};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::prelude::*;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "The meetings relay: forwards sealed media between the people in a meeting"
)]
struct Args {
    /// The UDP socket for everyone's media.
    #[arg(long, default_value = "127.0.0.1:0")]
    udp: SocketAddr,
    /// The address computers reach that socket on. Defaults to --udp's.
    #[arg(long)]
    public: Option<IpAddr>,
    /// The private API for the admin console (loopback only).
    #[arg(long, default_value = "127.0.0.1:0")]
    api: SocketAddr,
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

async fn run(args: Args) -> anyhow::Result<()> {
    let public = match args.public {
        Some(ip) => ip,
        None if args.udp.ip().is_unspecified() => {
            anyhow::bail!(
                "--udp {} needs --public: the address pages connect to",
                args.udp
            )
        }
        None => args.udp.ip(),
    };
    let mut terminate = signal(SignalKind::terminate()).context("listening for SIGTERM")?;
    let config = Config {
        udp: args.udp,
        public,
        api: args.api,
        connect_within: glass_media_relay::CONNECT_WITHIN,
    };
    let mut relay = Relay::start(config).await?;
    ready(&relay)?;
    tokio::select! {
        () = relay.stopped() => anyhow::bail!("the relay stopped"),
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("stopping");
    Ok(())
}

/// The one line on stdout.
fn ready(relay: &Relay) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "glass-sfu ready udp={} api={}",
        relay.media_addr(),
        relay.api_addr()
    )?;
    out.flush()?;
    Ok(())
}
