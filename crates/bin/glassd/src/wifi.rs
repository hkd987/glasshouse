//! The computer's Wi-Fi for Settings (CF-12, ADR 0059): wpa_supplicant's
//! control socket (`glass_platform::wifi`), and the organization's
//! passwords from a root-only file, as model secrets are kept.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use glass_first_party::settings_wifi::Radio;
use glass_platform::wifi::{Control, Known, Nearby, Status};

/// The radio as glassd drives it.
pub struct SystemRadio {
    control: Control,
    secrets: PathBuf,
}

impl SystemRadio {
    pub fn new(config: &crate::config::WifiConfig) -> Self {
        Self {
            control: Control::new(&config.control_dir, &config.interface),
            secrets: config.secrets.clone(),
        }
    }
}

/// Gives the computer's radio its station interface, if it has a radio
/// and the interface isn't there yet: rc.conf keeps it for every start
/// after (ADR 0059).
pub fn set_up_station() {
    let Ok(radios) = glass_platform::wifi::radios() else {
        return;
    };
    if let Some(radio) = radios.first()
        && let Err(e) = glass_platform::wifi::ensure_station(radio)
    {
        tracing::warn!("setting up Wi-Fi on {radio}: {e}");
    }
}

fn text(e: glass_platform::PlatformError) -> String {
    e.to_string()
}

impl Radio for SystemRadio {
    fn present(&self) -> bool {
        self.control.available()
    }
    fn status(&self) -> Result<Status, String> {
        self.control.status().map_err(text)
    }
    fn nearby(&self) -> Result<Vec<Nearby>, String> {
        // The next scan starts now; this is what the last one saw.
        let _ = self.control.scan();
        self.control.nearby().map_err(text)
    }
    fn known(&self) -> Result<Vec<Known>, String> {
        self.control.known().map_err(text)
    }
    fn add(&self, name: &str, password: Option<&str>, organization: bool) -> Result<u32, String> {
        self.control.add(name, password, organization).map_err(text)
    }
    fn select(&self, id: u32) -> Result<(), String> {
        self.control.select(id).map_err(text)
    }
    fn disable(&self, id: u32) -> Result<(), String> {
        self.control.disable(id).map_err(text)
    }
    fn remove(&self, id: u32) -> Result<(), String> {
        self.control.remove(id).map_err(text)
    }
    fn save(&self) -> Result<(), String> {
        self.control.save().map_err(text)
    }
    fn organization_password(&self, name: &str) -> Option<String> {
        match passwords(&self.secrets) {
            Ok(table) => table.get(name).cloned(),
            Err(e) => {
                tracing::warn!("{e}");
                None
            }
        }
    }
}

/// `[networks."<name>"] password = "…"` from `path`: root's, mode 600, or
/// refused, as model secrets are. Missing: none.
fn passwords(path: &Path) -> Result<std::collections::BTreeMap<String, String>, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    // glassd's own: root's on a computer, the developer's on theirs.
    let owner = glass_platform::users::effective_uid();
    if meta.uid() != owner {
        return Err(format!(
            "{} isn't owned by uid {owner}; its Wi-Fi passwords are ignored",
            path.display()
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is mode {:o}; only root may read it (chmod 600); its Wi-Fi passwords are ignored",
            path.display(),
            meta.mode() & 0o777
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(table
        .get("networks")
        .and_then(|n| n.as_table())
        .map(|networks| {
            networks
                .iter()
                .filter_map(|(name, v)| {
                    Some((name.clone(), v.get("password")?.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn cf12_the_organization_s_passwords_are_root_s_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wifi-secrets.toml");
        assert!(passwords(&path).unwrap().is_empty(), "missing: none");
        std::fs::write(
            &path,
            "[networks.\"Northwind Staff\"]\npassword = \"staff-password-not-real\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(passwords(&path).unwrap_err().contains("chmod 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            passwords(&path).unwrap()["Northwind Staff"],
            "staff-password-not-real"
        );
    }
}
