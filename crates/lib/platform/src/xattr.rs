//! Metadata stored with files as extended attributes, so it follows the file
//! through copies that preserve attributes and through ZFS send/receive:
//!
//! - the data label (DP-10), `glasshouse.label`;
//! - agent attribution, "by [agent] for [user]" (AU-10), `glasshouse.attribution`.
//!
//! FreeBSD: the `user` namespace. Linux (development hosts): `user.` prefix.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use glass_types::Label;

/// The label attribute name within the `user` namespace.
pub const LABEL_ATTR: &str = "glasshouse.label";
/// The attribution attribute name within the `user` namespace.
pub const ATTRIBUTION_ATTR: &str = "glasshouse.attribution";

/// The label stored on `path`, if any. A missing attribute is `Ok(None)`; a
/// value that is not a known label is an error, so a typo never silently
/// downgrades protection.
pub fn read_label(path: &Path) -> io::Result<Option<Label>> {
    let Some(raw) = get(path, LABEL_ATTR)? else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&raw);
    text.trim().parse::<Label>().map(Some).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        )
    })
}

pub fn write_label(path: &Path, label: Label) -> io::Result<()> {
    set(path, LABEL_ATTR, label.as_str().as_bytes())
}

/// Records who changed a file, such as "by invoice-demo for demo".
pub fn write_attribution(path: &Path, attribution: &str) -> io::Result<()> {
    set(path, ATTRIBUTION_ATTR, attribution.as_bytes())
}

pub fn read_attribution(path: &Path) -> io::Result<Option<String>> {
    Ok(get(path, ATTRIBUTION_ATTR)?.map(|raw| String::from_utf8_lossy(&raw).into_owned()))
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

#[cfg(target_os = "freebsd")]
fn get(path: &Path, attr: &str) -> io::Result<Option<Vec<u8>>> {
    let p = c_path(path)?;
    let name = CString::new(attr).expect("attribute names have no NUL");
    let mut buf = vec![0u8; 256];
    // SAFETY: `p` and `name` are valid C strings; `buf` is valid for writes
    // of `buf.len()` bytes for the duration of the call.
    let n = unsafe {
        libc::extattr_get_file(
            p.as_ptr(),
            libc::EXTATTR_NAMESPACE_USER,
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        let err = io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::ENOATTR) | Some(libc::EOPNOTSUPP) => Ok(None),
            _ => Err(err),
        };
    }
    buf.truncate(n as usize);
    Ok(Some(buf))
}

#[cfg(target_os = "freebsd")]
fn set(path: &Path, attr: &str, value: &[u8]) -> io::Result<()> {
    let p = c_path(path)?;
    let name = CString::new(attr).expect("attribute names have no NUL");
    // SAFETY: `p` and `name` are valid C strings; `value` is valid for reads
    // of `value.len()` bytes.
    let n = unsafe {
        libc::extattr_set_file(
            p.as_ptr(),
            libc::EXTATTR_NAMESPACE_USER,
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
        )
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn linux_name(attr: &str) -> CString {
    CString::new(format!("user.{attr}")).expect("attribute names have no NUL")
}

#[cfg(target_os = "linux")]
fn get(path: &Path, attr: &str) -> io::Result<Option<Vec<u8>>> {
    let p = c_path(path)?;
    let name = linux_name(attr);
    let mut buf = vec![0u8; 256];
    // SAFETY: `p` and `name` are valid C strings; `buf` is valid for writes
    // of `buf.len()` bytes for the duration of the call.
    let n = unsafe {
        libc::getxattr(
            p.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n < 0 {
        let err = io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::ENODATA) | Some(libc::ENOTSUP) => Ok(None),
            _ => Err(err),
        };
    }
    buf.truncate(n as usize);
    Ok(Some(buf))
}

#[cfg(target_os = "linux")]
fn set(path: &Path, attr: &str, value: &[u8]) -> io::Result<()> {
    let p = c_path(path)?;
    let name = linux_name(attr);
    // SAFETY: `p` and `name` are valid C strings; `value` is valid for reads
    // of `value.len()` bytes.
    let rc = unsafe {
        libc::setxattr(
            p.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "freebsd", target_os = "linux")))]
fn get(_path: &Path, _attr: &str) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

#[cfg(not(any(target_os = "freebsd", target_os = "linux")))]
fn set(_path: &Path, _attr: &str, _value: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "labels need extended attributes",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlabeled_files_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.txt");
        std::fs::write(&path, "x").unwrap();
        assert_eq!(read_label(&path).unwrap(), None);
    }

    #[test]
    fn labels_round_trip_where_supported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("labeled.txt");
        std::fs::write(&path, "x").unwrap();
        match write_label(&path, Label::Confidential) {
            Ok(()) => assert_eq!(read_label(&path).unwrap(), Some(Label::Confidential)),
            // Some filesystems (tmpfs without user xattrs, overlayfs) refuse.
            Err(e) => eprintln!("skipping: user xattrs unsupported here: {e}"),
        }
    }
}
