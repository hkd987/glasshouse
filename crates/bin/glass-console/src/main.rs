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
mod credits;
mod directory;
mod edits;
mod fixture;
mod impact;
mod meet;
mod relay;
mod server;
mod state;
mod studio;
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
    /// Our apps (manifests and source) and the SDK: what apps people share
    /// are compared with and built against (ADR 0032).
    #[arg(long, default_value = "apps")]
    apps: PathBuf,
    /// The pinned bundler (esbuild) the console builds shared apps with,
    /// confined by `unshare`.
    #[arg(
        long,
        default_value = "apps/node_modules/@esbuild/linux-x64/bin/esbuild"
    )]
    bundler: PathBuf,
    /// The meeting relay's API (`glass-sfu --api`), on this machine.
    /// Without it, meetings can't be joined (ADR 0030 §6).
    #[arg(long)]
    relay: Option<String>,
    /// Model credits (ADR 0039): a TOML file, mode 0600, with our
    /// OpenRouter `management_key`, the `operator_token` our staff add
    /// credits with, our `markup` over OpenRouter's price (0.25 is 25 %)
    /// and, for admins to pay by card, `stripe_secret_key` and
    /// `stripe_webhook_secret` (`endpoint`, `stripe_endpoint` optional).
    /// With `--dev` and no file, credits are kept in memory, reach no
    /// model, cost 25 % over and are paid by webhooks signed with
    /// `GLASS_CONSOLE_STRIPE_WEBHOOK` (or a fixed development secret).
    #[arg(long)]
    credits: Option<PathBuf>,
}

/// What `--credits` names.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreditsConfig {
    management_key: String,
    operator_token: String,
    /// Required: pricing is never a default.
    markup: f64,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    stripe_secret_key: Option<String>,
    #[serde(default)]
    stripe_webhook_secret: Option<String>,
    #[serde(default)]
    stripe_endpoint: Option<String>,
}

/// The markup `--dev` sells credits at.
const DEV_MARKUP: f64 = 0.25;

/// The credits config at `path`, if only its owner can read it.
fn credits_config(path: &std::path::Path) -> anyhow::Result<CreditsConfig> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if meta.permissions().mode() & 0o077 != 0 {
        anyhow::bail!(
            "{} can be read by others; it must be mode 0600",
            path.display()
        );
    }
    let config: CreditsConfig = toml::from_str(&std::fs::read_to_string(path)?)
        .map_err(|_| anyhow::anyhow!("{} isn't a valid credits file", path.display()))?;
    if config.operator_token.len() < 32 {
        anyhow::bail!(
            "the operator token in {} is too short to guess at",
            path.display()
        );
    }
    if !config.markup.is_finite() || !(0.0..=10.0).contains(&config.markup) {
        anyhow::bail!(
            "the markup in {} is a fraction, such as 0.25",
            path.display()
        );
    }
    if config.stripe_secret_key.is_some() != config.stripe_webhook_secret.is_some() {
        anyhow::bail!(
            "{} needs both stripe_secret_key and stripe_webhook_secret, or neither",
            path.display()
        );
    }
    Ok(config)
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
    if let Some(api) = &args.relay {
        console.set_relay(relay::Relay::new(api)?);
    }
    // Model credits (ADR 0039): OpenRouter with our management key, or in
    // memory for development.
    match &args.credits {
        Some(path) => {
            let config = credits_config(path)?;
            let payments = match (&config.stripe_secret_key, &config.stripe_webhook_secret) {
                (Some(key), Some(webhook)) => Some(std::sync::Arc::new(credits::StripePayments {
                    stripe: glass_gateway_providers::stripe::Stripe::new(
                        config.stripe_endpoint.as_deref(),
                        key,
                    ),
                    webhook_secret: webhook.clone(),
                })
                    as std::sync::Arc<dyn credits::Payments>),
                _ => None,
            };
            console.set_credits(
                std::sync::Arc::new(glass_gateway_providers::OpenRouterKeys::new(
                    config.endpoint.as_deref(),
                    &config.management_key,
                )),
                config.operator_token,
                config.markup,
                payments,
            );
        }
        None if args.dev => {
            let token = std::env::var("GLASS_CONSOLE_OPERATOR")
                .unwrap_or_else(|_| "dev-operator-token-not-a-secret".into());
            let webhook = std::env::var("GLASS_CONSOLE_STRIPE_WEBHOOK")
                .unwrap_or_else(|_| "whsec_dev_not_a_secret".into());
            console.set_credits(
                std::sync::Arc::new(credits::MemoryKeys::default()),
                token,
                DEV_MARKUP,
                Some(std::sync::Arc::new(credits::DevPayments::new(webhook))),
            );
        }
        None => {}
    }
    // Apps people share are checked and built here, confined, or refused.
    if args.bundler.is_file() && args.apps.join("sdk").is_dir() {
        console.set_builder(
            std::sync::Arc::new(glass_platform::sandbox::UnshareTools),
            args.apps.clone(),
            std::path::absolute(&args.bundler)?,
            std::path::absolute(args.state.join("builds"))?,
        );
    } else {
        tracing::warn!(
            bundler = %args.bundler.display(),
            "apps people share can't be checked here: no bundler or SDK"
        );
    }
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
