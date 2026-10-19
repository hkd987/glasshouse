//! The console's state and everything IT does to it: the policy draft,
//! signed versions and rings (AD-2, AD-3), the private catalog (AD-5,
//! DV-10, DV-11), the people devices get (AD-1), and the fleet's reports
//! (AD-4). Kept as JSON files in the
//! state directory, a stand-in for the managed database our web servers
//! will use; each change is written whole with a rename.
//!
//! The org's signing key lives here and nowhere else (DV-1, DV-10).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use glass_managed::{
    CatalogItem, CatalogRelease, DeviceReport, DocKind, InstallMode, OrgKey, PolicyRelease, Ring,
    Signed, StoreRefusal, SyncRequest, SyncResponse,
};
use glass_policy::Policy;
use glass_policy::document::PolicyDocument;
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

use crate::directory::{Directory, EVERYONE};

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
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetState {
    pub devices: BTreeMap<String, Device>,
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
    pub fleet: FleetState,
    /// The fleet changed since it was last written.
    pub fleet_dirty: bool,
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
            fleet: read_json(&dir.join("fleet.json"))?.unwrap_or_default(),
            fleet_dirty: false,
        };
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
        Ok(state)
    }

    pub fn public_key(&self) -> String {
        self.key.public().to_string()
    }

    // ---- policy (AD-2, AD-3) --------------------------------------------

    pub fn draft(&self) -> anyhow::Result<PolicyDocument> {
        Ok(toml::from_str(&self.policy.draft)?)
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

    /// The remote help roles in force (ADR 0028): the newest policy's, the
    /// one the pilot ring runs. None before anything is published.
    pub fn support_in_force(&self) -> glass_policy::document::Support {
        self.in_force(Ring::Pilot)
            .and_then(|v| toml::from_str::<PolicyDocument>(&v.text).ok())
            .map(|doc| doc.support)
            .unwrap_or_default()
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

    /// Publishes the draft to the pilot ring as the next version (AD-3).
    pub fn publish(
        &mut self,
        directory: &dyn Directory,
        note: &str,
        by: &str,
    ) -> Result<u64, Refused> {
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
    pub fn promote(&mut self, to: Ring) -> Result<u64, Refused> {
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
    pub fn rollback(
        &mut self,
        directory: &dyn Directory,
        to: u64,
        by: &str,
    ) -> Result<u64, Refused> {
        let earlier = self
            .policy
            .versions
            .iter()
            .find(|v| v.version == to)
            .ok_or_else(|| Refused::NotFound(format!("There is no v{to}.")))?;
        let release: PolicyRelease =
            serde_json::from_str(&earlier.signed.payload).map_err(internal)?;
        let draft: PolicyDocument = toml::from_str(&release.policy).map_err(internal)?;
        self.policy.draft = toml::to_string(&draft).map_err(internal)?;
        let version = self
            .release_policy(directory, &format!("Rolled back to v{to}"), by, Some(to))
            .map_err(|e| Refused::Invalid(vec![format!("{e:#}")]))?;
        for ring in Ring::ALL {
            self.policy.rings.insert(ring, version);
        }
        self.save_policy().map_err(internal)?;
        Ok(version)
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
        group: &str,
        mode: InstallMode,
        pin: bool,
        by: &str,
    ) -> Result<u64, Refused> {
        let entry = self.item_mut(id)?;
        entry.review = Review::Approved;
        entry.item.groups.insert(group.to_owned(), mode);
        entry.item.pinned = pin;
        entry.decided_by = Some(by.to_owned());
        self.release_catalog()
    }

    /// IT blocks an item: it leaves the catalog devices get.
    pub fn block(&mut self, id: &str, by: &str) -> Result<u64, Refused> {
        let entry = self.item_mut(id)?;
        entry.review = Review::Blocked;
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
                .filter(|i| i.review == Review::Approved)
                .map(|i| i.item.clone())
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
        SyncResponse {
            ring,
            policy,
            catalog,
            directory,
        }
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
