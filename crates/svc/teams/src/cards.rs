//! Approval cards (ADR 0065 §13): an Adaptive Card per approval, with
//! Universal Actions so that answering it, here or anywhere else, changes
//! it for everyone who sees it.
//!
//! Followed: "Universal Action Model"
//! (<https://learn.microsoft.com/en-us/adaptive-cards/authoring-cards/universal-action-model>),
//! Teams' "Use Universal Actions for Adaptive Cards"
//! (<https://learn.microsoft.com/en-us/microsoftteams/platform/task-modules-and-cards/cards/universal-actions-for-adaptive-cards/work-with-universal-actions-for-adaptive-cards>),
//! "Up to date views"
//! (<https://learn.microsoft.com/en-us/microsoftteams/platform/task-modules-and-cards/cards/universal-actions-for-adaptive-cards/up-to-date-views>)
//! and Adaptive Cards' text features
//! (<https://learn.microsoft.com/en-us/adaptive-cards/authoring-cards/text-features>).
//!
//! - The card is version 1.5, which Teams needs for `Action.Execute` and
//!   `refresh`. Approve and Don't are `Action.Execute` whose data is the
//!   approval's id and nothing else; the verb says which.
//! - A pending card's `refresh` names the people who may answer (their
//!   Teams ids, at most 60): when they look at it, Teams asks the app for
//!   its current state, so a card answered elsewhere loses its buttons.
//!   Answered or expired, it has no buttons and no refresh.
//! - Answered in Teams, the invoke's response carries the new card
//!   ([`invoke_response`]) and the front also updates the message
//!   (`PUT …/activities/<id>`) for everyone else.
//! - What an agent wrote is shown as `TextRun`s, which Adaptive Cards never
//!   read as Markdown: an agent can't put a link on a card.
//! - A confidential approval's card (labels, ADR 0018) says only "<Agent>
//!   needs you" and Open: never its summary, detail, action or button
//!   label.

use glass_agents_api::{ApprovalState, ApprovalView, Channel};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// An Adaptive Card's content type, in an attachment or an invoke's
/// response.
pub const CARD_CONTENT_TYPE: &str = "application/vnd.microsoft.card.adaptive";
/// The version Universal Actions need in Teams.
pub const CARD_VERSION: &str = "1.5";
pub const VERB_APPROVE: &str = "approve";
pub const VERB_DENY: &str = "deny";
/// The verb of a card's `refresh`.
pub const VERB_REFRESH: &str = "refresh";
/// The most people a card refreshes itself for.
pub const MAX_REFRESH_USERS: usize = 60;

/// What a card needs besides the approval.
#[derive(Debug, Clone, Copy)]
pub struct CardContext<'a> {
    /// The agent's name: "Reconciler".
    pub agent_name: &'a str,
    /// The approval in the web app: Open goes there.
    pub open_url: &'a str,
    /// Teams ids (`29:…`) of the people who may answer: the person, and
    /// any delegate. The card refreshes itself when they look at it.
    pub refresh_for: &'a [String],
    /// Whether the person looking may answer: buttons only if so. Always
    /// true for the card sent to the person; for a refresh, whether the
    /// viewer is the person or a delegate.
    pub answerable: bool,
    /// Who answered, as people know them, when the approval's `by` is an
    /// account name.
    pub answered_by: Option<&'a str>,
}

/// The card for `view` in its current state.
pub fn approval_card(view: &ApprovalView, ctx: &CardContext<'_>) -> Value {
    let agent = ctx.agent_name;
    let mut body = Vec::new();
    let mut actions = Vec::new();
    let mut refresh = None;
    match &view.state {
        ApprovalState::Pending => {
            if view.confidential {
                body.push(runs(&[bold(agent), plain(" needs you")]));
            } else {
                body.push(runs(&[bold(agent), plain(" asks")]));
                body.extend(request(view));
                if ctx.answerable {
                    actions.push(execute(&view.yes_label, VERB_APPROVE, &view.id, "positive"));
                    actions.push(execute("Don't", VERB_DENY, &view.id, "default"));
                }
            }
            actions.push(json!({"type": "Action.OpenUrl", "title": "Open", "url": ctx.open_url}));
            let users: Vec<&String> = ctx.refresh_for.iter().take(MAX_REFRESH_USERS).collect();
            refresh = Some(json!({
                "action": {
                    "type": "Action.Execute",
                    "title": "Refresh",
                    "verb": VERB_REFRESH,
                    "data": {"approval": view.id},
                },
                "userIds": users,
            }));
        }
        ApprovalState::Approved { by, at, via } | ApprovalState::Denied { by, at, via } => {
            let approved = matches!(view.state, ApprovalState::Approved { .. });
            body.push(heading(view, agent, " asked"));
            if !view.confidential {
                body.extend(request(view));
            }
            let who = ctx.answered_by.unwrap_or(by);
            let outcome = if approved {
                json!({"type": "TextRun", "text": "Approved", "weight": "Bolder", "color": "Good"})
            } else {
                json!({"type": "TextRun", "text": "Denied", "weight": "Bolder", "color": "Attention"})
            };
            let mut line = vec![
                outcome,
                plain(" by "),
                bold(who),
                plain(&format!(" {}", place(*via))),
            ];
            if *via != Channel::Teams {
                line.insert(0, plain("Answered elsewhere: "));
            }
            body.push(runs(&line));
            body.push(when(at));
        }
        ApprovalState::Expired => {
            body.push(heading(view, agent, " asked"));
            if !view.confidential {
                body.extend(request(view));
            }
            body.push(runs(&[
                json!({"type": "TextRun", "text": "Expired", "weight": "Bolder", "color": "Warning"}),
                plain(": it wasn't answered in time, or its task ended."),
            ]));
        }
    }
    let mut card = json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": CARD_VERSION,
        "body": body,
    });
    if !actions.is_empty() {
        card["actions"] = Value::Array(actions);
    }
    if let Some(refresh) = refresh {
        card["refresh"] = refresh;
    }
    card
}

/// "Reconciler asked", or for a confidential approval, "Reconciler's
/// question" alone.
fn heading(view: &ApprovalView, agent: &str, verb: &str) -> Value {
    if view.confidential {
        runs(&[bold(agent), plain("'s question")])
    } else {
        runs(&[bold(agent), plain(verb)])
    }
}

/// What it wants and the detail. Never for a confidential approval.
fn request(view: &ApprovalView) -> Vec<Value> {
    let mut out = vec![runs(&[json!({
        "type": "TextRun",
        "text": view.summary,
        "size": "Medium",
        "weight": "Bolder",
    })])];
    if !view.detail.is_empty() {
        out.push(runs(&[
            json!({"type": "TextRun", "text": view.detail, "isSubtle": true}),
        ]));
    }
    out
}

/// Where an answer came from, for people.
fn place(via: Channel) -> &'static str {
    match via {
        Channel::Teams => "in Teams",
        Channel::Web => "in the web app",
        Channel::Desktop => "in the desktop app",
        Channel::Cli => "on the command line",
        Channel::Api => "through the API",
    }
}

/// When, in the viewer's own time zone and language, if `at` is RFC 3339;
/// as written otherwise.
fn when(at: &str) -> Value {
    match OffsetDateTime::parse(at, &Rfc3339) {
        Ok(t) => {
            let t = t.to_offset(time::UtcOffset::UTC);
            let stamp = format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                t.year(),
                u8::from(t.month()),
                t.day(),
                t.hour(),
                t.minute(),
                t.second()
            );
            json!({
                "type": "TextBlock",
                "text": format!("{{{{DATE({stamp}, SHORT)}}}} at {{{{TIME({stamp})}}}}"),
                "isSubtle": true,
                "spacing": "None",
            })
        }
        Err(_) => runs(&[json!({"type": "TextRun", "text": at, "isSubtle": true})]),
    }
}

fn execute(title: &str, verb: &str, approval: &str, style: &str) -> Value {
    json!({
        "type": "Action.Execute",
        "title": title,
        "verb": verb,
        "data": {"approval": approval},
        "style": style,
    })
}

/// Text that is shown as written, never read as Markdown.
fn runs(inlines: &[Value]) -> Value {
    json!({"type": "RichTextBlock", "inlines": inlines})
}

fn plain(text: &str) -> Value {
    json!({"type": "TextRun", "text": text})
}

fn bold(text: &str) -> Value {
    json!({"type": "TextRun", "text": text, "weight": "Bolder"})
}

/// A message carrying `card`, to send or to update a sent one with.
/// `summary` is what a notification may show.
pub fn card_activity(card: Value, summary: &str) -> Value {
    json!({
        "type": "message",
        "summary": summary,
        "attachments": [{"contentType": CARD_CONTENT_TYPE, "content": card}],
    })
}

/// The message for `view`: its card, and a summary that says only "<Agent>
/// needs you" when it's confidential.
pub fn approval_activity(view: &ApprovalView, ctx: &CardContext<'_>) -> Value {
    let summary = if view.confidential {
        format!("{} needs you", ctx.agent_name)
    } else {
        format!("{}: {}", ctx.agent_name, view.summary)
    };
    card_activity(approval_card(view, ctx), &summary)
}

/// A plain-text message: an agent's answer, shown as written.
pub fn text_activity(text: &str) -> Value {
    json!({"type": "message", "textFormat": "plain", "text": text})
}

/// The body of the HTTP 200 that answers an `adaptiveCard/action` invoke
/// with `card`, shown in place of the one pressed.
pub fn invoke_response(card: Value) -> Value {
    json!({"statusCode": 200, "type": CARD_CONTENT_TYPE, "value": card})
}

/// The body of the HTTP 200 that answers an invoke with a message, such as
/// "Only Dana can answer this".
pub fn invoke_message(text: &str) -> Value {
    json!({
        "statusCode": 200,
        "type": "application/vnd.microsoft.activity.message",
        "value": text,
    })
}

/// The body of the HTTP 200 that answers an invoke the app couldn't handle
/// (400 or 500 in `statusCode`).
pub fn invoke_error(status: u16, message: &str) -> Value {
    let code = if status < 500 {
        "BadRequest"
    } else {
        "InternalServerError"
    };
    json!({
        "statusCode": status,
        "type": "application/vnd.microsoft.error",
        "value": {"code": code, "message": message},
    })
}
