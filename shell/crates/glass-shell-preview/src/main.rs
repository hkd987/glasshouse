//! `glass-shell-preview`: renders the shell's screens to PNG without a
//! display.
//!
//! ```text
//! glass-shell-preview [--out DIR] [--scale 2] [--socket /var/run/glasshouse/control.sock]
//! ```
//!
//! Writes `boot.png`, `login.png`, `desktop.png`, `command-bar.png` and the
//! expanded view: `expanded.png` (the Expanded artboard), with its Changes
//! tab, a roll back about to happen, a needs-you task's review and a
//! finished task's receipt. The desktop shows the Desktop artboard's
//! agents, or with `--socket` the caller's real tiles from a running glassd
//! (`task.list`), and `expanded.png` then opens the first of them
//! (`task.get`).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, bail};
use glass_api::{Receipt, TaskDetail, TaskRef, TaskView, TileState, UndoReport, methods};
use glass_shell::screens::expanded::{self, Expanded, Rollback, Tab};
use glass_shell::screens::{boot, command_bar, desktop, login};
use glass_shell::{Element, Message, Size, Theme, fixtures, render};
use glass_types::Timestamp;
use time::OffsetDateTime;

struct Args {
    out: PathBuf,
    scale: f32,
    socket: Option<PathBuf>,
}

fn args() -> anyhow::Result<Args> {
    let mut parsed = Args {
        out: PathBuf::from("shell-preview"),
        scale: 1.0,
        socket: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--out" => parsed.out = value()?.into(),
            "--scale" => parsed.scale = value()?.parse().context("--scale is a number")?,
            "--socket" => parsed.socket = Some(value()?.into()),
            "-h" | "--help" => {
                eprintln!("usage: glass-shell-preview [--out DIR] [--scale N] [--socket PATH]");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(parsed)
}

fn main() -> anyhow::Result<()> {
    let args = args()?;
    // Only readable while the process has one thread.
    let local_now =
        time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let theme = Theme::glass();
    let size = Size::new(1440.0, 900.0);
    let (w, h) = (size.width, size.height);

    let (live, live_detail) = match &args.socket {
        Some(socket) => {
            let (tasks, detail) = live_tasks(socket)?;
            (Some(tasks), detail)
        }
        None => (None, None),
    };
    let tasks = live.clone().unwrap_or_else(fixtures::tasks);
    let sign_in = fixtures::sign_in();

    let write =
        |name: &str, view: glass_shell::Element<'_, glass_shell::Message>| -> anyhow::Result<()> {
            let started = Instant::now();
            let frame = render::frame(view, size, args.scale);
            let path = args.out.join(name);
            frame
                .write_png(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            eprintln!(
                "{} ({}x{}, {} ms)",
                path.display(),
                frame.width,
                frame.height,
                started.elapsed().as_millis()
            );
            Ok(())
        };

    write("boot.png", boot::view(&theme, 0.42))?;
    write("login.png", login::view(&theme, &sign_in, (w, h)))?;

    // Live tiles count their time from now, not the artboard's afternoon.
    let state = match live {
        Some(_) => desktop::Desktop::live(&tasks, local_now),
        None => fixtures::desktop(&tasks),
    };
    let windows = vec![desktop::Window {
        title: "Q3 Forecast — Finance".into(),
        x: 48.0,
        y: 52.0,
        width: 968.0,
        height: 744.0,
        content: fixtures::forecast_sheet(),
    }];
    let fixture = live.is_none();
    let thumbnail = |task: &TaskView| {
        (fixture && task.agent.as_str() == "invoice-agent")
            .then(|| fixtures::invoice_entry_thumbnail(&theme))
    };
    write(
        "desktop.png",
        desktop::view(&theme, &state, windows, thumbnail, None, (w, h)),
    )?;

    // The command bar over the desktop (Ctrl Space).
    let recent = live.clone().unwrap_or_else(fixtures::recent_tasks);
    let bar = fixtures::command_bar(&recent);
    let behind = vec![desktop::Window {
        title: "Q3 Forecast — Finance".into(),
        x: 48.0,
        y: 52.0,
        width: 968.0,
        height: 744.0,
        content: glass_shell::blank(),
    }];
    // Tiles collapse while the command bar is open.
    let calm = desktop::Desktop {
        tiles_visible: false,
        ..fixtures::desktop(&tasks)
    };
    let view = desktop::view(
        &theme,
        &calm,
        behind,
        |_| None,
        Some(command_bar::overlay(&theme, &bar)),
        (w, h),
    );
    write("command-bar.png", view)?;

    // A tile opened into the expanded view (AW-10 to AW-16).
    let fixture = live.is_none();
    let expanded_view = |detail, now, tab, rollback| {
        expanded_screen(&theme, &tasks, fixture, detail, now, tab, rollback, (w, h))
    };
    if let Some(detail) = &live_detail {
        write(
            "expanded.png",
            expanded_view(detail, local_now, Tab::Activity, None),
        )?;
    }
    if live.is_some() {
        return Ok(());
    }
    let invoices = fixtures::invoice_detail();
    let at_artboard = utc(fixtures::expanded_now());
    write(
        "expanded.png",
        expanded_view(&invoices, at_artboard, Tab::Activity, None),
    )?;
    write(
        "expanded-changes.png",
        expanded_view(&invoices, at_artboard, Tab::Changes, None),
    )?;
    // Roll back to before the email: the matches since come undone, the
    // email stays sent (AW-15).
    let preview = Rollback::Preview {
        to: invoices.snapshots[1].id,
        report: UndoReport {
            dry_run: true,
            restored: ["0406", "0408", "0409", "0410", "0411"]
                .map(|n| format!("~/Invoices/MAR-26/INV-{n}.json"))
                .to_vec(),
            removed: Vec::new(),
            not_undone: vec![
                "Email to Northwind AP about INV-0407 — this left the machine and can't be \
                 recalled"
                    .into(),
            ],
        },
    };
    write(
        "expanded-rollback.png",
        expanded_view(&invoices, at_artboard, Tab::Activity, Some(&preview)),
    )?;
    // The expense agent's ask, opened from its tile's Review button.
    let expense = fixtures::expense_detail(&tasks[0]);
    write(
        "expanded-review.png",
        expanded_view(&expense, utc(fixtures::now()), Tab::Activity, None),
    )?;
    // The invoice run finished: its receipt.
    let mut done = invoices.clone();
    done.view.state = TileState::Done;
    done.view.progress = Some(glass_api::Progress {
        done: 40,
        total: 40,
    });
    done.view.caption = "Reconciled 40 March invoices".into();
    done.view.receipt = Some(Receipt {
        actions: 84,
        changes: 41,
        undoable: 40,
        left_machine: 1,
        flagged: vec![
            "INV-0407: total is $212.40 over PO 7752".into(),
            "INV-0433: no purchase order found".into(),
        ],
    });
    done.now = "Finished: 84 actions, 41 changes.".into();
    write(
        "expanded-done.png",
        expanded_view(&done, at_artboard, Tab::Changes, None),
    )?;
    Ok(())
}

/// The expanded view over the desktop, as the live shell shows it.
#[allow(clippy::too_many_arguments)]
fn expanded_screen<'a>(
    theme: &Theme,
    tasks: &'a [TaskView],
    fixture: bool,
    detail: &'a TaskDetail,
    now: OffsetDateTime,
    tab: Tab,
    rollback: Option<&'a Rollback>,
    size: (f32, f32),
) -> Element<'a, Message> {
    let state = Expanded {
        task: &detail.view,
        detail: Some(detail),
        tab,
        // A roll back goes to the snapshot the person picked.
        selected: rollback.map(Rollback::to),
        rollback,
        now,
    };
    let under = fixtures::expanded_desktop(tasks, &detail.view.agent_name);
    let under = if fixture {
        under
    } else {
        // Live tiles show today's clock.
        desktop::Desktop {
            app: under.app,
            menus: under.menus,
            tiles_visible: false,
            ..desktop::Desktop::live(tasks, now)
        }
    };
    desktop::view(
        theme,
        &under,
        Vec::new(),
        |_| None,
        Some(expanded::overlay(theme, &state)),
        size,
    )
}

fn utc(at: Timestamp) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(at.unix_seconds()).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// The caller's tiles, and the first one's detail for the expanded view.
fn live_tasks(socket: &std::path::Path) -> anyhow::Result<(Vec<TaskView>, Option<TaskDetail>)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let client = glass_rpc::Client::connect(socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        let tasks = client
            .call::<Vec<TaskView>>(methods::TASK_LIST, ())
            .await
            .map_err(|e| anyhow::anyhow!("task.list: {e}"))?;
        let detail = match desktop::ordered(&tasks).first() {
            Some(first) => Some(
                client
                    .call::<TaskDetail>(methods::TASK_GET, TaskRef { task: first.id })
                    .await
                    .map_err(|e| anyhow::anyhow!("task.get: {e}"))?,
            ),
            None => None,
        };
        Ok((tasks, detail))
    })
}
