//! The scenarios. Each drives glassd as a user would and checks the PRD
//! behavior it names. They run as an ordinary user (not root) inside the
//! FreeBSD VM; see `run-vm.sh`.

use std::time::Duration;

use anyhow::{Context, bail, ensure};
use glass_api::{ActivityKind, ApprovalAnswer, SystemStatus, TaskView, TileState, methods};
use glass_audit::{EventKind, Outcome};
use glass_types::{Label, TaskId, Tier};
use serde_json::Value;

use crate::harness::{Harness, mail_count};

const SHORT: Duration = Duration::from_secs(60);
const LONG: Duration = Duration::from_secs(180);

/// PA-1: an agent with no grants can't read user files, reach the network
/// or see other processes. Only meaningful on the jail backend.
pub async fn containment(h: &Harness) -> anyhow::Result<()> {
    let status: SystemStatus = h.call(methods::SYSTEM_STATUS, Value::Null).await?;
    ensure!(
        status.sandbox == "freebsd-jail",
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
    // "Containment: 19/19 checks passed"
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
