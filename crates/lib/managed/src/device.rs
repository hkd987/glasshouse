//! A computer's own key (ADR 0030 slice P): one P-256 pair per computer,
//! root only, that signs every request it makes to its console.
//!
//! The org's enrollment token reaches one route, [`REGISTER_PATH`], where a
//! computer registers its key's public half with proof that it holds the
//! secret one. Every other device route needs a signature over the method,
//! the path, the computer's id, the time and the body's SHA-256
//! ([`signing_input`]), so a request can't be moved to another route,
//! another computer or another body. The console refuses one more than
//! [`MAX_SKEW_SECS`] from its clock, and one it has already seen.
//!
//! The key says which computer is asking, not which person: the person
//! still comes from the computer's own sign-in, and their member keys
//! (chat) are registered by a request this key signs.

use std::path::Path;

use glass_crypto::{PublicKey, SecretKey, sha256};
use serde::{Deserialize, Serialize};

/// The one route the enrollment token alone reaches.
pub const REGISTER_PATH: &str = "/device/v1/register";
/// The computer's id, as it enrolled.
pub const DEVICE_HEADER: &str = "Glasshouse-Device";
/// When it signed, in seconds since 1970.
pub const TIME_HEADER: &str = "Glasshouse-Time";
/// Its signature over [`signing_input`], hex.
pub const SIGNATURE_HEADER: &str = "Glasshouse-Signature";
/// How far a request's time may be from the console's.
pub const MAX_SKEW_SECS: i64 = 120;

/// What a computer signs for a request: each part on its own line, the
/// body as its SHA-256, so neither side ever signs or parses the body
/// twice.
pub fn signing_input(method: &str, path: &str, device: &str, time: i64, body: &[u8]) -> Vec<u8> {
    format!(
        "glasshouse-device-request-v1\n{method}\n{path}\n{device}\n{time}\n{}",
        hex::encode(sha256(body))
    )
    .into_bytes()
}

/// What a computer signs to show it holds the key it registers.
fn proof_input(device: &str, key: &[u8]) -> Vec<u8> {
    format!(
        "glasshouse-device-register-v1\n{device}\n{}",
        hex::encode(key)
    )
    .into_bytes()
}

/// A computer's key: its id and the secret half, which never leaves it.
pub struct DeviceKey {
    device: String,
    secret: SecretKey,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl DeviceKey {
    /// A new key for the computer `device`.
    pub fn generate(device: &str) -> Result<Self, String> {
        Ok(Self {
            device: device.to_owned(),
            secret: SecretKey::generate().map_err(|e| e.to_string())?,
        })
    }

    /// The key kept as [`DeviceKey::to_bytes`] wrote it.
    pub fn from_bytes(device: &str, bytes: &[u8]) -> Result<Self, String> {
        Ok(Self {
            device: device.to_owned(),
            secret: SecretKey::from_bytes(bytes).map_err(|_| "the device key is damaged")?,
        })
    }

    /// The secret half, to keep where only root reads it.
    pub fn to_bytes(&self) -> Result<[u8; 32], String> {
        self.secret.to_bytes().map_err(|e| e.to_string())
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// The request that registers this key.
    pub fn registration(&self) -> Result<RegisterDevice, String> {
        let key = self.secret.public().map_err(|e| e.to_string())?.to_bytes();
        let proof = self
            .secret
            .sign(&proof_input(&self.device, &key))
            .map_err(|e| e.to_string())?;
        Ok(RegisterDevice {
            device: self.device.clone(),
            key: hex::encode(key),
            proof: hex::encode(proof),
        })
    }

    /// The headers that sign a request, at `time`.
    pub fn sign(
        &self,
        method: &str,
        path: &str,
        time: i64,
        body: &[u8],
    ) -> Result<[(&'static str, String); 3], String> {
        let signature = self
            .secret
            .sign(&signing_input(method, path, &self.device, time, body))
            .map_err(|e| e.to_string())?;
        Ok([
            (DEVICE_HEADER, self.device.clone()),
            (TIME_HEADER, time.to_string()),
            (SIGNATURE_HEADER, hex::encode(signature)),
        ])
    }
}

/// The computer's key (ADR 0030 slice P): the one kept at `path`, or a new
/// one written there for root alone. A key others can read isn't used.
pub fn kept(path: &Path, device: &str) -> Result<DeviceKey, String> {
    kept_secret(path, "the device key").map(|secret| DeviceKey {
        device: device.to_owned(),
        secret,
    })
}

/// A P-256 secret key kept at `path` as hex, or a new one written there for
/// its owner alone; `what` names it in errors. A key others can read isn't
/// used. The billing relay keeps its key this way too (ADR 0040).
pub fn kept_secret(path: &Path, what: &str) -> Result<SecretKey, String> {
    use std::os::unix::fs::MetadataExt;
    let shown = path.display();
    match std::fs::metadata(path) {
        // Empty: made and never written, by a glassd that stopped in
        // between before keys were written whole (ADR 0052). It never held
        // a key, so none was registered: a new one takes its place.
        Ok(meta) if meta.len() == 0 => write_secret(path, what, true),
        Ok(meta) => {
            if meta.mode() & 0o077 != 0 {
                return Err(format!(
                    "{what} {shown} can be read by others; it can't be trusted"
                ));
            }
            let text = std::fs::read_to_string(path).map_err(|e| format!("{what} {shown}: {e}"))?;
            let bytes =
                hex::decode(text.trim()).map_err(|_| format!("{what} {shown} is damaged"))?;
            SecretKey::from_bytes(&bytes).map_err(|_| format!("{what} {shown} is damaged"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => write_secret(path, what, false),
        Err(e) => Err(format!("{what} {shown}: {e}")),
    }
}

/// A new key at `path`, written whole under another name first and then
/// given the name (the DeviceEnrollment model, ADR 0052): stopped at any
/// point, it leaves the key whole or not there. Unless `replace`, one
/// another process gave the name first is the key.
fn write_secret(path: &Path, what: &str, replace: bool) -> Result<SecretKey, String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let shown = path.display();
    let failed = |e: std::io::Error| format!("{what} {shown}: {e}");
    let key = SecretKey::generate().map_err(|e| e.to_string())?;
    let bytes = key.to_bytes().map_err(|e| e.to_string())?;
    let new = path.with_extension(format!("new-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&new)
        .map_err(failed)?;
    let named = file
        .write_all(hex::encode(bytes).as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| match replace {
            true => std::fs::rename(&new, path),
            // A link fails if the name is taken, where a rename replaces.
            false => std::fs::hard_link(&new, path),
        });
    let _ = std::fs::remove_file(&new);
    match named {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return kept_secret(path, what);
        }
        Err(e) => return Err(failed(e)),
    }
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(failed)?;
    }
    Ok(key)
}

/// `POST /device/v1/register`, with the enrollment token: a computer's
/// public key, and its signature over its id and that key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterDevice {
    pub device: String,
    /// The public half, uncompressed SEC1, hex.
    pub key: String,
    pub proof: String,
}

impl RegisterDevice {
    /// The key, if the proof shows its holder signed for this computer.
    pub fn verify(&self) -> Result<PublicKey, String> {
        let bytes = hex::decode(&self.key).map_err(|_| "the key isn't hex")?;
        let key = PublicKey::from_bytes(&bytes).map_err(|_| "that isn't a P-256 public key")?;
        let proof = hex::decode(&self.proof).map_err(|_| "the proof isn't hex")?;
        if key.verify(&proof_input(&self.device, &bytes), &proof) {
            Ok(key)
        } else {
            Err("the proof isn't this key's signature for this computer".into())
        }
    }
}

/// Whether `signature` (hex) is `key`'s over the request.
pub fn verify_request(
    key: &PublicKey,
    method: &str,
    path: &str,
    device: &str,
    time: i64,
    body: &[u8],
    signature: &str,
) -> bool {
    hex::decode(signature)
        .is_ok_and(|sig| key.verify(&signing_input(method, path, device, time, body), &sig))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(key: &DeviceKey, path: &str, time: i64, body: &[u8]) -> (String, i64, String) {
        let [(_, device), (_, t), (_, sig)] = key.sign("POST", path, time, body).unwrap();
        (device, t.parse().unwrap(), sig)
    }

    #[test]
    fn adr0030_a_signed_request_verifies_only_as_it_was_sent() {
        let key = DeviceKey::generate("lab-07").unwrap();
        let public = key.registration().unwrap().verify().unwrap();
        let body = br#"{"device":"lab-07"}"#;
        let (device, time, sig) = headers(&key, "/device/v1/sync", 1_792_500_000, body);
        let ok = |method: &str, path: &str, device: &str, time: i64, body: &[u8]| {
            verify_request(&public, method, path, device, time, body, &sig)
        };
        assert!(ok("POST", "/device/v1/sync", &device, time, body));
        // Moved to another route, computer, time or body: no longer valid.
        assert!(!ok("POST", "/device/v1/report", &device, time, body));
        assert!(!ok("GET", "/device/v1/sync", &device, time, body));
        assert!(!ok("POST", "/device/v1/sync", "lab-08", time, body));
        assert!(!ok("POST", "/device/v1/sync", &device, time + 1, body));
        assert!(!ok(
            "POST",
            "/device/v1/sync",
            &device,
            time,
            br#"{"device":"lab-08"}"#
        ));
        // Another computer's key doesn't verify it.
        let other = DeviceKey::generate("lab-07").unwrap();
        let other_public = other.registration().unwrap().verify().unwrap();
        assert!(!verify_request(
            &other_public,
            "POST",
            "/device/v1/sync",
            &device,
            time,
            body,
            &sig
        ));
        assert!(!verify_request(
            &public,
            "POST",
            "/device/v1/sync",
            &device,
            time,
            body,
            "zz"
        ));
    }

    #[test]
    fn adr0030_a_registration_proves_the_key_for_its_computer() {
        let key = DeviceKey::generate("lab-07").unwrap();
        let registration = key.registration().unwrap();
        assert!(registration.verify().is_ok());
        // Claimed for another computer, or another key: refused.
        let moved = RegisterDevice {
            device: "lab-08".into(),
            ..registration.clone()
        };
        assert!(moved.verify().is_err());
        let swapped = RegisterDevice {
            key: DeviceKey::generate("lab-07")
                .unwrap()
                .registration()
                .unwrap()
                .key,
            ..registration
        };
        assert!(swapped.verify().is_err());
    }

    #[test]
    fn a_kept_key_comes_back_the_same_and_never_prints() {
        let key = DeviceKey::generate("lab-07").unwrap();
        let back = DeviceKey::from_bytes("lab-07", &key.to_bytes().unwrap()).unwrap();
        assert_eq!(
            back.registration().unwrap().key,
            key.registration().unwrap().key
        );
        let shown = format!("{key:?}");
        assert!(
            !shown.contains(&hex::encode(key.to_bytes().unwrap())),
            "{shown}"
        );
        assert!(DeviceKey::from_bytes("lab-07", &[1, 2, 3]).is_err());
    }

    /// glassd stopping between making the key file and writing it left it
    /// empty: "damaged", and the computer never synced again until someone
    /// deleted it by hand (the DeviceEnrollment model, ADR 0052). An empty
    /// file never held a key, so none was registered: a new one is made,
    /// and it's written whole before it has the name.
    #[test]
    fn adr0030_a_key_half_made_when_glassd_stopped_is_made_again() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let key = kept(&path, "lab-07").unwrap();
        let again = kept(&path, "lab-07").unwrap();
        assert_eq!(
            again.registration().unwrap().key,
            key.registration().unwrap().key
        );
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // Nothing left beside it.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["device.key"]);
        // A key with something in it that isn't one stays refused.
        std::fs::write(&path, "not a key").unwrap();
        assert!(kept(&path, "lab-07").unwrap_err().contains("damaged"));
    }
}
