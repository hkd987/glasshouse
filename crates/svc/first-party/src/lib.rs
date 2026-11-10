//! The first-party app set (NV-3): every app glassd ships, registered in one
//! place. Adding an app is a new crate under `crates/apps/` and one line
//! here; glassd and `glassctl policy check` both read this list. The model
//! gateway's `model.complete` is declared here too, so policy, handoff cards
//! and `tools/list` treat it like any action (ADR 0018).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use glass_actions::{Registry, RegistryError};
use glass_app_artifacts::ArtifactsProvider;
use glass_app_calendar::CalendarProvider;
use glass_app_catalog::{CatalogHandle, CatalogProvider};
use glass_app_chat::ChatProvider;
pub use glass_app_chat::{ChatHandle, Notice as ChatNotice};
use glass_app_docs::DocsProvider;
use glass_app_files::FilesProvider;
use glass_app_hardware::HardwareProvider;
pub use glass_app_hardware::{BuildService, HardwareHandle, Patches};
use glass_app_helpdesk::HelpdeskProvider;
use glass_app_mail::MailProvider;
pub use glass_app_mail::{
    MailHandle, Notice as MailNotice, Remote as MailRemote, server as mail_server,
};
pub use glass_app_meet::MeetHandle;
use glass_app_meet::MeetProvider;
use glass_app_people::PeopleProvider;
pub use glass_app_routines::Routines;
use glass_app_routines::RoutinesProvider;
use glass_app_settings::SettingsProvider;
pub use glass_app_settings::computer::Computer;
pub use glass_app_settings::wifi as settings_wifi;
pub use glass_app_settings::{ModelAgent, ModelKeys, SettingsHandle};
use glass_app_skills::SkillsProvider;
pub use glass_app_studio::Studio;
use glass_app_studio::StudioProvider;
use glass_app_system::{HostSystem, SystemOps, SystemProvider};
use glass_gateway::ModelAction;
use glass_managed::DirectoryHandle;
use glass_managed::meet::{Meetings, TranscriptRule};
use glass_managed::shared::Shared;

/// The providers that keep something in memory between calls, made once
/// and used by every registry built with the same `Kept`.
///
/// glassd builds a new registry whenever policy changes (ADR 0023), and a
/// provider made anew starts empty. For these four that lost something:
/// Docs forgot which windows had a doc open, so the next push was refused,
/// the page opened again, and the close record (the only audit record of
/// live edits, ADR 0035) counted only what came after; Meet forgot which
/// batches of transcript lines it had taken, so one sent twice was added
/// twice; and Artifacts' and Skills' one-at-a-time locks were two locks
/// while calls on the old registry finished. None of the four is made from
/// anything policy says, so the same one serves every registry.
#[derive(Clone, Default)]
pub struct Kept(Arc<KeptProviders>);

#[derive(Default)]
struct KeptProviders {
    artifacts: OnceLock<Arc<ArtifactsProvider>>,
    skills: OnceLock<Arc<SkillsProvider>>,
    docs: OnceLock<Arc<DocsProvider>>,
    meet: OnceLock<Arc<MeetProvider>>,
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kept")
            .field("made", &self.0.docs.get().is_some())
            .finish()
    }
}

/// What the apps need from the machine's configuration and policy.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The providers with something in memory, the same ones in every
    /// registry glassd builds ([`Kept`]).
    pub kept: Kept,
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
    /// Mail on this computer (ADR 0037): the link to that service, each
    /// person's sign-in, root only, and where their cache's sync left off.
    pub mail_handle: MailHandle,
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
    /// Each person's routines (DA-4), root's: the runtime runs them.
    pub routines: Routines,
    /// What agents read and do to fix the hardware with a kernel patch
    /// (ADR 0057): the build service and the runtime's patches, once both
    /// exist.
    pub hardware: HardwareHandle,
}

impl Setup {
    /// A setup that touches nothing, for listing actions (policy checks).
    pub fn offline() -> Self {
        Self {
            kept: Kept::default(),
            restartable_services: Vec::new(),
            helpdesk_outbox: PathBuf::from("/nonexistent"),
            system: Arc::new(HostSystem),
            catalog: CatalogHandle::new(),
            directory: DirectoryHandle::new(),
            mail: None,
            mail_handle: MailHandle::new("/nonexistent"),
            settings: SettingsHandle::new("/nonexistent"),
            chat: ChatHandle::new("/nonexistent"),
            meet: MeetHandle::new(),
            managed: false,
            studio: Studio::offline(),
            skills: PathBuf::from("/nonexistent"),
            routines: Routines::offline(),
            hardware: HardwareHandle::new("/nonexistent"),
        }
    }
}

/// Every first-party app's actions in one registry.
pub fn registry(setup: Setup) -> Result<Registry, RegistryError> {
    let mut registry = Registry::new();
    registry.register(Arc::new(FilesProvider))?;
    registry.register(Arc::new(
        MailProvider::new(setup.directory.clone())
            .settings(setup.mail)
            .mail(setup.mail_handle),
    ))?;
    // Meetings are conversations in chat's store (CF-21): Calendar and Meet
    // reach them through it, never through Chat (ADR 0013).
    let chat = ChatProvider::new(setup.chat, setup.directory.clone());
    let meetings: Arc<dyn Meetings> = Arc::new(chat.clone());
    // So are artifacts (ADR 0033), in the shared space chat's store keeps.
    let shared: Arc<dyn Shared> = Arc::new(chat.clone());
    // The four below are made once and kept ([`Kept`]): every registry
    // registers the same one.
    let kept = setup.kept.0.clone();
    let directory = setup.directory.clone();
    let artifacts = kept
        .artifacts
        .get_or_init(|| Arc::new(ArtifactsProvider::new(shared.clone(), directory.clone())));
    registry.register(artifacts.clone())?;
    let skills = kept.skills.get_or_init(|| {
        Arc::new(SkillsProvider::new(
            shared.clone(),
            directory.clone(),
            setup.skills,
        ))
    });
    registry.register(skills.clone())?;
    // And docs (ADR 0035), which pages also edit live.
    let docs = kept
        .docs
        .get_or_init(|| Arc::new(DocsProvider::new(shared.clone(), directory.clone())))
        .clone();
    registry.register(docs.clone())?;
    registry.register_live(docs);
    // A meeting is transcribed where IT turned transcripts on for its
    // organizer, unless they turned theirs off (ADR 0045).
    let settings = setup.settings.clone();
    let transcripts = TranscriptRule::new(move |login, uid| {
        settings.on(login, uid, glass_managed::meet::TRANSCRIPTS_SETTING)
    });
    registry.register(Arc::new(
        CalendarProvider::new(setup.directory.clone())
            .meetings(meetings.clone())
            .transcripts(transcripts),
    ))?;
    registry.register(Arc::new(SystemProvider::new(
        setup.system,
        setup.restartable_services,
    )))?;
    registry.register(Arc::new(HelpdeskProvider::new(setup.helpdesk_outbox)))?;
    registry.register(Arc::new(HardwareProvider::new(setup.hardware)))?;
    registry.register(Arc::new(
        CatalogProvider::new(setup.catalog).managed(setup.managed),
    ))?;
    registry.register(Arc::new(
        PeopleProvider::new(setup.directory.clone()).managed(setup.managed),
    ))?;
    registry.register(Arc::new(SettingsProvider::new(setup.settings)))?;
    // And transcripts, in the meetings' own conversations (ADR 0045).
    let meet = kept.meet.get_or_init(|| {
        Arc::new(MeetProvider::new(meetings, setup.meet, setup.directory).shared(shared))
    });
    registry.register(meet.clone())?;
    registry.register(Arc::new(chat))?;
    registry.register(Arc::new(StudioProvider::new(setup.studio)))?;
    registry.register(Arc::new(RoutinesProvider::new(setup.routines)))?;
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
            "mail.move",
            "mail.connect",
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

    /// The provider of `action` in `registry`: which one it is.
    fn provider_of(registry: &Registry, action: &str) -> *const () {
        let (_, provider) = registry.get(&action.parse().unwrap()).unwrap();
        Arc::as_ptr(provider).cast()
    }

    #[test]
    fn adr0023_a_registry_built_again_has_the_same_providers_where_they_hold_something() {
        // What glassd does at every policy release: the same setup, a new
        // registry. Docs' open sessions, Meet's batches, and Artifacts' and
        // Skills' locks are in memory, in their providers: made anew, each
        // release dropped them (and with Docs' sessions, the audit record
        // of the edits made before it).
        let setup = Setup::offline();
        let first = registry(setup.clone()).unwrap();
        let second = registry(setup.clone()).unwrap();
        for action in ["doc.open", "meet.join", "artifact.share", "skill.propose"] {
            assert_eq!(
                provider_of(&first, action),
                provider_of(&second, action),
                "{action}'s provider was made again"
            );
        }
        // Pages edit a doc through the same one, too.
        let live = |r: &Registry| r.live("docs").map(|l| Arc::as_ptr(&l).cast::<()>());
        assert!(live(&first).is_some());
        assert_eq!(live(&first), live(&second));
        // What policy shapes is made from the new policy each time.
        assert_ne!(
            provider_of(&first, "system.network.check"),
            provider_of(&second, "system.network.check")
        );
        // And another setup has providers of its own.
        let other = registry(Setup::offline()).unwrap();
        assert_ne!(
            provider_of(&first, "doc.open"),
            provider_of(&other, "doc.open")
        );
    }
}
