//! What IT sets for the computer that glassd applies itself (ADR 0031):
//! the time zone, with the base system's `tzsetup`; USB drives, with a
//! devd rule; and remixing apps, by removing our apps' source when it's
//! off (TL-6, ADR 0032). Applied at start and again whenever a policy changes them or
//! someone signs in or out: USB drives, like the browser's settings, follow
//! whoever is signed in. Desktops hear of a new time zone, for their clock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use glass_audit::{AuditEvent, EventKind, Outcome};
use glass_platform::PlatformError;
use glass_policy::document::SettingValue;
use glass_policy::settings::Resolved;
use glass_runtime::{Runtime, SettingsHandle};

/// Who owns a computer no organization manages (ADR 0038): the account
/// first-run setup made, as it recorded in `setup.toml` beside glassd's
/// state, or on a computer set up before it recorded one, its first
/// person's account (the lowest uid with a login).
pub(crate) fn owner(state_dir: &Path) -> Option<String> {
    let recorded = std::fs::read_to_string(state_dir.join("setup.toml")).ok();
    let people = glass_platform::users::people().unwrap_or_default();
    owner_of(recorded.as_deref(), &people)
}

fn owner_of(setup: Option<&str>, people: &[glass_platform::users::Person]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Setup {
        owner: Option<String>,
    }
    let recorded = setup
        .and_then(|text| toml::from_str::<Setup>(text).ok())
        .and_then(|s| s.owner)
        .filter(|login| glass_platform::users::valid_login(login));
    recorded.or_else(|| people.first().map(|p| p.name.clone()))
}

/// Learns the owner of a computer nobody manages when someone signs in or
/// out, while glassd knows of none: first-run setup makes the owner after
/// glassd has started, and their first session is the owner's like any
/// other (ADR 0038, specs/tla/Boot.tla). An owner, once known, stays.
pub(crate) fn learn_owner(
    settings: SettingsHandle,
    mut seats: tokio::sync::watch::Receiver<Option<String>>,
    read: impl Fn() -> Option<String> + Send + Sync + 'static,
) {
    let read = Arc::new(read);
    tokio::spawn(async move {
        while seats.changed().await.is_ok() {
            if settings.owner().is_some() {
                continue;
            }
            let read = Arc::clone(&read);
            if let Ok(owner) = tokio::task::spawn_blocking(move || read()).await
                && owner.is_some()
            {
                tracing::info!(owner = ?owner, "the computer's owner, from setup");
                settings.set_owner(owner);
            }
        }
    });
}

/// What glassd last applied.
#[derive(Debug, Default)]
struct Applied {
    zone: String,
    usb_storage: Option<bool>,
    remix: Option<bool>,
}

/// Runs `apply` with the computer's settings for whoever is signed in at
/// the screen (or nobody): now, whenever a policy changes them, and
/// whenever someone signs in or out. One run at a time, so a slow one
/// can't overwrite a newer one.
pub(crate) fn follow(
    settings: &SettingsHandle,
    runtime: &Arc<Runtime>,
    apply: impl Fn(&[Resolved], Option<&str>) + Send + Sync + 'static,
) {
    let mut seats = runtime.watch_seat();
    let run = {
        let settings = settings.clone();
        let runtime = Arc::downgrade(runtime);
        let one = Mutex::new(());
        Arc::new(move || {
            let _one = one.lock().unwrap_or_else(|p| p.into_inner());
            let Some(runtime) = runtime.upgrade() else {
                return;
            };
            let seat = runtime.seat_user();
            apply(&settings.computer(seat.as_deref()), seat.as_deref());
        })
    };
    run();
    {
        let run = Arc::clone(&run);
        settings.on_change(move |uid| {
            if uid.is_none() {
                run();
            }
        });
    }
    tokio::spawn(async move {
        while seats.changed().await.is_ok() {
            let run = Arc::clone(&run);
            let _ = tokio::task::spawn_blocking(move || run()).await;
        }
    });
}

/// Applies the time zone, USB drives and remixing now and whenever they
/// change. `source` is our apps' source folder (`[studio] source`).
pub(crate) fn watch(settings: &SettingsHandle, runtime: &Arc<Runtime>, source: PathBuf) {
    let applied = Mutex::new(Applied::default());
    let weak = Arc::downgrade(runtime);
    follow(settings, runtime, move |computer, seat| {
        let mut applied = applied.lock().unwrap_or_else(|p| p.into_inner());
        apply(
            computer,
            seat,
            weak.upgrade().as_deref(),
            &source,
            &mut applied,
        );
    });
}

/// TL-6: with remixing off, our apps' source leaves the computer. Removed
/// whole (links are removed, never followed); the next update brings it
/// back. Whether it removed anything.
fn remove_source(source: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(source) {
        Ok(_) => std::fs::remove_dir_all(source).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn apply(
    computer: &[Resolved],
    seat: Option<&str>,
    runtime: Option<&Runtime>,
    source: &Path,
    applied: &mut Applied,
) {
    let value = |id: &str| {
        computer
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.value.clone())
    };
    let record = |id: &str, value: serde_json::Value, done: std::result::Result<(), String>| {
        if let Some(runtime) = runtime {
            runtime.append_audit(setting_event(id, value, seat, done));
        }
    };

    if let Some(SettingValue::Text(zone)) = value("region.time_zone")
        && !zone.is_empty()
        && applied.zone != zone
    {
        match glass_platform::system::set_time_zone(&zone) {
            Ok(changed) => {
                if changed {
                    tracing::info!(%zone, "time zone set from IT's settings");
                    record("region.time_zone", zone.clone().into(), Ok(()));
                    if let Some(runtime) = runtime {
                        runtime.settings_changed();
                    }
                }
                applied.zone = zone;
            }
            Err(PlatformError::Unsupported(_)) => applied.zone = zone,
            Err(e) => {
                tracing::warn!("setting the time zone to {zone}: {e}");
                record("region.time_zone", zone.into(), Err(e.to_string()));
            }
        }
    }

    let allowed = !matches!(
        value("devices.usb_storage"),
        Some(SettingValue::Bool(false))
    );
    if applied.usb_storage != Some(allowed) {
        match glass_platform::system::set_usb_storage(allowed) {
            Ok(changed) => {
                if changed {
                    tracing::info!(allowed, "USB drives set from IT's settings");
                    record("devices.usb_storage", allowed.into(), Ok(()));
                }
                applied.usb_storage = Some(allowed);
            }
            // The development host: nothing to control, nothing to say.
            Err(PlatformError::Unsupported(_)) => applied.usb_storage = Some(allowed),
            Err(e) => {
                tracing::warn!("USB drives (allowed: {allowed}): {e}");
                record("devices.usb_storage", allowed.into(), Err(e.to_string()));
            }
        }
    }

    let remix = !matches!(value("apps.remix"), Some(SettingValue::Bool(false)));
    if applied.remix != Some(remix) {
        if remix {
            applied.remix = Some(true);
        } else {
            match remove_source(source) {
                Ok(removed) => {
                    if removed {
                        tracing::info!(source = %source.display(), "remixing is off: our apps' source removed");
                        record("apps.remix", false.into(), Ok(()));
                    }
                    applied.remix = Some(false);
                }
                Err(e) => {
                    tracing::warn!("removing {}: {e}", source.display());
                    record("apps.remix", false.into(), Err(e.to_string()));
                }
            }
        }
    }
}

/// The audit record of a computer setting glassd applied, or failed to,
/// with whoever was signed in.
fn setting_event(
    id: &str,
    value: serde_json::Value,
    seat: Option<&str>,
    done: std::result::Result<(), String>,
) -> AuditEvent {
    let shown = match &value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let whom = seat.map(|s| format!(" for {s}")).unwrap_or_default();
    let mut event = AuditEvent::new(
        EventKind::DeviceSettingApplied,
        match &done {
            Ok(()) => format!("IT's setting {id} ({shown}) applied on this computer{whom}"),
            Err(e) => format!("IT's setting {id} ({shown}) couldn't be applied{whom}: {e}"),
        },
    );
    event.user = seat.map(str::to_owned);
    event.outcome = Some(match done {
        Ok(()) => Outcome::Completed,
        Err(error) => Outcome::Failed { error },
    });
    event.detail = serde_json::json!({ "setting": id, "value": value });
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0038_the_owner_is_whom_setup_recorded_else_the_first_person() {
        let person = |name: &str, uid| glass_platform::users::Person {
            name: name.into(),
            full_name: String::new(),
            uid,
        };
        let people = [person("demo", 1001), person("dana", 1002)];
        let setup = "# First-run setup (UX-2)\nmodels = \"later\"\nowner = \"dana\"\n";
        assert_eq!(owner_of(Some(setup), &people).as_deref(), Some("dana"));
        // Set up before setup recorded one: the first person's.
        assert_eq!(
            owner_of(Some("models = \"later\"\n"), &people).as_deref(),
            Some("demo")
        );
        assert_eq!(owner_of(None, &people).as_deref(), Some("demo"));
        // Nothing a login can't be, and no one at all: no owner.
        assert_eq!(
            owner_of(Some("owner = \"../root\"\n"), &people).as_deref(),
            Some("demo")
        );
        assert_eq!(owner_of(None, &[]), None);
    }

    /// Boot.tla's "ownerAtStart": glassd starts before first-run setup
    /// makes the owner, so it knew of none for their whole first session,
    /// and Settings treated the computer as IT's.
    #[tokio::test]
    async fn adr0038_the_owner_setup_made_is_known_once_they_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let settings = SettingsHandle::new(dir.path().join("settings"));
        let (seat, seats) = tokio::sync::watch::channel(None);
        let made = Arc::new(Mutex::new(None::<String>));
        learn_owner(settings.clone(), seats, {
            let made = Arc::clone(&made);
            move || made.lock().unwrap().clone()
        });
        assert_eq!(settings.owner(), None, "glassd started before setup");
        // Setup made dana and signed her in.
        *made.lock().unwrap() = Some("dana".into());
        seat.send_replace(Some("dana".into()));
        for _ in 0..200 {
            if settings.owner().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(settings.owner().as_deref(), Some("dana"));
        // An owner, once known, stays.
        *made.lock().unwrap() = Some("sam".into());
        seat.send_replace(None);
        seat.send_replace(Some("sam".into()));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(settings.owner().as_deref(), Some("dana"));
    }

    fn computer(remix: bool) -> Vec<Resolved> {
        vec![Resolved {
            id: "apps.remix",
            value: SettingValue::Bool(remix),
            locked: true,
            source: glass_policy::settings::Source::Everyone,
        }]
    }

    /// TL-6: off, the source goes, whole, and what it linked to stays;
    /// on again, nothing is removed.
    #[test]
    fn tl6_remixing_off_removes_our_apps_source() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep.ts"), "kept").unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("apps/mail/src")).unwrap();
        std::fs::write(source.join("apps/mail/src/main.ts"), "mail").unwrap();
        std::os::unix::fs::symlink(&elsewhere, source.join("sdk")).unwrap();

        let mut applied = Applied::default();
        apply(&computer(true), None, None, &source, &mut applied);
        assert!(source.exists(), "on: nothing removed");
        apply(&computer(false), None, None, &source, &mut applied);
        assert!(!source.exists());
        assert!(elsewhere.join("keep.ts").exists(), "links aren't followed");
        assert_eq!(applied.remix, Some(false));
        // Off with nothing there, then on: nothing to do, nothing fails.
        apply(
            &computer(false),
            None,
            None,
            &source,
            &mut Applied::default(),
        );
        apply(&computer(true), None, None, &source, &mut applied);
        assert_eq!(applied.remix, Some(true));
    }

    #[test]
    fn adr0031_a_setting_applied_or_not_is_on_the_record() {
        let done = setting_event("region.time_zone", "Asia/Tokyo".into(), None, Ok(()));
        assert_eq!(done.kind, EventKind::DeviceSettingApplied);
        assert_eq!(done.outcome, Some(Outcome::Completed));
        assert_eq!(done.detail["value"], "Asia/Tokyo");
        assert!(
            done.summary
                .contains("region.time_zone (Asia/Tokyo) applied")
        );
        let failed = setting_event(
            "devices.usb_storage",
            false.into(),
            Some("dana"),
            Err("no devd".into()),
        );
        assert!(matches!(failed.outcome, Some(Outcome::Failed { .. })));
        assert!(failed.summary.contains("for dana: no devd"));
        assert_eq!(failed.user.as_deref(), Some("dana"));
    }
}
