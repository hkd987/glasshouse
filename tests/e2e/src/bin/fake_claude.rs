//! `glass-fake-claude`: just enough of Claude Code's headless mode for the
//! suites to run `glass-claude-runner` with no model and no account (ADR
//! 0042). It reads the request on stdin and `--mcp-config`, starts the MCP
//! server named there, lists its tools, and calls the first one the request
//! names, with the JSON object written after it, as the scripted model
//! does. It prints what Claude Code's `--output-format stream-json` would:
//! `system/init`, the tool use and its result, then `result`.
//!
//! `FAKE_CLAUDE_OWN_TOOLS=Bash,Read` lists tools of its own in `init`, as a
//! Claude Code that ignored `--tools ""` would, so the runner's refusal can
//! be tested.

#![allow(clippy::print_stdout)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout, Command, ExitCode, Stdio};

use serde_json::{Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fake claude: {e}");
            ExitCode::FAILURE
        }
    }
}

/// One MCP server over stdio: newline-delimited JSON-RPC.
struct Server {
    to: ChildStdin,
    from: BufReader<ChildStdout>,
    next: u64,
}

impl Server {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.to, "{line}").map_err(|e| e.to_string())?;
        loop {
            let mut reply = String::new();
            if self.from.read_line(&mut reply).map_err(|e| e.to_string())? == 0 {
                return Err(format!("the server closed during {method}"));
            }
            let reply: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
            if reply["id"] == id {
                return match reply.get("error") {
                    Some(error) => Err(format!("{method}: {error}")),
                    None => Ok(reply["result"].clone()),
                };
            }
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut prompt = String::new();
    std::io::stdin()
        .read_to_string(&mut prompt)
        .map_err(|e| e.to_string())?;
    let config = flag(&args, "--mcp-config").ok_or("no --mcp-config")?;
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(&config).map_err(|e| format!("{config}: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let (name, server) = config["mcpServers"]
        .as_object()
        .and_then(|s| s.iter().next())
        .ok_or("no MCP server in the config")?;
    let mut child = Command::new(server["command"].as_str().ok_or("no command")?)
        .args(
            server["args"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting the MCP server: {e}"))?;
    let mut mcp = Server {
        to: child.stdin.take().ok_or("no stdin")?,
        from: BufReader::new(child.stdout.take().ok_or("no stdout")?),
        next: 0,
    };
    mcp.call(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "fake-claude", "version": "0" } }),
    )?;
    let denied: Vec<String> = flag(&args, "--disallowedTools").into_iter().collect();
    let listed = mcp.call("tools/list", json!({}))?;
    let tools: Vec<String> = listed["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .map(|t| format!("mcp__{name}__{t}"))
        .filter(|t| !denied.contains(t))
        .collect();
    let mut offered = tools.clone();
    if let Ok(own) = std::env::var("FAKE_CLAUDE_OWN_TOOLS") {
        offered.extend(own.split(',').map(str::to_owned));
    }
    println!(
        "{}",
        json!({ "type": "system", "subtype": "init", "tools": offered,
                "mcp_servers": [{ "name": name, "status": "connected" }] })
    );
    // Claude Code's first step waits on the model: a round trip.
    std::thread::sleep(std::time::Duration::from_millis(500));

    // The first of its tools the request names, with the JSON after it.
    let prefix = format!("mcp__{name}__");
    let call = tools
        .iter()
        .filter_map(|t| t.strip_prefix(&prefix))
        .filter_map(|t| prompt.find(t).map(|at| (at, t)))
        .min_by_key(|(at, _)| *at);
    let mut said = format!("Fake Claude Code read: {}", prompt.trim());
    if let Some((at, tool)) = call {
        let after = &prompt[at + tool.len()..];
        let input = after
            .find('{')
            .and_then(|open| {
                serde_json::Deserializer::from_str(&after[open..])
                    .into_iter::<Value>()
                    .next()?
                    .ok()
            })
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        println!(
            "{}",
            json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "id": "t1", "name": format!("{prefix}{tool}"), "input": input }
            ]}})
        );
        let result = mcp.call("tools/call", json!({ "name": tool, "arguments": input }))?;
        let text: Vec<&str> = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect();
        println!(
            "{}",
            json!({ "type": "user", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": text.join("\n"),
                  "is_error": result["isError"] == true }
            ]}})
        );
        said = format!("Fake Claude Code. The tools returned: {}", text.join(" "));
    }
    println!(
        "{}",
        json!({ "type": "result", "subtype": "success", "is_error": false,
                "num_turns": 2, "result": said })
    );
    drop(mcp);
    let _ = child.wait();
    Ok(())
}
