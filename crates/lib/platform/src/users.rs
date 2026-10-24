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

/// Every account name in a `passwd(5)` file's text, people and system
/// accounts alike: names a new account can't take.
pub fn names_in(passwd: &str) -> Vec<String> {
    passwd
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split(':').next())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Every account name on this computer.
pub fn names() -> std::io::Result<Vec<String>> {
    Ok(names_in(&std::fs::read_to_string("/etc/passwd")?))
}

/// Whether `login` is a name [`create_person`] takes: a lowercase letter,
/// then up to 15 lowercase letters or digits.
pub fn valid_login(login: &str) -> bool {
    let mut chars = login.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && login.len() <= 16
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Why `full_name` can't be an account's full name, if it can't: it goes
/// into a `passwd(5)` field, where `:` and `,` separate fields.
pub fn full_name_problem(full_name: &str) -> Option<&'static str> {
    let name = full_name.trim();
    if name.is_empty() {
        Some("Enter your name")
    } else if name.chars().count() > 64 {
        Some("That name is too long")
    } else if name.chars().any(|c| c.is_control() || c == ':' || c == ',') {
        Some("A name can't have a colon or a comma")
    } else {
        None
    }
}

/// Ends every process of `login`'s that isn't in a jail, when their
/// session has ended: one left running could read the keyboard or the
/// screen it opened, and so the next person's password. Agents run in
/// jails and keep working (PM-5).
pub fn end_processes(login: &str) -> crate::Result<()> {
    use crate::PlatformError;

    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("ending processes".into()));
    }
    if login.is_empty() || login.starts_with('-') || login.contains(char::is_whitespace) {
        return Err(PlatformError::Unsafe(format!(
            "{login:?} is not an account name"
        )));
    }
    let status = std::process::Command::new("/bin/pkill")
        .args(["-KILL", "-U", login, "-j", "none"])
        .status()
        .map_err(|e| PlatformError::io("cannot run pkill", e))?;
    // 1: nothing was left running.
    match status.code() {
        Some(0 | 1) => Ok(()),
        _ => Err(PlatformError::Command {
            command: format!("pkill -KILL -U {login} -j none"),
            status: status.to_string(),
            stderr: String::new(),
        }),
    }
}

/// Creates a person's account (first-run setup, UX-2): a login shell, a
/// home of their own (its own ZFS dataset when `/home` is one, so undo can
/// restore it from snapshots, PA-5), and their password, which `pw` reads
/// from its standard input and never sees on a command line. Root only.
/// Its steps are separate commands: a power cut between them is what
/// [`finish_person`] and [`undo_person`] are for.
pub fn create_person(login: &str, full_name: &str, password: &str) -> crate::Result<()> {
    use crate::{PlatformError, cmd};

    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("creating accounts".into()));
    }
    if !valid_login(login) {
        return Err(PlatformError::Unsafe(format!(
            "{login:?} is not an account name"
        )));
    }
    if let Some(problem) = full_name_problem(full_name) {
        return Err(PlatformError::Unsafe(problem.into()));
    }
    if password.is_empty() || password.contains(['\n', '\r']) {
        return Err(PlatformError::Unsafe(
            "the password is empty or has a line break".into(),
        ));
    }
    let home = format!("/home/{login}");
    let dataset = home_dataset(login);
    if let Some(dataset) = &dataset {
        cmd::run("zfs", &["create", dataset])?;
    }
    let mut input = password.as_bytes().to_vec();
    input.push(b'\n');
    let created = cmd::run_with_input(
        "pw",
        &[
            "useradd",
            "-n",
            login,
            "-c",
            full_name.trim(),
            "-d",
            &home,
            "-s",
            "/bin/sh",
            "-m",
            "-M",
            "0755",
            "-h",
            "0",
        ],
        &input,
    );
    input.fill(0);
    if let Err(e) = created {
        // Nothing half made: the home made for an account that isn't
        // there would stop setup trying the same name again.
        if let Some(dataset) = &dataset {
            let _ = cmd::run("zfs", &["destroy", dataset]);
        }
        return Err(e);
    }
    cmd::run("chown", &[&format!("{login}:{login}"), &home]).map(drop)
}

/// The dataset a person's home is, or would be: under /home's own
/// dataset, when it has one.
fn home_dataset(login: &str) -> Option<String> {
    crate::cmd::run("zfs", &["list", "-H", "-o", "name,mountpoint", "/home"])
        .ok()
        .and_then(|out| {
            let mut fields = out.lines().next()?.split('\t');
            let (name, mountpoint) = (fields.next()?, fields.next()?);
            (mountpoint == "/home").then(|| format!("{name}/{login}"))
        })
}

/// Finishes the account first-run setup made for `login` when a power cut
/// came after `pw useradd` (ADR 0053): its home made theirs. Root only.
pub fn finish_person(login: &str) -> crate::Result<()> {
    use crate::{PlatformError, cmd};

    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("finishing accounts".into()));
    }
    if !valid_login(login) || by_name(login).is_none() {
        return Err(PlatformError::Unsafe(format!(
            "{login:?} is not an account here"
        )));
    }
    let home = format!("/home/{login}");
    cmd::run("chown", &[&format!("{login}:{login}"), &home]).map(drop)
}

/// Takes back the home first-run setup made for `login` when a power cut
/// came before its account was (ADR 0053), so setup can make it again: the
/// dataset, if it was made at `since` or later. One that was there before
/// stays. Root only.
pub fn undo_person(login: &str, since: std::time::SystemTime) -> crate::Result<()> {
    use crate::{PlatformError, cmd};

    if !cfg!(target_os = "freebsd") {
        return Err(PlatformError::Unsupported("taking accounts back".into()));
    }
    if !valid_login(login) || by_name(login).is_some() {
        return Err(PlatformError::Unsafe(format!(
            "{login:?} isn't a name setup left unfinished"
        )));
    }
    let Some(dataset) = home_dataset(login) else {
        return Ok(());
    };
    let Ok(created) = cmd::run("zfs", &["get", "-Hp", "-o", "value", "creation", &dataset]) else {
        return Ok(()); // no such dataset: nothing was made
    };
    let created: u64 = created.trim().parse().unwrap_or(0);
    let since = since
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_secs());
    if created >= since {
        cmd::run("zfs", &["destroy", &dataset])?;
    }
    Ok(())
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
        assert_eq!(
            names_in(passwd),
            ["root", "nobody", "glassd", "dana", "demo", "svc"]
        );
    }

    #[test]
    fn ux2_new_accounts_take_safe_names() {
        assert!(valid_login("dana") && valid_login("d2"));
        for bad in [
            "",
            "Dana",
            "2dana",
            "da-na",
            "da na",
            "dana:x",
            "averyveryverylongname",
        ] {
            assert!(!valid_login(bad), "{bad:?}");
        }
        assert_eq!(full_name_problem("Dana Ruiz"), None);
        assert!(full_name_problem("  ").is_some());
        assert!(
            full_name_problem("Dana:Ruiz").is_some(),
            "passwd's field separator"
        );
        assert!(
            full_name_problem("Dana, Room 4").is_some(),
            "gecos' separator"
        );
        assert!(full_name_problem("Dana\nRuiz").is_some());
    }
}
