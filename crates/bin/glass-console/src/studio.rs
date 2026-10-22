//! Apps people share back to the store (ADR 0032, R2; GA-8, GA-13).
//!
//! The console takes nothing on trust. It runs the studio's checks again,
//! against the actions computers offer, and builds the bundle itself with
//! the same pinned bundler, confined by `prlimit` and `unshare`. It
//! compares the source with its own copy of the app it remixes, never the
//! computer's word for it. What IT approves is the console's bundle, and
//! the signed catalog names its SHA-256.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use glass_api::AppManifest;
use glass_app_studio::diff::{self, FileChange};
use glass_app_studio::store::read_tree;
use glass_app_studio::{Bundler, checks};
use glass_managed::studio::Submission;
use glass_platform::sandbox::ToolRunner;
use glass_types::{ActionName, AgentId, SideEffect};
use serde::{Deserialize, Serialize};

/// One of the console's checks on a shared app, as the reviewer reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub ok: bool,
    pub text: String,
}

impl Check {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            ok: true,
            text: text.into(),
        }
    }

    fn failed(text: impl Into<String>) -> Self {
        Self {
            ok: false,
            text: text.into(),
        }
    }
}

/// The app a shared app remixes, as the console has it.
#[derive(Debug, Clone)]
pub struct Base {
    pub manifest: AppManifest,
    pub files: BTreeMap<String, String>,
}

/// What the console made of a submission.
#[derive(Debug, Clone, Default)]
pub struct Checked {
    pub checks: Vec<Check>,
    /// The console's own bundle, built only when every check before it
    /// passed.
    pub bundle: Option<BTreeMap<String, String>>,
    /// File by file, from the base (from nothing for a new app).
    pub changes: Vec<FileChange>,
    /// The privilege delta: what it may do that its base may not; every
    /// action for a new app.
    pub beyond_base: Vec<ActionName>,
    /// What it declares that can't be undone: a person must review it
    /// (DV-4).
    pub irreversible: Vec<ActionName>,
}

impl Checked {
    pub fn passed(&self) -> bool {
        self.bundle.is_some() && self.checks.iter().all(|c| c.ok)
    }

    pub fn failures(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| c.text.clone())
            .collect()
    }
}

/// Checks and builds shared apps.
#[derive(Debug)]
pub struct Builder {
    bundler: Bundler,
    /// Our apps: `<id>/manifest.json` and `<id>/src/`, and the SDK in
    /// `sdk/`.
    apps: PathBuf,
    /// Every action computers offer, and what it does.
    actions: BTreeMap<ActionName, SideEffect>,
}

impl Builder {
    /// Builds with `tools` running `program`, the pinned bundler, in
    /// `work`: on our servers, confined with `unshare`.
    pub fn with_tools(
        tools: Arc<dyn ToolRunner>,
        apps: PathBuf,
        program: PathBuf,
        work: PathBuf,
        actions: BTreeMap<ActionName, SideEffect>,
    ) -> Self {
        Self {
            bundler: Bundler::new(tools, program, apps.join("sdk"), work),
            apps,
            actions,
        }
    }

    /// Our app `id`: its manifest and source.
    pub fn ours(&self, id: &AgentId) -> Option<Base> {
        let dir = self.apps.join(id.as_str());
        let manifest: AppManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()?;
        let files = read_tree(&dir, "src").ok()?;
        (manifest.id == *id).then_some(Base { manifest, files })
    }

    /// Our apps' manifests: their ids and names are not for shared apps.
    pub fn our_apps(&self) -> Vec<AppManifest> {
        std::fs::read_dir(&self.apps)
            .into_iter()
            .flatten()
            .filter_map(|e| {
                let text = std::fs::read(e.ok()?.path().join("manifest.json")).ok()?;
                serde_json::from_slice::<AppManifest>(&text).ok()
            })
            .collect()
    }

    /// Checks `submission` as the app `manifest` (its id and version in the
    /// org's catalog), from `base` when it remixes one the console has,
    /// and builds it when every check passes.
    pub fn check(
        &self,
        submission: &Submission,
        manifest: &AppManifest,
        base: Option<&Base>,
    ) -> Checked {
        let mut out = Checked::default();
        match manifest.check() {
            Ok(()) => out.checks.push(Check::ok("Its manifest is complete.")),
            Err(why) => out
                .checks
                .push(Check::failed(format!("Its manifest: {why}."))),
        }
        let unknown: Vec<&ActionName> = manifest
            .actions
            .iter()
            .filter(|a| !self.actions.contains_key(*a))
            .collect();
        if unknown.is_empty() {
            out.checks.push(Check::ok(format!(
                "It calls only actions computers offer ({}).",
                manifest.actions.len()
            )));
        }
        for action in unknown {
            out.checks.push(Check::failed(format!(
                "{action} isn't an action computers offer."
            )));
        }
        let source = checks::check_source(manifest, &submission.files);
        if source.refusals.is_empty() {
            out.checks.push(Check::ok(
                "Its source passed the studio's checks: no code from text, no network or \
                 storage of its own, only the actions it declares.",
            ));
        }
        out.checks.extend(
            source
                .refusals
                .iter()
                .map(|r| Check::failed(format!("{r}."))),
        );
        out.checks.extend(
            source
                .notes
                .iter()
                .map(|n| Check::ok(format!("Note: {n}."))),
        );

        let empty = BTreeMap::new();
        let (base_files, base_actions) = match (&submission.based_on, base) {
            (Some(said), Some(base)) => {
                out.checks.push(if said.version == base.manifest.version {
                    Check::ok(format!(
                        "It remixes {} {}; the change is from the console's own copy.",
                        base.manifest.name, base.manifest.version
                    ))
                } else {
                    Check::ok(format!(
                        "It remixes {} {}; the change is from {}, the version the console has.",
                        base.manifest.name, said.version, base.manifest.version
                    ))
                });
                (&base.files, base.manifest.actions.as_slice())
            }
            (Some(said), None) => {
                out.checks.push(Check::ok(format!(
                    "It says it remixes {}, which the console has no source for, so it is \
                     shown as a new app.",
                    said.id
                )));
                (&empty, &[][..])
            }
            (None, _) => (&empty, &[][..]),
        };
        out.changes = diff::changes(base_files, &submission.files);
        out.beyond_base = manifest
            .actions
            .iter()
            .filter(|a| !base_actions.contains(a))
            .cloned()
            .collect();
        out.irreversible = manifest
            .actions
            .iter()
            .filter(|a| self.actions.get(*a) == Some(&SideEffect::Irreversible))
            .cloned()
            .collect();

        if out.checks.iter().all(|c| c.ok) {
            match self.bundler.bundle(manifest, &submission.files) {
                Ok(dist) => {
                    let bytes: usize = dist.values().map(String::len).sum();
                    out.checks.push(Check::ok(format!(
                        "The console built it itself, with no network: {} files, {} KB.",
                        dist.len(),
                        bytes.div_ceil(1024)
                    )));
                    out.bundle = Some(dist);
                }
                Err(found) => out.checks.extend(
                    found
                        .refusals
                        .iter()
                        .map(|r| Check::failed(format!("Building it: {r}."))),
                ),
            }
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;

    use glass_app_studio::testing::FakeBundler;
    use glass_managed::studio::{ShareTo, SharedBase};

    use super::*;

    pub(crate) fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    fn actions() -> BTreeMap<ActionName, SideEffect> {
        crate::impact::Titles::first_party()
            .lint_info
            .into_iter()
            .map(|(a, effect, _)| (a, effect))
            .collect()
    }

    fn builder(dir: &Path) -> Builder {
        Builder::with_tools(
            Arc::new(FakeBundler::default()),
            repo().join("apps"),
            PathBuf::from("/nonexistent/esbuild"),
            dir.join("builds"),
            actions(),
        )
    }

    /// Mail with "Write" for "New message", as `app` at org version 1.
    pub(crate) fn remix(builder: &Builder, extra: &[&str]) -> (Submission, AppManifest) {
        let mail = builder.ours(&"mail".parse().unwrap()).unwrap();
        let mut files = mail.files.clone();
        let main = files.get_mut(checks::MAIN).unwrap();
        *main = main.replace("\"New message\",", "\"Write\",");
        let mut manifest = mail.manifest.clone();
        manifest.id = "my-mail".parse().unwrap();
        manifest.name = "Mail, with Write".into();
        manifest.version = "2".into();
        for a in extra {
            manifest.actions.push(a.parse().unwrap());
        }
        let submission = Submission {
            app: "my-mail".parse().unwrap(),
            name: manifest.name.clone(),
            named: true,
            version: 2,
            maker: "dana".into(),
            device: "dev-1".into(),
            manifest: manifest.clone(),
            files,
            based_on: Some(SharedBase {
                id: "mail".parse().unwrap(),
                version: mail.manifest.version.clone(),
            }),
            note: "Write instead of New message".into(),
            share: ShareTo::Team {
                group: "ap-ops".into(),
            },
            notes: Vec::new(),
        };
        let mut org = manifest;
        org.id = "mail-with-write".parse().unwrap();
        org.version = "1".into();
        (submission, org)
    }

    #[test]
    fn ga8_the_reviewer_sees_the_change_from_our_source_and_what_it_adds() {
        let dir = tempfile::tempdir().unwrap();
        let builder = builder(dir.path());
        let (submission, org) = remix(&builder, &["chat.post"]);
        let base = builder.ours(&"mail".parse().unwrap());
        let checked = builder.check(&submission, &org, base.as_ref());
        assert!(checked.passed(), "{:?}", checked.checks);
        assert_eq!(checked.changes.len(), 1);
        assert_eq!(checked.changes[0].path, "src/main.ts");
        assert!(checked.changes[0].unified.contains("+"));
        assert!(checked.changes[0].unified.contains("Write"));
        // The privilege delta: what Mail itself can't do.
        assert_eq!(
            checked.beyond_base,
            ["chat.post".parse::<ActionName>().unwrap()]
        );
        // DV-4: sending mail and posting can't be undone.
        assert!(checked.irreversible.contains(&"chat.post".parse().unwrap()));
        assert!(checked.irreversible.contains(&"mail.send".parse().unwrap()));
        let bundle = checked.bundle.unwrap();
        assert!(bundle["index.html"].contains("<title>Mail, with Write</title>"));
    }

    #[test]
    fn ga13_the_console_refuses_what_the_computer_should_have() {
        let dir = tempfile::tempdir().unwrap();
        let builder = builder(dir.path());
        let (mut submission, mut org) = remix(&builder, &["files.wipe"]);
        let main = submission.files.get_mut(checks::MAIN).unwrap();
        main.push_str("\nfetch('https://example.com/');\n");
        org.actions.retain(|a| a.as_str() != "mail.send");
        let base = builder.ours(&"mail".parse().unwrap());
        let checked = builder.check(&submission, &org, base.as_ref());
        assert!(!checked.passed());
        let failures = checked.failures().join("\n");
        assert!(
            failures.contains("files.wipe isn't an action"),
            "{failures}"
        );
        assert!(failures.contains("mail.send"), "undeclared: {failures}");
        assert!(failures.contains("its own connection"), "{failures}");
        assert!(checked.bundle.is_none(), "nothing failing is built");
    }

    #[test]
    fn a_remix_of_an_app_the_console_lacks_reads_as_new() {
        let dir = tempfile::tempdir().unwrap();
        let builder = builder(dir.path());
        let (mut submission, org) = remix(&builder, &[]);
        submission.based_on = Some(SharedBase {
            id: "ledger".parse().unwrap(),
            version: "3".into(),
        });
        let checked = builder.check(&submission, &org, None);
        assert!(checked.passed(), "{:?}", checked.checks);
        assert_eq!(checked.beyond_base, org.actions);
        assert!(
            checked
                .changes
                .iter()
                .all(|c| c.status == diff::Status::Added)
        );
    }

    /// The real bundler, confined: skipped where it isn't installed.
    #[test]
    fn ga13_the_console_builds_it_itself_confined() {
        let esbuild = repo().join("apps/node_modules/@esbuild/linux-x64/bin/esbuild");
        if !esbuild.exists() {
            eprintln!("skipped: no bundler in apps/node_modules");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let builder = Builder::with_tools(
            Arc::new(glass_platform::sandbox::UnshareTools),
            repo().join("apps"),
            esbuild,
            dir.path().join("builds"),
            actions(),
        );
        let (submission, org) = remix(&builder, &[]);
        let base = builder.ours(&"mail".parse().unwrap());
        let checked = builder.check(&submission, &org, base.as_ref());
        assert!(checked.passed(), "{:?}", checked.checks);
        assert!(checked.bundle.unwrap()["app.js"].contains("Write"));
    }
}
