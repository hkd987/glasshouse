//! What an action's result carries (GA-1, ADR 0032): the shape of its
//! `data`, declared by its provider ([`crate::ActionProvider::returns`]) as
//! a small JSON Schema, so an app someone makes, or the agent making it,
//! reads the right fields. [`typescript`] shows it as the type an app's
//! code works with; [`conforms`] checks a real result against it, so a
//! provider's tests keep what it declares true.
//!
//! The schema is a subset: `type` (one or a list, `"null"` among them for
//! a field that may be missing a value), `properties`, `required`,
//! `additionalProperties: false`, `items`, `enum` and `description`.
//! Anything else is refused, so the two never read a schema differently.

use serde_json::Value;

const KEYWORDS: [&str; 7] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "description",
];

/// Whether `value` has the shape `schema` declares; where it doesn't, the
/// path to the first place that differs and how.
pub fn conforms(value: &Value, schema: &Value) -> Result<(), String> {
    check(value, schema, "data")
}

fn check(value: &Value, schema: &Value, at: &str) -> Result<(), String> {
    let Some(rules) = schema.as_object() else {
        return Err(format!("{at}: the schema isn't an object"));
    };
    if let Some(unknown) = rules.keys().find(|k| !KEYWORDS.contains(&k.as_str())) {
        return Err(format!(
            "{at}: the schema uses {unknown:?}, which isn't in the subset"
        ));
    }
    if let Some(types) = rules.get("type") {
        let types: Vec<&str> = match types {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => return Err(format!("{at}: type is a name or a list of names")),
        };
        let is = |t: &str| match t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !types.iter().any(|t| is(t)) {
            return Err(format!(
                "{at}: {} isn't {}",
                kind(value),
                types.join(" or ")
            ));
        }
    }
    if let Some(allowed) = rules.get("enum").and_then(Value::as_array)
        && !value.is_null()
        && !allowed.contains(value)
    {
        return Err(format!(
            "{at}: {value} isn't one of {}",
            Value::Array(allowed.clone())
        ));
    }
    if let Some(fields) = value.as_object() {
        let properties = rules.get("properties").and_then(Value::as_object);
        for name in rules
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !fields.contains_key(name) {
                return Err(format!("{at}.{name}: missing"));
            }
        }
        for (name, field) in fields {
            match properties.and_then(|p| p.get(name)) {
                Some(rule) => check(field, rule, &format!("{at}.{name}"))?,
                None if rules.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{at}.{name}: not declared"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(rule)) = (value.as_array(), rules.get("items")) {
        for (i, item) in items.iter().enumerate() {
            check(item, rule, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// `schema` as a TypeScript type, the way an app's code reads it: fields
/// that may be missing marked `?`, each field's description after it.
pub fn typescript(schema: &Value) -> String {
    ts(schema, 0)
}

fn ts(schema: &Value, depth: usize) -> String {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let one = |t: &str| -> String {
        match t {
            "object" => object(schema, depth),
            "array" => {
                let item = schema
                    .get("items")
                    .map_or_else(|| "unknown".to_owned(), |i| ts(i, depth));
                if item.contains(' ') && !item.starts_with('{') {
                    format!("({item})[]")
                } else {
                    format!("{item}[]")
                }
            }
            "integer" => "number".to_owned(),
            "string" | "number" | "boolean" | "null" => t.to_owned(),
            _ => "unknown".to_owned(),
        }
    };
    match types.as_slice() {
        [] => "unknown".to_owned(),
        all => all.iter().map(|t| one(t)).collect::<Vec<_>>().join(" | "),
    }
}

fn object(schema: &Value, depth: usize) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return "Record<string, unknown>".to_owned();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let pad = "  ".repeat(depth + 1);
    let mut out = String::from("{\n");
    for (name, field) in properties {
        let optional = if required.contains(&name.as_str()) {
            ""
        } else {
            "?"
        };
        let note = field
            .get("description")
            .and_then(Value::as_str)
            .map(|d| format!(" // {d}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{pad}{name}{optional}: {};{note}\n",
            ts(field, depth + 1)
        ));
    }
    out.push_str(&"  ".repeat(depth));
    out.push('}');
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event() -> Value {
        json!({
            "type": "object",
            "properties": {
                "event": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "start": { "type": "string", "description": "local time, YYYY-MM-DDTHH:MM" },
                        "kind": { "enum": ["event", "hold"] },
                        "attendees": { "type": "array", "items": { "type": "string" } },
                        "meeting": { "type": ["object", "null"], "properties": { "id": { "type": "string" } } }
                    },
                    "required": ["id", "start", "kind", "attendees"],
                    "additionalProperties": false
                }
            },
            "required": ["event"]
        })
    }

    #[test]
    fn ga1_a_result_conforms_to_its_shape_or_says_where_it_doesnt() {
        let schema = event();
        let good = json!({ "event": { "id": "e1", "start": "2026-10-23T09:30", "kind": "event", "attendees": ["a@x"] } });
        assert_eq!(conforms(&good, &schema), Ok(()));
        let cases = [
            (json!({}), "data.event: missing"),
            (
                json!({ "event": { "id": "e1", "start": "s", "kind": "talk", "attendees": [] } }),
                "data.event.kind: \"talk\" isn't one of",
            ),
            (
                json!({ "event": { "id": "e1", "start": "s", "kind": "hold", "attendees": [1] } }),
                "data.event.attendees[0]: a number isn't string",
            ),
            (
                json!({ "event": { "id": "e1", "start": "s", "kind": "hold", "attendees": [], "extra": 1 } }),
                "data.event.extra: not declared",
            ),
        ];
        for (value, said) in cases {
            let err = conforms(&value, &schema).unwrap_err();
            assert!(err.starts_with(said), "{err}");
        }
        // Outside the subset: refused, so the type and the check agree.
        let err = conforms(&json!(1), &json!({ "type": "integer", "minimum": 0 })).unwrap_err();
        assert!(err.contains("\"minimum\""), "{err}");
    }

    #[test]
    fn ga1_a_shape_reads_as_the_typescript_an_app_writes() {
        assert_eq!(
            typescript(&event()),
            "{\n  event: {\n    attendees: string[];\n    id: string;\n    kind: \"event\" | \"hold\";\n    meeting?: {\n      id?: string;\n    } | null;\n    start: string; // local time, YYYY-MM-DDTHH:MM\n  };\n}"
        );
        assert_eq!(
            typescript(&json!({ "type": "array", "items": { "type": ["string", "null"] } })),
            "(string | null)[]"
        );
    }
}

/// Sample data of `schema`'s shape, for trying an app without anyone's
/// data: two of each list, values from each field's name and description
/// (`start` a time on `today`, `attendees` addresses, `id` an id).
pub fn sample(schema: &Value, today: &str) -> Value {
    make(schema, "", 0, today)
}

fn make(schema: &Value, name: &str, n: usize, today: &str) -> Value {
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
    {
        return first.clone();
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let about = format!(
        "{} {}",
        name.to_lowercase(),
        schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase()
    );
    match types
        .iter()
        .copied()
        .find(|t| *t != "null")
        .unwrap_or("null")
    {
        "object" => Value::Object(
            schema
                .get("properties")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), make(v, k, n, today)))
                .collect(),
        ),
        "array" => {
            let item = schema.get("items").cloned().unwrap_or(Value::Null);
            Value::Array(
                (0..2)
                    .map(|i| make(&item, name, n * 2 + i, today))
                    .collect(),
            )
        }
        "string" => Value::String(text(name, &about, n, today)),
        "integer" => Value::from(n as u64 + 2),
        "number" => Value::from(n as f64 + 12.5),
        "boolean" => Value::Bool(false),
        _ => Value::Null,
    }
}

/// A string for the field `name` that reads like one.
fn text(name: &str, about: &str, n: usize, today: &str) -> String {
    const PEOPLE: [(&str, &str); 3] = [
        ("Dana Ruiz", "dana"),
        ("Sam Lee", "sam"),
        ("Priya Shah", "priya"),
    ];
    let (person, login) = PEOPLE[n % PEOPLE.len()];
    let hour = 9 + 2 * (n % 4);
    let name = name.to_lowercase();
    if about.contains("yyyy-mm-ddthh:mm")
        || matches!(name.as_str(), "start" | "end" | "from" | "to") && about.contains("time")
    {
        let minute = if name == "end" { 30 } else { 0 };
        return format!("{today}T{hour:02}:{minute:02}");
    }
    if about.contains("rfc 3339") || name.ends_with("_at") || name == "date" || name == "when" {
        return format!("{today}T{hour:02}:00:00Z");
    }
    if about.contains("yyyy-mm-dd") {
        return today.to_owned();
    }
    if name == "from" {
        return format!("{person} <{login}@example.com>");
    }
    if name.contains("email")
        || name == "address"
        || name == "to"
        || name == "attendees"
        || name == "invited"
        || about.contains("address")
    {
        return format!("{login}@example.com");
    }
    if name == "id"
        || name.ends_with("_id")
        || name == "event"
        || name == "draft"
        || name == "deleted"
    {
        return format!("sample-{}", n + 1);
    }
    if name == "login" || name == "organizer" || name == "user" {
        return login.to_owned();
    }
    if name == "name" {
        return person.to_owned();
    }
    if matches!(name.as_str(), "title" | "subject") {
        return format!("Sample {name} {}", n + 1);
    }
    if matches!(name.as_str(), "path" | "from_path" | "to_path") {
        return format!("Documents/sample-{}.txt", n + 1);
    }
    if about.contains("empty when") || name == "attribution" {
        return String::new();
    }
    format!("Sample {name}").trim().to_owned()
}

/// Where `value` doesn't fit `schema`, read leniently: required fields,
/// fields it doesn't declare, types and enums; any other keyword (lengths,
/// patterns) is the action's own to check. For an app's calls when it's
/// tried: what the action would refuse.
pub fn fits(value: &Value, schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    lenient(value, schema, "input", &mut found);
    found
}

fn lenient(value: &Value, schema: &Value, at: &str, found: &mut Vec<String>) {
    let Some(rules) = schema.as_object() else {
        return;
    };
    if let Some(types) = rules.get("type") {
        let types: Vec<&str> = match types {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        let is = |t: &str| match t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !types.is_empty() && !types.iter().any(|t| is(t)) {
            found.push(format!(
                "{at} is {}, not {}",
                kind(value),
                types.join(" or ")
            ));
            return;
        }
    }
    if let Some(allowed) = rules.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        found.push(format!(
            "{at} is {value}, not one of {}",
            Value::Array(allowed.clone())
        ));
    }
    if let Some(fields) = value.as_object() {
        let properties = rules.get("properties").and_then(Value::as_object);
        for name in rules
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !fields.contains_key(name) {
                found.push(format!("{at}.{name} is missing"));
            }
        }
        for (name, field) in fields {
            match properties.and_then(|p| p.get(name)) {
                Some(rule) => lenient(field, rule, &format!("{at}.{name}"), found),
                None if rules.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    found.push(format!("{at}.{name} isn't something it takes"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(rule)) = (value.as_array(), rules.get("items")) {
        for (i, item) in items.iter().enumerate() {
            lenient(item, rule, &format!("{at}[{i}]"), found);
        }
    }
}

#[cfg(test)]
mod samples {
    use serde_json::json;

    use super::*;

    #[test]
    fn ga1_sample_data_has_the_shape_and_reads_like_the_real_thing() {
        let shape = json!({
            "type": "object",
            "properties": {
                "events": { "type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "title": { "type": "string" },
                        "start": { "type": "string", "description": "the computer's local time, YYYY-MM-DDTHH:MM" },
                        "attendees": { "type": "array", "items": { "type": "string" }, "description": "email addresses" },
                        "kind": { "enum": ["event", "hold"] },
                        "meeting": { "type": ["object", "null"], "properties": { "id": { "type": "string" } } }
                    },
                    "required": ["id", "title", "start", "attendees", "kind"],
                    "additionalProperties": false
                } }
            },
            "required": ["events"]
        });
        let data = sample(&shape, "2026-10-23");
        assert_eq!(conforms(&data, &shape), Ok(()));
        let first = &data["events"][0];
        assert_eq!(first["title"], "Sample title 1");
        assert_eq!(first["start"], "2026-10-23T09:00");
        assert_eq!(
            first["attendees"],
            json!(["dana@example.com", "sam@example.com"])
        );
        assert_eq!(first["kind"], "event");
        assert_eq!(data["events"][1]["start"], "2026-10-23T11:00");
    }

    #[test]
    fn ga1_an_input_that_wont_do_says_why() {
        let takes = json!({
            "type": "object",
            "properties": {
                "folder": { "enum": ["inbox", "drafts"] },
                "limit": { "type": "integer", "minimum": 1 },
                "to": { "type": "array", "items": { "type": "string", "maxLength": 300 } }
            },
            "required": ["folder"],
            "additionalProperties": false
        });
        assert!(
            fits(
                &json!({ "folder": "inbox", "limit": 5, "to": ["a@x"] }),
                &takes
            )
            .is_empty()
        );
        assert_eq!(
            fits(
                &json!({ "folder": "Inbox", "limit": "5", "unread": true, "to": [1] }),
                &takes
            ),
            [
                "input.folder is \"Inbox\", not one of [\"inbox\",\"drafts\"]",
                "input.limit is a string, not integer",
                "input.to[0] is a number, not string",
                "input.unread isn't something it takes",
            ]
        );
        assert_eq!(fits(&json!({}), &takes), ["input.folder is missing"]);
    }
}
