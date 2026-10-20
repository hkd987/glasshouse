//! Model credits (ADR 0039): the model usage an org buys from us, routed
//! through OpenRouter. The org has one OpenRouter key, made with our
//! management key and capped at what it bought; its computers get it over
//! their signed sync and call models with it themselves.
//!
//! The org's key is kept here, mode 0600, because computers that enroll
//! later need it and OpenRouter shows it only once. It never appears in a
//! view an admin's browser gets, nor in a log. The management key stays
//! in the console's own config and never leaves it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context as _;
use glass_gateway_providers::{KeyUsage, OpenRouterKeys};
use glass_managed::ModelCredits;
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The most one grant adds, in dollars: a typing slip, not a purchase.
pub const MAX_GRANT_USD: f64 = 25_000.0;

/// What makes and manages org keys: OpenRouter with our management key,
/// or, for development and tests, [`MemoryKeys`].
pub trait KeyService: Send + Sync + std::fmt::Debug {
    /// A key named `name` that may spend `limit` dollars: its hash and
    /// the key itself.
    fn create(&self, name: &str, limit: f64) -> Result<(String, String), String>;
    fn set_limit(&self, hash: &str, limit: f64) -> Result<KeyUsage, String>;
    fn usage(&self, hash: &str) -> Result<KeyUsage, String>;
}

impl KeyService for OpenRouterKeys {
    fn create(&self, name: &str, limit: f64) -> Result<(String, String), String> {
        let made = OpenRouterKeys::create(self, name, limit)?;
        Ok((made.hash, made.key.expose().to_owned()))
    }

    fn set_limit(&self, hash: &str, limit: f64) -> Result<KeyUsage, String> {
        OpenRouterKeys::set_limit(self, hash, limit)
    }

    fn usage(&self, hash: &str) -> Result<KeyUsage, String> {
        OpenRouterKeys::usage(self, hash)
    }
}

/// Keys kept in memory, as OpenRouter would keep them: for development
/// (`--dev`) and tests. Nothing it makes reaches any model.
#[derive(Debug, Default)]
pub struct MemoryKeys {
    keys: Mutex<Vec<(String, KeyUsage)>>,
}

impl MemoryKeys {
    /// Records `dollars` spent on the key `hash`, as calls would.
    #[cfg(test)]
    pub fn spend(&self, hash: &str, dollars: f64) {
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, usage)) = keys.iter_mut().find(|(h, _)| h == hash) {
            usage.usage += dollars;
            usage.usage_monthly += dollars;
            usage.limit_remaining = usage.limit.map(|l| (l - usage.usage).max(0.0));
        }
    }
}

impl KeyService for MemoryKeys {
    fn create(&self, _name: &str, limit: f64) -> Result<(String, String), String> {
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        let hash = format!("{:064x}", keys.len() + 1);
        let key = format!("sk-or-v1-memory-{}", keys.len() + 1);
        keys.push((
            hash.clone(),
            KeyUsage {
                limit: Some(limit),
                limit_remaining: Some(limit),
                usage: 0.0,
                usage_monthly: 0.0,
                disabled: false,
            },
        ));
        Ok((hash, key))
    }

    fn set_limit(&self, hash: &str, limit: f64) -> Result<KeyUsage, String> {
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        let (_, usage) = keys
            .iter_mut()
            .find(|(h, _)| h == hash)
            .ok_or("no such key")?;
        usage.limit = Some(limit);
        usage.limit_remaining = Some((limit - usage.usage).max(0.0));
        Ok(usage.clone())
    }

    fn usage(&self, hash: &str) -> Result<KeyUsage, String> {
        let keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        keys.iter()
            .find(|(h, _)| h == hash)
            .map(|(_, u)| u.clone())
            .ok_or_else(|| "no such key".into())
    }
}

/// One addition to the org's credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub at: Timestamp,
    pub usd: f64,
    /// Ours: an order or invoice number, say.
    #[serde(default)]
    pub note: String,
}

/// The org's credits, as `credits.json` keeps them (mode 0600).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Credits {
    /// OpenRouter's id for the org's key.
    #[serde(default)]
    pub hash: Option<String>,
    /// The org's key, for its computers. Never in a view.
    #[serde(default)]
    key: Option<String>,
    /// Goes up whenever the key changes, so each computer fetches it once.
    #[serde(default)]
    pub version: u64,
    /// Every grant, in order: their sum is the key's limit.
    #[serde(default)]
    pub grants: Vec<Grant>,
}

impl std::fmt::Debug for Credits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credits")
            .field("hash", &self.hash)
            .field("version", &self.version)
            .field("grants", &self.grants)
            .finish_non_exhaustive()
    }
}

impl Credits {
    fn path(dir: &Path) -> PathBuf {
        dir.join("credits.json")
    }

    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn save(&self, dir: &Path) -> anyhow::Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let path = Self::path(dir);
        let tmp = path.with_extension("json.new");
        let _ = std::fs::remove_file(&tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }

    /// Dollars granted, in all: the key's limit.
    pub fn granted(&self) -> f64 {
        self.grants.iter().map(|g| g.usd).sum()
    }

    /// Adds `usd` to the org's credits: makes its key the first time,
    /// raises its limit after. Kept in `dir`.
    pub fn grant(
        &mut self,
        dir: &Path,
        keys: &dyn KeyService,
        org: &str,
        usd: f64,
        note: &str,
    ) -> Result<KeyUsage, String> {
        if !usd.is_finite() || usd <= 0.0 || usd > MAX_GRANT_USD {
            return Err(format!(
                "A grant is more than $0 and at most ${MAX_GRANT_USD:.0}."
            ));
        }
        if note.len() > 200 || note.chars().any(char::is_control) {
            return Err("The note is one line of at most 200 characters.".into());
        }
        let limit = self.granted() + usd;
        let usage = match &self.hash {
            Some(hash) => keys.set_limit(hash, limit)?,
            None => {
                let (hash, key) = keys.create(&format!("glasshouse-{org}"), limit)?;
                self.hash = Some(hash.clone());
                self.key = Some(key);
                self.version += 1;
                keys.usage(&hash)?
            }
        };
        self.grants.push(Grant {
            at: Timestamp::now(),
            usd,
            note: note.to_owned(),
        });
        self.save(dir).map_err(|e| format!("{e:#}"))?;
        Ok(usage)
    }

    /// What a computer that has version `have` gets: the key, when it
    /// changed since.
    pub fn for_device(&self, have: Option<u64>) -> Option<ModelCredits> {
        (self.version > 0 && have.is_none_or(|v| v < self.version)).then(|| ModelCredits {
            version: self.version,
            api_key: self.key.clone(),
        })
    }

    /// The Credits page: what was bought and spent, never the key.
    pub fn view(&self, usage: Option<Result<KeyUsage, String>>, on: bool) -> Value {
        let (spent, remaining, problem) = match usage {
            Some(Ok(u)) => (Some(u.usage), u.limit_remaining, None),
            Some(Err(e)) => (None, None, Some(e)),
            None => (None, None, None),
        };
        json!({
            "on": on,
            "has_key": self.hash.is_some(),
            "granted_usd": self.granted(),
            "spent_usd": spent,
            "remaining_usd": remaining,
            "grants": self.grants,
            "problem": problem,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0039_the_first_grant_makes_the_orgs_key_and_later_ones_raise_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let keys = MemoryKeys::default();
        let mut credits = Credits::default();
        assert_eq!(credits.for_device(None), None, "nothing to send yet");

        let usage = credits
            .grant(dir.path(), &keys, "acme", 100.0, "PO-1")
            .unwrap();
        assert_eq!(usage.limit, Some(100.0));
        let sent = credits.for_device(None).unwrap();
        assert_eq!(sent.version, 1);
        assert!(sent.api_key.as_deref().unwrap().starts_with("sk-or-v1-"));
        assert_eq!(credits.for_device(Some(1)), None, "the computer has it");

        keys.spend(credits.hash.as_deref().unwrap(), 30.0);
        let usage = credits
            .grant(dir.path(), &keys, "acme", 50.0, "PO-2")
            .unwrap();
        assert_eq!(
            (usage.limit, usage.limit_remaining, usage.usage),
            (Some(150.0), Some(120.0), 30.0)
        );
        assert_eq!(credits.version, 1, "the same key, a higher limit");

        // Kept 0600, and read back the same; the key never in a view.
        let path = dir.path().join("credits.json");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o077, 0);
        let back = Credits::load(dir.path()).unwrap();
        assert_eq!(back.granted(), 150.0);
        assert_eq!(back.for_device(None), credits.for_device(None));
        let view = credits.view(Some(Ok(usage)), true).to_string();
        assert!(!view.contains("sk-or-v1-"), "{view}");
        assert!(!format!("{credits:?}").contains("sk-or-v1-"));

        // Nothing silly is granted.
        for usd in [0.0, -5.0, f64::NAN, 1e9] {
            assert!(
                credits.grant(dir.path(), &keys, "acme", usd, "").is_err(),
                "{usd}"
            );
        }
        assert!(
            credits
                .grant(dir.path(), &keys, "acme", 5.0, "a\nb")
                .is_err()
        );
        assert_eq!(credits.granted(), 150.0);
    }
}
