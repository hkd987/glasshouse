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
    CompleteInput, Message, ModelProvider, ProviderError, ProviderRequest, Redactor, Replay,
    ToolCall, ToolResult, Vault,
};

/// The model providers this machine can reach, by the policy's provider id.
#[derive(Debug, Default, Clone)]
pub struct Gateway {
    providers: BTreeMap<String, Arc<dyn ModelProvider>>,
    seal: SealKey,
}

/// The key the gateway seals replays with (ADR 0043): random, and this
/// process's only. After a restart, replays sealed before it aren't sent.
#[derive(Clone)]
struct SealKey(Arc<[u8; 32]>);

impl Default for SealKey {
    fn default() -> Self {
        Self(Arc::new(
            glass_crypto::random().expect("the system's random numbers"),
        ))
    }
}

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey(..)")
    }
}

impl SealKey {
    /// The MAC over a provider id and its blocks, hex. The blocks' JSON is
    /// canonical because serde_json's maps are sorted: nothing in the
    /// workspace turns on its `preserve_order`.
    fn mac(&self, provider: &str, blocks: &[serde_json::Value]) -> Option<String> {
        let blocks = serde_json::to_string(blocks).ok()?;
        glass_crypto::hmac(
            &self.0[..],
            &[
                b"glasshouse replay v1\0",
                provider.as_bytes(),
                b"\0",
                blocks.as_bytes(),
            ],
        )
        .ok()
        .map(hex::encode)
    }

    fn seal(&self, provider: &str, blocks: Vec<serde_json::Value>) -> Option<Replay> {
        Some(Replay {
            provider: provider.to_owned(),
            seal: self.mac(provider, &blocks)?,
            blocks,
        })
    }

    fn opens(&self, replay: &Replay) -> bool {
        self.mac(&replay.provider, &replay.blocks)
            .is_some_and(|mac| glass_crypto::same(mac.as_bytes(), replay.seal.as_bytes()))
    }
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
    /// The tools the model asked for, their inputs with PII put back (ADR
    /// 0041). The agent makes each call, where every rule applies.
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: Option<String>,
    /// The reply as the provider wrote it, sealed, for the agent to send
    /// back on this assistant message (ADR 0043). Placeholders stay in.
    pub replay: Option<Replay>,
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
    /// The model a router chose to answer, such as OpenRouter's auto
    /// router (ADR 0039).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_by: Option<String>,
    /// What the provider charged, when it says; otherwise estimated from
    /// policy's prices, and absent when a price isn't set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Tools the call offered the model (ADR 0041), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_offered: Option<usize>,
    /// The tools the model asked for, by name: never their inputs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools_called: Vec<String>,
    /// Replacements by kind (DP-3).
    pub redacted: BTreeMap<String, usize>,
    /// Replays the agent sent that this gateway didn't seal, or that were
    /// changed: none of the call's replays went (ADR 0043).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub replays_dropped: bool,
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
        // Replays go as the provider wrote them, so only sealed ones go,
        // and then all of them: dropping one would change what the later
        // ones were written after, and the provider refuses those (ADR
        // 0043).
        let replays_dropped = call
            .input
            .messages
            .iter()
            .filter_map(|m| m.replay.as_ref())
            .any(|r| !self.seal.opens(r));
        let mut vault = Vault::default();
        let system = call
            .input
            .system
            .as_deref()
            .map(|s| call.redactor.redact(s, &mut vault));
        // Tool calls and results carry data too (ADR 0041): every string
        // in them is redacted as message text is.
        let messages: Vec<Message> = call
            .input
            .messages
            .iter()
            .map(|m| Message {
                role: m.role,
                content: call.redactor.redact(&m.content, &mut vault),
                tool_calls: m
                    .tool_calls
                    .iter()
                    .map(|c| ToolCall {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        input: redact_json(&c.input, call.redactor, &mut vault),
                    })
                    .collect(),
                tool_results: m
                    .tool_results
                    .iter()
                    .map(|r| ToolResult {
                        id: r.id.clone(),
                        content: call.redactor.redact(&r.content, &mut vault),
                        is_error: r.is_error,
                    })
                    .collect(),
                // The provider's own words from this conversation, which
                // left redacted: sent back as they came, never redacted
                // again, which would change them.
                replay: if replays_dropped {
                    None
                } else {
                    m.replay.clone()
                },
            })
            .collect();
        let tools = &call.input.tools;
        let sent = if tools.is_empty() {
            json!({ "system": system, "messages": messages })
        } else {
            json!({ "system": system, "messages": messages, "tools": tools })
        };
        let mut record = CallRecord {
            label: call.route.label,
            route: call.route.rule.clone(),
            model: None,
            provider: None,
            provider_model: None,
            region: None,
            input_tokens: None,
            output_tokens: None,
            served_by: None,
            cost_usd: None,
            tools_offered: (!tools.is_empty()).then_some(tools.len()),
            tools_called: Vec::new(),
            redacted: vault.counts().clone(),
            replays_dropped,
            request: Digest::of(&sent.to_string()),
            response: None,
            failed_over: Vec::new(),
            prompt: call.log_prompts.then(|| sent.clone()),
            reply: None,
        };

        let mut problems = Vec::new();
        for step in &call.route.steps {
            // A replay goes back only to the provider that wrote it; others
            // get the text and tool calls.
            let elsewhere = messages.iter().any(|m| {
                m.replay
                    .as_ref()
                    .is_some_and(|r| r.provider != step.provider)
            });
            let theirs: Vec<Message>;
            let messages = if elsewhere {
                theirs = messages
                    .iter()
                    .map(|m| Message {
                        replay: m.replay.clone().filter(|r| r.provider == step.provider),
                        ..m.clone()
                    })
                    .collect();
                &theirs
            } else {
                &messages
            };
            let result = match self.providers.get(&step.provider) {
                Some(provider) => provider.complete(&ProviderRequest {
                    model: &step.provider_model,
                    system: system.as_deref(),
                    messages,
                    max_tokens: call.input.max_tokens,
                    tools,
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
                    record.cost_usd = reply
                        .cost_usd
                        .or_else(|| cost(step, reply.input_tokens, reply.output_tokens));
                    record.served_by.clone_from(&reply.served_by);
                    record.tools_called = reply.tool_calls.iter().map(|c| c.name.clone()).collect();
                    let answered = if reply.tool_calls.is_empty() {
                        reply.text.clone()
                    } else {
                        json!({ "text": reply.text, "tool_calls": reply.tool_calls }).to_string()
                    };
                    record.response = Some(Digest::of(&answered));
                    if call.log_prompts {
                        record.reply = Some(answered);
                    }
                    return Ok(Completion {
                        text: vault.restore(&reply.text),
                        tool_calls: reply
                            .tool_calls
                            .iter()
                            .map(|c| ToolCall {
                                id: c.id.clone(),
                                name: c.name.clone(),
                                input: restore_json(&c.input, &vault),
                            })
                            .collect(),
                        stop_reason: reply.stop_reason,
                        replay: if reply.blocks.is_empty() {
                            None
                        } else {
                            self.seal.seal(&step.provider, reply.blocks)
                        },
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

/// `value` with every string in it redacted.
fn redact_json(
    value: &serde_json::Value,
    redactor: &Redactor,
    vault: &mut Vault,
) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(redactor.redact(s, vault)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| redact_json(v, redactor, vault))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_json(v, redactor, vault)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `value` with this call's PII put back in every string.
fn restore_json(value: &serde_json::Value, vault: &Vault) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(vault.restore(s)),
        Value::Array(items) => Value::Array(items.iter().map(|v| restore_json(v, vault)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), restore_json(v, vault)))
                .collect(),
        ),
        other => other.clone(),
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
    use crate::{ProviderReply, Role, ScriptedProvider};

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
            messages: vec![Message::text(Role::User, text)],
            max_tokens: 500,
            system: Some("Be brief.".into()),
            model: None,
            tools: Vec::new(),
        }
    }

    fn redactor() -> Redactor {
        Redactor::new(&Redaction {
            pii: vec![PiiKind::Email],
            patterns: Vec::new(),
        })
    }

    /// A router: it names the model it chose and what the call cost.
    #[derive(Debug)]
    struct Router;

    impl ModelProvider for Router {
        fn complete(&self, _: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
            Ok(ProviderReply {
                text: "ok".into(),
                input_tokens: 1_000_000,
                output_tokens: 0,
                stop_reason: Some("stop".into()),
                cost_usd: Some(0.42),
                served_by: Some("anthropic/claude-sonnet-4.5".into()),
                tool_calls: Vec::new(),
                blocks: Vec::new(),
            })
        }
    }

    fn files_tool(name: &str) -> crate::Tool {
        crate::Tool {
            name: name.into(),
            description: "Files in the task's folders.".into(),
            input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        }
    }

    #[test]
    fn adr0041_tool_results_are_redacted_and_the_models_calls_get_pii_back() {
        let scripted = Arc::new(ScriptedProvider::echo());
        let gateway = Gateway::new().with("scripted", scripted.clone());
        let route = route(vec![step("echo", "scripted")]);
        let redactor = redactor();

        // The model asks for files_list, its input naming a person: the
        // placeholder it saw comes back as the address.
        let mut asked = input(r#"Use files_list {"path": "Docs", "owner": "ana@example.com"}"#);
        asked.tools = vec![files_tool("files_list"), files_tool("files_read")];
        let done = gateway
            .complete(Call {
                input: &asked,
                route: &route,
                redactor: &redactor,
                log_prompts: false,
            })
            .unwrap();
        assert_eq!(done.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(done.tool_calls.len(), 1);
        assert_eq!(done.tool_calls[0].name, "files_list");
        assert_eq!(done.tool_calls[0].input["owner"], "ana@example.com");
        let sent = &scripted.seen()[0];
        assert!(sent.messages[0].content.contains("[redacted:email#1]"));
        assert_eq!(done.record.tools_offered, Some(2));
        assert_eq!(done.record.tools_called, vec!["files_list".to_owned()]);
        let record = serde_json::to_string(&done.record).unwrap();
        assert!(
            !record.contains("ana@example.com") && !record.contains("Docs"),
            "{record}"
        );

        // The agent made the call and sends back what it returned: redacted
        // on the way out like any text.
        let mut answered = asked.clone();
        answered.messages.push(Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: done.tool_calls.clone(),
            tool_results: Vec::new(),
            replay: None,
        });
        answered.messages.push(Message {
            role: Role::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![crate::ToolResult {
                id: done.tool_calls[0].id.clone(),
                content: "notes.txt, from bo@example.com".into(),
                is_error: false,
            }],
            replay: None,
        });
        let done = gateway
            .complete(Call {
                input: &answered,
                route: &route,
                redactor: &redactor,
                log_prompts: false,
            })
            .unwrap();
        assert!(done.tool_calls.is_empty());
        assert_eq!(done.stop_reason.as_deref(), Some("end_turn"));
        let sent = &scripted.seen()[1];
        let result = &sent.messages[2].tool_results[0].content;
        assert!(
            result.contains("[redacted:email#") && !result.contains("bo@example.com"),
            "{result}"
        );
        assert!(
            sent.messages[1].tool_calls[0].input["owner"]
                .as_str()
                .unwrap()
                .starts_with("[redacted:email#")
        );
        // The reply names what the tools returned, with the address back.
        assert!(done.text.contains("bo@example.com"), "{}", done.text);
    }

    /// One tool round: `first` with the model's call answered, the reply's
    /// replay (changed by `edit`) on the assistant message.
    fn answered(
        first: &CompleteInput,
        done: &Completion,
        edit: impl FnOnce(&mut Replay),
    ) -> CompleteInput {
        let mut replay = done.replay.clone().expect("a replay");
        edit(&mut replay);
        let mut next = first.clone();
        next.messages.push(Message {
            role: Role::Assistant,
            content: done.text.clone(),
            tool_calls: done.tool_calls.clone(),
            tool_results: Vec::new(),
            replay: Some(replay),
        });
        next.messages.push(Message {
            role: Role::User,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results: vec![crate::ToolResult {
                id: done.tool_calls[0].id.clone(),
                content: "notes.txt, from bo@example.com".into(),
                is_error: false,
            }],
            replay: None,
        });
        next
    }

    fn asks_for_files() -> CompleteInput {
        // Addresses in the instructions and the request: the placeholders
        // must come out the same on every call for the replay to hold.
        let mut first =
            input(r#"Ask zoe@example.com, then use files_list {"owner": "ana@example.com"}"#);
        first.system = Some("Be brief. Copy ana@example.com.".into());
        first.tools = vec![files_tool("files_list")];
        first
    }

    #[test]
    fn adr0043_the_providers_blocks_go_back_unchanged_and_the_conversation_holds() {
        let scripted = Arc::new(ScriptedProvider::echo());
        let gateway = Gateway::new().with("scripted", scripted.clone());
        let route = route(vec![step("echo", "scripted")]);
        let redactor = redactor();
        let send = |input: &CompleteInput| {
            gateway.complete(Call {
                input,
                route: &route,
                redactor: &redactor,
                log_prompts: false,
            })
        };

        let first = asks_for_files();
        let done = send(&first).unwrap();
        let replay = done.replay.clone().unwrap();
        assert_eq!(replay.provider, "scripted");
        assert_eq!(replay.blocks[0]["type"], "thinking");
        // The blocks stay as the provider wrote them: placeholders in.
        let blocks = serde_json::to_string(&replay.blocks).unwrap();
        assert!(!blocks.contains("ana@example.com"), "{blocks}");
        assert_eq!(done.tool_calls[0].input["owner"], "ana@example.com");

        // Sent back: the provider gets them byte for byte, and its check
        // that nothing before them changed passes, redaction included.
        let done = send(&answered(&first, &done, |_| {})).unwrap();
        assert!(!done.record.replays_dropped);
        let sent = &scripted.seen()[1];
        assert_eq!(sent.messages[1].replay.as_ref(), Some(&replay));
        assert!(
            done.text.ends_with("Its reasoning came back unchanged."),
            "{}",
            done.text
        );
        assert!(done.replay.is_some());
    }

    #[test]
    fn adr0043_a_changed_replay_never_leaves() {
        let scripted = Arc::new(ScriptedProvider::echo());
        let gateway = Gateway::new().with("scripted", scripted.clone());
        let route = route(vec![step("echo", "scripted")]);
        let redactor = redactor();
        let send = |input: &CompleteInput| {
            gateway
                .complete(Call {
                    input,
                    route: &route,
                    redactor: &redactor,
                    log_prompts: false,
                })
                .unwrap()
        };
        let first = asks_for_files();
        let done = send(&first);

        // A secret written into a block, a seal from another run, and a
        // block moved to another provider: none of them is sent, and the
        // record says so.
        let edits: [fn(&mut Replay); 3] = [
            |r| r.blocks[0]["signature"] = json!("AKIAIOSFODNN7EXAMPLE"),
            |r| r.seal = "0".repeat(64),
            |r| r.provider = "other".into(),
        ];
        for edit in edits {
            let changed = send(&answered(&first, &done, edit));
            assert!(changed.record.replays_dropped);
            let sent = scripted.seen().pop().unwrap();
            assert!(sent.messages.iter().all(|m| m.replay.is_none()));
            assert!(!changed.text.contains("came back"), "{}", changed.text);
        }

        // Another gateway, as after a restart, has another key.
        let other = Gateway::new().with("scripted", scripted.clone());
        let after = other
            .complete(Call {
                input: &answered(&first, &done, |_| {}),
                route: &route,
                redactor: &redactor,
                log_prompts: false,
            })
            .unwrap();
        assert!(after.record.replays_dropped);
    }

    #[test]
    fn adr0043_a_replay_goes_back_only_to_the_provider_that_wrote_it() {
        let wrote = Arc::new(ScriptedProvider::echo());
        let spare = Arc::new(ScriptedProvider::echo());
        let gateway = Gateway::new()
            .with("wrote", wrote.clone())
            .with("spare", spare.clone());
        let redactor = redactor();
        let first = asks_for_files();
        let done = gateway
            .complete(Call {
                input: &first,
                route: &route(vec![step("echo", "wrote")]),
                redactor: &redactor,
                log_prompts: false,
            })
            .unwrap();
        let done = gateway
            .complete(Call {
                input: &answered(&first, &done, |_| {}),
                route: &route(vec![step("echo", "spare")]),
                redactor: &redactor,
                log_prompts: false,
            })
            .unwrap();
        assert!(!done.record.replays_dropped);
        let sent = &spare.seen()[0];
        assert!(sent.messages[1].replay.is_none());
        assert_eq!(sent.messages[1].tool_calls.len(), 1);
    }

    #[test]
    fn adr0039_what_the_provider_charged_and_who_answered_go_on_the_record() {
        let gateway = Gateway::new().with("credits", Arc::new(Router));
        let input = input("hello");
        let route = route(vec![step("auto", "credits")]);
        let done = gateway
            .complete(Call {
                input: &input,
                route: &route,
                redactor: &redactor(),
                log_prompts: false,
            })
            .unwrap();
        // Policy's price would say $3.00; the provider's charge wins.
        assert_eq!(done.record.cost_usd, Some(0.42));
        assert_eq!(
            done.record.served_by.as_deref(),
            Some("anthropic/claude-sonnet-4.5")
        );
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
