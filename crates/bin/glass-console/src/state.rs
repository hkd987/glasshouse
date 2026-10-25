//! The console's state and everything IT does to it: the policy draft,
//! signed versions and rings (AD-2, AD-3), the private catalog (AD-5,
//! DV-10, DV-11), the people devices get (AD-1), and the fleet's reports
//! (AD-4). Kept as JSON files in the
//! state directory, a stand-in for the managed database our web servers
//! will use; each change is written whole with a rename.
//!
//! The org's signing key lives here and nowhere else (DV-1, DV-10).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use glass_api::AppManifest;
use glass_app_studio::diff::FileChange;
use glass_managed::hwpatch::Bundle as PatchBundle;
use glass_managed::hwreview::{
    OrgPatch, PatchFetch, PatchSubmission, PatchSubmitted, bundle_digest,
};
use glass_managed::kbuild::BuildRequest;
use glass_managed::studio::{
    Bundle, BundleRequest, OrgApp, ShareTo, SharedBase, Submission, Submitted,
};
use glass_managed::{
    CatalogItem, CatalogRelease, DeviceReport, DocKind, InstallMode, OrgKey, PolicyRelease, Ring,
    Signed, StoreRefusal, SyncRequest, SyncResponse,
};
use glass_policy::Policy;
use glass_policy::document::PolicyDocument;
use glass_types::{ActionName, AgentId, Timestamp};
use serde::{Deserialize, Serialize};

use crate::directory::{Directory, EVERYONE};
use crate::studio::{Base, Builder, Check, Checked};

/// The org this console serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Org {
    /// The id every release is signed for.
    pub id: String,
    pub name: String,
    /// What a device sends to enroll and sync (AD-10's stand-in).
    pub enrollment_token: String,
}

/// One published policy version (AD-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyVersion {
    pub version: u64,
    /// The whole `policy.toml` devices enforce, members filled in.
    pub text: String,
    pub note: String,
    pub by: String,
    pub at: Timestamp,
    #[serde(default)]
    pub rolls_back_to: Option<u64>,
    pub signed: Signed,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyState {
    /// IT's working copy, as policy text.
    pub draft: String,
    pub versions: Vec<PolicyVersion>,
    /// The version each ring has reached; later rings never ahead.
    pub rings: BTreeMap<Ring, u64>,
}

/// Where an item is in IT's review (AD-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    Waiting,
    Approved,
    Blocked,
}

/// An item in the org's catalog, with IT's review around it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsoleItem {
    pub item: CatalogItem,
    pub review: Review,
    /// Who asked IT for it.
    #[serde(default)]
    pub requested_by: String,
    /// The submission's automated checks, as reported with it (DV-2).
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub decided_by: Option<String>,
    /// An app someone made and shared: what the console made of it
    /// (ADR 0032, R2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<Shared>,
    /// The version computers have while a newer one waits for review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<CatalogItem>,
    /// A kernel patch an agent wrote on one of the org's computers, sent
    /// for review (ADR 0057, slice 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<PatchShared>,
}

/// Patches waiting for their build at once, at most.
const MAX_PATCH_BUILDS: usize = 16;

/// A kernel patch sent for IT's review, and its build here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchShared {
    /// The login of whoever asked for the fix.
    pub person: String,
    pub device: String,
    /// The computer's make and model.
    pub computer: String,
    /// What they said was wrong.
    pub asked: String,
    /// The patch's source, as the computer sent it.
    pub request: BuildRequest,
    pub build: PatchBuild,
    pub submitted_at: Timestamp,
    /// What an agent made of this version, when IT asked one to review it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_review: Option<crate::hardware::AgentReview>,
}

/// The build service's build of a patch sent for review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PatchBuild {
    Building,
    Built { log: String, bytes: u64 },
    Failed { log: String },
}

impl ConsoleItem {
    /// The version computers get, if any.
    pub fn live(&self) -> Option<&CatalogItem> {
        match self.review {
            Review::Approved => Some(&self.item),
            Review::Waiting => self.approved.as_ref(),
            Review::Blocked => None,
        }
    }
}

/// An app someone made and shared to the store, as the console checked it
/// (ADR 0032, R2). Its source, the console's bundle and the change from
/// what it remixes are kept beside the catalog, by version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shared {
    /// The login of whoever made it.
    pub maker: String,
    /// Their own id and version for it.
    pub maker_app: AgentId,
    pub maker_version: u32,
    pub device: String,
    pub share: ShareTo,
    /// What it's for, in the maker's words.
    pub note: String,
    #[serde(default)]
    pub based_on: Option<SharedBase>,
    /// What the computer's own checks noted.
    #[serde(default)]
    pub device_notes: Vec<String>,
    /// The console's checks.
    pub checks: Vec<Check>,
    /// The privilege delta.
    pub beyond_base: Vec<ActionName>,
    /// What can't be undone: a person must review it (DV-4).
    pub irreversible: Vec<ActionName>,
    pub submitted_at: Timestamp,
}

/// What a shared app will be in the catalog, worked out before the console
/// checks it.
#[derive(Debug, Clone)]
pub struct SharePlan {
    pub id: AgentId,
    pub version: u32,
    /// Its manifest with its catalog id, version and name.
    pub manifest: AppManifest,
    /// What it remixes, as the console has it.
    pub base: Option<Base>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogState {
    pub items: Vec<ConsoleItem>,
    /// The catalog release devices get: approved items only.
    #[serde(default)]
    pub released: Option<Signed>,
}

/// The people devices get (AD-1): the directory's last signed release.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DirectoryState {
    #[serde(default)]
    pub released: Option<Signed>,
}

/// A device in the fleet (AD-4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub ring: Ring,
    /// What kind of computer IT says it is (`kiosk`, `lab`), for roles
    /// that may reach it with nobody there (ADR 0028); none by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub enrolled: Timestamp,
    pub last_seen: Timestamp,
    #[serde(default)]
    pub report: Option<DeviceReport>,
    /// The computer's own key (ADR 0030 slice P), uncompressed SEC1 in
    /// hex: every request but registering it is signed with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// When IT removed it from the organization: refused from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Timestamp>,
}

/// Who a signed request comes from, as far as the fleet knows.
#[derive(Debug)]
pub enum Computer {
    /// Registered, with this key.
    Key(glass_crypto::PublicKey),
    /// IT removed it.
    Revoked,
    /// No key registered under that id.
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetState {
    pub devices: BTreeMap<String, Device>,
    /// The keys of computers IT removed and then forgot (ADR 0030 slice P,
    /// ADR 0052): one asking to register again is still refused as
    /// removed, while a computer set up again, with a new key, enrolls.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub removed_keys: BTreeSet<String>,
}

/// Why a computer's key wasn't registered.
#[derive(Debug, PartialEq, Eq)]
pub enum RegisterRefused {
    /// Bad input or a proof that doesn't hold: 400.
    Invalid(String),
    /// IT removed this computer: 403.
    Revoked,
    /// A rule says no: 409.
    Rule(String),
}

/// Why IT's request didn't go through, in words for the page.
#[derive(Debug)]
pub enum Refused {
    /// Bad input: 400.
    Invalid(Vec<String>),
    /// Nothing by that name: 404.
    NotFound(String),
    /// A rule says no: 409.
    Rule(String),
}

impl From<StoreRefusal> for Refused {
    fn from(e: StoreRefusal) -> Self {
        Self::Rule(format!("Not published to the public store: {e}."))
    }
}

pub struct State {
    dir: PathBuf,
    key: OrgKey,
    pub org: Org,
    pub policy: PolicyState,
    pub catalog: CatalogState,
    pub people: DirectoryState,
    /// Chat (ADR 0030): sealed, so nothing here reads it.
    pub chat: crate::chat::ChatState,
    pub fleet: FleetState,
    /// The fleet changed since it was last written.
    pub fleet_dirty: bool,
    /// Model credits (ADR 0039): what the org bought, and its key.
    pub credits: crate::credits::Credits,
    /// What each computer spent on them.
    pub usage: crate::credits::Usage,
    /// The pilot version's support roles, parsed once: every console
    /// request reads them, and a version's text never changes (ADR 0054).
    support: std::sync::Mutex<Option<(u64, glass_policy::document::Support)>>,
}

impl State {
    /// Opens the state in `dir`, creating the org, its key and a first
    /// policy version on first start: the shipped default, safe as it is,
    /// with the PRD's remote help tiers (CF-81).
    pub fn open(
        dir: &Path,
        org_id: &str,
        org_name: &str,
        directory: &dyn Directory,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let key_path = dir.join("org.key");
        let key = if key_path.exists() {
            let meta = std::fs::metadata(&key_path)?;
            if std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o077 != 0 {
                bail!(
                    "{} can be read by others; it must be mode 0600",
                    key_path.display()
                );
            }
            OrgKey::from_seed_hex(&std::fs::read_to_string(&key_path)?)
                .map_err(anyhow::Error::msg)?
        } else {
            let key = OrgKey::from_seed(random()?);
            write_private(&key_path, &key.seed_hex())?;
            key
        };
        let org = match read_json(&dir.join("org.json"))? {
            Some(org) => org,
            None => {
                let org = Org {
                    id: org_id.into(),
                    name: org_name.into(),
                    enrollment_token: hex::encode(random()?),
                };
                write_json(&dir.join("org.json"), &org)?;
                org
            }
        };
        let mut state = Self {
            dir: dir.to_owned(),
            key,
            org,
            policy: read_json(&dir.join("policy.json"))?.unwrap_or_default(),
            catalog: read_json(&dir.join("catalog.json"))?.unwrap_or_default(),
            people: read_json(&dir.join("people.json"))?.unwrap_or_default(),
            chat: read_json(&dir.join("chat.json"))?.unwrap_or_default(),
            fleet: read_json(&dir.join("fleet.json"))?.unwrap_or_default(),
            fleet_dirty: false,
            credits: crate::credits::Credits::load(dir)?,
            usage: crate::credits::Usage::load(dir)?,
            support: std::sync::Mutex::new(None),
        };
        if crate::chat::open_log(&mut state.chat, &dir.join(CHAT_LOG))? {
            // Moved to the log: chat.json is kept without them.
            state.save_chat()?;
        }
        if state.policy.versions.is_empty() {
            if state.policy.draft.is_empty() {
                state.policy.draft = format!(
                    "{}\n{}",
                    glass_policy::DEFAULT_POLICY,
                    glass_policy::SUPPORT_TIERS
                );
            }
            let version = state.release_policy(
                directory,
                "The default policy, with help desk, desktop support and security tiers",
                "console",
                None,
            )?;
            for ring in Ring::ALL {
                state.policy.rings.insert(ring, version);
            }
            state.save_policy()?;
        }
        state.ensure_chat()?;
        Ok(state)
    }

    pub fn public_key(&self) -> String {
        self.key.public().to_string()
    }

    /// Where the state is kept.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // ---- policy (AD-2, AD-3) --------------------------------------------

    pub fn draft(&self) -> anyhow::Result<PolicyDocument> {
        Ok(toml::from_str(&self.policy.draft)?)
    }

    /// What names the draft as it is now: a preview shows it, and publish
    /// takes only the draft its preview showed (AD-3, ADR 0053).
    pub fn draft_digest(&self) -> String {
        glass_managed::sha256_hex(self.policy.draft.as_bytes())
    }

    /// Replaces the draft, if it validates as devices will check it.
    pub fn save_draft(&mut self, doc: &PolicyDocument) -> Result<(), Refused> {
        if let Err(e) = Policy::from_document(doc.clone()) {
            return Err(Refused::Invalid(errors(&e)));
        }
        self.policy.draft =
            toml::to_string(doc).map_err(|e| Refused::Invalid(vec![e.to_string()]))?;
        self.save_policy().map_err(internal)?;
        Ok(())
    }

    /// Applies one of IT's edits to the draft, if the result validates.
    pub fn edit_draft(&mut self, edit: crate::edits::Edit) -> Result<(), Refused> {
        let mut doc = self.draft().map_err(internal)?;
        crate::edits::apply(&mut doc, edit).map_err(|e| Refused::Invalid(vec![e]))?;
        self.save_draft(&doc)
    }

    /// Throws the draft away: back to what the pilot ring runs.
    pub fn discard_draft(&mut self) -> Result<(), Refused> {
        let text = self
            .in_force(Ring::Pilot)
            .map(|v| v.text.clone())
            .ok_or_else(|| Refused::Rule("Nothing published yet.".into()))?;
        let doc: PolicyDocument = toml::from_str(&text).map_err(internal)?;
        self.save_draft(&doc)
    }

    /// The version every ring has reached, or `None` before any.
    pub fn in_force(&self, ring: Ring) -> Option<&PolicyVersion> {
        let version = self.policy.rings.get(&ring)?;
        self.policy.versions.iter().find(|v| v.version == *version)
    }

    /// The models the org's published policies run on its credits (ADR
    /// 0039), by the provider's id (`openrouter/auto`): the only ones the
    /// console forwards for its computers.
    pub fn credit_models(&self) -> std::collections::BTreeSet<String> {
        use glass_policy::document::ProviderKind;
        let mut models = std::collections::BTreeSet::new();
        for ring in Ring::ALL {
            let Some(doc) = self
                .in_force(ring)
                .and_then(|v| toml::from_str::<PolicyDocument>(&v.text).ok())
            else {
                continue;
            };
            let credits: Vec<&str> = doc
                .models
                .providers
                .iter()
                .filter(|p| p.kind == ProviderKind::Credits)
                .map(|p| p.id.as_str())
                .collect();
            models.extend(
                doc.models
                    .models
                    .iter()
                    .filter(|m| credits.contains(&m.provider.as_str()))
                    .map(|m| m.model.clone()),
            );
        }
        models
    }

    /// Writes what computers spent on credits, if anything changed.
    pub fn save_usage(&mut self) -> anyhow::Result<()> {
        let dir = self.dir.clone();
        self.usage.save(&dir)
    }

    /// The remote help roles in force (ADR 0028): the newest policy's, the
    /// one the pilot ring runs. None before anything is published.
    pub fn support_in_force(&self) -> glass_policy::document::Support {
        let Some(pilot) = self.in_force(Ring::Pilot) else {
            return Default::default();
        };
        let mut kept = self
            .support
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((version, support)) = kept.as_ref()
            && *version == pilot.version
        {
            return support.clone();
        }
        let support = toml::from_str::<PolicyDocument>(&pilot.text)
            .map(|doc| doc.support)
            .unwrap_or_default();
        *kept = Some((pilot.version, support.clone()));
        support
    }

    pub fn latest_version(&self) -> u64 {
        self.policy
            .versions
            .iter()
            .map(|v| v.version)
            .max()
            .unwrap_or(0)
    }

    /// The draft as devices will get it: each group's members from the
    /// directory, since devices resolve groups from policy (ADR 0023).
    pub fn draft_for_devices(&self, directory: &dyn Directory) -> anyhow::Result<String> {
        let mut doc = self.draft()?;
        for group in &mut doc.groups {
            if group.name != EVERYONE {
                group.members = directory
                    .members(&group.name)
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
            }
        }
        Ok(toml::to_string(&doc)?)
    }

    /// Signs the draft as the next version. Doesn't move any ring.
    fn release_policy(
        &mut self,
        directory: &dyn Directory,
        note: &str,
        by: &str,
        rolls_back_to: Option<u64>,
    ) -> anyhow::Result<u64> {
        let text = self.draft_for_devices(directory)?;
        Policy::from_toml(&text).map_err(|e| anyhow::anyhow!("{e}"))?;
        let version = self.latest_version() + 1;
        let payload = serde_json::to_string(&PolicyRelease {
            policy: text.clone(),
            note: note.into(),
            rolls_back_to,
        })?;
        let signed = self
            .key
            .sign(DocKind::Policy, &self.org.id, version, payload);
        self.policy.versions.push(PolicyVersion {
            version,
            text,
            note: note.into(),
            by: by.into(),
            at: Timestamp::now(),
            rolls_back_to,
            signed,
        });
        Ok(version)
    }

    /// Publishes the draft to the pilot ring as the next version (AD-3):
    /// the draft `previewed` names ([`draft_digest`](Self::draft_digest)),
    /// so another admin's edit since IT's preview never goes out unseen
    /// (specs/tla/PolicyConsole.tla).
    pub fn publish(
        &mut self,
        directory: &dyn Directory,
        note: &str,
        by: &str,
        previewed: &str,
    ) -> Result<u64, Refused> {
        if previewed != self.draft_digest() {
            return Err(Refused::Rule(
                "The draft changed since you previewed it, by another admin or by you: \
                 preview it again, then publish."
                    .into(),
            ));
        }
        let draft = self.draft_for_devices(directory).map_err(internal)?;
        if self.in_force(Ring::Pilot).is_some_and(|v| v.text == draft) {
            return Err(Refused::Rule(
                "Nothing to publish: the draft is what the pilot ring runs.".into(),
            ));
        }
        let version = self
            .release_policy(directory, note, by, None)
            .map_err(|e| Refused::Invalid(vec![format!("{e:#}")]))?;
        self.policy.rings.insert(Ring::Pilot, version);
        self.save_policy().map_err(internal)?;
        Ok(version)
    }

    /// Moves the next ring up to the one before it: pilot → early → broad.
    /// `seen` is the version IT saw the ring before run: one published or
    /// promoted since goes on only once IT has seen it (DV-12, ADR 0053).
    pub fn promote(&mut self, to: Ring, seen: u64) -> Result<u64, Refused> {
        let from = match to {
            Ring::Early => Ring::Pilot,
            Ring::Broad => Ring::Early,
            Ring::Pilot => {
                return Err(Refused::Rule(
                    "Publishing is what reaches the pilot ring.".into(),
                ));
            }
        };
        let version = *self
            .policy
            .rings
            .get(&from)
            .ok_or_else(|| Refused::Rule("Nothing to promote.".into()))?;
        if version != seen {
            return Err(Refused::Rule(format!(
                "The {} ring runs v{version} now, not the v{seen} you saw: see how it runs, \
                 then promote it.",
                from.as_str()
            )));
        }
        if self.policy.rings.get(&to) == Some(&version) {
            return Err(Refused::Rule(format!(
                "The {} ring already runs v{version}.",
                to.as_str()
            )));
        }
        self.policy.rings.insert(to, version);
        self.save_policy().map_err(internal)?;
        Ok(version)
    }

    /// Puts an earlier version's content back everywhere at once, as a new
    /// version: versions only go up, so devices can refuse replays (AD-3).
    /// The draft becomes that content too; whether that threw away edits
    /// not yet published, which IT is then told (AD-2, ADR 0053).
    pub fn rollback(
        &mut self,
        directory: &dyn Directory,
        to: u64,
        by: &str,
    ) -> Result<(u64, bool), Refused> {
        let earlier = self
            .policy
            .versions
            .iter()
            .find(|v| v.version == to)
            .ok_or_else(|| Refused::NotFound(format!("There is no v{to}.")))?;
        let release: PolicyRelease =
            serde_json::from_str(&earlier.signed.payload).map_err(internal)?;
        let draft: PolicyDocument = toml::from_str(&release.policy).map_err(internal)?;
        let unpublished = self.draft_for_devices(directory).map_err(internal)?;
        let discarded = self
            .in_force(Ring::Pilot)
            .is_none_or(|v| v.text != unpublished);
        self.policy.draft = toml::to_string(&draft).map_err(internal)?;
        let version = self
            .release_policy(directory, &format!("Rolled back to v{to}"), by, Some(to))
            .map_err(|e| Refused::Invalid(vec![format!("{e:#}")]))?;
        for ring in Ring::ALL {
            self.policy.rings.insert(ring, version);
        }
        self.save_policy().map_err(internal)?;
        Ok((version, discarded))
    }

    fn save_policy(&self) -> anyhow::Result<()> {
        write_json(&self.dir.join("policy.json"), &self.policy)
    }

    // ---- catalog (AD-5, DV-10, DV-11) -----------------------------------

    pub fn item(&self, id: &str) -> Result<&ConsoleItem, Refused> {
        self.catalog
            .items
            .iter()
            .find(|i| i.item.id.as_str() == id)
            .ok_or_else(|| Refused::NotFound(format!("No catalog item {id}.")))
    }

    fn item_mut(&mut self, id: &str) -> Result<&mut ConsoleItem, Refused> {
        self.catalog
            .items
            .iter_mut()
            .find(|i| i.item.id.as_str() == id)
            .ok_or_else(|| Refused::NotFound(format!("No catalog item {id}.")))
    }

    /// Adds an item waiting for review. Its id may not be another item's,
    /// whatever the kind: apps and agents share one namespace.
    pub fn submit(&mut self, item: ConsoleItem) -> Result<(), Refused> {
        if self.catalog.items.iter().any(|i| i.item.id == item.item.id) {
            return Err(Refused::Rule(format!(
                "{} is already an item's id; apps, agents and flows share one namespace.",
                item.item.id
            )));
        }
        self.catalog.items.push(item);
        self.save_catalog().map_err(internal)
    }

    /// IT approves an item for a group with an install mode (DV-11), and a
    /// new signed catalog goes to every device.
    pub fn approve(
        &mut self,
        id: &str,
        version: &str,
        group: &str,
        mode: InstallMode,
        pin: bool,
        by: &str,
    ) -> Result<u64, Refused> {
        let entry = self.item_mut(id)?;
        // GA-13: IT approves the version it reviewed. One shared while IT
        // read the last never goes out unseen (specs/tla/StoreShare.tla).
        if entry.item.version != version {
            return Err(Refused::Rule(format!(
                "Version {} of {} came in while you were reviewing version {version}; review it \
                 before you approve it.",
                entry.item.version, entry.item.name
            )));
        }
        if entry.hardware.is_some() {
            let item = entry.item.clone();
            let patch = self.sign_patch(&item, mode, by)?;
            let entry = self.item_mut(id)?;
            entry.item.patch = Some(patch);
        }
        let entry = self.item_mut(id)?;
        entry.review = Review::Approved;
        entry.approved = None;
        entry.item.groups.insert(group.to_owned(), mode);
        entry.item.pinned = pin;
        entry.decided_by = Some(by.to_owned());
        self.release_catalog()
    }

    // ---- kernel patches sent for review (ADR 0057, slice 3) -------------

    /// Where a patch's version keeps its source, the build service's build
    /// and, once IT approved it, the reviewed bundle.
    fn patch_dir(&self, id: &AgentId, version: &str) -> PathBuf {
        self.dir.join("patches").join(id.as_str()).join(version)
    }

    /// A computer sends an agent's patch for review: the next version of
    /// the item it waits as, built next.
    pub fn take_patch(
        &mut self,
        submission: &PatchSubmission,
        device: &str,
        directory: &dyn Directory,
    ) -> Result<PatchSubmitted, Refused> {
        submission.check().map_err(|e| Refused::Invalid(vec![e]))?;
        let request = &submission.request;
        let id: AgentId = request.id.parse().map_err(|_| {
            Refused::Invalid(vec![format!("{} can't be a catalog item's id", request.id)])
        })?;
        let current = self.catalog.items.iter().find(|i| i.item.id == id);
        if current.is_some_and(|i| i.hardware.is_none()) {
            return Err(Refused::Rule(format!(
                "{id} is already an app's or agent's id; the patch needs its own."
            )));
        }
        // One build of a patch at a time, and a bounded queue: a computer
        // can't keep the build service busy for everyone else.
        let building = |i: &ConsoleItem| {
            i.hardware
                .as_ref()
                .is_some_and(|h| h.build == PatchBuild::Building)
        };
        if current.is_some_and(building) {
            return Err(Refused::Rule(format!(
                "{id} is still being built here; send it again once it's built."
            )));
        }
        if self.catalog.items.iter().filter(|i| building(i)).count() >= MAX_PATCH_BUILDS {
            return Err(Refused::Rule(
                "The console is building as many patches as it takes at once; send it again \
                 later."
                    .into(),
            ));
        }
        let version = current
            .map_or(1, |i| i.item.version.parse::<u32>().unwrap_or(0) + 1)
            .to_string();
        let dir = self.patch_dir(&id, &version);
        std::fs::create_dir_all(&dir).map_err(internal)?;
        write_json(&dir.join("request.json"), request).map_err(internal)?;
        let person = directory.display_name(&submission.person);
        let item = CatalogItem {
            id: id.clone(),
            kind: glass_managed::ItemKind::HardwarePatch,
            name: request.summary.clone(),
            version: version.clone(),
            publisher: format!("An agent, for {person}"),
            publisher_verified: false,
            internal: true,
            summary: request.summary.clone(),
            keywords: request.targets.clone(),
            declares: glass_managed::Declared::default(),
            groups: BTreeMap::new(),
            pinned: false,
            provenance: Default::default(),
            app: None,
            patch: None,
        };
        let shared = PatchShared {
            person: submission.person.clone(),
            device: device.to_owned(),
            computer: submission.computer.clone(),
            asked: submission.asked.clone(),
            request: request.clone(),
            build: PatchBuild::Building,
            submitted_at: Timestamp::now(),
            agent_review: None,
        };
        match self.catalog.items.iter_mut().find(|i| i.item.id == id) {
            Some(entry) => {
                if entry.review == Review::Approved {
                    entry.approved = Some(entry.item.clone());
                }
                let groups = entry.item.groups.clone();
                entry.item = CatalogItem { groups, ..item };
                entry.review = Review::Waiting;
                entry.requested_by = person.clone();
                entry.decided_by = None;
                entry.hardware = Some(shared);
            }
            None => self.catalog.items.push(ConsoleItem {
                item,
                review: Review::Waiting,
                requested_by: person.clone(),
                checks: Vec::new(),
                decided_by: None,
                shared: None,
                approved: None,
                hardware: Some(shared),
            }),
        }
        self.save_catalog().map_err(internal)?;
        Ok(PatchSubmitted {
            id: id.to_string(),
            version,
            text: format!(
                "Sent \"{}\" to IT for review. Once they approve it, it's in the Store's \
                 Hardware category for computers like this one.",
                request.summary
            ),
        })
    }

    /// The patches waiting to be built: after a restart, those a build was
    /// under way for.
    pub fn patches_to_build(&self) -> Vec<(AgentId, String, BuildRequest)> {
        self.catalog
            .items
            .iter()
            .filter_map(|i| {
                let h = i.hardware.as_ref()?;
                (h.build == PatchBuild::Building)
                    .then(|| (i.item.id.clone(), i.item.version.clone(), h.request.clone()))
            })
            .collect()
    }

    /// The build service's answer for version `version` of `id`: kept if
    /// it's what was asked for, signed by `key`. A newer version sent since
    /// makes it moot.
    pub fn patch_built(
        &mut self,
        id: &AgentId,
        version: &str,
        result: Result<glass_managed::kbuild::BuildResult, String>,
        key: &glass_managed::PublicKey,
    ) -> Result<(), Refused> {
        let dir = self.patch_dir(id, version);
        let Some(entry) = self
            .catalog
            .items
            .iter_mut()
            .find(|i| i.item.id == *id && i.item.version == version)
        else {
            return Ok(());
        };
        let Some(shared) = entry.hardware.as_mut() else {
            return Ok(());
        };
        shared.build = match result {
            Err(why) => PatchBuild::Failed {
                log: format!("The build service couldn't build it: {why}"),
            },
            Ok(built) => match built.bundle.filter(|_| built.ok) {
                None => PatchBuild::Failed { log: built.log },
                Some(bundle) => match crate::hardware::check_build(&shared.request, &bundle, key) {
                    Err(why) => PatchBuild::Failed { log: why },
                    Ok(()) => {
                        write_json(&dir.join("built.json"), &bundle).map_err(internal)?;
                        PatchBuild::Built {
                            log: built.log,
                            bytes: serde_json::to_vec(&bundle).map_or(0, |b| b.len() as u64),
                        }
                    }
                },
            },
        };
        self.save_catalog().map_err(internal)
    }

    /// IT approves a patch: its build, signed again with the org's key as
    /// reviewed, is what computers fetch.
    fn sign_patch(
        &self,
        item: &CatalogItem,
        mode: InstallMode,
        by: &str,
    ) -> Result<OrgPatch, Refused> {
        if mode == InstallMode::Required {
            return Err(Refused::Rule(
                "A kernel patch is never pushed: make it available, and the person at each \
                 computer says yes."
                    .into(),
            ));
        }
        let entry = self.item(item.id.as_str())?;
        let shared = entry
            .hardware
            .as_ref()
            .ok_or_else(|| Refused::Rule(format!("{} isn't a kernel patch", item.id)))?;
        if !matches!(shared.build, PatchBuild::Built { .. }) {
            return Err(Refused::Rule(
                "It hasn't been built: only a patch that built can be approved.".into(),
            ));
        }
        let dir = self.patch_dir(&item.id, &item.version);
        let built: PatchBundle = read_json(&dir.join("built.json"))
            .map_err(internal)?
            .ok_or_else(|| Refused::Rule("Its build is missing; send it again.".into()))?;
        let revision = item.version.parse::<u64>().unwrap_or(1);
        let reviewed = crate::hardware::reviewed(&built, &self.key, revision).map_err(internal)?;
        write_json(&dir.join("reviewed.json"), &reviewed).map_err(internal)?;
        let bytes = serde_json::to_vec(&reviewed).map_or(0, |b| b.len() as u64);
        Ok(OrgPatch {
            patch: shared.request.id.clone(),
            release: shared.request.release.clone(),
            targets: shared.request.targets.clone(),
            revision,
            bundle_sha256: bundle_digest(&reviewed),
            bytes,
            kernel: shared.request.kernel,
            reviewed_by: by.to_owned(),
        })
    }

    /// Version `version` of kernel patch `id`, for an agent to review: the
    /// one on IT's screen, once its build is done.
    pub fn patch_for_review(&self, id: &str, version: &str) -> Result<PatchShared, Refused> {
        let entry = self.item(id)?;
        let shared = entry
            .hardware
            .as_ref()
            .ok_or_else(|| Refused::Rule(format!("{id} isn't a kernel patch")))?;
        if entry.item.version != version {
            return Err(Refused::Rule(format!(
                "Version {} came in since: review that one.",
                entry.item.version
            )));
        }
        if shared.build == PatchBuild::Building {
            return Err(Refused::Rule(
                "It's still being built: the agent reads it with its build.".into(),
            ));
        }
        Ok(shared.clone())
    }

    /// Keeps an agent's review of version `version` of `id`, unless a newer
    /// version came in while it read.
    pub fn keep_agent_review(
        &mut self,
        id: &str,
        version: &str,
        review: crate::hardware::AgentReview,
    ) -> Result<(), Refused> {
        let entry = self.item_mut(id)?;
        if entry.item.version != version {
            return Err(Refused::Rule(format!(
                "Version {} came in while the agent read version {version}: review that one.",
                entry.item.version
            )));
        }
        let Some(shared) = entry.hardware.as_mut() else {
            return Err(Refused::Rule(format!("{id} isn't a kernel patch")));
        };
        shared.agent_review = Some(review);
        self.save_catalog().map_err(internal)
    }

    /// A reviewed patch's bundle, for a computer, only at the revision the
    /// signed catalog gives computers.
    pub fn patch_bundle(&self, fetch: &PatchFetch) -> Result<PatchBundle, Refused> {
        let gone = || {
            Refused::NotFound(format!(
                "{} revision {} isn't in the catalog",
                fetch.id, fetch.revision
            ))
        };
        let live = self
            .catalog
            .items
            .iter()
            .find(|i| i.item.id.as_str() == fetch.id)
            .and_then(ConsoleItem::live)
            .filter(|live| {
                live.patch
                    .as_ref()
                    .is_some_and(|p| p.revision == fetch.revision)
            })
            .ok_or_else(gone)?;
        read_json(
            &self
                .patch_dir(&live.id, &live.version)
                .join("reviewed.json"),
        )
        .ok()
        .flatten()
        .ok_or_else(gone)
    }

    /// IT blocks an item: it leaves the catalog devices get.
    pub fn block(&mut self, id: &str, by: &str) -> Result<u64, Refused> {
        let entry = self.item_mut(id)?;
        entry.review = Review::Blocked;
        entry.approved = None;
        entry.decided_by = Some(by.to_owned());
        self.release_catalog()
    }

    /// The store rule: an org's items never go to the public store.
    pub fn publish_public(&self, id: &str) -> Result<(), Refused> {
        self.item(id)?.item.public_store_check()?;
        Err(Refused::Rule(
            "The public store isn't connected yet.".into(),
        ))
    }

    /// Signs the approved items as the next catalog version.
    pub fn release_catalog(&mut self) -> Result<u64, Refused> {
        let release = CatalogRelease {
            items: self
                .catalog
                .items
                .iter()
                .filter_map(ConsoleItem::live)
                .cloned()
                .collect(),
        };
        release.check().map_err(Refused::Rule)?;
        let version = self.catalog.released.as_ref().map_or(0, |s| s.version) + 1;
        let payload = serde_json::to_string(&release).map_err(internal)?;
        self.catalog.released =
            Some(
                self.key
                    .sign(DocKind::Catalog, &self.org.id, version, payload),
            );
        self.save_catalog().map_err(internal)?;
        Ok(version)
    }

    pub fn save_catalog(&self) -> anyhow::Result<()> {
        write_json(&self.dir.join("catalog.json"), &self.catalog)
    }

    // ---- apps people share (ADR 0032, R2; GA-8, GA-13) ------------------

    /// Where a shared app's version keeps its source, the console's bundle
    /// and the change from what it remixes.
    fn shared_dir(&self, id: &AgentId, version: &str) -> PathBuf {
        self.dir.join("apps").join(id.as_str()).join(version)
    }

    /// The item `maker` shared their app `app` as, if they shared it before.
    fn shared_as(&self, maker: &str, app: &AgentId) -> Option<&ConsoleItem> {
        self.catalog.items.iter().find(|i| {
            i.shared
                .as_ref()
                .is_some_and(|s| s.maker == maker && s.maker_app == *app)
        })
    }

    /// What `submission` will be in the catalog: the next version of the
    /// item its maker shared it as before, or a new item named for it.
    pub fn share_plan(
        &self,
        submission: &Submission,
        builder: &Builder,
        directory: &dyn Directory,
    ) -> Result<SharePlan, Refused> {
        let invalid = |why: String| Refused::Invalid(vec![why]);
        if !submission.app.is_personal() || submission.manifest.id != submission.app {
            return Err(invalid("only people's own apps are shared this way".into()));
        }
        if !directory.users().iter().any(|u| u.name == submission.maker) {
            return Err(invalid(format!(
                "{} isn't in the organization's directory",
                submission.maker
            )));
        }
        if let ShareTo::Team { group } = &submission.share
            && !directory
                .members(group)
                .contains(&submission.maker.as_str())
        {
            return Err(invalid(format!(
                "{} isn't in the {group} group",
                submission.maker
            )));
        }
        if submission.note.trim().is_empty() {
            return Err(invalid("say what it's for".into()));
        }
        let before = self.shared_as(&submission.maker, &submission.app);
        // A new version keeps its name in the store unless the person
        // named it: an agent's guess never renames it for everyone.
        let name = match before {
            Some(item) if !submission.named => item.item.name.trim(),
            _ => submission.name.trim(),
        };
        let ours = builder.our_apps();
        // A name colleagues could take for another app's is refused.
        let same = |other: &str| other.trim().eq_ignore_ascii_case(name);
        let taken_name = ours
            .iter()
            .map(|m| m.name.as_str())
            .chain(
                self.catalog
                    .items
                    .iter()
                    .filter(|i| before.is_none_or(|b| b.item.id != i.item.id))
                    .map(|i| i.item.name.as_str()),
            )
            .find(|other| same(other));
        if let Some(other) = taken_name {
            return Err(Refused::Rule(format!(
                "The store already has an app called {other}; name it for what it does, like \
                 \"{other}, with …\"."
            )));
        }
        let (id, version) = match before {
            Some(item) => (
                item.item.id.clone(),
                item.item.version.parse::<u32>().unwrap_or(0) + 1,
            ),
            None => {
                let mut taken: Vec<AgentId> = self
                    .catalog
                    .items
                    .iter()
                    .map(|i| i.item.id.clone())
                    .collect();
                taken.extend(ours.iter().map(|m| m.id.clone()));
                if let Ok(doc) = self.draft() {
                    taken.extend(doc.agents.iter().map(|a| a.id.clone()));
                }
                let stem = glass_app_studio::slug(name);
                let stem = if stem.is_empty() {
                    "shared-app".to_owned()
                } else {
                    stem
                };
                let id = glass_app_studio::unique_id(&stem, &taken)
                    .ok_or_else(|| Refused::Rule("Name it something more its own.".into()))?;
                (id, 1)
            }
        };
        let mut manifest = submission.manifest.clone();
        manifest.id = id.clone();
        manifest.version = version.to_string();
        manifest.name = name.to_owned();
        let base = submission
            .based_on
            .as_ref()
            .and_then(|b| builder.ours(&b.id).or_else(|| self.org_base(&b.id)));
        Ok(SharePlan {
            id,
            version,
            manifest,
            base,
        })
    }

    /// An app shared earlier, as computers have it, to remix again.
    fn org_base(&self, id: &AgentId) -> Option<Base> {
        let live = self
            .catalog
            .items
            .iter()
            .find(|i| i.item.id == *id)?
            .live()?;
        let app = live.app.as_ref()?;
        let files = read_json(&self.shared_dir(id, &live.version).join("source.json"))
            .ok()
            .flatten()?;
        Some(Base {
            manifest: app.manifest.clone(),
            files,
        })
    }

    /// Keeps a submission the console checked as `plan`, waiting for IT's
    /// review. A new version of an approved app leaves the approved one
    /// with computers until IT decides.
    pub fn take_share(
        &mut self,
        plan: SharePlan,
        submission: Submission,
        checked: Checked,
        directory: &dyn Directory,
    ) -> Result<Submitted, Refused> {
        let Some(bundle) = checked.bundle.clone().filter(|_| checked.passed()) else {
            return Err(Refused::Invalid(checked.failures()));
        };
        // Another submission of it may have come in while this one built.
        let current = self.catalog.items.iter().find(|i| i.item.id == plan.id);
        let expected = current.map_or(1, |i| i.item.version.parse::<u32>().unwrap_or(0) + 1);
        let same_maker = current.is_none_or(|i| {
            i.shared
                .as_ref()
                .is_some_and(|s| s.maker == submission.maker && s.maker_app == submission.app)
        });
        if expected != plan.version || !same_maker {
            return Err(Refused::Rule(
                "It changed while the console checked it; share it again.".into(),
            ));
        }
        let version = plan.version.to_string();
        let dir = self.shared_dir(&plan.id, &version);
        let keep = || -> anyhow::Result<()> {
            std::fs::create_dir_all(&dir)?;
            write_json(&dir.join("source.json"), &submission.files)?;
            write_json(&dir.join("changes.json"), &checked.changes)?;
            write_json(
                &dir.join("bundle.json"),
                &Bundle {
                    app: plan.id.clone(),
                    version: version.clone(),
                    files: bundle.clone(),
                },
            )
        };
        keep().map_err(internal)?;
        let app = OrgApp {
            manifest: plan.manifest.clone(),
            bundle_sha256: glass_managed::studio::digest(&bundle),
            source_sha256: glass_managed::studio::digest(&submission.files),
            made_by: submission.maker.clone(),
            based_on: submission
                .based_on
                .as_ref()
                .map(|b| format!("{}@{}", b.id, b.version)),
        };
        let maker = directory.display_name(&submission.maker);
        let shared = Shared {
            maker: submission.maker.clone(),
            maker_app: submission.app.clone(),
            maker_version: submission.version,
            device: submission.device.clone(),
            share: submission.share.clone(),
            note: submission.note.trim().to_owned(),
            based_on: submission.based_on.clone(),
            device_notes: submission.notes.clone(),
            checks: checked.checks.clone(),
            beyond_base: checked.beyond_base.clone(),
            irreversible: checked.irreversible.clone(),
            submitted_at: Timestamp::now(),
        };
        let item = CatalogItem {
            id: plan.id.clone(),
            kind: glass_managed::ItemKind::App,
            name: plan.manifest.name.clone(),
            version: version.clone(),
            publisher: maker.clone(),
            publisher_verified: false,
            internal: true,
            summary: shared.note.clone(),
            keywords: Vec::new(),
            declares: glass_managed::Declared {
                actions: plan
                    .manifest
                    .actions
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                default_tiers: BTreeMap::new(),
                max_label: plan.manifest.max_label(),
                egress: Vec::new(),
            },
            groups: BTreeMap::new(),
            pinned: false,
            provenance: Default::default(),
            app: Some(app),
            patch: None,
        };
        match self.catalog.items.iter_mut().find(|i| i.item.id == plan.id) {
            Some(entry) => {
                if entry.review == Review::Approved {
                    entry.approved = Some(entry.item.clone());
                }
                // Who it reaches carries over; IT may change it on approval.
                let (groups, pinned) = (entry.item.groups.clone(), entry.item.pinned);
                entry.item = CatalogItem {
                    groups,
                    pinned,
                    ..item
                };
                entry.review = Review::Waiting;
                entry.requested_by = maker.clone();
                entry.decided_by = None;
                entry.shared = Some(shared);
            }
            None => self.catalog.items.push(ConsoleItem {
                item,
                review: Review::Waiting,
                requested_by: maker.clone(),
                checks: Vec::new(),
                decided_by: None,
                shared: Some(shared),
                hardware: None,
                approved: None,
            }),
        }
        self.save_catalog().map_err(internal)?;
        let audience = match &submission.share {
            ShareTo::Team { group } => format!("the {} team", directory.group_display(group)),
            ShareTo::Org => "everyone in the organization".into(),
        };
        Ok(Submitted {
            id: plan.id,
            text: format!(
                "Sent {} to IT for review as version {version}. Once they approve it, {audience} \
                 can install it from the Store.",
                plan.manifest.name
            ),
        })
    }

    /// The change a shared app's waiting version makes, file by file.
    pub fn changes(&self, id: &str) -> Vec<FileChange> {
        let Ok(entry) = self.item(id) else {
            return Vec::new();
        };
        read_json(
            &self
                .shared_dir(&entry.item.id, &entry.item.version)
                .join("changes.json"),
        )
        .ok()
        .flatten()
        .unwrap_or_default()
    }

    /// The console's bundle of a shared app, for a computer, only at the
    /// version the signed catalog gives computers.
    pub fn org_bundle(&self, request: &BundleRequest) -> Result<Bundle, Refused> {
        let gone = || {
            Refused::NotFound(format!(
                "{} {} isn't in the catalog",
                request.app, request.version
            ))
        };
        let live = self
            .catalog
            .items
            .iter()
            .find(|i| i.item.id == request.app)
            .and_then(ConsoleItem::live)
            .filter(|live| live.app.is_some() && live.version == request.version)
            .ok_or_else(gone)?;
        read_json(&self.shared_dir(&live.id, &live.version).join("bundle.json"))
            .ok()
            .flatten()
            .ok_or_else(gone)
    }

    // ---- people (AD-1) ---------------------------------------------------

    /// Signs the directory's people as the next directory release, if they
    /// changed since the last one (a restart signs nothing new). Its
    /// version, when it signed one.
    pub fn release_directory(&mut self, directory: &dyn Directory) -> anyhow::Result<Option<u64>> {
        let release = directory.release();
        release.check().map_err(anyhow::Error::msg)?;
        let payload = serde_json::to_string(&release)?;
        if payload.len() > glass_managed::directory::MAX_PAYLOAD_BYTES {
            bail!(
                "the directory is {} bytes, more than devices take in one release ({})",
                payload.len(),
                glass_managed::directory::MAX_PAYLOAD_BYTES
            );
        }
        let unchanged = self
            .people
            .released
            .as_ref()
            .is_some_and(|s| s.payload_sha256 == glass_managed::sha256_hex(payload.as_bytes()));
        if unchanged {
            return Ok(None);
        }
        let version = self.people.released.as_ref().map_or(0, |s| s.version) + 1;
        self.people.released =
            Some(
                self.key
                    .sign(DocKind::Directory, &self.org.id, version, payload),
            );
        write_json(&self.dir.join("people.json"), &self.people)?;
        Ok(Some(version))
    }

    // ---- devices (AD-4, AD-10) ------------------------------------------

    /// A device's ring; one IT hasn't seen yet joins the broad ring.
    fn device(&mut self, id: &str) -> &mut Device {
        self.fleet_dirty = true;
        self.fleet
            .devices
            .entry(id.to_owned())
            .or_insert_with(|| Device {
                id: id.to_owned(),
                ring: Ring::Broad,
                kind: None,
                enrolled: Timestamp::now(),
                last_seen: Timestamp::now(),
                report: None,
                key: None,
                revoked: None,
            })
    }

    /// What a device should apply: anything newer than what it named, for
    /// its ring.
    pub fn sync(&mut self, request: &SyncRequest) -> SyncResponse {
        let device = self.device(&request.device);
        device.last_seen = Timestamp::now();
        let ring = device.ring;
        let policy = self
            .in_force(ring)
            .filter(|v| request.policy.is_none_or(|have| v.version > have))
            .map(|v| v.signed.clone());
        let catalog = self
            .catalog
            .released
            .as_ref()
            .filter(|s| request.catalog.is_none_or(|have| s.version > have))
            .cloned();
        let directory = self
            .people
            .released
            .as_ref()
            .filter(|s| request.directory.is_none_or(|have| s.version > have))
            .cloned();
        let chat = self
            .chat
            .release
            .as_ref()
            .filter(|s| request.chat.is_none_or(|have| s.version > have))
            .cloned();
        SyncResponse {
            ring,
            policy,
            catalog,
            directory,
            chat,
            credits: self.credits.for_device(request.credits),
            os_release: None,
        }
    }

    // ---- chat (ADR 0030) -------------------------------------------------

    /// Makes IT's compliance key, if there is none, and signs the release
    /// that gives every computer its public half. Development only: the
    /// private half is kept here, mode 0600. Slice C3 moves making it to
    /// IT's own browser, so our servers never hold it.
    pub fn ensure_chat(&mut self) -> anyhow::Result<()> {
        if self.chat.release.is_some() {
            return Ok(());
        }
        let secret = glass_crypto::SecretKey::generate().map_err(anyhow::Error::msg)?;
        let path = self.dir.join("compliance-dev.key");
        if !path.exists() {
            write_private(
                &path,
                &hex::encode(secret.to_bytes().map_err(anyhow::Error::msg)?),
            )?;
        }
        let secret = glass_crypto::SecretKey::from_bytes(&hex::decode(
            std::fs::read_to_string(&path)?.trim(),
        )?)
        .map_err(anyhow::Error::msg)?;
        let release =
            glass_managed::ChatRelease::new(&secret.public().map_err(anyhow::Error::msg)?);
        self.chat.compliance = release.compliance.clone();
        let payload = serde_json::to_string(&release)?;
        self.chat.release = Some(self.key.sign(DocKind::Chat, &self.org.id, 1, payload));
        self.save_chat()
    }

    // ---- the shared space's blobs (ADR 0033) ----------------------------

    fn blob_path(&self, id: &str) -> PathBuf {
        self.dir.join("blobs").join(format!("{id}.json"))
    }

    /// Keeps a sealed blob for a conversation its sender's computer is in.
    /// The console stores its ciphertext and can't open it.
    pub fn put_blob(&self, request: &glass_managed::shared::PutBlob) -> Result<(), Refused> {
        request
            .blob
            .check()
            .map_err(|e| Refused::Invalid(vec![e]))?;
        self.chat
            .member_here(&request.device, &request.login, &request.blob.conversation)?;
        let path = self.blob_path(&request.blob.id);
        if path.exists() {
            return Err(Refused::Rule("That blob is kept already.".into()));
        }
        std::fs::create_dir_all(self.dir.join("blobs")).map_err(internal)?;
        write_json(&path, &request.blob).map_err(internal)
    }

    /// A blob, for a computer of someone in its conversation.
    pub fn get_blob(
        &self,
        request: &glass_managed::shared::GetBlob,
    ) -> Result<glass_managed::shared::SealedBlob, Refused> {
        if !glass_managed::shared::is_blob_id(&request.id) {
            return Err(Refused::Invalid(vec![
                "a blob's id is bl- and 32 hex digits".into(),
            ]));
        }
        self.chat
            .member_here(&request.device, &request.login, &request.conversation)?;
        read_json::<glass_managed::shared::SealedBlob>(&self.blob_path(&request.id))
            .ok()
            .flatten()
            .filter(|b| b.conversation == request.conversation)
            .ok_or_else(|| Refused::NotFound("No such blob.".into()))
    }

    /// Chat's keys, conversations and epochs; its messages have a log of
    /// their own ([`save_chat_message`](Self::save_chat_message)).
    pub fn save_chat(&self) -> anyhow::Result<()> {
        write_json(&self.dir.join("chat.json"), &self.chat)
    }

    /// Keeps the message a post just took: a line on the chat log, not the
    /// org's history again (ADR 0052).
    pub fn save_chat_message(&self) -> anyhow::Result<()> {
        match self.chat.messages.last() {
            Some(message) => crate::chat::append_log(&self.dir.join(CHAT_LOG), message),
            None => Ok(()),
        }
    }

    /// Registers a computer's key (ADR 0030 slice P), with the proof that
    /// it holds it. The first key under an id is the one; the same key
    /// again is fine; another key is refused until IT forgets the old
    /// computer, and a removed computer is refused.
    pub fn register_device(
        &mut self,
        registration: &glass_managed::device::RegisterDevice,
    ) -> Result<(), RegisterRefused> {
        let id = registration.device.as_str();
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err(RegisterRefused::Invalid("name the device".into()));
        }
        registration.verify().map_err(RegisterRefused::Invalid)?;
        if self.fleet.removed_keys.contains(&registration.key) {
            return Err(RegisterRefused::Revoked);
        }
        if let Some(existing) = self.fleet.devices.get(id) {
            if existing.revoked.is_some() {
                return Err(RegisterRefused::Revoked);
            }
            match &existing.key {
                Some(key) if *key == registration.key => return Ok(()),
                Some(_) => {
                    return Err(RegisterRefused::Rule(format!(
                        "{id} is enrolled with another key. If this is the same computer, \
                         set up again, IT can remove {id} on the Devices page and it can \
                         enroll again."
                    )));
                }
                None => {}
            }
        }
        let device = self.device(id);
        device.key = Some(registration.key.clone());
        device.last_seen = Timestamp::now();
        Ok(())
    }

    /// The key a computer signs with, if it may still sign.
    pub fn computer(&self, id: &str) -> Computer {
        let Some(device) = self.fleet.devices.get(id) else {
            return Computer::Unknown;
        };
        if device.revoked.is_some() {
            return Computer::Revoked;
        }
        device
            .key
            .as_deref()
            .and_then(|k| hex::decode(k).ok())
            .and_then(|k| glass_crypto::PublicKey::from_bytes(&k).ok())
            .map_or(Computer::Unknown, Computer::Key)
    }

    /// Whether `key` (hex) is still the key `id` signs with: checked again
    /// under the lock after a request's signature was, so a computer IT
    /// removed or forgot meanwhile gets nothing more, and no entry is made
    /// for it (ADR 0052, the DeviceEnrollment model).
    /// The ring a computer is in: a new one is in the broad ring.
    pub fn ring_of(&self, id: &str) -> Ring {
        self.fleet.devices.get(id).map_or(Ring::Broad, |d| d.ring)
    }

    pub fn still_signs(&self, id: &str, key: &str) -> Computer {
        match self.computer(id) {
            Computer::Key(found)
                if self.fleet.devices.get(id).and_then(|d| d.key.as_deref()) == Some(key) =>
            {
                Computer::Key(found)
            }
            Computer::Revoked => Computer::Revoked,
            _ => Computer::Unknown,
        }
    }

    /// IT removes a computer from the organization (ADR 0030 slice P):
    /// from now on the console refuses everything it sends.
    pub fn revoke_device(&mut self, id: &str) -> Result<(), Refused> {
        let device = self
            .fleet
            .devices
            .get_mut(id)
            .ok_or_else(|| Refused::NotFound(format!("No device {id}.")))?;
        if device.revoked.is_none() {
            device.revoked = Some(Timestamp::now());
            self.fleet_dirty = true;
        }
        Ok(())
    }

    /// IT forgets a removed computer, so a computer can enroll under its
    /// id again (one set up again from scratch).
    pub fn forget_device(&mut self, id: &str) -> Result<(), Refused> {
        let device = self
            .fleet
            .devices
            .get(id)
            .ok_or_else(|| Refused::NotFound(format!("No device {id}.")))?;
        if device.revoked.is_none() {
            return Err(Refused::Rule(format!(
                "Remove {id} from the organization first."
            )));
        }
        if let Some(key) = device.key.clone() {
            self.fleet.removed_keys.insert(key);
        }
        self.fleet.devices.remove(id);
        self.fleet_dirty = true;
        Ok(())
    }

    pub fn report(&mut self, report: DeviceReport) {
        let device = self.device(&report.device.clone());
        device.last_seen = Timestamp::now();
        device.report = Some(report);
    }

    pub fn set_ring(&mut self, id: &str, ring: Ring) -> Result<(), Refused> {
        let device = self
            .fleet
            .devices
            .get_mut(id)
            .ok_or_else(|| Refused::NotFound(format!("No device {id}.")))?;
        device.ring = ring;
        self.fleet_dirty = true;
        Ok(())
    }

    /// Gives a device a kind for unattended access (ADR 0028), or none.
    pub fn set_kind(&mut self, id: &str, kind: Option<String>) -> Result<(), Refused> {
        if let Some(k) = &kind
            && (k.is_empty()
                || k.len() > 32
                || !k
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'))
        {
            return Err(Refused::Invalid(vec![
                "a kind is lowercase letters, digits, - and _".into(),
            ]));
        }
        let device = self
            .fleet
            .devices
            .get_mut(id)
            .ok_or_else(|| Refused::NotFound(format!("No device {id}.")))?;
        device.kind = kind;
        self.fleet_dirty = true;
        Ok(())
    }

    pub fn save_fleet(&mut self) -> anyhow::Result<()> {
        if self.fleet_dirty {
            write_json(&self.dir.join("fleet.json"), &self.fleet)?;
            self.fleet_dirty = false;
        }
        Ok(())
    }
}

fn internal(e: impl std::fmt::Display) -> Refused {
    Refused::Invalid(vec![e.to_string()])
}

fn errors(e: &glass_policy::PolicyError) -> Vec<String> {
    match e {
        glass_policy::PolicyError::Invalid(list) => list.clone(),
        other => vec![other.to_string()],
    }
}

/// 32 bytes from the system's random source.
pub fn random() -> anyhow::Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom")?;
    Ok(bytes)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Writes a sibling file and renames it over `path`.
/// Chat's sealed messages, a line each (ADR 0052).
const CHAT_LOG: &str = "chat-messages.jsonl";

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, serde_json::to_vec(value)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

fn write_private(path: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(text.as_bytes())?;
    Ok(())
}
