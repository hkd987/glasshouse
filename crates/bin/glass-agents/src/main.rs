//! `glass-agents`, the Glasshouse Agents front (ADR 0066): the web app,
//! the API the `glass` CLI uses, the Teams endpoint and metering, over the
//! glassd on its host. Everything an agent does still goes through glassd:
//! the front only asks it, as the person it acts for.
//!
//! As it starts it writes where it listens and its token to the person's
//! `~/.config/glasshouse/agents.json` (0600), which `glass` reads, and
//! prints the link that signs a browser in.

mod glassd;
mod server;
mod teams;
#[cfg(test)]
mod tests;
mod views;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::prelude::*;

#[derive(Debug, Parser)]
#[command(version, about = "Glasshouse Agents: the web app and API over glassd")]
struct Args {
    /// Where to listen.
    #[arg(long, default_value = "127.0.0.1:7500")]
    listen: SocketAddr,
    /// glassd's control socket.
    #[arg(long, env = "GLASS_CONTROL_SOCKET", default_value = glass_api::DEFAULT_CONTROL_SOCKET)]
    control_socket: PathBuf,
    /// The built web app.
    #[arg(long, default_value = "apps/agents/dist")]
    web: PathBuf,
    /// The org's name, as the web app shows it.
    #[arg(long, default_value = "Acme Corp")]
    org: String,
    /// The person's name, as people see it; their account's by default.
    #[arg(long)]
    name: Option<String>,
    /// Their email, for Teams and the web app.
    #[arg(long, default_value = "")]
    email: String,
    /// Where to write the URL and token for `glass`.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Teams (ADR 0065 §13): a TOML file with the org's app registration,
    /// the person's Entra object id and where to keep their chat.
    #[arg(long)]
    teams: Option<PathBuf>,
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

/// 32 random bytes as hex, from the kernel.
fn token() -> anyhow::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes where the front is and its token for `glass`, readable only by
/// the person.
fn write_token_file(path: &std::path::Path, url: &str, token: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let making = path.with_extension("json.new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&making)
        .with_context(|| format!("writing {}", making.display()))?;
    file.write_all(
        serde_json::json!({ "url": url, "token": token })
            .to_string()
            .as_bytes(),
    )?;
    std::fs::rename(&making, path).with_context(|| format!("writing {}", path.display()))
}

async fn run(args: Args) -> anyhow::Result<()> {
    let login = glass_platform::users::by_uid(glass_platform::users::effective_uid())
        .map(|u| u.name)
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "you".into());
    let person = glass_agents_api::Person {
        id: login.clone(),
        name: args.name.unwrap_or(login),
        email: args.email,
    };
    let token = token()?;
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("listening on {}", args.listen))?;
    let url = format!("http://{}", listener.local_addr()?);
    let token_file = match args.token_file {
        Some(path) => path,
        None => {
            let home = std::env::var("HOME").context("HOME isn't set")?;
            PathBuf::from(home).join(".config/glasshouse/agents.json")
        }
    };
    write_token_file(&token_file, &url, &token)?;
    tracing::info!(
        "Glasshouse Agents for {} at {url}/?token={token} (glass reads {})",
        person.name,
        token_file.display()
    );
    let teams = match &args.teams {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let config: teams::TeamsConfig =
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            tracing::info!("Teams: approvals go to the person's chat with the app");
            Some(teams::Teams::new(config)?)
        }
        None => None,
    };
    let front = server::Front::new(
        glassd::Glassd::new(args.control_socket),
        args.web,
        token,
        person,
        args.org,
        teams,
    );
    front.serve(listener).await
}
