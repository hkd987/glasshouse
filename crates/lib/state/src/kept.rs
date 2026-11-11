//! Which saved versions of a thing are kept, and which go (audit 5.3).
//!
//! Studio keeps each version of an app a person saved, and Routines each
//! version of a routine. Which one is in use is a file in the person's
//! home, and a task's undo puts that file back as it was. Both stores kept
//! every version for good.
//!
//! A version goes when all of these hold:
//!
//! - it isn't one of the newest few of its thing;
//! - it isn't the one in use;
//! - it wasn't in use at any time in the last [`AFTER_USE`].
//!
//! The last is what undo needs: undoing a change of version names the one
//! in use before it, and that one must still be there. A store notes a
//! version as in use when a call moves to it or away from it
//! ([`note_used`]), which is every change an undo can put back. A version
//! with no note counts from when it was saved, and never from before the
//! store began keeping notes ([`since`]): what was in use before then
//! isn't known, so nothing saved before then goes for [`AFTER_USE`].
//!
//! This module decides and keeps the notes; removing a version is its
//! store's.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a version stays after it was last known in use. An ended
/// task's undo reaches back 30 days; a task still open, as far as it has
/// been open. Three months is longer than tasks stay open.
pub const AFTER_USE: Duration = Duration::from_secs(90 * 24 * 3600);

/// The file under a store's root that says when it began keeping notes.
const SINCE: &str = ".kept-since";

/// A saved version: its number, and when it was last known in use (its
/// note, or when it was saved).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub number: u32,
    pub used: SystemTime,
}

fn seconds(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn read_time(path: &Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(text.trim().parse().ok()?))
}

fn write_time(path: &Path, at: SystemTime) -> io::Result<()> {
    crate::write(
        path,
        format!("{}\n", seconds(at)).as_bytes(),
        crate::PRIVATE,
    )
}

/// Notes in the file `note` that what it is beside is in use at `now`.
pub fn note_used(note: &Path, now: SystemTime) -> io::Result<()> {
    write_time(note, now)
}

/// When `note` last said so, if it ever did.
pub fn last_used(note: &Path) -> Option<SystemTime> {
    read_time(note)
}

/// When the store at `root` began keeping notes: `now`, the first time
/// this is asked of it.
pub fn since(root: &Path, now: SystemTime) -> io::Result<SystemTime> {
    let path = root.join(SINCE);
    if let Some(since) = read_time(&path) {
        return Ok(since);
    }
    write_time(&path, now)?;
    Ok(now)
}

/// The versions among `versions` that are past keeping at `now`, lowest
/// first: not one of the `newest` highest numbers, not `in_use`, and last
/// in use (or, from before `since`, counted from it) more than
/// [`AFTER_USE`] ago.
pub fn past_keeping(
    versions: &[Version],
    in_use: Option<u32>,
    newest: usize,
    since: SystemTime,
    now: SystemTime,
) -> Vec<u32> {
    let mut all: Vec<Version> = versions.to_vec();
    all.sort_by_key(|v| v.number);
    let older = all.len().saturating_sub(newest);
    all.into_iter()
        .take(older)
        .filter(|v| Some(v.number) != in_use)
        .filter(|v| {
            // A time after now, by this clock, is no time ago.
            now.duration_since(v.used.max(since))
                .is_ok_and(|ago| ago > AFTER_USE)
        })
        .map(|v| v.number)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn at(day: u64) -> SystemTime {
        UNIX_EPOCH + DAY * 20_000 + DAY * u32::try_from(day).unwrap()
    }

    fn saved(days: &[u64]) -> Vec<Version> {
        days.iter()
            .enumerate()
            .map(|(i, day)| Version {
                number: u32::try_from(i).unwrap() + 1,
                used: at(*day),
            })
            .collect()
    }

    #[test]
    fn audit_5_3_a_version_goes_only_when_old_out_of_use_and_long_unused() {
        // Six versions, one a day; notes kept from the start.
        let versions = saved(&[0, 1, 2, 3, 4, 5]);
        let gone = |in_use, newest, now| past_keeping(&versions, in_use, newest, at(0), at(now));
        // Within three months of its use, nothing goes, however many
        // newer ones there are.
        assert_eq!(gone(Some(6), 2, 60), Vec::<u32>::new());
        // After: all but the newest two, and never the one in use.
        assert_eq!(gone(Some(6), 2, 200), [1, 2, 3, 4]);
        assert_eq!(gone(Some(2), 2, 200), [1, 3, 4]);
        assert_eq!(gone(None, 6, 200), Vec::<u32>::new());
        // Each from its own last use: on day 92, only what was last used
        // by day 1.
        assert_eq!(gone(Some(6), 2, 92), [1, 2]);

        // One that was in use until lately stays: undo may name it.
        let mut versions = versions;
        versions[0].used = at(150);
        assert_eq!(
            past_keeping(&versions, Some(6), 2, at(0), at(200)),
            [2, 3, 4]
        );
    }

    #[test]
    fn audit_5_3_nothing_from_before_notes_were_kept_goes_for_three_months() {
        // Saved years before the store kept notes; which was in use, and
        // until when, nobody wrote down.
        let versions = saved(&[0, 1, 2, 3]);
        let began = at(1000);
        assert_eq!(
            past_keeping(&versions, Some(4), 1, began, at(1080)),
            Vec::<u32>::new()
        );
        assert_eq!(
            past_keeping(&versions, Some(4), 1, began, at(1091)),
            [1, 2, 3]
        );
    }

    #[test]
    fn audit_5_3_notes_are_kept_in_files_and_a_stores_beginning_once() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("app").join(".used");
        assert_eq!(last_used(&note), None);
        note_used(&note, at(3)).unwrap();
        assert_eq!(last_used(&note), Some(at(3)));
        note_used(&note, at(9)).unwrap();
        assert_eq!(last_used(&note), Some(at(9)));

        assert_eq!(since(dir.path(), at(5)).unwrap(), at(5));
        assert_eq!(since(dir.path(), at(50)).unwrap(), at(5));
        // A clock set wrong later doesn't move it back.
        assert_eq!(since(dir.path(), at(1)).unwrap(), at(5));
    }
}
