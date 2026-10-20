//! Managed settings: what an org's admin console sends its devices, and
//! what devices report back (ADR 0023; AD-1 … AD-5, DV-1, DV-3, DV-10,
//! DV-11).
//!
//! This crate is the contract between the console (`console/`, its own
//! workspace) and glassd, the way `glass-api` is between glassd and the
//! shell. It owns:
//!
//! - [`signing`]: the org's ed25519 key and the signed envelope every
//!   release travels in. The console signs; a device only verifies, against
//!   keys from its own config, and refuses anything not newer than what it
//!   has.
//! - [`catalog`]: the org's private catalog as devices receive it: items
//!   with their declared privileges (DV-3) and, per directory group, how
//!   each reaches its members (required, available or blocked, DV-11), and
//!   the search behind `catalog.search` (GA-2).
//! - [`sync`]: what a device asks for and gets, and what it reports for the
//!   fleet view (AD-4).
//! - [`store`]: the releases a device last applied, on its own disk, so it
//!   keeps working on them offline (ADR 0021).
//! - [`mail`]: the [`mail::MailLink`] the mail actions reach a person's
//!   mailbox through, whatever the provider (ADR 0037, IN-2).
//!
//! It does no network I/O: the console serves HTTP and glassd fetches
//! through `glass-gateway-providers`, the one crate that leaves the machine.

pub mod base64;
pub mod catalog;
pub mod chat;
pub mod device;
pub mod directory;
pub mod mail;
pub mod meet;
pub mod shared;
pub mod signing;
pub mod skills;
pub mod store;
pub mod studio;
pub mod sync;

pub use catalog::{
    CatalogItem, CatalogRelease, Declared, InstallMode, ItemKind, Match, Provenance, Scope,
    StoreRefusal,
};
pub use directory::{DirectoryHandle, DirectoryRelease, Person, Sender};
pub use signing::{DocKind, OrgKey, PublicKey, Refusal, Signed};
pub use store::ReleaseStore;
pub use sync::{
    ChatRelease, DeviceReport, MAX_WAIT_SECS, MadeApp, PolicyRelease, Ring, RunningAgent,
    SyncRequest, SyncResponse,
};

/// SHA-256 of `bytes`, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}
