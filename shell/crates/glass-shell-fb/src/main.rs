//! `glass-shell-fb`: the desktop on the console framebuffer, without a
//! compositor (ADR 0016, the first step of M2). It draws the caller's live
//! tiles from glassd and takes the keyboard and pointer from the kernel's
//! evdev devices: Approve on a tile answers its ask, clicking a tile opens
//! its expanded view (`task.get`) with Pause, Stop, Take over and Roll back,
//! Esc closes it, Ctrl Space opens the command bar, Shift Super Q signs out
//! (the program ends, and the sign-in screen comes back). It redraws when a task
//! changes, the person does something, or the menu bar clock turns over:
//! only the areas whose content changed are drawn again and copied to the
//! screen, and moving the pointer repaints only the rows it crosses.
//!
//! ```text
//! glass-shell-fb [--tty /dev/ttyv0] [--socket /var/run/glasshouse/control.sock] [--once] [--no-input] [--from-sign-in]
//! ```
//!
//! The terminal shows the desktop until the program ends (Ctrl-C or
//! SIGTERM), then goes back to text. With `--from-sign-in` (the sign-in
//! screen starts it so) signing out leaves the last frame up for the
//! sign-in screen to draw over, rather than flashing the text console. `--once` draws one frame and exits; it
//! stays on screen until the console next draws. The tiles are the caller's,
//! as `glassctl tasks` shows them, and the caller needs read and write
//! access to the terminal and read access to `/dev/input/event*`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use glass_api::{Event, TaskDetail, TaskRef, TaskView, UndoReport, methods};
use glass_rpc::{Client, ClientError};
use glass_shell::live::{Effect, Live};
use glass_shell::{Message, Theme, wallpaper};
use glass_shell_fb::display::Display;
use glass_shell_fb::input::{self, Input};
use time::{OffsetDateTime, UtcOffset};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

/// Changes that arrive this close together make one redraw.
const SETTLE: Duration = Duration::from_millis(50);
/// While glassd isn't answering, how often to try it again.
const RECONNECT: Duration = Duration::from_millis(500);

struct Args {
    tty: PathBuf,
    socket: PathBuf,
    once: bool,
    input: bool,
    from_sign_in: bool,
}

fn args() -> anyhow::Result<Args> {
    let mut parsed = Args {
        tty: PathBuf::from("/dev/ttyv0"),
        socket: PathBuf::from("/var/run/glasshouse/control.sock"),
        once: false,
        input: true,
        from_sign_in: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--tty" => parsed.tty = value()?.into(),
            "--socket" => parsed.socket = value()?.into(),
            "--once" => parsed.once = true,
            "--no-input" => parsed.input = false,
            "--from-sign-in" => parsed.from_sign_in = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: glass-shell-fb [--tty PATH] [--socket PATH] [--once] [--no-input] [--from-sign-in]"
                );
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
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(args, offset))
}

async fn run(args: Args, offset: UtcOffset) -> anyhow::Result<()> {
    let started = Instant::now();
    // The desktop comes up whether or not glassd answers yet (it may still
    // be starting, or restarting); its tiles arrive when it does.
    let (mut client, tasks) = match attach(&args.socket).await {
        Ok((client, tasks)) => (Some(client), tasks),
        Err(e) => {
            eprintln!("glassd: {e:#}; trying again");
            (None, Vec::new())
        }
    };

    let asked = started.elapsed();
    let live = Live::new(tasks)
        .without_wallpaper()
        .signed_in_as(signed_in_as());
    let mut shell = Shell::open(args.tty, offset, live)?;
    let opened = started.elapsed();
    shell.draw(true);
    eprintln!(
        "desktop up in {} ms: glassd {} ms, screen {} ms, first frame {} ms",
        started.elapsed().as_millis(),
        asked.as_millis(),
        (opened - asked).as_millis(),
        (started.elapsed() - opened).as_millis()
    );
    if args.once {
        return Ok(());
    }
    let (mut inputs, devices) = if args.input {
        input::start(shell.display.pixels())
    } else {
        (mpsc::unbounded_channel().1, 0)
    };
    eprintln!("reading {devices} input devices");
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    loop {
        let redraw;
        // When the next notification banner's time is up, and when the
        // command bar's caret blinks.
        let banner_gone = until(shell.live.next_change());
        let blink = until(shell.display.next_redraw());
        let reconnect = until(client.is_none().then(|| Instant::now() + RECONNECT));
        tokio::select! {
            note = next_note(client.as_ref()) => {
                let Some(note) = note else {
                    eprintln!("glassd closed the connection; trying again");
                    client = None;
                    continue;
                };
                // The open task changing asks for its detail again, once
                // for everything that arrived together.
                let mut effects = Vec::from_iter(shell.apply(note));
                let settled = tokio::time::sleep(SETTLE);
                tokio::pin!(settled);
                loop {
                    tokio::select! {
                        () = &mut settled => break,
                        Some(note) = next_note(client.as_ref()) => effects.extend(shell.apply(note)),
                    }
                }
                shell.carry_out_all(client.as_ref(), effects).await;
                redraw = true;
            }
            () = reconnect => {
                let Ok((back, tasks)) = attach(&args.socket).await else {
                    continue;
                };
                eprintln!("glassd: connected again, {} tiles", tasks.len());
                client = Some(back);
                let effects = Vec::from_iter(shell.live.reload(tasks));
                shell.carry_out_all(client.as_ref(), effects).await;
                redraw = true;
            }
            Some(first) = inputs.recv() => {
                // Everything already waiting, then one redraw for all of it.
                let mut batch = vec![first];
                while let Ok(more) = inputs.try_recv() {
                    batch.push(more);
                }
                let effects = shell.input(&batch);
                if effects.contains(&Effect::SignOut) {
                    eprintln!("signing out");
                    if args.from_sign_in {
                        shell.display.hand_over();
                    }
                    return Ok(());
                }
                shell.carry_out_all(client.as_ref(), effects).await;
                redraw = shell.changed();
            }
            () = banner_gone => redraw = shell.live.tick(Instant::now()),
            () = blink => {
                // Only the caret: not worth a log line twice a second.
                shell.draw(false);
                continue;
            }
            () = tokio::time::sleep(until_next_minute(OffsetDateTime::now_utc())) => redraw = true,
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
        }
        if redraw {
            shell.draw(true);
        }
    }
    Ok(())
}

/// Connects to glassd, subscribes to its events and lists the caller's
/// tasks: subscribing first, so no change falls between the two.
async fn attach(socket: &std::path::Path) -> anyhow::Result<(Client, Vec<TaskView>)> {
    let client = Client::connect(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let _: serde_json::Value = client
        .call(methods::EVENTS_SUBSCRIBE, ())
        .await
        .map_err(|e| anyhow!("events.subscribe: {e}"))?;
    let tasks: Vec<TaskView> = client
        .call(methods::TASK_LIST, ())
        .await
        .map_err(|e| anyhow!("task.list: {e}"))?;
    Ok((client, tasks))
}

/// glassd's next notification; `None` when the connection closed, and
/// never while there's no connection.
async fn next_note(client: Option<&Client>) -> Option<glass_rpc::Request> {
    match client {
        Some(client) => client.next_notification().await,
        None => std::future::pending().await,
    }
}

/// Waits until `at`, or forever without one.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// Who is signed in, as they'd read it: the account's full name, else its
/// login name (`su -l` sets `USER`).
fn signed_in_as() -> String {
    let login = std::env::var("USER").unwrap_or_default();
    glass_platform::users::people()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.name == login)
        .map(|p| p.full_name)
        .filter(|name| !name.is_empty())
        .unwrap_or(login)
}

/// The desktop on the framebuffer: its state and the screen it draws on.
struct Shell {
    display: Display,
    theme: Theme,
    offset: UtcOffset,
    live: Live,
    /// Something changed that the next frame should show.
    dirty: bool,
}

impl Shell {
    fn open(tty: PathBuf, offset: UtcOffset, live: Live) -> anyhow::Result<Self> {
        let theme = Theme::glass();
        // The wallpaper stays drawn behind the UI, so a change redraws only
        // what's over it.
        let display = Display::open(&tty, |w, h| wallpaper::backdrop(&theme.wallpaper, w, h))?;
        Ok(Self {
            display,
            theme,
            offset,
            live,
            dirty: false,
        })
    }

    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc().to_offset(self.offset)
    }

    /// Folds in one notification from glassd, and returns what it asks for
    /// (the open task's detail again, when that task changed).
    fn apply(&mut self, note: glass_rpc::Request) -> Option<Effect> {
        let event = note
            .params
            .filter(|_| note.method == methods::EVENT)
            .and_then(|params| serde_json::from_value::<Event>(params).ok());
        event.and_then(|event| self.live.apply(event))
    }

    /// Handles a batch of input: the pointer moves at once; where it went,
    /// clicks and keys go through the UI. Returns what they ask for.
    fn input(&mut self, batch: &[Input]) -> Vec<Effect> {
        let live = &self.live;
        let typed = self
            .display
            .input(batch, |key, modifiers| live.shortcut(key, modifiers));
        let mut messages = typed.shortcuts;
        if !typed.events.is_empty() {
            let bar = self.live.command_bar();
            let view = self
                .live
                .view(&self.theme, bar.as_ref(), self.now(), self.display.size());
            let updated = self.display.update(view, &typed.events);
            messages.extend(updated.messages);
            self.dirty |= updated.redraw;
        }
        let mut effects = Vec::new();
        for message in messages {
            // The pointer resting on a banner, or leaving it, changes only
            // when the banner goes.
            if !matches!(message, Message::HoldBanners | Message::LetBannersGo) {
                self.dirty = true;
            }
            match self.live.update(message) {
                Some(Effect::Focus(id)) => {
                    let bar = self.live.command_bar();
                    let view =
                        self.live
                            .view(&self.theme, bar.as_ref(), self.now(), self.display.size());
                    self.display.focus(view, id);
                }
                Some(effect) => effects.push(effect),
                None => {}
            }
        }
        effects
    }

    /// Whether the last input changed what the screen should show.
    fn changed(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Makes the calls effects ask for, then the ones their answers ask
    /// for, each once.
    async fn carry_out_all(&mut self, client: Option<&Client>, effects: Vec<Effect>) {
        let Some(client) = client else {
            if !effects.is_empty() {
                eprintln!("glassd isn't answering; not done: {effects:?}");
            }
            return;
        };
        let mut queue = VecDeque::new();
        for effect in effects {
            if !queue.contains(&effect) {
                queue.push_back(effect);
            }
        }
        while let Some(effect) = queue.pop_front() {
            for next in self.carry_out(client, effect).await {
                if !queue.contains(&next) {
                    queue.push_back(next);
                }
            }
        }
    }

    /// Makes the call an effect asks for and hands its answer to the
    /// desktop; returns what that asks for next. A refused control shows
    /// up in the tile's state, so it's only logged here; a refused roll
    /// back is told on its card.
    async fn carry_out(&mut self, client: &Client, effect: Effect) -> Vec<Effect> {
        match effect {
            Effect::Answer(answer) => {
                let result: Result<serde_json::Value, _> =
                    client.call(methods::APPROVAL_ANSWER, &answer).await;
                match result {
                    Ok(_) => eprintln!("answered {}", answer.approval),
                    Err(e) => eprintln!("approval.answer: {e}"),
                }
                Vec::new()
            }
            // Done in `input`, which has the view; signing out ends the loop.
            Effect::Focus(_) | Effect::SignOut => Vec::new(),
            Effect::Get(task) => {
                let started = Instant::now();
                let result: Result<TaskDetail, _> =
                    client.call(methods::TASK_GET, TaskRef { task }).await;
                match result {
                    Ok(detail) => {
                        eprintln!(
                            "task.get {}: {} ms",
                            task.short(),
                            started.elapsed().as_millis()
                        );
                        self.live.detail(detail);
                        self.dirty = true;
                    }
                    Err(e) => eprintln!("task.get: {e}"),
                }
                Vec::new()
            }
            Effect::Undo(request) => {
                let result: Result<UndoReport, _> = client.call(methods::TASK_UNDO, &request).await;
                self.dirty = true;
                match result {
                    Ok(report) => self.live.undo_report(report).into_iter().collect(),
                    Err(e) => {
                        eprintln!("task.undo: {e}");
                        self.live.undo_failed(plain(&e));
                        Vec::new()
                    }
                }
            }
            other => {
                let Some((method, task)) = other.task_call() else {
                    return Vec::new();
                };
                let result: Result<TaskView, _> = client.call(method, &task).await;
                match result {
                    // The new state shows at once; the event that follows
                    // changes nothing more.
                    Ok(view) => {
                        self.dirty = true;
                        self.live
                            .apply(Event::TaskUpdated {
                                task: Box::new(view),
                            })
                            .into_iter()
                            .collect()
                    }
                    Err(e) => {
                        eprintln!("{method}: {e}");
                        Vec::new()
                    }
                }
            }
        }
    }

    /// Draws what changed, with a line in the log if `log`.
    fn draw(&mut self, log: bool) {
        // This frame shows whatever was waiting to be shown.
        self.dirty = false;
        let bar = self.live.command_bar();
        let view = self
            .live
            .view(&self.theme, bar.as_ref(), self.now(), self.display.size());
        let drawn = self.display.draw(view);
        if log {
            eprintln!("{} tiles: {drawn}", self.live.tasks().len());
        }
    }
}

/// A refusal in the person's words: glassd's own message, without the
/// protocol's error code.
fn plain(error: &ClientError) -> String {
    match error {
        ClientError::Rpc(e) => e.message.clone(),
        ClientError::Closed | ClientError::Connect { .. } => {
            "Lost touch with the system. Try again in a moment.".to_owned()
        }
        ClientError::Decode(_) => "Got an answer this desktop doesn't understand.".to_owned(),
    }
}

/// How long until the clock next shows a new minute.
fn until_next_minute(now: OffsetDateTime) -> Duration {
    let into = Duration::new(u64::from(now.second()), now.nanosecond());
    Duration::from_secs(60).saturating_sub(into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_redraws_on_the_minute() {
        let at = OffsetDateTime::from_unix_timestamp(1_792_418_040 + 45).expect("valid");
        assert_eq!(until_next_minute(at), Duration::from_secs(15));
    }
}
