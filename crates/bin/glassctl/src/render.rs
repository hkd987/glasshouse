//! Plain-text renderings of what the desktop shell draws: the handoff card,
//! tiles, the expanded view, approvals and undo reports.
//!
//! Everything glassd sends is made display text again before it is
//! rendered ([`display_safe`]) or printed as JSON ([`json`]), so a server
//! can't drive the terminal whatever it sends: glassd does this too, but
//! an older or compromised one might not, and audit records come back as
//! they were recorded.

use std::fmt::Write;

use glass_api::{
    ActivityKind, ApprovalView, ChangeKind, DisplaySafe, HandoffCard, TaskDetail, TaskView,
    TileState, UndoReport,
};
use glass_audit::AuditRecord;
use glass_types::TaskId;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::style::{bold, dim, orange, red, state_label, state_marker};

/// A copy of `value` with every string in it passed through
/// `glass_api::display_line`, for rendering as text.
pub fn display_safe<T: Serialize + DeserializeOwned>(value: &T) -> anyhow::Result<T> {
    let mut json = serde_json::to_value(value)?;
    json.make_display_safe();
    Ok(serde_json::from_value(json)?)
}

/// `value` as JSON that is safe to print. serde_json escapes C0 controls;
/// this also writes DEL, C1 controls and invisible characters (direction
/// overrides and the like) as `\u` escapes, which JSON readers decode back,
/// so `--json` stays exact.
pub fn json(value: &impl Serialize, pretty: bool) -> serde_json::Result<String> {
    let text = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    // Outside strings, serde_json writes only ASCII, so anything matched
    // here is inside a string, where an escape means the same character.
    let escaped = |c: char| c == '\x7f' || (!c.is_ascii() && glass_api::display_char(c) != c);
    if !text.chars().any(escaped) {
        return Ok(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if escaped(c) {
            let _ = write!(out, "\\u{:04x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

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
    if let Some(layer) = &card.layer {
        let heading = format!("Your layer v{}", layer.version);
        match &layer.paused {
            Some(reason) => {
                let _ = writeln!(
                    out,
                    "\n  {} {}",
                    bold(&heading),
                    orange(&format!(
                        "· paused, running the published version: {reason}"
                    ))
                );
            }
            None => {
                let _ = writeln!(out, "\n  {}", bold(&heading));
                for line in &layer.summary {
                    let _ = writeln!(out, "    • {line}");
                }
            }
        }
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

/// How to connect the user's harness to a task it drives (HX-1, HX-6).
pub fn connect(view: &TaskView) -> String {
    let Some(socket) = &view.driver_socket else {
        return tile(view);
    };
    let mut out = String::new();
    let heading = match view.continues {
        Some(first) => format!(
            "Carrying on from {} in {} (task {}).",
            first.short(),
            view.agent_name,
            view.id.short()
        ),
        None => format!("Started {} (task {}).", view.agent_name, view.id.short()),
    };
    let _ = writeln!(out, "{}", bold(&heading));
    let config = serde_json::json!({
        "mcpServers": { "glasshouse": {
            "type": "stdio", "command": "glass-mcp", "args": [socket]
        }}
    });
    let _ = writeln!(
        out,
        "\nStart Claude Code connected to it (this session only):\n\n  claude --mcp-config '{config}'"
    );
    let _ = writeln!(
        out,
        "\nOther MCP clients: run {} as a stdio server.",
        bold(&format!("glass-mcp {}", socket.display()))
    );
    if view.continues.is_some() {
        let _ = writeln!(
            out,
            "It knows what the agent did and why it stopped; ask it to carry on."
        );
    }
    let _ = writeln!(
        out,
        "{}",
        dim(&format!(
            "Every action goes through your approvals and undo. Done? glassctl hand-back {}",
            view.id.short()
        ))
    );
    out
}

/// One tile (AW-2 to AW-4).
pub fn tile(view: &TaskView) -> String {
    let mut out = String::new();
    let progress = view
        .progress
        .map(|p| format!("  {}", dim(&format!("{} / {}", p.done, p.total))))
        .unwrap_or_default();
    let name = if view.title.is_empty() {
        bold(&view.agent_name)
    } else {
        format!("{} · {}", bold(&view.agent_name), view.title)
    };
    let _ = writeln!(
        out,
        "{} {name}  {}  {}  {}",
        state_marker(view.state),
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
        let _ = writeln!(out, "{}", answers(ask, Some(view.id)));
    }
    if let Some(failure) = &view.failure {
        let _ = writeln!(out, "  {}", red(failure));
    }
    match (&view.receipt, view.state) {
        (Some(receipt), TileState::Done) => {
            let _ = writeln!(
                out,
                "  Receipt: {}, {}",
                plural(receipt.actions, "action"),
                plural(receipt.changes, "change")
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
            dim(&format!(
                "glassctl retry {id}  ·  glassctl take-over {id}  ·  {undo}glassctl show {id}"
            ))
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

/// One line for the login screen: how many agents are in each state, the
/// ones that need you first. Empty when there are none.
pub fn summary(views: &[TaskView]) -> String {
    let order = [
        TileState::NeedsYou,
        TileState::Failed,
        TileState::Working,
        TileState::Starting,
        TileState::YouAreDriving,
        TileState::Paused,
        TileState::Done,
        TileState::Stopped,
    ];
    let parts: Vec<String> = order
        .iter()
        .filter_map(|state| {
            let n = views.iter().filter(|v| v.state == *state).count();
            (n > 0).then(|| {
                format!(
                    "{} {n} {}",
                    state_marker(*state),
                    state.label().to_lowercase()
                )
            })
        })
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    let next = if views.iter().any(|v| v.state == TileState::NeedsYou) {
        "glassctl approvals"
    } else {
        "glassctl tasks"
    };
    format!("{}   {}\n", parts.join("   "), dim(&format!("→ {next}")))
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
        let _ = writeln!(out, "{}", answers(ask, None));
    }
    out
}

/// An ask's answers, in the action's own words ("Approve and send"), with
/// the command for each. `task` adds its Review, for a tile.
fn answers(ask: &ApprovalView, task: Option<TaskId>) -> String {
    let id = ask.id.short();
    let review = task
        .map(|task| format!("{}: glassctl show {}  ·  ", ask.labels.review, task.short()))
        .unwrap_or_default();
    format!(
        "    {}",
        dim(&format!(
            "{review}{}: glassctl approve {id} [--for-task]  ·  Decline: glassctl deny {id}",
            ask.labels.approve
        ))
    )
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
        &format!("{verb} remove (created during the task):"),
        &report.removed,
    );
    list(&mut out, "Not undone:", &report.not_undone);
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

/// "1 change", "3 changes".
fn plural(n: u32, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use glass_api::{AgentMode, ApprovalState, AskLabels};
    use glass_types::{ApprovalId, Timestamp};

    use super::*;

    /// The text without terminal colors, which tests run in a terminal get.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(|&c| c == 'm');
            } else {
                out.push(c);
            }
        }
        out
    }

    /// A tile waiting on an ask to send mail.
    fn sample() -> (ApprovalView, TaskView) {
        let id = TaskId::new();
        let ask = ApprovalView {
            id: ApprovalId::new(),
            task: id,
            agent: "invoice-demo".parse().unwrap(),
            action: "mail.send".parse().unwrap(),
            summary: "Send 2 emails".into(),
            items: vec!["To a@example.com: Invoice 1".into()],
            rule: "default tier".into(),
            requested_at: Timestamp::now(),
            expires_at: Timestamp::now(),
            state: ApprovalState::Pending,
            labels: AskLabels::new("Review drafts", "Approve and send"),
        };
        let view = TaskView {
            id,
            agent: "invoice-demo".parse().unwrap(),
            agent_name: "Invoice demo".into(),
            user: "dana".into(),
            mode: AgentMode::Actions,
            state: TileState::NeedsYou,
            title: "Reconcile the October invoices".into(),
            caption: "Reminding late vendors".into(),
            progress: None,
            feed: Vec::new(),
            needs: Some(ask.clone()),
            receipt: None,
            failure: None,
            started_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            driver_socket: None,
            continues: None,
        };
        (ask, view)
    }

    #[test]
    fn tiles_name_the_task_and_asks_answer_in_the_actions_words() {
        let (ask, view) = sample();
        let shown = plain(&tile(&view));
        assert!(
            shown.starts_with("◉ Invoice demo · Reconcile the October invoices  actions"),
            "{shown}"
        );
        let (task, approval) = (view.id.short(), ask.id.short());
        assert!(shown.contains(&format!(
            "Review drafts: glassctl show {task}  ·  Approve and send: glassctl approve {approval}"
        )));
        assert!(plain(&approvals(&[ask])).contains("Approve and send: glassctl approve"));

        // An older glassd sends no title.
        let untitled = TaskView {
            title: String::new(),
            ..view
        };
        assert!(plain(&tile(&untitled)).starts_with("◉ Invoice demo  actions"));
    }

    /// Whether every escape sequence in `text` is one of glassctl's own
    /// colors (`ESC [ digits m`).
    fn only_colors(text: &str) -> bool {
        text.split('\x1b').skip(1).all(|after| {
            after.strip_prefix('[').is_some_and(|body| {
                let end = body.find(|c: char| !c.is_ascii_digit() && c != ';');
                end.is_some_and(|end| body[end..].starts_with('m'))
            })
        })
    }

    /// Text from a hostile or older server prints as text: it can't clear
    /// the screen, make a link, hide part of a line or reorder it (AW-4,
    /// AW-3). `--json` keeps it exact, escaped.
    #[test]
    fn aw4_server_text_is_printed_without_escapes() {
        let (mut ask, view) = sample();
        ask.summary = "Send 1 email\x1b[2J\x1b[H".into();
        ask.labels = AskLabels::new("Review", "Approve\u{202E}");
        let hostile = TaskView {
            caption:
                "Working\x1b[2J\x1b]8;;https://evil.example\x07here\x1b]8;;\x07\rAll done\u{202E}"
                    .into(),
            feed: vec![glass_api::FeedLine {
                at: Timestamp::now(),
                text: "Write a".into(),
                verb: "write".into(),
                target: "Reports/a\x1b[1A\u{9b}2Kb.csv".into(),
            }],
            needs: Some(ask),
            ..view
        };
        let shown = tile(&display_safe(&hostile).unwrap());
        assert!(only_colors(&shown), "{shown:?}");
        assert!(!shown.contains(['\r', '\u{202E}', '\u{9b}']), "{shown:?}");
        let shown = plain(&shown);
        assert!(
            shown.contains(
                "Working\u{FFFD}[2J\u{FFFD}]8;;https://evil.example\u{FFFD}here\u{FFFD}]8;;\u{FFFD} All done\u{FFFD}"
            ),
            "{shown}"
        );
        assert!(
            shown.contains("write  Reports/a\u{FFFD}[1A\u{FFFD}2Kb.csv"),
            "{shown}"
        );
        assert!(
            shown.contains("Needs you: Send 1 email\u{FFFD}[2J\u{FFFD}[H"),
            "{shown}"
        );
        assert!(
            shown.contains("Approve\u{FFFD}: glassctl approve"),
            "{shown}"
        );

        let printed = json(&hostile, true).unwrap();
        assert!(
            !printed.contains(['\x1b', '\u{202E}', '\u{9b}']),
            "{printed}"
        );
        assert!(printed.contains("All done\\u202e"));
        assert_eq!(serde_json::from_str::<TaskView>(&printed).unwrap(), hostile);
    }

    /// Audit records come back as they were recorded (they are history);
    /// `glassctl audit` shows them as text (AU-3).
    #[test]
    fn au3_audit_records_print_as_text() {
        let mut event = glass_audit::AuditEvent::new(
            glass_audit::EventKind::Action,
            "Write Reports/x\x1b[2J\u{202E}.csv",
        );
        event.detail = serde_json::json!({ "reason\x1b": "a\rb" });
        let record = AuditRecord {
            seq: 7,
            ts: Timestamp::now(),
            prev: "0".repeat(64),
            event,
        };
        let shown = audit(&display_safe(&vec![record]).unwrap());
        assert!(only_colors(&shown), "{shown:?}");
        assert!(plain(&shown).contains("Write Reports/x\u{FFFD}[2J\u{FFFD}.csv"));
    }
}
