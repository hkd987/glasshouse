//! `glass-shell-preview`: renders the shell's screens to PNG without a
//! display.
//!
//! ```text
//! glass-shell-preview [--out DIR] [--scale 2] [--socket /var/run/glasshouse/control.sock]
//! ```
//!
//! Writes `boot.png`, `login.png` and `desktop.png`. The desktop shows the
//! Desktop artboard's agents, or with `--socket` the caller's real tiles
//! from a running glassd (`task.list`).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, bail};
use glass_api::{TaskView, methods};
use glass_shell::screens::{boot, command_bar, desktop, login};
use glass_shell::{Size, Theme, fixtures, render};

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

    let live = match &args.socket {
        Some(socket) => Some(live_tasks(socket)?),
        None => None,
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
    let recent = live.unwrap_or_else(fixtures::recent_tasks);
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
    Ok(())
}

fn live_tasks(socket: &std::path::Path) -> anyhow::Result<Vec<TaskView>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let client = glass_rpc::Client::connect(socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        client
            .call::<Vec<TaskView>>(methods::TASK_LIST, ())
            .await
            .map_err(|e| anyhow::anyhow!("task.list: {e}"))
    })
}
