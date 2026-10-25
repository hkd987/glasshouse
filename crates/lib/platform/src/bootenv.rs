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

/// A new boot environment, received whole from `image`: a ZFS stream of
/// another computer's environment, as an OS update brings it (ADR 0061).
pub fn import(name: &str, image: &Path) -> Result<()> {
    checked(name)?;
    cmd::run_with_file("bectl", &["import", name], image).map(|_| ())
}

/// A new boot environment received from `image`, a ZFS stream of the
/// changes since `base`'s image snapshot (`@glass-image`, ADR 0061): a
/// clone of that snapshot with the changes on top, set up as `bectl`
/// sets up its own.
pub fn import_onto(name: &str, image: &Path, base: &str, snapshot: &str) -> Result<()> {
    checked(name)?;
    checked(base)?;
    if !valid_name(snapshot) {
        return Err(PlatformError::Unsupported(format!(
            "a snapshot named {snapshot:?}"
        )));
    }
    let root = be_root()?;
    let origin = format!("origin={root}/{base}@{snapshot}");
    let target = format!("{root}/{name}");
    cmd::run_with_file(
        "zfs",
        &[
            "receive",
            "-u",
            "-o",
            &origin,
            "-o",
            "canmount=noauto",
            "-o",
            "mountpoint=/",
            &target,
        ],
        image,
    )
    .map(|_| ())
}

/// Where boot environments live: the dataset holding the one the
/// computer starts from (`zroot/ROOT`).
fn be_root() -> Result<String> {
    let pools = cmd::run("zpool", &["list", "-H", "-o", "name"])?;
    for pool in pools.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let bootfs = cmd::run("zpool", &["get", "-H", "-o", "value", "bootfs", pool])?;
        if let Some((root, _)) = bootfs.trim().rsplit_once('/')
            && bootfs.trim() != "-"
        {
            return Ok(root.to_owned());
        }
    }
    Err(PlatformError::Unsupported(
        "a pool the computer boots from".into(),
    ))
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
///
/// `bectl` promotes a clone as it does: the snapshots it was made from move
/// to it, and ZFS refuses a name it already has. An OS update's environment
/// has its own `@glass-image`, received with it, beside the one it was
/// cloned from (ADR 0061), so the one moving is renamed first,
/// `<snapshot>-of-<environment>`. An environment promoted back (an update
/// taken out) gets its own name back.
pub fn activate(name: &str) -> Result<()> {
    checked(name)?;
    let root = be_root()?;
    let fs = format!("{root}/{name}");
    let renamed = make_way(&fs)?;
    if let Err(e) = cmd::run("bectl", &["activate", name]) {
        for (from, to) in renamed.iter().rev() {
            let _ = cmd::run("zfs", &["rename", to, from]);
        }
        return Err(e);
    }
    for (from, to) in names_back(&snapshots(&fs)?, name) {
        cmd::run(
            "zfs",
            &["rename", &format!("{fs}@{from}"), &format!("{fs}@{to}")],
        )?;
    }
    Ok(())
}

/// Renames the snapshots promoting `fs` would move onto names it has: each
/// `(old, new)`, in full.
fn make_way(fs: &str) -> Result<Vec<(String, String)>> {
    let origin = cmd::run("zfs", &["get", "-H", "-o", "value", "origin", fs])?;
    let Some((origin_fs, origin_snap)) = origin.trim().split_once('@') else {
        return Ok(Vec::new());
    };
    let env = origin_fs.rsplit('/').next().unwrap_or(origin_fs);
    let mut renamed = Vec::new();
    for snap in clashes(&snapshots(origin_fs)?, origin_snap, &snapshots(fs)?) {
        let (from, to) = (
            format!("{origin_fs}@{snap}"),
            format!("{origin_fs}@{snap}-of-{env}"),
        );
        cmd::run("zfs", &["rename", &from, &to])?;
        renamed.push((from, to));
    }
    Ok(renamed)
}

/// `fs`'s own snapshots, oldest first.
fn snapshots(fs: &str) -> Result<Vec<String>> {
    let text = cmd::run(
        "zfs",
        &[
            "list",
            "-H",
            "-o",
            "name",
            "-s",
            "createtxg",
            "-t",
            "snapshot",
            "-d",
            "1",
            fs,
        ],
    )?;
    Ok(text
        .lines()
        .filter_map(|l| l.trim().split_once('@').map(|(_, s)| s.to_owned()))
        .collect())
}

/// The snapshots a promotion moves from the origin's (those up to the one
/// the clone was made from, oldest first) whose names the clone has.
fn clashes(origin: &[String], made_from: &str, clone: &[String]) -> Vec<String> {
    let moving = origin
        .iter()
        .position(|s| s == made_from)
        .map_or(&[][..], |at| &origin[..=at]);
    moving
        .iter()
        .filter(|s| clone.contains(s))
        .cloned()
        .collect()
}

/// The snapshots renamed for `env` when another was promoted
/// (`<snapshot>-of-<env>`), back to their names, where it has none by then.
fn names_back(snaps: &[String], env: &str) -> Vec<(String, String)> {
    let suffix = format!("-of-{env}");
    snaps
        .iter()
        .filter_map(|s| {
            let own = s.strip_suffix(&suffix)?;
            (!snaps.iter().any(|t| t == own)).then(|| (s.clone(), own.to_owned()))
        })
        .collect()
}

/// Removes `name`, with the snapshot `bectl` made it from. An image's
/// snapshot, which an update's delta was received onto (ADR 0061), stays:
/// the image's next delta comes from it.
pub fn destroy(name: &str) -> Result<()> {
    checked(name)?;
    let root = be_root()?;
    let origin = cmd::run(
        "zfs",
        &[
            "get",
            "-H",
            "-o",
            "value",
            "origin",
            &format!("{root}/{name}"),
        ],
    )?;
    let made_for_it = origin
        .trim()
        .split_once('@')
        .is_none_or(|(_, snap)| bectl_snapshot(snap));
    let args: &[&str] = if made_for_it {
        &["destroy", "-o", name]
    } else {
        &["destroy", name]
    };
    cmd::run("bectl", args).map(|_| ())
}

/// One of `bectl create`'s own snapshots: `2026-10-25-10:32:27-0`.
fn bectl_snapshot(snap: &str) -> bool {
    let Some((at, n)) = snap.rsplit_once('-') else {
        return false;
    };
    let digit = |b: u8| b.is_ascii_digit();
    at.len() == 19
        && at.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 | 10 => b == b'-',
            13 | 16 => b == b':',
            _ => digit(b),
        })
        && !n.is_empty()
        && n.bytes().all(digit)
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

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The dev VM's first delta update (2026-10-25): `bectl activate`
    /// failed, "cannot promote ... conflicting snapshot 'glass-image' from
    /// parent". The update's own `@glass-image` came with the stream.
    #[test]
    fn ep11_an_update_promoted_moves_its_origin_image_snapshot_aside() {
        let default = names(&["2026-10-24-18:13:01-0", "glass-image"]);
        let update = names(&["glass-image"]);
        assert_eq!(clashes(&default, "glass-image", &update), ["glass-image"]);
        // Snapshots after the one it was made from stay where they are.
        let later = names(&["glass-image", "2026-10-25-09:24:33-0"]);
        assert!(clashes(&later, "glass-image", &names(&["2026-10-25-09:24:33-0"])).is_empty());
        // A kernel patch's environment has none of its own: nothing to move.
        assert!(clashes(&default, "2026-10-24-18:13:01-0", &[]).is_empty());
    }

    /// Taken out, the version before is promoted back and its image
    /// snapshot is `@glass-image` again, which the next delta comes from.
    #[test]
    fn ep11_the_version_before_gets_its_image_snapshot_name_back() {
        let back = names(&["glass-image-of-default", "2026-10-25-10:00:00-0"]);
        assert_eq!(
            names_back(&back, "default"),
            [(
                "glass-image-of-default".to_owned(),
                "glass-image".to_owned()
            )]
        );
        // Not another environment's, and not where it has its own.
        let update = names(&["glass-image-of-default", "glass-image"]);
        assert!(names_back(&update, "glass-os-2026.11.1").is_empty());
        assert!(names_back(&update, "default").is_empty());
    }

    /// The dev VM's update test (2026-10-25): an update that didn't start
    /// was removed with `bectl destroy -o`, and the snapshot it was
    /// received onto, the running image's `@glass-image`, went with it.
    #[test]
    fn ep11_removing_an_update_keeps_the_image_snapshot_it_came_from() {
        assert!(bectl_snapshot("2026-10-25-10:32:27-0"));
        assert!(bectl_snapshot("2026-10-25-10:32:27-12"));
        for kept in [
            "glass-image",
            "glass-image-of-default",
            "2026-10-25-10:32:27",
            "2026-10-25 10:32:27-0",
            "2026-10-25-10:32:27-x",
        ] {
            assert!(!bectl_snapshot(kept), "{kept}");
        }
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
