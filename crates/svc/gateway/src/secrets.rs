//! The narrowest stand-in for the secrets broker (MP-4, ADR 0018): one TOML
//! file only glassd's own user can read, with a table per provider id.
//!
//! ```toml
//! [anthropic]
//! api_key = "…"
//!
//! [bedrock]
//! access_key_id = "ASIA…"        # temporary credentials only (MP-1)
//! secret_access_key = "…"
//! session_token = "…"
//! expires = "2026-10-19T00:00:00Z"
//! ```
//!
//! It lives outside every path mounted into a jail, is refused unless it is a
//! regular file owned by glassd's user with no group or other access, and its
//! values can't be printed: [`Secret`] has no `Display` or `Serialize`, and
//! parse errors name a line, never its text.

use std::collections::BTreeMap;
use std::fmt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// A credential. Debug output shows only that there is one.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that sends it (a request header).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("can't read the model secrets file: {0}")]
    Io(#[from] std::io::Error),
    #[error("refusing the model secrets file: {0}")]
    Unsafe(String),
    #[error("the model secrets file is not valid TOML (line {line})")]
    Parse { line: usize },
    #[error("the model secrets file: {0}")]
    Invalid(String),
}

/// Credentials by provider id, then name.
#[derive(Debug, Default, Clone)]
pub struct Secrets {
    providers: BTreeMap<String, BTreeMap<String, Secret>>,
}

impl Secrets {
    /// Loads `path` if it exists. `owner` is the uid glassd runs as; the file
    /// must be a regular file it owns, readable by nobody else.
    pub fn load(path: &Path, owner: u32) -> Result<Self, SecretsError> {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        if !meta.file_type().is_file() {
            return Err(SecretsError::Unsafe(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        if meta.uid() != owner {
            return Err(SecretsError::Unsafe(format!(
                "{} is owned by uid {}, not {owner}",
                path.display(),
                meta.uid()
            )));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(SecretsError::Unsafe(format!(
                "{} is mode {:o}; only its owner may read it (chmod 600)",
                path.display(),
                meta.mode() & 0o777
            )));
        }
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn parse(text: &str) -> Result<Self, SecretsError> {
        let table: toml::Table = toml::from_str(text).map_err(|e| SecretsError::Parse {
            line: e
                .span()
                .map_or(0, |span| text[..span.start].matches('\n').count() + 1),
        })?;
        let mut providers = BTreeMap::new();
        for (provider, values) in table {
            let toml::Value::Table(values) = values else {
                return Err(SecretsError::Invalid(format!(
                    "{provider:?} must be a table of credentials"
                )));
            };
            let mut secrets = BTreeMap::new();
            for (name, value) in values {
                let value = match value {
                    toml::Value::String(s) => s,
                    toml::Value::Datetime(d) => d.to_string(),
                    _ => {
                        return Err(SecretsError::Invalid(format!(
                            "{provider}.{name} must be a string"
                        )));
                    }
                };
                secrets.insert(name, Secret(value));
            }
            providers.insert(provider, secrets);
        }
        Ok(Self { providers })
    }

    pub fn get(&self, provider: &str, name: &str) -> Option<&Secret> {
        self.providers.get(provider)?.get(name)
    }

    /// Provider ids that have credentials.
    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const FILE: &str = "[anthropic]\napi_key = \"sk-ant-test-value\"\n\n[bedrock]\nexpires = 2026-10-19T00:00:00Z\n";

    fn owner(dir: &Path) -> u32 {
        std::fs::metadata(dir).unwrap().uid()
    }

    #[test]
    fn mp4_only_a_private_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        let me = owner(dir.path());
        assert!(
            Secrets::load(&path, me)
                .unwrap()
                .get("anthropic", "api_key")
                .is_none()
        );

        std::fs::write(&path, FILE).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = Secrets::load(&path, me).unwrap_err();
        assert!(matches!(err, SecretsError::Unsafe(_)), "{err}");
        assert!(err.to_string().contains("chmod 600"));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            Secrets::load(&path, me + 1),
            Err(SecretsError::Unsafe(_))
        ));
        let secrets = Secrets::load(&path, me).unwrap();
        assert_eq!(
            secrets.get("anthropic", "api_key").map(Secret::expose),
            Some("sk-ant-test-value")
        );
        assert_eq!(
            secrets.get("bedrock", "expires").map(Secret::expose),
            Some("2026-10-19T00:00:00Z")
        );

        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(matches!(
            Secrets::load(&link, me),
            Err(SecretsError::Unsafe(_))
        ));
    }

    #[test]
    fn mp4_secrets_never_show_in_output() {
        let secrets = Secrets::parse(FILE).unwrap();
        let printed = format!("{secrets:?}");
        assert!(!printed.contains("sk-ant-test-value"), "{printed}");
        assert!(printed.contains("Secret(…)"));

        let broken = "[anthropic]\napi_key = \"sk-ant-oops\nmore = 1\n";
        let err = Secrets::parse(broken).unwrap_err();
        assert!(!err.to_string().contains("sk-ant-oops"), "{err}");
        assert!(err.to_string().contains("line 2"), "{err}");
    }
}
