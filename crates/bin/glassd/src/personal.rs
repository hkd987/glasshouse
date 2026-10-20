//! An individual's own AI models (ADR 0039 §6): on a computer no
//! organization manages, its owner (ADR 0038) connects OpenRouter or
//! Anthropic with their key, in Settings. glassd keeps the key root only
//! beside its state, which no jail mounts, adds the service to the
//! computer's own policy as its default model, and rebuilds the gateway
//! with it. The key is never handed back, logged or recorded.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use glass_first_party::ModelKeys;
use glass_policy::Policy;
use glass_policy::document::{ModelEntry, ProviderEntry, ProviderKind};
use glass_runtime::Runtime;

use crate::OrgData;
use crate::config::Config;

/// The provider and model id the owner's service has in policy.
pub(crate) const PROVIDER: &str = "personal";

/// Where the owner's key is kept: root only, in glassd's state.
pub(crate) fn path(config: &Config) -> PathBuf {
    config.state_dir.join("models/personal.toml")
}

/// The service the owner connected, from the kept file.
pub(crate) fn connected(config: &Config) -> Option<String> {
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(path(config)).ok()?).ok()?;
    Some(table.get(PROVIDER)?.get("service")?.as_str()?.to_owned())
}

/// `policy` with the owner's `service` as a provider and, when policy names
/// none, the default model for public and internal data. A policy that
/// already names `personal`, or one the result wouldn't validate as, stays
/// as it was.
pub(crate) fn with_models(policy: Policy, service: Option<&str>) -> Policy {
    let Some(service) = service else {
        return policy;
    };
    let (kind, name, model, model_name) = match service {
        "openrouter" => (
            ProviderKind::OpenRouter,
            "Your OpenRouter account",
            glass_gateway_providers::OPENROUTER_AUTO,
            "The best model for each request",
        ),
        "anthropic" => (
            ProviderKind::Anthropic,
            "Your Anthropic account",
            "claude-sonnet-5-5",
            "Claude Sonnet",
        ),
        _ => return policy,
    };
    let mut doc = policy.document().clone();
    if doc.models.providers.iter().any(|p| p.id == PROVIDER) {
        return policy;
    }
    doc.models.providers.push(ProviderEntry {
        id: PROVIDER.into(),
        kind,
        name: name.into(),
        region: None,
        endpoint: None,
    });
    doc.models.models.push(ModelEntry {
        id: PROVIDER.into(),
        provider: PROVIDER.into(),
        model: model.into(),
        name: model_name.into(),
        usd_per_mtok_in: None,
        usd_per_mtok_out: None,
    });
    doc.models.default.get_or_insert_with(|| PROVIDER.into());
    match Policy::from_document(doc) {
        Ok(with) => with,
        Err(e) => {
            tracing::warn!("the owner's AI models don't fit this policy: {e}");
            policy
        }
    }
}

/// The owner's AI models, as Settings connects them.
pub(crate) struct PersonalModels {
    config: Config,
    org: OrgData,
    /// The policy file's, before the owner's models are added.
    base: Mutex<Policy>,
    source: String,
    runtime: OnceLock<Weak<Runtime>>,
}

impl PersonalModels {
    pub(crate) fn new(config: Config, org: OrgData, base: Policy, source: String) -> Arc<Self> {
        Arc::new(Self {
            config,
            org,
            base: Mutex::new(base),
            source,
            runtime: OnceLock::new(),
        })
    }

    pub(crate) fn attach(&self, runtime: &Arc<Runtime>) {
        let _ = self.runtime.set(Arc::downgrade(runtime));
    }

    /// Puts the policy with the owner's models, and a gateway with their
    /// key, in force.
    fn reload(&self) -> Result<(), String> {
        let Some(runtime) = self.runtime.get().and_then(Weak::upgrade) else {
            return Err("glassd is stopping".into());
        };
        let base = self.base.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let policy = with_models(base, connected(&self.config).as_deref());
        let registry =
            crate::registry(&self.config, &policy, &self.org).map_err(|e| format!("{e:#}"))?;
        let models = crate::gateway(&self.config, &policy, &self.org);
        runtime.apply_policy(policy, registry, self.source.clone());
        runtime.set_gateway(models.gateway);
        Ok(())
    }
}

impl ModelKeys for PersonalModels {
    fn connected(&self) -> Option<String> {
        connected(&self.config)
    }

    fn connect(&self, choice: Option<(&str, &str)>) -> Result<(), String> {
        let file = path(&self.config);
        match choice {
            Some((service, key)) => {
                let text = format!(
                    "# The computer's owner's AI models, from Settings (ADR 0039).\n[{PROVIDER}]\nservice = {}\napi_key = {}\n",
                    toml::Value::String(service.into()),
                    toml::Value::String(key.into()),
                );
                crate::write_root_only(&file, text.as_bytes())
                    .map_err(|e| format!("the key couldn't be kept: {e}"))?;
            }
            None => match std::fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("the key couldn't be removed: {e}")),
            },
        }
        self.reload()
    }

    fn setup_choice(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.config.state_dir.join("setup.toml")).ok()?;
        let table: toml::Table = toml::from_str(&text).ok()?;
        Some(table.get("models")?.as_str()?.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0039_the_owners_service_joins_the_policy_as_its_default_model() {
        let base = Policy::from_toml(glass_policy::DEFAULT_POLICY).unwrap();
        assert!(base.document().models.default.is_none());

        let with = with_models(base.clone(), Some("openrouter"));
        let models = &with.document().models;
        assert_eq!(models.default.as_deref(), Some(PROVIDER));
        assert_eq!(models.providers[0].kind, ProviderKind::OpenRouter);
        assert_eq!(models.models[0].model, "openrouter/auto");
        let with = with_models(base.clone(), Some("anthropic"));
        assert_eq!(
            with.document().models.providers[0].kind,
            ProviderKind::Anthropic
        );

        // Nothing connected, or nothing we know: the policy as it was.
        assert_eq!(with_models(base.clone(), None).document(), base.document());
        assert_eq!(
            with_models(base.clone(), Some("elsewhere")).document(),
            base.document()
        );
        // A default the policy names stays.
        let named = Policy::from_toml(&format!(
            "{}\n[models]\ndefault = \"house\"\n[[models.provider]]\nid = \"scripted\"\nkind = \"scripted\"\nname = \"Scripted\"\n[[models.model]]\nid = \"house\"\nprovider = \"scripted\"\nmodel = \"echo\"\nname = \"House\"\n",
            glass_policy::DEFAULT_POLICY
        ))
        .unwrap();
        let with = with_models(named, Some("openrouter"));
        assert_eq!(with.document().models.default.as_deref(), Some("house"));
    }
}
