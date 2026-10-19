//! `glass-console`: the org's admin console (AD-6, ADR 0023). IT signs in, sets
//! agent policy per group and rolls it out in rings, approves catalog
//! items per group, and sees the fleet; devices sync signed policy and
//! catalog releases from it and report back.
//!
//! It runs on our web servers, never on a device. This slice has one mode,
//! `--dev`: loopback HTTP, sign-in by picking one of the directory's
//! admins or support staff, state as JSON files in a folder. Production adds TLS at our web
//! servers, the org's SSO and a managed database (not built).

mod access;
mod chat;
mod directory;
mod edits;
mod fixture;
mod impact;
mod server;
mod state;
mod views;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::Parser;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::prelude::*;

use crate::directory::LocalDirectory;

#[derive(Debug, Parser)]
#[command(version, about = "The org's admin console: policy, catalog and fleet")]
struct Args {
    /// Development mode: loopback HTTP, sign in as any directory admin.
    #[arg(long)]
    dev: bool,
    /// Where to listen (loopback only).
    #[arg(long, default_value = "127.0.0.1:7400")]
    listen: SocketAddr,
    /// The console's state: org, key, policy versions, catalog, fleet.
    #[arg(long, default_value = "build/console")]
    state: PathBuf,
    /// The built web app.
    #[arg(long, default_value = "apps/console/dist")]
    web: PathBuf,
    /// The org's id, on first start.
    #[arg(long, default_value = "acme")]
    org: String,
    /// The org's name, on first start.
    #[arg(long, default_value = "Acme Corp")]
    org_name: String,
    /// On first start, the person running the console joins IT's admins.
    #[arg(long, env = "USER", default_value = "pat")]
    admin: String,
    /// On first start, fill the console with this many devices and catalog
    /// items: `500,50`.
    #[arg(long)]
    fixture: Option<String>,
}

fn main() -> ExitCode {
    let filter = std::env::var("GLASS_LOG")
        .ok()
        .and_then(|spec| spec.parse::<Targets>().ok())
        .unwrap_or_else(|| Targets::new().with_default(LevelFilter::INFO));
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(false))
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

fn parse_fixture(spec: &str) -> anyhow::Result<(usize, usize)> {
    let (devices, items) = spec
        .split_once(',')
        .context("--fixture is <devices>,<items>, such as 500,50")?;
    Ok((devices.trim().parse()?, items.trim().parse()?))
}

async fn run(args: Args) -> anyhow::Result<()> {
    if !args.dev {
        bail!(
            "only --dev is built: our web servers will add TLS, the org's sign-in and a managed \
             database in front of this"
        );
    }
    if !args.listen.ip().is_loopback() {
        bail!("--dev listens on a loopback address only");
    }
    std::fs::create_dir_all(&args.state)
        .with_context(|| format!("creating {}", args.state.display()))?;
    let fixture = args.fixture.as_deref().map(parse_fixture).transpose()?;
    let directory_path = args.state.join("directory.toml");
    if !directory_path.exists() {
        let dir = match fixture {
            Some((devices, _)) => fixture::directory(devices, &args.admin),
            None => LocalDirectory::starter(&args.admin),
        };
        std::fs::write(&directory_path, toml::to_string(&dir)?)?;
    }
    let directory = LocalDirectory::load(&directory_path)?;
    let mut state = state::State::open(&args.state, &args.org, &args.org_name, &directory)?;
    if let Some((devices, items)) = fixture {
        fixture::fill(&mut state, &directory, devices, items)?;
    }
    // The people devices get (AD-1), signed again only when they changed.
    if let Some(version) = state.release_directory(&directory)? {
        tracing::info!(
            version,
            people = directory.users.len(),
            "directory released"
        );
    }
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("listening on {}", args.listen))?;
    let url = format!("http://{}", listener.local_addr()?);
    // What a development device needs to enroll (ADR 0023): the console's
    // address, the org, its public key and the enrollment token.
    let enrollment = serde_json::json!({
        "url": url,
        "org": state.org.id,
        "org_key": state.public_key(),
        "token": state.org.enrollment_token,
    });
    state::write_json(&args.state.join("enrollment.json"), &enrollment)?;
    tracing::info!(
        %url,
        org = %state.org.id,
        devices = state.fleet.devices.len(),
        items = state.catalog.items.len(),
        "admin console ready (development)"
    );
    let console = server::Console::new(state, Box::new(directory), args.web, url);
    console.serve(listener).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_are_devices_then_items() {
        assert_eq!(parse_fixture("500,50").unwrap(), (500, 50));
        assert!(parse_fixture("500").is_err());
    }
}
