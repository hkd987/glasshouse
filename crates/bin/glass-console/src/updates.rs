//! OS updates for the org's computers (ADR 0061): the console serves them
//! from a folder laid out as Glasshouse's update server is (`keys.json`,
//! `<channel>/release.json` with its image and kernel patch rebuilds
//! beside it), each computer its ring's channel, in parts, to computers
//! whose keys sign the request. It can keep that folder a mirror of the
//! update server (`--updates-from`).
//!
//! The console signs nothing here and is trusted for nothing: a computer
//! checks the root's key list, the channel's and the OS key's signatures
//! and the image's digest itself, as it does a folder's. What the console
//! decides is which channel each computer follows, by its ring, as IT
//! sets it.

use std::collections::BTreeMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use glass_managed::osupdate::{self, ImageDigest, MAX_PART, Release};
use glass_managed::{Ring, Signed};
use glass_types::Timestamp;
use serde_json::{Value, json};

use crate::state::{OsHold, Refused};

/// The most of `keys.json` or a `release.json` read.
const MAX_SIGNED: u64 = 1024 * 1024;
/// The largest image or rebuild mirrored.
const MAX_FILE: u64 = 16 * 1024 * 1024 * 1024;

/// The folder the console serves releases from.
#[derive(Debug)]
pub struct Updates {
    dir: PathBuf,
    /// Each channel's release, by when its file last changed.
    seen: Mutex<BTreeMap<&'static str, (SystemTime, Option<Offered>)>>,
    /// The update server this folder mirrors, and how the last round went.
    mirror: Mutex<Mirrored>,
}

/// A channel's release, as IT reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub number: u64,
    pub version: String,
    pub base: String,
    pub bytes: u64,
    pub delta_from: Option<String>,
}

#[derive(Debug, Default)]
struct Mirrored {
    from: Option<String>,
    at: Option<Timestamp>,
    error: Option<String>,
}

impl Updates {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            seen: Mutex::default(),
            mirror: Mutex::default(),
        }
    }

    /// The release on `ring`'s channel, if the folder has one that reads.
    pub fn offered(&self, ring: Ring) -> Option<Offered> {
        let channel = ring.as_str();
        let path = self.dir.join(channel).join("release.json");
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
        let mut seen = lock(&self.seen);
        if let Some((at, offered)) = seen.get(channel)
            && *at == modified
        {
            return offered.clone();
        }
        let offered = read_release(&path);
        seen.insert(channel, (modified, offered.clone()));
        offered
    }

    /// Part of `name` for a computer on `ring`, from byte `offset`: at most
    /// [`MAX_PART`] bytes, fewer at the file's end.
    pub fn part(&self, ring: Ring, name: &str, offset: u64) -> Result<Vec<u8>, Refused> {
        let path = if name == "keys.json" {
            self.dir.join(name)
        } else if osupdate::valid_file(name) {
            self.dir.join(ring.as_str()).join(name)
        } else {
            return Err(Refused::Invalid(vec![format!(
                "{name:?} isn't a release's file"
            )]));
        };
        let gone = || Refused::NotFound(format!("{name} isn't among this console's OS updates"));
        let mut file = std::fs::File::open(&path).map_err(|_| gone())?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| gone())?;
        let mut part = Vec::new();
        file.take(MAX_PART)
            .read_to_end(&mut part)
            .map_err(|_| gone())?;
        Ok(part)
    }

    /// What IT reads: each ring's release, whether IT holds it back, and
    /// the mirror.
    pub fn view(&self, holds: &BTreeMap<Ring, OsHold>) -> Value {
        let rings: Vec<Value> = Ring::ALL
            .iter()
            .map(|&ring| {
                let offered = self.offered(ring);
                json!({
                    "ring": ring,
                    "held": holds.get(&ring),
                    "number": offered.as_ref().map(|o| o.number),
                    "version": offered.as_ref().map(|o| &o.version),
                    "base": offered.as_ref().map(|o| &o.base),
                    "bytes": offered.as_ref().map(|o| o.bytes),
                    "delta_from": offered.as_ref().and_then(|o| o.delta_from.as_ref()),
                })
            })
            .collect();
        let mirror = lock(&self.mirror);
        json!({
            "rings": rings,
            "mirror": mirror.from.as_ref().map(|from| json!({
                "from": from,
                "at": mirror.at,
                "error": mirror.error,
            })),
        })
    }

    /// One round of mirroring the update server: its key list, then each
    /// channel's release once its image and rebuilds are here, so a
    /// computer is never told of a release whose files aren't. Files no
    /// release names any more go.
    pub fn mirror(&self, server: &glass_gateway_providers::UpdateClient) {
        let result = self.mirror_round(server);
        let mut mirror = lock(&self.mirror);
        mirror.from = Some(server.url().to_owned());
        mirror.at = Some(Timestamp::now());
        mirror.error = result.err();
        if let Some(e) = &mirror.error {
            tracing::warn!("updates: mirroring {}: {e}", server.url());
        }
    }

    fn mirror_round(&self, server: &glass_gateway_providers::UpdateClient) -> Result<(), String> {
        let small = |path: &str| -> Result<Option<Vec<u8>>, String> {
            let mut bytes = Vec::new();
            match server.get(path, 0, &mut bytes, MAX_SIGNED) {
                Ok(_) => Ok(Some(bytes)),
                Err(e) if e.contains("not on the update server") => Ok(None),
                Err(e) => Err(e),
            }
        };
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        if let Some(keys) = small("keys.json")? {
            write_whole(&self.dir.join("keys.json"), &keys)?;
        }
        let mut problems = Vec::new();
        for ring in Ring::ALL {
            let channel = ring.as_str();
            let dir = self.dir.join(channel);
            let Some(bytes) = small(&format!("{channel}/release.json"))? else {
                continue;
            };
            if std::fs::read(dir.join("release.json")).ok().as_deref() == Some(&bytes[..]) {
                continue;
            }
            let wanted = match files_of(&bytes) {
                Ok(files) => files,
                Err(e) => {
                    problems.push(format!("{channel}: {e}"));
                    continue;
                }
            };
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut fetched = Ok(());
            for (file, bytes) in &wanted {
                fetched = fetch(server, channel, &dir, file, *bytes);
                if fetched.is_err() {
                    break;
                }
            }
            match fetched {
                Ok(()) => {
                    write_whole(&dir.join("release.json"), &bytes)?;
                    tracing::info!("updates: {channel}'s release mirrored");
                    // What no release names any more.
                    if let Ok(entries) = std::fs::read_dir(&dir) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if name != "release.json" && !wanted.iter().any(|(f, _)| *f == name) {
                                let _ = std::fs::remove_file(entry.path());
                            }
                        }
                    }
                }
                Err(e) => problems.push(format!("{channel}: {e}")),
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }
}

/// A release's number, version and image, read without checking its
/// signatures: computers check them.
fn read_release(path: &Path) -> Option<Offered> {
    let signed: Signed = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let release: Release = serde_json::from_str(&signed.payload).ok()?;
    let image: ImageDigest = serde_json::from_str(&release.image.payload).ok()?;
    Some(Offered {
        number: signed.version,
        version: release.version,
        base: release.base,
        bytes: image.bytes,
        delta_from: image.delta_from,
    })
}

/// The files a release names, with the image's signed size: its image and
/// kernel patch rebuilds.
fn files_of(release: &[u8]) -> Result<Vec<(String, Option<u64>)>, String> {
    let signed: Signed =
        serde_json::from_slice(release).map_err(|e| format!("its release.json: {e}"))?;
    let release: Release =
        serde_json::from_str(&signed.payload).map_err(|e| format!("its release: {e}"))?;
    let image: ImageDigest =
        serde_json::from_str(&release.image.payload).map_err(|e| format!("its image: {e}"))?;
    let mut files = vec![(image.file, Some(image.bytes))];
    files.extend(release.patches.into_iter().map(|p| (p.bundle, None)));
    for (file, _) in &files {
        if !osupdate::valid_file(file) || file == "release.json" {
            return Err(format!("{file:?} isn't a release's file"));
        }
    }
    Ok(files)
}

/// `file` of `channel` from the server into `dir`: a download that stopped
/// goes on from where it was, and only a whole one takes the file's name.
/// An image of its signed size already here isn't fetched again.
fn fetch(
    server: &glass_gateway_providers::UpdateClient,
    channel: &str,
    dir: &Path,
    file: &str,
    bytes: Option<u64>,
) -> Result<(), String> {
    let to = dir.join(file);
    if let Some(bytes) = bytes
        && std::fs::metadata(&to).is_ok_and(|m| m.len() == bytes)
    {
        return Ok(());
    }
    let partial = dir.join(format!(".{file}.part"));
    let have = std::fs::metadata(&partial).map_or(0, |m| m.len());
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&partial)
        .map_err(|e| e.to_string())?;
    let max = bytes.unwrap_or(MAX_FILE).saturating_sub(have);
    server.get(&format!("{channel}/{file}"), have, &mut out, max)?;
    out.sync_all().map_err(|e| e.to_string())?;
    let got = std::fs::metadata(&partial).map_or(0, |m| m.len());
    if let Some(bytes) = bytes
        && got != bytes
    {
        return Err(format!(
            "{file}: {got} of {bytes} bytes; the next round goes on"
        ));
    }
    std::fs::rename(&partial, &to).map_err(|e| e.to_string())
}

fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("json.new");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use glass_managed::{DocKind, OrgKey};

    use super::*;

    /// An update server on this machine: files from `dir` by path, ranges
    /// honoured, and the first image it sends cut off after `cut` bytes
    /// when that's set.
    fn serve(dir: PathBuf, cut: Arc<AtomicU64>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut from = 0u64;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(range) = line.strip_prefix("Range: bytes=") {
                        from = range.trim().trim_end_matches('-').parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let path = first.split(' ').nth(1).unwrap().trim_start_matches('/');
                let Ok(bytes) = std::fs::read(dir.join(path)) else {
                    let _ =
                        stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n");
                    continue;
                };
                let body = &bytes[from as usize..];
                let status = if from > 0 {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                );
                let cut_at = if path.ends_with(".zfs") {
                    cut.swap(0, Ordering::SeqCst)
                } else {
                    0
                };
                if cut_at > 0 {
                    let _ = stream.write_all(&body[..cut_at as usize]);
                } else {
                    let _ = stream.write_all(body);
                }
            }
        });
        url
    }

    /// Release `number` on the pilot channel, with an image of `size`.
    fn publish(dir: &Path, number: u64, version: &str, size: usize) {
        let key = OrgKey::from_seed([3; 32]);
        let image: Vec<u8> = (0..size).map(|i| (i % 13) as u8).collect();
        let file = format!("glasshouse-{version}.zfs");
        std::fs::create_dir_all(dir.join("pilot")).unwrap();
        std::fs::write(dir.join("pilot").join(&file), &image).unwrap();
        let digest = ImageDigest {
            file,
            sha256: glass_managed::sha256_hex(&image),
            bytes: size as u64,
            version: version.into(),
            delta_from: None,
        };
        let release = Release {
            channel: "pilot".into(),
            version: version.into(),
            base: "15.1-RELEASE-p2".into(),
            image: key.sign(
                DocKind::OsImage,
                glass_managed::hwpatch::GLASSHOUSE,
                number,
                serde_json::to_string(&digest).unwrap(),
            ),
            fixes: vec![],
            patches: vec![],
            notes: String::new(),
        };
        let signed = key.sign(
            DocKind::OsRelease,
            glass_managed::hwpatch::GLASSHOUSE,
            number,
            serde_json::to_string(&release).unwrap(),
        );
        std::fs::write(
            dir.join("pilot/release.json"),
            serde_json::to_vec(&signed).unwrap(),
        )
        .unwrap();
    }

    /// The console mirrors the update server: a channel's release only once
    /// its image is whole here, a download cut off goes on in the next
    /// round, and files no release names go.
    #[test]
    fn bs4_the_console_mirrors_the_update_server() {
        let upstream = tempfile::tempdir().unwrap();
        let here = tempfile::tempdir().unwrap();
        std::fs::write(upstream.path().join("keys.json"), b"{\"keys\": 1}").unwrap();
        publish(upstream.path(), 7, "2026.11.1", 50_000);
        let cut = Arc::new(AtomicU64::new(0));
        let url = serve(upstream.path().to_owned(), cut.clone());
        let server = glass_gateway_providers::UpdateClient::new(&url, true).unwrap();
        let updates = Updates::new(here.path());

        updates.mirror(&server);
        assert_eq!(
            updates.view(&BTreeMap::new())["mirror"]["error"],
            Value::Null,
            "{}",
            updates.view(&BTreeMap::new())
        );
        assert_eq!(updates.offered(Ring::Pilot).unwrap().number, 7);
        assert_eq!(updates.offered(Ring::Broad), None);
        assert_eq!(
            std::fs::read(here.path().join("keys.json")).unwrap(),
            b"{\"keys\": 1}"
        );

        // Release 8: its image cut off partway, so computers still hear of 7.
        publish(upstream.path(), 8, "2026.11.2", 60_000);
        cut.store(20_000, Ordering::SeqCst);
        updates.mirror(&server);
        assert!(updates.view(&BTreeMap::new())["mirror"]["error"].is_string());
        assert_eq!(updates.offered(Ring::Pilot).unwrap().number, 7);

        // The next round goes on from where it stopped: 8, and 7's image gone.
        updates.mirror(&server);
        assert_eq!(
            updates.view(&BTreeMap::new())["mirror"]["error"],
            Value::Null,
            "{}",
            updates.view(&BTreeMap::new())
        );
        assert_eq!(updates.offered(Ring::Pilot).unwrap().number, 8);
        assert_eq!(
            std::fs::read(here.path().join("pilot/glasshouse-2026.11.2.zfs")).unwrap(),
            std::fs::read(upstream.path().join("pilot/glasshouse-2026.11.2.zfs")).unwrap()
        );
        assert!(!here.path().join("pilot/glasshouse-2026.11.1.zfs").exists());
    }
}
