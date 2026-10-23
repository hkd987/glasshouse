//! Skills (ADR 0034; SK-1 to SK-6): the instructions, examples and files
//! that teach an agent a task, shared with an owner, versions and evals,
//! and improved by anyone who can show their change scores better.
//!
//! A skill is a shared-space item of kind `skill`, reached through
//! [`Shared`] like an artifact. Each version is a sealed [`Bundle`] that
//! carries its evals; only the owner's versions count. Anyone in it can
//! propose a change, with the base's and the change's scores from one
//! eval run on their computer; the owner merges, with a reason when a case
//! that passed now fails (SK-3).
//!
//! - `skill.list`, `skill.read` (Auto): skills shared with the person and
//!   their own; one skill's files, versions, scores and proposals.
//! - `skill.draft` (Auto, this computer only): a candidate bundle to run
//!   the evals on before proposing or sharing it.
//! - `skill.results` (Auto, this computer only): an eval run's scores,
//!   reported by the `skill-evals` agent and nothing else.
//! - `skill.share`, `skill.propose`, `skill.merge` (Ask; others see it).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext, parse_input,
};
use glass_api::AskLabels;
use glass_managed::DirectoryHandle;
use glass_managed::chat::{ConversationKind, MAX_TEXT_CHARS, is_conversation_id};
use glass_managed::shared::{BlobKey, Item, ItemMessage, Part, Shared, new_blob_id};
use glass_managed::skills::{Bundle, CaseScore, SKILL_MIME, Scores};
use glass_types::{ActionName, Label, SideEffect, Tier, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The Skills app: what the person does in it is theirs.
pub const APP: &str = "skills";
/// The agent that runs evals (ADR 0034 §4): the only one whose results
/// are kept.
pub const EVALS_AGENT: &str = "skill-evals";

pub const LIST: &str = "skill.list";
pub const READ: &str = "skill.read";
pub const SHARE: &str = "skill.share";
pub const DRAFT: &str = "skill.draft";
pub const RESULTS: &str = "skill.results";
pub const PROPOSE: &str = "skill.propose";
pub const MERGE: &str = "skill.merge";

/// What this computer keeps per person.
const KEPT_DRAFTS: usize = 20;
const KEPT_RESULTS: usize = 100;

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
    note: String,
    by: String,
    at: String,
    merged: Option<String>,
    scores: Option<Scores>,
}

/// One proposal, and whether a version merged it.
#[derive(Debug, Clone)]
struct Proposal {
    id: String,
    by: String,
    attribution: Option<String>,
    note: String,
    at: String,
    blob: String,
    key: String,
    sha256: String,
    bytes: u64,
    base: u32,
    before: Scores,
    after: Scores,
    merged: bool,
}

/// A skill as its messages tell it.
#[derive(Debug, Clone)]
struct Skill {
    id: String,
    name: String,
    label: Label,
    owner: String,
    members: Vec<String>,
    versions: Vec<Version>,
    proposals: Vec<Proposal>,
}

impl Skill {
    fn from_item(item: Item) -> Self {
        let mut versions = Vec::new();
        let mut proposals = Vec::new();
        for ItemMessage {
            id,
            sender,
            at,
            text,
            attribution,
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
                    mime,
                    merged,
                    scores,
                    ..
                }) if mime == SKILL_MIME && sender == item.created_by => versions.push(Version {
                    number,
                    blob,
                    key,
                    sha256,
                    note: text,
                    by: sender,
                    at,
                    merged,
                    scores,
                }),
                Some(Part::Proposal {
                    blob,
                    key,
                    sha256,
                    bytes,
                    base,
                    before,
                    after,
                }) => proposals.push(Proposal {
                    id,
                    by: sender,
                    attribution,
                    note: text,
                    at,
                    blob,
                    key,
                    sha256,
                    bytes,
                    base,
                    before,
                    after,
                    merged: false,
                }),
                _ => {}
            }
        }
        versions.sort_by_key(|v| v.number);
        versions.dedup_by_key(|v| v.number);
        for proposal in &mut proposals {
            proposal.merged = versions
                .iter()
                .any(|v| v.merged.as_deref() == Some(proposal.id.as_str()));
        }
        Self {
            id: item.id,
            name: item.title,
            label: item.label,
            owner: item.created_by,
            members: item.members,
            versions,
            proposals,
        }
    }

    fn latest(&self) -> Option<&Version> {
        self.versions.last()
    }

    fn proposal(&self, id: &str) -> Option<&Proposal> {
        self.proposals.iter().find(|p| p.id == id)
    }

    fn open_proposals(&self) -> usize {
        self.proposals.iter().filter(|p| !p.merged).count()
    }
}

/// A candidate bundle on this computer, to run the evals on before it is
/// shared or proposed. Its id is its bundle's SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    id: String,
    /// The skill it changes; none for a new one.
    skill: Option<String>,
    /// Its skill's label, or what the task that drafted it had read.
    label: Label,
    bundle: Bundle,
    at: String,
}

/// What this computer keeps for one person, beside the shared space.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Local {
    drafts: Vec<Draft>,
    results: Vec<Scores>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    proposal: Option<String>,
    #[serde(default)]
    draft: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareInput {
    #[serde(default)]
    files: Option<BTreeMap<String, String>>,
    #[serde(default)]
    draft: Option<String>,
    #[serde(default)]
    with: Vec<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    label: Option<Label>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftInput {
    #[serde(default)]
    skill: Option<String>,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultsInput {
    #[serde(default)]
    skill: Option<String>,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    draft: Option<String>,
    model: String,
    runner: String,
    cases: Vec<CaseScore>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeInput {
    skill: String,
    draft: String,
    note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MergeInput {
    skill: String,
    proposal: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Whether `login` is safe to name a file after.
fn plain_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 64
        && !login.starts_with('.')
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The shape of an eval run's [`Scores`], or null where there may be none
/// (GA-1, ADR 0049).
fn scores_shape(nullable: bool) -> Value {
    json!({
        "type": if nullable { json!(["object", "null"]) } else { json!("object") },
        "properties": {
            "run": { "type": "string", "description": "the eval task's id; a proposal's before and after come from one run" },
            "model": { "type": "string", "description": "the model that answered" },
            "runner": { "type": "string", "description": "the eval runner and its version" },
            "by": { "type": "string", "description": "whose computer ran it, their login" },
            "at": { "type": "string", "description": "RFC 3339" },
            "sha256": { "type": "string", "description": "the bundle scored" },
            "cases": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "passed": { "type": "integer", "description": "how many of its runs passed" },
                        "runs": { "type": "integer" }
                    },
                    "required": ["id", "passed", "runs"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["run", "model", "runner", "by", "at", "sha256", "cases"],
        "additionalProperties": false
    })
}

fn label_shape() -> Value {
    json!({ "enum": ["public", "internal", "confidential", "restricted"], "description": "where its text may go: restricted, to no model" })
}

fn files_shape() -> Value {
    json!({ "type": "object", "description": "each file's text by its path in the skill, such as SKILL.md and evals.json" })
}

/// How many people improved `skill`: whose proposals were merged.
fn contributors(skill: &Skill) -> usize {
    let mut by: Vec<&str> = skill
        .proposals
        .iter()
        .filter(|p| p.merged)
        .map(|p| p.by.as_str())
        .collect();
    by.sort_unstable();
    by.dedup();
    by.len()
}

fn is_sha(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Skills on this computer.
#[derive(Debug, Clone)]
pub struct SkillsProvider {
    shared: Arc<dyn Shared>,
    directory: DirectoryHandle,
    /// Root's: each person's drafts and results.
    dir: PathBuf,
    /// Opened bundles by SHA-256: a version never changes.
    bundles: Arc<Mutex<BTreeMap<String, Bundle>>>,
    write: Arc<Mutex<()>>,
}

impl SkillsProvider {
    /// Skills in `shared`, keeping each person's drafts and eval results
    /// in `dir` (root's, as glassd's state is).
    pub fn new(
        shared: Arc<dyn Shared>,
        directory: DirectoryHandle,
        dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            shared,
            directory,
            dir: dir.into(),
            bundles: Arc::new(Mutex::new(BTreeMap::new())),
            write: Arc::new(Mutex::new(())),
        }
    }

    fn me(ctx: &TaskContext) -> &str {
        &ctx.principal.on_behalf_of.name
    }

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

    fn attribution(ctx: &TaskContext) -> Option<String> {
        let agent = ctx.principal.agent.as_str();
        (agent != APP).then(|| format!("by {agent} for {}", Self::me(ctx)))
    }

    // ---- this computer's drafts and results ----------------------------

    fn local_path(&self, login: &str) -> Result<PathBuf, ActionError> {
        if !plain_login(login) {
            return Err(failed("this person's name can't name a file"));
        }
        Ok(self.dir.join(format!("{login}.json")))
    }

    fn load(&self, login: &str) -> Local {
        self.local_path(login)
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Changes `login`'s local record, written whole, root's only.
    fn change<T>(
        &self,
        login: &str,
        f: impl FnOnce(&mut Local) -> Result<T, ActionError>,
    ) -> Result<T, ActionError> {
        let path = self.local_path(login)?;
        let _write = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        let mut local = self.load(login);
        let out = f(&mut local)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| failed(format!("keeping it: {e}")))?;
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&local).map_err(failed)?;
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| failed(format!("keeping it: {e}")))?;
            file.write_all(&bytes)
                .map_err(|e| failed(format!("keeping it: {e}")))?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| failed(format!("keeping it: {e}")))?;
        Ok(out)
    }

    fn draft(&self, login: &str, id: &str) -> Result<Draft, ActionError> {
        if !is_sha(id) {
            return Err(invalid("a draft's id is 64 hex digits, from skill.draft"));
        }
        self.load(login)
            .drafts
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| ActionError::NotFound(format!("no draft {id} on this computer")))
    }

    // ---- the shared space ------------------------------------------------

    fn skills(&self, login: &str) -> Vec<Skill> {
        self.shared
            .items(login, ConversationKind::Skill)
            .into_iter()
            .map(Skill::from_item)
            .filter(|s| !s.versions.is_empty())
            .collect()
    }

    fn skill(&self, login: &str, id: &str) -> Result<Skill, ActionError> {
        if !is_conversation_id(id) {
            return Err(invalid("a skill's id is cv- and 32 hex digits"));
        }
        self.shared
            .item(login, id)
            .filter(|item| item.kind == ConversationKind::Skill)
            .map(Skill::from_item)
            .filter(|s| !s.versions.is_empty())
            .ok_or_else(|| {
                ActionError::NotFound(format!(
                    "no skill {id} is shared with you; skill.list shows them"
                ))
            })
    }

    /// A bundle a version or proposal names, opened and checked against
    /// its SHA-256.
    fn bundle(
        &self,
        login: &str,
        skill: &str,
        blob: &str,
        key: &str,
        sha256: &str,
    ) -> Result<Bundle, ActionError> {
        if let Some(bundle) = self
            .bundles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(sha256)
        {
            return Ok(bundle.clone());
        }
        let sealed = self
            .shared
            .get_blob(login, skill, blob)
            .map_err(|e| failed(format!("Fetching the skill: {e}")))?;
        let bytes = BlobKey::from_hex(key)
            .and_then(|k| k.open(&sealed, sha256))
            .map_err(|e| failed(format!("Opening the skill: {e}")))?;
        let bundle = Bundle::from_bytes(&bytes).map_err(failed)?;
        self.bundles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sha256.to_owned(), bundle.clone());
        Ok(bundle)
    }

    fn version_bundle(
        &self,
        login: &str,
        skill: &Skill,
        v: &Version,
    ) -> Result<Bundle, ActionError> {
        self.bundle(login, &skill.id, &v.blob, &v.key, &v.sha256)
    }

    /// A skill's label for a preview, when the person is in it.
    fn label_of(&self, ctx: &TaskContext, id: &str) -> (String, Option<Label>) {
        self.skill(Self::me(ctx), id)
            .map_or_else(|_| ("a skill".into(), None), |s| (s.name, Some(s.label)))
    }

    /// Seals `bundle` for skill `id` and keeps it at the console: the
    /// blob, its key and its size.
    fn seal(
        &self,
        ctx: &TaskContext,
        id: &str,
        bundle: &Bundle,
    ) -> Result<(String, String, u64), ActionError> {
        let bytes = bundle.to_bytes();
        let key = BlobKey::generate().map_err(failed)?;
        let blob_id = new_blob_id().map_err(failed)?;
        let blob = key
            .seal(id, &blob_id, &bytes)
            .map_err(|e| invalid(e.to_string()))?;
        self.shared.put_blob(Self::me(ctx), &blob).map_err(failed)?;
        Ok((blob_id, key.to_hex(), bytes.len() as u64))
    }

    /// What a task that has read data labeled above the skill's may not
    /// put in it (DP-11).
    fn no_write_down(ctx: &TaskContext, skill: &Skill) -> Result<(), ActionError> {
        match ctx.data_label {
            Some(read) if read > skill.label => Err(ActionError::OutOfScope(format!(
                "this task has read {read} data, and {} is labeled {}",
                skill.name, skill.label
            ))),
            _ => Ok(()),
        }
    }

    // ---- actions ---------------------------------------------------------

    fn list(&self, ctx: &TaskContext) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let skills = self.skills(me);
        let mut text = format!(
            "{} skill{}.",
            skills.len(),
            if skills.len() == 1 { "" } else { "s" }
        );
        let mut views = Vec::new();
        for s in &skills {
            let latest = s.latest();
            let description = latest
                .and_then(|v| self.version_bundle(me, s, v).ok())
                .and_then(|b| b.check().ok())
                .map(|c| c.description)
                .unwrap_or_default();
            let score = latest.and_then(|v| v.scores.as_ref());
            text.push_str(&format!(
                "\n- {} v{} by {}: {}{} [{}]",
                s.name,
                latest.map_or(0, |v| v.number),
                self.name(&s.owner),
                description,
                score
                    .map(|sc| format!(" ({})", sc.summary()))
                    .unwrap_or_default(),
                s.id
            ));
            views.push(json!({
                "id": s.id,
                "name": s.name,
                "description": description,
                "label": s.label,
                "owner": s.owner,
                "owner_name": self.name(&s.owner),
                "mine": s.owner == me,
                "version": latest.map(|v| v.number),
                "score": score.map(|sc| json!({ "summary": sc.summary(), "passed": sc.passed(), "cases": sc.cases.len(), "model": sc.model })),
                "open_proposals": s.open_proposals(),
                "contributors": contributors(s),
            }));
        }
        Ok(ActionOutput {
            text,
            data: json!({ "skills": views }),
            changes: Vec::new(),
        })
    }

    fn read(&self, ctx: &TaskContext, input: ReadInput) -> Result<ActionOutput, ActionError> {
        let me = Self::me(ctx);
        if let (None, Some(draft)) = (&input.skill, &input.draft) {
            let draft = self.draft(me, draft)?;
            return Ok(ActionOutput {
                text: format!("Draft {} on this computer.", draft.id),
                data: json!({ "draft": draft.id, "label": draft.label, "files": draft.bundle.files }),
                changes: Vec::new(),
            });
        }
        let Some(id) = &input.skill else {
            return Err(invalid("name a skill, or a draft"));
        };
        self.ready(ctx)?;
        let skill = self.skill(me, id)?;
        let (files, sha256, what) = match (&input.proposal, &input.draft, input.version) {
            (Some(p), None, None) => {
                let p = skill.proposal(p).ok_or_else(|| {
                    ActionError::NotFound(format!("{} has no proposal {p}", skill.name))
                })?;
                let bundle = self.bundle(me, &skill.id, &p.blob, &p.key, &p.sha256)?;
                (bundle.files, p.sha256.clone(), format!("proposal {}", p.id))
            }
            (None, Some(d), None) => {
                let draft = self.draft(me, d)?;
                (
                    draft.bundle.files,
                    draft.id.clone(),
                    format!("draft {}", draft.id),
                )
            }
            (None, None, number) => {
                let v = match number {
                    Some(n) => skill.versions.iter().find(|v| v.number == n),
                    None => skill.latest(),
                }
                .ok_or_else(|| {
                    ActionError::NotFound(format!("{} has no such version", skill.name))
                })?;
                let bundle = self.version_bundle(me, &skill, v)?;
                (bundle.files, v.sha256.clone(), format!("v{}", v.number))
            }
            _ => {
                return Err(invalid(
                    "name a version, a proposal or a draft, not more than one",
                ));
            }
        };
        let mut text = format!(
            "{} ({what}) by {}, labeled {}: {} file{}.",
            skill.name,
            self.name(&skill.owner),
            skill.label,
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        );
        for v in &skill.versions {
            text.push_str(&format!(
                "\n- v{}{}: {}{}",
                v.number,
                v.merged
                    .as_ref()
                    .map(|p| format!(" (merged {p})"))
                    .unwrap_or_default(),
                v.note,
                v.scores
                    .as_ref()
                    .map(|s| format!(" [{}]", s.summary()))
                    .unwrap_or_default()
            ));
        }
        for p in skill.proposals.iter().filter(|p| !p.merged) {
            text.push_str(&format!(
                "\n- proposal {} by {} on v{}: {} → {}: {}",
                p.id,
                self.name(&p.by),
                p.base,
                p.before.summary(),
                p.after.summary(),
                p.note
            ));
        }
        let versions: Vec<Value> = skill
            .versions
            .iter()
            .map(|v| {
                json!({
                    "number": v.number, "note": v.note, "by": v.by, "by_name": self.name(&v.by),
                    "at": v.at, "merged": v.merged, "scores": v.scores,
                })
            })
            .collect();
        let proposals: Vec<Value> = skill
            .proposals
            .iter()
            .map(|p| {
                json!({
                    "id": p.id, "by": p.by, "by_name": self.name(&p.by), "attribution": p.attribution,
                    "note": p.note, "at": p.at, "base": p.base, "before": p.before, "after": p.after,
                    "merged": p.merged, "breaks": p.before.broken_by(&p.after),
                })
            })
            .collect();
        let local: Vec<Scores> = self
            .load(me)
            .results
            .into_iter()
            .filter(|r| r.sha256 == sha256)
            .collect();
        Ok(ActionOutput {
            text,
            data: json!({
                "id": skill.id,
                "name": skill.name,
                "label": skill.label,
                "owner": skill.owner,
                "owner_name": self.name(&skill.owner),
                "mine": skill.owner == me,
                "people": skill.members.iter().map(|m| json!({ "login": m, "name": self.name(m) })).collect::<Vec<_>>(),
                "sha256": sha256,
                "files": files,
                "versions": versions,
                "proposals": proposals,
                "results_here": local,
            }),
            changes: Vec::new(),
        })
    }

    /// The bundle an input gives as files or a draft, checked.
    fn given(
        &self,
        me: &str,
        files: Option<BTreeMap<String, String>>,
        draft: Option<&str>,
    ) -> Result<(Bundle, Option<Draft>), ActionError> {
        let (bundle, draft) = match (files, draft) {
            (Some(files), None) => (Bundle { files }, None),
            (None, Some(id)) => {
                let draft = self.draft(me, id)?;
                (draft.bundle.clone(), Some(draft))
            }
            _ => {
                return Err(invalid(
                    "give the skill's files, or a draft from skill.draft",
                ));
            }
        };
        bundle.check().map_err(invalid)?;
        Ok((bundle, draft))
    }

    fn audience(&self, me: &str, with: &[String]) -> Result<Vec<String>, ActionError> {
        let mut people = Vec::new();
        for login in with {
            if self.directory.person(login).is_none() {
                return Err(invalid(format!(
                    "{login} isn't in your organization's directory; skills stay inside the organization"
                )));
            }
            if login != me {
                people.push(login.clone());
            }
        }
        people.sort();
        people.dedup();
        Ok(people)
    }

    fn share(&self, ctx: &TaskContext, input: ShareInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let (bundle, draft) = self.given(me, input.files, input.draft.as_deref())?;
        if draft.as_ref().is_some_and(|d| d.skill.is_some()) {
            return Err(invalid(
                "that draft changes a skill; propose it with skill.propose",
            ));
        }
        let checked = bundle.check().map_err(invalid)?;
        let people = self.audience(me, &input.with)?;
        let label = input
            .label
            .unwrap_or(Label::Internal)
            .max(ctx.data_label.unwrap_or(Label::Public))
            .max(draft.as_ref().map_or(Label::Public, |d| d.label));
        // The scores this computer has for exactly this bundle, if any.
        let sha256 = bundle.sha256();
        let scores = self
            .load(me)
            .results
            .into_iter()
            .rev()
            .find(|r| r.sha256 == sha256);
        let id = self
            .shared
            .create(me, ConversationKind::Skill, &checked.name, label, &people)
            .map_err(failed)?;
        let note = input.note.as_deref().unwrap_or("").trim().to_owned();
        let posted = self.seal(ctx, &id, &bundle).and_then(|(blob, key, bytes)| {
            let part = Part::Version {
                number: 1,
                blob,
                key,
                sha256: sha256.clone(),
                bytes,
                mime: SKILL_MIME.into(),
                merged: None,
                scores: scores.clone(),
            };
            self.shared
                .post(me, &id, &note, Some(part), Self::attribution(ctx), None)
                .map_err(failed)
        });
        posted.map_err(|e| failed(format!("Nothing was shared: {e}")))?;
        let names: Vec<String> = people.iter().map(|p| self.name(p)).collect();
        Ok(ActionOutput {
            text: format!(
                "Shared {}{}{}.",
                checked.name,
                if names.is_empty() {
                    String::new()
                } else {
                    format!(" with {}", names.join(", "))
                },
                scores
                    .as_ref()
                    .map(|s| format!(", scoring {}", s.summary()))
                    .unwrap_or_default()
            ),
            data: json!({ "id": id, "name": checked.name, "label": label, "version": 1, "people": people, "scores": scores }),
            changes: Vec::new(),
        })
    }

    fn make_draft(
        &self,
        ctx: &TaskContext,
        input: DraftInput,
    ) -> Result<ActionOutput, ActionError> {
        let me = Self::me(ctx);
        let bundle = Bundle { files: input.files };
        let checked = bundle.check().map_err(invalid)?;
        let label = match &input.skill {
            Some(id) => {
                self.ready(ctx)?;
                let skill = self.skill(me, id)?;
                Self::no_write_down(ctx, &skill)?;
                skill.label
            }
            None => ctx
                .data_label
                .unwrap_or(Label::Internal)
                .max(Label::Internal),
        };
        let draft = Draft {
            id: bundle.sha256(),
            skill: input.skill,
            label,
            bundle,
            at: Timestamp::now().to_string(),
        };
        let id = draft.id.clone();
        self.change(me, |local| {
            local.drafts.retain(|d| d.id != draft.id);
            local.drafts.push(draft);
            let excess = local.drafts.len().saturating_sub(KEPT_DRAFTS);
            local.drafts.drain(..excess);
            Ok(())
        })?;
        Ok(ActionOutput {
            text: format!(
                "Drafted {} ({} eval case{}); run its evals with the skill-evals agent before proposing or sharing it. Draft {id}.",
                checked.name,
                checked.evals.cases.len(),
                if checked.evals.cases.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            data: json!({ "draft": id, "name": checked.name, "cases": checked.evals.cases.len(), "calls": checked.evals.calls() }),
            changes: Vec::new(),
        })
    }

    fn results(&self, ctx: &TaskContext, input: ResultsInput) -> Result<ActionOutput, ActionError> {
        if ctx.principal.agent.as_str() != EVALS_AGENT {
            return Err(ActionError::OutOfScope(format!(
                "only the {EVALS_AGENT} agent reports eval results"
            )));
        }
        let me = Self::me(ctx);
        let sha256 = match (&input.skill, input.version, &input.draft) {
            (_, None, Some(d)) => self.draft(me, d)?.id,
            (Some(id), number, None) => {
                self.ready(ctx)?;
                let skill = self.skill(me, id)?;
                let v = match number {
                    Some(n) => skill.versions.iter().find(|v| v.number == n),
                    None => skill.latest(),
                }
                .ok_or_else(|| {
                    ActionError::NotFound(format!("{} has no such version", skill.name))
                })?;
                v.sha256.clone()
            }
            _ => return Err(invalid("name what was run: a skill's version, or a draft")),
        };
        let scores = Scores {
            run: ctx.principal.task.to_string(),
            model: input.model,
            runner: input.runner,
            by: me.to_owned(),
            at: Timestamp::now().to_string(),
            sha256,
            cases: input.cases,
        };
        scores.check().map_err(invalid)?;
        let summary = scores.summary();
        self.change(me, |local| {
            local.results.push(scores);
            let excess = local.results.len().saturating_sub(KEPT_RESULTS);
            local.results.drain(..excess);
            Ok(())
        })?;
        Ok(ActionOutput {
            text: format!("Kept on this computer: {summary}."),
            data: json!({ "summary": summary }),
            changes: Vec::new(),
        })
    }

    /// The base's and the draft's scores from one run, the newest.
    fn scored_pair(local: &Local, base: &str, draft: &str) -> Option<(Scores, Scores)> {
        local.results.iter().rev().find_map(|after| {
            (after.sha256 == draft).then_some(())?;
            let before = local
                .results
                .iter()
                .find(|b| b.sha256 == base && b.run == after.run && b.model == after.model)?;
            Some((before.clone(), after.clone()))
        })
    }

    fn propose(&self, ctx: &TaskContext, input: ProposeInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let skill = self.skill(me, &input.skill)?;
        Self::no_write_down(ctx, &skill)?;
        let note = input.note.trim();
        if note.is_empty() || note.chars().count() > MAX_TEXT_CHARS {
            return Err(invalid(format!(
                "say what the change is and why, in 1 to {MAX_TEXT_CHARS} characters"
            )));
        }
        let draft = self.draft(me, &input.draft)?;
        if draft.skill.as_deref() != Some(skill.id.as_str()) {
            return Err(invalid(format!(
                "that draft isn't a change to {}",
                skill.name
            )));
        }
        let base = skill.latest().ok_or_else(|| failed("it has no version"))?;
        let (before, after) = Self::scored_pair(&self.load(me), &base.sha256, &draft.id).ok_or_else(|| {
            invalid(format!(
                "run the evals on v{} and this draft together first (the skill-evals agent with \"{} draft {}\"), so the scores compare",
                base.number, skill.id, draft.id
            ))
        })?;
        let (blob, key, bytes) = self.seal(ctx, &skill.id, &draft.bundle)?;
        let summary = format!("{} → {}", before.summary(), after.summary());
        let breaks = before.broken_by(&after);
        let id = self
            .shared
            .post(
                me,
                &skill.id,
                note,
                Some(Part::Proposal {
                    blob,
                    key,
                    sha256: draft.id.clone(),
                    bytes,
                    base: base.number,
                    before,
                    after,
                }),
                Self::attribution(ctx),
                None,
            )
            .map_err(failed)?;
        Ok(ActionOutput {
            text: format!(
                "Proposed a change to {}: {summary}{}.",
                skill.name,
                if breaks.is_empty() {
                    String::new()
                } else {
                    format!("; it breaks {}", breaks.join(", "))
                }
            ),
            data: json!({ "id": id, "skill": skill.id, "base": base.number, "breaks": breaks }),
            changes: Vec::new(),
        })
    }

    fn merge(&self, ctx: &TaskContext, input: MergeInput) -> Result<ActionOutput, ActionError> {
        self.ready(ctx)?;
        let me = Self::me(ctx);
        let skill = self.skill(me, &input.skill)?;
        if skill.owner != me {
            return Err(invalid(format!(
                "{} is {}'s to merge",
                skill.name,
                self.name(&skill.owner)
            )));
        }
        let p = skill.proposal(&input.proposal).ok_or_else(|| {
            ActionError::NotFound(format!("{} has no proposal {}", skill.name, input.proposal))
        })?;
        if p.merged {
            return Err(invalid("that proposal is merged already"));
        }
        let latest = skill.latest().ok_or_else(|| failed("it has no version"))?;
        if p.base != latest.number {
            return Err(invalid(format!(
                "it changes v{}, and {} is at v{} now; it needs proposing again on v{}",
                p.base, skill.name, latest.number, latest.number
            )));
        }
        let breaks = p.before.broken_by(&p.after);
        let reason = input.reason.as_deref().map(str::trim).unwrap_or("");
        if !breaks.is_empty() && reason.is_empty() {
            return Err(invalid(format!(
                "it breaks {} that passed before; merging it needs a reason (SK-3)",
                breaks.join(", ")
            )));
        }
        let number = latest.number + 1;
        let text = if reason.is_empty() {
            p.note.clone()
        } else {
            format!("{} (reason: {reason})", p.note)
        };
        self.shared
            .post(
                me,
                &skill.id,
                &text,
                Some(Part::Version {
                    number,
                    blob: p.blob.clone(),
                    key: p.key.clone(),
                    sha256: p.sha256.clone(),
                    bytes: p.bytes,
                    mime: SKILL_MIME.into(),
                    merged: Some(p.id.clone()),
                    scores: Some(p.after.clone()),
                }),
                Self::attribution(ctx),
                None,
            )
            .map_err(failed)?;
        Ok(ActionOutput {
            text: format!(
                "{} is at v{number}, from {}'s proposal: {}.",
                skill.name,
                self.name(&p.by),
                p.after.summary()
            ),
            data: json!({ "id": skill.id, "version": number, "merged": p.id }),
            changes: Vec::new(),
        })
    }
}

impl ActionProvider for SkillsProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let skill = json!({ "type": "string", "pattern": "^cv-[0-9a-f]{32}$", "description": "A skill's id from skill.list" });
        let draft = json!({ "type": "string", "pattern": "^[0-9a-f]{64}$", "description": "A draft's id from skill.draft" });
        let files = json!({
            "type": "object",
            "additionalProperties": { "type": "string" },
            "description": "The skill's files by relative path: SKILL.md (front matter with name: and description:, then the instructions), evals.json ({\"runs\": 3, \"cases\": [{\"id\", \"prompt\", \"files\"?, \"graders\": [{\"type\": \"contains\"|\"not_contains\"|\"equals\"|\"matches\"|\"json_path\"|\"rubric\", ...}]}]}), and any examples"
        });
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
                LIST,
                "See skills",
                "Skills shared with the person and their own: name, description, owner, version, its score and open proposals.",
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                SideEffect::None,
                Tier::Auto,
                AskLabels::default(),
            ),
            def(
                READ,
                "Read skills",
                "One skill's files (the latest version, or a version, proposal or draft), its versions with their scores, and its proposals with before and after scores.",
                json!({
                    "type": "object",
                    "properties": { "skill": skill, "version": { "type": "integer", "minimum": 1 }, "proposal": { "type": "string" }, "draft": draft },
                    "additionalProperties": false
                }),
                SideEffect::None,
                Tier::Auto,
                AskLabels::default(),
            ),
            def(
                SHARE,
                "Share skills",
                "Shares a new skill with people in the organization (logins), from its files or a draft whose evals ran. They can use it and propose changes; the person owns it.",
                json!({
                    "type": "object",
                    "properties": {
                        "files": files, "draft": draft,
                        "with": { "type": "array", "items": { "type": "string" } },
                        "note": { "type": "string", "maxLength": MAX_TEXT_CHARS },
                        "label": { "enum": ["public", "internal", "confidential", "restricted"] }
                    },
                    "additionalProperties": false
                }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the skill", "Share"),
            ),
            def(
                DRAFT,
                "Draft skills",
                "Keeps a candidate version on this computer (a change to a skill, or a new one) so its evals can run before it is proposed or shared. Nothing leaves the computer.",
                json!({
                    "type": "object",
                    "properties": { "skill": skill, "files": files },
                    "required": ["files"],
                    "additionalProperties": false
                }),
                SideEffect::LocalReversible,
                Tier::Auto,
                AskLabels::default(),
            ),
            def(
                RESULTS,
                "Keep eval results",
                "The skill-evals agent's report of one run: kept on this computer for proposals and shares.",
                json!({
                    "type": "object",
                    "properties": {
                        "skill": skill, "version": { "type": "integer", "minimum": 1 }, "draft": draft,
                        "model": { "type": "string" }, "runner": { "type": "string" },
                        "cases": { "type": "array", "items": { "type": "object" } }
                    },
                    "required": ["model", "runner", "cases"],
                    "additionalProperties": false
                }),
                SideEffect::LocalReversible,
                Tier::Auto,
                AskLabels::default(),
            ),
            def(
                PROPOSE,
                "Propose changes to skills",
                "Proposes a draft as the skill's next version, with the scores of the current version and the draft from one eval run. Its owner decides.",
                json!({
                    "type": "object",
                    "properties": { "skill": skill, "draft": draft, "note": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS } },
                    "required": ["skill", "draft", "note"],
                    "additionalProperties": false
                }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the proposal", "Propose"),
            ),
            def(
                MERGE,
                "Merge changes to skills",
                "Makes a proposal the next version of one of the person's skills. A proposal that breaks a case that passed needs a reason.",
                json!({
                    "type": "object",
                    "properties": { "skill": skill, "proposal": { "type": "string" }, "reason": { "type": "string", "maxLength": MAX_TEXT_CHARS } },
                    "required": ["skill", "proposal"],
                    "additionalProperties": false
                }),
                SideEffect::Irreversible,
                Tier::Ask,
                AskLabels::new("Review the merge", "Merge"),
            ),
        ]
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        let login = |what: &str| json!({ "type": "string", "description": what });
        let breaks = json!({ "type": "array", "items": { "type": "string" }, "description": "the cases that passed before and fail with it" });
        Some(match action.as_str() {
            LIST => json!({
                "type": "object",
                "properties": {
                    "skills": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "name": { "type": "string" },
                                "description": { "type": "string", "description": "what its SKILL.md says it does" },
                                "label": label_shape(),
                                "owner": login("its owner's login"),
                                "owner_name": { "type": "string" },
                                "mine": { "type": "boolean", "description": "the person owns it" },
                                "version": { "type": ["integer", "null"], "description": "its latest; null before one arrives" },
                                "score": {
                                    "type": ["object", "null"],
                                    "description": "its latest version's evals; null before they ran",
                                    "properties": {
                                        "summary": { "type": "string", "description": "such as \"4 of 5 cases (80%)\"" },
                                        "passed": { "type": "integer", "description": "cases passed" },
                                        "cases": { "type": "integer" },
                                        "model": { "type": "string" }
                                    },
                                    "required": ["summary", "passed", "cases", "model"],
                                    "additionalProperties": false
                                },
                                "open_proposals": { "type": "integer" },
                                "contributors": { "type": "integer", "description": "people whose proposals were merged" }
                            },
                            "required": ["id", "name", "description", "label", "owner", "owner_name", "mine", "version", "score", "open_proposals", "contributors"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["skills"],
                "additionalProperties": false
            }),
            READ => json!({
                "type": "object",
                "description": "a draft read alone has draft, label and files; a skill has the rest",
                "properties": {
                    "draft": { "type": "string", "description": "the draft's id" },
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                    "label": label_shape(),
                    "owner": login("its owner's login"),
                    "owner_name": { "type": "string" },
                    "mine": { "type": "boolean" },
                    "people": {
                        "type": "array",
                        "description": "who it's shared with",
                        "items": {
                            "type": "object",
                            "properties": { "login": { "type": "string" }, "name": { "type": "string" } },
                            "required": ["login", "name"],
                            "additionalProperties": false
                        }
                    },
                    "sha256": { "type": "string", "description": "the bundle read" },
                    "files": files_shape(),
                    "versions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "number": { "type": "integer" },
                                "note": { "type": "string" },
                                "by": login("who made it, their login"),
                                "by_name": { "type": "string" },
                                "at": { "type": "string", "description": "when it was posted" },
                                "merged": { "type": ["string", "null"], "description": "the proposal it merged; null for the owner's own" },
                                "scores": scores_shape(true)
                            },
                            "required": ["number", "note", "by", "by_name", "at", "merged", "scores"],
                            "additionalProperties": false
                        }
                    },
                    "proposals": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "by": login("who proposed it, their login"),
                                "by_name": { "type": "string" },
                                "attribution": { "type": ["string", "null"], "description": "the agent that made it for them, such as \"by claude-code for priya\"" },
                                "note": { "type": "string", "description": "what it changes and why" },
                                "at": { "type": "string", "description": "when it was posted" },
                                "base": { "type": "integer", "description": "the version it changes" },
                                "before": scores_shape(false),
                                "after": scores_shape(false),
                                "merged": { "type": "boolean" },
                                "breaks": breaks
                            },
                            "required": ["id", "by", "by_name", "attribution", "note", "at", "base", "before", "after", "merged", "breaks"],
                            "additionalProperties": false
                        }
                    },
                    "results_here": { "type": "array", "description": "eval runs this computer kept for the bundle read", "items": scores_shape(false) }
                },
                "required": ["label", "files"],
                "additionalProperties": false
            }),
            SHARE => json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "the skill's id" },
                    "name": { "type": "string" },
                    "label": label_shape(),
                    "version": { "type": "integer" },
                    "people": { "type": "array", "items": { "type": "string" }, "description": "logins it's shared with" },
                    "scores": scores_shape(true)
                },
                "required": ["id", "name", "label", "version", "people", "scores"],
                "additionalProperties": false
            }),
            DRAFT => json!({
                "type": "object",
                "properties": {
                    "draft": { "type": "string", "description": "the draft's id, as skill.read, skill.share and skill.propose take it" },
                    "name": { "type": "string" },
                    "cases": { "type": "integer", "description": "its eval cases" },
                    "calls": { "type": "integer", "description": "the model calls one eval run makes" }
                },
                "required": ["draft", "name", "cases", "calls"],
                "additionalProperties": false
            }),
            RESULTS => json!({
                "type": "object",
                "properties": { "summary": { "type": "string", "description": "such as \"4 of 5 cases (80%)\"" } },
                "required": ["summary"],
                "additionalProperties": false
            }),
            PROPOSE => json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "the proposal's id" },
                    "skill": { "type": "string" },
                    "base": { "type": "integer", "description": "the version it changes" },
                    "breaks": breaks
                },
                "required": ["id", "skill", "base", "breaks"],
                "additionalProperties": false
            }),
            MERGE => json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "the skill's id" },
                    "version": { "type": "integer", "description": "its new version" },
                    "merged": { "type": "string", "description": "the proposal merged" }
                },
                "required": ["id", "version", "merged"],
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
        let me = Self::me(ctx);
        match action.as_str() {
            LIST => {
                let _: NoInput = parse_input(input)?;
                Ok(Preview {
                    summary: "See skills".into(),
                    targets: vec!["Skills".into()],
                    ..Preview::default()
                })
            }
            READ => {
                let input: ReadInput = parse_input(input)?;
                // Reading one raises the task's label to its own.
                let (name, label) = match (&input.skill, &input.draft) {
                    (Some(id), _) => self.label_of(ctx, id),
                    (None, Some(d)) => self.draft(me, d).map_or(("a draft".into(), None), |d| {
                        ("a draft".into(), Some(d.label))
                    }),
                    _ => ("a skill".into(), None),
                };
                Ok(Preview {
                    summary: format!("Read {name}"),
                    targets: vec![name],
                    label,
                    ..Preview::default()
                })
            }
            SHARE => {
                let input: ShareInput = parse_input(input)?;
                let (bundle, _) = self.given(me, input.files.clone(), input.draft.as_deref())?;
                let checked = bundle.check().map_err(invalid)?;
                let people = self.audience(me, &input.with)?;
                let mut items = vec![format!("{}: {}", checked.name, checked.description)];
                items.push(if people.is_empty() {
                    "Only you can see it".into()
                } else {
                    format!(
                        "Shared with {}",
                        people
                            .iter()
                            .map(|p| self.name(p))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                });
                items.push(format!(
                    "{} files, {} eval cases",
                    bundle.files.len(),
                    checked.evals.cases.len()
                ));
                Ok(Preview {
                    summary: format!("Share the skill {}", checked.name),
                    targets: vec![checked.name],
                    items,
                    ..Preview::default()
                })
            }
            DRAFT => {
                let input: DraftInput = parse_input(input)?;
                let name = Bundle { files: input.files }
                    .check()
                    .map(|c| c.name)
                    .map_err(invalid)?;
                Ok(Preview {
                    summary: format!("Draft {name} on this computer"),
                    targets: vec![name],
                    ..Preview::default()
                })
            }
            RESULTS => {
                let _: ResultsInput = parse_input(input)?;
                Ok(Preview {
                    summary: "Keep eval results on this computer".into(),
                    targets: vec!["Skills".into()],
                    ..Preview::default()
                })
            }
            PROPOSE => {
                let input: ProposeInput = parse_input(input)?;
                let (name, _) = self.label_of(ctx, &input.skill);
                let mut items = vec![input.note.trim().to_owned()];
                if let (Ok(skill), Ok(draft)) =
                    (self.skill(me, &input.skill), self.draft(me, &input.draft))
                    && let Some(base) = skill.latest()
                    && let Some((before, after)) =
                        Self::scored_pair(&self.load(me), &base.sha256, &draft.id)
                {
                    items.push(format!(
                        "Scores: {} → {}",
                        before.summary(),
                        after.summary()
                    ));
                }
                Ok(Preview {
                    summary: format!("Propose a change to {name}"),
                    targets: vec![name],
                    items,
                    ..Preview::default()
                })
            }
            MERGE => {
                let input: MergeInput = parse_input(input)?;
                let (name, _) = self.label_of(ctx, &input.skill);
                let mut items = Vec::new();
                if let Ok(skill) = self.skill(me, &input.skill)
                    && let Some(p) = skill.proposal(&input.proposal)
                {
                    items.push(format!("{}'s change: {}", self.name(&p.by), p.note));
                    items.push(format!(
                        "Scores: {} → {}",
                        p.before.summary(),
                        p.after.summary()
                    ));
                    let breaks = p.before.broken_by(&p.after);
                    if !breaks.is_empty() {
                        items.push(format!("Breaks: {}", breaks.join(", ")));
                    }
                }
                Ok(Preview {
                    summary: format!("Merge a change to {name}"),
                    targets: vec![name],
                    items,
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
            DRAFT => self.make_draft(ctx, parse_input(input)?),
            RESULTS => self.results(ctx, parse_input(input)?),
            PROPOSE => self.propose(ctx, parse_input(input)?),
            MERGE => self.merge(ctx, parse_input(input)?),
            other => Err(ActionError::NotFound(format!("no action {other}"))),
        }
    }
}

#[cfg(test)]
mod tests;
