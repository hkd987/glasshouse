//! `glass-greeter`: the sign-in screen (ADR 0019). Runs as root on the first
//! virtual terminal, started by `rc.d/glasshouse_greeter`.
//!
//! ```text
//! glass-greeter [--tty /dev/ttyv0] [--session /usr/local/bin/glass-shell-fb]
//! glass-greeter --screen [--tty /dev/ttyv0]
//! ```
//!
//! Without `--screen` it loops: it shows the sign-in screen (itself, with
//! `--screen`, as a child process), and when a password checks out it hands
//! the terminal and input devices to that person and runs their desktop with
//! `su -l <user> -c <session>`. When the desktop ends (signing out), it takes
//! the devices back and shows the sign-in screen again.
//!
//! With `--screen` it shows the sign-in screen until someone signs in, then
//! prints `signed-in <user>` and exits. The screen runs as its own process
//! so that everything it opened (the framebuffer, the input devices) is
//! closed before the person's desktop opens them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail};
use glass_platform::auth::{self, AuthError};
use glass_platform::users;
use glass_shell::greeter::{Account, Effect, Greeter};
use glass_shell::screens::login;
use glass_shell::{Theme, wallpaper};
use glass_shell_fb::display::Display;
use glass_shell_fb::input;
use time::{OffsetDateTime, UtcOffset};
use tokio::process::Command;
use tokio::signal::unix::{Signal, SignalKind, signal};

/// The PAM policy (`/usr/local/etc/pam.d/glasshouse`).
const PAM_SERVICE: &str = "glasshouse";
/// After a refused password, the wait before the next try.
const AFTER_REFUSAL: Duration = Duration::from_secs(2);

struct Args {
    tty: PathBuf,
    session: PathBuf,
    screen: bool,
}

fn args() -> anyhow::Result<Args> {
    let mut parsed = Args {
        tty: PathBuf::from("/dev/ttyv0"),
        session: PathBuf::from("/usr/local/bin/glass-shell-fb"),
        screen: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--tty" => parsed.tty = value()?.into(),
            "--session" => parsed.session = value()?.into(),
            "--screen" => parsed.screen = true,
            "-h" | "--help" => {
                eprintln!("usage: glass-greeter [--tty PATH] [--session PATH] [--screen]");
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
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    if args.screen {
        let user = runtime.block_on(screen(&args.tty, offset))?;
        // The answer to `supervise`, which reads this process's output.
        writeln!(std::io::stdout(), "signed-in {user}")?;
        return Ok(());
    }
    runtime.block_on(supervise(&args))
}

/// The loop: the sign-in screen, then the desktop of whoever signed in.
/// Told to stop (SIGTERM, from `service glasshouse_greeter stop` or a
/// shutdown), it ends whichever is showing, waits for it and takes the
/// devices back, so nothing is left holding the screen.
async fn supervise(args: &Args) -> anyhow::Result<()> {
    let me = std::env::current_exe().context("finding this program")?;
    let mut terminate = signal(SignalKind::terminate())?;
    loop {
        hand_devices(&args.tty, 0, 0);
        let screen = Command::new(&me)
            .arg("--screen")
            .arg("--tty")
            .arg(&args.tty)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("starting the sign-in screen")?;
        let pid = screen.id();
        let Some(output) = until_stopped(screen.wait_with_output(), &mut terminate, || {
            pid.into_iter().for_each(end_screen);
        })
        .await
        else {
            return stopped(&args.tty);
        };
        let output = output.context("waiting for the sign-in screen")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let user = stdout
            .lines()
            .find_map(|line| line.strip_prefix("signed-in "))
            .map(str::to_owned);
        let (Some(user), true) = (user, output.status.success()) else {
            eprintln!(
                "glass-greeter: the sign-in screen ended ({})",
                output.status
            );
            if until_stopped(tokio::time::sleep(AFTER_REFUSAL), &mut terminate, || {})
                .await
                .is_none()
            {
                return stopped(&args.tty);
            }
            continue;
        };
        let Some(account) = users::by_name(&user) else {
            eprintln!("glass-greeter: {user} signed in but has no account");
            continue;
        };
        eprintln!("glass-greeter: {user} signed in");
        hand_devices(&args.tty, account.uid, account.gid);
        // su(1) as root asks for nothing more, and sets up the session as
        // login(1) would: groups, login class, environment, PAM session.
        let mut session = Command::new("/usr/bin/su")
            .arg("-l")
            .arg(&user)
            .arg("-c")
            .arg(&args.session)
            .stdin(Stdio::null())
            .spawn()
            .context("starting the desktop")?;
        let su = session.id();
        let Some(status) = until_stopped(session.wait(), &mut terminate, || {
            su.into_iter().for_each(end_session);
        })
        .await
        else {
            eprintln!("glass-greeter: ended {user}'s session");
            return stopped(&args.tty);
        };
        eprintln!("glass-greeter: {user}'s session ended ({status:?})");
    }
}

/// Waits for `work`, unless this program is told to stop first: then it
/// calls `stop`, still waits for `work` (whatever `stop` ended) and returns
/// `None`.
async fn until_stopped<T>(
    work: impl Future<Output = T>,
    terminate: &mut Signal,
    stop: impl FnOnce(),
) -> Option<T> {
    tokio::pin!(work);
    tokio::select! {
        done = &mut work => Some(done),
        _ = terminate.recv() => {
            stop();
            work.await;
            None
        }
    }
}

fn stopped(tty: &Path) -> anyhow::Result<()> {
    hand_devices(tty, 0, 0);
    eprintln!("glass-greeter: stopped");
    Ok(())
}

/// Ends the sign-in screen: it puts the terminal back to text on SIGTERM.
fn end_screen(pid: u32) {
    run_quietly("/bin/kill", &["-TERM", &pid.to_string()]);
}

/// Ends the desktop su(1) started: su puts its child in a process group of
/// its own and closes the PAM session once that child exits, so the whole
/// group gets SIGTERM, never su itself.
fn end_session(su: u32) {
    let children = std::process::Command::new("/bin/pgrep")
        .args(["-P", &su.to_string()])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    for child in children.split_whitespace() {
        run_quietly("/bin/kill", &["-TERM", "--", &format!("-{child}")]);
    }
}

fn run_quietly(program: &str, args: &[&str]) {
    let status = std::process::Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!("glass-greeter: {program} {}: {status:?}", args.join(" "));
    }
}

/// Gives the terminal and the input devices to `uid` (root takes them back
/// with 0), readable and writable by the owner only.
fn hand_devices(tty: &Path, uid: u32, gid: u32) {
    let inputs = glass_platform::input::devices().unwrap_or_default();
    for path in std::iter::once(tty.to_path_buf()).chain(inputs) {
        if let Err(e) = std::os::unix::fs::chown(&path, Some(uid), Some(gid)) {
            eprintln!("glass-greeter: {}: {e}", path.display());
        }
    }
}

/// The sign-in screen, until someone signs in; returns who.
async fn screen(tty: &Path, offset: UtcOffset) -> anyhow::Result<String> {
    let accounts = users::people()
        .context("reading the accounts")?
        .into_iter()
        .map(|p| Account {
            name: p.name,
            full_name: p.full_name,
        })
        .collect();
    let mut greeter = Greeter::new(accounts);
    let theme = Theme::glass();
    let mut display = Display::open(tty, |w, h| wallpaper::backdrop(&theme.wallpaper, w, h))?;
    let tty_name = tty
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let now = || OffsetDateTime::now_utc().to_offset(offset);
    let draw = |display: &mut Display, greeter: &Greeter| {
        let sign_in = greeter.screen(now());
        let line = display.draw(login::view(&theme, &sign_in, display.size()));
        eprintln!("sign-in screen: {line}");
    };

    {
        let sign_in = greeter.screen(now());
        display.focus(
            login::view(&theme, &sign_in, display.size()),
            login::PASSWORD,
        );
    }
    draw(&mut display, &greeter);
    let (mut inputs, _) = input::start(display.pixels());
    let mut terminate = signal(SignalKind::terminate())?;
    loop {
        tokio::select! {
            Some(first) = inputs.recv() => {
                let mut batch = vec![first];
                while let Ok(more) = inputs.try_recv() {
                    batch.push(more);
                }
                let typed = display.input(&batch, |_, _| None);
                let messages = {
                    let sign_in = greeter.screen(now());
                    let view = login::view(&theme, &sign_in, display.size());
                    display.update(view, &typed.events)
                };
                for message in messages {
                    match greeter.update(message) {
                        Some(Effect::Focus(id)) => {
                            let sign_in = greeter.screen(now());
                            display.focus(login::view(&theme, &sign_in, display.size()), id);
                        }
                        Some(Effect::SignIn { user, mut password }) => {
                            draw(&mut display, &greeter);
                            let checked = auth::check_password(PAM_SERVICE, &user, &tty_name, &password);
                            auth::wipe(&mut password);
                            auth::wipe(greeter.password_mut());
                            match checked {
                                Ok(()) => return Ok(user),
                                Err(AuthError::Refused) => {
                                    tokio::time::sleep(AFTER_REFUSAL).await;
                                    greeter.refused("That password didn't work. Try again.");
                                }
                                Err(other) => greeter.refused(sentence(&other.to_string())),
                            }
                        }
                        None => {}
                    }
                }
                draw(&mut display, &greeter);
            }
            () = tokio::time::sleep(Duration::from_secs(60 - u64::from(now().second()))) => {
                draw(&mut display, &greeter);
            }
            _ = terminate.recv() => bail!("stopped"),
        }
    }
}

/// An error message as the screen shows it: a sentence, capitalized.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(
            super::sentence("this account has expired"),
            "This account has expired"
        );
        assert_eq!(super::sentence(""), "");
    }
}
