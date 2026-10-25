//! The OS abstraction layer. Everything that touches FreeBSD-specific
//! facilities lives here, behind small interfaces the services use:
//!
//! - [`sandbox`]: where agents run. [`sandbox::JailSandbox`] is the real
//!   boundary (one jail per task, PA-1); [`sandbox::HostSandbox`] is a
//!   development stand-in that contains nothing.
//! - [`snapshot`]: point-in-time copies for rollback.
//!   [`snapshot::ZfsSnapshotter`] in production, [`snapshot::CopySnapshotter`]
//!   for development hosts without ZFS.
//! - [`fs::ConfinedDir`]: file access that cannot escape a granted root, used
//!   by brokers acting on an agent's behalf.
//! - [`users`] and [`xattr`]: account lookup and data labels stored with files.
//! - [`system`]: network, storage, load and services, for the IT self-service
//!   agent (AE-4).
//! - [`net`]: who owns the other end of a loopback connection, for the app
//!   bridge (ADR 0027).
//! - [`framebuffer`] and [`input`]: the console framebuffer and the evdev
//!   keyboard and pointer, for the shell where there is no compositor or GPU
//!   driver (ADR 0016).
//! - `auth` (feature `pam`): checking a password with PAM, for the sign-in
//!   screen (ADR 0019).
//!
//! All `unsafe` code in the workspace is confined to the FFI in `users`,
//! `xattr`, `framebuffer`, `input` and `auth`, each block with a SAFETY
//! note.

#[cfg(feature = "pam")]
#[allow(unsafe_code)]
pub mod auth;
pub mod bootenv;
mod cmd;
#[allow(unsafe_code)]
pub mod framebuffer;
pub mod fs;
pub mod hardware;
#[allow(unsafe_code)]
pub mod input;
#[cfg(target_os = "freebsd")]
mod ioctl;
pub mod net;
pub mod sandbox;
pub mod snapshot;
pub mod system;
#[allow(unsafe_code)]
pub mod users;
pub mod wifi;
#[allow(unsafe_code)]
pub mod xattr;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` failed ({status}): {stderr}")]
    Command {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("refusing to continue: {0}")]
    Unsafe(String),
    #[error("{0} is not supported on this system")]
    Unsupported(String),
    #[error("{path} is not inside any snapshot-capable dataset")]
    NotSnapshottable { path: PathBuf },
}

impl PlatformError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

pub type Result<T, E = PlatformError> = std::result::Result<T, E>;
