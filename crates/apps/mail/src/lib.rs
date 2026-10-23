//! The `mail` app's actions (NV-3, IN-2, ADR 0037): one set of actions,
//! whatever the person's mail service.
//!
//! With IT's `[mail] service = "imap"` and the person's password given once
//! through `mail.connect`, mail comes from their service through a
//! [`MailLink`](glass_managed::mail::MailLink) that glassd connects to the
//! shared [`MailHandle`], and `~/Mail` is its cache: each folder synced
//! into `~/Mail/<Folder>`, a file per message named after its UIDVALIDITY
//! and UID. Without a service, `~/Mail` is the whole store, as before EM0:
//! sent mail stays in Sent on this computer, and the Inbox is what is filed
//! there (the local store, for development and tests).
//!
//! Reading is quiet (`mail.list`, `mail.read`), and each message comes back
//! with its "by [agent] for [user]" line when an agent wrote it, so the Mail
//! app can show it inline (AU-10). Agents read text: HTML is reduced to it,
//! and only the Mail app is handed a message's HTML, which it shows in a
//! frame with no scripts and no network. Drafts stay on this computer and
//! undoable. Sending leaves the machine and can't be recalled, which is why
//! `mail.send` defaults to Ask; sends are batched ("Send 6 reminders" is
//! one call, one approval with a review list, DA-12). glassd builds each
//! message (`From:` from the directory, a Message-ID of its own, the
//! by-line header), submits it and files it in Sent; the audit record
//! keeps its Message-ID, size, SHA-256 and the server's reply, never its
//! text. Offline, a send says so and the message waits in Drafts.
//! `mail.move` files mail in Inbox, Archive, Junk or Trash; undo moves it
//! back. `mail.connect` is the person's own, in Mail: never an agent's.

mod cache;
mod handle;
mod message;

use std::path::PathBuf;

use glass_api::{AskLabels, ChangeKind};
use glass_managed::mail::{Account, LinkError, Outgoing, Role, Secret};
use glass_managed::{DirectoryHandle, Sender};
use glass_policy::document::{MailSecurity, MailServer, MailService, MailSettings};
use glass_types::{ActionName, SideEffect, Tier, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use glass_actions::{
    AUDIT_DETAIL, ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview,
    TaskContext, attribute, parse_input,
};

use crate::cache::{Cache, MAIL_DIR};
use crate::handle::SyncError;
pub use crate::handle::{MailHandle, Notice, Remote};
use crate::message::Draft;

/// The Mail app: the one caller handed a message's HTML, and the only one
/// that connects mail.
const APP: &str = "mail";
/// Most messages one `mail.send` or `mail.move` may carry.
const MAX_BATCH: usize = 50;
/// The most files a message carries, and their size together.
const MAX_ATTACHMENTS: usize = 10;
const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
/// Most messages one listing reads and returns; it returns
/// [`DEFAULT_LIMIT`] unless asked for more.
const MAX_LISTED: usize = 500;
const DEFAULT_LIMIT: usize = 50;
/// The longest password `mail.connect` takes.
const MAX_SECRET: usize = 1024;

/// A server from policy, as links reach it.
pub fn server(server: &MailServer) -> glass_managed::mail::Server {
    glass_managed::mail::Server {
        host: server.host.clone(),
        port: server.port,
        implicit_tls: server.security == MailSecurity::Tls,
    }
}

/// Mail, sent as the person the org's directory says they are, through
/// the service IT set in policy.
#[derive(Debug, Clone, Default)]
pub struct MailProvider {
    directory: DirectoryHandle,
    settings: Option<MailSettings>,
    mail: MailHandle,
}

/// Where a person's mail is, for one call.
enum Mode {
    /// `~/Mail` is the whole store: no service from IT, or one EM0 doesn't
    /// reach yet.
    Local,
    /// Their service, signed in.
    Remote(Box<(Remote, Account)>),
    /// IT set a service, and this computer can't reach it for them: why.
    NotConnected(String),
}

impl MailProvider {
    /// Mail whose `From:` is the person's name and address in `directory`
    /// (AD-1), or their login without one.
    pub fn new(directory: DirectoryHandle) -> Self {
        Self {
            directory,
            settings: None,
            mail: MailHandle::default(),
        }
    }

    /// The org's mail service from policy (NV-3): where mail goes, and the
    /// person's address when the directory has none.
    pub fn settings(mut self, settings: Option<MailSettings>) -> Self {
        self.settings = settings;
        self
    }

    /// The link to that service and each person's sign-in and cache
    /// state, which glassd keeps across policy changes (ADR 0037).
    pub fn mail(mut self, mail: MailHandle) -> Self {
        self.mail = mail;
        self
    }

    /// Who `login` sends as: the directory's name and address, else the
    /// address IT's template gives, else the login alone.
    fn sender(&self, login: &str) -> (Sender, &'static str) {
        let sender = self.directory.sender(login);
        if sender.email.is_some() {
            return (sender, "directory");
        }
        match self
            .settings
            .as_ref()
            .and_then(|s| s.address_template.as_deref())
        {
            Some(template) => (
                Sender {
                    name: login.to_owned(),
                    email: Some(glass_policy::mail::address_for(template, login)),
                },
                "template",
            ),
            None => (sender, "login"),
        }
    }

    fn imap(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|s| s.service == MailService::Imap)
    }

    fn mode(&self, login: &str) -> Mode {
        if !self.imap() {
            return Mode::Local;
        }
        match self.mail.account(login) {
            Some(both) => Mode::Remote(Box::new(both)),
            None if self.mail.remote().is_none() => {
                Mode::NotConnected("this computer can't reach the org's mail service yet".into())
            }
            None => Mode::NotConnected(
                "your mail isn't connected on this computer yet: connect it in Mail with your \
                 mail password"
                    .into(),
            ),
        }
    }

    /// `mail.account`: who the person sends as, and through which service.
    fn account(&self, ctx: &TaskContext) -> ActionOutput {
        let login = &ctx.principal.on_behalf_of.name;
        let (sender, source) = self.sender(login);
        let Some(settings) = &self.settings else {
            return ActionOutput {
                text: format!(
                    "No mail service from IT on this computer yet. Mail goes as {}, and sent mail \
                     stays in Sent here.",
                    sender.mailbox()
                ),
                // The same fields as with a service (IN-2), empty.
                data: json!({
                    "name": sender.name, "address": sender.email, "source": source,
                    "service": null, "service_title": null, "tenant": null,
                    "incoming": null, "outgoing": null,
                    "connected": false, "connectable": false,
                }),
                changes: Vec::new(),
            };
        };
        let server = |s: &Option<MailServer>| {
            s.as_ref()
                .map(|s| json!({ "host": s.host, "port": s.port, "security": s.security }))
        };
        let through = match &settings.tenant {
            Some(tenant) => format!("{} ({tenant})", settings.service.title()),
            None => settings.service.title().to_owned(),
        };
        let connected = self.imap() && self.mail.connected(login);
        let state = if connected {
            format!(
                "Connected as {}.",
                self.mail.user(login).unwrap_or_default()
            )
        } else if self.imap() {
            "Not connected on this computer yet: connect in Mail with your mail password. Until \
             then, mail can't be sent or received."
                .to_owned()
        } else {
            "Sending and receiving aren't connected to it yet: sent mail stays in Sent on this \
             computer."
                .to_owned()
        };
        ActionOutput {
            text: format!(
                "Mail goes as {} through {through}, set up by IT. {state}",
                sender.mailbox()
            ),
            data: json!({
                "name": sender.name,
                "address": sender.email,
                "source": source,
                "service": settings.service,
                "service_title": settings.service.title(),
                "tenant": settings.tenant,
                "incoming": server(&settings.incoming),
                "outgoing": server(&settings.outgoing),
                "connected": connected,
                "connectable": self.imap(),
            }),
            changes: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    to: Vec<String>,
    subject: String,
    body: String,
    /// Files sent with it, by their paths in the task's folders (ADR 0048).
    #[serde(default)]
    attachments: Vec<String>,
    /// The draft it is, `drafts/<file>`, whose files go with it.
    #[serde(default)]
    draft: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendInput {
    messages: Vec<Message>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInput {
    folder: Folder,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveInput {
    ids: Vec<String>,
    to: Folder,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkInput {
    ids: Vec<String>,
    read: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectInput {
    password: String,
    #[serde(default)]
    user: Option<String>,
}

/// A mailbox, as actions name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Folder {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Junk,
    Trash,
}

impl Folder {
    const ALL: [Folder; 6] = [
        Folder::Inbox,
        Folder::Drafts,
        Folder::Sent,
        Folder::Archive,
        Folder::Junk,
        Folder::Trash,
    ];

    fn role(self) -> Role {
        match self {
            Folder::Inbox => Role::Inbox,
            Folder::Drafts => Role::Drafts,
            Folder::Sent => Role::Sent,
            Folder::Archive => Role::Archive,
            Folder::Junk => Role::Junk,
            Folder::Trash => Role::Trash,
        }
    }

    fn of(role: Role) -> Folder {
        match role {
            Role::Inbox => Folder::Inbox,
            Role::Drafts => Folder::Drafts,
            Role::Sent => Folder::Sent,
            Role::Archive => Folder::Archive,
            Role::Junk => Folder::Junk,
            Role::Trash => Folder::Trash,
        }
    }

    fn dir(self) -> &'static str {
        cache::dir(self.role())
    }

    fn key(self) -> &'static str {
        cache::key(self.role())
    }

    /// Where `mail.move` may file mail (ADR 0037 §6).
    fn takes_moves(self) -> bool {
        matches!(
            self,
            Folder::Inbox | Folder::Archive | Folder::Junk | Folder::Trash
        )
    }
}

use message::Attribution;

/// A message as listings and reads return it.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct Summary {
    /// `<folder>/<file>`, what `mail.read` takes.
    id: String,
    folder: Folder,
    from: String,
    to: Vec<String>,
    subject: String,
    date: String,
    /// The start of the body, on one line.
    snippet: String,
    /// Who wrote it, when an agent or app did (AU-10).
    attribution: Option<Attribution>,
    /// Not opened by the person yet (ADR 0050).
    unread: bool,
}

/// A mailbox, as results name it ([`Folder`]).
fn folder_shape() -> Value {
    json!({ "enum": ["inbox", "drafts", "sent", "archive", "junk", "trash"] })
}

/// A message as listings ([`Summary`]) and, with `read`, reads return it.
fn summary_shape(read: bool) -> Value {
    let mut shape = json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "<folder>/<file>: what mail.read and mail.move take" },
            "folder": folder_shape(),
            "from": { "type": "string", "description": "\"Name <address>\" as the message has it" },
            "to": { "type": "array", "items": { "type": "string" } },
            "subject": { "type": "string" },
            "date": { "type": "string", "description": "RFC 3339, UTC; empty when unreadable" },
            "snippet": { "type": "string", "description": "the start of the body, on one line" },
            "attribution": {
                "type": ["object", "null"],
                "properties": {
                    "text": { "type": "string", "description": "\"by <agent> for <person>\"" },
                    "by": { "type": "string" },
                    "for": { "type": "string" }
                },
                "required": ["text", "by", "for"],
                "additionalProperties": false,
                "description": "who wrote it, when an agent or app did"
            },
            "unread": { "type": "boolean", "description": "the person hasn't opened it yet; drafts and sent mail are always read" }
        },
        "required": ["id", "folder", "from", "to", "subject", "date", "snippet", "attribution", "unread"],
        "additionalProperties": false
    });
    if read {
        shape["properties"]["body"] = json!({ "type": "string", "description": "the text" });
        shape["properties"]["html"] = json!({ "type": ["string", "null"], "description": "only for Mail itself; null for other apps and agents" });
        shape["properties"]["attachments"] = json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "size": { "type": "integer", "description": "bytes" },
                    "type": { "type": "string", "description": "its media type" }
                },
                "required": ["name", "size", "type"],
                "additionalProperties": false
            },
            "description": "the files it carries, by name; never their contents"
        });
        if let Some(required) = shape["required"].as_array_mut() {
            required.extend(["body", "html", "attachments"].map(|f| json!(f)));
        }
    }
    shape
}

/// A file's type from its name, for the message that carries it.
fn content_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
            "subject": { "type": "string" },
            "body": { "type": "string" },
            "attachments": {
                "type": "array",
                "items": { "type": "string" },
                "maxItems": MAX_ATTACHMENTS,
                "description": "Files to send with it, by their paths in this task's folders, as files.list gives them: \"Documents/ABC report.pdf\"."
            }
        },
        "required": ["to", "subject", "body"],
        "additionalProperties": false
    })
}

fn folder_names(filter: impl Fn(Folder) -> bool) -> Vec<&'static str> {
    Folder::ALL
        .into_iter()
        .filter(|f| filter(*f))
        .map(Folder::key)
        .collect()
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// Why a folder couldn't be brought up to date, said for whoever reads the
/// listing.
fn stale_note(error: &SyncError) -> String {
    match error {
        SyncError::NotConnected => {
            "mail isn't connected on this computer, so this is what it kept".into()
        }
        SyncError::Link(e @ LinkError::Unreachable(_)) => {
            format!("offline: {e}, so this is what this computer kept")
        }
        SyncError::Link(LinkError::SignIn(_)) => {
            "the mail server no longer accepts your password: connect again in Mail. This is \
             what this computer kept"
                .into()
        }
        SyncError::Link(LinkError::Refused(e)) | SyncError::Local(e) => {
            format!("couldn't bring it up to date ({e}), so this is what this computer kept")
        }
    }
}

impl MailProvider {
    fn mail_dir(ctx: &TaskContext) -> PathBuf {
        ctx.home.join(MAIL_DIR)
    }

    fn cache(ctx: &TaskContext) -> Result<Cache, ActionError> {
        Cache::open(&ctx.home, ctx.owner).map_err(|e| ActionError::io("mail store", e))
    }

    fn validate(message: &Message) -> Result<(), ActionError> {
        if message.to.is_empty() {
            return Err(ActionError::InvalidInput(
                "a message needs at least one recipient".into(),
            ));
        }
        for addr in &message.to {
            let ok = addr.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty()
                    && domain.contains('.')
                    && !addr
                        .chars()
                        .any(|c| c.is_whitespace() || c == '<' || c == '>')
            });
            if !ok {
                return Err(ActionError::InvalidInput(format!(
                    "{addr:?} is not an email address"
                )));
            }
        }
        if message.subject.contains(['\r', '\n']) {
            return Err(ActionError::InvalidInput("subject must be one line".into()));
        }
        if message.attachments.len() > MAX_ATTACHMENTS {
            return Err(ActionError::InvalidInput(format!(
                "a message carries at most {MAX_ATTACHMENTS} files"
            )));
        }
        Ok(())
    }

    /// Where `message`'s files are: each in a folder this task may read,
    /// so policy and labels judge them as any read (ADR 0048).
    fn attached<'c>(
        ctx: &'c TaskContext,
        message: &Message,
    ) -> Result<Vec<glass_actions::Resolved<'c>>, ActionError> {
        message.attachments.iter().map(|p| ctx.resolve(p)).collect()
    }

    /// The files the draft `message` comes from carries; none without one.
    fn draft_files(
        ctx: &TaskContext,
        message: &Message,
    ) -> Result<Vec<message::Attachment>, ActionError> {
        let Some(id) = &message.draft else {
            return Ok(Vec::new());
        };
        let (folder, name) = Self::message_ref(id)?;
        if folder.role() != Role::Drafts {
            return Err(ActionError::InvalidInput(format!(
                "{id} isn't a draft: only a draft's files go with it"
            )));
        }
        let raw = Self::cache(ctx)?
            .read(folder.role(), name)
            .map_err(|e| ActionError::io(format!("draft {id}"), e))?;
        Ok(message::attachments_of(&raw))
    }

    /// The names of the files `message` carries: its draft's, then those
    /// from the task's folders.
    fn file_names(ctx: &TaskContext, message: &Message) -> Result<Vec<String>, ActionError> {
        let mut names: Vec<String> = Self::draft_files(ctx, message)?
            .into_iter()
            .map(|f| f.name)
            .collect();
        names.extend(
            message
                .attachments
                .iter()
                .map(|p| p.rsplit('/').next().unwrap_or(p).to_owned()),
        );
        Ok(names)
    }

    /// `message`'s files: its draft's, and those read from the task's
    /// folders.
    fn attachments(
        ctx: &TaskContext,
        message: &Message,
    ) -> Result<Vec<message::Attachment>, ActionError> {
        let carried = Self::draft_files(ctx, message)?;
        let mut left = MAX_ATTACHMENT_BYTES
            .saturating_sub(carried.iter().map(|f| f.bytes.len() as u64).sum::<u64>());
        let read = Self::attached(ctx, message)?
            .into_iter()
            .map(|r| {
                let bytes = glass_platform::fs::ConfinedDir::open(&r.grant.root)
                    .and_then(|dir| dir.read(&r.rel, left))
                    .map_err(|e| match e.kind() {
                        std::io::ErrorKind::FileTooLarge => ActionError::InvalidInput(format!(
                            "the files come to more than {} MB together",
                            MAX_ATTACHMENT_BYTES / (1024 * 1024)
                        )),
                        _ => ActionError::io(&r.display, e),
                    })?;
                left = left.saturating_sub(bytes.len() as u64);
                let name = r.rel.file_name().map_or_else(
                    || "attachment".to_owned(),
                    |n| n.to_string_lossy().into_owned(),
                );
                Ok(message::Attachment {
                    content_type: content_type(&name).to_owned(),
                    name,
                    bytes,
                })
            })
            .collect::<Result<Vec<_>, ActionError>>()?;
        Ok(carried.into_iter().chain(read).collect())
    }

    /// `message` as glassd builds it, from the person and the agent, with
    /// its files.
    fn build(
        &self,
        ctx: &TaskContext,
        message: &Message,
        message_id: &str,
        attachments: &[message::Attachment],
    ) -> Vec<u8> {
        let (from, _) = self.sender(&ctx.principal.on_behalf_of.name);
        message::build(&Draft {
            attachments,
            from: &from,
            to: &message.to,
            subject: &message.subject,
            body: &message.body,
            attribution: &ctx.principal.attribution(),
            task: &ctx.principal.task.to_string(),
            message_id,
            date: Timestamp::now().unix_seconds(),
        })
    }

    /// The start of a local file's name: when, which, and a few words of
    /// the subject.
    fn stem(message: &Message, index: usize) -> String {
        let slug: String = message
            .subject
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .take(6)
            .collect::<Vec<_>>()
            .join("-");
        let stamp = Timestamp::now().unix_seconds();
        format!(
            "{stamp}-{index}-{}",
            if slug.is_empty() {
                "message".into()
            } else {
                slug
            }
        )
    }

    fn describe(message: &Message) -> String {
        format!("To {}: {}", message.to.join(", "), message.subject)
    }

    /// The review's line for `message`: whom, what, and the files with it.
    fn reviewed(ctx: &TaskContext, message: &Message) -> Result<String, ActionError> {
        let line = Self::describe(message);
        Ok(match Self::file_names(ctx, message)?.as_slice() {
            [] => line,
            names => format!("{line} · attached: {}", names.join(", ")),
        })
    }

    /// Checks a message id, `<folder>/<file>.eml`, and splits it. Plain
    /// names only, so an id never leaves its mailbox.
    fn message_ref(id: &str) -> Result<(Folder, &str), ActionError> {
        let bad = || ActionError::InvalidInput(format!("{id:?} is not a message id"));
        let (folder, name) = id.split_once('/').ok_or_else(bad)?;
        let folder = cache::role_of_key(folder).map(Folder::of).ok_or_else(bad)?;
        if cache::plain_name(name) {
            Ok((folder, name))
        } else {
            Err(bad())
        }
    }

    fn summary(folder: Folder, name: &str, parsed: &message::Parsed, unread: bool) -> Summary {
        Summary {
            id: format!("{}/{name}", folder.key()),
            folder,
            from: parsed.from.clone(),
            to: parsed.to.clone(),
            subject: parsed.subject.clone(),
            date: parsed.date.clone(),
            snippet: parsed.snippet.clone(),
            attribution: parsed.attribution.clone(),
            unread,
        }
    }

    /// Brings `folder` up to date when the person's service holds it and it
    /// isn't fresh; a note for the listing when it couldn't be.
    fn refresh(&self, ctx: &TaskContext, folder: Folder) -> Option<String> {
        let login = &ctx.principal.on_behalf_of.name;
        self.mail.remember(login, &ctx.home, ctx.owner);
        if folder == Folder::Drafts {
            return None;
        }
        match self.mode(login) {
            Mode::Local => None,
            Mode::NotConnected(why) => Some(format!("{why}; this is what this computer kept")),
            Mode::Remote(_) if self.mail.is_fresh(login, folder.role()) => None,
            Mode::Remote(_) => self
                .mail
                .sync(login, &ctx.home, ctx.owner, folder.role())
                .err()
                .map(|e| stale_note(&e)),
        }
    }

    fn list(
        &self,
        ctx: &TaskContext,
        folder: Folder,
        limit: usize,
    ) -> Result<ActionOutput, ActionError> {
        let note = self.refresh(ctx, folder);
        let cache = Self::cache(ctx)?;
        let names = cache
            .names(folder.role())
            .map_err(|e| ActionError::io("mail store", e))?;
        let read = cache
            .read_names(folder.role())
            .map_err(|e| ActionError::io("mail store", e))?;
        let unread = names.iter().filter(|n| !read.contains(*n)).count();
        // Stored names start with when they were saved or their UID, so
        // the last names are the newest; the Date header orders them.
        let mut messages: Vec<(i64, Summary)> = names
            .iter()
            .rev()
            .take(MAX_LISTED)
            .filter_map(|name| {
                let raw = cache.read_start(folder.role(), name).ok()?;
                let parsed = message::parse(&raw);
                let summary = Self::summary(folder, name, &parsed, !read.contains(name));
                Some((parsed.when, summary))
            })
            .collect();
        messages.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.id.cmp(&a.1.id)));
        messages.truncate(limit);
        let messages: Vec<Summary> = messages.into_iter().map(|(_, s)| s).collect();
        let n = messages.len();
        let mut text = format!("{} in {}", plural(n, "message"), folder.dir());
        if unread > 0 {
            text.push_str(&format!(", {unread} unread"));
        }
        if let Some(note) = note {
            text.push_str(&format!(" ({note})"));
        }
        Ok(ActionOutput {
            text,
            data: json!({ "folder": folder, "messages": messages, "unread": unread }),
            changes: Vec::new(),
        })
    }

    fn read(&self, ctx: &TaskContext, id: &str) -> Result<ActionOutput, ActionError> {
        let (folder, name) = Self::message_ref(id)?;
        let cache = Self::cache(ctx)?;
        let raw = cache
            .read(folder.role(), name)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ActionError::NotFound(format!(
                    "message {id}: it isn't in {} on this computer (moved, or not synced yet; list \
                 the folder again)",
                    folder.dir()
                )),
                _ => ActionError::io(format!("message {id}"), e),
            })?;
        let parsed = message::parse(&raw);
        let summary = Self::summary(folder, name, &parsed, !cache.is_read(folder.role(), name));
        let mut data = json!({ "message": summary });
        data["message"]["body"] = Value::String(parsed.body.clone());
        // Only the Mail app is handed HTML, which it frames with no scripts
        // and no network; agents read the text (ADR 0037).
        data["message"]["html"] = match (&parsed.html, ctx.principal.agent.as_str() == APP) {
            (Some(html), true) => Value::String(html.clone()),
            _ => Value::Null,
        };
        // The files it carries, by name: never their contents (ADR 0048).
        data["message"]["attachments"] = json!(parsed.attachments);
        let carries = match parsed.attachments.as_slice() {
            [] => String::new(),
            files => format!(
                " (attached: {})",
                files
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        Ok(ActionOutput {
            text: format!("From {}: {}{carries}", summary.from, summary.subject),
            data,
            changes: Vec::new(),
        })
    }

    fn draft(&self, ctx: &TaskContext, message: &Message) -> Result<ActionOutput, ActionError> {
        let cache = Self::cache(ctx)?;
        let (from, _) = self.sender(&ctx.principal.on_behalf_of.name);
        let files = Self::attachments(ctx, message)?;
        let raw = self.build(ctx, message, &message::new_message_id(&from), &files);
        let rel = cache
            .save_new(Role::Drafts, &Self::stem(message, 0), &raw)
            .map_err(|e| ActionError::io("saving draft", e))?;
        let path = cache.host_path(&rel);
        attribute(ctx, &path);
        let file = rel.file_name().map(|n| n.to_string_lossy().into_owned());
        Ok(ActionOutput {
            text: format!("Saved draft \"{}\"", message.subject),
            data: json!({
                "draft": rel.strip_prefix(MAIL_DIR).unwrap_or(&rel).display().to_string(),
                "id": file.map(|f| format!("drafts/{f}")),
            }),
            changes: vec![Change {
                kind: ChangeKind::Created,
                target: format!("Drafts: {}", message.subject),
                reversible: true,
                paths: vec![path],
            }],
        })
    }

    /// Keeps messages that weren't sent in Drafts; their ids.
    fn keep_unsent(&self, ctx: &TaskContext, cache: &Cache, messages: &[Message]) -> Vec<String> {
        let (from, _) = self.sender(&ctx.principal.on_behalf_of.name);
        messages
            .iter()
            .enumerate()
            .filter_map(|(i, m)| {
                let files = Self::attachments(ctx, m).unwrap_or_default();
                let raw = self.build(ctx, m, &message::new_message_id(&from), &files);
                let rel = cache.save_new(Role::Drafts, &Self::stem(m, i), &raw).ok()?;
                attribute(ctx, &cache.host_path(&rel));
                let name = rel.file_name()?.to_string_lossy().into_owned();
                Some(format!("drafts/{name}"))
            })
            .collect()
    }

    fn send(&self, ctx: &TaskContext, messages: &[Message]) -> Result<ActionOutput, ActionError> {
        let login = &ctx.principal.on_behalf_of.name;
        self.mail.remember(login, &ctx.home, ctx.owner);
        let cache = Self::cache(ctx)?;
        let (from, _) = self.sender(login);
        let remote = match self.mode(login) {
            Mode::Local => None,
            Mode::Remote(both) => Some(*both),
            Mode::NotConnected(why) => {
                let kept = self.keep_unsent(ctx, &cache, messages);
                return Err(ActionError::Failed(format!(
                    "Not sent: {why}. {} kept in Drafts ({}); send {} once mail is connected.",
                    if messages.len() == 1 {
                        "It's".to_owned()
                    } else {
                        format!("All {} are", messages.len())
                    },
                    kept.join(", "),
                    if messages.len() == 1 { "it" } else { "them" },
                )));
            }
        };
        if remote.is_some() && from.email.is_none() {
            return Err(ActionError::Failed(
                "Not sent: you have no mail address in the org's directory or IT's mail \
                 settings, so there's no one to send it as."
                    .into(),
            ));
        }
        // Every file read before anything leaves: one that can't be stops
        // the batch while nothing has been sent.
        let files: Vec<Vec<message::Attachment>> = messages
            .iter()
            .map(|m| Self::attachments(ctx, m))
            .collect::<Result<_, _>>()?;
        let mut changes = Vec::new();
        let mut audit = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let message_id = message::new_message_id(&from);
            let raw = self.build(ctx, message, &message_id, &files[index]);
            let (reply, path, filed) = match &remote {
                // No service: it stays in Sent here, as before EM0.
                None => {
                    let rel = cache
                        .save_new(Role::Sent, &Self::stem(message, index), &raw)
                        .map_err(|e| ActionError::io("sending", e))?;
                    (None, Some(cache.host_path(&rel)), true)
                }
                Some((remote, account)) => {
                    let envelope_from = from.email.as_deref().unwrap_or_default();
                    match remote.link.submit(
                        account,
                        &Outgoing {
                            from: envelope_from,
                            to: &message.to,
                            message: &raw,
                        },
                    ) {
                        Ok(submitted) => {
                            let (path, filed) = self.file_sent(ctx, &cache, remote, account, &raw);
                            (Some(submitted.reply), path, filed)
                        }
                        Err(e) => {
                            let kept = self.keep_unsent(ctx, &cache, &messages[index..]);
                            let why = match &e {
                                LinkError::Unreachable(m) => {
                                    format!(
                                        "you're offline or the mail server is out of reach ({m})"
                                    )
                                }
                                LinkError::SignIn(_) => "the mail server no longer accepts your \
                                                         password: connect again in Mail"
                                    .to_owned(),
                                LinkError::Refused(m) => format!("the mail server refused it: {m}"),
                            };
                            let waiting = format!(
                                "{} waiting in Drafts ({})",
                                plural(kept.len(), "message"),
                                kept.join(", ")
                            );
                            if index == 0 {
                                return Err(ActionError::Failed(format!(
                                    "Not sent: {why}. Nothing left this computer; {waiting}."
                                )));
                            }
                            return Ok(ActionOutput {
                                text: format!(
                                    "Sent {} of {}, then stopped: {why}. {waiting}.",
                                    index,
                                    messages.len()
                                ),
                                data: json!({ "sent": index, AUDIT_DETAIL: audit }),
                                changes,
                            });
                        }
                    }
                }
            };
            if let Some(path) = &path {
                attribute(ctx, path);
            }
            audit.push(json!({
                "message_id": format!("<{message_id}>"),
                "recipients": message.to.len(),
                // Which files left, by name, size and hash: never what's in
                // them (ADR 0048).
                "attachments": files[index]
                    .iter()
                    .map(|f| json!({
                        "name": f.name,
                        "size": f.bytes.len(),
                        "sha256": glass_managed::sha256_hex(&f.bytes),
                    }))
                    .collect::<Vec<_>>(),
                "size": raw.len(),
                "sha256": glass_managed::sha256_hex(&raw),
                "reply": reply,
                "filed_in_sent": filed,
            }));
            changes.push(Change {
                kind: ChangeKind::Sent,
                target: Self::describe(message),
                reversible: false,
                paths: path.into_iter().collect(),
            });
        }
        let unfiled = audit.iter().filter(|a| a["filed_in_sent"] == false).count();
        let mut text = format!("Sent {}", plural(messages.len(), "email"));
        if unfiled > 0 {
            text.push_str(&format!(
                "; {} couldn't be filed in Sent on the mail server",
                plural(unfiled, "copy")
            ));
        }
        Ok(ActionOutput {
            text,
            data: json!({ "sent": messages.len(), AUDIT_DETAIL: audit }),
            changes,
        })
    }

    /// Files a sent message in the service's Sent folder and the cache,
    /// byte for byte what left. Where it is here, and whether the server
    /// took it.
    fn file_sent(
        &self,
        ctx: &TaskContext,
        cache: &Cache,
        remote: &Remote,
        account: &Account,
        raw: &[u8],
    ) -> (Option<PathBuf>, bool) {
        let login = &ctx.principal.on_behalf_of.name;
        let Ok((name, mut state)) = self.mail.folder(login, remote, account, Role::Sent) else {
            return (None, false);
        };
        // Sent's UIDVALIDITY names the copy here: sync it once if this
        // computer hasn't.
        if state.is_none()
            && self
                .mail
                .sync(login, &ctx.home, ctx.owner, Role::Sent)
                .is_ok()
        {
            state = self
                .mail
                .folder(login, remote, account, Role::Sent)
                .ok()
                .and_then(|(_, s)| s);
        }
        match remote.link.append(account, &name, raw) {
            Ok(Some(uid)) => {
                let path = state.and_then(|s| {
                    let rel = cache
                        .keep(Role::Sent, &cache::synced_name(s.uid_validity, uid), raw)
                        .ok()?;
                    Some(cache.host_path(&rel))
                });
                (path, true)
            }
            Ok(None) => (None, true),
            Err(_) => (None, false),
        }
    }

    fn connect(
        &self,
        ctx: &TaskContext,
        input: &ConnectInput,
    ) -> Result<ActionOutput, ActionError> {
        let login = &ctx.principal.on_behalf_of.name;
        let (sender, _) = self.sender(login);
        let user = input.user.clone().or(sender.email.clone()).ok_or_else(|| {
            ActionError::Failed(
                "There's no mail address for you in the directory: give the name you sign in \
                     to mail with."
                    .into(),
            )
        })?;
        if !self.imap() {
            return Err(ActionError::Failed(
                "IT hasn't set up a mail service this computer connects to, so there's nothing \
                 to connect."
                    .into(),
            ));
        }
        let secret = Secret::new(input.password.clone());
        let Some((remote, account)) = self.mail.trial(login, &user, secret.clone()) else {
            return Err(ActionError::Failed(
                "This computer can't reach the org's mail service yet; nothing was saved.".into(),
            ));
        };
        remote.link.check(&account).map_err(|e| {
            ActionError::Failed(match e {
                LinkError::SignIn(_) => format!(
                    "The mail server didn't accept that password for {user}. Nothing was saved."
                ),
                LinkError::Unreachable(m) => {
                    format!("The mail server can't be reached ({m}). Nothing was saved; try again when online.")
                }
                LinkError::Refused(m) => format!("The mail server refused: {m}. Nothing was saved."),
            })
        })?;
        self.mail
            .save_sign_in(login, &user, &secret)
            .map_err(|e| ActionError::Failed(format!("Couldn't keep the sign-in: {e}")))?;
        self.mail.remember(login, &ctx.home, ctx.owner);
        // The Inbox right away, so Mail opens on it; a failure here is the
        // listing's to say.
        let _ = self.mail.sync(login, &ctx.home, ctx.owner, Role::Inbox);
        Ok(ActionOutput {
            text: format!(
                "Connected: mail comes and goes as {} through {}.",
                user, remote.incoming.host
            ),
            data: json!({ "connected": true, "user": user }),
            changes: Vec::new(),
        })
    }
}

impl ActionProvider for MailProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let listed = folder_names(|_| true);
        vec![
            ActionDef {
                name: "mail.account".parse().expect("static name"),
                title: "See the mail account".into(),
                description: "Who the user sends mail as (their name and address from the \
                              org's directory), the mail service IT set up for this computer, and \
                              whether it's connected."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "mail.list".parse().expect("static name"),
                title: "Read the mailboxes".into(),
                description: format!(
                    "List the messages in one mailbox ({}), newest first: sender, recipients, \
                     subject, date, the start of the body, and who wrote it when an agent did. Up \
                     to {MAX_LISTED}; {DEFAULT_LIMIT} unless limit says.",
                    listed.join(", ")
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "folder": { "type": "string", "enum": listed },
                        "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LISTED }
                    },
                    "required": ["folder"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "mail.read".parse().expect("static name"),
                title: "Read emails".into(),
                description: "Read one message, by the id mail.list gave it, as text.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: "mail.draft".parse().expect("static name"),
                title: "Draft an email".into(),
                description: "Save an email to Drafts without sending it.".into(),
                input_schema: message_schema(),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Auto,
                ask: AskLabels::new("Review draft", "Approve and save"),
            },
            ActionDef {
                name: "mail.send".parse().expect("static name"),
                title: "Send emails".into(),
                description: format!(
                    "Send one or more emails. Batch related messages into one call (up to \
                     {MAX_BATCH}); the user approves the batch once with a review list. Sent mail \
                     can't be recalled."
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "messages": { "type": "array", "items": message_schema(), "minItems": 1, "maxItems": MAX_BATCH }
                    },
                    "required": ["messages"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review drafts", "Approve and send"),
            },
            ActionDef {
                name: "mail.move".parse().expect("static name"),
                title: "File emails".into(),
                description: format!(
                    "Move messages, by the ids mail.list gave them, to inbox, archive, junk or \
                     trash (up to {MAX_BATCH}). Moving out of junk rescues a message. Undo moves \
                     them back."
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "ids": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": MAX_BATCH },
                        "to": { "type": "string", "enum": folder_names(Folder::takes_moves) }
                    },
                    "required": ["ids", "to"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Notify,
                ask: AskLabels::new("Review", "Approve and move"),
            },
            ActionDef {
                name: "mail.mark".parse().expect("static name"),
                title: "Mark emails read or unread".into(),
                description: format!(
                    "Mark messages, by the ids mail.list gave them, read or unread (up to \
                     {MAX_BATCH}), on this computer and on the mail server. Reading a message \
                     never marks it: what's new to the person stays new until they open it or \
                     this says otherwise. Drafts and sent mail are always read. Undo marks them \
                     back."
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "ids": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": MAX_BATCH },
                        "read": { "type": "boolean" }
                    },
                    "required": ["ids", "read"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Notify,
                ask: AskLabels::new("Review", "Approve and mark"),
            },
            ActionDef {
                name: "mail.connect".parse().expect("static name"),
                title: "Connect mail".into(),
                description: "The person connects their mail on this computer with their mail \
                              password, in Mail. Only people do this, never an agent."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "password": { "type": "string", "minLength": 1, "maxLength": MAX_SECRET },
                        "user": { "type": "string" }
                    },
                    "required": ["password"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Never,
                ask: AskLabels::new("Review", "Approve and connect"),
            },
        ]
    }

    fn prepare(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        // The person's own, made and handed over through the confined
        // handle: a chown on the joined path would follow a symlink they
        // put there (PA-3).
        Self::cache(ctx)?
            .prepare()
            .map_err(|e| ActionError::io("mail folders", e))
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        let mail = Self::mail_dir(ctx);
        match action.as_str() {
            "mail.account" => {
                let _: NoInput = parse_input(input)?;
                Ok(Preview {
                    summary: "See which mail account this is".into(),
                    targets: vec!["Mail account".into()],
                    ..Preview::default()
                })
            }
            "mail.list" => {
                let ListInput { folder, limit } = parse_input(input)?;
                if limit.is_some_and(|n| n == 0 || n > MAX_LISTED) {
                    return Err(ActionError::InvalidInput(format!(
                        "limit runs from 1 to {MAX_LISTED}"
                    )));
                }
                Ok(Preview {
                    summary: format!("Read mail: {}", folder.dir()),
                    targets: vec![folder.dir().to_owned()],
                    reads: vec![mail.join(folder.dir())],
                    ..Preview::default()
                })
            }
            "mail.read" => {
                let ReadInput { id } = parse_input(input)?;
                let (folder, name) = Self::message_ref(&id)?;
                Ok(Preview {
                    summary: format!("Read email {id}"),
                    targets: vec![id.clone()],
                    reads: vec![mail.join(folder.dir()).join(name)],
                    ..Preview::default()
                })
            }
            "mail.draft" => {
                let message: Message = parse_input(input)?;
                Self::validate(&message)?;
                let line = Self::reviewed(ctx, &message)?;
                Ok(Preview {
                    summary: format!("Draft email {}", line.replacen("To ", "to ", 1)),
                    targets: message.to.clone(),
                    items: vec![line],
                    reads: Self::attached(ctx, &message)?
                        .into_iter()
                        .map(|r| r.host)
                        .collect(),
                    writes: vec![mail.join(cache::dir(Role::Drafts))],
                    snapshot_roots: vec![mail.join(cache::dir(Role::Drafts))],
                    ..Preview::default()
                })
            }
            "mail.send" => {
                let SendInput { messages } = parse_input(input)?;
                if messages.is_empty() || messages.len() > MAX_BATCH {
                    return Err(ActionError::InvalidInput(format!(
                        "send 1 to {MAX_BATCH} messages per call"
                    )));
                }
                messages.iter().try_for_each(Self::validate)?;
                let n = messages.len();
                Ok(Preview {
                    summary: if n == 1 {
                        format!("Send email: {}", messages[0].subject)
                    } else {
                        format!("Send {n} emails")
                    },
                    targets: messages.iter().flat_map(|m| m.to.clone()).collect(),
                    items: messages
                        .iter()
                        .map(|m| Self::reviewed(ctx, m))
                        .collect::<Result<_, _>>()?,
                    // What each says: the person reads what they send.
                    texts: messages.iter().map(|m| m.body.clone()).collect(),
                    // What each carries is read, and judged, as any read.
                    reads: messages
                        .iter()
                        .map(|m| Self::attached(ctx, m))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .flatten()
                        .map(|r| r.host)
                        .collect(),
                    // Sent here; or Drafts, when it can't leave.
                    writes: vec![
                        mail.join(cache::dir(Role::Sent)),
                        mail.join(cache::dir(Role::Drafts)),
                    ],
                    ..Preview::default()
                })
            }
            "mail.move" => {
                let MoveInput { ids, to } = parse_input(input)?;
                let moves = self.moves(ctx, &ids, to)?;
                let cache = Self::cache(ctx)?;
                let items: Vec<String> = moves
                    .iter()
                    .map(|(folder, name)| {
                        let parsed = cache
                            .read_start(folder.role(), name)
                            .map(|raw| message::parse(&raw))
                            .ok();
                        match parsed {
                            Some(p) => format!(
                                "{}: from {}: {}",
                                folder.dir(),
                                p.from,
                                if p.subject.is_empty() {
                                    "(no subject)"
                                } else {
                                    &p.subject
                                }
                            ),
                            None => format!("{}: {name}", folder.dir()),
                        }
                    })
                    .collect();
                let mut dirs: Vec<PathBuf> = moves
                    .iter()
                    .map(|(f, _)| mail.join(f.dir()))
                    .chain([mail.join(to.dir())])
                    .collect();
                dirs.sort();
                dirs.dedup();
                Ok(Preview {
                    summary: match moves.as_slice() {
                        [_] => format!(
                            "Move an email to {}: {}",
                            to.dir(),
                            items[0].split_once(": ").map_or("", |(_, rest)| rest)
                        ),
                        _ => format!("Move {} emails to {}", moves.len(), to.dir()),
                    },
                    targets: ids.clone(),
                    items,
                    texts: Vec::new(),
                    reads: moves
                        .iter()
                        .map(|(f, n)| mail.join(f.dir()).join(n))
                        .collect(),
                    writes: dirs.clone(),
                    snapshot_roots: dirs,
                    label: None,
                })
            }
            "mail.mark" => {
                let MarkInput { ids, read } = parse_input(input)?;
                let marks = self.marks(ctx, &ids)?;
                let cache = Self::cache(ctx)?;
                let subjects: Vec<String> = marks
                    .iter()
                    .map(|(folder, name)| {
                        cache
                            .read_start(folder.role(), name)
                            .map(|raw| message::parse(&raw))
                            .map(|p| {
                                format!(
                                    "{}: from {}: {}",
                                    folder.dir(),
                                    p.from,
                                    if p.subject.is_empty() {
                                        "(no subject)"
                                    } else {
                                        &p.subject
                                    }
                                )
                            })
                            .unwrap_or_else(|_| format!("{}: {name}", folder.dir()))
                    })
                    .collect();
                let mut dirs: Vec<PathBuf> = marks
                    .iter()
                    .map(|(f, _)| mail.join(f.dir()).join(cache::READ_DIR))
                    .collect();
                dirs.sort();
                dirs.dedup();
                let word = marks::state_word(read);
                Ok(Preview {
                    summary: match subjects.as_slice() {
                        [one] => format!(
                            "Mark an email {word}: {}",
                            one.splitn(3, ": ").nth(2).unwrap_or_default()
                        ),
                        _ => format!("Mark {} emails {word}", marks.len()),
                    },
                    targets: ids.clone(),
                    items: subjects,
                    texts: Vec::new(),
                    reads: marks
                        .iter()
                        .map(|(f, n)| mail.join(f.dir()).join(n))
                        .collect(),
                    writes: dirs.clone(),
                    snapshot_roots: dirs,
                    label: None,
                })
            }
            "mail.connect" => {
                let input: ConnectInput = parse_input(input)?;
                if input.password.is_empty() || input.password.len() > MAX_SECRET {
                    return Err(ActionError::InvalidInput(format!(
                        "a password is 1 to {MAX_SECRET} characters"
                    )));
                }
                let (sender, _) = self.sender(&ctx.principal.on_behalf_of.name);
                let user = input
                    .user
                    .clone()
                    .or(sender.email)
                    .unwrap_or_else(|| ctx.principal.on_behalf_of.name.clone());
                // Never the password: the summary and targets are audited.
                Ok(Preview {
                    summary: format!("Connect mail as {user}"),
                    targets: vec![user],
                    ..Preview::default()
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "mail has no action {action}"
            ))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        match action.as_str() {
            "mail.account" => {
                let _: NoInput = parse_input(input)?;
                Ok(self.account(ctx))
            }
            "mail.list" => {
                let ListInput { folder, limit } = parse_input(input)?;
                let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LISTED);
                self.list(ctx, folder, limit)
            }
            "mail.read" => {
                let ReadInput { id } = parse_input(input)?;
                self.read(ctx, &id)
            }
            "mail.draft" => {
                let message: Message = parse_input(input)?;
                Self::validate(&message)?;
                self.draft(ctx, &message)
            }
            "mail.send" => {
                let SendInput { messages } = parse_input(input)?;
                if messages.is_empty() || messages.len() > MAX_BATCH {
                    return Err(ActionError::InvalidInput(format!(
                        "send 1 to {MAX_BATCH} messages per call"
                    )));
                }
                messages.iter().try_for_each(Self::validate)?;
                self.send(ctx, &messages)
            }
            "mail.move" => {
                let MoveInput { ids, to } = parse_input(input)?;
                self.move_messages(ctx, &ids, to)
            }
            "mail.mark" => {
                let MarkInput { ids, read } = parse_input(input)?;
                self.mark_messages(ctx, &ids, read)
            }
            "mail.connect" => {
                let input: ConnectInput = parse_input(input)?;
                self.connect(ctx, &input)
            }
            _ => Err(ActionError::InvalidInput(format!(
                "mail has no action {action}"
            ))),
        }
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        Some(match action.as_str() {
            "mail.list" => json!({
                "type": "object",
                "properties": {
                    "folder": folder_shape(),
                    "messages": { "type": "array", "items": summary_shape(false), "description": "newest first" },
                    "unread": { "type": "integer", "description": "how many in the whole folder the person hasn't opened, listed or not" }
                },
                "required": ["folder", "messages", "unread"],
                "additionalProperties": false
            }),
            "mail.read" => json!({
                "type": "object",
                "properties": { "message": summary_shape(true) },
                "required": ["message"],
                "additionalProperties": false
            }),
            "mail.draft" => json!({
                "type": "object",
                "properties": {
                    "draft": { "type": "string", "description": "where it was saved, Drafts/<file>" },
                    "id": { "type": ["string", "null"], "description": "what mail.read and mail.send's draft take: drafts/<file>" }
                },
                "required": ["draft", "id"],
                "additionalProperties": false
            }),
            "mail.send" => json!({
                "type": "object",
                "properties": { "sent": { "type": "integer", "description": "how many messages went" } },
                "required": ["sent"],
                "additionalProperties": false
            }),
            "mail.mark" => json!({
                "type": "object",
                "properties": {
                    "marked": { "type": "integer", "description": "how many changed; those already so don't count" },
                    "read": { "type": "boolean", "description": "what they are now" }
                },
                "required": ["marked", "read"],
                "additionalProperties": false
            }),
            "mail.move" => json!({
                "type": "object",
                "properties": {
                    "moved": { "type": "integer", "description": "how many messages moved" },
                    "to": folder_shape()
                },
                "required": ["moved", "to"],
                "additionalProperties": false
            }),
            "mail.account" => json!({
                "type": "object",
                "properties": {
                    "name": { "type": ["string", "null"], "description": "who mail goes as" },
                    "address": { "type": ["string", "null"], "description": "null when this computer has none for them yet" },
                    "source": { "type": "string", "description": "where the address came from" },
                    "service": { "type": ["string", "null"], "description": "IT's mail service, null without one" },
                    "service_title": { "type": ["string", "null"] },
                    "tenant": { "type": ["string", "null"] },
                    "incoming": { "type": ["object", "null"] },
                    "outgoing": { "type": ["object", "null"] },
                    "connected": { "type": "boolean", "description": "mail is sent and received through the service" },
                    "connectable": { "type": "boolean" }
                },
                "required": ["name", "address", "source", "service", "connected", "connectable"],
                "additionalProperties": false
            }),
            // Mail's own: the password goes only to the shipped Mail.
            _ => return None,
        })
    }

    fn revert(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        paths: &[PathBuf],
    ) -> Result<(), ActionError> {
        match action.as_str() {
            "mail.move" => self.move_back(ctx, paths),
            "mail.mark" => self.mark_back(ctx, paths),
            _ => Ok(()),
        }
    }
}

mod marks;
mod moves;

#[cfg(test)]
mod tests;
