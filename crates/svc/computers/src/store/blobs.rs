//! Where chunks and manifests are kept: S3, or a folder standing in for it.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glass_gateway_providers::s3::S3Client;

/// An object, as a listing finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub key: String,
    pub size: u64,
    /// When it was last written.
    pub modified: SystemTime,
    /// What tells this writing of it from the next, where the store has
    /// such a thing (S3's entity tag).
    pub tag: Option<String>,
}

/// A flat store of objects by key (`chunks/<sha256>`,
/// `computers/<computer>/versions/<version>.json`): S3, or a folder
/// standing in for it. Writing a key replaces its object whole; nobody
/// reads half of one.
pub trait Blobs: Send + Sync + fmt::Debug {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String>;
    /// [`Blobs::put`] for bytes whose SHA-256 (`sha256`, in hex) the caller
    /// has already: a chunk is named by it, and S3 wants it with every
    /// upload. A store with no use for it takes the bytes as `put` does.
    fn put_hashed(&self, key: &str, bytes: &[u8], sha256: &str) -> Result<(), String> {
        let _ = sha256;
        self.put(key, bytes)
    }
    /// Puts `bytes` at `key` only if nothing is there, and says whether it
    /// did. Of several asking at once for one key, one does: what a claim
    /// is made of (a computer's lease, ADR 0066).
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<bool, String>;
    /// The object at `key`, or `None` if there's none.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;
    fn exists(&self, key: &str) -> Result<bool, String>;
    /// When the object at `key` was last written, or `None` if there's
    /// none: what a save asks before it relies on a chunk it didn't send
    /// itself.
    fn modified(&self, key: &str) -> Result<Option<SystemTime>, String> {
        Ok(self
            .list(key)?
            .into_iter()
            .find(|listed| listed.key == key)
            .map(|listed| listed.modified))
    }
    /// Deletes `key`. Deleting what isn't there is no error.
    fn delete(&self, key: &str) -> Result<(), String>;
    /// Deletes an object only if it is still the one `listed`: not
    /// written again since. `Ok(false)` if it was, and it stays. GC
    /// deletes this way, so a chunk a save puts between GC's listing and
    /// its delete is not lost.
    fn delete_listed(&self, listed: &Listed) -> Result<bool, String> {
        self.delete(&listed.key).map(|()| true)
    }
    /// Every object whose key starts with `prefix`, in key order.
    fn list(&self, prefix: &str) -> Result<Vec<Listed>, String>;
}

/// A folder standing in for S3, for development and tests: each key a file
/// beneath it. A write goes to a file beside the key's, then is renamed
/// onto it, so a reader sees the old object or the new one, whole.
#[derive(Debug, Clone)]
pub struct FolderBlobs {
    root: PathBuf,
}

impl FolderBlobs {
    /// The store in `root`, made if it isn't there.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, String> {
        let root = root.into();
        fs::create_dir_all(&root)
            .map_err(|e| format!("can't make the store folder {}: {e}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `key` lives. Keys are names of letters, digits, `.`, `_` and
    /// `-` between `/`s, none starting with `.`: nothing reaches outside
    /// the folder, or onto a file being written.
    fn path(&self, key: &str) -> Result<PathBuf, String> {
        let fits = !key.is_empty()
            && key.split('/').all(|part| {
                !part.is_empty()
                    && !part.starts_with('.')
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            });
        if !fits {
            return Err(format!("{key:?} isn't a key the store folder can hold"));
        }
        Ok(self.root.join(key))
    }

    /// `bytes` whole in a file beside `path`, under a name of its own.
    fn staged(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
        let parent = path.parent().unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temp = parent.join(format!(".{name}.{}", super::unique()));
        let mut file = match File::create_new(&temp) {
            // Its folder is new, or a delete just took it away.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(parent)?;
                File::create_new(&temp)?
            }
            other => other?,
        };
        if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temp);
            return Err(e);
        }
        Ok(temp)
    }

    fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let temp = Self::staged(path, bytes)?;
        let written = fs::rename(&temp, path);
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written
    }

    /// Writes `path` only if it isn't there. A link to the whole file fails
    /// if the name is taken, for whoever asks second, in this process or
    /// another; a reader never sees half of it.
    fn write_new(path: &Path, bytes: &[u8]) -> io::Result<bool> {
        let temp = Self::staged(path, bytes)?;
        let linked = fs::hard_link(&temp, path);
        let _ = fs::remove_file(&temp);
        match linked {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }
}

impl Blobs for FolderBlobs {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.path(key)?;
        Self::write(&path, bytes).map_err(|e| format!("can't write {key} in the store folder: {e}"))
    }

    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        let path = self.path(key)?;
        Self::write_new(&path, bytes)
            .map_err(|e| format!("can't write {key} in the store folder: {e}"))
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        match fs::read(self.path(key)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("can't read {key} in the store folder: {e}")),
        }
    }

    fn exists(&self, key: &str) -> Result<bool, String> {
        match fs::symlink_metadata(self.path(key)?) {
            Ok(meta) => Ok(meta.is_file()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("can't look for {key} in the store folder: {e}")),
        }
    }

    fn modified(&self, key: &str) -> Result<Option<SystemTime>, String> {
        match fs::symlink_metadata(self.path(key)?) {
            Ok(meta) if meta.is_file() => meta
                .modified()
                .map(Some)
                .map_err(|e| format!("can't look at {key} in the store folder: {e}")),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("can't look for {key} in the store folder: {e}")),
        }
    }

    fn delete_listed(&self, listed: &Listed) -> Result<bool, String> {
        // A folder has no delete-if-unchanged: looked at again, then
        // deleted. Development and tests; S3 checks it itself.
        match self.modified(&listed.key)? {
            Some(at) if at != listed.modified => Ok(false),
            _ => self.delete(&listed.key).map(|()| true),
        }
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let path = self.path(key)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("can't delete {key} in the store folder: {e}")),
        }
        // Folders left empty go too, as they would in S3, where there are
        // none.
        let mut folder = path.parent();
        while let Some(dir) = folder.filter(|d| *d != self.root) {
            if fs::remove_dir(dir).is_err() {
                break;
            }
            folder = dir.parent();
        }
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        let failed = |e: io::Error| format!("can't list {prefix:?} in the store folder: {e}");
        // Starts in the folder the prefix names up to its last `/`.
        let start = prefix.rfind('/').map_or("", |at| &prefix[..at]);
        let mut folders = vec![(
            if start.is_empty() {
                self.root.clone()
            } else {
                self.path(start)?
            },
            start.to_owned(),
        )];
        let mut found = Vec::new();
        while let Some((dir, key)) = folders.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(failed(e)),
            };
            for entry in entries {
                // An object deleted while this lists (another host's, as
                // S3 would simply not list it) is not there: never an
                // error.
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(failed(e)),
                };
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let key = if key.is_empty() {
                    name
                } else {
                    format!("{key}/{name}")
                };
                // Doesn't follow a symlink.
                let meta = match entry.metadata() {
                    Ok(meta) => meta,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(failed(e)),
                };
                if meta.is_dir() {
                    let below = format!("{key}/");
                    if below.starts_with(prefix) || prefix.starts_with(&below) {
                        folders.push((entry.path(), key));
                    }
                } else if meta.is_file() && key.starts_with(prefix) {
                    found.push(Listed {
                        key,
                        size: meta.len(),
                        modified: meta.modified().map_err(failed)?,
                        tag: None,
                    });
                }
            }
        }
        found.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(found)
    }
}

/// S3, through [`S3Client`], every key under a prefix of its own
/// (`glasshouse/`), so one bucket can hold other things too.
#[derive(Debug)]
pub struct S3Blobs {
    client: S3Client,
    prefix: String,
}

/// The largest object read back: a manifest of a very large tree. Chunks
/// are far smaller.
const MAX_OBJECT: u64 = 1 << 30;

impl S3Blobs {
    pub fn new(client: S3Client, prefix: &str) -> Self {
        let prefix = prefix.trim_matches('/');
        Self {
            client,
            prefix: if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}/")
            },
        }
    }

    fn key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }
}

impl Blobs for S3Blobs {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        self.client.put(&self.key(key), bytes)
    }

    fn put_hashed(&self, key: &str, bytes: &[u8], sha256: &str) -> Result<(), String> {
        self.client.put_hashed(&self.key(key), bytes, sha256)
    }

    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        self.client.put_new(&self.key(key), bytes)
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.client.get(&self.key(key), MAX_OBJECT)
    }

    fn exists(&self, key: &str) -> Result<bool, String> {
        Ok(self.client.head(&self.key(key))?.is_some())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.client.delete(&self.key(key))
    }

    fn delete_listed(&self, listed: &Listed) -> Result<bool, String> {
        match &listed.tag {
            Some(tag) => self.client.delete_if(&self.key(&listed.key), tag),
            None => self.delete(&listed.key).map(|()| true),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        let now = SystemTime::now();
        Ok(self
            .client
            .list(&self.key(prefix))?
            .into_iter()
            .filter_map(|object| {
                Some(Listed {
                    key: object.key.strip_prefix(&self.prefix)?.to_owned(),
                    size: object.size,
                    // An object without a time counts as new, so GC keeps it.
                    modified: object.modified.unwrap_or(now),
                    tag: object.etag,
                })
            })
            .collect())
    }
}
