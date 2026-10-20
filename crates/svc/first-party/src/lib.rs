//! The first-party app set (NV-3): every app glassd ships, registered in one
//! place. Adding an app is a new crate under `crates/apps/` and one line
//! here; glassd and `glassctl policy check` both read this list. The model
//! gateway's `model.complete` is declared here too, so policy, handoff cards
//! and `tools/list` treat it like any action (ADR 0018).

use std::path::PathBuf;
use std::sync::Arc;

use glass_actions::{Registry, RegistryError};
use glass_app_artifacts::ArtifactsProvider;
use glass_app_calendar::CalendarProvider;
use glass_app_catalog::{CatalogHandle, CatalogProvider};
use glass_app_chat::ChatProvider;
pub use glass_app_chat::{ChatHandle, Notice as ChatNotice};
use glass_app_files::FilesProvider;
use glass_app_helpdesk::HelpdeskProvider;
use glass_app_mail::MailProvider;
pub use glass_app_meet::MeetHandle;
use glass_app_meet::MeetProvider;
use glass_app_people::PeopleProvider;
pub use glass_app_settings::SettingsHandle;
use glass_app_settings::SettingsProvider;
use glass_app_skills::SkillsProvider;
pub use glass_app_studio::Studio;
use glass_app_studio::StudioProvider;
use glass_app_system::{HostSystem, SystemOps, SystemProvider};
use glass_gateway::ModelAction;
use glass_managed::DirectoryHandle;
use glass_managed::meet::Meetings;
use glass_managed::shared::Shared;

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
    /// The org's mail service, from policy (NV-3).
    pub mail: Option<glass_policy::document::MailSettings>,
    /// Device settings: policy's and each person's choices (UX-1).
    pub settings: SettingsHandle,
    /// The org's chat on this computer: member keys and the sealed store
    /// (CF-91, ADR 0030). Unconnected, every chat action says there's none.
    pub chat: ChatHandle,
    /// How meetings reach the console (CF-21). Meetings themselves are
    /// conversations in `chat`'s store.
    pub meet: MeetHandle,
    /// This device is managed by an org, so a catalog is coming if none
    /// has arrived (the Store says which).
    pub managed: bool,
    /// The apps people make (ADR 0032): their source, checks and versions.
    pub studio: Studio,
    /// Each person's skill drafts and eval results (ADR 0034), root's.
    pub skills: PathBuf,
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
            mail: None,
            settings: SettingsHandle::new("/nonexistent"),
            chat: ChatHandle::new("/nonexistent"),
            meet: MeetHandle::new(),
            managed: false,
            studio: Studio::offline(),
            skills: PathBuf::from("/nonexistent"),
        }
    }
}

/// Every first-party app's actions in one registry.
pub fn registry(setup: Setup) -> Result<Registry, RegistryError> {
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider))?;
    registry.register(Arc::new(
        MailProvider::new(setup.directory.clone()).settings(setup.mail),
    ))?;
    // Meetings are conversations in chat's store (CF-21): Calendar and Meet
    // reach them through it, never through Chat (ADR 0013).
    let chat = ChatProvider::new(setup.chat, setup.directory.clone());
    let meetings: Arc<dyn Meetings> = Arc::new(chat.clone());
    // So are artifacts (ADR 0033), in the shared space chat's store keeps.
    let shared: Arc<dyn Shared> = Arc::new(chat.clone());
    registry.register(Arc::new(ArtifactsProvider::new(
        shared.clone(),
        setup.directory.clone(),
    )))?;
    registry.register(Arc::new(SkillsProvider::new(
        shared,
        setup.directory.clone(),
        setup.skills,
    )))?;
    registry.register(Arc::new(
        CalendarProvider::new(setup.directory.clone()).meetings(meetings.clone()),
    ))?;
    registry.register(Arc::new(SystemProvider::new(
        setup.system,
        setup.restartable_services,
    )))?;
    registry.register(Arc::new(HelpdeskProvider::new(setup.helpdesk_outbox)))?;
    registry.register(Arc::new(
        CatalogProvider::new(setup.catalog).managed(setup.managed),
    ))?;
    registry.register(Arc::new(
        PeopleProvider::new(setup.directory.clone()).managed(setup.managed),
    ))?;
    registry.register(Arc::new(SettingsProvider::new(setup.settings)))?;
    registry.register(Arc::new(MeetProvider::new(
        meetings,
        setup.meet,
        setup.directory,
    )))?;
    registry.register(Arc::new(chat))?;
    registry.register(Arc::new(StudioProvider::new(setup.studio)))?;
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
            "chat.post",
            "meet.list",
            "meet.join",
            "meet.keys",
            "meet.leave",
            "artifact.share",
            "artifact.comment",
            "skill.propose",
            "skill.results",
            "apps.source",
            "apps.save",
            "apps.create",
        ] {
            assert!(names.iter().any(|n| n == expected), "{expected}");
        }
    }
}
