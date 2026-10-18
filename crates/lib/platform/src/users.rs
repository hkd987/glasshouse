//! Account lookup through the system's name service (`getpwuid_r`,
//! `getpwnam_r`), so directory-backed accounts resolve the same way local
//! ones do.

use std::ffi::{CStr, CString};
use std::path::PathBuf;

/// A system account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInfo {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

/// The uid this process runs with: who owns what it may read privately,
/// such as glassd's model secrets file.
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid takes no arguments, touches no memory and can't fail.
    unsafe { libc::geteuid() }
}

pub fn by_uid(uid: u32) -> Option<UserInfo> {
    lookup(|pwd, buf, result| {
        // SAFETY: `pwd`, `buf` and `result` are valid for writes for the
        // durations of the call; `buf.len()` is the true buffer size.
        unsafe { libc::getpwuid_r(uid, pwd, buf.as_mut_ptr(), buf.len(), result) }
    })
}

pub fn by_name(name: &str) -> Option<UserInfo> {
    let name = CString::new(name).ok()?;
    lookup(|pwd, buf, result| {
        // SAFETY: as in `by_uid`; `name` is a valid NUL-terminated string
        // that outlives the call.
        unsafe { libc::getpwnam_r(name.as_ptr(), pwd, buf.as_mut_ptr(), buf.len(), result) }
    })
}

fn lookup(
    call: impl Fn(*mut libc::passwd, &mut [libc::c_char], *mut *mut libc::passwd) -> libc::c_int,
) -> Option<UserInfo> {
    let mut size = 1024;
    loop {
        let mut buf: Vec<libc::c_char> = vec![0; size];
        // SAFETY: an all-zero `passwd` is a valid (if meaningless) value for
        // a plain C struct of integers and pointers; it is only read after
        // the call fills it in.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = call(&mut pwd, &mut buf, &mut result);
        if rc == libc::ERANGE && size < 1 << 20 {
            size *= 4;
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        // SAFETY: on success the string fields point into `buf`, which is
        // still alive, and are NUL-terminated.
        let (name, home) = unsafe {
            (
                CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned(),
                CStr::from_ptr(pwd.pw_dir).to_string_lossy().into_owned(),
            )
        };
        return Some(UserInfo {
            name,
            uid: pwd.pw_uid,
            gid: pwd.pw_gid,
            home: home.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_resolves_both_ways() {
        let root = by_uid(0).expect("uid 0 exists everywhere we run");
        assert_eq!(root.uid, 0);
        assert_eq!(by_name(&root.name).unwrap().uid, 0);
    }

    #[test]
    fn unknown_accounts_are_none() {
        assert!(by_name("no-such-user-glasshouse").is_none());
        assert!(by_name("bad\0name").is_none());
    }
}
