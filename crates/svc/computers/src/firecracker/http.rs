//! One HTTP/1.1 exchange over a Unix socket, all Firecracker's API needs:
//! one request per connection, the reply's body framed by its
//! Content-Length (Firecracker's server sends one whenever there is a
//! body, and keeps the connection open after, so we never read to its end).

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The most of a reply read: Firecracker answers in small JSON.
const MAX_REPLY: u64 = 1024 * 1024;
/// The longest path a Unix socket address holds (`sun_path`, less its NUL).
const MAX_SOCKET_PATH: usize = 107;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Connects to the socket at `path`, however long the path is.
pub(crate) fn connect(path: &Path) -> io::Result<UnixStream> {
    let (_dir, path) = addressable(path)?;
    UnixStream::connect(path)
}

/// Makes a socket at `path` and listens on it, however long the path is.
pub(crate) fn bind(path: &Path) -> io::Result<UnixListener> {
    let (_dir, path) = addressable(path)?;
    UnixListener::bind(path)
}

/// A path to `path` that fits in a socket address, and the folder it goes
/// through, to hold open while it's used. A path too long (a jail deep in a
/// computer's folder) goes through an open descriptor of its folder
/// instead (Linux's /proc).
fn addressable(path: &Path) -> io::Result<(Option<File>, PathBuf)> {
    if path.as_os_str().len() <= MAX_SOCKET_PATH {
        return Ok((None, path.to_owned()));
    }
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "that isn't a path to a socket",
        ));
    };
    let dir = File::open(dir)?;
    let short = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(name);
    Ok((Some(dir), short))
}

/// Sends one request, with a JSON body if it has one, and reads the reply.
pub(crate) fn exchange(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<Reply> {
    let mut stream = connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(&request(method, path, body))?;
    read_reply(stream)
}

/// The request's bytes, headers as curl sends them in Firecracker's docs.
pub(crate) fn request(method: &str, path: &str, body: Option<&[u8]>) -> Vec<u8> {
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n");
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.unwrap_or_default());
    bytes
}

/// Reads one reply: its status line, its head, and Content-Length bytes.
pub(crate) fn read_reply(stream: impl Read) -> io::Result<Reply> {
    let mut reader = BufReader::new(stream.take(MAX_REPLY));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status =
        status(&line).ok_or_else(|| invalid(format!("its reply began {:?}", line.trim_end())))?;
    let mut length = 0u64;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the reply ended in its head",
            ));
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(invalid(format!("a header line {header:?}")));
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value
                .trim()
                .parse()
                .map_err(|_| invalid(format!("a Content-Length of {:?}", value.trim())))?;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(invalid(format!("a reply in {} encoding", value.trim())));
        }
    }
    if length > MAX_REPLY {
        return Err(invalid(format!("a reply of {length} bytes")));
    }
    let mut body = vec![0; length as usize];
    reader.read_exact(&mut body)?;
    Ok(Reply { status, body })
}

/// The status in `HTTP/1.1 204 ` (Firecracker leaves the reason out).
fn status(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    if !parts.next()?.starts_with("HTTP/1.") {
        return None;
    }
    parts.next()?.parse().ok()
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {what}"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn adr0065_firecracker_replies_are_read_by_their_length() {
        let empty = read_reply(
            &b"HTTP/1.1 204 \r\nServer: Firecracker API\r\nConnection: keep-alive\r\n\r\n"[..],
        )
        .unwrap();
        assert_eq!(
            empty,
            Reply {
                status: 204,
                body: vec![]
            }
        );

        let refusal = read_reply(
            &b"HTTP/1.1 400 \r\nServer: Firecracker API\r\nConnection: keep-alive\r\n\
               Content-Type: application/json\r\nContent-Length: 22\r\n\r\n\
               {\"fault_message\":\"no\"}and the next reply"[..],
        )
        .unwrap();
        assert_eq!(refusal.status, 400);
        assert_eq!(refusal.body, br#"{"fault_message":"no"}"#);

        for bad in [
            &b"SSH-2.0-OpenSSH\r\n\r\n"[..],
            b"HTTP/1.1 200 \r\nContent-Length: 10\r\n\r\nshort",
            b"HTTP/1.1 200 \r\nContent-Length: 99999999999\r\n\r\n",
            b"HTTP/1.1 200 \r\nTransfer-Encoding: chunked\r\n\r\n",
            b"HTTP/1.1 200 \r\nServer: Firecracker API\r\n",
        ] {
            assert!(read_reply(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn adr0065_requests_carry_json_as_firecrackers_docs_send_it() {
        assert_eq!(
            String::from_utf8(request("PATCH", "/vm", Some(br#"{"state":"Paused"}"#))).unwrap(),
            "PATCH /vm HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\
             Content-Type: application/json\r\nContent-Length: 18\r\n\r\n{\"state\":\"Paused\"}"
        );
        assert_eq!(
            String::from_utf8(request("GET", "/", None)).unwrap(),
            "GET / HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\r\n"
        );
    }

    #[test]
    fn adr0065_a_socket_deep_in_a_computers_folder_is_reached() {
        let base = std::env::temp_dir().join(format!("glass-fc-http-{}", std::process::id()));
        let deep = base
            .join("computers/a-computer-with-a-long-name/jail/firecracker")
            .join("a-computer-with-a-long-name/root");
        fs_reset(&base, &deep);
        // Where no socket address reaches.
        let long = deep.join("api.sock");
        assert!(long.as_os_str().len() > MAX_SOCKET_PATH);
        assert!(UnixListener::bind(&long).is_err());
        let listener = bind(&long).unwrap();
        assert!(long.exists());

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            (&stream).write_all(b"HTTP/1.1 204 \r\n\r\n").unwrap();
            // Firecracker keeps the connection; the client mustn't wait for its end.
            let _ = reader.read_to_end(&mut Vec::new());
        });
        let reply = exchange(&long, "GET", "/", None, Duration::from_secs(5)).unwrap();
        assert_eq!(reply.status, 204);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    fn fs_reset(base: &Path, deep: &Path) {
        let _ = std::fs::remove_dir_all(base);
        std::fs::create_dir_all(deep).unwrap();
    }
}
