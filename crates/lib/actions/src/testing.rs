//! Fixtures for app crates' tests: a task context like the runtime builds.

use std::path::Path;

use glass_platform::fs::Owner;
use glass_types::{AgentPrincipal, TaskId, UserRef};

use crate::{Grant, TaskContext};

/// A task for user `demo` (uid 1001) with home `root`, a read-only
/// `Invoices` grant and a writable `Reports` grant under it.
pub fn context(root: &Path) -> TaskContext {
    TaskContext {
        principal: AgentPrincipal {
            agent: "invoice-demo".parse().expect("static agent id"),
            on_behalf_of: UserRef {
                name: "demo".into(),
                uid: 1001,
            },
            task: TaskId::new(),
        },
        owner: Owner {
            uid: 1001,
            gid: 1001,
        },
        home: root.to_path_buf(),
        grants: vec![
            Grant {
                name: "Invoices".into(),
                root: root.join("Invoices"),
                writable: false,
            },
            Grant {
                name: "Reports".into(),
                root: root.join("Reports"),
                writable: true,
            },
        ],
    }
}
