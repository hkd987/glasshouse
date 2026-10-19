//! What IT sets for the whole computer that glassd applies itself (ADR
//! 0031): the time zone, with the base system's `tzsetup`, and USB drives,
//! with a devd rule. Each is applied once at start and again whenever a
//! policy changes it; desktops hear of a new time zone, for their clock.

use std::sync::{Arc, Mutex, Weak};

use glass_platform::PlatformError;
use glass_policy::document::SettingValue;
use glass_runtime::{Runtime, SettingsHandle};

/// What glassd last applied.
#[derive(Debug, Default)]
struct Applied {
    zone: String,
    usb_storage: Option<bool>,
}

/// Applies the computer's settings now and whenever they change.
pub(crate) fn watch(settings: &SettingsHandle, runtime: &Arc<Runtime>) {
    let apply = {
        let settings = settings.clone();
        let runtime = Arc::downgrade(runtime);
        let applied = Mutex::new(Applied::default());
        Arc::new(move || {
            let mut applied = applied.lock().unwrap_or_else(|p| p.into_inner());
            apply(&settings, &runtime, &mut applied);
        })
    };
    apply();
    settings.on_change(move |uid| {
        if uid.is_none() {
            apply();
        }
    });
}

fn apply(settings: &SettingsHandle, runtime: &Weak<Runtime>, applied: &mut Applied) {
    let device = settings.device();
    let value = |id: &str| device.iter().find(|r| r.id == id).map(|r| r.value.clone());

    if let Some(SettingValue::Text(zone)) = value("region.time_zone")
        && !zone.is_empty()
        && applied.zone != zone
    {
        match glass_platform::system::set_time_zone(&zone) {
            Ok(()) => {
                tracing::info!(%zone, "time zone set from IT's settings");
                applied.zone = zone;
                if let Some(runtime) = runtime.upgrade() {
                    runtime.settings_changed();
                }
            }
            Err(e) => tracing::warn!("setting the time zone to {zone}: {e}"),
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
                }
                applied.usb_storage = Some(allowed);
            }
            // The development host: nothing to control, nothing to say.
            Err(PlatformError::Unsupported(_)) => applied.usb_storage = Some(allowed),
            Err(e) => tracing::warn!("USB drives (allowed: {allowed}): {e}"),
        }
    }
}
