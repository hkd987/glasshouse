//! Artifacts (ADR 0033; AR-1 to AR-5): what people and their agents make
//! to be looked at (an HTML page, a chart, a small tool, a report),
//! shared with a home instead of dropped into chat as a file.
//!
//! An artifact is a conversation of kind `artifact` in the org's shared
//! space, reached through [`Shared`] (chat's store implements it, so this
//! crate doesn't depend on Chat). Its members are its audience and its
//! sealed name is its title. Each version is a sealed blob named by a
//! sealed message; comments, resolves and reactions are messages too.
//! glassd opens a version for a member and the person's app bridge serves
//! it, sandboxed, on an origin of its own: it is shown, never downloaded.
//!
//! - `artifact.list`, `artifact.read` (Auto): what was shared with the
//!   person and what they shared; one artifact's page and its feedback.
//! - `artifact.share`, `artifact.update`, `artifact.comment`,
//!   `artifact.resolve` (Ask; others see it): a new artifact, a new
//!   version, a comment, a comment resolved or reopened.
//!
//! The Artifacts app's own `[[app]]` tiers make what the person does in
//! it Auto, as Chat's do; agents ask.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext, parse_input,
};
use glass_api::AskLabels;
use glass_managed::DirectoryHandle;
use glass_managed::chat::{ConversationKind, MAX_TEXT_CHARS, is_conversation_id};
use glass_managed::shared::{
    Anchor, BlobKey, Item, ItemMessage, MAX_BLOB_BYTES, MAX_TITLE_CHARS, Part, Shared, new_blob_id,
};
use glass_platform::fs::ConfinedDir;
use glass_types::{ActionName, Label, SideEffect, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

/// The Artifacts app: what the person does in it is theirs.
pub const APP: &str = "artifacts";

pub const LIST: &str = "artifact.list";
pub const READ: &str = "artifact.read";
pub const SHARE: &str = "artifact.share";
pub const UPDATE: &str = "artifact.update";
pub const COMMENT: &str = "artifact.comment";
pub const RESOLVE: &str = "artifact.resolve";

/// What an artifact is, for now.
const HTML: &str = "text/html";

/// The longest element path a pinned comment keeps.
const MAX_ANCHOR_PATH: usize = 512;

fn failed(why: impl std::fmt::Display) -> ActionError {
    ActionError::Failed(why.to_string())
}

fn invalid(why: impl Into<String>) -> ActionError {
    ActionError::InvalidInput(why.into())
}

/// One version, as its message names it.
#[derive(Debug, Clone)]
struct Version {
    number: u32,
    blob: String,
    key: String,
    sha256: String,
    bytes: u64,
    note: String,
    by: String,
    attribution: Option<String>,
    at: String,
}

/// One comment, with whether it is resolved now.
#[derive(Debug, Clone)]
struct Comment {
    id: String,
    by: String,
    attribution: Option<String>,
    text: String,
    version: u32,
    anchor: Option<Anchor>,
    reply_to: Option<String>,
    resolved: bool,
    at: String,
}

/// An artifact as its messages tell it.
#[derive(Debug, Clone)]
struct Artifact {
    id: String,
    title: String,
    label: Label,
    owner: String,
    members: Vec<String>,
    versions: Vec<Version>,
    comments: Vec<Comment>,
}

impl Artifact {
    fn from_item(item: Item) -> Self {
        let mut versions = Vec::new();
        let mut comments: Vec<Comment> = Vec::new();
        let mut resolved: BTreeMap<String, bool> = BTreeMap::new();
        for ItemMessage {
            id,
            sender,
            at,
            text,
            attribution,
            reply_to,
            part,
            ..
        } in item.messages
        {
            match part {
                Some(Part::Version {
                    number,
                    blob,
                    key,
                    sha256,
                    bytes,
                    mime,
                    ..
                }) if mime == HTML && sender == item.created_by => versions.push(Version {
                    number,
                    blob,
                    key,
                    sha256,
                    bytes,
                    note: text,
                    by: sender,
                    attribution,
                    at,
                }),
                Some(Part::Comment { version, anchor }) => comments.push(Comment {
                    id,
                    by: sender,
                    attribution,
                    text,
                    version,
                    anchor,
                    reply_to,
                    resolved: false,
                    at,
                }),
                Some(Part::Resolve {
                    comment,
                    resolved: now,
                }) => {
                    resolved.insert(comment, now);
                }
                _ => {}
            }
        }
        // Only its maker adds versions, numbered in order.
        versions.sort_by_key(|v| v.number);
        versions.dedup_by_key(|v| v.number);
        for comment in &mut comments {
            comment.resolved = resolved.get(&comment.id).copied().unwrap_or(false);
        }
        Self {
            id: item.id,
            title: item.title,
            label: item.label,
            owner: item.created_by,
            members: item.members,
            versions,
            comments,
        }
    }

    fn latest(&self) -> Option<&Version> {
        self.versions.last()
    }

    fn version(&self, number: Option<u32>) -> Option<&Version> {
        match number {
            Some(n) => self.versions.iter().find(|v| v.number == n),
            None => self.latest(),
        }
    }

    /// Threads still open: a reply belongs to its thread, which is
    /// resolved or not as a whole.
    fn open_comments(&self) -> usize {
        self.comments
            .iter()
            .filter(|c| c.reply_to.is_none() && !c.resolved)
            .count()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    artifact: String,
    #[serde(default)]
    version: Option<u32>,
}

/// The page itself, or a file in a granted folder.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Content {
    #[serde(default)]
    html: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareInput {
    title: String,
    #[serde(flatten)]
    content: Content,
    /// Logins of people in the org.
    #[serde(default)]
    with: Vec<String>,
    /// A chat conversation: its people, and a card there.
    #[serde(default)]
    conversation: Option<String>,
    #[serde(default)]
    note: Option<String>,
    /// Internal unless said; never below what the task has read.
    #[serde(default)]
    label: Option<Label>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateInput {
    artifact: String,
    #[serde(flatten)]
    content: Content,
    note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentInput {
    artifact: String,
    text: String,
    #[serde(default)]
    anchor: Option<Anchor>,
    #[serde(default)]
    reply_to: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveInput {
    artifact: String,
    comment: String,
    #[serde(default = "yes")]
    resolved: bool,
}

fn yes() -> bool {
    true
}

/// Artifacts on this computer.
#[derive(Debug, Clone)]
pub struct ArtifactsProvider {
    shared: Arc<dyn Shared>,
    directory: DirectoryHandle,
    /// One update at a time on this computer: each reads the newest
    /// version's number and posts the next (ArtifactsShare.tla).
    updating: Arc<Mutex<()>>,
}

impl ArtifactsProvider {
    pub fn new(shared: Arc<dyn Shared>, directory: DirectoryHandle) -> Self {
        Self {
            shared,
            directory,
            updating: Arc::new(Mutex::new(())),
        }
    }

    fn me(ctx: &TaskContext) -> &str {
        &ctx.principal.on_behalf_of.name
    }

    /// The Artifacts app makes the person's keys the first time it opens;
    /// an agent never does.
    fn ready(&self, ctx: &TaskContext) -> Result<(), ActionError> {
        self.shared
            .ready(Self::me(ctx), ctx.principal.agent.as_str() == APP)
            .map_err(|e| failed(e.to_string()))
    }

    fn name(&self, login: &str) -> String {
        self.directory
            .person(login)
            .map(|p| p.name)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| login.to_owned())
    }

    /// `login`'s artifacts. One whose first version never arrived (its
    /// share failed partway) isn't one.
    fn artifacts(&self, login: &str) -> Vec<Artifact> {
        self.shared
            .items(login, ConversationKind::Artifact)
            .into_iter()
            .map(Artifact::from_item)
            .filter(|a| !a.versions.is_empty())
            .collect()
    }

    /// Artifact `id`, opening only it.
    fn artifact(&self, login: &str, id: &str) -> Result<Artifact, ActionError> {
        if !is_conversation_id(id) {
            return Err(invalid("an artifact's id is cv- and 32 hex digits"));
        }
        self.shared
            .item(login, id)
            .filter(|item| item.kind == ConversationKind::Artifact)
            .map(Artifact::from_item)
            .filter(|a| !a.versions.is_empty())
            .ok_or_else(|| {
                ActionError::NotFound(format!(
                    "no artifact {id} is shared with you; artifact.list shows them"
                ))
            })
    }

    /// Version `version` (the latest when none) of artifact `id`, opened
    /// for `login`: its number and HTML.
    pub fn page(
        &self,
        login: &str,
        id: &str,
        version: Option<u32>,
    ) -> Result<(u32, String), ActionError> {
        self.page_of(login, &self.artifact(login, id)?, version)
    }

    fn page_of(
        &self,
        login: &str,
        artifact: &Artifact,
        version: Option<u32>,
    ) -> Result<(u32, String), ActionError> {
        let v = artifact.version(version).ok_or_else(|| {
            ActionError::NotFound(format!("{} has no such version", artifact.title))
        })?;
        let blob = self
            .shared
            .get_blob(login, &artifact.id, &v.blob)
            .map_err(|e| failed(format!("Fetching {}: {e}", artifact.title)))?;
        let bytes = BlobKey::from_hex(&v.key)
            .and_then(|key| key.open(&blob, &v.sha256))
            .map_err(|e| failed(format!("Opening {}: {e}", artifact.title)))?;
        let html = String::from_utf8(bytes)
            .map_err(|_| failed(format!("{} isn't text", artifact.title)))?;
        Ok((v.number, html))
    }

    /// The page an input names, read from a granted folder through a
    /// confined directory when it names a path.
    fn content(&self, ctx: &TaskContext, content: &Content) -> Result<String, ActionError> {
        match (&content.html, &content.path) {
            (Some(html), None) => {
                if html.len() > MAX_BLOB_BYTES {
                    return Err(invalid(format!(
                        "the page is {} KB; an artifact is {} KB at most",
                        html.len().div_ceil(1024),
                        MAX_BLOB_BYTES / 1024
                    )));
                }
                Ok(html.clone())
            }
            (None, Some(path)) => {
                let resolved = ctx.resolve(path)?;
                let dir = ConfinedDir::open(&resolved.grant.root)
                    .map_err(|e| ActionError::io(&resolved.grant.name, e))?;
                let bytes = dir
                    .read(&resolved.rel, MAX_BLOB_BYTES as u64)
                    .map_err(|e| ActionError::io(&resolved.display, e))?;
                String::from_utf8(bytes)
                    .map_err(|_| invalid(format!("{} isn't text", resolved.display)))
            }
            _ => Err(invalid(
                "give the page as html, or the path of a file in a granted folder",
            )),
        }
    }

    /// The host path a share or update reads, for the broker's label check.
    fn reads(ctx: &TaskContext, content: &Content) -> Vec<std::path::PathBuf> {
        content
            .path
            .as_deref()
            .and_then(|p| ctx.resolve(p).ok())
            .map(|r| vec![r.host])
            .unwrap_or_default()
    }

    fn attribution(ctx: &TaskContext) -> Option<String> {
        let agent = ctx.principal.agent.as_str();
        (agent != APP).then(|| format!("by {agent} for {}", Self::me(ctx)))
    }

    /// A new artifact's label: the one asked for (internal unless said),
    /// and never below what the task has read (ADR 0034 §5).
    fn label_for(ctx: &TaskContext, asked: Option<Label>) -> Label {
        asked
            .unwrap_or(Label::Internal)
            .max(ctx.data_label.unwrap_or(Label::Public))
    }

    /// What a task that has read data labeled above `artifact`'s may not
    /// put in it (DP-11).
    fn no_write_down(ctx: &TaskContext, artifact: &Artifact) -> Result<(), ActionError> {
        match ctx.data_label {
            Some(read) if read > artifact.label => Err(ActionError::OutOfScope(format!(
                "this task has read {read} data, and {} is labeled {}; share a new artifact instead",
                artifact.title, artifact.label
            ))),
            _ => Ok(()),
        }
    }

    /// Seals `html` as the next version of item `id` and posts it.
    fn add_version(
        &self,
        ctx: &TaskContext,
        id: &str,
        number: u32,
        html: &str,
        note: &str,
    ) -> Result<(), ActionError> {
        let me = Self::me(ctx);
        let key = BlobKey::generate().map_err(failed)?;
        let blob_id = new_blob_id().map_err(failed)?;
        let blob = key
            .seal(id, &blob_id, html.as_bytes())
            .map_err(|e| invalid(e.to_string()))?;
        self.shared.put_blob(me, &blob).map_err(failed)?;
        let part = Part::Version {
            number,
            blob: blob_id,
            key: key.to_hex(),
            sha256: hex::encode(glass_crypto::sha256(html.as_bytes())),
            bytes: html.len() as u64,
            mime: HTML.into(),
            merged: None,
            scores: None,
        };
        self.shared
            .post(me, id, note, Some(part), Self::attribution(ctx), None)
            .map_err(failed)?;
        Ok(())
    }

    fn list(&self, ctx: &TaskContext) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let mut artifacts = self.artifacts(me);
        artifacts.sort_by(|a, b| {
            let at = |x: &Artifact| x.latest().map(|v| v.at.clone()).unwrap_or_default();
            at(b).cmp(&at(a))
        });
        let mut text = format!(
            "{} artifact{}.",
            artifacts.len(),
            if artifacts.len() == 1 { "" } else { "s" }
        );
        let views: Vec<Value> = artifacts
            .iter()
            .map(|a| {
                let latest = a.latest();
                text.push_str(&format!(
                    "\n- {} (v{}, by {}, {} open comment{}) [{}]",
                    a.title,
                    latest.map_or(0, |v| v.number),
                    self.name(&a.owner),
                    a.open_comments(),
                    if a.open_comments() == 1 { "" } else { "s" },
                    a.id
                ));
                json!({
                    "id": a.id,
                    "title": a.title,
                    "label": a.label,
                    "owner": a.owner,
                    "owner_name": self.name(&a.owner),
                    "mine": a.owner == me,
                    "people": a.members.iter().map(|m| json!({ "login": m, "name": self.name(m) })).collect::<Vec<_>>(),
                    "version": latest.map(|v| v.number),
                    "updated_at": latest.map(|v| &v.at),
                    "open_comments": a.open_comments(),
                })
            })
            .collect();
        Ok(ActionOutput {
            text,
            data: json!({ "artifacts": views }),
            changes: Vec::new(),
        })
    }

    fn read(&self, ctx: &TaskContext, input: ReadInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let artifact = self.artifact(me, &input.artifact)?;
        let (number, html) = self.page_of(me, &artifact, input.version)?;
        let mut text = format!(
            "{} v{number} by {}: {} KB.",
            artifact.title,
            self.name(&artifact.owner),
            html.len().div_ceil(1024)
        );
        if artifact.comments.is_empty() {
            text.push_str(" No comments yet.");
        }
        for c in &artifact.comments {
            text.push_str(&format!(
                "\n- [{}] {}{} on v{}{}: {} [{}]",
                if c.resolved { "resolved" } else { "open" },
                self.name(&c.by),
                c.attribution
                    .as_deref()
                    .map(|a| format!(" ({a})"))
                    .unwrap_or_default(),
                c.version,
                c.anchor
                    .as_ref()
                    .and_then(|a| a.path.as_deref())
                    .map(|p| format!(" at {p}"))
                    .unwrap_or_default(),
                c.text,
                c.id
            ));
        }
        let versions: Vec<Value> = artifact
            .versions
            .iter()
            .map(|v| {
                json!({
                    "number": v.number,
                    "note": v.note,
                    "by": v.by,
                    "by_name": self.name(&v.by),
                    "attribution": v.attribution,
                    "at": v.at,
                    "bytes": v.bytes,
                })
            })
            .collect();
        let comments: Vec<Value> = artifact
            .comments
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "by": c.by,
                    "by_name": self.name(&c.by),
                    "attribution": c.attribution,
                    "text": c.text,
                    "version": c.version,
                    "anchor": c.anchor,
                    "reply_to": c.reply_to,
                    "resolved": c.resolved,
                    "at": c.at,
                })
            })
            .collect();
        Ok(ActionOutput {
            text,
            data: json!({
                "id": artifact.id,
                "title": artifact.title,
                "label": artifact.label,
                "owner": artifact.owner,
                "owner_name": self.name(&artifact.owner),
                "mine": artifact.owner == me,
                "people": artifact.members.iter().map(|m| json!({ "login": m, "name": self.name(m) })).collect::<Vec<_>>(),
                "version": number,
                "versions": versions,
                "comments": comments,
                "html": html,
            }),
            changes: Vec::new(),
        })
    }

    /// Who a share reaches: the people named and a conversation's people,
    /// all in the directory, and its maker.
    fn audience(&self, ctx: &TaskContext, input: &ShareInput) -> Result<Vec<String>, ActionError> {
        let me = Self::me(ctx);
        let mut people: Vec<String> = input.with.clone();
        if let Some(conversation) = &input.conversation
            && !is_conversation_id(conversation)
        {
            return Err(invalid("a conversation's id is cv- and 32 hex digits"));
        }
        for login in &people {
            if self.directory.person(login).is_none() {
                return Err(invalid(format!(
                    "{login} isn't in your organization's directory; artifacts stay inside the organization"
                )));
            }
        }
        people.retain(|p| p != me);
        people.sort();
        people.dedup();
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

    fn share(&self, ctx: &TaskContext, input: ShareInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let title = Self::check_title(&input.title)?;
        let html = self.content(ctx, &input.content)?;
        let mut people = self.audience(ctx, &input)?;
        // A conversation's people see it too, and it shows there as a card.
        if let Some(conversation) = &input.conversation {
            let members = self.conversation_members(me, conversation)?;
            people.extend(members.into_iter().filter(|m| m != me));
            people.sort();
            people.dedup();
        }
        let label = Self::label_for(ctx, input.label);
        let id = self
            .shared
            .create(me, ConversationKind::Artifact, &title, label, &people)
            .map_err(failed)?;
        let note = input.note.as_deref().unwrap_or("").trim().to_owned();
        // Until its first version is posted it is no one's artifact, so a
        // failure here shares nothing.
        self.add_version(ctx, &id, 1, &html, &note)
            .map_err(|e| failed(format!("Nothing was shared: {e}")))?;
        if let Some(conversation) = &input.conversation {
            let card = Part::Shared {
                item: id.clone(),
                kind: "artifact".into(),
                title: title.clone(),
            };
            self.shared
                .post(
                    me,
                    conversation,
                    &format!("Shared {title}"),
                    Some(card),
                    Self::attribution(ctx),
                    None,
                )
                .map_err(failed)?;
        }
        let names: Vec<String> = people.iter().map(|p| self.name(p)).collect();
        Ok(ActionOutput {
            text: if names.is_empty() {
                format!("Saved {title}; only you can see it until you share it.")
            } else {
                format!("Shared {title} with {}.", names.join(", "))
            },
            data: json!({ "id": id, "title": title, "label": label, "version": 1, "people": people }),
            changes: Vec::new(),
        })
    }

    /// A chat conversation's people, if `me` is in it.
    fn conversation_members(&self, me: &str, id: &str) -> Result<Vec<String>, ActionError> {
        self.shared
            .item(me, id)
            .filter(|c| c.kind.is_chat())
            .map(|c| c.members)
            .ok_or_else(|| ActionError::NotFound(format!("you aren't in a conversation {id}")))
    }

    fn update(&self, ctx: &TaskContext, input: UpdateInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let _turn = self.updating.lock().unwrap_or_else(PoisonError::into_inner);
        let artifact = self.artifact(me, &input.artifact)?;
        if artifact.owner != me {
            return Err(invalid(format!(
                "{} is {}'s; comment on it, or share your own version",
                artifact.title,
                self.name(&artifact.owner)
            )));
        }
        Self::no_write_down(ctx, &artifact)?;
        let html = self.content(ctx, &input.content)?;
        let number = artifact.latest().map_or(1, |v| v.number + 1);
        self.add_version(ctx, &artifact.id, number, &html, input.note.trim())?;
        Ok(ActionOutput {
            text: format!("{} is at version {number}.", artifact.title),
            data: json!({ "id": artifact.id, "version": number }),
            changes: Vec::new(),
        })
    }

    fn comment(&self, ctx: &TaskContext, input: CommentInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let artifact = self.artifact(me, &input.artifact)?;
        Self::no_write_down(ctx, &artifact)?;
        let text = input.text.trim();
        if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
            return Err(invalid(format!(
                "a comment is 1 to {MAX_TEXT_CHARS} characters"
            )));
        }
        if let Some(anchor) = &input.anchor {
            let bad_point = |v: Option<u16>| v.is_some_and(|v| v > 1000);
            if anchor
                .path
                .as_ref()
                .is_some_and(|p| p.len() > MAX_ANCHOR_PATH || p.chars().any(char::is_control))
                || bad_point(anchor.x)
                || bad_point(anchor.y)
            {
                return Err(invalid(
                    "an anchor is an element path, or a point in thousandths of the page",
                ));
            }
        }
        if let Some(to) = &input.reply_to
            && !artifact.comments.iter().any(|c| &c.id == to)
        {
            return Err(invalid(format!("{} has no comment {to}", artifact.title)));
        }
        let version = artifact.latest().map_or(1, |v| v.number);
        let id = self
            .shared
            .post(
                me,
                &artifact.id,
                text,
                Some(Part::Comment {
                    version,
                    anchor: input.anchor.clone(),
                }),
                Self::attribution(ctx),
                input.reply_to.clone(),
            )
            .map_err(failed)?;
        Ok(ActionOutput {
            text: format!("Commented on {} v{version}.", artifact.title),
            data: json!({ "id": id, "artifact": artifact.id, "version": version }),
            changes: Vec::new(),
        })
    }

    fn resolve(&self, ctx: &TaskContext, input: ResolveInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let artifact = self.artifact(me, &input.artifact)?;
        if !artifact.comments.iter().any(|c| c.id == input.comment) {
            return Err(invalid(format!(
                "{} has no comment {}",
                artifact.title, input.comment
            )));
        }
        self.shared
            .post(
                me,
                &artifact.id,
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
                artifact.title
            ),
            data: json!({ "artifact": artifact.id, "comment": input.comment, "resolved": input.resolved }),
            changes: Vec::new(),
        })
    }

    /// An artifact's title and label for a preview, when the person is in
    /// it.
    fn about(&self, ctx: &TaskContext, id: &str) -> (String, Option<Label>) {
        self.artifact(Self::me(ctx), id).map_or_else(
            |_| ("an artifact".into(), None),
            |a| (a.title, Some(a.label)),
        )
    }

    fn title_of(&self, ctx: &TaskContext, id: &str) -> String {
        self.about(ctx, id).0
    }
}

impl ActionProvider for ArtifactsProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let artifact = json!({
            "type": "string",
            "pattern": "^cv-[0-9a-f]{32}$",
            "description": "An artifact's id from artifact.list"
        });
        let html = json!({
            "type": "string",
            "description": "The page, as one HTML file. Its own inline scripts, styles, images (data: URLs) and fonts run in the viewer's sandbox, which has no network: inline any library rather than loading it from a CDN."
        });
        let path = json!({
            "type": "string",
            "description": "Or the page as a file in a folder the task was given, such as Reports/q3.html"
        });
        let anchor = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "maxLength": MAX_ANCHOR_PATH, "description": "A CSS path to the element, from the page's body" },
                "x": { "type": "integer", "minimum": 0, "maximum": 1000 },
                "y": { "type": "integer", "minimum": 0, "maximum": 1000 }
            },
            "additionalProperties": false
        });
        vec![
            ActionDef {
                name: LIST.parse().expect("static name"),
                title: "See shared artifacts".into(),
                description: "Artifacts shared with the person and the ones they shared: title, \
                              owner, version and open comments, newest first."
                    .into(),
                input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: READ.parse().expect("static name"),
                title: "Read artifacts and their feedback".into(),
                description: "One artifact: a version's HTML (the latest by default), its \
                              versions, and every comment with where it is pinned and whether it \
                              is resolved."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "artifact": artifact, "version": { "type": "integer", "minimum": 1 } },
                    "required": ["artifact"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::None,
                default_tier: Tier::Auto,
                ask: AskLabels::default(),
            },
            ActionDef {
                name: SHARE.parse().expect("static name"),
                title: "Share artifacts".into(),
                description: "Gives an HTML page a home in the organization's shared space and \
                              shares it with people (logins) or a chat conversation, where it \
                              shows as a card. People open it in the viewer and comment on it; \
                              it is never sent as a file. With nobody named, only the person \
                              sees it."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "minLength": 1, "maxLength": MAX_TITLE_CHARS },
                        "html": html,
                        "path": path,
                        "with": { "type": "array", "items": { "type": "string" }, "description": "Logins of people in the organization" },
                        "conversation": { "type": "string", "pattern": "^cv-[0-9a-f]{32}$", "description": "A chat conversation from chat.list" },
                        "note": { "type": "string", "maxLength": MAX_TEXT_CHARS },
                        "label": { "enum": ["public", "internal", "confidential", "restricted"], "description": "Internal unless said; never below the data the task has read" }
                    },
                    "required": ["title"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review the share", "Share"),
            },
            ActionDef {
                name: UPDATE.parse().expect("static name"),
                title: "Share new versions of artifacts".into(),
                description: "A new version of one of the person's artifacts, with a note on what \
                              changed. Everyone it is shared with sees it; earlier comments are \
                              kept, marked with the version they were made on."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "artifact": artifact,
                        "html": html,
                        "path": path,
                        "note": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS }
                    },
                    "required": ["artifact", "note"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review the version", "Share it"),
            },
            ActionDef {
                name: COMMENT.parse().expect("static name"),
                title: "Comment on artifacts".into(),
                description: "A comment on an artifact's latest version, pinned to an element \
                              or a point when anchor is given, or a reply to a comment."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "artifact": artifact,
                        "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS },
                        "anchor": anchor,
                        "reply_to": { "type": "string", "description": "A comment's id from artifact.read" }
                    },
                    "required": ["artifact", "text"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review the comment", "Comment"),
            },
            ActionDef {
                name: RESOLVE.parse().expect("static name"),
                title: "Resolve comments on artifacts".into(),
                description: "Marks a comment resolved, or reopens it with resolved: false. \
                              Everyone the artifact is shared with sees it."
                    .into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "artifact": artifact,
                        "comment": { "type": "string" },
                        "resolved": { "type": "boolean" }
                    },
                    "required": ["artifact", "comment"],
                    "additionalProperties": false
                }),
                side_effect: SideEffect::Irreversible,
                default_tier: Tier::Ask,
                ask: AskLabels::new("Review the resolve", "Resolve"),
            },
        ]
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        let id = json!({ "type": "string", "description": "the artifact's id, cv-…" });
        let label = json!({ "enum": ["public", "internal", "confidential", "restricted"], "description": "how sensitive it is" });
        let people = json!({
            "type": "array",
            "description": "who else can see it",
            "items": {
                "type": "object",
                "properties": { "login": { "type": "string" }, "name": { "type": "string" } },
                "required": ["login", "name"],
                "additionalProperties": false
            }
        });
        let version = json!({ "type": "integer", "description": "its version number, from 1" });
        Some(match action.as_str() {
            LIST => json!({
                "type": "object",
                "properties": {
                    "artifacts": {
                        "type": "array",
                        "description": "the ones shared with you, most recently updated first",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": id,
                                "title": { "type": "string" },
                                "label": label,
                                "owner": { "type": "string", "description": "its maker's login" },
                                "owner_name": { "type": "string" },
                                "mine": { "type": "boolean", "description": "you made it" },
                                "people": people,
                                "version": { "type": "integer", "description": "its latest version's number" },
                                "updated_at": { "type": "string", "description": "its latest version's, RFC 3339" },
                                "open_comments": { "type": "integer", "description": "threads not resolved" }
                            },
                            "required": ["id", "title", "label", "owner", "owner_name", "mine", "people", "version", "updated_at", "open_comments"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["artifacts"],
                "additionalProperties": false
            }),
            READ => json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "title": { "type": "string" },
                    "label": label,
                    "owner": { "type": "string", "description": "its maker's login" },
                    "owner_name": { "type": "string" },
                    "mine": { "type": "boolean", "description": "you made it" },
                    "people": people,
                    "version": { "type": "integer", "description": "the version read" },
                    "versions": {
                        "type": "array",
                        "description": "oldest first",
                        "items": {
                            "type": "object",
                            "properties": {
                                "number": version,
                                "note": { "type": "string", "description": "what changed; empty when its maker said nothing" },
                                "by": { "type": "string", "description": "its maker's login" },
                                "by_name": { "type": "string" },
                                "attribution": { "type": ["string", "null"], "description": "the agent that made it for them; null when they did" },
                                "at": { "type": "string", "description": "RFC 3339" },
                                "bytes": { "type": "integer", "description": "the page's size" }
                            },
                            "required": ["number", "note", "by", "by_name", "attribution", "at", "bytes"],
                            "additionalProperties": false
                        }
                    },
                    "comments": {
                        "type": "array",
                        "description": "oldest first, resolved ones too",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "by": { "type": "string", "description": "the commenter's login" },
                                "by_name": { "type": "string" },
                                "attribution": { "type": ["string", "null"], "description": "the agent that wrote it for them; null when they did" },
                                "text": { "type": "string" },
                                "version": { "type": "integer", "description": "the version it was made on" },
                                "anchor": {
                                    "type": ["object", "null"],
                                    "description": "where on the page; null for the page as a whole",
                                    "properties": {
                                        "path": { "type": "string", "description": "an element's path" },
                                        "x": { "type": "integer", "description": "thousandths of the page's width" },
                                        "y": { "type": "integer", "description": "thousandths of its height" }
                                    },
                                    "additionalProperties": false
                                },
                                "reply_to": { "type": ["string", "null"], "description": "the comment it answers" },
                                "resolved": { "type": "boolean", "description": "its thread is resolved" },
                                "at": { "type": "string", "description": "RFC 3339" }
                            },
                            "required": ["id", "by", "by_name", "attribution", "text", "version", "anchor", "reply_to", "resolved", "at"],
                            "additionalProperties": false
                        }
                    },
                    "html": { "type": "string", "description": "the page" }
                },
                "required": ["id", "title", "label", "owner", "owner_name", "mine", "people", "version", "versions", "comments", "html"],
                "additionalProperties": false
            }),
            SHARE => json!({
                "type": "object",
                "properties": {
                    "id": id,
                    "title": { "type": "string" },
                    "label": label,
                    "version": version,
                    "people": { "type": "array", "items": { "type": "string" }, "description": "the logins it reached, without yours" }
                },
                "required": ["id", "title", "label", "version", "people"],
                "additionalProperties": false
            }),
            UPDATE => json!({
                "type": "object",
                "properties": { "id": id, "version": version },
                "required": ["id", "version"],
                "additionalProperties": false
            }),
            COMMENT => json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "the comment's id" },
                    "artifact": id,
                    "version": { "type": "integer", "description": "the version it's on" }
                },
                "required": ["id", "artifact", "version"],
                "additionalProperties": false
            }),
            RESOLVE => json!({
                "type": "object",
                "properties": {
                    "artifact": id,
                    "comment": { "type": "string" },
                    "resolved": { "type": "boolean", "description": "false when it was reopened" }
                },
                "required": ["artifact", "comment", "resolved"],
                "additionalProperties": false
            }),
            _ => return None,
        })
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                Ok(Preview {
                    summary: "See shared artifacts".into(),
                    targets: vec!["Artifacts".into()],
                    ..Preview::default()
                })
            }
            READ => {
                let input: ReadInput = parse_input(input)?;
                // Reading it raises the task's label to its own, so it has
                // to be found here, where it is judged: one this couldn't
                // find used to be "an artifact" with no label, which passes.
                let artifact = self.artifact(Self::me(ctx), &input.artifact)?;
                Ok(Preview {
                    summary: format!("Read {}", artifact.title),
                    targets: vec![artifact.title],
                    label: Some(artifact.label),
                    ..Preview::default()
                })
            }
            SHARE => {
                let input: ShareInput = parse_input(input)?;
                let title = Self::check_title(&input.title)?;
                let people = self.audience(ctx, &input)?;
                let mut items = Vec::new();
                if !people.is_empty() {
                    let names: Vec<String> = people.iter().map(|p| self.name(p)).collect();
                    items.push(format!("Shared with {}", names.join(", ")));
                }
                if input.conversation.is_some() {
                    items.push("Shown as a card in the conversation, to everyone in it".into());
                }
                if people.is_empty() && input.conversation.is_none() {
                    items.push("Only you can see it".into());
                }
                items.push(format!(
                    "Labeled {}; sealed to their computers and your organization's compliance key",
                    Self::label_for(ctx, input.label)
                ));
                if let Some(note) = input.note.as_deref().filter(|n| !n.trim().is_empty()) {
                    items.push(format!("Note: {note}"));
                }
                Ok(Preview {
                    summary: format!("Share {title}"),
                    targets: vec![title],
                    items,
                    reads: Self::reads(ctx, &input.content),
                    ..Preview::default()
                })
            }
            UPDATE => {
                let input: UpdateInput = parse_input(input)?;
                let title = self.title_of(ctx, &input.artifact);
                Ok(Preview {
                    summary: format!("Share a new version of {title}"),
                    targets: vec![title],
                    items: vec![format!("Note: {}", input.note.trim())],
                    reads: Self::reads(ctx, &input.content),
                    ..Preview::default()
                })
            }
            COMMENT => {
                let input: CommentInput = parse_input(input)?;
                let title = self.title_of(ctx, &input.artifact);
                Ok(Preview {
                    summary: format!("Comment on {title}"),
                    targets: vec![title],
                    items: vec![input.text.trim().to_owned()],
                    ..Preview::default()
                })
            }
            RESOLVE => {
                let input: ResolveInput = parse_input(input)?;
                let title = self.title_of(ctx, &input.artifact);
                Ok(Preview {
                    summary: format!(
                        "{} a comment on {title}",
                        if input.resolved { "Resolve" } else { "Reopen" }
                    ),
                    targets: vec![title],
                    ..Preview::default()
                })
            }
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
            SHARE => self.share(ctx, parse_input(input)?),
            UPDATE => self.update(ctx, parse_input(input)?),
            COMMENT => self.comment(ctx, parse_input(input)?),
            RESOLVE => self.resolve(ctx, parse_input(input)?),
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }
}

#[cfg(test)]
mod tests;
