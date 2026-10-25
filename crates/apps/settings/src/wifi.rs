//! Wi-Fi in Settings (CF-12, ADR 0059): the networks nearby, the one in
//! use, joining and forgetting, inside the rules IT set (or the computer's
//! owner did): the organization's networks, which glassd keeps joined by
//! themselves, blocked networks, and whether only the organization's may
//! be joined.
//!
//! [`Radio`] is the computer's Wi-Fi as glassd drives it (wpa_supplicant,
//! `glass_platform::wifi`); everything that decides is here, so it's tested
//! against a stand-in.

use glass_platform::wifi::{Known, Nearby, Security, Status};
use serde::Serialize;

/// The computer's Wi-Fi radio, as glassd drives it.
pub trait Radio: Send + Sync {
    /// Whether the computer has a radio with wpa_supplicant running on it.
    fn present(&self) -> bool;
    fn status(&self) -> Result<Status, String>;
    /// The networks nearby, from the last scan; starts the next one.
    fn nearby(&self) -> Result<Vec<Nearby>, String>;
    fn known(&self) -> Result<Vec<Known>, String>;
    fn add(&self, name: &str, password: Option<&str>, organization: bool) -> Result<u32, String>;
    fn select(&self, id: u32) -> Result<(), String>;
    fn disable(&self, id: u32) -> Result<(), String>;
    fn remove(&self, id: u32) -> Result<(), String>;
    fn save(&self) -> Result<(), String>;
    /// The organization's password for one of its networks, from glassd's
    /// root-only wifi-secrets.toml; none for an open one.
    fn organization_password(&self, name: &str) -> Option<String>;
}

/// What IT (or the computer's owner) decided about Wi-Fi.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub organization: Vec<String>,
    pub blocked: Vec<String>,
    pub only_organization: bool,
}

impl Rules {
    pub fn allows(&self, name: &str) -> Result<(), String> {
        if self.blocked.iter().any(|b| b == name) {
            return Err(format!("{name} is blocked on this computer."));
        }
        if self.only_organization && !self.organization.iter().any(|o| o == name) {
            return Err(format!(
                "This computer joins only your organization's networks, and {name} isn't one."
            ));
        }
        Ok(())
    }
}

/// A network as Settings lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Network {
    pub name: String,
    /// dBm, when it's nearby.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security: Option<Security>,
    /// The one in use.
    pub joined: bool,
    /// The computer joins it by itself: joined before, or the
    /// organization's.
    pub known: bool,
    /// Joining it failed, as a wrong password does: Settings asks for the
    /// password again.
    pub failed: bool,
    pub organization: bool,
    /// Why it can't be joined here, if it can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

/// What Settings shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    /// The computer has Wi-Fi.
    pub present: bool,
    /// wpa_supplicant's state, such as `COMPLETED` or `SCANNING`.
    pub state: String,
    pub joined: Option<String>,
    /// The address DHCP gave.
    pub address: Option<String>,
    pub only_organization: bool,
    /// Nearby first, strongest first; then the known ones out of range.
    pub networks: Vec<Network>,
}

pub fn view(radio: &dyn Radio, rules: &Rules) -> Result<View, String> {
    if !radio.present() {
        return Ok(View {
            present: false,
            state: String::new(),
            joined: None,
            address: None,
            only_organization: rules.only_organization,
            networks: Vec::new(),
        });
    }
    let status = radio.status()?;
    let known = radio.known()?;
    let nearby = radio.nearby()?;
    let is_known = |name: &str| known.iter().find(|k| k.name == name && !k.disabled);
    let mut networks: Vec<Network> = nearby
        .iter()
        .map(|n| Network {
            name: n.name.clone(),
            signal: Some(n.signal),
            security: Some(n.security),
            joined: status.network.as_deref() == Some(n.name.as_str()),
            known: is_known(&n.name).is_some(),
            failed: is_known(&n.name).is_some_and(|k| k.failed),
            organization: rules.organization.contains(&n.name),
            blocked: rules.allows(&n.name).err().or_else(|| {
                (n.security == Security::Enterprise && !rules.organization.contains(&n.name))
                    .then(|| format!("{} needs a sign-in this computer can't do yet.", n.name))
            }),
        })
        .collect();
    for k in known.iter().filter(|k| !k.disabled) {
        if !networks.iter().any(|n| n.name == k.name) {
            networks.push(Network {
                name: k.name.clone(),
                signal: None,
                security: None,
                joined: status.network.as_deref() == Some(k.name.as_str()),
                known: true,
                failed: k.failed,
                organization: k.organization,
                blocked: rules.allows(&k.name).err(),
            });
        }
    }
    Ok(View {
        present: true,
        state: status.state,
        joined: status.network,
        address: status.address,
        only_organization: rules.only_organization,
        networks,
    })
}

/// Joins `name`: one the computer knows, or a new one with `password`.
pub fn join(
    radio: &dyn Radio,
    rules: &Rules,
    name: &str,
    password: Option<&str>,
) -> Result<(), String> {
    rules.allows(name)?;
    if !radio.present() {
        return Err("This computer has no Wi-Fi.".into());
    }
    let known = radio.known()?;
    if let Some(k) = known.iter().find(|k| k.name == name) {
        if password.is_none() || k.organization {
            return radio.select(k.id);
        }
        // A new password for one joined before: the old entry goes.
        radio.remove(k.id)?;
    }
    let id = radio.add(name, password, false)?;
    radio.select(id)?;
    radio.save()
}

/// Forgets `name`, one the person joined. The organization's stay.
pub fn forget(radio: &dyn Radio, rules: &Rules, name: &str) -> Result<(), String> {
    if rules.organization.iter().any(|o| o == name) {
        return Err(format!(
            "{name} is your organization's network; it stays on this computer."
        ));
    }
    let known = radio.known()?;
    let ids: Vec<u32> = known
        .iter()
        .filter(|k| k.name == name)
        .map(|k| k.id)
        .collect();
    if ids.is_empty() {
        return Err(format!("This computer doesn't know {name}."));
    }
    for id in ids {
        radio.remove(id)?;
    }
    radio.save()
}

/// Makes wpa_supplicant's list what the rules say: the organization's
/// networks there (and only those still listed), blocked ones gone (the
/// computer leaves one it's on), and, with only the organization's allowed,
/// the person's own disabled. As glassd starts and whenever the rules
/// change. What it changed, in words.
pub fn reconcile(radio: &dyn Radio, rules: &Rules) -> Result<Vec<String>, String> {
    if !radio.present() {
        return Ok(Vec::new());
    }
    let mut did = Vec::new();
    let known = radio.known()?;
    for k in &known {
        let gone = (k.organization && !rules.organization.contains(&k.name))
            || rules.blocked.contains(&k.name);
        if gone {
            radio.remove(k.id)?;
            did.push(if rules.blocked.contains(&k.name) {
                format!("removed {} (blocked)", k.name)
            } else {
                format!("removed {} (no longer the organization's)", k.name)
            });
        } else if !k.organization && !k.disabled && rules.only_organization {
            radio.disable(k.id)?;
            did.push(format!("turned off {} (only the organization's)", k.name));
        }
    }
    for name in &rules.organization {
        if rules.blocked.contains(name) {
            continue;
        }
        if !known.iter().any(|k| k.organization && &k.name == name) {
            radio.add(name, radio.organization_password(name).as_deref(), true)?;
            did.push(format!("added {name} (the organization's)"));
        }
    }
    if !did.is_empty() {
        radio.save()?;
    }
    Ok(did)
}

#[cfg(test)]
pub(crate) mod fake {
    use std::sync::Mutex;

    use super::*;

    /// wpa_supplicant's list and the air, kept in memory.
    #[derive(Default)]
    pub struct FakeRadio {
        pub state: Mutex<FakeState>,
    }

    #[derive(Default)]
    pub struct FakeState {
        pub nearby: Vec<Nearby>,
        pub known: Vec<Known>,
        pub passwords: Vec<(u32, Option<String>)>,
        pub current: Option<u32>,
        pub saves: u32,
        pub next: u32,
    }

    impl FakeRadio {
        pub fn with(nearby: &[(&str, i32, Security)]) -> Self {
            let radio = Self::default();
            radio.state.lock().unwrap().nearby = nearby
                .iter()
                .map(|(n, s, sec)| Nearby {
                    name: (*n).into(),
                    signal: *s,
                    security: *sec,
                })
                .collect();
            radio
        }
    }

    impl Radio for FakeRadio {
        fn present(&self) -> bool {
            true
        }
        fn status(&self) -> Result<Status, String> {
            let s = self.state.lock().unwrap();
            let current = s.current.and_then(|id| s.known.iter().find(|k| k.id == id));
            Ok(match current {
                Some(k) => Status {
                    state: "COMPLETED".into(),
                    network: Some(k.name.clone()),
                    address: Some("192.168.4.20".into()),
                },
                None => Status {
                    state: "DISCONNECTED".into(),
                    network: None,
                    address: None,
                },
            })
        }
        fn nearby(&self) -> Result<Vec<Nearby>, String> {
            Ok(self.state.lock().unwrap().nearby.clone())
        }
        fn known(&self) -> Result<Vec<Known>, String> {
            let s = self.state.lock().unwrap();
            Ok(s.known
                .iter()
                .map(|k| Known {
                    current: s.current == Some(k.id),
                    ..k.clone()
                })
                .collect())
        }
        fn add(
            &self,
            name: &str,
            password: Option<&str>,
            organization: bool,
        ) -> Result<u32, String> {
            let mut s = self.state.lock().unwrap();
            let id = s.next;
            s.next += 1;
            s.known.push(Known {
                id,
                name: name.into(),
                current: false,
                disabled: false,
                failed: false,
                organization,
            });
            s.passwords.push((id, password.map(str::to_owned)));
            Ok(id)
        }
        fn select(&self, id: u32) -> Result<(), String> {
            self.state.lock().unwrap().current = Some(id);
            Ok(())
        }
        fn disable(&self, id: u32) -> Result<(), String> {
            let mut s = self.state.lock().unwrap();
            if s.current == Some(id) {
                s.current = None;
            }
            if let Some(k) = s.known.iter_mut().find(|k| k.id == id) {
                k.disabled = true;
            }
            Ok(())
        }
        fn remove(&self, id: u32) -> Result<(), String> {
            let mut s = self.state.lock().unwrap();
            if s.current == Some(id) {
                s.current = None;
            }
            s.known.retain(|k| k.id != id);
            Ok(())
        }
        fn save(&self) -> Result<(), String> {
            self.state.lock().unwrap().saves += 1;
            Ok(())
        }
        fn organization_password(&self, name: &str) -> Option<String> {
            (name == "Northwind Staff").then(|| "staff-password-not-real".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeRadio;
    use super::*;

    fn rules() -> Rules {
        Rules {
            organization: vec!["Northwind Staff".into()],
            blocked: vec!["Free Airport WiFi".into()],
            only_organization: false,
        }
    }

    fn air() -> FakeRadio {
        FakeRadio::with(&[
            ("Northwind Staff", -40, Security::Personal),
            ("Café Guest", -55, Security::Open),
            ("Free Airport WiFi", -60, Security::Open),
            ("Northwind Corp", -62, Security::Enterprise),
        ])
    }

    #[test]
    fn cf12_the_organization_s_networks_are_kept_and_joined_by_themselves() {
        let radio = air();
        let did = reconcile(&radio, &rules()).unwrap();
        assert_eq!(did, ["added Northwind Staff (the organization's)"]);
        let s = radio.state.lock().unwrap();
        assert_eq!(s.known[0].name, "Northwind Staff");
        assert!(s.known[0].organization);
        // Its password from the computer's own file, never from policy.
        assert_eq!(s.passwords[0].1.as_deref(), Some("staff-password-not-real"));
        drop(s);
        // Again: nothing to do.
        assert!(reconcile(&radio, &rules()).unwrap().is_empty());
        // It can't be forgotten from Settings.
        assert!(forget(&radio, &rules(), "Northwind Staff").is_err());
    }

    #[test]
    fn cf12_a_person_joins_and_forgets_their_own() {
        let radio = air();
        join(&radio, &rules(), "Café Guest", None).unwrap();
        let v = view(&radio, &rules()).unwrap();
        assert_eq!(v.joined.as_deref(), Some("Café Guest"));
        let cafe = v.networks.iter().find(|n| n.name == "Café Guest").unwrap();
        assert!(cafe.joined && cafe.known && !cafe.organization);
        forget(&radio, &rules(), "Café Guest").unwrap();
        assert_eq!(view(&radio, &rules()).unwrap().joined, None);
        assert!(radio.state.lock().unwrap().saves >= 2);
    }

    #[test]
    fn cf12_blocked_networks_and_only_the_organization_s() {
        let radio = air();
        let why = join(&radio, &rules(), "Free Airport WiFi", None).unwrap_err();
        assert!(why.contains("blocked"), "{why}");
        let v = view(&radio, &rules()).unwrap();
        let airport = v
            .networks
            .iter()
            .find(|n| n.name == "Free Airport WiFi")
            .unwrap();
        assert!(airport.blocked.is_some());
        let corp = v
            .networks
            .iter()
            .find(|n| n.name == "Northwind Corp")
            .unwrap();
        assert!(corp.blocked.as_deref().unwrap().contains("sign-in"));

        // Joined before it was blocked: blocking it leaves it.
        let open = Rules {
            blocked: vec![],
            ..rules()
        };
        join(&radio, &open, "Free Airport WiFi", None).unwrap();
        let did = reconcile(&radio, &rules()).unwrap();
        assert!(
            did.iter()
                .any(|d| d == "removed Free Airport WiFi (blocked)"),
            "{did:?}"
        );
        assert_eq!(view(&radio, &rules()).unwrap().joined, None);

        // Only the organization's: the person's own are turned off, and no
        // other can be joined.
        join(&radio, &rules(), "Café Guest", None).unwrap();
        let strict = Rules {
            only_organization: true,
            ..rules()
        };
        reconcile(&radio, &strict).unwrap();
        let v = view(&radio, &strict).unwrap();
        assert_eq!(v.joined, None, "left the café's network");
        assert!(
            join(&radio, &strict, "Café Guest", None)
                .unwrap_err()
                .contains("only your organization's")
        );
        join(&radio, &strict, "Northwind Staff", None).unwrap();
        assert_eq!(
            view(&radio, &strict).unwrap().joined.as_deref(),
            Some("Northwind Staff")
        );

        // No longer the organization's: it goes.
        let none = Rules::default();
        let did = reconcile(&radio, &none).unwrap();
        assert!(
            did.iter()
                .any(|d| d.contains("no longer the organization's")),
            "{did:?}"
        );
    }
}
