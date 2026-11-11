//! The `chat` app's actions (CF-91, ADR 0030): the org's chat, sealed end
//! to end between its computers, read and written here through glassd.
//!
//! Chat and agents go through the same four actions. Reading is quiet
//! (Auto); posting and making a channel leave the machine, so they ask
//! (Ask) unless the person typed them in Chat, whose `[[app]]` tiers lower
//! them. What an agent posts carries "by <agent> for <person>" inside the
//! seal, from the task's principal, never from the request (AU-10).
//! Previews and audit records name conversations, never message text.
//!
//! A meeting (CF-21) is a conversation too, of kind `meeting`, kept in the
//! same store. Chat's actions leave meetings out; Calendar and Meet reach
//! them through [`glass_managed::meet::Meetings`], which the provider
//! implements.

mod everyday;
mod fold;
mod shapes;
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
    Body, Conversation, ConversationKind, CreateConversation, Epoch, Header, LinkError, LookupKeys,
    MAX_MEMBERS, MAX_TEXT_CHARS, MemberKey, MemberSecret, Message, PostMessage, is_conversation_id,
    is_message_id, new_conversation_id, new_message_id, seal_name,
};
use glass_managed::meet::{MeetingDetails, MeetingInfo, Meetings};
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
    /// A thread's root: the root and its replies (CF-92).
    #[serde(default)]
    thread: Option<String>,
    /// Read without marking read: Chat's own window when the person isn't
    /// looking at it (hidden, or another window has the keyboard). An
    /// agent's read never marks anything, with or without it.
    #[serde(default)]
    peek: bool,
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

/// Whether Chat shows `c`: meetings are Calendar's and Meet's (CF-21).
fn in_chat(c: &Conversation) -> bool {
    c.kind.is_chat()
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
                .filter(|c| in_chat(c) && c.members.iter().any(|m| m == me))
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
            .find(|c| {
                matches!(c.kind, ConversationKind::Direct | ConversationKind::Group)
                    && c.members == want
            })
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
    /// `id` when the conversation's id was chosen already, as a meeting's
    /// is when Calendar makes the event.
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
        id: Option<&str>,
        label: Option<glass_types::Label>,
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
            // The apps that make the person's keys besides Chat.
            let app = match kind {
                ConversationKind::Meeting => "Calendar or Chat",
                ConversationKind::Artifact => "Artifacts or Chat",
                ConversationKind::Skill => "Skills or Chat",
                _ => "Chat",
            };
            return Err(ChatFailure::Local(format!(
                "{} {} opened {app} yet, so there's no key to seal to; ask them to open {app} once",
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
        let id = match id {
            Some(id) => id.to_owned(),
            None => new_conversation_id()?,
        };
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
            label,
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
            let mut store = self.handle.load_to_change(me);
            store.conversations.insert(id.clone(), conversation.clone());
            store.epochs.entry(id).or_default().push(epoch);
            for key in keys {
                store.keys.entry(key.id.clone()).or_insert(key);
            }
            self.handle.save(me, &store)?;
        }
        self.handle.changed(me, conversation.kind);
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
                self.open_conversation(
                    connection, compliance, member, me, members, kind, None, None, None,
                )?
            }
        };
        let title = self.title(&self.handle.load(me), Some(member), me, &conversation);
        let body = Body {
            text: post.text.clone(),
            attribution,
            reply_to: post.reply_to.clone(),
            sent_at: Timestamp::now().to_string(),
            part: None,
            mentions: fold::mentions(&post.text, &conversation.members, me),
        };
        let (conversation, id, seq) =
            self.send_body(connection, member, me, &conversation.id, &body)?;
        Ok((conversation, title, id, seq))
    }

    /// Seals `body` in conversation `id` as `me` and posts it. When someone
    /// changed who is in it meanwhile, the console refuses the old epoch:
    /// this computer catches up and seals it again, once, without the
    /// person seeing (ADR 0030 §1). The conversation, the message's id and
    /// its sequence number.
    fn send_body(
        &self,
        connection: &Connection,
        member: &Member,
        me: &str,
        id: &str,
        body: &Body,
    ) -> Result<(Conversation, String, u64), ChatFailure> {
        let mut again = true;
        loop {
            let store = self.handle.load(me);
            let conversation = store
                .conversations
                .get(id)
                .filter(|c| c.members.iter().any(|m| m == me))
                .cloned()
                .ok_or_else(|| ChatFailure::Local(format!("{me} isn't in {id}")))?;
            let key = store
                .epoch_key(member, &conversation.id, conversation.epoch)
                .ok_or_else(|| ChatFailure::Local("this computer can't open its key".into()))?;
            let header = Header {
                v: 1,
                org: connection.org.clone(),
                conversation: conversation.id.clone(),
                epoch: conversation.epoch,
                id: new_message_id(now_ms())?,
                sender: me.to_owned(),
                key: member.key.id.clone(),
            };
            let mut message = Message::seal(&header, body, &key, &member.secret)?;
            let request = PostMessage {
                device: connection.device.clone(),
                message: message.clone(),
            };
            let mut answer = connection.link.post(&request);
            if let Err(LinkError::Unreachable(lost)) = &answer {
                // The console may have taken it and the answer been lost, or
                // may take it yet. The same message again is taken now or
                // refused as posted already, never posted twice; any other
                // answer can't tell (ChatPost.tla).
                answer = match connection.link.post(&request) {
                    Err(LinkError::Refused(m)) if m.contains("posted already") => {
                        // Taken the first time: a sync brings its number.
                        let _ = self.handle.sync_once(0);
                        let seq = self
                            .handle
                            .load(me)
                            .messages
                            .get(&conversation.id)
                            .and_then(|kept| kept.iter().find(|m| m.header == message.header))
                            .map_or(0, |m| m.seq);
                        self.handle.changed(me, conversation.kind);
                        return Ok((conversation, header.id, seq));
                    }
                    Ok(seq) => Ok(seq),
                    Err(_) => {
                        return Err(ChatFailure::MaybePosted(format!(
                            "your organization's server didn't answer ({lost}), so the message may have been posted. If it was, it shows in the conversation once the server answers; look there before posting it again"
                        )));
                    }
                };
            }
            match answer {
                Ok(seq) => {
                    message.seq = seq;
                    message.at = Some(Timestamp::now().to_string());
                    {
                        let _write = self.handle.lock();
                        let mut store = self.handle.load_to_change(me);
                        store.keep(&conversation.id, message);
                        self.handle.save(me, &store)?;
                    }
                    self.handle.changed(me, conversation.kind);
                    return Ok((conversation, header.id, seq));
                }
                Err(LinkError::Refused(m)) if again && m.contains("old epoch") => {
                    again = false;
                    self.handle.sync_once(0).map_err(ChatFailure::Link)?;
                }
                Err(e) => return Err(ChatFailure::Link(e)),
            }
        }
    }

    /// Seals and posts a message in conversation or item `to`, which
    /// `login` is in. Its id.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn post_sealed(
        &self,
        login: &str,
        to: &str,
        text: &str,
        part: Option<glass_managed::shared::Part>,
        attribution: Option<String>,
        reply_to: Option<String>,
        mentions: Vec<String>,
    ) -> Result<String, ChatFailure> {
        let connection = self.handle.connection()?;
        let member = self.handle.existing(login).ok_or_else(|| {
            ChatFailure::NotSetUp(format!("{login} has no chat keys on this computer yet"))
        })?;
        let body = Body {
            text: text.to_owned(),
            attribution,
            reply_to,
            sent_at: Timestamp::now().to_string(),
            part,
            mentions,
        };
        Ok(self.send_body(&connection, &member, login, to, &body)?.1)
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
            .filter(|c| in_chat(c) && c.members.iter().any(|m| m == me))
            .map(|c| {
                let folded = self.folded(&store, &member, me, c);
                let seen = store.seen.get(&c.id).copied().unwrap_or(0);
                // Only messages people wrote are unread: never an edit, a
                // reaction, a pin or a topic (ADR 0030 C4a).
                let fresh: Vec<&fold::Shown> = folded
                    .messages
                    .iter()
                    .filter(|m| m.seq > seen && m.sender != *me && !m.deleted)
                    .collect();
                let unread = fresh.len();
                let mentions = fresh.iter().filter(|m| m.mentions.iter().any(|l| l == me)).count();
                let last = folded.messages.last();
                let title = self.title(&store, Some(&member), me, c);
                let line = format!(
                    "- {title} [{}]{}{}",
                    c.id,
                    if unread > 0 {
                        format!(", {unread} unread")
                    } else {
                        String::new()
                    },
                    if mentions > 0 {
                        format!(", {mentions} mentioning {me}")
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
                    "mentions": mentions,
                    "topic": folded.topic,
                    "notify": everyday::level_for(&store, c),
                    "draft": self.handle.draft(me, &c.id, None),
                    "last_at": last.map(|m| m.at.clone()),
                    "last_from": last.map(|m| self.name(&m.sender)),
                    "stamp": stamp(&store, &c.id),
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
        // Every message up to here is in the store read next.
        let fetched = self.handle.cursor();
        let store = self.handle.load(me);
        let conversation = store
            .conversations
            .get(&input.conversation)
            .filter(|c| in_chat(c) && c.members.iter().any(|m| m == me))
            .ok_or_else(|| {
                ActionError::NotFound(format!(
                    "you aren't in a conversation {}; chat.list shows yours",
                    input.conversation
                ))
            })?;
        let title = self.title(&store, Some(&member), me, conversation);
        let folded = self.folded(&store, &member, me, conversation);
        let (shown, more_before): (Vec<&fold::Shown>, bool) = match &input.thread {
            Some(root) => {
                let r = folded
                    .get(root)
                    .filter(|m| m.is_root())
                    .ok_or_else(|| ActionError::NotFound(format!("no thread {root} in {title}")))?;
                let replies: Vec<&fold::Shown> =
                    r.replies.iter().map(|&i| &folded.messages[i]).collect();
                let skip = replies.len().saturating_sub(limit);
                let mut shown = vec![r];
                shown.extend(&replies[skip..]);
                (shown, skip > 0)
            }
            None => {
                let roots: Vec<&fold::Shown> = folded.roots().collect();
                let index = |id: &str| roots.iter().position(|m| m.id == id);
                let window: &[&fold::Shown] = match (&input.before, &input.after) {
                    (Some(id), None) => {
                        let end = index(id)
                            .ok_or_else(|| ActionError::NotFound(format!("no message {id}")))?;
                        &roots[end.saturating_sub(limit)..end]
                    }
                    (None, Some(id)) => {
                        let start = index(id)
                            .ok_or_else(|| ActionError::NotFound(format!("no message {id}")))?
                            + 1;
                        &roots[start..(start + limit).min(roots.len())]
                    }
                    (None, None) => &roots[roots.len().saturating_sub(limit)..],
                    (Some(_), Some(_)) => return Err(invalid("give before or after, not both")),
                };
                let more = window
                    .first()
                    .is_some_and(|m| roots.first().is_some_and(|f| f.id != m.id));
                (window.to_vec(), more)
            }
        };
        let views: Vec<Value> = shown.iter().map(|m| self.view(&folded, m, me)).collect();
        let lines: Vec<String> = shown.iter().map(|m| self.line(m)).collect();
        // What the person reads in Chat is read; an agent reading for
        // them leaves it unread, and so does Chat refreshing a window
        // nobody is looking at (`peek`). The newest view marks everything
        // read, edits and reactions included, up to what this computer
        // has fetched: a message from before the person's own post that
        // arrives after it wasn't shown (ChatSync.tla).
        if ctx.principal.agent.as_str() == APP
            && input.before.is_none()
            && input.thread.is_none()
            && !input.peek
        {
            let _write = self.handle.lock();
            let mut store = self.handle.load_to_change(me);
            let seen = store.seen.entry(conversation.id.clone()).or_insert(0);
            let newest = folded.newest.min(fetched);
            if newest > *seen {
                *seen = newest;
                self.handle.save(me, &store).map_err(failed)?;
            }
        }
        let mut text = format!(
            "{title}{}: {}{}.",
            if input.thread.is_some() {
                ", a thread"
            } else {
                ""
            },
            plural(views.len(), "message", "messages"),
            if folded.sealed > 0 {
                format!(" ({} this computer can't open)", folded.sealed)
            } else {
                String::new()
            }
        );
        if let Some(topic) = &folded.topic {
            text.push_str(&format!(" Topic: {}.", display_line(topic)));
        }
        for line in lines {
            text.push('\n');
            text.push_str(&line);
        }
        let mut data = json!({
            "conversation": {
                "id": conversation.id,
                "kind": conversation.kind,
                "title": title,
                "topic": folded.topic,
                "pinned": folded.pinned,
                "notify": everyday::level_for(&store, conversation),
                "draft": self.handle.draft(me, &conversation.id, None),
            },
            "messages": views,
            "more_before": more_before,
        });
        // Where the organization keeps a conversation like this one only
        // so many days, a reader is told: history that ends is IT's
        // period's doing, not a fault.
        if let Some(days) = self.handle.kept_for().days(conversation.kind) {
            data["kept_days"] = json!(days);
        }
        if let Some(root) = &input.thread {
            data["thread"] = json!({ "root": root, "draft": self.handle.draft(me, &conversation.id, Some(root)) });
        }
        Ok(ActionOutput {
            text,
            data,
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
                None,
                None,
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

/// Meetings (CF-21) are conversations in chat's store: Calendar and Meet
/// reach them through this, so neither depends on Chat (ADR 0013).
/// The shared space (ADR 0033): artifacts, and next docs and sheets, are
/// conversations of their own kinds, which Chat doesn't show.
/// Conversation `c` as a shared item, its messages opened with `member`'s
/// keys; what this computer can't open is left out. None for an item whose
/// sealed label isn't the one the console keeps in the clear.
/// What changes whenever anything in conversation `id` does on this
/// computer: a message, an edit, a reaction, a pin, a topic, or messages
/// going because they are past the period chat is kept. A reader that
/// holds the conversation as of one stamp needn't read it again while the
/// list shows the same one (audit 10.4: Chat read the open conversation,
/// and its open thread, again at every change in any conversation).
fn stamp(store: &Store, id: &str) -> String {
    let all = store.messages.get(id).map_or(&[][..], Vec::as_slice);
    // How many, and the newest: one that arrives late, numbered before
    // the newest, changes the first.
    format!("{}:{}", all.len(), all.last().map_or(0, |m| m.seq))
}

fn open_item(
    store: &Store,
    member: &Member,
    c: &Conversation,
    messages: &[Message],
) -> Option<glass_managed::shared::Item> {
    let (title, label) = if c.kind.is_item() {
        let key = store.epoch_key(member, &c.id, 1)?;
        let name = glass_managed::shared::ItemName::open(&key, &c.id, c.name.as_deref()?).ok()?;
        if c.label != Some(name.label) {
            return None;
        }
        (name.title, name.label)
    } else {
        (
            store.name(member, c).unwrap_or_default(),
            glass_types::Label::Internal,
        )
    };
    let mut keys: BTreeMap<u32, Option<EpochKey>> = BTreeMap::new();
    let messages = messages
        .iter()
        .filter_map(|message| {
            let header = message.header().ok()?;
            let key = keys
                .entry(header.epoch)
                .or_insert_with(|| store.epoch_key(member, &c.id, header.epoch))
                .as_ref()?;
            let body = message.open(key).ok()?;
            Some(glass_managed::shared::ItemMessage {
                id: header.id,
                seq: message.seq,
                sender: header.sender,
                at: message.at.clone().unwrap_or(body.sent_at),
                text: body.text,
                attribution: body.attribution,
                reply_to: body.reply_to,
                part: body.part,
            })
        })
        .collect();
    Some(glass_managed::shared::Item {
        id: c.id.clone(),
        kind: c.kind,
        title,
        label,
        members: c.members.clone(),
        created_by: c.created_by.clone(),
        messages,
    })
}

impl ChatProvider {
    /// Conversation `c`'s messages here after sequence number `after`: a
    /// shared item's from its log.
    fn messages_of(
        &self,
        login: &str,
        store: &Store,
        c: &Conversation,
        after: u64,
    ) -> Vec<Message> {
        if c.kind.is_item() {
            self.handle.item_messages(login, &c.id, store, after)
        } else {
            let mut messages = store.messages.get(&c.id).cloned().unwrap_or_default();
            messages.retain(|m| m.seq > after);
            messages
        }
    }
}

impl glass_managed::shared::Shared for ChatProvider {
    fn ready(&self, login: &str, make: bool) -> Result<(), ChatFailure> {
        self.handle.compliance()?;
        if make {
            self.handle.member(login)?;
            return Ok(());
        }
        self.handle.connection()?;
        self.handle.existing(login).map(|_| ()).ok_or_else(|| {
            ChatFailure::NotSetUp(format!(
                "{login} hasn't opened Chat, Artifacts or Skills on this computer yet; each makes their keys the first time it opens"
            ))
        })
    }

    fn items(&self, login: &str, kind: ConversationKind) -> Vec<glass_managed::shared::Item> {
        let Some(member) = self.handle.existing(login) else {
            return Vec::new();
        };
        let store = self.handle.load(login);
        store
            .conversations
            .values()
            .filter(|c| c.kind == kind && c.members.iter().any(|m| m == login))
            .filter_map(|c| {
                let messages = self.messages_of(login, &store, c, 0);
                open_item(&store, &member, c, &messages)
            })
            .collect()
    }

    fn item_ids(&self, login: &str, kind: ConversationKind) -> Vec<String> {
        if self.handle.existing(login).is_none() {
            return Vec::new();
        }
        self.handle
            .load(login)
            .conversations
            .values()
            .filter(|c| c.kind == kind && c.members.iter().any(|m| m == login))
            .map(|c| c.id.clone())
            .collect()
    }

    fn item(&self, login: &str, id: &str) -> Option<glass_managed::shared::Item> {
        let member = self.handle.existing(login)?;
        let store = self.handle.load(login);
        store
            .conversations
            .get(id)
            .filter(|c| c.members.iter().any(|m| m == login))
            .and_then(|c| {
                let messages = self.messages_of(login, &store, c, 0);
                open_item(&store, &member, c, &messages)
            })
    }

    fn since(&self, login: &str, id: &str, seq: u64) -> Option<glass_managed::shared::Item> {
        let member = self.handle.existing(login)?;
        let store = self.handle.load(login);
        let c = store
            .conversations
            .get(id)
            .filter(|c| c.members.iter().any(|m| m == login))?;
        let messages = self.messages_of(login, &store, c, seq);
        open_item(&store, &member, c, &messages)
    }

    fn through(&self, _login: &str) -> u64 {
        // A sync keeps everyone's messages, then moves the cursor; a post
        // keeps this computer's own at once (DocsCollab.tla).
        self.handle.cursor()
    }

    fn create(
        &self,
        login: &str,
        kind: ConversationKind,
        title: &str,
        label: glass_types::Label,
        people: &[String],
    ) -> Result<String, ChatFailure> {
        if !kind.is_item() {
            return Err(ChatFailure::Local(format!("{kind:?} isn't a shared item")));
        }
        let connection = self.handle.connection()?;
        let compliance = self.handle.compliance()?;
        let member = self.handle.existing(login).ok_or_else(|| {
            ChatFailure::NotSetUp(format!("{login} has no chat keys on this computer yet"))
        })?;
        // Its title and label are sealed together; the label goes in the
        // clear too, for IT (ADR 0034 §5).
        let name = serde_json::to_string(&glass_managed::shared::ItemName {
            title: title.to_owned(),
            label,
        })
        .map_err(|e| ChatFailure::Local(e.to_string()))?;
        let conversation = self.open_conversation(
            &connection,
            &compliance,
            &member,
            login,
            members(login, people),
            kind,
            Some(&name),
            None,
            Some(label),
        )?;
        Ok(conversation.id)
    }

    fn post(
        &self,
        login: &str,
        to: &str,
        text: &str,
        part: Option<glass_managed::shared::Part>,
        attribution: Option<String>,
        reply_to: Option<String>,
    ) -> Result<String, ChatFailure> {
        self.post_sealed(login, to, text, part, attribution, reply_to, Vec::new())
    }

    fn put_blob(
        &self,
        login: &str,
        blob: &glass_managed::shared::SealedBlob,
    ) -> Result<(), ChatFailure> {
        let connection = self.handle.connection()?;
        connection
            .link
            .put(&glass_managed::shared::PutBlob {
                device: connection.device.clone(),
                login: login.to_owned(),
                blob: blob.clone(),
            })
            .map_err(ChatFailure::Link)?;
        // What this computer sent it needn't fetch back.
        self.handle.keep_blob(blob);
        Ok(())
    }

    fn get_blob(
        &self,
        login: &str,
        item: &str,
        id: &str,
    ) -> Result<glass_managed::shared::SealedBlob, ChatFailure> {
        let connection = self.handle.connection()?;
        // A blob never changes, so one this computer has isn't fetched
        // again (audit 9.3); it is still sealed, and whoever opens it
        // checks it against the SHA-256 its message names.
        if let Some(blob) = self.handle.kept_blob(login, item, id) {
            return Ok(blob);
        }
        let blob = connection
            .link
            .get(&glass_managed::shared::GetBlob {
                device: connection.device.clone(),
                login: login.to_owned(),
                conversation: item.to_owned(),
                id: id.to_owned(),
            })
            .map_err(ChatFailure::Link)?;
        if blob.id == id && blob.conversation == item {
            self.handle.keep_blob(&blob);
        }
        Ok(blob)
    }
}

impl Meetings for ChatProvider {
    fn ready(&self) -> Result<(), ChatFailure> {
        self.handle.compliance().map(|_| ())
    }

    fn member(
        &self,
        login: &str,
        make: bool,
    ) -> Result<Option<(MemberKey, MemberSecret)>, ChatFailure> {
        let member = if make {
            Some(self.handle.member(login)?)
        } else {
            self.handle.connection()?;
            self.handle.existing(login)
        };
        Ok(member.map(|m| (m.key, m.secret)))
    }

    fn meetings(&self, login: &str) -> Vec<MeetingInfo> {
        let Some(member) = self.handle.existing(login) else {
            return Vec::new();
        };
        let store = self.handle.load(login);
        store
            .conversations
            .values()
            .filter(|c| c.kind == ConversationKind::Meeting && c.members.iter().any(|m| m == login))
            .map(|c| MeetingInfo {
                id: c.id.clone(),
                organizer: c.created_by.clone(),
                members: c.members.clone(),
                details: store.meeting(&member, c),
            })
            .collect()
    }

    /// One meeting, opening only its details: Meet asks on every call
    /// (ADR 0052).
    fn meeting(&self, login: &str, id: &str) -> Option<MeetingInfo> {
        let member = self.handle.existing(login)?;
        let store = self.handle.load(login);
        let c = store.conversations.get(id).filter(|c| {
            c.kind == ConversationKind::Meeting && c.members.iter().any(|m| m == login)
        })?;
        Some(MeetingInfo {
            id: c.id.clone(),
            organizer: c.created_by.clone(),
            members: c.members.clone(),
            details: store.meeting(&member, c),
        })
    }

    fn create(
        &self,
        organizer: &str,
        make: bool,
        id: &str,
        people: &[String],
        details: &MeetingDetails,
    ) -> Result<(), ChatFailure> {
        let everyone = members(organizer, people);
        if !is_conversation_id(id) {
            return Err(ChatFailure::Local(format!("{id:?} is not a meeting's id")));
        }
        if let Some(login) = everyone.iter().find(|l| !plain_login(l)) {
            return Err(ChatFailure::Local(format!("{login:?} is not a login")));
        }
        if everyone.len() < 2 || everyone.len() > glass_managed::meet::MAX_PEOPLE {
            return Err(ChatFailure::Local(format!(
                "a meeting is for 2 to {} people",
                glass_managed::meet::MAX_PEOPLE
            )));
        }
        let connection = self.handle.connection()?;
        let compliance = self.handle.compliance()?;
        let member = if make {
            self.handle.member(organizer)?
        } else {
            self.handle.existing(organizer).ok_or_else(|| {
                ChatFailure::NotSetUp(format!(
                    "{organizer} hasn't opened Calendar or Chat on this computer yet; it makes their keys the first time it opens"
                ))
            })?
        };
        self.open_conversation(
            &connection,
            &compliance,
            &member,
            organizer,
            everyone,
            ConversationKind::Meeting,
            Some(&details.to_name()),
            Some(id),
            None,
        )?;
        Ok(())
    }

    fn lookup(&self, logins: &[String]) -> Result<Vec<MemberKey>, ChatFailure> {
        let connection = self.handle.connection()?;
        let found = connection
            .link
            .lookup(&LookupKeys {
                logins: logins.to_vec(),
            })
            .map_err(ChatFailure::Link)?;
        Ok(found
            .keys
            .into_iter()
            .filter(|k| logins.contains(&k.login) && k.keys().is_ok())
            .collect())
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
                              message. Each has its sender, time, text, reactions, replies and, \
                              for an agent's, who posted it for whom. Replies are in threads: \
                              give thread (a message id) to read one."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "conversation": conversation,
                        "before": { "type": "string", "description": "A message id: messages before it" },
                        "after": { "type": "string", "description": "A message id: messages after it" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": READ_MAX },
                        "thread": { "type": "string", "description": "A message id: that thread's messages" },
                        "peek": { "type": "boolean", "description": "Chat's own window, with nobody looking at it: what is read stays unread" }
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
                              @login in the text mentions a member. reply_to answers in a \
                              thread. Messages are sealed end to end and say they came from \
                              you for the person. They can't be taken back."
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
        .into_iter()
        .chain(everyday::defs())
        .collect()
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        shapes::of(action.as_str())
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
                    .filter(|c| in_chat(c) && c.members.iter().any(|m| m == me))
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
            other if everyday::ACTIONS.contains(&other) => self.preview_everyday(ctx, other, input),
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
            other if everyday::ACTIONS.contains(&other) => self.invoke_everyday(ctx, other, input),
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
    use glass_managed::chat::testing::MemoryLink;
    use glass_managed::chat::{COMPLIANCE, ChatLink, KeptFor};
    use glass_managed::directory::Applied;
    use glass_managed::shared::BlobLink;
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
            self.computer_through(name, self.link.clone())
        }

        /// One that reaches the console through `link`.
        fn computer_through(&self, name: &str, link: Arc<dyn ChatLink>) -> Computer {
            let handle = ChatHandle::new(self.dir.path().join(name));
            handle.connect("acme", name, link);
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
        assert!(refused.contains("hasn't set up chat"), "{refused}");

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
    fn cf21_chat_leaves_meetings_to_calendar_and_meet() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        let id = new_conversation_id().unwrap();
        let details = MeetingDetails {
            title: "Northwind renewal".into(),
            start: "2026-10-20T14:00".into(),
            end: "2026-10-20T15:00".into(),
            transcribed: false,
        };
        // An agent's call never makes the organizer's keys.
        let meetings: &dyn Meetings = &a.provider;
        let refused = meetings
            .create("dana", false, &id, &["priya".into()], &details)
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("hasn't opened Calendar or Chat")
        );
        meetings
            .create("dana", true, &id, &["priya".into()], &details)
            .unwrap();
        let (made, epochs) = org.link.conversations().pop().unwrap();
        assert_eq!(made.kind, ConversationKind::Meeting);
        assert_eq!(made.members, ["dana", "priya"]);
        // Its title is sealed, and IT's key opens it like any name.
        let sealed = made.name.clone().unwrap();
        assert!(!sealed.contains(&hex::encode("Northwind")));
        let key = epochs[0].open(COMPLIANCE, &org.it).unwrap();
        assert_eq!(MeetingDetails::open(&key, &id, &sealed).unwrap(), details);

        // Chat doesn't list it, read it, or post into it.
        let list = a.call("dana", LIST, json!({})).unwrap();
        assert!(list.text.starts_with("0 conversations"), "{}", list.text);
        assert!(a.call("dana", READ, json!({ "conversation": id })).is_err());
        assert!(
            a.call(
                "dana",
                POST,
                json!({ "posts": [{ "conversation": id, "text": "hi" }] })
            )
            .is_err()
        );
        // A message to the same people opens a direct conversation.
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "hi" }] }),
        )
        .unwrap();
        let kinds: Vec<ConversationKind> = org
            .link
            .conversations()
            .iter()
            .map(|(c, _)| c.kind)
            .collect();
        assert_eq!(kinds, [ConversationKind::Meeting, ConversationKind::Direct]);

        // The invitee's computer opens it after a sync, for Calendar.
        b.handle.sync_once(0).unwrap();
        let theirs = b.provider.meetings("priya");
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].organizer, "dana");
        assert_eq!(theirs[0].details.as_ref(), Some(&details));
        assert!(b.provider.meetings("sam").is_empty());
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
    /// Two people in a channel: Dana on computer a, Priya on b, both with
    /// Chat open, and the channel's id.
    fn launch_channel(org: &Org) -> (Computer, Computer, String) {
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call("dana", LIST, json!({})).unwrap();
        let made = a
            .call(
                "dana",
                CREATE,
                json!({ "name": "launch", "people": ["priya"] }),
            )
            .unwrap();
        let id = made.data["conversation"]["id"].as_str().unwrap().to_owned();
        b.handle.sync_once(0).unwrap();
        (a, b, id)
    }

    fn say(c: &Computer, login: &str, id: &str, text: &str, reply_to: Option<&str>) -> String {
        let out = c
            .call(
                login,
                POST,
                json!({ "posts": [{ "conversation": id, "text": text, "reply_to": reply_to }] }),
            )
            .unwrap();
        out.data["posted"][0]["id"].as_str().unwrap().to_owned()
    }

    fn sync(cs: &[&Computer]) {
        for c in cs {
            c.handle.sync_once(0).unwrap();
        }
    }

    #[test]
    fn cf92_two_computers_see_threads_reactions_edits_deletes_pins_and_topics() {
        let org = org();
        let (a, b, id) = launch_channel(&org);
        let root = say(&a, "dana", &id, "Ship Friday", None);
        sync(&[&b]);
        let reply = say(&b, "priya", &id, "Can we do Monday?", Some(&root));
        let oops = say(&b, "priya", &id, "oops, wrong channel", None);
        sync(&[&a]);
        let call = |c: &Computer, login: &str, action: &str, input: Value| {
            c.call(login, action, input).unwrap()
        };
        call(
            &a,
            "dana",
            everyday::REACT,
            json!({ "conversation": id, "message": reply, "emoji": "👍" }),
        );
        call(
            &b,
            "priya",
            everyday::REACT,
            json!({ "conversation": id, "message": root, "emoji": "👍" }),
        );
        call(
            &a,
            "dana",
            everyday::EDIT,
            json!({ "conversation": id, "message": root, "text": "Ship Monday" }),
        );
        call(
            &a,
            "dana",
            everyday::PIN,
            json!({ "conversation": id, "message": root }),
        );
        call(
            &b,
            "priya",
            everyday::TOPIC,
            json!({ "conversation": id, "topic": "Q4 launch" }),
        );
        call(
            &b,
            "priya",
            everyday::DELETE,
            json!({ "conversation": id, "message": oops }),
        );
        sync(&[&a, &b]);

        for (c, login) in [(&a, "dana"), (&b, "priya")] {
            let read = c.call(login, READ, json!({ "conversation": id })).unwrap();
            let conversation = &read.data["conversation"];
            assert_eq!(conversation["topic"], "Q4 launch", "{login}");
            assert_eq!(conversation["pinned"], json!([root]), "{login}");
            let messages = read.data["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 2, "{login}: the reply is in its thread");
            let r = &messages[0];
            assert_eq!(
                (
                    r["text"].as_str(),
                    r["edited"].as_bool(),
                    r["pinned"].as_bool(),
                    r["replies"].as_u64()
                ),
                (Some("Ship Monday"), Some(true), Some(true), Some(1)),
                "{login}"
            );
            assert_eq!(r["reactions"][0]["emoji"], "👍");
            assert_eq!(r["reactions"][0]["names"], json!(["Priya Shah"]));
            assert_eq!(r["reactions"][0]["mine"], login == "priya");
            assert_eq!(r["repliers"], json!(["Priya Shah"]));
            assert_eq!(
                (
                    messages[1]["deleted"].as_bool(),
                    messages[1]["text"].as_str()
                ),
                (Some(true), Some(""))
            );
            let thread = c
                .call(login, READ, json!({ "conversation": id, "thread": root }))
                .unwrap();
            let t = thread.data["messages"].as_array().unwrap();
            assert_eq!(t.len(), 2);
            assert_eq!(
                (t[1]["text"].as_str(), t[1]["thread"].as_str()),
                (Some("Can we do Monday?"), Some(root.as_str()))
            );
            assert_eq!(t[1]["reactions"][0]["names"], json!(["Dana Ruiz"]));
        }
        // An agent reading sees the same, in its lines.
        let read = a.call("dana", READ, json!({ "conversation": id })).unwrap();
        assert!(read.text.contains("Topic: Q4 launch."), "{}", read.text);
        assert!(
            read.text
                .contains("Ship Monday (edited) [1 reply] [👍 1] [pinned]"),
            "{}",
            read.text
        );
        assert!(read.text.contains("(deleted)"), "{}", read.text);
        // The console holds the edit and the delete as ciphertext like any
        // message: none of the new text is readable there.
        assert!(
            org.link
                .messages()
                .iter()
                .all(|m| !m.ct.contains(&hex::encode("Ship Monday")))
        );
    }

    // The owner's decision of 2026-11-11: how long chat is kept is IT's
    // to set, and unset it is kept. (The console in these tests stamps
    // every message 2026-10-19, so what arrives from it is more than a day
    // old; what a person posts here and now is not.)
    #[test]
    fn cf61_chat_older_than_its_rule_goes_from_a_computer_and_isnt_kept_again() {
        use glass_managed::shared::Shared;
        let org = org();
        let (a, b, channel) = launch_channel(&org);
        let root = say(&a, "dana", &channel, "Ship Friday", None);
        say(&a, "dana", &channel, "Budget is 40k", None);
        // An artifact shared with Priya: an item, kept as messages too.
        let item = Shared::create(
            &a.provider,
            "dana",
            ConversationKind::Artifact,
            "Q3 page",
            glass_types::Label::Internal,
            &["priya".to_owned()],
        )
        .unwrap();
        Shared::post(&a.provider, "dana", &item, "First cut", None, None, None).unwrap();
        sync(&[&b]);
        // Priya answers today, in the thread.
        let reply = say(&b, "priya", &channel, "Can we do Monday?", Some(&root));
        let shown = |c: &Computer, login: &str| -> Vec<String> {
            let read = c
                .call(login, READ, json!({ "conversation": channel }))
                .unwrap();
            read.data["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["text"].as_str().unwrap().to_owned())
                .collect()
        };
        let items = |c: &Computer| {
            Shared::item(&c.provider, "priya", &item)
                .unwrap()
                .messages
                .len()
        };
        // (A reply shows under what it answers, not in the timeline.)
        assert_eq!(shown(&b, "priya"), ["Ship Friday", "Budget is 40k"]);
        assert_eq!(items(&b), 1);

        // No rule: nothing ever goes, and nobody is told there's one.
        assert_eq!(b.handle.trim(), 0);
        let read = b
            .call("priya", READ, json!({ "conversation": channel }))
            .unwrap();
        assert!(read.data.get("kept_days").is_none(), "{}", read.data);

        // IT keeps direct messages a day: a channel isn't one, and stays.
        b.handle.set_kept_for(KeptFor {
            channel_days: 0,
            dm_days: 1,
        });
        assert_eq!(b.handle.trim(), 0);
        let read = b
            .call("priya", READ, json!({ "conversation": channel }))
            .unwrap();
        assert!(read.data.get("kept_days").is_none(), "{}", read.data);

        // And channels a day. Dana's two messages go; Priya's reply from
        // today stays, on its own now that what it answered is gone.
        let kept_for = KeptFor {
            channel_days: 1,
            dm_days: 1,
        };
        b.handle.set_kept_for(kept_for);
        assert_eq!(b.handle.trim(), 2);
        assert_eq!(shown(&b, "priya"), ["Can we do Monday?"]);
        let read = b
            .call("priya", READ, json!({ "conversation": channel }))
            .unwrap();
        assert_eq!(read.data["kept_days"], 1);
        assert_eq!(read.data["more_before"], false);
        assert_eq!(read.data["messages"][0]["id"], reply.as_str());
        assert!(
            b.notices.lock().unwrap().contains(&Notice::Trimmed {
                removed: 2,
                kept_for
            }),
            "said once, for glassd's record"
        );
        // The artifact is not chat: its messages are what it is.
        assert_eq!(items(&b), 1);
        // Again, with nothing left past the rule: nothing, and no notice.
        let told = b.notices.lock().unwrap().len();
        assert_eq!(b.handle.trim(), 0);
        assert_eq!(b.notices.lock().unwrap().len(), told);

        // The console still has the old messages, and sends them all again
        // when someone new starts Chat on this computer. They aren't kept.
        b.call("sam", LIST, json!({})).unwrap();
        sync(&[&b]);
        assert_eq!(shown(&b, "priya"), ["Can we do Monday?"]);
        assert_eq!(items(&b), 1);
    }

    // Audit 10.4: Chat read the open conversation again at every change
    // anywhere. The list's stamp says whether there is anything to read.
    #[test]
    fn cf92_a_conversations_stamp_changes_when_it_does_and_only_then() {
        let org = org();
        let (a, b, channel) = launch_channel(&org);
        let root = say(&a, "dana", &channel, "Ship Friday", None);
        let direct = a
            .call(
                "dana",
                POST,
                json!({ "posts": [{ "to": ["priya"], "text": "Lunch?" }] }),
            )
            .unwrap();
        let direct = direct.data["posted"][0]["conversation"]
            .as_str()
            .unwrap()
            .to_owned();
        sync(&[&b]);
        let stamps = |c: &Computer| -> (String, String) {
            let list = c.call("priya", LIST, json!({})).unwrap();
            let of = |id: &str| {
                list.data["conversations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["id"] == id)
                    .unwrap()["stamp"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            (of(&channel), of(&direct))
        };
        let first = stamps(&b);
        // Listing and reading change nothing.
        b.call("priya", READ, json!({ "conversation": channel }))
            .unwrap();
        assert_eq!(stamps(&b), first);

        // A message in the channel changes the channel's, not the other.
        say(&a, "dana", &channel, "Budget is 40k", None);
        sync(&[&b]);
        let second = stamps(&b);
        assert_ne!(second.0, first.0);
        assert_eq!(second.1, first.1);

        // So does what isn't a message people wrote: a reaction.
        a.call(
            "dana",
            everyday::REACT,
            json!({ "conversation": channel, "message": root, "emoji": "👍" }),
        )
        .unwrap();
        sync(&[&b]);
        let third = stamps(&b);
        assert_ne!(third.0, second.0);
        assert_eq!(third.1, first.1);

        // And messages going because they are past their period.
        b.handle.set_kept_for(KeptFor {
            channel_days: 0,
            dm_days: 1,
        });
        assert_eq!(b.handle.trim(), 1);
        let fourth = stamps(&b);
        assert_eq!(fourth.0, third.0);
        assert_ne!(fourth.1, first.1);
    }

    // ADR 0030 §4: a period for channels, and one for direct messages
    // and groups. Each goes by its own.
    #[test]
    fn cf61_channels_and_direct_messages_each_have_their_own_period() {
        let org = org();
        let (a, b, channel) = launch_channel(&org);
        say(&a, "dana", &channel, "Ship Friday", None);
        let direct = a
            .call(
                "dana",
                POST,
                json!({ "posts": [{ "to": ["priya"], "text": "Lunch?" }] }),
            )
            .unwrap();
        let direct = direct.data["posted"][0]["conversation"]
            .as_str()
            .unwrap()
            .to_owned();
        sync(&[&b]);
        let shown = |conversation: &str| -> usize {
            let read = b
                .call("priya", READ, json!({ "conversation": conversation }))
                .unwrap();
            read.data["messages"].as_array().unwrap().len()
        };
        let kept_days = |conversation: &str| {
            let read = b
                .call("priya", READ, json!({ "conversation": conversation }))
                .unwrap();
            read.data.get("kept_days").cloned()
        };
        assert_eq!((shown(&channel), shown(&direct)), (1, 1));

        // Direct messages a day (what the console sent is weeks old),
        // channels a hundred years.
        b.handle.set_kept_for(KeptFor {
            channel_days: KeptFor::MAX_DAYS,
            dm_days: 1,
        });
        assert_eq!(b.handle.trim(), 1);
        assert_eq!((shown(&channel), shown(&direct)), (1, 0));
        assert_eq!(kept_days(&channel), Some(json!(KeptFor::MAX_DAYS)));
        assert_eq!(kept_days(&direct), Some(json!(1)));

        // IT takes the periods away: nothing more goes, and nobody is
        // told of one.
        b.handle.set_kept_for(KeptFor::default());
        assert_eq!(b.handle.trim(), 0);
        assert_eq!((shown(&channel), shown(&direct)), (1, 0));
        assert_eq!(kept_days(&channel), None);
    }

    #[test]
    fn cf92_an_edit_or_delete_counts_only_from_its_sender() {
        let org = org();
        let (a, b, id) = launch_channel(&org);
        let root = say(&a, "dana", &id, "Budget is 40k", None);
        sync(&[&b]);
        let refused = b
            .call(
                "priya",
                everyday::EDIT,
                json!({ "conversation": id, "message": root, "text": "Budget is 90k" }),
            )
            .unwrap_err();
        assert!(refused.to_string().contains("only your own"), "{refused}");
        assert!(
            b.call(
                "priya",
                everyday::DELETE,
                json!({ "conversation": id, "message": root })
            )
            .is_err()
        );
        // A changed client that posts the parts anyway changes nothing on
        // any computer: the fold judges by the verified sender.
        use glass_managed::shared::{Part, Shared};
        Shared::post(
            &b.provider,
            "priya",
            &id,
            "",
            Some(Part::Edit {
                message: root.clone(),
                text: "Budget is 90k".into(),
            }),
            None,
            None,
        )
        .unwrap();
        Shared::post(
            &b.provider,
            "priya",
            &id,
            "",
            Some(Part::Delete {
                message: root.clone(),
            }),
            None,
            None,
        )
        .unwrap();
        sync(&[&a, &b]);
        for (c, login) in [(&a, "dana"), (&b, "priya")] {
            let read = c.call(login, READ, json!({ "conversation": id })).unwrap();
            let m = &read.data["messages"][0];
            assert_eq!(
                (
                    m["text"].as_str(),
                    m["edited"].as_bool(),
                    m["deleted"].as_bool()
                ),
                (Some("Budget is 40k"), Some(false), Some(false)),
                "{login}"
            );
        }
    }

    #[test]
    fn cf92_an_agent_edits_only_what_it_posted() {
        let org = org();
        let (a, _b, id) = launch_channel(&org);
        let agent = ctx("dana", "claude-code");
        let posted = a
            .provider
            .invoke(
                &agent,
                &POST.parse().unwrap(),
                &json!({ "posts": [{ "conversation": id, "text": "Draft agenda: 1. Budget" }] }),
            )
            .unwrap();
        let by_agent = posted.data["posted"][0]["id"].as_str().unwrap().to_owned();
        let typed = say(&a, "dana", &id, "I'll run the meeting", None);
        let edit = |message: &str| {
            a.provider.invoke(
                &agent,
                &everyday::EDIT.parse().unwrap(),
                &json!({ "conversation": id, "message": message, "text": "changed" }),
            )
        };
        assert!(edit(&by_agent).is_ok());
        let refused = edit(&typed).unwrap_err().to_string();
        assert!(refused.contains("only messages it posted"), "{refused}");
        let refused = a
            .provider
            .invoke(
                &agent,
                &everyday::DELETE.parse().unwrap(),
                &json!({ "conversation": id, "message": typed }),
            )
            .unwrap_err()
            .to_string();
        assert!(refused.contains("only messages it posted"), "{refused}");
        // Audit 6.5: what the action refuses, its preview refuses first, in
        // the same words. Nobody is asked to approve an edit that fails.
        let asked = |ctx: &TaskContext, action: &str, input: Value| {
            a.provider.preview(ctx, &action.parse().unwrap(), &input)
        };
        for (action, input) in [
            (
                everyday::EDIT,
                json!({ "conversation": id, "message": typed, "text": "changed" }),
            ),
            (
                everyday::DELETE,
                json!({ "conversation": id, "message": typed }),
            ),
            (
                everyday::EDIT,
                json!({ "conversation": id, "message": "0".repeat(64), "text": "changed" }),
            ),
            (
                everyday::PIN,
                json!({ "conversation": id, "message": "0".repeat(64), "pinned": true }),
            ),
            (
                everyday::REACT,
                json!({ "conversation": id, "message": "0".repeat(64), "emoji": "👍", "on": true }),
            ),
        ] {
            let refused = asked(&agent, action, input.clone())
                .unwrap_err()
                .to_string();
            let done = a
                .provider
                .invoke(&agent, &action.parse().unwrap(), &input)
                .unwrap_err()
                .to_string();
            assert_eq!(refused, done, "{action}");
        }
        // What it may change previews as before, with what was and will be.
        let edit = asked(
            &agent,
            everyday::EDIT,
            json!({ "conversation": id, "message": by_agent, "text": "Agenda: 1. Budget" }),
        )
        .unwrap();
        assert!(
            edit.summary.starts_with("Edit your message in"),
            "{}",
            edit.summary
        );
        assert_eq!(edit.items[0], "Was: changed");
        // The person edits either in Chat.
        a.call("dana", everyday::EDIT, json!({ "conversation": id, "message": by_agent, "text": "Agenda: 1. Budget 2. Hiring" })).unwrap();
        // A deleted message can't be edited, pinned or reacted to, and
        // the previews say so too.
        a.call(
            "dana",
            everyday::DELETE,
            json!({ "conversation": id, "message": typed }),
        )
        .unwrap();
        let me = ctx("dana", APP);
        for (action, input) in [
            (
                everyday::EDIT,
                json!({ "conversation": id, "message": typed, "text": "again" }),
            ),
            (
                everyday::PIN,
                json!({ "conversation": id, "message": typed, "pinned": true }),
            ),
            (
                everyday::REACT,
                json!({ "conversation": id, "message": typed, "emoji": "👍", "on": true }),
            ),
        ] {
            let refused = asked(&me, action, input.clone()).unwrap_err().to_string();
            assert!(
                refused.contains("that message was deleted"),
                "{action}: {refused}"
            );
            let done = a.call("dana", action, input).unwrap_err().to_string();
            assert_eq!(refused, done, "{action}");
        }
    }

    #[test]
    fn cf92_mentions_and_unread_count_only_messages() {
        let org = org();
        let (a, b, id) = launch_channel(&org);
        let m = say(
            &a,
            "dana",
            &id,
            "@priya can you review? cc @sam (not here)",
            None,
        );
        sync(&[&b]);
        let row = |c: &Computer, login: &str| {
            let list = c.call(login, LIST, json!({})).unwrap();
            list.data["conversations"][0].clone()
        };
        let r = row(&b, "priya");
        assert_eq!(
            (r["unread"].as_u64(), r["mentions"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(r["notify"], "mentions", "channels default to mentions");
        let read = b
            .call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        assert_eq!(
            read.data["messages"][0]["mentions"],
            json!([{ "login": "priya", "name": "Priya Shah" }])
        );
        assert_eq!(row(&b, "priya")["unread"], 0);
        // Edits, reactions and pins aren't new messages.
        a.call(
            "dana",
            everyday::EDIT,
            json!({ "conversation": id, "message": m, "text": "@priya can you review today?" }),
        )
        .unwrap();
        a.call(
            "dana",
            everyday::REACT,
            json!({ "conversation": id, "message": m, "emoji": "🙏" }),
        )
        .unwrap();
        a.call(
            "dana",
            everyday::PIN,
            json!({ "conversation": id, "message": m }),
        )
        .unwrap();
        sync(&[&b]);
        let r = row(&b, "priya");
        assert_eq!(
            (r["unread"].as_u64(), r["mentions"].as_u64()),
            (Some(0), Some(0))
        );
        // A reaction taken back, and one that's already in place, change
        // nothing more.
        let again = a
            .call(
                "dana",
                everyday::REACT,
                json!({ "conversation": id, "message": m, "emoji": "🙏" }),
            )
            .unwrap();
        assert_eq!(again.text, "Nothing to change.");
        let before = org.link.messages().len();
        a.call(
            "dana",
            everyday::REACT,
            json!({ "conversation": id, "message": m, "emoji": "🙏", "on": false }),
        )
        .unwrap();
        assert_eq!(org.link.messages().len(), before + 1);
        sync(&[&b]);
        let read = b
            .call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        assert_eq!(read.data["messages"][0]["reactions"], json!([]));
    }

    #[test]
    fn cf93_search_finds_edited_text_and_never_deleted_text() {
        let org = org();
        let (a, b, id) = launch_channel(&org);
        let m = say(&a, "dana", &id, "budget alpha for Q3", None);
        let gone = say(&a, "dana", &id, "secret gamma plan", None);
        a.call(
            "dana",
            everyday::EDIT,
            json!({ "conversation": id, "message": m, "text": "budget Beta for Q3" }),
        )
        .unwrap();
        a.call(
            "dana",
            everyday::DELETE,
            json!({ "conversation": id, "message": gone }),
        )
        .unwrap();
        sync(&[&b]);
        let hits = |q: &str| {
            b.call("priya", everyday::SEARCH, json!({ "query": q }))
                .unwrap()
                .data["hits"]
                .as_array()
                .unwrap()
                .len()
        };
        assert_eq!(hits("beta"), 1, "case doesn't matter");
        assert_eq!(hits("alpha"), 0, "an edit replaces the old text");
        assert_eq!(hits("gamma"), 0, "deleted text is never found");
        let found = b
            .call(
                "priya",
                everyday::SEARCH,
                json!({ "query": "budget", "conversation": id }),
            )
            .unwrap();
        assert_eq!(found.data["hits"][0]["title"], "#launch");
        assert_eq!(found.data["hits"][0]["message"], m);
        // The review list and audit name where, never what was looked for.
        let preview = b.preview("priya", everyday::SEARCH, json!({ "query": "budget" }));
        assert_eq!(preview.summary, "Search Chat");
        assert!(!format!("{preview:?}").contains("budget"));
        assert!(
            b.call("priya", everyday::SEARCH, json!({ "query": "a" }))
                .is_err()
        );
    }

    #[test]
    fn cf92_drafts_and_levels_stay_on_this_computer() {
        let org = org();
        let (a, _b, id) = launch_channel(&org);
        let words = "half-written thought about pricing";
        a.call(
            "dana",
            everyday::DRAFT,
            json!({ "conversation": id, "text": words }),
        )
        .unwrap();
        a.call(
            "dana",
            everyday::NOTIFY,
            json!({ "conversation": id, "level": "none" }),
        )
        .unwrap();
        let list = a.call("dana", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["draft"], words);
        assert_eq!(list.data["conversations"][0]["notify"], "none");
        // The draft is in memory, never on disk, and never at the console.
        for entry in walk(org.dir.path()) {
            let bytes = std::fs::read(&entry).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("pricing"),
                "{}",
                entry.display()
            );
        }
        assert!(
            org.link
                .messages()
                .iter()
                .all(|m| !m.ct.contains(&hex::encode("pricing")))
        );
        a.call(
            "dana",
            everyday::DRAFT,
            json!({ "conversation": id, "text": "" }),
        )
        .unwrap();
        assert!(
            a.call("dana", LIST, json!({})).unwrap().data["conversations"][0]["draft"].is_null()
        );
        assert!(
            a.call(
                "dana",
                everyday::NOTIFY,
                json!({ "conversation": id, "level": "loud" })
            )
            .is_err()
        );
    }

    fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }
    #[test]
    fn cf91_someone_added_reads_the_history_and_a_stale_post_is_sealed_again() {
        let org = org();
        let (a, b, id) = launch_channel(&org);
        let c = org.computer("dev-c");
        c.call("sam", LIST, json!({})).unwrap();
        say(&a, "dana", &id, "Kickoff notes: ship Monday", None);
        sync(&[&b]);
        let preview = a.preview(
            "dana",
            everyday::MEMBERS,
            json!({ "conversation": id, "add": ["sam"] }),
        );
        assert_eq!(preview.summary, "Add 1 person to #launch");
        assert_eq!(preview.items, ["Sam Lee (reads its history)"]);
        let added = a
            .call(
                "dana",
                everyday::MEMBERS,
                json!({ "conversation": id, "add": ["sam"] }),
            )
            .unwrap();
        assert_eq!(
            added.text,
            "Added Sam Lee to #launch. They can read its history."
        );
        // Sam's computer checks the signed chain, opens epoch 2 and through
        // it epoch 1, and reads what was said before he came.
        sync(&[&c]);
        let read = c.call("sam", READ, json!({ "conversation": id })).unwrap();
        assert_eq!(
            read.data["conversation"]["title"], "#launch",
            "the name opens too"
        );
        assert_eq!(
            read.data["messages"][0]["text"],
            "Kickoff notes: ship Monday"
        );
        // Priya's computer hasn't heard of the change; her post is refused
        // for the old epoch, caught up and sealed again without her seeing.
        let posted = say(&b, "priya", &id, "Welcome Sam", None);
        sync(&[&a, &c]);
        for (comp, login) in [(&a, "dana"), (&c, "sam")] {
            let read = comp
                .call(login, READ, json!({ "conversation": id }))
                .unwrap();
            let last = read.data["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone();
            assert_eq!(
                (last["id"].as_str(), last["text"].as_str()),
                (Some(posted.as_str()), Some("Welcome Sam")),
                "{login}"
            );
        }
        assert_eq!(b.handle.load("priya").conversations[&id].epoch, 2);
        // IT's key opens the new epoch as every other.
        let store = a.handle.load("dana");
        assert!(store.epochs[&id][1].open(COMPLIANCE, &org.it).is_ok());
        // Adding someone already in it changes nothing.
        let again = a
            .call(
                "dana",
                everyday::MEMBERS,
                json!({ "conversation": id, "add": ["sam"] }),
            )
            .unwrap();
        assert!(again.changes.is_empty());
    }

    #[test]
    fn cf91_a_member_the_console_adds_without_a_members_signature_gets_no_key() {
        use glass_managed::chat::{ChatSyncResponse, MemberOp};
        let org = org();
        let (a, b, id) = launch_channel(&org);
        let c = org.computer("dev-c");
        c.call("sam", LIST, json!({})).unwrap();
        // Sam, who isn't in #launch, makes an epoch adding himself, and a
        // console that would collude offers it to Priya's computer.
        let sam = c.handle.member("sam").unwrap();
        let store = a.handle.load("dana");
        let conversation = store.conversations[&id].clone();
        let key = glass_crypto::EpochKey::generate().unwrap();
        let mut forged = conversation.clone();
        forged.members = vec!["dana".into(), "priya".into(), "sam".into()];
        forged.epoch = 2;
        let keys: Vec<_> = store
            .keys
            .values()
            .cloned()
            .chain([sam.key.clone()])
            .collect();
        let epoch = Epoch::change(
            &id,
            2,
            &key,
            &glass_crypto::EpochKey::generate().unwrap(),
            &keys,
            &org.it.public().unwrap(),
            (&sam.key, &sam.secret),
            (
                forged.members.clone(),
                MemberOp::Add {
                    people: vec!["sam".into()],
                },
            ),
        )
        .unwrap();
        let response = ChatSyncResponse {
            conversations: vec![forged.clone()],
            epochs: vec![epoch],
            messages: Vec::new(),
            keys: keys.clone(),
            cursor: 0,
        };
        let mut refused = Vec::new();
        let mut store = b.handle.load_to_change("priya");
        store.absorb("acme", "priya", &response, &mut refused);
        assert_eq!(
            store.conversations[&id].members,
            ["dana", "priya"],
            "{refused:?}"
        );
        assert_eq!(store.conversations[&id].epoch, 1);
        assert!(
            refused
                .iter()
                .any(|(_, why)| why.contains("someone in a conversation")),
            "{refused:?}"
        );
        // Members changed with no epoch at all are refused too.
        let mut bare = conversation;
        bare.members.push("sam".into());
        let response = ChatSyncResponse {
            conversations: vec![bare],
            cursor: 0,
            ..Default::default()
        };
        let mut refused = Vec::new();
        store.absorb("acme", "priya", &response, &mut refused);
        assert_eq!(store.conversations[&id].members, ["dana", "priya"]);
        assert!(
            refused
                .iter()
                .any(|(_, why)| why.contains("without a member's signature")),
            "{refused:?}"
        );
        // And the console refuses Sam's epoch outright.
        let refused = glass_managed::chat::ChatLink::rotate(
            org.link.as_ref(),
            &glass_managed::chat::RotateEpoch {
                device: "dev-c".into(),
                epoch: response_epoch(&id, &sam, &keys, &org),
            },
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("someone in a conversation"),
            "{refused}"
        );
    }

    /// An epoch 2 for `id` made by `sam`, adding himself.
    fn response_epoch(id: &str, sam: &Member, keys: &[MemberKey], org: &Org) -> Epoch {
        Epoch::change(
            id,
            2,
            &glass_crypto::EpochKey::generate().unwrap(),
            &glass_crypto::EpochKey::generate().unwrap(),
            keys,
            &org.it.public().unwrap(),
            (&sam.key, &sam.secret),
            (
                vec!["dana".into(), "priya".into(), "sam".into()],
                glass_managed::chat::MemberOp::Add {
                    people: vec!["sam".into()],
                },
            ),
        )
        .unwrap()
    }

    /// GA-1 (ADR 0049): every action, run for real between two computers,
    /// returns what it declares: the first list before Chat is opened, a
    /// channel with a thread, a mention, a shared card, a reaction, a pin,
    /// a topic, an edit, a delete, drafts, a level and people added.
    #[test]
    fn ga1_what_each_action_returns_is_what_it_declares() {
        use glass_managed::shared::Part;

        let org = org();
        let (a, b, c) = (
            org.computer("dev-a"),
            org.computer("dev-b"),
            org.computer("dev-c"),
        );
        let mut checked: Vec<String> = Vec::new();
        let mut check = |action: &str, out: Result<ActionOutput, ActionError>| -> Value {
            let mut data = out.unwrap_or_else(|e| panic!("{action}: {e}")).data;
            if let Some(data) = data.as_object_mut() {
                data.remove(glass_actions::AUDIT_DETAIL);
            }
            let shape = shapes::of(action).unwrap_or_else(|| panic!("{action} declares nothing"));
            glass_actions::shape::conforms(&data, &shape)
                .unwrap_or_else(|e| panic!("{action}: {e}\n{data:#}"));
            checked.push(action.to_owned());
            data
        };

        // An agent asks before Sam has ever opened Chat here.
        let before = check(
            LIST,
            c.provider.invoke(
                &ctx("sam", "desktop-agent"),
                &LIST.parse().unwrap(),
                &json!({}),
            ),
        );
        assert_eq!(before["opened"], false);
        for (computer, login) in [(&a, "dana"), (&b, "priya"), (&c, "sam")] {
            check(LIST, computer.call(login, LIST, json!({})));
        }

        let made = check(
            CREATE,
            a.call(
                "dana",
                CREATE,
                json!({ "name": "launch", "people": ["priya"] }),
            ),
        );
        let id = made["conversation"]["id"].as_str().unwrap().to_owned();
        let posted = check(
            POST,
            a.call(
                "dana",
                POST,
                json!({ "posts": [
                    { "conversation": id, "text": "@priya can we ship Friday?" },
                    { "conversation": id, "text": "oops, wrong channel" },
                    { "to": ["priya"], "text": "Lunch?" }
                ] }),
            ),
        );
        let root = posted["posted"][0]["id"].as_str().unwrap().to_owned();
        let oops = posted["posted"][1]["id"].as_str().unwrap().to_owned();
        sync(&[&b]);
        check(
            POST,
            b.call(
                "priya",
                POST,
                json!({ "posts": [{ "conversation": id, "text": "Monday is safer", "reply_to": root }] }),
            ),
        );
        // A card for an item shared here, as Artifacts posts it.
        a.provider
            .post_sealed(
                "dana",
                &id,
                "",
                Some(Part::Shared {
                    item: "art-1".into(),
                    kind: "artifact".into(),
                    title: "Launch plan".into(),
                }),
                None,
                None,
                Vec::new(),
            )
            .unwrap();
        sync(&[&a]);

        let on = json!({ "conversation": id, "message": root, "emoji": "👍" });
        check(everyday::REACT, a.call("dana", everyday::REACT, on.clone()));
        check(everyday::REACT, a.call("dana", everyday::REACT, on));
        check(
            everyday::PIN,
            a.call(
                "dana",
                everyday::PIN,
                json!({ "conversation": id, "message": root }),
            ),
        );
        check(
            everyday::TOPIC,
            a.call(
                "dana",
                everyday::TOPIC,
                json!({ "conversation": id, "topic": "Launch week" }),
            ),
        );
        check(
            everyday::EDIT,
            a.call(
                "dana",
                everyday::EDIT,
                json!({ "conversation": id, "message": root, "text": "@priya can we ship Friday, or Monday?" }),
            ),
        );
        check(
            everyday::DELETE,
            a.call(
                "dana",
                everyday::DELETE,
                json!({ "conversation": id, "message": oops }),
            ),
        );
        check(
            everyday::DRAFT,
            a.call(
                "dana",
                everyday::DRAFT,
                json!({ "conversation": id, "text": "Monday works if" }),
            ),
        );
        check(
            everyday::DRAFT,
            a.call(
                "dana",
                everyday::DRAFT,
                json!({ "conversation": id, "thread": root, "text": "Agreed" }),
            ),
        );
        check(
            everyday::NOTIFY,
            a.call(
                "dana",
                everyday::NOTIFY,
                json!({ "conversation": id, "level": "all" }),
            ),
        );
        let add = json!({ "conversation": id, "add": ["sam"] });
        check(
            everyday::MEMBERS,
            a.call("dana", everyday::MEMBERS, add.clone()),
        );
        let again = check(everyday::MEMBERS, a.call("dana", everyday::MEMBERS, add));
        assert_eq!(again["added"], json!([]));

        let read = check(READ, a.call("dana", READ, json!({ "conversation": id })));
        let messages = read["messages"].as_array().unwrap();
        assert!(messages.iter().any(|m| m["shared"].is_object()), "{read:#}");
        assert!(messages.iter().any(|m| m["reactions"][0]["mine"] == true));
        assert!(
            messages
                .iter()
                .any(|m| m["mentions"][0]["login"] == "priya")
        );
        let thread = check(
            READ,
            a.call("dana", READ, json!({ "conversation": id, "thread": root })),
        );
        assert_eq!(thread["thread"]["draft"], "Agreed");
        assert_eq!(thread["messages"][1]["thread"], root.as_str());
        let found = check(
            everyday::SEARCH,
            a.call("dana", everyday::SEARCH, json!({ "query": "monday" })),
        );
        assert!(!found["hits"].as_array().unwrap().is_empty());
        let list = check(LIST, b.call("priya", LIST, json!({})));
        assert!(list["conversations"][0]["last_at"].is_string());

        let every: Vec<String> = a
            .provider
            .actions()
            .iter()
            .map(|d| d.name.to_string())
            .collect();
        for action in &every {
            assert!(checked.contains(action), "{action} wasn't run");
        }
    }

    /// The org's console, where a sync can be held while things happen
    /// (glassd's long poll waiting), and answers to posts lost.
    struct Held {
        console: Arc<MemoryLink>,
        /// When set, the next sync says it's waiting and waits for a go.
        hold: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
        /// Posts the console takes whose answers are lost.
        lose: std::sync::atomic::AtomicUsize,
    }

    impl Held {
        fn new(console: Arc<MemoryLink>) -> Self {
            Self {
                console,
                hold: Mutex::new(None),
                lose: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl std::fmt::Debug for Held {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Held")
        }
    }

    impl BlobLink for Held {
        fn put(&self, r: &glass_managed::shared::PutBlob) -> Result<(), LinkError> {
            self.console.put(r)
        }
        fn get(
            &self,
            r: &glass_managed::shared::GetBlob,
        ) -> Result<glass_managed::shared::SealedBlob, LinkError> {
            self.console.get(r)
        }
    }

    impl ChatLink for Held {
        fn register(&self, r: &glass_managed::chat::RegisterKey) -> Result<(), LinkError> {
            self.console.register(r)
        }
        fn lookup(&self, r: &LookupKeys) -> Result<glass_managed::chat::Keys, LinkError> {
            self.console.lookup(r)
        }
        fn create(&self, r: &CreateConversation) -> Result<u64, LinkError> {
            self.console.create(r)
        }
        fn post(&self, r: &PostMessage) -> Result<u64, LinkError> {
            use std::sync::atomic::Ordering::SeqCst;
            let answer = self.console.post(r);
            if self
                .lose
                .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err(LinkError::Unreachable("timed out".into()));
            }
            answer
        }
        fn rotate(&self, r: &glass_managed::chat::RotateEpoch) -> Result<u64, LinkError> {
            self.console.rotate(r)
        }
        fn sync(
            &self,
            r: &glass_managed::chat::ChatSyncRequest,
        ) -> Result<glass_managed::chat::ChatSyncResponse, LinkError> {
            let hold = self.hold.lock().unwrap().take();
            if let Some((waiting, go)) = hold {
                waiting.send(()).unwrap();
                go.recv().unwrap();
            }
            self.console.sync(r)
        }
    }

    // ChatSync.tla: a store that couldn't be kept moved the cursor on, so
    // what it held was never fetched again on this computer.
    #[test]
    fn cf91_what_a_full_disk_kept_out_arrives_once_there_is_room() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "The terms are in" }] }),
        )
        .unwrap();
        // Priya's store can't be written, as on a full disk.
        b.handle.fill_disk(true);
        let full = b.handle.sync_once(0);
        b.handle.fill_disk(false);
        b.handle.sync_once(0).unwrap();
        let list = b.call("priya", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["unread"], 1, "{}", list.text);
        assert!(full.is_err(), "glassd backs off: {full:?}");
        assert!(b.notices.lock().unwrap().iter().any(
            |n| matches!(n, Notice::Refused { reason, .. } if reason.contains("keeping chat"))
        ));
    }

    // ChatSync.tla: Sam starting chat while Priya's sync waited reset the
    // cursor to the beginning; the sync, begun at the beginning too, moved
    // it on past his first message.
    #[test]
    fn cf91_someone_starting_chat_during_a_sync_gets_what_was_sent_meanwhile() {
        let org = org();
        let held = Arc::new(Held::new(org.link.clone()));
        let (a, b) = (
            org.computer("dev-a"),
            org.computer_through("dev-b", held.clone()),
        );
        b.call("priya", LIST, json!({})).unwrap();
        let (waiting, waited) = std::sync::mpsc::channel();
        let (go, went) = std::sync::mpsc::channel();
        *held.hold.lock().unwrap() = Some((waiting, went));
        let polling = b.handle.clone();
        let poll = std::thread::spawn(move || polling.sync_once(25));
        waited.recv().unwrap();
        // While the long poll waits: Sam opens chat on the same computer and
        // Dana writes to him.
        b.call("sam", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["sam"], "text": "Welcome, Sam" }] }),
        )
        .unwrap();
        go.send(()).unwrap();
        poll.join().unwrap().unwrap();
        b.handle.sync_once(0).unwrap();
        let list = b.call("sam", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["unread"], 1, "{}", list.text);
    }

    // ChatSync.tla: the newest view marked everything up to Priya's own
    // reply seen, so Dana's message sent before it, fetched after, was never
    // unread.
    #[test]
    fn cf92_a_message_fetched_after_the_persons_own_reply_is_unread() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "Lunch?" }] }),
        )
        .unwrap();
        b.handle.sync_once(0).unwrap();
        let list = b.call("priya", LIST, json!({})).unwrap();
        let id = list.data["conversations"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        b.call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        let unread = |c: &Computer| {
            c.call("priya", LIST, json!({})).unwrap().data["conversations"][0]["unread"].clone()
        };
        assert_eq!(unread(&b), 0);
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "conversation": id, "text": "Or the board call at 1?" }] }),
        )
        .unwrap();
        // Priya answers the first before her computer has the second, and
        // her Chat shows the conversation.
        b.call(
            "priya",
            POST,
            json!({ "posts": [{ "conversation": id, "text": "Lunch, yes" }] }),
        )
        .unwrap();
        b.call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        b.handle.sync_once(0).unwrap();
        assert_eq!(unread(&b), 1, "Dana's second message wasn't shown");
        b.call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        assert_eq!(unread(&b), 0);
    }

    // Chat reads the open conversation again at every change, looking or
    // not: behind another window that read marked everything read, and the
    // person came back to no unread count for what they never saw.
    #[test]
    fn cf92_a_conversation_read_with_nobody_looking_stays_unread() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "Are you there?" }] }),
        )
        .unwrap();
        b.handle.sync_once(0).unwrap();
        let unread = |c: &Computer| {
            c.call("priya", LIST, json!({})).unwrap().data["conversations"][0]["unread"].clone()
        };
        let id = b.call("priya", LIST, json!({})).unwrap().data["conversations"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        // Her Chat has it open behind another window: it reads the same
        // messages, and they stay unread.
        let peeked = b
            .call("priya", READ, json!({ "conversation": id, "peek": true }))
            .unwrap();
        assert_eq!(peeked.data["messages"][0]["text"], "Are you there?");
        assert_eq!(unread(&b), 1);
        // She comes back to it.
        b.call("priya", READ, json!({ "conversation": id }))
            .unwrap();
        assert_eq!(unread(&b), 0);
    }

    // ChatPost.tla: the console took a message and its answer was lost.
    // Chat said "Nothing was queued; try again", and trying again posted it
    // twice.
    #[test]
    fn cf91_a_post_whose_answer_was_lost_is_posted_once() {
        use std::sync::atomic::Ordering::SeqCst;
        let org = org();
        let held = Arc::new(Held::new(org.link.clone()));
        let (a, b) = (
            org.computer_through("dev-a", held.clone()),
            org.computer("dev-b"),
        );
        b.call("priya", LIST, json!({})).unwrap();
        let post = |text: &str| {
            a.call(
                "dana",
                POST,
                json!({ "posts": [{ "to": ["priya"], "text": text }] }),
            )
        };
        held.lose.store(1, SeqCst);
        let sent = post("Signed");
        assert!(sent.is_ok(), "asked again with the same message: {sent:?}");
        assert_eq!(org.link.messages().len(), 1);

        // Both answers lost: the first was taken, the second refused as
        // posted already. Chat can't tell, and says so.
        held.lose.store(2, SeqCst);
        let maybe = post("Countersigned").unwrap_err().to_string();
        assert!(maybe.contains("may have been posted"), "{maybe}");
        assert_eq!(org.link.messages().len(), 2);
        b.handle.sync_once(0).unwrap();
        let list = b.call("priya", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["unread"], 2, "{}", list.text);
    }

    // A console sending a message again, in one answer or a later one, under
    // another number: kept once (absorb checks hashed headers now).
    #[test]
    fn cf91_a_message_sent_again_is_kept_once() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "Once" }] }),
        )
        .unwrap();
        let again = org.link.messages()[0].clone();
        org.link.inject(again.clone());
        b.handle.sync_once(0).unwrap();
        org.link.inject(again);
        b.handle.sync_once(0).unwrap();
        let list = b.call("priya", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["unread"], 1, "{}", list.text);
        let id = list.data["conversations"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let kept = &b.handle.load("priya").messages[&id];
        assert_eq!(kept.len(), 1);
    }

    // Audit 9.2: an action read the person's whole store from its file
    // five to eight times, parsing all of it each time.
    #[test]
    fn adr0052_a_persons_store_is_parsed_once_however_often_an_action_asks_for_it() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "The terms are in" }] }),
        )
        .unwrap();
        b.handle.sync_once(0).unwrap();
        b.call("priya", LIST, json!({})).unwrap();

        // From here every store is in memory as its file has it: a post, a
        // sync that brings it, a list and a read parse nothing.
        let (before_a, before_b) = (a.handle.parses(), b.handle.parses());
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "Signed, both sides" }] }),
        )
        .unwrap();
        b.handle.sync_once(0).unwrap();
        let list = b.call("priya", LIST, json!({})).unwrap();
        assert_eq!(list.data["conversations"][0]["unread"], 2, "{}", list.text);
        assert_eq!(a.handle.parses(), before_a, "dana's posting");
        assert_eq!(b.handle.parses(), before_b, "priya's sync and list");

        // What is in memory is what the file holds: a handle that reads
        // the file afresh (glassd started again) finds the same.
        let again = org.computer("dev-b");
        let list_again = again.call("priya", LIST, json!({})).unwrap();
        assert_eq!(list_again.data, list.data);
        assert!(again.handle.parses() > 0);

        // A file written by anything else is read again, not answered
        // from memory.
        let path = org.dir.path().join("dev-b/priya/store.json");
        let mut other = b.handle.load_to_change("priya");
        other.seen.clear();
        other.notify.insert("somewhere".into(), "none".into());
        std::fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
        let before = b.handle.parses();
        assert_eq!(b.handle.load("priya").notify["somewhere"], "none");
        assert_eq!(b.handle.parses(), before + 1);
    }

    // Audit 4.3: a store that couldn't be parsed was read as empty, and
    // the next save wrote over the only copy.
    #[test]
    fn cf91_a_store_that_isnt_one_is_set_aside_and_one_that_cant_be_read_isnt_written_over() {
        let org = org();
        let (a, b) = (org.computer("dev-a"), org.computer("dev-b"));
        b.call("priya", LIST, json!({})).unwrap();
        a.call(
            "dana",
            POST,
            json!({ "posts": [{ "to": ["priya"], "text": "The terms are in" }] }),
        )
        .unwrap();
        b.handle.sync_once(0).unwrap();
        let dir = org.dir.path().join("dev-b/priya");
        let path = dir.join("store.json");
        let whole = std::fs::read(&path).unwrap();

        // Cut short, as a power cut mid-write once left it.
        std::fs::write(&path, &whole[..whole.len() / 2]).unwrap();
        let store = b.handle.load("priya");
        assert!(store.conversations.is_empty());
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".damaged-"))
            .collect();
        assert_eq!(aside.len(), 1, "what was there is kept beside it");
        assert_eq!(
            std::fs::read(aside[0].path()).unwrap(),
            &whole[..whole.len() / 2]
        );
        // Chat starts again, and saves a new file.
        {
            let _write = b.handle.lock();
            let store = b.handle.load_to_change("priya");
            b.handle.save("priya", &store).unwrap();
        }
        assert!(path.is_file());

        // There, and not readable at all (here: not a file). Nothing is
        // written over it, and the person is told why.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("kept"), b"whatever this is").unwrap();
        assert!(b.handle.load("priya").conversations.is_empty());
        let refused = {
            let _write = b.handle.lock();
            let store = b.handle.load_to_change("priya");
            b.handle.save("priya", &store).unwrap_err()
        };
        assert!(
            matches!(&refused, ChatFailure::Local(why) if why.contains("couldn't be read, so nothing is written over it")),
            "{refused:?}"
        );
        assert_eq!(
            std::fs::read(path.join("kept")).unwrap(),
            b"whatever this is"
        );
        // Once it reads again, chat goes on.
        std::fs::remove_dir_all(&path).unwrap();
        b.handle.load("priya");
        {
            let _write = b.handle.lock();
            let store = b.handle.load_to_change("priya");
            b.handle.save("priya", &store).unwrap();
        }
    }
}
