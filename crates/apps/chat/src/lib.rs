//! The `chat` app's actions (CF-91, ADR 0030): the org's chat, sealed end
//! to end between its computers, read and written here through glassd.
//!
//! Chat and agents go through the same four actions. Reading is quiet
//! (Auto); posting and making a channel leave the machine, so they ask
//! (Ask) unless the person typed them in Chat, whose `[[app]]` tiers lower
//! them. What an agent posts carries "by <agent> for <person>" inside the
//! seal, from the task's principal, never from the request (AU-10).
//! Previews and audit records name conversations, never message text.

mod store;

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, parse_input,
};
use glass_api::{AskLabels, ChangeKind, display_line};
use glass_crypto::EpochKey;
use glass_managed::DirectoryHandle;
use glass_managed::chat::{
    Body, Conversation, ConversationKind, CreateConversation, Epoch, Header, LookupKeys,
    MAX_MEMBERS, MAX_TEXT_CHARS, Message, PostMessage, is_conversation_id, is_message_id,
    new_conversation_id, new_message_id, seal_name,
};
use glass_types::{ActionName, SideEffect, Tier, Timestamp};
use serde::Deserialize;
use serde_json::{Value, json};

pub use store::{ChatFailure, ChatHandle, Notice};
use store::{Compliance, Connection, Member, Store, plain_login};

pub const LIST: &str = "chat.list";
pub const READ: &str = "chat.read";
pub const POST: &str = "chat.post";
pub const CREATE: &str = "chat.create";
/// The Chat app's id: what the person types there carries no by-line.
pub const APP: &str = "chat";

/// Messages in one `chat.post`.
const MAX_POSTS: usize = 20;
const READ_DEFAULT: usize = 50;
const READ_MAX: usize = 200;
const MAX_NAME_CHARS: usize = 80;
/// How much of a message a review line shows (DA-12).
const ITEM_CHARS: usize = 300;

#[derive(Debug, Clone)]
pub struct ChatProvider {
    handle: ChatHandle,
    directory: DirectoryHandle,
}

impl ChatProvider {
    pub fn new(handle: ChatHandle, directory: DirectoryHandle) -> Self {
        Self { handle, directory }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    conversation: String,
    /// Messages before this one (an id from an earlier read).
    #[serde(default)]
    before: Option<String>,
    /// Messages after this one.
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PostInput {
    posts: Vec<Post>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    /// A conversation the person is in, from chat.list.
    #[serde(default)]
    conversation: Option<String>,
    /// Or people, by login: their direct message or group, made if new.
    #[serde(default)]
    to: Vec<String>,
    text: String,
    #[serde(default)]
    reply_to: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    name: String,
    people: Vec<String>,
}

/// Where a post goes.
enum Target {
    Existing(Conversation),
    New(Vec<String>),
}

fn invalid(m: impl Into<String>) -> ActionError {
    ActionError::InvalidInput(m.into())
}

fn failed(e: ChatFailure) -> ActionError {
    ActionError::Failed(e.to_string())
}

fn clip(text: &str, chars: usize) -> String {
    let line = display_line(text);
    match line.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.into_owned(),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `HH:MM` of an RFC 3339 time.
fn clock(at: &str) -> &str {
    at.get(11..16).unwrap_or(at)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// `me` and `people`, each once, sorted: a conversation's members.
fn members(me: &str, people: &[String]) -> Vec<String> {
    let mut all: Vec<String> = people.iter().cloned().chain([me.to_owned()]).collect();
    all.sort();
    all.dedup();
    all
}

impl ChatProvider {
    /// A person's name from the directory, or their login.
    fn name(&self, login: &str) -> String {
        self.directory
            .person(login)
            .map(|p| p.name)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| login.to_owned())
    }

    fn people_title(&self, me: &str, members: &[String]) -> String {
        let others: Vec<String> = members
            .iter()
            .filter(|m| *m != me)
            .map(|m| self.name(m))
            .collect();
        if others.is_empty() {
            "Just you".into()
        } else {
            others.join(", ")
        }
    }

    /// What a conversation is called, as one display line.
    fn title(&self, store: &Store, member: Option<&Member>, me: &str, c: &Conversation) -> String {
        let title = match c.kind {
            ConversationKind::Channel => format!(
                "#{}",
                member
                    .and_then(|m| store.name(m, c))
                    .unwrap_or_else(|| "channel".into())
            ),
            _ => self.people_title(me, &c.members),
        };
        display_line(&title).into_owned()
    }

    /// Checks people named by login: plain, in the directory when there
    /// is one, and not too many.
    fn check_people(&self, me: &str, people: &[String]) -> Result<(), ActionError> {
        if people.is_empty() || people.len() >= MAX_MEMBERS {
            return Err(invalid(format!("name 1 to {} people", MAX_MEMBERS - 1)));
        }
        let directory = self.directory.current();
        for login in people {
            if !plain_login(login) {
                return Err(invalid(format!("{login:?} is not a login")));
            }
            if let Some(d) = &directory
                && d.release.person(login).is_none()
            {
                return Err(invalid(format!(
                    "{login} isn't in your organization's directory; people.search finds people"
                )));
            }
        }
        if people.iter().all(|p| p == me) {
            return Err(invalid("name someone besides yourself"));
        }
        Ok(())
    }

    fn check_post(&self, me: &str, post: &Post) -> Result<(), ActionError> {
        match (&post.conversation, post.to.is_empty()) {
            (Some(id), true) if is_conversation_id(id) => {}
            (Some(id), true) => return Err(invalid(format!("{id:?} is not a conversation id"))),
            (None, false) => self.check_people(me, &post.to)?,
            _ => {
                return Err(invalid(
                    "each post has a conversation or people (to), not both",
                ));
            }
        }
        if post.text.trim().is_empty() {
            return Err(invalid("a message has some text"));
        }
        if post.text.chars().count() > MAX_TEXT_CHARS {
            return Err(invalid(format!(
                "a message is at most {MAX_TEXT_CHARS} characters"
            )));
        }
        if let Some(r) = &post.reply_to
            && !is_message_id(r)
        {
            return Err(invalid(format!("{r:?} is not a message id")));
        }
        Ok(())
    }

    fn target(&self, store: &Store, me: &str, post: &Post) -> Result<Target, ActionError> {
        if let Some(id) = &post.conversation {
            return store
                .conversations
                .get(id)
                .filter(|c| c.members.iter().any(|m| m == me))
                .cloned()
                .map(Target::Existing)
                .ok_or_else(|| {
                    ActionError::NotFound(format!(
                        "you aren't in a conversation {id}; chat.list shows yours"
                    ))
                });
        }
        let want = members(me, &post.to);
        Ok(store
            .conversations
            .values()
            .find(|c| c.kind != ConversationKind::Channel && c.members == want)
            .cloned()
            .map_or(Target::New(want), Target::Existing))
    }

    fn target_title(
        &self,
        store: &Store,
        member: Option<&Member>,
        me: &str,
        target: &Target,
    ) -> String {
        match target {
            Target::Existing(c) => self.title(store, member, me, c),
            Target::New(members) => display_line(&self.people_title(me, members)).into_owned(),
        }
    }

    /// A new conversation of `members`, its first epoch sealed to each of
    /// their computers and to IT, registered with the console and kept.
    #[allow(clippy::too_many_arguments)]
    fn open_conversation(
        &self,
        connection: &Connection,
        compliance: &Compliance,
        member: &Member,
        me: &str,
        members: Vec<String>,
        kind: ConversationKind,
        name: Option<&str>,
    ) -> Result<Conversation, ChatFailure> {
        let found = connection
            .link
            .lookup(&LookupKeys {
                logins: members.clone(),
            })
            .map_err(ChatFailure::Link)?;
        if found.compliance != compliance.hex {
            return Err(ChatFailure::Local(
                "your organization's server offered a compliance key that isn't the one IT signed, \
                 so nothing was sealed"
                    .into(),
            ));
        }
        let keys: Vec<_> = found
            .keys
            .into_iter()
            .filter(|k| members.contains(&k.login) && k.keys().is_ok())
            .collect();
        let missing: Vec<String> = members
            .iter()
            .filter(|login| !keys.iter().any(|k| &k.login == *login))
            .map(|login| self.name(login))
            .collect();
        if !missing.is_empty() {
            return Err(ChatFailure::Local(format!(
                "{} {} opened Chat yet, so there's no key to seal to; ask them to open Chat once",
                missing.join(", "),
                if missing.len() == 1 {
                    "hasn't"
                } else {
                    "haven't"
                },
            )));
        }
        if !keys.iter().any(|k| k.id == member.key.id) {
            return Err(ChatFailure::Local(
                "this computer's chat key isn't registered with your organization's server".into(),
            ));
        }
        let id = new_conversation_id()?;
        let epoch_key = EpochKey::generate()?;
        let epoch = Epoch::make(
            &id,
            1,
            &epoch_key,
            None,
            &keys,
            &compliance.key,
            (&member.key, &member.secret),
        )?;
        let sealed = name.map(|n| seal_name(&epoch_key, &id, 1, n)).transpose()?;
        let conversation = Conversation {
            id: id.clone(),
            kind,
            members,
            created_by: me.to_owned(),
            name: sealed,
            epoch: 1,
        };
        connection
            .link
            .create(&CreateConversation {
                device: connection.device.clone(),
                conversation: conversation.clone(),
                epoch: epoch.clone(),
            })
            .map_err(ChatFailure::Link)?;
        {
            let _write = self.handle.lock();
            let mut store = self.handle.load(me);
            store.conversations.insert(id.clone(), conversation.clone());
            store.epochs.entry(id).or_default().push(epoch);
            for key in keys {
                store.keys.entry(key.id.clone()).or_insert(key);
            }
            self.handle.save(me, &store)?;
        }
        self.handle.changed(me);
        Ok(conversation)
    }

    /// Seals and posts one message. Its conversation, title, id and
    /// sequence number.
    fn post_one(
        &self,
        connection: &Connection,
        compliance: &Compliance,
        member: &Member,
        me: &str,
        post: &Post,
        attribution: Option<String>,
    ) -> Result<(Conversation, String, String, u64), ChatFailure> {
        let target = self
            .target(&self.handle.load(me), me, post)
            .map_err(|e| ChatFailure::Local(e.to_string()))?;
        let conversation = match target {
            Target::Existing(c) => c,
            Target::New(members) => {
                let kind = if members.len() == 2 {
                    ConversationKind::Direct
                } else {
                    ConversationKind::Group
                };
                self.open_conversation(connection, compliance, member, me, members, kind, None)?
            }
        };
        let store = self.handle.load(me);
        let title = self.title(&store, Some(member), me, &conversation);
        let key = store
            .epoch_key(member, &conversation.id, conversation.epoch)
            .ok_or_else(|| ChatFailure::Local(format!("this computer can't open {title}'s key")))?;
        let header = Header {
            v: 1,
            org: connection.org.clone(),
            conversation: conversation.id.clone(),
            epoch: conversation.epoch,
            id: new_message_id(now_ms())?,
            sender: me.to_owned(),
            key: member.key.id.clone(),
        };
        let body = Body {
            text: post.text.clone(),
            attribution,
            reply_to: post.reply_to.clone(),
            sent_at: Timestamp::now().to_string(),
        };
        let mut message = Message::seal(&header, &body, &key, &member.secret)?;
        let seq = connection
            .link
            .post(&PostMessage {
                device: connection.device.clone(),
                message: message.clone(),
            })
            .map_err(ChatFailure::Link)?;
        message.seq = seq;
        message.at = Some(Timestamp::now().to_string());
        {
            let _write = self.handle.lock();
            let mut store = self.handle.load(me);
            store.keep(&conversation.id, message);
            self.handle.save(me, &store)?;
        }
        self.handle.changed(me);
        Ok((conversation, title, header.id, seq))
    }

    /// The person's keys. Chat makes and registers them the first time
    /// the person opens it, so others can reach them; an agent or another
    /// app uses them only once that has happened, and never makes them.
    fn member(&self, ctx: &TaskContext) -> Result<Option<Member>, ChatFailure> {
        let me = &ctx.principal.on_behalf_of.name;
        if ctx.principal.agent.as_str() == APP {
            return self.handle.member(me).map(Some);
        }
        self.handle.connection()?;
        Ok(self.handle.existing(me))
    }

    fn not_opened(me: &str) -> String {
        format!(
            "{me} hasn't opened Chat on this computer yet; Chat makes their keys the first time it opens"
        )
    }

    fn list(&self, ctx: &TaskContext) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let Some(member) = self.member(ctx).map_err(failed)? else {
            return Ok(ActionOutput {
                text: format!("No conversations: {}.", Self::not_opened(me)),
                data: json!({ "conversations": [], "me": me, "opened": false }),
                changes: Vec::new(),
            });
        };
        let store = self.handle.load(me);
        let mut rows: Vec<(u64, Value, String)> = store
            .conversations
            .values()
            .filter(|c| c.members.iter().any(|m| m == me))
            .map(|c| {
                let messages = store.messages.get(&c.id).map_or(&[][..], Vec::as_slice);
                let seen = store.seen.get(&c.id).copied().unwrap_or(0);
                let unread = messages
                    .iter()
                    .filter(|m| m.seq > seen && m.header().is_ok_and(|h| h.sender != *me))
                    .count();
                let last = messages.last();
                let title = self.title(&store, Some(&member), me, c);
                let line = format!(
                    "- {title} [{}]{}",
                    c.id,
                    if unread > 0 {
                        format!(", {unread} unread")
                    } else {
                        String::new()
                    }
                );
                let view = json!({
                    "id": c.id,
                    "kind": c.kind,
                    "title": title,
                    "people": c.members.iter().map(|m| json!({ "login": m, "name": self.name(m) })).collect::<Vec<_>>(),
                    "unread": unread,
                    "last_at": last.and_then(|m| m.at.clone()),
                    "last_from": last.and_then(|m| m.header().ok()).map(|h| self.name(&h.sender)),
                });
                (last.map_or(0, |m| m.seq), view, line)
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        let unread: u64 = rows.iter().filter_map(|r| r.1["unread"].as_u64()).sum();
        let mut text = format!(
            "{}; {unread} unread.",
            plural(rows.len(), "conversation", "conversations")
        );
        for row in &rows {
            text.push('\n');
            text.push_str(&row.2);
        }
        Ok(ActionOutput {
            text,
            data: json!({
                "conversations": rows.into_iter().map(|r| r.1).collect::<Vec<_>>(),
                "me": me,
                "compliance": self.handle.fingerprint(),
            }),
            changes: Vec::new(),
        })
    }

    fn read(&self, ctx: &TaskContext, input: ReadInput) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let limit = input.limit.unwrap_or(READ_DEFAULT).clamp(1, READ_MAX);
        let member = self
            .member(ctx)
            .map_err(failed)?
            .ok_or_else(|| ActionError::NotFound(Self::not_opened(me)))?;
        let store = self.handle.load(me);
        let conversation = store
            .conversations
            .get(&input.conversation)
            .filter(|c| c.members.iter().any(|m| m == me))
            .ok_or_else(|| {
                ActionError::NotFound(format!(
                    "you aren't in a conversation {}; chat.list shows yours",
                    input.conversation
                ))
            })?;
        let title = self.title(&store, Some(&member), me, conversation);
        let all = store
            .messages
            .get(&conversation.id)
            .map_or(&[][..], Vec::as_slice);
        let index = |id: &str| {
            all.iter()
                .position(|m| m.header().is_ok_and(|h| h.id == id))
        };
        let window: &[Message] = match (&input.before, &input.after) {
            (Some(id), None) => {
                let end =
                    index(id).ok_or_else(|| ActionError::NotFound(format!("no message {id}")))?;
                &all[end.saturating_sub(limit)..end]
            }
            (None, Some(id)) => {
                let start =
                    index(id).ok_or_else(|| ActionError::NotFound(format!("no message {id}")))? + 1;
                &all[start..(start + limit).min(all.len())]
            }
            (None, None) => &all[all.len().saturating_sub(limit)..],
            (Some(_), Some(_)) => return Err(invalid("give before or after, not both")),
        };
        let mut keys: BTreeMap<u32, Option<EpochKey>> = BTreeMap::new();
        let mut views = Vec::with_capacity(window.len());
        let mut lines = Vec::with_capacity(window.len());
        let mut sealed = 0;
        for message in window {
            let Ok(header) = message.header() else {
                sealed += 1;
                continue;
            };
            let key = keys
                .entry(header.epoch)
                .or_insert_with(|| store.epoch_key(&member, &conversation.id, header.epoch));
            let Some(body) = key.as_ref().and_then(|k| message.open(k).ok()) else {
                sealed += 1;
                continue;
            };
            let at = message.at.clone().unwrap_or_else(|| body.sent_at.clone());
            let name = self.name(&header.sender);
            lines.push(format!(
                "[{}] {name}{}: {}",
                clock(&at),
                body.attribution
                    .as_deref()
                    .map(|a| format!(" ({a})"))
                    .unwrap_or_default(),
                body.text
            ));
            views.push(json!({
                "id": header.id,
                "seq": message.seq,
                "sender": header.sender,
                "sender_name": name,
                "mine": header.sender == *me,
                "at": at,
                "text": body.text,
                "attribution": body.attribution,
                "reply_to": body.reply_to,
            }));
        }
        // What the person reads in Chat is read; an agent reading for
        // them leaves it unread.
        if ctx.principal.agent.as_str() == APP
            && let Some(newest) = window.last()
        {
            let _write = self.handle.lock();
            let mut store = self.handle.load(me);
            let seen = store.seen.entry(conversation.id.clone()).or_insert(0);
            if newest.seq > *seen {
                *seen = newest.seq;
                self.handle.save(me, &store).map_err(failed)?;
            }
        }
        let mut text = format!(
            "{title}: {}{}.",
            plural(views.len(), "message", "messages"),
            if sealed > 0 {
                format!(" ({sealed} this computer can't open)")
            } else {
                String::new()
            }
        );
        for line in lines {
            text.push('\n');
            text.push_str(&line);
        }
        Ok(ActionOutput {
            text,
            data: json!({
                "conversation": { "id": conversation.id, "kind": conversation.kind, "title": title },
                "messages": views,
                "more_before": window.first().is_some_and(|m| !std::ptr::eq(m, &all[0])),
            }),
            changes: Vec::new(),
        })
    }

    fn post(&self, ctx: &TaskContext, input: PostInput) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let connection = self.handle.connection().map_err(failed)?;
        let compliance = self.handle.compliance().map_err(failed)?;
        let member = self
            .member(ctx)
            .and_then(|m| m.ok_or_else(|| ChatFailure::NotSetUp(Self::not_opened(me))))
            .map_err(|e| ActionError::Failed(format!("Not sent: {e}.")))?;
        // AU-10: an agent's message says who wrote it, inside the seal.
        let attribution = (ctx.principal.agent.as_str() != APP)
            .then(|| format!("by {} for {me}", ctx.principal.agent));
        let mut posted = Vec::new();
        let mut changes = Vec::new();
        for post in &input.posts {
            match self.post_one(
                &connection,
                &compliance,
                &member,
                me,
                post,
                attribution.clone(),
            ) {
                Ok((conversation, title, id, seq)) => {
                    posted.push(json!({ "conversation": conversation.id, "title": title, "id": id, "seq": seq }));
                    changes.push(Change {
                        kind: ChangeKind::Sent,
                        target: format!("Message in {title}"),
                        reversible: false,
                        paths: Vec::new(),
                    });
                }
                Err(e) if posted.is_empty() => {
                    return Err(ActionError::Failed(format!("Not sent: {e}.")));
                }
                Err(e) => {
                    return Ok(ActionOutput {
                        text: format!(
                            "Posted {} of {}; the rest weren't sent: {e}.",
                            posted.len(),
                            input.posts.len()
                        ),
                        data: json!({ "posted": posted }),
                        changes,
                    });
                }
            }
        }
        let titles: Vec<&str> = posted.iter().filter_map(|p| p["title"].as_str()).collect();
        Ok(ActionOutput {
            text: format!(
                "Posted {} in {}.",
                plural(posted.len(), "message", "messages"),
                titles.join(", ")
            ),
            data: json!({ "posted": posted }),
            changes,
        })
    }

    fn create(&self, ctx: &TaskContext, input: CreateInput) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        let connection = self.handle.connection().map_err(failed)?;
        let compliance = self.handle.compliance().map_err(failed)?;
        let member = self
            .member(ctx)
            .and_then(|m| m.ok_or_else(|| ChatFailure::NotSetUp(Self::not_opened(me))))
            .map_err(|e| ActionError::Failed(format!("Not created: {e}.")))?;
        let name = input.name.trim().trim_start_matches('#');
        let conversation = self
            .open_conversation(
                &connection,
                &compliance,
                &member,
                me,
                members(me, &input.people),
                ConversationKind::Channel,
                Some(name),
            )
            .map_err(|e| ActionError::Failed(format!("Not created: {e}.")))?;
        let title = display_line(&format!("#{name}")).into_owned();
        Ok(ActionOutput {
            text: format!(
                "Created {title} with {}.",
                plural(conversation.members.len(), "person", "people")
            ),
            data: json!({ "conversation": { "id": conversation.id, "title": title } }),
            changes: vec![Change {
                kind: ChangeKind::Created,
                target: format!("Channel {title}"),
                reversible: false,
                paths: Vec::new(),
            }],
        })
    }

    fn check_create(&self, me: &str, input: &CreateInput) -> Result<String, ActionError> {
        let name = input.name.trim().trim_start_matches('#');
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(invalid(format!(
                "a channel's name is 1 to {MAX_NAME_CHARS} characters"
            )));
        }
        self.check_people(me, &input.people)?;
        Ok(name.to_owned())
    }
}

impl ActionProvider for ChatProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let conversation =
            json!({ "type": "string", "description": "A conversation id from chat.list (cv-…)" });
        vec![
            ActionDef {
                name: LIST.parse().expect("static name"),
                title: "Read your chat conversations".into(),
                description: "The person's conversations in the org's chat: channels, direct \
                              messages and groups, with who is in each, unread counts and \
                              last activity, newest first."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: READ.parse().expect("static name"),
                title: "Read a chat conversation".into(),
                description: "Messages in one of the person's conversations, oldest first: the \
                              newest 50 by default (up to 200), or those before or after a \
                              message. Each has its sender, time, text and, for an agent's, \
                              who posted it for whom."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "conversation": conversation,
                        "before": { "type": "string", "description": "A message id: messages before it" },
                        "after": { "type": "string", "description": "A message id: messages after it" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": READ_MAX }
                    },
                    "required": ["conversation"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: POST.parse().expect("static name"),
                title: "Post in chat".into(),
                description: "Post 1 to 20 messages, each in a conversation the person is in or \
                              to people by login (their direct message or group, made if new). \
                              Messages are sealed end to end and say they came from you for \
                              the person. They can't be taken back."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "posts": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": MAX_POSTS,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "conversation": conversation,
                                    "to": { "type": "array", "items": { "type": "string" }, "description": "People's logins, from people.search" },
                                    "text": { "type": "string", "maxLength": MAX_TEXT_CHARS },
                                    "reply_to": { "type": "string", "description": "The id of the message this answers" }
                                },
                                "required": ["text"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["posts"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review messages", "Approve and post"),
            },
            ActionDef {
                name: CREATE.parse().expect("static name"),
                title: "Make a chat channel".into(),
                description: "A named channel with the person and the people given (by login). \
                              Its name is sealed like its messages."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "maxLength": MAX_NAME_CHARS },
                        "people": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                    },
                    "required": ["name", "people"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review channel", "Approve and create"),
            },
        ]
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                Ok(Preview {
                    summary: "Read your chat conversations".into(),
                    targets: vec!["Chat".into()],
                    ..Preview::default()
                })
            }
            READ => {
                let input: ReadInput = parse_input(input)?;
                let store = self.handle.load(me);
                let member = self.handle.existing(me);
                let conversation = store
                    .conversations
                    .get(&input.conversation)
                    .filter(|c| c.members.iter().any(|m| m == me))
                    .ok_or_else(|| {
                        ActionError::NotFound(format!(
                            "you aren't in a conversation {}; chat.list shows yours",
                            input.conversation
                        ))
                    })?;
                let title = self.title(&store, member.as_ref(), me, conversation);
                Ok(Preview {
                    summary: format!("Read {title}"),
                    targets: vec![title],
                    ..Preview::default()
                })
            }
            POST => {
                let input: PostInput = parse_input(input)?;
                if input.posts.is_empty() || input.posts.len() > MAX_POSTS {
                    return Err(invalid(format!("post 1 to {MAX_POSTS} messages per call")));
                }
                let store = self.handle.load(me);
                let member = self.handle.existing(me);
                let mut targets: Vec<String> = Vec::new();
                let mut items = Vec::new();
                let mut first = None;
                for post in &input.posts {
                    self.check_post(me, post)?;
                    let target = self.target(&store, me, post)?;
                    let title = self.target_title(&store, member.as_ref(), me, &target);
                    items.push(format!("{title}: {}", clip(&post.text, ITEM_CHARS)));
                    let channel = matches!(&target, Target::Existing(c) if c.kind == ConversationKind::Channel);
                    first.get_or_insert((title.clone(), channel));
                    if !targets.contains(&title) {
                        targets.push(title);
                    }
                }
                let summary = match (input.posts.len(), first) {
                    (1, Some((title, true))) => format!("Post in {title}"),
                    (1, Some((title, false))) => format!("Message {title}"),
                    (n, _) => format!("Post {n} messages"),
                };
                Ok(Preview {
                    summary,
                    targets,
                    items,
                    ..Preview::default()
                })
            }
            CREATE => {
                let input: CreateInput = parse_input(input)?;
                let name = self.check_create(me, &input)?;
                let title = display_line(&format!("#{name}")).into_owned();
                let people: Vec<String> = members(me, &input.people)
                    .iter()
                    .map(|login| display_line(&self.name(login)).into_owned())
                    .collect();
                Ok(Preview {
                    summary: format!(
                        "Make {title} with {}",
                        plural(people.len(), "person", "people")
                    ),
                    targets: vec![title],
                    items: people,
                    ..Preview::default()
                })
            }
            other => Err(invalid(format!("chat has no action {other}"))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        let me = &ctx.principal.on_behalf_of.name;
        match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                self.list(ctx)
            }
            READ => self.read(ctx, parse_input(input)?),
            POST => {
                let input: PostInput = parse_input(input)?;
                if input.posts.is_empty() || input.posts.len() > MAX_POSTS {
                    return Err(invalid(format!("post 1 to {MAX_POSTS} messages per call")));
                }
                for post in &input.posts {
                    self.check_post(me, post)?;
                }
                self.post(ctx, input)
            }
            CREATE => {
                let input: CreateInput = parse_input(input)?;
                self.check_create(me, &input)?;
                self.create(ctx, input)
            }
            other => Err(invalid(format!("chat has no action {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use glass_crypto::SecretKey;
    use glass_managed::chat::COMPLIANCE;
    use glass_managed::chat::testing::MemoryLink;
    use glass_managed::directory::Applied;
    use glass_managed::{ChatRelease, DirectoryRelease, Person};

    use super::*;

    struct Org {
        dir: tempfile::TempDir,
        link: Arc<MemoryLink>,
        it: SecretKey,
        directory: DirectoryHandle,
    }

    struct Computer {
        handle: ChatHandle,
        provider: ChatProvider,
        notices: Arc<Mutex<Vec<Notice>>>,
    }

    fn org() -> Org {
        let it = SecretKey::generate().unwrap();
        let directory = DirectoryHandle::new();
        let person = |login: &str, name: &str| Person {
            login: login.into(),
            name: name.into(),
            email: format!("{login}@acme.example"),
            title: String::new(),
            department: String::new(),
        };
        directory.set(Applied {
            version: 1,
            release: DirectoryRelease {
                people: vec![
                    person("dana", "Dana Ruiz"),
                    person("priya", "Priya Shah"),
                    person("sam", "Sam Lee"),
                ],
            },
        });
        Org {
            dir: tempfile::tempdir().unwrap(),
            link: Arc::new(MemoryLink::new(&it.public().unwrap())),
            it,
            directory,
        }
    }

    impl Org {
        /// A managed computer in this org, with IT's Chat release applied.
        fn computer(&self, name: &str) -> Computer {
            let handle = ChatHandle::new(self.dir.path().join(name));
            handle.connect("acme", name, self.link.clone());
            let release = ChatRelease::new(&self.it.public().unwrap());
            handle
                .set_compliance(&release.compliance, &release.fingerprint)
                .unwrap();
            let notices = Arc::new(Mutex::new(Vec::new()));
            let heard = Arc::clone(&notices);
            handle.on_notice(move |n| heard.lock().unwrap().push(n.clone()));
            Computer {
                provider: ChatProvider::new(handle.clone(), self.directory.clone()),
                handle,
                notices,
            }
        }
    }

    fn ctx(login: &str, agent: &str) -> TaskContext {
        let mut ctx = glass_actions::testing::context(Path::new("/nonexistent"));
        ctx.principal.on_behalf_of.name = login.into();
        ctx.principal.agent = agent.parse().unwrap();
        ctx
    }

    impl Computer {
        fn call(
            &self,
            login: &str,
            action: &str,
            input: Value,
        ) -> Result<ActionOutput, ActionError> {
            self.provider
                .invoke(&ctx(login, APP), &action.parse().unwrap(), &input)
        }

        fn preview(&self, login: &str, action: &str, input: Value) -> Preview {
            self.provider
                .preview(&ctx(login, APP), &action.parse().unwrap(), &input)
                .unwrap()
        }
    }

    #[test]
    fn cf91_two_computers_message_each_other_sealed_signed_and_readable_by_it() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        // Priya opens Chat once: her computer makes and registers her key.
        let empty = b.call("priya", LIST, json!({})).unwrap();
        assert!(empty.text.starts_with("0 conversations"), "{}", empty.text);
        assert!(
            matches!(&b.notices.lock().unwrap()[0], Notice::KeyMade { login, .. } if login == "priya")
        );

        let text = "The Northwind terms are in";
        let post = json!({ "posts": [{ "to": ["priya"], "text": text }] });
        let preview = a.preview("dana", POST, post.clone());
        assert_eq!(preview.summary, "Message Priya Shah");
        assert_eq!(preview.targets, ["Priya Shah"]);
        assert_eq!(preview.items, [format!("Priya Shah: {text}")]);
        let sent = a.call("dana", POST, post).unwrap();
        assert_eq!(sent.text, "Posted 1 message in Priya Shah.");
        assert_eq!(sent.changes[0].kind, ChangeKind::Sent);
        assert!(!sent.changes[0].reversible);

        // The console holds ciphertext only.
        let relayed = org.link.messages();
        assert!(!relayed[0].ct.contains(&hex::encode(text)));
        assert!(!relayed[0].header.contains("Northwind"));

        assert!(b.handle.sync_once(0).unwrap());
        let list = b.call("priya", LIST, json!({})).unwrap();
        let row = &list.data["conversations"][0];
        assert_eq!(
            (row["title"].as_str(), row["unread"].as_u64()),
            (Some("Dana Ruiz"), Some(1))
        );
        let id = row["id"].as_str().unwrap().to_owned();
        let read = b
            .call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        let message = &read.data["messages"][0];
        assert_eq!(message["text"], text);
        assert_eq!(message["sender_name"], "Dana Ruiz");
        assert!(message["attribution"].is_null(), "she typed it in Chat");
        assert!(
            read.text.contains(&format!("Dana Ruiz: {text}")),
            "{}",
            read.text
        );
        // Read in Chat, so no longer unread.
        let list = b.call("priya", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["unread"], 0);

        // Priya answers in the same conversation; Dana's computer reads it.
        b.call(
            "priya",
            POST,
            json!({ "posts": [{ "conversation": id, "text": "Thanks, reading now" }] }),
        )
        .unwrap();
        a.handle.sync_once(0).unwrap();
        let read = a.call("dana", READ, json!({ "conversation": id })).unwrap();
        assert_eq!(read.data["messages"][1]["text"], "Thanks, reading now");
        assert_eq!(org.link.messages().len(), 2, "one conversation, two posts");

        // IT's compliance key opens the same conversation (ADR 0030, 4).
        let store = a.handle.load("dana");
        let key = store.epochs[&id][0].open(COMPLIANCE, &org.it).unwrap();
        assert_eq!(relayed[0].open(&key).unwrap().text, text);
    }

    #[test]
    fn cf91_a_record_that_doesnt_check_is_kept_out_and_noticed() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "Real" }] }),
        )
        .unwrap();
        // A console changing a sealed message: the signature no longer holds.
        let mut forged = org.link.messages()[0].clone();
        let id = forged.header().unwrap().id;
        forged.header = forged
            .header
            .replace(&id, &new_message_id(now_ms()).unwrap());
        org.link.inject(forged);
        b.handle.sync_once(0).unwrap();
        let id = b
            .handle
            .load("priya")
            .conversations
            .keys()
            .next()
            .unwrap()
            .clone();
        let read = b
            .call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        assert_eq!(read.data["messages"].as_array().unwrap().len(), 1);
        assert!(
            b.notices.lock().unwrap().iter().any(
                |n| matches!(n, Notice::Refused { reason, .. } if reason.contains("doesn't check"))
            ),
            "{:?}",
            b.notices.lock().unwrap()
        );
    }

    #[test]
    fn cf91_member_keys_are_root_only_in_the_state_dir() {
        let org = org();
        let a = org.computer("dev-a");
        a.call("dana", LIST, json!({})).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let person = org.dir.path().join("dev-a/dana");
        assert_eq!(mode(&person), 0o700);
        assert_eq!(mode(&person.join("member.key")), 0o600);
        assert_eq!(mode(&person.join("store.json")), 0o600);
        assert_eq!(std::fs::read(person.join("member.key")).unwrap().len(), 64);
        // A login is a folder name only when it is plain.
        let refused = a.call("../dana", LIST, json!({})).unwrap_err().to_string();
        assert!(refused.contains("can't use chat"), "{refused}");
    }

    #[test]
    fn cf91_without_a_console_its_setup_or_a_key_to_seal_to_nothing_is_sent() {
        let org = org();
        let unmanaged = ChatProvider::new(
            ChatHandle::new(org.dir.path().join("x")),
            org.directory.clone(),
        );
        let refused = unmanaged
            .invoke(&ctx("dana", APP), &LIST.parse().unwrap(), &json!({}))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("isn't managed by an organization"),
            "{refused}"
        );

        let not_set_up = ChatHandle::new(org.dir.path().join("y"));
        not_set_up.connect("acme", "dev-y", org.link.clone());
        let refused = ChatProvider::new(not_set_up, org.directory.clone())
            .invoke(
                &ctx("dana", APP),
                &POST.parse().unwrap(),
                &json!({ "posts": [{ "to": ["priya"], "text": "hi" }] }),
            )
            .unwrap_err()
            .to_string();
        assert!(refused.contains("hasn't set chat up"), "{refused}");

        let a = org.computer("dev-a");
        let post = json!({ "posts": [{ "to": ["sam"], "text": "hi" }] });
        let refused = a.call("dana", POST, post.clone()).unwrap_err().to_string();
        assert!(
            refused.contains("Sam Lee hasn't opened Chat yet"),
            "{refused}"
        );

        org.link.offline(true);
        let refused = a.call("dana", POST, post).unwrap_err().to_string();
        assert!(
            refused.starts_with("Not sent: your organization's server can't be reached"),
            "{refused}"
        );
        assert!(refused.contains("Nothing was queued"), "{refused}");
        assert!(org.link.messages().is_empty());

        // Someone outside the directory, or a bad id, is refused before
        // anything is sealed.
        org.link.offline(false);
        let outsider = a
            .call(
                "dana",
                POST,
                json!({ "posts": [{ "to": ["mallory"], "text": "hi" }] }),
            )
            .unwrap_err()
            .to_string();
        assert!(
            outsider.contains("isn't in your organization's directory"),
            "{outsider}"
        );
        assert!(
            a.call("dana", READ, json!({ "conversation": "cv-nope" }))
                .is_err()
        );
    }

    #[test]
    fn cf91_a_channel_has_a_sealed_name_and_its_people() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        let input = json!({ "name": "#northwind-deal", "people": ["priya"] });
        let preview = a.preview("dana", CREATE, input.clone());
        assert_eq!(preview.summary, "Make #northwind-deal with 2 people");
        let made = a.call("dana", CREATE, input).unwrap();
        assert_eq!(made.text, "Created #northwind-deal with 2 people.");
        let id = made.data["conversation"]["id"].as_str().unwrap().to_owned();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "conversation": id, "text": "Kickoff at 3" }] }),
        )
        .unwrap();
        b.handle.sync_once(0).unwrap();
        let list = b.call("priya", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["title"], "#northwind-deal");
        let preview = b.preview(
            "priya",
            POST,
            json!({ "posts": [{ "conversation": id, "text": "On it" }] }),
        );
        assert_eq!(preview.summary, "Post in #northwind-deal");
    }
}
