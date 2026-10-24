//! `glassctl`: hand off tasks, watch agent tiles, answer approvals, take
//! over, undo. A terminal front end to the same control API the desktop shell
//! uses. Add `--json` to any command for machine-readable output.

#![allow(clippy::print_stdout)]

mod render;
mod style;
mod tailor;

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use glass_api::{
    AgentSummary, AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalView, AuditVerifyResult,
    DisplaySafe, Event, HandoffCard, PowerResult, ProfileApp, ProfileAppKind, RoutineDraft,
    RoutineFromTask, RoutineRef, RoutineView, ScopeRequest, SystemStatus, TaskDetail, TaskRef,
    TaskRequest, TaskView, TileState, UndoReport, UndoRequest, methods,
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
    /// The apps in your profile, which IT's policy decides.
    Apps,
    /// Show the handoff card for a task without starting it.
    Plan(TaskArgs),
    /// Show the handoff card, then start the task.
    Run {
        #[command(flatten)]
        task: TaskArgs,
        /// Start without asking.
        #[arg(long, short)]
        yes: bool,
        /// For an agent driven by your own harness: run its driver as you
        /// until it's done, then hand the task back (ADR 0042).
        #[arg(long)]
        drive: bool,
    },
    /// Your agents' tiles.
    #[command(alias = "ps")]
    Tasks {
        /// One line of counts by state, for the login screen.
        #[arg(long)]
        summary: bool,
    },
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
    /// Take control from the agent (it pauses). On an ended task, carry on
    /// in your own harness, such as Claude Code, with what the agent did.
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
    /// Start an ended task's request again as a new task.
    Retry {
        task: String,
    },
    /// Roll a task back: everything, to a timeline marker, or single changes.
    Undo {
        task: String,
        /// Timeline marker to roll back to (see `glassctl show`).
        #[arg(long)]
        to: Option<String>,
        /// Undo only this change, by its number in `glassctl show`. Repeatable.
        #[arg(long = "change", value_name = "N")]
        changes: Vec<u32>,
        /// Show what would change without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show or change your layer on an agent: your settings and limits,
    /// saved as versions on top of the published agent.
    Tailor(tailor::TailorArgs),
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
    /// Your routines: tasks you saved to run again, by themselves or when
    /// you start them (DA-4).
    Routines,
    /// Start a routine, or make one from a task you ran: this computer
    /// writes its code from the task's steps, with no model.
    #[command(subcommand)]
    Routine(RoutineCommand),
    /// Policy tools (offline).
    #[command(subcommand)]
    Policy(PolicyCommand),
    /// This computer's parts and whether this OS drives each: for checking
    /// a laptop before a pilot (DK-12). As root, to read USB devices and
    /// the battery.
    Hardware {
        /// Judge another computer from what `tests/hardware/capture.sh`
        /// saved there, instead of this one.
        #[arg(long)]
        from: Option<PathBuf>,
        #[command(subcommand)]
        patch: Option<HardwareCommand>,
    },
    /// Machine power events (root; run by the rc.d suspend/resume hooks).
    #[command(subcommand)]
    System(SystemCommand),
}

#[derive(Debug, Subcommand)]
enum RoutineCommand {
    /// Start a routine now, by its id from `glassctl routines`.
    Run { routine: String },
    /// Show the code this computer writes from a task's steps.
    Draft { task: String },
    /// Save a task as a routine, with the code this computer writes from
    /// its steps.
    Save {
        task: String,
        /// What to call it.
        #[arg(long)]
        name: String,
        /// When it runs by itself: "weekdays 09:00", "daily 18:30",
        /// "mon,thu 08:30" or "hourly". Without one, when you start it.
        #[arg(long)]
        schedule: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum HardwareCommand {
    /// Kernel patches (root; ADR 0057): set one up to try at the next
    /// start, see where it is, or take it back.
    #[command(subcommand)]
    Patch(PatchCommand),
}

#[derive(Debug, Subcommand)]
enum PatchCommand {
    /// Check a patch's bundle and set it up in a boot environment of its
    /// own; the next start tries it, once.
    Apply { bundle: PathBuf },
    /// The patch being tried, and the ones tried before.
    Status,
    /// Take back a patch set up but not yet tried.
    Cancel,
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
    /// A short name for the task. Without one, it's named from the prompt.
    #[arg(long)]
    title: Option<String>,
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
            title: self.title.clone(),
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // An error can carry the server's words.
            eprintln!("glassctl: {}", glass_api::display_line(&format!("{e:#}")));
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

    /// Prints `value` as JSON, or as `text` otherwise. Either way nothing
    /// in it reaches the terminal as a control or direction character.
    fn show<T: Serialize + DeserializeOwned>(
        &self,
        value: &T,
        text: impl FnOnce(&T) -> String,
    ) -> anyhow::Result<()> {
        if self.json {
            println!("{}", render::json(value, true)?);
        } else {
            print!("{}", text(&render::display_safe(value)?));
        }
        Ok(())
    }

    /// Resolves a short or full task id among the caller's tasks.
    /// A task glassd may no longer hold, such as one a routine is made
    /// from: its full id as given, else as `task` finds it.
    async fn ended_task(&self, reference: &str) -> anyhow::Result<TaskId> {
        match reference.parse::<TaskId>() {
            Ok(id) => Ok(id),
            Err(_) => self.task(reference).await,
        }
    }

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
    if let Command::Hardware { from, patch: None } = &cli.command {
        let report = match from {
            Some(dir) => {
                glass_platform::hardware::assess(&glass_platform::hardware::Sources::read_dir(dir)?)
            }
            None => glass_platform::hardware::report()?,
        };
        if cli.json {
            println!("{}", render::json(&report, true)?);
        } else {
            print!("{}", render::hardware(&render::display_safe(&report)?));
        }
        return Ok(());
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
                    if s.contained { String::new() } else { style::red("  (NOT A SECURITY BOUNDARY)") },
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
        Command::Apps => {
            let apps: Vec<ProfileApp> = ctl.call(methods::APPS_LIST, Value::Null).await?;
            ctl.show(&apps, |list| {
                list.iter()
                    .map(|a| {
                        let kind = match a.kind {
                            ProfileAppKind::Web => "app",
                            ProfileAppKind::Native => "native app",
                            ProfileAppKind::Console => "admin console",
                        };
                        let url = a
                            .url
                            .as_deref()
                            .map(|u| format!("  {u}"))
                            .unwrap_or_default();
                        format!(
                            "{}  {}  {}{}\n",
                            style::bold(a.id.as_str()),
                            a.name,
                            style::dim(kind),
                            style::dim(&url)
                        )
                    })
                    .collect()
            })
        }
        Command::Plan(args) => {
            let card: HandoffCard = ctl.call(methods::TASK_PLAN, args.request()?).await?;
            ctl.show(&card, render::card)
        }
        Command::Run { task, yes, drive } => {
            let request = task.request()?;
            let card: HandoffCard = ctl.call(methods::TASK_PLAN, &request).await?;
            if !ctl.json {
                print!("{}", render::card(&render::display_safe(&card)?));
            }
            if !yes && !confirm("\nStart this task?")? {
                bail!("not started");
            }
            let view: TaskView = ctl.call(methods::TASK_START, &request).await?;
            let view = until_started(&ctl, view).await?;
            if drive {
                return drive_task(&ctl, view, &request.prompt).await;
            }
            ctl.show(&view, |v| {
                if v.driver_socket.is_some() {
                    format!("\n{}", render::connect(v))
                } else {
                    format!(
                        "\nStarted {} ({}). Watch it with `glassctl watch`.\n",
                        v.agent_name,
                        v.id.short()
                    )
                }
            })
        }
        Command::Tasks { summary } => {
            let tasks: Vec<TaskView> = ctl.call(methods::TASK_LIST, Value::Null).await?;
            if summary {
                ctl.show(&tasks, |t| render::summary(t))
            } else {
                ctl.show(&tasks, |t| render::tiles(t))
            }
        }
        Command::Show { task } => {
            let detail: TaskDetail = ctl.task_call(methods::TASK_GET, &task).await?;
            if ctl.json {
                return ctl.show(&detail, render::detail);
            }
            print!("{}", render::detail_shown(detail));
            Ok(())
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
        Command::TakeOver { task } => {
            let detail: TaskDetail = ctl.task_call(methods::TASK_GET, &task).await?;
            if detail.view.state.is_live() {
                lifecycle(&ctl, methods::TASK_TAKE_OVER, &task).await
            } else {
                let view: TaskView = ctl
                    .call(
                        methods::TASK_CONTINUE,
                        TaskRef {
                            task: detail.view.id,
                        },
                    )
                    .await?;
                let view = until_started(&ctl, view).await?;
                ctl.show(&view, render::connect)
            }
        }
        Command::HandBack { task } => lifecycle(&ctl, methods::TASK_HAND_BACK, &task).await,
        Command::Stop { task } => lifecycle(&ctl, methods::TASK_STOP, &task).await,
        Command::Dismiss { task } => ctl.task_call(methods::TASK_DISMISS, &task).await,
        Command::Routines => {
            let routines: Vec<RoutineView> = ctl.call(methods::ROUTINE_LIST, Value::Null).await?;
            ctl.show(&routines, |list| {
                if list.is_empty() {
                    return "No routines yet. Ask the desktop agent to do something every \
                            morning, or again later, and it offers to save one.\n"
                        .to_owned();
                }
                list.iter()
                    .map(|r| {
                        format!(
                            "{}  {} (version {})\n  Runs {}. {}\n  Calls: {}\n",
                            style::bold(&r.routine),
                            r.name,
                            r.version,
                            r.runs,
                            style::dim(&r.request),
                            if r.calls.is_empty() {
                                "nothing".to_owned()
                            } else {
                                r.calls.join(", ")
                            }
                        )
                    })
                    .collect()
            })
        }
        Command::Routine(RoutineCommand::Draft { task }) => {
            let task = ctl.ended_task(&task).await?;
            let draft: RoutineDraft = ctl.call(methods::ROUTINE_DRAFT, TaskRef { task }).await?;
            if ctl.json {
                return ctl.show(&draft, |_| String::new());
            }
            print!("{}", render::routine_draft(&draft));
            Ok(())
        }
        Command::Routine(RoutineCommand::Save {
            task,
            name,
            schedule,
        }) => {
            let task = ctl.ended_task(&task).await?;
            let saved: RoutineView = ctl
                .call(
                    methods::ROUTINE_SAVE,
                    RoutineFromTask {
                        task,
                        name,
                        schedule,
                    },
                )
                .await?;
            ctl.show(&saved, |r| {
                format!(
                    "Saved the routine {} ({}). It runs {}, with no model; \
                     `glassctl routine run {}` starts it now.\n",
                    r.name, r.routine, r.runs, r.routine
                )
            })
        }
        Command::Routine(RoutineCommand::Run { routine }) => {
            let view: TaskView = ctl
                .call(methods::ROUTINE_RUN, RoutineRef { routine })
                .await?;
            ctl.show(&view, |v| {
                format!(
                    "Started the routine {} ({}). Watch it with `glassctl watch`.\n",
                    v.title,
                    v.id.short()
                )
            })
        }
        Command::Retry { task } => {
            let view: TaskView = ctl.task_call(methods::TASK_RETRY, &task).await?;
            ctl.show(&view, |v| {
                format!(
                    "Started again as {}. Watch it with `glassctl watch`.\n",
                    v.id.short()
                )
            })
        }
        Command::Undo {
            task,
            to,
            changes,
            dry_run,
        } => {
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
                        changes,
                        dry_run,
                    },
                )
                .await?;
            ctl.show(&report, render::undo)
        }
        Command::Tailor(args) => tailor::run(&ctl, args).await,
        Command::Audit { task } => {
            let records: Vec<AuditRecord> = ctl.task_call(methods::AUDIT_TASK, &task).await?;
            ctl.show(&records, |r| render::audit(r))
        }
        Command::Verify => {
            let result: AuditVerifyResult = ctl.call(methods::AUDIT_VERIFY, Value::Null).await?;
            ctl.show(&result, |r| {
                let unfinished = match r.unfinished {
                    0 => String::new(),
                    n => format!(", then {n} bytes of a record not finished"),
                };
                format!(
                    "Audit log intact: {} records, chain head {}{unfinished}\n",
                    r.records,
                    r.last_hash.get(..16).unwrap_or(&r.last_hash)
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
        Command::Hardware {
            patch: Some(HardwareCommand::Patch(cmd)),
            ..
        } => {
            let status: glass_api::PatchStatus = match cmd {
                PatchCommand::Apply { bundle } => {
                    // glassd reads the bundle: by its absolute path.
                    let bundle = std::fs::canonicalize(&bundle)
                        .with_context(|| format!("{}", bundle.display()))?;
                    ctl.call(
                        methods::HARDWARE_PATCH_APPLY,
                        glass_api::PatchApply { bundle },
                    )
                    .await?
                }
                PatchCommand::Status => {
                    ctl.call(methods::HARDWARE_PATCH_STATUS, Value::Null)
                        .await?
                }
                PatchCommand::Cancel => {
                    ctl.call(methods::HARDWARE_PATCH_CANCEL, Value::Null)
                        .await?
                }
            };
            ctl.show(&status, render::patch_status)
        }
        Command::Policy(_) | Command::Hardware { .. } => {
            unreachable!("handled before connecting")
        }
    }
}

/// For a task the user's harness drives, waits until its socket is up
/// (the task has left Starting), so the connect command works at once.
async fn until_started(ctl: &Ctl, view: TaskView) -> anyhow::Result<TaskView> {
    if view.driver_socket.is_none() {
        return Ok(view);
    }
    for _ in 0..100 {
        let detail: TaskDetail = ctl
            .call(methods::TASK_GET, TaskRef { task: view.id })
            .await?;
        if detail.view.state != TileState::Starting {
            return Ok(detail.view);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(view)
}

/// Runs an attended agent's driver as the person, then hands the task back
/// with its answer, or stops it if the driver couldn't finish (ADR 0042).
/// glassd never starts a driver: it runs here, as whoever asked.
async fn drive_task(ctl: &Ctl, view: TaskView, prompt: &str) -> anyhow::Result<()> {
    let Some(socket) = &view.driver_socket else {
        bail!(
            "{} runs on its own; --drive is for agents your harness drives",
            view.agent_name
        );
    };
    let agents: Vec<AgentSummary> = ctl.call(methods::AGENTS_LIST, Value::Null).await?;
    let Some(driver) = agents
        .into_iter()
        .find(|a| a.id == view.agent)
        .and_then(|a| a.driver)
    else {
        let _: TaskView = ctl
            .call(methods::TASK_STOP, TaskRef { task: view.id })
            .await?;
        bail!(
            "{} has no driver; connect your harness with `glassctl run` instead",
            view.agent_name
        );
    };
    let status = tokio::process::Command::new(&driver)
        .env("GLASS_AGENT_SOCKET", socket)
        .env("GLASS_TASK_ID", view.id.to_string())
        .env("GLASS_TASK_PROMPT", prompt)
        .stdin(std::process::Stdio::null())
        .status()
        .await
        .with_context(|| format!("running {driver}"))?;
    let task = TaskRef { task: view.id };
    if !status.success() {
        let ended: TaskView = ctl.call(methods::TASK_STOP, task).await?;
        ctl.show(&ended, render::tile)?;
        bail!("{} didn't finish", view.agent_name);
    }
    let _: TaskView = ctl.call(methods::TASK_HAND_BACK, &task).await?;
    let detail: TaskDetail = ctl.call(methods::TASK_GET, task).await?;
    ctl.show(&detail, render::detail)
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

/// Live tiles. Everything here is only shown, so text is made display text
/// as it arrives.
async fn watch(ctl: &Ctl) -> anyhow::Result<()> {
    let tasks: Vec<TaskView> = ctl.call(methods::TASK_LIST, Value::Null).await?;
    let tasks = render::display_safe(&tasks)?;
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
        let Some(mut params) = note.params else {
            continue;
        };
        if !ctl.json {
            params.make_display_safe();
        }
        let Ok(event) = serde_json::from_value::<Event>(params.clone()) else {
            continue;
        };
        if ctl.json {
            println!("{}", render::json(&params, false)?);
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
            // The desktop's business, not a tile's.
            Event::SettingsChanged => continue,
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
            let registry = glass_first_party::registry(glass_first_party::Setup::offline())?;
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
