//! Runs an attended task on the person's own Claude Code and Claude plan
//! (ADR 0042, MP-5, HX-1, HX-7). Their client (`glassctl run --drive`, the
//! command bar) starts it as them, never glassd, with the task socket in
//! `GLASS_AGENT_SOCKET`.
//!
//! It launches their unmodified `claude` headless, with their own sign-in
//! and environment: Glasshouse never reads, keeps or passes on their Claude
//! credentials. Claude Code gets no tools of its own (`--tools ""`) and one
//! MCP server, this task's socket through `glass-mcp`, so every step it
//! takes is a call policy decides, asks for, records and can undo. Its
//! first event lists the tools it has: anything but ours, and it is stopped
//! before it does anything. Its steps become the tile's captions and its
//! last word the task's answer.

use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};

use anyhow::{Context, bail};
use glass_agent_sdk::Agent;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// The MCP server's name in Claude Code's config: its tools are
/// `mcp__glasshouse__<tool>`.
const SERVER: &str = "glasshouse";
/// The most turns Claude Code takes before it stops.
const MAX_TURNS: u32 = 30;
/// Tools of ours Claude Code doesn't need: it is a model already.
const DENIED: [&str; 1] = ["model_complete"];

const SYSTEM: &str = "You are working as the Glasshouse desktop agent, for the person \
on this computer, through the glasshouse tools: they are the only way you can see or \
change anything here. Some ask the person first, and policy may refuse a call: a refused \
or declined call comes back as an error. Don't retry it; work around it or say what you \
couldn't do. Keep to what was asked. Call report_progress with a short caption at each \
step. When you're done, answer the person in a few short paragraphs of plain words: what \
you did and what you found.";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-claude-runner: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// What Claude Code's stream says, as far as the runner cares.
#[derive(Debug, PartialEq)]
enum Event {
    /// Its first event: the tools it has and its MCP servers' states.
    Init {
        tools: Vec<String>,
        servers: Vec<(String, String)>,
    },
    /// The tools it is about to use, by name.
    Uses(Vec<String>),
    /// Its last event: the answer, or why it couldn't give one.
    Done { ok: bool, text: String },
}

/// One line of `--output-format stream-json`.
fn event(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    match (v["type"].as_str()?, v["subtype"].as_str()) {
        ("system", Some("init")) => {
            let strings =
                |key: &str| -> Vec<Value> { v[key].as_array().cloned().unwrap_or_default() };
            Some(Event::Init {
                tools: strings("tools")
                    .iter()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect(),
                servers: strings("mcp_servers")
                    .iter()
                    .filter_map(|s| {
                        Some((
                            s["name"].as_str()?.to_owned(),
                            s["status"].as_str()?.to_owned(),
                        ))
                    })
                    .collect(),
            })
        }
        ("assistant", _) => {
            let used: Vec<String> = v["message"]["content"]
                .as_array()?
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| b["name"].as_str().map(str::to_owned))
                .collect();
            (!used.is_empty()).then_some(Event::Uses(used))
        }
        ("result", subtype) => {
            let ok = subtype == Some("success") && v["is_error"] != true;
            let text = v["result"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| subtype.map(|s| s.replace('_', " ")))
                .unwrap_or_default();
            Some(Event::Done { ok, text })
        }
        _ => None,
    }
}

/// The containment line: Claude Code may have this computer's tools and
/// nothing else, and must have reached them.
fn check_init(tools: &[String], servers: &[(String, String)]) -> Result<(), String> {
    let prefix = format!("mcp__{SERVER}__");
    let own: Vec<&str> = tools
        .iter()
        .filter(|t| !t.starts_with(&prefix))
        .map(String::as_str)
        .collect();
    if !own.is_empty() {
        return Err(format!(
            "Claude Code came up with tools of its own ({}); stopped before it did anything",
            own.join(", ")
        ));
    }
    match servers.iter().find(|(name, _)| name == SERVER) {
        Some((_, status)) if status == "connected" => Ok(()),
        Some((_, status)) => Err(format!(
            "Claude Code couldn't reach this computer's tools ({status})"
        )),
        None => Err("Claude Code didn't load this computer's tools".into()),
    }
}

/// The tile's caption for a tool Claude Code is using, if it should show:
/// its own progress reports speak for themselves.
fn using(tool: &str) -> Option<String> {
    let name = tool.strip_prefix(&format!("mcp__{SERVER}__"))?;
    (name != "report_progress").then(|| format!("Using {}", name.replace('_', " ")))
}

/// A tile caption from the answer's first line.
fn caption(answer: &str) -> String {
    let line = answer
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Done")
        .trim();
    let mut caption: String = line.chars().take(100).collect();
    if line.chars().count() > 100 {
        caption.push('…');
    }
    caption
}

/// Claude Code's arguments: headless, its own tools off, ours only.
fn claude_args(config: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--tools",
        "",
        "--strict-mcp-config",
        "--mcp-config",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(config.display().to_string());
    args.extend(
        [
            "--allowedTools".to_owned(),
            format!("mcp__{SERVER}__*"),
            "--disallowedTools".to_owned(),
        ]
        .into_iter()
        .chain(DENIED.iter().map(|t| format!("mcp__{SERVER}__{t}")))
        .chain(
            [
                "--permission-mode",
                "dontAsk",
                "--append-system-prompt",
                SYSTEM,
                "--disable-slash-commands",
                "--no-session-persistence",
                "--max-turns",
            ]
            .map(str::to_owned),
        ),
    );
    args.push(MAX_TURNS.to_string());
    args
}

/// `glass-mcp`, beside this program.
fn bridge() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("glass-mcp")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("glass-mcp"))
}

async fn run() -> anyhow::Result<()> {
    let socket = std::env::var("GLASS_AGENT_SOCKET").context("no GLASS_AGENT_SOCKET")?;
    let agent = Agent::from_env()
        .await
        .context("connecting to the task socket")?;
    eprintln!("task {}: {}", agent.task_id, agent.prompt);

    // An empty folder of its own: no project settings, hooks or MCP servers
    // to pick up from wherever it was started.
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let work = base.join(format!("glass-claude-{}", agent.task_id));
    std::fs::create_dir_all(&work).with_context(|| format!("creating {}", work.display()))?;
    let config = work.join("mcp.json");
    std::fs::write(
        &config,
        json!({ "mcpServers": { SERVER: {
            "type": "stdio",
            "command": bridge(),
            "args": [socket],
        }}})
        .to_string(),
    )?;

    let claude = std::env::var("GLASS_CLAUDE").unwrap_or_else(|_| "claude".into());
    let outcome = drive(&agent, &claude, &config, &work).await;
    let _ = std::fs::remove_dir_all(&work);
    match outcome {
        Ok(answer) => Ok(agent.answer(&caption(&answer), &answer).await?),
        Err(e) => {
            // The tile says why; whoever started it stops the task.
            let _ = agent
                .progress(&format!("Couldn't finish: {e:#}"), None, None)
                .await;
            Err(e)
        }
    }
}

/// Runs Claude Code on the task and returns its answer.
async fn drive(agent: &Agent, claude: &str, config: &Path, work: &Path) -> anyhow::Result<String> {
    agent
        .progress("Starting your Claude Code", None, None)
        .await?;
    let mut child = Command::new(claude)
        .args(claude_args(config))
        .current_dir(work)
        // The person's environment, as they set it up, but for ours and
        // what keeps Claude Code to what it was given here.
        .env_remove("GLASS_AGENT_SOCKET")
        .env_remove("GLASS_TASK_ID")
        .env_remove("GLASS_TASK_PROMPT")
        .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .env("ENABLE_CLAUDEAI_MCP_SERVERS", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {claude}: is Claude Code installed?"))?;

    // The request goes on stdin, never as an argument it could be read as.
    let mut stdin = child.stdin.take().context("no stdin")?;
    stdin.write_all(agent.prompt.as_bytes()).await?;
    drop(stdin);

    let stderr = child.stderr.take().context("no stderr")?;
    let complaints = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut last = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                last = line;
            }
        }
        last
    });

    let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    let mut started = false;
    let mut done = None;
    while let Some(line) = lines.next_line().await? {
        match event(&line) {
            Some(Event::Init { tools, servers }) => {
                if let Err(why) = check_init(&tools, &servers) {
                    let _ = child.kill().await;
                    bail!(why);
                }
                started = true;
                agent.progress("Thinking", None, None).await?;
            }
            Some(_) if !started => {
                let _ = child.kill().await;
                bail!("Claude Code didn't say which tools it has; stopped");
            }
            Some(Event::Uses(tools)) => {
                if let Some(caption) = tools.iter().find_map(|t| using(t)) {
                    agent.progress(&caption, None, None).await?;
                }
            }
            Some(Event::Done { ok, text }) => done = Some((ok, text)),
            None => {}
        }
    }
    let status = child.wait().await?;
    let complaint = complaints.await.unwrap_or_default();
    match done {
        Some((true, text)) if !text.trim().is_empty() => Ok(text),
        Some((true, _)) => Ok("Done.".into()),
        Some((false, text)) => bail!("Claude Code couldn't finish: {text}"),
        None if !complaint.is_empty() => bail!("Claude Code stopped ({status}): {complaint}"),
        None => bail!("Claude Code stopped ({status}) without an answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0042_the_stream_says_what_it_has_what_it_uses_and_how_it_ended() {
        let init = r#"{"type":"system","subtype":"init","tools":["mcp__glasshouse__files_list"],"mcp_servers":[{"name":"glasshouse","status":"connected"}]}"#;
        assert_eq!(
            event(init),
            Some(Event::Init {
                tools: vec!["mcp__glasshouse__files_list".into()],
                servers: vec![("glasshouse".into(), "connected".into())],
            })
        );
        let used = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"t1","name":"mcp__glasshouse__files_list","input":{}}]}}"#;
        assert_eq!(
            event(used),
            Some(Event::Uses(vec!["mcp__glasshouse__files_list".into()]))
        );
        let said = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hi"}]}}"#;
        assert_eq!(event(said), None);
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"Two files."}"#;
        assert_eq!(
            event(ok),
            Some(Event::Done {
                ok: true,
                text: "Two files.".into()
            })
        );
        let max = r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#;
        assert_eq!(
            event(max),
            Some(Event::Done {
                ok: false,
                text: "error max turns".into()
            })
        );
        assert_eq!(event("not json"), None);
    }

    #[test]
    fn adr0042_claude_code_with_any_tool_of_its_own_is_stopped() {
        let connected = vec![("glasshouse".to_owned(), "connected".to_owned())];
        assert!(check_init(&["mcp__glasshouse__files_list".into()], &connected).is_ok());
        let why = check_init(
            &["mcp__glasshouse__files_list".into(), "Bash".into()],
            &connected,
        )
        .unwrap_err();
        assert!(why.contains("Bash"), "{why}");
        assert!(
            check_init(&["mcp__other__x".into()], &connected).is_err(),
            "another server's tools are not ours"
        );
        let failed = vec![("glasshouse".to_owned(), "failed".to_owned())];
        assert!(check_init(&[], &failed).unwrap_err().contains("failed"));
        assert!(check_init(&[], &[]).is_err());
    }

    #[test]
    fn adr0042_claude_code_is_told_to_use_ours_and_nothing_else() {
        let args = claude_args(Path::new("/tmp/x/mcp.json"));
        let after = |flag: &str| {
            let i = args.iter().position(|a| a == flag).unwrap();
            args[i + 1].clone()
        };
        assert_eq!(after("--tools"), "", "its own tools off");
        assert!(args.contains(&"--strict-mcp-config".to_owned()));
        assert_eq!(after("--mcp-config"), "/tmp/x/mcp.json");
        assert_eq!(after("--allowedTools"), "mcp__glasshouse__*");
        assert_eq!(
            after("--disallowedTools"),
            "mcp__glasshouse__model_complete"
        );
        assert_eq!(after("--permission-mode"), "dontAsk");
        assert!(
            !args.iter().any(|a| a == "--bare"),
            "--bare ignores a plan's sign-in"
        );
        assert_eq!(
            using("mcp__glasshouse__files_list").as_deref(),
            Some("Using files list")
        );
        assert_eq!(using("mcp__glasshouse__report_progress"), None);
        assert_eq!(caption("\nTwo files.\nMore"), "Two files.");
    }
}
