//! A stand-in for the bundler, for tests here and in the runtime: no
//! esbuild needed.

use std::path::{Path, PathBuf};
use std::process::Command;

use glass_platform::sandbox::{ProcessSpec, Sandbox, SandboxSpec, ToolOutput, ToolRun};
use serde_json::json;

use crate::store;

/// A stand-in for the bundler: writes what esbuild would, reading what it
/// was given, and reports every source file as an input (plus `extra`).
#[derive(Debug, Default)]
pub struct FakeBundler {
    /// An input it also reports, as if the source had reached it.
    pub extra: Option<String>,
}

impl Sandbox for FakeBundler {
    fn kind(&self) -> &'static str {
        "fake"
    }
    fn is_security_boundary(&self) -> bool {
        false
    }
    fn create(&self, _: &SandboxSpec) -> glass_platform::Result<()> {
        unimplemented!()
    }
    fn command(&self, _: &SandboxSpec, _: &ProcessSpec) -> Command {
        unimplemented!()
    }
    fn destroy(&self, _: &SandboxSpec) -> glass_platform::Result<()> {
        unimplemented!()
    }
    fn agent_path(&self, _: &SandboxSpec, inside: &Path) -> PathBuf {
        inside.to_owned()
    }
    fn cleanup_stale(&self) -> glass_platform::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn run_tool(&self, run: &ToolRun) -> glass_platform::Result<ToolOutput> {
        let failed = |source| glass_platform::PlatformError::Io {
            context: "the stand-in bundler".into(),
            source,
        };
        let mut inputs = serde_json::Map::new();
        for (path, _) in store::read_tree(&run.dir.join("app"), "src").map_err(failed)? {
            inputs.insert(format!("app/{path}"), json!({}));
        }
        inputs.insert("sdk/src/index.ts".into(), json!({}));
        if let Some(extra) = &self.extra {
            inputs.insert(extra.clone(), json!({}));
        }
        let out = run.dir.join("out");
        for (name, text) in [
            ("meta.json", json!({ "inputs": inputs }).to_string()),
            ("app.js", "console.log('built')".to_owned()),
            ("app.css", "body{}".to_owned()),
        ] {
            std::fs::write(out.join(name), text).map_err(failed)?;
        }
        Ok(ToolOutput {
            success: true,
            stderr: String::new(),
            timed_out: false,
        })
    }
}
