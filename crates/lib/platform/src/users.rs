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

/// Someone who can sign in on this computer: a local account in the
/// people's uid range with a login shell, for the sign-in screen
/// (ADR 0019). Directory accounts come with directory sign-in (CF-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    /// The full name from the account's comment field, if any.
    pub full_name: String,
    pub uid: u32,
}

/// The people in `/etc/passwd`, in uid order.
pub fn people() -> std::io::Result<Vec<Person>> {
    Ok(people_in(&std::fs::read_to_string("/etc/passwd")?))
}

/// The people in a `passwd(5)` file's text: uids from 1000 up (not
/// `nobody`), with a shell that lets them in.
pub fn people_in(passwd: &str) -> Vec<Person> {
    const NO_LOGIN: [&str; 3] = ["/usr/sbin/nologin", "/sbin/nologin", "/usr/bin/false"];
    let mut found: Vec<Person> = passwd
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let [name, _, uid, _, gecos, _, shell] = fields[..] else {
                return None;
            };
            let uid: u32 = uid.parse().ok()?;
            let can_sign_in =
                (1000..65534).contains(&uid) && !shell.is_empty() && !NO_LOGIN.contains(&shell);
            can_sign_in.then(|| Person {
                name: name.to_owned(),
                // The first comma-separated part is the full name.
                full_name: gecos.split(',').next().unwrap_or_default().to_owned(),
                uid,
            })
        })
        .collect();
    found.sort_by_key(|p| p.uid);
    found
}

#[cfg(test)]
mod people_tests {
    use super::*;

    #[test]
    fn people_are_local_accounts_with_a_login_shell() {
        let passwd = "\
# comment
root:*:0:0:Charlie &:/root:/bin/sh
nobody:*:65534:65534:Unprivileged user:/nonexistent:/usr/sbin/nologin
glassd:*:900:900:Glasshouse daemon:/var/db/glasshouse:/usr/sbin/nologin
dana:*:1002:1002:Dana Ruiz,Room 4,,:/home/dana:/bin/sh
demo:*:1001:1001:Glasshouse demo user:/home/demo:/bin/sh
svc:*:1003:1003::/home/svc:/usr/sbin/nologin
";
        let people = people_in(passwd);
        let names: Vec<(&str, &str)> = people
            .iter()
            .map(|p| (p.name.as_str(), p.full_name.as_str()))
            .collect();
        assert_eq!(
            names,
            [("demo", "Glasshouse demo user"), ("dana", "Dana Ruiz")]
        );
    }
}
