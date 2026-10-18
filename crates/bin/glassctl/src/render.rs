//! Plain-text renderings of what the desktop shell draws: the handoff card,
//! tiles, the expanded view, approvals and undo reports.

use std::fmt::Write;

use glass_api::{
    ActivityKind, ApprovalView, ChangeKind, HandoffCard, TaskDetail, TaskView, TileState,
    UndoReport,
};
use glass_audit::AuditRecord;

use crate::style::{bold, dim, orange, red, state_label, state_marker};

pub fn card(card: &HandoffCard) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} {}", bold("Hand off to"), bold(&card.agent.name));
    let _ = writeln!(out, "  \"{}\"\n", card.prompt);
    let version = if card.agent.version.is_empty() {
        String::new()
    } else {
        format!(" v{}", card.agent.version)
    };
    let mode = format!("{:?}", card.agent.mode).to_lowercase();
    let _ = writeln!(
        out,
        "  {}  {}",
        bold("Plan"),
        dim(&format!("{}{version} · {mode}", card.agent.name))
    );
    for (i, step) in card.plan.iter().enumerate() {
        let _ = writeln!(out, "    {}. {step}", i + 1);
    }
    let _ = writeln!(out, "\n  {}", bold("Touches"));
    if card.touches.is_empty() {
        let _ = writeln!(
            out,
            "    {}",
            dim("no folders (it can only use app actions)")
        );
    }
    for line in &card.touches {
        let _ = writeln!(out, "    {:<40} {}", line.path.display(), dim(&line.access));
    }
    section(&mut out, "Asks you before", &card.asks_before);
    section(&mut out, "Shows in the activity feed", &card.notifies);
    let _ = writeln!(
        out,
        "\n  {} {}",
        bold("Can't do"),
        dim("· set by IT, locked")
    );
    for line in &card.cant_touch {
        let _ = writeln!(out, "    🔒 {line}");
    }
    let data = card
        .max_label
        .map(|label| format!("Up to {label} data"))
        .unwrap_or_default();
    let _ = writeln!(out, "\n  {}", bold("Data and model"));
    let _ = writeln!(out, "    {data} · {}", card.model);
    out
}

fn section(out: &mut String, title: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n  {}", bold(title));
    for line in lines {
        let _ = writeln!(out, "    • {line}");
    }
}

/// One tile (AW-2 to AW-4).
pub fn tile(view: &TaskView) -> String {
    let mut out = String::new();
    let progress = view
        .progress
        .map(|p| format!("  {}", dim(&format!("{} / {}", p.done, p.total))))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "{} {}  {}  {}  {}",
        state_marker(view.state),
        bold(&view.agent_name),
        dim(&format!("{:?}", view.mode).to_lowercase()),
        state_label(view.state),
        dim(&view.id.short()),
    );
    let _ = writeln!(out, "  {}{progress}", view.caption);
    if view.state.is_live() || view.state == TileState::Failed {
        for (i, line) in view.feed.iter().enumerate() {
            let branch = if i + 1 == view.feed.len() {
                "└"
            } else {
                "├"
            };
            // Verb in its own column, as the tile shows it (AW-3).
            let text = if line.verb.is_empty() {
                line.text.clone()
            } else {
                format!("{:<6} {}", line.verb, line.target)
            };
            let _ = writeln!(out, "  {} {text}", dim(branch));
        }
    }
    if let Some(ask) = &view.needs {
        let _ = writeln!(out, "  {} {}", orange("Needs you:"), ask.summary);
        let _ = writeln!(
            out,
            "    {}",
            dim(&format!(
                "glassctl approve {id} [--for-task]  ·  glassctl deny {id}  ·  glassctl show {task}",
                id = ask.id.short(),
                task = view.id.short()
            ))
        );
    }
    if let Some(failure) = &view.failure {
        let _ = writeln!(out, "  {}", red(failure));
    }
    match (&view.receipt, view.state) {
        (Some(receipt), TileState::Done) => {
            let _ = writeln!(
                out,
                "  Receipt: {} actions, {} changes",
                receipt.actions, receipt.changes
            );
            for flag in &receipt.flagged {
                let _ = writeln!(out, "    {} {flag}", orange("⚑"));
            }
        }
        // A tile that stopped early says what's safe (the Failed mockup).
        (Some(receipt), _) => {
            let _ = writeln!(out, "  {}", receipt.outcome());
        }
        (None, _) => {}
    }
    if view.state == TileState::Failed {
        let id = view.id.short();
        let undo = match &view.receipt {
            Some(r) if r.undoable > 0 => format!("glassctl undo {id}  ·  "),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "    {}",
            dim(&format!("glassctl retry {id}  ·  {undo}glassctl show {id}"))
        );
    }
    out
}

pub fn tiles(views: &[TaskView]) -> String {
    if views.is_empty() {
        return dim("No agents. Hand one a task with `glassctl run <agent> \"<task>\"`.\n");
    }
    // Needs-you tiles go first (AW-1).
    let mut sorted: Vec<&TaskView> = views.iter().collect();
    sorted.sort_by_key(|v| {
        (
            v.state != TileState::NeedsYou,
            !v.state.is_live(),
            v.started_at,
        )
    });
    sorted
        .iter()
        .map(|v| tile(v))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The expanded view (AW-10 to AW-16).
pub fn detail(d: &TaskDetail) -> String {
    let mut out = tile(&d.view);
    let _ = writeln!(out, "\n{} {}", bold("Now:"), d.now);
    let _ = writeln!(out, "{} \"{}\"", dim("Task:"), d.prompt);
    let _ = writeln!(out, "\n{}", bold("Activity"));
    for entry in &d.activity {
        let mark = match entry.kind {
            ActivityKind::Action => "·",
            ActivityKind::Notice => "i",
            ActivityKind::Approval => "?",
            ActivityKind::Snapshot => "◇",
            ActivityKind::Blocked => "⊘",
            ActivityKind::State => "»",
        };
        let _ = writeln!(out, "  {} {mark} {}", dim(&entry.at.clock()), entry.text);
    }
    let _ = writeln!(out, "\n{}", bold("Changes"));
    if d.changes.is_empty() {
        let _ = writeln!(out, "  {}", dim("none"));
    }
    for change in &d.changes {
        let kind = match change.kind {
            ChangeKind::Created => "created",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
            ChangeKind::Moved => "moved",
            ChangeKind::Sent => "sent",
        };
        let note = if !change.reversible {
            format!("  {}", orange("can't be undone"))
        } else if change.undone {
            format!("  {}", dim("undone"))
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "  {:>3} {} {:<9} {}{note}",
            dim(&format!("#{}", change.id)),
            dim(&change.at.clock()),
            kind,
            change.target
        );
    }
    if d.changes.iter().any(|c| c.reversible && !c.undone) {
        let _ = writeln!(
            out,
            "      {}",
            dim(&format!(
                "glassctl undo {} --change <#> [--dry-run]",
                d.view.id.short()
            ))
        );
    }
    let _ = writeln!(out, "\n{}", bold("Can't touch"));
    for line in &d.cant_touch {
        let _ = writeln!(out, "  🔒 {line}");
    }
    let _ = writeln!(out, "\n{}", bold("Timeline"));
    if d.snapshots.is_empty() {
        let _ = writeln!(out, "  {}", dim("no snapshots"));
    }
    for marker in &d.snapshots {
        let _ = writeln!(
            out,
            "  ◇ {}  {}  {}",
            marker.at.clock(),
            marker.reason,
            dim(&format!(
                "glassctl undo {} --to {}",
                d.view.id.short(),
                marker.id.short()
            ))
        );
    }
    out
}

pub fn approvals(views: &[ApprovalView]) -> String {
    if views.is_empty() {
        return dim("Nothing needs you.\n");
    }
    let mut out = String::new();
    for ask in views {
        let _ = writeln!(
            out,
            "{} {}  {}  {}",
            orange("◉"),
            bold(&ask.summary),
            dim(&ask.agent.to_string()),
            dim(&ask.id.short())
        );
        for item in &ask.items {
            let _ = writeln!(out, "    • {item}");
        }
        let _ = writeln!(
            out,
            "    {}",
            dim(&format!(
                "{} · expires {}",
                ask.rule,
                ask.expires_at.clock()
            ))
        );
    }
    out
}

pub fn undo(report: &UndoReport) -> String {
    let mut out = String::new();
    let verb = if report.dry_run { "Would" } else { "Did" };
    let _ = writeln!(
        out,
        "{}",
        bold(if report.dry_run {
            "Rollback preview"
        } else {
            "Rolled back"
        })
    );
    let list = |out: &mut String, title: &str, items: &[String]| {
        if !items.is_empty() {
            let _ = writeln!(out, "  {title}");
            for item in items {
                let _ = writeln!(out, "    • {item}");
            }
        }
    };
    list(&mut out, &format!("{verb} restore:"), &report.restored);
    list(
        &mut out,
        &format!("{verb} remove (created by the agent):"),
        &report.removed,
    );
    list(&mut out, "Can't undo:", &report.not_undone);
    if report.restored.is_empty() && report.removed.is_empty() && report.not_undone.is_empty() {
        let _ = writeln!(out, "  {}", dim("nothing to roll back"));
    }
    out
}

pub fn audit(records: &[AuditRecord]) -> String {
    let mut out = String::new();
    for r in records {
        let decision = r
            .event
            .decision
            .as_ref()
            .map(|d| format!(" [{} · {}]", d.tier, d.rule))
            .unwrap_or_default();
        let outcome = r
            .event
            .outcome
            .as_ref()
            .map(|o| {
                format!(
                    " → {}",
                    serde_json::to_value(o)
                        .ok()
                        .and_then(|v| v["status"].as_str().map(str::to_owned))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{:>5} {} {:<18} {}{}{}",
            r.seq,
            dim(&r.ts.to_string()),
            format!("{:?}", r.event.kind),
            r.event.summary,
            dim(&decision),
            outcome
        );
    }
    out
}
