//! Model routing (DP-2, MP-6, MP-8): which approved model a call may go to,
//! given the most sensitive label of the data it carries.
//!
//! The org's `[models]` list is the allowlist. Group and agent `models`
//! lists, and anything a task applies later, only ever remove models from
//! it, so nothing below IT's policy can widen where data goes (ADR 0014).

use std::collections::BTreeSet;

use glass_types::Label;
use serde::{Deserialize, Serialize};

use crate::document::{GroupPolicy, Models, ProviderKind, Redaction};

/// The model rules in force for one task.
#[derive(Debug, Clone)]
pub struct ModelRules {
    doc: Models,
    /// Model ids this task may use: the org's list, narrowed.
    allowed: BTreeSet<String>,
}

/// Where one call goes: the models it may use, in order (MP-8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Route {
    /// The label the call was routed for.
    pub label: Label,
    /// The first is tried first; the rest take over on an outage.
    pub steps: Vec<RouteStep>,
    /// The rule that chose these, for the audit record.
    pub rule: String,
}

/// One model a call may go to, with everything the gateway needs to reach it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteStep {
    /// The policy's id for the model.
    pub model: String,
    /// As users see it: "Claude Sonnet".
    pub name: String,
    /// The policy's id for the provider; also its key in the secrets file.
    pub provider: String,
    /// As users see it: "Amazon Bedrock".
    pub provider_name: String,
    pub kind: ProviderKind,
    /// The provider's own model id.
    pub provider_model: String,
    pub region: Option<String>,
    pub endpoint: Option<String>,
    pub usd_per_mtok_in: Option<f64>,
    pub usd_per_mtok_out: Option<f64>,
    pub usd_per_mtok_cache_read: Option<f64>,
}

/// Why a call can't go anywhere. The reason is shown to the agent and the
/// user; the rule goes in the audit record.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct ModelRefusal {
    pub reason: String,
    pub rule: String,
}

impl ModelRules {
    /// The rules for an agent in `groups`: the org's models, less any a group
    /// or the agent leaves out.
    pub(crate) fn new(doc: &Models, groups: &[&GroupPolicy], agent: Option<&[String]>) -> Self {
        let mut allowed: BTreeSet<String> = doc.models.iter().map(|m| m.id.clone()).collect();
        let lists = groups
            .iter()
            .filter_map(|g| g.models.as_deref())
            .chain(agent);
        for list in lists {
            allowed.retain(|id| list.contains(id));
        }
        Self {
            doc: doc.clone(),
            allowed,
        }
    }

    /// Keeps only the models in `keep`. Never adds one: a model outside the
    /// current list stays out (ADR 0014, layers only narrow).
    pub fn narrow(&mut self, keep: &[String]) {
        self.allowed.retain(|id| keep.contains(id));
    }

    /// Model ids this task may use.
    pub fn allowed(&self) -> impl Iterator<Item = &str> {
        self.allowed.iter().map(String::as_str)
    }

    /// The label of content no rule labels.
    pub fn unlabeled(&self) -> Label {
        self.doc.unlabeled
    }

    pub fn max_output_tokens(&self) -> u32 {
        self.doc.max_output_tokens
    }

    /// Whether audit records keep the redacted prompt and reply text.
    pub fn log_prompts(&self) -> bool {
        self.doc.log_prompts
    }

    pub fn redaction(&self) -> &Redaction {
        &self.doc.redact
    }

    /// Where a call carrying data labeled `label` may go. `requested`, a
    /// model id the agent asked for, goes first if it's allowed and is a
    /// refusal if not.
    pub fn route(&self, label: Label, requested: Option<&str>) -> Result<Route, ModelRefusal> {
        if !label.agent_may_open() {
            return Err(ModelRefusal {
                reason: format!("{label} data never goes to a model"),
                rule: format!("data label {label}"),
            });
        }
        let rule = format!("model route for {label} data");
        let candidates: Vec<&str> = match self.doc.routes.get(&label) {
            Some(ids) => ids.iter().map(String::as_str).collect(),
            // Confidential data goes only where a route says (PRD data labels).
            None if label <= Label::Internal => self.doc.default.as_deref().into_iter().collect(),
            None => Vec::new(),
        };
        if candidates.is_empty() {
            return Err(ModelRefusal {
                reason: format!("no model is approved for {label} data"),
                rule,
            });
        }
        let mut usable: Vec<&str> = candidates
            .into_iter()
            .filter(|id| self.allowed.contains(*id))
            .collect();
        if usable.is_empty() {
            return Err(ModelRefusal {
                reason: format!(
                    "none of the models approved for {label} data is allowed for this agent"
                ),
                rule: format!("{rule}, narrowed by group or agent model lists"),
            });
        }
        if let Some(want) = requested {
            let Some(at) = usable.iter().position(|id| *id == want) else {
                return Err(ModelRefusal {
                    reason: format!(
                        "{want} isn't approved for {label} data for this agent; leave model out, or use {}",
                        usable.join(" or ")
                    ),
                    rule,
                });
            };
            let first = usable.remove(at);
            usable.insert(0, first);
        }
        let steps = usable.into_iter().filter_map(|id| self.step(id)).collect();
        Ok(Route { label, steps, rule })
    }

    fn step(&self, id: &str) -> Option<RouteStep> {
        let model = self.doc.models.iter().find(|m| m.id == id)?;
        let provider = self.doc.providers.iter().find(|p| p.id == model.provider)?;
        Some(RouteStep {
            model: model.id.clone(),
            name: model.name.clone(),
            provider: provider.id.clone(),
            provider_name: provider.name.clone(),
            kind: provider.kind,
            provider_model: model.model.clone(),
            region: provider.region.clone(),
            endpoint: provider.endpoint.clone(),
            usd_per_mtok_in: model.usd_per_mtok_in,
            usd_per_mtok_out: model.usd_per_mtok_out,
            usd_per_mtok_cache_read: model.usd_per_mtok_cache_read,
        })
    }
}

/// Checks the `[models]` section and every model list that refers to it.
pub(crate) fn validate(doc: &crate::document::PolicyDocument, errors: &mut Vec<String>) {
    let models = &doc.models;
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    };
    let mut providers = BTreeSet::new();
    for p in &models.providers {
        if !plain(&p.id) {
            errors.push(format!(
                "models.provider {:?}: ids are lowercase letters, digits, - and _",
                p.id
            ));
        }
        if !providers.insert(p.id.as_str()) {
            errors.push(format!(
                "models.provider {:?} is defined more than once",
                p.id
            ));
        }
        if let Some(endpoint) = &p.endpoint
            && !endpoint.starts_with("https://")
        {
            errors.push(format!(
                "models.provider {:?}: endpoint must be an https:// URL",
                p.id
            ));
        }
        match (&p.kind, &p.region) {
            (ProviderKind::Bedrock, None) => errors.push(format!(
                "models.provider {:?}: Bedrock calls need a region (MP-2)",
                p.id
            )),
            (_, Some(region)) if !plain(region) => {
                errors.push(format!(
                    "models.provider {:?}: {region:?} is not a region",
                    p.id
                ));
            }
            _ => {}
        }
    }
    let mut ids = BTreeSet::new();
    for m in &models.models {
        if !plain(&m.id) {
            errors.push(format!(
                "models.model {:?}: ids are lowercase letters, digits, - and _",
                m.id
            ));
        }
        if !ids.insert(m.id.as_str()) {
            errors.push(format!("models.model {:?} is defined more than once", m.id));
        }
        if !providers.contains(m.provider.as_str()) {
            errors.push(format!(
                "models.model {:?} runs on provider {:?}, which is not defined",
                m.id, m.provider
            ));
        }
        let prices = [
            m.usd_per_mtok_in,
            m.usd_per_mtok_out,
            m.usd_per_mtok_cache_read,
        ];
        if prices.iter().flatten().any(|p| !p.is_finite() || *p < 0.0) {
            errors.push(format!("models.model {:?}: prices can't be negative", m.id));
        }
    }
    let mut known = |what: String, id: &str| {
        if !ids.contains(id) {
            errors.push(format!("{what} names model {id:?}, which is not defined"));
        }
    };
    if let Some(id) = &models.default {
        known("models.default".into(), id);
    }
    for (label, route) in &models.routes {
        for id in route {
            known(format!("models.routes.{label}"), id);
        }
    }
    for group in &doc.groups {
        for id in group.models.iter().flatten() {
            known(format!("group {:?}", group.name), id);
        }
    }
    for agent in &doc.agents {
        for id in agent.models.iter().flatten() {
            known(format!("agent \"{}\"", agent.id), id);
        }
    }
    if models.routes.contains_key(&Label::Restricted) {
        errors.push("models.routes.restricted: restricted data never goes to a model".into());
    }
    if models.max_output_tokens == 0 {
        errors.push("models.max_output_tokens must be greater than zero".into());
    }
    let mut names = BTreeSet::new();
    for pattern in &models.redact.patterns {
        if !plain(&pattern.name) {
            errors.push(format!(
                "models.redact.pattern {:?}: names are lowercase letters, digits, - and _",
                pattern.name
            ));
        }
        if !names.insert(pattern.name.as_str()) {
            errors.push(format!(
                "models.redact.pattern {:?} is defined more than once",
                pattern.name
            ));
        }
        let len = pattern.pattern.chars().filter(|c| *c != '\\').count();
        if !(3..=100).contains(&len) {
            errors.push(format!(
                "models.redact.pattern {:?}: a pattern is 3 to 100 characters",
                pattern.name
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Policy, PolicyError};

    use super::*;

    const POLICY: &str = r#"
        version = 1

        [[group]]
        name = "everyone"
        agents = ["writer", "narrow"]

        [[group]]
        name = "finance"
        members = ["fran"]
        models = ["claude-bedrock", "claude-bedrock-west"]

        [[agent]]
        id = "writer"
        allowed_actions = ["model.complete"]

        [[agent]]
        id = "narrow"
        allowed_actions = ["model.complete"]
        models = ["claude-api"]

        [models]
        default = "claude-api"

        [models.routes]
        internal = ["claude-bedrock", "claude-bedrock-west", "claude-api"]
        confidential = ["claude-bedrock"]

        [[models.provider]]
        id = "bedrock"
        kind = "bedrock"
        name = "Amazon Bedrock"
        region = "us-east-1"

        [[models.provider]]
        id = "bedrock-west"
        kind = "bedrock"
        name = "Amazon Bedrock (us-west-2)"
        region = "us-west-2"

        [[models.provider]]
        id = "anthropic"
        kind = "anthropic"
        name = "Anthropic API"

        [[models.model]]
        id = "claude-bedrock"
        provider = "bedrock"
        model = "anthropic.claude-sonnet"
        name = "Claude Sonnet"
        usd_per_mtok_in = 3.0
        usd_per_mtok_out = 15.0

        [[models.model]]
        id = "claude-bedrock-west"
        provider = "bedrock-west"
        model = "anthropic.claude-sonnet"
        name = "Claude Sonnet"

        [[models.model]]
        id = "claude-api"
        provider = "anthropic"
        model = "claude-sonnet"
        name = "Claude Sonnet"
    "#;

    fn rules(user: &str, agent: &str) -> ModelRules {
        Policy::from_toml(POLICY)
            .unwrap()
            .effective(user, &agent.parse().unwrap())
            .unwrap()
            .models()
            .clone()
    }

    fn order(route: &Route) -> Vec<&str> {
        route.steps.iter().map(|s| s.model.as_str()).collect()
    }

    #[test]
    fn dp2_each_label_goes_where_its_route_says() {
        let rules = rules("dana", "writer");
        let public = rules.route(Label::Public, None).unwrap();
        assert_eq!(order(&public), ["claude-api"], "no route: the default");
        let internal = rules.route(Label::Internal, None).unwrap();
        assert_eq!(
            order(&internal),
            ["claude-bedrock", "claude-bedrock-west", "claude-api"]
        );
        assert_eq!(internal.rule, "model route for internal data");
        let step = &internal.steps[0];
        assert_eq!(
            (step.provider_name.as_str(), step.region.as_deref()),
            ("Amazon Bedrock", Some("us-east-1"))
        );
        assert_eq!(step.usd_per_mtok_out, Some(15.0));
        let confidential = rules.route(Label::Confidential, None).unwrap();
        assert_eq!(order(&confidential), ["claude-bedrock"], "Bedrock only");
        let restricted = rules.route(Label::Restricted, None).unwrap_err();
        assert!(restricted.reason.contains("never goes to a model"));
    }

    #[test]
    fn dp2_confidential_data_needs_a_route_of_its_own() {
        let text = POLICY.replace("confidential = [\"claude-bedrock\"]", "");
        let rules = Policy::from_toml(&text)
            .unwrap()
            .effective("dana", &"writer".parse().unwrap())
            .unwrap()
            .models()
            .clone();
        let refused = rules.route(Label::Confidential, None).unwrap_err();
        assert_eq!(refused.reason, "no model is approved for confidential data");
    }

    #[test]
    fn dp2_groups_and_agents_only_narrow_the_allowlist() {
        // MP-6: the finance group may use Bedrock only.
        let finance = rules("fran", "writer");
        assert_eq!(
            finance.allowed().collect::<Vec<_>>(),
            ["claude-bedrock", "claude-bedrock-west"]
        );
        assert_eq!(
            order(&finance.route(Label::Internal, None).unwrap()),
            ["claude-bedrock", "claude-bedrock-west"]
        );
        let public = finance.route(Label::Public, None).unwrap_err();
        assert!(public.rule.contains("narrowed"), "{public:?}");

        // A finance member running an agent limited to the API: nothing left.
        let both = rules("fran", "narrow");
        assert_eq!(both.allowed().count(), 0);
        assert!(both.route(Label::Internal, None).is_err());
    }

    #[test]
    fn dp2_narrowing_never_widens() {
        let mut rules = rules("fran", "writer");
        rules.narrow(&["claude-api".into(), "claude-bedrock".into()]);
        assert_eq!(
            rules.allowed().collect::<Vec<_>>(),
            ["claude-bedrock"],
            "claude-api was never allowed for finance, so naming it adds nothing"
        );
        rules.narrow(&[]);
        assert!(rules.route(Label::Public, None).is_err());
    }

    #[test]
    fn dp2_a_requested_model_goes_first_or_is_refused() {
        let rules = rules("dana", "writer");
        let route = rules.route(Label::Internal, Some("claude-api")).unwrap();
        assert_eq!(
            order(&route),
            ["claude-api", "claude-bedrock", "claude-bedrock-west"]
        );
        let refused = rules
            .route(Label::Confidential, Some("claude-api"))
            .unwrap_err();
        assert!(
            refused
                .reason
                .starts_with("claude-api isn't approved for confidential data"),
            "{refused:?}"
        );
        assert!(rules.route(Label::Internal, Some("gpt-x")).is_err());
    }

    #[test]
    fn no_models_section_means_no_model_calls() {
        let rules = Policy::from_toml(crate::DEFAULT_POLICY)
            .unwrap()
            .effective("dana", &"invoice-demo".parse().unwrap())
            .unwrap()
            .models()
            .clone();
        assert!(rules.route(Label::Public, None).is_err());
        assert_eq!(rules.unlabeled(), Label::Internal);
        assert!(!rules.log_prompts());
    }

    #[test]
    fn validation_checks_every_model_reference() {
        let bad = POLICY
            .replace("region = \"us-west-2\"", "")
            .replace("default = \"claude-api\"", "default = \"ghost\"")
            .replace(
                "confidential = [\"claude-bedrock\"]",
                "restricted = [\"claude-bedrock\"]",
            )
            .replace("models = [\"claude-api\"]", "models = [\"phantom\"]")
            .replace(
                "name = \"Anthropic API\"",
                "name = \"x\"\nendpoint = \"http://x\"",
            );
        let Err(PolicyError::Invalid(errors)) = Policy::from_toml(&bad) else {
            panic!("expected validation errors");
        };
        for expected in [
            "need a region",
            "models.default names model \"ghost\"",
            "restricted data never goes to a model",
            "agent \"narrow\" names model \"phantom\"",
            "endpoint must be an https:// URL",
        ] {
            assert!(
                errors.iter().any(|e| e.contains(expected)),
                "{expected} in {errors:?}"
            );
        }
    }
}
