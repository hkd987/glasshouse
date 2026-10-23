#![allow(clippy::unwrap_used)]

use std::path::Path;

use serde_json::json;

use super::*;

/// An app that declares calendar.read and mail.draft, as glassd would set it up.
fn setup() -> Setup {
    serde_json::from_value(json!({
        "app": "my-today",
        "name": "Today",
        "user": "demo",
        "actions": {
            "calendar.list": {
                "title": "List events", "tier": "auto", "side_effect": "none",
                "sample": { "from": "2026-10-23T00:00", "to": "2026-10-24T00:00", "events": [
                    { "id": "e1", "title": "Team standup", "start": "2026-10-23T09:30", "attendees": ["sam@example.com"] }
                ] }
            },
            "mail.draft": {
                "title": "Draft an email", "tier": "notify", "side_effect": "local_reversible",
                "sample": { "draft": "Drafts/1.eml", "id": "drafts/1.eml" }
            }
        }
    }))
    .unwrap()
}

#[test]
fn ga1_an_app_that_works_shows_its_screen_its_calls_and_what_each_button_does() {
    let bundle = r#"
        const root = document.getElementById("app");
        async function call(action, input) {
          const r = await fetch("/_bridge/call", { method: "POST", body: JSON.stringify({ action, input }) });
          return r.json();
        }
        function draw(events, note) {
          root.replaceChildren();
          const h1 = document.createElement("h1"); h1.textContent = "Today"; root.append(h1);
          for (const e of events) {
            const li = document.createElement("li");
            li.append(`${e.start.slice(11)} ${e.title}`);
            const b = document.createElement("button");
            b.textContent = "Running late";
            b.addEventListener("click", async () => {
              const r = await call("mail.draft", { to: e.attendees, subject: "Late", body: "5 minutes" });
              draw(events, r.ok ? "Draft saved" : r.text);
            });
            li.append(b);
            root.append(li);
          }
          if (note) { const p = document.createElement("p"); p.textContent = note; root.append(p); }
        }
        const listed = await call("calendar.list", { from: "2026-10-23", to: "2026-10-24" });
        draw(listed.data.events, "");
        document.documentElement.dataset.ready = "true";
    "#;
    let report = try_app(bundle, &setup()).unwrap();
    assert_eq!(report.errors, Vec::<String>::new());
    assert_eq!(report.screen, "Today\n09:30 Team standup[Running late]");
    assert_eq!(report.calls[0].action, "calendar.list");
    assert_eq!(report.presses.len(), 1);
    let press = &report.presses[0];
    assert_eq!(press.button, "Running late");
    assert_eq!(press.calls[0].action, "mail.draft");
    assert_eq!(press.calls[0].input["to"], json!(["sam@example.com"]));
    assert_eq!(press.appeared, ["Draft saved"]);
}

#[test]
fn ga1_a_lists_rows_are_one_kind_of_button_a_toolbars_are_not() {
    // Mail's folders: a row each, the same code behind them. Each toolbar
    // button runs code of its own, and a bound one's code can't be read.
    let bundle = r#"
        const root = document.getElementById("app");
        const shown = document.createElement("p");
        const button = (words, onclick) => {
          const b = document.createElement("button");
          b.textContent = words;
          b.onclick = onclick;
          return b;
        };
        const nav = document.createElement("nav");
        for (const folder of ["Inbox", "Drafts", "Sent", "Archive"]) {
          nav.append(button(folder, () => { shown.textContent = folder; }));
        }
        const bar = document.createElement("div");
        bar.append(
          button("Save draft", () => { shown.textContent = "saved"; }),
          button("Discard", () => { shown.textContent = "gone"; }),
          button("Undo", function () { shown.textContent = this; }.bind("undone")),
          button("Redo", function () { shown.textContent = this; }.bind("redone")),
        );
        root.append(nav, bar, shown);
    "#;
    let report = try_app(bundle, &setup()).unwrap();
    let pressed: Vec<&str> = report.presses.iter().map(|p| p.button.as_str()).collect();
    assert_eq!(pressed, ["Inbox", "Save draft", "Discard", "Undo", "Redo"]);
    assert_eq!(report.presses[0].appeared, ["Inbox"]);
    assert_eq!(report.presses[4].appeared, ["redone"]);
}

#[test]
fn ga1_what_goes_wrong_says_where() {
    // Today's first try: a bridge it never made.
    let report = try_app("bridge.call('calendar.list', {});", &setup()).unwrap();
    assert_eq!(
        report.errors,
        ["when it opened: ReferenceError: bridge is not defined"]
    );

    // An async load nobody awaited, reading data it doesn't have.
    let report = try_app(
        r#"async function load() {
             const r = await (await fetch("/_bridge/call", { method: "POST", body: JSON.stringify({ action: "calendar.list", input: {} }) })).json();
             document.getElementById("app").textContent = r.data.event.title;
           }
           load();"#,
        &setup(),
    )
    .unwrap();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(
        report.errors[0].starts_with("not caught: TypeError"),
        "{:?}",
        report.errors
    );

    // A button whose handler throws, an action it doesn't declare, and a
    // request that isn't its bridge's.
    let report = try_app(
        r#"const b = document.createElement("button");
           b.textContent = "Go";
           b.onclick = () => { fetch("/_bridge/call", { method: "POST", body: JSON.stringify({ action: "chat.post", input: {} }) }); null.x; };
           document.getElementById("app").append(b);
           fetch("https://example.com/x").catch((e) => console.error(e.message));"#,
        &setup(),
    )
    .unwrap();
    assert_eq!(report.screen, "[Go]");
    assert_eq!(report.presses[0].calls[0].action, "chat.post");
    assert!(
        report.presses[0].errors[0].starts_with("on click: TypeError"),
        "{:?}",
        report.presses
    );
    assert_eq!(
        report.console_errors,
        ["fetch https://example.com/x: an app reaches only its own bridge"]
    );

    // Code that isn't code.
    let report = try_app("const = ;", &setup()).unwrap();
    assert!(
        report.errors[0].starts_with("it doesn't parse: SyntaxError"),
        "{:?}",
        report.errors
    );
}

#[test]
fn ga1_a_page_that_never_stops_is_stopped() {
    // A loop that never ends, and a timer that keeps coming back.
    let report = try_app("for (;;) {}", &setup()).unwrap();
    assert!(report.errors[0].contains("loop"), "{:?}", report.errors);
    let report = try_app(
        "let n = 0; setInterval(() => { n += 1; document.getElementById('app').textContent = String(n); }, 10);",
        &setup(),
    )
    .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    // Three make-believe seconds of a 10 ms interval.
    assert_eq!(report.screen, "300");
}

/// The repository's apps folder, with esbuild, when `make apps-build` has run.
fn esbuild() -> Option<std::path::PathBuf> {
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    let program = apps.join("node_modules/@esbuild/linux-x64/bin/esbuild");
    program.exists().then_some(program)
}

#[test]
fn ga1_the_template_bundled_with_the_sdk_opens_and_searches() {
    let Some(esbuild) = esbuild() else {
        eprintln!("skipped: no esbuild in apps/node_modules (make apps-build)");
        return;
    };
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    let out = tempfile_dir();
    let built = std::process::Command::new(esbuild)
        .current_dir(&apps)
        .args([
            "sdk/template/src/main.ts",
            "--bundle",
            "--format=esm",
            "--target=chrome120",
            "--minify",
            &format!("--outfile={}", out.join("app.js").display()),
        ])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let bundle = std::fs::read_to_string(out.join("app.js")).unwrap();
    let mut setup = setup();
    setup.actions.insert(
        "people.search".into(),
        json!({ "title": "Find people", "tier": "auto", "side_effect": "none",
                "sample": { "directory_version": 1, "people": [{ "login": "priya", "name": "Priya Shah", "email": "priya@example.com", "title": "Counsel", "department": "Legal" }] } }),
    );
    let report = try_app(&bundle, &setup).unwrap();
    assert_eq!(report.errors, Vec::<String>::new());
    assert!(
        report.screen.starts_with("New app\n[input: Find someone]"),
        "{}",
        report.screen
    );
    let _ = std::fs::remove_dir_all(out);
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-app-try-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
