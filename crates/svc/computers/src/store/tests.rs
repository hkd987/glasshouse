#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs::{self, Permissions};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use glass_gateway_providers::s3::S3Client;
use glass_gateway_providers::s3::testing::StandIn;
use serde_json::json;

use super::*;

/// Bytes that don't repeat, so each chunk of them is its own.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
}

/// Files of several sizes (none, a few bytes, exactly a chunk, over a
/// chunk, zeros), modes from 0600 to setuid, a relative and an absolute
/// symlink, an empty folder and a read-only one.
fn make_tree(dir: &Path) {
    fs::create_dir_all(dir.join("bin")).unwrap();
    write(&dir.join("empty"), b"", 0o644);
    write(&dir.join("small.txt"), b"hello", 0o600);
    write(&dir.join("bin/tool"), &noise(1, 1000), 0o755);
    write(&dir.join("bin/setuid"), b"#!/bin/sh\n", 0o4755);
    write(&dir.join("big.bin"), &noise(2, CHUNK_SIZE + 4321), 0o640);
    write(&dir.join("exact.bin"), &noise(3, CHUNK_SIZE), 0o644);
    write(&dir.join("zeros"), &[0; 100], 0o644);
    symlink("../big.bin", dir.join("bin/big")).unwrap();
    symlink("/etc/passwd", dir.join("passwd")).unwrap();
    fs::create_dir(dir.join("empty-dir")).unwrap();
    fs::set_permissions(dir.join("empty-dir"), Permissions::from_mode(0o750)).unwrap();
    fs::create_dir(dir.join("ro")).unwrap();
    write(&dir.join("ro/inside"), b"x", 0o444);
    fs::set_permissions(dir.join("ro"), Permissions::from_mode(0o555)).unwrap();
}

/// Lets a test's temporary folder be removed.
fn writable(dir: &Path) {
    let ro = dir.join("ro");
    if ro.is_dir() {
        fs::set_permissions(ro, Permissions::from_mode(0o755)).unwrap();
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Node {
    Folder {
        mode: u32,
        mtime: (i64, i64),
    },
    File {
        mode: u32,
        mtime: (i64, i64),
        sha256: String,
    },
    Link(PathBuf),
}

/// Everything a restore should put back, by path ("" is the folder itself).
fn snapshot(dir: &Path) -> BTreeMap<String, Node> {
    fn visit(dir: &Path, at: &str, out: &mut BTreeMap<String, Node>) {
        let meta = fs::symlink_metadata(dir).unwrap();
        out.insert(
            at.to_owned(),
            Node::Folder {
                mode: meta.mode() & 0o7777,
                mtime: (meta.mtime(), meta.mtime_nsec()),
            },
        );
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let path = if at.is_empty() {
                name
            } else {
                format!("{at}/{name}")
            };
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                visit(&entry.path(), &path, out);
            } else if meta.is_symlink() {
                out.insert(path, Node::Link(fs::read_link(entry.path()).unwrap()));
            } else {
                out.insert(
                    path,
                    Node::File {
                        mode: meta.mode() & 0o7777,
                        mtime: (meta.mtime(), meta.mtime_nsec()),
                        sha256: hex::encode(Sha256::digest(fs::read(entry.path()).unwrap())),
                    },
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(dir, "", &mut out);
    out
}

/// A folder store that remembers what was put in it.
#[derive(Debug)]
struct Counting {
    inner: FolderBlobs,
    puts: Mutex<Vec<String>>,
}

impl Counting {
    fn taken(&self) -> Vec<String> {
        std::mem::take(&mut *self.puts.lock().unwrap())
    }
}

impl Blobs for Counting {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        self.puts.lock().unwrap().push(key.to_owned());
        self.inner.put(key, bytes)
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        self.inner.put_new(key, bytes)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.inner.get(key)
    }
    fn exists(&self, key: &str) -> Result<bool, String> {
        self.inner.exists(key)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.inner.delete(key)
    }
    fn list(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        self.inner.list(prefix)
    }
}

fn folder_store(dir: &Path) -> (Store, Arc<FolderBlobs>) {
    let blobs = Arc::new(FolderBlobs::new(dir.join("store")).unwrap());
    (Store::new(blobs.clone()), blobs)
}

fn chunk_count(blobs: &dyn Blobs) -> usize {
    blobs.list("chunks/").unwrap().len()
}

#[test]
fn adr0065_a_computer_sleeps_to_chunks_and_wakes_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("dana");
    make_tree(&tree);
    let (store, blobs) = folder_store(tmp.path());

    let saved = store.save_tree("dana-reconciler", &tree).unwrap();
    assert_eq!(saved.files, 8);
    let total = 5 + 1000 + 10 + (CHUNK_SIZE + 4321) + CHUNK_SIZE + 100 + 1;
    assert_eq!(saved.bytes, total as u64);
    assert_eq!(saved.uploaded, saved.bytes - 100, "zeros aren't stored");
    assert!(saved.skipped.is_empty());
    // small, tool, setuid, big's two, exact and inside.
    assert_eq!(chunk_count(&*blobs), 7);
    assert_eq!(
        store.versions("dana-reconciler").unwrap(),
        std::slice::from_ref(&saved.version)
    );
    assert_eq!(
        store.latest("dana-reconciler").unwrap(),
        Some(saved.version.clone())
    );

    let original = snapshot(&tree);
    // Into a folder that isn't there yet, and into one that's empty.
    let woken = tmp.path().join("woken/dana");
    store
        .restore_tree("dana-reconciler", &saved.version, &woken)
        .unwrap();
    assert_eq!(snapshot(&woken), original);
    let empty = tmp.path().join("empty");
    fs::create_dir(&empty).unwrap();
    store
        .restore_tree("dana-reconciler", &saved.version, &empty)
        .unwrap();
    assert_eq!(snapshot(&empty), original);
    assert_eq!(
        fs::read_dir(tmp.path().join("woken")).unwrap().count(),
        1,
        "nothing left beside it"
    );

    let err = store
        .restore_tree("dana-reconciler", &saved.version, &tree)
        .unwrap_err();
    assert!(err.contains("isn't empty"), "{err}");
    let err = store
        .restore_file("dana-reconciler", &saved.version, &tmp.path().join("x"))
        .unwrap_err();
    assert!(err.contains("is a folder tree, not one file"), "{err}");
    let err = store
        .restore_tree(
            "dana-reconciler",
            "20261027T000000.000000000Z",
            &tmp.path().join("y"),
        )
        .unwrap_err();
    assert!(err.contains("has no version"), "{err}");
    assert!(store.save_tree("../dana", &tree).is_err());

    for dir in [&tree, &woken, &empty] {
        writable(dir);
    }
}

#[test]
fn adr0065_a_second_save_uploads_only_the_changed_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("dana");
    make_tree(&tree);
    let blobs = Arc::new(Counting {
        inner: FolderBlobs::new(tmp.path().join("store")).unwrap(),
        puts: Mutex::new(Vec::new()),
    });
    let store = Store::new(blobs.clone());
    let first = store.save_tree("dana", &tree).unwrap();
    let before = snapshot(&tree);
    blobs.taken();

    // One byte of the big file's first chunk.
    let mut big = fs::read(tree.join("big.bin")).unwrap();
    big[10] ^= 0xff;
    fs::write(tree.join("big.bin"), &big).unwrap();
    let second = store.save_tree("dana", &tree).unwrap();
    assert_eq!(second.uploaded, CHUNK_SIZE as u64);
    assert_eq!(second.bytes, first.bytes);
    let put = blobs.taken();
    assert_eq!(put.len(), 2, "{put:?}");
    assert!(put[0].starts_with("chunks/"));
    assert_eq!(
        put[1],
        format!("computers/dana/versions/{}.json", second.version)
    );

    // Nothing changed: nothing uploaded but the manifest.
    let third = store.save_tree("dana", &tree).unwrap();
    assert_eq!(third.uploaded, 0);
    assert_eq!(blobs.taken().len(), 1);

    assert!(first.version < second.version && second.version < third.version);
    assert_eq!(
        store.versions("dana").unwrap(),
        [
            first.version.clone(),
            second.version.clone(),
            third.version.clone()
        ]
    );
    assert_eq!(store.latest("dana").unwrap(), Some(third.version));
    store
        .restore_tree("dana", &first.version, &tmp.path().join("one"))
        .unwrap();
    store
        .restore_tree("dana", &second.version, &tmp.path().join("two"))
        .unwrap();
    assert_eq!(snapshot(&tmp.path().join("one")), before);
    assert_eq!(snapshot(&tmp.path().join("two")), snapshot(&tree));
    for dir in ["dana", "one", "two"] {
        writable(&tmp.path().join(dir));
    }
}

#[test]
fn adr0065_a_sparse_image_keeps_its_zeros_out_of_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, blobs) = folder_store(tmp.path());
    // Four chunks: zeros, data, zeros, and ten bytes of data at the end.
    let image = tmp.path().join("disk.img");
    let file = fs::File::create(&image).unwrap();
    file.set_len(3 * CHUNK_SIZE as u64 + 10).unwrap();
    file.write_all_at(&noise(4, CHUNK_SIZE), CHUNK_SIZE as u64)
        .unwrap();
    file.write_all_at(&noise(5, 10), 3 * CHUNK_SIZE as u64)
        .unwrap();
    drop(file);
    fs::set_permissions(&image, Permissions::from_mode(0o600)).unwrap();

    let saved = store.save_file("dana", &image).unwrap();
    assert_eq!((saved.files, saved.bytes), (1, 3 * CHUNK_SIZE as u64 + 10));
    assert_eq!(saved.uploaded, CHUNK_SIZE as u64 + 10);
    assert_eq!(chunk_count(&*blobs), 2);

    let woken = tmp.path().join("woken/disk.img");
    store.restore_file("dana", &saved.version, &woken).unwrap();
    assert_eq!(fs::read(&woken).unwrap(), fs::read(&image).unwrap());
    let (a, b) = (fs::metadata(&image).unwrap(), fs::metadata(&woken).unwrap());
    assert_eq!(
        (a.mode(), a.mtime(), a.mtime_nsec()),
        (b.mode(), b.mtime(), b.mtime_nsec())
    );
    // Restored again over what's there.
    fs::write(&woken, b"something else").unwrap();
    store.restore_file("dana", &saved.version, &woken).unwrap();
    assert_eq!(fs::read(&woken).unwrap(), fs::read(&image).unwrap());
    assert_eq!(fs::read_dir(tmp.path().join("woken")).unwrap().count(), 1);

    let err = store
        .restore_tree("dana", &saved.version, &tmp.path().join("tree"))
        .unwrap_err();
    assert!(err.contains("is one file, not a folder tree"), "{err}");

    // Nothing but zeros stores nothing.
    let blank = tmp.path().join("blank.img");
    fs::File::create(&blank)
        .unwrap()
        .set_len(2 * CHUNK_SIZE as u64)
        .unwrap();
    assert_eq!(store.save_file("blank", &blank).unwrap().uploaded, 0);
    assert_eq!(chunk_count(&*blobs), 2);
}

#[test]
fn adr0065_a_tampered_or_missing_chunk_fails_the_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("dana");
    fs::create_dir(&tree).unwrap();
    write(&tree.join("notes.txt"), &noise(6, 5000), 0o644);
    let (store, blobs) = folder_store(tmp.path());
    let saved = store.save_tree("dana", &tree).unwrap();
    let chunk = blobs.list("chunks/").unwrap().remove(0);
    let path = blobs.root().join(&chunk.key);

    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(&path, &bytes).unwrap();
    let woken = tmp.path().join("woken");
    let err = store
        .restore_tree("dana", &saved.version, &woken)
        .unwrap_err();
    assert!(err.contains("doesn't match its SHA-256"), "{err}");
    assert!(!woken.exists());
    assert_eq!(
        fs::read_dir(tmp.path()).unwrap().count(),
        2,
        "nothing half-restored"
    );

    fs::remove_file(&path).unwrap();
    let empty = tmp.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let err = store
        .restore_tree("dana", &saved.version, &empty)
        .unwrap_err();
    assert!(err.contains("isn't in the store"), "{err}");
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0, "emptied again");
}

#[test]
fn adr0065_gc_keeps_every_chunk_a_version_refers_to() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, blobs) = folder_store(tmp.path());
    let collector = Store::new(blobs.clone()).with_grace(Duration::ZERO);
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    for dir in [&a, &b] {
        fs::create_dir(dir).unwrap();
        write(&dir.join("shared"), &noise(10, 2000), 0o644);
    }
    write(&a.join("own"), &noise(11, 1000), 0o644);
    let first = store.save_tree("a", &a).unwrap();
    write(&a.join("own"), &noise(12, 1000), 0o644);
    let second = store.save_tree("a", &a).unwrap();
    store.save_tree("b", &b).unwrap();
    assert_eq!(chunk_count(&*blobs), 3);

    store.remove_version("a", &first.version).unwrap();
    assert_eq!(
        store.versions("a").unwrap(),
        std::slice::from_ref(&second.version)
    );
    // Within the grace period, an unreferenced chunk may be a save's that
    // hasn't written its manifest yet: kept.
    assert_eq!(
        store.gc().unwrap(),
        Collected {
            kept: 3,
            deleted: 0,
            freed: 0
        }
    );
    assert_eq!(
        collector.gc().unwrap(),
        Collected {
            kept: 2,
            deleted: 1,
            freed: 1000
        }
    );
    store
        .restore_tree("a", &second.version, &tmp.path().join("a-woken"))
        .unwrap();
    assert_eq!(snapshot(&tmp.path().join("a-woken")), snapshot(&a));

    // Removing a computer frees what only it had.
    store.remove("a").unwrap();
    assert_eq!(store.versions("a").unwrap(), Vec::<String>::new());
    assert_eq!(collector.gc().unwrap().deleted, 1);
    let b_version = store.latest("b").unwrap().unwrap();
    store
        .restore_tree("b", &b_version, &tmp.path().join("b-woken"))
        .unwrap();
    assert_eq!(snapshot(&tmp.path().join("b-woken")), snapshot(&b));
    assert!(!blobs.root().join("computers/a").exists());

    // A manifest it can't read stops it: that manifest may refer to chunks.
    blobs
        .put(
            "computers/c/versions/20261027T000000.000000000Z.json",
            b"{not json",
        )
        .unwrap();
    store.remove("b").unwrap();
    let err = collector.gc().unwrap_err();
    assert!(err.contains("deleted nothing"), "{err}");
    assert_eq!(chunk_count(&*blobs), 1);
}

#[test]
fn adr0065_a_manifest_cannot_reach_outside_the_folder_it_restores_into() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, blobs) = folder_store(tmp.path());
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let version = "20261027T120000.000000000Z";
    let stamp = json!({"mode": 0o755, "mtime": 0, "mtime_nsec": 0});
    let folder = |path: &str| json!({"path": path, "kind": "directory", "mode": 0o755, "mtime": 0, "mtime_nsec": 0});
    let file = |path: &str| json!({"path": path, "kind": "file", "size": 0, "chunks": [], "mode": 0o644, "mtime": 0, "mtime_nsec": 0});
    let link = |path: &str, target: &str| json!({"path": path, "kind": "symlink", "target": target, "mode": 0o777, "mtime": 0, "mtime_nsec": 0});
    let outside_target = outside.display().to_string();
    let cases = [
        vec![file("../escaped")],
        vec![file("/tmp/escaped")],
        vec![folder("a"), file("a/../../escaped")],
        vec![file("./escaped")],
        vec![folder("a"), file("a//escaped")],
        // Through a symlink: its folder is never one listed before it.
        vec![link("a", &outside_target), file("a/escaped")],
        vec![folder("a"), link("a", &outside_target)],
        vec![file("a/escaped")],
        vec![
            json!({"path": "x", "kind": "file", "size": 1, "chunks": ["../../escaped"], "mode": 0o644, "mtime": 0, "mtime_nsec": 0}),
        ],
        vec![
            json!({"path": "x", "kind": "file", "size": 1, "chunks": [], "mode": 0o644, "mtime": 0, "mtime_nsec": 0}),
        ],
    ];
    let woken = tmp.path().join("woken");
    for entries in cases {
        let manifest = json!({
            "format": 1, "computer": "evil", "version": version, "chunk_size": CHUNK_SIZE,
            "of": "tree", "root": stamp, "entries": entries,
        });
        blobs
            .put(
                &format!("computers/evil/versions/{version}.json"),
                manifest.to_string().as_bytes(),
            )
            .unwrap();
        let err = store.restore_tree("evil", version, &woken).unwrap_err();
        assert!(err.contains("is refused"), "{entries:?}: {err}");
        assert!(!woken.exists(), "{entries:?}");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "{entries:?}");
    }

    // A symlink pointing outside is made as it was, and never followed.
    let manifest = json!({
        "format": 1, "computer": "evil", "version": version, "chunk_size": CHUNK_SIZE,
        "of": "tree", "root": stamp,
        "entries": [link("out", &outside_target), folder("in"), file("in/x")],
    });
    blobs
        .put(
            &format!("computers/evil/versions/{version}.json"),
            manifest.to_string().as_bytes(),
        )
        .unwrap();
    store.restore_tree("evil", version, &woken).unwrap();
    assert_eq!(fs::read_link(woken.join("out")).unwrap(), outside);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn adr0065_s3_blobs_carry_a_computer_to_s3_and_back() {
    let s3 = StandIn::start();
    let client = S3Client::new(
        &glass_gateway_providers::s3::S3Config {
            kms_key_id: Some("alias/glass-computers".into()),
            ..s3.config()
        },
        StandIn::credentials(),
    )
    .unwrap();
    let store = Store::new(Arc::new(S3Blobs::new(client, "/glasshouse/")));
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("dana");
    fs::create_dir_all(tree.join("work/empty")).unwrap();
    write(&tree.join("work/report.md"), b"# Reconciled\n", 0o644);
    write(
        &tree.join("work/data.bin"),
        &noise(7, CHUNK_SIZE + 10),
        0o600,
    );
    symlink("work/report.md", tree.join("report")).unwrap();

    let saved = store.save_tree("dana", &tree).unwrap();
    assert_eq!(saved.uploaded, saved.bytes);
    let woken = tmp.path().join("woken");
    store.restore_tree("dana", &saved.version, &woken).unwrap();
    assert_eq!(snapshot(&woken), snapshot(&tree));
    assert_eq!(
        store.versions("dana").unwrap(),
        std::slice::from_ref(&saved.version)
    );
    assert_eq!(store.save_tree("dana", &tree).unwrap().uploaded, 0);

    let keys = s3.keys();
    assert_eq!(
        keys.iter()
            .filter(|k| k.starts_with("glasshouse/chunks/"))
            .count(),
        3
    );
    assert_eq!(
        keys.iter()
            .filter(|k| k.starts_with("glasshouse/computers/dana/versions/"))
            .count(),
        2
    );
    assert!(
        keys.iter().all(|k| k.starts_with("glasshouse/")),
        "{keys:?}"
    );
    let seen = s3.seen();
    assert!(seen.iter().all(|r| r.refused.is_none()));
    assert!(
        seen.iter()
            .filter(|r| r.method == "PUT")
            .all(|r| r.header("x-amz-server-side-encryption") == Some("aws:kms"))
    );
}

/// Against S3 and KMS on this machine (deploy/local, moto): the real
/// client, signed with keys from the stand-in metadata service as an agent
/// host's are from its instance role, every object under the stack's KMS key.
#[test]
#[ignore = "against S3 and KMS on this machine: make aws-local-test"]
fn adr0065_local_aws_a_computer_goes_to_s3_and_comes_back_the_same() {
    let var = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| {
            panic!("{name} isn't set: make aws-local, then make aws-local-test")
        })
    };
    let client = || {
        S3Client::new(
            &glass_gateway_providers::s3::S3Config {
                bucket: var("GLASS_LOCAL_BUCKET"),
                region: var("GLASS_LOCAL_REGION"),
                endpoint: Some(var("GLASS_LOCAL_S3")),
                kms_key_id: Some(var("GLASS_LOCAL_KMS_KEY")),
            },
            glass_gateway_providers::s3::S3Credentials::from_host().unwrap(),
        )
        .unwrap()
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let prefix = format!("test-{}-{nanos}/", std::process::id());
    let store = Store::new(Arc::new(S3Blobs::new(client(), &prefix)));
    // Collects at once, with no save under way.
    let collector =
        Store::new(Arc::new(S3Blobs::new(client(), &prefix))).with_grace(Duration::ZERO);
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("dana");
    make_tree(&tree);
    write(
        &tree.join("work.bin"),
        &noise(9, 10 * CHUNK_SIZE + 77),
        0o600,
    );

    let first = store.save_tree("dana", &tree).unwrap();
    assert!(first.uploaded > 10 * CHUNK_SIZE as u64, "{first:?}");
    let woken = tmp.path().join("woken");
    store.restore_tree("dana", &first.version, &woken).unwrap();
    assert_eq!(snapshot(&woken), snapshot(&tree));
    writable(&woken);
    fs::remove_dir_all(&woken).unwrap();

    // Asleep again unchanged: nothing new goes up. One byte changed: one chunk.
    let second = store.save_tree("dana", &tree).unwrap();
    assert_eq!(second.uploaded, 0);
    let file = OpenOptions::new()
        .write(true)
        .open(tree.join("work.bin"))
        .unwrap();
    file.write_at(b"!", 5 * CHUNK_SIZE as u64 + 3).unwrap();
    drop(file);
    let third = store.save_tree("dana", &tree).unwrap();
    assert_eq!(third.uploaded, CHUNK_SIZE as u64);

    // Older versions go, and with them the one chunk only they had.
    store.remove_version("dana", &first.version).unwrap();
    store.remove_version("dana", &second.version).unwrap();
    let collected = collector.gc().unwrap();
    assert_eq!((collected.deleted, collected.freed), (1, CHUNK_SIZE as u64));
    assert_eq!(
        store.versions("dana").unwrap(),
        std::slice::from_ref(&third.version)
    );
    store.restore_tree("dana", &third.version, &woken).unwrap();
    assert_eq!(snapshot(&woken), snapshot(&tree));

    store.remove("dana").unwrap();
    collector.gc().unwrap();
    assert_eq!(client().list(&prefix).unwrap(), []);
    writable(&woken);
    writable(&tree);
}

#[test]
fn adr0065_the_store_folder_writes_whole_objects_and_lists_by_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let blobs = FolderBlobs::new(tmp.path().join("store")).unwrap();
    for key in [
        "chunks/aa",
        "chunks/ab",
        "chunksx",
        "computers/dana/versions/1.json",
    ] {
        blobs.put(key, key.as_bytes()).unwrap();
    }
    blobs.put("chunks/aa", b"again").unwrap();
    assert_eq!(blobs.get("chunks/aa").unwrap().unwrap(), b"again");
    assert_eq!(blobs.get("chunks/zz").unwrap(), None);
    assert!(blobs.exists("chunks/ab").unwrap() && !blobs.exists("chunks/zz").unwrap());
    let keys = |prefix: &str| {
        blobs
            .list(prefix)
            .unwrap()
            .into_iter()
            .map(|l| l.key)
            .collect::<Vec<_>>()
    };
    assert_eq!(keys("chunks/"), ["chunks/aa", "chunks/ab"]);
    assert_eq!(keys("chunks"), ["chunks/aa", "chunks/ab", "chunksx"]);
    assert_eq!(keys("computers/da"), ["computers/dana/versions/1.json"]);
    assert_eq!(keys("nothing/"), Vec::<String>::new());
    assert_eq!(blobs.list("chunks/aa").unwrap()[0].size, 5);
    // No file being written is ever left, or listed.
    assert_eq!(
        fs::read_dir(tmp.path().join("store/chunks"))
            .unwrap()
            .count(),
        2
    );

    blobs.delete("computers/dana/versions/1.json").unwrap();
    blobs.delete("computers/dana/versions/1.json").unwrap();
    assert!(
        !tmp.path().join("store/computers").exists(),
        "no empty folders left"
    );
    for key in ["../x", "/x", "a/.hidden", "", "a//b", "a b"] {
        assert!(blobs.put(key, b"x").is_err(), "{key:?}");
    }
}

#[test]
fn adr0065_versions_are_times_that_sort() {
    let id = version_id(1_793_115_012_123_456_789);
    assert_eq!(id, "20261027T153012.123456789Z");
    assert_eq!(version_nanos(&id), Some(1_793_115_012_123_456_789));
    assert!(valid_version(&id));
    for bad in [
        "",
        "latest",
        "20261027T153012Z",
        "../20261027T153012.123456789Z",
    ] {
        assert!(!valid_version(bad), "{bad}");
    }
    assert!(version_id(1) < version_id(1_000_000_000_000_000_000));
}

#[test]
fn adr0065_a_computers_record_is_kept_beside_it_and_listed_by_its_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _) = folder_store(&tmp.path().join("store"));
    let tree = tmp.path().join("tree");
    make_tree(&tree);
    store.save_tree("u7-aaaa", &tree).unwrap();
    store.put_record("u7-aaaa", b"{\"id\":\"keeper\"}").unwrap();
    store.put_record("u7-bbbb", b"{\"id\":\"other\"}").unwrap();
    store
        .put_record("u70-cccc", b"{\"id\":\"not-u7s\"}")
        .unwrap();

    let mut mine = store.records("u7-").unwrap();
    mine.sort();
    assert_eq!(
        mine,
        [
            ("u7-aaaa".to_owned(), b"{\"id\":\"keeper\"}".to_vec()),
            ("u7-bbbb".to_owned(), b"{\"id\":\"other\"}".to_vec()),
        ]
    );
    // GC reads only manifests; a record isn't one.
    store.gc().unwrap();
    assert_eq!(store.versions("u7-aaaa").unwrap().len(), 1);
    assert!(store.records("u7/../").is_err());
    // Forgetting a computer forgets its record.
    store.remove("u7-aaaa").unwrap();
    assert_eq!(store.records("u7-aaaa").unwrap(), []);
    writable(&tree);
}

/// A folder store that takes its time, as S3 does: what it was asked about,
/// how many chunks were being put at once at the most, and, if told to,
/// one chunk it refuses.
#[derive(Debug, Default)]
struct Slow {
    inner: Option<FolderBlobs>,
    put_takes: std::time::Duration,
    asked: Mutex<Vec<String>>,
    flying: std::sync::atomic::AtomicUsize,
    most: std::sync::atomic::AtomicUsize,
    /// Puts of chunks refused from this one on (counting from 1); 0 for none.
    refuse_from: usize,
    chunk_puts: std::sync::atomic::AtomicUsize,
}

impl Slow {
    fn new(dir: &Path, put_takes_ms: u64) -> Self {
        Self {
            inner: Some(FolderBlobs::new(dir.join("store")).unwrap()),
            put_takes: std::time::Duration::from_millis(put_takes_ms),
            ..Self::default()
        }
    }
    fn inner(&self) -> &FolderBlobs {
        self.inner.as_ref().unwrap()
    }
    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

impl Blobs for Slow {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        use std::sync::atomic::Ordering::SeqCst;
        if !key.starts_with("chunks/") {
            return self.inner().put(key, bytes);
        }
        let now = self.flying.fetch_add(1, SeqCst) + 1;
        self.most.fetch_max(now, SeqCst);
        std::thread::sleep(self.put_takes);
        let nth = self.chunk_puts.fetch_add(1, SeqCst) + 1;
        let result = if self.refuse_from > 0 && nth >= self.refuse_from {
            Err("the store refused it (a test's)".to_owned())
        } else {
            self.inner().put(key, bytes)
        };
        self.flying.fetch_sub(1, SeqCst);
        result
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        self.inner().put_new(key, bytes)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.inner().get(key)
    }
    fn exists(&self, key: &str) -> Result<bool, String> {
        self.inner().exists(key)
    }
    fn modified(&self, key: &str) -> Result<Option<std::time::SystemTime>, String> {
        self.asked.lock().unwrap().push(key.to_owned());
        self.inner().modified(key)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.inner().delete(key)
    }
    fn list(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        self.inner().list(prefix)
    }
}

/// A file of `chunks` chunks, each unlike the others and none of zeros.
fn many_chunks(path: &Path, chunks: usize) {
    use std::io::Write;
    let mut file = fs::File::create(path).unwrap();
    for i in 0..chunks {
        let mut chunk = vec![(i % 250) as u8 + 1; CHUNK_SIZE];
        chunk[..8].copy_from_slice(&(i as u64).to_le_bytes());
        file.write_all(&chunk).unwrap();
    }
}

#[test]
fn adr0066_a_save_doesnt_ask_about_chunks_its_last_version_has() {
    // Audit 7.1: most of a computer is as it last slept, and every chunk
    // of it was a question to S3 that said "yes".
    let tmp = tempfile::tempdir().unwrap();
    let image = tmp.path().join("disk.img");
    many_chunks(&image, 6);
    let blobs = Arc::new(Slow::new(tmp.path(), 0));
    let store = Store::new(blobs.clone());
    let first = store.save_file("dana", &image).unwrap();
    assert_eq!(first.uploaded, 6 * CHUNK_SIZE as u64);
    // Asked about once each the first time: nothing says they're there.
    assert_eq!(blobs.asked().len(), 6);

    // One chunk changes.
    let mut bytes = fs::read(&image).unwrap();
    bytes[2 * CHUNK_SIZE + 100] ^= 0xff;
    fs::write(&image, &bytes).unwrap();
    let second = store.save_file("dana", &image).unwrap();
    assert_eq!(second.uploaded, CHUNK_SIZE as u64);
    let asked = blobs.asked();
    assert_eq!(
        asked.len(),
        1,
        "only the changed chunk is asked about: {asked:?}"
    );

    // Nothing changes: nothing is asked or stored but the manifest.
    let third = store.save_file("dana", &image).unwrap();
    assert_eq!(third.uploaded, 0);
    assert!(blobs.asked().is_empty());

    // And it comes back as it was saved.
    let back = tmp.path().join("back.img");
    store.restore_file("dana", &third.version, &back).unwrap();
    assert_eq!(fs::read(&back).unwrap(), bytes);
    // Another computer's save knows nothing of dana's chunks' being there
    // until it asks: its own last version is what it goes by.
    let other = store.save_file("eli", &image).unwrap();
    assert_eq!(other.uploaded, 0, "the store has them all");
    assert_eq!(blobs.asked().len(), 6);
}

#[test]
fn adr0066_a_saves_chunks_go_up_a_few_at_a_time_and_never_more() {
    let tmp = tempfile::tempdir().unwrap();
    let image = tmp.path().join("disk.img");
    many_chunks(&image, 10);
    let blobs = Arc::new(Slow::new(tmp.path(), 40));
    let store = Store::new(blobs.clone());
    let saved = store.save_file("dana", &image).unwrap();
    assert_eq!(saved.uploaded, 10 * CHUNK_SIZE as u64);
    use std::sync::atomic::Ordering::SeqCst;
    let most = blobs.most.load(SeqCst);
    assert!(
        most > 1 && most <= CHUNKS_AT_ONCE,
        "{most} chunks were being stored at once at the most"
    );
    // None is still on its way once the save has returned.
    assert_eq!(blobs.flying.load(SeqCst), 0);
    let back = tmp.path().join("back.img");
    store.restore_file("dana", &saved.version, &back).unwrap();
    assert_eq!(fs::read(&back).unwrap(), fs::read(&image).unwrap());
}

#[test]
fn adr0066_a_chunk_the_store_refuses_fails_the_save_and_leaves_no_version() {
    let tmp = tempfile::tempdir().unwrap();
    let image = tmp.path().join("disk.img");
    many_chunks(&image, 10);
    let blobs = Arc::new(Slow {
        refuse_from: 3,
        ..Slow::new(tmp.path(), 10)
    });
    let store = Store::new(blobs.clone());
    let failed = store.save_file("dana", &image).unwrap_err();
    assert!(failed.contains("refused"), "{failed}");
    use std::sync::atomic::Ordering::SeqCst;
    // The save stopped sending once a chunk failed, waited for those on
    // their way, and wrote no manifest: there is no version to wake to
    // that names a chunk the store doesn't have.
    assert_eq!(blobs.flying.load(SeqCst), 0);
    assert!(blobs.chunk_puts.load(SeqCst) < 10);
    assert!(store.versions("dana").unwrap().is_empty());
}

// One host at a time per computer (ADR 0066): the lease.

fn at(secs: i64) -> glass_types::Timestamp {
    glass_types::Timestamp::from_unix_seconds(1_800_000_000 + secs).unwrap()
}

fn held(claim: Claim) -> Lease {
    match claim {
        Claim::Held(lease) => lease,
        Claim::Elsewhere(lease) => panic!("{} has it: {lease:?}", lease.host),
    }
}

fn elsewhere(claim: Claim) -> Lease {
    match claim {
        Claim::Elsewhere(lease) => lease,
        Claim::Held(lease) => panic!("it was taken: {lease:?}"),
    }
}

#[test]
fn adr0066_a_computer_is_one_hosts_until_that_host_gives_it_up() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, blobs) = folder_store(tmp.path());
    assert_eq!(store.lease("dana").unwrap(), None);

    let a = held(store.claim("dana", "host-a", at(0), LEASE_FOR).unwrap());
    assert_eq!((a.term, a.host.as_str(), a.released), (1, "host-a", false));
    // Another host is told who has it, and nothing changes.
    let told = elsewhere(store.claim("dana", "host-b", at(5), LEASE_FOR).unwrap());
    assert_eq!(told, a);
    assert_eq!(store.lease("dana").unwrap(), Some(a.clone()));

    // Renewed, it holds on from the renewal.
    let a = store.renew("dana", &a, at(100)).unwrap().unwrap();
    assert_eq!((a.term, a.since, a.renewed), (1, at(0), at(100)));
    let told = elsewhere(store.claim("dana", "host-b", at(219), LEASE_FOR).unwrap());
    assert_eq!(told.renewed, at(100));

    // Given up, it's the next host's at once, as the next term; the terms
    // before it are gone from the store.
    store.release("dana", &a, at(220)).unwrap();
    assert!(store.lease("dana").unwrap().unwrap().released);
    let b = held(store.claim("dana", "host-b", at(221), LEASE_FOR).unwrap());
    assert_eq!((b.term, b.host.as_str()), (2, "host-b"));
    let kept: Vec<String> = blobs
        .list("computers/dana/lease/")
        .unwrap()
        .into_iter()
        .map(|l| l.key)
        .collect();
    assert_eq!(kept, ["computers/dana/lease/00000000000000000002.json"]);
    // A lease is nothing GC reads as a version.
    assert!(store.gc().is_ok());
    assert!(store.versions("dana").unwrap().is_empty());

    // A name that isn't a host's is no host.
    assert!(store.claim("dana", "host b", at(300), LEASE_FOR).is_err());
    assert!(store.claim("dana", "", at(300), LEASE_FOR).is_err());
}

#[test]
fn adr0066_a_lease_runs_out_only_once_its_host_stopped_renewing_for_all_of_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _) = folder_store(tmp.path());
    let a = held(store.claim("dana", "host-a", at(0), LEASE_FOR).unwrap());
    let lease_for = i64::try_from(LEASE_FOR.as_secs()).unwrap();
    // A second short of the whole lease: still host A's.
    elsewhere(
        store
            .claim("dana", "host-b", at(lease_for - 1), LEASE_FOR)
            .unwrap(),
    );
    // Host A went away with it awake. Once the lease has run out another
    // host takes the computer, to wake it as it last slept.
    let b = held(
        store
            .claim("dana", "host-b", at(lease_for), LEASE_FOR)
            .unwrap(),
    );
    assert_eq!(b.term, 2);
    // Host A, back, is told at its next renewal, and when it asks again.
    assert_eq!(store.renew("dana", &a, at(lease_for + 1)).unwrap(), None);
    let told = elsewhere(store.reclaim("dana", "host-a", at(lease_for + 2)).unwrap());
    assert_eq!(told.host, "host-b");
    assert_eq!(store.lease("dana").unwrap().unwrap().host, "host-b");
    // A clock that's behind the renewal reads the lease as holding.
    assert!(b.holds(at(0), LEASE_FOR));
}

#[test]
fn adr0066_a_host_asking_again_keeps_its_computer_only_if_no_other_host_had_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _) = folder_store(tmp.path());
    // Never taken (a computer from before leases): it's the host's.
    let first = held(store.reclaim("dana", "host-a", at(0)).unwrap());
    assert_eq!(first.term, 1);
    // Its glassd started again, or it was out of reach for a while, and
    // nobody took the computer meanwhile: a new term, however long it was.
    let again = held(store.reclaim("dana", "host-a", at(10_000)).unwrap());
    assert_eq!((again.term, again.since), (2, at(10_000)));

    // Out of reach again; host B takes the computer, works on it, puts it
    // to sleep and gives it up. Host A's copy is from before all that: it
    // isn't told "nobody has it, go on", though nobody has.
    let b = held(
        store
            .claim("dana", "host-b", at(20_000), LEASE_FOR)
            .unwrap(),
    );
    store.release("dana", &b, at(20_100)).unwrap();
    let told = elsewhere(store.reclaim("dana", "host-a", at(20_200)).unwrap());
    assert_eq!((told.host.as_str(), told.released), ("host-b", true));
    assert_eq!(
        store.lease("dana").unwrap().unwrap().term,
        3,
        "nothing taken"
    );
    // To wake it from the store, as any host may, it asks as any host does.
    let woke = held(
        store
            .claim("dana", "host-a", at(20_300), LEASE_FOR)
            .unwrap(),
    );
    assert_eq!(woke.term, 4);
    // A lease this host gave up itself isn't one it still has.
    store.release("dana", &woke, at(20_400)).unwrap();
    elsewhere(store.reclaim("dana", "host-a", at(20_500)).unwrap());
}

#[test]
fn adr0066_of_hosts_asking_at_once_one_gets_the_computer() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, _) = folder_store(tmp.path());
    for round in 0..5 {
        let now = at(round * 1000);
        let claims: Vec<Claim> = std::thread::scope(|scope| {
            let asks: Vec<_> = (0..8)
                .map(|n| {
                    let store = &store;
                    scope.spawn(move || {
                        store
                            .claim("dana", &format!("host-{n}"), now, LEASE_FOR)
                            .unwrap()
                    })
                })
                .collect();
            asks.into_iter().map(|ask| ask.join().unwrap()).collect()
        });
        let winners: Vec<&Lease> = claims
            .iter()
            .filter_map(|claim| match claim {
                Claim::Held(lease) => Some(lease),
                Claim::Elsewhere(_) => None,
            })
            .collect();
        assert_eq!(winners.len(), 1, "round {round}: {claims:?}");
        // Everyone else was told it's the winner's.
        for claim in &claims {
            if let Claim::Elsewhere(lease) = claim {
                assert_eq!(&lease, winners.first().unwrap());
            }
        }
        store.release("dana", winners[0], now).unwrap();
    }
    // One host asking twice at once (keeping its leases, and a sleep) has
    // the one term both times.
    let both: Vec<Lease> = std::thread::scope(|scope| {
        let asks: Vec<_> = (0..2)
            .map(|_| {
                let store = &store;
                scope.spawn(move || held(store.reclaim("solo", "host-a", at(0)).unwrap()))
            })
            .collect();
        asks.into_iter().map(|ask| ask.join().unwrap()).collect()
    });
    assert_eq!(both[0], both[1]);
}

#[test]
fn adr0066_a_lease_that_cant_be_read_is_nobodys_to_take() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, blobs) = folder_store(tmp.path());
    held(store.claim("dana", "host-a", at(0), LEASE_FOR).unwrap());
    blobs
        .put("computers/dana/lease/00000000000000000001.json", b"{")
        .unwrap();
    let failed = store
        .claim("dana", "host-b", at(100_000), LEASE_FOR)
        .unwrap_err();
    assert!(failed.contains("nobody takes this computer"), "{failed}");
    // Nor one that says it's another term than its name.
    let other = serde_json::to_vec(&Lease {
        term: 7,
        host: "host-a".into(),
        since: at(0),
        renewed: at(0),
        released: true,
    })
    .unwrap();
    blobs
        .put("computers/dana/lease/00000000000000000001.json", &other)
        .unwrap();
    assert!(store.reclaim("dana", "host-a", at(1)).is_err());
}

#[test]
fn adr0066_s3_lets_one_host_take_a_computer() {
    let s3 = StandIn::start();
    let store = Store::new(Arc::new(S3Blobs::new(s3.client(), "/glasshouse/")));
    let claims: Vec<Claim> = std::thread::scope(|scope| {
        let asks: Vec<_> = (0..6)
            .map(|n| {
                let store = &store;
                scope.spawn(move || {
                    store
                        .claim("dana", &format!("i-0host{n}"), at(0), LEASE_FOR)
                        .unwrap()
                })
            })
            .collect();
        asks.into_iter().map(|ask| ask.join().unwrap()).collect()
    });
    let mine: Vec<Lease> = claims
        .into_iter()
        .filter_map(|claim| match claim {
            Claim::Held(lease) => Some(lease),
            Claim::Elsewhere(_) => None,
        })
        .collect();
    assert_eq!(mine.len(), 1);
    let seen = s3.seen();
    assert!(seen.iter().all(|r| r.refused.is_none()), "{seen:?}");
    // Every take was a put that S3 was told to refuse if the term is
    // there. (A host that asked after the winner was through never put.)
    let takes: Vec<_> = seen
        .iter()
        .filter(|r| r.method == "PUT" && r.target.contains("/lease/"))
        .collect();
    assert!(!takes.is_empty());
    assert!(takes.iter().all(|r| r.header("if-none-match") == Some("*")));

    let renewed = store.renew("dana", &mine[0], at(30)).unwrap().unwrap();
    store.release("dana", &renewed, at(60)).unwrap();
    let next = held(store.claim("dana", "i-0other", at(61), LEASE_FOR).unwrap());
    assert_eq!(next.term, 2);
    assert_eq!(
        s3.keys()
            .into_iter()
            .filter(|k| k.contains("/lease/"))
            .collect::<Vec<_>>(),
        ["glasshouse/computers/dana/lease/00000000000000000002.json"]
    );
}

// GC and saves (audit 5.3): chunks no version names are collected, and a
// save never ends up naming one that was.

type Hook = Mutex<Option<Box<dyn FnOnce() + Send>>>;

/// A store in which something happens, once, at a chosen moment: just
/// before a version is written (a save's last step), or just after the
/// chunks are listed (GC's first).
#[derive(Default)]
struct Meanwhile {
    inner: Option<Arc<dyn Blobs>>,
    before_version: Hook,
    reading_a_version: Hook,
    after_listing_chunks: Hook,
}

impl std::fmt::Debug for Meanwhile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Meanwhile")
    }
}

impl Meanwhile {
    fn over(inner: Arc<dyn Blobs>) -> Arc<Self> {
        Arc::new(Self {
            inner: Some(inner),
            ..Self::default()
        })
    }
    fn inner(&self) -> &dyn Blobs {
        self.inner.as_deref().unwrap()
    }
    fn run(hook: &Hook) {
        let hook = hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl Blobs for Meanwhile {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        if key.contains("/versions/") {
            Self::run(&self.before_version);
        }
        self.inner().put(key, bytes)
    }
    fn put_hashed(&self, key: &str, bytes: &[u8], sha256: &str) -> Result<(), String> {
        self.inner().put_hashed(key, bytes, sha256)
    }
    fn put_new(&self, key: &str, bytes: &[u8]) -> Result<bool, String> {
        self.inner().put_new(key, bytes)
    }
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        if key.contains("/versions/") {
            Self::run(&self.reading_a_version);
        }
        self.inner().get(key)
    }
    fn exists(&self, key: &str) -> Result<bool, String> {
        self.inner().exists(key)
    }
    fn modified(&self, key: &str) -> Result<Option<std::time::SystemTime>, String> {
        self.inner().modified(key)
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.inner().delete(key)
    }
    fn delete_listed(&self, listed: &Listed) -> Result<bool, String> {
        self.inner().delete_listed(listed)
    }
    fn list(&self, prefix: &str) -> Result<Vec<Listed>, String> {
        let listed = self.inner().list(prefix);
        if prefix == "chunks/" {
            Self::run(&self.after_listing_chunks);
        }
        listed
    }
}

/// Makes every chunk in the folder store look written `ago` ago.
fn age_chunks(blobs: &FolderBlobs, ago: Duration) {
    let then = std::time::SystemTime::now() - ago;
    for listed in blobs.list("chunks/").unwrap() {
        fs::File::options()
            .write(true)
            .open(blobs.root().join(&listed.key))
            .unwrap()
            .set_modified(then)
            .unwrap();
    }
}

#[test]
fn adr0066_gc_in_the_middle_of_a_save_takes_nothing_the_save_names() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = Arc::new(FolderBlobs::new(tmp.path().join("store")).unwrap());
    let image = tmp.path().join("disk.img");
    many_chunks(&image, 6);

    // A computer slept once and was removed since: its chunks are in the
    // store, two days old, and no version names them.
    let plain = Store::new(folder.clone());
    plain.save_file("gone", &image).unwrap();
    plain.remove("gone").unwrap();
    age_chunks(&folder, Duration::from_secs(2 * 24 * 3600));
    assert_eq!(chunk_count(folder.as_ref()), 6);

    // Another computer with the same bytes sleeps, and GC runs after its
    // chunks are in and before its version is written: the moment a save
    // that relied on those old chunks lost them.
    let meanwhile = Meanwhile::over(folder.clone());
    *meanwhile.before_version.lock().unwrap() = Some(Box::new({
        let collector = Store::new(folder.clone());
        move || {
            let collected = collector.gc().unwrap();
            assert_eq!(
                (collected.deleted, collected.kept),
                (0, 6),
                "sent again by the save, they are new"
            );
        }
    }));
    let store = Store::new(meanwhile.clone());
    let saved = store.save_file("dana", &image).unwrap();
    assert_eq!(saved.uploaded, fs::metadata(&image).unwrap().len());
    assert!(meanwhile.before_version.lock().unwrap().is_none(), "GC ran");
    let back = tmp.path().join("back.img");
    store.restore_file("dana", &saved.version, &back).unwrap();
    assert_eq!(fs::read(&back).unwrap(), fs::read(&image).unwrap());

    // A third computer with the same bytes, a moment later: the chunks
    // are in the store and young, and none is sent again.
    let counting = Arc::new(Counting {
        inner: FolderBlobs::new(tmp.path().join("store")).unwrap(),
        puts: Mutex::new(Vec::new()),
    });
    let third = Store::new(counting.clone())
        .save_file("sam", &image)
        .unwrap();
    assert_eq!(third.uploaded, 0);
    assert!(
        counting
            .taken()
            .iter()
            .all(|key| !key.starts_with("chunks/")),
        "only its version was written"
    );

    // What no version names and is a day old goes; what one names stays.
    plain.remove("dana").unwrap();
    plain.remove("sam").unwrap();
    plain.save_file("kept", &image).unwrap();
    let other = tmp.path().join("other.img");
    write(&other, &noise(77, CHUNK_SIZE + 5), 0o600);
    let orphan = plain.save_file("orphan", &other).unwrap();
    plain.remove("orphan").unwrap();
    assert!(orphan.uploaded > 0);
    age_chunks(&folder, Duration::from_secs(2 * 24 * 3600));
    let collected = plain.gc().unwrap();
    assert_eq!((collected.deleted, collected.kept), (2, 6));
    assert_eq!(collected.freed, (CHUNK_SIZE + 5) as u64);
    let back = tmp.path().join("kept.img");
    let kept = plain.latest("kept").unwrap().unwrap();
    plain.restore_file("kept", &kept, &back).unwrap();
    assert_eq!(fs::read(&back).unwrap(), fs::read(&image).unwrap());
}

#[test]
fn adr0066_a_chunk_sent_again_after_gc_listed_it_stays() {
    // In a folder (the stand-in), and in S3, where the delete itself is
    // refused: the object is not the one that was listed.
    let s3 = StandIn::start();
    let tmp = tempfile::tempdir().unwrap();
    let stores: [(&str, Arc<dyn Blobs>); 2] = [
        (
            "a folder",
            Arc::new(FolderBlobs::new(tmp.path().join("store")).unwrap()),
        ),
        ("S3", Arc::new(S3Blobs::new(s3.client(), "/glasshouse/"))),
    ];
    for (name, blobs) in stores {
        let bytes = noise(5, 1000);
        let sha = hex::encode(Sha256::digest(&bytes));
        let key = format!("chunks/{sha}");
        blobs.put_hashed(&key, &bytes, &sha).unwrap();
        let meanwhile = Meanwhile::over(blobs.clone());
        // No version names it and it is old enough (no grace here). Then,
        // between GC's listing and its delete, a save sends it again.
        *meanwhile.after_listing_chunks.lock().unwrap() = Some(Box::new({
            let (blobs, key, bytes, sha) = (blobs.clone(), key.clone(), bytes.clone(), sha.clone());
            move || {
                std::thread::sleep(Duration::from_millis(20));
                blobs.put_hashed(&key, &bytes, &sha).unwrap();
            }
        }));
        let collector = Store::new(meanwhile.clone()).with_grace(Duration::ZERO);
        let collected = collector.gc().unwrap();
        assert_eq!((collected.deleted, collected.freed), (0, 0), "{name}");
        assert_eq!(blobs.get(&key).unwrap().unwrap(), bytes, "{name}");
        // Left alone since, it goes at the next pass.
        let collected = collector.gc().unwrap();
        assert_eq!(collected.deleted, 1, "{name}");
        assert!(blobs.get(&key).unwrap().is_none(), "{name}");
    }
    let seen = s3.seen();
    assert!(seen.iter().all(|r| r.refused.is_none()), "{seen:?}");
    let deletes: Vec<_> = seen.iter().filter(|r| r.method == "DELETE").collect();
    assert_eq!(deletes.len(), 2);
    assert!(deletes.iter().all(|r| r.header("if-match").is_some()));
}

// specs/tla/ChunkStore.tla ("ageAtListing"): GC judged a chunk's age after
// it had read the versions. A computer that slept while it read them, in
// a pass slow enough, had its chunks found old and named by no version.
#[test]
fn adr0066_a_chunk_written_while_gc_reads_the_versions_is_young_to_that_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = Arc::new(FolderBlobs::new(tmp.path().join("store")).unwrap());
    let plain = Store::new(folder.clone());
    let first = tmp.path().join("first.img");
    many_chunks(&first, 2);
    plain.save_file("first", &first).unwrap();
    let second = tmp.path().join("second.img");
    write(&second, &noise(31, CHUNK_SIZE + 9), 0o600);

    // GC, with a grace of a tenth of a second, is reading the versions
    // when another computer sleeps; the pass then takes three times its
    // grace to get to the chunks.
    let meanwhile = Meanwhile::over(folder.clone());
    *meanwhile.reading_a_version.lock().unwrap() = Some(Box::new({
        let (saver, second) = (Store::new(folder.clone()), second.clone());
        move || {
            saver.save_file("second", &second).unwrap();
            std::thread::sleep(Duration::from_millis(300));
        }
    }));
    let collector = Store::new(meanwhile.clone()).with_grace(Duration::from_millis(100));
    std::thread::sleep(Duration::from_millis(150));
    let collected = collector.gc().unwrap();
    assert!(meanwhile.reading_a_version.lock().unwrap().is_none());
    assert_eq!(collected.deleted, 0, "{collected:?}");
    let back = tmp.path().join("back.img");
    let version = plain.latest("second").unwrap().unwrap();
    plain.restore_file("second", &version, &back).unwrap();
    assert_eq!(fs::read(&back).unwrap(), fs::read(&second).unwrap());
}
