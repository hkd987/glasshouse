//! Payments on their way to consoles, kept on disk so a restart loses none.
//!
//! One file per checkout session in `outbox/` until its console takes it,
//! written to a temporary name and renamed into place. A delivery that
//! still fails after [`Timing::give_up`] moves to `dead/`, and a payment
//! naming no console we know goes to `unroutable/`; our operators see both
//! and can send a dead one again. Every delivered one is a line in
//! `delivered.jsonl`. A session is queued once, however often Stripe sends
//! its event.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use glass_crypto::sha256;
use glass_managed::billing::BillingEvent;
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

/// How often to try, and for how long.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// The wait after the first failure; each one after waits three times
    /// as long, up to `longest`.
    pub first: Duration,
    pub longest: Duration,
    /// How long after Stripe's event a delivery is still tried.
    pub give_up: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(5),
            longest: Duration::from_secs(3600),
            give_up: Duration::from_secs(3 * 24 * 3600),
        }
    }
}

impl Timing {
    /// The wait after `failures` failed attempts.
    pub fn after(&self, failures: u32) -> Duration {
        let factor = 3u32.saturating_pow(failures.saturating_sub(1));
        self.first.saturating_mul(factor).min(self.longest)
    }
}

/// One payment on its way to its console.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    pub event: BillingEvent,
    /// Its console's address when it was queued.
    pub console: String,
    pub attempts: u32,
    /// When to try next, in seconds since 1970.
    pub next_at: i64,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub delivered_at: Option<Timestamp>,
}

/// A paid checkout no console of ours is for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unroutable {
    pub session: String,
    pub org: Option<String>,
    pub cents: u64,
    pub at: Timestamp,
}

#[derive(Debug)]
pub struct Outbox {
    dir: PathBuf,
    timing: Timing,
    /// Sessions queued, delivered, dead or unroutable: never queued again.
    seen: HashSet<String>,
}

impl Outbox {
    /// The outbox kept under `dir`.
    pub fn open(dir: &Path, timing: Timing) -> anyhow::Result<Self> {
        for sub in ["outbox", "dead", "unroutable"] {
            std::fs::create_dir_all(dir.join(sub))
                .with_context(|| format!("{}", dir.join(sub).display()))?;
        }
        let mut outbox = Self {
            dir: dir.to_owned(),
            timing,
            seen: HashSet::new(),
        };
        for d in outbox
            .list("outbox")?
            .into_iter()
            .chain(outbox.list("dead")?)
        {
            outbox.seen.insert(d.event.session);
        }
        for u in outbox.unroutable()? {
            outbox.seen.insert(u.session);
        }
        for d in outbox.read_delivered(usize::MAX)? {
            outbox.seen.insert(d.event.session);
        }
        Ok(outbox)
    }

    /// Whether `session` was taken before.
    pub fn seen(&self, session: &str) -> bool {
        self.seen.contains(session)
    }

    /// Queues `event` for the console at `console`, due now. `false` when
    /// its session was taken before.
    pub fn queue(&mut self, event: BillingEvent, console: &str, now: i64) -> anyhow::Result<bool> {
        if self.seen(&event.session) {
            return Ok(false);
        }
        let delivery = Delivery {
            console: console.to_owned(),
            attempts: 0,
            next_at: now,
            last_error: None,
            delivered_at: None,
            event,
        };
        self.write("outbox", &delivery.event.session, &delivery)?;
        self.seen.insert(delivery.event.session.clone());
        Ok(true)
    }

    /// Keeps a paid checkout no console is for. `false` when seen before.
    pub fn unroutable_payment(&mut self, payment: Unroutable) -> anyhow::Result<bool> {
        if self.seen(&payment.session) {
            return Ok(false);
        }
        self.write("unroutable", &payment.session, &payment)?;
        self.seen.insert(payment.session.clone());
        Ok(true)
    }

    /// Deliveries due by `now`, oldest first.
    pub fn due(&self, now: i64) -> anyhow::Result<Vec<Delivery>> {
        let mut due: Vec<Delivery> = self
            .list("outbox")?
            .into_iter()
            .filter(|d| d.next_at <= now)
            .collect();
        due.sort_by_key(|d| d.next_at);
        Ok(due)
    }

    /// When the next delivery is due, if any is queued.
    pub fn next_due(&self) -> anyhow::Result<Option<i64>> {
        Ok(self.list("outbox")?.iter().map(|d| d.next_at).min())
    }

    /// Its console took it.
    pub fn delivered(&mut self, mut delivery: Delivery) -> anyhow::Result<()> {
        use std::io::Write as _;
        delivery.attempts += 1;
        delivery.last_error = None;
        delivery.delivered_at = Some(Timestamp::now());
        let mut line = serde_json::to_vec(&delivery)?;
        line.push(b'\n');
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("delivered.jsonl"))?;
        log.write_all(&line)?;
        log.sync_all()?;
        let _ = std::fs::remove_file(self.path("outbox", &delivery.event.session));
        Ok(())
    }

    /// Its console didn't take it: tried again later, or given up on once
    /// it's older than the timing allows. Returns when it's tried next.
    pub fn failed(
        &mut self,
        mut delivery: Delivery,
        error: &str,
        now: i64,
    ) -> anyhow::Result<Option<i64>> {
        delivery.attempts += 1;
        delivery.last_error = Some(error.chars().take(300).collect());
        let age = now - delivery.event.at.unix_seconds();
        if age >= i64::try_from(self.timing.give_up.as_secs()).unwrap_or(i64::MAX) {
            self.write("dead", &delivery.event.session, &delivery)?;
            let _ = std::fs::remove_file(self.path("outbox", &delivery.event.session));
            return Ok(None);
        }
        let wait = i64::try_from(self.timing.after(delivery.attempts).as_secs()).unwrap_or(3600);
        delivery.next_at = now + wait.max(1);
        let next = delivery.next_at;
        self.write("outbox", &delivery.event.session, &delivery)?;
        Ok(Some(next))
    }

    /// Sends a dead delivery again, now, to `console`. `false` if there's
    /// no dead one for `session`.
    pub fn revive(&mut self, session: &str, console: &str, now: i64) -> anyhow::Result<bool> {
        let path = self.path("dead", session);
        let Ok(text) = std::fs::read(&path) else {
            return Ok(false);
        };
        let mut delivery: Delivery = serde_json::from_slice(&text)?;
        delivery.console = console.to_owned();
        delivery.next_at = now;
        // A fresh window to try in, from now.
        delivery.event.at = Timestamp::now();
        self.write("outbox", session, &delivery)?;
        std::fs::remove_file(path)?;
        Ok(true)
    }

    /// What's queued, or dead.
    pub fn list(&self, which: &str) -> anyhow::Result<Vec<Delivery>> {
        self.read_all(which)
    }

    pub fn unroutable(&self) -> anyhow::Result<Vec<Unroutable>> {
        self.read_all("unroutable")
    }

    /// The last `n` delivered, newest last.
    pub fn delivered_log(&self, n: usize) -> anyhow::Result<Vec<Delivery>> {
        self.read_delivered(n)
    }

    fn read_delivered(&self, n: usize) -> anyhow::Result<Vec<Delivery>> {
        let text = match std::fs::read_to_string(self.dir.join("delivered.jsonl")) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let all: Vec<Delivery> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Ok(all[all.len().saturating_sub(n)..].to_vec())
    }

    fn read_all<T: serde::de::DeserializeOwned>(&self, which: &str) -> anyhow::Result<Vec<T>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.dir.join(which))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                let text = std::fs::read(&path)?;
                out.push(
                    serde_json::from_slice(&text)
                        .with_context(|| format!("{} is damaged", path.display()))?,
                );
            }
        }
        Ok(out)
    }

    /// A session's file: named by its hash, whatever the session says.
    fn path(&self, which: &str, session: &str) -> PathBuf {
        let name = hex::encode(&sha256(session.as_bytes())[..16]);
        self.dir.join(which).join(format!("{name}.json"))
    }

    fn write<T: Serialize>(&self, which: &str, session: &str, value: &T) -> anyhow::Result<()> {
        use std::io::Write as _;
        let path = self.path(which, session);
        let tmp = path.with_extension("tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use glass_managed::billing::EventKind;

    use super::*;

    fn event(session: &str, at: Timestamp) -> BillingEvent {
        BillingEvent {
            kind: EventKind::CreditsPaid,
            org: "acme".into(),
            session: session.into(),
            cents: 5000,
            currency: "usd".into(),
            at,
        }
    }

    #[test]
    fn adr0040_a_session_is_queued_once_and_kept_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp::now().unix_seconds();
        let mut outbox = Outbox::open(dir.path(), Timing::default()).unwrap();
        assert!(
            outbox
                .queue(event("cs_1", Timestamp::now()), "https://a", now)
                .unwrap()
        );
        assert!(
            !outbox
                .queue(event("cs_1", Timestamp::now()), "https://a", now)
                .unwrap()
        );
        assert_eq!(outbox.due(now).unwrap().len(), 1);

        // A restart finds it, and still won't queue it twice.
        let mut outbox = Outbox::open(dir.path(), Timing::default()).unwrap();
        let due = outbox.due(now).unwrap();
        assert_eq!(due[0].event.session, "cs_1");
        assert!(
            !outbox
                .queue(event("cs_1", Timestamp::now()), "https://a", now)
                .unwrap()
        );

        // Delivered: gone from the queue, in the log, still not queued again.
        outbox.delivered(due[0].clone()).unwrap();
        assert!(outbox.due(now + 10).unwrap().is_empty());
        let outbox = Outbox::open(dir.path(), Timing::default()).unwrap();
        assert_eq!(outbox.delivered_log(5).unwrap()[0].attempts, 1);
        assert!(outbox.seen("cs_1"));
    }

    #[test]
    fn adr0040_a_failed_delivery_waits_longer_each_time_then_is_given_up() {
        let dir = tempfile::tempdir().unwrap();
        let timing = Timing {
            first: Duration::from_secs(5),
            longest: Duration::from_secs(60),
            give_up: Duration::from_secs(3600),
        };
        assert_eq!(timing.after(1).as_secs(), 5);
        assert_eq!(timing.after(2).as_secs(), 15);
        assert_eq!(timing.after(3).as_secs(), 45);
        assert_eq!(timing.after(9).as_secs(), 60);

        let at = Timestamp::now();
        let now = at.unix_seconds();
        let mut outbox = Outbox::open(dir.path(), timing).unwrap();
        outbox.queue(event("cs_2", at), "https://a", now).unwrap();
        let d = outbox.due(now).unwrap().remove(0);
        assert_eq!(outbox.failed(d, "HTTP 503", now).unwrap(), Some(now + 5));
        assert!(outbox.due(now).unwrap().is_empty(), "not due yet");
        let d = outbox.due(now + 5).unwrap().remove(0);
        assert_eq!(d.last_error.as_deref(), Some("HTTP 503"));
        assert_eq!(
            outbox.failed(d, "HTTP 503", now + 5).unwrap(),
            Some(now + 20)
        );

        // An hour on, it's dead; our operators can send it again.
        let d = outbox.due(now + 20).unwrap().remove(0);
        assert_eq!(outbox.failed(d, "HTTP 503", now + 3600).unwrap(), None);
        assert!(outbox.next_due().unwrap().is_none());
        assert_eq!(outbox.list("dead").unwrap().len(), 1);
        assert!(outbox.revive("cs_2", "https://a", now + 3700).unwrap());
        assert_eq!(outbox.due(now + 3700).unwrap().len(), 1);
        assert!(!outbox.revive("cs_2", "https://a", now + 3700).unwrap());
    }
}
