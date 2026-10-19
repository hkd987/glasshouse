//! What Activity Monitor shows, from one sample of the machine: the
//! person's agents, each with everything running in its jail, then their
//! apps and other processes. Pure, so it's tested without a machine.

use glass_api::{TaskView, TileState};
use glass_platform::system::{Memory, UserProcess};
use glass_types::TaskId;

/// One sample, as the window shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// How busy the processors were since the last sample; `None` until
    /// there are two.
    pub cpu_percent: Option<f64>,
    pub memory: Memory,
    pub agents: Vec<AgentRow>,
    pub apps: Vec<AppRow>,
    /// glassd answered, so the agents are known.
    pub agents_known: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRow {
    /// `None` for processes in a jail no live task of the person's names.
    pub task: Option<TaskId>,
    pub title: String,
    pub agent: String,
    pub state: Option<TileState>,
    pub cpu_percent: f64,
    pub memory_kb: u64,
    pub processes: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppRow {
    pub pid: u32,
    pub name: String,
    /// "App", "System" (keeps the session up) or "Process".
    pub kind: &'static str,
    pub cpu_percent: f64,
    pub memory_kb: u64,
    /// The desktop itself and this window's own sampling can't be quit
    /// from here.
    pub quittable: bool,
}

/// Apps people know by name.
fn app_name(command: &str) -> Option<&'static str> {
    match command {
        "glass-terminal" => Some("Terminal"),
        "glass-activity" => Some("Activity Monitor"),
        "glass-hello" => Some("Hello"),
        _ => None,
    }
}

/// Processes that keep the session up: quitting them would sign the
/// person out.
fn keeps_session(command: &str) -> bool {
    matches!(command, "glass-shell-fb" | "su" | "login")
}

/// The jail an agent's task runs in (the runtime names it so).
fn jail_of(task: TaskId) -> String {
    format!("glass-{}", task.short())
}

/// Sorts processes into the person's live agents and everything else.
/// `own` is this app's pid: it shows, but its sampling (`ps`) doesn't.
pub fn group(
    processes: &[UserProcess],
    tasks: &[TaskView],
    own: u32,
) -> (Vec<AgentRow>, Vec<AppRow>) {
    let mut agents: Vec<(String, AgentRow)> = tasks
        .iter()
        .filter(|t| t.state.is_live())
        .map(|t| {
            (
                jail_of(t.id),
                AgentRow {
                    task: Some(t.id),
                    title: t.title.clone(),
                    agent: t.agent_name.clone(),
                    state: Some(t.state),
                    cpu_percent: 0.0,
                    memory_kb: 0,
                    processes: 0,
                },
            )
        })
        .collect();
    let mut apps = Vec::new();
    for p in processes {
        match &p.jail {
            Some(jail) => {
                let at = match agents.iter().position(|(j, _)| j == jail) {
                    Some(at) => at,
                    None => {
                        agents.push((
                            jail.clone(),
                            AgentRow {
                                task: None,
                                title: "An agent that has ended".into(),
                                agent: String::new(),
                                state: None,
                                cpu_percent: 0.0,
                                memory_kb: 0,
                                processes: 0,
                            },
                        ));
                        agents.len() - 1
                    }
                };
                let row = &mut agents[at].1;
                row.cpu_percent += p.cpu_percent;
                row.memory_kb += p.memory_kb;
                row.processes += 1;
            }
            None if p.command == "ps" => {}
            None => apps.push(AppRow {
                pid: p.pid,
                name: match (app_name(&p.command), p.command.as_str()) {
                    (Some(name), _) => name.to_owned(),
                    (None, "glass-shell-fb") => "Desktop".to_owned(),
                    (None, command) => command.to_owned(),
                },
                kind: match (app_name(&p.command), keeps_session(&p.command)) {
                    (Some(_), _) => "App",
                    (None, true) => "System",
                    (None, false) => "Process",
                },
                cpu_percent: p.cpu_percent,
                memory_kb: p.memory_kb,
                quittable: p.pid != own && !keeps_session(&p.command),
            }),
        }
    }
    let mut agents: Vec<AgentRow> = agents.into_iter().map(|(_, row)| row).collect();
    agents.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
    apps.sort_by(|a, b| {
        b.cpu_percent
            .total_cmp(&a.cpu_percent)
            .then(b.memory_kb.cmp(&a.memory_kb))
    });
    (agents, apps)
}

/// "0.4%", "12%" past ten.
pub fn cpu_text(percent: f64) -> String {
    if percent >= 10.0 {
        format!("{percent:.0}%")
    } else {
        format!("{percent:.1}%")
    }
}

/// "840 KB", "24 MB", "1.2 GB".
pub fn memory_text(kb: u64) -> String {
    match kb {
        0..1024 => format!("{kb} KB"),
        1024..1_048_576 => format!("{} MB", kb / 1024),
        _ => format!("{:.1} GB", kb as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, jail: Option<&str>, cpu: f64, kb: u64, command: &str) -> UserProcess {
        UserProcess {
            pid,
            jail: jail.map(str::to_owned),
            cpu_percent: cpu,
            memory_kb: kb,
            command: command.into(),
        }
    }

    fn task(state: TileState) -> TaskView {
        let mut view = glass_shell::fixtures::tasks().remove(0);
        view.id = TaskId::new();
        view.state = state;
        view
    }

    #[test]
    fn agents_add_up_their_jails_and_apps_stand_alone() {
        let busy = task(TileState::Working);
        let ended = task(TileState::Done);
        let jail = jail_of(busy.id);
        let processes = [
            process(10, Some(&jail), 40.0, 2048, "node"),
            process(11, Some(&jail), 10.0, 1024, "git"),
            process(12, Some("glass-0000dead"), 1.0, 10, "sh"),
            process(20, None, 2.0, 30_000, "glass-terminal"),
            process(21, None, 0.0, 9_000, "glass-shell-fb"),
            process(22, None, 0.5, 5_000, "glass-activity"),
            process(23, None, 0.0, 100, "ps"),
        ];
        let (agents, apps) = group(&processes, &[busy.clone(), ended], 22);

        assert_eq!(agents.len(), 2, "the ended task's tile isn't an agent");
        assert_eq!(agents[0].task, Some(busy.id));
        assert_eq!(agents[0].cpu_percent, 50.0);
        assert_eq!((agents[0].memory_kb, agents[0].processes), (3072, 2));
        assert_eq!(agents[0].title, busy.title);
        assert_eq!(agents[1].task, None, "a jail no live task names");

        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Terminal", "Activity Monitor", "Desktop"]);
        let quittable: Vec<bool> = apps.iter().map(|a| a.quittable).collect();
        assert_eq!(quittable, [true, false, false]);
        let kinds: Vec<&str> = apps.iter().map(|a| a.kind).collect();
        assert_eq!(kinds, ["App", "App", "System"]);
    }

    #[test]
    fn a_live_agent_shows_before_its_processes_start() {
        let (agents, apps) = group(&[], &[task(TileState::NeedsYou)], 1);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].processes, 0);
        assert!(apps.is_empty());
    }

    #[test]
    fn numbers_read_at_a_glance() {
        assert_eq!(cpu_text(0.44), "0.4%");
        assert_eq!(cpu_text(37.6), "38%");
        assert_eq!(memory_text(840), "840 KB");
        assert_eq!(memory_text(24 * 1024 + 5), "24 MB");
        assert_eq!(memory_text(1_258_291), "1.2 GB");
    }
}
