#![allow(clippy::unwrap_used)]

use super::*;

/// The demo invoices, as `files_list` and `files_read` return them.
struct Invoices {
    seen: Vec<(String, Value)>,
    gone: bool,
}

impl Invoices {
    fn new() -> Box<Self> {
        Box::new(Self {
            seen: Vec::new(),
            gone: false,
        })
    }
}

const INVOICES: [(&str, &str, &str, &str); 3] = [
    ("inv-1.txt", "120.00", "2026-10-01", "unpaid"),
    ("inv-2.txt", "1840.50", "2026-10-05", "paid"),
    ("inv-3.txt", "462.80", "2026-09-30", "unpaid"),
];

impl Tools for Invoices {
    fn call(&mut self, name: &str, input: Value) -> Result<Reply, String> {
        if self.gone {
            return Err("the agent stopped".into());
        }
        self.seen.push((name.to_owned(), input.clone()));
        Ok(match name {
            "files_list" => Reply {
                ok: true,
                text: INVOICES.iter().map(|i| i.0).collect::<Vec<_>>().join("\n"),
                data: Value::Null,
            },
            "files_read" => {
                let path = input["path"].as_str().unwrap_or_default();
                match INVOICES.iter().find(|i| path.ends_with(i.0)) {
                    Some((_, amount, due, status)) => Reply {
                        ok: true,
                        text: format!("Amount: {amount}\nDue: {due}\nStatus: {status}\n"),
                        data: Value::Null,
                    },
                    None => Reply {
                        ok: false,
                        text: format!("No such file: {path}"),
                        data: Value::Null,
                    },
                }
            }
            "mail_list" => Reply {
                ok: true,
                text: "2 messages in Inbox".into(),
                data: json!({ "messages": [{ "from": "Dana" }, { "from": "Priya" }] }),
            },
            _ => Reply {
                ok: false,
                text: format!("There's no tool {name:?} here."),
                data: Value::Null,
            },
        })
    }
}

fn ran(source: &str) -> Result<Run, ScriptError> {
    run(source, "2026-10-22", Invoices::new())
}

#[test]
fn adr0044_exact_figures_come_from_the_tools_data() {
    // What Haiku got wrong by hand: days overdue, weighted, summed.
    let run = ran(r#"
        const names = call("files_list", { path: "Invoices" }).text.split("\n");
        let owed = 0, weighted = 0;
        for (const name of names) {
          const text = call("files_read", { path: "Invoices/" + name }).text;
          const field = (k) => text.match(new RegExp(k + ": (.*)"))[1];
          if (field("Status") !== "unpaid") continue;
          const amount = parseFloat(field("Amount"));
          owed += amount;
          weighted += amount * daysBetween(field("Due"), today());
          print(name, daysBetween(field("Due"), today()));
        }
        ({ owed: round(owed), weighted: round(weighted) })
    "#)
    .unwrap();
    // 120.00 x 21 + 462.80 x 22 = 12,701.60
    assert_eq!(run.result, json!({ "owed": 582.8, "weighted": 12701.6 }));
    assert_eq!(run.printed, ["inv-1.txt 21", "inv-3.txt 22"]);
    assert_eq!(run.calls, 4);
}

#[test]
fn adr0044_a_tools_data_and_failures_reach_the_script() {
    let run = ran(r#"
        const mail = call("mail_list", { folder: "Inbox" });
        const listed = call("files_list", { path: "Invoices", limit: 10 });
        const missing = call("files_read", { path: "nope.txt" });
        const odd = call("shell_run", { cmd: "rm -rf /" });
        [mail.data.messages.map((m) => m.from), missing.ok, missing.text, odd.ok]
    "#)
    .unwrap();
    assert_eq!(
        run.result,
        json!([["Dana", "Priya"], false, "No such file: nope.txt", false])
    );
}

#[test]
fn adr0044_a_script_reaches_nothing_but_its_tools() {
    // No module system: an import never loads anything.
    for source in [r#"import("/etc/passwd")"#, r#"import("./notes.js")"#] {
        let err = ran(source).unwrap_err();
        assert!(matches!(err, ScriptError::Script(_)), "{source}: {err:?}");
    }
    assert!(matches!(
        ran(r#"import fs from "fs"; 1"#).unwrap_err(),
        ScriptError::Script(_)
    ));
    // Nothing of a host: no require, process, fetch, timers or sockets.
    let run = ran(
        "[typeof require, typeof process, typeof fetch, typeof XMLHttpRequest, \
          typeof setTimeout, typeof WebSocket, typeof Deno, typeof Bun]",
    )
    .unwrap();
    assert_eq!(run.result, json!(vec!["undefined"; 8]));
}

#[test]
fn adr0044_a_runaway_script_stops() {
    for source in [
        "while (true) {}",
        "for (;;) {}",
        "function f() { return f(); } f()",
    ] {
        let err = ran(source).unwrap_err();
        assert!(err.to_string().contains("RuntimeLimit"), "{source}: {err}");
    }
    let err = ran(r#"for (let i = 0; i < 200; i++) call("files_list", {})"#).unwrap_err();
    assert!(err.to_string().contains("at most 100 tool calls"), "{err}");
}

#[test]
fn adr0044_the_host_failing_stops_the_script_even_if_it_catches() {
    let mut tools = Invoices::new();
    tools.gone = true;
    let err = run(
        r#"try { call("files_list", {}) } catch (e) { "carried on" }"#,
        "2026-10-22",
        tools,
    )
    .unwrap_err();
    assert_eq!(err, ScriptError::Host("the agent stopped".into()));
}

#[test]
fn adr0044_mistakes_say_where() {
    let err = ran("let x = 1 2").unwrap_err();
    let ScriptError::Script(message) = err else {
        panic!("{err:?}")
    };
    assert!(message.contains("SyntaxError"), "{message}");
    assert!(message.contains("line 1"), "{message}");
    let err = ran(r#"call(42)"#).unwrap_err();
    assert!(err.to_string().contains("the tool is a name"), "{err}");
    assert!(
        ran("throw new Error('x'.repeat(5000))")
            .unwrap_err()
            .to_string()
            .ends_with('…')
    );
}

#[test]
fn adr0044_helpers_for_dates_and_money() {
    let run = ran(r#"
        console.log("logged", { n: 1 });
        [today(), daysBetween("2026-09-30", "2026-10-22"), daysBetween("2026-10-22T09:30", "2026-10-01"),
         addDays("2026-10-22", 10), weekday("2026-10-22"), round(120 + 310.25 + 462.8), round(2 / 3, 3)]
    "#)
    .unwrap();
    assert_eq!(
        run.result,
        json!([
            "2026-10-22",
            22,
            -21,
            "2026-11-01",
            "Thursday",
            893.05,
            0.667
        ])
    );
    assert_eq!(run.printed, [r#"logged {"n":1}"#]);
    assert!(
        ran(r#"daysBetween("soon", today())"#)
            .unwrap_err()
            .to_string()
            .contains("not a date")
    );
    // An async script's result is what it resolved to.
    assert_eq!(ran("(async () => 6 * 7)()").unwrap().result, json!(42));
    assert!(run_on_date("not-a-date").is_err());
}

fn run_on_date(today: &str) -> Result<Run, ScriptError> {
    run("1", today, Invoices::new())
}

#[test]
fn adr0044_whole_numbers_cross_as_integers() {
    // A tool whose schema asks for an integer gets 10, not 10.0.
    assert_eq!(
        tidy(json!({ "limit": 10.0, "share": 0.5, "items": [3.0, -2.0], "big": 1e300 })),
        json!({ "limit": 10, "share": 0.5, "items": [3, -2], "big": 1e300 })
    );
    assert_eq!(ran("[1 + 1, 7 / 2]").unwrap().result, json!([2, 3.5]));
}

#[test]
fn adr0044_scripts_as_models_write_them() {
    // A result returned, as from a function body, with a helper of its own.
    let run = ran(r#"
        function round(x) { return Math.floor(x); }
        const today = "my own";
        let console = { log() {} };
        const total = [1.7, 2.6].reduce((a, b) => a + b, 0);
        return { total: round(total), today };
    "#)
    .unwrap();
    assert_eq!(run.result, json!({ "total": 4, "today": "my own" }));
    // A `return` that isn't the problem keeps the first error.
    let err = ran("return 1 2").unwrap_err();
    assert!(err.to_string().contains("return"), "{err}");
}
