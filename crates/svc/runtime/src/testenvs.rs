//! Boot environments in a temporary directory, for the tests of kernel
//! patches (ADR 0057) and OS updates (ADR 0061).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::hwpatch::Envs;

/// Boot environments in a temporary directory, as the loader and `bectl`
/// treat them: a start takes the start-once environment, clearing it,
/// and a module that panics as it loads sends the computer back round.
pub(crate) struct Fake {
    pub(crate) dir: tempfile::TempDir,
    state: Mutex<FakeState>,
}

#[derive(Default)]
pub(crate) struct FakeState {
    pub(crate) envs: Vec<String>,
    pub(crate) running: String,
    pub(crate) permanent: String,
    pub(crate) once: Option<String>,
    /// Modules that panic as they load.
    pub(crate) panics: Vec<String>,
    /// Modules that don't link against this kernel.
    pub(crate) broken: Vec<String>,
    pub(crate) loaded: Vec<String>,
    /// Drivers the kernel has built in: `kldload -n` says they're loaded.
    pub(crate) built_in: Vec<String>,
    pub(crate) mounts: BTreeMap<String, PathBuf>,
    /// The hardware watchdog is armed.
    pub(crate) watchdog: bool,
    /// It's armed on its own, for the shutdown, not by `watchdogd`.
    pub(crate) shutdown_watchdog: bool,
    /// The last start came from the watchdog's restart.
    pub(crate) restarted: bool,
}

impl Fake {
    pub(crate) fn new() -> Self {
        let fake = Self {
            dir: tempfile::tempdir().unwrap(),
            state: Mutex::new(FakeState {
                envs: vec!["default".into()],
                running: "default".into(),
                permanent: "default".into(),
                ..FakeState::default()
            }),
        };
        std::fs::create_dir_all(fake.env_dir("default").join("boot/kernel")).unwrap();
        std::fs::write(
            fake.env_dir("default").join("boot/kernel/if_iwlwifi.ko"),
            b"stock",
        )
        .unwrap();
        fake
    }

    pub(crate) fn env_dir(&self, env: &str) -> PathBuf {
        self.dir.path().join("envs").join(env)
    }

    pub(crate) fn s(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    /// The shutdown stops every process, `watchdogd` with them, which turns
    /// the watchdog off as it exits unless it was armed on its own.
    pub(crate) fn processes_stopped(&self) {
        let mut s = self.s();
        s.watchdog = std::mem::take(&mut s.shutdown_watchdog);
    }

    /// A restart: the loader's choice, then the modules loaded at start;
    /// one that panics restarts the computer at once.
    pub(crate) fn restart(&self) {
        loop {
            let mut s = self.s();
            let env = s.once.take().unwrap_or_else(|| s.permanent.clone());
            s.running = env.clone();
            s.loaded.clear();
            s.restarted = false;
            s.watchdog = false;
            s.shutdown_watchdog = false;
            drop(s);
            let conf = std::fs::read_to_string(self.env_dir(&env).join("boot/loader.conf"))
                .unwrap_or_default();
            let at_start: Vec<String> = conf
                .lines()
                .filter_map(|l| l.strip_suffix("_load=\"YES\""))
                .map(str::to_owned)
                .collect();
            let mut s = self.s();
            if at_start.iter().any(|m| s.panics.contains(m)) {
                continue;
            }
            s.loaded = at_start;
            return;
        }
    }
}

impl Envs for Fake {
    fn running(&self) -> Result<String, String> {
        Ok(self.s().running.clone())
    }
    fn exists(&self, env: &str) -> Result<bool, String> {
        Ok(self.s().envs.iter().any(|e| e == env))
    }
    fn starts_once(&self, env: &str) -> Result<bool, String> {
        Ok(self.s().once.as_deref() == Some(env))
    }
    fn permanent(&self, env: &str) -> Result<bool, String> {
        Ok(self.s().permanent == env)
    }
    fn create(&self, env: &str) -> Result<(), String> {
        let from = self.env_dir(&self.s().running);
        let to = self.env_dir(env);
        std::fs::create_dir_all(to.join("boot/kernel")).unwrap();
        for entry in std::fs::read_dir(from.join("boot/kernel")).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), to.join("boot/kernel").join(entry.file_name())).unwrap();
        }
        if let Ok(conf) = std::fs::read(from.join("boot/loader.conf")) {
            std::fs::write(to.join("boot/loader.conf"), conf).unwrap();
        }
        self.s().envs.push(env.into());
        Ok(())
    }
    /// `image` is a JSON map of paths to contents, standing in for a ZFS
    /// stream.
    fn import(&self, env: &str, image: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(image).map_err(|e| e.to_string())?;
        let files: BTreeMap<String, String> =
            serde_json::from_str(&text).map_err(|e| format!("cannot receive stream: {e}"))?;
        if self.s().envs.iter().any(|e| e == env) {
            return Err(format!("{env} exists"));
        }
        let to = self.env_dir(env);
        for (path, content) in files {
            let path = to.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        std::fs::create_dir_all(to.join("boot/kernel")).unwrap();
        self.s().envs.push(env.into());
        Ok(())
    }
    /// `base`'s files, then the stream's on top.
    fn import_onto(&self, env: &str, image: &Path, base: &str) -> Result<(), String> {
        if !self.s().envs.iter().any(|e| e == base) {
            return Err(format!("no {base}"));
        }
        let from = self.env_dir(base);
        let to = self.env_dir(env);
        copy_tree(&from, &to);
        self.s().envs.push(env.into());
        let text = std::fs::read_to_string(image).map_err(|e| e.to_string())?;
        let files: BTreeMap<String, String> =
            serde_json::from_str(&text).map_err(|e| format!("cannot receive stream: {e}"))?;
        for (path, content) in files {
            let path = to.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        Ok(())
    }
    fn mount(&self, env: &str, at: &Path) -> Result<(), String> {
        std::fs::remove_dir(at).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(self.env_dir(env), at).map_err(|e| e.to_string())?;
        self.s().mounts.insert(env.into(), at.to_owned());
        Ok(())
    }
    fn unmount(&self, env: &str) -> Result<(), String> {
        let at = self.s().mounts.remove(env).ok_or("not mounted")?;
        std::fs::remove_file(&at).map_err(|e| e.to_string())?;
        std::fs::create_dir(&at).map_err(|e| e.to_string())
    }
    fn activate_once(&self, env: &str) -> Result<(), String> {
        self.s().once = Some(env.into());
        Ok(())
    }
    fn cancel_once(&self) -> Result<(), String> {
        self.s().once = None;
        Ok(())
    }
    fn activate(&self, env: &str) -> Result<(), String> {
        self.s().permanent = env.into();
        Ok(())
    }
    fn destroy(&self, env: &str) -> Result<(), String> {
        let mut s = self.s();
        if s.running == env || s.permanent == env {
            return Err(format!("{env} is in use"));
        }
        s.envs.retain(|e| e != env);
        drop(s);
        let _ = std::fs::remove_dir_all(self.env_dir(env));
        Ok(())
    }
    fn kernel_release(&self) -> Result<String, String> {
        Ok("15.1-RELEASE-p1".into())
    }
    fn root(&self) -> PathBuf {
        self.env_dir(&self.s().running)
    }
    fn module_loaded(&self, name: &str) -> bool {
        self.s().loaded.iter().any(|m| m == name)
    }
    fn load_module(&self, name: &str) -> Result<(), String> {
        let mut s = self.s();
        if s.broken.iter().any(|m| m == name) {
            return Err("kldload: an error occurred while loading module".into());
        }
        if !s.built_in.iter().any(|m| m == name) {
            s.loaded.push(name.into());
        }
        Ok(())
    }
    fn built_in(&self, name: &str) -> bool {
        self.s().built_in.iter().any(|m| m == name)
    }
    fn watchdog(&self, on: bool) -> Result<(), String> {
        self.s().watchdog = on;
        Ok(())
    }
    fn watchdog_through_shutdown(&self) -> Result<(), String> {
        let mut s = self.s();
        s.shutdown_watchdog = s.watchdog;
        Ok(())
    }
    fn watchdog_restarted(&self) -> bool {
        self.s().restarted
    }
    fn kernel_messages(&self, name: &str) -> String {
        if self.s().broken.iter().any(|m| m == name) {
            "link_elf_obj: symbol lkpi_80211_pm_driver undefined".into()
        } else {
            String::new()
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
