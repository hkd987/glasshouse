//! The org's signing key and the envelope releases travel in (DV-1, DV-10,
//! EP-12).
//!
//! The console holds the org's ed25519 key; devices hold only its public
//! half, written into their config at enrollment, never taken from a
//! console response. A signature covers the document's kind, the org, a
//! version that only goes up, when it was issued, and the SHA-256 of the
//! payload, so:
//!
//! - a catalog release can't be replayed as a policy (or the reverse), nor
//!   one org's release at another org's devices;
//! - an old release can't be replayed to undo a newer one: a device refuses
//!   any version not above the one it has. Rolling back is publishing the
//!   earlier content again as a new version.
//!
//! Several keys may be trusted at once, so the org key can be rotated
//! without re-enrolling the fleet (EP-12).

use std::fmt;
use std::str::FromStr;

use ed25519_compact::{KeyPair, Seed, Signature};
use glass_types::Timestamp;
use serde::{Deserialize, Serialize};

use crate::sha256_hex;

/// The envelope format; part of what is signed.
pub const FORMAT: &str = "glasshouse-signed-v1";

/// What a release carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    /// Agent policy (`policy.toml`), AD-2.
    Policy,
    /// The org's private catalog, DV-10.
    Catalog,
    /// The org's people, for addressing them (AD-1).
    Directory,
    /// Chat's settings: IT's compliance key and retention (ADR 0030).
    Chat,
    /// A kernel patch and the modules built from it (ADR 0057).
    HardwarePatch,
}

impl DocKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Catalog => "catalog",
            Self::Directory => "directory",
            Self::Chat => "chat",
            Self::HardwarePatch => "hardware_patch",
        }
    }
}

impl fmt::Display for DocKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An org's signing key. Lives with the console only (DV-10); a device
/// never has one.
pub struct OrgKey {
    pair: KeyPair,
}

impl fmt::Debug for OrgKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never the secret half.
        f.debug_struct("OrgKey")
            .field("public", &self.public().to_string())
            .finish()
    }
}

impl OrgKey {
    /// The key for a 32-byte seed. Callers get the seed from the system's
    /// random source.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            pair: KeyPair::from_seed(Seed::new(seed)),
        }
    }

    /// Reads a seed written by [`OrgKey::seed_hex`].
    pub fn from_seed_hex(text: &str) -> Result<Self, String> {
        let bytes = hex::decode(text.trim()).map_err(|e| format!("not hex: {e}"))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "a seed is 32 bytes".to_owned())?;
        Ok(Self::from_seed(seed))
    }

    /// The seed, for the console's own key file (mode 0600).
    pub fn seed_hex(&self) -> String {
        hex::encode(self.pair.sk.seed().as_ref())
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.pair.pk)
    }

    /// Signs `payload` as release `version` of `kind` for `org`.
    pub fn sign(&self, kind: DocKind, org: &str, version: u64, payload: String) -> Signed {
        // Whole seconds, so the text form is the same after a round trip.
        let now = Timestamp::now();
        let issued_at = Timestamp::from_unix_seconds(now.unix_seconds()).unwrap_or(now);
        let mut signed = Signed {
            format: FORMAT.into(),
            kind,
            org: org.to_owned(),
            version,
            issued_at,
            key: self.public().id(),
            payload_sha256: sha256_hex(payload.as_bytes()),
            payload,
            signature: String::new(),
        };
        let signature = self.pair.sk.sign(signed.message(), None);
        signed.signature = hex::encode(signature.as_ref());
        signed
    }
}

/// An org's public key, written `ed25519:<64 hex digits>`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(ed25519_compact::PublicKey);

impl PublicKey {
    /// A short id for the key: the first 16 hex digits of its SHA-256.
    pub fn id(&self) -> String {
        let digest = sha256_hex(self.0.as_ref());
        format!("ed25519:{}", &digest[..16])
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ed25519:{}", hex::encode(self.0.as_ref()))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for PublicKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex_part = s
            .trim()
            .strip_prefix("ed25519:")
            .ok_or_else(|| "an org key starts with \"ed25519:\"".to_owned())?;
        let bytes = hex::decode(hex_part).map_err(|e| format!("org key: not hex: {e}"))?;
        ed25519_compact::PublicKey::from_slice(&bytes)
            .map(Self)
            .map_err(|_| "org key: not an ed25519 public key".to_owned())
    }
}

impl Serialize for PublicKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A signed release, as it travels and as a device keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signed {
    pub format: String,
    pub kind: DocKind,
    /// The org that issued it.
    pub org: String,
    /// Goes up by one with every release of this kind, rollbacks included.
    pub version: u64,
    pub issued_at: Timestamp,
    /// Which org key signed it ([`PublicKey::id`]).
    pub key: String,
    /// The document, as JSON text, exactly as signed.
    pub payload: String,
    pub payload_sha256: String,
    /// The ed25519 signature over [`Signed::message`], hex.
    pub signature: String,
}

/// Why a device refused a release. It keeps what it has.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("unknown release format {0:?}")]
    Format(String),
    #[error("expected a {expected} release, got a {got} release")]
    WrongKind { expected: DocKind, got: DocKind },
    #[error("the release is for org {got:?}, not {expected:?}")]
    WrongOrg { expected: String, got: String },
    #[error("signed with key {0}, which this device doesn't trust")]
    UnknownKey(String),
    #[error("the payload doesn't match its hash")]
    HashMismatch,
    #[error("the signature doesn't verify")]
    BadSignature,
    #[error("version {got} is not newer than version {have}, which is in force")]
    NotNewer { have: u64, got: u64 },
    #[error("the payload doesn't parse: {0}")]
    Payload(String),
}

impl Signed {
    /// The bytes the signature covers.
    fn message(&self) -> Vec<u8> {
        format!(
            "{FORMAT}\nkind: {}\norg: {}\nversion: {}\nissued_at: {}\npayload_sha256: {}\n",
            self.kind, self.org, self.version, self.issued_at, self.payload_sha256
        )
        .into_bytes()
    }

    /// Checks the release is a `kind` from `org`, signed by one of `keys`,
    /// and newer than `have`. Returns the payload. Nothing here trusts
    /// anything in the release itself.
    pub fn verify(
        &self,
        kind: DocKind,
        org: &str,
        keys: &[PublicKey],
        have: Option<u64>,
    ) -> Result<&str, Refusal> {
        if self.format != FORMAT {
            return Err(Refusal::Format(self.format.clone()));
        }
        if self.kind != kind {
            return Err(Refusal::WrongKind {
                expected: kind,
                got: self.kind,
            });
        }
        if self.org != org {
            return Err(Refusal::WrongOrg {
                expected: org.to_owned(),
                got: self.org.clone(),
            });
        }
        let key = keys
            .iter()
            .find(|k| k.id() == self.key)
            .ok_or_else(|| Refusal::UnknownKey(self.key.clone()))?;
        if sha256_hex(self.payload.as_bytes()) != self.payload_sha256 {
            return Err(Refusal::HashMismatch);
        }
        let signature = hex::decode(&self.signature)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .ok_or(Refusal::BadSignature)?;
        key.0
            .verify(self.message(), &signature)
            .map_err(|_| Refusal::BadSignature)?;
        if let Some(have) = have
            && self.version <= have
        {
            return Err(Refusal::NotNewer {
                have,
                got: self.version,
            });
        }
        Ok(&self.payload)
    }

    /// [`Signed::verify`], then the payload parsed as `T`.
    pub fn open<T: serde::de::DeserializeOwned>(
        &self,
        kind: DocKind,
        org: &str,
        keys: &[PublicKey],
        have: Option<u64>,
    ) -> Result<T, Refusal> {
        let payload = self.verify(kind, org, keys, have)?;
        serde_json::from_str(payload).map_err(|e| Refusal::Payload(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> OrgKey {
        OrgKey::from_seed([n; 32])
    }

    #[test]
    fn dv1_a_release_verifies_with_the_org_key_only() {
        let org = key(1);
        let signed = org.sign(DocKind::Policy, "acme", 3, "{\"a\":1}".into());
        let keys = [org.public()];
        assert_eq!(
            signed.verify(DocKind::Policy, "acme", &keys, Some(2)),
            Ok("{\"a\":1}")
        );
        let stranger = [key(2).public()];
        assert!(matches!(
            signed.verify(DocKind::Policy, "acme", &stranger, None),
            Err(Refusal::UnknownKey(_))
        ));
    }

    #[test]
    fn dv1_tampering_with_any_signed_field_is_refused() {
        let org = key(1);
        let keys = [org.public()];
        let signed = org.sign(DocKind::Policy, "acme", 3, "{\"a\":1}".into());

        let mut payload = signed.clone();
        payload.payload = "{\"a\":2}".into();
        assert_eq!(
            payload.verify(DocKind::Policy, "acme", &keys, None),
            Err(Refusal::HashMismatch)
        );
        payload.payload_sha256 = sha256_hex(payload.payload.as_bytes());
        assert_eq!(
            payload.verify(DocKind::Policy, "acme", &keys, None),
            Err(Refusal::BadSignature)
        );

        let mut version = signed.clone();
        version.version = 99;
        assert_eq!(
            version.verify(DocKind::Policy, "acme", &keys, None),
            Err(Refusal::BadSignature)
        );

        let mut garbled = signed;
        garbled.signature = "zz".into();
        assert_eq!(
            garbled.verify(DocKind::Policy, "acme", &keys, None),
            Err(Refusal::BadSignature)
        );
    }

    #[test]
    fn a_catalog_release_cant_pass_for_a_policy_or_another_orgs() {
        let org = key(1);
        let keys = [org.public()];
        let catalog = org.sign(DocKind::Catalog, "acme", 1, "{}".into());
        assert!(matches!(
            catalog.verify(DocKind::Policy, "acme", &keys, None),
            Err(Refusal::WrongKind { .. })
        ));
        assert!(matches!(
            catalog.verify(DocKind::Catalog, "other", &keys, None),
            Err(Refusal::WrongOrg { .. })
        ));
    }

    #[test]
    fn ad3_an_older_release_cant_be_replayed() {
        let org = key(1);
        let keys = [org.public()];
        let old = org.sign(DocKind::Policy, "acme", 4, "{}".into());
        assert_eq!(
            old.verify(DocKind::Policy, "acme", &keys, Some(4)),
            Err(Refusal::NotNewer { have: 4, got: 4 })
        );
        assert!(old.verify(DocKind::Policy, "acme", &keys, Some(3)).is_ok());
    }

    #[test]
    fn ep12_a_rotated_key_is_trusted_alongside_the_old_one() {
        let (old, new) = (key(1), key(2));
        let keys = [old.public(), new.public()];
        let signed = new.sign(DocKind::Catalog, "acme", 1, "{}".into());
        assert!(signed.verify(DocKind::Catalog, "acme", &keys, None).is_ok());
    }

    #[test]
    fn keys_round_trip_as_text_and_survive_a_json_round_trip() {
        let org = key(7);
        let text = org.public().to_string();
        assert!(
            text.starts_with("ed25519:") && text.len() == 8 + 64,
            "{text}"
        );
        assert_eq!(text.parse::<PublicKey>().unwrap(), org.public());
        assert!("ed25519:00".parse::<PublicKey>().is_err());
        assert!("rsa:00".parse::<PublicKey>().is_err());
        let again = OrgKey::from_seed_hex(&org.seed_hex()).unwrap();
        assert_eq!(again.public(), org.public());
        assert!(
            !format!("{org:?}").contains(&org.seed_hex()),
            "no secret in Debug"
        );

        let signed = org.sign(DocKind::Policy, "acme", 1, "{}".into());
        let json = serde_json::to_string(&signed).unwrap();
        let back: Signed = serde_json::from_str(&json).unwrap();
        assert!(
            back.verify(DocKind::Policy, "acme", &[org.public()], None)
                .is_ok()
        );
    }
}
