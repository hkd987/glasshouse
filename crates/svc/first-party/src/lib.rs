//! The first-party app set (NV-3): every app glassd ships, registered in one
//! place. Adding an app is a new crate under `crates/apps/` and one line
//! here; glassd and `glassctl policy check` both read this list. The model
//! gateway's `model.complete` is declared here too, so policy, handoff cards
//! and `tools/list` treat it like any action (ADR 0018).

use std::path::PathBuf;
use std::sync::Arc;

use glass_actions::{Registry, RegistryError};
use glass_app_calendar::CalendarProvider;
use glass_app_catalog::{CatalogHandle, CatalogProvider};
use glass_app_files::FilesProvider;
use glass_app_helpdesk::HelpdeskProvider;
use glass_app_mail::MailProvider;
use glass_app_people::PeopleProvider;
use glass_app_system::{HostSystem, SystemOps, SystemProvider};
use glass_gateway::ModelAction;
use glass_managed::DirectoryHandle;

/// What the apps need from the machine's configuration and policy.
#[derive(Debug, Clone)]
pub struct Setup {
    /// Services the IT agent may restart (policy `[fixes] services`).
    pub restartable_services: Vec<String>,
    /// Where help desk tickets wait until an adapter delivers them.
    pub helpdesk_outbox: PathBuf,
    /// The machine the system app inspects; tests pass a fake.
    pub system: Arc<dyn SystemOps>,
    /// The org's catalog as this device last applied it, which glassd's
    /// managed sync swaps (ADR 0023); `catalog.search` reads it (GA-2).
    pub catalog: CatalogHandle,
    /// The org's people as this device last applied them (ADR 0023):
    /// `people.search` reads them, and Mail and Calendar send as the person
    /// they name (AD-1).
    pub directory: DirectoryHandle,
    /// This device is managed by an org, so a catalog is coming if none
    /// has arrived (the Store says which).
    pub managed: bool,
}

impl Setup {
    /// A setup that touches nothing, for listing actions (policy checks).
    pub fn offline() -> Self {
        Self {
            restartable_services: Vec::new(),
            helpdesk_outbox: PathBuf::from("/nonexistent"),
            system: Arc::new(HostSystem),
            catalog: CatalogHandle::new(),
            directory: DirectoryHandle::new(),
            managed: false,
        }
    }
}

/// Every first-party app's actions in one registry.
pub fn registry(setup: Setup) -> Result<Registry, RegistryError> {
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider))?;
    registry.register(Arc::new(MailProvider::new(setup.directory.clone())))?;
    registry.register(Arc::new(CalendarProvider::new(setup.directory.clone())))?;
    registry.register(Arc::new(SystemProvider::new(
        setup.system,
        setup.restartable_services,
    )))?;
    registry.register(Arc::new(HelpdeskProvider::new(setup.helpdesk_outbox)))?;
    registry.register(Arc::new(
        CatalogProvider::new(setup.catalog).managed(setup.managed),
    ))?;
    registry.register(Arc::new(
        PeopleProvider::new(setup.directory).managed(setup.managed),
    ))?;
    registry.register(Arc::new(ModelAction))?;
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_registers_without_collisions() {
        let registry = registry(Setup::offline()).unwrap();
        let names: Vec<String> = registry
            .lint_info()
            .iter()
            .map(|(name, _, _)| name.to_string())
            .collect();
        for expected in [
            "files.read",
            "mail.send",
            "calendar.hold",
            "calendar.invite",
            "system.network.check",
            "helpdesk.ticket.create",
            "model.complete",
            "catalog.search",
            "people.search",
        ] {
            assert!(names.iter().any(|n| n == expected), "{expected}");
        }
    }
}
