//! `glass-vmguest`: the exec service in a Firecracker agent computer's guest
//! (ADR 0065 §10). It runs each task's process as `glass-vmexec` on the
//! host asks, and streams back its output and how it ended;
//! `glass_computers::firecracker::exec` has the protocol.
//!
//! ```text
//! glass-vmguest [--listen /run/glass-vmguest.sock] [--runs /run/glass-vmguest]
//!               [--connect 'socat - VSOCK-CONNECT:2:{port}' | --connect unix:<path>]
//! ```
//!
//! It runs as root in the guest. It opens no vsock socket itself: the guest
//! image bridges vsock port 10000 to `--listen` (`socat
//! VSOCK-LISTEN:10000,fork,reuseaddr UNIX-CONNECT:/run/glass-vmguest.sock`),
//! and `--connect` takes each connection to a task's socket to the host's
//! port.

use std::fs::{self, Permissions};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use glass_computers::firecracker::vmguest::{self, Connector, GuestConfig};

const USAGE: &str = "usage: glass-vmguest [--listen <socket>] [--runs <folder>] \
                     [--connect <command with {port}> | --connect unix:<path with {port}>]";

fn main() -> ExitCode {
    let mut listen = PathBuf::from(vmguest::DEFAULT_LISTEN);
    let mut runs = PathBuf::from(vmguest::DEFAULT_RUNS);
    let mut connect = vmguest::DEFAULT_CONNECT.to_owned();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let (Some(name), Some(value)) = (arg.to_str(), args.next()) else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        match name {
            "--listen" => listen = value.into(),
            "--runs" => runs = value.into(),
            "--connect" => connect = value.to_string_lossy().into_owned(),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let connect = match Connector::parse(&connect) {
        Ok(connect) => connect,
        Err(e) => {
            eprintln!("glass-vmguest: {e}");
            return ExitCode::from(2);
        }
    };
    // A socket left by an earlier run of the service.
    if fs::symlink_metadata(&listen).is_ok_and(|m| m.file_type().is_socket()) {
        let _ = fs::remove_file(&listen);
    }
    let listener = match UnixListener::bind(&listen) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!(
                "glass-vmguest: couldn't listen at {}: {e}",
                listen.display()
            );
            return ExitCode::FAILURE;
        }
    };
    // Only its own user (and the vsock bridge, run as it) talks to it.
    if let Err(e) = fs::set_permissions(&listen, Permissions::from_mode(0o600)) {
        eprintln!(
            "glass-vmguest: couldn't keep {} to itself: {e}",
            listen.display()
        );
        return ExitCode::FAILURE;
    }
    match vmguest::serve(listener, Arc::new(GuestConfig::new(runs, connect))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("glass-vmguest: {e}");
            ExitCode::FAILURE
        }
    }
}
