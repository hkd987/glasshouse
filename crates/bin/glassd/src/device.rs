//! What IT sets for the whole computer that glassd applies itself (ADR
//! 0031): the time zone, with the base system's `tzsetup`, and USB drives,
//! with a devd rule. Each is applied once at start and again whenever a
//! policy changes it; desktops hear of a new time zone, for their clock.

use std::sync::{Arc, Mutex, Weak};

use glass_audit::{AuditEvent, EventKind, Outcome};
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
    let runtime = runtime.upgrade();
    let record = |id: &str, value: serde_json::Value, done: std::result::Result<(), String>| {
        if let Some(runtime) = &runtime {
            runtime.append_audit(setting_event(id, value, done));
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
                    if let Some(runtime) = &runtime {
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
}

/// The audit record of a computer setting glassd applied, or failed to.
fn setting_event(
    id: &str,
    value: serde_json::Value,
    done: std::result::Result<(), String>,
) -> AuditEvent {
    let shown = match &value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut event = AuditEvent::new(
        EventKind::DeviceSettingApplied,
        match &done {
            Ok(()) => format!("IT's setting {id} ({shown}) applied on this computer"),
            Err(e) => format!("IT's setting {id} ({shown}) couldn't be applied: {e}"),
        },
    );
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
    fn adr0031_a_setting_applied_or_not_is_on_the_record() {
        let done = setting_event("region.time_zone", "Asia/Tokyo".into(), Ok(()));
        assert_eq!(done.kind, EventKind::DeviceSettingApplied);
        assert_eq!(done.outcome, Some(Outcome::Completed));
        assert_eq!(done.detail["value"], "Asia/Tokyo");
        assert!(
            done.summary
                .contains("region.time_zone (Asia/Tokyo) applied")
        );
        let failed = setting_event("devices.usb_storage", false.into(), Err("no devd".into()));
        assert!(matches!(failed.outcome, Some(Outcome::Failed { .. })));
        assert!(failed.summary.contains("no devd"));
    }
}
