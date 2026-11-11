//! Docs between computers through an in-memory console (ADR 0035): an
//! agent makes a doc and writes into it, colleagues edit it live through
//! their Docs app's pushes and pulls, and roles, versions and labels hold.

use std::path::PathBuf;

use glass_app_chat::{ChatHandle, ChatProvider};
use glass_crypto::SecretKey;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::directory::Applied;
use glass_managed::{ChatRelease, DirectoryRelease, Person};
use yrs::{ReadTxn, Text, Transact, XmlFragment, XmlTextPrelim};

use super::*;

const AGENT: &str = "claude-code";
const PLAN: &str = "# Q3 plan\n\nShip **Northwind** by Friday.\n\n## Risks\n\nNone yet.\n";

struct Org {
    dir: tempfile::TempDir,
    chat: Arc<MemoryLink>,
    it: SecretKey,
    directory: DirectoryHandle,
}

struct Computer {
    chat: ChatHandle,
    docs: DocsProvider,
    roster: Arc<ChatProvider>,
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
    fn computer(&self, name: &str) -> Computer {
        let dir = self.dir.path().join(name);
        let chat = ChatHandle::new(dir.join("chat"));
        chat.connect("acme", name, self.chat.clone());
        let release = ChatRelease::new(&self.it.public().unwrap());
        chat.set_compliance(&release.compliance, &release.fingerprint)
            .unwrap();
        let roster = Arc::new(ChatProvider::new(chat.clone(), self.directory.clone()));
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        Computer {
            docs: DocsProvider::new(roster.clone(), self.directory.clone()),
            roster,
            chat,
            home,
        }
    }

    /// Dana's, Priya's and Sam's computers, each having opened Docs once.
    fn three(&self) -> (Computer, Computer, Computer) {
        let a = self.computer("dev-a");
        let b = self.computer("dev-b");
        let c = self.computer("dev-c");
        a.call("dana", APP, LIST, json!({})).unwrap();
        b.call("priya", APP, LIST, json!({})).unwrap();
        c.call("sam", APP, LIST, json!({})).unwrap();
        (a, b, c)
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
        self.with(&self.ctx(login, agent), action, input)
    }

    fn with(
        &self,
        ctx: &TaskContext,
        action: &str,
        input: Value,
    ) -> Result<ActionOutput, ActionError> {
        self.docs.invoke(ctx, &action.parse().unwrap(), &input)
    }

    fn sync(&self) {
        self.chat.sync_once(0).unwrap();
    }

    fn markdown(&self, login: &str, id: &str) -> String {
        self.sync();
        self.call(login, AGENT, READ, json!({ "doc": id }))
            .unwrap()
            .data["markdown"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

/// What a page does when someone types `text` at the end: a new paragraph,
/// as a Yjs update against the state it opened.
fn typed(state: &str, text: &str) -> String {
    let doc = ydoc::doc_from(&[base64::decode(state).unwrap()]).unwrap();
    let fragment = doc.get_or_insert_xml_fragment(ydoc::FRAGMENT);
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let len = fragment.len(&txn);
        let p = fragment.insert(&mut txn, len, yrs::XmlElementPrelim::empty("paragraph"));
        let t = p.push_back(&mut txn, XmlTextPrelim::new(""));
        t.insert(&mut txn, 0, text);
    }
    base64::encode(&doc.transact().encode_state_as_update_v1(&before))
}

/// Dana's agent makes the plan with Priya editing and Sam viewing; their
/// computers have synced.
fn planned(org: &Org) -> (Computer, Computer, Computer, String) {
    let (a, b, c) = org.three();
    let made = a
        .call(
            "dana",
            AGENT,
            CREATE,
            json!({ "title": "Q3 plan", "with": ["priya"], "viewers": ["sam"], "markdown": PLAN }),
        )
        .unwrap();
    assert_eq!(made.text, "Made Q3 plan, shared with Priya Shah, Sam Lee.");
    let id = made.data["id"].as_str().unwrap().to_owned();
    b.sync();
    c.sync();
    (a, b, c, id)
}

#[test]
fn dc6_an_agent_makes_a_doc_and_colleagues_read_it_as_markdown() {
    let org = org();
    let (_a, b, c, id) = planned(&org);
    assert_eq!(
        b.markdown("priya", &id),
        "# Q3 plan\n\nShip **Northwind** by Friday.\n\n## Risks\n\nNone yet.\n"
    );
    let list = c.call("sam", APP, LIST, json!({})).unwrap();
    let d = &list.data["docs"][0];
    assert_eq!(d["title"], "Q3 plan");
    assert_eq!(d["role"], "viewer");
    assert_eq!(d["owner_name"], "Dana Ruiz");
    assert_eq!(d["label"], "internal");
    let read = b.call("priya", AGENT, READ, json!({ "doc": id })).unwrap();
    assert_eq!(read.data["role"], "editor");
}

#[test]
fn dc3_two_people_edit_live_and_see_each_others_changes() {
    let org = org();
    let (a, b, _c, id) = planned(&org);
    // Each opens it in Docs: an editing session.
    let dana = a.ctx("dana", APP);
    let priya = b.ctx("priya", APP);
    let opened_a = a.with(&dana, OPEN, json!({ "doc": id })).unwrap();
    let opened_b = b.with(&priya, OPEN, json!({ "doc": id })).unwrap();
    assert_eq!(opened_b.data["role"], "editor");
    let cursor_b = opened_b.data["cursor"].as_u64().unwrap();

    // Dana types; Priya's page pulls it and gets the same document.
    let update = typed(
        opened_a.data["state"].as_str().unwrap(),
        "Kickoff on Monday.",
    );
    a.docs.push(&dana, &id, "", &update).unwrap();
    b.sync();
    let pulled = b.docs.pull(&priya, &id, "", cursor_b).unwrap();
    assert_eq!(pulled.updates.len(), 1);
    assert_eq!(pulled.updates[0]["by_name"], "Dana Ruiz");
    let page = ydoc::doc_from(&[
        base64::decode(opened_b.data["state"].as_str().unwrap()).unwrap(),
        base64::decode(pulled.updates[0]["update"].as_str().unwrap()).unwrap(),
    ])
    .unwrap();
    assert!(markdown::render(&ydoc::blocks(&page)).ends_with("Kickoff on Monday.\n"));
    assert!(b.markdown("priya", &id).ends_with("Kickoff on Monday.\n"));
    // Nothing new: an empty pull, and Dana's own push isn't echoed to her.
    assert!(
        b.docs
            .pull(&priya, &id, "", pulled.cursor)
            .unwrap()
            .updates
            .is_empty()
    );
    a.sync();
    assert!(
        a.docs
            .pull(&dana, &id, "", 0)
            .unwrap()
            .updates
            .iter()
            .all(|u| u["by"] != "dana" || u["update"] != update)
    );

    // Closing says what the session changed, once.
    let summary = a.docs.closed(&dana.principal.task);
    assert_eq!(summary.len(), 1);
    assert!(
        summary[0].starts_with("Q3 plan: 1 change in 1 minute"),
        "{summary:?}"
    );
    assert!(a.docs.closed(&dana.principal.task).is_empty());
}

#[test]
fn dc4_roles_hold_on_every_computer() {
    let org = org();
    let (a, b, c, id) = planned(&org);
    let sam = c.ctx("sam", APP);
    let opened = c.with(&sam, OPEN, json!({ "doc": id })).unwrap();
    assert_eq!(opened.data["role"], "viewer");
    // A viewer's page can't push, an agent for them can't write or comment.
    let update = typed(opened.data["state"].as_str().unwrap(), "Sam was here");
    assert!(c.docs.push(&sam, &id, "", &update).is_err());
    assert!(
        c.call("sam", AGENT, WRITE, json!({ "doc": id, "markdown": "x" }))
            .is_err()
    );
    assert!(
        c.call("sam", AGENT, COMMENT, json!({ "doc": id, "text": "x" }))
            .is_err()
    );
    // A push from a session that didn't open the doc is refused too.
    assert!(b.docs.push(&b.ctx("priya", APP), &id, "", &update).is_err());
    // Dana makes Sam a commenter: he comments; others see it.
    assert!(
        b.call("priya", APP, ROLES, json!({ "doc": id, "editors": [] }))
            .is_err(),
        "only the owner"
    );
    a.call(
        "dana",
        APP,
        ROLES,
        json!({ "doc": id, "editors": ["priya"], "commenters": ["sam"] }),
    )
    .unwrap();
    c.sync();
    c.call(
        "sam",
        AGENT,
        COMMENT,
        json!({ "doc": id, "text": "What's the budget?" }),
    )
    .unwrap();
    a.sync();
    let read = a.call("dana", AGENT, READ, json!({ "doc": id })).unwrap();
    assert_eq!(read.data["comments"][0]["by_name"], "Sam Lee");
    assert!(read.text.contains("Sam Lee: What's the budget?"));
    // Someone not in it can't be given a role (membership is fixed, C1).
    let outside = a.call(
        "dana",
        APP,
        ROLES,
        json!({ "doc": id, "editors": ["priya"], "commenters": ["root"] }),
    );
    assert!(outside.is_err());
    // Priya opened it to edit; Dana makes her a viewer: her open page's
    // next push is refused without her opening it again.
    let priya = b.ctx("priya", APP);
    let opened = b.with(&priya, OPEN, json!({ "doc": id })).unwrap();
    assert_eq!(opened.data["role"], "editor");
    a.call(
        "dana",
        APP,
        ROLES,
        json!({ "doc": id, "editors": [], "commenters": ["sam"] }),
    )
    .unwrap();
    b.sync();
    let update = typed(opened.data["state"].as_str().unwrap(), "Priya was here");
    let refused = b.docs.push(&priya, &id, "", &update).unwrap_err();
    assert!(refused.to_string().contains("not edit"), "{refused}");
}

// Audit 6.5: a preview named the doc and asked nothing else, so a person
// was asked to approve a change that was then refused.
#[test]
fn dc4_a_change_someone_may_not_make_is_refused_before_anyone_is_asked_to_approve_it() {
    let org = org();
    let (a, _b, c, id) = planned(&org);
    let asked = |computer: &Computer, login: &str, action: &str, input: &Value| {
        computer
            .docs
            .preview(&computer.ctx(login, AGENT), &action.parse().unwrap(), input)
    };
    // Sam can view it. Whatever the action refuses him, its preview
    // refuses him first, in the same words.
    for (action, input) in [
        (WRITE, json!({ "doc": id, "markdown": "x" })),
        (COMMENT, json!({ "doc": id, "text": "x" })),
        (VERSION, json!({ "doc": id, "name": "Draft 1" })),
        (ROLES, json!({ "doc": id, "editors": [] })),
        (RESOLVE, json!({ "doc": id, "comment": "nope" })),
    ] {
        let refused = asked(&c, "sam", action, &input).unwrap_err().to_string();
        let done = c.call("sam", AGENT, action, input).unwrap_err().to_string();
        assert_eq!(refused, done, "{action}");
    }
    // And what the action refuses anyone, for what was asked.
    for (action, input) in [
        (WRITE, json!({ "doc": id, "markdown": "   " })),
        (WRITE, json!({ "doc": id, "markdown": "x", "at": "middle" })),
        (COMMENT, json!({ "doc": id, "text": "" })),
        (VERSION, json!({ "doc": id, "name": "" })),
        (
            ROLES,
            json!({ "doc": id, "editors": ["priya"], "commenters": ["root"] }),
        ),
        (RESOLVE, json!({ "doc": id, "comment": "nope" })),
        (
            WRITE,
            json!({ "doc": format!("cv-{}", "0".repeat(32)), "markdown": "x" }),
        ),
    ] {
        let refused = asked(&a, "dana", action, &input).unwrap_err().to_string();
        let done = a
            .call("dana", AGENT, action, input)
            .unwrap_err()
            .to_string();
        assert_eq!(refused, done, "{action}");
    }
    // What its owner may do is previewed as it was, by the doc's name.
    let write = asked(
        &a,
        "dana",
        WRITE,
        &json!({ "doc": id, "markdown": "## Next\n\nShip it" }),
    )
    .unwrap();
    assert_eq!(write.summary, "Write in Q3 plan");
    assert_eq!(write.items, ["## Next", "Ship it"]);
    let roles = asked(
        &a,
        "dana",
        ROLES,
        &json!({ "doc": id, "editors": ["priya"] }),
    )
    .unwrap();
    assert_eq!(roles.summary, "Change who edits Q3 plan");
}

#[test]
fn dc6_an_agents_write_lands_after_the_section_and_is_marked_as_its() {
    let org = org();
    let (a, b, _c, id) = planned(&org);
    b.call(
        "priya",
        AGENT,
        WRITE,
        json!({ "doc": id, "markdown": "- Supplier delay", "at": { "after": "Risks" } }),
    )
    .unwrap();
    a.sync();
    let md = a.markdown("dana", &id);
    assert!(
        md.find("None yet.").unwrap() < md.find("Supplier delay").unwrap(),
        "{md}"
    );
    let opened = a.call("dana", APP, OPEN, json!({ "doc": id })).unwrap();
    let page =
        ydoc::doc_from(&[base64::decode(opened.data["state"].as_str().unwrap()).unwrap()]).unwrap();
    let marked = ydoc::blocks(&page).iter().any(|b| match b {
        markdown::Block::Bullets(items) => items.iter().flatten().any(|p| matches!(p,
            markdown::Block::Paragraph(spans) if spans.iter().any(|s| s.marks.contains(&markdown::Mark::Ai("by claude-code for priya".into()))))),
        _ => false,
    });
    assert!(marked, "an agent's text is marked as its own (DC-2)");
    assert!(
        b.call(
            "priya",
            AGENT,
            WRITE,
            json!({ "doc": id, "markdown": "x", "at": "middle" })
        )
        .is_err()
    );
}

#[test]
fn dc3_versions_and_snapshots_keep_the_doc_whole_for_a_fresh_reader() {
    let org = org();
    let (a, _b, c, id) = planned(&org);
    let dana = a.ctx("dana", APP);
    let mut state = a.with(&dana, OPEN, json!({ "doc": id })).unwrap().data["state"]
        .as_str()
        .unwrap()
        .to_owned();
    for n in 0..(SNAPSHOT_EVERY + 5) {
        let update = typed(&state, &format!("Line {n}"));
        a.docs.push(&dana, &id, "", &update).unwrap();
        let doc = ydoc::doc_from(&[
            base64::decode(&state).unwrap(),
            base64::decode(&update).unwrap(),
        ])
        .unwrap();
        state = base64::encode(&ydoc::state(&doc));
    }
    a.with(
        &dana,
        VERSION,
        json!({ "doc": id, "name": "Before review" }),
    )
    .unwrap();
    // A computer that never opened it loads it from the newest snapshot on.
    c.sync();
    let fresh = DocsProvider::new(c.roster.clone(), org.directory.clone());
    let read = fresh
        .invoke(
            &c.ctx("sam", AGENT),
            &READ.parse().unwrap(),
            &json!({ "doc": id }),
        )
        .unwrap();
    let md = read.data["markdown"].as_str().unwrap();
    assert!(md.starts_with("# Q3 plan"), "{md}");
    assert!(md.contains(&format!("Line {}", SNAPSHOT_EVERY + 4)), "{md}");
    let opened = fresh
        .invoke(
            &c.ctx("sam", APP),
            &OPEN.parse().unwrap(),
            &json!({ "doc": id }),
        )
        .unwrap();
    assert_eq!(opened.data["versions"][0]["name"], "Before review");
    assert_eq!(opened.data["versions"][0]["by_name"], "Dana Ruiz");
}

/// `login`'s page on `computer`, opened by `ctx`: its state and cursor.
fn opened(computer: &Computer, ctx: &TaskContext, id: &str) -> (String, u64) {
    let out = computer.with(ctx, OPEN, json!({ "doc": id })).unwrap();
    (
        out.data["state"].as_str().unwrap().to_owned(),
        out.data["cursor"].as_u64().unwrap(),
    )
}

/// What a computer that opens the doc from its log reads.
fn fresh_read(org: &Org, computer: &Computer, login: &str, id: &str) -> String {
    computer.sync();
    DocsProvider::new(computer.roster.clone(), org.directory.clone())
        .invoke(
            &computer.ctx(login, AGENT),
            &READ.parse().unwrap(),
            &json!({ "doc": id }),
        )
        .unwrap()
        .data["markdown"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn dc3_an_edit_that_arrives_after_ones_own_later_one_still_lands() {
    // DocsCollab.tla, pastGaps: a computer keeps its own post at once, so
    // Priya's earlier edit, still on its way, arrived after it and fell
    // behind the cursor: Dana's computer and page never had it.
    let org = org();
    let (a, b, _c, id) = planned(&org);
    a.sync();
    let dana = a.ctx("dana", APP);
    let priya = b.ctx("priya", APP);
    let (state_a, cursor) = opened(&a, &dana, &id);
    let (state_b, _) = opened(&b, &priya, &id);
    b.docs
        .push(&priya, &id, "", &typed(&state_b, "Priya's line."))
        .unwrap();
    a.docs
        .push(&dana, &id, "", &typed(&state_a, "Dana's line."))
        .unwrap();
    let early = a.docs.pull(&dana, &id, "", cursor).unwrap();
    a.sync();
    let late = a.docs.pull(&dana, &id, "", early.cursor).unwrap();
    assert!(
        late.updates.iter().any(|u| u["by"] == "priya"),
        "Dana's page hears Priya's line: {late:?}"
    );
    let md = a.markdown("dana", &id);
    assert!(
        md.contains("Priya's line.") && md.contains("Dana's line."),
        "{md}"
    );
}

#[test]
fn dc3_a_version_named_while_an_edit_is_on_its_way_doesnt_drop_it() {
    // DocsCollab.tla, byPosition: a fresh reader skipped every edit before
    // the newest snapshot, though Dana's computer hadn't had Priya's.
    let org = org();
    let (a, b, c, id) = planned(&org);
    a.sync();
    let priya = b.ctx("priya", APP);
    let (state_b, _) = opened(&b, &priya, &id);
    b.docs
        .push(&priya, &id, "", &typed(&state_b, "Priya's line."))
        .unwrap();
    a.call("dana", APP, VERSION, json!({ "doc": id, "name": "Draft" }))
        .unwrap();
    let md = fresh_read(&org, &c, "sam", &id);
    assert!(md.contains("Priya's line."), "{md}");
    assert!(md.starts_with("# Q3 plan"), "{md}");
}

#[test]
fn dc4_an_edit_made_while_its_author_edited_reaches_every_page() {
    // DocsCollab.tla, rolesNow: a pull judged each update by who edits
    // now, so Dana's page lost what Priya wrote before Dana made her a
    // viewer, though Dana's computer kept it.
    let org = org();
    let (a, b, _c, id) = planned(&org);
    a.sync();
    let dana = a.ctx("dana", APP);
    let priya = b.ctx("priya", APP);
    let (_, cursor) = opened(&a, &dana, &id);
    let (state_b, _) = opened(&b, &priya, &id);
    b.docs
        .push(&priya, &id, "", &typed(&state_b, "Priya's line."))
        .unwrap();
    a.sync();
    a.call("dana", APP, ROLES, json!({ "doc": id, "editors": [] }))
        .unwrap();
    a.sync();
    let pulled = a.docs.pull(&dana, &id, "", cursor).unwrap();
    assert!(
        pulled.updates.iter().any(|u| u["by"] == "priya"),
        "{pulled:?}"
    );
    assert!(a.markdown("dana", &id).contains("Priya's line."));
}

#[test]
fn dc4_a_version_posted_after_its_author_stopped_editing_isnt_taken() {
    // DocsCollab.tla, finalRoles: a fresh reader judged the newest
    // snapshot's sender by who edits now, so it took a version Priya
    // posted while a viewer, with her refused edit, once she edited again.
    let org = org();
    let (a, b, c, id) = planned(&org);
    let priya = b.ctx("priya", APP);
    let (state_b, _) = opened(&b, &priya, &id);
    a.sync();
    a.call("dana", APP, ROLES, json!({ "doc": id, "editors": [] }))
        .unwrap();
    // Before Priya's computer hears it, she types and names a version.
    b.docs
        .push(&priya, &id, "", &typed(&state_b, "Priya, too late."))
        .unwrap();
    b.call("priya", APP, VERSION, json!({ "doc": id, "name": "Mine" }))
        .unwrap();
    a.sync();
    a.call(
        "dana",
        APP,
        ROLES,
        json!({ "doc": id, "editors": ["priya"] }),
    )
    .unwrap();
    let following = a.markdown("dana", &id);
    assert!(!following.contains("Priya, too late."), "{following}");
    let md = fresh_read(&org, &c, "sam", &id);
    assert!(!md.contains("Priya, too late."), "{md}");
    assert!(md.starts_with("# Q3 plan"), "{md}");
}

#[test]
fn dc4_an_edit_posted_after_its_author_stopped_editing_leaves_their_computer_too() {
    // DocsCollab.tla, keepRefused and pastGaps: Priya's computer kept her
    // refused edit, and, having pulled her own post first, never heard
    // that she views it now.
    let org = org();
    let (a, b, _c, id) = planned(&org);
    let priya = b.ctx("priya", APP);
    let (state_b, _) = opened(&b, &priya, &id);
    a.sync();
    a.call("dana", APP, ROLES, json!({ "doc": id, "editors": [] }))
        .unwrap();
    b.docs
        .push(&priya, &id, "", &typed(&state_b, "Priya, too late."))
        .unwrap();
    b.docs.pull(&priya, &id, "", 0).unwrap();
    b.sync();
    a.sync();
    assert!(!a.markdown("dana", &id).contains("Priya, too late."));
    let md = b.markdown("priya", &id);
    assert!(!md.contains("Priya, too late."), "{md}");
    let again = b.docs.push(&priya, &id, "", &typed(&state_b, "Again."));
    assert!(
        again.unwrap_err().to_string().contains("not edit"),
        "she views it now"
    );
}

#[test]
fn adr0034_a_doc_is_labeled_and_takes_nothing_from_a_task_that_read_more() {
    let org = org();
    let (a, b, _c, id) = planned(&org);
    let preview = b
        .docs
        .preview(
            &b.ctx("priya", AGENT),
            &READ.parse().unwrap(),
            &json!({ "doc": id }),
        )
        .unwrap();
    assert_eq!(preview.label, Some(Label::Internal));
    // A read is judged by its doc's label, so one whose doc can't be found
    // is refused where it is judged. It used to be "a doc" with no label,
    // which passes: the read then ran with nothing having been checked,
    // if it found the doc after all.
    for action in [READ, OPEN] {
        let missing = b.docs.preview(
            &b.ctx("priya", AGENT),
            &action.parse().unwrap(),
            &json!({ "doc": "0000000000000000" }),
        );
        assert!(
            matches!(
                missing,
                Err(ActionError::NotFound(_) | ActionError::InvalidInput(_))
            ),
            "{action}: {missing:?}"
        );
    }
    let mut ctx = b.ctx("priya", AGENT);
    ctx.data_label = Some(Label::Confidential);
    let refused = b.with(
        &ctx,
        WRITE,
        json!({ "doc": id, "markdown": "board numbers" }),
    );
    assert!(
        matches!(refused, Err(ActionError::OutOfScope(_))),
        "{refused:?}"
    );
    // A doc made by such a task is confidential.
    let mut ctx = a.ctx("dana", AGENT);
    ctx.data_label = Some(Label::Confidential);
    let made = a
        .with(&ctx, CREATE, json!({ "title": "Board", "with": ["priya"] }))
        .unwrap();
    assert_eq!(made.data["label"], "confidential");
}

#[test]
fn dc6_what_an_agent_sends_as_markdown_has_a_depth_and_a_size() {
    let org = org();
    let (a, _b, _c, id) = planned(&org);
    let ctx = a.ctx("dana", AGENT);
    // Nested far past what a 2 MiB stack took: the preview, which runs
    // before anyone is asked, reads it as one block.
    let nested = format!("{}x", "> ".repeat(50_000));
    let preview = a
        .docs
        .preview(
            &ctx,
            &CREATE.parse().unwrap(),
            &json!({ "title": "Deep", "markdown": nested }),
        )
        .unwrap();
    assert!(
        preview.items.iter().any(|i| i == "Starts with 1 block"),
        "{:?}",
        preview.items
    );
    a.with(&ctx, WRITE, json!({ "doc": id, "markdown": nested }))
        .unwrap();
    assert!(a.markdown("dana", &id).contains('x'));
    // More than one call writes is refused before it is read, in the
    // preview and in the call.
    let long = "word ".repeat(MAX_MARKDOWN / 5 + 1);
    for (action, input) in [
        (CREATE, json!({ "title": "Long", "markdown": long })),
        (WRITE, json!({ "doc": id, "markdown": long })),
    ] {
        let previewed = a.docs.preview(&ctx, &action.parse().unwrap(), &input);
        assert!(
            matches!(&previewed, Err(ActionError::InvalidInput(why)) if why.contains("in parts")),
            "{action}: {previewed:?}"
        );
        let called = a.with(&ctx, action, input);
        assert!(
            matches!(&called, Err(ActionError::InvalidInput(_))),
            "{action}: {called:?}"
        );
    }
}

#[test]
fn dc_others_see_nothing_until_the_person_says_yes() {
    let org = org();
    let a = org.computer("dev-a");
    let tiers: BTreeMap<String, (Tier, SideEffect)> = a
        .docs
        .actions()
        .into_iter()
        .map(|d| (d.name.to_string(), (d.default_tier, d.side_effect)))
        .collect();
    for action in [LIST, READ, OPEN] {
        assert_eq!(tiers[action], (Tier::Auto, SideEffect::None), "{action}");
    }
    // Until undo can revert an edit that reached others (ADR 0035 §6).
    for action in [CREATE, WRITE, COMMENT, RESOLVE, VERSION, ROLES] {
        assert_eq!(
            tiers[action],
            (Tier::Ask, SideEffect::Irreversible),
            "{action}"
        );
    }
}

#[test]
fn ga1_what_each_action_returns_is_what_it_declares() {
    let org = org();
    let (a, b, _c, id) = planned(&org);
    let check = |action: &str, out: Result<ActionOutput, ActionError>| {
        let mut data = out.unwrap_or_else(|e| panic!("{action}: {e}")).data;
        // What the runtime keeps for the record never reaches the caller.
        if let Some(fields) = data.as_object_mut() {
            fields.remove(glass_actions::AUDIT_DETAIL);
        }
        let shape = a.docs.returns(&action.parse().unwrap()).unwrap();
        glass_actions::shape::conforms(&data, &shape).unwrap_or_else(|e| panic!("{action}: {e}"));
        data
    };
    check(
        WRITE,
        b.call(
            "priya",
            AGENT,
            WRITE,
            json!({ "doc": id, "markdown": "- Supplier delay", "at": { "after": "Risks" } }),
        ),
    );
    // A comment anchored in the text, and a reply to it.
    let commented = check(
        COMMENT,
        b.call(
            "priya",
            AGENT,
            COMMENT,
            json!({ "doc": id, "text": "Which supplier?", "anchor": { "path": base64::encode(b"pos") } }),
        ),
    );
    a.sync();
    check(
        COMMENT,
        a.call(
            "dana",
            AGENT,
            COMMENT,
            json!({ "doc": id, "text": "Northwind's", "reply_to": commented["id"] }),
        ),
    );
    check(
        VERSION,
        a.call("dana", APP, VERSION, json!({ "doc": id, "name": "Draft" })),
    );
    check(
        RESOLVE,
        a.call(
            "dana",
            AGENT,
            RESOLVE,
            json!({ "doc": id, "comment": commented["id"] }),
        ),
    );
    check(
        ROLES,
        a.call(
            "dana",
            APP,
            ROLES,
            json!({ "doc": id, "editors": ["priya"], "commenters": ["sam"] }),
        ),
    );
    a.sync();
    let listed = check(LIST, a.call("dana", AGENT, LIST, json!({})));
    assert!(!listed["docs"].as_array().unwrap().is_empty());
    let read = check(READ, a.call("dana", AGENT, READ, json!({ "doc": id })));
    assert_eq!(read["comments"].as_array().unwrap().len(), 2);
    let opened = check(OPEN, a.call("dana", APP, OPEN, json!({ "doc": id })));
    assert_eq!(opened["versions"][0]["name"], "Draft");
    check(
        CREATE,
        a.call(
            "dana",
            AGENT,
            CREATE,
            json!({ "title": "Notes", "commenters": ["sam"] }),
        ),
    );
    // Every action the provider declares has a shape.
    for def in a.docs.actions() {
        assert!(a.docs.returns(&def.name).is_some(), "{}", def.name);
    }
}

/// DC-1 speed (ADR 0052): a doc with 10,000 updates, a snapshot every
/// 200 as Docs posts them. By hand:
/// `cargo test -p glass-app-docs --profile vm -- --ignored --nocapture dc1_`.
#[test]
#[ignore = "a measurement, run by hand"]
fn dc1_opening_a_doc_with_10000_updates() {
    const UPDATES: usize = 10_000;
    let org = org();
    let (a, b, _c, id) = planned(&org);
    a.sync();
    let dana = a.ctx("dana", APP);
    let (state, _) = opened(&a, &dana, &id);
    // The page's document, typed into a line at a time.
    let page = ydoc::doc_from(&[base64::decode(&state).unwrap()]).unwrap();
    let fragment = page.get_or_insert_xml_fragment(ydoc::FRAGMENT);
    let started = Instant::now();
    let mut upto = 0;
    for n in 0..UPDATES {
        let before = page.transact().state_vector();
        {
            let mut txn = page.transact_mut();
            let len = fragment.len(&txn);
            let p = fragment.insert(&mut txn, len, yrs::XmlElementPrelim::empty("paragraph"));
            let t = p.push_back(&mut txn, XmlTextPrelim::new(""));
            t.insert(&mut txn, 0, &format!("Line {n} of the plan."));
        }
        let update = page.transact().encode_state_as_update_v1(&before);
        a.roster
            .post(
                "dana",
                &id,
                "",
                Some(Part::Update {
                    data: base64::encode(&update),
                }),
                None,
                None,
            )
            .unwrap();
        if (n + 1) % SNAPSHOT_EVERY == 0 {
            upto = a
                .roster
                .since("dana", &id, upto)
                .unwrap()
                .messages
                .last()
                .unwrap()
                .seq;
            let state = ydoc::state(&page);
            let (blob, key, sha256, bytes) = a.docs.blob("dana", &id, &state).unwrap();
            a.roster
                .post(
                    "dana",
                    &id,
                    "",
                    Some(Part::Snapshot {
                        blob,
                        key,
                        sha256,
                        bytes,
                        upto,
                        name: None,
                    }),
                    None,
                    None,
                )
                .unwrap();
        }
    }
    a.sync();
    b.sync();
    eprintln!("posted {UPDATES} updates in {:?}", started.elapsed());
    let time = |what: &str, f: &mut dyn FnMut()| {
        let mut runs: Vec<std::time::Duration> = (0..5)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .collect();
        runs.sort();
        eprintln!("{what}: median {:?} (fastest {:?})", runs[2], runs[0]);
    };
    let priya = b.ctx("priya", APP);
    time(
        "doc.open, first on this computer (from the log)",
        &mut || {
            let fresh = DocsProvider::new(b.roster.clone(), org.directory.clone());
            let out = fresh
                .invoke(&priya, &OPEN.parse().unwrap(), &json!({ "doc": id }))
                .unwrap();
            assert!(out.data["state"].as_str().unwrap().len() > 100_000);
        },
    );
    let out = b.with(&priya, OPEN, json!({ "doc": id })).unwrap();
    let cursor = out.data["cursor"].as_u64().unwrap();
    time("doc.open again (kept in memory)", &mut || {
        b.with(&priya, OPEN, json!({ "doc": id })).unwrap();
    });
    time("a pull with nothing new", &mut || {
        b.docs.pull(&priya, &id, "", cursor).unwrap();
    });
    let state = out.data["state"].as_str().unwrap().to_owned();
    let mut n = 0;
    time("a push", &mut || {
        n += 1;
        b.docs
            .push(&priya, &id, "", &typed(&state, &format!("Priya {n}")))
            .unwrap();
    });
    time("doc.read (Markdown)", &mut || {
        b.call("priya", AGENT, READ, json!({ "doc": id })).unwrap();
    });
    time("doc.list (this doc and nothing else)", &mut || {
        let out = b.with(&priya, LIST, json!({})).unwrap();
        assert_eq!(out.data["docs"].as_array().unwrap().len(), 1);
    });
}

#[test]
fn dc3_the_page_knows_the_words_a_closed_session_is_told() {
    // The Docs page tells "not open, open it again" from a refusal by the
    // wording (apps/docs/src/live.ts, failureKind). Reworded here so that
    // it no longer matches, the page would stop sending a doc's edits
    // where it should open the doc again; its unit test pinned words
    // glassd doesn't say.
    let page = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/docs/src/live.ts"),
    )
    .unwrap();
    let line = page
        .lines()
        .find(|l| l.contains("return \"not-open\"") && l.contains(".test(message)"))
        .expect("failureKind's line for a session that isn't open");
    let pattern = line
        .split_once("(/")
        .unwrap()
        .1
        .split_once("/i.test")
        .unwrap()
        .0;
    let words: Vec<String> = pattern
        .split('|')
        .map(|w| w.trim_start_matches("\\b").to_lowercase())
        .collect();
    assert!(words.len() > 2, "{words:?}");
    let said = crate::NOT_OPEN.to_lowercase();
    assert!(
        words.iter().any(|w| said.contains(w.as_str())),
        "the page looks for one of {words:?}; glassd says {said:?}"
    );
    // And the page's own test pins what glassd says, word for word.
    let pinned = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/docs/test/live.test.ts"),
    )
    .unwrap();
    assert!(
        pinned.contains(crate::NOT_OPEN),
        "live.test.ts no longer pins {:?}",
        crate::NOT_OPEN
    );
}

/// The shared space, with posts to one item held until the test lets
/// them go: a console that takes its time over one doc.
#[derive(Debug)]
struct Slow {
    inner: Arc<dyn Shared>,
    held: Mutex<Option<String>>,
    let_go: (Mutex<bool>, std::sync::Condvar),
    arrived: std::sync::atomic::AtomicUsize,
}

impl Slow {
    fn hold(&self, item: &str) {
        *self.held.lock().unwrap() = Some(item.to_owned());
    }

    fn release(&self) {
        *self.let_go.0.lock().unwrap() = true;
        self.let_go.1.notify_all();
    }

    fn arrived(&self) -> usize {
        self.arrived.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Shared for Slow {
    fn ready(&self, login: &str, make: bool) -> Result<(), glass_managed::chat::ChatFailure> {
        self.inner.ready(login, make)
    }
    fn items(&self, login: &str, kind: ConversationKind) -> Vec<Item> {
        self.inner.items(login, kind)
    }
    fn item_ids(&self, login: &str, kind: ConversationKind) -> Vec<String> {
        self.inner.item_ids(login, kind)
    }
    fn item(&self, login: &str, id: &str) -> Option<Item> {
        self.inner.item(login, id)
    }
    fn since(&self, login: &str, id: &str, seq: u64) -> Option<Item> {
        self.inner.since(login, id, seq)
    }
    fn through(&self, login: &str) -> u64 {
        self.inner.through(login)
    }
    fn create(
        &self,
        login: &str,
        kind: ConversationKind,
        title: &str,
        label: Label,
        members: &[String],
    ) -> Result<String, glass_managed::chat::ChatFailure> {
        self.inner.create(login, kind, title, label, members)
    }
    fn post(
        &self,
        login: &str,
        to: &str,
        text: &str,
        part: Option<Part>,
        attribution: Option<String>,
        reply_to: Option<String>,
    ) -> Result<String, glass_managed::chat::ChatFailure> {
        if self.held.lock().unwrap().as_deref() == Some(to) {
            self.arrived
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut gone = self.let_go.0.lock().unwrap();
            while !*gone {
                gone = self.let_go.1.wait(gone).unwrap();
            }
        }
        self.inner
            .post(login, to, text, part, attribution, reply_to)
    }
    fn put_blob(
        &self,
        login: &str,
        blob: &glass_managed::shared::SealedBlob,
    ) -> Result<(), glass_managed::chat::ChatFailure> {
        self.inner.put_blob(login, blob)
    }
    fn get_blob(
        &self,
        login: &str,
        item: &str,
        id: &str,
    ) -> Result<glass_managed::shared::SealedBlob, glass_managed::chat::ChatFailure> {
        self.inner.get_blob(login, item, id)
    }
}

/// The shared space, counting the messages it opened for whoever asked.
#[derive(Debug)]
struct Counted {
    inner: Arc<dyn Shared>,
    opened: std::sync::atomic::AtomicUsize,
}

impl Counted {
    fn count(&self, messages: usize) {
        self.opened
            .fetch_add(messages, std::sync::atomic::Ordering::SeqCst);
    }

    fn opened(&self) -> usize {
        self.opened.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Shared for Counted {
    fn ready(&self, login: &str, make: bool) -> Result<(), glass_managed::chat::ChatFailure> {
        self.inner.ready(login, make)
    }
    fn items(&self, login: &str, kind: ConversationKind) -> Vec<Item> {
        let items = self.inner.items(login, kind);
        self.count(items.iter().map(|i| i.messages.len()).sum());
        items
    }
    fn item_ids(&self, login: &str, kind: ConversationKind) -> Vec<String> {
        self.inner.item_ids(login, kind)
    }
    fn item(&self, login: &str, id: &str) -> Option<Item> {
        let item = self.inner.item(login, id);
        self.count(item.as_ref().map_or(0, |i| i.messages.len()));
        item
    }
    fn since(&self, login: &str, id: &str, seq: u64) -> Option<Item> {
        let item = self.inner.since(login, id, seq);
        self.count(item.as_ref().map_or(0, |i| i.messages.len()));
        item
    }
    fn through(&self, login: &str) -> u64 {
        self.inner.through(login)
    }
    fn create(
        &self,
        login: &str,
        kind: ConversationKind,
        title: &str,
        label: Label,
        members: &[String],
    ) -> Result<String, glass_managed::chat::ChatFailure> {
        self.inner.create(login, kind, title, label, members)
    }
    fn post(
        &self,
        login: &str,
        to: &str,
        text: &str,
        part: Option<Part>,
        attribution: Option<String>,
        reply_to: Option<String>,
    ) -> Result<String, glass_managed::chat::ChatFailure> {
        self.inner
            .post(login, to, text, part, attribution, reply_to)
    }
    fn put_blob(
        &self,
        login: &str,
        blob: &glass_managed::shared::SealedBlob,
    ) -> Result<(), glass_managed::chat::ChatFailure> {
        self.inner.put_blob(login, blob)
    }
    fn get_blob(
        &self,
        login: &str,
        item: &str,
        id: &str,
    ) -> Result<glass_managed::shared::SealedBlob, glass_managed::chat::ChatFailure> {
        self.inner.get_blob(login, item, id)
    }
}

// Audit 9.3, the rest: the list opened every message of every doc (every
// edit ever made) to show when each last changed and who may do what.
#[test]
fn dc1_the_list_opens_a_docs_messages_once_and_then_only_what_is_new() {
    let org = org();
    let (a, mut b, _c, id) = planned(&org);
    let counted = Arc::new(Counted {
        inner: b.roster.clone(),
        opened: std::sync::atomic::AtomicUsize::new(0),
    });
    b.docs = DocsProvider::new(counted.clone(), org.directory.clone());
    let all = |c: &Computer| c.roster.item("priya", &id).unwrap().messages.len();
    let listed = |c: &Computer| {
        let out = c.call("priya", APP, LIST, json!({})).unwrap();
        assert_eq!(out.data["docs"].as_array().unwrap().len(), 1);
        out.data["docs"][0].clone()
    };

    // The first list reads the doc's messages; the next, none.
    let first = listed(&b);
    assert_eq!(first["title"], "Q3 plan");
    assert_eq!(first["role"], "editor");
    assert_eq!(first["owner"], "dana");
    assert!(first["updated_at"].is_string(), "{first}");
    let mut read = counted.opened();
    assert_eq!(read, all(&b));
    assert_eq!(listed(&b), first);
    assert_eq!(counted.opened(), read, "nothing opened again");

    // Dana types. The list opens what arrived, and nothing from before.
    let dana = a.ctx("dana", APP);
    let (state, _) = opened(&a, &dana, &id);
    a.docs
        .push(&dana, &id, "", &typed(&state, "One more thing."))
        .unwrap();
    b.sync();
    let before = read;
    listed(&b);
    read = counted.opened();
    assert!(read > before, "the new edit was looked at");
    assert_eq!(read, all(&b), "each message once");

    // The owner makes Priya a commenter: the list says so from that one
    // message.
    a.call(
        "dana",
        APP,
        ROLES,
        json!({ "doc": id, "editors": [], "commenters": ["priya"] }),
    )
    .unwrap();
    b.sync();
    let after = listed(&b);
    assert_eq!(after["role"], "commenter");
    assert_eq!(counted.opened(), read + 1);
    read += 1;

    // Roles from someone who isn't the owner count for nothing, here as
    // in the doc itself.
    b.roster
        .post(
            "priya",
            &id,
            "",
            Some(Part::Roles {
                editors: vec!["priya".into()],
                commenters: Vec::new(),
            }),
            None,
            None,
        )
        .unwrap();
    assert_eq!(listed(&b)["role"], "commenter");
    assert_eq!(counted.opened(), read + 1);
    // Her own post is ahead of what this computer has been sent up to
    // (another computer's earlier message could still arrive before it),
    // so it is looked at again until a sync is past it. Then no more.
    b.sync();
    listed(&b);
    let settled = counted.opened();
    assert_eq!(listed(&b)["role"], "commenter");
    assert_eq!(counted.opened(), settled);

    // As after glassd starts again: everything is read once more, and
    // the list is the same.
    let fresh = DocsProvider::new(b.roster.clone(), org.directory.clone());
    let again = fresh
        .invoke(&b.ctx("priya", APP), &LIST.parse().unwrap(), &json!({}))
        .unwrap();
    assert_eq!(again.data["docs"][0], listed(&b));
}

// Audit 9.3: one lock covered every doc, person and session, and a write
// posted to the console inside it, so a slow console stalled every doc
// operation on the computer.
#[test]
fn dc3_one_docs_slow_post_doesnt_hold_up_another_doc() {
    let org = org();
    let mut a = org.computer("dev-a");
    let slow = Arc::new(Slow {
        inner: a.roster.clone(),
        held: Mutex::new(None),
        let_go: (Mutex::new(false), std::sync::Condvar::new()),
        arrived: std::sync::atomic::AtomicUsize::new(0),
    });
    a.docs = DocsProvider::new(slow.clone(), org.directory.clone());
    a.call("dana", APP, LIST, json!({})).unwrap();
    let make = |title: &str| {
        let made = a
            .call(
                "dana",
                AGENT,
                CREATE,
                json!({ "title": title, "markdown": "# Start\n" }),
            )
            .unwrap();
        made.data["id"].as_str().unwrap().to_owned()
    };
    let (plan, notes) = (make("Q3 plan"), make("Notes"));

    // A write to the plan is on its way to a console that doesn't answer.
    slow.hold(&plan);
    std::thread::scope(|scope| {
        let stuck = scope.spawn(|| {
            a.call(
                "dana",
                AGENT,
                WRITE,
                json!({ "doc": plan, "markdown": "Ship by Friday.", "at": "end" }),
            )
        });
        while slow.arrived() == 0 {
            std::thread::yield_now();
        }

        // Meanwhile the other doc is read and written, and the list of
        // docs is given: none of it waits for the plan's post.
        let (done, finished) = std::sync::mpsc::channel();
        let (a, notes) = (&a, &notes);
        scope.spawn(move || {
            let wrote = a.call(
                "dana",
                AGENT,
                WRITE,
                json!({ "doc": notes, "markdown": "Call Priya.", "at": "end" }),
            );
            let read = a.call("dana", AGENT, READ, json!({ "doc": notes }));
            let listed = a.call("dana", AGENT, LIST, json!({}));
            let _ = done.send((wrote.is_ok(), read, listed.is_ok()));
        });
        let other = finished.recv_timeout(std::time::Duration::from_secs(20));
        // Let go whatever came of it, or the scope would wait for good.
        let still_stuck = !stuck.is_finished();
        slow.release();
        let (wrote, read, listed) =
            other.expect("another doc waited for the plan's post to the console");
        assert!(wrote && listed);
        assert!(read.unwrap().text.contains("Call Priya."));
        assert!(still_stuck, "the plan's write was held all the while");

        // Let go, the plan's write lands as it was.
        stuck.join().unwrap().unwrap();
    });
    assert!(a.markdown("dana", &plan).contains("Ship by Friday."));
    assert!(a.markdown("dana", &notes).contains("Call Priya."));
}
