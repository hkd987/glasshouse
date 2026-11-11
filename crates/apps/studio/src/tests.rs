use std::path::PathBuf;

use super::testing::FakeBundler;
use super::*;

/// The repository's apps: Mail's source and manifest, the SDK.
fn apps() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps")
}

/// Our apps' source as images lay it out: `apps/<app>` and `sdk`, here
/// links into the repository's.
pub(crate) fn source_root(dir: &Path) -> PathBuf {
    let root = dir.join("source");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(apps(), root.join("apps")).unwrap();
    std::os::unix::fs::symlink(apps().join("sdk"), root.join("sdk")).unwrap();
    root
}

/// What policy and the catalog say, as the tests set them.
#[derive(Debug, Default)]
struct World {
    /// IT turned remixing off (TL-6).
    off: bool,
    /// Actions an app the person makes may never have.
    never: Vec<&'static str>,
    /// What the catalog offers, for any need.
    offers: Vec<(&'static str, &'static str)>,
}

impl Surroundings for World {
    fn remixing(&self) -> bool {
        !self.off
    }
    fn profile(&self, _login: &str) -> BTreeSet<AgentId> {
        ["mail", "calendar", "settings"]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect()
    }
    fn refusals(&self, _login: &str, manifest: &AppManifest) -> Vec<String> {
        manifest
            .actions
            .iter()
            .filter(|a| self.never.contains(&a.as_str()))
            .map(|a| format!("your organization never allows {a} in apps you make"))
            .collect()
    }
    fn offers(&self, _groups: &[String], _need: &str) -> Vec<(AgentId, String)> {
        self.offers
            .iter()
            .map(|(id, name)| (id.parse().unwrap(), (*name).to_owned()))
            .collect()
    }
    fn capabilities(&self, _login: &str) -> Vec<Capability> {
        vec![
            Capability {
                name: "calendar.read".parse().unwrap(),
                title: "Read an event".into(),
                description: "One event, with its attendees.".into(),
                side_effect: SideEffect::None,
                tier: Tier::Auto,
                input: json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
                returns: Some(json!({
                    "type": "object",
                    "properties": { "event": {
                        "type": "object",
                        "properties": { "title": { "type": "string" }, "attendees": { "type": "array", "items": { "type": "string" }, "description": "email addresses" } },
                        "required": ["title", "attendees"]
                    } },
                    "required": ["event"]
                })),
            },
            Capability {
                name: "mail.draft".parse().unwrap(),
                title: "Draft an email".into(),
                description: "Saves an email in Drafts.".into(),
                side_effect: SideEffect::LocalReversible,
                tier: Tier::Notify,
                input: json!({ "type": "object", "properties": { "to": { "type": "array", "items": { "type": "string" } }, "subject": { "type": "string" } }, "required": ["to"] }),
                returns: None,
            },
        ]
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    provider: StudioProvider,
    studio: Studio,
    ctx: TaskContext,
}

fn fixture_with(world: World, bundler: Arc<dyn ToolRunner>, program: PathBuf) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let studio = Studio::new(StudioPaths {
        store: dir.path().join("state/apps"),
        installed: apps(),
        source: source_root(dir.path()),
        bundler: program,
        tryout: Some(tryout(dir.path())),
    });
    studio.set_tools(bundler);
    studio.set_surroundings(Arc::new(world));
    let mut ctx = glass_actions::testing::context(&home);
    ctx.principal.agent = "claude-code".parse().unwrap();
    let provider = StudioProvider::new(studio.clone());
    provider.prepare(&ctx).unwrap();
    Fixture {
        _dir: dir,
        provider,
        studio,
        ctx,
    }
}

/// Where glass-app-try is, for the stand-in bundler to answer as it.
fn tryout(dir: &Path) -> PathBuf {
    let program = dir.join("app-try/glass-app-try");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(&program, "").unwrap();
    program
}

fn fixture(world: World) -> Fixture {
    fixture_with(
        world,
        Arc::new(FakeBundler::default()),
        "/nonexistent".into(),
    )
}

impl Fixture {
    fn call(&self, action: &str, input: Value) -> Result<ActionOutput, ActionError> {
        let action: ActionName = action.parse().unwrap();
        self.provider.preview(&self.ctx, &action, &input)?;
        self.provider.invoke(&self.ctx, &action, &input)
    }
}

/// Mail's source with an unread count in its title.
fn mail_remix(f: &Fixture) -> Value {
    let source = f.call(SOURCE, json!({ "app": "mail" })).unwrap();
    let mut files = source.data["files"].clone();
    let main = files["src/main.ts"].as_str().unwrap().to_owned();
    files["src/main.ts"] = json!(format!(
        "{main}\n// Unread count in the title.\ndocument.title = `Mail`;\n"
    ));
    json!({
        "base": "mail",
        "manifest": source.data["manifest"],
        "files": files,
        "note": "Shows how many are unread in the title",
    })
}

#[test]
fn tl5_mail_remixed_is_the_persons_own_app_based_on_mail() {
    let f = fixture(World::default());
    let bases = f.call(BASES, json!({ "need": "read my mail" })).unwrap();
    assert_eq!(bases.data["bases"][0]["id"], "mail", "{}", bases.data);
    assert!(bases.text.contains("Mail can be remixed"), "{}", bases.text);

    let saved = f.call(SAVE, mail_remix(&f)).unwrap();
    assert_eq!(saved.data["app"], "my-mail");
    assert_eq!(saved.data["version"], 1);
    assert_eq!(saved.data["based_on"]["id"], "mail");
    assert!(
        saved.text.contains("your version of Mail"),
        "{}",
        saved.text
    );
    assert_eq!(saved.changes[0].paths, [f.ctx.home.join(store::MINE)]);

    // GA-5: it's the person's, at the version they use, and serves what
    // was built, with a page made here.
    let own = f.studio.own_apps("demo", &f.ctx.home);
    assert_eq!(own.len(), 1);
    assert_eq!(own[0].manifest.id.as_str(), "my-mail");
    assert_eq!(own[0].record.saved_by.as_str(), "claude-code");
    let (version, files) = f
        .studio
        .served("demo", &f.ctx.home, &own[0].manifest.id)
        .unwrap();
    assert_eq!(version, 1);
    assert!(files["index.html"].contains("<title>My Mail</title>"));
    assert_eq!(files["app.js"], "console.log('built')");
    assert!(
        f.studio
            .served("someone-else", &f.ctx.home, &own[0].manifest.id)
            .is_none()
    );
}

#[test]
fn ga3_a_remix_undone_and_made_again_goes_on_where_it_was() {
    let f = fixture(World::default());
    f.call(SAVE, mail_remix(&f)).unwrap();
    // Undo puts their app list back as it was before the save.
    store::choose(&f.ctx.home, f.ctx.owner, Default::default()).unwrap();
    assert!(f.studio.own_apps("demo", &f.ctx.home).is_empty());

    let again = f.call(SAVE, mail_remix(&f)).unwrap();
    assert_eq!(again.data["app"], "my-mail", "not my-mail-2");
    assert_eq!(again.data["version"], 2, "after the version undone");
    // A second remix beside the one they use is an app of its own, with a
    // name of its own in the launcher.
    let beside = f.call(SAVE, mail_remix(&f)).unwrap();
    assert_eq!(
        (&beside.data["app"], &beside.data["name"]),
        (&json!("my-mail-2"), &json!("My Mail 2"))
    );
}

#[test]
fn ga3_every_save_is_a_version_and_going_back_is_choosing_one() {
    let f = fixture(World::default());
    f.call(SAVE, mail_remix(&f)).unwrap();
    let mut next = mail_remix(&f);
    next.as_object_mut().unwrap().remove("base");
    next["app"] = json!("my-mail");
    next["note"] = json!("Bigger subject lines");
    let saved = f.call(SAVE, next).unwrap();
    assert_eq!(saved.data["version"], 2);

    let versions = f.call(VERSIONS, json!({ "app": "my-mail" })).unwrap();
    assert_eq!(versions.data["versions"][0]["version"], 2);
    assert_eq!(versions.data["versions"][0]["current"], true);
    assert_eq!(
        versions.data["versions"][1]["note"],
        "Shows how many are unread in the title"
    );

    f.call(REVERT, json!({ "app": "my-mail", "version": 1 }))
        .unwrap();
    assert_eq!(
        f.studio
            .served("demo", &f.ctx.home, &"my-mail".parse().unwrap())
            .unwrap()
            .0,
        1
    );
    // Versions never change: 2 is still there to go back to.
    assert_eq!(
        f.studio
            .store()
            .versions("demo", &"my-mail".parse().unwrap()),
        [1, 2]
    );
    assert!(
        f.call(REVERT, json!({ "app": "my-mail", "version": 7 }))
            .is_err()
    );
    // Audit 6.5: nobody is asked to approve going back to a version that
    // isn't there; the preview refuses as the action does.
    let asked = |version: u32| {
        f.provider.preview(
            &f.ctx,
            &REVERT.parse().unwrap(),
            &json!({ "app": "my-mail", "version": version }),
        )
    };
    assert_eq!(
        asked(7).unwrap_err().to_string(),
        f.call(REVERT, json!({ "app": "my-mail", "version": 7 }))
            .unwrap_err()
            .to_string()
    );
    assert!(asked(2).unwrap().summary.starts_with("Use version 2 of "));
}

/// Mail's compose button, as its source has it.
const COMPOSE: &str = "\"data-test\": \"compose\" },\n      \"New message\"";

#[test]
fn tl5_an_agent_reads_one_file_and_saves_an_edit_to_it() {
    // An agent whose results are cut at a length reads Mail a file at a
    // time and changes one string, without sending the app back whole.
    let f = fixture(World::default());
    let listed = f.call(SOURCE, json!({ "app": "mail" })).unwrap();
    assert!(
        listed.text.contains("src/main.ts (") && listed.text.contains(" lines)"),
        "{}",
        listed.text
    );
    let main = std::fs::read_to_string(apps().join("mail/src/main.ts")).unwrap();
    let read = f
        .call(SOURCE, json!({ "app": "mail", "path": "src/main.ts" }))
        .unwrap();
    assert_eq!(read.text, main, "the file, exactly");
    assert_eq!(read.data["path"], "src/main.ts");

    let edit = |old: &str, new: &str| {
        json!({
            "base": "mail",
            "edits": [{ "path": "src/main.ts", "old": old, "new": new }],
            "note": "Write, not New message",
        })
    };
    let saved = f
        .call(
            SAVE,
            edit(COMPOSE, &COMPOSE.replace("New message", "Write")),
        )
        .unwrap();
    assert_eq!(saved.data["app"], "my-mail", "{}", saved.text);
    let app: AgentId = "my-mail".parse().unwrap();
    let store = f.studio.store();
    let source = store.source("demo", &app, 1).unwrap();
    assert!(source["src/main.ts"].contains(&COMPOSE.replace("New message", "Write")));
    assert_eq!(
        source.len(),
        listed.data["files"].as_object().unwrap().len()
    );
    // What it declares is Mail's, as nothing said otherwise.
    let declares = &listed.data["manifest"];
    let manifest = store.manifest("demo", &app, 1).unwrap();
    assert_eq!(json!(manifest.actions), declares["actions"]);
    assert_eq!(manifest.name, "My Mail");

    // The next version starts from the last: a second edit, a new file.
    let next = f
        .call(
            SAVE,
            json!({
                "app": "my-mail",
                "edits": [
                    { "path": "src/main.ts", "old": "\"Write\"", "new": "\"Write one\"" },
                    { "path": "src/extra.css", "new": ".x { color: red; }" },
                ],
                "note": "Write one",
            }),
        )
        .unwrap();
    assert_eq!(next.data["version"], 2);
    let source = store.source("demo", &app, 2).unwrap();
    assert!(source["src/main.ts"].contains("\"Write one\""));
    assert_eq!(source["src/extra.css"], ".x { color: red; }");
}

#[test]
fn tl5_an_edit_that_doesnt_fit_says_why_and_saves_nothing() {
    let f = fixture(World::default());
    let save = |edits: Value| {
        f.call(SAVE, json!({ "base": "mail", "edits": edits, "note": "x" }))
            .unwrap_err()
            .to_string()
    };
    let err = save(json!([{ "path": "src/main.ts", "old": "Not in Mail at all", "new": "" }]));
    assert!(err.contains("old isn't in the file"), "{err}");
    let err = save(json!([{ "path": "src/main.ts", "old": "const", "new": "let" }]));
    assert!(err.contains("times; include more"), "{err}");
    let err = save(json!([{ "path": "src/nope.ts", "old": "x", "new": "y" }]));
    assert!(
        err.contains("edit 1 (src/nope.ts): there's no such file"),
        "{err}"
    );
    let err = save(json!([{ "path": "src/main.ts", "new": "y" }]));
    assert!(err.contains("the file is there"), "{err}");
    // A second edit failing keeps the first from saving too.
    let err = save(json!([
        { "path": "src/main.ts", "old": COMPOSE, "new": "Write" },
        { "path": "src/main.ts", "old": "Not there", "new": "" },
    ]));
    assert!(err.contains("edit 2 (src/main.ts): old isn't"), "{err}");
    // An edit's new file is checked like any other.
    let err = save(json!([{ "path": "notes.txt", "new": "hi" }]));
    assert!(
        err.contains("notes.txt: an app's source lives under src/"),
        "{err}"
    );
    assert!(f.studio.own_apps("demo", &f.ctx.home).is_empty());
    let err = f
        .call(SOURCE, json!({ "app": "mail", "path": "src/nope.ts" }))
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no src/nope.ts; it has"), "{err}");
}

#[test]
fn ga5_a_remix_can_do_only_what_its_maker_can() {
    let f = fixture(World {
        never: vec!["mail.send"],
        ..World::default()
    });
    let err = f.call(SAVE, mail_remix(&f)).unwrap_err().to_string();
    assert!(err.contains("never allows mail.send"), "{err}");
    assert!(f.studio.own_apps("demo", &f.ctx.home).is_empty());
}

#[test]
fn ga4_undeclared_calls_markup_and_outside_code_are_refused() {
    let f = fixture(World::default());
    let mut remix = mail_remix(&f);
    remix["manifest"]["actions"] = json!(["mail.list", "mail.read"]);
    let err = f.call(SAVE, remix).unwrap_err().to_string();
    assert!(
        err.contains("calls mail.send, which the manifest doesn't declare"),
        "{err}"
    );

    let mut remix = mail_remix(&f);
    remix["files"]["src/extra.ts"] =
        json!("export const x = (n: Element) => { n.innerHTML = '<b>'; };");
    let err = f.call(SAVE, remix).unwrap_err().to_string();
    assert!(err.contains("src/extra.ts:1: builds markup"), "{err}");

    // Something the bundler read that isn't the app's or the SDK's.
    let f = fixture_with(
        World::default(),
        Arc::new(FakeBundler {
            extra: Some("../etc/passwd".into()),
            ..FakeBundler::default()
        }),
        "/nonexistent".into(),
    );
    let err = f.call(SAVE, mail_remix(&f)).unwrap_err().to_string();
    assert!(err.contains("uses ../etc/passwd"), "{err}");
    assert!(
        f.studio
            .store()
            .versions("demo", &"my-mail".parse().unwrap())
            .is_empty()
    );
}

/// glassd (root) reads what the bundler (nobody) left confined to the
/// build folder: a link it leaves to someone's file serves nothing.
#[test]
fn ga5_a_link_the_bundler_leaves_is_never_followed() {
    let secret = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(secret.path(), "alice's private notes").unwrap();
    let f = fixture_with(
        World::default(),
        Arc::new(FakeBundler {
            link: Some(secret.path().to_owned()),
            ..FakeBundler::default()
        }),
        "/nonexistent".into(),
    );
    f.call(SAVE, mail_remix(&f)).unwrap();
    let (_, files) = f
        .studio
        .served("demo", &f.ctx.home, &"my-mail".parse().unwrap())
        .unwrap();
    assert!(!files.contains_key("app.css"), "{files:?}");
    assert!(!files.values().any(|t| t.contains("private notes")));
}

#[test]
fn ga5_only_the_persons_own_apps_can_be_remixed() {
    let f = fixture(World::default());
    let mut remix = mail_remix(&f);
    remix["base"] = json!("chat");
    let err = f.call(SAVE, remix).unwrap_err().to_string();
    assert!(err.contains("isn't one of your apps"), "{err}");
    let err = f
        .call(SOURCE, json!({ "app": "chat" }))
        .unwrap_err()
        .to_string();
    assert!(err.contains("isn't an app of yours"), "{err}");
    // Someone else's app is nobody's to read here.
    let err = f
        .call(SOURCE, json!({ "app": "my-mail" }))
        .unwrap_err()
        .to_string();
    assert!(err.contains("isn't an app of yours"), "{err}");
}

fn new_app(f: &Fixture, declined: &[&str]) -> Value {
    let template = f.call(SOURCE, json!({ "template": true })).unwrap();
    let mut manifest = template.data["manifest"].clone();
    manifest["name"] = json!("Vendor disputes");
    json!({
        "need": "track vendor disputes",
        "declined": declined,
        "manifest": manifest,
        "files": template.data["files"],
        "note": "First version",
    })
}

#[test]
fn ga12_the_store_first_then_a_remix_then_a_new_app() {
    let f = fixture(World {
        offers: vec![("dispute-tracker", "Dispute tracker")],
        ..World::default()
    });
    let err = f.call(CREATE, new_app(&f, &[])).unwrap_err().to_string();
    assert!(
        err.contains("Dispute tracker (dispute-tracker) in your organization's catalog"),
        "{err}"
    );
    let made = f.call(CREATE, new_app(&f, &["dispute-tracker"])).unwrap();
    assert_eq!(made.data["app"], "my-vendor-disputes");
    let record = f
        .studio
        .store()
        .record("demo", &"my-vendor-disputes".parse().unwrap(), 1)
        .unwrap();
    assert_eq!(record.need.as_deref(), Some("track vendor disputes"));
    assert_eq!(record.declined[0].as_str(), "dispute-tracker");
    assert!(record.based_on.is_none());

    // A remixable app that meets the need comes before a new one.
    let mut mailish = new_app(&f, &[]);
    mailish["need"] = json!("read and send mail");
    mailish["manifest"]["name"] = json!("Mail helper");
    let f = fixture(World::default());
    let err = f.call(CREATE, mailish.clone()).unwrap_err().to_string();
    assert!(err.contains("Mail (mail), which you can remix"), "{err}");
    mailish["declined"] = json!(["mail"]);
    assert!(f.call(CREATE, mailish).is_ok());

    // A word in common is chance: Settings "shows", Chat keeps "drafts".
    let mut today = new_app(&f, &[]);
    today["need"] = json!("Show today's meetings and draft a running-late note to the attendees");
    today["manifest"]["name"] = json!("Today");
    let made = f.call(CREATE, today).unwrap();
    assert_eq!(made.data["app"], "my-today");
}

#[test]
fn tl6_with_remixing_off_nothing_is_remixed_or_made() {
    let f = fixture(World {
        off: true,
        ..World::default()
    });
    for (action, input) in [
        (BASES, json!({})),
        (SOURCE, json!({ "app": "mail" })),
        (VERSIONS, json!({})),
    ] {
        let err = f.call(action, input).unwrap_err().to_string();
        assert!(
            err.contains("IT turned remixing apps off"),
            "{action}: {err}"
        );
    }
}

#[test]
fn ids_are_personal_and_distinct() {
    let taken: Vec<AgentId> = vec!["my-mail".parse().unwrap()];
    assert_eq!(id_for("Mail", &taken).unwrap().as_str(), "my-mail-2");
    assert_eq!(id_for("  Ünïcode!! ", &[]).unwrap().as_str(), "my-n-code");
    assert_eq!(id_for("???", &[]).unwrap().as_str(), "my-app");
    assert!(id_for("Mail", &[]).unwrap().is_personal());
    assert_eq!(id_for("My Mail", &[]).unwrap().as_str(), "my-mail");
}

#[test]
fn the_inventory_lists_every_app_made_here() {
    let f = fixture(World::default());
    f.call(SAVE, mail_remix(&f)).unwrap();
    let inventory = f.studio.inventory();
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].0.owner, "demo");
    assert_eq!(inventory[0].1.id.as_str(), "my-mail");
}

/// The real bundler, when the apps workspace has it (`npm ci` in apps/):
/// Mail's own source, remixed, builds. Without it this proves nothing;
/// `apps/e2e/studio.spec.ts` and the VM scenario `studio` always bundle.
#[test]
fn ga4_mail_remixed_builds_with_esbuild() {
    let esbuild = apps().join("node_modules/@esbuild/linux-x64/bin/esbuild");
    if !esbuild.exists() {
        eprintln!(
            "skipped: no esbuild in apps/node_modules (npm ci in apps/); the e2e suites bundle"
        );
        return;
    }
    let f = fixture_with(
        World::default(),
        Arc::new(glass_platform::sandbox::HostSandbox),
        esbuild,
    );
    let saved = f.call(SAVE, mail_remix(&f)).unwrap();
    let (_, files) = f
        .studio
        .served(
            "demo",
            &f.ctx.home,
            &saved.data["app"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
    assert!(files["app.js"].len() > 10_000, "{}", files["app.js"].len());
    assert!(files.contains_key("app.css") && files.contains_key("sw.js"));

    // An import from outside the app and the SDK is refused before the
    // bundler runs (it wouldn't resolve there either).
    let mut remix = mail_remix(&f);
    remix["files"]["src/main.ts"] = json!("import pad from \"left-pad\";\nconsole.log(pad);\n");
    remix["manifest"]["actions"] = json!(["mail.list"]);
    let err = f.call(SAVE, remix).unwrap_err().to_string();
    assert!(err.contains("imports left-pad"), "{err}");
}

/// The console, as far as sharing goes: what was submitted.
#[derive(Debug, Default)]
struct FakeConsole {
    got: std::sync::Mutex<Vec<glass_managed::studio::Submission>>,
}

impl glass_managed::studio::ShareLink for FakeConsole {
    fn submit(
        &self,
        submission: &glass_managed::studio::Submission,
    ) -> Result<glass_managed::studio::Submitted, String> {
        self.got.lock().unwrap().push(submission.clone());
        Ok(glass_managed::studio::Submitted {
            id: "mail-with-write".parse().unwrap(),
            text: "Sent for review.".into(),
        })
    }

    fn bundle(
        &self,
        _request: &glass_managed::studio::BundleRequest,
    ) -> Result<glass_managed::studio::Bundle, String> {
        Err("not here".into())
    }
}

#[test]
fn ga8_ga13_a_person_shares_their_app_through_the_store_only() {
    let f = fixture(World::default());
    f.call(SAVE, mail_remix(&f)).unwrap();
    let share = json!({ "app": "my-mail", "to": "org", "note": "Write instead of New message" });
    // No org manages this computer yet: nowhere to share to.
    let err = f.call(SHARE, share.clone()).unwrap_err().to_string();
    assert!(err.contains("isn't managed by an organization"), "{err}");

    let console = Arc::new(FakeConsole::default());
    f.studio.set_share(console.clone(), "dev-1".into());
    let preview = f
        .provider
        .preview(&f.ctx, &SHARE.parse().unwrap(), &share)
        .unwrap();
    assert!(preview.items[0].contains("Sends its source to your organization's store"));
    let out = f.call(SHARE, share).unwrap();
    assert_eq!(out.data["id"], "mail-with-write");
    let got = console.got.lock().unwrap()[0].clone();
    assert_eq!((got.maker.as_str(), got.device.as_str()), ("demo", "dev-1"));
    assert_eq!(got.based_on.unwrap().id.as_str(), "mail");
    assert!(got.files["src/main.ts"].contains("Unread count in the title"));
    // Not named: a new version would keep its name in the store.
    assert_eq!((got.name.as_str(), got.named), ("My Mail", false));
    f.call(
        SHARE,
        json!({ "app": "my-mail", "to": "org", "name": "Mail, with Write", "note": "x" }),
    )
    .unwrap();
    let got = console.got.lock().unwrap()[1].clone();
    assert_eq!((got.name.as_str(), got.named), ("Mail, with Write", true));

    // A team is one of the person's own groups.
    let err = f
        .call(
            SHARE,
            json!({ "app": "my-mail", "to": "team", "team": "finance", "note": "x" }),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("finance isn't one of your groups"), "{err}");
    assert!(
        f.call(
            SHARE,
            json!({ "app": "my-other", "to": "org", "note": "x" })
        )
        .is_err()
    );
}

#[test]
fn ga1_an_agent_making_an_app_sees_what_each_action_takes_and_returns() {
    let f = fixture(World::default());
    // Everything an app of theirs may call, with how it runs there.
    let listed = f.call(ACTIONS, json!({})).unwrap();
    assert!(
        listed
            .text
            .contains("calendar.read: Read an event (reads; runs without asking)"),
        "{}",
        listed.text
    );
    assert!(
        listed.text.contains(
            "mail.draft: Draft an email (changes things on this computer, undoably; runs and tells the person; its data isn't described yet)"
        ),
        "{}",
        listed.text
    );
    // The closest to the need first.
    let ranked = f
        .call(ACTIONS, json!({ "need": "draft an email" }))
        .unwrap();
    assert_eq!(ranked.data["actions"][0]["name"], "mail.draft");

    // In full: what it takes, and its data as the TypeScript the app reads.
    let read = f
        .call(
            ACTIONS,
            json!({ "actions": ["calendar.read", "chat.post"] }),
        )
        .unwrap();
    assert!(read.text.contains("const result = await bridge.call(\"calendar.read\", input), where input is:\n{\n  id: string;\n}"), "{}", read.text);
    assert!(
        read.text.contains("result.data is:\n{\n  event: {\n    attendees: string[]; // email addresses\n    title: string;\n  };\n}"),
        "{}",
        read.text
    );
    assert!(
        read.text
            .contains("chat.post: not something an app of yours can call here"),
        "{}",
        read.text
    );
    assert_eq!(read.data["actions"].as_array().unwrap().len(), 1);
    let draft = f
        .call(ACTIONS, json!({ "actions": ["mail.draft"] }))
        .unwrap();
    assert!(
        draft
            .text
            .contains("not described yet: call it once with your own tool"),
        "{}",
        draft.text
    );
    let many: Vec<String> = (0..13).map(|i| format!("calendar.x{i}")).collect();
    assert!(f.call(ACTIONS, json!({ "actions": many })).is_err());
}

#[test]
fn ga1_each_version_is_tried_and_the_agent_hears_what_to_fix() {
    let report = json!({
        "errors": ["when it opened: ReferenceError: bridge is not defined"],
        "console_errors": [], "calls": [], "screen": "", "presses": []
    });
    let f = fixture_with(
        World::default(),
        Arc::new(FakeBundler {
            tried: Some(report),
            ..FakeBundler::default()
        }),
        "/nonexistent".into(),
    );
    let saved = f.call(CREATE, new_app(&f, &[])).unwrap();
    assert!(
        saved
            .text
            .contains("Tried here, with sample data in place of the person's"),
        "{}",
        saved.text
    );
    assert!(
        saved.text.ends_with(
            "Problems to fix, then save again:\n- when it opened: ReferenceError: bridge is not defined\n- it showed nothing when it opened"
        ),
        "{}",
        saved.text
    );
    assert_eq!(saved.data["tried"]["problems"].as_array().unwrap().len(), 2);
    // Kept with the version, as its checks' notes are.
    let record = f
        .studio
        .store()
        .record("demo", &"my-vendor-disputes".parse().unwrap(), 1)
        .unwrap();
    assert!(
        record
            .notes
            .iter()
            .any(|n| n == "when tried: when it opened: ReferenceError: bridge is not defined"),
        "{:?}",
        record.notes
    );

    // A quiet try says so.
    let f = fixture(World::default());
    let saved = f.call(CREATE, new_app(&f, &[])).unwrap();
    assert!(
        saved.text.contains("Opening it showed:\n| New app"),
        "{}",
        saved.text
    );
    assert!(
        saved.text.ends_with("No problems when it was tried."),
        "{}",
        saved.text
    );
}

#[test]
fn ga1_what_each_action_returns_is_what_it_declares() {
    let f = fixture(World::default());
    let checked = std::cell::RefCell::new(BTreeSet::new());
    let run = |name: &str, input: Value| {
        let mut out = f.call(name, input).unwrap();
        if let Some(data) = out.data.as_object_mut() {
            data.remove(glass_actions::AUDIT_DETAIL);
        }
        let shape = f.provider.returns(&name.parse().unwrap()).unwrap();
        glass_actions::shape::conforms(&out.data, &shape)
            .unwrap_or_else(|e| panic!("{name}: {e}\n{}", out.data));
        checked.borrow_mut().insert(name.to_owned());
        out.data
    };
    run(BASES, json!({ "need": "mail with a write button" }));
    run(BASES, json!({}));
    run(ACTIONS, json!({ "need": "draft an email" }));
    run(
        ACTIONS,
        json!({ "actions": ["calendar.read", "mail.draft"] }),
    );
    run(SOURCE, json!({ "template": true }));
    run(SOURCE, json!({ "app": "mail" }));
    run(SOURCE, json!({ "app": "mail", "path": "src/main.ts" }));
    run(SAVE, mail_remix(&f));
    run(SAVE, mail_remix(&f));
    run(SOURCE, json!({ "app": "my-mail" }));
    run(CREATE, new_app(&f, &[]));
    run(VERSIONS, json!({}));
    run(VERSIONS, json!({ "app": "my-mail" }));
    run(REVERT, json!({ "app": "my-mail", "version": 1 }));
    f.studio
        .set_share(Arc::new(FakeConsole::default()), "dev-1".into());
    run(
        SHARE,
        json!({ "app": "my-mail", "to": "org", "note": "Write" }),
    );
    // Every action the studio declares, run and checked.
    let declared: BTreeSet<String> = f
        .provider
        .actions()
        .iter()
        .map(|d| d.name.to_string())
        .collect();
    assert_eq!(*checked.borrow(), declared);
}
