//! The model providers that leave the machine (ADR 0018): the Anthropic
//! Messages API (MP-4) and Amazon Bedrock (MP-1, MP-2), over HTTPS.
//!
//! This is the only crate with an HTTP and TLS stack: ureq on the base
//! system's OpenSSL, through native-tls, verifying servers against the
//! system trust store. Keeping it here keeps every other crate free of it,
//! and only `glassd` links it.
//!
//! [`connect`] builds the gateway for a machine from policy's providers and
//! the secrets file. A provider without usable credentials is left out with
//! a note, so calls routed to it fail over to the next model (MP-8).

mod anthropic;
mod bedrock;
mod http;
mod sigv4;

use std::sync::Arc;

use glass_gateway::{Gateway, ScriptedProvider, Secrets};
use glass_policy::document::{ProviderEntry, ProviderKind};

pub use anthropic::AnthropicProvider;
pub use bedrock::{AwsCredentials, BedrockProvider};

/// A gateway with a provider for each policy entry this machine can use,
/// and a note for each it can't. Notes never contain a credential.
pub fn connect(providers: &[ProviderEntry], secrets: &Secrets) -> (Gateway, Vec<String>) {
    let mut gateway = Gateway::new();
    let mut notes = Vec::new();
    for entry in providers {
        match entry.kind {
            ProviderKind::Scripted => gateway.add(&entry.id, Arc::new(ScriptedProvider::echo())),
            ProviderKind::Anthropic => match secrets.get(&entry.id, "api_key") {
                Some(key) => gateway.add(
                    &entry.id,
                    Arc::new(AnthropicProvider::new(
                        entry.endpoint.as_deref(),
                        key.clone(),
                    )),
                ),
                None => notes.push(format!(
                    "model provider {}: no api_key in the secrets file, so calls skip it",
                    entry.id
                )),
            },
            ProviderKind::Bedrock => match AwsCredentials::from_secrets(secrets, &entry.id) {
                Ok(credentials) => gateway.add(
                    &entry.id,
                    Arc::new(BedrockProvider::new(
                        entry.region.as_deref().unwrap_or("us-east-1"),
                        entry.endpoint.as_deref(),
                        credentials,
                    )),
                ),
                Err(why) => notes.push(format!(
                    "model provider {}: {why}, so calls skip it",
                    entry.id
                )),
            },
        }
    }
    (gateway, notes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn providers_without_credentials_are_left_out_with_a_note() {
        let policy = glass_policy::Policy::from_toml(
            r#"
            version = 1
            [[models.provider]]
            id = "scripted"
            kind = "scripted"
            name = "Scripted"
            [[models.provider]]
            id = "anthropic"
            kind = "anthropic"
            name = "Anthropic API"
            [[models.provider]]
            id = "bedrock"
            kind = "bedrock"
            name = "Amazon Bedrock"
            region = "us-east-1"
            "#,
        )
        .unwrap();
        let providers = &policy.document().models.providers;
        let secrets = Secrets::parse("[anthropic]\napi_key = \"not-a-real-key\"\n").unwrap();
        let (gateway, notes) = connect(providers, &secrets);
        assert_eq!(
            gateway.providers().collect::<Vec<_>>(),
            ["anthropic", "scripted"]
        );
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("model provider bedrock: no access_key_id"));
        assert!(!format!("{gateway:?} {notes:?}").contains("not-a-real-key"));
    }
}
