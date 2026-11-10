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
use glass_shell::screens::setup::{self, ModelChoice};
use glass_shell::screens::{login, system};
use glass_shell::setup::{Effect as SetupEffect, Setup};
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
/// How long the sign-in screen waits before telling glassd again who is at
/// the screen, while glassd doesn't answer (starting, starting again).
const TELL_AGAIN: Duration = Duration::from_secs(2);
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
        let user = runtime.block_on(async {
            if first_run() {
                setup(&args.tty).await
            } else {
                screen(&args.tty, offset).await
            }
        })?;
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
    // A setup a power cut interrupted is finished or undone before anything
    // shows (ADR 0053).
    match recover_setup(
        Path::new(SETUP_MAKING),
        Path::new(SETUP_RECORD),
        |login| users::by_name(login).is_some(),
        |login| Ok(users::finish_person(login)?),
        |login, since| Ok(users::undo_person(login, since)?),
    ) {
        Ok(Some(Recovered::Finished(login))) => {
            eprintln!("glass-greeter: finished the setup a power cut interrupted for {login}")
        }
        Ok(Some(Recovered::Undone(login))) => eprintln!(
            "glass-greeter: took back what an interrupted setup made for {login}; setup asks again"
        ),
        Ok(None) => {}
        Err(e) => eprintln!("glass-greeter: finishing an interrupted setup: {e:#}"),
    }
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
        // `exec`: the desktop takes the place of su's shell, so it is su's
        // one child, the process glassd takes `seat.active` from (ADR 0031).
        let command = match &runtime {
            Some(dir) => format!(
                "exec env XDG_RUNTIME_DIR={} {}",
                dir.display(),
                args.session
            ),
            None => format!("exec {}", args.session),
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
        // glassd learns who is at the screen and which `su` runs their
        // desktop (ADR 0031), so the computer can sleep for them and what
        // follows them (the owner, USB drives, the browser) does. It may
        // be starting, or starting again, as they sign in: it's told until
        // it hears (ADR 0053).
        let telling = su.map(|pid| tokio::spawn(tell_seat(user.clone(), pid)));
        // A keyboard or a mouse that comes while they're signed in is
        // theirs as the ones here at sign-in are.
        let handing = tokio::spawn(hand_later_devices(account.uid, account.gid));
        let tty_name = tty_name(&args.tty);
        let serving = serve(&mut session, &user, &tty_name);
        let ended = until_stopped(serving, &mut terminate, || {
            su.into_iter().for_each(end_session);
        })
        .await;
        // Over before the devices go back to root, or to anyone else.
        handing.abort();
        let _ = handing.await;
        // Never told after they left: glassd refuses a session that has
        // ended, should a try already under way land late.
        if let Some(telling) = telling {
            telling.abort();
        }
        let Some(status) = ended else {
            eprintln!("glass-greeter: ended {user}'s session");
            return stopped(&args.tty);
        };
        eprintln!("glass-greeter: {user}'s session ended ({status:?})");
        match tell_glassd(glass_api::methods::SEAT_END, serde_json::Value::Null).await {
            Told::Heard => {}
            Told::Refused(why) | Told::NotHeard(why) => eprintln!("glass-greeter: {why}"),
        }
        // What the session left running goes with it before the screen
        // and input devices go to anyone else (agents, in jails, stay).
        if let Err(e) = users::end_processes(&user) {
            eprintln!("glass-greeter: ending {user}'s processes: {e}");
        }
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

/// Tells glassd who signed in (`seat.begin`) until it hears, every
/// [`TELL_AGAIN`] while it doesn't answer; a refusal is final. The log says
/// the first time it didn't answer, and when it heard after all.
async fn tell_seat(user: String, session_pid: u32) {
    let begin = serde_json::json!({ "user": user, "session_pid": session_pid });
    let mut waited = false;
    loop {
        match tell_glassd(glass_api::methods::SEAT_BEGIN, begin.clone()).await {
            Told::Heard => {
                if waited {
                    eprintln!("glass-greeter: glassd heard {user} signed in");
                }
                return;
            }
            Told::Refused(why) => {
                eprintln!("glass-greeter: {why}");
                return;
            }
            Told::NotHeard(why) => {
                if !waited {
                    eprintln!("glass-greeter: {why}; telling it again until it hears");
                }
                waited = true;
            }
        }
        tokio::time::sleep(TELL_AGAIN).await;
    }
}

/// What came of telling glassd something.
enum Told {
    Heard,
    /// glassd answered no.
    Refused(String),
    /// glassd isn't there, or didn't answer in time.
    NotHeard(String),
}

/// Tells glassd about the seat (ADR 0031). The desktop works without it;
/// the computer just doesn't sleep on its own, and its settings don't
/// follow who is at the screen.
async fn tell_glassd(method: &str, params: serde_json::Value) -> Told {
    let socket = std::env::var_os(glass_api::CONTROL_SOCKET_ENV).map_or_else(
        || PathBuf::from(glass_api::DEFAULT_CONTROL_SOCKET),
        PathBuf::from,
    );
    let told = async {
        let client = glass_rpc::Client::connect(&socket).await?;
        client.call::<serde_json::Value>(method, params).await
    };
    match tokio::time::timeout(Duration::from_secs(2), told).await {
        Ok(Ok(_)) => Told::Heard,
        Ok(Err(e @ glass_rpc::ClientError::Rpc(_))) => {
            Told::Refused(format!("glassd refused {method}: {e}"))
        }
        Ok(Err(e)) => Told::NotHeard(format!("telling glassd {method}: {e}")),
        Err(_) => Told::NotHeard(format!("glassd didn't answer {method}")),
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

/// While `uid` is signed in: gives them the input devices that come
/// later. A device node is root's when it appears, so the desktop, which
/// looks for new devices as this does, couldn't open a mouse plugged back
/// in or a keyboard that paired after sign-in. Looked for twice as often
/// as the desktop looks, so it's theirs by the desktop's next look.
async fn hand_later_devices(uid: u32, gid: u32) {
    use std::os::unix::fs::MetadataExt;
    loop {
        tokio::time::sleep(input::RESCAN / 2).await;
        for path in glass_platform::input::devices().unwrap_or_default() {
            if !std::fs::metadata(&path).is_ok_and(|m| m.uid() != uid) {
                continue;
            }
            match std::os::unix::fs::chown(&path, Some(uid), Some(gid)) {
                Ok(()) => eprintln!("glass-greeter: {} came, and is theirs", path.display()),
                Err(e) => eprintln!("glass-greeter: {}: {e}", path.display()),
            }
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
    let orphaned = orphaned();
    tokio::pin!(orphaned);
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
            () = &mut orphaned => bail!("the sign-in screen's own supervisor is gone"),
        }
    }
}

/// glassd's settings: a `[managed]` section means an organization manages
/// this computer (ADR 0023).
const GLASSD_CONFIG: &str = "/usr/local/etc/glasshouse/glassd.toml";
/// What first-run setup recorded, beside glassd's state.
const SETUP_RECORD: &str = "/var/db/glasshouse/setup.toml";
/// What setup is making, written whole before it makes anything and put in
/// [`SETUP_RECORD`]'s place once the account is whole: a setup a power cut
/// interrupted is finished or undone when the greeter next starts (UX-2,
/// ADR 0053, specs/tla/Boot.tla).
const SETUP_MAKING: &str = "/var/db/glasshouse/setup.making.toml";

/// First-run setup instead of the sign-in screen (UX-2): nobody can sign in
/// yet, and no organization manages the computer. An enrolled computer
/// asks nothing; its people come from the directory.
fn first_run() -> bool {
    let nobody = users::people().is_ok_and(|people| people.is_empty());
    let set_up = Path::new(SETUP_RECORD).exists();
    let config = std::fs::read_to_string(GLASSD_CONFIG).ok();
    wants_setup(nobody, set_up, config.as_deref())
}

/// Setup is for a computer nobody signs in to yet, that hasn't been set up
/// before and that no organization manages. It runs as root for whoever is
/// at the screen, so anything in doubt means no setup: a config it can't
/// read (every image has one) and any way of writing `managed` in it.
fn wants_setup(nobody: bool, set_up: bool, glassd_config: Option<&str>) -> bool {
    nobody && !set_up && glassd_config.is_some_and(|config| !managed(config))
}

/// Whether glassd's config sets `managed` in any TOML spelling: the
/// `[managed]` table (a comment after it, its subtables), dotted
/// `managed.url` keys or `managed = { … }`. Read generously: a false
/// "managed" only skips setup.
fn managed(config: &str) -> bool {
    config.lines().any(|line| {
        let key = line.trim_start().trim_start_matches(['[', '"', ' ', '\t']);
        key.strip_prefix("managed").is_some_and(|rest| {
            rest.trim_start_matches(['"', ' ', '\t'])
                .starts_with([']', '.', '='])
        })
    })
}

/// First-run setup, until its person's account exists; returns who, to
/// be signed in like someone who typed their password.
async fn setup(tty: &Path) -> anyhow::Result<String> {
    let taken = users::names().context("reading the accounts")?;
    let mut setup = Setup::new(taken);
    let theme = Theme::glass();
    // Setup covers the whole screen: no wallpaper behind it.
    let mut display = Display::open(tty, |_, _| Vec::new())?;
    let draw = |display: &mut Display, setup: &Setup| {
        let line = display.draw(setup::view(&theme, &setup.screen(), display.size()));
        eprintln!("setup: {line}");
    };
    display.focus(
        setup::view(&theme, &setup.screen(), display.size()),
        setup::NAME,
    );
    draw(&mut display, &setup);
    eprintln!("glass-greeter: first-run setup");
    let (mut inputs, _) = input::start(display.pixels());
    let mut terminate = signal(SignalKind::terminate())?;
    let orphaned = orphaned();
    tokio::pin!(orphaned);
    loop {
        tokio::select! {
            Some(first) = inputs.recv() => {
                let mut batch = vec![first];
                while let Ok(more) = inputs.try_recv() {
                    batch.push(more);
                }
                let typed = display.input(&batch, |_, _| None);
                let updated = display.update(
                    setup::view(&theme, &setup.screen(), display.size()),
                    &typed.events,
                );
                let redraw = updated.redraw || !updated.messages.is_empty();
                for message in updated.messages {
                    match setup.update(message) {
                        Some(SetupEffect::Focus(id)) => {
                            display.focus(setup::view(&theme, &setup.screen(), display.size()), id);
                        }
                        Some(SetupEffect::Create { login, full_name, mut password, models }) => {
                            draw(&mut display, &setup);
                            let (who, name) = (login.clone(), full_name.clone());
                            let record = setup_record(&login, models.map(ModelChoice::id));
                            let created = tokio::task::spawn_blocking(move || {
                                let created = make_account(&who, &name, &password, &record);
                                auth::wipe(&mut password);
                                created
                            })
                            .await
                            .context("creating the account")?;
                            setup.wipe();
                            match created {
                                Ok(()) => {
                                    eprintln!("glass-greeter: set up {login}");
                                    display.hand_over();
                                    return Ok(login);
                                }
                                Err(e) => {
                                    eprintln!("glass-greeter: creating {login}: {e:#}");
                                    setup.failed("Your account couldn't be created. Try again.");
                                    // The password is to type again.
                                    display.focus(
                                        setup::view(&theme, &setup.screen(), display.size()),
                                        setup::PASSWORD,
                                    );
                                }
                            }
                        }
                        None => {}
                    }
                }
                if redraw {
                    draw(&mut display, &setup);
                }
            }
            // The fields' caret blinks.
            () = blink(display.next_redraw()) => {
                display.draw(setup::view(&theme, &setup.screen(), display.size()));
            }
            _ = terminate.recv() => bail!("stopped"),
            () = &mut orphaned => bail!("the sign-in screen's own supervisor is gone"),
        }
    }
}

/// What setup records that the account doesn't hold: how agents should
/// reach AI models (MP-1), for Settings and the model gateway, and whose
/// computer it is (ADR 0038): its owner changes its own settings.
fn setup_record(owner: &str, models: Option<&str>) -> String {
    format!(
        "# First-run setup (UX-2), {}\nowner = \"{owner}\"\nmodels = \"{}\"\n",
        OffsetDateTime::now_utc().date(),
        models.unwrap_or("later")
    )
}

/// Setup's account, all or nothing across a power cut (ADR 0053): what it's
/// making is written first, then the account is made and the record put in
/// its place. A refusal takes the journal back; a power cut leaves it for
/// [`recover_setup`].
fn make_account(login: &str, full_name: &str, password: &str, record: &str) -> anyhow::Result<()> {
    let making = Path::new(SETUP_MAKING);
    write_whole(making, record)?;
    if let Err(e) = users::create_person(login, full_name, password) {
        let _ = std::fs::remove_file(making);
        return Err(e.into());
    }
    keep_record(making, Path::new(SETUP_RECORD))
}

/// What a setup a power cut interrupted left (ADR 0053), finished or
/// undone: with its account made, the home is made theirs and the record
/// kept; with none, the home it made goes, so the name can be chosen again
/// and setup asks again. Whose setup it was, if there was one.
/// What a start did about a setup a power cut interrupted.
#[derive(Debug, PartialEq, Eq)]
enum Recovered {
    /// The account was there: its home made theirs, the record kept.
    Finished(String),
    /// It wasn't: the home setup made taken back, and setup asks again.
    Undone(String),
}

fn recover_setup(
    making: &Path,
    record: &Path,
    exists: impl Fn(&str) -> bool,
    finish: impl Fn(&str) -> anyhow::Result<()>,
    undo: impl Fn(&str, std::time::SystemTime) -> anyhow::Result<()>,
) -> anyhow::Result<Option<Recovered>> {
    let Ok(text) = std::fs::read_to_string(making) else {
        return Ok(None);
    };
    let since = std::fs::metadata(making)?.modified()?;
    let Some(login) = recorded_owner(&text) else {
        std::fs::remove_file(making)?;
        return Ok(None);
    };
    if exists(&login) {
        finish(&login)?;
        keep_record(making, record)?;
        Ok(Some(Recovered::Finished(login)))
    } else {
        undo(&login, since)?;
        std::fs::remove_file(making)?;
        Ok(Some(Recovered::Undone(login)))
    }
}

/// The `owner = "<login>"` line setup writes.
fn recorded_owner(record: &str) -> Option<String> {
    record.lines().find_map(|line| {
        let value = line.trim().strip_prefix("owner")?.trim_start();
        let login = value
            .strip_prefix('=')?
            .trim()
            .strip_prefix('"')?
            .strip_suffix('"')?;
        users::valid_login(login).then(|| login.to_owned())
    })
}

/// Puts the journal in the record's place, for good.
fn keep_record(making: &Path, record: &Path) -> anyhow::Result<()> {
    std::fs::rename(making, record).with_context(|| format!("keeping {}", record.display()))?;
    sync_dir(record)
}

/// Writes `path` whole: a temporary file, synced, renamed into place.
fn write_whole(path: &Path, text: &str) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let tmp = path.with_extension("new");
    let mut file =
        std::fs::File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    sync_dir(path)
}

fn sync_dir(path: &Path) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))
}

/// Waits until `at`, or forever without one.
/// Resolves once the process that started this one is gone.
///
/// The screen is drawn by a process of its own, which hands who signed in
/// back to the one that started it. If that one dies, this one would draw
/// on with nobody to hand anything to: a sign-in that checks a password
/// and goes nowhere. It would also keep the log's pipe open, which is what
/// daemon(8) waits on before it starts the sign-in screen again
/// (rc.d/glasshouse_greeter). So it ends itself.
async fn orphaned() {
    let started_by = std::os::unix::process::parent_id();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if std::os::unix::process::parent_id() != started_by {
            return;
        }
    }
}

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
    fn ux2_setup_only_on_an_empty_unmanaged_computer() {
        let plain = "control_socket = \"/x\"\n# [managed] comes with enrollment\n";
        assert!(super::wants_setup(true, false, Some(plain)));
        for managed in [
            "[managed]\nurl = \"https://console\"\n",
            "[managed] # by the org\n",
            "  [managed.keys]\n",
            "managed.url = \"https://console\"\n",
            "managed = { url = \"https://console\" }\n",
            "[\"managed\"]\n",
        ] {
            let config = format!("{plain}{managed}");
            assert!(
                !super::wants_setup(true, false, Some(&config)),
                "enrolled: {managed}"
            );
        }
        assert!(
            !super::wants_setup(false, false, Some(plain)),
            "someone can sign in already"
        );
        assert!(
            !super::wants_setup(true, true, Some(plain)),
            "set up before (its only account disabled since)"
        );
        assert!(
            !super::wants_setup(true, false, None),
            "a config it can't read might be an enrolled computer's"
        );
    }

    /// Boot.tla's "noJournal": a power cut between setup's steps left a
    /// home with no account (that name refused for good) or an account
    /// whose home wasn't theirs. The next start finishes or undoes it.
    #[test]
    fn ux2_a_setup_a_power_cut_interrupted_is_finished_or_undone() {
        use std::cell::RefCell;
        let dir = std::env::temp_dir().join(format!("glass-greeter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (making, record) = (dir.join("setup.making.toml"), dir.join("setup.toml"));
        let text = super::setup_record("dana", Some("api_key"));
        let done = RefCell::new(Vec::new());
        let recover = |account: bool| {
            super::recover_setup(
                &making,
                &record,
                |_| account,
                |login| {
                    done.borrow_mut().push(format!("finish {login}"));
                    Ok(())
                },
                |login, _| {
                    done.borrow_mut().push(format!("undo {login}"));
                    Ok(())
                },
            )
            .unwrap()
        };
        // Nothing interrupted: nothing to do.
        assert_eq!(recover(true), None);

        // Cut after `pw useradd`: the home is made theirs, the record kept.
        super::write_whole(&making, &text).unwrap();
        assert_eq!(
            recover(true),
            Some(super::Recovered::Finished("dana".into()))
        );
        assert_eq!(done.borrow().as_slice(), ["finish dana"]);
        assert_eq!(std::fs::read_to_string(&record).unwrap(), text);
        assert!(!making.exists());

        // Cut before it: the home goes, and setup asks again.
        std::fs::remove_file(&record).unwrap();
        super::write_whole(&making, &text).unwrap();
        assert_eq!(
            recover(false),
            Some(super::Recovered::Undone("dana".into()))
        );
        assert_eq!(done.borrow().last().unwrap(), "undo dana");
        assert!(!making.exists() && !record.exists());

        assert_eq!(super::recorded_owner(&text).as_deref(), Some("dana"));
        assert_eq!(super::recorded_owner("owner = \"../root\"\n"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(
            super::sentence("this account has expired"),
            "This account has expired"
        );
        assert_eq!(super::sentence(""), "");
    }
}
