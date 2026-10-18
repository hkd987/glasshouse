//! `glassctl tailor`: show and change your layer on an agent (PRD
//! "Tailoring catalog items"). Each change saves a new version; `--undo`
//! goes back one.

use std::fmt::Write;

use anyhow::bail;
use glass_api::{
    CheckResult, LayerChange, LayerRef, LayerRevert, LayerStatus, LayerView, PointKind, methods,
};
use glass_types::{AgentId, Tier};
use serde_json::Value;

use crate::Ctl;
use crate::style::{bold, dim, green, orange, red};

#[derive(Debug, clap::Args)]
pub struct TailorArgs {
    /// Agent id, from `glassctl agents`.
    agent: String,
    /// Change a setting, such as `flag_over=1000`. Repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    set: Vec<String>,
    /// Go back to the published value of a setting. Repeatable.
    #[arg(long, value_name = "KEY")]
    unset: Vec<String>,
    /// Block an action on every task with this agent, such as `mail.send`.
    #[arg(long, value_name = "ACTION")]
    block: Vec<String>,
    /// Make an action ask first on every task with this agent.
    #[arg(long, value_name = "ACTION")]
    ask: Vec<String>,
    /// Drop a limit this layer added.
    #[arg(long, value_name = "ACTION")]
    unblock: Vec<String>,
    /// What you asked for, in your words; kept with the version.
    #[arg(long)]
    note: Option<String>,
    /// Go back to the version before the one in use.
    #[arg(long, conflicts_with_all = ["revert", "off"])]
    undo: bool,
    /// Switch to this version.
    #[arg(long, value_name = "N", conflicts_with = "off")]
    revert: Option<u32>,
    /// Turn the layer off: tasks run the published agent.
    #[arg(long)]
    off: bool,
}

impl TailorArgs {
    fn changes_content(&self) -> bool {
        !(self.set.is_empty()
            && self.unset.is_empty()
            && self.block.is_empty()
            && self.ask.is_empty()
            && self.unblock.is_empty()
            && self.note.is_none())
    }
}

pub async fn run(ctl: &Ctl, args: TailorArgs) -> anyhow::Result<()> {
    let agent: AgentId = args.agent.parse().map_err(|e| anyhow::anyhow!("{e}"))?;
    let current: LayerView = ctl
        .call(
            methods::LAYER_GET,
            LayerRef {
                agent: agent.clone(),
            },
        )
        .await?;
    let view: LayerView = if args.undo || args.off || args.revert.is_some() {
        if args.changes_content() {
            bail!("change settings or switch versions, not both at once");
        }
        let to = if args.off {
            None
        } else if let Some(to) = args.revert {
            Some(to)
        } else {
            match current.active {
                None => bail!("there is no layer to undo"),
                Some(1) => None,
                Some(active) => Some(active - 1),
            }
        };
        ctl.call(methods::LAYER_REVERT, LayerRevert { agent, to })
            .await?
    } else if args.changes_content() {
        let mut change = starting_point(&current, agent);
        apply(&mut change, &args)?;
        ctl.call(methods::LAYER_SET, change).await?
    } else {
        current
    };
    ctl.show(&view, |v| render(v, &args.agent))
}

/// The version in use, as the base for a change.
fn starting_point(view: &LayerView, agent: AgentId) -> LayerChange {
    let active = view
        .active
        .and_then(|a| view.history.iter().find(|v| v.version == a));
    LayerChange {
        agent,
        settings: active.map(|v| v.settings.clone()).unwrap_or_default(),
        restrict: active.map(|v| v.restrict.clone()).unwrap_or_default(),
        note: String::new(),
    }
}

fn apply(change: &mut LayerChange, args: &TailorArgs) -> anyhow::Result<()> {
    for pair in &args.set {
        let Some((key, value)) = pair.split_once('=') else {
            bail!("--set takes KEY=VALUE, such as flag_over=1000");
        };
        change
            .settings
            .insert(key.trim().to_owned(), parse_value(value));
    }
    for key in &args.unset {
        change.settings.remove(key);
    }
    let limits = args
        .block
        .iter()
        .map(|a| (a, Tier::Never))
        .chain(args.ask.iter().map(|a| (a, Tier::Ask)));
    for (action, tier) in limits {
        change.restrict.retain(|(p, _)| p != action);
        change.restrict.push((action.clone(), tier));
    }
    change.restrict.retain(|(p, _)| !args.unblock.contains(p));
    change.note = args.note.clone().unwrap_or_default();
    Ok(())
}

/// `true`, `false` and numbers are typed; anything else is text. The
/// daemon checks the type against the setting.
fn parse_value(text: &str) -> Value {
    match text {
        "true" | "on" | "yes" => Value::Bool(true),
        "false" | "off" | "no" => Value::Bool(false),
        _ => serde_json::from_str::<serde_json::Number>(text)
            .map(Value::Number)
            .unwrap_or_else(|_| Value::String(text.to_owned())),
    }
}

fn render(view: &LayerView, agent: &str) -> String {
    let mut out = String::new();
    let status = match &view.status {
        LayerStatus::Off => dim("no layer · published version"),
        LayerStatus::On => green(&format!("your layer v{} · on", view.active.unwrap_or(0))),
        LayerStatus::Paused { reason } => orange(&format!(
            "your layer v{} · paused: {reason}",
            view.active.unwrap_or(0)
        )),
    };
    let _ = writeln!(
        out,
        "{} {} v{}   {status}",
        bold("Tailor"),
        bold(&view.agent.name),
        view.agent.version
    );

    let active = view
        .active
        .and_then(|a| view.history.iter().find(|v| v.version == a));
    let _ = writeln!(out, "\n  {}", bold("You can change"));
    if view.points.is_empty() {
        let _ = writeln!(
            out,
            "    {}",
            dim("no settings; a layer can only add limits")
        );
    }
    for point in &view.points {
        let value = active
            .and_then(|v| v.settings.get(&point.key))
            .map(show_value)
            .unwrap_or_else(|| {
                dim(&point.default.as_ref().map_or("—".to_owned(), |d| {
                    format!("{} (published)", show_value(d))
                }))
            });
        let kind = match point.kind {
            PointKind::Choice => point.choices.join("|"),
            other => format!("{other:?}").to_lowercase(),
        };
        let _ = writeln!(
            out,
            "    {:<16} {:<30} {:<13} {value}",
            point.key,
            point.title,
            dim(&kind)
        );
    }

    if let Some(version) = active {
        let note = if version.note.is_empty() {
            String::new()
        } else {
            format!(" · \"{}\"", version.note)
        };
        let _ = writeln!(
            out,
            "\n  {}{}",
            bold(&format!("Your layer v{}", version.version)),
            dim(&note)
        );
        if version.summary.is_empty() {
            let _ = writeln!(out, "    {}", dim("no changes"));
        }
        for line in &version.summary {
            let _ = writeln!(out, "    • {line}");
        }
        let _ = writeln!(out, "\n  {}", bold("Checks before it turns on"));
        for check in &view.checks {
            let mark = match check.result {
                CheckResult::Passed => green("✓"),
                CheckResult::Failed => red("✗"),
                CheckResult::NotAvailable => dim("–"),
            };
            let _ = writeln!(out, "    {mark} {:<40} {}", check.name, dim(&check.detail));
        }
    }

    if !view.history.is_empty() {
        let _ = writeln!(out, "\n  {}", bold("Versions"));
        for version in &view.history {
            let mark = if Some(version.version) == view.active {
                green("← in use")
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "    v{:<3} {}  {:<36} {mark}",
                version.version,
                dim(&version.at.clock()),
                version.note
            );
        }
    }
    let hint = match view.status {
        LayerStatus::Off if view.history.is_empty() => {
            format!("glassctl tailor {agent} --set KEY=VALUE [--block ACTION] [--note \"…\"]")
        }
        LayerStatus::Off => format!("glassctl tailor {agent} --revert N  ·  --set KEY=VALUE"),
        _ => format!("glassctl tailor {agent} --set KEY=VALUE  ·  --undo  ·  --off"),
    };
    let _ = writeln!(out, "\n  {}", dim(&hint));
    out
}

fn show_value(value: &Value) -> String {
    match value {
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn values_are_typed_and_limits_replace_each_other() {
        assert_eq!(parse_value("1000"), serde_json::json!(1000));
        assert_eq!(parse_value("off"), Value::Bool(false));
        assert_eq!(
            parse_value("also flag renewals"),
            serde_json::json!("also flag renewals")
        );

        let mut change = LayerChange {
            agent: "invoice-demo".parse().unwrap(),
            settings: BTreeMap::from([("flag_over".into(), serde_json::json!(5))]),
            restrict: vec![("mail.send".into(), Tier::Ask)],
            note: String::new(),
        };
        let args = TailorArgs {
            agent: "invoice-demo".into(),
            set: vec!["remind_vendors=false".into()],
            unset: vec!["flag_over".into()],
            block: vec!["mail.send".into()],
            ask: Vec::new(),
            unblock: Vec::new(),
            note: Some("no reminders".into()),
            undo: false,
            revert: None,
            off: false,
        };
        apply(&mut change, &args).unwrap();
        assert_eq!(
            change.settings,
            BTreeMap::from([("remind_vendors".into(), Value::Bool(false))])
        );
        assert_eq!(change.restrict, [("mail.send".to_owned(), Tier::Never)]);
        assert_eq!(change.note, "no reminders");
    }
}
