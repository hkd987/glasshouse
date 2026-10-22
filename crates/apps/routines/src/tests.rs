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
