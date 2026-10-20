//! `mail.move` (ADR 0037 §6): files messages in Inbox, Archive, Junk or
//! Trash, on the person's service (IMAP MOVE) and in the cache, and undo's
//! hook that moves them back.
//!
//! A move's change names two files: where the message was in the cache and
//! where it is now, each named after its folder's UIDVALIDITY and its UID.
//! Undo puts the files back from the snapshot taken before the move, then
//! [`MailProvider::move_back`] moves the message back on the server, which
//! gives it a new UID there, and renames the restored file to match.

use std::path::{Path, PathBuf};

use glass_api::ChangeKind;
use glass_managed::mail::{Account, LinkError, Role};
use serde_json::json;

use glass_actions::{ActionError, ActionOutput, Change, TaskContext};

use crate::cache::{self, Cache};
use crate::{Folder, MAX_BATCH, MailProvider, Mode, Remote, message, plural};

fn link_failure(doing: &str, e: &LinkError) -> String {
    match e {
        LinkError::Unreachable(m) => {
            format!("{doing}: you're offline or the mail server is out of reach ({m})")
        }
        LinkError::SignIn(_) => {
            format!(
                "{doing}: the mail server no longer accepts your password; connect again in Mail"
            )
        }
        LinkError::Refused(m) => format!("{doing}: the mail server refused ({m})"),
    }
}

impl MailProvider {
    /// The messages a move names, checked: in a folder mail is filed from,
    /// not in `to` already, each once.
    pub(crate) fn moves(
        &self,
        _ctx: &TaskContext,
        ids: &[String],
        to: Folder,
    ) -> Result<Vec<(Folder, String)>, ActionError> {
        if ids.is_empty() || ids.len() > MAX_BATCH {
            return Err(ActionError::InvalidInput(format!(
                "move 1 to {MAX_BATCH} messages per call"
            )));
        }
        if !to.takes_moves() {
            return Err(ActionError::InvalidInput(
                "mail is filed in inbox, archive, junk or trash".into(),
            ));
        }
        let mut moves: Vec<(Folder, String)> = Vec::new();
        for id in ids {
            let (folder, name) = Self::message_ref(id)?;
            if folder == Folder::Drafts {
                return Err(ActionError::InvalidInput(format!(
                    "{id} is a draft: drafts stay in Drafts until they're sent"
                )));
            }
            if folder == to {
                return Err(ActionError::InvalidInput(format!(
                    "{id} is in {} already",
                    to.dir()
                )));
            }
            if !moves.iter().any(|(f, n)| *f == folder && n == name) {
                moves.push((folder, name.to_owned()));
            }
        }
        Ok(moves)
    }

    pub(crate) fn move_messages(
        &self,
        ctx: &TaskContext,
        ids: &[String],
        to: Folder,
    ) -> Result<ActionOutput, ActionError> {
        let login = &ctx.principal.on_behalf_of.name;
        self.mail.remember(login, &ctx.home, ctx.owner);
        let moves = self.moves(ctx, ids, to)?;
        let cache = Self::cache(ctx)?;
        let remote = match self.mode(login) {
            Mode::Local => None,
            Mode::Remote(both) => Some(*both),
            Mode::NotConnected(why) => {
                return Err(ActionError::Failed(format!("Not moved: {why}.")));
            }
        };
        let mut changes = Vec::new();
        for (index, (from, name)) in moves.iter().enumerate() {
            let subject = cache
                .read_start(from.role(), name)
                .map(|raw| message::parse(&raw).subject)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => ActionError::NotFound(format!(
                        "{}/{name}: it isn't in {} on this computer any more",
                        from.key(),
                        from.dir()
                    )),
                    _ => ActionError::io(format!("{}/{name}", from.key()), e),
                })?;
            let moved = match (&remote, cache::parse_synced(name)) {
                (Some((remote, account)), Some((validity, uid))) => self.move_on_server(
                    ctx,
                    &cache,
                    remote,
                    account,
                    (*from, name),
                    (validity, uid),
                    to,
                ),
                // The local store's own file: moved here, as before EM0.
                _ => cache
                    .rename((from.role(), name), (to.role(), name))
                    .map(|()| Some(name.clone()))
                    .map_err(|e| format!("moving {}/{name}: {e}", from.key())),
            };
            let new = match moved {
                Ok(new) => new,
                Err(why) if index == 0 => {
                    return Err(ActionError::Failed(format!("Not moved: {why}.")));
                }
                Err(why) => {
                    return Ok(ActionOutput {
                        text: format!(
                            "Moved {} of {} to {}, then stopped: {why}.",
                            index,
                            moves.len(),
                            to.dir()
                        ),
                        data: json!({ "moved": index, "to": to }),
                        changes,
                    });
                }
            };
            let mut paths = vec![cache.host_path(&cache::rel_dir(from.role()).join(name))];
            paths.extend(new.map(|n| cache.host_path(&cache::rel_dir(to.role()).join(n))));
            changes.push(Change {
                kind: ChangeKind::Moved,
                target: format!(
                    "{} → {}: {}",
                    from.dir(),
                    to.dir(),
                    if subject.is_empty() {
                        "(no subject)"
                    } else {
                        &subject
                    }
                ),
                reversible: true,
                paths,
            });
        }
        Ok(ActionOutput {
            text: format!("Moved {} to {}", plural(moves.len(), "email"), to.dir()),
            data: json!({ "moved": moves.len(), "to": to }),
            changes,
        })
    }

    /// Moves one synced message on the server and in the cache; its file's
    /// name in `to`, when this computer knows it.
    #[allow(clippy::too_many_arguments)]
    fn move_on_server(
        &self,
        ctx: &TaskContext,
        cache: &Cache,
        remote: &Remote,
        account: &Account,
        (from, name): (Folder, &str),
        (validity, uid): (u32, u32),
        to: Folder,
    ) -> Result<Option<String>, String> {
        let login = &ctx.principal.on_behalf_of.name;
        let folder = |role: Role| {
            self.mail
                .folder(login, remote, account, role)
                .map_err(|e| format!("finding {}: {e:?}", cache::dir(role)))
        };
        let (from_name, from_state) = folder(from.role())?;
        if from_state.is_some_and(|s| s.uid_validity != validity) {
            return Err(format!(
                "{} changed on the server since it was listed; list it again",
                from.dir()
            ));
        }
        // The destination's UIDVALIDITY names the moved file: sync it once
        // if this computer hasn't.
        if folder(to.role())?.1.is_none() {
            self.mail
                .sync(login, &ctx.home, ctx.owner, to.role())
                .map_err(|e| format!("reading {}: {e:?}", to.dir()))?;
        }
        let (to_name, to_state) = folder(to.role())?;
        let message_id = cache
            .read_start(from.role(), name)
            .ok()
            .and_then(|raw| message::parse(&raw).message_id);
        let new_uid = remote
            .link
            .move_to(account, &from_name, uid, &to_name)
            .map_err(|e| link_failure("moving it", &e))?;
        let Some(to_validity) = to_state.map(|s| s.uid_validity) else {
            cache
                .remove(from.role(), name)
                .map_err(|e| format!("updating this computer's copy: {e}"))?;
            return Ok(None);
        };
        match new_uid {
            Some(new_uid) => {
                let new = cache::synced_name(to_validity, new_uid);
                cache
                    .rename((from.role(), name), (to.role(), &new))
                    .map_err(|e| format!("updating this computer's copy: {e}"))?;
                Ok(Some(new))
            }
            // No UIDPLUS: find it in a fresh copy of the destination.
            None => {
                cache
                    .remove(from.role(), name)
                    .map_err(|e| format!("updating this computer's copy: {e}"))?;
                let _ = self.mail.sync(login, &ctx.home, ctx.owner, to.role());
                Ok(message_id.and_then(|id| find(cache, to.role(), to_validity, &id)))
            }
        }
    }

    /// Undo's hook (ADR 0037): the files are back; the message goes back on
    /// the server too.
    pub(crate) fn move_back(
        &self,
        ctx: &TaskContext,
        paths: &[PathBuf],
    ) -> Result<(), ActionError> {
        let login = &ctx.principal.on_behalf_of.name;
        let [was, now] = paths else {
            // The local store's move, or one whose destination wasn't
            // known: the files are all there is.
            return Ok(());
        };
        let (Some((from, (validity, _))), Some((to, (_, uid)))) =
            (place(ctx, was), place(ctx, now))
        else {
            return Ok(());
        };
        let Mode::Remote(both) = self.mode(login) else {
            return Err(ActionError::Failed(
                "the message is back here, but mail isn't connected, so it couldn't be moved \
                 back on the mail server"
                    .into(),
            ));
        };
        let (remote, account) = *both;
        let cache = Self::cache(ctx)?;
        let failed = |e: String| ActionError::Failed(e);
        let (to_name, _) = self
            .mail
            .folder(login, &remote, &account, to)
            .map_err(|e| failed(format!("{e:?}")))?;
        let (from_name, from_state) = self
            .mail
            .folder(login, &remote, &account, from)
            .map_err(|e| failed(format!("{e:?}")))?;
        let back = remote
            .link
            .move_to(&account, &to_name, uid, &from_name)
            .map_err(|e| failed(link_failure("moving it back on the mail server", &e)))?;
        // The restored file names the UID it had; the server gave it a
        // new one.
        let restored = was
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match (back, from_state) {
            (Some(back), Some(state)) if state.uid_validity == validity => cache
                .rename(
                    (from, &restored),
                    (from, &cache::synced_name(validity, back)),
                )
                .map_err(|e| ActionError::io("the restored message", e)),
            _ => {
                let _ = self.mail.sync(login, &ctx.home, ctx.owner, from);
                Ok(())
            }
        }
    }
}

/// A file in the person's cache: its folder, and the UIDVALIDITY and UID
/// its name carries.
fn place(ctx: &TaskContext, path: &Path) -> Option<(Role, (u32, u32))> {
    let rel = path.strip_prefix(ctx.home.join(cache::MAIL_DIR)).ok()?;
    let mut parts = rel.components();
    let folder = parts.next()?.as_os_str().to_str()?;
    let name = parts.next()?.as_os_str().to_str()?;
    if parts.next().is_some() {
        return None;
    }
    let role = Role::ALL.into_iter().find(|r| cache::dir(*r) == folder)?;
    Some((role, cache::parse_synced(name)?))
}

/// The synced message in `role` with Message-ID `id`.
fn find(cache: &Cache, role: Role, validity: u32, id: &str) -> Option<String> {
    cache
        .names(role)
        .ok()?
        .into_iter()
        .filter(|n| cache::parse_synced(n).is_some_and(|(v, _)| v == validity))
        .find(|n| {
            cache
                .read_start(role, n)
                .is_ok_and(|raw| message::parse(&raw).message_id.as_deref() == Some(id))
        })
}
