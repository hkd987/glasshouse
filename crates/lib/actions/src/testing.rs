//! Fixtures for app crates' tests: a task context like the runtime builds.

use std::path::Path;

use glass_platform::fs::Owner;
use glass_types::{AgentPrincipal, TaskId, UserRef};

use crate::{Grant, TaskContext};

/// A task for user `demo` (uid 1001, in group `everyone` only) with home
/// `root`, a read-only `Invoices` grant and a writable `Reports` grant
/// under it.
///
/// What its actions write is given to [`owner`]: uid 1001 where the tests
/// run as root, as a broker does, and whoever runs them anywhere else.
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
        owner: owner(root),
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
        groups: vec!["everyone".into()],
        data_label: None,
    }
}

/// Who a test's task writes for under `root`. Root gives files to someone
/// else (uid 1001), which is what a broker does and what the tests of
/// ownership need. Nobody else can give a file away, and uid 1001 is only
/// sometimes who runs the tests (it is on some build machines, which is how
/// a test that depends on it passes there and fails on a laptop): so
/// anyone else's task writes for them, the owner of `root`, which they made.
pub fn owner(root: &Path) -> Owner {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(root) {
        Ok(meta) if meta.uid() != 0 => Owner {
            uid: meta.uid(),
            gid: meta.gid(),
        },
        _ => Owner {
            uid: 1001,
            gid: 1001,
        },
    }
}
