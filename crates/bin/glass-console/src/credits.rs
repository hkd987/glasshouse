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
use std::collections::BTreeMap;
use std::sync::Arc;

use glass_gateway::{ModelProvider, ProviderError, ProviderReply, ProviderRequest, Secret};
use glass_gateway_providers::stripe::{Checkout, CheckoutRequest, Stripe};
use glass_gateway_providers::{KeyUsage, OpenRouterAccount, OpenRouterKeys, OpenRouterProvider};
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
    /// Our account: its balance, and what every org's key may still spend.
    fn account(&self) -> Result<OpenRouterAccount, String>;
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

    fn account(&self) -> Result<OpenRouterAccount, String> {
        OpenRouterKeys::account(self)
    }
}

/// Keys kept in memory, as OpenRouter would keep them: for development
/// (`--dev`) and tests. Nothing it makes reaches any model.
#[derive(Debug, Default)]
pub struct MemoryKeys {
    /// Hash, key and usage.
    keys: Mutex<Vec<(String, String, KeyUsage)>>,
    /// Our account's balance, if a test sets one: $1,000 otherwise.
    balance: Mutex<Option<f64>>,
}

/// What development's OpenRouter account holds.
const DEV_BALANCE_USD: f64 = 1000.0;

impl MemoryKeys {
    /// Sets our account's balance, as buying OpenRouter credits would.
    #[cfg(test)]
    pub fn set_balance(&self, dollars: f64) {
        *self.balance.lock().unwrap_or_else(|p| p.into_inner()) = Some(dollars);
    }

    /// Records `dollars` spent on the key `hash`, as calls would.
    #[cfg(test)]
    pub fn spend(&self, hash: &str, dollars: f64) {
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, _, usage)) = keys.iter_mut().find(|(h, _, _)| h == hash) {
            charge(usage, dollars);
        }
    }

    /// Charges `dollars` to `key` as OpenRouter would a call: refused once
    /// the key's limit is spent.
    fn charge_key(&self, key: &str, dollars: f64) -> Result<(), ProviderError> {
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        let Some((_, _, usage)) = keys.iter_mut().find(|(_, k, _)| k == key) else {
            return Err(ProviderError::NotConfigured("HTTP 401: no such key".into()));
        };
        if usage.limit_remaining.is_some_and(|left| left < dollars) {
            return Err(ProviderError::Unavailable(
                "HTTP 402: the credits for this key are used up".into(),
            ));
        }
        charge(usage, dollars);
        Ok(())
    }
}

fn charge(usage: &mut KeyUsage, dollars: f64) {
    usage.usage += dollars;
    usage.usage_monthly += dollars;
    usage.limit_remaining = usage.limit.map(|l| (l - usage.usage).max(0.0));
}

impl KeyService for MemoryKeys {
    fn create(&self, _name: &str, limit: f64) -> Result<(String, String), String> {
        let mut keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        let hash = format!("{:064x}", keys.len() + 1);
        let key = format!("sk-or-v1-memory-{}", keys.len() + 1);
        keys.push((
            hash.clone(),
            key.clone(),
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
        let (_, _, usage) = keys
            .iter_mut()
            .find(|(h, _, _)| h == hash)
            .ok_or("no such key")?;
        usage.limit = Some(limit);
        usage.limit_remaining = Some((limit - usage.usage).max(0.0));
        Ok(usage.clone())
    }

    fn usage(&self, hash: &str) -> Result<KeyUsage, String> {
        let keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        keys.iter()
            .find(|(h, _, _)| h == hash)
            .map(|(_, _, u)| u.clone())
            .ok_or_else(|| "no such key".into())
    }

    fn account(&self) -> Result<OpenRouterAccount, String> {
        let keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        let spent: f64 = keys.iter().map(|(_, _, u)| u.usage).sum();
        let bought = self
            .balance
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unwrap_or(DEV_BALANCE_USD);
        Ok(OpenRouterAccount {
            balance: bought - spent,
            outstanding: keys.iter().filter_map(|(_, _, u)| u.limit_remaining).sum(),
        })
    }
}

/// Sends a model call on to a provider with the org's key (ADR 0039):
/// OpenRouter, or for development and tests, [`DevModels`].
pub trait ModelForwarder: Send + Sync + std::fmt::Debug {
    fn complete(
        &self,
        key: &str,
        request: &ProviderRequest<'_>,
    ) -> Result<ProviderReply, ProviderError>;
}

/// OpenRouter with the org's key. The provider (and its TLS setup) is
/// kept while the key stays the same.
pub struct OpenRouterForwarder {
    endpoint: Option<String>,
    made: Mutex<Option<(String, Arc<OpenRouterProvider>)>>,
}

impl OpenRouterForwarder {
    pub fn new(endpoint: Option<&str>) -> Self {
        Self {
            endpoint: endpoint.map(str::to_owned),
            made: Mutex::new(None),
        }
    }
}

/// Never the key.
impl std::fmt::Debug for OpenRouterForwarder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouterForwarder")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl ModelForwarder for OpenRouterForwarder {
    fn complete(
        &self,
        key: &str,
        request: &ProviderRequest<'_>,
    ) -> Result<ProviderReply, ProviderError> {
        let provider = {
            let mut made = self.made.lock().unwrap_or_else(|p| p.into_inner());
            match &*made {
                Some((k, provider)) if k == key => Arc::clone(provider),
                _ => {
                    let provider = Arc::new(OpenRouterProvider::new(
                        self.endpoint.as_deref(),
                        Secret::new(key),
                    ));
                    *made = Some((key.to_owned(), Arc::clone(&provider)));
                    provider
                }
            }
        };
        provider.complete(request)
    }
}

/// For `--dev` and tests: answers on this machine, charging the in-memory
/// key as OpenRouter would ($3 in and $15 out per million tokens). It
/// calls a tool the way the scripted provider does (ADR 0041), so tool use
/// crosses the console in the suites.
#[derive(Debug)]
pub struct DevModels {
    keys: Arc<MemoryKeys>,
}

impl DevModels {
    pub fn new(keys: Arc<MemoryKeys>) -> Self {
        Self { keys }
    }
}

impl ModelForwarder for DevModels {
    fn complete(
        &self,
        key: &str,
        request: &ProviderRequest<'_>,
    ) -> Result<ProviderReply, ProviderError> {
        let asked: usize = request
            .messages
            .iter()
            .map(|m| m.content.chars().count())
            .sum::<usize>()
            + request.system.map_or(0, |s| s.chars().count());
        let call = glass_gateway::scripted_call(request);
        let last = request.messages.last();
        let text = match (&call, last) {
            (Some(call), _) => format!("Calling {} on model credits.", call.name),
            (None, Some(m)) if !m.tool_results.is_empty() => {
                let results: Vec<String> = m
                    .tool_results
                    .iter()
                    .map(|r| r.content.chars().take(200).collect())
                    .collect();
                format!(
                    "Answered on model credits. The tools returned: {}",
                    results.join(" | ")
                )
            }
            (None, m) => format!(
                "Answered on model credits: {}",
                m.map(|m| m.content.chars().take(200).collect::<String>())
                    .unwrap_or_default()
            ),
        };
        let input = (asked as u64).div_ceil(4).max(1);
        let output = (text.chars().count() as u64).div_ceil(4);
        let cost = ((input * 3 + output * 15) as f64 / 1_000_000.0 * 1e6).round() / 1e6;
        self.keys.charge_key(key, cost)?;
        Ok(ProviderReply {
            text,
            input_tokens: input,
            output_tokens: output,
            stop_reason: Some(if call.is_some() { "tool_use" } else { "stop" }.into()),
            cost_usd: Some(cost),
            served_by: Some("development/echo".into()),
            tool_calls: call.into_iter().collect(),
            blocks: Vec::new(),
            ..ProviderReply::default()
        })
    }
}

/// What one computer spent on the org's credits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceUsage {
    pub calls: u64,
    /// OpenRouter's dollars.
    pub provider_usd: f64,
    #[serde(default)]
    pub last: Option<Timestamp>,
}

/// Each computer's calls on the org's credits (ADR 0039), as the console
/// forwarded them: who, how many and what they cost, never what was said.
/// Kept in `model-usage.json`, written in the background.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceUsage>,
    #[serde(skip)]
    dirty: bool,
}

impl Usage {
    fn path(dir: &Path) -> PathBuf {
        dir.join("model-usage.json")
    }

    /// What every computer's calls cost, in OpenRouter's dollars.
    pub fn provider_spent(&self) -> f64 {
        self.devices.values().map(|d| d.provider_usd).sum()
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

    /// One call by `device` that cost OpenRouter's `provider_usd`.
    pub fn record(&mut self, device: &str, provider_usd: f64) {
        let usage = self.devices.entry(device.to_owned()).or_default();
        usage.calls += 1;
        usage.provider_usd += provider_usd;
        usage.last = Some(Timestamp::now());
        self.dirty = true;
    }

    /// Writes it if a call came in since it was last written.
    pub fn save(&mut self, dir: &Path) -> anyhow::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let path = Self::path(dir);
        let tmp = path.with_extension("json.new");
        std::fs::write(&tmp, serde_json::to_vec(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        self.dirty = false;
        Ok(())
    }
}

/// How the org pays for credits: Stripe with our keys, through the billing
/// relay (ADR 0040), or for development a stand-in where no money moves.
pub trait Payments: Send + Sync + std::fmt::Debug {
    /// A checkout page for `cents` of credits for `org`.
    fn checkout(
        &self,
        org: &str,
        cents: u64,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<Checkout, String>;
    /// Who tells this console a payment went through.
    fn events(&self) -> &PaymentEvents;
}

/// Who tells a console that a checkout was paid.
#[derive(Debug, Clone)]
pub enum PaymentEvents {
    /// Stripe itself, at `/stripe/webhook`, signed with this endpoint's
    /// secret: a Stripe account with this console alone.
    Stripe { webhook_secret: String },
    /// The billing relay (ADR 0040), at `/billing/v1/events`, signed with
    /// its key: one Stripe account for every org's console.
    Relay { key: glass_crypto::PublicKey },
}

/// Stripe, with our secret key and the webhook endpoint's secret.
#[derive(Debug)]
pub struct StripePayments {
    pub stripe: Stripe,
    pub events: PaymentEvents,
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

    fn events(&self) -> &PaymentEvents {
        &self.events
    }
}

/// The billing relay (ADR 0040): it makes checkouts with Stripe's key, for
/// the org this console's token is for, and pushes payments here.
#[derive(Debug)]
pub struct RelayPayments {
    pub relay: glass_gateway_providers::billing::BillingRelay,
    pub events: PaymentEvents,
}

impl Payments for RelayPayments {
    fn checkout(
        &self,
        _org: &str,
        cents: u64,
        success_url: &str,
        cancel_url: &str,
    ) -> Result<Checkout, String> {
        let made = self.relay.checkout(&glass_managed::billing::CheckoutAsk {
            cents,
            success_url: success_url.to_owned(),
            cancel_url: cancel_url.to_owned(),
        })?;
        Ok(Checkout {
            id: made.id,
            url: made.url,
        })
    }

    fn events(&self) -> &PaymentEvents {
        &self.events
    }
}

/// For `--dev` and tests: no money moves. A checkout goes straight to the
/// page Stripe would send the admin back to; what pays it is a webhook
/// signed with this secret, as Stripe's would be.
#[derive(Debug)]
pub struct DevPayments {
    events: PaymentEvents,
    made: Mutex<u64>,
}

impl DevPayments {
    pub fn new(webhook_secret: impl Into<String>) -> Self {
        Self {
            events: PaymentEvents::Stripe {
                webhook_secret: webhook_secret.into(),
            },
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

    fn events(&self) -> &PaymentEvents {
        &self.events
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
    /// Computers no longer hold the key (ADR 0039): those sent it before
    /// were told to forget it.
    #[serde(default)]
    withdrawn: bool,
    /// Below this many of the org's dollars left, the console warns its
    /// admins; unset, [`Credits::alert_below`] picks one.
    #[serde(default)]
    pub alert_below_usd: Option<f64>,
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
        let mut credits: Self = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        // Kept from when computers held the key: a new version tells them
        // to forget it.
        if credits.version > 0 && !credits.withdrawn {
            credits.version += 1;
            credits.withdrawn = true;
            credits.save(dir)?;
        }
        Ok(credits)
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

    /// Below how many of the org's dollars left its admins are warned:
    /// theirs, or a tenth of what they bought, at least $5.
    pub fn alert_below(&self) -> f64 {
        self.alert_below_usd
            .unwrap_or_else(|| ((self.granted() * 0.1).max(5.0) * 100.0).round() / 100.0)
    }

    /// Sets the warning's threshold, in the org's dollars; `None` goes back
    /// to the default.
    pub fn set_alert(&mut self, dir: &Path, usd: Option<f64>) -> Result<(), String> {
        if let Some(usd) = usd
            && (!usd.is_finite() || !(0.0..=MAX_GRANT_USD).contains(&usd))
        {
            return Err(format!(
                "Warn below an amount from $0 to ${MAX_GRANT_USD:.0}."
            ));
        }
        self.alert_below_usd = usd.map(|u| (u * 100.0).round() / 100.0);
        self.save(dir).map_err(|e| format!("{e:#}"))
    }

    /// The org's dollars rate: what it paid for each of OpenRouter's.
    fn rate(&self) -> f64 {
        if self.limit() > 0.0 {
            self.granted() / self.limit()
        } else {
            1.0
        }
    }

    /// What's left, in the org's dollars, by the console's own record of
    /// its computers' calls: OpenRouter's own figure needs a round trip.
    pub fn left(&self, by: &Usage) -> f64 {
        ((self.limit() - by.provider_spent()) * self.rate()).max(0.0)
    }

    /// How the org's credits stand: `None` while it has none, `low` below
    /// the threshold, `used_up` with under a cent left.
    pub fn standing(&self, left: f64) -> Option<&'static str> {
        if self.grants.is_empty() {
            None
        } else if left < 0.01 {
            Some("used_up")
        } else if left < self.alert_below() {
            Some("low")
        } else {
            None
        }
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
                self.withdrawn = true;
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
        // The key stays here (ADR 0039): computers call through the console.
        // A computer that was sent it before hears that it's gone.
        (self.version > 0 && have.is_none_or(|v| v < self.version)).then_some(ModelCredits {
            version: self.version,
            api_key: None,
        })
    }

    /// The org's OpenRouter key, for forwarding its computers' calls.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The Credits page: what was bought and spent, in the org's dollars
    /// (OpenRouter's at the rate it paid), never the key or our markup.
    pub fn view(&self, usage: Option<Result<KeyUsage, String>>, on: bool, by: &Usage) -> Value {
        let rate = self.rate();
        let cents = |usd: f64| (usd * rate * 100.0).round() / 100.0;
        let (spent, remaining, problem) = match usage {
            Some(Ok(u)) => (Some(cents(u.usage)), u.limit_remaining.map(cents), None),
            Some(Err(e)) => (None, None, Some(e)),
            None => (None, None, None),
        };
        let mut computers: Vec<(&String, &DeviceUsage)> = by.devices.iter().collect();
        computers.sort_by(|a, b| b.1.provider_usd.total_cmp(&a.1.provider_usd));
        let computers: Vec<Value> = computers
            .into_iter()
            .take(20)
            .map(|(device, u)| {
                json!({ "device": device, "calls": u.calls, "spent_usd": cents(u.provider_usd), "last": u.last })
            })
            .collect();
        let grants: Vec<Value> = self
            .grants
            .iter()
            .map(|g| json!({ "at": g.at, "usd": g.usd, "note": g.note, "paid": g.payment.is_some() }))
            .collect();
        // OpenRouter's figure when it answered, else the console's own.
        let standing = self.standing(remaining.unwrap_or_else(|| self.left(by)));
        json!({
            "on": on,
            "standing": standing,
            "alert_below_usd": self.alert_below(),
            "alert_set": self.alert_below_usd.is_some(),
            "has_key": self.hash.is_some(),
            "granted_usd": self.granted(),
            "spent_usd": spent,
            "remaining_usd": remaining,
            "grants": grants,
            "terms_accepted": self.terms.is_some(),
            "by_computer": computers,
            "problem": problem,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0039_admins_are_warned_when_credits_run_low_or_out() {
        let dir = tempfile::tempdir().unwrap();
        let keys = MemoryKeys::default();
        let mut credits = Credits::default();
        let mut usage = Usage::default();
        // Nothing bought: nothing to warn about.
        assert_eq!(credits.standing(0.0), None);

        // $100 paid at 25 % over: $80 at OpenRouter, $100 to the org.
        credits
            .grant(dir.path(), &keys, "acme", 100.0, 0.25, "PO-1", None)
            .unwrap();
        assert_eq!(credits.alert_below(), 10.0, "a tenth of what was bought");
        assert_eq!(credits.left(&usage), 100.0);
        assert_eq!(credits.standing(credits.left(&usage)), None);

        // $73 at OpenRouter spent: $91.25 of the org's, $8.75 left.
        usage.record("lab-1", 73.0);
        assert_eq!(credits.left(&usage), 8.75);
        assert_eq!(credits.standing(credits.left(&usage)), Some("low"));
        // The admin's own threshold, kept.
        credits.set_alert(dir.path(), Some(5.0)).unwrap();
        assert_eq!(credits.standing(credits.left(&usage)), None);
        assert_eq!(Credits::load(dir.path()).unwrap().alert_below(), 5.0);
        assert!(credits.set_alert(dir.path(), Some(-1.0)).is_err());
        assert!(credits.set_alert(dir.path(), Some(f64::NAN)).is_err());
        credits.set_alert(dir.path(), None).unwrap();
        assert_eq!(credits.alert_below(), 10.0);

        usage.record("lab-2", 7.0);
        assert_eq!(credits.left(&usage), 0.0);
        assert_eq!(credits.standing(0.0), Some("used_up"));
        // A small purchase still warns at $5.
        let mut small = Credits::default();
        small
            .grant(dir.path(), &keys, "acme", 20.0, 0.0, "PO-2", None)
            .unwrap();
        assert_eq!(small.alert_below(), 5.0);
    }

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
        assert_eq!(sent.api_key, None, "the key stays on our servers");
        assert!(credits.key().unwrap().starts_with("sk-or-v1-"));
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
        let view = credits
            .view(Some(Ok(usage)), true, &Usage::default())
            .to_string();
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
        let view = credits.view(Some(Ok(usage)), false, &Usage::default());
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
