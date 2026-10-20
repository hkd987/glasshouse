//! A provider that answers on this machine with a deterministic reply and
//! sends nothing anywhere: for tests, development and the e2e suite.

use std::sync::Mutex;

use crate::{Message, ModelProvider, ProviderError, ProviderReply, ProviderRequest};

/// What a scripted provider was sent, for tests to inspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

#[derive(Debug)]
pub struct ScriptedProvider {
    fails: Option<ProviderError>,
    seen: Mutex<Vec<Seen>>,
}

impl Default for ScriptedProvider {
    fn default() -> Self {
        Self::echo()
    }
}

impl ScriptedProvider {
    /// Replies by describing what it received, including the last user
    /// message and the instructions as they arrived, so a test can see
    /// what left the machine.
    pub fn echo() -> Self {
        Self {
            fails: None,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Fails every call with `error`, to exercise failover.
    pub fn failing(error: ProviderError) -> Self {
        Self {
            fails: Some(error),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Every request it has been sent.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// A rough token count: four characters a token, as providers estimate.
fn tokens(chars: usize) -> u64 {
    chars.div_ceil(4) as u64
}

impl ModelProvider for ScriptedProvider {
    fn complete(&self, request: &ProviderRequest<'_>) -> Result<ProviderReply, ProviderError> {
        self.seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Seen {
                model: request.model.to_owned(),
                system: request.system.map(str::to_owned),
                messages: request.messages.to_vec(),
                max_tokens: request.max_tokens,
            });
        if let Some(error) = &self.fails {
            return Err(error.clone());
        }
        let chars: usize = request.system.map_or(0, |s| s.chars().count())
            + request
                .messages
                .iter()
                .map(|m| m.content.chars().count())
                .sum::<usize>();
        let last: String = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == crate::Role::User)
            .map(|m| m.content.chars().take(2000).collect())
            .unwrap_or_default();
        let mut text = format!(
            "Scripted reply from {model} (nothing left this computer). You sent {n} message(s), \
             {chars} characters. Your last message read: {last}",
            model = request.model,
            n = request.messages.len(),
        );
        if let Some(system) = request.system {
            let system: String = system.chars().take(2000).collect();
            text.push_str(&format!(" Your instructions read: {system}"));
        }
        let output = tokens(text.chars().count()).min(u64::from(request.max_tokens));
        Ok(ProviderReply {
            input_tokens: tokens(chars),
            output_tokens: output,
            stop_reason: Some("end_turn".into()),
            cost_usd: None,
            served_by: None,
            text,
        })
    }
}
