//! Who the app acts for: the person's Glasshouse Agents address, kept in
//! the app's settings file (not a secret), and their token, kept only in
//! the system's keychain (ADR 0067, 0071): the Keychain on a Mac, the
//! Credential Manager on Windows, the kernel's keyring on Linux.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Context;
use keyring_core::{Entry, Error as KeyringError};

/// The keychain entry's service: one entry per Glasshouse Agents address.
pub const SERVICE: &str = "Glasshouse Agents";

/// Makes the system's keychain the one entries are kept in.
pub fn use_system_keychain() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new()?;
    #[cfg(target_os = "windows")]
    let store = windows_native_keyring_store::Store::new()?;
    #[cfg(target_os = "linux")]
    let store = linux_keyutils_keyring_store::Store::new()?;
    keyring_core::set_default_store(store);
    Ok(())
}

/// The token kept for `address`, if there is one.
pub fn token_for(address: &str) -> anyhow::Result<Option<String>> {
    match Entry::new(SERVICE, address)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(e) => Err(e).context("reading this computer's keychain"),
    }
}

/// Keeps `token` for `address`, replacing any before it.
pub fn keep(address: &str, token: &str) -> anyhow::Result<()> {
    Entry::new(SERVICE, address)?
        .set_password(token)
        .context("keeping your sign-in in this computer's keychain")
}

/// Forgets the token for `address`; none kept is fine.
pub fn forget(address: &str) -> anyhow::Result<()> {
    match Entry::new(SERVICE, address)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(e) => Err(e).context("removing your sign-in from this computer's keychain"),
    }
}

/// Where the app keeps its settings: the address, nothing secret.
pub fn settings_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("Glasshouse Agents").join("settings.json"))
}

/// The address the settings file names.
pub fn saved_address() -> Option<String> {
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&text).ok()?;
    settings["url"].as_str().map(str::to_owned)
}

pub fn save_address(address: Option<&str>) -> anyhow::Result<()> {
    let path = settings_path().context("this computer has no folder for an app's settings")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    }
    let settings = serde_json::json!({ "url": address });
    std::fs::write(&path, settings.to_string())
        .with_context(|| format!("writing {}", path.display()))
}

/// The address as typed, as the app keeps it: `https://`, no trailing
/// slash, and only HTTPS unless it's this computer's own (development).
pub fn tidy_address(typed: &str) -> Result<String, String> {
    let typed = typed.trim().trim_end_matches('/');
    if typed.is_empty() {
        return Err("Type your company's Glasshouse Agents address.".into());
    }
    let address = if typed.contains("://") {
        typed.to_owned()
    } else {
        format!("https://{typed}")
    };
    glass_gateway_providers::agents::AgentsClient::new(&address, "")
        .map(|_| address)
        .map_err(|_| {
            format!("{typed} isn't an HTTPS address: Glasshouse Agents is only reached over HTTPS.")
        })
}

/// What every part of the app shares: the address, the token, and what the
/// sign-in pages should say.
#[derive(Debug, Default)]
pub struct State {
    pub address: Option<String>,
    pub token: Option<String>,
    /// For the next sign-in page: why the person is signed out, or why
    /// signing in didn't finish.
    pub note: Option<String>,
    /// The person's name, once Glasshouse Agents has said it.
    pub name: Option<String>,
    /// Signing in: the page the browser was sent to.
    pub waiting: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Shared(Arc<Mutex<State>>);

impl Shared {
    pub fn new(state: State) -> Self {
        Self(Arc::new(Mutex::new(state)))
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The address and token, signed in.
    pub fn signed_in(&self) -> Option<(String, String)> {
        let state = self.lock();
        Some((state.address.clone()?, state.token.clone()?))
    }

    /// Signed out, here and in the keychain, with `why` for the sign-in
    /// page. Only if `token` is still the one in use: a sign-in that
    /// replaced it stays.
    pub fn sign_out(&self, token: Option<&str>, why: Option<String>) {
        let mut state = self.lock();
        if token.is_some() && state.token.as_deref() != token {
            return;
        }
        if let Some(address) = &state.address
            && let Err(e) = forget(address)
        {
            eprintln!("glass-agents-app: {e:#}");
        }
        state.token = None;
        state.name = None;
        state.note = why;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0071_the_token_is_kept_in_the_keychain_and_forgotten_there() {
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        let address = "https://agents.example.com";
        assert_eq!(token_for(address).unwrap(), None);
        keep(address, "gat_one").unwrap();
        keep(address, "gat_two").unwrap();
        assert_eq!(token_for(address).unwrap().as_deref(), Some("gat_two"));
        assert_eq!(token_for("https://other.example.com").unwrap(), None);

        let shared = Shared::new(State {
            address: Some(address.into()),
            token: Some("gat_two".into()),
            ..State::default()
        });
        // An old token's refusal doesn't sign out a newer sign-in.
        shared.sign_out(Some("gat_one"), Some("ran out".into()));
        assert!(shared.signed_in().is_some());
        shared.sign_out(Some("gat_two"), Some("Your sign-in ran out.".into()));
        assert!(shared.signed_in().is_none());
        assert_eq!(shared.lock().note.as_deref(), Some("Your sign-in ran out."));
        assert_eq!(token_for(address).unwrap(), None);
        forget(address).unwrap();
    }

    #[test]
    fn adr0071_an_address_is_https_unless_its_this_computers() {
        assert_eq!(
            tidy_address(" agents.acme.example/ ").unwrap(),
            "https://agents.acme.example"
        );
        assert_eq!(
            tidy_address("http://127.0.0.1:7613").unwrap(),
            "http://127.0.0.1:7613"
        );
        assert!(tidy_address("http://agents.acme.example").is_err());
        assert!(tidy_address("  ").is_err());
    }
}
