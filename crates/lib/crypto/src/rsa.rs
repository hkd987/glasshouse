//! RS256 checks (RSASSA-PKCS1-v1_5 with SHA-256) of what others sign: the
//! Bot Connector's tokens for the Teams relay (ADR 0065 §13). Not part of
//! the sealing suite: verifying only, with keys of 2048 bits or more.

use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::sign::Verifier;

/// The smallest RSA key accepted.
pub const MIN_RSA_BITS: usize = 2048;
/// The largest, so a key can't make checking slow.
const MAX_RSA_BITS: usize = 16384;

/// Why an RS256 signature wasn't accepted.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RsaError {
    #[error("not an RSA public key of {MIN_RSA_BITS} to {MAX_RSA_BITS} bits")]
    BadKey,
    #[error("the signature doesn't verify")]
    BadSignature,
}

/// Whether `signature` is an RS256 signature of `signed` by the RSA public
/// key with `modulus` and `exponent` (big-endian, as a JWK's `n` and `e`
/// decode to).
pub fn verify_rs256(
    modulus: &[u8],
    exponent: &[u8],
    signed: &[u8],
    signature: &[u8],
) -> Result<(), RsaError> {
    let n = modulus.strip_prefix(&[0]).unwrap_or(modulus);
    let bits = n.len() * 8 - n.first().map_or(0, |b| b.leading_zeros() as usize);
    let odd_exponent = exponent.last().is_some_and(|b| b & 1 == 1);
    if !(MIN_RSA_BITS..=MAX_RSA_BITS).contains(&bits) || !odd_exponent || exponent.len() > 8 {
        return Err(RsaError::BadKey);
    }
    let key = BigNum::from_slice(n)
        .and_then(|n| Ok((n, BigNum::from_slice(exponent)?)))
        .and_then(|(n, e)| Rsa::from_public_components(n, e))
        .and_then(PKey::from_rsa)
        .map_err(|_| RsaError::BadKey)?;
    let verified = Verifier::new(MessageDigest::sha256(), &key)
        .and_then(|mut v| v.verify_oneshot(signature, signed))
        .unwrap_or(false);
    if verified {
        Ok(())
    } else {
        Err(RsaError::BadSignature)
    }
}

/// An RSA key that signs RS256, standing in for a token issuer in tests
/// here and in other crates (`features = ["testing"]`).
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use openssl::hash::MessageDigest;
    use openssl::pkey::{PKey, Private};
    use openssl::rsa::Rsa;
    use openssl::sign::Signer;

    pub struct RsaTestKey(PKey<Private>);

    impl RsaTestKey {
        /// A new key of `bits`.
        pub fn generate(bits: u32) -> Self {
            let rsa = Rsa::generate(bits).expect("OpenSSL makes an RSA key");
            Self(PKey::from_rsa(rsa).expect("an RSA key is a key"))
        }

        /// The modulus, big-endian.
        pub fn modulus(&self) -> Vec<u8> {
            self.0.rsa().expect("an RSA key").n().to_vec()
        }

        /// The public exponent, big-endian.
        pub fn exponent(&self) -> Vec<u8> {
            self.0.rsa().expect("an RSA key").e().to_vec()
        }

        /// `data` signed RS256.
        pub fn sign_rs256(&self, data: &[u8]) -> Vec<u8> {
            Signer::new(MessageDigest::sha256(), &self.0)
                .and_then(|mut s| s.sign_oneshot_to_vec(data))
                .expect("OpenSSL signs")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::RsaTestKey;
    use super::*;

    #[test]
    fn adr0065_rs256_accepts_only_the_keys_own_signature() {
        let key = RsaTestKey::generate(2048);
        let (n, e) = (key.modulus(), key.exponent());
        let signed = b"eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJhcHAifQ";
        let signature = key.sign_rs256(signed);
        assert_eq!(verify_rs256(&n, &e, signed, &signature), Ok(()));
        // A leading zero byte on the modulus, as some encoders add, is the
        // same key.
        let padded = [&[0u8][..], &n].concat();
        assert_eq!(verify_rs256(&padded, &e, signed, &signature), Ok(()));

        // Changed text, a changed signature, another key's signature, or
        // nothing at all.
        let changed = b"eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJldmlsIn0";
        assert_eq!(
            verify_rs256(&n, &e, changed, &signature),
            Err(RsaError::BadSignature)
        );
        let mut flipped = signature.clone();
        flipped[10] ^= 1;
        assert_eq!(
            verify_rs256(&n, &e, signed, &flipped),
            Err(RsaError::BadSignature)
        );
        let other = RsaTestKey::generate(2048);
        assert_eq!(
            verify_rs256(&n, &e, signed, &other.sign_rs256(signed)),
            Err(RsaError::BadSignature)
        );
        assert_eq!(
            verify_rs256(&n, &e, signed, &[]),
            Err(RsaError::BadSignature)
        );
    }

    #[test]
    fn adr0065_rs256_refuses_keys_too_small_to_trust() {
        let small = RsaTestKey::generate(1024);
        let signed = b"header.payload";
        let signature = small.sign_rs256(signed);
        assert_eq!(
            verify_rs256(&small.modulus(), &small.exponent(), signed, &signature),
            Err(RsaError::BadKey)
        );
        let key = RsaTestKey::generate(2048);
        let n = key.modulus();
        assert_eq!(
            verify_rs256(&n, &[2], signed, &signature),
            Err(RsaError::BadKey),
            "an even exponent"
        );
        assert_eq!(
            verify_rs256(&n, &[], signed, &signature),
            Err(RsaError::BadKey)
        );
        assert_eq!(
            verify_rs256(&[], &[1, 0, 1], signed, &signature),
            Err(RsaError::BadKey)
        );
    }
}
