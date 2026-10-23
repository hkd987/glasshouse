#![allow(clippy::unwrap_used)]

use super::*;

/// What Haiku wrote for overdue invoices, as a routine keeps it.
const OVERDUE: &str = r#"const names = call("files_list", { path: "Invoices" }).text.split("\n");
let owed = 0;
for (const name of names) {
  const text = call("files_read", { path: "Invoices/" + name }).text;
  if (/Status: unpaid/.test(text)) owed += parseFloat(text.match(/Amount: (.*)/)[1]);
}
return "Unpaid: " + round(owed);"#;

struct Fixture {
    _dir: tempfile::TempDir,
    provider: RoutinesProvider,
    routines: Routines,
    ctx: TaskContext,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let routines = Routines::new(dir.path().join("state/routines"));
    let mut ctx = glass_actions::testing::context(&home);
    ctx.principal.agent = "desktop-agent".parse().unwrap();
    let provider = RoutinesProvider::new(routines.clone());
    provider.prepare(&ctx).unwrap();
    Fixture {
        _dir: dir,
        provider,
        routines,
        ctx,
    }
}

impl Fixture {
    fn call(&self, action: &str, input: Value) -> Result<ActionOutput, ActionError> {
        self.preview(action, &input)?;
        self.provider
            .invoke(&self.ctx, &action.parse().unwrap(), &input)
    }

    fn preview(&self, action: &str, input: &Value) -> Result<Preview, ActionError> {
        self.provider
            .preview(&self.ctx, &action.parse().unwrap(), input)
    }
}

fn overdue() -> Value {
    json!({
        "name": "Overdue invoices",
        "request": "Tell me which invoices are unpaid and what they come to.",
        "script": OVERDUE,
        "schedule": "weekdays 09:00",
        "note": "First version",
    })
}

#[test]
fn da4_a_routine_keeps_the_tasks_scope_its_script_and_when_it_runs() {
    let f = fixture();
    let saved = f.call(SAVE, overdue()).unwrap();
    assert_eq!(saved.data["routine"], "overdue-invoices");
    assert_eq!(saved.data["version"], 1);
    assert!(
        saved.text.contains("It runs every weekday at 09:00"),
        "{}",
        saved.text
    );
    assert_eq!(saved.changes[0].paths, [f.ctx.home.join(store::MINE)]);

    let kept = f.routines.record("demo", "overdue-invoices", 1).unwrap();
    assert_eq!(kept.script, OVERDUE);
    assert_eq!(kept.calls, ["files_list", "files_read"]);
    assert_eq!(kept.agent.as_str(), "desktop-agent");
    assert_eq!(kept.saved_in, f.ctx.principal.task);
    // The scope is the task's, never the input's (DA-4).
    assert_eq!(kept.read, [f.ctx.home.join("Invoices")]);
    assert_eq!(kept.write, [f.ctx.home.join("Reports")]);
    for extra in ["read", "write", "agent"] {
        let mut sneaky = overdue();
        sneaky[extra] = json!(["/etc"]);
        assert!(f.call(SAVE, sneaky).is_err(), "{extra}");
    }

    let listed = f.call(LIST, json!({})).unwrap();
    assert_eq!(
        listed.text,
        "Overdue invoices (overdue-invoices, version 1): every weekday at 09:00"
    );
    let read = f
        .call(READ, json!({ "routine": "overdue-invoices" }))
        .unwrap();
    assert_eq!(read.data["script"], OVERDUE);
    assert_eq!(read.data["schedule"], "weekdays 09:00");
}

#[test]
fn da4_the_person_sees_what_will_run_before_its_saved() {
    let f = fixture();
    let preview = f.preview(SAVE, &overdue()).unwrap();
    assert_eq!(preview.summary, "Save the routine Overdue invoices");
    let items = preview.items.join("\n");
    for line in [
        "Does: Tell me which invoices are unpaid and what they come to.",
        "Runs every weekday at 09:00, in this computer's time",
        "Calls: files_list, files_read, each at its tier, as desktop-agent would",
        "The script, 7 lines:",
        "  return \"Unpaid: \" + round(owed);",
    ] {
        assert!(items.contains(line), "{line:?} in\n{items}");
    }
    assert!(items.contains(&format!("Reads: {}", f.ctx.home.join("Invoices").display())));
    // Its approval is the person's, and it can be undone.
    let def = f
        .provider
        .actions()
        .into_iter()
        .find(|d| d.name.as_str() == SAVE)
        .unwrap();
    assert_eq!(def.default_tier, Tier::Ask);
    assert_eq!(preview.writes, [f.ctx.home.join(store::MINE)]);
    // A long script says how much more there is.
    let mut long = overdue();
    long["script"] = json!("print(1);\n".repeat(60));
    let items = f.preview(SAVE, &long).unwrap().items;
    assert_eq!(items.last().unwrap(), "  … and 20 more lines");
}

#[test]
fn da4_every_save_is_a_version_and_removing_one_is_undoable() {
    let f = fixture();
    f.call(SAVE, overdue()).unwrap();
    let next = json!({
        "routine": "overdue-invoices",
        "request": "Tell me which invoices are unpaid, oldest first.",
        "script": OVERDUE.replace("Unpaid", "Owed"),
        "note": "Says owed",
    });
    assert_eq!(
        f.preview(SAVE, &next).unwrap().summary,
        "Save version 2 of the routine Overdue invoices"
    );
    let saved = f.call(SAVE, next).unwrap();
    assert_eq!(
        (&saved.data["version"], &saved.data["name"]),
        (&json!(2), &json!("Overdue invoices"))
    );
    // Without a schedule, it runs only when started.
    assert_eq!(saved.data["runs"], "when started");
    assert_eq!(f.routines.versions("demo", "overdue-invoices"), [1, 2]);
    assert_eq!(f.routines.current("demo", &f.ctx.home)[0].version, 2);
    // A second routine by the same name is one of its own.
    let again = f.call(SAVE, overdue()).unwrap();
    assert_eq!(again.data["routine"], "overdue-invoices-2");

    let removed = f
        .call(DELETE, json!({ "routine": "overdue-invoices" }))
        .unwrap();
    assert_eq!(removed.changes[0].kind, ChangeKind::Deleted);
    let left: Vec<String> = f
        .routines
        .current("demo", &f.ctx.home)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(left, ["overdue-invoices-2"]);
    // Its versions stay, so undo (the choices file restored) brings it back.
    assert_eq!(f.routines.versions("demo", "overdue-invoices"), [1, 2]);
    let err = f
        .call(READ, json!({ "routine": "overdue-invoices" }))
        .unwrap_err();
    assert!(
        err.to_string().contains("isn't a routine of yours"),
        "{err}"
    );
}

#[test]
fn da4_a_routine_that_wont_do_says_why() {
    let f = fixture();
    let save = |change: &dyn Fn(&mut Value)| {
        let mut input = overdue();
        change(&mut input);
        f.call(SAVE, input).unwrap_err().to_string()
    };
    assert!(save(&|i| i["schedule"] = json!("every so often")).contains("isn't a schedule"));
    assert!(save(&|i| i["script"] = json!("  ")).contains("the script is the routine"));
    assert!(
        save(&|i| i["script"] = json!("x".repeat(MAX_SCRIPT_BYTES + 1))).contains("at most 32 KB")
    );
    assert!(save(&|i| i["name"] = json!("two\nlines")).contains("name it in a line"));
    assert!(save(&|i| i["note"] = json!("")).contains("what this version is"));
    assert!(save(&|i| i["routine"] = json!("someone-elses")).contains("isn't a routine of yours"));
    assert!(
        save(&|i| {
            i.as_object_mut().unwrap().remove("name");
        })
        .contains("name the routine")
    );
    assert!(f.routines.logins().is_empty(), "nothing kept");
}

#[test]
fn da4_the_tools_a_script_calls_are_read_from_it() {
    assert_eq!(
        calls(
            "const a = call('mail_list', {});\nawait call( \"files_read\" , {});\n\
             recall('nope'); x.call('nope2'); call(name, {}); call(`calendar_list`, {}); call('mail_list')"
        ),
        ["mail_list", "files_read", "calendar_list"]
    );
}

#[test]
fn da4_an_id_is_never_a_path_whatever_the_choices_file_says() {
    let f = fixture();
    f.call(SAVE, overdue()).unwrap();
    // The person may edit their choices by hand: only ids of theirs count.
    let mut chosen = store::chosen(&f.ctx.home);
    for sneaky in ["../../../etc", "..", "a/b", "Overdue", "-x", ""] {
        chosen.insert(sneaky.into(), 1);
    }
    store::choose(&f.ctx.home, f.ctx.owner, chosen).unwrap();
    let ids: Vec<String> = f
        .routines
        .current("demo", &f.ctx.home)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, ["overdue-invoices"]);
    for sneaky in ["../../../etc", "..", "a/b"] {
        assert!(f.routines.record("demo", sneaky, 1).is_none());
        assert!(f.routines.versions("demo", sneaky).is_empty());
        let err = f.call(READ, json!({ "routine": sneaky })).unwrap_err();
        assert!(
            err.to_string().contains("isn't a routine of yours"),
            "{err}"
        );
    }
    // Nor a login that's a path.
    assert!(
        f.routines
            .record("../demo", "overdue-invoices", 1)
            .is_none()
    );
    // A record moved under another name isn't that one.
    let kept = f.routines.record("demo", "overdue-invoices", 1).unwrap();
    assert_eq!((kept.owner.as_str(), kept.version), ("demo", 1));
    assert!(store::valid_id("overdue-invoices-2") && !store::valid_id("Overdue"));
}

#[test]
fn adr0047_a_routine_goes_back_to_an_earlier_version_and_any_version_reads() {
    let f = fixture();
    f.call(SAVE, overdue()).unwrap();
    f.call(
        SCHEDULE,
        json!({ "routine": "overdue-invoices", "schedule": "fri 17:00" }),
    )
    .unwrap();

    // Its versions, newest first, and which one runs.
    let read = f
        .call(READ, json!({ "routine": "overdue-invoices" }))
        .unwrap();
    let versions = read.data["versions"].as_array().unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|v| (
                v["version"].as_u64().unwrap(),
                v["in_use"].as_bool().unwrap()
            ))
            .collect::<Vec<_>>(),
        [(2, true), (1, false)]
    );
    assert_eq!(versions[1]["note"], "First version");
    assert_eq!(versions[1]["runs"], "every weekday at 09:00");
    assert_eq!(read.data["in_use"], true);
    // An earlier one reads with its own code and schedule.
    let first = f
        .call(READ, json!({ "routine": "overdue-invoices", "version": 1 }))
        .unwrap();
    assert_eq!(
        (
            &first.data["version"],
            &first.data["in_use"],
            &first.data["runs"]
        ),
        (&json!(1), &json!(false), &json!("every weekday at 09:00"))
    );
    assert_eq!(first.data["script"], OVERDUE);
    assert!(
        first.text.contains("not the one in use (version 2)"),
        "{}",
        first.text
    );

    // Back to version 1: the person's choice, undoable, what it changes said first.
    let input = json!({ "routine": "overdue-invoices", "version": 1 });
    let preview = f.preview(USE, &input).unwrap();
    assert_eq!(
        preview.summary,
        "Go back to version 1 of the routine Overdue invoices"
    );
    assert!(
        preview
            .items
            .contains(&"Runs every weekday at 09:00 again, not every Friday at 17:00".to_owned()),
        "{:?}",
        preview.items
    );
    assert_eq!(preview.writes, [f.ctx.home.join(store::MINE)]);
    let used = f.call(USE, input.clone()).unwrap();
    assert!(used.changes[0].reversible);
    assert_eq!(f.routines.current("demo", &f.ctx.home)[0].version, 1);
    assert!(
        used.text.contains("Undo puts version 2 back"),
        "{}",
        used.text
    );
    // Both versions stay; the next save is version 3.
    assert_eq!(f.routines.versions("demo", "overdue-invoices"), [1, 2]);

    // Not one that's in use, nor one never saved.
    let err = f.call(USE, input).unwrap_err().to_string();
    assert!(err.contains("is version 1 already"), "{err}");
    let err = f
        .call(USE, json!({ "routine": "overdue-invoices", "version": 7 }))
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no version 7"), "{err}");
    let err = f
        .call(READ, json!({ "routine": "overdue-invoices", "version": 7 }))
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no version 7"), "{err}");
}

/// The runtime as Routines' tests see it: what's running, and who heard of changes.
#[derive(Debug, Default)]
struct Watched {
    running: std::sync::Mutex<BTreeMap<String, glass_types::TaskId>>,
    woken: std::sync::Mutex<Vec<String>>,
}

impl Host for Watched {
    fn run(&self, _: &str, _: u32, _: &str) -> Result<glass_types::TaskId, String> {
        Err("not here".into())
    }
    fn repeats(&self, _: &str, _: u32) -> Vec<Repeat> {
        Vec::new()
    }
    fn dismiss(&self, _: &str, _: u32, _: glass_types::TaskId) -> Result<(), String> {
        Ok(())
    }
    fn running(&self, _: &str, _: u32) -> BTreeMap<String, glass_types::TaskId> {
        self.running.lock().unwrap().clone()
    }
    fn changed(&self, login: &str) {
        self.woken.lock().unwrap().push(login.to_owned());
    }
}

#[test]
fn adr0047_routines_windows_hear_of_every_change_and_see_what_runs() {
    let f = fixture();
    let host = std::sync::Arc::new(Watched::default());
    f.routines.set_host(host.clone());
    let woken = || std::mem::take(&mut *host.woken.lock().unwrap());

    f.call(SAVE, overdue()).unwrap();
    assert_eq!(woken(), ["demo"], "saved");
    let routine = json!({ "routine": "overdue-invoices" });
    f.call(
        PAUSE,
        json!({ "routine": "overdue-invoices", "paused": true }),
    )
    .unwrap();
    assert_eq!(woken(), ["demo"], "paused");
    f.call(
        SCHEDULE,
        json!({ "routine": "overdue-invoices", "schedule": null }),
    )
    .unwrap();
    assert_eq!(woken(), ["demo"], "rescheduled");
    f.call(USE, json!({ "routine": "overdue-invoices", "version": 1 }))
        .unwrap();
    assert_eq!(woken(), ["demo"], "an earlier version");
    // Reading changes nothing, and wakes no one.
    f.call(LIST, json!({})).unwrap();
    f.call(READ, routine.clone()).unwrap();
    assert!(woken().is_empty());

    // A run going now shows, wherever it was started.
    let run = glass_types::TaskId::new();
    host.running
        .lock()
        .unwrap()
        .insert("overdue-invoices".into(), run);
    let listed = f.call(LIST, json!({})).unwrap();
    assert_eq!(listed.data["routines"][0]["running"], json!(run));
    assert_eq!(
        f.call(READ, routine.clone()).unwrap().data["running"],
        json!(run)
    );
    host.running.lock().unwrap().clear();
    assert_eq!(
        f.call(LIST, json!({})).unwrap().data["routines"][0]["running"],
        Value::Null
    );

    f.call(DELETE, routine).unwrap();
    assert_eq!(woken(), ["demo"], "removed");
}

/// A runtime that starts routines, offers one repeated request, and has
/// one run of a routine going.
#[derive(Debug)]
struct Runs {
    repeat: Repeat,
    running: glass_types::TaskId,
}

impl Host for Runs {
    fn run(&self, _: &str, _: u32, _: &str) -> Result<glass_types::TaskId, String> {
        Ok(glass_types::TaskId::new())
    }
    fn repeats(&self, _: &str, _: u32) -> Vec<Repeat> {
        vec![self.repeat.clone()]
    }
    fn dismiss(&self, _: &str, _: u32, _: glass_types::TaskId) -> Result<(), String> {
        Ok(())
    }
    fn running(&self, _: &str, _: u32) -> BTreeMap<String, glass_types::TaskId> {
        BTreeMap::from([("overdue-invoices".to_owned(), self.running)])
    }
    fn changed(&self, _: &str) {}
}

#[test]
fn ga1_what_each_action_returns_is_what_it_declares() {
    let f = fixture();
    let repeat = Repeat {
        task: glass_types::TaskId::new(),
        request: "Total the unpaid invoices".into(),
        agent: "desktop-agent".parse().unwrap(),
        times: 3,
        last_at: Timestamp::now(),
        script: OVERDUE.into(),
        steps: vec!["List Invoices".into(), "Read each invoice".into()],
        read: vec![f.ctx.home.join("Invoices")],
        write: Vec::new(),
    };
    f.routines.set_host(std::sync::Arc::new(Runs {
        repeat: repeat.clone(),
        running: glass_types::TaskId::new(),
    }));
    let run = |name: &str, input: Value| {
        let mut out = f
            .call(name, input)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        if let Some(data) = out.data.as_object_mut() {
            data.remove(glass_actions::AUDIT_DETAIL);
        }
        let shape = f.provider.returns(&name.parse().unwrap()).unwrap();
        glass_actions::shape::conforms(&out.data, &shape).unwrap_or_else(|e| panic!("{name}: {e}"));
        out.data
    };
    let routine = json!({ "routine": "overdue-invoices" });
    // Nothing yet: empty lists, nulls where there's no run.
    run(LIST, json!({}));
    run(SAVE, overdue());
    run(LIST, json!({}));
    // With a run kept, and one going.
    f.routines
        .record_run(
            "demo",
            "overdue-invoices",
            Run {
                task: glass_types::TaskId::new(),
                version: 1,
                scheduled: true,
                started_at: Timestamp::now(),
                ended_at: Timestamp::now(),
                outcome: Outcome::Done,
                model: false,
                said: "Unpaid: 1,250".into(),
                changes: 0,
            },
        )
        .unwrap();
    let listed = run(LIST, json!({}));
    assert!(listed["routines"][0]["last_run"].is_object());
    assert!(listed["routines"][0]["running"].is_string());
    run(
        SCHEDULE,
        json!({ "routine": "overdue-invoices", "schedule": "fri 17:00" }),
    );
    let read = run(READ, routine.clone());
    assert_eq!(read["runs_kept"].as_array().unwrap().len(), 1);
    run(READ, json!({ "routine": "overdue-invoices", "version": 1 }));
    run(USE, json!({ "routine": "overdue-invoices", "version": 1 }));
    run(
        SCHEDULE,
        json!({ "routine": "overdue-invoices", "schedule": null }),
    );
    run(
        PAUSE,
        json!({ "routine": "overdue-invoices", "paused": true }),
    );
    // Paused and unscheduled: nothing will start it.
    assert_eq!(run(READ, routine.clone())["next_run_at"], Value::Null);
    run(RUN, routine.clone());
    run(REPEATS, json!({}));
    run(
        KEEP,
        json!({ "task": repeat.task, "name": "Unpaid total", "schedule": "mon 08:00" }),
    );
    run(DISMISS, json!({ "task": repeat.task }));
    run(DELETE, routine);

    // Every action declares its data.
    for def in f.provider.actions() {
        assert!(f.provider.returns(&def.name).is_some(), "{}", def.name);
    }
}
