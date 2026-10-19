//! ADR 0030's sealing, checked against the CFRG's published HPKE vector
//! for exactly our suite, then the properties chat relies on.

#![allow(clippy::unwrap_used)]

use glass_crypto::{CryptoError, EpochKey, PublicKey, Sealed, SecretKey, open, seal, testing};
use serde_json::Value;

fn vector() -> Value {
    serde_json::from_str(include_str!("hpke-p256-sha256-aes256gcm.json")).unwrap()
}

fn bytes(v: &Value, key: &str) -> Vec<u8> {
    hex::decode(v[key].as_str().unwrap()).unwrap()
}

#[test]
fn hpke_opens_the_cfrg_vectors_for_p256_sha256_aes256gcm() {
    let v = vector();
    assert_eq!(
        (
            v["mode"].as_u64(),
            v["kem_id"].as_u64(),
            v["kdf_id"].as_u64(),
            v["aead_id"].as_u64()
        ),
        (Some(0), Some(16), Some(1), Some(2))
    );
    let sk_e = SecretKey::from_bytes(&bytes(&v, "skEm")).unwrap();
    let sk_r = SecretKey::from_bytes(&bytes(&v, "skRm")).unwrap();
    assert_eq!(
        sk_e.public().unwrap().to_bytes().to_vec(),
        bytes(&v, "pkEm")
    );
    let pk_r = sk_r.public().unwrap();
    assert_eq!(pk_r.to_bytes().to_vec(), bytes(&v, "pkRm"));

    let info = bytes(&v, "info");
    let first = &v["encryptions"][0];
    let (aad, pt, ct) = (bytes(first, "aad"), bytes(first, "pt"), bytes(first, "ct"));
    let sealed = testing::seal_with_ephemeral(&sk_e, &pk_r, &info, &aad, &pt).unwrap();
    assert_eq!(sealed.enc.to_vec(), bytes(&v, "enc"));
    assert_eq!(sealed.ct, ct, "the vector's ciphertext, byte for byte");
    assert_eq!(open(&sk_r, &sealed, &info, &aad).unwrap(), pt);
}

#[test]
fn a_changed_header_or_ciphertext_does_not_open() {
    let me = SecretKey::generate().unwrap();
    let sealed = seal(
        &me.public().unwrap(),
        b"glass-chat envelope v1",
        b"cv-1 epoch 4",
        b"the epoch key",
    )
    .unwrap();
    assert_eq!(
        open(&me, &sealed, b"glass-chat envelope v1", b"cv-1 epoch 4").unwrap(),
        b"the epoch key"
    );
    // Another context, another key, a flipped bit: each refused, alike.
    assert_eq!(
        open(&me, &sealed, b"glass-chat envelope v1", b"cv-1 epoch 5"),
        Err(CryptoError::Open)
    );
    assert_eq!(
        open(&me, &sealed, b"another use", b"cv-1 epoch 4"),
        Err(CryptoError::Open)
    );
    let stranger = SecretKey::generate().unwrap();
    assert_eq!(
        open(
            &stranger,
            &sealed,
            b"glass-chat envelope v1",
            b"cv-1 epoch 4"
        ),
        Err(CryptoError::Open)
    );
    let mut flipped = sealed.clone();
    flipped.ct[3] ^= 1;
    assert_eq!(
        open(&me, &flipped, b"glass-chat envelope v1", b"cv-1 epoch 4"),
        Err(CryptoError::Open)
    );
    let mut bad_enc: Sealed = sealed;
    bad_enc.enc[10] ^= 1;
    assert!(open(&me, &bad_enc, b"glass-chat envelope v1", b"cv-1 epoch 4").is_err());

    // Messages: the header is bound, the body sealed.
    let epoch = EpochKey::generate().unwrap();
    let key = epoch.message_key(b"cv-1|4|0192f3").unwrap();
    let (nonce, ct) = key.seal(b"{\"sender\":\"priya\"}", b"hello").unwrap();
    assert_eq!(
        key.open(&nonce, b"{\"sender\":\"priya\"}", &ct).unwrap(),
        b"hello"
    );
    assert!(
        key.open(&nonce, b"{\"sender\":\"sam\"}", &ct).is_err(),
        "a forged sender"
    );
    let other = epoch.message_key(b"cv-1|4|0192f4").unwrap();
    assert!(
        other.open(&nonce, b"{\"sender\":\"priya\"}", &ct).is_err(),
        "another message's key"
    );
}

#[test]
fn the_newest_epoch_key_opens_every_earlier_one() {
    let first = EpochKey::generate().unwrap();
    let second = EpochKey::generate().unwrap();
    let third = EpochKey::generate().unwrap();
    let to_first = second.seal_previous(&first, b"cv-1|2").unwrap();
    let to_second = third.seal_previous(&second, b"cv-1|3").unwrap();
    let back = third.open_previous(&to_second, b"cv-1|3").unwrap();
    assert_eq!(back, second);
    assert_eq!(back.open_previous(&to_first, b"cv-1|2").unwrap(), first);
    assert!(
        first.open_previous(&to_second, b"cv-1|3").is_err(),
        "an older key opens nothing newer"
    );
    assert!(
        third.open_previous(&to_second, b"cv-2|3").is_err(),
        "bound to its conversation"
    );
}

#[test]
fn a_signature_is_its_keys_over_its_message_only() {
    let alice = SecretKey::generate().unwrap();
    let sig = alice.sign(b"header nonce ciphertext").unwrap();
    let pk = alice.public().unwrap();
    assert!(pk.verify(b"header nonce ciphertext", &sig));
    assert!(!pk.verify(b"header nonce ciphertexT", &sig));
    assert!(
        !SecretKey::generate()
            .unwrap()
            .public()
            .unwrap()
            .verify(b"header nonce ciphertext", &sig)
    );
    assert!(!pk.verify(b"header nonce ciphertext", &sig[..63]));

    // Keys round-trip; anything off the curve or out of range is refused.
    let again = SecretKey::from_bytes(&alice.to_bytes().unwrap()).unwrap();
    assert_eq!(again.public().unwrap(), pk);
    assert_eq!(PublicKey::from_bytes(&pk.to_bytes()).unwrap(), pk);
    let mut off = pk.to_bytes();
    off[40] ^= 1;
    assert_eq!(PublicKey::from_bytes(&off), Err(CryptoError::BadKey));
    assert_eq!(
        SecretKey::from_bytes(&[0u8; 32]).unwrap_err(),
        CryptoError::BadKey
    );
    assert_eq!(
        SecretKey::from_bytes(&[0xffu8; 32]).unwrap_err(),
        CryptoError::BadKey
    );
}
