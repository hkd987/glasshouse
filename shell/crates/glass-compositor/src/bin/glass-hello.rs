//! `glass-hello`: one window in one color, to see the compositor at work.
//!
//! ```text
//! glass-hello [--title TEXT] [--color RRGGBB] [--size WIDTHxHEIGHT]
//! ```
//!
//! Connects to `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` (`wayland-0` when that's
//! unset) and stays until the window is closed or the compositor goes.

use std::sync::mpsc::channel;

use anyhow::{Context, bail};
use glass_compositor::client::{self, Spec};

fn main() -> anyhow::Result<()> {
    let mut spec = Spec {
        title: "Hello".into(),
        app_id: "glass.hello".into(),
        size: (480, 300),
        color: 0xFF2F_7BEA,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--title" => spec.title = value()?,
            "--color" => {
                let hex = value()?;
                spec.color = 0xFF00_0000
                    | u32::from_str_radix(hex.trim_start_matches('#'), 16)
                        .with_context(|| format!("--color {hex}: not RRGGBB"))?;
            }
            "--size" => {
                let size = value()?;
                let (w, h) = size
                    .split_once('x')
                    .with_context(|| format!("--size {size}: not WIDTHxHEIGHT"))?;
                spec.size = (w.parse()?, h.parse()?);
            }
            "-h" | "--help" => {
                eprintln!("usage: glass-hello [--title TEXT] [--color RRGGBB] [--size WxH]");
                return Ok(());
            }
            other => bail!("unknown argument {other}"),
        }
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR isn't set")?;
    let display = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
    let socket = std::path::Path::new(&dir).join(display);
    let (events, _) = channel();
    client::run(&socket, &spec, &events, None)
}
