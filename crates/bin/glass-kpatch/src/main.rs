//! `glass-kpatch`: the build service's side of kernel patches (ADR 0057).
//!
//! - `keygen` makes the key the build service signs bundles with, and
//!   prints its public half for computers' `[hardware] keys`.
//! - `bundle` signs a patch (its `patch.toml` and `patch.diff`) with the
//!   modules built from it into one bundle, which `glassctl hardware patch
//!   apply` takes.
//!
//! Building the modules is `os/freebsd/scripts/kernel-patch.sh`'s, on a
//! computer running the release they're for. Never on the person's
//! computer (BS-4).

#![allow(clippy::print_stdout)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use glass_managed::hwpatch::{Bundle, GLASSHOUSE, Manifest, Module, Review};
use glass_managed::{DocKind, OrgKey, base64, sha256_hex};
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(
    name = "glass-kpatch",
    about = "Sign kernel patches and their modules (ADR 0057)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Make a signing key; prints its public half.
    Keygen {
        /// Where the key's seed goes (mode 0600). Refuses to replace one.
        #[arg(long)]
        out: PathBuf,
    },
    /// Print a signing key's public half.
    Public {
        #[arg(long)]
        key: PathBuf,
    },
    /// Sign a patch and the modules built from it into one bundle.
    Bundle {
        /// The patch's folder: `patch.toml` and `patch.diff`.
        patch: PathBuf,
        /// The modules built from it, by their file names.
        #[arg(long)]
        modules: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

/// `patch.toml`: what a patch is, kept beside it in the repo.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchFile {
    id: String,
    revision: u64,
    summary: String,
    release: String,
    origin: String,
    review: Review,
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    requirements: Vec<String>,
    /// The kept patches it was built on top of (ADR 0057): a computer
    /// keeping one that rewrites the same files takes this one only if it
    /// names that one here.
    #[serde(default)]
    includes: Vec<String>,
    /// Times a computer must wake from sleep on it before keeping it: for
    /// a patch to sleep or a driver's resume (ADR 0057).
    #[serde(default)]
    resumes: u32,
    /// The patch changes code the kernel is built with: the kernel is
    /// rebuilt with the image's configuration and carried too.
    #[serde(default)]
    kernel: bool,
    #[serde(default, rename = "module")]
    modules: Vec<ModuleFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleFile {
    /// Its folder under `sys/modules`, which the build script builds.
    #[allow(dead_code)]
    dir: String,
    /// The module it builds: `if_iwlwifi.ko`.
    file: String,
    #[serde(default)]
    load_at_boot: bool,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Keygen { out } => {
            let key = OrgKey::from_seed(glass_crypto::random::<32>()?);
            write_new(&out, key.seed_hex().as_bytes())?;
            println!("{}", key.public());
            Ok(())
        }
        Command::Public { key } => {
            println!("{}", read_key(&key)?.public());
            Ok(())
        }
        Command::Bundle {
            patch,
            modules,
            key,
            out,
        } => {
            let key = read_key(&key)?;
            let bundle = bundle(&patch, &modules, &key)?;
            std::fs::write(&out, serde_json::to_vec(&bundle)?)
                .with_context(|| out.display().to_string())?;
            println!(
                "{} r{}: {} module(s), signed by {}",
                out.display(),
                bundle.signed.version,
                bundle.modules.len(),
                key.public().id()
            );
            Ok(())
        }
    }
}

fn bundle(dir: &Path, modules_dir: &Path, key: &OrgKey) -> anyhow::Result<Bundle> {
    let file: PatchFile =
        toml::from_str(&std::fs::read_to_string(dir.join("patch.toml")).context("patch.toml")?)
            .context("patch.toml")?;
    let patch = std::fs::read_to_string(dir.join("patch.diff")).context("patch.diff")?;
    let mut modules = BTreeMap::new();
    let mut listed = Vec::new();
    let kernel = file.kernel.then(|| ModuleFile {
        dir: String::new(),
        file: glass_managed::hwpatch::KERNEL.into(),
        load_at_boot: false,
    });
    for m in kernel.iter().chain(&file.modules) {
        if !glass_managed::hwpatch::valid_module_file(&m.file) {
            bail!("{:?} isn't a module's file name", m.file);
        }
        let bytes = std::fs::read(modules_dir.join(&m.file))
            .with_context(|| format!("{} wasn't built", m.file))?;
        if !bytes.starts_with(b"\x7fELF") {
            bail!("{} isn't a module", m.file);
        }
        listed.push(Module {
            file: m.file.clone(),
            sha256: sha256_hex(&bytes),
            load_at_boot: m.load_at_boot,
        });
        modules.insert(m.file.clone(), base64::encode(&bytes));
    }
    let manifest = Manifest {
        id: file.id,
        summary: file.summary,
        release: file.release,
        origin: file.origin,
        review: file.review,
        targets: file.targets,
        modules: listed,
        patch_sha256: sha256_hex(patch.as_bytes()),
        requirements: file.requirements,
        includes: file.includes,
        resumes: file.resumes,
    };
    if !glass_managed::hwpatch::valid_id(&manifest.id) {
        bail!("{:?} isn't a patch id", manifest.id);
    }
    let payload = serde_json::to_string(&manifest)?;
    Ok(Bundle {
        signed: key.sign(DocKind::HardwarePatch, GLASSHOUSE, file.revision, payload),
        patch,
        modules,
    })
}

fn read_key(path: &Path) -> anyhow::Result<OrgKey> {
    OrgKey::from_seed_hex(
        &std::fs::read_to_string(path).with_context(|| path.display().to_string())?,
    )
    .map_err(anyhow::Error::msg)
}

/// Writes a new file for root alone; never over one already there.
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

    /// What this tool signs, a computer opens: the build service's bundle
    /// and glassd's check agree (ADR 0057).
    #[test]
    fn adr0057_a_bundle_signed_here_opens_on_a_computer_that_trusts_the_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("patch.toml"),
            r#"id = "lkpi-wlan-suspend"
revision = 1
summary = "Wi-Fi works again after sleep"
release = "15.1-RELEASE"
origin = "https://cgit.freebsd.org/src/commit/?id=0c0f6654"
review = "reviewed"
targets = ["pci:8086:2725"]

[[module]]
dir = "iwlwifi"
file = "if_iwlwifi.ko"
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("patch.diff"), "--- a/sys/x\n+++ b/sys/x\n").unwrap();
        let built = dir.path().join("built");
        std::fs::create_dir(&built).unwrap();
        std::fs::write(built.join("if_iwlwifi.ko"), b"\x7fELF module").unwrap();

        let key = OrgKey::from_seed([9; 32]);
        let bundle = bundle(dir.path(), &built, &key).unwrap();
        let opened = bundle.open(GLASSHOUSE, &[key.public()], None).unwrap();
        assert_eq!(opened.manifest.id, "lkpi-wlan-suspend");
        assert_eq!(opened.modules[0].1, b"\x7fELF module");

        // Something that isn't a module isn't signed as one.
        std::fs::write(built.join("if_iwlwifi.ko"), b"#!/bin/sh").unwrap();
        assert!(super::bundle(dir.path(), &built, &key).is_err());
    }
}
