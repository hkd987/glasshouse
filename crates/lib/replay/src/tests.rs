#![allow(clippy::unwrap_used)]

use serde_json::json;

use super::*;

fn call(tool: &str, input: Value, text: &str, data: Value, changes: bool) -> Call {
    Call {
        tool: tool.into(),
        input,
        text: text.into(),
        data,
        changes,
        day: None,
    }
}

fn message(id: &str, from: &str, subject: &str) -> Value {
    json!({ "folder": "Inbox", "from": from, "id": id, "subject": subject })
}

/// "Archive the newsletters in my Inbox", as the agent did it once: it
/// listed the Inbox and moved the ones from Vendor News.
fn newsletters(inbox: &[Value]) -> Vec<Call> {
    let mut calls = vec![call(
        "mail_list",
        json!({ "folder": "Inbox" }),
        &format!("{} messages in Inbox", inbox.len()),
        json!({ "messages": inbox }),
        false,
    )];
    for m in inbox.iter().filter(|m| m["from"] == "Vendor News") {
        calls.push(call(
            "mail_move",
            json!({ "folder": "Archive", "id": m["id"] }),
            &format!("Moved \"{}\" to Archive", m["subject"].as_str().unwrap()),
            json!({ "id": m["id"], "folder": "Archive" }),
            true,
        ));
    }
    calls
}

fn this_week() -> Vec<Value> {
    vec![
        message("m-11", "Dana Ruiz", "Renewal terms"),
        message("m-12", "Vendor News", "Your weekly digest"),
        message("m-13", "Mei Chen", "Lunch Friday?"),
        message("m-14", "Vendor News", "Ten tips for a tidier inbox"),
    ]
}

#[test]
fn nv21_calls_made_for_each_item_become_a_loop_with_the_rule_that_picked_them() {
    let replay = compile(&newsletters(&this_week())).unwrap();
    assert_eq!(
        replay.script,
        "// Written by this computer from the calls a task made, with no model.\n\
         function need(r, tool) { if (!r.ok) throw new Error(tool + \": \" + r.text); return r; }\n\
         const said = [];\n\
         const r0 = need(call(\"mail_list\", {\"folder\": \"Inbox\"}), \"mail_list\");\n\
         for (const item of r0.data.messages || []) {\n\
         \x20 if (!(item.from === \"Vendor News\")) continue;\n\
         \x20 const s0 = need(call(\"mail_move\", {\"folder\": \"Archive\", \"id\": item.id}), \"mail_move\");\n\
         \x20 said.push(s0.text);\n\
         }\n\
         return said.length ? said.join(\"\\n\") : \"Nothing to do this time.\";\n"
    );
    assert_eq!(
        replay.steps,
        [
            "mail_list {\"folder\": \"Inbox\"}",
            "For each of step 1's messages whose from is \"Vendor News\": \
             mail_move {\"folder\": \"Archive\", \"id\": item.id}",
        ]
    );
}

#[test]
fn nv21_the_same_flow_on_other_days_is_the_same_code() {
    // Next week: other messages, in other places.
    let next_week = vec![
        message("m-31", "Vendor News", "Spring sale"),
        message("m-32", "Priya Shah", "Can we move our 1:1?"),
        message("m-33", "Vendor News", "Five more tips"),
        message("m-34", "IT Help Desk", "Maintenance"),
        message("m-35", "Priya Shah", "Agenda"),
    ];
    let first = compile(&newsletters(&this_week())).unwrap();
    assert_eq!(
        compile(&newsletters(&this_week())).unwrap(),
        first,
        "deterministic"
    );
    assert_eq!(compile(&newsletters(&next_week)).unwrap(), first);
    // A limit on the listing this time: other code, the same steps. Another
    // folder, or another place to move them, isn't.
    let mut limited = newsletters(&next_week);
    limited[0].input["limit"] = json!(500);
    let limited = compile(&limited).unwrap();
    assert_ne!(limited.script, first.script);
    assert_eq!(limited.shape, first.shape);
    let mut elsewhere = newsletters(&next_week);
    elsewhere[0].input["folder"] = json!("Spam");
    assert_ne!(compile(&elsewhere).unwrap().shape, first.shape);
    let mut other_place = newsletters(&next_week);
    for c in &mut other_place[1..] {
        c.input["folder"] = json!("Trash");
    }
    assert_ne!(compile(&other_place).unwrap().shape, first.shape);
    // One newsletter among senders who all differ is one example: nothing
    // says sender is what picked it, so it's the same message again, and
    // the code differs, so the flow doesn't count as repeating.
    let one = vec![
        message("m-51", "Vendor News", "Spring sale"),
        message("m-52", "Priya Shah", "Can we move our 1:1?"),
    ];
    let literal = compile(&newsletters(&one)).unwrap();
    assert!(
        literal.script.contains("\"id\": \"m-51\""),
        "{}",
        literal.script
    );
    assert_ne!(literal, first);
}

#[test]
fn nv21_paths_made_from_a_listing_and_ids_passed_on_are_followed() {
    let calls = vec![
        call(
            "files_list",
            json!({ "path": "Invoices" }),
            "inv-1.txt\ninv-2.txt\nold",
            json!({ "entries": [
                { "dir": false, "name": "inv-1.txt" },
                { "dir": false, "name": "inv-2.txt" },
                { "dir": true, "name": "old" },
            ] }),
            false,
        ),
        call(
            "files_read",
            json!({ "path": "Invoices/inv-1.txt" }),
            "Amount: 120.00",
            json!({ "path": "Invoices/inv-1.txt" }),
            false,
        ),
        call(
            "files_read",
            json!({ "path": "Invoices/inv-2.txt" }),
            "Amount: 462.80",
            json!({ "path": "Invoices/inv-2.txt" }),
            false,
        ),
        call(
            "mail_draft",
            json!({ "body": "Two invoices are due.", "subject": "Invoices", "to": "ana@example.com" }),
            "Saved a draft to ana@example.com",
            json!({ "id": "d-17" }),
            true,
        ),
        call(
            "mail_send",
            json!({ "id": "d-17" }),
            "Sent",
            json!({}),
            true,
        ),
    ];
    let replay = compile(&calls).unwrap();
    let script = &replay.script;
    assert!(
        script.contains(
            "for (const item of r0.data.entries || []) {\n  if (!(item.dir === false)) continue;\n"
        ),
        "{script}"
    );
    assert!(
        script.contains("call(\"files_read\", {\"path\": \"Invoices/\" + item.name})"),
        "{script}"
    );
    // A draft's id, passed on to the send: next time, that run's draft.
    assert!(
        script.contains("call(\"mail_send\", {\"id\": r3.data.id})"),
        "{script}"
    );
    // Text the model wrote is the same each time, as the steps show.
    assert!(
        script.contains("\"body\": \"Two invoices are due.\""),
        "{script}"
    );
    assert_eq!(replay.steps.len(), 4);
}

#[test]
fn nv21_items_no_rule_picks_are_the_same_ones_again() {
    // The agent moved two messages for reasons of its own: no one sender,
    // no sender they all lack, no word in common.
    let inbox = this_week();
    let mut calls = newsletters(&inbox)[..1].to_vec();
    for id in ["m-11", "m-12"] {
        calls.push(call(
            "mail_move",
            json!({ "folder": "Archive", "id": id }),
            "Moved",
            json!({}),
            true,
        ));
    }
    let script = compile(&calls).unwrap().script;
    assert!(!script.contains("for ("), "{script}");
    assert!(
        script.contains("{\"folder\": \"Archive\", \"id\": \"m-11\"}"),
        "{script}"
    );
    assert!(
        script.contains("{\"folder\": \"Archive\", \"id\": \"m-12\"}"),
        "{script}"
    );
}

#[test]
fn nv21_what_cant_be_replayed_says_why() {
    let read_only = vec![call(
        "files_list",
        json!({ "path": "Invoices" }),
        "inv-1.txt",
        json!({}),
        false,
    )];
    assert!(compile(&read_only).unwrap_err().contains("only read"));
    assert!(compile(&[]).unwrap_err().contains("no tool calls"));
    let many = vec![call("files_write", json!({}), "", json!({}), true); MAX_CALLS + 1];
    assert!(compile(&many).unwrap_err().contains("at most 400"));
}

#[test]
fn nv21_rules_are_tried_in_order_and_ids_never_make_one() {
    let items = [
        json!({ "id": "a1", "status": "paid", "subject": "Invoice March" }),
        json!({ "id": "a2", "status": "unpaid", "subject": "Invoice April" }),
        json!({ "id": "a3", "status": "unpaid", "subject": "Lunch" }),
        json!({ "id": "a4", "status": "paid", "subject": "Notes" }),
        json!({ "id": "a5", "status": "void", "subject": "Draft" }),
    ];
    // One value; not one value; a word, in that order.
    assert_eq!(
        rule(&items, &[1, 2], None),
        Some(Rule::Is("status".into(), json!("unpaid")))
    );
    assert_eq!(
        rule(&items, &[4], None),
        Some(Rule::Is("status".into(), json!("void"))),
        "one example, on a field others share values of"
    );
    assert_eq!(
        rule(&items, &[1, 2, 4], None),
        Some(Rule::IsNot("status".into(), json!("paid")))
    );
    assert_eq!(
        rule(&items, &[0, 1], None),
        Some(Rule::Has("subject".into(), "invoice".into()))
    );
    // One example and only names or words pick it: no rule.
    assert_eq!(rule(&items, &[3], None), None);
    // Ids all differ: a rule on one would pick these and none ever again.
    let ids = [
        json!({ "id": "a1" }),
        json!({ "id": "a2" }),
        json!({ "id": "a3" }),
    ];
    assert_eq!(rule(&ids, &[0], None), None);
}

/// The tools of a small mailbox, for running replay code.
struct Mailbox {
    messages: Vec<Value>,
    archived: Vec<String>,
}

impl glass_script::Tools for Mailbox {
    fn call(&mut self, name: &str, input: Value) -> Result<glass_script::Reply, String> {
        Ok(match name {
            "mail_list" => glass_script::Reply {
                ok: true,
                text: format!("{} messages", self.messages.len()),
                data: json!({ "messages": self.messages }),
            },
            // Mail's own move: many at once.
            "mail_move" if input.get("ids").is_some() => {
                let ids: Vec<String> = serde_json::from_value(input["ids"].clone()).unwrap();
                if ids.is_empty() {
                    return Ok(glass_script::Reply {
                        ok: false,
                        text: "Name at least one message".into(),
                        data: Value::Null,
                    });
                }
                self.messages
                    .retain(|m| !ids.iter().any(|id| m["id"] == *id));
                self.archived.extend(ids.iter().cloned());
                glass_script::Reply {
                    ok: true,
                    text: format!(
                        "Moved {} messages to {}",
                        ids.len(),
                        input["to"].as_str().unwrap()
                    ),
                    data: Value::Null,
                }
            }
            "mail_move" => {
                let id = input["id"].as_str().unwrap_or_default().to_owned();
                match self.messages.iter().position(|m| m["id"] == id) {
                    Some(at) => {
                        let gone = self.messages.remove(at);
                        self.archived.push(id);
                        glass_script::Reply {
                            ok: true,
                            text: format!(
                                "Moved \"{}\" to {}",
                                gone["subject"].as_str().unwrap(),
                                input["folder"].as_str().unwrap()
                            ),
                            data: Value::Null,
                        }
                    }
                    None => glass_script::Reply {
                        ok: false,
                        text: format!("No message {id}"),
                        data: Value::Null,
                    },
                }
            }
            other => glass_script::Reply {
                ok: false,
                text: format!("There's no tool {other:?} here."),
                data: Value::Null,
            },
        })
    }
}

#[test]
fn nv21_the_code_runs_on_next_weeks_mail_and_stops_when_a_call_fails() {
    let replay = compile(&newsletters(&this_week())).unwrap();
    let next_week = vec![
        message("m-31", "Vendor News", "Spring sale"),
        message("m-32", "Priya Shah", "Can we move our 1:1?"),
        message("m-33", "Vendor News", "Five more tips"),
    ];
    let run = glass_script::run(
        &replay.script,
        "2026-10-29",
        Box::new(Mailbox {
            messages: next_week,
            archived: Vec::new(),
        }),
    )
    .unwrap();
    assert_eq!(
        run.result,
        json!("Moved \"Spring sale\" to Archive\nMoved \"Five more tips\" to Archive")
    );
    assert_eq!(run.calls, 3);
    // Nothing to do: it says so.
    let quiet = glass_script::run(
        &replay.script,
        "2026-11-05",
        Box::new(Mailbox {
            messages: vec![message("m-41", "Mei Chen", "Thanks")],
            archived: Vec::new(),
        }),
    )
    .unwrap();
    assert_eq!(quiet.result, json!("Nothing to do this time."));
    // A call that fails stops it, for the agent to take over.
    let calls = vec![call(
        "mail_move",
        json!({ "folder": "Archive", "id": "m-99" }),
        "Moved",
        json!({}),
        true,
    )];
    let err = glass_script::run(
        &compile(&calls).unwrap().script,
        "2026-11-05",
        Box::new(Mailbox {
            messages: Vec::new(),
            archived: Vec::new(),
        }),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("mail_move: No message m-99"),
        "{err}"
    );
}

#[test]
fn nv21_a_list_made_from_a_listings_items_is_that_listing_kept_by_the_rule() {
    // Mail's move takes many ids at once: the agent moved the newsletters
    // in one call.
    let inbox = this_week();
    let calls = vec![
        newsletters(&inbox).remove(0),
        call(
            "mail_move",
            json!({ "ids": ["m-12", "m-14"], "to": "archive" }),
            "Moved 2 messages to archive",
            json!({}),
            true,
        ),
    ];
    let replay = compile(&calls).unwrap();
    assert!(
        replay.script.contains(
            "const l1_0 = (r0.data.messages || []).filter(item => item.from === \"Vendor News\").map(item => item.id);\n\
             const r1 = l1_0.length ? need(call(\"mail_move\", {\"ids\": l1_0, \"to\": \"archive\"}), \"mail_move\") : { ok: true, text: \"\", data: {} };\n\
             if (l1_0.length) said.push(r1.text);"
        ),
        "{}",
        replay.script
    );
    assert_eq!(
        replay.steps[1],
        "mail_move {\"ids\": (r0.data.messages || []).filter(item => item.from === \"Vendor News\").map(item => item.id), \"to\": \"archive\"}"
    );
    let next_week = vec![
        message("m-31", "Vendor News", "Spring sale"),
        message("m-32", "Priya Shah", "Can we move our 1:1?"),
        message("m-33", "Vendor News", "Five more tips"),
    ];
    let run = glass_script::run(
        &replay.script,
        "2026-10-29",
        Box::new(Mailbox {
            messages: next_week,
            archived: Vec::new(),
        }),
    )
    .unwrap();
    assert_eq!(run.result, json!("Moved 2 messages to archive"));
    // None this time: the call is skipped, not made empty.
    let quiet = glass_script::run(
        &replay.script,
        "2026-11-05",
        Box::new(Mailbox {
            messages: vec![
                message("m-41", "Mei Chen", "Thanks"),
                message("m-42", "Mei Chen", "Again"),
            ],
            archived: Vec::new(),
        }),
    )
    .unwrap();
    assert_eq!(quiet.result, json!("Nothing to do this time."));
    assert_eq!(quiet.calls, 1);
}

#[test]
fn nv21_what_mail_and_files_say_never_becomes_code() {
    // A subject, a field name and a word from a message: each only ever a
    // string in the code.
    let evil = "\"); throw new Error(\"pwned\"); (\"";
    let field = "a\"]; throw 1; x[\"";
    let inbox = vec![
        json!({ "id": "m-1", "from": evil, field: "x", "subject": evil }),
        json!({ "id": "m-2", "from": "Mei", field: "y", "subject": "Lunch" }),
        json!({ "id": "m-3", "from": evil, field: "z", "subject": evil }),
    ];
    let mut calls = vec![call(
        "mail_list",
        json!({ "folder": evil }),
        "3 messages",
        json!({ "messages": inbox }),
        false,
    )];
    for id in ["m-1", "m-3"] {
        calls.push(call(
            "mail_move",
            json!({ evil: id, "folder": evil }),
            "Moved",
            json!({}),
            true,
        ));
    }
    let script = compile(&calls).unwrap().script;
    // Every occurrence is inside a JSON string, quotes escaped.
    assert!(!script.contains("throw new Error(\"pwned\")"), "{script}");
    assert!(!script.contains("throw 1;"), "{script}");
    let run = glass_script::run(
        &script,
        "2026-10-22",
        Box::new(Mailbox {
            messages: Vec::new(),
            archived: Vec::new(),
        }),
    );
    // It runs as code that calls tools, and never as the message's.
    assert!(
        !run.as_ref().is_err_and(|e| e.to_string().contains("pwned")),
        "{run:?}"
    );
}

#[test]
fn adr0046_numbers_make_a_rule_over_or_under_a_round_one() {
    let items = [
        json!({ "id": "i1", "client": "Acme", "amount": 1250 }),
        json!({ "id": "i2", "client": "Birch", "amount": 400 }),
        json!({ "id": "i3", "client": "Cole", "amount": 3200.5 }),
        json!({ "id": "i4", "client": "Dune", "amount": 980 }),
        json!({ "id": "i5", "client": "Elm", "amount": "1800" }),
    ];
    let over = rule(&items, &[0, 2, 4], None).unwrap();
    assert_eq!(over, Rule::Over("amount".into(), 1000.0));
    assert_eq!(rule_words(&over), " whose amount is over 1,000");
    assert_eq!(rule_js(&over), "Number(item.amount) > 1000");
    assert_eq!(
        rule(&items, &[1, 3], None),
        Some(Rule::Under("amount".into(), 1000.0))
    );
    // Not split by any number, nor by one example: no rule.
    assert_eq!(rule(&items, &[0, 1], None), None);
    assert_eq!(rule(&items, &[2], None), None);
    // The roundest number between, nearest the middle.
    assert_eq!(round_between(120.0, 4500.0, true, false), Some(1000.0));
    assert_eq!(round_between(25.0, 6000.0, true, false), Some(1000.0));
    assert_eq!(round_between(1210.0, 1260.0, true, false), Some(1230.0));
    assert_eq!(round_between(12.31, 12.38, true, false), Some(12.34));
    assert_eq!(round_between(-5.0, 30.0, true, false), Some(0.0));
    assert_eq!(round_between(5.0, 5.0, true, false), None);
    assert_eq!(number_words(1_234_567.5), "1,234,567.5");
    assert_eq!(number(&json!("12.50")), Some(12.5));
    assert_eq!(number(&json!("1,250")), None, "as written, or not a number");
}

#[test]
fn adr0046_dates_make_a_rule_on_days_before_the_day_it_runs() {
    let today = day_number("2026-10-23");
    let due = |dates: &[&str]| -> Vec<Value> {
        dates
            .iter()
            .enumerate()
            .map(|(i, d)| json!({ "id": format!("inv-{i}"), "due": d }))
            .collect()
    };
    // Overdue: before today, whatever the gap.
    let items = due(&[
        "2026-10-01",
        "2026-10-20",
        "2026-10-23",
        "2026-11-05",
        "2026-10-30T09:00:00Z",
    ]);
    let overdue = rule(&items, &[0, 1], today).unwrap();
    assert_eq!(overdue, Rule::Older("due".into(), 0));
    assert_eq!(rule_words(&overdue), " whose due is before today");
    assert_eq!(rule_js(&overdue), "daysBetween(item.due, today()) > 0");
    // Today or later: the others.
    assert_eq!(
        rule(&items, &[2, 3, 4], today),
        Some(Rule::Newer("due".into(), 0))
    );
    // Old mail: the round number of days nearest the gap's middle.
    let mail = due(&[
        "2026-09-08",
        "2026-08-24",
        "2026-07-25",
        "2026-10-21",
        "2026-10-18",
        "2026-10-13",
    ]);
    let old = rule(&mail, &[0, 1, 2], today).unwrap();
    assert_eq!(old, Rule::Older("due".into(), 30));
    assert_eq!(rule_words(&old), " whose due is more than 30 days ago");
    assert_eq!(
        rule(&mail, &[3, 4, 5], today),
        Some(Rule::Newer("due".into(), 30))
    );
    // Not knowing the day it was listed, no rule on a date.
    assert_eq!(rule(&items, &[0, 1], None), None);
    // Day numbers both ways, as the script runner counts days.
    assert_eq!(today, Some(20_749));
    assert_eq!(local_day(0, 0), "1970-01-01");
    assert_eq!(local_day(1_792_674_030, 0), "2026-10-22");
    assert_eq!(local_day(1_792_674_030 + 12 * 3600, 0), "2026-10-23");
    assert_eq!(
        local_day(1_792_674_030 + 12 * 3600, -4 * 3600),
        "2026-10-22"
    );
    assert_eq!(day_number("2024-02-29"), Some(19_782));
    for wrong in [
        "2026-02-30",
        "2026-13-01",
        "20261023",
        "2026-10-23x",
        "Oct 23",
    ] {
        assert_eq!(day_number(wrong), None, "{wrong}");
    }
}

/// An invoicing app's tools: its invoices, and a reminder for one.
struct Ledger {
    invoices: Vec<Value>,
    reminded: Vec<String>,
}

impl glass_script::Tools for Ledger {
    fn call(&mut self, name: &str, input: Value) -> Result<glass_script::Reply, String> {
        Ok(match name {
            "invoices_list" => glass_script::Reply {
                ok: true,
                text: format!("{} invoices", self.invoices.len()),
                data: json!({ "invoices": self.invoices }),
            },
            "invoices_remind" => {
                let id = input["id"].as_str().unwrap().to_owned();
                self.reminded.push(id.clone());
                glass_script::Reply {
                    ok: true,
                    text: format!("Reminded {id}"),
                    data: Value::Null,
                }
            }
            other => glass_script::Reply {
                ok: false,
                text: format!("There's no tool {other:?} here."),
                data: Value::Null,
            },
        })
    }
}

fn invoice(id: &str, amount: f64, due: &str) -> Value {
    json!({ "id": id, "client": "Acme", "amount": amount, "due": due })
}

#[test]
fn adr0046_reminders_for_overdue_invoices_run_on_a_later_day_by_that_day() {
    // "Remind clients of overdue invoices": the agent reminded the two
    // past their due date on 23 October.
    let listed = vec![
        invoice("inv-1", 1200.0, "2026-10-02"),
        invoice("inv-2", 300.0, "2026-10-30"),
        invoice("inv-3", 800.0, "2026-10-19"),
        invoice("inv-4", 2500.0, "2026-11-12"),
    ];
    let mut calls = vec![Call {
        day: Some("2026-10-23".into()),
        ..call(
            "invoices_list",
            json!({}),
            "4 invoices",
            json!({ "invoices": listed }),
            false,
        )
    }];
    for id in ["inv-1", "inv-3"] {
        calls.push(call(
            "invoices_remind",
            json!({ "id": id }),
            &format!("Reminded {id}"),
            Value::Null,
            true,
        ));
    }
    let replay = compile(&calls).unwrap();
    assert!(
        replay
            .script
            .contains("if (!(daysBetween(item.due, today()) > 0)) continue;"),
        "{}",
        replay.script
    );
    assert!(
        replay
            .steps
            .iter()
            .any(|s| s.contains("whose due is before today")),
        "{:?}",
        replay.steps
    );
    // Two weeks on: what's overdue then, by that day.
    let ledger = Ledger {
        invoices: vec![
            invoice("inv-2", 300.0, "2026-10-30"),
            invoice("inv-4", 2500.0, "2026-11-12"),
            invoice("inv-5", 640.0, "2026-11-20"),
        ],
        reminded: Vec::new(),
    };
    let run = glass_script::run(&replay.script, "2026-11-06", Box::new(ledger)).unwrap();
    assert_eq!(run.result, json!("Reminded inv-2"));
    // The same calls on another day compile the same.
    assert_eq!(compile(&calls).unwrap(), replay);
}
