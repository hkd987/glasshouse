//! Agent computers through the runtime (ADR 0065, ADR 0066): an agent
//! keeps its computer between tasks, its person watches the screen and
//! takes over the keyboard, and all of it is on the record.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use glass_api::{ComputerNew, ComputerState, PersonaNew, ScreenInput, ScreenRequest, TileState};
use glass_computers::namespaces::NamespaceHypervisor;
use glass_computers::sandbox::ComputerSandbox;
use glass_computers::store::{FolderBlobs, Store};
use glass_runtime::{CatalogEntry, RuntimeError};

use crate::common::{self, Harness, eventually, harness};

/// An agent that keeps what it found on its computer, and finds it again:
/// the invoice demo's id, which the default policy lets people run.
fn reconciler() -> CatalogEntry {
    CatalogEntry {
        id: "invoice-demo".parse().unwrap(),
        name: "Reconciler".into(),
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "test -S \"$GLASS_AGENT_SOCKET\" || exit 3
             case \"$GLASS_TASK_PROMPT\" in
               Match*) echo 'Matched 207 of 214 invoices'; echo 7 > /work/unmatched ;;
               *) echo \"Still $(cat /work/unmatched) unmatched\" ;;
             esac"
                .into(),
        ],
        plan: vec!["Match invoices".into()],
        ..common::repeater()
    }
}

fn on_computers(h: &Harness) -> Option<Arc<glass_runtime::Runtime>> {
    if let Err(why) = NamespaceHypervisor::available() {
        eprintln!("skipped: {why}");
        return None;
    }
    let dir = h.dir.path().join("computers");
    // Its owner's runs pass through to their own folder.
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(h.dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let computers = Arc::new(
        ComputerSandbox::new(dir, Arc::new(NamespaceHypervisor::new(Vec::new())), 1024, 1).unwrap(),
    );
    Some(common::rebuild_on(
        h,
        glass_policy::DEFAULT_POLICY,
        vec![reconciler()],
        computers.clone(),
        Some(computers),
    ))
}

/// As [`on_computers`], with a store for computers to sleep in.
fn on_computers_with_store(h: &Harness) -> Option<Arc<glass_runtime::Runtime>> {
    if let Err(why) = NamespaceHypervisor::available() {
        eprintln!("skipped: {why}");
        return None;
    }
    let dir = h.dir.path().join("computers");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(h.dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let blobs = FolderBlobs::new(h.dir.path().join("store")).unwrap();
    let computers = Arc::new(
        ComputerSandbox::new(dir, Arc::new(NamespaceHypervisor::new(Vec::new())), 1024, 1)
            .unwrap()
            .with_store(Arc::new(Store::new(Arc::new(blobs))), "S3: acme-agents"),
    );
    Some(common::rebuild_on(
        h,
        glass_policy::DEFAULT_POLICY,
        vec![reconciler()],
        computers.clone(),
        Some(computers),
    ))
}

fn ask(prompt: &str) -> glass_api::TaskRequest {
    glass_api::TaskRequest {
        persona: None,
        agent: "invoice-demo".parse().unwrap(),
        prompt: prompt.into(),
        scope: Default::default(),
        title: None,
        computer: Some("reconciler".into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0066_an_agent_keeps_its_computer_between_tasks() {
    let h = harness();
    let Some(rt) = on_computers(&h) else {
        return;
    };
    let made = rt
        .computer_make(
            &h.me,
            ComputerNew {
                id: "reconciler".into(),
                name: "Reconciler".into(),
                role: "Matches invoices to payments".into(),
                agent: "invoice-demo".parse().unwrap(),
            },
        )
        .unwrap();
    assert_eq!(made.state, ComputerState::Ready);
    assert_eq!(made.kind, "Linux namespaces");

    let first = rt.start(&h.me, ask("Match March's invoices")).unwrap();
    assert_eq!(first.computer.as_deref(), Some("reconciler"));
    eventually(|| rt.get(&h.me, first.id).unwrap().view.state == TileState::Done).await;

    // The next task finds what the first left on its computer.
    let second = rt.start(&h.me, ask("How many are left?")).unwrap();
    eventually(|| rt.get(&h.me, second.id).unwrap().view.state == TileState::Done).await;

    let screen = rt
        .computer_screen(
            &h.me,
            &ScreenRequest {
                id: "reconciler".into(),
                offset: 0,
            },
        )
        .unwrap();
    assert!(
        screen.text.contains("Matched 207 of 214 invoices"),
        "{}",
        screen.text
    );
    assert!(screen.text.contains("Still 7 unmatched"), "{}", screen.text);

    let detail = rt.get(&h.me, second.id).unwrap();
    assert!(
        detail
            .activity
            .iter()
            .any(|a| a.text == "Agent working on its own computer"),
        "{:?}",
        detail.activity
    );
    let listed = rt.computer_list(&h.me).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].live_tasks.is_empty());

    let audit = fs::read_to_string(h.audit_path()).unwrap();
    assert!(audit.contains("Made Reconciler a computer of its own"));
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0066_a_person_takes_the_keyboard_and_the_agent_waits() {
    let h = harness();
    let Some(rt) = on_computers(&h) else {
        return;
    };
    rt.computer_make(
        &h.me,
        ComputerNew {
            id: "reconciler".into(),
            name: "Reconciler".into(),
            role: String::new(),
            agent: "invoice-demo".parse().unwrap(),
        },
    )
    .unwrap();
    let typed = ScreenInput {
        id: "reconciler".into(),
        line: "echo 3 > /work/unmatched; cat /work/unmatched".into(),
    };
    assert!(matches!(
        rt.computer_input(&h.me, typed.clone()),
        Err(RuntimeError::Conflict(_))
    ));

    rt.computer_take_over(&h.me, "reconciler").unwrap();
    let refused = rt.start(&h.me, ask("How many are left?"));
    assert!(matches!(refused, Err(RuntimeError::Conflict(ref m)) if m.contains("keyboard")));

    rt.computer_input(&h.me, typed).unwrap();
    let request = ScreenRequest {
        id: "reconciler".into(),
        offset: 0,
    };
    eventually(|| {
        rt.computer_screen(&h.me, &request)
            .unwrap()
            .text
            .contains("\n3\n")
    })
    .await;
    let screen = rt.computer_screen(&h.me, &request).unwrap();
    assert!(
        screen.text.contains("root@reconciler:~# echo 3"),
        "{}",
        screen.text
    );
    assert!(screen.taken_over);

    // Handed back, the agent finds what its person left.
    rt.computer_hand_back(&h.me, "reconciler").unwrap();
    let task = rt.start(&h.me, ask("How many are left?")).unwrap();
    eventually(|| rt.get(&h.me, task.id).unwrap().view.state == TileState::Done).await;
    let screen = rt.computer_screen(&h.me, &request).unwrap();
    assert!(screen.text.contains("Still 3 unmatched"), "{}", screen.text);

    let audit = fs::read_to_string(h.audit_path()).unwrap();
    for line in [
        "Took over Reconciler's computer",
        "Typed a line on an agent's computer",
        "Handed Reconciler's computer back",
    ] {
        assert!(audit.contains(line), "{line}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0066_only_its_persons_agents_and_computers() {
    let h = harness();
    let Some(rt) = on_computers(&h) else {
        return;
    };
    // An agent the catalog doesn't have.
    let unknown = rt.computer_make(
        &h.me,
        ComputerNew {
            id: "helper".into(),
            name: "Helper".into(),
            role: String::new(),
            agent: "not-in-catalog".parse().unwrap(),
        },
    );
    assert!(matches!(unknown, Err(RuntimeError::PermissionDenied(_))));
    // A computer they don't have.
    let missing = rt.start(&h.me, ask("Match"));
    assert!(matches!(missing, Err(RuntimeError::NotFound(_))));
    // A runtime without computers refuses tasks that name one.
    let plain = common::rebuild_with(&h, glass_policy::DEFAULT_POLICY, vec![reconciler()]);
    assert!(matches!(
        plain.start(&h.me, ask("Match")),
        Err(RuntimeError::Invalid(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0066_unused_computers_sleep_and_a_host_being_replaced_sleeps_them_all() {
    let h = harness();
    let Some(rt) = on_computers_with_store(&h) else {
        return;
    };
    rt.computer_make(
        &h.me,
        ComputerNew {
            id: "reconciler".into(),
            name: "Reconciler".into(),
            role: "Matches invoices to payments".into(),
            agent: "invoice-demo".parse().unwrap(),
        },
    )
    .unwrap();
    let first = rt.start(&h.me, ask("Match March's invoices")).unwrap();
    eventually(|| rt.get(&h.me, first.id).unwrap().view.state == TileState::Done).await;
    let state = |rt: &glass_runtime::Runtime| rt.computer_list(&h.me).unwrap()[0].state;

    // Just used, it stays awake; unused for long enough, it sleeps.
    rt.sleep_idle_computers(std::time::Duration::from_secs(3600))
        .await;
    assert_eq!(state(&rt), ComputerState::Ready);
    assert_eq!(rt.computers_awake(), 1);
    rt.sleep_idle_computers(std::time::Duration::ZERO).await;
    assert_eq!(state(&rt), ComputerState::Asleep);
    assert_eq!(rt.computers_awake(), 0);
    rt.computer_wake(&h.me, "reconciler").await.unwrap();
    assert_eq!(rt.computers_awake(), 1);

    // The host is being replaced: its computers sleep, and none starts here
    // again; the task asked for says why, for its person to ask again.
    rt.leave_host(std::time::Duration::from_secs(60)).await;
    assert_eq!(state(&rt), ComputerState::Asleep);
    let refused = rt.start(&h.me, ask("How many are left?")).unwrap();
    eventually(|| rt.get(&h.me, refused.id).unwrap().view.state == TileState::Failed).await;
    let detail = rt.get(&h.me, refused.id).unwrap();
    assert!(
        format!("{detail:?}").contains("this host is being replaced"),
        "{detail:?}"
    );
    assert_eq!(state(&rt), ComputerState::Asleep);

    let audit = fs::read_to_string(h.audit_path()).unwrap();
    assert!(audit.contains("after 0 minutes unused"), "{audit}");
    assert!(audit.contains("This host is being replaced"));
    assert!(audit.contains("Put Reconciler's computer to sleep: this host is being replaced"));
}

fn persona_ask(persona: &str, prompt: &str) -> glass_api::TaskRequest {
    glass_api::TaskRequest {
        computer: Some(glass_api::PERSON_COMPUTER.into()),
        persona: Some(persona.into()),
        ..ask(prompt)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0068_a_persons_agents_work_on_their_one_computer() {
    let h = harness();
    let Some(rt) = on_computers(&h) else {
        return;
    };
    let new = |id: &str, name: &str| PersonaNew {
        id: id.into(),
        name: name.into(),
        role: String::new(),
        agent: "invoice-demo".parse().unwrap(),
    };
    rt.persona_make(&h.me, new("reconciler", "Reconciler"))
        .unwrap();
    let view = rt
        .persona_make(&h.me, new("closer", "Month-end closer"))
        .unwrap();
    assert_eq!(view.personas.len(), 2);
    assert_eq!(rt.computer_list(&h.me).unwrap().len(), 1);

    // An agent IT didn't make available can't be one of theirs.
    let refused = rt.persona_make(
        &h.me,
        PersonaNew {
            agent: "not-in-the-catalog".parse().unwrap(),
            ..new("rogue", "Rogue")
        },
    );
    assert!(
        matches!(refused, Err(RuntimeError::PermissionDenied(_))),
        "{refused:?}"
    );

    // Each persona's task is its own, on the one computer.
    let first = rt
        .start(&h.me, persona_ask("reconciler", "Match March's invoices"))
        .unwrap();
    assert_eq!(first.persona.as_deref(), Some("reconciler"));
    eventually(|| rt.get(&h.me, first.id).unwrap().view.state == TileState::Done).await;
    let second = rt
        .start(&h.me, persona_ask("closer", "How many are left?"))
        .unwrap();
    eventually(|| rt.get(&h.me, second.id).unwrap().view.state == TileState::Done).await;
    assert_eq!(
        rt.get(&h.me, second.id).unwrap().view.persona.as_deref(),
        Some("closer")
    );

    // Asks that don't fit are refused before anything starts.
    let unknown = rt.start(&h.me, persona_ask("nobody", "Hello"));
    assert!(
        matches!(unknown, Err(RuntimeError::NotFound(_))),
        "{unknown:?}"
    );
    let homeless = rt.start(
        &h.me,
        glass_api::TaskRequest {
            persona: Some("reconciler".into()),
            computer: None,
            ..ask("Hello")
        },
    );
    assert!(
        matches!(homeless, Err(RuntimeError::Invalid(_))),
        "{homeless:?}"
    );
    let other_agent = rt.start(
        &h.me,
        glass_api::TaskRequest {
            agent: "repeater".parse().unwrap(),
            ..persona_ask("reconciler", "Hello")
        },
    );
    assert!(other_agent.is_err(), "a persona runs only its own agent");

    let audit = fs::read_to_string(h.audit_path()).unwrap();
    assert!(
        audit.contains("Made a computer of their own, with Reconciler on it"),
        "{audit}"
    );
    assert!(audit.contains("Added Month-end closer to their computer"));
    let removed = rt.persona_remove(&h.me, "closer").unwrap();
    assert_eq!(removed.personas.len(), 1);
}
