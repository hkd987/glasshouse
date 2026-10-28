//! The jailer's command line for one computer, as Firecracker v1.17.0's
//! `docs/jailer.md` describes it.
//!
//! The jailer makes `<chroot base>/<firecracker's file name>/<id>/root`,
//! copies Firecracker in, puts it in a cgroup (v2) with the computer's
//! memory and CPU, chroots it there in a mount namespace and a PID
//! namespace of its own, drops to the unprivileged user and runs it. With
//! `--new-pid-ns` the jailer itself exits once Firecracker runs, leaving its
//! pid in `<jail>/<firecracker's file name>.pid`; with `--daemonize`
//! Firecracker has its own session and no terminal.
//!
//! The chroot base is the computer's own folder (`<dir>/jail`), so
//! everything a running computer has is in its folder.

use std::process::Command;

use super::{FirecrackerConfig, JAIL};
use crate::hypervisor::ComputerSpec;

/// Where Firecracker serves its API, in its jail.
pub const API_SOCKET: &str = "/api.sock";

/// The period CPU limits are counted in: 100 ms, in microseconds.
const CPU_PERIOD_US: u64 = 100_000;

/// The jailer's command for `computer`.
pub fn command(config: &FirecrackerConfig, computer: &ComputerSpec) -> Command {
    let mut command = Command::new(&config.jailer);
    command
        .arg("--id")
        .arg(&computer.id)
        .arg("--exec-file")
        .arg(&config.firecracker)
        .arg("--uid")
        .arg(config.uid.to_string())
        .arg("--gid")
        .arg(config.gid.to_string())
        .args(["--cgroup-version", "2"])
        .arg("--cgroup")
        .arg(format!("memory.max={}", memory_max(config, computer)))
        .arg("--cgroup")
        .arg(format!(
            "cpu.max={} {CPU_PERIOD_US}",
            u64::from(computer.vcpus.max(1)) * CPU_PERIOD_US
        ));
    if let Some(parent) = &config.parent_cgroup {
        command.arg("--parent-cgroup").arg(parent);
    }
    command
        .arg("--chroot-base-dir")
        .arg(computer.dir.join(JAIL));
    if let Some(netns) = &config.netns {
        command.arg("--netns").arg(netns);
    }
    command
        .args(["--daemonize", "--new-pid-ns", "--"])
        .args(["--api-sock", API_SOCKET]);
    command
}

/// The cgroup's memory, in bytes: the guest's and Firecracker's own.
fn memory_max(config: &FirecrackerConfig, computer: &ComputerSpec) -> u64 {
    (u64::from(computer.memory_mib) + u64::from(config.overhead_mib)) * 1024 * 1024
}
