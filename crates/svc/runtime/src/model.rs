//! Model calls in the pipeline (ADR 0018). `model.complete` is an action
//! like any other (tier, ask, audit); on top of that, the most sensitive
//! label of the data the task has read decides where the call may go (DP-2,
//! DP-11), and the gateway sends it, redacted, along that route.

use std::path::Path;

use glass_actions::{ActionError, ActionOutput, Grant};
use glass_gateway::{Call, CallRecord, CompleteInput, Redactor};
use glass_policy::{EffectivePolicy, ModelRefusal, Route, RouteStep};
use glass_types::{Label, Tier};
use serde_json::{Value, json};

use crate::task::Task;
use crate::{CatalogEntry, Runtime, labels};

/// A model call policy has routed, ready to send.
pub(crate) struct Routed {
    pub input: CompleteInput,
    pub route: Route,
}

/// What the pipeline needs back from a model call.
pub(crate) struct Sent {
    pub result: Result<ActionOutput, ActionError>,
    pub record: CallRecord,
    /// The summary and target naming the model that answered, which after a
    /// failover isn't the first on the route.
    pub answered: Option<(String, Vec<String>)>,
}

impl Runtime {
    /// Where this call may go, or why it may go nowhere.
    pub(crate) fn route_model(&self, task: &Task, args: &Value) -> Result<Routed, ModelRefusal> {
        let input = CompleteInput::parse(args).map_err(|e| ModelRefusal {
            reason: e.to_string(),
            rule: "models.input".into(),
        })?;
        let policy = task.policy();
        let rules = policy.models();
        if input.max_tokens > rules.max_output_tokens() {
            return Err(ModelRefusal {
                reason: format!(
                    "max_tokens can be at most {} for this task",
                    rules.max_output_tokens()
                ),
                rule: "models.max_output_tokens".into(),
            });
        }
        let label = call_label(task);
        let route = rules.route(label, input.model.as_deref())?;
        Ok(Routed { input, route })
    }

    /// Sends a routed call through the gateway. Blocks; call it from the
    /// blocking pool.
    pub(crate) fn send_model(
        gateway: &glass_gateway::Gateway,
        policy: &EffectivePolicy,
        routed: &Routed,
    ) -> Sent {
        let rules = policy.models();
        let redactor = Redactor::new(rules.redaction());
        let call = Call {
            input: &routed.input,
            route: &routed.route,
            redactor: &redactor,
            log_prompts: rules.log_prompts(),
        };
        match gateway.complete(call) {
            Ok(done) => {
                let r = &done.record;
                let data = json!({
                    "model": r.model,
                    "provider": r.provider,
                    "label": r.label,
                    "input_tokens": r.input_tokens,
                    "output_tokens": r.output_tokens,
                    "stop_reason": done.stop_reason,
                    "redacted": r.redacted,
                });
                let answered = routed
                    .route
                    .steps
                    .iter()
                    .find(|s| Some(&s.model) == r.model.as_ref())
                    .map(|step| summary_for(step, &routed.input));
                Sent {
                    result: Ok(ActionOutput {
                        text: done.text,
                        data,
                        changes: Vec::new(),
                    }),
                    record: done.record,
                    answered,
                }
            }
            Err(failure) => Sent {
                result: Err(ActionError::Failed(failure.message)),
                record: failure.record,
                answered: None,
            },
        }
    }

    /// The handoff card's model line: where this task's data would go, per
    /// policy, for the labels of the folders it's given.
    pub(crate) fn model_line(
        &self,
        entry: &CatalogEntry,
        policy: &EffectivePolicy,
        grants: &[Grant],
        home: &Path,
    ) -> String {
        let uses_models = self
            .registry()
            .get(&glass_gateway::MODEL_COMPLETE.parse().expect("static name"))
            .is_some_and(|(def, _)| {
                policy.tier_for(&def.name, def.default_tier).tier != Tier::Never
            });
        if !uses_models {
            // Agents that don't use the gateway: scripted ones, or a
            // harness that talks to its own provider (HX-7).
            return if entry.model.is_empty() {
                "No model (scripted agent)".into()
            } else {
                entry.model.clone()
            };
        }
        let rules = policy.models();
        // The most sensitive data the task could read from its folders:
        // nothing above the agent's limit opens, and unlabeled files count
        // as policy says.
        let label = grants
            .iter()
            .filter_map(|g| labels::label_of(policy, home, &g.root).ok().flatten())
            .map(|label| label.min(policy.max_label()))
            .fold(rules.unlabeled(), Label::max);
        // The catalog's model is a preference, used when policy allows it.
        let preferred = Some(entry.model.as_str()).filter(|m| !m.is_empty());
        let route = rules
            .route(label, preferred)
            .or_else(|_| rules.route(label, None));
        match route {
            Ok(route) => match route.steps.first() {
                Some(step) => format!("{} via {}", step.name, step.provider_name),
                None => format!("No approved model for {label} data"),
            },
            Err(_) => format!("No approved model for {label} data"),
        }
    }
}

/// The label a model call carries: the most sensitive data the task has
/// read so far (DP-11), and never less than policy's label for unlabeled
/// content.
pub(crate) fn call_label(task: &Task) -> Label {
    let unlabeled = task.policy().models().unlabeled();
    task.lock()
        .data_label
        .map_or(unlabeled, |read| read.max(unlabeled))
}

/// The audit detail for a model call, under `"model"`.
pub(crate) fn audit_detail(record: &CallRecord) -> Value {
    serde_json::to_value(record).unwrap_or(Value::Null)
}

/// The audit detail for a call policy refused before routing, so the record
/// still names the label and any model the agent asked for.
pub(crate) fn refusal_detail(task: &Task, args: &Value) -> Value {
    json!({
        "label": call_label(task),
        "requested": args.get("model").and_then(Value::as_str),
    })
}

/// A short summary for the feed, approvals and audit: "Ask Claude Sonnet via
/// Amazon Bedrock: 2 messages, 1,204 characters".
pub(crate) fn summary(routed: &Routed) -> (String, Vec<String>) {
    match routed.route.steps.first() {
        Some(step) => summary_for(step, &routed.input),
        None => (
            format!("Ask an AI model: {}", routed.input.describe()),
            Vec::new(),
        ),
    }
}

fn summary_for(step: &RouteStep, input: &CompleteInput) -> (String, Vec<String>) {
    let target = format!("{} via {}", step.name, step.provider_name);
    (format!("Ask {target}: {}", input.describe()), vec![target])
}
