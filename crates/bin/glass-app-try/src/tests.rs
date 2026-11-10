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
fn ga1_fields_are_typed_into_and_a_form_a_press_reveals_is_pressed_next() {
    // Compose opens a form; Send is pressed before the rest of the page,
    // with its fields typed into. A search box submits with Enter.
    let bundle = r#"
        const root = document.getElementById("app");
        async function call(action, input) {
          const r = await fetch("/_bridge/call", { method: "POST", body: JSON.stringify({ action, input }) });
          return r.json();
        }
        const shown = document.createElement("p");
        const find = document.createElement("form");
        const query = document.createElement("input");
        query.type = "search";
        query.setAttribute("aria-label", "Search mail");
        find.append(query);
        find.addEventListener("submit", (e) => { e.preventDefault(); shown.textContent = `Found ${query.value}`; });
        const compose = document.createElement("button");
        compose.textContent = "Compose";
        compose.onclick = () => {
          const form = document.createElement("form");
          const to = document.createElement("input");
          to.placeholder = "To";
          const when = document.createElement("input");
          when.type = "date";
          when.id = "when";
          const label = document.createElement("label");
          label.setAttribute("for", "when");
          label.textContent = "Send on";
          const body = document.createElement("textarea");
          body.name = "body";
          const send = document.createElement("button");
          send.textContent = "Send";
          form.append(to, label, when, body, send);
          form.addEventListener("submit", async (e) => {
            e.preventDefault();
            const r = await call("mail.draft", { to: [to.value], subject: when.value, body: body.value });
            shown.textContent = r.ok ? "Drafted" : r.text;
            form.remove();
          });
          root.append(form);
        };
        const later = document.createElement("button");
        later.textContent = "Later";
        later.onclick = () => { shown.textContent = "Later"; };
        root.append(find, compose, later, shown);
    "#;
    let report = try_app(bundle, &setup()).unwrap();
    assert_eq!(report.errors, Vec::<String>::new());
    let pressed: Vec<&str> = report.presses.iter().map(|p| p.button.as_str()).collect();
    assert_eq!(
        pressed,
        ["Compose", "Send", "Later", "Enter in Search mail"]
    );
    let send = &report.presses[1];
    let today = send.typed[1].text.clone();
    assert_eq!(today.len(), 10, "a date: {today}");
    assert_eq!(
        send.typed,
        [
            Typed {
                field: "To".into(),
                text: "sam@example.com".into()
            },
            Typed {
                field: "Send on".into(),
                text: today.clone()
            },
            Typed {
                field: "body".into(),
                text: "Sample text.".into()
            },
        ]
    );
    // Typed before the first press, the search box is typed once.
    assert_eq!(
        report.presses[0].typed,
        [Typed {
            field: "Search mail".into(),
            text: "Sample".into()
        }]
    );
    assert_eq!(send.calls[0].action, "mail.draft");
    assert_eq!(
        send.calls[0].input,
        json!({ "to": ["sam@example.com"], "subject": today, "body": "Sample text." })
    );
    assert_eq!(send.appeared, ["Drafted"]);
    assert_eq!(report.presses[3].appeared, ["Found Sample"]);
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
    // It searches as you type: typing alone, a step of its own.
    let typing = &report.presses[0];
    assert_eq!(typing.button, "");
    assert_eq!(
        typing.typed,
        [Typed {
            field: "Find someone".into(),
            text: "Sample".into()
        }]
    );
    assert_eq!(typing.calls[0].action, "people.search");
    assert_eq!(typing.calls[0].input["query"], "Sample");
    assert!(
        typing.appeared.iter().any(|l| l.contains("Priya Shah")),
        "{:?}",
        typing.appeared
    );
    let _ = std::fs::remove_dir_all(out);
}

/// Bundles `source` (TypeScript) as an app in the apps folder would be,
/// with the SDK; `None` without esbuild.
fn bundled(source: &str) -> Option<String> {
    use std::io::Write;
    let esbuild = esbuild()?;
    let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    // From standard input, so imports resolve from the apps folder.
    let mut build = std::process::Command::new(esbuild)
        .current_dir(&apps)
        .args([
            "--bundle",
            "--format=esm",
            "--target=chrome120",
            "--loader=ts",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    build
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let built = build.wait_with_output().unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    Some(String::from_utf8(built.stdout).unwrap())
}

// Mail and Settings draw with the SDK's redraw in place, and a remix of
// either is tried here before it's saved. The stand-in page lacked what a
// redraw reads (an element's attributes) and writes (a text node's value):
// every remix of Mail "showed nothing when it opened".
#[test]
fn ga1_an_app_that_redraws_in_place_with_the_sdk_opens_and_takes_a_press() {
    let Some(bundle) = bundled(
        r#"
        import { h, lasting, ready, redraw } from "@glasshouse/sdk";
        const root = document.getElementById("app") as HTMLElement;
        const state = { pressed: 0, notice: null as string | null };
        const kept = lasting(h("label", { class: "field" }, h("input", { name: "to", "aria-label": "To" })));
        function draw(): void {
          redraw(root, () => [
            h("h1", {}, "Counter"),
            state.notice && h("div", { class: "banner", role: "status" }, state.notice),
            h(
              "main",
              { "data-pressed": String(state.pressed > 0) },
              h("p", { "data-test": "count" }, `Pressed ${state.pressed} times`),
              kept,
              h("button", { onclick: () => { state.pressed += 1; state.notice = `That makes ${state.pressed}`; draw(); } }, "Press"),
            ),
          ]);
        }
        draw();
        draw();
        ready();
        "#,
    ) else {
        eprintln!("skipped: no esbuild in apps/node_modules (make apps-build)");
        return;
    };
    let report = try_app(&bundle, &setup()).unwrap();
    assert_eq!(report.errors, Vec::<String>::new());
    assert_eq!(
        report.screen,
        "Counter\nPressed 0 times\n[input: To]\n[Press]"
    );
    let press = report
        .presses
        .iter()
        .find(|p| p.button == "Press")
        .unwrap_or_else(|| panic!("no press of the button: {:?}", report.presses));
    assert_eq!(press.errors, Vec::<String>::new());
    assert!(
        press.appeared.iter().any(|l| l == "That makes 1")
            && press.appeared.iter().any(|l| l.contains("Pressed 1 times")),
        "{:?}",
        press.appeared
    );
}

#[test]
fn the_stand_in_page_has_what_a_redraw_reads_and_writes() {
    let bundle = r#"
        const root = document.getElementById("app");
        const p = document.createElement("p");
        p.setAttribute("data-key", "a");
        p.setAttribute("class", "row");
        p.append("before");
        root.append(p);
        const names = Array.from(p.attributes).map(({ name, value }) => `${name}=${value}`).join(" ");
        p.firstChild.nodeValue = `after: ${names} / ${p.getAttributeNames().join(",")}`;
        document.documentElement.dataset.ready = "true";
    "#;
    let report = try_app(bundle, &setup()).unwrap();
    assert_eq!(report.errors, Vec::<String>::new());
    assert_eq!(
        report.screen,
        "after: data-key=a class=row / data-key,class"
    );
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("glass-app-try-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
