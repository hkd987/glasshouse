//! The shell on the console framebuffer, before the compositor (ADR 0016):
//! the screen ([`display`]) and the keyboard and pointer ([`input`],
//! [`keys`], [`pointer`]) that the desktop (`glass-shell-fb`) and the
//! sign-in screen (`glass-greeter`, ADR 0019) share, and the desktop's calls
//! to glassd, made off its loop ([`calls`]).

pub mod calls;
pub mod display;
pub mod input;
pub mod keys;
pub mod pointer;
