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
