//! Bundling an app's checked source into what the bridge serves (ADR 0032):
//! esbuild, as the apps workspace builds ours (`apps/build.mjs`), run once
//! in a folder of its own ([`ToolRunner::run_tool`]): in a jail on the
//! computer, in its own namespaces on our servers, with no network, for a
//! minute at most.
//!
//! The folder holds the app's source under [`APP_DIR`] and a copy of the
//! SDK under [`SDK_DIR`], with `node_modules/@glasshouse/sdk` pointing at
//! it, so `import … from "@glasshouse/sdk"` and the first-party apps'
//! `@import "../../sdk/src/base.css"` both resolve. The bundler's own list
//! of what it read ([`crate::checks::check_inputs`]) must name nothing
//! else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use glass_api::{AppManifest, AppPermission};
use glass_platform::fs::ConfinedDir;
use glass_platform::sandbox::{ToolRun, ToolRunner};

use crate::checks::{self, Findings};

/// Where the app's source is in the build folder, and the SDK.
pub const APP_DIR: &str = "app/";
pub const SDK_DIR: &str = "sdk/";
/// How long a bundle may take.
const LIMIT: Duration = Duration::from_secs(60);
/// One built file's largest size.
const MAX_BUILT_BYTES: u64 = 4 * 1024 * 1024;
/// Who the bundler runs as in a jail.
const BUILD_USER: &str = "nobody";

/// What the bridge serves for an app, by file name.
pub type Dist = BTreeMap<String, String>;

/// Bundles apps' source; cheap to clone.
#[derive(Debug, Clone)]
pub struct Bundler {
    tools: Arc<dyn ToolRunner>,
    /// esbuild: one statically linked program.
    program: PathBuf,
    /// The SDK's installed copy: `package.json` and `src/`.
    sdk: PathBuf,
    /// Where build folders go, under glassd's state directory.
    work: PathBuf,
}

static BUILDS: AtomicU64 = AtomicU64::new(0);

impl Bundler {
    pub fn new(tools: Arc<dyn ToolRunner>, program: PathBuf, sdk: PathBuf, work: PathBuf) -> Self {
        Self {
            tools,
            program,
            sdk,
            work,
        }
    }

    /// Bundles `source` (checked already) as `manifest`'s app: its page,
    /// script, stylesheet and, with `offline`, the SDK's service worker.
    pub fn bundle(
        &self,
        manifest: &AppManifest,
        source: &BTreeMap<String, String>,
    ) -> Result<Dist, Findings> {
        let name = format!(
            "glass-build-{}-{}",
            std::process::id(),
            BUILDS.fetch_add(1, Ordering::Relaxed)
        );
        let dir = self.work.join(&name);
        let built = self
            .prepare(&dir, source)
            .and_then(|()| self.run(&name, &dir, manifest));
        let _ = std::fs::remove_dir_all(&dir);
        built
    }

    fn prepare(&self, dir: &Path, source: &BTreeMap<String, String>) -> Result<(), Findings> {
        let failed = |what: &str, e: std::io::Error| Findings {
            refusals: vec![format!("couldn't build here ({what}: {e})")],
            notes: Vec::new(),
        };
        // The build folders' folder is root's alone: nobody can't rename a
        // build folder or reach one from outside its jail.
        std::fs::create_dir_all(dir.join("out")).map_err(|e| failed("build folder", e))?;
        for private in [self.work.parent(), Some(self.work.as_path())]
            .into_iter()
            .flatten()
        {
            std::fs::set_permissions(private, std::os::unix::fs::PermissionsExt::from_mode(0o700))
                .map_err(|e| failed("build folder", e))?;
        }
        for (path, text) in source {
            let file = dir.join(APP_DIR).join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| failed("source folder", e))?;
            }
            std::fs::write(file, text).map_err(|e| failed("source", e))?;
        }
        let sdk = dir.join(SDK_DIR);
        std::fs::create_dir_all(&sdk).map_err(|e| failed("SDK folder", e))?;
        std::fs::copy(self.sdk.join("package.json"), sdk.join("package.json"))
            .map_err(|e| failed("the SDK", e))?;
        for (path, text) in
            crate::store::read_tree(&self.sdk, "src").map_err(|e| failed("the SDK", e))?
        {
            let file = sdk.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| failed("SDK folder", e))?;
            }
            std::fs::write(file, text).map_err(|e| failed("the SDK", e))?;
        }
        let modules = dir.join("node_modules/@glasshouse");
        std::fs::create_dir_all(&modules).map_err(|e| failed("modules", e))?;
        std::os::unix::fs::symlink("../../sdk", modules.join("sdk"))
            .map_err(|e| failed("modules", e))?;
        // In a jail the bundler is nobody: the folder is theirs to read and
        // write, inside glassd's own folder, which they can't reach.
        if glass_platform::users::effective_uid() == 0 && self.tools.runs_as_user() {
            let user = glass_platform::users::by_name(BUILD_USER)
                .ok_or_else(|| failed("the build user", std::io::ErrorKind::NotFound.into()))?;
            give(dir, user.uid, user.gid).map_err(|e| failed("build folder", e))?;
        }
        Ok(())
    }

    fn run(&self, name: &str, dir: &Path, manifest: &AppManifest) -> Result<Dist, Findings> {
        let mut args = vec![format!("app={APP_DIR}{}", checks::MAIN)];
        let styled = dir.join(APP_DIR).join(checks::STYLES).exists();
        if styled {
            args.push(format!("app={APP_DIR}{}", checks::STYLES));
        }
        let offline = manifest.may(AppPermission::Offline);
        if offline {
            args.push(format!("sw={SDK_DIR}src/sw.ts"));
        }
        args.extend(
            [
                "--bundle",
                "--format=esm",
                "--target=chrome120",
                "--minify",
                "--legal-comments=none",
                "--log-level=error",
                "--log-limit=20",
                "--color=false",
                "--outdir=out",
                "--metafile=out/meta.json",
            ]
            .map(str::to_owned),
        );
        let out = self
            .tools
            .run_tool(&ToolRun {
                name: name.to_owned(),
                program: self.program.clone(),
                dir: dir.to_owned(),
                args,
                limit: LIMIT,
                user: BUILD_USER.to_owned(),
            })
            .map_err(|e| Findings {
                refusals: vec![format!("couldn't run the bundler: {e}")],
                notes: Vec::new(),
            })?;
        if out.timed_out {
            return Err(Findings {
                refusals: vec![format!("building took longer than {} s", LIMIT.as_secs())],
                notes: Vec::new(),
            });
        }
        if !out.success {
            return Err(Findings {
                refusals: errors(&out.stderr),
                notes: Vec::new(),
            });
        }
        let meta: serde_json::Value = read(dir, "out/meta.json")
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let inputs = meta["inputs"]
            .as_object()
            .map(|m| m.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let found = checks::check_inputs(inputs.iter().map(String::as_str));
        if !found.passed() || inputs.is_empty() {
            return Err(found);
        }
        let mut dist = Dist::new();
        dist.insert("index.html".into(), page(manifest, styled));
        for file in ["app.js", "app.css", "sw.js"] {
            if let Some(text) = read(dir, &format!("out/{file}")) {
                dist.insert(file.into(), text);
            }
        }
        if !dist.contains_key("app.js") {
            return Err(Findings {
                refusals: vec!["the bundler made no script".into()],
                notes: Vec::new(),
            });
        }
        Ok(dist)
    }
}

/// Makes `dir` and everything in it `uid`'s.
fn give(dir: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
    std::os::unix::fs::lchown(dir, Some(uid), Some(gid))?;
    if dir.is_dir() && !dir.is_symlink() {
        for entry in std::fs::read_dir(dir)? {
            give(&entry?.path(), uid, gid)?;
        }
    }
    Ok(())
}

/// A built file, as text, if it is there and not too big. The bundler
/// (nobody, on input an agent wrote) could leave anything in the folder,
/// a link to another person's file included, so glassd reads it confined
/// to the folder, whose own path only root can change.
fn read(dir: &Path, rel: &str) -> Option<String> {
    let bytes = ConfinedDir::open(dir)
        .ok()?
        .read(Path::new(rel), MAX_BUILT_BYTES)
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// The bundler's errors as findings: its own words, a line each, with the
/// build folder's paths read as the app's.
fn errors(stderr: &str) -> Vec<String> {
    let lines: Vec<String> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.chars().all(|c| c == '^' || c == '~' || c == '│'))
        .map(|l| l.trim_start_matches("✘ ").replace(APP_DIR, ""))
        .take(20)
        .collect();
    if lines.is_empty() {
        vec!["the bundler failed without saying why".into()]
    } else {
        lines
    }
}

/// The page every app is served with: its name, its stylesheet and its
/// script. Made here, never taken from the source, so it holds nothing
/// else.
fn page(manifest: &AppManifest, styled: bool) -> String {
    let title: String = manifest
        .name
        .chars()
        .map(|c| match c {
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '&' => "&amp;".to_owned(),
            '"' => "&quot;".to_owned(),
            c => c.to_string(),
        })
        .collect();
    let styles = if styled {
        "    <link rel=\"stylesheet\" href=\"./app.css\" />\n"
    } else {
        ""
    };
    format!(
        "<!doctype html>\n<html lang=\"en\">\n  <head>\n    <meta charset=\"utf-8\" />\n    \
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n    \
         <title>{title}</title>\n{styles}    <script type=\"module\" src=\"./app.js\"></script>\n  \
         </head>\n  <body>\n    <div id=\"app\" class=\"app\"></div>\n  </body>\n</html>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_is_ours_with_the_name_as_text() {
        let manifest: AppManifest = serde_json::from_value(serde_json::json!({
            "id": "my-mail", "name": "Mail <b>&</b>", "version": "1",
            "actions": ["mail.list"], "labels": ["internal"],
        }))
        .unwrap();
        let html = page(&manifest, true);
        assert!(html.contains("<title>Mail &lt;b&gt;&amp;&lt;/b&gt;</title>"));
        assert!(html.contains("./app.css") && html.contains("./app.js"));
        assert!(!page(&manifest, false).contains("app.css"));
    }

    #[test]
    fn the_bundlers_errors_read_as_the_apps() {
        let stderr = "✘ [ERROR] Could not resolve \"left-pad\"\n\n    app/src/main.ts:3:20:\n      3 │ import pad from \"left-pad\";\n        ╵                 ~~~~~~~~~~\n";
        let lines = errors(stderr);
        assert_eq!(lines[0], "[ERROR] Could not resolve \"left-pad\"");
        assert_eq!(lines[1], "src/main.ts:3:20:");
        assert!(errors("").len() == 1);
    }
}
