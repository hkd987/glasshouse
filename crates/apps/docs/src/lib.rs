//! Docs (ADR 0035; DC-1 to DC-6): a clean block editor whose documents are
//! shared-space items, edited live by the people in them and read and
//! written by agents as Markdown.
//!
//! A doc is an item of kind `doc` reached through [`Shared`]. Its content
//! is one Yjs document: edits are `Update` parts (or blobs), the whole
//! state is a `Snapshot` blob from time to time, and the owner's `Roles`
//! part says who edits and who comments. This computer keeps each opened
//! doc's merged state in memory ([`yrs`]), so opening it again costs
//! nothing and agents read it as Markdown.
//!
//! - `doc.list`, `doc.read` (Auto): the person's docs; one as Markdown.
//! - `doc.create`, `doc.write`, `doc.comment` (Ask; others see them).
//! - `doc.open`, `doc.resolve`, `doc.version`, `doc.roles`: what the Docs
//!   app does for the person.
//! - Live edits ([`Live`]): the Docs app's pushes and pulls for a doc its
//!   session opened, audited once when the session closes.

pub mod markdown;
pub mod ydoc;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Live, Preview, Pulled, TaskContext,
    parse_input,
};
use glass_api::AskLabels;
use glass_managed::DirectoryHandle;
use glass_managed::base64;
use glass_managed::chat::{ConversationKind, MAX_TEXT_CHARS, is_conversation_id};
use glass_managed::shared::{
    Anchor, BlobKey, Item, ItemMessage, MAX_BLOB_BYTES, MAX_TITLE_CHARS, Part, Shared, new_blob_id,
};
use glass_types::{ActionName, Label, SideEffect, TaskId, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ydoc::At;

/// The Docs app: what the person does in it is theirs.
pub const APP: &str = "docs";

pub const LIST: &str = "doc.list";
pub const READ: &str = "doc.read";
pub const CREATE: &str = "doc.create";
pub const WRITE: &str = "doc.write";
pub const COMMENT: &str = "doc.comment";
pub const OPEN: &str = "doc.open";
pub const RESOLVE: &str = "doc.resolve";
pub const VERSION: &str = "doc.version";
pub const ROLES: &str = "doc.roles";

/// The largest update a message carries; larger ones go as blobs.
const MAX_INLINE_UPDATE: usize = 8 * 1024;
/// Updates between snapshots (ADR 0035 §2).
const SNAPSHOT_EVERY: usize = 200;
/// The longest version name and anchor.
const MAX_NAME: usize = 100;
const MAX_ANCHOR: usize = 512;

fn failed(why: impl std::fmt::Display) -> ActionError {
    ActionError::Failed(why.to_string())
}

fn invalid(why: impl Into<String>) -> ActionError {
    ActionError::InvalidInput(why.into())
}

/// What a person may do in a doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Commenter,
    Editor,
    Owner,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Commenter => "commenter",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }

    fn edits(self) -> bool {
        self >= Role::Editor
    }

    fn comments(self) -> bool {
        self >= Role::Commenter
    }
}

#[derive(Debug, Clone)]
struct Comment {
    id: String,
    by: String,
    attribution: Option<String>,
    text: String,
    anchor: Option<Anchor>,
    reply_to: Option<String>,
    resolved: bool,
    at: String,
}

#[derive(Debug, Clone)]
struct Version {
    id: String,
    name: String,
    by: String,
    at: String,
}

/// One doc as this computer knows it for one person: its merged state and
/// what its messages said.
#[derive(Debug)]
struct Cached {
    id: String,
    doc: yrs::Doc,
    /// The newest message applied.
    seq: u64,
    title: String,
    label: Label,
    owner: String,
    members: Vec<String>,
    editors: Vec<String>,
    commenters: Vec<String>,
    comments: Vec<Comment>,
    versions: Vec<Version>,
    /// Updates since the last snapshot.
    since_snapshot: usize,
    updated_at: String,
}

impl Cached {
    fn role(&self, login: &str) -> Role {
        if login == self.owner {
            Role::Owner
        } else if self.editors.iter().any(|e| e == login) {
            Role::Editor
        } else if self.commenters.iter().any(|c| c == login) {
            Role::Commenter
        } else {
            Role::Viewer
        }
    }
}

/// A doc an app session opened.
#[derive(Debug)]
struct Opened {
    title: String,
    role: Role,
    pushes: u32,
    since: Instant,
    /// Messages this session posted and the page that pushed each, so a
    /// pull doesn't echo a page's own edits but does bring another
    /// window's.
    posted: Vec<(String, String)>,
}

#[derive(Debug, Default)]
struct State {
    docs: BTreeMap<(String, String), Cached>,
    sessions: BTreeMap<TaskId, BTreeMap<String, Opened>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocInput {
    doc: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    title: String,
    #[serde(default)]
    label: Option<Label>,
    /// People who edit.
    #[serde(default)]
    with: Vec<String>,
    #[serde(default)]
    commenters: Vec<String>,
    #[serde(default)]
    viewers: Vec<String>,
    /// What it starts with.
    #[serde(default)]
    markdown: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WhereInput {
    Named(String),
    After { after: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    doc: String,
    markdown: String,
    /// "end" (the default), "replace", or { "after": "<heading>" }.
    #[serde(default)]
    at: Option<WhereInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentInput {
    doc: String,
    text: String,
    #[serde(default)]
    anchor: Option<Anchor>,
    #[serde(default)]
    reply_to: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveInput {
    doc: String,
    comment: String,
    #[serde(default = "yes")]
    resolved: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionInput {
    doc: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesInput {
    doc: String,
    editors: Vec<String>,
    #[serde(default)]
    commenters: Vec<String>,
}

/// Docs on this computer.
#[derive(Debug, Clone)]
pub struct DocsProvider {
    shared: Arc<dyn Shared>,
    directory: DirectoryHandle,
    state: Arc<Mutex<State>>,
}

impl DocsProvider {
    pub fn new(shared: Arc<dyn Shared>, directory: DirectoryHandle) -> Self {
        Self {
            shared,
            directory,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    fn me(ctx: &TaskContext) -> &str {
        &ctx.principal.on_behalf_of.name
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn ready(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        self.shared
            .ready(Self::me(ctx), ctx.principal.agent.as_str() == APP)
            .map_err(|e| failed(format!("The shared space isn't ready: {e}")))
    }

    fn name(&self, login: &str) -> String {
        self.directory
            .person(login)
            .map(|p| p.name)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| login.to_owned())
    }

    fn attribution(ctx: &TaskContext) -> Option<String> {
        let agent = ctx.principal.agent.as_str();
        (agent != APP).then(|| format!("by {agent} for {}", Self::me(ctx)))
    }

    fn check_id(id: &str) -> Result<(), ActionError> {
        if is_conversation_id(id) {
            Ok(())
        } else {
            Err(invalid("a doc's id is cv- and 32 hex digits"))
        }
    }

    // ---- reading docs --------------------------------------------------

    /// An update's bytes, from a message's part.
    fn update_bytes(&self, login: &str, item: &str, part: &Part) -> Option<Vec<u8>> {
        match part {
            Part::Update { data } => base64::decode(data),
            Part::UpdateBlob {
                blob, key, sha256, ..
            }
            | Part::Snapshot {
                blob, key, sha256, ..
            } => {
                let sealed = self.shared.get_blob(login, item, blob).ok()?;
                BlobKey::from_hex(key).ok()?.open(&sealed, sha256).ok()
            }
            _ => None,
        }
    }

    /// Applies `messages` (in order) to `cached`.
    fn absorb(&self, login: &str, cached: &mut Cached, messages: Vec<ItemMessage>) {
        let item = cached.id.clone();
        for m in messages {
            cached.seq = cached.seq.max(m.seq);
            cached.updated_at.clone_from(&m.at);
            let Some(part) = m.part else { continue };
            let role = cached.role(&m.sender);
            match &part {
                Part::Update { .. } | Part::UpdateBlob { .. } if role.edits() => {
                    if let Some(bytes) = self.update_bytes(login, &item, &part) {
                        let _ = ydoc::apply(&cached.doc, &[bytes]);
                        cached.since_snapshot += 1;
                    }
                }
                Part::Snapshot { name, .. } if role.edits() => {
                    if let Some(bytes) = self.update_bytes(login, &item, &part) {
                        let _ = ydoc::apply(&cached.doc, &[bytes]);
                        cached.since_snapshot = 0;
                    }
                    if let Some(name) = name {
                        cached.versions.push(Version {
                            id: m.id.clone(),
                            name: name.clone(),
                            by: m.sender.clone(),
                            at: m.at.clone(),
                        });
                    }
                }
                Part::Roles {
                    editors,
                    commenters,
                } if m.sender == cached.owner => {
                    let member = |l: &&String| cached.members.contains(l);
                    cached.editors = editors.iter().filter(member).cloned().collect();
                    cached.commenters = commenters.iter().filter(member).cloned().collect();
                }
                Part::Comment { anchor, .. } if role.comments() => cached.comments.push(Comment {
                    id: m.id.clone(),
                    by: m.sender.clone(),
                    attribution: m.attribution.clone(),
                    text: m.text.clone(),
                    anchor: anchor.clone(),
                    reply_to: m.reply_to.clone(),
                    resolved: false,
                    at: m.at.clone(),
                }),
                Part::Resolve { comment, resolved } if role.comments() => {
                    if let Some(c) = cached.comments.iter_mut().find(|c| &c.id == comment) {
                        c.resolved = *resolved;
                    }
                }
                _ => {}
            }
        }
    }

    /// Doc `id` for `login`, brought up to date, under the state lock.
    fn with_doc<T>(
        &self,
        login: &str,
        id: &str,
        f: impl FnOnce(&mut Cached) -> Result<T, ActionError>,
    ) -> Result<T, ActionError> {
        Self::check_id(id)?;
        let key = (login.to_owned(), id.to_owned());
        let mut state = self.state();
        match state.docs.get_mut(&key) {
            Some(cached) => {
                if let Some(item) = self.shared.since(login, id, cached.seq) {
                    cached.members = item.members.clone();
                    self.absorb(login, cached, item.messages);
                }
            }
            None => {
                let item = self
                    .shared
                    .item(login, id)
                    .filter(|i| i.kind == ConversationKind::Doc)
                    .ok_or_else(|| {
                        ActionError::NotFound(format!(
                            "no doc {id} is shared with you; doc.list shows them"
                        ))
                    })?;
                let cached = self.load(login, item);
                state.docs.insert(key.clone(), cached);
            }
        }
        let cached = state
            .docs
            .get_mut(&key)
            .ok_or_else(|| failed("lost the doc"))?;
        f(cached)
    }

    /// A doc from its whole log: its newest snapshot, then what came after.
    fn load(&self, login: &str, item: Item) -> Cached {
        let mut cached = Cached {
            id: item.id.clone(),
            doc: yrs::Doc::new(),
            seq: 0,
            title: item.title.clone(),
            label: item.label,
            owner: item.created_by.clone(),
            members: item.members.clone(),
            editors: Vec::new(),
            commenters: Vec::new(),
            comments: Vec::new(),
            versions: Vec::new(),
            since_snapshot: 0,
            updated_at: String::new(),
        };
        // Roles first, so the snapshot's sender is judged by them.
        let roles: Vec<ItemMessage> = item
            .messages
            .iter()
            .filter(|m| matches!(m.part, Some(Part::Roles { .. })))
            .cloned()
            .collect();
        self.absorb(login, &mut cached, roles);
        let newest = item.messages.iter().rposition(|m| {
            matches!(m.part, Some(Part::Snapshot { .. })) && cached.role(&m.sender).edits()
        });
        let mut skip = 0;
        if let Some(at) = newest {
            // Older edits are in it; comments, versions and roles are not.
            skip = at;
        }
        let messages: Vec<ItemMessage> = item
            .messages
            .into_iter()
            .enumerate()
            .filter(|(n, m)| {
                *n >= skip
                    || !matches!(
                        m.part,
                        Some(
                            Part::Update { .. }
                                | Part::UpdateBlob { .. }
                                | Part::Snapshot { name: None, .. }
                        )
                    )
            })
            .map(|(_, m)| m)
            .collect();
        self.absorb(login, &mut cached, messages);
        cached
    }

    fn people(&self, cached: &Cached) -> Vec<Value> {
        cached
            .members
            .iter()
            .map(|m| json!({ "login": m, "name": self.name(m), "role": cached.role(m).as_str() }))
            .collect()
    }

    fn comments(&self, cached: &Cached) -> Vec<Value> {
        cached
            .comments
            .iter()
            .map(|c| {
                json!({
                    "id": c.id, "by": c.by, "by_name": self.name(&c.by), "attribution": c.attribution,
                    "text": c.text, "anchor": c.anchor, "reply_to": c.reply_to, "resolved": c.resolved, "at": c.at,
                })
            })
            .collect()
    }

    // ---- writing docs --------------------------------------------------

    /// Seals and keeps `bytes` as a blob of `item`: its part's fields.
    fn blob(
        &self,
        login: &str,
        item: &str,
        bytes: &[u8],
    ) -> Result<(String, String, String, u64), ActionError> {
        let key = BlobKey::generate().map_err(failed)?;
        let id = new_blob_id().map_err(failed)?;
        let sealed = key
            .seal(item, &id, bytes)
            .map_err(|e| invalid(e.to_string()))?;
        self.shared.put_blob(login, &sealed).map_err(failed)?;
        Ok((
            id,
            key.to_hex(),
            hex::encode(glass_crypto::sha256(bytes)),
            bytes.len() as u64,
        ))
    }

    /// Posts `update` to `item`: inline when small, else as a blob. The
    /// message's id.
    fn post_update(
        &self,
        ctx: &TaskContext,
        item: &str,
        update: &[u8],
    ) -> Result<String, ActionError> {
        let me = Self::me(ctx);
        let part = if update.len() <= MAX_INLINE_UPDATE {
            Part::Update {
                data: base64::encode(update),
            }
        } else {
            let (blob, key, sha256, bytes) = self.blob(me, item, update)?;
            Part::UpdateBlob {
                blob,
                key,
                sha256,
                bytes,
            }
        };
        self.shared
            .post(me, item, "", Some(part), Self::attribution(ctx), None)
            .map_err(failed)
    }

    /// Posts the whole state when enough edits have piled up.
    fn maybe_snapshot(
        &self,
        ctx: &TaskContext,
        item: &str,
        cached: &mut Cached,
        name: Option<String>,
    ) -> Result<Option<String>, ActionError> {
        if name.is_none() && cached.since_snapshot < SNAPSHOT_EVERY {
            return Ok(None);
        }
        let me = Self::me(ctx);
        let state = ydoc::state(&cached.doc);
        if state.len() > MAX_BLOB_BYTES {
            // Too large for one blob until blobs are chunked (S1).
            return Ok(None);
        }
        let (blob, key, sha256, bytes) = self.blob(me, item, &state)?;
        let id = self
            .shared
            .post(
                me,
                item,
                "",
                Some(Part::Snapshot {
                    blob,
                    key,
                    sha256,
                    bytes,
                    upto: cached.seq,
                    name,
                }),
                Self::attribution(ctx),
                None,
            )
            .map_err(failed)?;
        cached.since_snapshot = 0;
        Ok(Some(id))
    }

    fn no_write_down(ctx: &TaskContext, cached: &Cached) -> Result<(), ActionError> {
        match ctx.data_label {
            Some(read) if read > cached.label => Err(ActionError::OutOfScope(format!(
                "this task has read {read} data, and {} is labeled {}",
                cached.title, cached.label
            ))),
            _ => Ok(()),
        }
    }

    fn audience(&self, me: &str, logins: &[String]) -> Result<Vec<String>, ActionError> {
        let mut people = Vec::new();
        for login in logins {
            if self.directory.person(login).is_none() {
                return Err(invalid(format!(
                    "{login} isn't in your organization's directory; docs stay inside the organization"
                )));
            }
            if login != me && !people.contains(login) {
                people.push(login.clone());
            }
        }
        Ok(people)
    }

    fn check_title(title: &str) -> Result<String, ActionError> {
        let title = title.trim();
        if title.is_empty()
            || title.chars().count() > MAX_TITLE_CHARS
            || title.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "a title is one line of 1 to {MAX_TITLE_CHARS} characters"
            )));
        }
        Ok(title.to_owned())
    }

    // ---- actions -------------------------------------------------------

    fn list(&self, ctx: &TaskContext) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let mut docs: Vec<Item> = self.shared.items(me, ConversationKind::Doc);
        docs.sort_by(|a, b| {
            let at = |i: &Item| i.messages.last().map(|m| m.at.clone()).unwrap_or_default();
            at(b).cmp(&at(a))
        });
        let mut text = format!(
            "{} doc{}.",
            docs.len(),
            if docs.len() == 1 { "" } else { "s" }
        );
        let views: Vec<Value> = docs
            .iter()
            .map(|item| {
                // Roles from the owner's newest Roles part.
                let roles = item
                    .messages
                    .iter()
                    .rev()
                    .filter(|m| m.sender == item.created_by)
                    .find_map(|m| match &m.part {
                        Some(Part::Roles { editors, commenters }) => Some((editors.clone(), commenters.clone())),
                        _ => None,
                    })
                    .unwrap_or_default();
                let role = |l: &str| {
                    if l == item.created_by {
                        Role::Owner
                    } else if roles.0.iter().any(|e| e == l) {
                        Role::Editor
                    } else if roles.1.iter().any(|c| c == l) {
                        Role::Commenter
                    } else {
                        Role::Viewer
                    }
                };
                text.push_str(&format!(
                    "\n- {} by {} ({}) [{}]",
                    item.title,
                    self.name(&item.created_by),
                    role(me).as_str(),
                    item.id
                ));
                json!({
                    "id": item.id,
                    "title": item.title,
                    "label": item.label,
                    "owner": item.created_by,
                    "owner_name": self.name(&item.created_by),
                    "mine": item.created_by == me,
                    "role": role(me).as_str(),
                    "people": item.members.iter().map(|m| json!({ "login": m, "name": self.name(m), "role": role(m).as_str() })).collect::<Vec<_>>(),
                    "updated_at": item.messages.last().map(|m| &m.at),
                })
            })
            .collect();
        Ok(ActionOutput {
            text,
            data: json!({ "docs": views }),
            changes: Vec::new(),
        })
    }

    fn read(&self, ctx: &TaskContext, input: DocInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        self.with_doc(me, &input.doc, |cached| {
            let markdown = markdown::render(&ydoc::blocks(&cached.doc));
            let open: Vec<&Comment> = cached.comments.iter().filter(|c| !c.resolved).collect();
            let mut text = format!(
                "{} ({}, by {}, labeled {}):\n\n{markdown}",
                cached.title,
                cached.role(me).as_str(),
                self.name(&cached.owner),
                cached.label
            );
            if !open.is_empty() {
                text.push_str("\nOpen comments:");
                for c in open {
                    text.push_str(&format!("\n- {}: {} [{}]", self.name(&c.by), c.text, c.id));
                }
            }
            Ok(ActionOutput {
                text,
                data: json!({
                    "id": input.doc, "title": cached.title, "label": cached.label,
                    "role": cached.role(me).as_str(), "markdown": markdown,
                    "comments": self.comments(cached),
                }),
                changes: Vec::new(),
            })
        })
    }

    fn create(&self, ctx: &TaskContext, input: CreateInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let title = Self::check_title(&input.title)?;
        let editors = self.audience(me, &input.with)?;
        let commenters = self.audience(me, &input.commenters)?;
        let viewers = self.audience(me, &input.viewers)?;
        let mut members: Vec<String> = editors
            .iter()
            .chain(&commenters)
            .chain(&viewers)
            .cloned()
            .collect();
        members.sort();
        members.dedup();
        let editors: Vec<String> = editors
            .into_iter()
            .filter(|e| !commenters.contains(e) && !viewers.contains(e))
            .collect();
        let commenters: Vec<String> = commenters
            .into_iter()
            .filter(|c| !viewers.contains(c))
            .collect();
        let label = input
            .label
            .unwrap_or(Label::Internal)
            .max(ctx.data_label.unwrap_or(Label::Public));
        let start = input
            .markdown
            .as_deref()
            .map(markdown::parse)
            .unwrap_or_default();
        let id = self
            .shared
            .create(me, ConversationKind::Doc, &title, label, &members)
            .map_err(failed)?;
        self.shared
            .post(
                me,
                &id,
                "",
                Some(Part::Roles {
                    editors: editors.clone(),
                    commenters: commenters.clone(),
                }),
                Self::attribution(ctx),
                None,
            )
            .map_err(failed)?;
        if !start.is_empty() {
            let doc = yrs::Doc::new();
            let by = Self::attribution(ctx);
            let update = ydoc::write(&doc, &start, &At::End, by.as_deref().unwrap_or(me))
                .map_err(invalid)?;
            self.post_update(ctx, &id, &update)?;
        }
        let names: Vec<String> = members.iter().map(|p| self.name(p)).collect();
        Ok(ActionOutput {
            text: if names.is_empty() {
                format!("Made {title}; only you can see it.")
            } else {
                format!("Made {title}, shared with {}.", names.join(", "))
            },
            data: json!({ "id": id, "title": title, "label": label, "editors": editors, "commenters": commenters }),
            changes: Vec::new(),
        })
    }

    fn write(&self, ctx: &TaskContext, input: WriteInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        if input.markdown.trim().is_empty() {
            return Err(invalid("say what to write, as Markdown"));
        }
        let at = match input.at {
            None => At::End,
            Some(WhereInput::Named(s)) if s == "end" => At::End,
            Some(WhereInput::Named(s)) if s == "replace" => At::Replace,
            Some(WhereInput::After { after }) => At::AfterSection(after),
            Some(WhereInput::Named(other)) => {
                return Err(invalid(format!(
                    "at is \"end\", \"replace\" or {{\"after\": \"<heading>\"}}, not {other:?}"
                )));
            }
        };
        let blocks = markdown::parse(&input.markdown);
        let by = Self::attribution(ctx).unwrap_or_else(|| me.to_owned());
        self.with_doc(me, &input.doc, |cached| {
            if !cached.role(me).edits() {
                return Err(invalid(format!(
                    "{} is {}'s; you can't edit it",
                    cached.title,
                    self.name(&cached.owner)
                )));
            }
            Self::no_write_down(ctx, cached)?;
            let update = ydoc::write(&cached.doc, &blocks, &at, &by).map_err(invalid)?;
            let id = self.post_update(ctx, &input.doc, &update)?;
            cached.since_snapshot += 1;
            self.maybe_snapshot(ctx, &input.doc, cached, None)?;
            Ok(ActionOutput {
                text: format!(
                    "Wrote {} block{} into {}.",
                    blocks.len(),
                    if blocks.len() == 1 { "" } else { "s" },
                    cached.title
                ),
                data: json!({ "id": id, "doc": input.doc }),
                changes: Vec::new(),
            })
        })
    }

    fn comment(&self, ctx: &TaskContext, input: CommentInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let text = input.text.trim();
        if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
            return Err(invalid(format!(
                "a comment is 1 to {MAX_TEXT_CHARS} characters"
            )));
        }
        if let Some(anchor) = &input.anchor
            && (anchor
                .path
                .as_ref()
                .is_some_and(|p| p.len() > MAX_ANCHOR || base64::decode(p).is_none())
                || anchor.x.is_some()
                || anchor.y.is_some())
        {
            return Err(invalid(
                "a doc comment's anchor is a Yjs relative position, base64",
            ));
        }
        self.with_doc(me, &input.doc, |cached| {
            if !cached.role(me).comments() {
                return Err(invalid(format!(
                    "you can view {} but not comment",
                    cached.title
                )));
            }
            Self::no_write_down(ctx, cached)?;
            if let Some(to) = &input.reply_to
                && !cached.comments.iter().any(|c| &c.id == to)
            {
                return Err(invalid(format!("{} has no comment {to}", cached.title)));
            }
            let id = self
                .shared
                .post(
                    me,
                    &input.doc,
                    text,
                    Some(Part::Comment {
                        version: 0,
                        anchor: input.anchor.clone(),
                    }),
                    Self::attribution(ctx),
                    input.reply_to.clone(),
                )
                .map_err(failed)?;
            Ok(ActionOutput {
                text: format!("Commented on {}.", cached.title),
                data: json!({ "id": id }),
                changes: Vec::new(),
            })
        })
    }

    fn open(&self, ctx: &TaskContext, input: DocInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let (out, title, role) = self.with_doc(me, &input.doc, |cached| {
            let role = cached.role(me);
            Ok((
                json!({
                    "id": input.doc,
                    "title": cached.title,
                    "label": cached.label,
                    "owner": cached.owner,
                    "owner_name": self.name(&cached.owner),
                    "role": role.as_str(),
                    "people": self.people(cached),
                    "state": base64::encode(&ydoc::state(&cached.doc)),
                    "cursor": cached.seq,
                    "versions": cached.versions.iter().map(|v| json!({ "id": v.id, "name": v.name, "by_name": self.name(&v.by), "at": v.at })).collect::<Vec<_>>(),
                    "comments": self.comments(cached),
                }),
                cached.title.clone(),
                role,
            ))
        })?;
        self.state()
            .sessions
            .entry(ctx.principal.task)
            .or_default()
            .entry(input.doc.clone())
            .and_modify(|o| {
                // Opened again, as another window does or after a change of
                // roles: the role it has now.
                o.role = role;
                o.title.clone_from(&title);
            })
            .or_insert_with(|| Opened {
                title: title.clone(),
                role,
                pushes: 0,
                since: Instant::now(),
                posted: Vec::new(),
            });
        Ok(ActionOutput {
            text: format!("Opened {title} ({}).", role.as_str()),
            data: out,
            changes: Vec::new(),
        })
    }

    fn resolve(&self, ctx: &TaskContext, input: ResolveInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        self.with_doc(me, &input.doc, |cached| {
            if !cached.role(me).comments() || !cached.comments.iter().any(|c| c.id == input.comment)
            {
                return Err(invalid(format!(
                    "{} has no comment {} you can resolve",
                    cached.title, input.comment
                )));
            }
            self.shared
                .post(
                    me,
                    &input.doc,
                    "",
                    Some(Part::Resolve {
                        comment: input.comment.clone(),
                        resolved: input.resolved,
                    }),
                    Self::attribution(ctx),
                    None,
                )
                .map_err(failed)?;
            Ok(ActionOutput {
                text: format!(
                    "{} a comment on {}.",
                    if input.resolved {
                        "Resolved"
                    } else {
                        "Reopened"
                    },
                    cached.title
                ),
                data: json!({}),
                changes: Vec::new(),
            })
        })
    }

    fn version(&self, ctx: &TaskContext, input: VersionInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let name = input.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "a version's name is one line of 1 to {MAX_NAME} characters"
            )));
        }
        self.with_doc(me, &input.doc, |cached| {
            if !cached.role(me).edits() {
                return Err(invalid(format!(
                    "only {}'s editors name its versions",
                    cached.title
                )));
            }
            let id = self
                .maybe_snapshot(ctx, &input.doc, cached, Some(name.clone()))?
                .ok_or_else(|| failed("this doc is too large to keep as one version yet"))?;
            Ok(ActionOutput {
                text: format!("Named this version of {} \"{name}\".", cached.title),
                data: json!({ "id": id }),
                changes: Vec::new(),
            })
        })
    }

    fn roles(&self, ctx: &TaskContext, input: RolesInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        self.with_doc(me, &input.doc, |cached| {
            if cached.owner != me {
                return Err(invalid(format!(
                    "{} is {}'s to share",
                    cached.title,
                    self.name(&cached.owner)
                )));
            }
            let outside: Vec<&String> = input
                .editors
                .iter()
                .chain(&input.commenters)
                .filter(|l| !cached.members.contains(l))
                .collect();
            if !outside.is_empty() {
                return Err(invalid(format!(
                    "{} isn't in {}; who is in a doc is fixed when it's made, for now",
                    outside
                        .iter()
                        .map(|l| self.name(l))
                        .collect::<Vec<_>>()
                        .join(", "),
                    cached.title
                )));
            }
            self.shared
                .post(
                    me,
                    &input.doc,
                    "",
                    Some(Part::Roles {
                        editors: input.editors.clone(),
                        commenters: input.commenters.clone(),
                    }),
                    Self::attribution(ctx),
                    None,
                )
                .map_err(failed)?;
            Ok(ActionOutput {
                text: format!("Updated who edits {}.", cached.title),
                data: json!({}),
                changes: Vec::new(),
            })
        })
    }

    /// A doc's title and label for a preview.
    fn about(&self, ctx: &TaskContext, id: &str) -> (String, Option<Label>) {
        self.with_doc(Self::me(ctx), id, |c| Ok((c.title.clone(), Some(c.label))))
            .unwrap_or_else(|_| ("a doc".into(), None))
    }
}

impl ActionProvider for DocsProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let doc = json!({ "type": "string", "pattern": "^cv-[0-9a-f]{32}$", "description": "A doc's id from doc.list" });
        let logins = json!({ "type": "array", "items": { "type": "string" } });
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
        let doc_only = json!({ "type": "object", "properties": { "doc": doc }, "required": ["doc"], "additionalProperties": false });
        vec![
            def(
                LIST,
                "See docs",
                "The person's docs and the ones shared with them: title, owner, their role, newest first.",
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                SideEffect::None,
                Tier::Auto,
                AskLabels::default(),
            ),
            def(
                READ,
                "Read docs",
                "One doc as Markdown, with its open comments.",
                doc_only.clone(),
                SideEffect::None,
                Tier::Auto,
                AskLabels::default(),
            ),
            def(
                CREATE,
                "Make docs",
                "A new doc, shared with people in the organization as editors (with), commenters or viewers, optionally starting with Markdown. Who is in it is fixed once it's made.",
                json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE_CHARS },
                        "label": { "enum": ["public", "internal", "confidential", "restricted"] },
                        "with": logins, "commenters": logins, "viewers": logins,
                        "markdown": { "type": "string" }
                    },
                    "required": ["title"],
                    "additionalProperties": false
                }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the doc", "Make it"),
            ),
            def(
                WRITE,
                "Write in docs",
                "Writes Markdown into a doc the person edits: at the end, after the section a heading starts ({\"after\": \"Heading\"}), or replacing everything. Everyone in it sees it at once, marked as written by the agent.",
                json!({
                    "type": "object",
                    "properties": {
                        "doc": doc,
                        "markdown": { "type": "string", "minLength": 1 },
                        "at": { "oneOf": [ { "enum": ["end", "replace"] }, { "type": "object", "properties": { "after": { "type": "string" } }, "required": ["after"], "additionalProperties": false } ] }
                    },
                    "required": ["doc", "markdown"],
                    "additionalProperties": false
                }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the edit", "Write it"),
            ),
            def(
                COMMENT,
                "Comment on docs",
                "A comment on a doc, or a reply to one.",
                json!({
                    "type": "object",
                    "properties": {
                        "doc": doc,
                        "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS },
                        "anchor": { "type": "object", "properties": { "path": { "type": "string", "maxLength": MAX_ANCHOR } }, "additionalProperties": false },
                        "reply_to": { "type": "string" }
                    },
                    "required": ["doc", "text"],
                    "additionalProperties": false
                }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the comment", "Comment"),
            ),
            def(
                OPEN,
                "Open docs to edit",
                "The Docs app opens a doc to edit it live: its whole state, cursor, comments and versions.",
                doc_only,
                SideEffect::None,
                Tier::Auto,
                AskLabels::default(),
            ),
            def(
                RESOLVE,
                "Resolve comments on docs",
                "Marks a comment on a doc resolved, or reopens it.",
                json!({ "type": "object", "properties": { "doc": doc, "comment": { "type": "string" }, "resolved": { "type": "boolean" } }, "required": ["doc", "comment"], "additionalProperties": false }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the resolve", "Resolve"),
            ),
            def(
                VERSION,
                "Name versions of docs",
                "Names the doc as it is now, as a version in its history.",
                json!({ "type": "object", "properties": { "doc": doc, "name": { "type": "string", "minLength": 1, "maxLength": MAX_NAME } }, "required": ["doc", "name"], "additionalProperties": false }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the version", "Name it"),
            ),
            def(
                ROLES,
                "Choose who edits docs",
                "The owner chooses who in a doc edits and who comments; everyone else views.",
                json!({ "type": "object", "properties": { "doc": doc, "editors": logins, "commenters": logins }, "required": ["doc", "editors"], "additionalProperties": false }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the roles", "Change"),
            ),
        ]
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        let titled = |summary: &str, id: &str, read: bool| {
            let (title, label) = self.about(ctx, id);
            Preview {
                summary: format!("{summary} {title}"),
                targets: vec![title],
                // Reading a doc raises the task's label to its own.
                label: if read { label } else { None },
                ..Preview::default()
            }
        };
        match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                Ok(Preview {
                    summary: "See docs".into(),
                    targets: vec!["Docs".into()],
                    ..Preview::default()
                })
            }
            READ => Ok(titled("Read", &parse_input::<DocInput>(input)?.doc, true)),
            OPEN => Ok(titled("Open", &parse_input::<DocInput>(input)?.doc, true)),
            CREATE => {
                let input: CreateInput = parse_input(input)?;
                let title = Self::check_title(&input.title)?;
                let me = Self::me(ctx);
                let mut items = Vec::new();
                for (who, list) in [
                    ("Edit", &input.with),
                    ("Comment", &input.commenters),
                    ("View", &input.viewers),
                ] {
                    let people = self.audience(me, list)?;
                    if !people.is_empty() {
                        items.push(format!(
                            "{who}: {}",
                            people
                                .iter()
                                .map(|p| self.name(p))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
                if items.is_empty() {
                    items.push("Only you can see it".into());
                }
                if let Some(md) = &input.markdown {
                    items.push(format!(
                        "Starts with {} block{}",
                        markdown::parse(md).len(),
                        if markdown::parse(md).len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    ));
                }
                Ok(Preview {
                    summary: format!("Make the doc {title}"),
                    targets: vec![title],
                    items,
                    ..Preview::default()
                })
            }
            WRITE => {
                let parsed: WriteInput = parse_input(input)?;
                let mut preview = titled("Write in", &parsed.doc, false);
                preview.items = parsed
                    .markdown
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(12)
                    .map(str::to_owned)
                    .collect();
                Ok(preview)
            }
            COMMENT => {
                let parsed: CommentInput = parse_input(input)?;
                let mut preview = titled("Comment on", &parsed.doc, false);
                preview.items = vec![parsed.text.trim().to_owned()];
                Ok(preview)
            }
            RESOLVE => Ok(titled(
                "Resolve a comment on",
                &parse_input::<ResolveInput>(input)?.doc,
                false,
            )),
            VERSION => Ok(titled(
                "Name a version of",
                &parse_input::<VersionInput>(input)?.doc,
                false,
            )),
            ROLES => Ok(titled(
                "Change who edits",
                &parse_input::<RolesInput>(input)?.doc,
                false,
            )),
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                self.list(ctx)
            }
            READ => self.read(ctx, parse_input(input)?),
            CREATE => self.create(ctx, parse_input(input)?),
            WRITE => self.write(ctx, parse_input(input)?),
            COMMENT => self.comment(ctx, parse_input(input)?),
            OPEN => self.open(ctx, parse_input(input)?),
            RESOLVE => self.resolve(ctx, parse_input(input)?),
            VERSION => self.version(ctx, parse_input(input)?),
            ROLES => self.roles(ctx, parse_input(input)?),
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }
}

impl Live for DocsProvider {
    fn app(&self) -> &'static str {
        APP
    }

    fn push(
        &self,
        ctx: &TaskContext,
        item: &str,
        client: &str,
        update: &str,
    ) -> Result<u64, ActionError> {
        let me = Self::me(ctx);
        let session = ctx.principal.task;
        let role = self
            .state()
            .sessions
            .get(&session)
            .and_then(|s| s.get(item))
            .map(|o| o.role)
            .ok_or_else(|| invalid("this doc isn't open in this window; open it again"))?;
        if !role.edits() {
            return Err(invalid("you can view this doc but not edit it"));
        }
        let bytes = base64::decode(update).ok_or_else(|| invalid("an update is base64"))?;
        if !ydoc::is_update(&bytes) {
            return Err(invalid("that isn't a Yjs update"));
        }
        let (id, cursor) = self.with_doc(me, item, |cached| {
            // The role now, not only as opened: a demotion holds at once.
            if !cached.role(me).edits() {
                return Err(invalid("you can view this doc but not edit it"));
            }
            Self::no_write_down(ctx, cached)?;
            ydoc::apply(&cached.doc, std::slice::from_ref(&bytes)).map_err(invalid)?;
            let id = self.post_update(ctx, item, &bytes)?;
            cached.since_snapshot += 1;
            self.maybe_snapshot(ctx, item, cached, None)?;
            Ok((id, cached.seq))
        })?;
        if let Some(opened) = self
            .state()
            .sessions
            .get_mut(&session)
            .and_then(|s| s.get_mut(item))
        {
            opened.pushes += 1;
            opened.posted.push((client.to_owned(), id));
        }
        Ok(cursor)
    }

    fn pull(
        &self,
        ctx: &TaskContext,
        item: &str,
        client: &str,
        since: u64,
    ) -> Result<Pulled, ActionError> {
        let me = Self::me(ctx);
        let session = ctx.principal.task;
        let posted: Vec<String> = self
            .state()
            .sessions
            .get(&session)
            .and_then(|s| s.get(item))
            .map(|o| {
                o.posted
                    .iter()
                    .filter(|(by, _)| by == client)
                    .map(|(_, id)| id.clone())
                    .collect()
            })
            .ok_or_else(|| invalid("this doc isn't open in this window; open it again"))?;
        let fresh = self
            .shared
            .since(me, item, since)
            .ok_or_else(|| invalid("no such doc"))?;
        // Bring the cache up to date too, and judge senders by its roles.
        self.with_doc(me, item, |cached| {
            let mut updates = Vec::new();
            let mut cursor = since;
            for m in &fresh.messages {
                cursor = cursor.max(m.seq);
                let Some(part) = &m.part else { continue };
                if !matches!(part, Part::Update { .. } | Part::UpdateBlob { .. } | Part::Snapshot { .. })
                    || !cached.role(&m.sender).edits()
                    || posted.contains(&m.id)
                {
                    continue;
                }
                if let Some(bytes) = self.update_bytes(me, item, part) {
                    updates.push(json!({ "update": base64::encode(&bytes), "by": m.sender, "by_name": self.name(&m.sender) }));
                }
            }
            Ok(Pulled { updates, cursor })
        })
    }

    fn closed(&self, session: &TaskId) -> Vec<String> {
        let opened = self.state().sessions.remove(session).unwrap_or_default();
        opened
            .into_values()
            .filter(|o| o.pushes > 0)
            .map(|o| {
                let minutes = o.since.elapsed().as_secs().div_ceil(60).max(1);
                format!(
                    "{}: {} change{} in {} minute{}",
                    o.title,
                    o.pushes,
                    if o.pushes == 1 { "" } else { "s" },
                    minutes,
                    if minutes == 1 { "" } else { "s" }
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
