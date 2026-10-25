//! `glass-release`: the release side of OS updates (ADR 0061; EP-11,
//! EP-12, BS-4).
//!
//! - `keygen` makes a key (the root, the OS key, a channel's), and prints
//!   its public half. The root's goes in images' `[update] roots`.
//! - `keys` signs the root's key list: the keys in use, by purpose, and
//!   those revoked.
//! - `release` signs an image's digest with the OS key and the release with
//!   its channel's key, and puts both beside the image in a channel's
//!   folder: what `glassctl update` reads.
//!
//! The keys made here are development keys. The release pipeline's live in
//! hardware security modules, used in a recorded ceremony (EP-11).

#![allow(clippy::print_stdout)]

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use glass_managed::hwpatch::GLASSHOUSE;
use glass_managed::osupdate::{ImageDigest, KeyList, ListedKey, PatchRebuild, Release};
use glass_managed::{DocKind, OrgKey, PublicKey};

#[derive(Debug, Parser)]
#[command(name = "glass-release", about = "Sign OS updates (ADR 0061)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// Parsed once from the command line: its size doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
enum Command {
    /// Make a key; prints its public half.
    Keygen {
        /// Where the key's seed goes (mode 0600). Refuses to replace one.
        #[arg(long)]
        out: PathBuf,
    },
    /// Print a key's public half.
    Public {
        #[arg(long)]
        key: PathBuf,
    },
    /// The root's key list, signed: `--key os=<public>`,
    /// `--key channel:pilot=<public>`, as many as there are.
    Keys {
        #[arg(long)]
        root: PathBuf,
        /// Goes up with every list.
        #[arg(long)]
        version: u64,
        #[arg(long = "key", value_name = "PURPOSE=PUBLIC")]
        keys: Vec<String>,
        /// Public keys no longer trusted for anything.
        #[arg(long = "revoke", value_name = "PUBLIC")]
        revoked: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Sign a release of `image` into `<source>/<channel>/`: the image
    /// copied there, its digest signed by the OS key, the release by the
    /// channel's key. With `--sha256` and `--bytes`, the image is already
    /// there (made where the source is) and only its digest is signed.
    Release {
        image: PathBuf,
        #[arg(long, requires = "bytes")]
        sha256: Option<String>,
        #[arg(long, requires = "sha256")]
        bytes: Option<u64>,
        /// The image is a delta: the changes since this version's image,
        /// which the computer must run.
        #[arg(long)]
        delta_from: Option<String>,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        channel: String,
        /// The release number: above every one before on the channel.
        #[arg(long)]
        number: u64,
        /// For people: `2026.11.1`. The image's os-release.json says the
        /// same.
        #[arg(long)]
        version: String,
        /// The base release the image is built on.
        #[arg(long)]
        base: String,
        #[arg(long)]
        os_key: PathBuf,
        #[arg(long)]
        channel_key: PathBuf,
        /// Kernel patches whose fix the image has.
        #[arg(long = "fixes")]
        fixes: Vec<String>,
        /// A kernel patch's rebuild for this image: `<id>=<bundle file>`.
        #[arg(long = "patch", value_name = "ID=BUNDLE")]
        patches: Vec<String>,
        #[arg(long, default_value = "")]
        notes: String,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Keygen { out } => {
            let key = OrgKey::from_seed(glass_crypto::random::<32>()?);
            write_new(&out, key.seed_hex().as_bytes())?;
            println!("{}", key.public());
        }
        Command::Public { key } => println!("{}", read_key(&key)?.public()),
        Command::Keys {
            root,
            version,
            keys,
            revoked,
            out,
        } => {
            let list = key_list(&keys, &revoked)?;
            let signed = read_key(&root)?.sign(
                DocKind::OsKeys,
                GLASSHOUSE,
                version,
                serde_json::to_string(&list)?,
            );
            std::fs::write(&out, serde_json::to_vec_pretty(&signed)?)
                .with_context(|| out.display().to_string())?;
            println!(
                "{} v{version}: {} key(s), {} revoked",
                out.display(),
                list.keys.len(),
                list.revoked.len()
            );
        }
        Command::Release {
            image,
            sha256,
            bytes,
            delta_from,
            source,
            channel,
            number,
            version,
            base,
            os_key,
            channel_key,
            fixes,
            patches,
            notes,
        } => {
            let dir = source.join(&channel);
            std::fs::create_dir_all(&dir).with_context(|| dir.display().to_string())?;
            let file = format!(
                "glasshouse-{version}{}",
                glass_managed::osupdate::IMAGE_SUFFIX
            );
            let (sha256, bytes) = match (sha256, bytes) {
                (Some(sha256), Some(bytes)) => (sha256, bytes),
                _ => {
                    let digested = digest(&image)?;
                    let to = dir.join(&file);
                    if std::fs::canonicalize(&image).ok() != std::fs::canonicalize(&to).ok() {
                        std::fs::copy(&image, &to).with_context(|| to.display().to_string())?;
                    }
                    digested
                }
            };
            let digest = ImageDigest {
                file,
                sha256,
                bytes,
                version: version.clone(),
                delta_from,
            };
            let release = Release {
                channel: channel.clone(),
                version: version.clone(),
                base,
                image: read_key(&os_key)?.sign(
                    DocKind::OsImage,
                    GLASSHOUSE,
                    number,
                    serde_json::to_string(&digest)?,
                ),
                fixes,
                patches: patches
                    .iter()
                    .map(|p| {
                        let (id, bundle) = p
                            .split_once('=')
                            .with_context(|| format!("{p}: <id>=<bundle file>"))?;
                        Ok(PatchRebuild {
                            id: id.into(),
                            bundle: bundle.into(),
                        })
                    })
                    .collect::<anyhow::Result<_>>()?,
                notes,
            };
            let signed = read_key(&channel_key)?.sign(
                DocKind::OsRelease,
                GLASSHOUSE,
                number,
                serde_json::to_string(&release)?,
            );
            let out = dir.join("release.json");
            std::fs::write(&out, serde_json::to_vec_pretty(&signed)?)
                .with_context(|| out.display().to_string())?;
            println!(
                "{}: {version} (release {number}) on {channel}, {bytes} bytes",
                out.display()
            );
        }
    }
    Ok(())
}

fn key_list(keys: &[String], revoked: &[String]) -> anyhow::Result<KeyList> {
    let keys = keys
        .iter()
        .map(|k| {
            let (purpose, key) = k
                .split_once('=')
                .with_context(|| format!("{k}: <purpose>=<public key>"))?;
            if purpose != "os" && !purpose.starts_with("channel:") {
                bail!("{purpose}: os, or channel:<name>");
            }
            Ok(ListedKey {
                purpose: purpose.into(),
                key: key.parse().map_err(anyhow::Error::msg)?,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    let revoked = revoked
        .iter()
        .map(|k| {
            k.parse::<PublicKey>()
                .map(|k| k.id())
                .map_err(anyhow::Error::msg)
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(KeyList { keys, revoked })
}

/// The image's SHA-256 and size, read as a stream.
fn digest(path: &Path) -> anyhow::Result<(String, u64)> {
    let mut file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((hex::encode(hasher.finalize()), total))
}

fn read_key(path: &Path) -> anyhow::Result<OrgKey> {
    OrgKey::from_seed_hex(
        std::fs::read_to_string(path)
            .with_context(|| path.display().to_string())?
            .trim(),
    )
    .map_err(anyhow::Error::msg)
}

/// Writes a new file for its owner alone; never over one already there.
fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| path.display().to_string())?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0061_a_key_list_takes_only_known_purposes() {
        let key = OrgKey::from_seed([2; 32]).public().to_string();
        let list = key_list(&[format!("os={key}"), format!("channel:pilot={key}")], &[]).unwrap();
        assert_eq!(list.keys.len(), 2);
        assert!(key_list(&[format!("console={key}")], &[]).is_err());
        let revoked = key_list(&[], std::slice::from_ref(&key)).unwrap();
        assert_eq!(
            revoked.revoked,
            vec![key.parse::<PublicKey>().unwrap().id()]
        );
    }

    #[test]
    fn adr0061_an_image_is_digested_as_a_stream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.zfs");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            digest(&path).unwrap(),
            (
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
                3
            )
        );
    }
}
