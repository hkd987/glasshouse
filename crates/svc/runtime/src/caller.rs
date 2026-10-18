use std::path::PathBuf;

use glass_platform::users;

/// Who is calling the control API, as the kernel reported their uid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

impl Caller {
    /// Resolves a kernel-reported uid. Unknown uids get no access.
    pub fn from_uid(uid: u32) -> Option<Self> {
        users::by_uid(uid).map(|u| Self {
            name: u.name,
            uid: u.uid,
            gid: u.gid,
            home: u.home,
        })
    }

    /// Root administers the machine: sees every task, verifies the log.
    /// Answering approvals stays with each task's own user.
    pub fn is_admin(&self) -> bool {
        self.uid == 0
    }
}
