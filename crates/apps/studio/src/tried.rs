//! Trying a version as it's saved (GA-1, ADR 0049): what the agent that
//! wrote it hears. glass-app-try opens the built app with sample data
//! shaped as each declared action's result, types into its fields, presses
//! each kind of button once and reports what it saw; here that becomes the save's words: what the app
//! showed, what it called and with what, and the problems to fix, with
//! each call's input checked against what its action takes.

use serde_json::{Value, json};

use crate::Capability;

/// The longest call input quoted, and the most screen lines shown.
const QUOTED_CHARS: usize = 160;
const SHOWN_LINES: usize = 12;

/// What glass-app-try is given for `manifest`'s app: each declared action
/// it may call, with sample data shaped as its result.
pub(crate) fn setup(
    app: &str,
    name: &str,
    user: &str,
    permissions: Vec<String>,
    declared: &[glass_types::ActionName],
    capabilities: &[Capability],
    today: &str,
) -> Value {
    let actions: serde_json::Map<String, Value> = declared
        .iter()
        .filter_map(|a| capabilities.iter().find(|c| &c.name == a))
        .map(|c| {
            (
                c.name.to_string(),
                json!({
                    "title": c.title,
                    "tier": c.tier,
                    "side_effect": c.side_effect,
                    "sample": c.returns.as_ref().map(|r| glass_actions::shape::sample(r, today)),
                }),
            )
        })
        .collect();
    json!({ "app": app, "name": name, "user": user, "permissions": permissions, "actions": actions })
}

/// How a try went, in words, and the problems found.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tried {
    pub text: String,
    pub problems: Vec<String>,
}

pub(crate) fn tell(
    report: &Value,
    declared: &[glass_types::ActionName],
    capabilities: &[Capability],
) -> Tried {
    let mut problems: Vec<String> = strings(&report["errors"]);
    let mut called: Vec<String> = Vec::new();
    let mut calls = |calls: &Value, problems: &mut Vec<String>| -> Vec<String> {
        calls
            .as_array()
            .into_iter()
            .flatten()
            .map(|call| {
                let action = call["action"].as_str().unwrap_or_default();
                called.push(action.to_owned());
                let input = &call["input"];
                match declared.iter().find(|a| a.as_str() == action) {
                    None => problems.push(format!(
                        "it calls {action}, which its manifest doesn't declare: the call is refused"
                    )),
                    Some(_) => {
                        if let Some(c) = capabilities.iter().find(|c| c.name.as_str() == action) {
                            for wrong in glass_actions::shape::fits(input, &c.input) {
                                problems.push(format!("{action}'s {wrong}"));
                            }
                        }
                    }
                }
                format!("{action} {}", quote(input))
            })
            .collect()
    };
    let opened = calls(&report["calls"], &mut problems);
    let screen = report["screen"].as_str().unwrap_or_default();
    let mut text = String::from(
        "Tried here, with sample data in place of the person's; nothing was really called.",
    );
    if screen.trim().is_empty() {
        problems.push("it showed nothing when it opened".into());
    } else {
        let lines: Vec<&str> = screen.lines().collect();
        text.push_str("\nOpening it showed:");
        for line in lines.iter().take(SHOWN_LINES) {
            text.push_str(&format!("\n| {line}"));
        }
        if lines.len() > SHOWN_LINES {
            text.push_str(&format!("\n| … {} more lines", lines.len() - SHOWN_LINES));
        }
    }
    if !opened.is_empty() {
        text.push_str(&format!("\nIt called {}.", opened.join(", ")));
    }
    for press in report["presses"].as_array().into_iter().flatten() {
        let button = press["button"].as_str().unwrap_or_default();
        let made = calls(&press["calls"], &mut problems);
        let appeared = strings(&press["appeared"]);
        let typed: Vec<String> = press["typed"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                format!(
                    "{} into [{}]",
                    quote(&t["text"]),
                    t["field"].as_str().unwrap_or_default()
                )
            })
            .collect();
        let mut line = match (typed.as_slice(), button) {
            ([], _) => format!("\nPressing [{button}] "),
            (typed, "") => format!("\nTyping {} ", and(typed)),
            (typed, _) => format!("\nTyping {}, then pressing [{button}] ", and(typed)),
        };
        line.push_str(&match made.as_slice() {
            [] => "called nothing".to_owned(),
            made => format!("called {}", made.join(", ")),
        });
        if !appeared.is_empty() {
            line.push_str(&format!(", then it showed \"{}\"", appeared.join(" / ")));
        }
        text.push_str(&line);
        text.push('.');
        problems.extend(strings(&press["errors"]).into_iter().map(|e| match button {
            "" => format!("typing: {e}"),
            _ => format!("pressing [{button}]: {e}"),
        }));
    }
    // What it called with no shape to sample: what it shows from them is untried.
    let unshaped: Vec<&str> = declared
        .iter()
        .map(|a| a.as_str())
        .filter(|a| called.iter().any(|c| c == a))
        .filter(|a| {
            capabilities
                .iter()
                .any(|c| c.name.as_str() == *a && c.returns.is_none())
        })
        .collect();
    if !unshaped.is_empty() {
        text.push_str(&format!(
            "\n{} {} no description of {} data yet, so the try gave {} none: what the app shows from {} is untried.",
            unshaped.join(" and "),
            if unshaped.len() == 1 { "has" } else { "have" },
            if unshaped.len() == 1 { "its" } else { "their" },
            if unshaped.len() == 1 { "it" } else { "them" },
            if unshaped.len() == 1 { "it" } else { "them" },
        ));
    }
    let logged = strings(&report["console_errors"]);
    if !logged.is_empty() {
        text.push_str(&format!("\nIt logged errors: {}.", logged.join("; ")));
    }
    problems.dedup();
    if problems.is_empty() {
        text.push_str("\nNo problems when it was tried.");
    } else {
        text.push_str("\nProblems to fix, then save again:");
        for problem in &problems {
            text.push_str(&format!("\n- {problem}"));
        }
    }
    Tried { text, problems }
}

/// "a", "a and b", "a, b and c".
fn and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn quote(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() <= QUOTED_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(QUOTED_CHARS).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use glass_types::{SideEffect, Tier};

    use super::*;

    fn capabilities() -> Vec<Capability> {
        vec![
            Capability {
                name: "mail.list".parse().unwrap(),
                title: "List mail".into(),
                description: String::new(),
                side_effect: SideEffect::None,
                tier: Tier::Auto,
                input: json!({ "type": "object", "properties": { "folder": { "enum": ["inbox", "drafts"] }, "limit": { "type": "integer" } }, "required": ["folder"], "additionalProperties": false }),
                returns: Some(
                    json!({ "type": "object", "properties": { "messages": { "type": "array", "items": { "type": "object", "properties": { "subject": { "type": "string" } } } } } }),
                ),
            },
            Capability {
                name: "mail.draft".parse().unwrap(),
                title: "Draft an email".into(),
                description: String::new(),
                side_effect: SideEffect::LocalReversible,
                tier: Tier::Notify,
                input: json!({ "type": "object", "properties": { "to": { "type": "array", "items": { "type": "string" } } }, "required": ["to"] }),
                returns: None,
            },
        ]
    }

    #[test]
    fn ga1_the_setup_has_sample_data_for_what_the_app_declares() {
        let declared = ["mail.list".parse().unwrap()];
        let made = setup(
            "my-today",
            "Today",
            "demo",
            vec![],
            &declared,
            &capabilities(),
            "2026-10-23",
        );
        assert_eq!(
            made["actions"]["mail.list"]["sample"]["messages"][0]["subject"],
            "Sample subject 1"
        );
        assert_eq!(made["actions"]["mail.list"]["tier"], "auto");
        assert!(
            made["actions"].get("mail.draft").is_none(),
            "only what it declares"
        );
    }

    #[test]
    fn ga1_a_try_says_what_it_saw_and_what_to_fix() {
        let declared = ["mail.list".parse().unwrap(), "mail.draft".parse().unwrap()];
        let report = json!({
            "errors": [],
            "console_errors": [],
            "calls": [{ "action": "mail.list", "input": { "folder": "Inbox", "limit": 500 } }],
            "screen": "Today\nInbox: 2 emails\n09:00 Sample title 1[Running late]",
            "presses": [
                { "button": "Running late", "calls": [{ "action": "mail.draft", "input": { "to": [] } }], "errors": [], "appeared": ["Draft saved"] },
                { "button": "Send", "typed": [{ "field": "To", "text": "sam@example.com" }, { "field": "Subject", "text": "Sample subject" }], "calls": [], "errors": [], "appeared": [] },
                { "button": "Refresh", "calls": [{ "action": "calendar.list", "input": {} }], "errors": ["on click: TypeError: x is undefined"], "appeared": [] }
            ]
        });
        let tried = tell(&report, &declared, &capabilities());
        assert_eq!(
            tried.problems,
            [
                "mail.list's input.folder is \"Inbox\", not one of [\"inbox\",\"drafts\"]",
                "it calls calendar.list, which its manifest doesn't declare: the call is refused",
                "pressing [Refresh]: on click: TypeError: x is undefined",
            ]
        );
        assert!(tried.text.contains("Opening it showed:\n| Today\n| Inbox: 2 emails\n| 09:00 Sample title 1[Running late]"), "{}", tried.text);
        assert!(
            tried
                .text
                .contains("It called mail.list {\"folder\":\"Inbox\",\"limit\":500}."),
            "{}",
            tried.text
        );
        assert!(tried.text.contains("Pressing [Running late] called mail.draft {\"to\":[]}, then it showed \"Draft saved\"."), "{}", tried.text);
        assert!(
            tried.text.contains(
                "Typing \"sam@example.com\" into [To] and \"Sample subject\" into [Subject], then pressing [Send] called nothing."
            ),
            "{}",
            tried.text
        );
        assert!(
            tried
                .text
                .ends_with("- pressing [Refresh]: on click: TypeError: x is undefined"),
            "{}",
            tried.text
        );
        assert!(
            tried.text.contains("mail.draft has no description of its data yet, so the try gave it none: what the app shows from it is untried."),
            "{}",
            tried.text
        );

        let quiet = tell(
            &json!({ "errors": ["when it opened: ReferenceError: bridge is not defined"], "screen": "", "calls": [], "presses": [] }),
            &declared,
            &capabilities(),
        );
        assert_eq!(
            quiet.problems,
            [
                "when it opened: ReferenceError: bridge is not defined",
                "it showed nothing when it opened"
            ]
        );
    }
}
