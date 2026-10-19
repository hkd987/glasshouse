//! `glass-greeter`: the sign-in screen (ADR 0019). Runs as root on the first
//! virtual terminal, started by `rc.d/glasshouse_greeter`.
//!
//! ```text
//! glass-greeter [--tty /dev/ttyv0] [--session "/usr/local/bin/glass-shell-fb --from-sign-in"]
//! glass-greeter --screen [--tty /dev/ttyv0]
//! ```
//!
//! Without `--screen` it loops: it shows the sign-in screen (itself, with
//! `--screen`, as a child process), and when a password checks out it hands
//! the terminal and input devices to that person and runs their desktop with
//! `su -l <user> -c <session>`. When the desktop ends (signing out), it takes
//! the devices back and shows the sign-in screen again. While the desktop
//! runs, it checks the password that unlocks the desktop's locked screen
//! (`glass_shell::session`): the desktop isn't root and can't ask PAM.
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
use glass_shell::screens::{login, system};
use glass_shell::{Power, session};
use glass_shell::{Theme, wallpaper};
use glass_shell_fb::display::Display;
use glass_shell_fb::input;
use time::{OffsetDateTime, UtcOffset};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::signal::unix::{Signal, SignalKind, signal};

/// The PAM policy (`/usr/local/etc/pam.d/glasshouse`).
const PAM_SERVICE: &str = "glasshouse";
/// After a refused password, the wait before the next try.
const AFTER_REFUSAL: Duration = Duration::from_secs(2);
/// The longest line taken from the desktop at once: a password fits many
/// times over, and nothing the desktop writes makes this root process hold
/// more than that.
const LONGEST_LINE: u64 = 4096;

struct Args {
    tty: PathBuf,
    /// The desktop: a command for the person's login shell.
    session: String,
    screen: bool,
}

fn args() -> anyhow::Result<Args> {
    let mut parsed = Args {
        tty: PathBuf::from("/dev/ttyv0"),
        session: "/usr/local/bin/glass-shell-fb --from-sign-in".into(),
        screen: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--tty" => parsed.tty = value()?.into(),
            "--session" => parsed.session = value()?,
            "--screen" => parsed.screen = true,
            "-h" | "--help" => {
                eprintln!("usage: glass-greeter [--tty PATH] [--session COMMAND] [--screen]");
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
        // Where the desktop's apps find it (their Wayland socket): su -l
        // starts from a clean environment, so it's set on the command.
        let runtime = runtime_dir(account.uid, account.gid);
        let command = match &runtime {
            Some(dir) => format!("env XDG_RUNTIME_DIR={} {}", dir.display(), args.session),
            None => args.session.clone(),
        };
        // su(1) as root asks for nothing more, and sets up the session as
        // login(1) would: groups, login class, environment, PAM session.
        let mut session = Command::new("/usr/bin/su")
            .arg("-l")
            .arg(&user)
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("starting the desktop")?;
        let su = session.id();
        let tty_name = tty_name(&args.tty);
        let serving = serve(&mut session, &user, &tty_name);
        let Some(status) = until_stopped(serving, &mut terminate, || {
            su.into_iter().for_each(end_session);
        })
        .await
        else {
            eprintln!("glass-greeter: ended {user}'s session");
            return stopped(&args.tty);
        };
        eprintln!("glass-greeter: {user}'s session ended ({status:?})");
        if let Some(dir) = &runtime {
            let _ = std::fs::remove_dir_all(dir);
        }
        // The desktop asked to restart or shut down: it can't (it isn't
        // root); this does, from the person at the screen, whose desktop
        // left "Restarting…" up. Shutting down stops this service too.
        let asked = status
            .as_ref()
            .ok()
            .and_then(|s| session::power_after(s.code()));
        if let Some(power) = asked {
            hand_devices(&args.tty, 0, 0);
            let how = match power {
                Power::Restart => "-r",
                Power::ShutDown => "-p",
            };
            eprintln!("glass-greeter: {user} asked to {power:?}");
            // The base system's shutdown messages go to the first terminal;
            // the last frame moves to one nothing else writes to.
            let leaving = leaving_screen(power);
            run_quietly("/sbin/shutdown", &[how, "now"]);
            // Until shutting down stops this service; the frame stays up.
            terminate.recv().await;
            if let Some(display) = leaving {
                display.hand_over();
            }
            return stopped(&args.tty);
        }
    }
}

/// Waits for the desktop to end, answering its requests to unlock its
/// locked screen meanwhile (`session`): the password is checked with PAM for
/// the account signed in and no other, one request at a time, with a wait
/// after a wrong one. Anything else the desktop prints goes to the log.
async fn serve(
    session: &mut Child,
    user: &str,
    tty_name: &str,
) -> std::io::Result<std::process::ExitStatus> {
    let mut answers = session.stdin.take();
    let mut output = session.stdout.take().map(BufReader::new);
    let mut line = Vec::new();
    loop {
        let next = async {
            match output.as_mut() {
                Some(out) => out.take(LONGEST_LINE).read_until(b'\n', &mut line).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            status = session.wait() => return status,
            read = next => {
                if !matches!(read, Ok(1..)) {
                    // The desktop closed its output: no more requests.
                    output = None;
                    continue;
                }
                let mut text = String::from_utf8_lossy(&line).into_owned();
                line.fill(0);
                line.clear();
                match session::parse_unlock(&text) {
                    Some(password) => {
                        let answer = unlock(user, tty_name, password).await;
                        eprintln!("glass-greeter: {user}'s screen: {answer}");
                        if let Some(desktop) = answers.as_mut() {
                            let sent = desktop.write_all(format!("{answer}\n").as_bytes()).await;
                            if let Err(e) = sent {
                                eprintln!("glass-greeter: answering {user}'s desktop: {e}");
                            }
                        }
                    }
                    None => eprint!("{text}"),
                }
                auth::wipe(&mut text);
            }
        }
    }
}

/// Checks the password that unlocks `user`'s screen: [`session::UNLOCKED`]
/// or [`session::REFUSED`], the latter after [`AFTER_REFUSAL`].
async fn unlock(user: &str, tty_name: &str, password: session::Password) -> &'static str {
    let (user, tty_name) = (user.to_owned(), tty_name.to_owned());
    // PAM blocks, and may take a while (it waits after a failure itself).
    let checked = tokio::task::spawn_blocking(move || {
        auth::check_password(PAM_SERVICE, &user, &tty_name, password.as_str())
    })
    .await;
    match checked {
        Ok(Ok(())) => session::UNLOCKED,
        Ok(Err(AuthError::Refused)) => {
            tokio::time::sleep(AFTER_REFUSAL).await;
            session::REFUSED
        }
        Ok(Err(other)) => {
            eprintln!("glass-greeter: unlocking: {other}");
            session::REFUSED
        }
        Err(e) => {
            eprintln!("glass-greeter: unlocking: {e}");
            session::REFUSED
        }
    }
}

/// "ttyv0" for `/dev/ttyv0`, as PAM's `PAM_TTY`.
fn tty_name(tty: &Path) -> String {
    tty.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Where each person's runtime directories live (XDG), one per uid.
const RUNTIME_DIRS: &str = "/var/run/user";

/// The session's runtime directory, `/var/run/user/<uid>`: the person's
/// alone (0700), and empty at the start of each session, so nothing left
/// from an earlier one (a socket, a symlink) is in it. The apps' Wayland
/// socket lives there. None if it can't be made; the desktop then runs
/// without app windows.
fn runtime_dir(uid: u32, gid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = Path::new(RUNTIME_DIRS).join(uid.to_string());
    let made = (|| -> std::io::Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(RUNTIME_DIRS)?;
        if dir.symlink_metadata().is_ok() {
            // Doesn't follow a symlink: removes the link itself.
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        std::os::unix::fs::chown(&dir, Some(uid), Some(gid))
    })();
    match made {
        Ok(()) => Some(dir),
        Err(e) => {
            eprintln!("glass-greeter: {}: {e}", dir.display());
            None
        }
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

/// The terminal the restarting or shutting-down frame shows on: one without
/// a login (`/etc/ttys`) or console messages.
const LEAVING_TTY: &str = "/dev/ttyv8";

/// "Restarting…" or "Shutting down…" on `LEAVING_TTY`, made the terminal on
/// screen. Without it (no such terminal) the desktop's own last frame
/// stays, with the console's messages over it.
fn leaving_screen(power: Power) -> Option<Display> {
    let mut display = Display::open(Path::new(LEAVING_TTY), |w, h| {
        vec![0; w as usize * h as usize * 4]
    })
    .inspect_err(|e| eprintln!("glass-greeter: {LEAVING_TTY}: {e:#}"))
    .ok()?;
    display.hide_pointer();
    // Switch first: the console repaints the terminal it switches to.
    if let Err(e) = display.show() {
        eprintln!("glass-greeter: showing {LEAVING_TTY}: {e}");
    }
    display.draw(system::leaving_overlay(power));
    Some(display)
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
    let tty_name = tty_name(tty);
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
                let updated = {
                    let sign_in = greeter.screen(now());
                    let view = login::view(&theme, &sign_in, display.size());
                    display.update(view, &typed.events)
                };
                // The password field is all there is to type into: a click
                // anywhere else leaves it the keyboard, as a Mac's does.
                if batch.iter().any(|i| matches!(i, input::Input::Button(..))) {
                    let sign_in = greeter.screen(now());
                    display.focus(login::view(&theme, &sign_in, display.size()), login::PASSWORD);
                }
                let redraw = updated.redraw || !updated.messages.is_empty();
                for message in updated.messages {
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
                                Ok(()) => {
                                    // Up until the desktop draws over it.
                                    display.hand_over();
                                    return Ok(user);
                                }
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
                if redraw {
                    draw(&mut display, &greeter);
                }
            }
            () = tokio::time::sleep(Duration::from_secs(60 - u64::from(now().second()))) => {
                draw(&mut display, &greeter);
            }
            // The password field's caret blinks.
            () = blink(display.next_redraw()) => {
                // Only the caret: not worth a log line twice a second.
                let sign_in = greeter.screen(now());
                display.draw(login::view(&theme, &sign_in, display.size()));
            }
            _ = terminate.recv() => bail!("stopped"),
        }
    }
}

/// Waits until `at`, or forever without one.
async fn blink(at: Option<std::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
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
