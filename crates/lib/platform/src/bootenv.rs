//! ZFS boot environments: copies of the base system's root file system, of
//! which the computer starts one (EP-1, ADR 0057). Driven through `bectl`
//! (ADR 0007).
//!
//! A change to the base is tried in a boot environment of its own: made from
//! the running one, changed while mounted, then started once
//! ([`activate_once`]). If that start goes wrong, the next one is the old
//! environment again, by itself. Only a start that was confirmed makes the
//! new one permanent ([`activate`]).

use std::path::{Path, PathBuf};

use crate::{PlatformError, Result, cmd};

/// One boot environment, as `bectl list -H` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootEnv {
    pub name: String,
    /// The computer is running from it.
    pub running: bool,
    /// The computer starts from it from the next start on.
    pub on_restart: bool,
    /// The computer starts from it at the next start only (`bectl activate -t`).
    pub next_start_only: bool,
    /// Where it's mounted, if it is.
    pub mountpoint: Option<PathBuf>,
}

/// Every boot environment.
pub fn list() -> Result<Vec<BootEnv>> {
    supported()?;
    Ok(parse_list(&cmd::run("bectl", &["list", "-H"])?))
}

/// The boot environment the computer is running from.
pub fn running() -> Result<String> {
    list()?
        .into_iter()
        .find(|b| b.running)
        .map(|b| b.name)
        .ok_or_else(|| PlatformError::Unsupported("a boot environment running".into()))
}

/// A new boot environment, a copy of the running one.
pub fn create(name: &str) -> Result<()> {
    checked(name)?;
    cmd::run("bectl", &["create", name]).map(|_| ())
}

/// Mounts `name` at `at`, so its files can be changed.
pub fn mount(name: &str, at: &Path) -> Result<()> {
    checked(name)?;
    let at = at
        .to_str()
        .ok_or_else(|| PlatformError::Unsupported(format!("mounting at {}", at.display())))?;
    cmd::run("bectl", &["mount", name, at]).map(|_| ())
}

pub fn unmount(name: &str) -> Result<()> {
    checked(name)?;
    cmd::run("bectl", &["umount", name]).map(|_| ())
}

/// The next start, and only that one, is from `name`.
pub fn activate_once(name: &str) -> Result<()> {
    checked(name)?;
    cmd::run("bectl", &["activate", "-t", name]).map(|_| ())
}

/// Takes back a start-once that hasn't happened yet.
pub fn cancel_once() -> Result<()> {
    supported()?;
    cmd::run("bectl", &["activate", "-T"]).map(|_| ())
}

/// Every start from now on is from `name`.
pub fn activate(name: &str) -> Result<()> {
    checked(name)?;
    cmd::run("bectl", &["activate", name]).map(|_| ())
}

/// Removes `name`, with the snapshot it was made from.
pub fn destroy(name: &str) -> Result<()> {
    checked(name)?;
    cmd::run("bectl", &["destroy", "-o", name]).map(|_| ())
}

fn supported() -> Result<()> {
    if cfg!(target_os = "freebsd") {
        Ok(())
    } else {
        Err(PlatformError::Unsupported("boot environments".into()))
    }
}

fn checked(name: &str) -> Result<()> {
    supported()?;
    if valid_name(name) {
        Ok(())
    } else {
        Err(PlatformError::Unsupported(format!(
            "a boot environment named {name:?}"
        )))
    }
}

/// A name `bectl` takes as a name and nothing else: letters, digits, `.`,
/// `_` and `-`, not starting with `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `bectl list -H`: name, active (`N` now, `R` on restart, `T` next start
/// only, `-` none), mountpoint (`-` none), space, created.
fn parse_list(text: &str) -> Vec<BootEnv> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            let active = fields.next()?.trim();
            let mountpoint = fields.next()?.trim();
            if name.is_empty() {
                return None;
            }
            Some(BootEnv {
                name: name.to_owned(),
                running: active.contains('N'),
                on_restart: active.contains('R'),
                next_start_only: active.contains('T'),
                mountpoint: (mountpoint != "-").then(|| PathBuf::from(mountpoint)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As 15.1's `bectl list -H` printed it in the dev VM (2026-10-24): one
    /// running, one made, then set to start once, then mounted.
    #[test]
    fn ep1_reads_which_environment_runs_and_which_starts_next() {
        let made = "default\tNR\t/\t3.56G\t1970-01-01 00:00\n\
                    glass-probe-be\t-\t-\t8K\t2026-10-24 18:13\n";
        let envs = parse_list(made);
        assert_eq!(envs.len(), 2);
        assert!(envs[0].running && envs[0].on_restart && !envs[0].next_start_only);
        assert_eq!(envs[0].mountpoint.as_deref(), Some(Path::new("/")));
        assert!(!envs[1].running && !envs[1].on_restart && envs[1].mountpoint.is_none());

        let once = parse_list(
            "default\tNR\t/\t3.56G\t1970-01-01 00:00\n\
             glass-probe-be\tT\t/tmp/tmp.EwQe2EcHLt\t200K\t2026-10-24 18:13\n",
        );
        assert!(once[1].next_start_only && !once[1].on_restart);
        assert_eq!(
            once[1].mountpoint.as_deref(),
            Some(Path::new("/tmp/tmp.EwQe2EcHLt"))
        );
    }

    #[test]
    fn only_plain_names_reach_bectl() {
        for good in ["default", "glass-hw-lkpi-wlan-suspend-1", "15.1_p2"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            "-T",
            "a b",
            "a/b",
            "a;reboot",
            "zroot/ROOT/default",
            &"x".repeat(65),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
