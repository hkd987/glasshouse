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
               Hold*) sleep 60 ;;
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

fn reconciler_persona(rt: &glass_runtime::Runtime, h: &Harness) {
    rt.persona_make(
        &h.me,
        PersonaNew {
            id: "reconciler".into(),
            name: "Reconciler".into(),
            role: String::new(),
            agent: "invoice-demo".parse().unwrap(),
        },
    )
    .unwrap();
}

fn daily(persona: &str, when: &str, zone: Option<&str>) -> glass_api::ScheduleNew {
    glass_api::ScheduleNew {
        persona: persona.into(),
        prompt: "Match the day's invoices".into(),
        when: when.into(),
        zone: zone.map(Into::into),
    }
}

// 2026-10-28 08:30, 09:00, and 09:00 the day after, UTC.
const BEFORE_NINE: i64 = 1_793_176_200;
const NINE: i64 = 1_793_178_000;
const DAY: i64 = 86_400;

#[tokio::test(flavor = "multi_thread")]
async fn adr0069_a_schedule_wakes_its_sleeping_computer_and_runs_once() {
    let h = harness();
    let Some(rt) = on_computers_with_store(&h) else {
        return;
    };
    let store = Arc::new(glass_runtime::FileSchedules::new(
        h.dir.path().join("schedules.json"),
    ));
    rt.set_schedules(store.clone());
    reconciler_persona(&rt, &h);
    let made = rt
        .schedule_make_at(&h.me, daily("reconciler", "daily 09:00", None), BEFORE_NINE)
        .unwrap();
    assert_eq!(made.describe, "every day at 09:00");
    assert_eq!(made.next_run.unwrap().unix_seconds(), NINE);
    rt.computer_sleep(&h.me, glass_api::PERSON_COMPUTER)
        .await
        .unwrap();
    let state = || rt.computer_list(&h.me).unwrap()[0].state;
    assert_eq!(state(), ComputerState::Asleep);

    // Not yet; then at nine, once, however many times it's looked for.
    assert!(rt.run_due_schedules(NINE - 1).is_empty());
    let turns = rt.run_due_schedules(NINE + 20);
    let [(id, glass_runtime::Turn::Started(task))] = turns.as_slice() else {
        panic!("{turns:?}");
    };
    assert_eq!(id, &made.id);
    assert!(
        rt.run_due_schedules(NINE + 40).is_empty(),
        "a slot runs once"
    );
    eventually(|| rt.get(&h.me, *task).unwrap().view.state == TileState::Done).await;
    let view = rt.get(&h.me, *task).unwrap().view;
    assert_eq!(view.persona.as_deref(), Some("reconciler"));
    assert_eq!(state(), ComputerState::Ready, "the run woke it");

    // How it went is kept with the schedule; the next slot is tomorrow's.
    eventually(|| {
        rt.schedules_of(&h.me).unwrap()[0]
            .last
            .as_ref()
            .is_some_and(|l| l.outcome == "done")
    })
    .await;
    let kept = &rt.schedules_of(&h.me).unwrap()[0];
    assert_eq!(kept.last.as_ref().unwrap().task, Some(*task));
    assert_eq!(kept.next_run.unwrap().unix_seconds(), NINE + DAY);
    assert_eq!(kept.failures, 0);

    // Late by more than an hour: let pass, not replayed.
    let turns = rt.run_due_schedules(NINE + DAY + 2 * 3600);
    assert_eq!(turns, vec![(made.id.clone(), glass_runtime::Turn::Missed)]);
    let kept = &rt.schedules_of(&h.me).unwrap()[0];
    assert_eq!(kept.last.as_ref().unwrap().outcome, "missed");
    assert_eq!(kept.next_run.unwrap().unix_seconds(), NINE + 2 * DAY);

    // Being replaced, this host claims nothing: another host will.
    rt.leave_host(std::time::Duration::from_secs(60)).await;
    assert!(rt.run_due_schedules(NINE + 2 * DAY + 10).is_empty());

    let audit = fs::read_to_string(h.audit_path()).unwrap();
    assert!(
        audit.contains("Scheduled reconciler: every day at 09:00"),
        "{audit}"
    );
    assert!(audit.contains("Started the scheduled run for reconciler"));
    assert!(audit.contains("Let a late slot pass for reconciler"));
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0069_only_their_own_agents_and_schedules_within_limits() {
    let h = harness();
    let Some(rt) = on_computers(&h) else {
        return;
    };
    // No store, no schedules.
    assert!(matches!(
        rt.schedule_make_at(&h.me, daily("reconciler", "daily 09:00", None), BEFORE_NINE),
        Err(RuntimeError::Conflict(_))
    ));
    rt.set_schedules(Arc::new(glass_runtime::FileSchedules::new(
        h.dir.path().join("schedules.json"),
    )));
    reconciler_persona(&rt, &h);
    let make = |new| rt.schedule_make_at(&h.me, new, BEFORE_NINE);
    for (bad, why) in [
        (
            daily("nobody", "daily 09:00", None),
            "an agent they don't have",
        ),
        (
            daily("reconciler", "at nine-ish", None),
            "a schedule it can't read",
        ),
        (
            daily("reconciler", "daily 25:00", None),
            "an hour that isn't one",
        ),
        (
            daily("reconciler", "daily 09:00", Some("Mars/Olympus")),
            "a zone that isn't one",
        ),
        (
            daily("reconciler", "daily 09:00", Some("../../etc/passwd")),
            "a path for a zone",
        ),
        (
            glass_api::ScheduleNew {
                prompt: "  ".into(),
                ..daily("reconciler", "hourly", None)
            },
            "no directions",
        ),
        (
            glass_api::ScheduleNew {
                prompt: "x".repeat(glass_api::SCHEDULE_PROMPT_MAX_CHARS + 1),
                ..daily("reconciler", "hourly", None)
            },
            "directions too long",
        ),
    ] {
        assert!(make(bad).is_err(), "{why}");
    }

    // Twenty on at most; off, another may go on.
    let mut ids = Vec::new();
    for _ in 0..glass_api::SCHEDULES_MAX {
        ids.push(make(daily("reconciler", "hourly", None)).unwrap().id);
    }
    assert!(matches!(
        make(daily("reconciler", "hourly", None)),
        Err(RuntimeError::Conflict(_))
    ));
    rt.schedule_enable_at(&h.me, &ids[0], false, BEFORE_NINE)
        .unwrap();
    let extra = make(daily("reconciler", "hourly", None)).unwrap();
    assert!(matches!(
        rt.schedule_enable_at(&h.me, &ids[0], true, BEFORE_NINE),
        Err(RuntimeError::Conflict(_))
    ));
    rt.schedule_remove(&h.me, &extra.id).unwrap();

    // Someone else sees none of them and changes none of them.
    let other = glass_runtime::Caller {
        name: "someone-else".into(),
        uid: 4242,
        gid: 4242,
        home: h.dir.path().join("someone-else"),
    };
    assert!(rt.schedules_of(&other).unwrap().is_empty());
    assert!(matches!(
        rt.schedule_remove(&other, &ids[1]),
        Err(RuntimeError::NotFound(_))
    ));
    assert!(matches!(
        rt.schedule_enable_at(&other, &ids[1], false, BEFORE_NINE),
        Err(RuntimeError::NotFound(_))
    ));
    assert!(matches!(
        rt.schedule_make_at(&other, daily("reconciler", "hourly", None), BEFORE_NINE),
        Err(RuntimeError::NotFound(_))
    ));
    assert_eq!(
        rt.schedules_of(&h.me).unwrap().len(),
        glass_api::SCHEDULES_MAX
    );
    assert!(
        rt.schedules_of(&h.me)
            .unwrap()
            .iter()
            .skip(1)
            .all(|s| s.enabled)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0069_a_schedule_that_keeps_failing_turns_itself_off() {
    let h = harness();
    let Some(rt) = on_computers(&h) else {
        return;
    };
    rt.set_schedules(Arc::new(glass_runtime::FileSchedules::new(
        h.dir.path().join("schedules.json"),
    )));
    reconciler_persona(&rt, &h);
    let made = rt
        .schedule_make_at(&h.me, daily("reconciler", "daily 09:00", None), BEFORE_NINE)
        .unwrap();
    // The agent it directs is gone: every run is refused.
    rt.persona_remove(&h.me, "reconciler").unwrap();
    for day in 0..i64::from(glass_api::SCHEDULE_FAILURES_MAX) {
        let turns = rt.run_due_schedules(NINE + day * DAY + 5);
        assert!(
            matches!(turns.as_slice(), [(_, glass_runtime::Turn::Refused(why))] if why.contains("no longer one of their agents")),
            "{turns:?}"
        );
    }
    let kept = &rt.schedules_of(&h.me).unwrap()[0];
    assert!(!kept.enabled);
    assert!(
        kept.disabled_why
            .as_deref()
            .unwrap()
            .contains("10 runs in a row"),
        "{kept:?}"
    );
    assert!(
        rt.run_due_schedules(NINE + 20 * DAY).is_empty(),
        "off, it doesn't run"
    );
    // On again, its failures are forgotten and its next slot counted from then.
    reconciler_persona(&rt, &h);
    let on = rt
        .schedule_enable_at(&h.me, &made.id, true, NINE + 20 * DAY + 60)
        .unwrap();
    assert_eq!((on.enabled, on.failures), (true, 0));
    assert_eq!(on.next_run.unwrap().unix_seconds(), NINE + 21 * DAY);
}

#[tokio::test(flavor = "multi_thread")]
async fn adr0069_a_late_slot_never_starts_a_second_run_beside_one_going() {
    let h = harness();
    let Some(rt) = on_computers(&h) else {
        return;
    };
    rt.set_schedules(Arc::new(glass_runtime::FileSchedules::new(
        h.dir.path().join("schedules.json"),
    )));
    reconciler_persona(&rt, &h);
    let made = rt
        .schedule_make_at(
            &h.me,
            glass_api::ScheduleNew {
                prompt: "Hold the ledger".into(),
                ..daily("reconciler", "daily 09:00", None)
            },
            BEFORE_NINE,
        )
        .unwrap();
    let turns = rt.run_due_schedules(NINE + 5);
    let [(_, glass_runtime::Turn::Started(going))] = turns.as_slice() else {
        panic!("{turns:?}");
    };
    // A day on, the next slot is two hours late while the first run still
    // goes (ScheduleClaims.tla found it): it passes, and the run going is
    // still the schedule's last, so the slot after doesn't start a second.
    let turns = rt.run_due_schedules(NINE + DAY + 2 * 3600);
    assert_eq!(turns, vec![(made.id.clone(), glass_runtime::Turn::Skipped)]);
    let kept = &rt.schedules_of(&h.me).unwrap()[0];
    assert_eq!(kept.last.as_ref().unwrap().outcome, "running");
    assert_eq!(kept.last.as_ref().unwrap().task, Some(*going));
    let turns = rt.run_due_schedules(NINE + 2 * DAY + 5);
    assert_eq!(turns, vec![(made.id.clone(), glass_runtime::Turn::Skipped)]);
    rt.stop(&h.me, *going).unwrap();
}
