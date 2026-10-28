//! Remixing an app on the computer (TL-5, GA-3, GA-4, ADR 0032): the
//! person's own Claude Code reads Mail's source and saves their version;
//! glassd bundles it in a jail of its own (no network, as nobody, gone with
//! its mount when the bundler exits) and serves it over the app socket to
//! its maker only. Each save is tried, statically linked glass-app-try in a
//! jail of its own with sample data, and the save says what the app showed
//! (ADR 0049). Undo takes the remix out of their apps again.

use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use glass_api::{AppFiles, ScopeRequest, TaskRequest, TaskView, TileState, UndoRequest, methods};
use glass_rpc::mcp::CallToolResult;
use serde_json::{Value, json};

use crate::harness::Harness;

/// Saving an app: the checks, a bundle in a fresh jail, a try of it in
/// another (ADR 0049), the version kept. Measured on the development VM
/// (TCG, no KVM) at 5.7–6.5 s, and 7.4–10.2 s when the emulator translates
/// the try-out's code afresh: after a deploy or a quiet spell. 2.0 s before
/// the try, which takes 0.15 s natively.
const SAVE_BUDGET: Duration = Duration::from_secs(15);

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

/// What `program args…` prints, for checking jails and mounts.
fn output(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new(program).args(args).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn studio(h: &Harness) -> anyhow::Result<()> {
    let request = TaskRequest {
        persona: None,
        agent: "claude-code".parse().map_err(|e| anyhow::anyhow!("{e}"))?,
        prompt: "Make my Mail say Write instead of New message".into(),
        scope: ScopeRequest::default(),
        title: None,
        computer: None,
    };
    let view: TaskView = h.call(methods::TASK_START, &request).await?;
    let socket = view
        .driver_socket
        .clone()
        .context("a socket to connect to")?;
    h.wait_for(view.id, TileState::Working, Duration::from_secs(60))
        .await?;
    let client = glass_rpc::Client::connect(&socket).await?;
    let _: Value = client
        .call(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "glass-e2e", "version": "0"}}),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let source = tool(&client, "apps_source", json!({ "app": "mail" })).await?;
    ensure!(!source.is_error, "apps_source: {}", source.text());
    let data = source.structured_content.context("Mail's source")?;
    let mut files = data["files"].clone();
    let main = files["src/main.ts"].as_str().context("Mail's main.ts")?;
    ensure!(
        main.contains("\"New message\","),
        "Mail's source has its button"
    );
    files["src/main.ts"] = json!(main.replace("\"New message\",", "\"Write\","));

    // Whatever the checks below say, the remix is undone after: a failed run
    // leaves no app behind to take the next run's name.
    let checked = saved_and_served(&client, &data, files).await;
    let undone: anyhow::Result<glass_api::UndoReport> = h
        .call(
            methods::TASK_UNDO,
            UndoRequest {
                task: view.id,
                to: None,
                changes: vec![],
                dry_run: false,
            },
        )
        .await;
    let _: Value = h
        .call(methods::TASK_STOP, glass_api::TaskRef { task: view.id })
        .await
        .unwrap_or_default();
    let (app, apps) = checked?;
    undone?;
    ensure!(
        apps.call::<AppFiles>(glass_api::app_methods::APP_FILES, json!({ "app": app }))
            .await
            .is_err(),
        "undone, the remix is gone"
    );
    Ok(())
}

/// Saves the remix and checks what came of it: tried in a jail, nothing
/// left behind, served to its maker. The app's id, and the app socket.
async fn saved_and_served(
    client: &glass_rpc::Client,
    data: &Value,
    files: Value,
) -> anyhow::Result<(String, glass_rpc::Client)> {
    let started = Instant::now();
    let saved = tool(
        client,
        "apps_save",
        json!({ "base": "mail", "manifest": data["manifest"], "files": files, "note": "Write, not New message" }),
    )
    .await?;
    let took = started.elapsed();
    ensure!(!saved.is_error, "apps_save: {}", saved.text());
    println!(
        "    studio: saved a remix of Mail in {} ms",
        took.as_millis()
    );
    ensure!(
        took < SAVE_BUDGET,
        "saving took {took:?} (budget {SAVE_BUDGET:?})"
    );
    let said = saved.text();
    let app = saved.structured_content.context("what was saved")?["app"]
        .as_str()
        .context("the app's id")?
        .to_owned();
    ensure!(app.starts_with("my-mail"), "{app}");

    // Tried in the jail: Mail opened on sample data and drew its folders.
    ensure!(
        said.contains("Tried here, with sample data") && !said.contains("couldn't be tried"),
        "the save says how trying it went: {said}"
    );
    ensure!(
        said.contains("Opening it showed:") && said.contains("[Write]"),
        "the try saw the remix's button: {said}"
    );
    println!(
        "    studio: tried it in a jail: {}",
        said.lines()
            .find(|l| l.starts_with("No problems") || l.starts_with("Problems"))
            .unwrap_or("?")
    );

    // The bundler's and the try-out's jails and mounts went with them.
    ensure!(
        !output("jls", &["name"])?.contains("glass-build")
            && !output("jls", &["name"])?.contains("glass-try"),
        "a build jail was left"
    );
    ensure!(
        !output("mount", &[])?.contains("/.build/"),
        "a build mount was left"
    );

    // Its maker's bridge gets what glassd built.
    let apps = glass_rpc::Client::connect(
        std::env::var(glass_api::APP_SOCKET_ENV)
            .unwrap_or_else(|_| glass_api::DEFAULT_APP_SOCKET.into()),
    )
    .await?;
    let served: AppFiles = apps
        .call(glass_api::app_methods::APP_FILES, json!({ "app": app }))
        .await
        .map_err(|e| anyhow::anyhow!("app.files: {e}"))?;
    ensure!(served.files["index.html"].contains("<title>My Mail"));
    ensure!(
        served.files["app.js"].contains("Write"),
        "the bundle has the change"
    );
    Ok((app, apps))
}
