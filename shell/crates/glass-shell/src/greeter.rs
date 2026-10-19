//! The sign-in screen's state (ADR 0019): who is signing in, what they've
//! typed, and what the last try said. The greeter program feeds it input,
//! checks the password with PAM when asked, and starts the session.

use time::OffsetDateTime;
use time::macros::format_description;

use crate::Message;
use crate::screens::login::{self, SignIn};

/// Someone who can sign in on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The login name.
    pub name: String,
    /// The full name, if the account has one.
    pub full_name: String,
}

impl Account {
    /// The name the screen shows.
    pub fn shown(&self) -> &str {
        if self.full_name.trim().is_empty() {
            &self.name
        } else {
            &self.full_name
        }
    }

    /// "Dana Ruiz" → "DR".
    pub fn initials(&self) -> String {
        initials(self.shown())
    }
}

/// "Dana Ruiz" → "DR": the first letters of the first two words.
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// What a message asks the greeter to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Give the keyboard focus to the widget with this id.
    Focus(&'static str),
    /// Check this password for this account, and if it's right, start
    /// their session. The caller wipes the password when done.
    SignIn { user: String, password: String },
}

#[derive(Debug)]
pub struct Greeter {
    accounts: Vec<Account>,
    current: usize,
    password: String,
    message: Option<String>,
    /// A password is being checked or a session is running.
    busy: bool,
}

impl Greeter {
    pub fn new(accounts: Vec<Account>) -> Self {
        let message = accounts
            .is_empty()
            .then(|| "There's no one to sign in on this computer yet".to_owned());
        Self {
            accounts,
            current: 0,
            password: String::new(),
            message,
            busy: false,
        }
    }

    /// Who is signing in.
    pub fn account(&self) -> Option<&Account> {
        self.accounts.get(self.current)
    }

    pub fn update(&mut self, message: Message) -> Option<Effect> {
        if self.busy {
            return None;
        }
        match message {
            Message::PasswordChanged(text) => {
                self.password = text;
                self.message = None;
                None
            }
            Message::SignIn if !self.password.is_empty() => {
                let user = self.account()?.name.clone();
                self.busy = true;
                Some(Effect::SignIn {
                    user,
                    password: std::mem::take(&mut self.password),
                })
            }
            Message::SwitchUser if self.accounts.len() > 1 => {
                self.current = (self.current + 1) % self.accounts.len();
                self.password.clear();
                self.message = None;
                Some(Effect::Focus(login::PASSWORD))
            }
            _ => None,
        }
    }

    /// The password didn't sign in: say why and take another.
    pub fn refused(&mut self, why: impl Into<String>) {
        self.busy = false;
        self.message = Some(why.into());
    }

    /// The session ended: back to an empty password field.
    pub fn signed_out(&mut self) {
        self.busy = false;
        self.password.clear();
        self.message = None;
    }

    /// The typed password, for the caller to wipe when it's done with the
    /// screen.
    pub fn password_mut(&mut self) -> &mut String {
        &mut self.password
    }

    /// The screen at `now`, local time.
    pub fn screen(&self, now: OffsetDateTime) -> SignIn {
        let account = self.account();
        let (date, time) = clock_lines(now);
        SignIn {
            date,
            time,
            user: account.map(|a| a.shown().to_owned()).unwrap_or_default(),
            initials: account.map(Account::initials).unwrap_or_default(),
            managed_by: "Signs in with this computer's accounts".into(),
            battery_percent: None,
            password: self.password.clone(),
            message: self.message.clone(),
            // No fingerprint sign-in yet.
            fingerprint: false,
            switch_user: true,
            wallpaper: false,
        }
    }
}

/// The sign-in screen's (and the lock screen's) date and big clock:
/// "Monday, October 19" and "8:02".
pub fn clock_lines(now: OffsetDateTime) -> (String, String) {
    let format =
        |f: &[time::format_description::BorrowedFormatItem<'_>]| now.format(f).unwrap_or_default();
    (
        format(format_description!(
            "[weekday repr:long], [month repr:long] [day padding:none]"
        )),
        format(format_description!("[hour repr:12 padding:none]:[minute]")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accounts() -> Vec<Account> {
        vec![
            Account {
                name: "dana".into(),
                full_name: "Dana Ruiz".into(),
            },
            Account {
                name: "demo".into(),
                full_name: String::new(),
            },
        ]
    }

    #[test]
    fn accounts_show_their_full_name_and_initials() {
        let [dana, demo] = <[Account; 2]>::try_from(accounts()).expect("two");
        assert_eq!(
            (dana.shown(), dana.initials().as_str()),
            ("Dana Ruiz", "DR")
        );
        assert_eq!((demo.shown(), demo.initials().as_str()), ("demo", "D"));
    }

    #[test]
    fn signing_in_hands_over_the_password_once_and_clears_the_field() {
        let mut greeter = Greeter::new(accounts());
        assert_eq!(greeter.update(Message::SignIn), None, "nothing typed");
        greeter.update(Message::PasswordChanged("hunter2".into()));
        assert_eq!(
            greeter.update(Message::SignIn),
            Some(Effect::SignIn {
                user: "dana".into(),
                password: "hunter2".into()
            })
        );
        assert!(
            greeter
                .screen(OffsetDateTime::UNIX_EPOCH)
                .password
                .is_empty()
        );
        // While it's being checked, nothing else gets in.
        assert_eq!(greeter.update(Message::PasswordChanged("x".into())), None);
        assert_eq!(greeter.update(Message::SignIn), None);
    }

    #[test]
    fn a_refusal_shows_why_until_the_next_keystroke() {
        let mut greeter = Greeter::new(accounts());
        greeter.update(Message::PasswordChanged("wrong".into()));
        greeter.update(Message::SignIn);
        greeter.refused("That password didn't work");
        let at = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            greeter.screen(at).message.as_deref(),
            Some("That password didn't work")
        );
        greeter.update(Message::PasswordChanged("r".into()));
        assert_eq!(greeter.screen(at).message, None);
    }

    #[test]
    fn switch_user_moves_to_the_next_account_with_an_empty_field() {
        let mut greeter = Greeter::new(accounts());
        greeter.update(Message::PasswordChanged("half".into()));
        assert_eq!(
            greeter.update(Message::SwitchUser),
            Some(Effect::Focus(login::PASSWORD))
        );
        let screen = greeter.screen(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(
            (screen.user.as_str(), screen.password.as_str()),
            ("demo", "")
        );
    }

    #[test]
    fn the_screen_shows_the_local_date_and_time() {
        let at = time::macros::datetime!(2026-10-19 14:14 UTC);
        let screen = Greeter::new(accounts()).screen(at);
        assert_eq!(
            (screen.date.as_str(), screen.time.as_str()),
            ("Monday, October 19", "2:14")
        );
    }
}
