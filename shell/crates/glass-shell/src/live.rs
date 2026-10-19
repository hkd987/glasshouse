//! The live desktop: the user's tiles as glassd reports them, what the
//! person has open, and what their clicks and keys ask of glassd. A shell
//! feeds it glassd's events and the person's input, draws its view, and
//! makes the calls it returns, handing their answers back
//! ([`Live::detail`], [`Live::undo_report`]).

use glass_api::{
    AnswerApproval, ApprovalAnswer, ApprovalScope, Event, TaskDetail, TaskRef, TaskView,
    UndoReport, UndoRequest, methods,
};
use std::time::{Duration, Instant};

use glass_api::TileState;
use glass_types::{ApprovalId, SnapshotId, TaskId};
use iced_core::keyboard::{Key, Modifiers, key::Named};
use time::OffsetDateTime;

use crate::screens::command_bar::{self, CommandBar};
use crate::screens::desktop::{self, Desktop, Tiles};
use crate::screens::expanded::{self, Expanded, Rollback, Tab};
use crate::screens::system;
use crate::{Element, Message, Power, Theme};

#[derive(Debug)]
pub struct Live {
    /// From `task.list`, then kept current from `events.subscribe`.
    tasks: Vec<TaskView>,
    /// The command bar's text while it's open.
    command: Option<String>,
    /// Whether the view draws the wallpaper, or the screen keeps it as its
    /// backdrop.
    wallpaper: bool,
    /// The tile opened into the expanded view.
    open: Option<Open>,
    /// Tiles showing as notifications, newest last.
    banners: Vec<Banner>,
    /// The pointer is on the banners, so none goes.
    held: bool,
    /// The agents menu is open.
    menu: bool,
    /// The system menu (under the mark in the menu bar) is open.
    system_menu: bool,
    /// Who is signed in, as the system menu names them.
    person: String,
    /// This build and computer, for About Glasshouse.
    about: system::About,
    /// About or Force Quit, when one is open.
    window: Option<SystemWindow>,
    /// The Trash has something in it.
    trash_full: bool,
    /// Restarting or shutting down: the desktop's last frame says so.
    leaving: Option<Power>,
}

/// A window the system menu opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemWindow {
    About,
    /// The agent chosen to stop, if one is.
    ForceQuit(Option<TaskId>),
    /// "Restart now?" or "Shut down now?"
    Power(Power),
}

/// How long a notification stays on screen, as a macOS banner does. One
/// that needs the person stays a little longer; either way the agents menu
/// keeps every agent a click away, and the pill shows the orange dot.
pub const BANNER: Duration = Duration::from_secs(4);
pub const NEEDS_YOU_BANNER: Duration = Duration::from_secs(7);
/// Banners at once; an older one makes room for a newer one.
const MOST_BANNERS: usize = 3;
/// Once the pointer leaves the banners, the least time they stay.
pub const AFTER_HOLD: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Banner {
    task: TaskId,
    until: Instant,
}

/// An expanded tile and the view's own state.
#[derive(Debug)]
struct Open {
    task: TaskId,
    /// From `task.get`, refetched when the task changes.
    detail: Option<TaskDetail>,
    tab: Tab,
    selected: Option<SnapshotId>,
    rollback: Option<Rollback>,
}

/// What a message asks the shell to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Answer an ask (`approval.answer`): approve, or decline.
    Answer(AnswerApproval),
    /// Give the keyboard focus to the widget with this id.
    Focus(&'static str),
    /// Fetch the open task's detail (`task.get`) and hand it to
    /// [`Live::detail`].
    Get(TaskId),
    /// `task.pause`, `task.resume`, `task.stop`, `task.take_over` and
    /// `task.hand_back` (AW-11); each answers with the task's new
    /// `TaskView`, which [`Live::apply`] takes as a `TaskUpdated` event.
    Pause(TaskId),
    Resume(TaskId),
    Stop(TaskId),
    TakeOver(TaskId),
    HandBack(TaskId),
    /// End the session: the desktop program exits and the sign-in screen
    /// comes back (ADR 0019).
    SignOut,
    /// End the session so the sign-in screen restarts or shuts down the
    /// computer (`session::Power`); the desktop shows it's leaving first.
    Power(Power),
    /// Roll back (`task.undo`, AW-15) and hand the report to
    /// [`Live::undo_report`], or the refusal to [`Live::undo_failed`].
    Undo(UndoRequest),
}

impl Effect {
    /// The control-API call for the effects that act on a task by
    /// reference, and answer with its `TaskView`.
    pub fn task_call(&self) -> Option<(&'static str, TaskRef)> {
        let (method, task) = match *self {
            Effect::Pause(task) => (methods::TASK_PAUSE, task),
            Effect::Resume(task) => (methods::TASK_RESUME, task),
            Effect::Stop(task) => (methods::TASK_STOP, task),
            Effect::TakeOver(task) => (methods::TASK_TAKE_OVER, task),
            Effect::HandBack(task) => (methods::TASK_HAND_BACK, task),
            _ => return None,
        };
        Some((method, TaskRef { task }))
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Live {
    /// The desktop with the agents `task.list` gave. Any that need the
    /// person show as banners at first, so what's waiting is seen.
    pub fn new(tasks: Vec<TaskView>) -> Self {
        let mut live = Self {
            tasks,
            command: None,
            wallpaper: true,
            open: None,
            banners: Vec::new(),
            held: false,
            menu: false,
            system_menu: false,
            person: String::new(),
            about: system::About::default(),
            window: None,
            trash_full: false,
            leaving: None,
        };
        let waiting: Vec<TaskId> = live
            .tasks
            .iter()
            .filter(|t| t.state == TileState::NeedsYou)
            .map(|t| t.id)
            .collect();
        let at = Instant::now();
        for task in waiting {
            live.notify(task, TileState::NeedsYou, at);
        }
        live
    }

    /// Leaves the wallpaper out of the view, for a screen that keeps it as
    /// its backdrop (`render::Screen::set_backdrop`).
    pub fn without_wallpaper(mut self) -> Self {
        self.wallpaper = false;
        self
    }

    /// Whether the Trash has anything in it; true when that changed.
    pub fn set_trash_full(&mut self, full: bool) -> bool {
        std::mem::replace(&mut self.trash_full, full) != full
    }

    /// This build and computer, for About Glasshouse.
    pub fn about(mut self, about: system::About) -> Self {
        self.about = about;
        self
    }

    /// Who is signed in, for "Sign out <name>" in the system menu.
    pub fn signed_in_as(mut self, person: impl Into<String>) -> Self {
        self.person = person.into();
        self
    }

    pub fn tasks(&self) -> &[TaskView] {
        &self.tasks
    }

    /// The task in the expanded view.
    pub fn open_task(&self) -> Option<TaskId> {
        self.open.as_ref().map(|o| o.task)
    }

    /// Folds in one of glassd's events. Returns a refetch when the task in
    /// the expanded view changed: the tile's own fields are current at
    /// once, its activity and changes after `task.get`.
    pub fn apply(&mut self, event: Event) -> Option<Effect> {
        self.apply_at(event, Instant::now())
    }

    /// [`Live::apply`] at a given moment: a change worth knowing about
    /// shows as a banner from `at`.
    pub fn apply_at(&mut self, event: Event, at: Instant) -> Option<Effect> {
        match event {
            Event::TaskUpdated { task } => {
                let id = task.id;
                let before = self
                    .tasks
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| (t.state, t.needs.as_ref().map(|a| a.id)));
                if worth_a_banner(before, &task) {
                    self.notify(id, task.state, at);
                }
                match self.tasks.iter_mut().find(|t| t.id == id) {
                    Some(tile) => *tile = *task,
                    None => self.tasks.push(*task),
                }
                (self.open_task() == Some(id)).then_some(Effect::Get(id))
            }
            Event::TaskRemoved { task } => {
                self.tasks.retain(|t| t.id != task);
                self.banners.retain(|b| b.task != task);
                // The tile was dismissed: nothing to go back to.
                if self.open_task() == Some(task) {
                    self.open = None;
                }
                None
            }
        }
    }

    /// The task list afresh, from `task.list` after glassd came back: tiles
    /// that are gone leave the banners and the expanded view; the open one,
    /// if still there, has its detail fetched again. Nothing shows as news.
    pub fn reload(&mut self, tasks: Vec<TaskView>) -> Option<Effect> {
        self.tasks = tasks;
        let present = |id: TaskId| self.tasks.iter().any(|t| t.id == id);
        self.banners.retain(|b| present(b.task));
        match self.open_task() {
            Some(open) if present(open) => Some(Effect::Get(open)),
            Some(_) => {
                self.open = None;
                None
            }
            None => None,
        }
    }

    /// Shows `task` as a banner from `at`.
    fn notify(&mut self, task: TaskId, state: TileState, at: Instant) {
        let lasts = if state == TileState::NeedsYou {
            NEEDS_YOU_BANNER
        } else {
            BANNER
        };
        if self.banners.is_empty() {
            // The pointer was on banners that are gone.
            self.held = false;
        }
        self.banners.retain(|b| b.task != task);
        self.banners.push(Banner {
            task,
            until: at + lasts,
        });
        if self.banners.len() > MOST_BANNERS {
            self.banners.remove(0);
        }
    }

    /// Takes away the banners whose time is up at `at`, unless the pointer
    /// is on them. True when any went, so the screen needs drawing.
    pub fn tick(&mut self, at: Instant) -> bool {
        if self.held {
            return false;
        }
        let before = self.banners.len();
        self.banners.retain(|b| b.until > at);
        self.banners.len() != before
    }

    /// When the next banner goes, for the shell to wake then.
    pub fn next_change(&self) -> Option<Instant> {
        if self.held {
            return None;
        }
        self.banners.iter().map(|b| b.until).min()
    }

    /// The tasks showing as banners.
    pub fn banners(&self) -> Vec<TaskId> {
        self.banners.iter().map(|b| b.task).collect()
    }

    /// The task an ask belongs to.
    fn asking(&self, approval: ApprovalId) -> Option<TaskId> {
        self.tasks
            .iter()
            .find(|t| t.needs.as_ref().is_some_and(|a| a.id == approval))
            .map(|t| t.id)
    }

    /// `task.get`'s answer for the expanded view.
    pub fn detail(&mut self, detail: TaskDetail) {
        let Some(open) = self.open.as_mut().filter(|o| o.task == detail.view.id) else {
            return;
        };
        // A snapshot that's gone (or a task that has none) can't be picked.
        if let Some(picked) = open.selected
            && !detail.snapshots.iter().any(|s| s.id == picked)
        {
            open.selected = None;
        }
        open.detail = Some(detail);
    }

    /// `task.undo`'s answer: what a roll back would change, then what it
    /// changed. After a real one, the detail is refetched to show it.
    pub fn undo_report(&mut self, report: UndoReport) -> Option<Effect> {
        let open = self.open.as_mut()?;
        match (open.rollback.take(), report.dry_run) {
            (Some(Rollback::Checking { to }), true) => {
                open.rollback = Some(Rollback::Preview { to, report });
                None
            }
            (Some(Rollback::RollingBack { to }), false) => {
                open.rollback = Some(Rollback::Done { to, report });
                Some(Effect::Get(open.task))
            }
            // An answer for a card that was closed meanwhile.
            (other, _) => {
                open.rollback = other;
                None
            }
        }
    }

    /// `task.undo` refused, in words for the person.
    pub fn undo_failed(&mut self, message: String) {
        if let Some(open) = self.open.as_mut()
            && let Some(rollback) = open.rollback.as_ref().filter(|r| r.in_flight())
        {
            open.rollback = Some(Rollback::Failed {
                to: rollback.to(),
                message,
            });
        }
    }

    /// What `message` asks for. Messages for parts not built yet (the
    /// desktop agent's answers, sign in) do nothing.
    pub fn update(&mut self, message: Message) -> Option<Effect> {
        self.update_at(message, Instant::now())
    }

    /// [`Live::update`] at a given moment, which times the banners.
    pub fn update_at(&mut self, message: Message, at: Instant) -> Option<Effect> {
        let effect = self.react(message, at);
        // Banners something now covers can't hear the pointer leave them.
        if !self.banners_shown() {
            self.held = false;
        }
        effect
    }

    /// Whether the banners are on screen: there are some, and nothing is
    /// over them (as [`Live::view`] decides).
    fn banners_shown(&self) -> bool {
        !self.banners.is_empty()
            && !self.menu
            && !self.system_menu
            && self.window.is_none()
            && self.open.is_none()
            && self.command.is_none()
    }

    fn react(&mut self, message: Message, at: Instant) -> Option<Effect> {
        match message {
            Message::HoldBanners => {
                self.held = !self.banners.is_empty();
                None
            }
            Message::LetBannersGo => {
                if std::mem::take(&mut self.held) {
                    for banner in &mut self.banners {
                        banner.until = banner.until.max(at + AFTER_HOLD);
                    }
                }
                None
            }
            // AW-10: open the tile; its activity and changes come from
            // `task.get`.
            Message::OpenTask(task) => self.open(task),
            Message::OpenAbout => {
                self.system_menu = false;
                self.window = Some(SystemWindow::About);
                None
            }
            Message::OpenForceQuit => {
                self.system_menu = false;
                self.menu = false;
                self.command = None;
                self.window = Some(SystemWindow::ForceQuit(None));
                None
            }
            Message::ChooseToQuit(task) => {
                if let Some(SystemWindow::ForceQuit(chosen)) = &mut self.window {
                    *chosen = Some(task);
                }
                None
            }
            Message::ForceQuit => {
                let Some(SystemWindow::ForceQuit(Some(task))) = self.window else {
                    return None;
                };
                self.window = None;
                Some(Effect::Stop(task))
            }
            Message::CloseSystemWindow => {
                self.window = None;
                None
            }
            Message::AskPower(power) => {
                self.system_menu = false;
                self.menu = false;
                self.command = None;
                self.window = Some(SystemWindow::Power(power));
                None
            }
            Message::ConfirmPower => {
                let Some(SystemWindow::Power(power)) = self.window.take() else {
                    return None;
                };
                self.leaving = Some(power);
                Some(Effect::Power(power))
            }
            Message::ToggleSystemMenu => {
                self.system_menu = !self.system_menu;
                if self.system_menu {
                    self.menu = false;
                    self.command = None;
                }
                None
            }
            Message::ToggleAgents => {
                self.menu = !self.menu;
                self.system_menu = false;
                if self.menu {
                    // Everything is in the menu; nothing else on top.
                    self.command = None;
                    self.banners.clear();
                }
                None
            }
            Message::CloseAgents => {
                self.menu = false;
                None
            }
            // DA-12: Review opens the expanded view on the ask's items.
            Message::Review(approval) => {
                let task = self
                    .tasks
                    .iter()
                    .find(|t| t.needs.as_ref().is_some_and(|a| a.id == approval))?
                    .id;
                self.open(task)
            }
            // AW-16: back to the tile; the agent carries on.
            Message::CloseTask => {
                self.open = None;
                None
            }
            Message::ShowTab(tab) => {
                if let Some(open) = &mut self.open {
                    open.tab = tab;
                }
                None
            }
            // DA-12: the needs-you tile's primary button approves once.
            Message::Approve(approval) => {
                // Answered: its banner has done its job.
                if let Some(task) = self.asking(approval) {
                    self.banners.retain(|b| b.task != task);
                }
                Some(Effect::Answer(AnswerApproval {
                    approval,
                    answer: ApprovalAnswer::Approve {
                        scope: ApprovalScope::Once,
                    },
                }))
            }
            Message::Decline(approval) => Some(Effect::Answer(AnswerApproval {
                approval,
                answer: ApprovalAnswer::Deny,
            })),
            // AW-11.
            Message::Pause(task) => Some(Effect::Pause(task)),
            Message::Resume(task) => Some(Effect::Resume(task)),
            Message::Stop(task) => Some(Effect::Stop(task)),
            Message::TakeOver(task) => Some(Effect::TakeOver(task)),
            Message::HandBack(task) => Some(Effect::HandBack(task)),
            // AW-15: pick a marker, see what rolling back to it would
            // change (`dry_run`), then roll back.
            Message::SelectSnapshot(snapshot) => {
                let open = self.open.as_mut()?;
                if open.rollback.as_ref().is_some_and(Rollback::in_flight) {
                    return None;
                }
                open.selected = Some(snapshot);
                open.rollback = None;
                None
            }
            Message::RollBack => {
                let open = self.open.as_mut()?;
                if open.rollback.as_ref().is_some_and(Rollback::in_flight) {
                    return None;
                }
                let detail = open.detail.as_ref()?;
                let to = expanded::target(&detail.snapshots, open.selected)?.id;
                open.rollback = Some(Rollback::Checking { to });
                Some(Effect::Undo(UndoRequest {
                    task: open.task,
                    to: Some(to),
                    changes: Vec::new(),
                    dry_run: true,
                }))
            }
            Message::ConfirmRollBack => {
                let open = self.open.as_mut()?;
                let Some(Rollback::Preview { to, .. }) = open.rollback else {
                    return None;
                };
                open.rollback = Some(Rollback::RollingBack { to });
                Some(Effect::Undo(UndoRequest {
                    task: open.task,
                    to: Some(to),
                    changes: Vec::new(),
                    dry_run: false,
                }))
            }
            Message::CancelRollBack => {
                if let Some(open) = &mut self.open
                    && !open.rollback.as_ref().is_some_and(Rollback::in_flight)
                {
                    open.rollback = None;
                }
                None
            }
            Message::OpenCommandBar => {
                self.system_menu = false;
                self.command.get_or_insert_with(String::new);
                Some(Effect::Focus(command_bar::INPUT))
            }
            Message::CommandChanged(text) => {
                if let Some(command) = &mut self.command {
                    *command = text;
                }
                None
            }
            Message::CloseCommandBar => {
                self.command = None;
                None
            }
            Message::RunSuggestion(_)
            | Message::AcceptAnswer
            | Message::PasswordChanged(_)
            | Message::SignIn
            | Message::SwitchUser => None,
            Message::SignOut => {
                self.system_menu = false;
                Some(Effect::SignOut)
            }
        }
    }

    fn open(&mut self, task: TaskId) -> Option<Effect> {
        // Opened from its banner or the menu: those step aside.
        self.menu = false;
        self.banners.retain(|b| b.task != task);
        // Opening the tile that's already open keeps its tab and detail.
        if self.open_task() != Some(task) {
            self.open = Some(Open {
                task,
                detail: None,
                tab: Tab::default(),
                selected: None,
                rollback: None,
            });
        }
        Some(Effect::Get(task))
    }

    /// The desktop's own keys: Ctrl Space opens and closes the command bar
    /// (DA-1); Esc closes About or Force Quit, the command bar, the agents
    /// menu, then a roll back card, then the expanded view (AW-16); Shift
    /// Super Q signs out; Alt Super Esc (a Mac's Option Command Esc) or Ctrl
    /// Shift Esc (Windows' task manager, DK-2) opens Force Quit.
    pub fn shortcut(&self, key: &Key, modifiers: Modifiers) -> Option<Message> {
        let bar = self.command.is_some();
        match key {
            Key::Named(Named::Escape)
                if (modifiers.alt() && modifiers.logo())
                    || (modifiers.control() && modifiers.shift()) =>
            {
                Some(Message::OpenForceQuit)
            }
            Key::Named(Named::Escape) if self.window.is_some() => Some(Message::CloseSystemWindow),
            Key::Named(Named::Space) if modifiers.control() => Some(if bar {
                Message::CloseCommandBar
            } else {
                Message::OpenCommandBar
            }),
            // Sign out, as on a Mac (Shift Command Q).
            Key::Character(c)
                if c.eq_ignore_ascii_case("q") && modifiers.shift() && modifiers.logo() =>
            {
                Some(Message::SignOut)
            }
            Key::Named(Named::Escape) if bar => Some(Message::CloseCommandBar),
            Key::Named(Named::Escape) if self.menu => Some(Message::CloseAgents),
            Key::Named(Named::Escape) if self.system_menu => Some(Message::ToggleSystemMenu),
            Key::Named(Named::Escape) => {
                let open = self.open.as_ref()?;
                Some(match &open.rollback {
                    Some(rollback) if !rollback.in_flight() => Message::CancelRollBack,
                    _ => Message::CloseTask,
                })
            }
            _ => None,
        }
    }

    /// The command bar's content while it's open, for [`Live::view`].
    pub fn command_bar(&self) -> Option<CommandBar<'_>> {
        self.command.as_ref().map(|query| CommandBar {
            query: query.clone(),
            answer: None,
            suggestions: Vec::new(),
            recent: &self.tasks,
            model: String::new(),
        })
    }

    /// The expanded view's content while a tile is open, at `now` (local
    /// time). The tile's own fields come from the event-fed list, so they
    /// are current even before a refetch lands.
    pub fn expanded(&self, now: OffsetDateTime) -> Option<Expanded<'_>> {
        let open = self.open.as_ref()?;
        let detail = open.detail.as_ref();
        let task = self
            .tasks
            .iter()
            .find(|t| t.id == open.task)
            .or(detail.map(|d| &d.view))?;
        Some(Expanded {
            task,
            detail,
            tab: open.tab,
            selected: open.selected,
            rollback: open.rollback.as_ref(),
            now,
        })
    }

    /// The desktop at `now` (local time), with the expanded view and `bar`
    /// from [`Live::command_bar`] over it.
    pub fn view<'a>(
        &'a self,
        theme: &Theme,
        bar: Option<&'a CommandBar<'a>>,
        now: OffsetDateTime,
        size: (f32, f32),
    ) -> Element<'a, Message> {
        // Restarting or shutting down: that's all the screen says, over
        // everything, until the machine goes.
        if let Some(power) = self.leaving {
            return system::leaving_overlay(power);
        }
        let expanded = self.expanded(now);
        // Nothing sits over the command bar or the expanded view; otherwise
        // the menu if it's open, else whatever is a banner right now.
        let tiles =
            if bar.is_some() || expanded.is_some() || self.system_menu || self.window.is_some() {
                Tiles::Hidden
            } else if self.menu {
                Tiles::Menu
            } else {
                let shown: Vec<&TaskView> = self
                    .banners
                    .iter()
                    .filter_map(|b| self.tasks.iter().find(|t| t.id == b.task))
                    .collect();
                if shown.is_empty() {
                    Tiles::Hidden
                } else {
                    Tiles::Banners(shown)
                }
            };
        let mut state = Desktop {
            tiles,
            wallpaper: self.wallpaper,
            system_menu: self.system_menu.then(|| self.person.clone()),
            trash_full: self.trash_full,
            ..Desktop::live(&self.tasks, now)
        };
        let mut overlays = Vec::new();
        if let Some(expanded) = &expanded {
            // The agent is the focused app (the artboard's menu bar).
            state.app = expanded.task.agent_name.clone();
            state.menus = expanded::MENUS.map(String::from).to_vec();
            overlays.push(expanded::overlay(theme, expanded));
        }
        overlays.extend(bar.map(|bar| command_bar::overlay(theme, bar)));
        match self.window {
            Some(SystemWindow::About) => overlays.push(system::about_overlay(theme, &self.about)),
            Some(SystemWindow::ForceQuit(chosen)) => {
                let running = self.tasks.iter().filter(|t| t.state.is_live()).collect();
                overlays.push(system::force_quit_overlay(
                    theme,
                    &system::ForceQuit { running, chosen },
                ));
            }
            Some(SystemWindow::Power(power)) => {
                let running = self.tasks.iter().filter(|t| t.state.is_live()).count();
                overlays.push(system::power_overlay(theme, power, running));
            }
            None => {}
        }
        let overlay = match overlays.len() {
            0 | 1 => overlays.pop(),
            _ => Some(crate::layered(overlays)),
        };
        desktop::view(theme, &state, Vec::new(), |_| None, overlay, size)
    }
}

/// Whether a task's update is worth a notification: a new agent, an ask
/// (a new one, too), or the end of its work. Progress and captions aren't.
fn worth_a_banner(before: Option<(TileState, Option<ApprovalId>)>, now: &TaskView) -> bool {
    let Some((state, ask)) = before else {
        return true;
    };
    let ended = matches!(
        now.state,
        TileState::Done | TileState::Failed | TileState::Stopped
    );
    let asks = now.state == TileState::NeedsYou && ask != now.needs.as_ref().map(|a| a.id);
    (state != now.state && (ended || now.state == TileState::NeedsYou)) || asks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use glass_api::TileState;

    /// A live desktop with the artboard's invoice agent among the tiles.
    fn with_invoices() -> (Live, TaskDetail) {
        let detail = fixtures::invoice_detail();
        let mut tasks = fixtures::tasks();
        tasks.push(detail.view.clone());
        (Live::new(tasks), detail)
    }

    fn opened() -> (Live, TaskDetail) {
        let (mut live, detail) = with_invoices();
        assert_eq!(
            live.update(Message::OpenTask(detail.view.id)),
            Some(Effect::Get(detail.view.id))
        );
        live.detail(detail.clone());
        (live, detail)
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(fixtures::expanded_now().unix_seconds()).expect("valid")
    }

    #[test]
    fn da12_approve_on_a_tile_answers_its_ask_once() {
        let tasks = fixtures::tasks();
        let ask = tasks
            .iter()
            .find_map(|t| t.needs.clone())
            .expect("a needs-you tile");
        let mut live = Live::new(tasks);
        assert_eq!(
            live.update(Message::Approve(ask.id)),
            Some(Effect::Answer(AnswerApproval {
                approval: ask.id,
                answer: ApprovalAnswer::Approve {
                    scope: ApprovalScope::Once
                },
            }))
        );
    }

    #[test]
    fn da1_ctrl_space_opens_the_command_bar_and_focuses_it() {
        let mut live = Live::default();
        let open = live.shortcut(&Key::Named(Named::Space), Modifiers::CTRL);
        assert_eq!(open, Some(Message::OpenCommandBar));
        assert_eq!(
            live.update(Message::OpenCommandBar),
            Some(Effect::Focus(command_bar::INPUT))
        );
        live.update(Message::CommandChanged("prep me".into()));
        assert_eq!(live.command_bar().map(|b| b.query), Some("prep me".into()));
        // Space alone is for typing.
        assert_eq!(
            live.shortcut(&Key::Named(Named::Space), Modifiers::empty()),
            None
        );
        let close = live.shortcut(&Key::Named(Named::Escape), Modifiers::empty());
        assert_eq!(close, Some(Message::CloseCommandBar));
        live.update(Message::CloseCommandBar);
        assert!(live.command_bar().is_none());
    }

    #[test]
    fn events_update_add_and_remove_tiles() {
        let tasks = fixtures::tasks();
        let mut live = Live::new(tasks[..1].to_vec());
        let mut done = tasks[0].clone();
        done.state = TileState::Done;
        live.apply(Event::TaskUpdated {
            task: Box::new(done),
        });
        live.apply(Event::TaskUpdated {
            task: Box::new(tasks[1].clone()),
        });
        assert_eq!(live.tasks().len(), 2);
        assert_eq!(live.tasks()[0].state, TileState::Done);
        live.apply(Event::TaskRemoved { task: tasks[0].id });
        assert_eq!(live.tasks().len(), 1);
    }

    #[test]
    fn aw10_opening_a_tile_fetches_its_detail() {
        let (live, detail) = opened();
        assert_eq!(live.open_task(), Some(detail.view.id));
        let view = live.expanded(now()).expect("open");
        assert_eq!(view.detail, Some(&detail));
        assert_eq!(view.tab, Tab::Activity);
    }

    #[test]
    fn aw10_a_detail_for_another_task_is_ignored() {
        let (mut live, detail) = with_invoices();
        live.update(Message::OpenTask(detail.view.id));
        let mut other = detail.clone();
        other.view.id = TaskId::new();
        live.detail(other);
        assert_eq!(live.expanded(now()).and_then(|v| v.detail), None);
    }

    #[test]
    fn da12_review_opens_the_expanded_view_on_the_ask() {
        let (mut live, _) = with_invoices();
        let tile = live
            .tasks()
            .iter()
            .find(|t| t.needs.is_some())
            .cloned()
            .expect("a needs-you tile");
        let ask = tile.needs.as_ref().map(|a| a.id).expect("an ask");
        assert_eq!(
            live.update(Message::Review(ask)),
            Some(Effect::Get(tile.id))
        );
        assert_eq!(live.open_task(), Some(tile.id));
        // Its own answers: approve once, or decline.
        assert_eq!(
            live.update(Message::Decline(ask)),
            Some(Effect::Answer(AnswerApproval {
                approval: ask,
                answer: ApprovalAnswer::Deny,
            }))
        );
    }

    #[test]
    fn aw16_esc_collapses_the_expanded_view() {
        let (mut live, _) = opened();
        let esc = live.shortcut(&Key::Named(Named::Escape), Modifiers::empty());
        assert_eq!(esc, Some(Message::CloseTask));
        assert_eq!(live.update(Message::CloseTask), None);
        assert!(live.expanded(now()).is_none());
        // Nothing left to close.
        assert_eq!(
            live.shortcut(&Key::Named(Named::Escape), Modifiers::empty()),
            None
        );
    }

    #[test]
    fn aw16_esc_closes_the_command_bar_before_the_expanded_view() {
        let (mut live, _) = opened();
        live.update(Message::OpenCommandBar);
        assert_eq!(
            live.shortcut(&Key::Named(Named::Escape), Modifiers::empty()),
            Some(Message::CloseCommandBar)
        );
    }

    #[test]
    fn aw16_tiles_keep_updating_behind_the_expanded_view() {
        let (mut live, detail) = opened();
        let mut tile = live.tasks()[0].clone();
        tile.caption = "Matched receipt 6".into();
        // Another tile changing needs no refetch.
        assert_eq!(
            live.apply(Event::TaskUpdated {
                task: Box::new(tile)
            }),
            None
        );
        assert_eq!(live.tasks()[0].caption, "Matched receipt 6");
        assert_eq!(live.open_task(), Some(detail.view.id));
    }

    #[test]
    fn aw13_the_open_task_changing_refetches_its_detail() {
        let (mut live, detail) = opened();
        let mut tile = detail.view.clone();
        tile.progress = Some(glass_api::Progress {
            done: 13,
            total: 40,
        });
        assert_eq!(
            live.apply(Event::TaskUpdated {
                task: Box::new(tile)
            }),
            Some(Effect::Get(detail.view.id))
        );
        // The header shows the event's fields at once.
        let view = live.expanded(now()).expect("open");
        assert_eq!(expanded::chip(view.task), "Working · 13 of 40");
    }

    #[test]
    fn aw16_a_dismissed_task_closes_its_expanded_view() {
        let (mut live, detail) = opened();
        live.apply(Event::TaskRemoved {
            task: detail.view.id,
        });
        assert!(live.open_task().is_none());
    }

    #[test]
    fn aw13_tabs_switch_between_activity_and_changes() {
        let (mut live, _) = opened();
        live.update(Message::ShowTab(Tab::Changes));
        assert_eq!(live.expanded(now()).map(|v| v.tab), Some(Tab::Changes));
    }

    #[test]
    fn aw11_header_controls_call_their_methods() {
        let (mut live, detail) = opened();
        let id = detail.view.id;
        let cases = [
            (Message::Pause(id), methods::TASK_PAUSE),
            (Message::Resume(id), methods::TASK_RESUME),
            (Message::Stop(id), methods::TASK_STOP),
            (Message::TakeOver(id), methods::TASK_TAKE_OVER),
            (Message::HandBack(id), methods::TASK_HAND_BACK),
        ];
        for (message, method) in cases {
            let effect = live.update(message).expect("an effect");
            assert_eq!(effect.task_call(), Some((method, TaskRef { task: id })));
        }
        assert_eq!(Effect::Get(id).task_call(), None);
    }

    #[test]
    fn aw15_roll_back_asks_with_dry_run_first_then_rolls_back() {
        let (mut live, detail) = opened();
        let id = detail.view.id;
        let earlier = detail.snapshots[1].id;
        live.update(Message::SelectSnapshot(earlier));
        assert_eq!(
            live.update(Message::RollBack),
            Some(Effect::Undo(UndoRequest {
                task: id,
                to: Some(earlier),
                changes: Vec::new(),
                dry_run: true,
            }))
        );
        // A second press while it checks does nothing.
        assert_eq!(live.update(Message::RollBack), None);
        let preview = UndoReport {
            dry_run: true,
            restored: vec!["~/Invoices/MAR-26/INV-0411.json".into()],
            removed: Vec::new(),
            not_undone: Vec::new(),
        };
        assert_eq!(live.undo_report(preview.clone()), None);
        let view = live.expanded(now()).expect("open");
        assert_eq!(
            view.rollback,
            Some(&Rollback::Preview {
                to: earlier,
                report: preview
            })
        );
        assert_eq!(
            live.update(Message::ConfirmRollBack),
            Some(Effect::Undo(UndoRequest {
                task: id,
                to: Some(earlier),
                changes: Vec::new(),
                dry_run: false,
            }))
        );
        let done = UndoReport {
            dry_run: false,
            ..UndoReport::default()
        };
        // After the real roll back, the detail is fetched again.
        assert_eq!(live.undo_report(done), Some(Effect::Get(id)));
        assert!(matches!(
            live.expanded(now()).and_then(|v| v.rollback),
            Some(Rollback::Done { .. })
        ));
        live.update(Message::CancelRollBack);
        assert!(live.expanded(now()).and_then(|v| v.rollback).is_none());
    }

    #[test]
    fn aw15_roll_back_goes_to_the_latest_snapshot_unless_one_is_picked() {
        let (mut live, detail) = opened();
        let Some(Effect::Undo(request)) = live.update(Message::RollBack) else {
            panic!("a dry run");
        };
        assert_eq!(request.to, detail.snapshots.last().map(|s| s.id));
    }

    #[test]
    fn aw15_esc_cancels_a_roll_back_before_closing_the_view() {
        let (mut live, _) = opened();
        live.update(Message::RollBack);
        live.undo_report(UndoReport {
            dry_run: true,
            ..UndoReport::default()
        });
        assert_eq!(
            live.shortcut(&Key::Named(Named::Escape), Modifiers::empty()),
            Some(Message::CancelRollBack)
        );
        live.update(Message::CancelRollBack);
        assert_eq!(
            live.shortcut(&Key::Named(Named::Escape), Modifiers::empty()),
            Some(Message::CloseTask)
        );
    }

    #[test]
    fn aw15_a_refused_roll_back_says_why() {
        let (mut live, _) = opened();
        live.update(Message::RollBack);
        live.undo_failed("no task 1a2b3c4d".into());
        assert!(matches!(
            live.expanded(now()).and_then(|v| v.rollback),
            Some(Rollback::Failed { message, .. }) if message == "no task 1a2b3c4d"
        ));
    }

    #[test]
    fn aw15_without_snapshots_there_is_nothing_to_roll_back_to() {
        let (mut live, mut detail) = with_invoices();
        detail.snapshots.clear();
        live.update(Message::OpenTask(detail.view.id));
        live.detail(detail);
        assert_eq!(live.update(Message::RollBack), None);
    }

    fn working(tasks: &[TaskView]) -> TaskView {
        tasks
            .iter()
            .find(|t| t.state == TileState::Working)
            .cloned()
            .expect("a working tile")
    }

    #[test]
    fn ux_a_new_agent_shows_as_a_banner_that_goes_after_a_few_seconds() {
        let tasks = fixtures::tasks();
        let mut live = Live::new(Vec::new());
        let at = Instant::now();
        let mut task = working(&tasks);
        live.apply_at(
            Event::TaskUpdated {
                task: Box::new(task.clone()),
            },
            at,
        );
        assert_eq!(live.banners(), [task.id]);
        assert_eq!(live.next_change(), Some(at + BANNER));
        // Progress isn't news: no new banner, no longer stay.
        task.caption = "Matched 12 of 40".into();
        live.apply_at(
            Event::TaskUpdated {
                task: Box::new(task.clone()),
            },
            at + Duration::from_secs(2),
        );
        assert_eq!(live.next_change(), Some(at + BANNER));
        assert!(!live.tick(at + BANNER - Duration::from_millis(1)));
        assert!(live.tick(at + BANNER), "time's up");
        assert!(live.banners().is_empty());
        assert_eq!(live.next_change(), None);
        // Its end is news again.
        task.state = TileState::Done;
        live.apply_at(
            Event::TaskUpdated {
                task: Box::new(task.clone()),
            },
            at + Duration::from_secs(9),
        );
        assert_eq!(live.banners(), [task.id]);
    }

    #[test]
    fn reloading_after_glassd_comes_back_keeps_what_is_still_there() {
        let tasks = fixtures::tasks();
        let (kept, gone) = (tasks[0].clone(), tasks[1].clone());
        let mut live = Live::new(tasks);
        live.update(Message::OpenTask(gone.id));
        assert_eq!(live.reload(vec![kept.clone()]), None);
        assert_eq!(live.open_task(), None, "its tile is gone");
        assert!(live.banners().iter().all(|&b| b == kept.id));
        live.update(Message::OpenTask(kept.id));
        assert_eq!(live.reload(vec![kept.clone()]), Some(Effect::Get(kept.id)));
        assert_eq!(live.tasks().len(), 1);
    }

    #[test]
    fn ux_a_banner_stays_while_the_pointer_is_on_it() {
        let tasks = fixtures::tasks();
        let mut live = Live::new(Vec::new());
        let at = Instant::now();
        let task = working(&tasks);
        live.apply_at(
            Event::TaskUpdated {
                task: Box::new(task.clone()),
            },
            at,
        );
        live.update_at(Message::HoldBanners, at + Duration::from_secs(1));
        assert_eq!(live.next_change(), None);
        assert!(!live.tick(at + BANNER * 3), "read as long as it takes");
        assert_eq!(live.banners(), [task.id]);
        // Let go, it stays a moment longer, then goes.
        let left = at + BANNER * 3;
        live.update_at(Message::LetBannersGo, left);
        assert_eq!(live.next_change(), Some(left + AFTER_HOLD));
        assert!(live.tick(left + AFTER_HOLD));
        assert!(live.banners().is_empty());
    }

    #[test]
    fn ux_banners_the_menu_covers_stop_being_held() {
        let tasks = fixtures::tasks();
        let mut live = Live::new(Vec::new());
        let at = Instant::now();
        live.apply_at(
            Event::TaskUpdated {
                task: Box::new(working(&tasks)),
            },
            at,
        );
        live.update_at(Message::HoldBanners, at);
        live.update_at(Message::ToggleAgents, at);
        live.update_at(Message::ToggleAgents, at);
        // A new banner isn't stuck behind a pointer that left long ago.
        let later = at + Duration::from_secs(30);
        let mut done = working(&tasks);
        done.state = TileState::Done;
        live.apply_at(
            Event::TaskUpdated {
                task: Box::new(done),
            },
            later,
        );
        assert_eq!(live.next_change(), Some(later + BANNER));
    }

    #[test]
    fn force_quit_stops_the_chosen_agent_at_once() {
        let tasks = fixtures::tasks();
        let running = tasks
            .iter()
            .find(|t| t.state.is_live())
            .expect("a running agent");
        let mut live = Live::new(tasks.clone());
        let esc = Key::Named(Named::Escape);
        for keys in [
            Modifiers::ALT | Modifiers::LOGO,
            Modifiers::CTRL | Modifiers::SHIFT,
        ] {
            assert_eq!(live.shortcut(&esc, keys), Some(Message::OpenForceQuit));
        }
        live.update(Message::OpenForceQuit);
        assert_eq!(live.update(Message::ForceQuit), None, "nothing chosen yet");
        live.update(Message::ChooseToQuit(running.id));
        assert_eq!(
            live.update(Message::ForceQuit),
            Some(Effect::Stop(running.id))
        );
        assert_eq!(live.shortcut(&esc, Modifiers::empty()), None, "it closed");
    }

    #[test]
    fn restart_and_shut_down_ask_first_then_leave_a_last_frame() {
        let mut live = Live::new(fixtures::tasks());
        for power in [Power::Restart, Power::ShutDown] {
            live.update(Message::AskPower(power));
            let esc = Key::Named(Named::Escape);
            assert_eq!(
                live.shortcut(&esc, Modifiers::empty()),
                Some(Message::CloseSystemWindow),
                "Esc says no"
            );
            live.update(Message::CloseSystemWindow);
            assert_eq!(live.update(Message::ConfirmPower), None, "nothing asked");
        }
        live.update(Message::AskPower(Power::Restart));
        assert_eq!(
            live.update(Message::ConfirmPower),
            Some(Effect::Power(Power::Restart))
        );
        assert_eq!(live.leaving, Some(Power::Restart));
    }

    #[test]
    fn about_and_force_quit_close_with_esc_and_hide_the_banners() {
        let mut live = Live::new(fixtures::tasks());
        assert!(!live.banners().is_empty());
        live.update(Message::OpenAbout);
        let esc = Key::Named(Named::Escape);
        assert_eq!(
            live.shortcut(&esc, Modifiers::empty()),
            Some(Message::CloseSystemWindow)
        );
        assert_eq!(live.next_change(), live.next_change());
        live.update(Message::CloseSystemWindow);
        assert_eq!(live.shortcut(&esc, Modifiers::empty()), None);
    }

    #[test]
    fn the_system_menu_closes_with_esc_and_gives_way_to_the_agents_menu() {
        let mut live = Live::new(fixtures::tasks());
        live.update(Message::ToggleSystemMenu);
        let esc = Key::Named(Named::Escape);
        assert_eq!(
            live.shortcut(&esc, Modifiers::empty()),
            Some(Message::ToggleSystemMenu)
        );
        live.update(Message::ToggleAgents);
        assert_eq!(
            live.shortcut(&esc, Modifiers::empty()),
            Some(Message::CloseAgents),
            "one menu at a time"
        );
    }

    #[test]
    fn ux_an_ask_stays_a_little_longer_and_approving_it_takes_it_away() {
        let tasks = fixtures::tasks();
        let asking = tasks
            .iter()
            .find(|t| t.state == TileState::NeedsYou)
            .cloned()
            .expect("a needs-you tile");
        let at = Instant::now();
        let mut live = Live::new(Vec::new());
        live.apply_at(
            Event::TaskUpdated {
                task: Box::new(asking.clone()),
            },
            at,
        );
        assert_eq!(live.next_change(), Some(at + NEEDS_YOU_BANNER));
        let ask = asking.needs.as_ref().expect("an ask").id;
        live.update(Message::Approve(ask));
        assert!(live.banners().is_empty());
    }

    #[test]
    fn ux_whats_waiting_at_sign_in_shows_and_the_rest_waits_in_the_menu() {
        let tasks = fixtures::tasks();
        let live = Live::new(tasks.clone());
        let waiting: Vec<TaskId> = tasks
            .iter()
            .filter(|t| t.state == TileState::NeedsYou)
            .map(|t| t.id)
            .collect();
        assert_eq!(live.banners(), waiting);
    }

    #[test]
    fn ux_the_agents_pill_opens_a_menu_of_every_agent_and_esc_closes_it() {
        let tasks = fixtures::tasks();
        let mut live = Live::new(tasks.clone());
        live.update(Message::ToggleAgents);
        assert!(live.banners().is_empty(), "the menu has them all");
        let esc = live.shortcut(&Key::Named(Named::Escape), Modifiers::empty());
        assert_eq!(esc, Some(Message::CloseAgents));
        live.update(Message::ToggleAgents);
        // A tile in the menu opens its expanded view and closes the menu.
        let id = working(&tasks).id;
        assert_eq!(live.update(Message::OpenTask(id)), Some(Effect::Get(id)));
        assert_eq!(
            live.shortcut(&Key::Named(Named::Escape), Modifiers::empty()),
            Some(Message::CloseTask),
            "Esc now closes the expanded view, the menu is gone"
        );
    }

    #[test]
    fn shift_super_q_signs_out() {
        let mut live = Live::default();
        let q = Key::Character("Q".into());
        let message = live.shortcut(&q, Modifiers::SHIFT | Modifiers::LOGO);
        assert_eq!(message, Some(Message::SignOut));
        assert_eq!(live.update(Message::SignOut), Some(Effect::SignOut));
        assert_eq!(
            live.shortcut(&q, Modifiers::SHIFT),
            None,
            "Shift Q is typing"
        );
    }
}
