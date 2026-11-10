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
use std::sync::{Mutex, MutexGuard, OnceLock};

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

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Kept {
    people: Vec<Person>,
    /// The lowest uid nobody has had: the next new person's, at least. Kept
    /// so that a uid is never given twice, whatever happens to the list.
    #[serde(default)]
    next_uid: u32,
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
    /// People's folders on this host, each named by its owner's uid: a
    /// uid one of them is named for is someone's, whatever the list says.
    homes: OnceLock<PathBuf>,
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
    ///
    /// A registry that is there and can't be read is an error, and glassd
    /// doesn't start on it: read as "nobody yet", the next person to sign
    /// in was given the first uid again, which is someone else's, and with
    /// it their folder and everything kept under their uid.
    pub fn open(path: PathBuf, base: u32) -> Result<Self, String> {
        let kept: Kept = glass_state::read_json(&path)
            .map_err(|e| {
                format!(
                    "{e}. It says which account on this host is whose, so glassd won't guess: \
                     put it right, or move it and people's folders away to start them afresh"
                )
            })?
            .unwrap_or_default();
        Ok(Self {
            path,
            base,
            kept: Mutex::new(kept),
            homes: OnceLock::new(),
        })
    }

    /// Where people's folders are, each named by its owner's uid. A new
    /// person is never given a uid one of them is named for: that folder
    /// is someone's, even if a registry restored from a backup doesn't
    /// know them.
    pub fn keep_clear_of(&self, homes: PathBuf) {
        let _ = self.homes.set(homes);
    }

    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Changes the registry: `change` is made to a copy, the copy is kept
    /// on disk, and only then is it what this host goes by. A change that
    /// couldn't be kept (a full disk) didn't happen: before, it stayed in
    /// memory, and the person it made was gone again at the next start
    /// with their uid free for someone else.
    fn change<T>(&self, change: impl FnOnce(&mut Kept) -> Result<T, String>) -> Result<T, String> {
        let mut kept = self.kept();
        let mut next = kept.clone();
        let out = change(&mut next)?;
        glass_state::write_json_pretty(&self.path, &next, glass_state::PRIVATE)
            .map_err(|e| format!("keeping who signed in: {e}"))?;
        *kept = next;
        Ok(out)
    }

    /// A uid nobody has or had: past every one given, and not one a
    /// folder is already named for.
    fn free_uid(&self, kept: &Kept) -> Result<u32, String> {
        let none_left = || "no uids left for people on this host".to_owned();
        let given = kept
            .people
            .iter()
            .map(|p| p.uid.saturating_add(1))
            .max()
            .unwrap_or(0);
        let mut uid = self.base.max(kept.next_uid).max(given);
        let last = self
            .base
            .checked_add(PEOPLE_MAX as u32)
            .ok_or_else(none_left)?;
        while let Some(homes) = self.homes.get()
            && homes.join(uid.to_string()).exists()
        {
            uid = uid.checked_add(1).ok_or_else(none_left)?;
        }
        if uid >= last {
            return Err(none_left());
        }
        Ok(uid)
    }

    /// The person these claims are, made the first time: a new uid.
    pub fn person(&self, claims: &Claims) -> Result<Person, String> {
        let sub = claims.sub.trim();
        if sub.is_empty() || sub.len() > SUB_MAX || sub.chars().any(char::is_control) {
            return Err("the sign-in has no usable subject".into());
        }
        let clean = |text: &str| glass_api::display_line(text.trim()).into_owned();
        let (email, name) = (clean(&claims.email), clean(&claims.name));
        // Known, and as they were: nothing to keep.
        if let Some(found) = self.kept().people.iter().find(|p| p.key == sub)
            && (email.is_empty() || found.email == email)
            && (name.is_empty() || found.name == name)
        {
            return Ok(found.clone());
        }
        self.change(|kept| {
            if let Some(found) = kept.people.iter_mut().find(|p| p.key == sub) {
                // Names and emails change; the subject doesn't.
                if !email.is_empty() {
                    found.email = email;
                }
                if !name.is_empty() {
                    found.name = name;
                }
                return Ok(found.clone());
            }
            if kept.people.len() >= PEOPLE_MAX {
                return Err("no uids left for people on this host".into());
            }
            let uid = self.free_uid(kept)?;
            let person = Person {
                key: sub.to_owned(),
                name: if name.is_empty() { email.clone() } else { name },
                email,
                uid,
                first_seen: Timestamp::now(),
            };
            kept.people.push(person.clone());
            kept.next_uid = uid + 1;
            Ok(person)
        })
    }

    pub fn by_uid(&self, uid: u32) -> Option<Person> {
        self.kept().people.iter().find(|p| p.uid == uid).cloned()
    }

    /// A token for `glass`, for a person signed in now; only its hash is
    /// kept.
    pub fn issue_token(&self, person: &Person, now: i64) -> Result<(String, i64), String> {
        let token = format!("{}{}", glass_api::TOKEN_PREFIX, random_hex()?);
        let until = now + TOKEN_DAYS * 86_400;
        self.change(|kept| {
            kept.tokens.retain(|_, (_, until)| *until > now);
            kept.tokens
                .insert(hash(&token), (person.key.clone(), until));
            Ok(())
        })?;
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
        self.change(|kept| {
            let before = kept.tokens.len();
            kept.tokens.retain(|_, (key, _)| key != &person.key);
            Ok(before - kept.tokens.len())
        })
    }

    /// A one-time login code for `glass login`; only its hash is kept.
    pub fn issue_code(&self, person: &Person, now: i64) -> Result<(String, i64), String> {
        let code = format!("{}{}", glass_api::CODE_PREFIX, random_hex()?);
        let until = now + CODE_SECS;
        self.change(|kept| {
            kept.codes.retain(|_, (_, until)| *until > now);
            kept.codes.insert(hash(&code), (person.key.clone(), until));
            Ok(())
        })?;
        Ok((code, until))
    }

    /// Whose code this was: it works once, and not after it expires.
    pub fn take_code(&self, code: &str, now: i64) -> Result<Person, String> {
        let code = hash(code.trim());
        // One this host never gave changes nothing, and writes nothing.
        if !self.kept().codes.contains_key(&code) {
            return Err("that sign-in code isn't one this host gave, or it was used".into());
        }
        let (key, until) = self
            .change(|kept| Ok(kept.codes.remove(&code)))?
            .ok_or("that sign-in code isn't one this host gave, or it was used")?;
        if until <= now {
            return Err("that sign-in code is out of date: run glass login again".into());
        }
        self.kept()
            .people
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
        let people = People::open(dir.path().join("people.json"), 200_000).unwrap();
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
        let reopened = People::open(dir.path().join("people.json"), 200_000).unwrap();
        assert_eq!(reopened.by_uid(200_001).unwrap().key, "sub-sam");
        assert!(reopened.by_token(&sams, now).is_some());
    }

    fn someone(sub: &str) -> Claims {
        Claims {
            sub: sub.into(),
            email: format!("{sub}@example.com"),
            name: String::new(),
        }
    }

    #[test]
    fn adr0067_a_registry_that_cant_be_read_stops_the_host_instead_of_renumbering_people() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("people.json");
        let people = People::open(path.clone(), 200_000).unwrap();
        let dana = people.person(&someone("dana")).unwrap();
        assert_eq!(dana.uid, 200_000);
        drop(people);
        // Cut short by a crash, or written by a version this one can't
        // read: read as "nobody yet", the next person in got 200,000,
        // which is Dana's, and with it Dana's folder.
        let whole = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &whole[..whole.len() / 2]).unwrap();
        let refused = People::open(path.clone(), 200_000).unwrap_err();
        assert!(
            refused.contains("is damaged") && refused.contains("won't guess"),
            "{refused}"
        );
        // Put right, it is what it was.
        std::fs::write(&path, whole).unwrap();
        let people = People::open(path, 200_000).unwrap();
        assert_eq!(people.person(&someone("dana")).unwrap().uid, 200_000);
        assert_eq!(people.person(&someone("sam")).unwrap().uid, 200_001);
    }

    #[test]
    fn adr0067_a_person_who_couldnt_be_kept_wasnt_made_and_no_uid_is_given_twice() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let homes = dir.path().join("people");
        let open = || {
            let people = People::open(state.join("people.json"), 200_000).unwrap();
            people.keep_clear_of(homes.clone());
            people
        };
        let people = open();
        assert_eq!(people.person(&someone("dana")).unwrap().uid, 200_000);
        // The disk is full (here: the registry's place is taken by a
        // folder, so the new file can't be put there). Sam can't be kept.
        let real = state.join("people.json");
        let moved = state.join("people.json.away");
        std::fs::rename(&real, &moved).unwrap();
        std::fs::create_dir(&real).unwrap();
        let refused = people.person(&someone("sam")).unwrap_err();
        assert!(refused.contains("keeping who signed in"), "{refused}");
        // And isn't someone this host goes by meanwhile: it used to hand
        // Sam uid 200,001 from memory at the next try, make their folder,
        // and forget them at the next start.
        assert!(people.by_uid(200_001).is_none());
        assert!(
            people
                .issue_token(&people.by_uid(200_000).unwrap(), 0)
                .is_err()
        );
        std::fs::remove_dir(&real).unwrap();
        std::fs::rename(&moved, &real).unwrap();
        assert_eq!(people.person(&someone("sam")).unwrap().uid, 200_001);
        drop(people);

        // A registry from before Sam (a backup put back), with Sam's
        // folder still on the host: Priya isn't given Sam's uid, whose
        // folder it would open, nor is anyone after her.
        std::fs::create_dir_all(homes.join("200001")).unwrap();
        let kept: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&real).unwrap()).unwrap();
        let mut older = kept.clone();
        older["people"].as_array_mut().unwrap().truncate(1);
        older["next_uid"] = 0.into();
        std::fs::write(&real, serde_json::to_vec(&older).unwrap()).unwrap();
        let people = open();
        assert_eq!(people.person(&someone("priya")).unwrap().uid, 200_002);
        assert_eq!(people.person(&someone("lee")).unwrap().uid, 200_003);
        // Someone already known is who they were, at no cost.
        assert_eq!(people.person(&someone("dana")).unwrap().uid, 200_000);
    }
}
