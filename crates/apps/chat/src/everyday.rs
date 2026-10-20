//! Chat's everyday features (CF-92, CF-93; ADR 0030 C4a): reactions,
//! edits, deletes, pins and topics as sealed parts, search on this
//! computer, drafts and how much the person wants to hear. Agents may
//! react, edit, delete, pin and set topics only with a yes, and may edit or
//! delete only what they posted; drafts and notification levels are the
//! person's own settings, which no agent's allowed actions name.

use std::sync::Arc;

use glass_actions::{
    ActionDef, ActionError, ActionOutput, Change, Preview, TaskContext, parse_input,
};
use glass_api::{AskLabels, ChangeKind, display_line};
use glass_managed::chat::{
    Conversation, ConversationKind, MAX_TEXT_CHARS, is_conversation_id, is_message_id,
};
use glass_managed::shared::Part;
use glass_types::{SideEffect, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::fold::{Folded, Opened, Shown};
use crate::store::{Member, Store};
use crate::{APP, ChatProvider, clip, clock, failed, in_chat, invalid, plural};

pub const REACT: &str = "chat.react";
pub const EDIT: &str = "chat.edit";
pub const DELETE: &str = "chat.delete";
pub const PIN: &str = "chat.pin";
pub const TOPIC: &str = "chat.topic";
pub const SEARCH: &str = "chat.search";
pub const DRAFT: &str = "chat.draft";
pub const NOTIFY: &str = "chat.notify";

pub const MEMBERS: &str = "chat.members";

/// Every action this module serves.
pub const ACTIONS: [&str; 9] = [
    REACT, EDIT, DELETE, PIN, TOPIC, SEARCH, DRAFT, NOTIFY, MEMBERS,
];

const MAX_TOPIC_CHARS: usize = 250;
const SEARCH_DEFAULT: usize = 20;
const SEARCH_MAX: usize = 50;
/// Messages a search looks through at most, newest conversations first.
const SEARCH_SCAN: usize = 20_000;
const MAX_QUERY_CHARS: usize = 200;
pub const LEVELS: [&str; 3] = ["all", "mentions", "none"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReactInput {
    conversation: String,
    message: String,
    emoji: String,
    #[serde(default = "yes")]
    on: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditInput {
    conversation: String,
    message: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteInput {
    conversation: String,
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PinInput {
    conversation: String,
    message: String,
    #[serde(default = "yes")]
    pinned: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TopicInput {
    conversation: String,
    topic: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchInput {
    query: String,
    #[serde(default)]
    conversation: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DraftInput {
    conversation: String,
    #[serde(default)]
    thread: Option<String>,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotifyInput {
    conversation: String,
    level: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MembersInput {
    conversation: String,
    /// People to add, by login.
    add: Vec<String>,
}

/// A short emoji: a few characters, none of them letters, digits, spaces
/// or controls.
fn check_emoji(emoji: &str) -> Result<(), ActionError> {
    let ok = !emoji.is_empty()
        && emoji.len() <= 32
        && emoji.chars().count() <= 8
        && !emoji
            .chars()
            .any(|c| c.is_ascii_alphanumeric() || c.is_whitespace() || c.is_control());
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("{emoji:?} isn't an emoji")))
    }
}

fn check_ids(conversation: &str, message: Option<&str>) -> Result<(), ActionError> {
    if !is_conversation_id(conversation) {
        return Err(invalid(format!(
            "{conversation:?} is not a conversation id"
        )));
    }
    if let Some(m) = message
        && !is_message_id(m)
    {
        return Err(invalid(format!("{m:?} is not a message id")));
    }
    Ok(())
}

/// The level unset means: everything in direct messages and groups, and
/// mentions in channels, as people expect from Slack.
pub(crate) fn level_for(store: &Store, c: &Conversation) -> &'static str {
    match store.notify.get(&c.id).map(String::as_str) {
        Some("all") => "all",
        Some("mentions") => "mentions",
        Some("none") => "none",
        _ if c.kind == ConversationKind::Channel => "mentions",
        _ => "all",
    }
}

impl ChatProvider {
    /// `c` folded for `member`'s person: every message this computer can
    /// open, edits, deletes, reactions, pins and topics applied. Folded once
    /// per new message.
    pub(crate) fn folded(
        &self,
        store: &Store,
        member: &Member,
        me: &str,
        c: &Conversation,
    ) -> Arc<Folded> {
        let all = store.messages.get(&c.id).map_or(&[][..], Vec::as_slice);
        let stamp = (all.len(), all.last().map_or(0, |m| m.seq));
        if let Some(f) = self.handle.cached_fold(me, &c.id, stamp) {
            return f;
        }
        let mut keys = std::collections::BTreeMap::new();
        let mut opened = Vec::with_capacity(all.len());
        let mut sealed = 0;
        for message in all {
            let Ok(header) = message.header() else {
                sealed += 1;
                continue;
            };
            let key = keys
                .entry(header.epoch)
                .or_insert_with(|| store.epoch_key(member, &c.id, header.epoch));
            let Some(body) = key.as_ref().and_then(|k| message.open(k).ok()) else {
                sealed += 1;
                continue;
            };
            opened.push(Opened {
                id: header.id,
                seq: message.seq,
                sender: header.sender,
                at: message.at.clone().unwrap_or_else(|| body.sent_at.clone()),
                body,
            });
        }
        let mut folded = Folded::new(&opened, c.kind == ConversationKind::Channel);
        folded.sealed = sealed;
        let folded = Arc::new(folded);
        self.handle.keep_fold(me, &c.id, stamp, Arc::clone(&folded));
        folded
    }

    /// One message as the Chat app and agents read it.
    pub(crate) fn view(&self, folded: &Folded, m: &Shown, me: &str) -> Value {
        let shared = match &m.shared {
            Some(Part::Shared { item, kind, title }) => {
                Some(json!({ "item": item, "kind": kind, "title": title }))
            }
            _ => None,
        };
        let replies: Vec<&Shown> = m.replies.iter().map(|&i| &folded.messages[i]).collect();
        let mut repliers: Vec<String> = Vec::new();
        for r in replies.iter().rev() {
            let name = self.name(&r.sender);
            if !repliers.contains(&name) && repliers.len() < 3 {
                repliers.push(name);
            }
        }
        json!({
            "id": m.id,
            "seq": m.seq,
            "sender": m.sender,
            "sender_name": self.name(&m.sender),
            "mine": m.sender == me,
            "at": m.at,
            "text": m.text,
            "attribution": m.attribution,
            "reply_to": m.reply_to,
            "shared": shared,
            "edited": m.edited,
            "deleted": m.deleted,
            "reactions": m.reactions.iter().map(|(emoji, by)| json!({
                "emoji": emoji,
                "count": by.len(),
                "mine": by.iter().any(|b| b == me),
                "names": by.iter().map(|b| self.name(b)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "pinned": m.pinned,
            "mentions": m.mentions.iter().map(|l| json!({ "login": l, "name": self.name(l) })).collect::<Vec<_>>(),
            "thread": m.thread,
            "replies": replies.len(),
            "last_reply_at": replies.last().map(|r| r.at.clone()),
            "repliers": repliers,
        })
    }

    /// One message as a line for an agent.
    pub(crate) fn line(&self, m: &Shown) -> String {
        let mut line = format!(
            "[{}] {}{}: {}",
            clock(&m.at),
            self.name(&m.sender),
            m.attribution
                .as_deref()
                .map(|a| format!(" ({a})"))
                .unwrap_or_default(),
            if m.deleted {
                "(deleted)"
            } else {
                m.text.as_str()
            }
        );
        if m.edited {
            line.push_str(" (edited)");
        }
        if let Some(Part::Shared { kind, item, .. }) = &m.shared {
            line.push_str(&format!(" [{kind} {item}]"));
        }
        if !m.replies.is_empty() {
            line.push_str(&format!(
                " [{}]",
                plural(m.replies.len(), "reply", "replies")
            ));
        }
        for (emoji, by) in &m.reactions {
            line.push_str(&format!(" [{emoji} {}]", by.len()));
        }
        if m.pinned {
            line.push_str(" [pinned]");
        }
        line.push_str(&format!(" (id {})", m.id));
        line
    }

    /// A conversation `me` is in, for the actions here.
    fn mine(&self, store: &Store, me: &str, id: &str) -> Result<Conversation, ActionError> {
        store
            .conversations
            .get(id)
            .filter(|c| in_chat(c) && c.members.iter().any(|m| m == me))
            .cloned()
            .ok_or_else(|| {
                ActionError::NotFound(format!(
                    "you aren't in a conversation {id}; chat.list shows yours"
                ))
            })
    }

    fn keys(&self, ctx: &TaskContext) -> Result<Member, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        self.member(ctx)
            .map_err(failed)?
            .ok_or_else(|| ActionError::NotFound(Self::not_opened(me)))
    }

    /// The conversation, its title and fold, and message `id` in it.
    fn message(
        &self,
        ctx: &TaskContext,
        conversation: &str,
        id: &str,
    ) -> Result<(Conversation, String, Arc<Folded>, Shown), ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let member = self.keys(ctx)?;
        let store = self.handle.load(me);
        let c = self.mine(&store, me, conversation)?;
        let title = self.title(&store, Some(&member), me, &c);
        let folded = self.folded(&store, &member, me, &c);
        let m = folded
            .get(id)
            .cloned()
            .ok_or_else(|| ActionError::NotFound(format!("no message {id} in {title}")))?;
        Ok((c, title, folded, m))
    }

    /// Whether whoever calls may change `m`: the person's own message, and
    /// for an agent one it posted (AU-10's by-line).
    fn may_change(&self, ctx: &TaskContext, m: &Shown) -> Result<(), ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        if m.sender != *me {
            return Err(invalid("you can edit or delete only your own messages"));
        }
        let agent = ctx.principal.agent.as_str();
        if agent != APP && m.attribution.as_deref() != Some(&format!("by {agent} for {me}")) {
            return Err(invalid(format!(
                "{agent} can edit or delete only messages it posted; this one is {me}'s own"
            )));
        }
        if m.deleted {
            return Err(invalid("that message was deleted"));
        }
        Ok(())
    }

    /// Posts `part` in `c` as the person, sealed like any message.
    fn send(
        &self,
        ctx: &TaskContext,
        c: &Conversation,
        part: Part,
        mentions: Vec<String>,
    ) -> Result<String, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let attribution = (ctx.principal.agent.as_str() != APP)
            .then(|| format!("by {} for {me}", ctx.principal.agent));
        self.post_sealed(me, &c.id, "", Some(part), attribution, None, mentions)
            .map_err(|e| ActionError::Failed(format!("Not sent: {e}.")))
    }

    fn sent(text: String, data: Value, title: &str, what: &str) -> ActionOutput {
        ActionOutput {
            text,
            data,
            changes: vec![Change {
                kind: ChangeKind::Sent,
                target: format!("{what} in {title}"),
                reversible: false,
                paths: Vec::new(),
            }],
        }
    }

    pub(crate) fn react(
        &self,
        ctx: &TaskContext,
        input: ReactInput,
    ) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let (c, title, _, m) = self.message(ctx, &input.conversation, &input.message)?;
        if m.deleted {
            return Err(invalid("that message was deleted"));
        }
        let on_now = m
            .reactions
            .iter()
            .any(|(e, by)| *e == input.emoji && by.iter().any(|b| b == me));
        let data = json!({ "message": m.id, "emoji": input.emoji, "on": input.on });
        if on_now == input.on {
            return Ok(ActionOutput {
                text: "Nothing to change.".into(),
                data,
                changes: Vec::new(),
            });
        }
        self.send(
            ctx,
            &c,
            Part::Reaction {
                to: m.id.clone(),
                emoji: input.emoji.clone(),
                off: !input.on,
            },
            Vec::new(),
        )?;
        let text = if input.on {
            format!("Reacted {} in {title}.", input.emoji)
        } else {
            format!("Took back {} in {title}.", input.emoji)
        };
        Ok(Self::sent(text, data, &title, "Reaction"))
    }

    pub(crate) fn edit(
        &self,
        ctx: &TaskContext,
        input: EditInput,
    ) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let (c, title, _, m) = self.message(ctx, &input.conversation, &input.message)?;
        self.may_change(ctx, &m)?;
        let data = json!({ "message": m.id });
        if m.text == input.text {
            return Ok(ActionOutput {
                text: "Nothing to change.".into(),
                data,
                changes: Vec::new(),
            });
        }
        let mentions = crate::fold::mentions(&input.text, &c.members, me);
        self.send(
            ctx,
            &c,
            Part::Edit {
                message: m.id.clone(),
                text: input.text,
            },
            mentions,
        )?;
        Ok(Self::sent(
            format!("Edited your message in {title}."),
            data,
            &title,
            "Edit",
        ))
    }

    pub(crate) fn delete(
        &self,
        ctx: &TaskContext,
        input: DeleteInput,
    ) -> Result<ActionOutput, ActionError> {
        let (c, title, _, m) = self.message(ctx, &input.conversation, &input.message)?;
        self.may_change(ctx, &m)?;
        self.send(
            ctx,
            &c,
            Part::Delete {
                message: m.id.clone(),
            },
            Vec::new(),
        )?;
        Ok(Self::sent(
            format!("Deleted your message in {title}. Anyone who read it already may remember it."),
            json!({ "message": m.id }),
            &title,
            "Delete",
        ))
    }

    pub(crate) fn pin(
        &self,
        ctx: &TaskContext,
        input: PinInput,
    ) -> Result<ActionOutput, ActionError> {
        let (c, title, _, m) = self.message(ctx, &input.conversation, &input.message)?;
        if m.deleted {
            return Err(invalid("that message was deleted"));
        }
        let data = json!({ "message": m.id, "pinned": input.pinned });
        if m.pinned == input.pinned {
            return Ok(ActionOutput {
                text: "Nothing to change.".into(),
                data,
                changes: Vec::new(),
            });
        }
        self.send(
            ctx,
            &c,
            Part::Pin {
                message: m.id.clone(),
                pinned: input.pinned,
            },
            Vec::new(),
        )?;
        let text = if input.pinned {
            format!("Pinned {}'s message in {title}.", self.name(&m.sender))
        } else {
            format!("Unpinned {}'s message in {title}.", self.name(&m.sender))
        };
        Ok(Self::sent(text, data, &title, "Pin"))
    }

    pub(crate) fn topic(
        &self,
        ctx: &TaskContext,
        input: TopicInput,
    ) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let member = self.keys(ctx)?;
        let store = self.handle.load(me);
        let c = self.mine(&store, me, &input.conversation)?;
        let title = self.title(&store, Some(&member), me, &c);
        let topic = input.topic.trim().to_owned();
        if self
            .folded(&store, &member, me, &c)
            .topic
            .as_deref()
            .unwrap_or("")
            == topic
        {
            return Ok(ActionOutput {
                text: "Nothing to change.".into(),
                data: json!({ "topic": topic }),
                changes: Vec::new(),
            });
        }
        self.send(
            ctx,
            &c,
            Part::Topic {
                text: topic.clone(),
            },
            Vec::new(),
        )?;
        let text = if topic.is_empty() {
            format!("Cleared {title}'s topic.")
        } else {
            format!("Set {title}'s topic.")
        };
        Ok(Self::sent(text, json!({ "topic": topic }), &title, "Topic"))
    }

    pub(crate) fn search(
        &self,
        ctx: &TaskContext,
        input: SearchInput,
    ) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let limit = input.limit.unwrap_or(SEARCH_DEFAULT).clamp(1, SEARCH_MAX);
        let member = self.keys(ctx)?;
        let store = self.handle.load(me);
        let mut conversations: Vec<Conversation> = match &input.conversation {
            Some(id) => vec![self.mine(&store, me, id)?],
            None => store
                .conversations
                .values()
                .filter(|c| in_chat(c) && c.members.iter().any(|m| m == me))
                .cloned()
                .collect(),
        };
        // Newest activity first, so the cap leaves out the oldest.
        let last = |c: &Conversation| {
            store
                .messages
                .get(&c.id)
                .and_then(|m| m.last())
                .map_or(0, |m| m.seq)
        };
        conversations.sort_by_key(|c| std::cmp::Reverse(last(c)));
        let needle = input.query.trim().to_lowercase();
        let mut hits: Vec<(u64, Value, String)> = Vec::new();
        let mut scanned = 0;
        let mut capped = false;
        for c in &conversations {
            let folded = self.folded(&store, &member, me, c);
            let title = self.title(&store, Some(&member), me, c);
            for m in folded.messages.iter().rev() {
                if scanned >= SEARCH_SCAN {
                    capped = true;
                    break;
                }
                scanned += 1;
                if m.deleted || !m.text.to_lowercase().contains(&needle) {
                    continue;
                }
                let snippet = clip(&m.text, 200);
                hits.push((
                    m.seq,
                    json!({
                        "conversation": c.id,
                        "title": title,
                        "message": m.id,
                        "thread": m.thread,
                        "sender_name": self.name(&m.sender),
                        "at": m.at,
                        "text": snippet,
                    }),
                    format!(
                        "- {title}, {} at {}: {snippet} (conversation {}, message {})",
                        self.name(&m.sender),
                        clock(&m.at),
                        c.id,
                        m.id
                    ),
                ));
            }
        }
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        let more = hits.len() > limit;
        hits.truncate(limit);
        let mut text = format!(
            "{} for that{}{}.",
            plural(hits.len(), "message", "messages"),
            if more {
                " (the newest; there are more)"
            } else {
                ""
            },
            if capped {
                ", in the newest messages only"
            } else {
                ""
            },
        );
        for h in &hits {
            text.push('\n');
            text.push_str(&h.2);
        }
        Ok(ActionOutput {
            text,
            data: json!({ "hits": hits.into_iter().map(|h| h.1).collect::<Vec<_>>(), "more": more }),
            changes: Vec::new(),
        })
    }

    pub(crate) fn draft(
        &self,
        ctx: &TaskContext,
        input: DraftInput,
    ) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let store = self.handle.load(me);
        let c = self.mine(&store, me, &input.conversation)?;
        self.handle
            .set_draft(me, &c.id, input.thread.as_deref(), &input.text);
        Ok(ActionOutput {
            text: if input.text.trim().is_empty() {
                "Draft cleared.".into()
            } else {
                "Draft kept.".into()
            },
            data: json!({ "conversation": c.id, "thread": input.thread }),
            changes: Vec::new(),
        })
    }

    pub(crate) fn notify(
        &self,
        ctx: &TaskContext,
        input: NotifyInput,
    ) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let member = self.keys(ctx)?;
        let _write = self.handle.lock();
        let mut store = self.handle.load(me);
        let c = self.mine(&store, me, &input.conversation)?;
        let title = self.title(&store, Some(&member), me, &c);
        store.notify.insert(c.id.clone(), input.level.clone());
        self.handle.save(me, &store).map_err(failed)?;
        Ok(ActionOutput {
            text: match input.level.as_str() {
                "all" => format!("You'll hear of every message in {title}."),
                "mentions" => format!("You'll hear of {title} only when you're mentioned."),
                _ => format!("You won't hear of {title}."),
            },
            data: json!({ "conversation": c.id, "level": input.level }),
            changes: Vec::new(),
        })
    }

    /// Adds people to a channel or group (ADR 0030 C2): a new epoch, sealed
    /// to everyone in it after and to IT, with the key before it sealed
    /// under it, so the people added read the whole history, as in Slack.
    pub(crate) fn add_members(
        &self,
        ctx: &TaskContext,
        input: MembersInput,
    ) -> Result<ActionOutput, ActionError> {
        use glass_crypto::EpochKey;
        use glass_managed::chat::{Epoch, LookupKeys, MemberOp, RotateEpoch};

        let me = &ctx.principal.on_behalf_of.name;
        let not_added = |e: String| ActionError::Failed(format!("Nobody was added: {e}."));
        let connection = self
            .handle
            .connection()
            .map_err(|e| not_added(e.to_string()))?;
        let compliance = self
            .handle
            .compliance()
            .map_err(|e| not_added(e.to_string()))?;
        let member = self.keys(ctx)?;
        self.check_people(me, &input.add)?;
        let store = self.handle.load(me);
        let c = self.mine(&store, me, &input.conversation)?;
        let title = self.title(&store, Some(&member), me, &c);
        if !matches!(c.kind, ConversationKind::Channel | ConversationKind::Group) {
            return Err(invalid(
                "people are added to channels and groups; a direct message stays between two",
            ));
        }
        let mut add: Vec<String> = input
            .add
            .iter()
            .filter(|l| !c.members.contains(l))
            .cloned()
            .collect();
        add.sort();
        add.dedup();
        if add.is_empty() {
            return Ok(ActionOutput {
                text: format!("They're in {title} already."),
                data: json!({ "conversation": c.id, "added": [] }),
                changes: Vec::new(),
            });
        }
        let mut after: Vec<String> = c.members.iter().chain(&add).cloned().collect();
        after.sort();
        let found = connection
            .link
            .lookup(&LookupKeys {
                logins: after.clone(),
            })
            .map_err(|e| not_added(e.to_string()))?;
        if found.compliance != compliance.hex {
            return Err(not_added(
                "your organization's server offered a compliance key that isn't the one IT signed"
                    .into(),
            ));
        }
        let keys: Vec<_> = found
            .keys
            .into_iter()
            .filter(|k| after.contains(&k.login) && k.keys().is_ok())
            .collect();
        let missing: Vec<String> = add
            .iter()
            .filter(|l| !keys.iter().any(|k| &k.login == *l))
            .map(|l| self.name(l))
            .collect();
        if !missing.is_empty() {
            return Err(not_added(format!(
                "{} {} opened Chat yet, so there's no key to seal to; ask them to open Chat once",
                missing.join(", "),
                if missing.len() == 1 {
                    "hasn't"
                } else {
                    "haven't"
                }
            )));
        }
        let current = store
            .epoch_key(&member, &c.id, c.epoch)
            .ok_or_else(|| not_added("this computer can't open the conversation's key".into()))?;
        let key = EpochKey::generate().map_err(|e| not_added(e.to_string()))?;
        let epoch = Epoch::change(
            &c.id,
            c.epoch + 1,
            &key,
            &current,
            &keys,
            &compliance.key,
            (&member.key, &member.secret),
            (
                after.clone(),
                MemberOp::Add {
                    people: add.clone(),
                },
            ),
        )
        .map_err(|e| not_added(e.to_string()))?;
        connection
            .link
            .rotate(&RotateEpoch {
                device: connection.device.clone(),
                epoch: epoch.clone(),
            })
            .map_err(|e| not_added(e.to_string()))?;
        {
            let _write = self.handle.lock();
            let mut store = self.handle.load(me);
            if let Some(kept) = store.conversations.get_mut(&c.id) {
                kept.members.clone_from(&after);
                kept.epoch = epoch.body.number;
            }
            store.epochs.entry(c.id.clone()).or_default().push(epoch);
            for k in keys {
                store.keys.entry(k.id.clone()).or_insert(k);
            }
            self.handle.save(me, &store).map_err(failed)?;
        }
        self.handle.changed(me, c.kind);
        let names: Vec<String> = add.iter().map(|l| self.name(l)).collect();
        Ok(ActionOutput {
            text: format!(
                "Added {} to {title}. They can read its history.",
                names.join(", ")
            ),
            data: json!({ "conversation": c.id, "added": add }),
            changes: vec![Change {
                kind: ChangeKind::Sent,
                target: format!("People added to {title}"),
                reversible: false,
                paths: Vec::new(),
            }],
        })
    }

    /// Checks an input before its preview or its call.
    pub(crate) fn check(&self, action: &str, input: &Value) -> Result<(), ActionError> {
        match action {
            REACT => {
                let i: ReactInput = parse_input(input)?;
                check_ids(&i.conversation, Some(&i.message))?;
                check_emoji(&i.emoji)
            }
            EDIT => {
                let i: EditInput = parse_input(input)?;
                check_ids(&i.conversation, Some(&i.message))?;
                if i.text.trim().is_empty() {
                    return Err(invalid("a message has some text; chat.delete deletes one"));
                }
                if i.text.chars().count() > MAX_TEXT_CHARS {
                    return Err(invalid(format!(
                        "a message is at most {MAX_TEXT_CHARS} characters"
                    )));
                }
                Ok(())
            }
            DELETE => {
                let i: DeleteInput = parse_input(input)?;
                check_ids(&i.conversation, Some(&i.message))
            }
            PIN => {
                let i: PinInput = parse_input(input)?;
                check_ids(&i.conversation, Some(&i.message))
            }
            TOPIC => {
                let i: TopicInput = parse_input(input)?;
                check_ids(&i.conversation, None)?;
                if i.topic.chars().count() > MAX_TOPIC_CHARS
                    || i.topic.chars().any(char::is_control)
                {
                    return Err(invalid(format!(
                        "a topic is one line of at most {MAX_TOPIC_CHARS} characters"
                    )));
                }
                Ok(())
            }
            SEARCH => {
                let i: SearchInput = parse_input(input)?;
                let n = i.query.trim().chars().count();
                if !(2..=MAX_QUERY_CHARS).contains(&n) {
                    return Err(invalid(format!(
                        "search for 2 to {MAX_QUERY_CHARS} characters"
                    )));
                }
                if let Some(c) = &i.conversation {
                    check_ids(c, None)?;
                }
                Ok(())
            }
            DRAFT => {
                let i: DraftInput = parse_input(input)?;
                check_ids(&i.conversation, i.thread.as_deref())?;
                if i.text.chars().count() > MAX_TEXT_CHARS {
                    return Err(invalid(format!(
                        "a draft is at most {MAX_TEXT_CHARS} characters"
                    )));
                }
                Ok(())
            }
            NOTIFY => {
                let i: NotifyInput = parse_input(input)?;
                check_ids(&i.conversation, None)?;
                if !LEVELS.contains(&i.level.as_str()) {
                    return Err(invalid("a level is all, mentions or none"));
                }
                Ok(())
            }
            MEMBERS => {
                let i: MembersInput = parse_input(input)?;
                check_ids(&i.conversation, None)
            }
            other => Err(invalid(format!("chat has no action {other}"))),
        }
    }

    pub(crate) fn invoke_everyday(
        &self,
        ctx: &TaskContext,
        action: &str,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        self.check(action, input)?;
        match action {
            REACT => self.react(ctx, parse_input(input)?),
            EDIT => self.edit(ctx, parse_input(input)?),
            DELETE => self.delete(ctx, parse_input(input)?),
            PIN => self.pin(ctx, parse_input(input)?),
            TOPIC => {
                let i: TopicInput = parse_input(input)?;
                let me = &ctx.principal.on_behalf_of.name;
                let store = self.handle.load(me);
                if self.mine(&store, me, &i.conversation)?.kind != ConversationKind::Channel {
                    return Err(invalid("only channels have topics"));
                }
                self.topic(ctx, i)
            }
            SEARCH => self.search(ctx, parse_input(input)?),
            DRAFT => self.draft(ctx, parse_input(input)?),
            NOTIFY => self.notify(ctx, parse_input(input)?),
            MEMBERS => self.add_members(ctx, parse_input(input)?),
            other => Err(invalid(format!("chat has no action {other}"))),
        }
    }

    /// What the person is asked, or what the audit says; never message
    /// text in a summary or a target.
    pub(crate) fn preview_everyday(
        &self,
        ctx: &TaskContext,
        action: &str,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        self.check(action, input)?;
        let me = &ctx.principal.on_behalf_of.name;
        let store = self.handle.load(me);
        let member = self.handle.existing(me);
        let titled = |id: &str| -> Result<(Conversation, String), ActionError> {
            let c = self.mine(&store, me, id)?;
            let title = self.title(&store, member.as_ref(), me, &c);
            Ok((c, title))
        };
        // The message's text, for the review list only (DA-12).
        let quote = |c: &Conversation, id: &str| -> Option<(String, String)> {
            let member = member.as_ref()?;
            let folded = self.folded(&store, member, me, c);
            let m = folded.get(id)?;
            Some((self.name(&m.sender), clip(&m.text, crate::ITEM_CHARS)))
        };
        let preview = |summary: String, title: String, items: Vec<String>| Preview {
            summary,
            targets: vec![title],
            items,
            ..Preview::default()
        };
        Ok(match action {
            REACT => {
                let i: ReactInput = parse_input(input)?;
                let (c, title) = titled(&i.conversation)?;
                let (who, text) = quote(&c, &i.message).unwrap_or_default();
                let verb = if i.on { "React" } else { "Take back" };
                preview(
                    format!("{verb} {} on {who}'s message in {title}", i.emoji),
                    title,
                    vec![format!("{who}: {text}")],
                )
            }
            EDIT => {
                let i: EditInput = parse_input(input)?;
                let (c, title) = titled(&i.conversation)?;
                let (_, was) = quote(&c, &i.message).unwrap_or_default();
                preview(
                    format!("Edit your message in {title}"),
                    title,
                    vec![
                        format!("Was: {was}"),
                        format!("Now: {}", clip(&i.text, crate::ITEM_CHARS)),
                    ],
                )
            }
            DELETE => {
                let i: DeleteInput = parse_input(input)?;
                let (c, title) = titled(&i.conversation)?;
                let (_, text) = quote(&c, &i.message).unwrap_or_default();
                preview(format!("Delete your message in {title}"), title, vec![text])
            }
            PIN => {
                let i: PinInput = parse_input(input)?;
                let (c, title) = titled(&i.conversation)?;
                let (who, text) = quote(&c, &i.message).unwrap_or_default();
                let verb = if i.pinned { "Pin" } else { "Unpin" };
                preview(
                    format!("{verb} {who}'s message in {title}"),
                    title,
                    vec![format!("{who}: {text}")],
                )
            }
            TOPIC => {
                let i: TopicInput = parse_input(input)?;
                let (_, title) = titled(&i.conversation)?;
                preview(
                    format!("Set {title}'s topic"),
                    title,
                    vec![display_line(i.topic.trim()).into_owned()],
                )
            }
            SEARCH => {
                let i: SearchInput = parse_input(input)?;
                let title = match &i.conversation {
                    Some(id) => titled(id)?.1,
                    None => "Chat".into(),
                };
                preview(format!("Search {title}"), title, Vec::new())
            }
            DRAFT => {
                let i: DraftInput = parse_input(input)?;
                let (_, title) = titled(&i.conversation)?;
                preview(format!("Keep a draft in {title}"), title, Vec::new())
            }
            NOTIFY => {
                let i: NotifyInput = parse_input(input)?;
                let (_, title) = titled(&i.conversation)?;
                preview(
                    format!("Notifications for {title}: {}", i.level),
                    title,
                    Vec::new(),
                )
            }
            MEMBERS => {
                let i: MembersInput = parse_input(input)?;
                self.check_people(me, &i.add)?;
                let (_, title) = titled(&i.conversation)?;
                let names: Vec<String> = i
                    .add
                    .iter()
                    .map(|l| display_line(&self.name(l)).into_owned())
                    .collect();
                preview(
                    format!("Add {} to {title}", plural(names.len(), "person", "people")),
                    title,
                    names
                        .iter()
                        .map(|n| format!("{n} (reads its history)"))
                        .collect(),
                )
            }
            other => return Err(invalid(format!("chat has no action {other}"))),
        })
    }
}

/// The actions' definitions.
pub(crate) fn defs() -> Vec<ActionDef> {
    let conversation =
        json!({ "type": "string", "description": "A conversation id from chat.list (cv-…)" });
    let message = json!({ "type": "string", "description": "A message id from chat.read" });
    let def =
        |name: &str, title: &str, description: &str, schema: Value, side_effect, tier, ask| {
            ActionDef {
                name: name.parse().expect("static name"),
                title: title.into(),
                description: description.into(),
                input_schema: schema,
                side_effect,
                default_tier: tier,
                ask,
            }
        };
    vec![
        def(
            REACT,
            "React to a chat message",
            "Adds an emoji reaction to a message, or takes one back (on: false). Others see it.",
            json!({
                "type": "object",
                "properties": {
                    "conversation": conversation, "message": message,
                    "emoji": { "type": "string", "maxLength": 32 },
                    "on": { "type": "boolean", "default": true }
                },
                "required": ["conversation", "message", "emoji"],
                "additionalProperties": false
            }),
            SideEffect::Irreversible,
            Tier::Ask,
            AskLabels::new("Review reaction", "Approve and react"),
        ),
        def(
            EDIT,
            "Edit a chat message",
            "Changes the text of one of the person's messages; an agent may edit only messages it \
             posted. Everyone sees it's edited; anyone who read the old text may remember it.",
            json!({
                "type": "object",
                "properties": {
                    "conversation": conversation, "message": message,
                    "text": { "type": "string", "maxLength": MAX_TEXT_CHARS }
                },
                "required": ["conversation", "message", "text"],
                "additionalProperties": false
            }),
            SideEffect::Irreversible,
            Tier::Ask,
            AskLabels::new("Review edit", "Approve and edit"),
        ),
        def(
            DELETE,
            "Delete a chat message",
            "Deletes one of the person's messages for everyone; an agent may delete only messages \
             it posted. Anyone who read it may remember it, and IT's records keep it.",
            json!({
                "type": "object",
                "properties": { "conversation": conversation, "message": message },
                "required": ["conversation", "message"],
                "additionalProperties": false
            }),
            SideEffect::Irreversible,
            Tier::Ask,
            AskLabels::new("Review delete", "Approve and delete"),
        ),
        def(
            PIN,
            "Pin a chat message",
            "Pins a message in a conversation for everyone in it, or unpins it (pinned: false).",
            json!({
                "type": "object",
                "properties": {
                    "conversation": conversation, "message": message,
                    "pinned": { "type": "boolean", "default": true }
                },
                "required": ["conversation", "message"],
                "additionalProperties": false
            }),
            SideEffect::Irreversible,
            Tier::Ask,
            AskLabels::new("Review pin", "Approve and pin"),
        ),
        def(
            TOPIC,
            "Set a channel's topic",
            "Sets the topic everyone in a channel sees; empty clears it.",
            json!({
                "type": "object",
                "properties": {
                    "conversation": conversation,
                    "topic": { "type": "string", "maxLength": MAX_TOPIC_CHARS }
                },
                "required": ["conversation", "topic"],
                "additionalProperties": false
            }),
            SideEffect::Irreversible,
            Tier::Ask,
            AskLabels::new("Review topic", "Approve and set"),
        ),
        def(
            SEARCH,
            "Search chat",
            "Finds messages containing some text, across the person's conversations or in one, \
             newest first (up to 50). It runs on this computer; the org's server can't read chat.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 2, "maxLength": MAX_QUERY_CHARS },
                    "conversation": conversation,
                    "limit": { "type": "integer", "minimum": 1, "maximum": SEARCH_MAX }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            SideEffect::None,
            Tier::Auto,
            AskLabels::default(),
        ),
        def(
            DRAFT,
            "Keep a chat draft",
            "Keeps what the person is writing in a conversation or thread until they send it; \
             empty text clears it. Chat's own.",
            json!({
                "type": "object",
                "properties": {
                    "conversation": conversation,
                    "thread": message,
                    "text": { "type": "string", "maxLength": MAX_TEXT_CHARS }
                },
                "required": ["conversation", "text"],
                "additionalProperties": false
            }),
            SideEffect::None,
            Tier::Auto,
            AskLabels::default(),
        ),
        def(
            MEMBERS,
            "Add people to a chat channel or group",
            "Adds people (by login) to a channel or group the person is in. They read its whole \
             history, as in Slack, and everyone in it sees they were added.",
            json!({
                "type": "object",
                "properties": {
                    "conversation": conversation,
                    "add": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "People's logins, from people.search" }
                },
                "required": ["conversation", "add"],
                "additionalProperties": false
            }),
            SideEffect::Irreversible,
            Tier::Ask,
            AskLabels::new("Review people", "Approve and add"),
        ),
        def(
            NOTIFY,
            "Choose what to hear of in a conversation",
            "Every message, only mentions, or nothing. Chat's own.",
            json!({
                "type": "object",
                "properties": {
                    "conversation": conversation,
                    "level": { "type": "string", "enum": LEVELS }
                },
                "required": ["conversation", "level"],
                "additionalProperties": false
            }),
            SideEffect::None,
            Tier::Auto,
            AskLabels::default(),
        ),
    ]
}
