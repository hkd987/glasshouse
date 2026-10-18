//! `glassctl`: hand off tasks, watch agent tiles, answer approvals, take
//! over, undo. A terminal front end to the same control API the desktop shell
//! uses. Add `--json` to any command for machine-readable output.

#![allow(clippy::print_stdout)]

mod render;
mod style;

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use glass_api::{
    AgentSummary, AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalView, AuditVerifyResult,
    Event, HandoffCard, PowerResult, ScopeRequest, SystemStatus, TaskDetail, TaskRef, TaskRequest,
    TaskView, TileState, UndoReport, UndoRequest, methods,
};
use glass_audit::AuditRecord;
use glass_rpc::Client;
use glass_types::{ApprovalId, TaskId, Tier};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Debug, Parser)]
#[command(
    name = "glassctl",
    version,
    about = "Hand off work to agents, watch them, approve, take over and undo"
)]
struct Cli {
    /// The glassd control socket.
    #[arg(long, global = true, env = glass_api::CONTROL_SOCKET_ENV, default_value = glass_api::DEFAULT_CONTROL_SOCKET)]
    socket: PathBuf,
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Daemon status: sandbox backend, policy source, counts.
    Status,
    /// Agents you may hand work to.
    Agents,
    /// Show the handoff card for a task without starting it.
    Plan(TaskArgs),
    /// Show the handoff card, then start the task.
    Run {
        #[command(flatten)]
        task: TaskArgs,
        /// Start without asking.
        #[arg(long, short)]
        yes: bool,
    },
    /// Your agents' tiles.
    #[command(alias = "ps")]
    Tasks,
    /// A task's expanded view: now, activity, changes, limits, timeline.
    Show {
        task: String,
    },
    /// Live tiles. Ctrl-C to exit.
    Watch,
    /// Asks waiting for you.
    Approvals,
    /// Approve an ask, once or for the rest of its task.
    Approve {
        approval: String,
        /// Allow this action for the rest of the task.
        #[arg(long)]
        for_task: bool,
    },
    /// Decline an ask. The agent continues without it.
    Deny {
        approval: String,
    },
    /// Hold the agent at its next safe point.
    Pause {
        task: String,
    },
    Resume {
        task: String,
    },
    /// Take control from the agent (it pauses).
    TakeOver {
        task: String,
    },
    /// Give control back; the agent learns what you changed.
    HandBack {
        task: String,
    },
    /// Stop the agent. Its work stays, and stays undoable.
    Stop {
        task: String,
    },
    /// Remove a finished tile.
    Dismiss {
        task: String,
    },
    /// Roll a task back: everything, or to a timeline marker.
    Undo {
        task: String,
        /// Timeline marker to roll back to (see `glassctl show`).
        #[arg(long)]
        to: Option<String>,
        /// Show what would change without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Rebuild a task end to end from the audit log.
    Audit {
        task: String,
    },
    /// Verify the whole audit log's hash chain (root).
    Verify,
    /// Wait for a task to reach a state. For scripts.
    Wait {
        task: String,
        #[arg(long, value_enum, default_value = "ended")]
        until: Until,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Policy tools (offline).
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// Machine power events (root; run by the rc.d suspend/resume hooks).
    #[command(subcommand)]
    System(SystemCommand),
}

#[derive(Debug, Subcommand)]
enum SystemCommand {
    /// The machine is about to sleep: pause running agents (PM-5).
    Suspend,
    /// The machine woke: agents paused for sleep carry on.
    Resume,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Until {
    NeedsYou,
    Ended,
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    /// Print the built-in default policy.
    Default,
    /// Validate a policy file and show risky rules.
    Check { file: PathBuf },
}

#[derive(Debug, clap::Args)]
struct TaskArgs {
    /// Agent id, from `glassctl agents`.
    agent: String,
    /// What to do, in plain language.
    #[arg(required = true)]
    prompt: Vec<String>,
    /// A folder the agent may read. Repeatable.
    #[arg(long, value_name = "DIR")]
    read: Vec<PathBuf>,
    /// A folder the agent may change (through undoable actions). Repeatable.
    #[arg(long, value_name = "DIR")]
    write: Vec<PathBuf>,
    /// Block an action or pattern for this task, such as `mail.send`.
    #[arg(long, value_name = "ACTION")]
    block: Vec<String>,
    /// Make an action ask first for this task.
    #[arg(long, value_name = "ACTION")]
    ask: Vec<String>,
}

impl TaskArgs {
    fn request(&self) -> anyhow::Result<TaskRequest> {
        let absolute = |paths: &[PathBuf]| -> anyhow::Result<Vec<PathBuf>> {
            paths
                .iter()
                .map(|p| {
                    std::fs::canonicalize(p)
                        .with_context(|| format!("{} does not exist", p.display()))
                })
                .collect()
        };
        let restrict = self
            .block
            .iter()
            .map(|a| (a.clone(), Tier::Never))
            .chain(self.ask.iter().map(|a| (a.clone(), Tier::Ask)))
            .collect();
        Ok(TaskRequest {
            agent: self.agent.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
            prompt: self.prompt.join(" "),
            scope: ScopeRequest {
                read: absolute(&self.read)?,
                write: absolute(&self.write)?,
                restrict,
            },
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glassctl: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

struct Ctl {
    client: Client,
    json: bool,
}

impl Ctl {
    async fn call<R: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> anyhow::Result<R> {
        self.client
            .call(method, params)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Prints `value` as JSON, or as `text` otherwise.
    fn show<T: Serialize>(&self, value: &T, text: impl FnOnce(&T) -> String) -> anyhow::Result<()> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else {
            print!("{}", text(value));
        }
        Ok(())
    }

    /// Resolves a short or full task id among the caller's tasks.
    async fn task(&self, reference: &str) -> anyhow::Result<TaskId> {
        let tasks: Vec<TaskView> = self.call(methods::TASK_LIST, Value::Null).await?;
        pick(
            tasks
                .iter()
                .map(|t| t.id)
                .filter(|id| id.matches_ref(reference)),
            "task",
            reference,
        )
    }

    async fn approval(&self, reference: &str) -> anyhow::Result<ApprovalId> {
        let asks: Vec<ApprovalView> = self.call(methods::APPROVAL_LIST, Value::Null).await?;
        pick(
            asks.iter()
                .map(|a| a.id)
                .filter(|id| id.matches_ref(reference)),
            "pending approval",
            reference,
        )
    }

    async fn task_call<R: DeserializeOwned>(
        &self,
        method: &str,
        reference: &str,
    ) -> anyhow::Result<R> {
        let task = self.task(reference).await?;
        self.call(method, TaskRef { task }).await
    }
}

fn pick<T: Copy>(
    mut matches: impl Iterator<Item = T>,
    kind: &str,
    reference: &str,
) -> anyhow::Result<T> {
    match (matches.next(), matches.next()) {
        (Some(one), None) => Ok(one),
        (None, _) => bail!("no {kind} {reference}"),
        (Some(_), Some(_)) => bail!("{reference} matches more than one {kind}; use more digits"),
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    if let Command::Policy(cmd) = &cli.command {
        return policy(cmd);
    }
    let client = Client::connect(&cli.socket)
        .await
        .with_context(|| "is glassd running? (service glassd status)".to_string())?;
    let ctl = Ctl {
        client,
        json: cli.json,
    };

    match cli.command {
        Command::Status => {
            let status: SystemStatus = ctl.call(methods::SYSTEM_STATUS, Value::Null).await?;
            ctl.show(&status, |s| {
                format!(
                    "glassd {}\n  sandbox   {}{}\n  policy    {}\n  running   {} agents\n  waiting   {} approvals\n",
                    s.version,
                    s.sandbox,
                    if s.sandbox == "freebsd-jail" { String::new() } else { style::red("  (NOT A SECURITY BOUNDARY)") },
                    s.policy,
                    s.tasks_running,
                    s.approvals_pending
                )
            })
        }
        Command::Agents => {
            let agents: Vec<AgentSummary> = ctl.call(methods::AGENTS_LIST, Value::Null).await?;
            ctl.show(&agents, |list| {
                list.iter()
                    .map(|a| {
                        format!(
                            "{}  {}\n  {}\n",
                            style::bold(a.id.as_str()),
                            a.name,
                            style::dim(&a.description)
                        )
                    })
                    .collect()
            })
        }
        Command::Plan(args) => {
            let card: HandoffCard = ctl.call(methods::TASK_PLAN, args.request()?).await?;
            ctl.show(&card, render::card)
        }
        Command::Run { task, yes } => {
            let request = task.request()?;
            let card: HandoffCard = ctl.call(methods::TASK_PLAN, &request).await?;
            if !ctl.json {
                print!("{}", render::card(&card));
            }
            if !yes && !confirm("\nStart this task?")? {
                bail!("not started");
            }
            let view: TaskView = ctl.call(methods::TASK_START, &request).await?;
            ctl.show(&view, |v| {
                format!(
                    "\nStarted {} ({}). Watch it with `glassctl watch`.\n",
                    v.agent_name,
                    v.id.short()
                )
            })
        }
        Command::Tasks => {
            let tasks: Vec<TaskView> = ctl.call(methods::TASK_LIST, Value::Null).await?;
            ctl.show(&tasks, |t| render::tiles(t))
        }
        Command::Show { task } => {
            let detail: TaskDetail = ctl.task_call(methods::TASK_GET, &task).await?;
            ctl.show(&detail, render::detail)
        }
        Command::Watch => watch(&ctl).await,
        Command::Approvals => {
            let asks: Vec<ApprovalView> = ctl.call(methods::APPROVAL_LIST, Value::Null).await?;
            ctl.show(&asks, |a| render::approvals(a))
        }
        Command::Approve { approval, for_task } => {
            let scope = if for_task {
                ApprovalScope::Task
            } else {
                ApprovalScope::Once
            };
            answer(&ctl, &approval, ApprovalAnswer::Approve { scope }).await
        }
        Command::Deny { approval } => answer(&ctl, &approval, ApprovalAnswer::Deny).await,
        Command::Pause { task } => lifecycle(&ctl, methods::TASK_PAUSE, &task).await,
        Command::Resume { task } => lifecycle(&ctl, methods::TASK_RESUME, &task).await,
        Command::TakeOver { task } => lifecycle(&ctl, methods::TASK_TAKE_OVER, &task).await,
        Command::HandBack { task } => lifecycle(&ctl, methods::TASK_HAND_BACK, &task).await,
        Command::Stop { task } => lifecycle(&ctl, methods::TASK_STOP, &task).await,
        Command::Dismiss { task } => ctl.task_call(methods::TASK_DISMISS, &task).await,
        Command::Undo { task, to, dry_run } => {
            let id = ctl.task(&task).await?;
            let to = match to {
                Some(marker) => {
                    let detail: TaskDetail =
                        ctl.call(methods::TASK_GET, TaskRef { task: id }).await?;
                    Some(pick(
                        detail
                            .snapshots
                            .iter()
                            .map(|s| s.id)
                            .filter(|s| s.matches_ref(&marker)),
                        "timeline marker",
                        &marker,
                    )?)
                }
                None => None,
            };
            let report: UndoReport = ctl
                .call(
                    methods::TASK_UNDO,
                    UndoRequest {
                        task: id,
                        to,
                        dry_run,
                    },
                )
                .await?;
            ctl.show(&report, render::undo)
        }
        Command::Audit { task } => {
            let records: Vec<AuditRecord> = ctl.task_call(methods::AUDIT_TASK, &task).await?;
            ctl.show(&records, |r| render::audit(r))
        }
        Command::Verify => {
            let result: AuditVerifyResult = ctl.call(methods::AUDIT_VERIFY, Value::Null).await?;
            ctl.show(&result, |r| {
                format!(
                    "Audit log intact: {} records, chain head {}\n",
                    r.records,
                    &r.last_hash[..16]
                )
            })
        }
        Command::Wait {
            task,
            until,
            timeout,
        } => wait(&ctl, &task, until, timeout).await,
        Command::System(cmd) => {
            let (method, verb) = match cmd {
                SystemCommand::Suspend => (methods::SYSTEM_SUSPEND, "Paused"),
                SystemCommand::Resume => (methods::SYSTEM_RESUME, "Resumed"),
            };
            let result: PowerResult = ctl.call(method, Value::Null).await?;
            ctl.show(&result, |r| {
                let noun = if r.tasks == 1 { "agent" } else { "agents" };
                format!("{verb} {} {noun}\n", r.tasks)
            })
        }
        Command::Policy(_) => unreachable!("handled before connecting"),
    }
}

async fn lifecycle(ctl: &Ctl, method: &str, task: &str) -> anyhow::Result<()> {
    let view: TaskView = ctl.task_call(method, task).await?;
    ctl.show(&view, render::tile)
}

async fn answer(ctl: &Ctl, reference: &str, answer: ApprovalAnswer) -> anyhow::Result<()> {
    let approval = ctl.approval(reference).await?;
    let view: ApprovalView = ctl
        .call(
            methods::APPROVAL_ANSWER,
            AnswerApproval { approval, answer },
        )
        .await?;
    ctl.show(&view, |v| {
        let how = match answer {
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            } => "Approved once",
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Task,
            } => "Approved for the rest of the task",
            ApprovalAnswer::Deny => "Declined",
        };
        format!("{how}: {}\n", v.summary)
    })
}

async fn watch(ctl: &Ctl) -> anyhow::Result<()> {
    let tasks: Vec<TaskView> = ctl.call(methods::TASK_LIST, Value::Null).await?;
    let mut tiles: BTreeMap<TaskId, TaskView> = tasks.into_iter().map(|t| (t.id, t)).collect();
    let _: Value = ctl.call(methods::EVENTS_SUBSCRIBE, Value::Null).await?;
    let redraw = |tiles: &BTreeMap<TaskId, TaskView>| {
        let views: Vec<TaskView> = tiles.values().cloned().collect();
        print!(
            "{}{}\n{}",
            style::clear_screen(),
            style::bold("Agents"),
            render::tiles(&views)
        );
        let _ = std::io::stdout().flush();
    };
    let interactive = std::io::stdout().is_terminal() && !ctl.json;
    if interactive {
        redraw(&tiles);
    }
    while let Some(note) = ctl.client.next_notification().await {
        let Some(params) = note.params else { continue };
        let Ok(event) = serde_json::from_value::<Event>(params.clone()) else {
            continue;
        };
        if ctl.json {
            println!("{params}");
            continue;
        }
        match event {
            Event::TaskUpdated { task } => {
                if !interactive {
                    println!(
                        "{} {} {}: {}",
                        task.id.short(),
                        task.agent,
                        task.state.label(),
                        task.caption
                    );
                }
                tiles.insert(task.id, *task);
            }
            Event::TaskRemoved { task } => {
                tiles.remove(&task);
            }
        }
        if interactive {
            redraw(&tiles);
        }
    }
    Ok(())
}

async fn wait(ctl: &Ctl, reference: &str, until: Until, timeout: u64) -> anyhow::Result<()> {
    let task = ctl.task(reference).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    loop {
        let detail: TaskDetail = ctl.call(methods::TASK_GET, TaskRef { task }).await?;
        let reached = match until {
            Until::NeedsYou => detail.view.state == TileState::NeedsYou,
            Until::Ended => !detail.view.state.is_live(),
        };
        if reached || (matches!(until, Until::NeedsYou) && !detail.view.state.is_live()) {
            return ctl.show(&detail.view, render::tile);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "timed out waiting; the task is {}",
                detail.view.state.label()
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not a terminal; pass --yes to start without confirming");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn policy(cmd: &PolicyCommand) -> anyhow::Result<()> {
    match cmd {
        PolicyCommand::Default => {
            print!("{}", glass_policy::DEFAULT_POLICY);
            Ok(())
        }
        PolicyCommand::Check { file } => {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()))?;
            let policy = glass_policy::Policy::from_toml(&text)?;
            let mut registry = glass_actions::Registry::new();
            registry.register(std::sync::Arc::new(glass_actions::providers::FilesProvider))?;
            registry.register(std::sync::Arc::new(glass_actions::providers::MailProvider))?;
            let warnings = policy.lint(&registry.lint_info());
            println!(
                "{} is valid: {} groups, {} agents.",
                file.display(),
                policy.document().groups.len(),
                policy.document().agents.len()
            );
            for warning in &warnings {
                println!("  warning: {warning}");
            }
            Ok(())
        }
    }
}
