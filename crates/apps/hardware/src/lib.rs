//! The `hardware` app's actions (ADR 0057, slice 2): what an agent reads and
//! does to fix this computer's hardware with a kernel patch.
//!
//! - `hardware.report`: what this OS makes of each part (DK-12).
//! - `hardware.source_search` and `hardware.source_read`: the base
//!   system's kernel source of the release this computer runs, held by the
//!   build service, never on the computer.
//! - `hardware.patch_build`: a patch the agent wrote, built by the build
//!   service into a bundle it signs as a community patch, or answered with
//!   the compiler's errors. Nothing on the computer changes. The bundle is
//!   kept here, root's; the agent gets its id.
//! - `hardware.patch_propose`: a built patch set up to try at the next
//!   restart, as `glassctl hardware patch apply` does. The person says yes
//!   every time: policy can't lower it below Ask, and no approval covers
//!   the rest of the task (`glass_policy::EVERY_TIME`). The person reads
//!   the diff in the review. Then it's tried, on probation and kept, or
//!   goes back by itself, like any kernel patch.
//!
//! On a managed computer that doesn't take patches nobody reviewed (IT's
//! `hardware.community_patches`), `hardware.patch_propose` sends the
//! patch's source to IT for review instead (slice 3): nothing changes on
//! the computer, and once IT approves it, it's in the Store's Hardware
//! category for computers like it.
//!
//! glassd gives the handle the build service's client, the runtime's
//! kernel patches and, on a managed computer, the console's link once they
//! exist ([`HardwareHandle::set_service`], [`HardwareHandle::set_patches`],
//! [`HardwareHandle::set_review`]).

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext,
};
use glass_api::{AskLabels, ChangeKind, PatchTrial};
use glass_managed::hwreview::{MAX_ASKED, PatchLink, PatchSubmission};
use glass_managed::kbuild::{
    BuildModule, BuildRequest, BuildResult, SourceFound, SourceRead, SourceSearch, SourceText,
};
use glass_types::{ActionName, SideEffect, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

pub const REPORT: &str = "hardware.report";
pub const SOURCE_SEARCH: &str = "hardware.source_search";
pub const SOURCE_READ: &str = "hardware.source_read";
pub const PATCH_BUILD: &str = "hardware.patch_build";
pub const PATCH_PROPOSE: &str = "hardware.patch_propose";

/// Built patches kept (audit 5.3). A build is what an agent proposes
/// next, and what the person is asked about; it carries its modules and,
/// for some patches, a kernel: 40 to 160 MB. Every one was kept for good.
pub const BUILDS_KEPT: usize = 4;

/// The kernel patch build service (ADR 0057): glassd's client for it.
pub trait BuildService: Send + Sync {
    fn search(&self, request: &SourceSearch) -> Result<SourceFound, String>;
    fn read(&self, request: &SourceRead) -> Result<SourceText, String>;
    fn build(&self, request: &BuildRequest) -> Result<BuildResult, String>;
}

/// The runtime's kernel patches, as root sets them up.
pub trait Patches: Send + Sync {
    /// The release this computer runs: what patches are built for.
    fn release(&self) -> Result<String, String>;
    /// Sets up `bundle` to try at the next start (`hwpatch::apply`).
    fn apply(&self, bundle: &[u8]) -> Result<PatchTrial, String>;
    /// Whether [`Self::apply`] would take `bundle`, changing nothing.
    fn check(&self, bundle: &[u8]) -> Result<(), String>;
    /// Whether `bundle` could run here at all (its release, a driver the
    /// kernel has built in): what goes to IT for review must.
    fn fits(&self, bundle: &[u8]) -> Result<(), String>;
    /// Whether this computer tries patches nobody reviewed.
    fn takes_community(&self) -> bool;
}

/// What this computer's hardware report is, for tests to replace.
pub type ReportFn = Arc<dyn Fn() -> Result<glass_platform::hardware::Report, String> + Send + Sync>;

struct Inner {
    /// Where built bundles wait, root's.
    builds: PathBuf,
    service: RwLock<Option<Arc<dyn BuildService>>>,
    patches: RwLock<Option<Arc<dyn Patches>>>,
    /// IT's review, on a managed computer.
    review: RwLock<Option<Arc<dyn PatchLink>>>,
    report: ReportFn,
}

/// The hardware app's state on this computer.
#[derive(Clone)]
pub struct HardwareHandle(Arc<Inner>);

impl std::fmt::Debug for HardwareHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareHandle")
            .field("builds", &self.0.builds)
            .finish_non_exhaustive()
    }
}

impl HardwareHandle {
    /// Built bundles kept in `builds`, which glassd owns.
    pub fn new(builds: impl Into<PathBuf>) -> Self {
        Self::with_report(
            builds,
            Arc::new(|| glass_platform::hardware::report().map_err(|e| e.to_string())),
        )
    }

    pub fn with_report(builds: impl Into<PathBuf>, report: ReportFn) -> Self {
        Self(Arc::new(Inner {
            builds: builds.into(),
            service: RwLock::new(None),
            patches: RwLock::new(None),
            review: RwLock::new(None),
            report,
        }))
    }

    /// Patches this computer doesn't take go to IT for review through
    /// `link` (a managed computer).
    pub fn set_review(&self, link: Arc<dyn PatchLink>) {
        *self
            .0
            .review
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(link);
    }

    /// Where an agent's patch goes: tried here, or sent to IT.
    fn route(&self) -> Result<Route, ActionError> {
        if self.patches()?.takes_community() {
            return Ok(Route::Try);
        }
        match self
            .0
            .review
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            Some(link) => Ok(Route::Review(link)),
            None => Err(ActionError::Failed(
                "this computer takes only reviewed kernel patches, and an agent's patch is \
                 nobody's reviewed one: its owner can allow them in Settings"
                    .into(),
            )),
        }
    }

    /// What `build` was built from.
    fn request(&self, build: &str) -> Result<BuildRequest, ActionError> {
        let path = self.0.builds.join(format!("{build}.request.json"));
        let bytes = std::fs::read(&path).map_err(|_| not_kept(build))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| ActionError::Failed(format!("build {build}: {e}")))
    }

    pub fn set_service(&self, service: Arc<dyn BuildService>) {
        *self
            .0
            .service
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(service);
    }

    pub fn set_patches(&self, patches: Arc<dyn Patches>) {
        *self
            .0
            .patches
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(patches);
    }

    fn service(&self) -> Result<Arc<dyn BuildService>, ActionError> {
        self.0
            .service
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                ActionError::Failed("this computer has no kernel patch build service".into())
            })
    }

    fn patches(&self) -> Result<Arc<dyn Patches>, ActionError> {
        self.0
            .patches
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| ActionError::Failed("this computer takes no kernel patches".into()))
    }

    fn release(&self) -> Result<String, ActionError> {
        self.patches()?.release().map_err(ActionError::Failed)
    }

    /// A built bundle, by the id `hardware.patch_build` gave.
    fn bundle(
        &self,
        build: &str,
    ) -> Result<(Vec<u8>, glass_managed::hwpatch::Bundle), ActionError> {
        if build.len() != 16 || !build.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ActionError::InvalidInput(format!(
                "{build:?} isn't a build's id"
            )));
        }
        let path = self.0.builds.join(format!("{build}.bundle.json"));
        let bytes = std::fs::read(&path).map_err(|_| not_kept(build))?;
        let bundle = serde_json::from_slice(&bytes)
            .map_err(|e| ActionError::Failed(format!("build {build}: {e}")))?;
        Ok((bytes, bundle))
    }

    /// Keeps a built bundle and what it was built from, by an id for it.
    fn keep(
        &self,
        bundle: &glass_managed::hwpatch::Bundle,
        request: &BuildRequest,
    ) -> Result<String, ActionError> {
        let bytes = serde_json::to_vec(bundle).map_err(|e| ActionError::Failed(e.to_string()))?;
        let id = glass_managed::sha256_hex(&bytes)[..16].to_owned();
        std::fs::create_dir_all(&self.0.builds)
            .map_err(|e| ActionError::io(self.0.builds.display(), e))?;
        let source = serde_json::to_vec(request).map_err(|e| ActionError::Failed(e.to_string()))?;
        let path = self.0.builds.join(format!("{id}.request.json"));
        write_private(&path, &source).map_err(|e| ActionError::io(path.display(), e))?;
        let path = self.0.builds.join(format!("{id}.bundle.json"));
        write_private(&path, &bytes).map_err(|e| ActionError::io(path.display(), e))?;
        self.trim(&id);
        Ok(id)
    }

    /// Removes all but the newest [`BUILDS_KEPT`] builds, `kept` (the one
    /// just written) among them, and what a build that was never finished
    /// left. Best effort: a build that can't be removed is tried again
    /// after the next one.
    fn trim(&self, kept: &str) {
        let Ok(entries) = std::fs::read_dir(&self.0.builds) else {
            return;
        };
        let mut builds: Vec<(std::time::SystemTime, String)> = Vec::new();
        let mut requests: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if let Some(id) = name.strip_suffix(".bundle.json") {
                if id != kept
                    && let Ok(written) = entry.metadata().and_then(|m| m.modified())
                {
                    builds.push((written, id.to_owned()));
                }
            } else if let Some(id) = name.strip_suffix(".request.json") {
                requests.push(id.to_owned());
            }
        }
        builds.sort();
        let over = (builds.len() + 1).saturating_sub(BUILDS_KEPT);
        let remove = |id: &str, part: &str| {
            let _ = std::fs::remove_file(self.0.builds.join(format!("{id}.{part}")));
        };
        for (_, id) in builds.iter().take(over) {
            remove(id, "bundle.json");
            remove(id, "request.json");
        }
        // What a build was made from is written first: one whose bundle
        // never followed (glassd stopped between the two) is nobody's.
        for id in requests {
            if id != kept && !self.0.builds.join(format!("{id}.bundle.json")).exists() {
                remove(&id, "request.json");
            }
        }
    }
}

/// A build that isn't here: never made, or made before the newest
/// [`BUILDS_KEPT`].
fn not_kept(build: &str) -> ActionError {
    ActionError::NotFound(format!(
        "build {build} isn't kept here (the newest {BUILDS_KEPT} builds are): build the patch, \
         then propose that build"
    ))
}

/// Where an agent's patch goes (slice 3).
enum Route {
    /// Tried here at the next restart.
    Try,
    /// Sent to IT for review.
    Review(Arc<dyn PatchLink>),
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    pattern: String,
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[serde(default)]
    from: Option<u32>,
    #[serde(default)]
    lines: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildInput {
    id: String,
    summary: String,
    diff: String,
    #[serde(default)]
    modules: Vec<BuildModule>,
    #[serde(default)]
    kernel: bool,
    #[serde(default)]
    targets: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeInput {
    build: String,
    /// What the person said was wrong, in their words: IT reads it with a
    /// patch sent for review.
    #[serde(default)]
    problem: String,
}

#[derive(Debug, Clone)]
pub struct HardwareProvider {
    handle: HardwareHandle,
}

impl HardwareProvider {
    pub fn new(handle: HardwareHandle) -> Self {
        Self { handle }
    }
}

/// A diff's files and the lines it adds and removes, for the review.
fn diff_stats(diff: &str) -> (Vec<String>, usize, usize) {
    let files = diff
        .lines()
        .filter_map(|l| l.strip_prefix("+++ b/"))
        .map(|f| f.split_whitespace().next().unwrap_or("").to_owned())
        .collect();
    let added = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let removed = diff
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();
    (files, added, removed)
}

impl ActionProvider for HardwareProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let def = |name: &str, title: &str, description: &str, side_effect, tier, schema: Value| {
            ActionDef {
                name: name.parse().expect("static name"),
                title: title.into(),
                description: description.into(),
                input_schema: schema,
                side_effect,
                default_tier: tier,
                ask: AskLabels::default(),
            }
        };
        vec![
            def(
                REPORT,
                "Read the hardware report",
                "Each part of this computer (graphics, Wi-Fi, Ethernet, Bluetooth, camera, sound, \
                 touchpad, sleep, battery, TPM), its devices and drivers, and whether this OS \
                 drives it. Reads only.",
                SideEffect::None,
                Tier::Auto,
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            ),
            def(
                SOURCE_SEARCH,
                "Search the kernel's source",
                "Lines matching an extended regular expression in the kernel source of the \
                 release this computer runs, under sys/<path> (dev/iwlwifi, or empty for all). \
                 At most 200 matches. The build service holds the source.",
                SideEffect::Disclose,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }),
            ),
            def(
                SOURCE_READ,
                "Read the kernel's source",
                "Lines of a file in the kernel source of the release this computer runs, from \
                 sys/: at most 400 at a time, from line `from`.",
                SideEffect::Disclose,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "from": { "type": "integer", "minimum": 1 },
                        "lines": { "type": "integer", "minimum": 1, "maximum": 400 }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            ),
            def(
                PATCH_BUILD,
                "Build a kernel patch",
                "Builds a patch (a unified diff of files under sys/, a/ and b/ prefixes) for the \
                 release this computer runs: the modules it names (their folders under \
                 sys/modules), and the kernel if it changes code built into it. Answers with the \
                 compiler's errors, or a build id to propose. Nothing on this computer changes.",
                SideEffect::Disclose,
                Tier::Auto,
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "lowercase letters, digits and -" },
                        "summary": { "type": "string", "description": "what it fixes, for a person" },
                        "diff": { "type": "string" },
                        "modules": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "dir": { "type": "string" },
                                    "file": { "type": "string" },
                                    "load_at_boot": { "type": "boolean" }
                                },
                                "required": ["dir", "file"],
                                "additionalProperties": false
                            }
                        },
                        "kernel": { "type": "boolean" },
                        "targets": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["id", "summary", "diff"],
                    "additionalProperties": false
                }),
            ),
            ActionDef {
                ask: AskLabels::new("Read the patch", "Go ahead"),
                ..def(
                    PATCH_PROPOSE,
                    "Propose a kernel patch",
                    "Sets up a built patch to try at the next restart, with the person's yes, \
                     every time. It runs in the kernel, outside every sandbox; if the computer \
                     doesn't start or crashes before its probation ends, it goes back by itself. \
                     On a computer that takes only patches IT reviewed, it goes to IT for \
                     review instead: pass what the person said was wrong as problem.",
                    SideEffect::Irreversible,
                    Tier::Ask,
                    json!({
                        "type": "object",
                        "properties": {
                            "build": { "type": "string" },
                            "problem": { "type": "string", "description": "what the person said was wrong, in their words" }
                        },
                        "required": ["build"],
                        "additionalProperties": false
                    }),
                )
            },
        ]
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        Some(match action.as_str() {
            REPORT => json!({
                "type": "object",
                "properties": {
                    "maker": { "type": "string" },
                    "model": { "type": "string" },
                    "processor": { "type": "string" },
                    "memory_mb": { "type": "integer" },
                    "parts": { "type": "array", "items": { "type": "object" } },
                    "complete": { "type": "boolean" }
                },
                "required": ["maker", "model", "parts", "complete"]
            }),
            SOURCE_SEARCH => json!({
                "type": "object",
                "properties": {
                    "matches": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "file": { "type": "string" },
                                "line": { "type": "integer" },
                                "text": { "type": "string" }
                            },
                            "required": ["file", "line", "text"]
                        }
                    },
                    "truncated": { "type": "boolean" }
                },
                "required": ["matches", "truncated"]
            }),
            SOURCE_READ => json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "from": { "type": "integer" },
                    "text": { "type": "string" },
                    "total_lines": { "type": "integer" }
                },
                "required": ["path", "from", "text", "total_lines"]
            }),
            PATCH_BUILD => json!({
                "type": "object",
                "properties": {
                    "ok": { "type": "boolean" },
                    "log": { "type": "string", "description": "the compiler's errors, or what was built" },
                    "build": { "type": ["string", "null"], "description": "what hardware.patch_propose takes" }
                },
                "required": ["ok", "log", "build"]
            }),
            PATCH_PROPOSE => json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "state": { "type": "string", "description": "staged, or sent_for_review: IT reviews it first" },
                    "env": { "type": ["string", "null"], "description": "the boot environment it's tried in" }
                },
                "required": ["id", "state", "env"]
            }),
            _ => return None,
        })
    }

    fn preview(
        &self,
        _ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        match action.as_str() {
            REPORT => {
                let _: NoInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: "Read this computer's hardware report".into(),
                    targets: vec!["Hardware report".into()],
                    ..Preview::default()
                })
            }
            SOURCE_SEARCH => {
                let input: SearchInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: format!("Search the kernel's source for {:?}", input.pattern),
                    targets: vec![format!("sys/{}", input.path)],
                    ..Preview::default()
                })
            }
            SOURCE_READ => {
                let input: ReadInput = glass_actions::parse_input(input)?;
                Ok(Preview {
                    summary: format!("Read sys/{}", input.path),
                    targets: vec![format!("sys/{}", input.path)],
                    ..Preview::default()
                })
            }
            PATCH_BUILD => {
                let input: BuildInput = glass_actions::parse_input(input)?;
                let (files, added, removed) = diff_stats(&input.diff);
                Ok(Preview {
                    summary: format!(
                        "Build kernel patch {}: {} (+{added} −{removed})",
                        input.id, input.summary
                    ),
                    targets: files,
                    ..Preview::default()
                })
            }
            PATCH_PROPOSE => {
                let input: ProposeInput = glass_actions::parse_input(input)?;
                let (bytes, bundle) = self.handle.bundle(&input.build)?;
                // Nobody is asked about a patch this computer would refuse,
                // nor IT about one it couldn't run.
                let patches = self.handle.patches()?;
                if matches!(self.handle.route()?, Route::Review(_)) {
                    patches.fits(&bytes)
                } else {
                    patches.check(&bytes)
                }
                .map_err(ActionError::InvalidInput)?;
                let manifest: glass_managed::hwpatch::Manifest =
                    serde_json::from_str(&bundle.signed.payload)
                        .map_err(|e| ActionError::Failed(e.to_string()))?;
                let (files, added, removed) = diff_stats(&bundle.patch);
                let mut items = vec![
                    format!("What it fixes: {}", manifest.summary),
                    "Nobody reviewed it: an agent wrote it. It runs in the kernel, outside \
                     every sandbox."
                        .into(),
                    format!(
                        "It changes {} (+{added} −{removed}); the diff is below",
                        files.join(", ")
                    ),
                    format!(
                        "It replaces {}",
                        manifest
                            .modules
                            .iter()
                            .map(|m| m.file.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ];
                let review = matches!(self.handle.route()?, Route::Review(_));
                items.push(if review {
                    "This computer takes only patches IT reviewed, so nothing changes on it \
                     now: IT gets the patch's source and what you said was wrong, builds it \
                     and reads it. Once they approve it, it's in the Store's Hardware \
                     category for computers like this one."
                        .into()
                } else {
                    "The next restart tries it once. If the computer doesn't start, or \
                     crashes or hangs before a day of use, it goes back by itself."
                        .into()
                });
                let mut texts = vec![String::new(); items.len()];
                texts[2] = bundle.patch.clone();
                items.truncate(texts.len());
                Ok(Preview {
                    summary: if review {
                        format!("Send kernel patch {} to IT for review", manifest.id)
                    } else {
                        format!("Try kernel patch {} at the next restart", manifest.id)
                    },
                    targets: vec![format!("Kernel patch {}", manifest.id)],
                    items,
                    texts,
                    ..Preview::default()
                })
            }
            other => Err(ActionError::InvalidInput(format!(
                "hardware has no action {other}"
            ))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        let output = |text: String, data: Value| ActionOutput {
            text,
            data,
            changes: Vec::new(),
        };
        match action.as_str() {
            REPORT => {
                let _: NoInput = glass_actions::parse_input(input)?;
                let report = (self.handle.0.report)().map_err(ActionError::Failed)?;
                let blockers = report.blockers().count();
                Ok(output(
                    format!(
                        "{} {}: {} parts, {blockers} this OS can't drive",
                        report.maker,
                        report.model,
                        report.parts.len()
                    ),
                    serde_json::to_value(&report).unwrap_or_default(),
                ))
            }
            SOURCE_SEARCH => {
                let input: SearchInput = glass_actions::parse_input(input)?;
                if input.pattern.is_empty() || input.pattern.len() > 200 {
                    return Err(ActionError::InvalidInput(
                        "a pattern of 1 to 200 characters".into(),
                    ));
                }
                if !glass_managed::kbuild::valid_path(&input.path) {
                    return Err(ActionError::InvalidInput(format!(
                        "{:?} isn't a path under sys/",
                        input.path
                    )));
                }
                let found = self
                    .handle
                    .service()?
                    .search(&SourceSearch {
                        release: self.handle.release()?,
                        pattern: input.pattern.clone(),
                        path: input.path,
                    })
                    .map_err(ActionError::Failed)?;
                Ok(output(
                    format!(
                        "{} matches for {:?}{}",
                        found.matches.len(),
                        input.pattern,
                        if found.truncated { ", and more" } else { "" }
                    ),
                    serde_json::to_value(&found).unwrap_or_default(),
                ))
            }
            SOURCE_READ => {
                let input: ReadInput = glass_actions::parse_input(input)?;
                if !glass_managed::kbuild::valid_path(&input.path) || input.path.is_empty() {
                    return Err(ActionError::InvalidInput(format!(
                        "{:?} isn't a file under sys/",
                        input.path
                    )));
                }
                let text = self
                    .handle
                    .service()?
                    .read(&SourceRead {
                        release: self.handle.release()?,
                        path: input.path,
                        from: input.from.unwrap_or(1).max(1),
                        lines: input
                            .lines
                            .unwrap_or(glass_managed::kbuild::MAX_LINES)
                            .clamp(1, glass_managed::kbuild::MAX_LINES),
                    })
                    .map_err(ActionError::Failed)?;
                Ok(output(
                    format!(
                        "sys/{} from line {} of {}",
                        text.path, text.from, text.total_lines
                    ),
                    serde_json::to_value(&text).unwrap_or_default(),
                ))
            }
            PATCH_BUILD => {
                let input: BuildInput = glass_actions::parse_input(input)?;
                let request = BuildRequest {
                    release: self.handle.release()?,
                    id: input.id,
                    summary: input.summary,
                    diff: input.diff,
                    modules: input.modules,
                    kernel: input.kernel,
                    targets: input.targets,
                    origin: format!("agent:{}", ctx.principal.task),
                };
                request.check().map_err(ActionError::InvalidInput)?;
                let built = self
                    .handle
                    .service()?
                    .build(&request)
                    .map_err(ActionError::Failed)?;
                match built.bundle {
                    Some(bundle) if built.ok => {
                        let build = self.handle.keep(&bundle, &request)?;
                        Ok(output(
                            format!("Built {}: propose it with build {build}", request.id),
                            json!({ "ok": true, "log": built.log, "build": build }),
                        ))
                    }
                    _ => Ok(output(
                        format!(
                            "{} didn't build; the compiler's errors are in log",
                            request.id
                        ),
                        json!({ "ok": false, "log": built.log, "build": null }),
                    )),
                }
            }
            PATCH_PROPOSE => {
                let input: ProposeInput = glass_actions::parse_input(input)?;
                let (bytes, _) = self.handle.bundle(&input.build)?;
                if let Route::Review(link) = self.handle.route()? {
                    // Its source, never the binary built here (slice 3).
                    let computer = (self.handle.0.report)()
                        .map(|r| format!("{} {}", r.maker, r.model).trim().to_owned())
                        .unwrap_or_default();
                    let submission = PatchSubmission {
                        request: self.handle.request(&input.build)?,
                        person: ctx.principal.on_behalf_of.name.clone(),
                        asked: input.problem.chars().take(MAX_ASKED).collect(),
                        computer,
                    };
                    submission.check().map_err(ActionError::InvalidInput)?;
                    let sent = link.submit(&submission).map_err(ActionError::Failed)?;
                    return Ok(ActionOutput {
                        text: sent.text,
                        data: json!({
                            "id": submission.request.id,
                            "state": "sent_for_review",
                            "env": null,
                        }),
                        changes: Vec::new(),
                    });
                }
                let trial = self
                    .handle
                    .patches()?
                    .apply(&bytes)
                    .map_err(ActionError::Failed)?;
                Ok(ActionOutput {
                    text: format!(
                        "Kernel patch {} is set up: the next restart tries it, and it goes back \
                         by itself if the computer doesn't start",
                        trial.id
                    ),
                    data: json!({ "id": trial.id, "state": trial.state, "env": trial.env }),
                    changes: vec![Change {
                        kind: ChangeKind::Created,
                        target: format!("Kernel patch {}, to try at the next restart", trial.id),
                        reversible: false,
                        paths: Vec::new(),
                    }],
                })
            }
            other => Err(ActionError::InvalidInput(format!(
                "hardware has no action {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;

    use glass_api::TrialState;
    use glass_managed::hwpatch::{Bundle, GLASSHOUSE, Manifest, Module, Review};
    use glass_managed::kbuild::SourceMatch;
    use glass_managed::{DocKind, OrgKey, base64, sha256_hex};
    use glass_types::Timestamp;

    use super::*;

    /// The build service: it builds a diff that says `fixed`, and answers
    /// anything else with a compiler error.
    #[derive(Default)]
    struct Service {
        asked: Mutex<Vec<String>>,
    }

    impl BuildService for Service {
        fn search(&self, request: &SourceSearch) -> Result<SourceFound, String> {
            self.asked
                .lock()
                .unwrap()
                .push(format!("search {}", request.release));
            Ok(SourceFound {
                matches: vec![SourceMatch {
                    file: "sys/dev/glass/glass.c".into(),
                    line: 3,
                    text: "static int glass_suspend(device_t dev)".into(),
                }],
                truncated: false,
            })
        }
        fn read(&self, request: &SourceRead) -> Result<SourceText, String> {
            Ok(SourceText {
                path: request.path.clone(),
                from: request.from,
                text: "int x;\n".into(),
                total_lines: 1,
            })
        }
        fn build(&self, request: &BuildRequest) -> Result<BuildResult, String> {
            self.asked
                .lock()
                .unwrap()
                .push(format!("build {}", request.origin));
            if !request.diff.contains("fixed") {
                return Ok(BuildResult {
                    ok: false,
                    log: "glass.c:3:5: error: use of undeclared identifier 'fixd'".into(),
                    bundle: None,
                });
            }
            let module = b"\x7fELF glass.ko".to_vec();
            let manifest = Manifest {
                id: request.id.clone(),
                summary: request.summary.clone(),
                release: request.release.clone(),
                origin: request.origin.clone(),
                review: Review::Community,
                targets: vec![],
                modules: vec![Module {
                    file: "glass.ko".into(),
                    sha256: sha256_hex(&module),
                    load_at_boot: false,
                }],
                patch_sha256: sha256_hex(request.diff.as_bytes()),
                requirements: vec![],
                includes: vec![],
                resumes: 0,
            };
            Ok(BuildResult {
                ok: true,
                log: "built glass.ko".into(),
                bundle: Some(Bundle {
                    signed: OrgKey::from_seed([7; 32]).sign(
                        DocKind::HardwarePatch,
                        GLASSHOUSE,
                        1,
                        serde_json::to_string(&manifest).unwrap(),
                    ),
                    patch: request.diff.clone(),
                    modules: BTreeMap::from([("glass.ko".into(), base64::encode(&module))]),
                }),
            })
        }
    }

    #[derive(Default)]
    struct Kernel {
        applied: Mutex<Vec<Vec<u8>>>,
        /// It takes only patches IT reviewed.
        reviewed_only: Mutex<bool>,
        /// Why it would refuse what it's given, if it would.
        refuse: Mutex<Option<String>>,
    }

    impl Patches for Kernel {
        fn release(&self) -> Result<String, String> {
            Ok("15.1-RELEASE-p1".into())
        }
        fn takes_community(&self) -> bool {
            !*self.reviewed_only.lock().unwrap()
        }
        fn check(&self, _bundle: &[u8]) -> Result<(), String> {
            self.refuse.lock().unwrap().clone().map_or(Ok(()), Err)
        }
        fn fits(&self, _bundle: &[u8]) -> Result<(), String> {
            self.refuse.lock().unwrap().clone().map_or(Ok(()), Err)
        }
        fn apply(&self, bundle: &[u8]) -> Result<PatchTrial, String> {
            self.applied.lock().unwrap().push(bundle.to_vec());
            Ok(PatchTrial {
                id: "glass-resume".into(),
                revision: 1,
                summary: String::new(),
                review: "community".into(),
                env: "glass-hw-glass-resume-r1".into(),
                from_env: "default".into(),
                state: TrialState::Staged,
                modules: vec!["glass.ko".into()],
                staged_at: Timestamp::now(),
                ended_at: None,
                note: None,
                ran_minutes: 0,
                probation_minutes: 0,
                woke: 0,
                probation_resumes: 0,
            })
        }
    }

    fn setup(dir: &Path) -> (HardwareProvider, Arc<Service>, Arc<Kernel>) {
        let handle = HardwareHandle::with_report(
            dir.join("builds"),
            Arc::new(|| {
                glass_platform::hardware::Sources::read_dir(Path::new("/nonexistent"))
                    .map(|s| glass_platform::hardware::assess(&s))
                    .map_err(|e| e.to_string())
            }),
        );
        let service = Arc::new(Service::default());
        let kernel = Arc::new(Kernel::default());
        handle.set_service(service.clone());
        handle.set_patches(kernel.clone());
        (HardwareProvider::new(handle), service, kernel)
    }

    fn call(
        provider: &HardwareProvider,
        ctx: &TaskContext,
        name: &str,
        input: Value,
    ) -> Result<ActionOutput, ActionError> {
        let action: ActionName = name.parse().unwrap();
        provider.preview(ctx, &action, &input)?;
        let out = provider.invoke(ctx, &action, &input)?;
        let shape = provider.returns(&action).unwrap();
        glass_actions::shape::conforms(&out.data, &shape).unwrap_or_else(|e| panic!("{name}: {e}"));
        Ok(out)
    }

    fn build_input(diff: &str) -> Value {
        json!({
            "id": "glass-resume",
            "summary": "The glass device works again after sleep",
            "diff": format!("--- a/sys/dev/glass/glass.c\n+++ b/sys/dev/glass/glass.c\n@@ -3 +3 @@\n-old\n+{diff}\n"),
            "modules": [{ "dir": "glass", "file": "glass.ko" }]
        })
    }

    #[test]
    fn adr0057_an_agent_builds_a_patch_until_it_compiles_then_proposes_it() {
        let dir = tempfile::tempdir().unwrap();
        let (provider, service, kernel) = setup(dir.path());
        let ctx = glass_actions::testing::context(dir.path());

        let found = call(
            &provider,
            &ctx,
            SOURCE_SEARCH,
            json!({ "pattern": "suspend", "path": "dev/glass" }),
        )
        .unwrap();
        assert_eq!(found.data["matches"][0]["line"], 3);
        call(
            &provider,
            &ctx,
            SOURCE_READ,
            json!({ "path": "dev/glass/glass.c" }),
        )
        .unwrap();

        // The compiler's errors come back as a result the agent reads.
        let failed = call(&provider, &ctx, PATCH_BUILD, build_input("fixd")).unwrap();
        assert_eq!(failed.data["ok"], false);
        assert!(
            failed.data["log"]
                .as_str()
                .unwrap()
                .contains("undeclared identifier")
        );

        let built = call(&provider, &ctx, PATCH_BUILD, build_input("fixed")).unwrap();
        let build = built.data["build"].as_str().unwrap().to_owned();
        assert_eq!(build.len(), 16);
        let asked = service.asked.lock().unwrap().clone();
        assert!(
            asked.contains(&"search 15.1-RELEASE-p1".to_owned()),
            "the release it runs"
        );
        assert!(
            asked.contains(&format!("build agent:{}", ctx.principal.task)),
            "signed as the agent's: {asked:?}"
        );
        assert!(
            kernel.applied.lock().unwrap().is_empty(),
            "building changes nothing here"
        );

        // A patch this computer would refuse is refused before anyone is
        // asked (the dev VM's snd_hda.ko for a kernel with it built in).
        let action: ActionName = PATCH_PROPOSE.parse().unwrap();
        *kernel.refuse.lock().unwrap() = Some("the kernel has snd_hda built in".into());
        let refused = provider
            .preview(&ctx, &action, &json!({ "build": build }))
            .unwrap_err();
        assert!(refused.to_string().contains("built in"), "{refused}");
        *kernel.refuse.lock().unwrap() = None;

        // The review shows what it fixes, that nobody reviewed it, and the
        // diff itself.
        let preview = provider
            .preview(&ctx, &action, &json!({ "build": build }))
            .unwrap();
        assert!(
            preview
                .items
                .iter()
                .any(|i| i.contains("Nobody reviewed it"))
        );
        assert!(preview.texts.iter().any(|t| t.contains("+fixed")));
        let proposed = call(&provider, &ctx, PATCH_PROPOSE, json!({ "build": build })).unwrap();
        assert_eq!(proposed.data["state"], "staged");
        let applied = kernel.applied.lock().unwrap();
        let bytes = std::fs::read(
            dir.path()
                .join("builds")
                .join(format!("{build}.bundle.json")),
        )
        .unwrap();
        assert_eq!(applied[0], bytes, "what's set up is what was built");
    }

    // Audit 5.3: every build an agent made stayed on the disk, a kernel's
    // worth each.
    #[test]
    fn adr0057_only_the_newest_builds_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (provider, _, kernel) = setup(dir.path());
        let ctx = glass_actions::testing::context(dir.path());
        let builds = dir.path().join("builds");
        // What a build that never finished left: what it was made from,
        // and no bundle.
        std::fs::create_dir_all(&builds).unwrap();
        std::fs::write(builds.join("00000000deadbeef.request.json"), b"{}").unwrap();

        let mut made = Vec::new();
        for attempt in 0..BUILDS_KEPT + 2 {
            let built = call(
                &provider,
                &ctx,
                PATCH_BUILD,
                build_input(&format!("fixed, attempt {attempt}")),
            )
            .unwrap();
            made.push(built.data["build"].as_str().unwrap().to_owned());
            // Builds take minutes apart; these, long enough to tell which
            // came first.
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let mut kept: Vec<String> = std::fs::read_dir(&builds)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        kept.sort();
        let mut newest: Vec<String> = made[2..]
            .iter()
            .flat_map(|id| [format!("{id}.bundle.json"), format!("{id}.request.json")])
            .collect();
        newest.sort();
        assert_eq!(
            kept, newest,
            "the newest {BUILDS_KEPT}, each with its request"
        );

        // The newest is proposed as before; one that's gone says what to do.
        let action: ActionName = PATCH_PROPOSE.parse().unwrap();
        let gone = provider
            .preview(&ctx, &action, &json!({ "build": made[0] }))
            .unwrap_err();
        assert!(matches!(gone, ActionError::NotFound(_)), "{gone}");
        assert!(gone.to_string().contains("build the patch"), "{gone}");
        let proposed = call(
            &provider,
            &ctx,
            PATCH_PROPOSE,
            json!({ "build": made.last().unwrap() }),
        )
        .unwrap();
        assert_eq!(proposed.data["state"], "staged");
        assert_eq!(kernel.applied.lock().unwrap().len(), 1);

        // One more build, and the oldest of the four goes: never the one
        // just made. (A patch built again is a build of its own: a bundle
        // carries when it was signed.)
        std::thread::sleep(std::time::Duration::from_millis(15));
        let one_more = call(
            &provider,
            &ctx,
            PATCH_BUILD,
            build_input("fixed, attempt 2"),
        )
        .unwrap();
        let one_more = one_more.data["build"].as_str().unwrap().to_owned();
        assert!(builds.join(format!("{one_more}.bundle.json")).exists());
        if one_more != made[2] {
            assert!(!builds.join(format!("{}.bundle.json", made[2])).exists());
        }
        assert!(builds.join(format!("{}.bundle.json", made[3])).exists());
        let kept = std::fs::read_dir(&builds).unwrap().count();
        assert_eq!(kept, 2 * BUILDS_KEPT, "four builds, each with its request");
    }

    /// IT's console, as a managed computer reaches it.
    #[derive(Default)]
    struct Console(Mutex<Vec<PatchSubmission>>);

    impl PatchLink for Console {
        fn submit(
            &self,
            submission: &PatchSubmission,
        ) -> Result<glass_managed::hwreview::PatchSubmitted, String> {
            self.0.lock().unwrap().push(submission.clone());
            Ok(glass_managed::hwreview::PatchSubmitted {
                id: submission.request.id.clone(),
                version: "1".into(),
                text: "Sent to IT for review.".into(),
            })
        }
        fn bundle(
            &self,
            _: &glass_managed::hwreview::PatchFetch,
        ) -> Result<glass_managed::hwpatch::Bundle, String> {
            Err("not here".into())
        }
    }

    /// Slice 3: on a computer that takes only reviewed patches, an agent's
    /// patch goes to IT as source, with the person's yes; nothing changes
    /// here. With nobody to review it, it's refused before anyone is asked.
    #[test]
    fn adr0057_a_patch_this_computer_does_not_take_goes_to_it_for_review() {
        let dir = tempfile::tempdir().unwrap();
        let (provider, _, kernel) = setup(dir.path());
        *kernel.reviewed_only.lock().unwrap() = true;
        let mut ctx = glass_actions::testing::context(dir.path());
        ctx.principal.on_behalf_of.name = "dana".into();
        let built = call(&provider, &ctx, PATCH_BUILD, build_input("fixed")).unwrap();
        let build = built.data["build"].as_str().unwrap().to_owned();
        let action: ActionName = PATCH_PROPOSE.parse().unwrap();
        let input = json!({ "build": build, "problem": "The touchpad stops after sleep" });

        // Unmanaged, community patches off: no one to send it to.
        let refused = provider.preview(&ctx, &action, &input).unwrap_err();
        assert!(refused.to_string().contains("only reviewed"), "{refused}");

        // Managed: the review says where it goes, and it goes there.
        let console = Arc::new(Console::default());
        provider.handle.set_review(console.clone());
        // Never one that couldn't run here, a driver the kernel has built in.
        *kernel.refuse.lock().unwrap() = Some("the kernel has snd_hda built in".into());
        let refused = provider.preview(&ctx, &action, &input).unwrap_err();
        assert!(refused.to_string().contains("built in"), "{refused}");
        *kernel.refuse.lock().unwrap() = None;
        let preview = provider.preview(&ctx, &action, &input).unwrap();
        assert!(
            preview.summary.contains("to IT for review"),
            "{}",
            preview.summary
        );
        assert!(
            preview
                .items
                .iter()
                .any(|i| i.contains("nothing changes on it"))
        );
        let sent = call(&provider, &ctx, PATCH_PROPOSE, input).unwrap();
        assert_eq!(sent.data["state"], "sent_for_review");
        assert!(sent.changes.is_empty());
        assert!(
            kernel.applied.lock().unwrap().is_empty(),
            "nothing tried here"
        );
        let submitted = console.0.lock().unwrap();
        assert_eq!(submitted[0].person, "dana");
        assert_eq!(submitted[0].asked, "The touchpad stops after sleep");
        assert!(submitted[0].request.diff.contains("+fixed"), "its source");
        assert!(submitted[0].request.origin.starts_with("agent:"));
    }

    #[test]
    fn adr0057_what_an_agent_names_stays_in_the_kernel_s_source() {
        let dir = tempfile::tempdir().unwrap();
        let (provider, _, _) = setup(dir.path());
        let ctx = glass_actions::testing::context(dir.path());
        assert!(
            call(
                &provider,
                &ctx,
                SOURCE_READ,
                json!({ "path": "../../etc/master.passwd" })
            )
            .is_err()
        );
        assert!(
            call(
                &provider,
                &ctx,
                SOURCE_SEARCH,
                json!({ "pattern": "x", "path": "/etc" })
            )
            .is_err()
        );
        let outside = json!({
            "id": "glass-resume",
            "summary": "x",
            "diff": "--- a/etc/rc.conf\n+++ b/etc/rc.conf\n@@ -1 +1 @@\n-a\n+b\n",
            "modules": [{ "dir": "glass", "file": "glass.ko" }]
        });
        assert!(call(&provider, &ctx, PATCH_BUILD, outside).is_err());
        // A build id is ours, never a path.
        assert!(
            call(
                &provider,
                &ctx,
                PATCH_PROPOSE,
                json!({ "build": "../../../etc/x" })
            )
            .is_err()
        );
        assert!(matches!(
            call(
                &provider,
                &ctx,
                PATCH_PROPOSE,
                json!({ "build": "0123456789abcdef" })
            ),
            Err(ActionError::NotFound(_))
        ));
    }

    #[test]
    fn dk12_the_report_is_this_computer_s() {
        let dir = tempfile::tempdir().unwrap();
        let (provider, _, _) = setup(dir.path());
        let ctx = glass_actions::testing::context(dir.path());
        // The stand-in reads a capture that isn't there.
        assert!(call(&provider, &ctx, REPORT, json!({})).is_err());
        for def in provider.actions() {
            assert!(provider.returns(&def.name).is_some(), "{}", def.name);
        }
        let propose = provider
            .actions()
            .into_iter()
            .find(|d| d.name.as_str() == PATCH_PROPOSE)
            .unwrap();
        assert_eq!(propose.default_tier, Tier::Ask);
    }
}
