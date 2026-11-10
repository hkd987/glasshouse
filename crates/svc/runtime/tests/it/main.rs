//! The runtime's integration tests, one binary: each module was its own
//! test binary, and each linked the whole runtime and every app again
//! (about 70 MB and a link apiece; docs/dev/build-performance.md).
//! `cargo test -p glass-runtime --test it chat::` runs one module's.

mod common;

mod agent_folders;
mod apps;
mod artifacts;
mod audit;
mod bounds;
mod catalog;
mod chat;
mod computers;
mod continuation;
mod docs;
mod flows;
mod mail;
mod meet;
mod models;
mod people;
mod pipeline;
mod profiles;
mod routines;
mod seat;
mod settings;
mod skills;
mod speed;
mod store;
mod studio;
mod tailoring;
mod undo;
