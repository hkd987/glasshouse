//! What each of the studio's actions returns (ADR 0049): the shape of its
//! `data`, as `glass_actions::shape` reads it.

use serde_json::{Value, json};

use crate::{ACTIONS, BASES, CREATE, REVERT, SAVE, SHARE, SOURCE, VERSIONS};

/// An app's id: `my-…` for one a person made.
fn app() -> Value {
    json!({ "type": "string", "description": "the app's id; my-… for one a person made" })
}

/// The installed app a version remixes; null for one from the template.
fn based_on() -> Value {
    json!({
        "type": ["object", "null"],
        "properties": {
            "id": { "type": "string" },
            "name": { "type": "string" },
            "version": { "type": "string", "description": "the installed app's version, such as 1.4.0" }
        },
        "required": ["id", "name", "version"],
        "additionalProperties": false,
        "description": "the app it remixes; null when it started from the template"
    })
}

/// Files by path, each its whole text.
fn files() -> Value {
    json!({ "type": "object", "description": "each file's text, by its path (src/main.ts, manifest.json)" })
}

/// One saved version, as `apps.versions` lists it.
fn version() -> Value {
    json!({
        "type": "object",
        "properties": {
            "app": app(),
            "name": { "type": "string" },
            "version": { "type": "integer", "description": "1 for the first save, one more for each after" },
            "current": { "type": "boolean", "description": "the version the person uses" },
            "based_on": based_on(),
            "note": { "type": "string", "description": "what changed, in the words of whoever saved it" },
            "saved_at": { "type": "string", "description": "RFC 3339, UTC" },
            "saved_by": { "type": "string", "description": "the agent or app that saved it" },
            "beyond_base": { "type": "array", "items": { "type": "string" }, "description": "actions it declares that its base doesn't" }
        },
        "required": ["app", "name", "version", "current", "based_on", "note", "saved_at", "saved_by", "beyond_base"],
        "additionalProperties": false
    })
}

/// How a run reads: without asking, telling the person, asking first.
fn tier() -> Value {
    json!({ "enum": ["auto", "notify", "ask", "never"], "description": "how a call runs from the app: auto without asking, notify telling the person, ask after they approve" })
}

fn side_effect() -> Value {
    json!({ "enum": ["none", "local_reversible", "disclose", "irreversible"], "description": "what a call does: none, a change undo takes back, sending data out, or something that can't be undone" })
}

pub(crate) fn returns(action: &str) -> Option<Value> {
    Some(match action {
        BASES => json!({
            "type": "object",
            "properties": {
                "bases": {
                    "type": "array",
                    "description": "the closest to the need first",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": app(),
                            "name": { "type": "string" },
                            "version": { "type": "string", "description": "an installed app's version, or a person's app's latest number" },
                            "description": { "type": "string" },
                            "yours": { "type": "boolean", "description": "an app the person made" },
                            "remixable": { "type": "boolean" },
                            "why_not": { "type": ["string", "null"], "description": "why it can't be remixed; null when it can" },
                            "match": { "type": "integer", "description": "how many of the need's words it shares; 0 without a need" }
                        },
                        "required": ["id", "name", "version", "description", "yours", "remixable", "why_not", "match"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["bases"],
            "additionalProperties": false
        }),
        // Four answers: the template; a person's app or an installed one,
        // whole; or one file, whose text is the result's text.
        SOURCE => json!({
            "type": "object",
            "description": "the template ({template, manifest, files}); an app ({app, name, version, yours, manifest, files}, with based_on when it's the person's); or with path, one file ({app, of, path, lines}), whose text is result.text",
            "properties": {
                "template": { "type": "boolean" },
                "app": app(),
                "name": { "type": "string" },
                "version": { "type": ["integer", "string"], "description": "a person's app's version number, or an installed app's version" },
                "yours": { "type": "boolean" },
                "based_on": based_on(),
                "manifest": { "type": "object", "description": "the template's whole manifest; an app's as it declares it (name, description, actions, permissions, labels)" },
                "files": files(),
                "of": { "type": "string", "description": "the app and version the file is from" },
                "path": { "type": "string" },
                "lines": { "type": "integer" }
            },
            "additionalProperties": false
        }),
        SAVE | CREATE => json!({
            "type": "object",
            "properties": {
                "app": app(),
                "name": { "type": "string" },
                "version": { "type": "integer" },
                "based_on": based_on(),
                "beyond_base": { "type": "array", "items": { "type": "string" }, "description": "actions it declares that its base doesn't" },
                "notes": { "type": "array", "items": { "type": "string" }, "description": "what the checks and the try-out noted without refusing" },
                "tried": {
                    "type": ["object", "null"],
                    "properties": {
                        "problems": { "type": "array", "items": { "type": "string" }, "description": "what to fix and save again; empty when it worked" }
                    },
                    "required": ["problems"],
                    "additionalProperties": false,
                    "description": "how it went when this version was tried with sample data; null where nothing tries apps"
                }
            },
            "required": ["app", "name", "version", "based_on", "beyond_base", "notes", "tried"],
            "additionalProperties": false
        }),
        VERSIONS => json!({
            "type": "object",
            "description": "with an app, its versions, newest first; without, the person's apps, each at its latest",
            "properties": {
                "app": app(),
                "versions": { "type": "array", "items": version() },
                "apps": { "type": "array", "items": version() }
            },
            "additionalProperties": false
        }),
        REVERT => json!({
            "type": "object",
            "properties": {
                "app": app(),
                "version": { "type": "integer", "description": "the version the person uses now" }
            },
            "required": ["app", "version"],
            "additionalProperties": false
        }),
        SHARE => json!({
            "type": "object",
            "properties": {
                "app": app(),
                "version": { "type": "integer" },
                "id": { "type": "string", "description": "the submission's id at the store" },
                "to": {
                    "type": "object",
                    "properties": {
                        "to": { "enum": ["team", "org"] },
                        "group": { "type": "string", "description": "the team, when it's for one" }
                    },
                    "required": ["to"],
                    "additionalProperties": false
                }
            },
            "required": ["app", "version", "id", "to"],
            "additionalProperties": false
        }),
        ACTIONS => json!({
            "type": "object",
            "description": "without names, every action an app may call, in brief; with names, each in full",
            "properties": {
                "actions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "title": { "type": "string" },
                            "description": { "type": "string", "description": "in full only" },
                            "side_effect": side_effect(),
                            "tier": tier(),
                            "described": { "type": "boolean", "description": "in brief only: whether its data is described" },
                            "input": { "type": "object", "description": "in full only: the JSON Schema of what it takes" },
                            "returns": { "type": ["object", "null"], "description": "in full only: the shape of its data; null when not described yet" }
                        },
                        "required": ["name", "title", "side_effect", "tier"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["actions"],
            "additionalProperties": false
        }),
        _ => return None,
    })
}
