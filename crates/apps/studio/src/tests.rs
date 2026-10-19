use std::path::PathBuf;

use super::testing::FakeBundler;
use super::*;

/// The repository's apps: Mail's source and manifest, the SDK.
fn apps() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps")
}

/// What policy and the catalog say, as the tests set them.
#[derive(Debug, Default)]
struct World {
    /// Actions an app the person makes may never have.
    never: Vec<&'static str>,
    /// What the catalog offers, for any need.
    offers: Vec<(&'static str, &'static str)>,
}

impl Surroundings for World {
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
}

struct Fixture {
    _dir: tempfile::TempDir,
    provider: StudioProvider,
    studio: Studio,
    ctx: TaskContext,
}

fn fixture_with(world: World, bundler: Arc<dyn Sandbox>, program: PathBuf) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let studio = Studio::new(StudioPaths {
        store: dir.path().join("state/apps"),
        installed: apps(),
        sdk: apps().join("sdk"),
        bundler: program,
    });
    studio.set_sandbox(bundler);
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
/// Mail's own source, remixed, builds.
#[test]
fn ga4_mail_remixed_builds_with_esbuild() {
    let esbuild = apps().join("node_modules/@esbuild/linux-x64/bin/esbuild");
    if !esbuild.exists() {
        eprintln!("skipped: no esbuild in apps/node_modules (npm ci in apps/)");
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

    // An import from outside the app and the SDK doesn't resolve.
    let mut remix = mail_remix(&f);
    remix["files"]["src/main.ts"] = json!("import pad from \"left-pad\";\nconsole.log(pad);\n");
    remix["manifest"]["actions"] = json!(["mail.list"]);
    let err = f.call(SAVE, remix).unwrap_err().to_string();
    assert!(err.contains("Could not resolve \"left-pad\""), "{err}");
}
