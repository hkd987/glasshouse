//! Bluetooth in Settings (CF-13, ADR 0062): keyboards, mice and other
//! classic devices, paired, seen and removed, inside the rules IT set (or
//! the computer's owner did): whether Bluetooth may be on, and whether only
//! input devices may be paired.
//!
//! The controller is the base system's stack as glassd drives it
//! (`glass_platform::bluetooth`); everything that decides is here, so it's
//! tested against the stand-in. A device is paired only as the last search
//! found it: the request names its address, never what it is.

pub use glass_platform::bluetooth::{Controller, Device, Kind};
use serde::Serialize;

/// What IT (or the computer's owner) decided about Bluetooth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    pub enabled: bool,
    pub input_only: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            enabled: true,
            input_only: false,
        }
    }
}

impl Rules {
    /// Whether `device` may be paired here.
    pub fn allows(&self, device: &Device) -> Result<(), String> {
        if !self.enabled {
            return Err("Bluetooth is turned off on this computer.".into());
        }
        if self.input_only && !device.kind().input() {
            return Err(format!(
                "This computer pairs only keyboards, mice and other input devices, and {} is \
                 a {}.",
                shown_name(device),
                device.kind().label().to_lowercase()
            ));
        }
        if device.kind() == Kind::Audio {
            return Err(
                "Headphones and speakers can't be used over Bluetooth on this computer yet.".into(),
            );
        }
        Ok(())
    }
}

fn shown_name(device: &Device) -> String {
    if device.name.is_empty() {
        device.address.clone()
    } else {
        device.name.clone()
    }
}

/// A device as Settings lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shown {
    pub address: String,
    pub name: String,
    pub kind: Kind,
    pub label: &'static str,
    pub paired: bool,
    pub connected: bool,
    /// Why it can't be paired here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
}

/// What Settings shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    /// The computer has a controller.
    pub present: bool,
    /// The stack is up: devices connect.
    pub on: bool,
    /// IT (or the owner) lets it be on.
    pub allowed: bool,
    pub input_only: bool,
    /// Paired first, then what the last search found.
    pub devices: Vec<Shown>,
}

/// What Settings shows, with `nearby` from the last search.
pub fn view(
    controller: Option<&Controller>,
    rules: &Rules,
    nearby: &[Device],
) -> Result<View, String> {
    let Some(controller) = controller else {
        return Ok(View {
            present: false,
            on: false,
            allowed: rules.enabled,
            input_only: rules.input_only,
            devices: Vec::new(),
        });
    };
    let text = |e: glass_platform::PlatformError| e.to_string();
    let on = controller.on().map_err(text)?;
    let paired = controller.paired().map_err(text)?;
    let connected = if on {
        controller.connected().map_err(text)?
    } else {
        Vec::new()
    };
    let shown = |d: &Device, paired: bool| Shown {
        address: d.address.clone(),
        name: shown_name(d),
        kind: d.kind(),
        label: d.kind().label(),
        paired,
        connected: connected.contains(&d.address),
        blocked: if paired { None } else { rules.allows(d).err() },
    };
    let mut devices: Vec<Shown> = paired.iter().map(|d| shown(d, true)).collect();
    devices.extend(
        nearby
            .iter()
            .filter(|d| !paired.iter().any(|p| p.address == d.address))
            .map(|d| shown(d, false)),
    );
    Ok(View {
        present: true,
        on,
        allowed: rules.enabled,
        input_only: rules.input_only,
        devices,
    })
}

/// Pairs the device at `address` that the last search found: the PIN the
/// person types on it.
pub fn pair(
    controller: &Controller,
    rules: &Rules,
    nearby: &[Device],
    address: &str,
) -> Result<String, String> {
    let device = nearby
        .iter()
        .find(|d| d.address == address.to_ascii_lowercase())
        .ok_or_else(|| "That device wasn't found nearby: look for devices again.".to_owned())?;
    rules.allows(device)?;
    if !controller.on().map_err(|e| e.to_string())? {
        return Err("Turn Bluetooth on first.".into());
    }
    let pin = glass_platform::bluetooth::pin().map_err(|e| e.to_string())?;
    controller.pair(device, &pin).map_err(|e| e.to_string())?;
    Ok(pin)
}

/// Turns Bluetooth on or off, as the person may when IT lets it be on.
pub fn power(controller: &Controller, rules: &Rules, on: bool) -> Result<(), String> {
    if on && !rules.enabled {
        return Err("Bluetooth is turned off on this computer.".into());
    }
    controller.set_on(on).map_err(|e| e.to_string())
}

/// Makes the controller what the rules say, as glassd starts and whenever
/// they change: off when IT turned it off. What it changed, in words.
pub fn reconcile(controller: &Controller, rules: &Rules) -> Result<Vec<String>, String> {
    if !rules.enabled && controller.on().map_err(|e| e.to_string())? {
        controller.set_on(false).map_err(|e| e.to_string())?;
        return Ok(vec!["Bluetooth turned off".into()]);
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use glass_platform::bluetooth::StandIn;

    use super::*;

    fn device(address: &str, name: &str, class: u32) -> Device {
        Device {
            address: address.into(),
            name: name.into(),
            class,
        }
    }

    #[test]
    fn cf13_it_turns_bluetooth_off_or_allows_only_input_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bt.json");
        let keyboard = device("00:1f:20:3a:4b:5c", "Keyboard K380", 0x002540);
        let phone = device("00:80:37:29:19:a4", "Pav's T39", 0x520204);
        let headset = device("00:1b:66:01:02:03", "Headphones", 0x240404);
        let nearby = vec![keyboard.clone(), phone.clone(), headset.clone()];
        std::fs::write(
            &path,
            serde_json::to_string(&StandIn {
                on: true,
                nearby: nearby.clone(),
                ..StandIn::default()
            })
            .unwrap(),
        )
        .unwrap();
        let c = Controller::stand_in(&path);
        let open = Rules::default();

        // Only what the search found, by its address.
        let unknown = pair(&c, &open, &nearby, "00:00:00:00:00:01").unwrap_err();
        assert!(unknown.contains("wasn't found"), "{unknown}");
        // Headsets need the base system's work first.
        assert!(pair(&c, &open, &nearby, &headset.address).is_err());

        // Input only: the phone is refused, the keyboard pairs.
        let input = Rules {
            input_only: true,
            ..open
        };
        let shown = view(Some(&c), &input, &nearby).unwrap();
        let phone_row = shown
            .devices
            .iter()
            .find(|d| d.address == phone.address)
            .unwrap();
        assert!(
            phone_row
                .blocked
                .as_deref()
                .unwrap()
                .contains("input devices")
        );
        assert!(pair(&c, &input, &nearby, &phone.address).is_err());
        let pin = pair(&c, &input, &nearby, "00:1F:20:3A:4B:5C").unwrap();
        assert_eq!(pin.len(), 6);
        let shown = view(Some(&c), &input, &nearby).unwrap();
        assert_eq!(shown.devices[0].address, keyboard.address);
        assert!(shown.devices[0].paired && shown.devices[0].connected);
        assert_eq!(shown.devices[0].label, "Keyboard");

        // Off: it comes down, nothing pairs, and the person can't turn it on.
        let off = Rules {
            enabled: false,
            ..open
        };
        assert_eq!(reconcile(&c, &off).unwrap(), ["Bluetooth turned off"]);
        assert!(reconcile(&c, &off).unwrap().is_empty(), "once");
        assert!(power(&c, &off, true).is_err());
        assert!(pair(&c, &off, &nearby, &phone.address).is_err());
        let shown = view(Some(&c), &off, &nearby).unwrap();
        assert!(!shown.on && !shown.allowed);
        assert!(shown.devices[0].paired, "paired devices are kept");

        // Allowed again: the person turns it on.
        power(&c, &open, true).unwrap();
        assert!(view(Some(&c), &open, &nearby).unwrap().on);
        // No controller: nothing to show.
        assert!(!view(None, &open, &[]).unwrap().present);
    }
}
