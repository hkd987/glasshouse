//! `mail.mark` (ADR 0050): marks messages read or unread, on this computer
//! and on the person's service (IMAP `\Seen`), and undo's hook that tells
//! the server again.
//!
//! What the person has read is an empty file per message in the folder's
//! `.read` ([`cache`]). A mark's change names that file, so undo puts it
//! back (or takes it away) from the snapshot like any other, then
//! [`MailProvider::mark_back`] tells the server what the file says now.
//! Reading a message (`mail.read`) never marks it: an agent going through
//! the inbox leaves what's new to the person new. Mail marks a message
//! read when the person opens it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glass_api::ChangeKind;
use glass_managed::mail::Role;
use serde_json::json;

use glass_actions::{ActionError, ActionOutput, Change, TaskContext};

use crate::cache;
use crate::handle::{self, SyncError};
use crate::{Folder, MAX_BATCH, MailProvider, Mode, message, plural};

/// What a mark says the message became.
pub(crate) fn state_word(read: bool) -> &'static str {
    if read { "read" } else { "unread" }
}

impl MailProvider {
    /// The messages a mark names, checked: in a folder that keeps read
    /// state, kept on this computer, each once.
    pub(crate) fn marks(
        &self,
        ctx: &TaskContext,
        ids: &[String],
    ) -> Result<Vec<(Folder, String)>, ActionError> {
        if ids.is_empty() || ids.len() > MAX_BATCH {
            return Err(ActionError::InvalidInput(format!(
                "mark 1 to {MAX_BATCH} messages per call"
            )));
        }
        let cache = Self::cache(ctx)?;
        let mut marks: Vec<(Folder, String)> = Vec::new();
        for id in ids {
            let (folder, name) = Self::message_ref(id)?;
            if !cache::tracks_reading(folder.role()) {
                return Err(ActionError::InvalidInput(format!(
                    "{id} is in {}: drafts and sent mail are always read",
                    folder.dir()
                )));
            }
            if !cache
                .names(folder.role())
                .is_ok_and(|n| n.iter().any(|n| n == name))
            {
                return Err(ActionError::NotFound(format!(
                    "message {id}: it isn't in {} on this computer (moved, or not synced yet; \
                     list the folder again)",
                    folder.dir()
                )));
            }
            if !marks.iter().any(|(f, n)| *f == folder && n == name) {
                marks.push((folder, name.to_owned()));
            }
        }
        Ok(marks)
    }

    /// `mail.mark`: the server first, so a refusal changes nothing here.
    pub(crate) fn mark_messages(
        &self,
        ctx: &TaskContext,
        ids: &[String],
        read: bool,
    ) -> Result<ActionOutput, ActionError> {
        let marks = self.marks(ctx, ids)?;
        let cache = Self::cache(ctx)?;
        let login = &ctx.principal.on_behalf_of.name;
        // One sync or mark at a time: a sync reading the server's state
        // before this mark can't put the old state back after it.
        let person = self.mail.person_lock(login);
        let _one = handle::lock(&person);
        let changing: Vec<&(Folder, String)> = marks
            .iter()
            .filter(|(f, n)| cache.is_read(f.role(), n) != read)
            .collect();
        let waiting = self.tell_server(login, &changing, read)?;
        let mut changes = Vec::new();
        for (folder, name) in &changing {
            cache
                .set_read(folder.role(), name, read)
                .map_err(|e| ActionError::io(format!("{}/{name}", folder.key()), e))?;
            let subject = cache
                .read_start(folder.role(), name)
                .map(|raw| message::parse(&raw).subject)
                .unwrap_or_default();
            changes.push(Change {
                kind: if read {
                    ChangeKind::Created
                } else {
                    ChangeKind::Deleted
                },
                target: format!(
                    "{}: {} → {}",
                    folder.dir(),
                    if subject.is_empty() {
                        "(no subject)"
                    } else {
                        &subject
                    },
                    state_word(read)
                ),
                reversible: true,
                paths: vec![cache.host_path(&cache::rel_read_dir(folder.role()).join(name))],
            });
        }
        let word = state_word(read);
        let mut text = match (changing.len(), marks.len()) {
            (0, 1) => format!("Already {word}"),
            (0, n) => format!("All {n} already {word}"),
            (n, _) => format!("Marked {} {word}", plural(n, "email")),
        };
        if waiting {
            text.push_str(match self.mode(login) {
                Mode::NotConnected(_) => {
                    " (the mail server hears once mail is connected on this computer)"
                }
                _ => " (the mail server hears when you're back online)",
            });
        }
        Ok(ActionOutput {
            text,
            data: json!({ "marked": changing.len(), "read": read }),
            changes,
        })
    }

    /// Tells the person's service of marks on its messages; whether any
    /// wait for it to be back in reach. The local store has no server.
    fn tell_server(
        &self,
        login: &str,
        marks: &[&(Folder, String)],
        read: bool,
    ) -> Result<bool, ActionError> {
        if !self.imap() {
            return Ok(false);
        }
        let mut synced: BTreeMap<(Role, u32), Vec<u32>> = BTreeMap::new();
        for (folder, name) in marks {
            if let Some((validity, uid)) = cache::parse_synced(name) {
                synced
                    .entry((folder.role(), validity))
                    .or_default()
                    .push(uid);
            }
        }
        let mut waiting = false;
        for ((role, validity), uids) in synced {
            match self.mail.tell_server(login, role, validity, &uids, read) {
                Ok(told) => waiting |= !told,
                Err(SyncError::Link(e)) => {
                    return Err(ActionError::Failed(format!(
                        "Not marked: the mail server refused ({e})."
                    )));
                }
                Err(SyncError::Local(why)) => {
                    return Err(ActionError::Failed(format!("Not marked: {why}.")));
                }
                Err(SyncError::NotConnected) => waiting = true,
            }
        }
        Ok(waiting)
    }

    /// Undo's hook (ADR 0050): the read marker is back as it was; the
    /// server hears so too.
    pub(crate) fn mark_back(
        &self,
        ctx: &TaskContext,
        paths: &[PathBuf],
    ) -> Result<(), ActionError> {
        let login = &ctx.principal.on_behalf_of.name;
        let cache = Self::cache(ctx)?;
        let person = self.mail.person_lock(login);
        let _one = handle::lock(&person);
        for path in paths {
            let Some((role, name)) = marker(ctx, path) else {
                continue;
            };
            let folder = Folder::of(role);
            let read = cache.is_read(role, &name);
            self.tell_server(login, &[&(folder, name)], read)?;
        }
        Ok(())
    }
}

/// The folder and message a read marker's host path names.
fn marker(ctx: &TaskContext, path: &Path) -> Option<(Role, String)> {
    let rel = path.strip_prefix(ctx.home.join(cache::MAIL_DIR)).ok()?;
    let mut parts = rel.components();
    let folder = parts.next()?.as_os_str().to_str()?;
    if parts.next()?.as_os_str() != cache::READ_DIR {
        return None;
    }
    let name = parts.next()?.as_os_str().to_str()?;
    if parts.next().is_some() || !cache::plain_name(name) {
        return None;
    }
    let role = Role::ALL.into_iter().find(|r| cache::dir(*r) == folder)?;
    Some((role, name.to_owned()))
}
