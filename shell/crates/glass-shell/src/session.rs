//! How a desktop session ends, as the sign-in screen (ADR 0019) learns it:
//! the desktop's exit status. Restarting and shutting down need root, which
//! the desktop doesn't have and the sign-in screen does, so the desktop asks
//! by ending with one of these statuses and the sign-in screen does it.

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

#[cfg(test)]
mod tests {
    use super::*;

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
