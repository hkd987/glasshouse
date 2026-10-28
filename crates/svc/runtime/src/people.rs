//! Who a person is on a shared agent host (ADR 0067): the people who
//! signed in, each with an account of their own on this host, and the
//! proofs glassd checks before it believes the front acts for one.
//!
//! - [`People`]: the registry. A person's key is their sign-in subject
//!   (`sub`), stable across hosts; their uid is this host's, from a
//!   reserved range, with no password and no login. Tokens for `glass` and
//!   one-time login codes are kept as hashes only.
//! - [`verify_load_balancer`]: the load balancer's signed assertion
//!   (`x-amzn-oidc-data`): ES256 by AWS's key for its `kid`, from this
//!   stack's load balancer, unexpired.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use glass_types::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long a `glass` token lasts.
pub const TOKEN_DAYS: i64 = 30;
/// How long a login code lasts: the browser hands it to `glass` at once.
pub const CODE_SECS: i64 = 120;
/// Clock skew allowed on an assertion's expiry.
const SKEW_SECS: i64 = 60;
/// A sign-in subject's longest.
const SUB_MAX: usize = 255;
/// People one host keeps: its uid range's size.
const PEOPLE_MAX: usize = 100_000;

/// Who signed in, as their proof says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
}

/// A person known to this host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// Their sign-in subject.
    pub key: String,
    pub name: String,
    pub email: String,
    /// Theirs on this host only.
    pub uid: u32,
    pub first_seen: Timestamp,
}

impl Person {
    /// What policy and the record call them: their email, or their
    /// subject when the sign-in gave none.
    pub fn login(&self) -> String {
        if self.email.is_empty() {
            format!("sub:{}", self.key)
        } else {
            self.email.to_lowercase()
        }
    }

    /// Their key in the store (ADR 0068): the same on every host, letters
    /// and digits only.
    pub fn owner_key(&self) -> String {
        owner_key(&self.key)
    }
}

/// A person's key in the store, from their sign-in subject: 96 bits of
/// its SHA-256, so it fits the store's names and says nothing of them.
pub fn owner_key(sub: &str) -> String {
    let digest = Sha256::digest(sub.as_bytes());
    format!("p{}", hex::encode(&digest[..12]))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    people: Vec<Person>,
    /// `glass` tokens: their SHA-256, whose (key), until when.
    #[serde(default)]
    tokens: BTreeMap<String, (String, i64)>,
    /// Login codes: their SHA-256, whose, until when. Each works once.
    #[serde(default)]
    codes: BTreeMap<String, (String, i64)>,
}

#[derive(Debug)]
pub struct People {
    path: PathBuf,
    /// The first uid people get.
    base: u32,
    kept: Mutex<Kept>,
}

fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

fn random_hex() -> Result<String, String> {
    glass_crypto::random::<32>()
        .map(hex::encode)
        .map_err(|e| format!("no randomness: {e}"))
}

impl People {
    /// The registry kept at `path`; people's uids start at `base`.
    pub fn open(path: PathBuf, base: u32) -> Self {
        let kept = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            base,
            kept: Mutex::new(kept),
        }
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, kept: &Kept) -> Result<(), String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let making = self.path.with_extension("json.new");
        let text = serde_json::to_vec_pretty(kept).map_err(|e| e.to_string())?;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&making)
            .and_then(|mut f| f.write_all(&text))
            .and_then(|()| std::fs::rename(&making, &self.path))
            .map_err(|e| format!("keeping who signed in: {e}"))
    }

    /// The person these claims are, made the first time: a new uid.
    pub fn person(&self, claims: &Claims) -> Result<Person, String> {
        let sub = claims.sub.trim();
        if sub.is_empty() || sub.len() > SUB_MAX || sub.chars().any(char::is_control) {
            return Err("the sign-in has no usable subject".into());
        }
        let clean = |text: &str| glass_api::display_line(text.trim()).into_owned();
        let mut kept = self.kept();
        if let Some(found) = kept.people.iter_mut().find(|p| p.key == sub) {
            // Names and emails change; the subject doesn't.
            let (email, name) = (clean(&claims.email), clean(&claims.name));
            let changed = (!email.is_empty() && found.email != email)
                || (!name.is_empty() && found.name != name);
            if !email.is_empty() {
                found.email = email;
            }
            if !name.is_empty() {
                found.name = name;
            }
            let found = found.clone();
            if changed {
                self.save(&kept)?;
            }
            return Ok(found);
        }
        if kept.people.len() >= PEOPLE_MAX {
            return Err("no uids left for people on this host".into());
        }
        let uid = self
            .base
            .checked_add(u32::try_from(kept.people.len()).map_err(|e| e.to_string())?)
            .ok_or("no uids left for people on this host")?;
        let email = clean(&claims.email);
        let name = match clean(&claims.name) {
            n if n.is_empty() => email.clone(),
            n => n,
        };
        let person = Person {
            key: sub.to_owned(),
            name,
            email,
            uid,
            first_seen: Timestamp::now(),
        };
        kept.people.push(person.clone());
        self.save(&kept)?;
        Ok(person)
    }

    pub fn by_uid(&self, uid: u32) -> Option<Person> {
        self.kept().people.iter().find(|p| p.uid == uid).cloned()
    }

    /// A token for `glass`, for a person signed in now; only its hash is
    /// kept.
    pub fn issue_token(&self, person: &Person, now: i64) -> Result<(String, i64), String> {
        let token = format!("{}{}", glass_api::TOKEN_PREFIX, random_hex()?);
        let until = now + TOKEN_DAYS * 86_400;
        let mut kept = self.kept();
        kept.tokens.retain(|_, (_, until)| *until > now);
        kept.tokens
            .insert(hash(&token), (person.key.clone(), until));
        self.save(&kept)?;
        Ok((token, until))
    }

    /// Whose token this is, while it lasts.
    pub fn by_token(&self, token: &str, now: i64) -> Option<Person> {
        if !token.starts_with(glass_api::TOKEN_PREFIX) {
            return None;
        }
        let kept = self.kept();
        let (key, until) = kept.tokens.get(&hash(token))?;
        if *until <= now {
            return None;
        }
        kept.people.iter().find(|p| &p.key == key).cloned()
    }

    /// Revokes every token of a person's: `glass` on every computer of
    /// theirs signs in again.
    pub fn revoke_tokens(&self, person: &Person) -> Result<usize, String> {
        let mut kept = self.kept();
        let before = kept.tokens.len();
        kept.tokens.retain(|_, (key, _)| key != &person.key);
        let gone = before - kept.tokens.len();
        self.save(&kept)?;
        Ok(gone)
    }

    /// A one-time login code for `glass login`; only its hash is kept.
    pub fn issue_code(&self, person: &Person, now: i64) -> Result<(String, i64), String> {
        let code = format!("{}{}", glass_api::CODE_PREFIX, random_hex()?);
        let until = now + CODE_SECS;
        let mut kept = self.kept();
        kept.codes.retain(|_, (_, until)| *until > now);
        kept.codes.insert(hash(&code), (person.key.clone(), until));
        self.save(&kept)?;
        Ok((code, until))
    }

    /// Whose code this was: it works once, and not after it expires.
    pub fn take_code(&self, code: &str, now: i64) -> Result<Person, String> {
        let mut kept = self.kept();
        let taken = kept.codes.remove(&hash(code.trim()));
        self.save(&kept)?;
        let (key, until) =
            taken.ok_or("that sign-in code isn't one this host gave, or it was used")?;
        if until <= now {
            return Err("that sign-in code is out of date: run glass login again".into());
        }
        kept.people
            .iter()
            .find(|p| p.key == key)
            .cloned()
            .ok_or_else(|| "whoever that code was for isn't known here any more".into())
    }
}

/// Base64url, with or without the padding the load balancer adds.
fn b64url(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return Err("not base64".into()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// The load balancer's assertion of who signed in (`x-amzn-oidc-data`).
/// `key` gives AWS's public key for a key id.
pub fn verify_load_balancer(
    token: &str,
    signer: &str,
    key: &dyn Fn(&str) -> Result<glass_crypto::PublicKey, String>,
    now: i64,
) -> Result<Claims, String> {
    let mut parts = token.trim().split('.');
    let (Some(head), Some(body), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("not a signed assertion".into());
    };
    let header: serde_json::Value =
        serde_json::from_slice(&b64url(head)?).map_err(|_| "its header isn't JSON")?;
    if header["alg"] != "ES256" {
        return Err("not signed ES256".into());
    }
    if signer.is_empty() || header["signer"] != signer {
        return Err("not signed by this stack's load balancer".into());
    }
    let exp = match &header["exp"] {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .ok_or("it has no expiry")?;
    if exp + SKEW_SECS < now {
        return Err("it has expired".into());
    }
    let kid = header["kid"].as_str().unwrap_or_default();
    let fits = !kid.is_empty()
        && kid.len() <= 64
        && kid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !fits {
        return Err("its key id isn't one AWS gives".into());
    }
    let public = key(kid)?;
    let signature = b64url(sig)?;
    // The signed bytes are the header and body as sent, padding and all.
    let signed = format!("{head}.{body}");
    if !public.verify(signed.as_bytes(), &signature) {
        return Err("its signature doesn't verify".into());
    }
    let claims: Claims = serde_json::from_slice(&b64url(body)?)
        .map_err(|_| "its claims aren't what a sign-in gives")?;
    if claims.sub.trim().is_empty() {
        return Err("it names nobody".into());
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const SIGNER: &str = "arn:aws:elasticloadbalancing:us-east-1:123456789012:loadbalancer/app/agents/0123456789abcdef";

    fn b64(bytes: &[u8]) -> String {
        // Base64url with padding, as the load balancer writes it.
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.len();
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= n {
                    out.push(T[((v >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn assertion(
        key: &glass_crypto::SecretKey,
        header: serde_json::Value,
        claims: serde_json::Value,
    ) -> String {
        let signed = format!(
            "{}.{}",
            b64(header.to_string().as_bytes()),
            b64(claims.to_string().as_bytes())
        );
        format!("{signed}.{}", b64(&key.sign(signed.as_bytes()).unwrap()))
    }

    fn header(exp: i64) -> serde_json::Value {
        serde_json::json!({"typ": "JWT", "kid": "8f4c-01", "alg": "ES256", "signer": SIGNER, "exp": exp})
    }

    fn dana() -> serde_json::Value {
        serde_json::json!({"sub": "sub-dana", "email": "Dana@Example.com", "name": "Dana Alvarez"})
    }

    #[test]
    fn adr0067_the_load_balancers_assertion_names_who_signed_in() {
        let key = glass_crypto::SecretKey::generate().unwrap();
        let public = key.public().unwrap();
        let keys = |kid: &str| {
            assert_eq!(kid, "8f4c-01");
            Ok(public.clone())
        };
        let now = 1_800_000_000;
        let token = assertion(&key, header(now + 120), dana());
        assert!(token.contains('='), "padded, as AWS sends it");
        let claims = verify_load_balancer(&token, SIGNER, &keys, now).unwrap();
        assert_eq!(claims.sub, "sub-dana");
        assert_eq!(claims.name, "Dana Alvarez");
    }

    #[test]
    fn adr0067_forged_expired_and_foreign_assertions_are_refused() {
        let key = glass_crypto::SecretKey::generate().unwrap();
        let public = key.public().unwrap();
        let keys = |_: &str| Ok(public.clone());
        let now = 1_800_000_000;
        let good = assertion(&key, header(now + 120), dana());
        let refused = |token: &str, signer: &str| {
            verify_load_balancer(token, signer, &keys, now).unwrap_err()
        };

        // Expired, beyond a minute's skew.
        assert!(refused(&assertion(&key, header(now - 61), dana()), SIGNER).contains("expired"));
        // Another load balancer's, or none named.
        let other = "arn:aws:elasticloadbalancing:us-east-1:999999999999:loadbalancer/app/x/1";
        assert!(refused(&good, other).contains("this stack's"));
        assert!(refused(&good, "").contains("this stack's"));
        // Signed by another key.
        let stranger = glass_crypto::SecretKey::generate().unwrap();
        assert!(
            refused(&assertion(&stranger, header(now + 120), dana()), SIGNER).contains("verify")
        );
        // Claims changed after signing: someone else's subject.
        let parts: Vec<&str> = good.split('.').collect();
        let swapped = format!(
            "{}.{}.{}",
            parts[0],
            b64(serde_json::json!({"sub": "sub-sam"}).to_string().as_bytes()),
            parts[2]
        );
        assert!(refused(&swapped, SIGNER).contains("verify"));
        // alg none, HS256 and no signature at all.
        for alg in ["none", "HS256", "RS256"] {
            let mut h = header(now + 120);
            h["alg"] = alg.into();
            assert!(
                refused(&assertion(&key, h, dana()), SIGNER).contains("ES256"),
                "{alg}"
            );
        }
        assert!(refused(&format!("{}.{}.", parts[0], parts[1]), SIGNER).contains("verify"));
        assert!(refused("not.an", SIGNER).contains("not a signed assertion"));
        assert!(refused("a.b.c.d", SIGNER).contains("not a signed assertion"));
        // A key id that could be a path.
        let mut h = header(now + 120);
        h["kid"] = "../../etc/passwd".into();
        assert!(refused(&assertion(&key, h, dana()), SIGNER).contains("key id"));
        // Nobody named.
        let nobody = serde_json::json!({"sub": "  ", "email": "x@example.com"});
        assert!(refused(&assertion(&key, header(now + 120), nobody), SIGNER).contains("nobody"));
    }

    #[test]
    fn adr0067_each_person_gets_a_uid_and_tokens_and_codes_work_as_they_should() {
        let dir = tempfile::tempdir().unwrap();
        let people = People::open(dir.path().join("people.json"), 200_000);
        let claims = |sub: &str, email: &str| Claims {
            sub: sub.into(),
            email: email.into(),
            name: String::new(),
        };
        let dana = people
            .person(&claims("sub-dana", "Dana@Example.com"))
            .unwrap();
        let sam = people
            .person(&claims("sub-sam", "sam@example.com"))
            .unwrap();
        assert_eq!((dana.uid, sam.uid), (200_000, 200_001));
        assert_eq!(dana.login(), "dana@example.com");
        assert_ne!(dana.owner_key(), sam.owner_key());
        assert!(dana.owner_key().bytes().all(|b| b.is_ascii_alphanumeric()));
        // The same subject is the same person, whatever their email says.
        let again = people
            .person(&claims("sub-dana", "dana.alvarez@example.com"))
            .unwrap();
        assert_eq!(again.uid, dana.uid);
        assert!(people.person(&claims("", "x@example.com")).is_err());
        assert!(people.person(&claims("a\nb", "x@example.com")).is_err());

        let now = 1_800_000_000;
        let (token, until) = people.issue_token(&dana, now).unwrap();
        assert!(token.starts_with("gat_"));
        assert_eq!(people.by_token(&token, now).unwrap().key, "sub-dana");
        assert!(people.by_token(&token, until).is_none(), "expired");
        assert!(people.by_token("gat_wrong", now).is_none());
        assert!(
            people
                .by_token(&token.replacen("gat_", "gac_", 1), now)
                .is_none()
        );
        // Only hashes are kept.
        let file = std::fs::read_to_string(dir.path().join("people.json")).unwrap();
        assert!(!file.contains(&token[4..]));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("people.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // Revoked, every token of theirs goes; Sam's stays.
        let (sams, _) = people.issue_token(&sam, now).unwrap();
        assert_eq!(people.revoke_tokens(&dana).unwrap(), 1);
        assert!(people.by_token(&token, now).is_none());
        assert!(people.by_token(&sams, now).is_some());

        // A login code works once, and not late.
        let (code, until) = people.issue_code(&sam, now).unwrap();
        assert!(code.starts_with("gac_"));
        assert_eq!(people.take_code(&code, now).unwrap().key, "sub-sam");
        assert!(
            people
                .take_code(&code, now)
                .unwrap_err()
                .contains("was used")
        );
        let (late, until_late) = people.issue_code(&sam, now).unwrap();
        assert_eq!(until_late, until);
        assert!(
            people
                .take_code(&late, until_late)
                .unwrap_err()
                .contains("out of date")
        );

        // Kept across restarts.
        let reopened = People::open(dir.path().join("people.json"), 200_000);
        assert_eq!(reopened.by_uid(200_001).unwrap().key, "sub-sam");
        assert!(reopened.by_token(&sams, now).is_some());
    }
}
