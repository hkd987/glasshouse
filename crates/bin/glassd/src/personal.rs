//! An individual's own AI models (ADR 0039 §6): on a computer no
//! organization manages, its owner (ADR 0038) connects OpenRouter or
//! Anthropic with their key, in Settings. glassd keeps the key root only
//! beside its state, which no jail mounts, adds the service to the
//! computer's own policy as its default model, and rebuilds the gateway
//! with it. The key is never handed back, logged or recorded.
//!
//! The owner also chooses which agents may ask those models: the IT agent
//! until they say otherwise, any other on their word. Their choice is kept
//! beside the key and adds `model.complete` to those agents' rules.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use glass_first_party::{ModelAgent, ModelKeys};
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

/// Which agents may ask the owner's models until they choose: ours that
/// ask models for the person (AE-4).
pub(crate) const DEFAULT_AGENTS: [&str; 1] = ["it-agent"];

/// The action an agent needs to ask a model.
const MODEL_COMPLETE: &str = "model.complete";

/// Where the owner's choice of agents is kept.
fn agents_path(config: &Config) -> PathBuf {
    config.state_dir.join("models/agents.toml")
}

/// The agents the owner lets ask their models: their choice, or ours.
pub(crate) fn chosen(config: &Config) -> Vec<String> {
    std::fs::read_to_string(agents_path(config))
        .ok()
        .and_then(|text| toml::from_str::<toml::Table>(&text).ok())
        .and_then(|t| {
            t.get("agents")?.as_array().map(|list| {
                list.iter()
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_else(|| DEFAULT_AGENTS.iter().map(|a| (*a).to_owned()).collect())
}

/// Whether `rules` let an agent ask models already.
fn asks_models(rules: &glass_policy::document::AgentPolicy) -> bool {
    let action: glass_types::ActionName = MODEL_COMPLETE.parse().expect("static name");
    rules.allowed_actions.iter().any(|p| p.matches(&action))
}

/// The service the owner connected, from the kept file.
pub(crate) fn connected(config: &Config) -> Option<String> {
    let table: toml::Table = toml::from_str(&std::fs::read_to_string(path(config)).ok()?).ok()?;
    Some(table.get(PROVIDER)?.get("service")?.as_str()?.to_owned())
}

/// `policy` with the owner's `service` as a provider and, when policy names
/// none, the default model for public and internal data, and `agents`
/// allowed to ask it. A policy that already names `personal`, or one the
/// result wouldn't validate as, stays as it was.
pub(crate) fn with_models(policy: Policy, service: Option<&str>, agents: &[String]) -> Policy {
    let Some(service) = service else {
        return policy;
    };
    // OpenRouter says what each call cost; Anthropic's prices are its
    // list prices for the model (ADR 0043).
    let (kind, name, model, model_name, prices) = match service {
        "openrouter" => (
            ProviderKind::OpenRouter,
            "Your OpenRouter account",
            glass_gateway_providers::OPENROUTER_AUTO,
            "The best model for each request",
            [None; 3],
        ),
        "anthropic" => (
            ProviderKind::Anthropic,
            "Your Anthropic account",
            "claude-opus-5-5",
            "Claude Opus",
            [Some(4.0), Some(20.0), Some(0.2)],
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
        usd_per_mtok_in: prices[0],
        usd_per_mtok_out: prices[1],
        usd_per_mtok_cache_read: prices[2],
    });
    doc.models.default.get_or_insert_with(|| PROVIDER.into());
    for rules in doc
        .agents
        .iter_mut()
        .filter(|a| agents.iter().any(|id| id == a.id.as_str()))
    {
        if !asks_models(rules) {
            rules
                .allowed_actions
                .push(MODEL_COMPLETE.parse().expect("static pattern"));
        }
    }
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
        let policy = with_models(
            base,
            connected(&self.config).as_deref(),
            &chosen(&self.config),
        );
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

    /// The agents on this computer that run a program of their own (not a
    /// person's harness), with policy rules.
    fn agents(&self) -> Vec<ModelAgent> {
        let base = self.base.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let chosen = chosen(&self.config);
        self.config
            .agents
            .iter()
            .filter(|entry| !entry.attended)
            .filter_map(|entry| {
                let rules = base.document().agents.iter().find(|a| a.id == entry.id)?;
                let always = asks_models(rules);
                Some(ModelAgent {
                    id: entry.id.to_string(),
                    name: entry.name.clone(),
                    on: always || chosen.iter().any(|c| c == entry.id.as_str()),
                    always,
                })
            })
            .collect()
    }

    fn allow(&self, agent: &str, on: bool) -> Result<(), String> {
        let mut chosen = chosen(&self.config);
        chosen.retain(|a| a != agent);
        if on {
            chosen.push(agent.to_owned());
        }
        chosen.sort();
        let text = format!(
            "# The agents the computer's owner lets ask their AI models, from Settings (ADR 0039).\nagents = {}\n",
            toml::Value::Array(chosen.into_iter().map(toml::Value::String).collect())
        );
        crate::write_root_only(&agents_path(&self.config), text.as_bytes())
            .map_err(|e| format!("the choice couldn't be kept: {e}"))?;
        self.reload()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0039_the_owners_service_joins_the_policy_as_its_default_model() {
        let base = Policy::from_toml(glass_policy::DEFAULT_POLICY).unwrap();
        assert!(base.document().models.default.is_none());

        let with = with_models(base.clone(), Some("openrouter"), &[]);
        let models = &with.document().models;
        assert_eq!(models.default.as_deref(), Some(PROVIDER));
        assert_eq!(models.providers[0].kind, ProviderKind::OpenRouter);
        assert_eq!(models.models[0].model, "openrouter/auto");
        let with = with_models(base.clone(), Some("anthropic"), &[]);
        assert_eq!(
            with.document().models.providers[0].kind,
            ProviderKind::Anthropic
        );

        // Nothing connected, or nothing we know: the policy as it was.
        assert_eq!(
            with_models(base.clone(), None, &[]).document(),
            base.document()
        );
        assert_eq!(
            with_models(base.clone(), Some("elsewhere"), &[]).document(),
            base.document()
        );
        // A default the policy names stays.
        let named = Policy::from_toml(&format!(
            "{}\n[models]\ndefault = \"house\"\n[[models.provider]]\nid = \"scripted\"\nkind = \"scripted\"\nname = \"Scripted\"\n[[models.model]]\nid = \"house\"\nprovider = \"scripted\"\nmodel = \"echo\"\nname = \"House\"\n",
            glass_policy::DEFAULT_POLICY
        ))
        .unwrap();
        let with = with_models(named, Some("openrouter"), &[]);
        assert_eq!(with.document().models.default.as_deref(), Some("house"));
    }

    #[test]
    fn adr0039_the_agents_the_owner_chooses_may_ask_their_models_and_no_others() {
        let base = Policy::from_toml(glass_policy::DEFAULT_POLICY).unwrap();
        let asks = |p: &Policy, id: &str| {
            p.document()
                .agents
                .iter()
                .find(|a| a.id.as_str() == id)
                .is_some_and(asks_models)
        };
        assert!(!asks(&base, "it-agent") && asks(&base, "skill-evals"));

        let chosen: Vec<String> = DEFAULT_AGENTS.iter().map(|a| (*a).to_owned()).collect();
        let with = with_models(base.clone(), Some("openrouter"), &chosen);
        assert!(asks(&with, "it-agent"));
        assert!(!asks(&with, "invoice-demo"));
        // Once, however often it's applied.
        let twice = with_models(with.clone(), Some("openrouter"), &chosen);
        let rules = twice
            .document()
            .agents
            .iter()
            .find(|a| a.id.as_str() == "it-agent")
            .unwrap();
        assert_eq!(
            rules
                .allowed_actions
                .iter()
                .filter(|p| p.as_str() == MODEL_COMPLETE)
                .count(),
            1
        );
        // None chosen, or no models connected: no agent gains them.
        assert!(!asks(
            &with_models(base.clone(), Some("openrouter"), &[]),
            "it-agent"
        ));
        assert!(!asks(&with_models(base.clone(), None, &chosen), "it-agent"));
        // An agent policy has no rules for gains nothing.
        let stranger = with_models(base, Some("openrouter"), &["nobody".to_owned()]);
        assert!(
            stranger
                .document()
                .agents
                .iter()
                .all(|a| a.id.as_str() != "nobody")
        );
    }
}
