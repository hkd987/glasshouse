//! Customization points (TL-1): checking values against what the publisher
//! declared, and describing them in plain language.

use std::collections::BTreeMap;

use glass_api::{CustomizationPoint, PointKind};
use serde_json::Value;

const MAX_TEXT: usize = 200;
const MAX_INSTRUCTIONS: usize = 2000;

/// Checks a catalog entry's declarations, so a bad one fails at startup
/// rather than when someone tailors the agent.
pub fn check_declared(points: &[CustomizationPoint]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for point in points {
        if point.key.is_empty()
            || !point
                .key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'_')
        {
            return Err(format!(
                "customization point {:?}: keys are lowercase letters and _",
                point.key
            ));
        }
        if !seen.insert(&point.key) {
            return Err(format!(
                "customization point {} is declared twice",
                point.key
            ));
        }
        if (point.kind == PointKind::Choice) == point.choices.is_empty() {
            return Err(format!(
                "customization point {}: choices go with kind = \"choice\" and only with it",
                point.key
            ));
        }
        if let Some(default) = &point.default {
            check_value(point, default)
                .map_err(|e| format!("customization point {}: default {e}", point.key))?;
        }
    }
    Ok(())
}

/// Every value must be for a declared point and of its kind; anything else
/// is locked (TL-1). Returns every problem, not just the first.
pub fn check_settings(
    points: &[CustomizationPoint],
    settings: &BTreeMap<String, Value>,
) -> Result<(), Vec<String>> {
    let problems: Vec<String> = settings
        .iter()
        .filter_map(|(key, value)| match points.iter().find(|p| &p.key == key) {
            None => Some(format!("{key} can't be changed on this agent")),
            Some(point) => check_value(point, value)
                .err()
                .map(|e| format!("{}: {e}", point.title)),
        })
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn check_value(point: &CustomizationPoint, value: &Value) -> Result<(), String> {
    match (point.kind, value) {
        (PointKind::Number, Value::Number(_)) | (PointKind::Flag, Value::Bool(_)) => Ok(()),
        (PointKind::Text, Value::String(s)) if s.contains('\n') => Err("must be one line".into()),
        (PointKind::Text, Value::String(s)) if s.chars().count() > MAX_TEXT => {
            Err(format!("must be at most {MAX_TEXT} characters"))
        }
        (PointKind::Instructions, Value::String(s)) if s.chars().count() > MAX_INSTRUCTIONS => {
            Err(format!("must be at most {MAX_INSTRUCTIONS} characters"))
        }
        (PointKind::Text | PointKind::Instructions, Value::String(_)) => Ok(()),
        (PointKind::Choice, Value::String(s)) if point.choices.contains(s) => Ok(()),
        (PointKind::Choice, _) => Err(format!("must be one of {}", point.choices.join(", "))),
        (PointKind::Number, _) => Err("must be a number".into()),
        (PointKind::Flag, _) => Err("must be true or false".into()),
        (PointKind::Text | PointKind::Instructions, _) => Err("must be text".into()),
    }
}

/// What the agent gets: every point's default, overridden by the layer.
/// Values for points the agent no longer declares are dropped.
pub fn resolve(
    points: &[CustomizationPoint],
    settings: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    points
        .iter()
        .filter_map(|p| {
            settings
                .get(&p.key)
                .or(p.default.as_ref())
                .map(|v| (p.key.clone(), v.clone()))
        })
        .collect()
}

/// One line per value the layer sets, such as "Flag invoices over: 1000".
pub fn describe(points: &[CustomizationPoint], settings: &BTreeMap<String, Value>) -> Vec<String> {
    settings
        .iter()
        .map(|(key, value)| {
            let title = points
                .iter()
                .find(|p| &p.key == key)
                .map_or(key.as_str(), |p| p.title.as_str());
            let shown = match value {
                Value::Bool(true) => "on".to_owned(),
                Value::Bool(false) => "off".to_owned(),
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{title}: {shown}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn point(key: &str, kind: PointKind) -> CustomizationPoint {
        CustomizationPoint {
            key: key.into(),
            title: key.replace('_', " "),
            kind,
            choices: Vec::new(),
            default: None,
        }
    }

    fn settings(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn tl1_only_declared_points_of_the_right_kind_can_change() {
        let points = [
            point("flag_over", PointKind::Number),
            point("remind", PointKind::Flag),
            CustomizationPoint {
                choices: vec!["short".into(), "long".into()],
                ..point("length", PointKind::Choice)
            },
        ];
        check_settings(
            &points,
            &settings(&[
                ("flag_over", json!(1000)),
                ("remind", json!(false)),
                ("length", json!("short")),
            ]),
        )
        .unwrap();

        let problems = check_settings(
            &points,
            &settings(&[
                ("flag_over", json!("lots")),
                ("length", json!("medium")),
                ("model", json!("other")),
            ]),
        )
        .unwrap_err();
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems
                .iter()
                .any(|p| p == "model can't be changed on this agent")
        );
    }

    #[test]
    fn text_is_one_short_line() {
        let points = [point("name", PointKind::Text)];
        assert!(check_settings(&points, &settings(&[("name", json!("a\nb"))])).is_err());
        assert!(check_settings(&points, &settings(&[("name", json!("x".repeat(201)))])).is_err());
    }

    #[test]
    fn declarations_are_checked() {
        let mut bad = point("Flag-Over", PointKind::Number);
        assert!(check_declared(&[bad.clone()]).is_err());
        bad.key = "flag_over".into();
        bad.default = Some(json!("ten"));
        assert!(check_declared(&[bad.clone()]).is_err());
        bad.default = Some(json!(10));
        check_declared(&[bad.clone()]).unwrap();
        assert!(
            check_declared(&[bad.clone(), bad]).is_err(),
            "duplicate key"
        );
        assert!(check_declared(&[point("length", PointKind::Choice)]).is_err());
    }

    #[test]
    fn defaults_fill_in_and_dropped_points_disappear() {
        let points = [CustomizationPoint {
            default: Some(json!(true)),
            ..point("remind", PointKind::Flag)
        }];
        assert_eq!(resolve(&points, &BTreeMap::new())["remind"], json!(true));
        let resolved = resolve(
            &points,
            &settings(&[("remind", json!(false)), ("gone", json!(1))]),
        );
        assert_eq!(resolved, settings(&[("remind", json!(false))]));
        assert_eq!(
            describe(&points, &settings(&[("remind", json!(false))])),
            ["remind: off"]
        );
    }
}
