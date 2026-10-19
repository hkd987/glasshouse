//! The scenarios. Each drives glassd as a user would and checks the PRD
//! behavior it names. They run as an ordinary user (not root) inside the
//! FreeBSD VM; see `run-vm.sh`.

use std::time::Duration;

use anyhow::{Context, bail, ensure};
use glass_api::{
    ActivityKind, ApprovalAnswer, HandoffCard, LayerChange, LayerRevert, LayerStatus, LayerView,
    ScopeRequest, SystemStatus, TaskRequest, TaskView, TileState, methods,
};
use glass_audit::{EventKind, Outcome};
use glass_types::{AgentId, Label, TaskId, Tier};
use serde_json::Value;

use crate::harness::{Harness, mail_count};

const SHORT: Duration = Duration::from_secs(60);
const LONG: Duration = Duration::from_secs(180);

/// PA-1: an agent with no grants can't read user files, reach the network
/// or see other processes. Only meaningful on the jail backend.
pub async fn containment(h: &Harness) -> anyhow::Result<()> {
    let status: SystemStatus = h.call(methods::SYSTEM_STATUS, Value::Null).await?;
    ensure!(
        status.contained && status.sandbox == "jail",
        "containment needs the jail backend, not {}",
        status.sandbox
    );
    let task = h
        .start("containment-probe", "Try to escape the sandbox", &[], &[])
        .await?;
    let detail = h
        .wait_for(task, TileState::Done, LONG)
        .await
        .context("the probe found a leak")?;
    // "Containment: 21/21 checks passed"
    let counts = detail
        .view
        .caption
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.split_once('/'));
    match counts {
        Some((passed, total)) if passed == total && passed != "0" => {
            println!("    {}", detail.view.caption);
            Ok(())
        }
        _ => bail!("unexpected probe result: {}", detail.view.caption),
    }
}

/// The whole delegation loop: handoff, Auto reads, a Notify write with a
/// snapshot, one batched Ask (DA-12), the receipt, the audit trail (AU-1,
/// AU-3) and Undo all (AU-21).
pub async fn invoices(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("invoices")?;
    let sent_before = mail_count(&h.home, "Sent");
    let task = h
        .start(
            "invoice-demo",
            "Reconcile invoices and remind vendors",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;

    h.wait_for(task, TileState::NeedsYou, LONG).await?;
    let ask = h.the_ask(task).await?;
    ensure!(
        ask.summary.starts_with("Send ") && ask.items.len() >= 2,
        "one batched ask, got {:?}",
        ask.summary
    );
    ensure!(
        ws.reports.join("reconciliation.csv").exists(),
        "the summary is written before the ask"
    );
    h.approve_once(&ask).await?;

    let done = h.wait_for(task, TileState::Done, SHORT).await?;
    let receipt = done.view.receipt.context("a done tile has a receipt")?;
    ensure!(
        receipt.flagged.len() == ask.items.len(),
        "every sent message is flagged on the receipt"
    );
    ensure!(
        mail_count(&h.home, "Sent") == sent_before + ask.items.len(),
        "the reminders were sent"
    );
    let csv = std::fs::read_to_string(ws.reports.join("reconciliation.csv"))?;
    ensure!(
        csv.lines().any(|l| l.starts_with("TOTAL,")),
        "summary has a total line"
    );
    ensure!(
        done.snapshots.len() >= 2,
        "snapshots at start and before the write"
    );

    let records = h.audit(task).await?;
    let tiers: Vec<Tier> = records
        .iter()
        .filter(|r| r.event.kind == EventKind::Action)
        .filter_map(|r| r.event.decision.as_ref().map(|d| d.tier))
        .collect();
    for tier in [Tier::Auto, Tier::Notify, Tier::Ask] {
        ensure!(tiers.contains(&tier), "audit shows a {tier} action");
    }
    ensure!(
        records
            .iter()
            .any(|r| r.event.kind == EventKind::ApprovalAnswered),
        "the answer is on the record"
    );
    ensure!(
        records
            .iter()
            .any(|r| r.event.kind == EventKind::TaskFinished),
        "the end is on the record"
    );

    let preview = h.undo(task, true).await?;
    ensure!(
        preview
            .removed
            .iter()
            .any(|p| p.ends_with("reconciliation.csv")),
        "{preview:?}"
    );
    ensure!(
        preview.not_undone.len() == ask.items.len(),
        "sent mail is listed as not undoable"
    );
    ensure!(
        ws.reports.join("reconciliation.csv").exists(),
        "a dry run changes nothing"
    );
    h.undo(task, false).await?;
    ensure!(
        !ws.reports.join("reconciliation.csv").exists(),
        "undo removed the summary"
    );
    ensure!(
        mail_count(&h.home, "Sent") == sent_before + ask.items.len(),
        "undo can't recall mail"
    );
    Ok(())
}

/// A declined ask: the agent carries on without it (DA-11).
pub async fn decline(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("decline")?;
    let sent_before = mail_count(&h.home, "Sent");
    let task = h
        .start(
            "invoice-demo",
            "Reconcile invoices",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;
    h.wait_for(task, TileState::NeedsYou, LONG).await?;
    let ask = h.the_ask(task).await?;
    h.answer(&ask, ApprovalAnswer::Deny).await?;
    let done = h.wait_for(task, TileState::Done, SHORT).await?;
    ensure!(
        mail_count(&h.home, "Sent") == sent_before,
        "nothing was sent"
    );
    ensure!(
        done.activity.iter().any(|a| a.text.starts_with("Declined")),
        "the decline is in the activity log"
    );
    let denied = h
        .audit(task)
        .await?
        .into_iter()
        .any(|r| r.event.outcome == Some(Outcome::Denied));
    ensure!(denied, "the denied action is on the record");
    Ok(())
}

/// The Changes view's per-change Undo, and Retry on an ended task, on real
/// ZFS snapshots (Changes and Calm mockups).
pub async fn changes(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("changes")?;
    let prompt = "Reconcile invoices";
    let first = h
        .start("invoice-demo", prompt, &[&ws.invoices], &[&ws.reports])
        .await?;
    h.wait_for(first, TileState::NeedsYou, LONG).await?;
    h.answer(&h.the_ask(first).await?, ApprovalAnswer::Deny)
        .await?;
    let done = h.wait_for(first, TileState::Done, SHORT).await?;
    let summary = ws.reports.join("reconciliation.csv");
    let draft = done
        .changes
        .iter()
        .find(|c| c.action.as_str() == "mail.draft")
        .context("the agent saved a draft")?;
    let draft_path = draft.paths.first().context("the draft names its file")?;
    ensure!(draft_path.starts_with("~/Mail/Drafts/"), "{draft_path}");
    let draft_file = h.home.join(draft_path.trim_start_matches("~/"));
    ensure!(draft_file.exists(), "{} exists", draft_file.display());

    let preview = h.undo_changes(first, vec![draft.id], true).await?;
    ensure!(preview.removed == [draft_path.clone()], "{preview:?}");
    ensure!(preview.restored.is_empty(), "{preview:?}");
    h.undo_changes(first, vec![draft.id], false).await?;
    ensure!(!draft_file.exists(), "undo removed just the draft");
    ensure!(summary.exists(), "the other change stays");
    let after = h.detail(first).await?;
    let receipt = after.view.receipt.context("an ended task has a receipt")?;
    ensure!(
        receipt.undoable == receipt.changes - 1,
        "the undone change no longer counts as undoable: {receipt:?}"
    );

    let second = h.act(methods::TASK_RETRY, first).await?.id;
    ensure!(second != first, "retry starts a new task");
    h.wait_for(second, TileState::NeedsYou, LONG).await?;
    h.answer(&h.the_ask(second).await?, ApprovalAnswer::Deny)
        .await?;
    let retried = h.wait_for(second, TileState::Done, SHORT).await?;
    ensure!(retried.prompt == prompt, "same request");
    ensure!(
        retried
            .activity
            .iter()
            .any(|a| a.text.contains(&format!("retry of {}", first.short()))),
        "the retry names the task it repeats"
    );
    h.undo(second, false).await?;
    h.undo(first, false).await?;
    ensure!(!summary.exists(), "undo all cleaned up");
    Ok(())
}

/// IT self-service (AE-4) on the real system: a cache cleanup that undo puts
/// back from a ZFS snapshot, a service restart from IT's list after an
/// approval, and a help desk ticket when there's no fix.
pub async fn it(h: &Harness) -> anyhow::Result<()> {
    let cache = h.home.join(".cache/glass-e2e");
    std::fs::create_dir_all(&cache)?;
    std::fs::write(cache.join("blob.bin"), vec![7u8; 2 * 1024 * 1024])?;

    let task = h
        .start("it-agent", "My disk is filling up", &[], &[])
        .await?;
    let done = h.wait_for(task, TileState::Done, LONG).await?;
    ensure!(
        !cache.exists(),
        "the cache was cleaned: {}",
        done.view.caption
    );
    ensure!(
        done.view.caption.starts_with("Fixed."),
        "{}",
        done.view.caption
    );
    let change = done
        .changes
        .iter()
        .find(|c| c.action.as_str() == "system.storage.clean_cache")
        .context("the cleanup is on the Changes list")?;
    ensure!(change.reversible, "a cache cleanup can be undone");
    h.undo_changes(task, vec![change.id], false).await?;
    ensure!(cache.join("blob.bin").exists(), "undo put the cache back");
    std::fs::remove_dir_all(&cache)?;

    let task = h.start("it-agent", "Please restart cron", &[], &[]).await?;
    h.wait_for(task, TileState::NeedsYou, LONG).await?;
    let ask = h.the_ask(task).await?;
    ensure!(ask.summary == "Restart the cron service", "{}", ask.summary);
    h.approve_once(&ask).await?;
    let done = h.wait_for(task, TileState::Done, SHORT).await?;
    ensure!(
        done.view.caption == "Fixed: Restarted cron",
        "{}",
        done.view.caption
    );

    let task = h
        .start("it-agent", "The printer won't print", &[], &[])
        .await?;
    h.wait_for(task, TileState::NeedsYou, LONG).await?;
    let ask = h.the_ask(task).await?;
    ensure!(
        ask.summary.starts_with("Open a help desk ticket"),
        "{}",
        ask.summary
    );
    h.approve_once(&ask).await?;
    let done = h.wait_for(task, TileState::Done, SHORT).await?;
    let receipt = done.view.receipt.context("an ended task has a receipt")?;
    ensure!(
        receipt.left_machine == 1,
        "the ticket left the machine: {receipt:?}"
    );
    ensure!(
        receipt
            .flagged
            .iter()
            .any(|f| f.starts_with("Help desk ticket HD-")),
        "{receipt:?}"
    );
    Ok(())
}

/// Take over on an ended task (AW-22 for actions agents, HX-1): the task
/// carries on in the user's own harness, here played by this test through
/// the task's socket, with what the first agent did in its instructions.
pub async fn harness(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("harness")?;
    let first = h
        .start(
            "invoice-demo",
            "Reconcile invoices",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;
    h.wait_for(first, TileState::NeedsYou, LONG).await?;
    h.act(methods::TASK_STOP, first).await?;
    wait_until_finished(h, first).await?;

    let view = h.act(methods::TASK_CONTINUE, first).await?;
    ensure!(view.continues == Some(first), "{view:?}");
    let socket = view
        .driver_socket
        .clone()
        .context("a socket to connect to")?;
    // The socket is up once the task leaves Starting.
    h.wait_for(view.id, TileState::Working, SHORT).await?;
    ensure!(
        socket.starts_with("/var/run/glasshouse/attended"),
        "{}",
        socket.display()
    );
    let client = glass_rpc::Client::connect(&socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let init: Value = client
        .call(
            "initialize",
            serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "glass-e2e", "version": "0"}}),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let instructions = init["instructions"].as_str().unwrap_or_default();
    ensure!(
        instructions.contains("which was stopped") && instructions.contains("reconciliation.csv"),
        "{instructions}"
    );
    let listed: glass_rpc::mcp::CallToolResult = client
        .call(
            "tools/call",
            serde_json::json!({"name": "files_list", "arguments": {"path": "Invoices"}}),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure!(!listed.is_error, "{}", listed.text());

    let done = h.act(methods::TASK_HAND_BACK, view.id).await?;
    ensure!(done.state == TileState::Done, "{:?}", done.state);
    wait_until_finished(h, view.id).await?;
    ensure!(!socket.exists(), "the socket goes with the task");
    h.undo(first, false).await?;
    Ok(())
}

/// Tailoring (TL-1, TL-2, TL-10): a personal layer on the invoice agent
/// flags big invoices, turns reminders off and blocks sends; a layer that
/// would loosen sends is refused; turning it off puts the published agent
/// back.
pub async fn tailor(h: &Harness) -> anyhow::Result<()> {
    let agent: AgentId = "invoice-demo".parse().map_err(|e| anyhow::anyhow!("{e}"))?;
    let off = LayerRevert {
        agent: agent.clone(),
        to: None,
    };
    let _: LayerView = h.call(methods::LAYER_REVERT, &off).await?;

    let loosen = LayerChange {
        agent: agent.clone(),
        settings: Default::default(),
        restrict: vec![("mail.send".into(), Tier::Auto)],
        note: "send without asking".into(),
    };
    let err = h.call_err(methods::LAYER_SET, &loosen).await?;
    ensure!(err.contains("Permissions only narrow"), "{err}");

    let layer = LayerChange {
        agent: agent.clone(),
        settings: [
            ("flag_over".to_owned(), serde_json::json!(1000)),
            ("remind_vendors".to_owned(), serde_json::json!(false)),
        ]
        .into(),
        restrict: vec![("mail.send".into(), Tier::Never)],
        note: "flag big invoices, no reminders".into(),
    };
    let view: LayerView = h.call(methods::LAYER_SET, &layer).await?;
    ensure!(view.status == LayerStatus::On, "{:?}", view.status);
    let version = view.active.context("a version is in use")?;

    let ws = h.workspace("tailor")?;
    let task = h
        .start(
            "invoice-demo",
            "Reconcile invoices",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;
    // Sends are blocked and reminders off, so nothing asks.
    let done = h.wait_for(task, TileState::Done, LONG).await?;
    ensure!(
        done.activity
            .iter()
            .any(|a| a.text.starts_with(&format!("Using your layer v{version}"))),
        "the task says which layer it runs with"
    );
    let csv = std::fs::read_to_string(ws.reports.join("reconciliation.csv"))?;
    ensure!(
        csv.lines().next().is_some_and(|l| l.ends_with(",flagged")),
        "{csv}"
    );
    ensure!(
        csv.lines().any(|l| l.ends_with(",yes")),
        "the invoice over $1000 is flagged: {csv}"
    );
    ensure!(h.pending().await?.iter().all(|a| a.task != task));
    h.undo(task, false).await?;

    let view: LayerView = h.call(methods::LAYER_REVERT, &off).await?;
    ensure!(view.status == LayerStatus::Off);
    ensure!(
        view.history.iter().any(|v| v.version == version),
        "history stays"
    );
    let request = TaskRequest {
        agent,
        prompt: "Reconcile invoices".into(),
        scope: ScopeRequest::default(),
        title: None,
    };
    let card: HandoffCard = h.call(methods::TASK_PLAN, &request).await?;
    ensure!(card.layer.is_none(), "the published agent again");
    ensure!(card.asks_before.iter().any(|a| a == "Send emails"));
    Ok(())
}

/// Data labels stored with files (DP-10) block even Auto reads above the
/// agent's limit, and restricted data is never opened.
pub async fn labels(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("labels")?;
    let mut files: Vec<_> = std::fs::read_dir(&ws.invoices)?
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    ensure!(files.len() >= 3, "need three invoices");
    glass_platform::xattr::write_label(&files[0], Label::Confidential)
        .context("labeling (needs xattr support)")?;
    glass_platform::xattr::write_label(&files[1], Label::Restricted)?;

    let task = h
        .start(
            "invoice-demo",
            "Reconcile invoices",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;
    let detail = match h.wait_for(task, TileState::NeedsYou, LONG).await {
        Ok(detail) => detail,
        Err(_) => h.detail(task).await?,
    };
    let blocked: Vec<&str> = detail
        .activity
        .iter()
        .filter(|a| a.kind == ActivityKind::Blocked)
        .map(|a| a.text.as_str())
        .collect();
    ensure!(
        blocked.iter().any(|t| t.contains("labeled confidential")),
        "confidential read blocked: {blocked:?}"
    );
    ensure!(
        blocked.iter().any(|t| t.contains("labeled restricted")),
        "restricted read blocked: {blocked:?}"
    );
    if let Ok(ask) = h.the_ask(task).await {
        h.answer(&ask, ApprovalAnswer::Deny).await?;
    }
    h.wait_for(task, TileState::Done, SHORT).await?;
    Ok(())
}

/// Stop works during an ask: the agent is killed, the ask withdrawn, the
/// sandbox torn down (PA-2).
pub async fn stop(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("stop")?;
    let task = h
        .start(
            "invoice-demo",
            "Reconcile invoices",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;
    h.wait_for(task, TileState::NeedsYou, LONG).await?;
    let view = h.act(methods::TASK_STOP, task).await?;
    ensure!(view.state == TileState::Stopped);
    ensure!(
        h.pending().await?.iter().all(|a| a.task != task),
        "the ask was withdrawn"
    );
    wait_until_finished(h, task).await
}

/// Take over during an ask (AW-22), change a file by hand, hand back: the
/// agent is told what changed (AW-21) and the ask is still waiting.
pub async fn takeover(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("takeover")?;
    let task = h
        .start(
            "invoice-demo",
            "Reconcile invoices",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;
    h.wait_for(task, TileState::NeedsYou, LONG).await?;
    let view = h.act(methods::TASK_TAKE_OVER, task).await?;
    ensure!(view.state == TileState::YouAreDriving);
    std::fs::write(
        ws.reports.join("notes-by-hand.txt"),
        "typed while driving\n",
    )?;
    let view = h.act(methods::TASK_HAND_BACK, task).await?;
    ensure!(
        view.caption.contains("changed 1 file"),
        "hand back says what changed: {}",
        view.caption
    );
    ensure!(
        view.state == TileState::NeedsYou,
        "the pending ask survives take over"
    );
    let ask = h.the_ask(task).await?;
    h.answer(&ask, ApprovalAnswer::Deny).await?;
    h.wait_for(task, TileState::Done, SHORT).await?;
    Ok(())
}

/// Users can only grant folders in their own home.
pub async fn scope(h: &Harness) -> anyhow::Result<()> {
    let request = serde_json::json!({
        "agent": "invoice-demo",
        "prompt": "Reconcile",
        "scope": { "read": ["/etc"], "write": [], "restrict": [] }
    });
    let err = h.call_err(methods::TASK_PLAN, request).await?;
    ensure!(err.contains("outside your home"), "{err}");
    let loosen = serde_json::json!({
        "agent": "invoice-demo",
        "prompt": "Reconcile",
        "scope": { "read": [], "write": [], "restrict": [["mail.send", "auto"]] }
    });
    let err = h.call_err(methods::TASK_PLAN, loosen).await?;
    ensure!(err.contains("only make it stricter"), "{err}");
    Ok(())
}

/// Starts a task that waits on an ask and prints its id, for the cross-user
/// and crash-recovery checks.
pub async fn hold(h: &Harness) -> anyhow::Result<()> {
    let ws = h.workspace("hold")?;
    let task = h
        .start(
            "invoice-demo",
            "Reconcile invoices",
            &[&ws.invoices],
            &[&ws.reports],
        )
        .await?;
    h.wait_for(task, TileState::NeedsYou, LONG).await?;
    std::mem::forget(ws); // the task still uses it
    println!("{task}");
    Ok(())
}

/// Run as a different user: someone else's task is invisible and
/// unanswerable, and its sandbox unreachable.
pub async fn isolation(h: &Harness, task: TaskId) -> anyhow::Result<()> {
    let mine: Vec<TaskView> = h.call(methods::TASK_LIST, Value::Null).await?;
    ensure!(
        mine.iter().all(|t| t.id != task),
        "another user's task is listed"
    );
    let err = h
        .call_err(methods::TASK_GET, glass_api::TaskRef { task })
        .await?;
    ensure!(err.contains("no task"), "{err}");
    let err = h
        .call_err(methods::TASK_STOP, glass_api::TaskRef { task })
        .await?;
    ensure!(err.contains("no task"), "{err}");
    ensure!(
        h.pending().await?.is_empty(),
        "another user's asks are listed"
    );
    let sandboxes = std::path::Path::new("/var/db/glasshouse/sandboxes");
    if std::fs::read_dir(sandboxes).is_ok() {
        bail!("{} is readable by other users", sandboxes.display());
    }
    Ok(())
}

/// After the system suspended (PM-5): the task is paused for sleep.
pub async fn sleeping(h: &Harness, task: TaskId) -> anyhow::Result<()> {
    let detail = h.detail(task).await?;
    ensure!(
        detail.view.state == TileState::Paused,
        "state is {:?}",
        detail.view.state
    );
    ensure!(
        detail.view.caption == "Paused for sleep",
        "caption is {:?}",
        detail.view.caption
    );
    Ok(())
}

/// After the system resumed: the task carries on (here, back to its ask).
pub async fn awake(h: &Harness, task: TaskId) -> anyhow::Result<()> {
    let detail = h.detail(task).await?;
    ensure!(
        detail.view.state == TileState::NeedsYou,
        "state is {:?}",
        detail.view.state
    );
    ensure!(
        detail
            .activity
            .iter()
            .any(|a| a.text == "Resumed after sleep"),
        "wake is in the activity log"
    );
    Ok(())
}

/// Stops a task started by `hold`.
pub async fn release(h: &Harness, task: TaskId) -> anyhow::Result<()> {
    h.act(methods::TASK_STOP, task).await?;
    wait_until_finished(h, task).await
}

async fn wait_until_finished(h: &Harness, task: TaskId) -> anyhow::Result<()> {
    for _ in 0..150 {
        if h.audit(task)
            .await?
            .iter()
            .any(|r| r.event.kind == EventKind::TaskFinished)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    bail!("the task never finished tearing down")
}
