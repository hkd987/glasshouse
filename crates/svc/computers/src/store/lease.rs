//! One host at a time per computer (ADR 0066): a lease, kept in the store
//! beside the computer's versions.
//!
//! Hosts share nothing but the store, so the store is where they agree who
//! has a computer awake. A lease is a small object per **term**
//! (`computers/<computer>/lease/<term>.json`): which host, since when, and
//! when it last said it's still there. The newest term is the one that
//! counts.
//!
//! - **Taking it** is making the next term's object, and only if it isn't
//!   there ([`super::Blobs::put_new`], S3's `If-None-Match: *`). Of two
//!   hosts that ask at once, the store lets one through. Nothing here
//!   reads, decides and then writes over another host's object.
//! - **Keeping it** is the holder writing its own term's object again with
//!   the time ([`Store::renew`]), every [`RENEW_EVERY`]. Only the holder
//!   writes there.
//! - **Giving it up** is the holder marking its term released
//!   ([`Store::release`]), once the computer is asleep in the store.
//! - **Another host** leaves a lease alone until it's released, or its
//!   holder hasn't renewed it for [`LEASE_FOR`]: a host that died with the
//!   computer awake. Then it takes the next term.
//!
//! A holder trusts its own lease only for [`TRUST_FOR`] after the store
//! last took a renewal, which is well short of [`LEASE_FOR`]: past that it
//! takes a new term before it writes anything of the computer to the
//! store, in the same race as anyone else, and if it lost, it knows. The
//! gap between the two is what a slow request and two hosts' clocks may
//! use up; hosts keep time with the same service.

use std::time::Duration;

use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

use super::{Store, check_computer};

/// How long after its holder last renewed it a lease still holds: until
/// then no other host takes the computer. A host that died with a
/// computer awake keeps it from waking elsewhere for this long.
pub const LEASE_FOR: Duration = Duration::from_secs(120);

/// How often a holder renews.
pub const RENEW_EVERY: Duration = Duration::from_secs(30);

/// How long after a renewal the store took its holder acts on a lease
/// without asking again.
pub const TRUST_FOR: Duration = Duration::from_secs(75);

/// Who has a computer awake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// Which claim this is of the computer's: 1, 2, and so on.
    pub term: u64,
    /// The host's name for itself, the same for as long as it lives.
    pub host: String,
    /// When it took this term.
    pub since: Timestamp,
    /// When it last said it still has it.
    pub renewed: Timestamp,
    /// Given up: the computer is asleep in the store.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub released: bool,
}

impl Lease {
    /// Whether another host must leave the computer alone at `now`.
    pub fn holds(&self, now: Timestamp, lease_for: Duration) -> bool {
        let age = now
            .unix_seconds()
            .saturating_sub(self.renewed.unix_seconds());
        // Renewed after now, by this clock: it holds.
        !self.released && u64::try_from(age).map_or(true, |age| age < lease_for.as_secs())
    }
}

/// What asking for a computer's lease came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// It's this host's, as this lease.
    Held(Lease),
    /// Another host has it awake.
    Elsewhere(Lease),
}

fn lease_prefix(computer: &str) -> String {
    format!("computers/{computer}/lease/")
}

/// Terms' keys sort as their numbers do.
fn lease_key(computer: &str, term: u64) -> String {
    format!("{}{term:020}.json", lease_prefix(computer))
}

fn next_term(lease: &Lease) -> Result<u64, String> {
    lease
        .term
        .checked_add(1)
        .ok_or_else(|| "this computer has had every term there is".to_owned())
}

/// A host's name for itself: an instance id, a machine's name.
fn check_host(host: &str) -> Result<(), String> {
    let fits = (1..=64).contains(&host.len())
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if fits {
        Ok(())
    } else {
        Err(format!("{host:?} isn't a host's name"))
    }
}

impl Store {
    /// `computer`'s terms in the store, oldest first.
    fn terms(&self, computer: &str) -> Result<Vec<u64>, String> {
        let prefix = lease_prefix(computer);
        let mut terms: Vec<u64> = self
            .blobs
            .list(&prefix)?
            .into_iter()
            .filter_map(|listed| {
                let term: u64 = listed
                    .key
                    .strip_prefix(&prefix)?
                    .strip_suffix(".json")?
                    .parse()
                    .ok()?;
                (lease_key(computer, term) == listed.key).then_some(term)
            })
            .collect();
        terms.sort_unstable();
        Ok(terms)
    }

    /// `computer`'s lease as it is now: its newest term's, or `None` if no
    /// host ever took it.
    pub fn lease(&self, computer: &str) -> Result<Option<Lease>, String> {
        check_computer(computer)?;
        // The newest term is never removed, but a newer one can be made,
        // and this one removed, between the listing and the read.
        for _ in 0..3 {
            let Some(term) = self.terms(computer)?.pop() else {
                return Ok(None);
            };
            let key = lease_key(computer, term);
            let Some(bytes) = self.blobs.get(&key)? else {
                continue;
            };
            let lease: Lease = serde_json::from_slice(&bytes).map_err(|e| {
                format!("{key} can't be read ({e}): nobody takes this computer until it can")
            })?;
            if lease.term != term {
                return Err(format!(
                    "{key} says it's term {}: nobody takes this computer until that's put right",
                    lease.term
                ));
            }
            return Ok(Some(lease));
        }
        Err(format!(
            "the lease of {computer} kept changing while it was read"
        ))
    }

    /// Asks for `computer` for `host`, which doesn't have it awake: to
    /// wake it, or to change its record while it sleeps.
    ///
    /// It's `host`'s ([`Claim::Held`], a new term) if no other host has
    /// it: never taken, released, or not renewed for `lease_for`. A lease
    /// `host` itself left (its glassd stopped in the middle of a wake) is
    /// taken again as a new term. Otherwise [`Claim::Elsewhere`], with
    /// who has it.
    pub fn claim(
        &self,
        computer: &str,
        host: &str,
        now: Timestamp,
        lease_for: Duration,
    ) -> Result<Claim, String> {
        check_host(host)?;
        let next = match self.lease(computer)? {
            Some(lease) if lease.host != host && lease.holds(now, lease_for) => {
                return Ok(Claim::Elsewhere(lease));
            }
            Some(lease) => next_term(&lease)?,
            None => 1,
        };
        self.take(computer, host, next, now)
    }

    /// Asks for `computer` again for `host`, which has it awake and must
    /// be sure it's still its own: its glassd started again, or the store
    /// hasn't taken a renewal for longer than it trusts.
    ///
    /// It's `host`'s (a new term) only if the newest lease is its own and
    /// not given up, or there's none. Anything else means another host has
    /// had the computer since, whatever became of that host's lease
    /// (released, or run out): what `host` has is no longer the computer,
    /// and [`Claim::Elsewhere`] says so.
    pub fn reclaim(&self, computer: &str, host: &str, now: Timestamp) -> Result<Claim, String> {
        check_host(host)?;
        let next = match self.lease(computer)? {
            Some(lease) if lease.host == host && !lease.released => next_term(&lease)?,
            Some(lease) => return Ok(Claim::Elsewhere(lease)),
            None => 1,
        };
        self.take(computer, host, next, now)
    }

    /// Makes `term` `host`'s, if no host has made it.
    fn take(&self, computer: &str, host: &str, term: u64, now: Timestamp) -> Result<Claim, String> {
        let mine = Lease {
            term,
            host: host.to_owned(),
            since: now,
            renewed: now,
            released: false,
        };
        let bytes = serde_json::to_vec(&mine).map_err(|e| format!("a lease: {e}"))?;
        if self.blobs.put_new(&lease_key(computer, term), &bytes)? {
            // Earlier terms have nothing more to say. One left behind is
            // only a small object; the next claim removes it.
            if let Ok(terms) = self.terms(computer) {
                for earlier in terms.into_iter().filter(|earlier| *earlier < term) {
                    let _ = self.blobs.delete(&lease_key(computer, earlier));
                }
            }
            return Ok(Claim::Held(mine));
        }
        // Another asked at the same moment, and the store let it through.
        match self.lease(computer)? {
            // This host itself, asked twice at once (keeping its leases,
            // and a sleep): both have the one term.
            Some(lease) if lease.host == host && !lease.released => Ok(Claim::Held(lease)),
            Some(lease) => Ok(Claim::Elsewhere(lease)),
            None => Err(format!(
                "the lease of {computer} was taken and is gone again: ask once more"
            )),
        }
    }

    /// Says at `now` that `lease`'s host still has `computer`. `None` if
    /// it doesn't: another host took the next term, which it does only
    /// after this one went unrenewed for the whole of the lease.
    pub fn renew(
        &self,
        computer: &str,
        lease: &Lease,
        now: Timestamp,
    ) -> Result<Option<Lease>, String> {
        check_computer(computer)?;
        let renewed = Lease {
            renewed: now,
            released: false,
            ..lease.clone()
        };
        let bytes = serde_json::to_vec(&renewed).map_err(|e| format!("a lease: {e}"))?;
        // Written first, looked for after: a host that reads this term
        // from here on finds it renewed, and one that took the next term
        // before now is found here.
        self.blobs.put(&lease_key(computer, lease.term), &bytes)?;
        let taken = match lease.term.checked_add(1) {
            Some(next) => self.blobs.exists(&lease_key(computer, next))?,
            None => false,
        };
        Ok((!taken).then_some(renewed))
    }

    /// Gives `computer` up: `lease`'s host has put it to sleep in the
    /// store, and any host may take it.
    pub fn release(&self, computer: &str, lease: &Lease, now: Timestamp) -> Result<(), String> {
        check_computer(computer)?;
        let released = Lease {
            renewed: now,
            released: true,
            ..lease.clone()
        };
        let bytes = serde_json::to_vec(&released).map_err(|e| format!("a lease: {e}"))?;
        self.blobs.put(&lease_key(computer, lease.term), &bytes)
    }
}
