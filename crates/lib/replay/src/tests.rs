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
        rule(&items, &[1, 2]),
        Some(Rule::Is("status".into(), json!("unpaid")))
    );
    assert_eq!(
        rule(&items, &[4]),
        Some(Rule::Is("status".into(), json!("void"))),
        "one example, on a field others share values of"
    );
    assert_eq!(
        rule(&items, &[1, 2, 4]),
        Some(Rule::IsNot("status".into(), json!("paid")))
    );
    assert_eq!(
        rule(&items, &[0, 1]),
        Some(Rule::Has("subject".into(), "invoice".into()))
    );
    // One example and only names or words pick it: no rule.
    assert_eq!(rule(&items, &[3]), None);
    // Ids all differ: a rule on one would pick these and none ever again.
    let ids = [
        json!({ "id": "a1" }),
        json!({ "id": "a2" }),
        json!({ "id": "a3" }),
    ];
    assert_eq!(rule(&ids, &[0]), None);
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
