//! What the relay is told: Stripe's keys, our operators' token and the
//! consoles it pushes to, from a TOML file only its owner can read, or
//! development stand-ins with `--dev`.

use std::path::Path;

use anyhow::{Context, bail};
use glass_crypto::sha256;
use serde::Deserialize;

/// The secret Stripe signs development webhooks with, unless
/// `GLASS_BILLING_STRIPE_WEBHOOK` names another. Not a secret.
pub const DEV_WEBHOOK_SECRET: &str = "whsec_dev_not_a_secret";
/// Our operators' token in development. Not a secret.
pub const DEV_OPERATOR_TOKEN: &str = "dev-operator-token-not-a-secret";
/// Every development console's token. Not a secret.
pub const DEV_CONSOLE_TOKEN: &str = "dev-billing-token-not-a-secret";

/// The file, as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    stripe_secret_key: String,
    stripe_webhook_secret: String,
    #[serde(default)]
    stripe_endpoint: Option<String>,
    /// SHA-256 of the token our operators read deliveries with, hex.
    operator_token_sha256: String,
    #[serde(default, rename = "console")]
    consoles: Vec<ConsoleEntry>,
}

/// One org's console.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleEntry {
    pub org: String,
    /// Where it serves, such as `https://acme.console.glasshouse.example`.
    pub url: String,
    /// SHA-256 of the token it asks for checkouts with, hex: the relay
    /// never keeps the token itself.
    pub token_sha256: String,
}

/// How the relay makes checkouts.
#[derive(Debug, Clone)]
pub enum Checkouts {
    /// Stripe's API with our secret key.
    Stripe {
        endpoint: Option<String>,
        secret_key: String,
    },
    /// A stand-in that "pays" at once: the admin goes straight back.
    Development,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub webhook_secret: String,
    pub checkouts: Checkouts,
    pub operator_token_sha256: [u8; 32],
    pub consoles: Vec<ConsoleEntry>,
    /// Plain HTTP to consoles on this machine (development).
    pub allow_insecure_http: bool,
}

impl Config {
    /// The config in `path`, if only its owner can read it.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?;
        if meta.mode() & 0o077 != 0 {
            bail!(
                "{} can be read by others; it holds our Stripe keys (chmod 600)",
                path.display()
            );
        }
        let text = std::fs::read_to_string(path)?;
        // Parse errors name a line, never the text: the file holds secrets.
        let file: File = toml::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "{} isn't a valid billing config ({})",
                path.display(),
                e.span()
                    .map_or_else(|| "?".into(), |s| format!("at byte {}", s.start))
            )
        })?;
        let config = Self {
            webhook_secret: file.stripe_webhook_secret,
            checkouts: Checkouts::Stripe {
                endpoint: file.stripe_endpoint,
                secret_key: file.stripe_secret_key,
            },
            operator_token_sha256: digest(&file.operator_token_sha256)
                .context("operator_token_sha256")?,
            consoles: file.consoles,
            allow_insecure_http: false,
        };
        config.check()?;
        Ok(config)
    }

    /// Development: loopback consoles given as `org=url`, the development
    /// tokens and webhook secret, and checkouts that pay at once.
    pub fn development(consoles: &[String]) -> anyhow::Result<Self> {
        let token = hex::encode(sha256(DEV_CONSOLE_TOKEN.as_bytes()));
        let consoles = consoles
            .iter()
            .map(|spec| {
                let (org, url) = spec
                    .split_once('=')
                    .with_context(|| format!("--console {spec:?} is org=url"))?;
                Ok(ConsoleEntry {
                    org: org.trim().to_owned(),
                    url: url.trim().to_owned(),
                    token_sha256: token.clone(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let config = Self {
            webhook_secret: std::env::var("GLASS_BILLING_STRIPE_WEBHOOK")
                .unwrap_or_else(|_| DEV_WEBHOOK_SECRET.into()),
            checkouts: Checkouts::Development,
            operator_token_sha256: sha256(DEV_OPERATOR_TOKEN.as_bytes()),
            consoles,
            allow_insecure_http: true,
        };
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> anyhow::Result<()> {
        if !self.webhook_secret.starts_with("whsec_") {
            bail!("the Stripe webhook secret starts with whsec_");
        }
        if let Checkouts::Stripe { secret_key, .. } = &self.checkouts
            && !(secret_key.starts_with("sk_") || secret_key.starts_with("rk_"))
        {
            bail!("the Stripe secret key starts with sk_ (or rk_ for a restricted key)");
        }
        let mut orgs = std::collections::BTreeSet::new();
        for c in &self.consoles {
            if c.org.is_empty() || !orgs.insert(c.org.as_str()) {
                bail!("each console names a different org ({:?})", c.org);
            }
            digest(&c.token_sha256).with_context(|| format!("{}'s token_sha256", c.org))?;
            let https = c.url.starts_with("https://");
            if !https && !(self.allow_insecure_http && c.url.starts_with("http://")) {
                bail!("{}'s console must be reached over HTTPS: {}", c.org, c.url);
            }
        }
        Ok(())
    }

    /// The console with this org.
    pub fn console(&self, org: &str) -> Option<&ConsoleEntry> {
        self.consoles.iter().find(|c| c.org == org)
    }

    /// The console whose token this is.
    pub fn console_with_token(&self, token: &str) -> Option<&ConsoleEntry> {
        let given = sha256(token.as_bytes());
        self.consoles
            .iter()
            .find(|c| digest(&c.token_sha256).is_ok_and(|d| same(&d, &given)))
    }

    /// Whether `token` is our operators'.
    pub fn is_operator(&self, token: &str) -> bool {
        same(&sha256(token.as_bytes()), &self.operator_token_sha256)
    }
}

fn digest(text: &str) -> anyhow::Result<[u8; 32]> {
    hex::decode(text.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .context("a SHA-256 is 64 hex digits")
}

/// Equal, in time that doesn't depend on where they differ.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn write(dir: &Path, text: &str, mode: u32) -> std::path::PathBuf {
        let path = dir.join("billing.toml");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn adr0040_the_config_keeps_token_digests_and_refuses_what_it_shouldnt_take() {
        let dir = tempfile::tempdir().unwrap();
        let token = hex::encode(sha256(b"acme-token"));
        let ops = hex::encode(sha256(b"ops-token"));
        let good = format!(
            "stripe_secret_key = \"sk_test_x\"\nstripe_webhook_secret = \"whsec_x\"\n\
             operator_token_sha256 = \"{ops}\"\n\
             [[console]]\norg = \"acme\"\nurl = \"https://acme.console.example\"\ntoken_sha256 = \"{token}\"\n"
        );
        let config = Config::load(&write(dir.path(), &good, 0o600)).unwrap();
        assert_eq!(config.console_with_token("acme-token").unwrap().org, "acme");
        assert!(config.console_with_token("other").is_none());
        assert!(config.is_operator("ops-token") && !config.is_operator("acme-token"));

        // Readable by others: refused, as are plain HTTP and a repeated org.
        assert!(Config::load(&write(dir.path(), &good, 0o644)).is_err());
        let http = good.replace("https://acme", "http://acme");
        assert!(Config::load(&write(dir.path(), &http, 0o600)).is_err());
        let twice = format!(
            "{good}[[console]]\norg = \"acme\"\nurl = \"https://b.example\"\ntoken_sha256 = \"{token}\"\n"
        );
        assert!(Config::load(&write(dir.path(), &twice, 0o600)).is_err());
        // A parse error never quotes the file.
        let broken = good.replace("whsec_x\"", "whsec_x");
        let err = Config::load(&write(dir.path(), &broken, 0o600))
            .unwrap_err()
            .to_string();
        assert!(
            !err.contains("sk_test_x") && !err.contains("whsec_x"),
            "{err}"
        );
    }

    #[test]
    fn adr0040_development_consoles_are_org_equals_url() {
        let config = Config::development(&["acme=http://127.0.0.1:7400".into()]).unwrap();
        assert_eq!(config.console("acme").unwrap().url, "http://127.0.0.1:7400");
        assert!(config.console_with_token(DEV_CONSOLE_TOKEN).is_some());
        assert!(Config::development(&["acme".into()]).is_err());
    }
}
