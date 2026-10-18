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
}

/// Every action the OS knows, and which provider runs it.
#[derive(Debug, Default)]
pub struct Registry {
    actions: BTreeMap<ActionName, (ActionDef, Arc<dyn ActionProvider>)>,
    by_mcp: BTreeMap<String, ActionName>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Arc<dyn ActionProvider>) -> Result<(), RegistryError> {
        for def in provider.actions() {
            if self.actions.contains_key(&def.name) {
                return Err(RegistryError::Duplicate(def.name));
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
