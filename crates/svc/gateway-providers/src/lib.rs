//! The model providers that leave the machine (ADR 0018): the Anthropic
//! Messages API (MP-4), Amazon Bedrock (MP-1, MP-2) and OpenRouter (model
//! credits and individuals' own keys, ADR 0039), over HTTPS; Stripe, which
//! takes payment for model credits ([`stripe`]); the
//! org's admin console, for managed settings ([`ConsoleClient`], ADR
//! 0023); and the person's mail servers, over IMAP and SMTP submission
//! ([`ImapLink`], ADR 0037).
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
pub mod billing;
mod console;
mod credits;
mod http;
pub mod mail;
mod openrouter;
mod sigv4;
pub mod stripe;
mod tools;

use std::sync::Arc;

use glass_gateway::{Gateway, ScriptedProvider, Secrets};
use glass_policy::document::{ProviderEntry, ProviderKind};

pub use anthropic::AnthropicProvider;
pub use bedrock::{AwsCredentials, BedrockProvider};
pub use console::ConsoleClient;
pub use credits::{CreditsLink, CreditsProvider};
pub use mail::ImapLink;
pub use openrouter::{
    AUTO as OPENROUTER_AUTO, Account as OpenRouterAccount, KeyUsage, MadeKey, OpenRouterKeys,
    OpenRouterProvider,
};

/// A gateway with a provider for each policy entry this machine can use,
/// and a note for each it can't. Notes never contain a credential. Model
/// credits go through the org's console, on a managed computer's `credits`
/// link (ADR 0039).
pub fn connect(
    providers: &[ProviderEntry],
    secrets: &Secrets,
    credits: Option<&Arc<CreditsLink>>,
) -> (Gateway, Vec<String>) {
    let mut gateway = Gateway::new();
    let mut notes = Vec::new();
    for entry in providers {
        match entry.kind {
            ProviderKind::Scripted => gateway.add(&entry.id, Arc::new(ScriptedProvider::echo())),
            ProviderKind::Anthropic => match secrets.get(&entry.id, "api_key") {
                Some(key) => gateway.add(
                    &entry.id,
                    Arc::new(
                        AnthropicProvider::new(entry.endpoint.as_deref(), key.clone())
                            .checking_history(entry.check_history),
                    ),
                ),
                None => notes.push(format!(
                    "model provider {}: no api_key in the secrets file, so calls skip it",
                    entry.id
                )),
            },
            ProviderKind::Credits => match credits {
                Some(link) => gateway.add(&entry.id, Arc::new(CreditsProvider::new(Arc::clone(link)))),
                None => notes.push(format!(
                    "model provider {}: model credits come through an organization's admin console, and no organization manages this computer",
                    entry.id
                )),
            },
            ProviderKind::OpenRouter => match secrets.get(&entry.id, "api_key") {
                Some(key) => gateway.add(
                    &entry.id,
                    Arc::new(OpenRouterProvider::new(
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
            [[models.provider]]
            id = "credits"
            kind = "openrouter"
            name = "Glasshouse model credits"
            "#,
        )
        .unwrap();
        let providers = &policy.document().models.providers;
        let secrets = Secrets::parse(
            "[anthropic]\napi_key = \"not-a-real-key\"\n[credits]\napi_key = \"sk-or-not-real\"\n",
        )
        .unwrap();
        let (gateway, notes) = connect(providers, &secrets, None);
        assert_eq!(
            gateway.providers().collect::<Vec<_>>(),
            ["anthropic", "credits", "scripted"]
        );
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("model provider bedrock: no access_key_id"));
        let shown = format!("{gateway:?} {notes:?}");
        assert!(!shown.contains("not-a-real-key") && !shown.contains("sk-or-not-real"));
    }
}
