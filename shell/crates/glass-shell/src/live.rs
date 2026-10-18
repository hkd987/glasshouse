//! The live desktop: the user's tiles as glassd reports them, what the
//! person has open, and what their clicks and keys ask of glassd. A shell
//! feeds it glassd's events and the person's input, draws its view, and
//! makes the calls it returns.

use glass_api::{AnswerApproval, ApprovalAnswer, ApprovalScope, Event, TaskView};
use iced_core::keyboard::{Key, Modifiers, key::Named};
use time::OffsetDateTime;

use crate::screens::command_bar::{self, CommandBar};
use crate::screens::desktop::{self, Desktop};
use crate::{Element, Message, Theme};

#[derive(Debug)]
pub struct Live {
    /// From `task.list`, then kept current from `events.subscribe`.
    tasks: Vec<TaskView>,
    /// The command bar's text while it's open.
    command: Option<String>,
    /// Whether the view draws the wallpaper, or the screen keeps it as its
    /// backdrop.
    wallpaper: bool,
}

/// What a message asks the shell to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Answer an ask (`approval.answer`).
    Answer(AnswerApproval),
    /// Give the keyboard focus to the widget with this id.
    Focus(&'static str),
}

impl Default for Live {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Live {
    pub fn new(tasks: Vec<TaskView>) -> Self {
        Self {
            tasks,
            command: None,
            wallpaper: true,
        }
    }

    /// Leaves the wallpaper out of the view, for a screen that keeps it as
    /// its backdrop (`render::Screen::set_backdrop`).
    pub fn without_wallpaper(mut self) -> Self {
        self.wallpaper = false;
        self
    }

    pub fn tasks(&self) -> &[TaskView] {
        &self.tasks
    }

    /// Folds in one of glassd's events.
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::TaskUpdated { task } => match self.tasks.iter_mut().find(|t| t.id == task.id) {
                Some(tile) => *tile = *task,
                None => self.tasks.push(*task),
            },
            Event::TaskRemoved { task } => self.tasks.retain(|t| t.id != task),
        }
    }

    /// What `message` asks for. Messages for parts not built yet (the
    /// expanded view, the desktop agent's answers) do nothing.
    pub fn update(&mut self, message: Message) -> Option<Effect> {
        match message {
            // DA-12: the needs-you tile's primary button approves once.
            Message::Approve(approval) => Some(Effect::Answer(AnswerApproval {
                approval,
                answer: ApprovalAnswer::Approve {
                    scope: ApprovalScope::Once,
                },
            })),
            Message::OpenCommandBar => {
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
            _ => None,
        }
    }

    /// The desktop's own keys: Ctrl Space opens and closes the command bar
    /// (DA-1), Esc closes it.
    pub fn shortcut(&self, key: &Key, modifiers: Modifiers) -> Option<Message> {
        let open = self.command.is_some();
        match key {
            Key::Named(Named::Space) if modifiers.control() => Some(if open {
                Message::CloseCommandBar
            } else {
                Message::OpenCommandBar
            }),
            Key::Named(Named::Escape) if open => Some(Message::CloseCommandBar),
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

    /// The desktop at `now` (local time), with `bar` from
    /// [`Live::command_bar`] over it.
    pub fn view<'a>(
        &'a self,
        theme: &Theme,
        bar: Option<&'a CommandBar<'a>>,
        now: OffsetDateTime,
        size: (f32, f32),
    ) -> Element<'a, Message> {
        let state = Desktop {
            // Tiles collapse while the command bar is open.
            tiles_visible: bar.is_none(),
            wallpaper: self.wallpaper,
            ..Desktop::live(&self.tasks, now)
        };
        let overlay = bar.map(|bar| command_bar::overlay(theme, bar));
        desktop::view(theme, &state, Vec::new(), |_| None, overlay, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use glass_api::TileState;

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
}
