//! What the desktop asks of the sign-in screen (ADR 0019), which is root
//! and started its session; the desktop isn't root.
//!
//! - Restarting and shutting down end the session with an exit status of
//!   their own ([`exit_code`]), and the sign-in screen does it.
//! - Unlocking the locked screen needs the person's password checked with
//!   PAM: the desktop writes `unlock`, a tab and the password as one line
//!   to its standard output, and the sign-in screen answers on the
//!   desktop's standard input with `unlocked` or `refused`. It checks only
//!   the session's own account, and waits after a wrong password.

/// Restart or shut down the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
}

/// Exit statuses no other ending uses (errors exit 1, panics 101).
const RESTART: i32 = 75;
const SHUT_DOWN: i32 = 76;

/// The status the desktop ends with to ask for `power`.
pub fn exit_code(power: Power) -> i32 {
    match power {
        Power::Restart => RESTART,
        Power::ShutDown => SHUT_DOWN,
    }
}

/// What a session that ended with `code` asked for, if anything.
pub fn power_after(code: Option<i32>) -> Option<Power> {
    match code? {
        RESTART => Some(Power::Restart),
        SHUT_DOWN => Some(Power::ShutDown),
        _ => None,
    }
}

/// A password on its way to be checked: never printed, wiped when dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub fn new(text: String) -> Self {
        Self(text)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(…)")
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Overwrites a string's bytes and empties it, as well as safe code can
/// (the sign-in screen's own copies go through `glass_platform::auth`).
pub fn wipe(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

/// The line the desktop writes to ask for the screen to be unlocked.
pub fn unlock_request(password: &Password) -> String {
    format!("unlock\t{}\n", password.as_str())
}

/// The password in an unlock request line, if it is one.
pub fn parse_unlock(line: &str) -> Option<Password> {
    line.strip_prefix("unlock\t")
        .map(|p| Password::new(p.trim_end_matches(['\r', '\n']).to_owned()))
}

/// The answers to an unlock request.
pub const UNLOCKED: &str = "unlocked";
pub const REFUSED: &str = "refused";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlock_request_carries_the_password_and_never_prints_it() {
        let password = Password::new("hunter2 with spaces".into());
        let line = unlock_request(&password);
        assert_eq!(parse_unlock(&line).as_ref(), Some(&password));
        assert_eq!(format!("{password:?}"), "Password(…)");
        assert_eq!(parse_unlock("signed-in demo"), None);
    }

    #[test]
    fn a_session_asks_for_power_only_with_its_own_statuses() {
        for power in [Power::Restart, Power::ShutDown] {
            assert_eq!(power_after(Some(exit_code(power))), Some(power));
        }
        for ordinary in [None, Some(0), Some(1), Some(101)] {
            assert_eq!(power_after(ordinary), None);
        }
    }
}
