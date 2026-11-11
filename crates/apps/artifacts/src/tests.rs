//! Artifacts between computers through an in-memory console (ADR 0033):
//! chat's store keeps each artifact as a sealed conversation, the console
//! keeps its versions' sealed blobs.

use std::path::PathBuf;

use glass_app_chat::{ChatHandle, ChatProvider};
use glass_crypto::SecretKey;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::directory::Applied;
use glass_managed::{ChatRelease, DirectoryRelease, Person};

use super::*;

const PAGE: &str = "<!doctype html><title>Q3</title><main><h1>Q3 forecast</h1>\
                    <table><tr><td>Northwind</td><td>42,000</td></tr></table></main>";
const PAGE_2: &str = "<!doctype html><title>Q3</title><main><h1>Q3 forecast</h1>\
                      <table><tr><td>Northwind</td><td>48,500</td></tr></table></main>";

/// Who acts: the person in the Artifacts app, or their agent.
const AGENT: &str = "claude-code";

struct Org {
    dir: tempfile::TempDir,
    chat: Arc<MemoryLink>,
    it: SecretKey,
    directory: DirectoryHandle,
}

struct Computer {
    chat: ChatHandle,
    roster: ChatProvider,
    artifacts: ArtifactsProvider,
    home: PathBuf,
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
        chat: Arc::new(MemoryLink::new(&it.public().unwrap())),
        it,
        directory,
    }
}

impl Org {
    /// A managed computer in this org, with IT's Chat release applied.
    fn computer(&self, name: &str) -> Computer {
        let dir = self.dir.path().join(name);
        let chat = ChatHandle::new(dir.join("chat"));
        chat.connect("acme", name, self.chat.clone());
        let release = ChatRelease::new(&self.it.public().unwrap());
        chat.set_compliance(&release.compliance, &release.fingerprint)
            .unwrap();
        let roster = ChatProvider::new(chat.clone(), self.directory.clone());
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("Reports")).unwrap();
        std::fs::create_dir_all(home.join("Invoices")).unwrap();
        Computer {
            artifacts: ArtifactsProvider::new(Arc::new(roster.clone()), self.directory.clone()),
            chat,
            roster,
            home,
        }
    }

    /// Dana's and Priya's computers, each person having opened Artifacts
    /// once (which makes their keys).
    fn pair(&self) -> (Computer, Computer) {
        let (a, b) = (self.computer("dev-a"), self.computer("dev-b"));
        a.call("dana", APP, LIST, json!({})).unwrap();
        b.call("priya", APP, LIST, json!({})).unwrap();
        (a, b)
    }
}

impl Computer {
    fn ctx(&self, login: &str, agent: &str) -> TaskContext {
        let mut ctx = glass_actions::testing::context(&self.home);
        ctx.principal.on_behalf_of.name = login.into();
        ctx.principal.agent = agent.parse().unwrap();
        ctx
    }

    fn call(
        &self,
        login: &str,
        agent: &str,
        action: &str,
        input: Value,
    ) -> Result<ActionOutput, ActionError> {
        self.artifacts
            .invoke(&self.ctx(login, agent), &action.parse().unwrap(), &input)
    }

    fn preview(&self, login: &str, action: &str, input: Value) -> Result<Preview, ActionError> {
        self.artifacts
            .preview(&self.ctx(login, AGENT), &action.parse().unwrap(), &input)
    }

    fn sync(&self) {
        self.chat.sync_once(0).unwrap();
    }

    fn shared(&self) -> &dyn Shared {
        &self.roster
    }
}

/// Dana's agent shares the page with Priya; Priya's computer has synced.
fn shared(org: &Org) -> (Computer, Computer, String) {
    let (a, b) = org.pair();
    let out = a
        .call(
            "dana",
            AGENT,
            SHARE,
            json!({ "title": "Q3 forecast", "html": PAGE, "with": ["priya"], "note": "First cut" }),
        )
        .unwrap();
    assert_eq!(out.text, "Shared Q3 forecast with Priya Shah.");
    let id = out.data["id"].as_str().unwrap().to_owned();
    b.sync();
    (a, b, id)
}

#[test]
fn ar1_an_agent_shares_a_page_a_colleague_pins_a_comment_and_version_2_follows() {
    let org = org();
    let (a, b, id) = shared(&org);

    // Priya sees it, from Dana, and opens it.
    let list = b.call("priya", APP, LIST, json!({})).unwrap();
    let mine = &list.data["artifacts"][0];
    assert_eq!(mine["title"], "Q3 forecast");
    assert_eq!(mine["owner_name"], "Dana Ruiz");
    assert_eq!(mine["mine"], false);
    assert_eq!(mine["version"], 1);
    let read = b
        .call("priya", APP, READ, json!({ "artifact": id }))
        .unwrap();
    assert_eq!(read.data["html"], PAGE);
    assert_eq!(read.data["versions"][0]["note"], "First cut");
    assert_eq!(
        read.data["versions"][0]["attribution"], "by claude-code for dana",
        "AU-10: an agent's version says so"
    );
    assert!(read.text.contains("No comments yet."), "{}", read.text);

    // She pins a comment to the table.
    let comment = b
        .call(
            "priya",
            APP,
            COMMENT,
            json!({
                "artifact": id,
                "text": "Northwind renewed at 48.5k, not 42k",
                "anchor": { "path": "main > table:nth-of-type(1)" }
            }),
        )
        .unwrap();
    let comment_id = comment.data["id"].as_str().unwrap().to_owned();

    // Dana's agent reads the feedback, ships version 2 and resolves it.
    a.sync();
    let feedback = a
        .call("dana", AGENT, READ, json!({ "artifact": id }))
        .unwrap();
    let c = &feedback.data["comments"][0];
    assert_eq!(c["by_name"], "Priya Shah");
    assert_eq!(c["version"], 1);
    assert_eq!(c["resolved"], false);
    assert_eq!(c["anchor"]["path"], "main > table:nth-of-type(1)");
    assert!(
        feedback
            .text
            .contains("[open] Priya Shah on v1 at main > table:nth-of-type(1)"),
        "{}",
        feedback.text
    );
    let v2 = a
        .call(
            "dana",
            AGENT,
            UPDATE,
            json!({ "artifact": id, "html": PAGE_2, "note": "Northwind at 48.5k" }),
        )
        .unwrap();
    assert_eq!(v2.data["version"], 2);
    a.call(
        "dana",
        AGENT,
        RESOLVE,
        json!({ "artifact": id, "comment": comment_id }),
    )
    .unwrap();

    // Priya sees version 2; her comment is resolved and about version 1.
    b.sync();
    let now = b
        .call("priya", APP, READ, json!({ "artifact": id }))
        .unwrap();
    assert_eq!(now.data["version"], 2);
    assert_eq!(now.data["html"], PAGE_2);
    assert_eq!(now.data["comments"][0]["version"], 1, "AR-4");
    assert_eq!(now.data["comments"][0]["resolved"], true);
    let list = b.call("priya", APP, LIST, json!({})).unwrap();
    assert_eq!(list.data["artifacts"][0]["open_comments"], 0);
    // The first version is still there.
    let (number, html) = b.artifacts.page("priya", &id, Some(1)).unwrap();
    assert_eq!((number, html.as_str()), (1, PAGE));
    assert!(b.artifacts.page("priya", &id, Some(3)).is_err());

    // A reply belongs to its thread: one open thread, not two.
    b.call(
        "priya",
        APP,
        COMMENT,
        json!({ "artifact": id, "text": "Also Q4?", "reply_to": comment_id }),
    )
    .unwrap();
    b.call(
        "priya",
        APP,
        RESOLVE,
        json!({ "artifact": id, "comment": comment_id, "resolved": false }),
    )
    .unwrap();
    let list = b.call("priya", APP, LIST, json!({})).unwrap();
    assert_eq!(list.data["artifacts"][0]["open_comments"], 1);
    b.call(
        "priya",
        APP,
        RESOLVE,
        json!({ "artifact": id, "comment": comment_id }),
    )
    .unwrap();
    let list = b.call("priya", APP, LIST, json!({})).unwrap();
    assert_eq!(list.data["artifacts"][0]["open_comments"], 0);

    // Reopened.
    b.call(
        "priya",
        APP,
        RESOLVE,
        json!({ "artifact": id, "comment": comment_id, "resolved": false }),
    )
    .unwrap();
    let again = b
        .call("priya", APP, READ, json!({ "artifact": id }))
        .unwrap();
    assert_eq!(again.data["comments"][0]["resolved"], false);
}

// Audit 9.3: a page was fetched from the console every time it was read,
// though a version's page never changes.
#[test]
fn ar2_a_page_fetched_once_is_read_again_without_the_console() {
    let org = org();
    let (a, b, id) = shared(&org);
    let read = |c: &Computer, login: &str, version: Option<u32>| {
        c.call(
            login,
            AGENT,
            READ,
            json!({ "artifact": id, "version": version }),
        )
    };
    // Priya reads it: her computer fetches the page.
    let first = read(&b, "priya", None).unwrap();
    assert!(first.data["html"].as_str().unwrap().contains("Q3 forecast"));

    // With the console out of reach, the page her computer has; and
    // Dana's computer, which sent it, never needed it back.
    org.chat.offline(true);
    let again = read(&b, "priya", None).unwrap();
    assert_eq!(again.data["html"], first.data["html"]);
    assert_eq!(
        read(&a, "dana", None).unwrap().data["html"],
        first.data["html"]
    );
    org.chat.offline(false);

    // A version her computer never fetched isn't one it can show then;
    // the first still is.
    a.call(
        "dana",
        AGENT,
        UPDATE,
        json!({ "artifact": id, "html": PAGE_2, "note": "Second cut" }),
    )
    .unwrap();
    b.sync();
    org.chat.offline(true);
    let second = read(&b, "priya", None).unwrap_err();
    assert!(
        second.to_string().contains("Fetching Q3 forecast"),
        "{second}"
    );
    assert_eq!(
        read(&b, "priya", Some(1)).unwrap().data["html"],
        first.data["html"]
    );
    org.chat.offline(false);
    assert!(read(&b, "priya", None).unwrap().data["html"] != first.data["html"]);
}

#[test]
fn ar2_only_its_people_open_it_and_the_console_reads_none_of_it() {
    let org = org();
    let (a, b, id) = shared(&org);
    let c = org.computer("dev-c");
    c.call("sam", APP, LIST, json!({})).unwrap();
    c.sync();

    // Sam isn't in it: he doesn't see it, can't read it, can't fetch it.
    let list = c.call("sam", APP, LIST, json!({})).unwrap();
    assert_eq!(list.data["artifacts"].as_array().unwrap().len(), 0);
    let read = c.call("sam", AGENT, READ, json!({ "artifact": id }));
    assert!(matches!(read, Err(ActionError::NotFound(_))), "{read:?}");
    let blob = org.chat.blobs()[0].id.clone();
    assert!(c.shared().get_blob("sam", &id, &blob).is_err());
    // Nor can Priya fetch it from a computer where she has no key.
    assert!(c.shared().get_blob("priya", &id, &blob).is_err());

    // Only its maker adds versions: Priya can't, and a version she posts
    // into it herself is ignored.
    let update = b.call(
        "priya",
        AGENT,
        UPDATE,
        json!({ "artifact": id, "html": "<h1>mine now</h1>", "note": "x" }),
    );
    assert!(
        matches!(update, Err(ActionError::InvalidInput(_))),
        "{update:?}"
    );
    // Audit 6.5: and she isn't asked to approve it first. The preview
    // refuses as the action does, as it does a comment with nothing in it
    // and an artifact that isn't there.
    for (who, action, input) in [
        (
            "priya",
            UPDATE,
            json!({ "artifact": id, "html": "<h1>mine now</h1>", "note": "x" }),
        ),
        ("priya", COMMENT, json!({ "artifact": id, "text": "   " })),
        (
            "dana",
            UPDATE,
            json!({ "artifact": format!("cv-{}", "0".repeat(32)), "html": "<p>x</p>", "note": "x" }),
        ),
    ] {
        let computer = if who == "priya" { &b } else { &a };
        let refused = computer
            .preview(who, action, input.clone())
            .unwrap_err()
            .to_string();
        let done = computer
            .call(who, AGENT, action, input)
            .unwrap_err()
            .to_string();
        assert_eq!(refused, done, "{who} {action}");
    }
    let forged = Part::Version {
        number: 2,
        blob: blob.clone(),
        key: BlobKey::generate().unwrap().to_hex(),
        sha256: "0".repeat(64),
        bytes: 1,
        mime: HTML.into(),
        merged: None,
        scores: None,
    };
    b.shared()
        .post("priya", &id, "", Some(forged), None, None)
        .unwrap();
    let read = b
        .call("priya", APP, READ, json!({ "artifact": id }))
        .unwrap();
    assert_eq!(read.data["version"], 1);
    assert_eq!(read.data["html"], PAGE);

    // The console keeps ciphertext: neither the page, nor its title.
    let seen = |bytes: &str| {
        org.chat
            .blobs()
            .iter()
            .any(|b| b.ct.contains(&hex::encode(bytes)))
            || org
                .chat
                .messages()
                .iter()
                .any(|m| serde_json::to_string(m).unwrap().contains(bytes))
    };
    assert!(!seen("Northwind"));
    assert!(!seen("Q3 forecast"));
    let (conversation, _) = org
        .chat
        .conversations()
        .into_iter()
        .find(|(c, _)| c.id == id)
        .unwrap();
    assert_eq!(conversation.kind, ConversationKind::Artifact);
    // Its label is in the clear for IT's inventory, and sealed with its title.
    assert_eq!(conversation.label, Some(Label::Internal));
    assert!(
        !serde_json::to_string(&conversation)
            .unwrap()
            .contains("Q3 forecast")
    );
}

#[test]
fn ar1_an_agent_waits_for_the_person_to_open_artifacts_once() {
    let org = org();
    let a = org.computer("dev-a");
    let share = a.call(
        "dana",
        AGENT,
        SHARE,
        json!({ "title": "Q3 forecast", "html": PAGE }),
    );
    let Err(ActionError::Failed(why)) = share else {
        panic!("{share:?}");
    };
    assert!(
        why.contains("hasn't opened Chat, Artifacts or Skills"),
        "{why}"
    );
    // Once she has, it saves; shared with nobody, only she sees it.
    a.call("dana", APP, LIST, json!({})).unwrap();
    let out = a
        .call(
            "dana",
            AGENT,
            SHARE,
            json!({ "title": "Q3 forecast", "html": PAGE }),
        )
        .unwrap();
    assert!(out.text.contains("only you can see it"), "{}", out.text);
    let list = a.call("dana", APP, LIST, json!({})).unwrap();
    assert_eq!(list.data["artifacts"][0]["mine"], true);
}

#[test]
fn ar1_a_share_that_fails_partway_shares_nothing() {
    let org = org();
    let (a, b) = org.pair();
    // The console takes the item, then is out of reach for its version.
    struct Flaky {
        inner: ChatProvider,
        link: Arc<MemoryLink>,
    }
    impl std::fmt::Debug for Flaky {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Flaky")
        }
    }
    impl Shared for Flaky {
        fn ready(&self, l: &str, m: bool) -> Result<(), glass_managed::chat::ChatFailure> {
            self.inner.ready(l, m)
        }
        fn items(&self, l: &str, k: ConversationKind) -> Vec<Item> {
            self.inner.items(l, k)
        }
        fn item(&self, l: &str, id: &str) -> Option<Item> {
            Shared::item(&self.inner, l, id)
        }
        fn create(
            &self,
            l: &str,
            k: ConversationKind,
            t: &str,
            label: Label,
            m: &[String],
        ) -> Result<String, glass_managed::chat::ChatFailure> {
            let id = self.inner.create(l, k, t, label, m);
            self.link.offline(true);
            id
        }
        fn post(
            &self,
            l: &str,
            to: &str,
            t: &str,
            p: Option<Part>,
            a: Option<String>,
            r: Option<String>,
        ) -> Result<String, glass_managed::chat::ChatFailure> {
            self.inner.post(l, to, t, p, a, r)
        }
        fn put_blob(
            &self,
            l: &str,
            blob: &glass_managed::shared::SealedBlob,
        ) -> Result<(), glass_managed::chat::ChatFailure> {
            self.inner.put_blob(l, blob)
        }
        fn get_blob(
            &self,
            l: &str,
            i: &str,
            id: &str,
        ) -> Result<glass_managed::shared::SealedBlob, glass_managed::chat::ChatFailure> {
            self.inner.get_blob(l, i, id)
        }
    }
    let flaky = ArtifactsProvider::new(
        Arc::new(Flaky {
            inner: a.roster.clone(),
            link: org.chat.clone(),
        }),
        org.directory.clone(),
    );
    let share = flaky.invoke(
        &a.ctx("dana", AGENT),
        &SHARE.parse().unwrap(),
        &json!({ "title": "Q3 forecast", "html": PAGE, "with": ["priya"] }),
    );
    let Err(ActionError::Failed(why)) = share else {
        panic!("{share:?}");
    };
    assert!(why.starts_with("Nothing was shared"), "{why}");
    org.chat.offline(false);
    b.sync();
    for (computer, login) in [(&a, "dana"), (&b, "priya")] {
        let list = computer.call(login, APP, LIST, json!({})).unwrap();
        assert_eq!(
            list.data["artifacts"].as_array().unwrap().len(),
            0,
            "{login}"
        );
    }
}

#[test]
fn ar4_two_updates_at_once_make_two_versions() {
    // ArtifactsShare.tla: an update read the newest version's number, then
    // posted the next. Two at once on one computer (the person in the app
    // and an agent) posted the same number; the later was never shown,
    // though its call said "is at version 2".
    let org = org();
    let (a, _b, id) = shared(&org);
    use std::sync::atomic::Ordering;
    type Racer = (ArtifactsProvider, TaskContext, String);
    type Answer = Result<ActionOutput, ActionError>;
    struct Racing {
        inner: ChatProvider,
        other: std::sync::OnceLock<Racer>,
        fired: std::sync::atomic::AtomicBool,
        raced: std::sync::Mutex<Option<std::thread::JoinHandle<Answer>>>,
    }
    impl std::fmt::Debug for Racing {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Racing")
        }
    }
    impl Shared for Racing {
        fn ready(&self, l: &str, m: bool) -> Result<(), glass_managed::chat::ChatFailure> {
            self.inner.ready(l, m)
        }
        fn items(&self, l: &str, k: ConversationKind) -> Vec<Item> {
            self.inner.items(l, k)
        }
        fn item(&self, l: &str, id: &str) -> Option<Item> {
            Shared::item(&self.inner, l, id)
        }
        fn create(
            &self,
            l: &str,
            k: ConversationKind,
            t: &str,
            label: Label,
            m: &[String],
        ) -> Result<String, glass_managed::chat::ChatFailure> {
            self.inner.create(l, k, t, label, m)
        }
        fn post(
            &self,
            l: &str,
            to: &str,
            t: &str,
            p: Option<Part>,
            a: Option<String>,
            r: Option<String>,
        ) -> Result<String, glass_managed::chat::ChatFailure> {
            // The first version posted: the other update runs meanwhile,
            // as far as it gets in a second.
            if matches!(p, Some(Part::Version { .. })) && !self.fired.swap(true, Ordering::SeqCst) {
                let (provider, ctx, id) = self.other.get().unwrap().clone();
                let other = std::thread::spawn(move || {
                    provider.invoke(
                        &ctx,
                        &UPDATE.parse().unwrap(),
                        &json!({ "artifact": id, "html": PAGE, "note": "The agent's" }),
                    )
                });
                for _ in 0..100 {
                    if other.is_finished() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                *self.raced.lock().unwrap() = Some(other);
            }
            self.inner.post(l, to, t, p, a, r)
        }
        fn put_blob(
            &self,
            l: &str,
            blob: &glass_managed::shared::SealedBlob,
        ) -> Result<(), glass_managed::chat::ChatFailure> {
            self.inner.put_blob(l, blob)
        }
        fn get_blob(
            &self,
            l: &str,
            i: &str,
            id: &str,
        ) -> Result<glass_managed::shared::SealedBlob, glass_managed::chat::ChatFailure> {
            self.inner.get_blob(l, i, id)
        }
    }
    let racing = Arc::new(Racing {
        inner: a.roster.clone(),
        other: std::sync::OnceLock::new(),
        fired: std::sync::atomic::AtomicBool::new(false),
        raced: std::sync::Mutex::new(None),
    });
    let provider = ArtifactsProvider::new(racing.clone(), org.directory.clone());
    racing
        .other
        .set((provider.clone(), a.ctx("dana", AGENT), id.clone()))
        .unwrap();
    let mine = provider
        .invoke(
            &a.ctx("dana", APP),
            &UPDATE.parse().unwrap(),
            &json!({ "artifact": id, "html": PAGE_2, "note": "Mine" }),
        )
        .unwrap();
    let other = racing.raced.lock().unwrap().take().unwrap();
    let theirs = other.join().unwrap().unwrap();
    let (mine, theirs) = (mine.data["version"].clone(), theirs.data["version"].clone());
    assert_ne!(mine, theirs, "two versions, two numbers");
    for (number, html, note) in [(&mine, PAGE_2, "Mine"), (&theirs, PAGE, "The agent's")] {
        let read = a
            .call(
                "dana",
                APP,
                READ,
                json!({ "artifact": id, "version": number }),
            )
            .unwrap();
        assert_eq!(read.data["html"], html, "v{number} is what its call posted");
        let versions = read.data["versions"].as_array().unwrap();
        assert!(
            versions
                .iter()
                .any(|v| v["number"] == *number && v["note"] == note),
            "{versions:?}"
        );
    }
}

#[test]
fn ar1_a_page_comes_from_a_granted_folder_and_only_from_one() {
    let org = org();
    let (a, _b) = org.pair();
    std::fs::write(a.home.join("Reports/q3.html"), PAGE).unwrap();
    let out = a
        .call(
            "dana",
            AGENT,
            SHARE,
            json!({ "title": "Q3 forecast", "path": "Reports/q3.html", "with": ["priya"] }),
        )
        .unwrap();
    let id = out.data["id"].as_str().unwrap();
    assert_eq!(a.artifacts.page("dana", id, None).unwrap().1, PAGE);
    // The preview names the file it reads, for the broker's label check.
    let preview = a
        .preview(
            "dana",
            SHARE,
            json!({ "title": "Q3 forecast", "path": "Reports/q3.html", "with": ["priya"] }),
        )
        .unwrap();
    assert_eq!(preview.reads, vec![a.home.join("Reports/q3.html")]);
    // A read is judged by the artifact's label: found, it carries it;
    // not found, it is refused where it is judged (it used to pass as
    // "an artifact" with no label).
    let found = a.preview("dana", READ, json!({ "artifact": id })).unwrap();
    assert!(found.label.is_some(), "{found:?}");
    let missing = a.preview("dana", READ, json!({ "artifact": "0000000000000000" }));
    assert!(
        matches!(
            missing,
            Err(ActionError::NotFound(_) | ActionError::InvalidInput(_))
        ),
        "{missing:?}"
    );

    for path in ["../../etc/passwd", "/etc/passwd", "Secrets/x.html"] {
        let share = a.call("dana", AGENT, SHARE, json!({ "title": "x", "path": path }));
        assert!(share.is_err(), "{path}: {share:?}");
    }
    // A symlink out of the folder isn't followed.
    std::os::unix::fs::symlink("/etc/hostname", a.home.join("Reports/out.html")).unwrap();
    let share = a.call(
        "dana",
        AGENT,
        SHARE,
        json!({ "title": "x", "path": "Reports/out.html" }),
    );
    assert!(share.is_err(), "{share:?}");
}

#[test]
fn ar3_shared_to_a_conversation_it_reaches_its_people_as_a_card() {
    let org = org();
    let (a, b) = org.pair();
    let c = org.computer("dev-c");
    c.call("sam", APP, LIST, json!({})).unwrap();
    let team = a
        .roster
        .invoke(
            &a.ctx("dana", "chat"),
            &"chat.create".parse().unwrap(),
            &json!({ "name": "Planning", "people": ["priya", "sam"] }),
        )
        .unwrap()
        .data["conversation"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = a
        .call(
            "dana",
            AGENT,
            SHARE,
            json!({ "title": "Q3 forecast", "html": PAGE, "conversation": team }),
        )
        .unwrap();
    assert_eq!(out.text, "Shared Q3 forecast with Priya Shah, Sam Lee.");
    let id = out.data["id"].as_str().unwrap().to_owned();
    for (computer, login) in [(&b, "priya"), (&c, "sam")] {
        computer.sync();
        let read = computer
            .call(login, APP, READ, json!({ "artifact": id }))
            .unwrap();
        assert_eq!(read.data["html"], PAGE);
        let planning = computer
            .shared()
            .items(login, ConversationKind::Channel)
            .into_iter()
            .find(|c| c.id == team)
            .unwrap();
        let card = planning.messages.last().unwrap();
        assert_eq!(
            card.part,
            Some(Part::Shared {
                item: id.clone(),
                kind: "artifact".into(),
                title: "Q3 forecast".into(),
            })
        );
        assert_eq!(card.attribution.as_deref(), Some("by claude-code for dana"));
        // Chat shows it as a card, and an agent reading chat finds its id.
        let read = computer
            .roster
            .invoke(
                &computer.ctx(login, "chat"),
                &"chat.read".parse().unwrap(),
                &json!({ "conversation": team }),
            )
            .unwrap();
        let last = read.data["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(last["shared"]["item"], id);
        assert_eq!(last["shared"]["title"], "Q3 forecast");
        assert!(
            read.text.contains(&format!("[artifact {id}]")),
            "{}",
            read.text
        );
    }
    // The Chat app shows chat's kinds only; the artifact is not one.
    assert!(!ConversationKind::Artifact.is_chat());
}

#[test]
fn ar5_what_is_asked_says_who_will_see_it() {
    let org = org();
    let (a, b) = org.pair();
    let preview = a
        .preview(
            "dana",
            SHARE,
            json!({ "title": "Q3 forecast", "html": PAGE, "with": ["priya", "dana"], "note": "First cut" }),
        )
        .unwrap();
    assert_eq!(preview.summary, "Share Q3 forecast");
    assert_eq!(preview.items[0], "Shared with Priya Shah");
    assert!(preview.items.iter().any(|i| i == "Note: First cut"));
    let alone = a
        .preview(
            "dana",
            SHARE,
            json!({ "title": "Q3 forecast", "html": PAGE }),
        )
        .unwrap();
    assert_eq!(alone.items[0], "Only you can see it");

    // Artifacts stay inside the organization, and inputs are checked.
    let bad = [
        json!({ "title": "Q3", "html": PAGE, "with": ["mallory@evil.example"] }),
        json!({ "title": "", "html": PAGE }),
        json!({ "title": "a\nb", "html": PAGE }),
        json!({ "title": "Q3" }),
        json!({ "title": "Q3", "html": PAGE, "path": "Reports/q3.html" }),
        json!({ "title": "Q3", "html": "x".repeat(MAX_BLOB_BYTES + 1) }),
        json!({ "title": "Q3", "html": PAGE, "conversation": "general" }),
    ];
    for input in bad {
        let share = a.call("dana", AGENT, SHARE, input.clone());
        assert!(
            matches!(share, Err(ActionError::InvalidInput(_))),
            "{input}: {share:?}"
        );
    }
    let out = a
        .call(
            "dana",
            AGENT,
            SHARE,
            json!({ "title": "Q3", "html": PAGE, "with": ["priya"] }),
        )
        .unwrap();
    let id = out.data["id"].as_str().unwrap().to_owned();
    b.sync();
    for anchor in [json!({ "x": 1001 }), json!({ "path": "a\u{7}b" })] {
        let comment = b.call(
            "priya",
            APP,
            COMMENT,
            json!({ "artifact": id, "text": "here", "anchor": anchor }),
        );
        assert!(
            matches!(comment, Err(ActionError::InvalidInput(_))),
            "{comment:?}"
        );
    }
    let reply = b.call(
        "priya",
        APP,
        COMMENT,
        json!({ "artifact": id, "text": "here", "reply_to": "nope" }),
    );
    assert!(
        matches!(reply, Err(ActionError::InvalidInput(_))),
        "{reply:?}"
    );
}

#[test]
fn ar_others_see_nothing_until_the_person_says_yes() {
    let org = org();
    let a = org.computer("dev-a");
    let tiers: BTreeMap<String, (Tier, SideEffect)> = a
        .artifacts
        .actions()
        .into_iter()
        .map(|d| (d.name.to_string(), (d.default_tier, d.side_effect)))
        .collect();
    assert_eq!(tiers[LIST], (Tier::Auto, SideEffect::None));
    assert_eq!(tiers[READ], (Tier::Auto, SideEffect::None));
    // A resolve reaches everyone in it too.
    for action in [SHARE, UPDATE, COMMENT, RESOLVE] {
        assert_eq!(
            tiers[action],
            (Tier::Ask, SideEffect::Irreversible),
            "{action}"
        );
    }
}

#[test]
fn adr0034_an_artifact_is_labeled_at_least_what_its_task_read() {
    let org = org();
    let (a, b) = org.pair();
    // A task that read confidential data shares without saying: the
    // artifact is confidential, and the preview says so.
    let mut ctx = a.ctx("dana", AGENT);
    ctx.data_label = Some(Label::Confidential);
    let input = json!({ "title": "Board deck", "html": PAGE, "with": ["priya"] });
    let preview = a
        .artifacts
        .preview(&ctx, &SHARE.parse().unwrap(), &input)
        .unwrap();
    assert!(
        preview
            .items
            .iter()
            .any(|i| i.starts_with("Labeled confidential")),
        "{:?}",
        preview.items
    );
    let out = a
        .artifacts
        .invoke(&ctx, &SHARE.parse().unwrap(), &input)
        .unwrap();
    assert_eq!(out.data["label"], "confidential");
    let id = out.data["id"].as_str().unwrap().to_owned();
    // Asking for less doesn't lower it.
    let lower = json!({ "title": "Board deck", "html": PAGE, "label": "public" });
    let out = a
        .artifacts
        .invoke(&ctx, &SHARE.parse().unwrap(), &lower)
        .unwrap();
    assert_eq!(out.data["label"], "confidential");

    // Reading it names its label, so the pipeline raises the reader's.
    b.sync();
    let read = b
        .artifacts
        .preview(
            &b.ctx("priya", AGENT),
            &READ.parse().unwrap(),
            &json!({ "artifact": id }),
        )
        .unwrap();
    assert_eq!(read.label, Some(Label::Confidential));
    let listed = b.call("priya", APP, LIST, json!({})).unwrap();
    assert_eq!(listed.data["artifacts"][0]["label"], "confidential");

    // An internal artifact takes nothing from a task that read more.
    let internal = a
        .call("dana", AGENT, SHARE, json!({ "title": "Q3", "html": PAGE }))
        .unwrap();
    let internal = internal.data["id"].as_str().unwrap();
    for (action, input) in [
        (
            UPDATE,
            json!({ "artifact": internal, "html": PAGE_2, "note": "more" }),
        ),
        (
            COMMENT,
            json!({ "artifact": internal, "text": "see the board deck" }),
        ),
    ] {
        let refused = a.artifacts.invoke(&ctx, &action.parse().unwrap(), &input);
        assert!(
            matches!(refused, Err(ActionError::OutOfScope(_))),
            "{action}: {refused:?}"
        );
    }
}

#[test]
fn ga1_what_each_action_returns_is_what_it_declares() {
    let org = org();
    let (a, b) = org.pair();
    let check = |action: &str, out: Result<ActionOutput, ActionError>| {
        let mut data = out.unwrap_or_else(|e| panic!("{action}: {e}")).data;
        // What the runtime keeps for the record never reaches the caller.
        if let Some(fields) = data.as_object_mut() {
            fields.remove(glass_actions::AUDIT_DETAIL);
        }
        let shape = a.artifacts.returns(&action.parse().unwrap()).unwrap();
        glass_actions::shape::conforms(&data, &shape).unwrap_or_else(|e| panic!("{action}: {e}"));
        data
    };
    let shared = check(
        SHARE,
        a.call(
            "dana",
            AGENT,
            SHARE,
            json!({ "title": "Q3 forecast", "html": PAGE, "with": ["priya"], "note": "First cut" }),
        ),
    );
    let id = shared["id"].as_str().unwrap().to_owned();
    b.sync();
    // A comment pinned to a point, one on an element, and a reply.
    let pinned = check(
        COMMENT,
        b.call(
            "priya",
            APP,
            COMMENT,
            json!({ "artifact": id, "text": "Is this net?", "anchor": { "x": 400, "y": 120 } }),
        ),
    );
    check(
        COMMENT,
        b.call(
            "priya",
            AGENT,
            COMMENT,
            json!({ "artifact": id, "text": "Source?", "anchor": { "path": "main > table" } }),
        ),
    );
    a.sync();
    check(
        COMMENT,
        a.call(
            "dana",
            APP,
            COMMENT,
            json!({ "artifact": id, "text": "Gross", "reply_to": pinned["id"] }),
        ),
    );
    check(
        UPDATE,
        a.call(
            "dana",
            AGENT,
            UPDATE,
            json!({ "artifact": id, "html": PAGE_2, "note": "Net figures" }),
        ),
    );
    check(
        RESOLVE,
        a.call(
            "dana",
            APP,
            RESOLVE,
            json!({ "artifact": id, "comment": pinned["id"] }),
        ),
    );
    b.sync();
    let listed = check(LIST, b.call("priya", APP, LIST, json!({})));
    assert_eq!(listed["artifacts"][0]["version"], 2);
    let read = check(
        READ,
        b.call("priya", AGENT, READ, json!({ "artifact": id })),
    );
    assert_eq!(read["comments"].as_array().unwrap().len(), 3);
    assert_eq!(read["versions"].as_array().unwrap().len(), 2);
    // Every action the provider declares has a shape.
    for def in a.artifacts.actions() {
        assert!(a.artifacts.returns(&def.name).is_some(), "{}", def.name);
    }
}
