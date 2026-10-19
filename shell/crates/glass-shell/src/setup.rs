//! First-run setup's state (UX-2): what the person has answered so far.
//! A computer nobody has signed in to yet, and that no organization
//! manages, asks three questions (who you are, a password, how agents
//! reach AI models) and then signs its new person in. An org-enrolled
//! computer asks none: its people come from the directory.
//!
//! The greeter program shows it, creates the account when asked
//! ([`Effect::Create`]) and starts the session.

use crate::Message;
use crate::screens::setup::{self, ModelChoice, Step};

/// The shortest password setup takes.
pub const MIN_PASSWORD: usize = 8;

/// What a message asks the greeter to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Give the keyboard focus to the widget with this id.
    Focus(&'static str),
    /// Create this account, record the choice, then sign them in. The
    /// caller wipes the password when done.
    Create {
        login: String,
        full_name: String,
        password: String,
        models: Option<ModelChoice>,
    },
}

#[derive(Debug)]
pub struct Setup {
    step: Step,
    full_name: String,
    password: String,
    confirm: String,
    models: Option<ModelChoice>,
    message: Option<String>,
    /// Names the new account can't take: every account on the computer.
    taken: Vec<String>,
    /// The account is being created.
    busy: bool,
}

impl Setup {
    pub fn new(taken: Vec<String>) -> Self {
        Self {
            step: Step::Account,
            full_name: String::new(),
            password: String::new(),
            confirm: String::new(),
            models: None,
            message: None,
            taken,
            busy: false,
        }
    }

    /// The account name the full name gives: its first word, in lowercase
    /// letters and digits, made unique among the computer's accounts.
    pub fn login(&self) -> String {
        login_for(&self.full_name, &self.taken)
    }

    pub fn update(&mut self, message: Message) -> Option<Effect> {
        if self.busy {
            return None;
        }
        match message {
            Message::SetupName(text) => {
                self.full_name = text;
                self.message = None;
                None
            }
            Message::SetupPassword(text) => {
                self.password = text;
                self.message = None;
                None
            }
            Message::SetupConfirm(text) => {
                self.confirm = text;
                self.message = None;
                None
            }
            Message::SetupFocus(id) => Some(Effect::Focus(id)),
            Message::SetupModels(choice) => {
                self.models = Some(choice);
                None
            }
            Message::SetupBack => {
                self.message = None;
                self.step = match self.step {
                    Step::Account | Step::Models => Step::Account,
                    Step::Ready => Step::Models,
                };
                (self.step == Step::Account).then_some(Effect::Focus(setup::NAME))
            }
            Message::SetupNext => match self.step {
                Step::Account => {
                    self.message = self.account_problem();
                    if self.message.is_none() {
                        self.step = Step::Models;
                    }
                    None
                }
                Step::Models => {
                    self.step = Step::Ready;
                    None
                }
                Step::Ready => {
                    self.busy = true;
                    Some(Effect::Create {
                        login: self.login(),
                        full_name: self.full_name.trim().to_owned(),
                        password: std::mem::take(&mut self.password),
                        models: self.models,
                    })
                }
            },
            _ => None,
        }
    }

    /// What's wrong with the account step's answers, if anything.
    fn account_problem(&self) -> Option<String> {
        let name = self.full_name.trim();
        if name.is_empty() {
            return Some("Enter your name".into());
        }
        // The account's full name goes into passwd(5), where `:` and `,`
        // separate fields; the system checks again as it creates it.
        if name.chars().count() > 64 || name.chars().any(|c| c.is_control() || c == ':' || c == ',')
        {
            return Some("Use letters and spaces for your name, up to 64".into());
        }
        if self.password.chars().count() < MIN_PASSWORD {
            return Some(format!(
                "Choose a password of at least {MIN_PASSWORD} characters"
            ));
        }
        if self.password != self.confirm {
            return Some("The passwords don't match".into());
        }
        None
    }

    /// Creating the account didn't work: say why, back on the account step.
    pub fn failed(&mut self, why: impl Into<String>) {
        self.busy = false;
        self.step = Step::Account;
        self.confirm.clear();
        self.message = Some(why.into());
    }

    /// The typed passwords, for the caller to wipe when it's done.
    pub fn wipe(&mut self) {
        crate::session::wipe(&mut self.password);
        crate::session::wipe(&mut self.confirm);
    }

    /// What the screen shows now.
    pub fn screen(&self) -> setup::SetupScreen {
        setup::SetupScreen {
            step: self.step,
            full_name: self.full_name.clone(),
            login: self.login(),
            password: self.password.clone(),
            confirm: self.confirm.clone(),
            models: self.models,
            message: self.message.clone(),
            busy: self.busy,
        }
    }
}

/// "Dana Ruiz" → "dana", "dana2" when there's already a dana.
pub fn login_for(full_name: &str, taken: &[String]) -> String {
    let first: String = full_name
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .skip_while(char::is_ascii_digit)
        .take(14)
        .collect();
    let base = if first.is_empty() {
        "user".to_owned()
    } else {
        first
    };
    std::iter::once(base.clone())
        .chain((2..).map(|n| format!("{base}{n}")))
        .find(|name| !taken.iter().any(|t| t == name))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taken() -> Vec<String> {
        ["root", "toor", "daemon", "user", "glassd"]
            .map(str::to_owned)
            .to_vec()
    }

    #[test]
    fn ux2_account_names_come_from_the_first_name_and_never_clash() {
        assert_eq!(login_for("Dana Ruiz", &taken()), "dana");
        assert_eq!(login_for("  José Núñez", &taken()), "jos");
        assert_eq!(login_for("Root Beer", &taken()), "root2");
        assert_eq!(login_for("42", &taken()), "user2");
        assert_eq!(login_for("", &taken()), "user2");
        let long = login_for("Alexandriaocasioberenice Smith", &taken());
        assert_eq!(long, "alexandriaocas", "14 letters, room for a number");
    }

    #[test]
    fn ux2_three_answers_then_the_account_is_created_once() {
        let mut setup = Setup::new(taken());
        assert_eq!(setup.update(Message::SetupNext), None);
        assert_eq!(setup.screen().message.as_deref(), Some("Enter your name"));
        setup.update(Message::SetupName("Dana Ruiz".into()));
        setup.update(Message::SetupPassword("short".into()));
        setup.update(Message::SetupNext);
        assert!(setup.screen().message.unwrap().contains("at least 8"));
        setup.update(Message::SetupPassword("correct horse".into()));
        setup.update(Message::SetupConfirm("correct hose".into()));
        setup.update(Message::SetupNext);
        assert_eq!(
            setup.screen().message.as_deref(),
            Some("The passwords don't match")
        );
        setup.update(Message::SetupConfirm("correct horse".into()));
        setup.update(Message::SetupNext);
        assert_eq!(setup.screen().step, Step::Models);
        setup.update(Message::SetupModels(ModelChoice::ClaudeCode));
        setup.update(Message::SetupNext);
        assert_eq!(setup.screen().step, Step::Ready);
        assert_eq!(
            setup.update(Message::SetupNext),
            Some(Effect::Create {
                login: "dana".into(),
                full_name: "Dana Ruiz".into(),
                password: "correct horse".into(),
                models: Some(ModelChoice::ClaudeCode),
            })
        );
        assert_eq!(setup.update(Message::SetupNext), None, "once");
        setup.failed("That didn't work");
        assert_eq!(setup.screen().step, Step::Account);
    }
}
