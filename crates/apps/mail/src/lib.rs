//! The `mail` app's actions, backed by the first-party mail store in the
//! user's home (`~/Mail/Drafts`, `~/Mail/Sent`).
//!
//! Drafts are local and undoable. Sending leaves the machine and can't be
//! recalled, which is why `mail.send` defaults to Ask. Sends are batched:
//! "Send 6 reminders" is one call, one approval with a review list (DA-12).
//! Until the mail service lands, "sent" messages are delivered to `~/Mail/Sent`.

use std::path::PathBuf;

use glass_api::ChangeKind;
use glass_platform::fs::{ConfinedDir, Owner};
use glass_types::{ActionName, SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, attribute,
    parse_input,
};

const MAIL_DIR: &str = "Mail";
const DRAFTS: &str = "Drafts";
const SENT: &str = "Sent";
/// Most messages one `mail.send` may carry.
const MAX_BATCH: usize = 50;

#[derive(Debug, Default)]
pub struct MailProvider;

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

    fn render(ctx: &TaskContext, message: &Message, sent: bool) -> String {
        let mut text = format!(
            "From: {user}\nTo: {to}\nSubject: {subject}\nDate: {date}\nX-Glasshouse-Attribution: {attr}\nX-Glasshouse-Task: {task}\n",
            user = ctx.principal.on_behalf_of.name,
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
}

impl ActionProvider for MailProvider {
    fn actions(&self) -> Vec<ActionDef> {
        vec![
            ActionDef {
                name: "mail.draft".parse().expect("static name"),
                title: "Draft an email".into(),
                description: "Save an email to Drafts without sending it.".into(),
                input_schema: message_schema(),
                side_effect: SideEffect::LocalReversible,
                default_tier: Tier::Auto,
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
            },
        ]
    }

    fn prepare(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        let home = ConfinedDir::open(&ctx.home).map_err(|e| ActionError::io("home folder", e))?;
        for folder in [DRAFTS, SENT] {
            let rel = PathBuf::from(MAIL_DIR).join(folder);
            if !home.exists(&rel) {
                home.create_dir_all(&rel)
                    .map_err(|e| ActionError::io("mail folders", e))?;
                for dir in [PathBuf::from(MAIL_DIR), rel.clone()] {
                    std::os::unix::fs::chown(
                        ctx.home.join(&dir),
                        Some(ctx.owner.uid),
                        Some(ctx.owner.gid),
                    )
                    .map_err(|e| ActionError::io("mail folders", e))?;
                }
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
        let mail = ConfinedDir::open(&Self::mail_dir(ctx))
            .map_err(|e| ActionError::io("mail store", e))?;
        match action.as_str() {
            "mail.draft" => {
                let message: Message = parse_input(input)?;
                Self::validate(&message)?;
                let rel = Self::save_new(
                    &mail,
                    DRAFTS,
                    &message,
                    0,
                    &Self::render(ctx, &message, false),
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
                        &Self::render(ctx, message, true),
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
        MailProvider.prepare(&ctx).unwrap();
        (dir, ctx)
    }

    #[test]
    fn batched_sends_are_one_preview_with_a_review_list() {
        let (_dir, ctx) = setup();
        let input = json!({ "messages": (0..6).map(|i| json!({
            "to": [format!("vendor{i}@example.com")], "subject": "Invoice reminder", "body": "Please pay."
        })).collect::<Vec<_>>() });
        let preview = MailProvider
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
        let out = MailProvider
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
        let first = MailProvider.invoke(&ctx, &draft, &input).unwrap();
        let second = MailProvider.invoke(&ctx, &draft, &input).unwrap();
        assert_ne!(first.changes[0].paths, second.changes[0].paths);
        let count = fs::read_dir(dir.path().join("Mail/Drafts"))
            .unwrap()
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn rejects_bad_addresses_and_header_injection() {
        let (_dir, ctx) = setup();
        let draft = "mail.draft".parse().unwrap();
        let bad_addr = json!({ "to": ["not an address"], "subject": "x", "body": "y" });
        assert!(MailProvider.preview(&ctx, &draft, &bad_addr).is_err());
        let injected = json!({ "to": ["a@b.co"], "subject": "x\nBcc: all@corp.com", "body": "y" });
        assert!(MailProvider.preview(&ctx, &draft, &injected).is_err());
    }
}
