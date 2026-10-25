//! What a computer asks the kernel patch build service, and what it answers
//! (ADR 0057, slice 2): the base system's source of the computer's release,
//! read and searched, and a patch an agent wrote, built into a signed
//! bundle or answered with the compiler's errors.
//!
//! The source and the compiler are the service's, never the computer's:
//! nothing is compiled on a person's computer (BS-4). An agent's patch is
//! signed as a community patch, `origin` `agent:<task>`: nobody here read
//! it, so a computer takes it only if it takes community patches, and only
//! with the person's yes (`hardware.patch_propose`, every time).

use serde::{Deserialize, Serialize};

use crate::hwpatch::{Bundle, valid_id};

/// The most of a diff the service takes.
pub const MAX_DIFF_BYTES: usize = 512 * 1024;
/// The most matches a search answers with.
pub const MAX_MATCHES: usize = 200;
/// The most lines a read answers with.
pub const MAX_LINES: u32 = 400;

/// `POST /v1/source/search`: lines matching `pattern` (an extended regular
/// expression) under `sys/<path>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSearch {
    pub release: String,
    pub pattern: String,
    /// Under `sys/`: `dev/iwlwifi`, or empty for all of it.
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMatch {
    /// From `sys/`: `sys/dev/iwlwifi/mvm/ops.c`.
    pub file: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFound {
    pub matches: Vec<SourceMatch>,
    /// More matched than [`MAX_MATCHES`].
    #[serde(default)]
    pub truncated: bool,
}

/// `POST /v1/source/read`: lines `from` (1-based) on of `sys/<path>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRead {
    pub release: String,
    pub path: String,
    #[serde(default = "first")]
    pub from: u32,
    #[serde(default = "most")]
    pub lines: u32,
}

fn first() -> u32 {
    1
}

fn most() -> u32 {
    MAX_LINES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceText {
    pub path: String,
    pub from: u32,
    pub text: String,
    pub total_lines: u32,
}

/// `POST /v1/build`: a patch to build for `release`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRequest {
    pub release: String,
    pub id: String,
    /// What it fixes, for a person.
    pub summary: String,
    /// A unified diff against the release's source, of files under `sys/`.
    pub diff: String,
    /// The modules it rebuilds, by their folder under `sys/modules`.
    #[serde(default)]
    pub modules: Vec<BuildModule>,
    /// It changes code built into the kernel: the kernel is rebuilt too.
    #[serde(default)]
    pub kernel: bool,
    /// What it's for: `pci:8086:2725`.
    #[serde(default)]
    pub targets: Vec<String>,
    /// `agent:<task>`.
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildModule {
    pub dir: String,
    pub file: String,
    #[serde(default)]
    pub load_at_boot: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildResult {
    pub ok: bool,
    /// The compiler's errors, or why it wasn't built; what was built when
    /// it was.
    pub log: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<Bundle>,
}

/// A path under `sys/` that names nothing outside it.
pub fn valid_path(path: &str) -> bool {
    path.len() <= 200
        && !path.starts_with('/')
        && path.split('/').all(|part| part != ".." && part != ".")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+'))
}

impl BuildRequest {
    /// What's wrong with it before anything is built, if anything.
    pub fn check(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!(
                "{:?} isn't a patch id: lowercase letters, digits and -",
                self.id
            ));
        }
        if self.summary.trim().is_empty() || self.summary.len() > 200 {
            return Err("a summary of up to 200 characters says what it fixes".into());
        }
        if self.diff.len() > MAX_DIFF_BYTES {
            return Err(format!("the diff is over {MAX_DIFF_BYTES} bytes"));
        }
        let files: Vec<&str> = self
            .diff
            .lines()
            .filter_map(|l| l.strip_prefix("+++ b/"))
            .map(|f| f.split_whitespace().next().unwrap_or(""))
            .collect();
        if files.is_empty() {
            return Err("the diff changes no file (it needs `+++ b/sys/...` lines)".into());
        }
        if let Some(f) = files
            .iter()
            .find(|f| !f.starts_with("sys/") || !valid_path(f))
        {
            return Err(format!("{f}: a patch changes files under sys/ only"));
        }
        if self.modules.is_empty() && !self.kernel {
            return Err("it rebuilds nothing: name its modules, or the kernel".into());
        }
        for m in &self.modules {
            if !valid_path(&m.dir) || m.dir.is_empty() {
                return Err(format!("{:?} isn't a folder under sys/modules", m.dir));
            }
            if !crate::hwpatch::valid_module_file(&m.file) || m.file == crate::hwpatch::KERNEL {
                return Err(format!("{:?} isn't a module's file name", m.file));
            }
        }
        if !self.origin.starts_with("agent:") && self.origin != "glasshouse" {
            return Err("origin is agent:<task>".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> BuildRequest {
        BuildRequest {
            release: "15.1-RELEASE".into(),
            id: "iwlwifi-resume".into(),
            summary: "Wi-Fi works again after sleep".into(),
            diff: "--- a/sys/dev/iwlwifi/mvm/ops.c\n+++ b/sys/dev/iwlwifi/mvm/ops.c\n@@ -1 +1 @@\n-a\n+b\n"
                .into(),
            modules: vec![BuildModule {
                dir: "iwlwifi".into(),
                file: "if_iwlwifi.ko".into(),
                load_at_boot: false,
            }],
            kernel: false,
            targets: vec![],
            origin: "agent:9b1f".into(),
        }
    }

    #[test]
    fn adr0057_a_build_request_stays_in_the_kernel_s_source() {
        assert_eq!(request().check(), Ok(()));
        let bad = |edit: fn(&mut BuildRequest)| {
            let mut r = request();
            edit(&mut r);
            r.check().unwrap_err()
        };
        assert!(
            bad(|r| r.diff = r.diff.replace("sys/dev", "etc/../sys/dev")).contains("under sys/")
        );
        assert!(bad(|r| r.diff = r.diff.replace("b/sys/dev", "b/usr/bin")).contains("under sys/"));
        assert!(bad(|r| r.diff = "nothing".into()).contains("changes no file"));
        assert!(bad(|r| r.modules[0].dir = "../../etc".into()).contains("sys/modules"));
        assert!(bad(|r| r.modules[0].file = "kernel".into()).contains("module"));
        assert!(bad(|r| r.modules.clear()).contains("rebuilds nothing"));
        assert!(bad(|r| r.id = "Wi-Fi!".into()).contains("patch id"));
        assert!(bad(|r| r.origin = "me".into()).contains("origin"));
        assert!(valid_path("dev/iwlwifi/mvm"));
        assert!(!valid_path("/etc/passwd"));
        assert!(!valid_path("dev/../../etc"));
    }
}
