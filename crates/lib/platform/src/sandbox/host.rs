//! A development stand-in for jails. NOT A SECURITY BOUNDARY.
//!
//! Agents run as ordinary child processes of the daemon with the sandbox
//! layout under a host directory; granted paths appear as symlinks. This lets
//! the whole system, including every broker check, run and be tested on
//! Linux and macOS laptops. The daemon refuses this backend unless its config
//! explicitly opts into development mode, and labels it in every status
//! report.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{ProcessSpec, Sandbox, SandboxSpec, check_name, create_layout};
use crate::{PlatformError, Result};

#[derive(Debug, Default)]
pub struct HostSandbox;

impl Sandbox for HostSandbox {
    fn kind(&self) -> &'static str {
        "host-insecure"
    }

    fn is_security_boundary(&self) -> bool {
        false
    }

    fn create(&self, spec: &SandboxSpec) -> Result<()> {
        check_name(&spec.name)?;
        if spec.root.exists() {
            return Err(PlatformError::Unsafe(format!(
                "sandbox root {} already exists; refusing to reuse it",
                spec.root.display()
            )));
        }
        create_layout(spec)?;
        for mount in &spec.mounts {
            let link = self.host_path(spec, &mount.target);
            if let Some(parent) = link.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| PlatformError::io("creating mount parent", e))?;
            }
            symlink(&mount.source, &link).map_err(|e| PlatformError::io("linking mount", e))?;
        }
        Ok(())
    }

    fn command(&self, spec: &SandboxSpec, process: &ProcessSpec) -> Command {
        let mut command = Command::new(&process.program);
        command
            .args(&process.args)
            .env_clear()
            .envs(process.env.iter().map(|(k, v)| (k, v)))
            .current_dir(spec.root.join("work"));
        command
    }

    fn destroy(&self, spec: &SandboxSpec) -> Result<()> {
        check_name(&spec.name)?;
        // remove_dir_all removes symlinks themselves and never follows them,
        // so granted paths are untouched.
        match fs::remove_dir_all(&spec.root) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PlatformError::io(
                format!("removing {}", spec.root.display()),
                e,
            )),
        }
    }

    fn agent_path(&self, spec: &SandboxSpec, inside: &Path) -> PathBuf {
        self.host_path(spec, inside)
    }

    fn cleanup_stale(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn run_tool(&self, run: &super::ToolRun) -> Result<super::ToolOutput> {
        super::tool::run_unconfined(run)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::sandbox::{AGENT_SOCKET_PATH, Mount};

    #[test]
    fn creates_layout_runs_and_cleans_up_without_touching_grants() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("Invoices");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("inv-1.txt"), "100.00").unwrap();
        let meta = fs::metadata(dir.path()).unwrap();
        let spec = SandboxSpec {
            name: "glass-test".into(),
            root: dir.path().join("sandbox"),
            mounts: vec![Mount {
                source: data.clone(),
                target: "/data/Invoices".into(),
                writable: false,
            }],
            user: "me".into(),
            uid: meta.uid(),
            gid: meta.gid(),
        };
        let sandbox = HostSandbox;
        sandbox.create(&spec).unwrap();
        assert!(
            sandbox
                .host_path(&spec, Path::new(AGENT_SOCKET_PATH))
                .parent()
                .unwrap()
                .is_dir()
        );

        let process = ProcessSpec {
            program: "/bin/cat".into(),
            args: vec![
                sandbox
                    .agent_path(&spec, Path::new("/data/Invoices/inv-1.txt"))
                    .display()
                    .to_string(),
            ],
            env: vec![],
        };
        let out = sandbox.command(&spec, &process).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "100.00");

        sandbox.destroy(&spec).unwrap();
        assert!(!spec.root.exists());
        assert_eq!(
            fs::read_to_string(data.join("inv-1.txt")).unwrap(),
            "100.00"
        );
        sandbox.destroy(&spec).unwrap();
    }
}
