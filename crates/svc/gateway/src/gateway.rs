//! Sending one call: redact, try each model on the route until one answers
//! (MP-8), restore PII in the reply, and describe the call for the audit log
//! (DP-5) without the prompt text unless policy asks for it.

use std::collections::BTreeMap;
use std::sync::Arc;

use glass_policy::{Route, RouteStep};
use glass_types::Label;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest as _, Sha256};

use crate::{
    CompleteInput, Message, ModelProvider, ProviderError, ProviderRequest, Redactor, Vault,
};

/// The model providers this machine can reach, by the policy's provider id.
#[derive(Debug, Default, Clone)]
pub struct Gateway {
    providers: BTreeMap<String, Arc<dyn ModelProvider>>,
}

/// One call, after policy has routed it.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    pub input: &'a CompleteInput,
    pub route: &'a Route,
    pub redactor: &'a Redactor,
    /// Keep the redacted prompt and reply in the record (policy
    /// `models.log_prompts`).
    pub log_prompts: bool,
}

/// A call a model answered.
#[derive(Debug, Clone)]
pub struct Completion {
    /// The reply with this call's PII put back.
    pub text: String,
    pub stop_reason: Option<String>,
    pub record: CallRecord,
}

/// A call no model answered.
#[derive(Debug, Clone)]
pub struct Failure {
    /// Shown to the agent and the user.
    pub message: String,
    pub record: CallRecord,
}

/// What the audit log keeps about a model call (DP-5, AU-1). Prompt and reply
/// text appear only when policy turns `log_prompts` on, and then redacted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallRecord {
    /// The most sensitive label of the data the call carried.
    pub label: Label,
    /// The routing rule that applied.
    pub route: String,
    /// The model that answered: policy id, provider, the provider's id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Estimated from policy's prices; absent when a price isn't set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Replacements by kind (DP-3).
    pub redacted: BTreeMap<String, usize>,
    /// The request as sent (after redaction).
    pub request: Digest,
    /// The reply as received (before PII came back).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Digest>,
    /// Models that were tried first and failed, in order (MP-8).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed_over: Vec<Attempt>,
    /// Only with `log_prompts`: the redacted request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<serde_json::Value>,
    /// Only with `log_prompts`: the reply as received.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
}

/// Size and fingerprint of text that crossed the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Digest {
    pub chars: usize,
    /// SHA-256 of the UTF-8 bytes, hex. For the request, of its canonical
    /// JSON: `{"system":…,"messages":[{"role":…,"content":…}]}`.
    pub sha256: String,
}

impl Digest {
    fn of(text: &str) -> Self {
        Self {
            chars: text.chars().count(),
            sha256: hex::encode(Sha256::digest(text.as_bytes())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Attempt {
    pub model: String,
    pub provider: String,
    pub error: String,
}

impl Gateway {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, provider: impl Into<String>, model: Arc<dyn ModelProvider>) {
        self.providers.insert(provider.into(), model);
    }

    pub fn with(mut self, provider: impl Into<String>, model: Arc<dyn ModelProvider>) -> Self {
        self.add(provider, model);
        self
    }

    /// Provider ids this machine can reach.
    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Sends `call` along its route. Blocks until a model answers or every
    /// model on the route has failed.
    pub fn complete(&self, call: Call<'_>) -> Result<Completion, Box<Failure>> {
        let mut vault = Vault::default();
        let system = call
            .input
            .system
            .as_deref()
            .map(|s| call.redactor.redact(s, &mut vault));
        let messages: Vec<Message> = call
            .input
            .messages
            .iter()
            .map(|m| Message {
                role: m.role,
                content: call.redactor.redact(&m.content, &mut vault),
            })
            .collect();
        let sent = json!({ "system": system, "messages": messages });
        let mut record = CallRecord {
            label: call.route.label,
            route: call.route.rule.clone(),
            model: None,
            provider: None,
            provider_model: None,
            region: None,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            redacted: vault.counts().clone(),
            request: Digest::of(&sent.to_string()),
            response: None,
            failed_over: Vec::new(),
            prompt: call.log_prompts.then(|| sent.clone()),
            reply: None,
        };

        let mut problems = Vec::new();
        for step in &call.route.steps {
            let result = match self.providers.get(&step.provider) {
                Some(provider) => provider.complete(&ProviderRequest {
                    model: &step.provider_model,
                    system: system.as_deref(),
                    messages: &messages,
                    max_tokens: call.input.max_tokens,
                }),
                None => Err(ProviderError::NotConfigured(
                    "no credentials or connection for it".into(),
                )),
            };
            match result {
                Ok(reply) => {
                    record.model = Some(step.model.clone());
                    record.provider = Some(step.provider.clone());
                    record.provider_model = Some(step.provider_model.clone());
                    record.region.clone_from(&step.region);
                    record.input_tokens = Some(reply.input_tokens);
                    record.output_tokens = Some(reply.output_tokens);
                    record.cost_usd = cost(step, reply.input_tokens, reply.output_tokens);
                    record.response = Some(Digest::of(&reply.text));
                    if call.log_prompts {
                        record.reply = Some(reply.text.clone());
                    }
                    return Ok(Completion {
                        text: vault.restore(&reply.text),
                        stop_reason: reply.stop_reason,
                        record,
                    });
                }
                Err(error) => {
                    let line = format!("{} via {} {error}", step.name, step.provider_name);
                    record.failed_over.push(Attempt {
                        model: step.model.clone(),
                        provider: step.provider.clone(),
                        error: error.to_string(),
                    });
                    if !error.fails_over() {
                        return Err(Box::new(Failure {
                            message: line,
                            record,
                        }));
                    }
                    problems.push(line);
                }
            }
        }
        Err(Box::new(Failure {
            message: format!(
                "no approved model could take the call: {}",
                problems.join("; ")
            ),
            record,
        }))
    }
}

/// Dollars for a call, when policy prices both directions.
fn cost(step: &RouteStep, input: u64, output: u64) -> Option<f64> {
    let (inp, out) = (step.usd_per_mtok_in?, step.usd_per_mtok_out?);
    let dollars = (input as f64 * inp + output as f64 * out) / 1_000_000.0;
    // Micro-dollars are plenty; keeps records short.
    Some((dollars * 1_000_000.0).round() / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use glass_policy::document::{PiiKind, ProviderKind, Redaction};
    use serde_json::Value;

    use super::*;
    use crate::{Role, ScriptedProvider};

    fn step(model: &str, provider: &str) -> RouteStep {
        RouteStep {
            model: model.into(),
            name: "Claude Sonnet".into(),
            provider: provider.into(),
            provider_name: provider.to_uppercase(),
            kind: ProviderKind::Scripted,
            provider_model: format!("{model}-v1"),
            region: None,
            endpoint: None,
            usd_per_mtok_in: Some(3.0),
            usd_per_mtok_out: Some(15.0),
        }
    }

    fn route(steps: Vec<RouteStep>) -> Route {
        Route {
            label: Label::Internal,
            steps,
            rule: "model route for internal data".into(),
        }
    }

    fn input(text: &str) -> CompleteInput {
        CompleteInput {
            messages: vec![Message {
                role: Role::User,
                content: text.into(),
            }],
            max_tokens: 500,
            system: Some("Be brief.".into()),
            model: None,
        }
    }

    fn redactor() -> Redactor {
        Redactor::new(&Redaction {
            pii: vec![PiiKind::Email],
            patterns: Vec::new(),
        })
    }

    #[test]
    fn dp3_what_leaves_is_redacted_and_the_reply_restored() {
        let echo = Arc::new(ScriptedProvider::echo());
        let gateway = Gateway::new().with("primary", echo.clone());
        let input = input("Email ana@example.com the key AKIAIOSFODNN7EXAMPLE");
        let route = route(vec![step("sonnet", "primary")]);
        let done = gateway
            .complete(Call {
                input: &input,
                route: &route,
                redactor: &redactor(),
                log_prompts: false,
            })
            .unwrap();
        let seen = echo.seen();
        assert_eq!(
            seen[0].messages[0].content,
            "Email [redacted:email#1] the key [redacted:aws-access-key#1]"
        );
        assert_eq!(seen[0].model, "sonnet-v1");
        assert!(
            done.text
                .contains("Email ana@example.com the key [redacted:aws-access-key#1]")
        );

        // DP-5: sizes, hashes, tokens and cost; no prompt text.
        let record = serde_json::to_value(&done.record).unwrap();
        assert_eq!(record["model"], "sonnet");
        assert_eq!(record["provider"], "primary");
        assert_eq!(record["redacted"]["aws-access-key"], 1);
        assert_eq!(record["request"]["sha256"].as_str().unwrap().len(), 64);
        assert!(record["input_tokens"].as_u64().unwrap() > 0);
        assert!(record["cost_usd"].as_f64().unwrap() > 0.0);
        assert!(record.get("prompt").is_none() && record.get("reply").is_none());
        let text = record.to_string();
        assert!(!text.contains("ana@example.com") && !text.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn dp5_policy_can_keep_the_redacted_prompt() {
        let gateway = Gateway::new().with("primary", Arc::new(ScriptedProvider::echo()));
        let input = input("Key AKIAIOSFODNN7EXAMPLE");
        let route = route(vec![step("sonnet", "primary")]);
        let done = gateway
            .complete(Call {
                input: &input,
                route: &route,
                redactor: &redactor(),
                log_prompts: true,
            })
            .unwrap();
        let record = serde_json::to_value(&done.record).unwrap();
        assert_eq!(
            record["prompt"]["messages"][0]["content"],
            "Key [redacted:aws-access-key#1]"
        );
        assert_eq!(record["prompt"]["system"], "Be brief.");
        assert!(
            record["reply"]
                .as_str()
                .unwrap()
                .starts_with("Scripted reply")
        );
        assert!(!record.to_string().contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn mp8_an_outage_fails_over_to_the_next_model() {
        let down = Arc::new(ScriptedProvider::failing(ProviderError::Unavailable(
            "rate limited".into(),
        )));
        let up = Arc::new(ScriptedProvider::echo());
        let gateway = Gateway::new()
            .with("down", down.clone())
            .with("up", up.clone());
        let input = input("hello");
        let route = route(vec![
            step("first", "down"),
            step("unset", "nowhere"),
            step("second", "up"),
        ]);
        let call = Call {
            input: &input,
            route: &route,
            redactor: &redactor(),
            log_prompts: false,
        };
        let done = gateway.complete(call).unwrap();
        assert_eq!(done.record.model.as_deref(), Some("second"));
        let tried: Vec<&str> = done
            .record
            .failed_over
            .iter()
            .map(|a| a.model.as_str())
            .collect();
        assert_eq!(tried, ["first", "unset"]);
        assert!(done.record.failed_over[0].error.contains("rate limited"));
        assert_eq!(down.seen().len(), 1);
    }

    #[test]
    fn a_refused_request_stops_and_nothing_left_means_a_failure() {
        let refuses = Arc::new(ScriptedProvider::failing(ProviderError::Rejected(
            "prompt is too long".into(),
        )));
        let spare = Arc::new(ScriptedProvider::echo());
        let gateway = Gateway::new().with("a", refuses).with("b", spare.clone());
        let input = input("hello");
        let route = route(vec![step("first", "a"), step("second", "b")]);
        let call = Call {
            input: &input,
            route: &route,
            redactor: &redactor(),
            log_prompts: false,
        };
        let failure = gateway.complete(call).unwrap_err();
        assert_eq!(
            failure.message,
            "Claude Sonnet via A refused the request (prompt is too long)"
        );
        assert!(
            spare.seen().is_empty(),
            "another provider would refuse it too"
        );

        let empty = Gateway::new();
        let failure = empty.complete(call).unwrap_err();
        assert!(
            failure
                .message
                .starts_with("no approved model could take the call")
        );
        let record: Value = serde_json::to_value(&failure.record).unwrap();
        assert!(record.get("model").is_none());
        assert_eq!(record["failed_over"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn cost_needs_both_prices() {
        let mut s = step("m", "p");
        assert_eq!(cost(&s, 1_000_000, 100_000), Some(4.5));
        s.usd_per_mtok_out = None;
        assert_eq!(cost(&s, 10, 10), None);
    }
}
