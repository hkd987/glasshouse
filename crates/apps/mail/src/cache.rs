//! `~/Mail`, the cache (ADR 0037 §6): a folder per mailbox, a file per
//! message. Mail synced from the person's service is named after its
//! folder's UIDVALIDITY and its UID (`0896246782-0000000042.eml`), so a
//! message id (`inbox/<file>`) says where the message is on the server, and
//! path labels, offline reads (DP-30) and draft undo work as they did when
//! `~/Mail` was the whole store. Files under any other name are the local
//! store's (drafts, and everything when no service is connected): a sync
//! never touches them.
//!
//! What the person has read is an empty file per message in the folder's
//! `.read` (ADR 0050): a mark's change names those files, so undo puts them
//! back as it does any file, and a move or a sync carries them along.
//! Drafts and sent mail are always read.
//!
//! Every write goes through `ConfinedDir` at the person's home and is
//! handed to them, so a link they put in `~/Mail` can't send root's writes
//! elsewhere (PA-3).

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use glass_managed::mail::Role;
use glass_platform::fs::{ConfinedDir, EntryInfo, Owner};

pub(crate) const MAIL_DIR: &str = "Mail";
/// A folder's read markers, by message file name (ADR 0050).
pub(crate) const READ_DIR: &str = ".read";
/// What a listing reads of each message: its header and the start of its
/// body.
const SUMMARY_BYTES: u64 = 64 * 1024;
/// The largest message kept whole; larger ones keep their header.
pub(crate) const MAX_MESSAGE_BYTES: u64 = 25 * 1024 * 1024;

/// A mailbox: what `mail.list` takes and a message id starts with.
pub(crate) fn key(role: Role) -> &'static str {
    match role {
        Role::Inbox => "inbox",
        Role::Drafts => "drafts",
        Role::Sent => "sent",
        Role::Archive => "archive",
        Role::Junk => "junk",
        Role::Trash => "trash",
    }
}

/// Its folder in `~/Mail`, and how people name it.
pub(crate) fn dir(role: Role) -> &'static str {
    match role {
        Role::Inbox => "Inbox",
        Role::Drafts => "Drafts",
        Role::Sent => "Sent",
        Role::Archive => "Archive",
        Role::Junk => "Junk",
        Role::Trash => "Trash",
    }
}

pub(crate) fn role_of_key(key_: &str) -> Option<Role> {
    Role::ALL.into_iter().find(|r| key(*r) == key_)
}

/// `Mail/<Folder>`, relative to the home.
pub(crate) fn rel_dir(role: Role) -> PathBuf {
    Path::new(MAIL_DIR).join(dir(role))
}

/// `Mail/<Folder>/.read`, relative to the home.
pub(crate) fn rel_read_dir(role: Role) -> PathBuf {
    rel_dir(role).join(READ_DIR)
}

/// Whether a folder keeps read state: drafts and sent mail are the
/// person's own, always read.
pub(crate) fn tracks_reading(role: Role) -> bool {
    !matches!(role, Role::Drafts | Role::Sent)
}

/// The file a synced message is kept in.
pub(crate) fn synced_name(uid_validity: u32, uid: u32) -> String {
    format!("{uid_validity:010}-{uid:010}.eml")
}

/// A synced message's UIDVALIDITY and UID, from its file name.
pub(crate) fn parse_synced(name: &str) -> Option<(u32, u32)> {
    let stem = name.strip_suffix(".eml")?;
    let (validity, uid) = stem.split_once('-')?;
    if validity.len() != 10 || uid.len() != 10 {
        return None;
    }
    Some((validity.parse().ok()?, uid.parse().ok()?))
}

/// A file name a message id may carry: plain, so an id never leaves its
/// folder.
pub(crate) fn plain_name(name: &str) -> bool {
    name.len() <= 200
        && name.ends_with(".eml")
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The person's home, opened for the cache.
pub(crate) struct Cache {
    home: ConfinedDir,
    owner: Owner,
}

impl Cache {
    pub(crate) fn open(home: &Path, owner: Owner) -> std::io::Result<Self> {
        Ok(Self {
            home: ConfinedDir::open(home)?,
            owner,
        })
    }

    /// Makes the mail folders the person doesn't have yet, theirs, with
    /// their read markers' folders.
    pub(crate) fn prepare(&self) -> std::io::Result<()> {
        for role in Role::ALL {
            let rel = rel_dir(role);
            if !self.home.exists(&rel) {
                self.home.create_dir_all_as(&rel, self.owner)?;
            }
            let read = rel_read_dir(role);
            if tracks_reading(role) && !self.home.exists(&read) {
                self.home.create_dir_all_as(&read, self.owner)?;
            }
        }
        Ok(())
    }

    /// The messages in a folder the person has read, by file name; every
    /// one kept, where the folder keeps no read state.
    pub(crate) fn read_names(&self, role: Role) -> std::io::Result<BTreeSet<String>> {
        if !tracks_reading(role) {
            return Ok(self.names(role)?.into_iter().collect());
        }
        match self.home.list(&rel_read_dir(role)) {
            Ok(entries) => Ok(entries
                .into_iter()
                .filter(|e| !e.is_dir && plain_name(&e.name))
                .map(|e| e.name)
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(e) => Err(e),
        }
    }

    /// Whether the person has read a message.
    pub(crate) fn is_read(&self, role: Role, name: &str) -> bool {
        !tracks_reading(role) || self.home.exists(&rel_read_dir(role).join(name))
    }

    /// Marks a message read or unread; whether that changed anything.
    pub(crate) fn set_read(&self, role: Role, name: &str, read: bool) -> std::io::Result<bool> {
        if !tracks_reading(role) || self.is_read(role, name) == read {
            return Ok(false);
        }
        let marker = rel_read_dir(role).join(name);
        if read {
            if !self.home.exists(&rel_read_dir(role)) {
                self.home
                    .create_dir_all_as(&rel_read_dir(role), self.owner)?;
            }
            match self.home.write_new(&marker, b"", Some(self.owner)) {
                Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
                _ => {}
            }
        } else {
            match self.home.remove_file(&marker) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(true)
    }

    /// The message files in a folder, by name.
    pub(crate) fn names(&self, role: Role) -> std::io::Result<Vec<String>> {
        Ok(self.entries(role)?.into_iter().map(|e| e.name).collect())
    }

    /// The message files in a folder, by name, with their size and when
    /// they last changed: what tells a listing's kept summary is still good.
    pub(crate) fn entries(&self, role: Role) -> std::io::Result<Vec<EntryInfo>> {
        match self.home.list(&rel_dir(role)) {
            Ok(entries) => Ok(entries
                .into_iter()
                .filter(|e| !e.is_dir && plain_name(&e.name))
                .collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// The UIDs kept for a folder at `uid_validity`.
    pub(crate) fn uids(&self, role: Role, uid_validity: u32) -> std::io::Result<Vec<u32>> {
        let mut uids: Vec<u32> = self
            .names(role)?
            .iter()
            .filter_map(|n| parse_synced(n))
            .filter(|(v, _)| *v == uid_validity)
            .map(|(_, u)| u)
            .collect();
        uids.sort_unstable();
        Ok(uids)
    }

    /// The UIDVALIDITY most of a folder's synced files carry, if any.
    pub(crate) fn kept_validity(&self, role: Role) -> std::io::Result<Option<u32>> {
        let mut counts: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
        for (validity, _) in self.names(role)?.iter().filter_map(|n| parse_synced(n)) {
            *counts.entry(validity).or_default() += 1;
        }
        Ok(counts.into_iter().max_by_key(|(_, n)| *n).map(|(v, _)| v))
    }

    /// Keeps a synced message; one already kept stays as it is.
    pub(crate) fn keep(&self, role: Role, name: &str, raw: &[u8]) -> std::io::Result<PathBuf> {
        let rel = rel_dir(role).join(name);
        match self.home.write_new(&rel, raw, Some(self.owner)) {
            Ok(()) => Ok(rel),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(rel),
            Err(e) => Err(e),
        }
    }

    /// Puts `raw` in place of a message that is here: a draft the person
    /// went on writing. Whole or not at all, as every write here is.
    pub(crate) fn replace(&self, role: Role, name: &str, raw: &[u8]) -> std::io::Result<PathBuf> {
        let rel = rel_dir(role).join(name);
        self.home.write(&rel, raw, Some(self.owner))?;
        Ok(rel)
    }

    /// Saves a new local message (a draft, or a send with no service)
    /// under a name nobody has taken: two tasks writing the same subject in
    /// the same second get two files.
    pub(crate) fn save_new(&self, role: Role, stem: &str, raw: &[u8]) -> std::io::Result<PathBuf> {
        for attempt in 1..=100 {
            let name = if attempt == 1 {
                format!("{stem}.eml")
            } else {
                format!("{stem}-{attempt}.eml")
            };
            let rel = rel_dir(role).join(name);
            match self.home.write_new(&rel, raw, Some(self.owner)) {
                Ok(()) => return Ok(rel),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no free file name",
        ))
    }

    /// Removes a message, and whether it was read with it.
    pub(crate) fn remove(&self, role: Role, name: &str) -> std::io::Result<()> {
        match self.home.remove_file(&rel_dir(role).join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        self.set_read(role, name, false).map(drop)
    }

    /// Moves a message, and whether it was read with it: a move keeps it
    /// read or not, on the server too (IMAP MOVE keeps its flags).
    pub(crate) fn rename(&self, from: (Role, &str), to: (Role, &str)) -> std::io::Result<()> {
        let read = self.is_read(from.0, from.1);
        self.home
            .rename(&rel_dir(from.0).join(from.1), &rel_dir(to.0).join(to.1))?;
        self.set_read(from.0, from.1, false)?;
        self.set_read(to.0, to.1, read).map(drop)
    }

    /// A whole message, up to the cap.
    pub(crate) fn read(&self, role: Role, name: &str) -> std::io::Result<Vec<u8>> {
        self.home
            .read(&rel_dir(role).join(name), MAX_MESSAGE_BYTES + 64 * 1024)
    }

    /// The start of a message: enough for a listing.
    pub(crate) fn read_start(&self, role: Role, name: &str) -> std::io::Result<Vec<u8>> {
        let file = self.home.open_file(&rel_dir(role).join(name))?;
        let mut out = Vec::new();
        file.take(SUMMARY_BYTES).read_to_end(&mut out)?;
        Ok(out)
    }

    /// Where a file in the cache is on the host.
    pub(crate) fn host_path(&self, rel: &Path) -> PathBuf {
        self.home.root().join(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_names_carry_uidvalidity_and_uid_and_nothing_else_parses() {
        let name = synced_name(896_246_782, 42);
        assert_eq!(name, "0896246782-0000000042.eml");
        assert_eq!(parse_synced(&name), Some((896_246_782, 42)));
        assert!(plain_name(&name));
        for local in [
            "1792400000-0-renewal-terms.eml",
            "1792400000-0.eml",
            "0896246782-42.eml",
            "x-0000000042.eml",
        ] {
            assert_eq!(parse_synced(local), None, "{local}");
        }
    }
}
