//! Tailoring (PRD "Tailoring catalog items"): the control API's `layer.*`
//! methods, the checks a layer passes before it turns on, and applying a
//! user's layer to their new tasks. Storage and value checks live in
//! `glass-tailoring`; what touches policy is here.

use glass_api::{
    CheckResult, LayerChange, LayerCheck, LayerLine, LayerRevert, LayerVersion, LayerView,
};
use glass_audit::{AuditEvent, EventKind};
use glass_policy::ActionPattern;
use glass_tailoring::{Layer, Recheck, StoreError, Version};
use glass_types::{AgentId, Timestamp};

use crate::error::Result;
use crate::{Caller, CatalogEntry, Runtime, RuntimeError};

/// The layer a new task runs with.
pub(crate) struct TaskLayer {
    /// What applies; `None` while the layer is paused.
    pub version: Option<Version>,
    /// The handoff card's layer line.
    pub line: LayerLine,
}

const ALLOWED: &str = "Only what the publisher lets you change";
const NARROWS: &str = "Permissions only narrow";
const RELEASE_BAR: &str = "Release bar";

impl Runtime {
    /// The caller's layer on an agent and what they can change (TL-1, TL-2).
    pub fn layer_get(&self, caller: &Caller, agent: &AgentId) -> Result<LayerView> {
        let entry = self.tailorable(caller, agent)?;
        let layer = self.current_layer(caller, entry)?;
        Ok(self.layer_view(caller, entry, &layer))
    }

    /// Saves a new version of the caller's layer and turns it on, if every
    /// check passes; otherwise saves nothing and says why.
    pub fn layer_set(&self, caller: &Caller, change: LayerChange) -> Result<LayerView> {
        let entry = self.tailorable(caller, &change.agent)?;
        self.refuse_unrecorded()?;
        let candidate = Version {
            version: 0,
            at: Timestamp::now(),
            note: change.note,
            settings: change.settings,
            restrict: change.restrict,
        };
        refuse_failed(&self.layer_checks(caller, entry, &candidate))?;
        let (layer, version) = self
            .layers
            .update(caller.uid, &entry.id, |layer| {
                layer.add(
                    &caller.name,
                    candidate.note,
                    candidate.settings,
                    candidate.restrict,
                    &entry.version,
                )
            })
            .map_err(store_error)?;
        self.audit_layer(
            caller,
            entry,
            format!("Saved your layer v{version} on {}", entry.name),
            &layer,
        );
        Ok(self.layer_view(caller, entry, &layer))
    }

    /// Switches to an earlier version (undoing later changes), or turns the
    /// layer off. Earlier versions must still fit the agent.
    pub fn layer_revert(&self, caller: &Caller, revert: LayerRevert) -> Result<LayerView> {
        let entry = self.tailorable(caller, &revert.agent)?;
        self.refuse_unrecorded()?;
        if let Some(to) = revert.to {
            let layer = self
                .layers
                .load(caller.uid, &entry.id)
                .map_err(store_error)?;
            let version = layer.version(to).ok_or_else(|| {
                RuntimeError::NotFound(format!(
                    "there is no version {to} of your layer on {}",
                    entry.name
                ))
            })?;
            let problems = fit_problems(entry, version);
            if !problems.is_empty() {
                return Err(RuntimeError::Invalid(format!(
                    "version {to} no longer fits {} {}: {}",
                    entry.name,
                    entry.version,
                    problems.join("; ")
                )));
            }
        }
        let (layer, switched) = self
            .layers
            .update(caller.uid, &entry.id, |layer| {
                layer.switch(revert.to, &entry.version)
            })
            .map_err(store_error)?;
        switched.map_err(RuntimeError::NotFound)?;
        let summary = match revert.to {
            Some(to) => format!("Switched your layer on {} to v{to}", entry.name),
            None => format!("Turned off your layer on {}", entry.name),
        };
        self.audit_layer(caller, entry, summary, &layer);
        Ok(self.layer_view(caller, entry, &layer))
    }

    /// TL-13 at startup: re-checks every layer whose agent has a new
    /// version, pausing the ones that no longer fit, so owners see it (and
    /// the audit log has it) before their next task.
    pub fn recheck_layers(&self) {
        let stored = match self.layers.all() {
            Ok(stored) => stored,
            Err(e) => {
                tracing::warn!(error = %e, "can't list tailoring layers");
                return;
            }
        };
        for (uid, agent) in stored {
            let (Some(caller), Some(entry)) = (self.caller(uid), self.catalog_entry(&agent)) else {
                continue;
            };
            if let Err(e) = self.current_layer(&caller, entry) {
                tracing::warn!(user = uid, agent = %agent, error = %e, "can't re-check a layer");
            }
        }
    }

    /// The layer a new task runs with, after a re-check if the agent changed.
    pub(crate) fn layer_for_task(
        &self,
        caller: &Caller,
        entry: &CatalogEntry,
    ) -> Result<Option<TaskLayer>> {
        let layer = self.current_layer(caller, entry)?;
        Ok(layer.active_version().map(|active| TaskLayer {
            version: layer.in_use().cloned(),
            line: LayerLine {
                version: active.version,
                summary: self.summary(entry, active),
                paused: layer.paused.clone(),
            },
        }))
    }

    /// Like actions, layer changes stop when they can't be recorded.
    fn refuse_unrecorded(&self) -> Result<()> {
        if self.audit_healthy() {
            Ok(())
        } else {
            Err(RuntimeError::Conflict(
                "the audit log can't be written, so changes are refused until it can".into(),
            ))
        }
    }

    fn catalog_entry(&self, agent: &AgentId) -> Option<&CatalogEntry> {
        self.config.catalog.iter().find(|e| &e.id == agent)
    }

    /// Users tailor agents they're allowed to run.
    fn tailorable(&self, caller: &Caller, agent: &AgentId) -> Result<&CatalogEntry> {
        let entry = self
            .catalog_entry(agent)
            .ok_or_else(|| RuntimeError::NotFound(format!("no agent {agent} in the catalog")))?;
        if self.policy.effective(&caller.name, agent).is_none() {
            return Err(RuntimeError::PermissionDenied(format!(
                "your groups are not allowed to run {agent}"
            )));
        }
        Ok(entry)
    }

    /// The stored layer, re-checked first if the agent's version changed
    /// since it was last checked (TL-13).
    fn current_layer(&self, caller: &Caller, entry: &CatalogEntry) -> Result<Layer> {
        let (layer, outcome) = self
            .layers
            .update(caller.uid, &entry.id, |layer| {
                layer.recheck(&entry.version, |version| {
                    let problems = fit_problems(entry, version);
                    if problems.is_empty() {
                        Ok(())
                    } else {
                        Err(problems)
                    }
                })
            })
            .map_err(store_error)?;
        if let Some(outcome) = outcome {
            let summary = match outcome {
                Recheck::Passed => format!(
                    "Re-checked your layer on {} against version {}: it still fits",
                    entry.name, entry.version
                ),
                Recheck::Paused(reason) => {
                    format!("Paused your layer on {}: it {reason}", entry.name)
                }
            };
            self.audit_layer(caller, entry, summary, &layer);
        }
        Ok(layer)
    }

    /// What a version must pass before it turns on.
    fn layer_checks(
        &self,
        caller: &Caller,
        entry: &CatalogEntry,
        version: &Version,
    ) -> Vec<LayerCheck> {
        let allowed = match glass_tailoring::check_settings(&entry.customize, &version.settings) {
            Ok(()) => passed(ALLOWED, "Every change is one the publisher allows"),
            Err(problems) => failed(ALLOWED, problems.join("; ")),
        };
        vec![
            allowed,
            self.narrowing_check(caller, entry, version),
            // TL-11 needs evaluation suites (AE-10), which don't exist yet.
            LayerCheck {
                name: RELEASE_BAR.into(),
                result: CheckResult::NotAvailable,
                detail: "This agent has no evaluation suite yet".into(),
            },
        ]
    }

    /// TL-10: limits may only make tiers stricter than the caller's policy.
    /// A layer has no way to add data or actions, so this is the whole
    /// permission check.
    fn narrowing_check(
        &self,
        caller: &Caller,
        entry: &CatalogEntry,
        version: &Version,
    ) -> LayerCheck {
        let Some(mut policy) = self.policy.effective(&caller.name, &entry.id) else {
            return failed(NARROWS, "your groups are not allowed to run this agent");
        };
        let known = self.registry.known_tiers();
        let problems: Vec<String> = version
            .restrict
            .iter()
            .filter_map(|(pattern, tier)| {
                let pattern: ActionPattern = match pattern.parse() {
                    Ok(p) => p,
                    Err(e) => return Some(format!("{e}")),
                };
                policy
                    .restrict(pattern, *tier, &known)
                    .err()
                    .map(|e| e.to_string())
            })
            .collect();
        if !problems.is_empty() {
            failed(NARROWS, problems.join("; "))
        } else if version.restrict.is_empty() {
            passed(NARROWS, "Nothing new: the same data and actions")
        } else {
            passed(NARROWS, "Only stricter limits, nothing new")
        }
    }

    fn summary(&self, entry: &CatalogEntry, version: &Version) -> Vec<String> {
        let mut lines = glass_tailoring::describe(&entry.customize, &version.settings);
        lines.extend(version.restrict.iter().map(|(pattern, tier)| {
            let title = pattern
                .parse()
                .ok()
                .and_then(|name| self.registry.get(&name))
                .map_or_else(|| pattern.clone(), |(def, _)| def.title.clone());
            format!("{title}: {tier}")
        }));
        lines
    }

    fn layer_view(&self, caller: &Caller, entry: &CatalogEntry, layer: &Layer) -> LayerView {
        LayerView {
            agent: entry.summary(),
            points: entry.customize.clone(),
            status: layer.status(),
            active: layer.active,
            history: layer
                .versions
                .iter()
                .rev()
                .map(|v| LayerVersion {
                    version: v.version,
                    at: v.at,
                    note: v.note.clone(),
                    settings: v.settings.clone(),
                    restrict: v.restrict.clone(),
                    summary: self.summary(entry, v),
                })
                .collect(),
            checks: layer
                .active_version()
                .map(|v| self.layer_checks(caller, entry, v))
                .unwrap_or_default(),
        }
    }

    fn audit_layer(&self, caller: &Caller, entry: &CatalogEntry, summary: String, layer: &Layer) {
        let mut event = AuditEvent::new(EventKind::LayerChanged, summary);
        event.user = Some(caller.name.clone());
        event.agent = Some(entry.id.clone());
        let active = layer.active_version();
        event.detail = serde_json::json!({
            "agent_version": entry.version,
            "active": layer.active,
            "status": layer.status(),
            "settings": active.map(|v| &v.settings),
            "restrict": active.map(|v| &v.restrict),
        });
        self.audit(event);
    }
}

/// The activity line a task starts with when the user has a layer.
pub(crate) fn note(line: &LayerLine) -> String {
    match &line.paused {
        Some(reason) => format!(
            "Your layer v{} is paused ({reason}); running the published version",
            line.version
        ),
        None if line.summary.is_empty() => format!("Using your layer v{}", line.version),
        None => format!(
            "Using your layer v{}: {}",
            line.version,
            line.summary.join("; ")
        ),
    }
}

/// Whether a saved version still fits the agent as published now: its
/// values match the declared points and its limits name valid patterns.
/// Policy is checked when limits apply, where they can only narrow.
fn fit_problems(entry: &CatalogEntry, version: &Version) -> Vec<String> {
    let mut problems = glass_tailoring::check_settings(&entry.customize, &version.settings)
        .err()
        .unwrap_or_default();
    problems.extend(
        version
            .restrict
            .iter()
            .filter_map(|(pattern, _)| pattern.parse::<ActionPattern>().err())
            .map(|e| e.to_string()),
    );
    problems
}

fn passed(name: &str, detail: &str) -> LayerCheck {
    LayerCheck {
        name: name.into(),
        result: CheckResult::Passed,
        detail: detail.into(),
    }
}

fn failed(name: &str, detail: impl Into<String>) -> LayerCheck {
    LayerCheck {
        name: name.into(),
        result: CheckResult::Failed,
        detail: detail.into(),
    }
}

fn refuse_failed(checks: &[LayerCheck]) -> Result<()> {
    let failures: Vec<String> = checks
        .iter()
        .filter(|c| c.result == CheckResult::Failed)
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError::Invalid(format!(
            "the layer wasn't saved. {}",
            failures.join(". ")
        )))
    }
}

fn store_error(e: StoreError) -> RuntimeError {
    tracing::error!(error = %e, "tailoring layer store");
    RuntimeError::Internal("couldn't read or save the layer".into())
}
