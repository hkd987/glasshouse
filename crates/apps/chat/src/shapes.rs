//! What each chat action's result carries (GA-1, ADR 0049): the shape of
//! its `data`, so an app someone makes, or the agent making it, reads the
//! right fields. The test `ga1_what_each_action_returns_is_what_it_declares`
//! runs every action and checks these against what it really returned.

use serde_json::{Value, json};

use crate::everyday::{DELETE, DRAFT, EDIT, LEVELS, MEMBERS, NOTIFY, PIN, REACT, SEARCH, TOPIC};
use crate::{CREATE, LIST, POST, READ};

/// The shape of `action`'s data; None for an action chat doesn't have.
pub(crate) fn of(action: &str) -> Option<Value> {
    let id = |what: &str| json!({ "type": "string", "description": what });
    let conversation = id("the conversation's id (cv-…)");
    let message = id("the message's id");
    Some(match action {
        LIST => object(
            json!({
                "conversations": {
                    "type": "array",
                    "description": "most recent activity first",
                    "items": conversation_row()
                },
                "me": id("the person's login"),
                "compliance": { "type": ["string", "null"], "description": "the fingerprint of IT's compliance key every conversation is also sealed to; null before IT's Chat release arrives" },
                "opened": { "type": "boolean", "description": "false, with no conversations, when the person hasn't opened Chat on this computer yet" }
            }),
            &["conversations", "me"],
        ),
        READ => object(
            json!({
                "conversation": object(
                    json!({
                        "id": conversation,
                        "kind": kind(),
                        "title": { "type": "string" },
                        "topic": { "type": ["string", "null"], "description": "a channel's; null when it has none" },
                        "pinned": { "type": "array", "items": { "type": "string" }, "description": "pinned messages' ids, most recently pinned first" },
                        "notify": notify(),
                        "draft": draft()
                    }),
                    &["id", "kind", "title", "topic", "pinned", "notify", "draft"],
                ),
                "messages": { "type": "array", "description": "oldest first; a thread's root first, then its replies", "items": message_view() },
                "more_before": { "type": "boolean", "description": "whether older messages come before these (read with before: the first one's id)" },
                "thread": object(
                    json!({ "root": message, "draft": draft() }),
                    &["root", "draft"],
                )
            }),
            &["conversation", "messages", "more_before"],
        ),
        POST => object(
            json!({
                "posted": {
                    "type": "array",
                    "description": "one per message posted, in the order given; fewer than asked when some weren't sent",
                    "items": object(
                        json!({
                            "conversation": conversation,
                            "title": { "type": "string" },
                            "id": message,
                            "seq": { "type": "integer", "description": "its place in the conversation" }
                        }),
                        &["conversation", "title", "id", "seq"],
                    )
                }
            }),
            &["posted"],
        ),
        CREATE => object(
            json!({
                "conversation": object(
                    json!({ "id": conversation, "title": { "type": "string", "description": "the channel's name, with #" } }),
                    &["id", "title"],
                )
            }),
            &["conversation"],
        ),
        REACT => object(
            json!({
                "message": message,
                "emoji": { "type": "string" },
                "on": { "type": "boolean", "description": "whether the person's reaction is on now" }
            }),
            &["message", "emoji", "on"],
        ),
        EDIT | DELETE => object(json!({ "message": message }), &["message"]),
        PIN => object(
            json!({ "message": message, "pinned": { "type": "boolean" } }),
            &["message", "pinned"],
        ),
        TOPIC => object(
            json!({ "topic": { "type": "string", "description": "the channel's topic now; empty when cleared" } }),
            &["topic"],
        ),
        SEARCH => object(
            json!({
                "hits": {
                    "type": "array",
                    "description": "newest first",
                    "items": object(
                        json!({
                            "conversation": conversation,
                            "title": { "type": "string", "description": "the conversation's" },
                            "message": message,
                            "thread": { "type": ["string", "null"], "description": "the thread's root, for a reply" },
                            "sender_name": { "type": "string" },
                            "at": at(),
                            "text": { "type": "string", "description": "the message, cut to 200 characters" }
                        }),
                        &["conversation", "title", "message", "thread", "sender_name", "at", "text"],
                    )
                },
                "more": { "type": "boolean", "description": "whether more messages matched than were returned" }
            }),
            &["hits", "more"],
        ),
        DRAFT => object(
            json!({
                "conversation": conversation,
                "thread": { "type": ["string", "null"], "description": "the thread's root, for a reply's draft" }
            }),
            &["conversation", "thread"],
        ),
        NOTIFY => object(
            json!({ "conversation": conversation, "level": { "type": "string", "enum": LEVELS } }),
            &["conversation", "level"],
        ),
        MEMBERS => object(
            json!({
                "conversation": conversation,
                "added": { "type": "array", "items": { "type": "string" }, "description": "the logins added; empty when they were all in it already" }
            }),
            &["conversation", "added"],
        ),
        _ => return None,
    })
}

/// A closed object: `properties`, the `required` ones always there.
fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn at() -> Value {
    json!({ "type": "string", "description": "when it was sent, RFC 3339 (2026-10-23T09:30:00Z)" })
}

fn kind() -> Value {
    json!({ "type": "string", "enum": ["direct", "group", "channel"], "description": "two people, a few, or a named channel" })
}

fn notify() -> Value {
    json!({ "type": "string", "enum": LEVELS, "description": "when the person hears of new messages here" })
}

fn draft() -> Value {
    json!({ "type": ["string", "null"], "description": "what the person had started writing; null when nothing" })
}

fn person() -> Value {
    object(
        json!({ "login": { "type": "string" }, "name": { "type": "string" } }),
        &["login", "name"],
    )
}

/// A conversation as `chat.list` lists it.
fn conversation_row() -> Value {
    object(
        json!({
            "id": { "type": "string", "description": "the conversation's id (cv-…)" },
            "kind": kind(),
            "title": { "type": "string" },
            "people": { "type": "array", "items": person(), "description": "everyone in it, the person too" },
            "unread": { "type": "integer", "description": "messages others wrote since the person last read it in Chat" },
            "mentions": { "type": "integer", "description": "unread messages that mention the person" },
            "topic": { "type": ["string", "null"] },
            "notify": notify(),
            "draft": draft(),
            "last_at": { "type": ["string", "null"], "description": "when its newest message was sent, RFC 3339; null when it has none" },
            "last_from": { "type": ["string", "null"], "description": "who sent its newest message, by name" }
        }),
        &[
            "id",
            "kind",
            "title",
            "people",
            "unread",
            "mentions",
            "topic",
            "notify",
            "draft",
            "last_at",
            "last_from",
        ],
    )
}

/// A message as `chat.read` shows it.
fn message_view() -> Value {
    object(
        json!({
            "id": { "type": "string" },
            "seq": { "type": "integer", "description": "its place in the conversation" },
            "sender": { "type": "string", "description": "their login" },
            "sender_name": { "type": "string" },
            "mine": { "type": "boolean", "description": "whether the person sent it" },
            "at": at(),
            "text": { "type": "string", "description": "the latest edit's words; empty once deleted" },
            "attribution": { "type": ["string", "null"], "description": "the agent that wrote it for its sender (\"by desktop-agent for dana\"); null when they typed it" },
            "reply_to": { "type": ["string", "null"], "description": "the message it quotes" },
            "shared": {
                "type": ["object", "null"],
                "description": "an item shared here, shown as a card",
                "properties": {
                    "item": { "type": "string", "description": "the item's id" },
                    "kind": { "type": "string", "description": "artifact, skill, doc and the like" },
                    "title": { "type": "string" }
                },
                "required": ["item", "kind", "title"],
                "additionalProperties": false
            },
            "edited": { "type": "boolean" },
            "deleted": { "type": "boolean" },
            "reactions": {
                "type": "array",
                "description": "in the order first used",
                "items": object(
                    json!({
                        "emoji": { "type": "string" },
                        "count": { "type": "integer" },
                        "mine": { "type": "boolean", "description": "whether the person's is on" },
                        "names": { "type": "array", "items": { "type": "string" } }
                    }),
                    &["emoji", "count", "mine", "names"],
                )
            },
            "pinned": { "type": "boolean" },
            "mentions": { "type": "array", "items": person() },
            "thread": { "type": ["string", "null"], "description": "the thread's root, for a reply" },
            "replies": { "type": "integer", "description": "a root's replies" },
            "last_reply_at": { "type": ["string", "null"] },
            "repliers": { "type": "array", "items": { "type": "string" }, "description": "up to three who replied, by name, most recent first" }
        }),
        &[
            "id",
            "seq",
            "sender",
            "sender_name",
            "mine",
            "at",
            "text",
            "attribution",
            "reply_to",
            "shared",
            "edited",
            "deleted",
            "reactions",
            "pinned",
            "mentions",
            "thread",
            "replies",
            "last_reply_at",
            "repliers",
        ],
    )
}
