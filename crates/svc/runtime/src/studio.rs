//! The studio (ADR 0032) and the runtime: the apps people make come from
//! the studio's store ([`PersonalApps`] for [`Studio`]), and what the studio
//! asks of the system is answered from this runtime's policy and registry
//! ([`StudioSurroundings`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Weak};

use glass_api::AppManifest;
use glass_app_studio::{Studio, Surroundings};
use glass_types::{AgentId, Tier};

use crate::{PersonalApp, PersonalApps, Runtime};

impl PersonalApps for Studio {
    fn apps(&self, login: &str, home: &Path) -> Vec<PersonalApp> {
        self.own_apps(login, home)
            .into_iter()
            .map(|own| PersonalApp {
                manifest: own.manifest,
                based_on: own.record.based_on.map(|b| b.id),
            })
            .collect()
    }

    fn files(
        &self,
        login: &str,
        home: &Path,
        app: &AgentId,
    ) -> Option<(u32, BTreeMap<String, String>)> {
        self.served(login, home, app)
    }
}

/// What the org's catalog offers someone in some groups for a need.
pub type Offers = dyn Fn(&[String], &str) -> Vec<(AgentId, String)> + Send + Sync;

/// The studio's view of the system: profiles and rules from the runtime's
/// policy in force, actions from its registry, offers from the catalog.
pub struct StudioSurroundings {
    rt: Weak<Runtime>,
    offers: Box<Offers>,
}

impl StudioSurroundings {
    pub fn new(
        rt: &Arc<Runtime>,
        offers: impl Fn(&[String], &str) -> Vec<(AgentId, String)> + Send + Sync + 'static,
    ) -> Self {
        Self {
            rt: Arc::downgrade(rt),
            offers: Box::new(offers),
        }
    }
}

impl fmt::Debug for StudioSurroundings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StudioSurroundings").finish_non_exhaustive()
    }
}

impl Surroundings for StudioSurroundings {
    fn remixing(&self) -> bool {
        self.rt
            .upgrade()
            .is_some_and(|rt| rt.device_settings.remixing())
    }

    fn profile(&self, login: &str) -> BTreeSet<AgentId> {
        self.rt.upgrade().map_or_else(BTreeSet::new, |rt| {
            rt.policy().apps_for(login).into_iter().cloned().collect()
        })
    }

    /// GA-5: an app someone makes may do only what they may, through
    /// actions this computer has, without the rules policy gives other apps
    /// (`Policy::for_app` gives a personal id none).
    fn refusals(&self, login: &str, manifest: &AppManifest) -> Vec<String> {
        let Some(rt) = self.rt.upgrade() else {
            return vec!["the system is stopping".into()];
        };
        let (policy, registry) = (rt.policy(), rt.registry());
        let effective =
            policy.for_app(login, &manifest.id, &manifest.actions, manifest.max_label());
        let mut refusals = Vec::new();
        for action in &manifest.actions {
            match registry.get(action) {
                None => refusals.push(format!("{action} isn't an action on this computer")),
                // A remix of Mail keeps Mail's connect flow but can never
                // run it: the password goes only to the shipped Mail, so
                // a page an agent wrote can't collect it (ADR 0037).
                Some(_) if glass_policy::persons_own(action) => {}
                Some((def, _))
                    if effective.tier_for(action, def.default_tier).tier == Tier::Never =>
                {
                    refusals.push(format!(
                        "{action}: your organization's policy doesn't allow it in apps you make"
                    ));
                }
                Some(_) => {}
            }
        }
        if manifest.max_label() > effective.max_label() {
            refusals.push(format!(
                "it declares {} data, and you may open {} at most",
                manifest.max_label(),
                effective.max_label()
            ));
        }
        refusals
    }

    fn offers(&self, groups: &[String], need: &str) -> Vec<(AgentId, String)> {
        (self.offers)(groups, need)
    }
}
