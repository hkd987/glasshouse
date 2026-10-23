//! The model gateway end to end (DP-1, DP-2, DP-3, DP-5, ADR 0018), with the
//! scripted provider, so nothing leaves the VM.
//!
//! It needs the test policy `glass-e2e models-policy` prints: the default
//! policy plus a scripted model, which the attended `claude-code` entry and
//! the IT agent may call. The suite plays `claude-code` over its task
//! socket, as `harness` does; the IT agent calls the same tool from its
//! jail (AE-4). `run-vm.sh` installs the policy, restarts glassd, runs this
//! scenario and puts things back.

use std::time::Duration;

use anyhow::{Context, bail, ensure};
use glass_api::{HandoffCard, ScopeRequest, TaskRequest, TaskView, TileState, methods};
use glass_audit::{EventKind, Outcome};
use glass_policy::Policy;
use glass_policy::document::{Models, PolicyDocument};
use glass_rpc::mcp::{CallToolResult, ListToolsResult};
use serde_json::{Value, json};

use crate::harness::Harness;

/// AWS's documentation example key, not a real one.
const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

const MODELS: &str = r#"
default = "echo"

[redact]
pii = ["email"]

[[provider]]
id = "scripted"
kind = "scripted"
name = "Scripted (no network)"

[[model]]
id = "echo"
provider = "scripted"
model = "echo-1"
name = "Echo"
usd_per_mtok_in = 3.0
usd_per_mtok_out = 15.0
"#;

/// The default policy, a scripted model, and model calls for the attended
/// `claude-code` entry and the IT agent.
pub fn policy() -> anyhow::Result<String> {
    with_models(MODELS, "the model gateway scenario")
}

/// The default policy with one real model through OpenRouter (its id
/// there, such as `anthropic/claude-sonnet-4.5`), for
/// `tools/dev/real-model.sh`. The key is glassd's model secrets', never
/// the policy's.
/// Anthropic's current models, cheapest first, with their list prices.
const LADDER: [(&str, &str); 3] = [
    // Prompts up to 100K tokens; the cheapest check of the harness.
    (
        "claude-haiku-5-5",
        "usd_per_mtok_in = 0.1\nusd_per_mtok_out = 0.5\nusd_per_mtok_cache_read = 0.01\n",
    ),
    (
        "claude-sonnet-5-5",
        "usd_per_mtok_in = 2.0\nusd_per_mtok_out = 10.0\nusd_per_mtok_cache_read = 0.2\n",
    ),
    (
        "claude-opus-5-5",
        "usd_per_mtok_in = 4.0\nusd_per_mtok_out = 20.0\nusd_per_mtok_cache_read = 0.2\n",
    ),
];

/// A policy for the agents on a real model. With `ladder`, on Anthropic,
/// the other current models too, chosen by cost (ADR 0044): an agent that
/// asks starts on the cheapest and steps up a model at a time.
pub fn real_policy(provider: &str, model: &str, ladder: bool) -> anyhow::Result<String> {
    ensure!(
        !model.is_empty() && !model.contains(['"', '\\', '\n']),
        "{model:?} isn't a model id"
    );
    // On Anthropic, every call asks for the thinking-history check, so a
    // harness that edits its history fails here even on an account older
    // than the check's default (ADR 0043).
    let (name, check) = match provider {
        "openrouter" => ("OpenRouter", ""),
        "anthropic" => ("Anthropic", "check_history = true\n"),
        _ => anyhow::bail!("{provider:?} isn't openrouter or anthropic"),
    };
    ensure!(
        !ladder || provider == "anthropic",
        "the ladder is Anthropic's models"
    );
    // OpenRouter says what each call cost; for Anthropic's current models
    // the record prices them from their list prices (ADR 0043).
    let prices = LADDER
        .iter()
        .find(|(id, _)| *id == model)
        .map_or("", |(_, prices)| *prices);
    let others: Vec<(&str, &str)> = match ladder {
        true => LADDER.into_iter().filter(|(id, _)| *id != model).collect(),
        false => Vec::new(),
    };
    let mut route = vec!["\"real\"".to_owned()];
    let mut more = String::new();
    for (id, prices) in &others {
        // "claude-sonnet-5-5" is "sonnet" here.
        let short = id.split('-').nth(1).unwrap_or(id);
        route.push(format!("\"{short}\""));
        more.push_str(&format!(
            "\n[[model]]\nid = \"{short}\"\nprovider = \"{provider}\"\nmodel = \"{id}\"\nname = \"{id}\"\n{prices}"
        ));
    }
    let (by_cost, routes) = match ladder {
        true => (
            "by_cost = true\n",
            format!(
                "\n[routes]\npublic = [{0}]\ninternal = [{0}]\n",
                route.join(", ")
            ),
        ),
        false => ("", String::new()),
    };
    let models = format!(
        r#"
default = "real"
{by_cost}
[redact]
pii = ["email"]

[[provider]]
id = "{provider}"
kind = "{provider}"
name = "{name}"
{check}
[[model]]
id = "real"
provider = "{provider}"
model = "{model}"
name = "{model}"
{prices}{more}{routes}"#
    );
    with_models(&models, "tools/dev/real-model.sh")
}

fn with_models(models: &str, purpose: &str) -> anyhow::Result<String> {
    let mut doc: PolicyDocument = toml::from_str(glass_policy::DEFAULT_POLICY)?;
    for id in ["claude-code", "it-agent"] {
        let agent = doc
            .agents
            .iter_mut()
            .find(|a| a.id.as_str() == id)
            .with_context(|| format!("the default policy has a {id} agent"))?;
        agent.allowed_actions.push(
            "model.complete"
                .parse()
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
    }
    doc.models = toml::from_str::<Models>(models)?;
    let text = format!(
        "# Written by glass-e2e for {purpose}.\n{}",
        toml::to_string(&doc)?
    );
    Policy::from_toml(&text)?;
    Ok(text)
}

async fn tool(
    client: &glass_rpc::Client,
    name: &str,
    arguments: Value,
) -> anyhow::Result<CallToolResult> {
    client
        .call(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{name}: {e}"))
}

pub async fn models(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("models")?;
    let request = TaskRequest {
        agent: "claude-code".parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        prompt: "Draft a reply to a vendor with a model's help".into(),
        scope: ScopeRequest {
            read: vec![ws.invoices.clone()],
            write: Vec::new(),
            restrict: Vec::new(),
        },
        title: None,
    };
    // DP-2: the card says where this task's data would go.
    let card: HandoffCard = h.call(methods::TASK_PLAN, &request).await?;
    ensure!(
        card.model == "Echo via Scripted (no network)",
        "card model line: {:?} (is the models-policy installed?)",
        card.model
    );

    let view: TaskView = h.call(methods::TASK_START, &request).await?;
    let socket = view
        .driver_socket
        .clone()
        .context("a socket to connect to")?;
    h.wait_for(view.id, TileState::Working, Duration::from_secs(60))
        .await?;
    let client = glass_rpc::Client::connect(&socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let init: Value = client
        .call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "glass-e2e", "version": "0"}}),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure!(
        init["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains("model_complete"),
        "the instructions name the model tool"
    );
    let tools: ListToolsResult = client
        .call("tools/list", Value::Null)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure!(
        tools.tools.iter().any(|t| t.name == "model_complete"),
        "model_complete is listed"
    );

    // DP-3: the secret and the address are placeholders on the way out; the
    // address comes back, the key doesn't.
    let reply = tool(
        &client,
        "model_complete",
        json!({
            "messages": [{"role": "user", "content": format!("Tell ana@example.com that key {AWS_KEY} was rotated")}],
            "max_tokens": 200
        }),
    )
    .await?;
    ensure!(!reply.is_error, "{}", reply.text());
    let text = reply.text();
    ensure!(
        text.contains("Tell ana@example.com that key [redacted:aws-access-key#1] was rotated"),
        "{text}"
    );
    ensure!(!text.contains(AWS_KEY), "the key never comes back");

    // DP-2: a model outside the allowlist is a refusal the agent can read.
    let refused = tool(
        &client,
        "model_complete",
        json!({
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 50,
            "model": "not-approved"
        }),
    )
    .await?;
    ensure!(
        refused.is_error && refused.text().contains("isn't approved"),
        "{}",
        refused.text()
    );

    let done = h.act(methods::TASK_HAND_BACK, view.id).await?;
    ensure!(done.state == TileState::Done, "{:?}", done.state);
    finished(h, view).await?;

    // DP-5: both calls are on the record, with no prompt text.
    let records = h.audit(done.id).await?;
    let calls: Vec<_> = records
        .iter()
        .filter(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"))
        .collect();
    ensure!(
        calls.len() == 2,
        "{} model calls on the record",
        calls.len()
    );
    let sent = &calls[0].event;
    ensure!(
        sent.outcome == Some(Outcome::Completed),
        "{:?}",
        sent.outcome
    );
    let m = &sent.detail["model"];
    ensure!(m["provider"] == "scripted" && m["model"] == "echo", "{m}");
    ensure!(m["label"] == "internal", "{m}");
    ensure!(m["input_tokens"].as_u64().unwrap_or(0) > 0, "{m}");
    ensure!(m["cost_usd"].as_f64().is_some(), "{m}");
    ensure!(
        m["redacted"]["aws-access-key"] == 1 && m["redacted"]["email"] == 1,
        "{m}"
    );
    ensure!(
        m["request"]["sha256"].as_str().map(str::len) == Some(64),
        "{m}"
    );
    ensure!(
        calls[1].event.outcome == Some(Outcome::Blocked),
        "{:?}",
        calls[1].event.outcome
    );
    let log = serde_json::to_string(&records)?;
    ensure!(
        !log.contains(AWS_KEY) && !log.contains("ana@example.com"),
        "neither the key nor the address is in the audit log"
    );
    it_agent(h).await?;
    desktop_agent(h, &ws).await?;
    desktop_agent_script(h, &ws).await?;
    desktop_agent_routine(h, &ws).await?;
    desktop_agent_flow(h).await
}

/// AE-3, ADR 0041: the desktop agent, in its jail, offers the model its
/// tools; the scripted model asks for files_list; the agent makes the call,
/// policy decides it, and the model answers from what it returned.
async fn desktop_agent(h: &Harness, ws: &crate::harness::Workspace) -> anyhow::Result<()> {
    let task = h
        .start(
            "desktop-agent",
            r#"What's in my folder? Use files_list {"path": ""}"#,
            &[ws.invoices.as_path()],
            &[],
        )
        .await?;
    h.wait_for(task, TileState::Done, Duration::from_secs(60))
        .await?;
    let records = h.audit(task).await?;
    let models: Vec<_> = records
        .iter()
        .filter(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"))
        .collect();
    ensure!(models.len() == 2, "{} model calls", models.len());
    let first = &models[0].event.detail["model"];
    ensure!(
        first["tools_called"] == json!(["files_list"]) && first["tools_offered"].as_u64() > Some(0),
        "{first}"
    );
    let listed = records
        .iter()
        .find(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("files.list"))
        .context("the agent made the call the model proposed")?;
    ensure!(
        listed.event.outcome == Some(Outcome::Completed),
        "{:?}",
        listed.event.outcome
    );
    let detail = h.detail(task).await?;
    let answer = detail.answer.unwrap_or_default();
    ensure!(answer.contains("The tools returned"), "{answer}");
    // ADR 0043: its reasoning went back to the model as written, with
    // nothing before it changed.
    ensure!(
        answer.contains("Its reasoning came back unchanged."),
        "{answer}"
    );
    Ok(())
}

/// ADR 0044: in its jail, the desktop agent runs the script the model
/// wrote. The script's call goes over the task socket like any (decided,
/// audited), its result goes back to the model, and a script that tries to
/// import a file gets an error instead.
async fn desktop_agent_script(h: &Harness, ws: &crate::harness::Workspace) -> anyhow::Result<()> {
    let task = h
        .start(
            "desktop-agent",
            r#"How many? Use run_script {"source": "const r = call('files_list', {path: ''}); ({ok: r.ok, seen: r.text.length > 0})"}"#,
            &[ws.invoices.as_path()],
            &[],
        )
        .await?;
    h.wait_for(task, TileState::Done, Duration::from_secs(60))
        .await?;
    let records = h.audit(task).await?;
    let listed = records
        .iter()
        .find(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("files.list"))
        .context("the script's call went over the socket and was audited")?;
    ensure!(
        listed.event.outcome == Some(Outcome::Completed),
        "{:?}",
        listed.event.outcome
    );
    let first = records
        .iter()
        .find(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"))
        .context("a model call")?;
    ensure!(
        first.event.detail["model"]["tools_called"] == json!(["run_script"]),
        "{}",
        first.event.detail["model"]
    );
    let answer = h.detail(task).await?.answer.unwrap_or_default();
    ensure!(
        answer.contains(r#""result":{"ok":true,"seen":true}"#),
        "{answer}"
    );

    let task = h
        .start(
            "desktop-agent",
            r#"Look. Use run_script {"source": "import('/etc/passwd')"}"#,
            &[ws.invoices.as_path()],
            &[],
        )
        .await?;
    h.wait_for(task, TileState::Done, Duration::from_secs(60))
        .await?;
    let answer = h.detail(task).await?.answer.unwrap_or_default();
    ensure!(answer.contains("The script failed"), "{answer}");
    ensure!(!answer.contains("root:"), "{answer}");
    Ok(())
}

/// Code this computer writes from what a task did (NV-21, ADR 0046), in
/// the jail: the same request done the same way twice says so; saved from
/// the task, with no model writing it, its run drafts the mail again with
/// no model.
async fn desktop_agent_flow(h: &Harness) -> anyhow::Result<()> {
    let prompt = r#"Draft it. Use mail_draft {"to": ["ana@example.com"], "subject": "Weekly numbers", "body": "Here are this week's numbers."}"#;
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let task = h.start("desktop-agent", prompt, &[], &[]).await?;
        h.wait_for(task, TileState::Done, Duration::from_secs(60))
            .await?;
        tasks.push(task);
    }
    let said: Vec<String> = h
        .detail(tasks[1])
        .await?
        .activity
        .into_iter()
        .map(|a| a.text)
        .collect();
    ensure!(
        said.iter()
            .any(|t| t.starts_with(&format!("The same steps as task {}", tasks[0].short()))),
        "{said:?}"
    );
    let draft: glass_api::RoutineDraft = h
        .call(
            methods::ROUTINE_DRAFT,
            glass_api::TaskRef { task: tasks[1] },
        )
        .await?;
    ensure!(
        draft.script.contains(r#"call("mail_draft", {"#),
        "{}",
        draft.script
    );

    let saved: glass_api::RoutineView = h
        .call(
            methods::ROUTINE_SAVE,
            glass_api::RoutineFromTask {
                task: tasks[1],
                name: "Weekly numbers draft".into(),
                schedule: None,
            },
        )
        .await?;
    let run: TaskView = h
        .call(
            methods::ROUTINE_RUN,
            glass_api::RoutineRef {
                routine: saved.routine.clone(),
            },
        )
        .await?;
    h.wait_for(run.id, TileState::Done, Duration::from_secs(60))
        .await?;
    let answer = h.detail(run.id).await?.answer.unwrap_or_default();
    ensure!(answer.to_lowercase().contains("draft"), "{answer}");
    let actions: Vec<String> = h
        .audit(run.id)
        .await?
        .iter()
        .filter(|r| r.event.outcome == Some(Outcome::Completed))
        .filter_map(|r| r.event.action.as_ref().map(|a| a.as_str().to_owned()))
        .collect();
    ensure!(actions.iter().any(|a| a == "mail.draft"), "{actions:?}");
    ensure!(
        !actions.iter().any(|a| a == "model.complete"),
        "no model: {actions:?}"
    );

    // Off the person's list again.
    let file =
        std::path::PathBuf::from(std::env::var("HOME")?).join(glass_app_routines::store::MINE);
    let mut kept: Value = serde_json::from_slice(&std::fs::read(&file)?)?;
    kept["routines"]
        .as_object_mut()
        .context("the routines file")?
        .remove(&saved.routine);
    std::fs::write(&file, serde_json::to_vec(&kept)?)?;
    Ok(())
}

/// A routine in the jail (DA-4, NV-21, ADR 0044): the desktop agent saves
/// one, the person approves it, and its run does the job with the script
/// alone, no model; a routine whose script breaks has the model take over.
async fn desktop_agent_routine(h: &Harness, ws: &crate::harness::Workspace) -> anyhow::Result<()> {
    let save = |name: &str, script: &str| {
        format!(
            r#"Save it. Use routines_save {}"#,
            json!({
                "name": name,
                "request": "Say whether my invoices folder lists.",
                "script": script,
                "note": "First version",
            })
        )
    };
    let mut ids = Vec::new();
    for (name, script) in [
        (
            "Listing check",
            "const r = call('files_list', {path: ''}); return 'Listed: ' + (r.ok ? 'yes' : 'no');",
        ),
        (
            "Broken check",
            "call('files_list', {path: ''}); throw new Error('the folder moved');",
        ),
    ] {
        let task = h
            .start(
                "desktop-agent",
                &save(name, script),
                &[ws.invoices.as_path()],
                &[],
            )
            .await?;
        h.wait_for(task, TileState::NeedsYou, Duration::from_secs(60))
            .await?;
        let ask = h.the_ask(task).await?;
        ensure!(
            ask.summary == format!("Save the routine {name}"),
            "{}",
            ask.summary
        );
        ensure!(
            ask.items
                .iter()
                .any(|i| i == "Calls: files_list, each at its tier, as desktop-agent would"),
            "{:?}",
            ask.items
        );
        h.approve_once(&ask).await?;
        h.wait_for(task, TileState::Done, Duration::from_secs(60))
            .await?;
        let saved = h.audit(task).await?.into_iter().find(|r| {
            r.event.action.as_ref().map(|a| a.as_str()) == Some("routines.save")
                && r.event.outcome == Some(Outcome::Completed)
        });
        ensure!(saved.is_some(), "{name} saved");
        // This run's: the one with this workspace's scope.
        let listed: Vec<glass_api::RoutineView> =
            h.call(methods::ROUTINE_LIST, Value::Null).await?;
        let mine = listed
            .iter()
            .find(|r| r.name == name && r.read == [ws.invoices.clone()])
            .with_context(|| format!("{name} with this task's scope in {listed:?}"))?;
        ids.push(mine.routine.clone());
    }

    // Its run: the script, over the task's socket, and no model.
    let run: TaskView = h
        .call(
            methods::ROUTINE_RUN,
            glass_api::RoutineRef {
                routine: ids[0].clone(),
            },
        )
        .await?;
    h.wait_for(run.id, TileState::Done, Duration::from_secs(60))
        .await?;
    let answer = h.detail(run.id).await?.answer.unwrap_or_default();
    ensure!(answer == "Listed: yes", "{answer}");
    let records = h.audit(run.id).await?;
    let actions: Vec<&str> = records
        .iter()
        .filter(|r| r.event.outcome == Some(Outcome::Completed))
        .filter_map(|r| r.event.action.as_ref().map(|a| a.as_str()))
        .collect();
    ensure!(
        actions.contains(&"routines.read") && actions.contains(&"files.list"),
        "{actions:?}"
    );
    ensure!(
        !actions.contains(&"model.complete"),
        "no model: {actions:?}"
    );

    // A broken script: the model takes over from the request.
    let run: TaskView = h
        .call(
            methods::ROUTINE_RUN,
            glass_api::RoutineRef {
                routine: ids[1].clone(),
            },
        )
        .await?;
    h.wait_for(run.id, TileState::Done, Duration::from_secs(60))
        .await?;
    let detail = h.detail(run.id).await?;
    ensure!(
        detail
            .activity
            .iter()
            .any(|a| a.text.contains("The routine's script failed")),
        "{:?}",
        detail.activity
    );
    let called = h
        .audit(run.id)
        .await?
        .iter()
        .any(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"));
    ensure!(called, "the model took over");

    // The person takes them off their list, as they may by hand.
    let file =
        std::path::PathBuf::from(std::env::var("HOME")?).join(glass_app_routines::store::MINE);
    let mut kept: Value = serde_json::from_slice(&std::fs::read(&file)?)?;
    let kept_ids = kept["routines"]
        .as_object_mut()
        .context("the routines file")?;
    for id in &ids {
        kept_ids.remove(id);
    }
    std::fs::write(&file, serde_json::to_vec(&kept)?)?;
    let listed: Vec<glass_api::RoutineView> = h.call(methods::ROUTINE_LIST, Value::Null).await?;
    ensure!(
        !listed.iter().any(|r| ids.contains(&r.routine)),
        "{listed:?}"
    );
    Ok(())
}

/// AE-4: the IT agent, in its jail, asks the model what the request is
/// and, with nothing it can fix, for an assessment for IT's ticket. The
/// scripted reply isn't a sorting, so keywords pick the checks.
async fn it_agent(h: &Harness) -> anyhow::Result<()> {
    let task = h
        .start("it-agent", "The printer won't print", &[], &[])
        .await?;
    h.wait_for(task, TileState::NeedsYou, Duration::from_secs(60))
        .await?;
    let ask = h.the_ask(task).await?;
    ensure!(
        ask.summary.starts_with("Open a help desk ticket"),
        "{}",
        ask.summary
    );
    h.approve_once(&ask).await?;
    h.wait_for(task, TileState::Done, Duration::from_secs(30))
        .await?;
    let calls: Vec<_> = h
        .audit(task)
        .await?
        .into_iter()
        .filter(|r| r.event.action.as_ref().map(|a| a.as_str()) == Some("model.complete"))
        .collect();
    ensure!(
        calls.len() == 2,
        "{} IT agent model calls on the record",
        calls.len()
    );
    for call in &calls {
        let m = &call.event.detail["model"];
        ensure!(
            call.event.outcome == Some(Outcome::Completed) && m["provider"] == "scripted",
            "{:?} {m}",
            call.event.outcome
        );
    }
    Ok(())
}

async fn finished(h: &Harness, view: TaskView) -> anyhow::Result<()> {
    for _ in 0..150 {
        if h.audit(view.id)
            .await?
            .iter()
            .any(|r| r.event.kind == EventKind::TaskFinished)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("the task never finished")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_test_policy_is_valid() {
        let text = super::policy().unwrap();
        assert!(text.contains("model.complete"));
        assert!(text.contains("Scripted (no network)"));
    }
}
