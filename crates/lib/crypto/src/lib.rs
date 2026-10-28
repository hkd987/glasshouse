//! Sealing for chat and meetings (ADR 0030), on the base system's OpenSSL
//! (ADR 0018) and nothing else.
//!
//! - [`seal`] and [`open`]: HPKE base mode (RFC 9180) with DHKEM(P-256,
//!   HKDF-SHA256), HKDF-SHA256 and AES-256-GCM, to give a key to one
//!   computer's member key or to IT's compliance key. Checked against the
//!   CFRG's published vector for exactly this suite.
//! - [`SecretKey::sign`] and [`PublicKey::verify`]: ECDSA P-256 over
//!   SHA-256, as 64 bytes `r ‖ s`, the form WebCrypto uses.
//! - [`MessageKey`]: one AES-256-GCM key per message, derived from the
//!   conversation's epoch key.
//! - [`EpochKey::seal_previous`]: each epoch key seals the one before it,
//!   so the newest opens the whole history.
//!
//! One suite only. Every key here is P-256, every hash SHA-256 and every
//! cipher AES-256-GCM, so the TypeScript copy for IT's browser has the
//! same and nothing to negotiate.
//!
//! [`rsa`] only checks others' signatures: Microsoft's RS256 tokens on
//! calls to the Teams relay (ADR 0065 §13). Nothing here signs with RSA.

pub mod rsa;

use openssl::bn::{BigNum, BigNumContext};
use openssl::derive::Deriver;
use openssl::ec::{EcGroup, EcKey, EcPoint, PointConversionForm};
use openssl::ecdsa::EcdsaSig;
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::{PKey, Private, Public};
use openssl::sign::Signer;
use openssl::symm::{Cipher, decrypt_aead, encrypt_aead};

/// Why sealing, opening or verifying didn't work. Opening says only that it
/// failed, never why, so a failure tells an attacker nothing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("not a P-256 key")]
    BadKey,
    #[error("couldn't open it: wrong key, or changed")]
    Open,
    #[error("the system's cryptography failed: {0}")]
    System(String),
}

impl From<openssl::error::ErrorStack> for CryptoError {
    fn from(e: openssl::error::ErrorStack) -> Self {
        Self::System(e.to_string())
    }
}

type Result<T> = std::result::Result<T, CryptoError>;

/// A public key's size: uncompressed SEC1, `04 ‖ x ‖ y`.
pub const PUBLIC_KEY_LEN: usize = 65;
/// An ECDSA P-256 signature: `r ‖ s`.
pub const SIGNATURE_LEN: usize = 64;
/// An AES-256-GCM tag, at the end of every ciphertext.
pub const TAG_LEN: usize = 16;
/// An AES-GCM nonce.
pub const NONCE_LEN: usize = 12;

fn group() -> Result<EcGroup> {
    Ok(EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?)
}

/// Random bytes from the system.
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0u8; N];
    openssl::rand::rand_bytes(&mut out)?;
    Ok(out)
}

/// SHA-256.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    openssl::sha::sha256(data)
}

/// Whether `a` and `b` are equal, in time that doesn't depend on where they
/// differ: for comparing MACs.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && openssl::memcmp::eq(a, b)
}

/// A P-256 private key. Never printed.
pub struct SecretKey {
    key: EcKey<Private>,
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

/// A P-256 public key, checked to be on the curve.
#[derive(Clone)]
pub struct PublicKey {
    key: EcKey<Public>,
    bytes: [u8; PUBLIC_KEY_LEN],
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey({:02x?}..)", &self.bytes[1..5])
    }
}

impl PartialEq for PublicKey {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for PublicKey {}

impl SecretKey {
    pub fn generate() -> Result<Self> {
        let group = group()?;
        Ok(Self {
            key: EcKey::generate(&group)?,
        })
    }

    /// From the 32-byte scalar, refusing zero and anything not below the
    /// curve's order.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(CryptoError::BadKey);
        }
        let group = group()?;
        let scalar = BigNum::from_slice(bytes)?;
        let mut ctx = BigNumContext::new()?;
        let mut order = BigNum::new()?;
        group.order(&mut order, &mut ctx)?;
        if scalar.num_bits() == 0 || scalar >= order {
            return Err(CryptoError::BadKey);
        }
        let mut point = EcPoint::new(&group)?;
        point.mul_generator2(&group, &scalar, &mut ctx)?;
        let key = EcKey::from_private_components(&group, &scalar, &point)?;
        key.check_key().map_err(|_| CryptoError::BadKey)?;
        Ok(Self { key })
    }

    /// The 32-byte scalar, for glassd's root-only key file.
    pub fn to_bytes(&self) -> Result<[u8; 32]> {
        let v = self.key.private_key().to_vec_padded(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        Ok(out)
    }

    pub fn public(&self) -> Result<PublicKey> {
        let group = group()?;
        let mut ctx = BigNumContext::new()?;
        let bytes =
            self.key
                .public_key()
                .to_bytes(&group, PointConversionForm::UNCOMPRESSED, &mut ctx)?;
        PublicKey::from_bytes(&bytes)
    }

    /// The shared secret with `peer`: the x coordinate of the product.
    fn agree(&self, peer: &PublicKey) -> Result<Vec<u8>> {
        let me = PKey::from_ec_key(self.key.clone())?;
        let them = PKey::from_ec_key(peer.key.clone())?;
        let mut deriver = Deriver::new(&me)?;
        deriver.set_peer(&them)?;
        Ok(deriver.derive_to_vec()?)
    }

    /// ECDSA over SHA-256 of `message`, as `r ‖ s`.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; SIGNATURE_LEN]> {
        let sig = EcdsaSig::sign(&sha256(message), &self.key)?;
        let mut out = [0u8; SIGNATURE_LEN];
        out[..32].copy_from_slice(&sig.r().to_vec_padded(32)?);
        out[32..].copy_from_slice(&sig.s().to_vec_padded(32)?);
        Ok(out)
    }
}

impl PublicKey {
    /// From uncompressed SEC1 bytes, refusing a point not on the curve.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != PUBLIC_KEY_LEN || bytes[0] != 4 {
            return Err(CryptoError::BadKey);
        }
        let group = group()?;
        let mut ctx = BigNumContext::new()?;
        let point =
            EcPoint::from_bytes(&group, bytes, &mut ctx).map_err(|_| CryptoError::BadKey)?;
        let key = EcKey::from_public_key(&group, &point).map_err(|_| CryptoError::BadKey)?;
        key.check_key().map_err(|_| CryptoError::BadKey)?;
        let mut out = [0u8; PUBLIC_KEY_LEN];
        out.copy_from_slice(bytes);
        Ok(Self { key, bytes: out })
    }

    pub fn to_bytes(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.bytes
    }

    /// From a PEM `PUBLIC KEY` (SubjectPublicKeyInfo), as AWS publishes its
    /// load balancers' keys: P-256 only, checked to be on the curve.
    pub fn from_pem(pem: &str) -> Result<Self> {
        let key = EcKey::public_key_from_pem(pem.as_bytes()).map_err(|_| CryptoError::BadKey)?;
        if key.group().curve_name() != Some(Nid::X9_62_PRIME256V1) {
            return Err(CryptoError::BadKey);
        }
        let group = group()?;
        let mut ctx = BigNumContext::new()?;
        let bytes = key
            .public_key()
            .to_bytes(&group, PointConversionForm::UNCOMPRESSED, &mut ctx)
            .map_err(|_| CryptoError::BadKey)?;
        Self::from_bytes(&bytes)
    }

    /// Whether `signature` is this key's over `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        if signature.len() != SIGNATURE_LEN {
            return false;
        }
        let (Ok(r), Ok(s)) = (
            BigNum::from_slice(&signature[..32]),
            BigNum::from_slice(&signature[32..]),
        ) else {
            return false;
        };
        EcdsaSig::from_private_components(r, s)
            .and_then(|sig| sig.verify(&sha256(message), &self.key))
            .unwrap_or(false)
    }
}

// ---- HKDF-SHA256 (RFC 5869), on OpenSSL's HMAC --------------------------

/// HMAC-SHA256 of `parts`, one after another. The model gateway seals the
/// blocks it hands agents with it (ADR 0043).
pub fn hmac(key: &[u8], parts: &[&[u8]]) -> Result<[u8; 32]> {
    // An empty key is a block of zeros to HMAC; OpenSSL wants it spelled.
    let zeros = [0u8; 32];
    let key = PKey::hmac(if key.is_empty() { &zeros } else { key })?;
    let mut signer = Signer::new(MessageDigest::sha256(), &key)?;
    for part in parts {
        signer.update(part)?;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&signer.sign_to_vec()?);
    Ok(out)
}

fn hkdf_extract(salt: &[u8], ikm: &[&[u8]]) -> Result<[u8; 32]> {
    hmac(salt, ikm)
}

fn hkdf_expand(prk: &[u8], info: &[&[u8]], len: usize) -> Result<Vec<u8>> {
    debug_assert!(len <= 255 * 32);
    let mut out = Vec::with_capacity(len);
    let mut previous: Vec<u8> = Vec::new();
    let mut counter = 1u8;
    while out.len() < len {
        let mut parts: Vec<&[u8]> = vec![&previous];
        parts.extend_from_slice(info);
        let c = [counter];
        parts.push(&c);
        let block = hmac(prk, &parts)?;
        out.extend_from_slice(&block);
        previous = block.to_vec();
        counter += 1;
    }
    out.truncate(len);
    Ok(out)
}

/// HKDF-SHA256 with an empty salt: `len` bytes of key from `ikm` for
/// `info`.
pub fn hkdf(ikm: &[u8], info: &[u8], len: usize) -> Result<Vec<u8>> {
    let prk = hkdf_extract(&[], &[ikm])?;
    hkdf_expand(&prk, &[info], len)
}

// ---- HPKE base mode (RFC 9180) ------------------------------------------

const KEM_ID: u16 = 0x0010;
const KDF_ID: u16 = 0x0001;
const AEAD_ID: u16 = 0x0002;
const MODE_BASE: u8 = 0;

fn kem_suite() -> [u8; 5] {
    let [a, b] = KEM_ID.to_be_bytes();
    [b'K', b'E', b'M', a, b]
}

fn hpke_suite() -> [u8; 10] {
    let [k1, k2] = KEM_ID.to_be_bytes();
    let [d1, d2] = KDF_ID.to_be_bytes();
    let [a1, a2] = AEAD_ID.to_be_bytes();
    [b'H', b'P', b'K', b'E', k1, k2, d1, d2, a1, a2]
}

fn labeled_extract(suite: &[u8], salt: &[u8], label: &[u8], ikm: &[u8]) -> Result<[u8; 32]> {
    hkdf_extract(salt, &[b"HPKE-v1", suite, label, ikm])
}

fn labeled_expand(
    suite: &[u8],
    prk: &[u8],
    label: &[u8],
    info: &[u8],
    len: u16,
) -> Result<Vec<u8>> {
    let l = len.to_be_bytes();
    hkdf_expand(prk, &[&l, b"HPKE-v1", suite, label, info], usize::from(len))
}

/// DHKEM's shared secret from a Diffie-Hellman output and `enc ‖ pkR`.
fn extract_and_expand(dh: &[u8], kem_context: &[u8]) -> Result<Vec<u8>> {
    let suite = kem_suite();
    let prk = labeled_extract(&suite, &[], b"eae_prk", dh)?;
    labeled_expand(&suite, &prk, b"shared_secret", kem_context, 32)
}

/// HPKE's key and base nonce for one seal, from the KEM's shared secret.
struct Schedule {
    key: Vec<u8>,
    base_nonce: Vec<u8>,
}

fn key_schedule(shared_secret: &[u8], info: &[u8]) -> Result<Schedule> {
    let suite = hpke_suite();
    let psk_id_hash = labeled_extract(&suite, &[], b"psk_id_hash", &[])?;
    let info_hash = labeled_extract(&suite, &[], b"info_hash", info)?;
    let mut context = vec![MODE_BASE];
    context.extend_from_slice(&psk_id_hash);
    context.extend_from_slice(&info_hash);
    let secret = labeled_extract(&suite, shared_secret, b"secret", &[])?;
    Ok(Schedule {
        key: labeled_expand(&suite, &secret, b"key", &context, 32)?,
        base_nonce: labeled_expand(&suite, &secret, b"base_nonce", &context, 12)?,
    })
}

/// Something sealed to a public key: the sender's one-time public key and
/// the ciphertext (with its tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub enc: [u8; PUBLIC_KEY_LEN],
    pub ct: Vec<u8>,
}

fn aead_seal(key: &[u8], nonce: &[u8], aad: &[u8], pt: &[u8]) -> Result<Vec<u8>> {
    let mut tag = [0u8; TAG_LEN];
    let mut ct = encrypt_aead(Cipher::aes_256_gcm(), key, Some(nonce), aad, pt, &mut tag)?;
    ct.extend_from_slice(&tag);
    Ok(ct)
}

fn aead_open(key: &[u8], nonce: &[u8], aad: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
    if ct.len() < TAG_LEN {
        return Err(CryptoError::Open);
    }
    let (body, tag) = ct.split_at(ct.len() - TAG_LEN);
    decrypt_aead(Cipher::aes_256_gcm(), key, Some(nonce), aad, body, tag)
        .map_err(|_| CryptoError::Open)
}

fn seal_with(
    ephemeral: &SecretKey,
    to: &PublicKey,
    info: &[u8],
    aad: &[u8],
    pt: &[u8],
) -> Result<Sealed> {
    let enc = ephemeral.public()?.to_bytes();
    let dh = ephemeral.agree(to)?;
    let mut kem_context = enc.to_vec();
    kem_context.extend_from_slice(&to.to_bytes());
    let shared = extract_and_expand(&dh, &kem_context)?;
    let schedule = key_schedule(&shared, info)?;
    // One message per seal: sequence 0, so the nonce is the base nonce.
    let ct = aead_seal(&schedule.key, &schedule.base_nonce, aad, pt)?;
    Ok(Sealed { enc, ct })
}

/// Seals `pt` to `to`: only its secret key opens it. `info` names what it
/// is for; `aad` is bound to it unencrypted.
pub fn seal(to: &PublicKey, info: &[u8], aad: &[u8], pt: &[u8]) -> Result<Sealed> {
    seal_with(&SecretKey::generate()?, to, info, aad, pt)
}

/// Opens what [`seal`] sealed to `me`'s public key, for the same `info`
/// and `aad`.
pub fn open(me: &SecretKey, sealed: &Sealed, info: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let sender = PublicKey::from_bytes(&sealed.enc).map_err(|_| CryptoError::Open)?;
    let dh = me.agree(&sender)?;
    let mut kem_context = sealed.enc.to_vec();
    kem_context.extend_from_slice(&me.public()?.to_bytes());
    let shared = extract_and_expand(&dh, &kem_context)?;
    let schedule = key_schedule(&shared, info)?;
    aead_open(&schedule.key, &schedule.base_nonce, aad, &sealed.ct)
}

// ---- Chat's keys (ADR 0030, decision 1) ----------------------------------

/// A conversation's key for one epoch: 32 random bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct EpochKey([u8; 32]);

impl std::fmt::Debug for EpochKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EpochKey(..)")
    }
}

/// What a previous epoch key is sealed under.
const PREVIOUS_INFO: &[u8] = b"glass-chat epoch v1";
/// What a message key is derived for.
const MESSAGE_INFO: &[u8] = b"glass-chat msg v1";

impl EpochKey {
    pub fn generate() -> Result<Self> {
        Ok(Self(random()?))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let key: [u8; 32] = bytes.try_into().map_err(|_| CryptoError::BadKey)?;
        Ok(Self(key))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn aead_key(&self, info: &[u8], context: &[u8]) -> Result<Vec<u8>> {
        let mut full = info.to_vec();
        full.extend_from_slice(context);
        hkdf(&self.0, &full, 32)
    }

    /// Seals `previous` under this key, for the epoch record `context`
    /// names (conversation and epoch number): the newest key opens every
    /// earlier one.
    pub fn seal_previous(&self, previous: &EpochKey, context: &[u8]) -> Result<Vec<u8>> {
        let key = self.aead_key(PREVIOUS_INFO, context)?;
        let nonce: [u8; NONCE_LEN] = random()?;
        let mut out = nonce.to_vec();
        out.extend(aead_seal(&key, &nonce, context, &previous.0)?);
        Ok(out)
    }

    /// Opens what [`EpochKey::seal_previous`] sealed for `context`.
    pub fn open_previous(&self, sealed: &[u8], context: &[u8]) -> Result<EpochKey> {
        if sealed.len() < NONCE_LEN {
            return Err(CryptoError::Open);
        }
        let key = self.aead_key(PREVIOUS_INFO, context)?;
        let (nonce, ct) = sealed.split_at(NONCE_LEN);
        EpochKey::from_bytes(&aead_open(&key, nonce, context, ct)?).map_err(|_| CryptoError::Open)
    }

    /// The key for one message, named by `context` (conversation, epoch
    /// and message id).
    pub fn message_key(&self, context: &[u8]) -> Result<MessageKey> {
        let key = self.aead_key(MESSAGE_INFO, context)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(&key);
        Ok(MessageKey(out))
    }
}

/// One message's AES-256-GCM key.
pub struct MessageKey([u8; 32]);

impl MessageKey {
    /// `(nonce, ciphertext with tag)`, with `header` bound unencrypted.
    pub fn seal(&self, header: &[u8], body: &[u8]) -> Result<([u8; NONCE_LEN], Vec<u8>)> {
        let nonce: [u8; NONCE_LEN] = random()?;
        Ok((nonce, aead_seal(&self.0, &nonce, header, body)?))
    }

    pub fn open(&self, nonce: &[u8], header: &[u8], ct: &[u8]) -> Result<Vec<u8>> {
        if nonce.len() != NONCE_LEN {
            return Err(CryptoError::Open);
        }
        aead_open(&self.0, nonce, header, ct)
    }
}

#[doc(hidden)]
pub mod testing {
    //! For the vector tests only: HPKE with a chosen one-time key.
    use super::*;

    pub fn seal_with_ephemeral(
        ephemeral: &SecretKey,
        to: &PublicKey,
        info: &[u8],
        aad: &[u8],
        pt: &[u8],
    ) -> Result<Sealed> {
        seal_with(ephemeral, to, info, aad, pt)
    }
}

#[cfg(test)]
mod pem_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn pem_of(public: &PublicKey) -> String {
        String::from_utf8(public.key.public_key_to_pem().unwrap()).unwrap()
    }

    #[test]
    fn adr0067_a_load_balancers_pem_key_verifies_its_signatures() {
        let secret = SecretKey::generate().unwrap();
        let public = secret.public().unwrap();
        let read = PublicKey::from_pem(&pem_of(&public)).unwrap();
        assert_eq!(read, public);
        let signature = secret.sign(b"header.claims").unwrap();
        assert!(read.verify(b"header.claims", &signature));
        assert!(!read.verify(b"header.other", &signature));
    }

    #[test]
    fn adr0067_only_a_p256_public_key_is_taken() {
        let p384 = EcKey::generate(&EcGroup::from_curve_name(Nid::SECP384R1).unwrap()).unwrap();
        let pem = String::from_utf8(p384.public_key_to_pem().unwrap()).unwrap();
        assert!(PublicKey::from_pem(&pem).is_err());
        assert!(PublicKey::from_pem("not a key").is_err());
        assert!(PublicKey::from_pem("").is_err());
    }
}
