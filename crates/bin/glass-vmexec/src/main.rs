//! `glass-vmexec`: runs one task's process in a Firecracker agent computer
//! (ADR 0065 §10), through the exec service in its guest (`glass-vmguest`),
//! over Firecracker's vsock socket in the computer's jail:
//!
//! ```text
//! glass-vmexec --uds <jail>/vsock.sock --port 10000 --socket <task socket> -- <request>
//! ```
//!
//! glassd runs it as a task's command (`FirecrackerHypervisor::command`).
//! Its output is the process's, as it comes. It exits as the process did
//! (128 + a signal that ended it), with 127 if the guest couldn't start it,
//! and 125 if it couldn't run the task at all. Killing it ends the task in
//! the guest. `glass_computers::firecracker::exec` has the protocol.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let code = glass_computers::firecracker::vmexec::run(
        &args,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(code)
}
