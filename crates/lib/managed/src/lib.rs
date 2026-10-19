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
//!
//! It does no network I/O: the console serves HTTP and glassd fetches
//! through `glass-gateway-providers`, the one crate that leaves the machine.

pub mod catalog;
pub mod signing;
pub mod store;
pub mod sync;

pub use catalog::{
    CatalogItem, CatalogRelease, Declared, InstallMode, ItemKind, Match, Provenance, Scope,
    StoreRefusal,
};
pub use signing::{DocKind, OrgKey, PublicKey, Refusal, Signed};
pub use store::ReleaseStore;
pub use sync::{
    DeviceReport, MAX_WAIT_SECS, PolicyRelease, Ring, RunningAgent, SyncRequest, SyncResponse,
};

/// SHA-256 of `bytes`, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}
