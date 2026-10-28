//! `glass`: Glasshouse Agents from the command line (ADR 0066), for
//! technical people: their agents, asking one, answering its questions,
//! watching and taking over its computer. It calls the same API the web
//! app does, with the person's token, so the same rules apply.
//!
//! Where the front is and the token: `GLASS_AGENTS_URL` and
//! `GLASS_AGENTS_TOKEN`, or `~/.config/glasshouse/agents.json`, which
//! `glass login` writes (and a development front writes as it starts).

#![allow(clippy::print_stdout)]

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use glass_agents_api as api;
use glass_gateway_providers::agents::AgentsClient;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Your agents, each on a computer of its own",
    after_help = "With no command, lists your agents."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Your agents and what they're doing.
    Agents,
    /// The agents IT made available to you.
    Templates,
    /// Makes an agent from one of IT's, with a computer of its own.
    New {
        template: String,
        /// Its name, if not the template's.
        #[arg(long)]
        name: Option<String>,
    },
    /// Asks an agent to do something, and waits for what it says.
    Ask {
        agent: String,
        #[arg(required = true)]
        text: Vec<String>,
        /// Don't wait: print the task and return.
        #[arg(long)]
        no_wait: bool,
    },
    /// An agent's conversation, latest last.
    Show {
        agent: String,
        /// How many messages.
        #[arg(long, default_value_t = 12)]
        last: usize,
    },
    /// Questions your agents are waiting on you for.
    Approvals,
    /// Lets an agent do what it asked.
    Approve { approval: String },
    /// Tells an agent no.
    Deny { approval: String },
    /// What an agent's computer's screen shows.
    Screen {
        agent: String,
        /// Keep printing what it shows next.
        #[arg(long, short)]
        follow: bool,
    },
    /// Takes an agent's keyboard: its new tasks wait until you hand it back.
    TakeOver { agent: String },
    /// Gives an agent its keyboard back.
    HandBack { agent: String },
    /// Runs a line on an agent's computer while you have its keyboard.
    Type {
        agent: String,
        #[arg(required = true)]
        line: Vec<String>,
    },
    /// Puts back what an agent's latest task changed.
    Undo { agent: String },
    /// Saves an agent's computer to storage and frees this host of it.
    Sleep { agent: String },
    /// Brings an agent's computer back from storage.
    Wake { agent: String },
    /// Signs this computer in: the address, then the token pasted.
    Login { url: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command.unwrap_or(Command::Agents)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn config_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME isn't set")?;
    Ok(PathBuf::from(home).join(".config/glasshouse/agents.json"))
}

/// The front and the token to call it with.
fn client() -> anyhow::Result<AgentsClient> {
    let (url, token) = match (
        std::env::var("GLASS_AGENTS_URL"),
        std::env::var("GLASS_AGENTS_TOKEN"),
    ) {
        (Ok(url), Ok(token)) => (url, token),
        _ => {
            let path = config_path()?;
            let text = std::fs::read_to_string(&path).with_context(|| {
                format!(
                    "not signed in: run glass login <address> ({} is missing)",
                    path.display()
                )
            })?;
            let config: Value = serde_json::from_str(&text)
                .with_context(|| format!("{} isn't what glass login writes", path.display()))?;
            (
                config["url"].as_str().unwrap_or_default().to_owned(),
                config["token"].as_str().unwrap_or_default().to_owned(),
            )
        }
    };
    AgentsClient::new(&url, &token).map_err(|e| anyhow::anyhow!("{e}"))
}

struct Api(AgentsClient);

impl Api {
    fn read<T: DeserializeOwned>(
        answer: glass_gateway_providers::agents::Answer,
    ) -> anyhow::Result<T> {
        if !(200..300).contains(&answer.status) {
            let said = serde_json::from_str::<api::ApiError>(&answer.body)
                .map(|e| e.error)
                .unwrap_or(answer.body);
            bail!("{said}");
        }
        serde_json::from_str(&answer.body).context("an answer glass couldn't read")
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        Self::read(self.0.get(path).map_err(|e| anyhow::anyhow!("{e}"))?)
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: Value) -> anyhow::Result<T> {
        Self::read(
            self.0
                .post(path, &body)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        )
    }
}

fn status_word(status: api::AgentStatus) -> &'static str {
    match status {
        api::AgentStatus::Starting => "Starting",
        api::AgentStatus::Working => "Working",
        api::AgentStatus::NeedsYou => "Needs you",
        api::AgentStatus::TakenOver => "You have it",
        api::AgentStatus::Idle => "Idle",
        api::AgentStatus::Asleep => "Asleep",
    }
}

fn computer_word(state: api::ComputerState) -> &'static str {
    match state {
        api::ComputerState::New => "new",
        api::ComputerState::Starting => "starting",
        api::ComputerState::Ready => "ready",
        api::ComputerState::Running => "running",
        api::ComputerState::Saving => "saving",
        api::ComputerState::Asleep => "asleep in storage",
        api::ComputerState::Waking => "waking",
    }
}

/// One line of a message, as the terminal shows it.
fn message_lines(message: &api::Message) -> Vec<String> {
    let who = match message.from {
        api::From::Person => "You",
        api::From::Agent => "Agent",
    };
    match &message.body {
        api::Body::Text { text, .. } => vec![format!("{who}: {text}")],
        api::Body::Progress { steps } => steps.iter().map(|s| format!("  · {s}")).collect(),
        api::Body::Approval(a) => {
            let mut lines = vec![format!("Asks: {}", a.summary)];
            if !a.detail.is_empty() && !a.confidential {
                lines.extend(a.detail.lines().map(|l| format!("  {l}")));
            }
            if a.state == api::ApprovalState::Pending {
                lines.push(format!(
                    "  glass approve {}   ·   glass deny {}",
                    a.id, a.id
                ));
            }
            lines
        }
        api::Body::Done {
            text,
            changed,
            undoable,
        } => {
            let mut lines: Vec<String> = text.lines().map(|l| format!("Agent: {l}")).collect();
            if !changed.is_empty() {
                lines.push(format!("  Changed: {}", changed.join(", ")));
            }
            if *undoable {
                lines.push("  glass undo puts it back".into());
            }
            lines
        }
        api::Body::Note { text } => vec![format!("  ({text})")],
    }
}

fn agents(api: &Api) -> anyhow::Result<()> {
    let agents: Vec<api::AgentView> = api.get("agents")?;
    if agents.is_empty() {
        println!(
            "No agents yet. glass templates lists what you can make; glass new <template> makes one."
        );
        return Ok(());
    }
    let width = agents
        .iter()
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(0);
    for agent in &agents {
        let mut doing = agent.doing.clone();
        if agent.pending > 0 {
            doing = format!("{doing} ({} waiting on you)", agent.pending);
        }
        println!(
            "{:width$}  {:<11}  {:<40}  {} ({})",
            agent.name,
            status_word(agent.status),
            doing.chars().take(40).collect::<String>(),
            agent.id,
            computer_word(agent.computer.state),
        );
    }
    Ok(())
}

/// The id an agent goes by, from its id or its name.
fn agent_id(api: &Api, named: &str) -> anyhow::Result<String> {
    let agents: Vec<api::AgentView> = api.get("agents")?;
    agents
        .iter()
        .find(|a| a.id == named || a.name.eq_ignore_ascii_case(named))
        .map(|a| a.id.clone())
        .with_context(|| format!("you have no agent {named:?}; glass agents lists them"))
}

/// Waits for a task to end or ask, printing its computer's screen as it
/// goes and what it said at the end.
fn wait_for(api: &Api, agent: &str, task: &str) -> anyhow::Result<()> {
    // Its screen from where this task starts on it: a computer that was
    // asleep gets its whole screen back as it wakes.
    let marker = format!("── task glass-{} ──", &task[task.len().saturating_sub(8)..]);
    let mut offset = api
        .get::<api::Screen>(&format!("agents/{agent}/screen"))?
        .offset;
    let mut before = Some(String::new());
    let started = Instant::now();
    let mut said_waiting = false;
    loop {
        let screen: api::Screen = api.get(&format!("agents/{agent}/screen?since={offset}"))?;
        let shown = match before.take() {
            Some(mut held) => {
                held.push_str(&screen.text);
                match held.find(&marker) {
                    Some(at) => Some(held[at..].to_owned()),
                    None => {
                        before = Some(held);
                        None
                    }
                }
            }
            None => Some(screen.text),
        };
        if let Some(text) = shown.filter(|t| !t.is_empty()) {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        offset = screen.offset;
        let detail: api::AgentDetail = api.get(&format!("agents/{agent}"))?;
        let mine: Vec<&api::Message> = detail.messages.iter().filter(|m| m.task == task).collect();
        let ended = mine.iter().any(|m| {
            matches!(m.body, api::Body::Done { .. })
                || m.id.ends_with("/failed")
                || m.id.ends_with("/stopped")
        });
        let asking = mine.iter().any(
            |m| matches!(&m.body, api::Body::Approval(a) if a.state == api::ApprovalState::Pending),
        );
        if ended || (asking && !said_waiting) {
            println!();
            for message in mine.iter().filter(|m| m.from == api::From::Agent) {
                if matches!(message.body, api::Body::Progress { .. }) {
                    continue;
                }
                for line in message_lines(message) {
                    println!("{line}");
                }
            }
            if ended {
                return Ok(());
            }
            said_waiting = true;
            println!("It waits for your answer.");
        }
        if started.elapsed() > Duration::from_secs(30 * 60) {
            bail!("still working after 30 minutes; glass show {agent} has the rest");
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

fn run(command: Command) -> anyhow::Result<()> {
    if let Command::Login { url } = &command {
        return login(url);
    }
    let api = Api(client()?);
    match command {
        Command::Agents => agents(&api)?,
        Command::Templates => {
            let templates: Vec<api::Template> = api.get("templates")?;
            for t in templates {
                println!("{:<20} {}  {}", t.id, t.name, t.does);
            }
        }
        Command::New { template, name } => {
            let made: api::AgentView =
                api.post("agents", json!({ "template": template, "name": name }))?;
            println!(
                "Made {} ({}), with a computer of its own: {}",
                made.name, made.id, made.computer.kind
            );
        }
        Command::Ask {
            agent,
            text,
            no_wait,
        } => {
            let id = agent_id(&api, &agent)?;
            let asked: api::Asked = api.post(
                &format!("agents/{id}/ask"),
                json!({ "text": text.join(" "), "via": "cli" }),
            )?;
            if no_wait {
                println!("Asked {id}: task {}", asked.task);
            } else {
                wait_for(&api, &id, &asked.task)?;
            }
        }
        Command::Show { agent, last } => {
            let id = agent_id(&api, &agent)?;
            let detail: api::AgentDetail = api.get(&format!("agents/{id}"))?;
            println!(
                "{} · {} · computer {} ({})",
                detail.agent.name,
                status_word(detail.agent.status),
                computer_word(detail.agent.computer.state),
                detail.agent.computer.kind
            );
            let skip = detail.messages.len().saturating_sub(last);
            for message in detail.messages.iter().skip(skip) {
                for line in message_lines(message) {
                    println!("{line}");
                }
            }
        }
        Command::Approvals => {
            let pending: Vec<api::PendingApproval> = api.get("approvals")?;
            if pending.is_empty() {
                println!("Nothing is waiting on you.");
            }
            for p in pending {
                println!("{}: {}", p.agent_name, p.approval.summary);
                if !p.approval.confidential {
                    for line in p.approval.detail.lines() {
                        println!("  {line}");
                    }
                }
                println!(
                    "  glass approve {}   ·   glass deny {}",
                    p.approval.id, p.approval.id
                );
            }
        }
        Command::Approve { approval } => answer(&api, &approval, true)?,
        Command::Deny { approval } => answer(&api, &approval, false)?,
        Command::Screen { agent, follow } => {
            let id = agent_id(&api, &agent)?;
            let mut offset = 0;
            loop {
                let screen: api::Screen = api.get(&format!("agents/{id}/screen?since={offset}"))?;
                print!("{}", screen.text);
                let _ = std::io::stdout().flush();
                offset = screen.offset;
                if !follow {
                    break;
                }
                std::thread::sleep(Duration::from_millis(700));
            }
        }
        Command::TakeOver { agent } => {
            let id = agent_id(&api, &agent)?;
            let _: api::AgentView = api.post(&format!("agents/{id}/take-over"), json!({}))?;
            println!(
                "You have {id}'s keyboard: glass type {id} <command> runs a line on its computer; \
                 its new tasks wait until glass hand-back {id}."
            );
        }
        Command::HandBack { agent } => {
            let id = agent_id(&api, &agent)?;
            let _: api::AgentView = api.post(&format!("agents/{id}/hand-back"), json!({}))?;
            println!("Handed {id} back its keyboard.");
        }
        Command::Type { agent, line } => {
            let id = agent_id(&api, &agent)?;
            let before: api::Screen = api.get(&format!("agents/{id}/screen"))?;
            let _: api::Screen = api.post(
                &format!("agents/{id}/input"),
                json!({ "line": line.join(" ") }),
            )?;
            // What it prints, for as long as it keeps printing (a few seconds).
            let mut offset = before.offset;
            let mut quiet = 0;
            while quiet < 4 {
                std::thread::sleep(Duration::from_millis(400));
                let screen: api::Screen = api.get(&format!("agents/{id}/screen?since={offset}"))?;
                if screen.text.is_empty() {
                    quiet += 1;
                } else {
                    quiet = 0;
                    print!("{}", screen.text);
                    let _ = std::io::stdout().flush();
                }
                offset = screen.offset;
            }
        }
        Command::Undo { agent } => {
            let id = agent_id(&api, &agent)?;
            let result: api::UndoResult = api.post(&format!("agents/{id}/undo"), json!({}))?;
            println!("{}", result.text);
            for left in result.left {
                println!("  Not put back: {left}");
            }
        }
        Command::Sleep { agent } => {
            let id = agent_id(&api, &agent)?;
            let view: api::AgentView = api.post(&format!("agents/{id}/sleep"), json!({}))?;
            println!(
                "{} is asleep: {}",
                view.name,
                computer_word(view.computer.state)
            );
        }
        Command::Wake { agent } => {
            let id = agent_id(&api, &agent)?;
            let view: api::AgentView = api.post(&format!("agents/{id}/wake"), json!({}))?;
            println!(
                "{} is awake: its computer is {}",
                view.name,
                computer_word(view.computer.state)
            );
        }
        Command::Login { .. } => unreachable!("handled above"),
    }
    Ok(())
}

fn answer(api: &Api, approval: &str, yes: bool) -> anyhow::Result<()> {
    let answered: api::ApprovalView = api.post(
        &format!("approvals/{approval}"),
        json!({ "yes": yes, "via": "cli" }),
    )?;
    println!(
        "{}: {}",
        if yes { "Approved" } else { "Said no" },
        answered.summary
    );
    Ok(())
}

/// Keeps the address and a token pasted from the web app, readable only
/// by the person.
fn login(url: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let url = url.trim_end_matches('/');
    eprint!("Paste the token from {url} (Settings, Command line): ");
    let _ = std::io::stderr().flush();
    let mut token = String::new();
    std::io::stdin().lock().read_line(&mut token)?;
    let token = token.trim();
    if token.is_empty() {
        bail!("no token");
    }
    let check = Api(AgentsClient::new(url, token).map_err(|e| anyhow::anyhow!("{e}"))?);
    let session: api::Session = check.get("session")?;
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(json!({ "url": url, "token": token }).to_string().as_bytes())?;
    println!("Signed in as {} at {}", session.person.name, session.org);
    Ok(())
}
