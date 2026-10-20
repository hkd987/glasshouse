use std::collections::BTreeMap;
use std::sync::Arc;

use glass_types::{ActionName, SideEffect, Tier};

use crate::{ActionDef, ActionProvider};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("action {0} is declared twice")]
    Duplicate(ActionName),
    #[error("actions {0} and {1} map to the same MCP tool name")]
    McpCollision(ActionName, ActionName),
    #[error("action {0}'s answer labels must each be one line of 1 to {max} characters", max = glass_api::AskLabels::MAX_CHARS)]
    AskLabels(ActionName),
}

/// Every action the OS knows, and which provider runs it.
#[derive(Debug, Default)]
pub struct Registry {
    actions: BTreeMap<ActionName, (ActionDef, Arc<dyn ActionProvider>)>,
    by_mcp: BTreeMap<String, ActionName>,
    /// Live editing's providers, by the app whose sessions use them (ADR
    /// 0035 §3).
    live: BTreeMap<&'static str, Arc<dyn crate::Live>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a provider of live edits for its app's sessions.
    pub fn register_live(&mut self, live: Arc<dyn crate::Live>) {
        self.live.insert(live.app(), live);
    }

    /// The live provider for app `app`'s sessions.
    pub fn live(&self, app: &str) -> Option<Arc<dyn crate::Live>> {
        self.live.get(app).cloned()
    }

    /// Every live provider, to tell each when a session ends.
    pub fn lives(&self) -> Vec<Arc<dyn crate::Live>> {
        self.live.values().cloned().collect()
    }

    pub fn register(&mut self, provider: Arc<dyn ActionProvider>) -> Result<(), RegistryError> {
        for def in provider.actions() {
            if self.actions.contains_key(&def.name) {
                return Err(RegistryError::Duplicate(def.name));
            }
            if !def.ask.fit() {
                return Err(RegistryError::AskLabels(def.name));
            }
            let mcp = def.mcp_name();
            if let Some(existing) = self.by_mcp.get(&mcp) {
                return Err(RegistryError::McpCollision(existing.clone(), def.name));
            }
            self.by_mcp.insert(mcp, def.name.clone());
            self.actions
                .insert(def.name.clone(), (def, Arc::clone(&provider)));
        }
        Ok(())
    }

    pub fn get(&self, name: &ActionName) -> Option<(&ActionDef, &Arc<dyn ActionProvider>)> {
        self.actions.get(name).map(|(d, p)| (d, p))
    }

    pub fn by_mcp_name(&self, tool: &str) -> Option<(&ActionDef, &Arc<dyn ActionProvider>)> {
        self.by_mcp.get(tool).and_then(|name| self.get(name))
    }

    pub fn defs(&self) -> impl Iterator<Item = &ActionDef> {
        self.actions.values().map(|(d, _)| d)
    }

    pub fn providers(&self) -> Vec<Arc<dyn ActionProvider>> {
        let mut seen: Vec<Arc<dyn ActionProvider>> = Vec::new();
        for (_, provider) in self.actions.values() {
            if !seen.iter().any(|p| Arc::ptr_eq(p, provider)) {
                seen.push(Arc::clone(provider));
            }
        }
        seen
    }

    /// `(name, default tier)` for every action, as policy evaluation needs.
    pub fn known_tiers(&self) -> Vec<(ActionName, Tier)> {
        self.defs()
            .map(|d| (d.name.clone(), d.default_tier))
            .collect()
    }

    /// `(name, side effect, default tier)` for policy linting.
    pub fn lint_info(&self) -> Vec<(ActionName, SideEffect, Tier)> {
        self.defs()
            .map(|d| (d.name.clone(), d.side_effect, d.default_tier))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{ActionError, ActionOutput, Preview, TaskContext};

    #[derive(Debug)]
    struct Labeled(&'static str);

    impl ActionProvider for Labeled {
        fn actions(&self) -> Vec<ActionDef> {
            vec![
                ActionDef {
                    name: "notes.publish".parse().unwrap(),
                    title: "Publish a note".into(),
                    description: String::new(),
                    input_schema: json!({ "type": "object" }),
                    side_effect: SideEffect::Irreversible,
                    default_tier: Tier::Ask,
                    ask: Default::default(),
                }
                .asking("Review note", self.0),
            ]
        }

        fn preview(
            &self,
            _: &TaskContext,
            _: &ActionName,
            _: &Value,
        ) -> Result<Preview, ActionError> {
            Ok(Preview::default())
        }

        fn invoke(
            &self,
            _: &TaskContext,
            _: &ActionName,
            _: &Value,
        ) -> Result<ActionOutput, ActionError> {
            Err(ActionError::Failed("not in tests".into()))
        }
    }

    #[test]
    fn answer_labels_must_fit_a_tile_button() {
        let mut registry = Registry::new();
        registry
            .register(Arc::new(Labeled("Approve and publish")))
            .unwrap();
        let (def, _) = registry.get(&"notes.publish".parse().unwrap()).unwrap();
        assert_eq!(def.ask.approve, "Approve and publish");

        let mut registry = Registry::new();
        assert!(matches!(
            registry.register(Arc::new(Labeled("Approve and publish to everyone"))),
            Err(RegistryError::AskLabels(_))
        ));
    }
}
