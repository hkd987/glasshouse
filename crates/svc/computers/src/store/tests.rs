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
