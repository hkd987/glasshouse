//! The `mail` app's actions, backed by the first-party mail store in the
//! user's home (`~/Mail/Inbox`, `~/Mail/Drafts`, `~/Mail/Sent`).
//!
//! Reading the mailboxes is quiet (`mail.list`, `mail.read`), and each
//! message comes back with its "by [agent] for [user]" line when an agent
//! wrote it, so the Mail app can show it inline (AU-10). Drafts are local
//! and undoable. Sending leaves the machine and can't be
//! recalled, which is why `mail.send` defaults to Ask. Sends are batched:
//! "Send 6 reminders" is one call, one approval with a review list (DA-12).
//! Until the mail service lands, "sent" messages are delivered to `~/Mail/Sent`
//! and the inbox is whatever is filed in `~/Mail/Inbox`.

use std::path::{Path, PathBuf};

use glass_api::{AskLabels, ChangeKind};
use glass_managed::{DirectoryHandle, Sender};
use glass_platform::fs::{ConfinedDir, Owner};
use glass_policy::document::MailSettings;
use glass_types::{ActionName, SideEffect, Tier, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, attribute,
    parse_input,
};

const MAIL_DIR: &str = "Mail";
const INBOX: &str = "Inbox";
const DRAFTS: &str = "Drafts";
const SENT: &str = "Sent";
/// Most messages one `mail.send` may carry.
const MAX_BATCH: usize = 50;
/// Largest message `mail.read` opens.
const MAX_MESSAGE_BYTES: u64 = 1024 * 1024;
/// Most messages one listing reads and returns; it returns
/// [`DEFAULT_LIMIT`] unless asked for more.
const MAX_LISTED: usize = 500;
const DEFAULT_LIMIT: usize = 50;
/// The header that carries "by [agent] for [user]" (AU-10).
const ATTRIBUTION_HEADER: &str = "X-Glasshouse-Attribution";

/// Mail, sent as the person the org's directory says they are, through
/// the service IT set in policy.
#[derive(Debug, Clone, Default)]
pub struct MailProvider {
    directory: DirectoryHandle,
    settings: Option<MailSettings>,
}

impl MailProvider {
    /// Mail whose `From:` is the person's name and address in `directory`
    /// (AD-1), or their login without one.
    pub fn new(directory: DirectoryHandle) -> Self {
        Self {
            directory,
            settings: None,
        }
    }

    /// The org's mail service from policy (NV-3): where mail goes, and the
    /// person's address when the directory has none.
    pub fn settings(mut self, settings: Option<MailSettings>) -> Self {
        self.settings = settings;
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

    /// `mail.account`: who the person sends as, and through which service.
    fn account(&self, ctx: &TaskContext) -> ActionOutput {
        let (sender, source) = self.sender(&ctx.principal.on_behalf_of.name);
        let Some(settings) = &self.settings else {
            return ActionOutput {
                text: format!(
                    "No mail service from IT on this computer yet. Mail goes as {}, and sent mail \
                     stays in Sent here.",
                    sender.mailbox()
                ),
                data: json!({
                    "name": sender.name, "address": sender.email, "source": source,
                    "service": null, "connected": false,
                }),
                changes: Vec::new(),
            };
        };
        let server = |s: &Option<glass_policy::document::MailServer>| {
            s.as_ref()
                .map(|s| json!({ "host": s.host, "port": s.port, "security": s.security }))
        };
        let through = match &settings.tenant {
            Some(tenant) => format!("{} ({tenant})", settings.service.title()),
            None => settings.service.title().to_owned(),
        };
        ActionOutput {
            text: format!(
                "Mail goes as {} through {through}, set up by IT. Sending and receiving aren't \
                 connected to it yet: sent mail stays in Sent on this computer.",
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
                "connected": false,
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

/// A mailbox in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Folder {
    Inbox,
    Drafts,
    Sent,
}

impl Folder {
    const ALL: [Folder; 3] = [Folder::Inbox, Folder::Drafts, Folder::Sent];

    fn dir(self) -> &'static str {
        match self {
            Folder::Inbox => INBOX,
            Folder::Drafts => DRAFTS,
            Folder::Sent => SENT,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Drafts => "drafts",
            Folder::Sent => "sent",
        }
    }
}

/// "by invoice-demo for demo", split into who wrote it and for whom.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct Attribution {
    text: String,
    by: String,
    #[serde(rename = "for")]
    for_user: String,
}

impl Attribution {
    fn parse(text: &str) -> Option<Self> {
        let (by, for_user) = text.strip_prefix("by ")?.rsplit_once(" for ")?;
        Some(Self {
            text: text.to_owned(),
            by: by.to_owned(),
            for_user: for_user.to_owned(),
        })
    }
}

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
}

/// Splits a stored message into its headers and body. Folded header lines
/// join the header before them.
fn parse_message(text: &str) -> (Vec<(String, String)>, String) {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut lines = text.split_inclusive('\n');
    for raw in lines.by_ref() {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    (headers, lines.collect())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map_or("", |(_, v)| v.as_str())
}

fn message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
            "subject": { "type": "string" },
            "body": { "type": "string" }
        },
        "required": ["to", "subject", "body"],
        "additionalProperties": false
    })
}

impl MailProvider {
    fn mail_dir(ctx: &TaskContext) -> PathBuf {
        ctx.home.join(MAIL_DIR)
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
        Ok(())
    }

    fn render(&self, ctx: &TaskContext, message: &Message, sent: bool) -> String {
        let mut text = format!(
            "From: {from}\nTo: {to}\nSubject: {subject}\nDate: {date}\nX-Glasshouse-Attribution: {attr}\nX-Glasshouse-Task: {task}\n",
            from = self.sender(&ctx.principal.on_behalf_of.name).0.mailbox(),
            to = message.to.join(", "),
            subject = message.subject,
            date = Timestamp::now(),
            attr = ctx.principal.attribution(),
            task = ctx.principal.task,
        );
        if sent {
            text.push_str("X-Glasshouse-Sent: yes\n");
        }
        text.push('\n');
        text.push_str(&message.body);
        if !message.body.ends_with('\n') {
            text.push('\n');
        }
        text
    }

    /// Saves a message under a name nobody else has taken: two tasks
    /// mailing the same subject in the same second get two files.
    fn save_new(
        mail: &ConfinedDir,
        folder: &str,
        message: &Message,
        index: usize,
        text: &str,
        owner: Owner,
    ) -> std::io::Result<PathBuf> {
        let name = Self::file_name(message, index);
        let stem = name.trim_end_matches(".eml");
        for attempt in 1..=100 {
            let file = if attempt == 1 {
                name.clone()
            } else {
                format!("{stem}-{attempt}.eml")
            };
            let rel = PathBuf::from(folder).join(file);
            match mail.write_new(&rel, text.as_bytes(), Some(owner)) {
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

    fn file_name(message: &Message, index: usize) -> String {
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
            "{stamp}-{index}-{}.eml",
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

    /// Checks a message id, `<folder>/<file>.eml`, and splits it. Plain
    /// names only, so an id never leaves its mailbox.
    fn message_ref(id: &str) -> Result<(Folder, &str), ActionError> {
        let bad = || ActionError::InvalidInput(format!("{id:?} is not a message id"));
        let (folder, name) = id.split_once('/').ok_or_else(bad)?;
        let folder = Folder::ALL
            .into_iter()
            .find(|f| f.key() == folder)
            .ok_or_else(bad)?;
        let plain = name.len() <= 200
            && name.ends_with(".eml")
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if plain {
            Ok((folder, name))
        } else {
            Err(bad())
        }
    }

    fn summary(folder: Folder, name: &str, text: &str) -> (Summary, String) {
        let (headers, body) = parse_message(text);
        let snippet: String = body
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(140)
            .collect();
        let summary = Summary {
            id: format!("{}/{name}", folder.key()),
            folder,
            from: header(&headers, "From").to_owned(),
            to: header(&headers, "To")
                .split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .map(str::to_owned)
                .collect(),
            subject: header(&headers, "Subject").to_owned(),
            date: header(&headers, "Date").to_owned(),
            snippet,
            attribution: Attribution::parse(header(&headers, ATTRIBUTION_HEADER)),
        };
        (summary, body)
    }

    fn read_text(mail: &ConfinedDir, rel: &Path) -> Result<String, ActionError> {
        let raw = mail
            .read(rel, MAX_MESSAGE_BYTES)
            .map_err(|e| ActionError::io(format!("message {}", rel.display()), e))?;
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}

impl ActionProvider for MailProvider {
    fn actions(&self) -> Vec<ActionDef> {
        vec![
            ActionDef {
                name: "mail.account".parse().expect("static name"),
                title: "See the mail account".into(),
                description: "Who the user sends mail as (their name and address from the \
                              org's directory) and the mail service IT set up for this computer."
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
                    "List the messages in one mailbox (inbox, drafts or sent), newest first: \
                     sender, recipients, subject, date, the start of the body, and who wrote it \
                     when an agent did. Up to {MAX_LISTED}; {DEFAULT_LIMIT} unless limit says."
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "folder": { "type": "string", "enum": ["inbox", "drafts", "sent"] },
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
                description: "Read one message, by the id mail.list gave it.".into(),
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
        ]
    }

    fn prepare(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        let home = ConfinedDir::open(&ctx.home).map_err(|e| ActionError::io("home folder", e))?;
        for folder in [INBOX, DRAFTS, SENT] {
            let rel = PathBuf::from(MAIL_DIR).join(folder);
            // The person's own, made and handed over through the confined
            // handle: a chown on the joined path would follow a symlink
            // they put there (PA-3).
            if !home.exists(&rel) {
                home.create_dir_all_as(&rel, ctx.owner)
                    .map_err(|e| ActionError::io("mail folders", e))?;
            }
        }
        Ok(())
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
                    items: Vec::new(),
                    reads: vec![mail.join(folder.dir())],
                    writes: Vec::new(),
                    snapshot_roots: Vec::new(),
                })
            }
            "mail.read" => {
                let ReadInput { id } = parse_input(input)?;
                let (folder, name) = Self::message_ref(&id)?;
                Ok(Preview {
                    summary: format!("Read email {id}"),
                    targets: vec![id.clone()],
                    items: Vec::new(),
                    reads: vec![mail.join(folder.dir()).join(name)],
                    writes: Vec::new(),
                    snapshot_roots: Vec::new(),
                })
            }
            "mail.draft" => {
                let message: Message = parse_input(input)?;
                Self::validate(&message)?;
                let line = Self::describe(&message);
                Ok(Preview {
                    summary: format!("Draft email {}", line.replacen("To ", "to ", 1)),
                    targets: message.to.clone(),
                    items: vec![line],
                    reads: Vec::new(),
                    writes: vec![mail.join(DRAFTS)],
                    snapshot_roots: vec![mail.join(DRAFTS)],
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
                    items: messages.iter().map(Self::describe).collect(),
                    reads: Vec::new(),
                    writes: vec![mail.join(SENT)],
                    snapshot_roots: Vec::new(),
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
        if action.as_str() == "mail.account" {
            let _: NoInput = parse_input(input)?;
            return Ok(self.account(ctx));
        }
        let mail = ConfinedDir::open(&Self::mail_dir(ctx))
            .map_err(|e| ActionError::io("mail store", e))?;
        match action.as_str() {
            "mail.list" => {
                let ListInput { folder, limit } = parse_input(input)?;
                let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LISTED);
                let entries = match mail.list(Path::new(folder.dir())) {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                    Err(e) => return Err(ActionError::io("mail store", e)),
                };
                // Stored names start with the time they were saved, so the
                // last names are the newest; the Date header orders them.
                let mut messages: Vec<Summary> = entries
                    .iter()
                    .rev()
                    .filter(|e| !e.is_dir)
                    .filter(|e| Self::message_ref(&format!("{}/{}", folder.key(), e.name)).is_ok())
                    .take(MAX_LISTED)
                    .filter_map(|e| {
                        let text =
                            Self::read_text(&mail, &Path::new(folder.dir()).join(&e.name)).ok()?;
                        Some(Self::summary(folder, &e.name, &text).0)
                    })
                    .collect();
                messages.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
                messages.truncate(limit);
                let n = messages.len();
                Ok(ActionOutput {
                    text: format!(
                        "{n} message{} in {}",
                        if n == 1 { "" } else { "s" },
                        folder.dir()
                    ),
                    data: json!({ "folder": folder, "messages": messages }),
                    changes: Vec::new(),
                })
            }
            "mail.read" => {
                let ReadInput { id } = parse_input(input)?;
                let (folder, name) = Self::message_ref(&id)?;
                let text = Self::read_text(&mail, &Path::new(folder.dir()).join(name))?;
                let (summary, body) = Self::summary(folder, name, &text);
                let mut data = json!({ "message": summary });
                data["message"]["body"] = Value::String(body);
                Ok(ActionOutput {
                    text: format!("From {}: {}", summary.from, summary.subject),
                    data,
                    changes: Vec::new(),
                })
            }
            "mail.draft" => {
                let message: Message = parse_input(input)?;
                Self::validate(&message)?;
                let rel = Self::save_new(
                    &mail,
                    DRAFTS,
                    &message,
                    0,
                    &self.render(ctx, &message, false),
                    ctx.owner,
                )
                .map_err(|e| ActionError::io("saving draft", e))?;
                attribute(ctx, &Self::mail_dir(ctx).join(&rel));
                let target = format!("Drafts: {}", message.subject);
                Ok(ActionOutput {
                    text: format!("Saved draft \"{}\"", message.subject),
                    data: json!({ "draft": rel.display().to_string() }),
                    changes: vec![Change {
                        kind: ChangeKind::Created,
                        target,
                        reversible: true,
                        paths: vec![Self::mail_dir(ctx).join(&rel)],
                    }],
                })
            }
            "mail.send" => {
                let SendInput { messages } = parse_input(input)?;
                messages.iter().try_for_each(Self::validate)?;
                let mut changes = Vec::new();
                for (index, message) in messages.iter().enumerate() {
                    let rel = Self::save_new(
                        &mail,
                        SENT,
                        message,
                        index,
                        &self.render(ctx, message, true),
                        ctx.owner,
                    )
                    .map_err(|e| ActionError::io("sending", e))?;
                    attribute(ctx, &Self::mail_dir(ctx).join(&rel));
                    changes.push(Change {
                        kind: ChangeKind::Sent,
                        target: Self::describe(message),
                        reversible: false,
                        paths: vec![Self::mail_dir(ctx).join(&rel)],
                    });
                }
                Ok(ActionOutput {
                    text: format!(
                        "Sent {} email{}",
                        messages.len(),
                        if messages.len() == 1 { "" } else { "s" }
                    ),
                    data: json!({ "sent": messages.len() }),
                    changes,
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "mail has no action {action}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use glass_actions::testing::context;

    fn setup() -> (tempfile::TempDir, TaskContext) {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context(dir.path());
        let meta = fs::metadata(dir.path()).unwrap();
        ctx.owner = glass_platform::fs::Owner {
            uid: meta.uid(),
            gid: meta.gid(),
        };
        MailProvider::default().prepare(&ctx).unwrap();
        (dir, ctx)
    }

    #[test]
    fn pa3_mail_folders_are_never_made_through_a_symlink_out_of_home() {
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(MAIL_DIR)).unwrap();
        let ctx = context(dir.path());
        assert!(MailProvider::default().prepare(&ctx).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn batched_sends_are_one_preview_with_a_review_list() {
        let (_dir, ctx) = setup();
        let input = json!({ "messages": (0..6).map(|i| json!({
            "to": [format!("vendor{i}@example.com")], "subject": "Invoice reminder", "body": "Please pay."
        })).collect::<Vec<_>>() });
        let preview = MailProvider::default()
            .preview(&ctx, &"mail.send".parse().unwrap(), &input)
            .unwrap();
        assert_eq!(preview.summary, "Send 6 emails");
        assert_eq!(preview.items.len(), 6);
        assert!(preview.snapshot_roots.is_empty(), "sends are not undoable");
    }

    #[test]
    fn drafts_carry_attribution_headers() {
        let (dir, ctx) = setup();
        let input =
            json!({ "to": ["ap@example.com"], "subject": "Q3 summary", "body": "Attached." });
        let out = MailProvider::default()
            .invoke(&ctx, &"mail.draft".parse().unwrap(), &input)
            .unwrap();
        assert!(out.changes[0].reversible);
        let drafts: Vec<_> = fs::read_dir(dir.path().join("Mail/Drafts"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(drafts.len(), 1);
        let text = fs::read_to_string(drafts[0].path()).unwrap();
        assert!(text.contains("X-Glasshouse-Attribution: by invoice-demo for demo"));
    }

    #[test]
    fn same_subject_in_the_same_second_makes_two_drafts() {
        // Two tasks drafting at once must not overwrite each other.
        let (dir, ctx) = setup();
        let input = json!({ "to": ["ap@example.com"], "subject": "Same", "body": "x" });
        let draft = "mail.draft".parse().unwrap();
        let first = MailProvider::default()
            .invoke(&ctx, &draft, &input)
            .unwrap();
        let second = MailProvider::default()
            .invoke(&ctx, &draft, &input)
            .unwrap();
        assert_ne!(first.changes[0].paths, second.changes[0].paths);
        let count = fs::read_dir(dir.path().join("Mail/Drafts"))
            .unwrap()
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn au10_listings_and_reads_say_who_wrote_each_message() {
        let (dir, ctx) = setup();
        fs::write(
            dir.path().join("Mail/Inbox/1792400000-0-northwind.eml"),
            "From: Dana Ruiz <dana@northwind.example>\nTo: demo@corp.example\n\
             Subject: Renewal terms\nDate: 2026-10-19T08:00:00Z\n\nHi,\n\nCan we talk\n terms?\n",
        )
        .unwrap();
        let input = json!({ "to": ["dana@northwind.example"], "subject": "Re: Renewal terms", "body": "Yes." });
        MailProvider::default()
            .invoke(&ctx, &"mail.draft".parse().unwrap(), &input)
            .unwrap();

        let list = "mail.list".parse().unwrap();
        let preview = MailProvider::default()
            .preview(&ctx, &list, &json!({ "folder": "inbox" }))
            .unwrap();
        assert_eq!(preview.reads, vec![dir.path().join("Mail/Inbox")]);
        assert!(preview.writes.is_empty());
        let inbox = MailProvider::default()
            .invoke(&ctx, &list, &json!({ "folder": "inbox" }))
            .unwrap();
        let message = &inbox.data["messages"][0];
        assert_eq!(message["subject"], "Renewal terms");
        assert_eq!(message["snippet"], "Hi, Can we talk terms?");
        assert!(message["attribution"].is_null(), "a person wrote it");

        let drafts = MailProvider::default()
            .invoke(&ctx, &list, &json!({ "folder": "drafts" }))
            .unwrap();
        let draft = &drafts.data["messages"][0];
        assert_eq!(draft["attribution"]["by"], "invoice-demo");
        assert_eq!(draft["attribution"]["for"], "demo");

        let read = MailProvider::default()
            .invoke(
                &ctx,
                &"mail.read".parse().unwrap(),
                &json!({ "id": draft["id"] }),
            )
            .unwrap();
        assert_eq!(read.data["message"]["body"], "Yes.\n");
        assert_eq!(
            read.data["message"]["attribution"]["text"],
            "by invoice-demo for demo"
        );
    }

    #[test]
    fn message_ids_stay_inside_their_mailbox() {
        let (_dir, ctx) = setup();
        let read = "mail.read".parse().unwrap();
        for id in [
            "inbox/../../etc/passwd.eml",
            "inbox/.hidden.eml",
            "archive/x.eml",
            "inbox/x.txt",
            "inbox",
        ] {
            assert!(
                matches!(
                    MailProvider::default().preview(&ctx, &read, &json!({ "id": id })),
                    Err(ActionError::InvalidInput(_))
                ),
                "{id}"
            );
        }
        assert!(
            MailProvider::default()
                .preview(
                    &ctx,
                    &"mail.list".parse().unwrap(),
                    &json!({ "folder": "trash" })
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_bad_addresses_and_header_injection() {
        let (_dir, ctx) = setup();
        let draft = "mail.draft".parse().unwrap();
        let bad_addr = json!({ "to": ["not an address"], "subject": "x", "body": "y" });
        assert!(
            MailProvider::default()
                .preview(&ctx, &draft, &bad_addr)
                .is_err()
        );
        let injected = json!({ "to": ["a@b.co"], "subject": "x\nBcc: all@corp.com", "body": "y" });
        assert!(
            MailProvider::default()
                .preview(&ctx, &draft, &injected)
                .is_err()
        );
    }
}
