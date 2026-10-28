//! Agent computers (ADR 0065, ADR 0066): an agent keeps a computer of its
//! own between tasks, where it is root and nothing asks; the rules stay at
//! its edge, the task socket.
//!
//! - [`sandbox`]: [`sandbox::ComputerSandbox`], the runtime's sandbox
//!   backend over a hypervisor: one computer per agent, each task run in it.
//! - [`namespaces`]: Linux namespaces (`unshare`), where there's no KVM.
//! - [`firecracker`]: Firecracker microVMs, on AWS.
//! - [`store`]: where a computer lives while it sleeps: chunks in S3, or a
//!   folder standing in for it.

pub mod firecracker;
pub mod hypervisor;
pub mod namespaces;
pub mod sandbox;
pub mod store;
