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
use glass_gateway_providers::stripe::{Checkout, CheckoutRequest, Stripe};
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

/// How the org pays for credits: Stripe with our keys, or for development
/// and tests, [`DevPayments`].
pub trait Payments: Send + Sync + std::fmt::Debug {
    /// A checkout page for `cents` of credits for `org`.
    fn checkout(
        &self,
        org: &str,
        cents: u64,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<Checkout, String>;
    /// The secret Stripe signs this console's webhooks with.
    fn webhook_secret(&self) -> &str;
}

/// Stripe, with our secret key and the webhook endpoint's secret.
#[derive(Debug)]
pub struct StripePayments {
    pub stripe: Stripe,
    pub webhook_secret: String,
}

impl Payments for StripePayments {
    fn checkout(
        &self,
        org: &str,
        cents: u64,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<Checkout, String> {
        self.stripe.checkout(CheckoutRequest {
            org,
            cents,
            success_url,
            cancel_url,
        })
    }

    fn webhook_secret(&self) -> &str {
        &self.webhook_secret
    }
}

/// For `--dev` and tests: no money moves. A checkout goes straight to the
/// page Stripe would send the admin back to; what pays it is a webhook
/// signed with this secret, as Stripe's would be.
#[derive(Debug)]
pub struct DevPayments {
    pub webhook_secret: String,
    made: Mutex<u64>,
}

impl DevPayments {
    pub fn new(webhook_secret: impl Into<String>) -> Self {
        Self {
            webhook_secret: webhook_secret.into(),
            made: Mutex::new(0),
        }
    }
}

impl Payments for DevPayments {
    fn checkout(
        &self,
        _org: &str,
        _cents: u64,
        success_url: &str,
        _cancel_url: &str,
    ) -> Result<Checkout, String> {
        let mut made = self.made.lock().unwrap_or_else(|p| p.into_inner());
        *made += 1;
        Ok(Checkout {
            id: format!("cs_dev_{made}"),
            url: success_url.to_owned(),
        })
    }

    fn webhook_secret(&self) -> &str {
        &self.webhook_secret
    }
}

/// The smallest purchase, in dollars.
pub const MIN_PURCHASE_USD: f64 = 10.0;

/// One addition to the org's credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub at: Timestamp,
    /// What the org paid, or was given, in dollars: what it sees.
    pub usd: f64,
    /// What it buys at OpenRouter, after our markup: what the key may
    /// spend. Grants from before the markup bought what they cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_usd: Option<f64>,
    /// Ours: an order or invoice number, say.
    #[serde(default)]
    pub note: String,
    /// The Stripe checkout that paid for it: each is granted once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment: Option<String>,
}

impl Grant {
    fn provider(&self) -> f64 {
        self.provider_usd.unwrap_or(self.usd)
    }
}

/// Who accepted the model providers' terms for the org, and when: OpenRouter
/// requires an org's people to be bound by them (its terms §5.1, §5.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Accepted {
    pub by: String,
    pub at: Timestamp,
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
    /// The org's acceptance of the model providers' terms.
    #[serde(default)]
    pub terms: Option<Accepted>,
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

    /// Dollars granted, in all, as the org sees them.
    pub fn granted(&self) -> f64 {
        self.grants.iter().map(|g| g.usd).sum()
    }

    /// What the key may spend at OpenRouter, in all: its limit.
    pub fn limit(&self) -> f64 {
        self.grants.iter().map(Grant::provider).sum()
    }

    /// Whether the checkout `session` was already granted.
    pub fn paid(&self, session: &str) -> bool {
        self.grants
            .iter()
            .any(|g| g.payment.as_deref() == Some(session))
    }

    /// Records that `by` accepted the model providers' terms for the org.
    pub fn accept_terms(&mut self, dir: &Path, by: &str) -> anyhow::Result<()> {
        if self.terms.is_none() {
            self.terms = Some(Accepted {
                by: by.to_owned(),
                at: Timestamp::now(),
            });
            self.save(dir)?;
        }
        Ok(())
    }

    /// Adds `usd` to the org's credits, of which OpenRouter gets what's
    /// left after our `markup` (0.25 is 25 % over OpenRouter's price):
    /// makes the org's key the first time, raises its limit after. A
    /// `payment` already granted is granted once. Kept in `dir`.
    #[allow(clippy::too_many_arguments)]
    pub fn grant(
        &mut self,
        dir: &Path,
        keys: &dyn KeyService,
        org: &str,
        usd: f64,
        markup: f64,
        note: &str,
        payment: Option<&str>,
    ) -> Result<KeyUsage, String> {
        if !usd.is_finite() || usd <= 0.0 || usd > MAX_GRANT_USD {
            return Err(format!(
                "A grant is more than $0 and at most ${MAX_GRANT_USD:.0}."
            ));
        }
        if !markup.is_finite() || !(0.0..=10.0).contains(&markup) {
            return Err(format!("{markup} isn't a markup"));
        }
        if note.len() > 200 || note.chars().any(char::is_control) {
            return Err("The note is one line of at most 200 characters.".into());
        }
        if let (Some(session), Some(hash)) = (payment, &self.hash)
            && self.paid(session)
        {
            return keys.usage(hash);
        }
        // Whole cents, rounded down: never more than was paid for.
        let provider_usd = (usd / (1.0 + markup) * 100.0).floor() / 100.0;
        let limit = self.limit() + provider_usd;
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
            provider_usd: Some(provider_usd),
            note: note.to_owned(),
            payment: payment.map(str::to_owned),
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

    /// The Credits page: what was bought and spent, in the org's dollars
    /// (OpenRouter's at the rate it paid), never the key or our markup.
    pub fn view(&self, usage: Option<Result<KeyUsage, String>>, on: bool) -> Value {
        let rate = if self.limit() > 0.0 {
            self.granted() / self.limit()
        } else {
            1.0
        };
        let cents = |usd: f64| (usd * rate * 100.0).round() / 100.0;
        let (spent, remaining, problem) = match usage {
            Some(Ok(u)) => (Some(cents(u.usage)), u.limit_remaining.map(cents), None),
            Some(Err(e)) => (None, None, Some(e)),
            None => (None, None, None),
        };
        let grants: Vec<Value> = self
            .grants
            .iter()
            .map(|g| json!({ "at": g.at, "usd": g.usd, "note": g.note, "paid": g.payment.is_some() }))
            .collect();
        json!({
            "on": on,
            "has_key": self.hash.is_some(),
            "granted_usd": self.granted(),
            "spent_usd": spent,
            "remaining_usd": remaining,
            "grants": grants,
            "terms_accepted": self.terms.is_some(),
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
            .grant(dir.path(), &keys, "acme", 100.0, 0.0, "PO-1", None)
            .unwrap();
        assert_eq!(usage.limit, Some(100.0));
        let sent = credits.for_device(None).unwrap();
        assert_eq!(sent.version, 1);
        assert!(sent.api_key.as_deref().unwrap().starts_with("sk-or-v1-"));
        assert_eq!(credits.for_device(Some(1)), None, "the computer has it");

        keys.spend(credits.hash.as_deref().unwrap(), 30.0);
        let usage = credits
            .grant(dir.path(), &keys, "acme", 50.0, 0.0, "PO-2", None)
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
                credits
                    .grant(dir.path(), &keys, "acme", usd, 0.0, "", None)
                    .is_err(),
                "{usd}"
            );
        }
        assert!(
            credits
                .grant(dir.path(), &keys, "acme", 5.0, 0.0, "a\nb", None)
                .is_err()
        );
        assert_eq!(credits.granted(), 150.0);
    }

    #[test]
    fn adr0039_a_paid_checkout_buys_credits_less_our_markup_once_and_shows_in_the_orgs_dollars() {
        let dir = tempfile::tempdir().unwrap();
        let keys = MemoryKeys::default();
        let mut credits = Credits::default();
        // $100 paid at a 25 % markup buys $80 at OpenRouter.
        let usage = credits
            .grant(
                dir.path(),
                &keys,
                "acme",
                100.0,
                0.25,
                "Paid by card",
                Some("cs_1"),
            )
            .unwrap();
        assert_eq!(usage.limit, Some(80.0));
        assert!(credits.paid("cs_1"));
        // Stripe sends the event again: nothing more is granted.
        let again = credits
            .grant(
                dir.path(),
                &keys,
                "acme",
                100.0,
                0.25,
                "Paid by card",
                Some("cs_1"),
            )
            .unwrap();
        assert_eq!(again.limit, Some(80.0));
        assert_eq!(credits.grants.len(), 1);

        // OpenRouter spends $20: the org sees $25 of its $100 spent.
        keys.spend(credits.hash.as_deref().unwrap(), 20.0);
        let usage = keys.usage(credits.hash.as_deref().unwrap()).unwrap();
        let view = credits.view(Some(Ok(usage)), false);
        assert_eq!(
            (
                &view["granted_usd"],
                &view["spent_usd"],
                &view["remaining_usd"]
            ),
            (&json!(100.0), &json!(25.0), &json!(75.0))
        );
        assert_eq!(view["grants"][0]["paid"], true);
        let shown = view.to_string();
        assert!(
            !shown.contains("cs_1") && !shown.contains("provider"),
            "{shown}"
        );

        // Odd amounts round down to the cent at OpenRouter.
        credits
            .grant(dir.path(), &keys, "acme", 10.0, 0.3, "", Some("cs_2"))
            .unwrap();
        assert_eq!(credits.limit(), 80.0 + 7.69);
        assert!(
            credits
                .grant(dir.path(), &keys, "acme", 10.0, -1.0, "", None)
                .is_err()
        );

        // Who accepted the providers' terms is kept once.
        credits.accept_terms(dir.path(), "pat").unwrap();
        credits.accept_terms(dir.path(), "sam").unwrap();
        assert_eq!(Credits::load(dir.path()).unwrap().terms.unwrap().by, "pat");
    }
}
