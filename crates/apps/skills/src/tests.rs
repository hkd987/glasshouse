//! Skills between computers through an in-memory console (ADR 0034):
//! drafts and eval results stay on a computer, proposals and versions go
//! through the shared space.

use std::path::PathBuf;

use glass_app_chat::{ChatHandle, ChatProvider};
use glass_crypto::SecretKey;
use glass_managed::chat::testing::MemoryLink;
use glass_managed::directory::Applied;
use glass_managed::{ChatRelease, DirectoryRelease, Person};

use super::*;

const AGENT: &str = "claude-code";

/// A skill whose one case wants "30 days"; it says 14.
fn files(policy: &str) -> Value {
    let evals = json!({
        "runs": 3,
        "cases": [
            { "id": "window", "prompt": "How long do refunds take?", "graders": [{ "type": "contains", "text": "30 days" }] },
            { "id": "tone", "prompt": "A customer is angry about a refund.", "graders": [{ "type": "not_contains", "text": "calm down" }] }
        ]
    });
    json!({
        "SKILL.md": format!("---\nname: Refunds\ndescription: Answers questions about refunds\n---\n{policy}\n"),
        "evals.json": evals.to_string(),
    })
}

struct Org {
    dir: tempfile::TempDir,
    chat: Arc<MemoryLink>,
    it: SecretKey,
    directory: DirectoryHandle,
}

struct Computer {
    chat: ChatHandle,
    skills: SkillsProvider,
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
        let roster = ChatProvider::new(chat.clone(), self.directory.clone());
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        Computer {
            skills: SkillsProvider::new(
                Arc::new(roster),
                self.directory.clone(),
                dir.join("skills"),
            ),
            chat,
            home,
        }
    }

    /// Dana's and Priya's computers, each having opened Skills once.
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
        self.with(&self.ctx(login, agent), action, input)
    }

    fn with(
        &self,
        ctx: &TaskContext,
        action: &str,
        input: Value,
    ) -> Result<ActionOutput, ActionError> {
        self.skills.invoke(ctx, &action.parse().unwrap(), &input)
    }

    fn sync(&self) {
        self.chat.sync_once(0).unwrap();
    }

    /// The skill-evals agent's report: in one run (`eval`), each target
    /// with how many of 3 runs each case passed.
    fn report(&self, eval: &TaskContext, target: Value, cases: &[(&str, u32)]) {
        let mut input = json!({
            "model": "claude-test",
            "runner": "skill-evals 1",
            "cases": cases.iter().map(|(id, passed)| json!({ "id": id, "passed": passed, "runs": 3 })).collect::<Vec<_>>(),
        });
        for (k, v) in target.as_object().unwrap() {
            input[k] = v.clone();
        }
        self.with(eval, RESULTS, input).unwrap();
    }
}

/// Dana drafts the skill, runs its evals (one case fails) and shares it
/// with Priya; Priya's computer has synced.
fn shared(org: &Org) -> (Computer, Computer, String) {
    let (a, b) = org.pair();
    let draft = a
        .call(
            "dana",
            AGENT,
            DRAFT,
            json!({ "files": files("Refunds take 14 days.") }),
        )
        .unwrap();
    let draft = draft.data["draft"].as_str().unwrap().to_owned();
    let eval = a.ctx("dana", EVALS_AGENT);
    a.report(
        &eval,
        json!({ "draft": draft }),
        &[("window", 0), ("tone", 3)],
    );
    let out = a
        .call(
            "dana",
            AGENT,
            SHARE,
            json!({ "draft": draft, "with": ["priya"], "note": "First version" }),
        )
        .unwrap();
    assert_eq!(
        out.text,
        "Shared Refunds with Priya Shah, scoring 1 of 2 cases (50%)."
    );
    let id = out.data["id"].as_str().unwrap().to_owned();
    b.sync();
    (a, b, id)
}

#[test]
fn sk1_a_skill_is_shared_with_its_evals_and_its_score() {
    let org = org();
    let (_a, b, id) = shared(&org);
    let list = b.call("priya", APP, LIST, json!({})).unwrap();
    let s = &list.data["skills"][0];
    assert_eq!(s["id"], id);
    assert_eq!(s["name"], "Refunds");
    assert_eq!(s["description"], "Answers questions about refunds");
    assert_eq!(s["owner_name"], "Dana Ruiz");
    assert_eq!(s["version"], 1);
    assert_eq!(s["score"]["summary"], "1 of 2 cases (50%)");
    assert_eq!(s["label"], "internal");
    let read = b
        .call("priya", AGENT, READ, json!({ "skill": id }))
        .unwrap();
    assert!(
        read.data["files"]["SKILL.md"]
            .as_str()
            .unwrap()
            .contains("14 days")
    );
    assert_eq!(read.data["versions"][0]["scores"]["by"], "dana");
}

#[test]
fn sk3_a_change_that_fixes_a_case_is_proposed_with_both_scores_and_merged() {
    let org = org();
    let (a, b, id) = shared(&org);

    // Priya drafts the fix and runs the evals on v1 and her draft together.
    let draft = b
        .call(
            "priya",
            AGENT,
            DRAFT,
            json!({ "skill": id, "files": files("Refunds take 30 days.") }),
        )
        .unwrap()
        .data["draft"]
        .as_str()
        .unwrap()
        .to_owned();
    // Without scores from one run, there's nothing to compare.
    let early = b.call(
        "priya",
        AGENT,
        PROPOSE,
        json!({ "skill": id, "draft": draft, "note": "It's 30 days" }),
    );
    let Err(ActionError::InvalidInput(why)) = early else {
        panic!("{early:?}")
    };
    assert!(why.contains(&format!("\"{id} draft {draft}\"")), "{why}");

    let eval = b.ctx("priya", EVALS_AGENT);
    b.report(
        &eval,
        json!({ "skill": id, "version": 1 }),
        &[("window", 0), ("tone", 3)],
    );
    b.report(
        &eval,
        json!({ "draft": draft }),
        &[("window", 3), ("tone", 3)],
    );
    let preview = b
        .skills
        .preview(
            &b.ctx("priya", AGENT),
            &PROPOSE.parse().unwrap(),
            &json!({ "skill": id, "draft": draft, "note": "It's 30 days" }),
        )
        .unwrap();
    assert!(
        preview
            .items
            .iter()
            .any(|i| i == "Scores: 1 of 2 cases (50%) → 2 of 2 cases (100%)"),
        "{:?}",
        preview.items
    );
    let proposed = b
        .call(
            "priya",
            AGENT,
            PROPOSE,
            json!({ "skill": id, "draft": draft, "note": "It's 30 days" }),
        )
        .unwrap();
    assert_eq!(
        proposed.text,
        "Proposed a change to Refunds: 1 of 2 cases (50%) → 2 of 2 cases (100%)."
    );
    let proposal = proposed.data["id"].as_str().unwrap().to_owned();

    // Only Dana merges.
    let not_hers = b.call(
        "priya",
        AGENT,
        MERGE,
        json!({ "skill": id, "proposal": proposal }),
    );
    assert!(
        matches!(not_hers, Err(ActionError::InvalidInput(_))),
        "{not_hers:?}"
    );
    a.sync();
    let read = a
        .call(
            "dana",
            AGENT,
            READ,
            json!({ "skill": id, "proposal": proposal }),
        )
        .unwrap();
    assert!(
        read.data["files"]["SKILL.md"]
            .as_str()
            .unwrap()
            .contains("30 days")
    );
    let p = &read.data["proposals"][0];
    assert_eq!(p["by_name"], "Priya Shah");
    assert_eq!(p["attribution"], "by claude-code for priya");
    assert_eq!(p["breaks"].as_array().unwrap().len(), 0);
    let merged = a
        .call(
            "dana",
            APP,
            MERGE,
            json!({ "skill": id, "proposal": proposal }),
        )
        .unwrap();
    assert_eq!(merged.data["version"], 2);

    // Everyone gets v2, scoring 2 of 2; it credits Priya.
    b.sync();
    let list = b.call("priya", APP, LIST, json!({})).unwrap();
    let s = &list.data["skills"][0];
    assert_eq!(s["version"], 2);
    assert_eq!(s["score"]["summary"], "2 of 2 cases (100%)");
    assert_eq!(s["contributors"], 1);
    assert_eq!(s["open_proposals"], 0);
    let v2 = b
        .call("priya", AGENT, READ, json!({ "skill": id }))
        .unwrap();
    assert!(
        v2.data["files"]["SKILL.md"]
            .as_str()
            .unwrap()
            .contains("30 days")
    );
    assert_eq!(v2.data["versions"][1]["merged"], proposal);
    // Merged once only.
    let again = a.call(
        "dana",
        APP,
        MERGE,
        json!({ "skill": id, "proposal": proposal }),
    );
    assert!(again.is_err());
}

#[test]
fn sk3_a_change_that_breaks_a_case_merges_only_with_a_reason() {
    let org = org();
    let (a, b, id) = shared(&org);
    let draft = b
        .call("priya", AGENT, DRAFT, json!({ "skill": id, "files": files("Refunds take 30 days. Tell angry customers to calm down.") }))
        .unwrap()
        .data["draft"]
        .as_str()
        .unwrap()
        .to_owned();
    let eval = b.ctx("priya", EVALS_AGENT);
    b.report(&eval, json!({ "skill": id }), &[("window", 0), ("tone", 3)]);
    b.report(
        &eval,
        json!({ "draft": draft }),
        &[("window", 3), ("tone", 1)],
    );
    let proposed = b
        .call(
            "priya",
            AGENT,
            PROPOSE,
            json!({ "skill": id, "draft": draft, "note": "30 days, firmer tone" }),
        )
        .unwrap();
    assert!(
        proposed.text.ends_with("; it breaks tone."),
        "{}",
        proposed.text
    );
    let proposal = proposed.data["id"].as_str().unwrap().to_owned();
    a.sync();
    let bare = a.call(
        "dana",
        APP,
        MERGE,
        json!({ "skill": id, "proposal": proposal }),
    );
    let Err(ActionError::InvalidInput(why)) = bare else {
        panic!("{bare:?}")
    };
    assert!(why.contains("breaks tone") && why.contains("SK-3"), "{why}");
    a.call(
        "dana",
        APP,
        MERGE,
        json!({ "skill": id, "proposal": proposal, "reason": "The tone case is out of date; fixing it next" }),
    )
    .unwrap();
    let read = a.call("dana", AGENT, READ, json!({ "skill": id })).unwrap();
    assert!(
        read.data["versions"][1]["note"]
            .as_str()
            .unwrap()
            .contains("reason: The tone case")
    );
}

#[test]
fn sk5_only_the_eval_agent_reports_results_and_runs_compare_only_with_themselves() {
    let org = org();
    let (_a, b, id) = shared(&org);
    let draft = b
        .call(
            "priya",
            AGENT,
            DRAFT,
            json!({ "skill": id, "files": files("Refunds take 30 days.") }),
        )
        .unwrap()
        .data["draft"]
        .as_str()
        .unwrap()
        .to_owned();
    // An agent can't claim scores.
    let claimed = b.call(
        "priya",
        AGENT,
        RESULTS,
        json!({ "draft": draft, "model": "m", "runner": "me", "cases": [{ "id": "window", "passed": 3, "runs": 3 }] }),
    );
    assert!(
        matches!(claimed, Err(ActionError::OutOfScope(_))),
        "{claimed:?}"
    );
    // Two separate runs don't make a comparison.
    b.report(
        &b.ctx("priya", EVALS_AGENT),
        json!({ "skill": id }),
        &[("window", 0), ("tone", 3)],
    );
    b.report(
        &b.ctx("priya", EVALS_AGENT),
        json!({ "draft": draft }),
        &[("window", 3), ("tone", 3)],
    );
    let propose = b.call(
        "priya",
        AGENT,
        PROPOSE,
        json!({ "skill": id, "draft": draft, "note": "30 days" }),
    );
    assert!(
        matches!(propose, Err(ActionError::InvalidInput(_))),
        "{propose:?}"
    );
    // Drafts and results stay on this computer, root's only.
    let path = org.dir.path().join("dev-b/skills/priya.json");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn sk1_what_a_skill_holds_is_checked_and_stays_in_the_org() {
    let org = org();
    let (a, _b) = org.pair();
    let mut escape = files("x");
    escape["../../etc/cron.d/x"] = json!("* * * * * root sh");
    let mut no_evals = files("x");
    no_evals.as_object_mut().unwrap().remove("evals.json");
    for bad in [
        json!({ "files": escape }),
        json!({ "files": no_evals }),
        json!({ "files": { "SKILL.md": "no front matter", "evals.json": "{}" } }),
        json!({ "files": files("x"), "with": ["mallory@evil.example"] }),
        json!({}),
    ] {
        let share = a.call("dana", AGENT, SHARE, bad.clone());
        assert!(
            matches!(share, Err(ActionError::InvalidInput(_))),
            "{bad}: {share:?}"
        );
    }
    // Shared straight from files, it has no score yet.
    let out = a
        .call("dana", AGENT, SHARE, json!({ "files": files("x") }))
        .unwrap();
    assert_eq!(out.text, "Shared Refunds.");
    assert!(out.data["scores"].is_null());
}

#[test]
fn sk_others_see_nothing_until_the_person_says_yes() {
    let org = org();
    let a = org.computer("dev-a");
    let tiers: BTreeMap<String, (Tier, SideEffect)> = a
        .skills
        .actions()
        .into_iter()
        .map(|d| (d.name.to_string(), (d.default_tier, d.side_effect)))
        .collect();
    assert_eq!(tiers[LIST], (Tier::Auto, SideEffect::None));
    assert_eq!(tiers[READ], (Tier::Auto, SideEffect::None));
    assert_eq!(tiers[DRAFT], (Tier::Auto, SideEffect::LocalReversible));
    assert_eq!(tiers[RESULTS], (Tier::Auto, SideEffect::LocalReversible));
    for action in [SHARE, PROPOSE, MERGE] {
        assert_eq!(
            tiers[action],
            (Tier::Ask, SideEffect::Irreversible),
            "{action}"
        );
    }
}
