//! Checking a person's password with the system's PAM (OpenPAM), for the
//! sign-in screen (ADR 0019). Behind the `pam` feature, so only the sign-in
//! program links libpam. FreeBSD only.
//!
//! The password goes to PAM through its conversation function and nowhere
//! else; the copies this module makes are wiped before they're freed, and
//! [`wipe`] lets the caller wipe its own.

use std::ffi::{CStr, CString};
use std::ptr;

use libc::{c_char, c_int, c_void, size_t};

/// Why a sign-in didn't go through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// A wrong password or an unknown account: one case, so the screen
    /// never tells which.
    #[error("that password didn't work")]
    Refused,
    /// The password was right, but the account can't sign in now, such as
    /// an expired one or one that needs a new password first.
    #[error("{0}")]
    Account(String),
    /// PAM itself failed.
    #[error("signing in isn't working: {0}")]
    Unavailable(String),
}

/// Checks `user`'s `password` with PAM service `service` (its policy is
/// `/etc/pam.d/<service>` or `/usr/local/etc/pam.d/<service>`), on the
/// terminal `tty` (such as `ttyv0`): authentication, then whether the
/// account may sign in.
pub fn check_password(
    service: &str,
    user: &str,
    tty: &str,
    password: &str,
) -> Result<(), AuthError> {
    let text = |s: &str| {
        CString::new(s).map_err(|_| AuthError::Unavailable("text with a NUL byte".into()))
    };
    let (service, tty) = (text(service)?, text(tty)?);
    // A name with a NUL byte names no account.
    let user = CString::new(user).map_err(|_| AuthError::Refused)?;
    let mut password = Secret(CString::new(password).map_err(|_| AuthError::Refused)?);
    let result = authenticate(&service, &user, &tty, &password);
    password.wipe();
    result
}

/// Overwrites a string's bytes in place, in a way the compiler keeps, and
/// empties it: for the caller's copy of a password.
pub fn wipe(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    // SAFETY: `bytes` is valid for writes of its length.
    unsafe { explicit_bzero(bytes.as_mut_ptr().cast(), bytes.len()) };
}

/// A C string that's wiped before it's freed.
struct Secret(CString);

impl Secret {
    fn wipe(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes_with_nul();
        // SAFETY: `bytes` is valid for writes of its length.
        unsafe { explicit_bzero(bytes.as_mut_ptr().cast(), bytes.len()) };
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.wipe();
    }
}

// <security/pam_constants.h> and <security/pam_types.h>.
const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 6;
const PAM_PERM_DENIED: c_int = 7;
const PAM_MAXTRIES: c_int = 8;
const PAM_AUTH_ERR: c_int = 9;
const PAM_NEW_AUTHTOK_REQD: c_int = 10;
const PAM_USER_UNKNOWN: c_int = 13;
const PAM_ACCT_EXPIRED: c_int = 17;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x1;
const PAM_TTY: c_int = 3;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type Converse = unsafe extern "C" fn(
    c_int,
    *mut *const PamMessage,
    *mut *mut PamResponse,
    *mut c_void,
) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<Converse>,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn pam_set_item(handle: *mut c_void, item: c_int, value: *const c_void) -> c_int;
    fn pam_authenticate(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(handle: *mut c_void, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut c_void, error: c_int) -> *const c_char;
}

unsafe extern "C" {
    // FreeBSD's libc; the libc crate doesn't declare it there.
    fn explicit_bzero(buf: *mut c_void, len: size_t);
}

fn authenticate(
    service: &CStr,
    user: &CStr,
    tty: &CStr,
    password: &Secret,
) -> Result<(), AuthError> {
    let conv = PamConv {
        conv: Some(converse),
        appdata_ptr: ptr::from_ref(password).cast_mut().cast(),
    };
    let mut handle: *mut c_void = ptr::null_mut();
    // SAFETY: the strings are valid C strings and `conv` outlives the
    // handle, which `pam_end` below closes; `converse` reads `password`
    // only during the calls in this function.
    unsafe {
        let started = pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle);
        if started != PAM_SUCCESS || handle.is_null() {
            return Err(AuthError::Unavailable(format!(
                "pam_start failed ({started})"
            )));
        }
        let mut status = pam_set_item(handle, PAM_TTY, tty.as_ptr().cast());
        let result = if status != PAM_SUCCESS {
            Err(AuthError::Unavailable(describe(handle, status)))
        } else {
            status = pam_authenticate(handle, PAM_DISALLOW_NULL_AUTHTOK);
            match status {
                PAM_SUCCESS => {
                    status = pam_acct_mgmt(handle, PAM_DISALLOW_NULL_AUTHTOK);
                    match status {
                        PAM_SUCCESS => Ok(()),
                        PAM_NEW_AUTHTOK_REQD => Err(AuthError::Account(
                            "this account needs a new password before it can sign in".into(),
                        )),
                        PAM_ACCT_EXPIRED => {
                            Err(AuthError::Account("this account has expired".into()))
                        }
                        PAM_PERM_DENIED => {
                            Err(AuthError::Account("this account can't sign in here".into()))
                        }
                        other => Err(AuthError::Account(describe(handle, other))),
                    }
                }
                PAM_AUTH_ERR | PAM_USER_UNKNOWN | PAM_MAXTRIES | PAM_PERM_DENIED => {
                    Err(AuthError::Refused)
                }
                other => Err(AuthError::Unavailable(describe(handle, other))),
            }
        };
        pam_end(handle, status);
        result
    }
}

/// PAM's words for a status.
///
/// # Safety
/// `handle` is a live PAM handle.
unsafe fn describe(handle: *mut c_void, status: c_int) -> String {
    // SAFETY: the caller's handle is live; PAM returns a static string or
    // null.
    let text = unsafe { pam_strerror(handle, status) };
    if text.is_null() {
        format!("PAM error {status}")
    } else {
        // SAFETY: a non-null result is a NUL-terminated string.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

/// The conversation: answers the password prompt with the password, and
/// nothing else. PAM frees the replies, so they're allocated with malloc.
///
/// # Safety
/// Called by PAM with `n` messages and `data` pointing to the `Secret`
/// passed to `pam_start`.
unsafe extern "C" fn converse(
    n: c_int,
    messages: *mut *const PamMessage,
    replies: *mut *mut PamResponse,
    data: *mut c_void,
) -> c_int {
    let Ok(count) = usize::try_from(n) else {
        return PAM_CONV_ERR;
    };
    if count == 0 || messages.is_null() || replies.is_null() || data.is_null() {
        return PAM_CONV_ERR;
    }
    // SAFETY: `data` is the `Secret` `authenticate` passed in, alive for
    // the whole conversation; PAM passes `count` message pointers.
    unsafe {
        let password = &*data.cast::<Secret>();
        let answers = libc::calloc(count, size_of::<PamResponse>()).cast::<PamResponse>();
        if answers.is_null() {
            return PAM_BUF_ERR;
        }
        let mut status = PAM_SUCCESS;
        for i in 0..count {
            let message = *messages.add(i);
            if message.is_null() {
                continue;
            }
            match (*message).msg_style {
                PAM_PROMPT_ECHO_OFF => {
                    let copy = libc::strdup(password.0.as_ptr());
                    if copy.is_null() {
                        status = PAM_BUF_ERR;
                        break;
                    }
                    (*answers.add(i)).resp = copy;
                }
                // The user is already named; any other question can't be
                // answered from a sign-in screen.
                PAM_PROMPT_ECHO_ON => {
                    status = PAM_CONV_ERR;
                    break;
                }
                // Information and error messages need no answer.
                _ => {}
            }
        }
        if status != PAM_SUCCESS {
            for i in 0..count {
                let reply = (*answers.add(i)).resp;
                if !reply.is_null() {
                    explicit_bzero(reply.cast(), libc::strlen(reply));
                    libc::free(reply.cast());
                }
            }
            libc::free(answers.cast());
            return status;
        }
        *replies = answers;
        PAM_SUCCESS
    }
}
