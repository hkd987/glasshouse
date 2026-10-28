//! Running a task's process in a Firecracker computer (ADR 0065 §10): the
//! guest has no network device, so the host reaches it over vsock.
//!
//! Two programs speak this: `glass-vmexec` on the host
//! ([`super::vmexec`]), which [`super::FirecrackerHypervisor`] returns as a
//! task's command, and `glass-vmguest` in the guest ([`super::vmguest`]).
//!
//! # The protocol
//!
//! The guest's exec service is reached on vsock port [`EXEC_PORT`]. For
//! each task the host runs `glass-vmexec --uds <vsock socket> --port
//! <EXEC_PORT> --socket <task socket> -- <request>`, where `<vsock socket>`
//! is Firecracker's vsock socket in the computer's jail and `<task socket>`
//! the task's socket on the host. The helper:
//!
//! 1. Listens on `<vsock socket>_<socket.port>` (the port from the
//!    request), relaying each connection made there to the task socket.
//!    That is how the task socket reaches the guest: Firecracker hands the
//!    guest's connections to host port `P` (CID 2) to whatever listens on
//!    `<vsock socket>_P`.
//! 2. Connects to `<vsock socket>` and sends `CONNECT <EXEC_PORT>\n`.
//!    Firecracker connects it to the guest and answers `OK <n>\n`; if no
//!    one listens, it closes the connection.
//! 3. Sends the request: one line of JSON ([`ExecRequest`]) ending in `\n`,
//!    then nothing more, keeping its side open: the connection ending is
//!    how the guest learns the task is over. JSON escapes newlines in
//!    values, so the line is whole:
//!
//!    ```json
//!    {"program":"/usr/local/bin/agent","args":["--task","t-1"],
//!     "env":[["HOME","/root"],["LANG","C.UTF-8"]],"workdir":"/work/t-1",
//!     "socket":{"path":"/run/glasshouse/agent.sock","port":20000}}
//!    ```
//!    (on one line). `env` is the whole environment, in order; nothing is
//!    inherited from the service.
//! 4. Reads frames ([`Frame`]) until the last one, writing `out` to its
//!    standard output and `err` to its standard error as they come:
//!    - `out <n>\n` then `n` bytes: the process's standard output;
//!    - `err <n>\n` then `n` bytes: its standard error;
//!    - `exit {"code":0}\n` or `exit {"signal":9}\n`: it ended. Last frame;
//!      the helper exits with the code, or 128 + the signal;
//!    - `fail {"error":"..."}\n`: it couldn't be started. Last frame; the
//!      helper says why on its standard error and exits with 127.
//!
//!    If it can't reach the guest, or the connection ends before the last
//!    frame, the helper says so and exits with 125.
//!
//! The guest service runs `program` with `args` and exactly `env`, in
//! `workdir` (made if missing), standard input from `/dev/null`, in a
//! process group of its own. For that process alone (its own mount
//! namespace, so tasks running at once each see their own) it provides a
//! socket at `socket.path` ([`AGENT_SOCKET_PATH`]) and relays each
//! connection to it to the host: vsock CID 2, port `socket.port`.
//!
//! The task's processes end with the task (PA-2): when the helper's
//! connection closes, or the helper is killed, the service kills the
//! process group; and when the process ends, what it left running goes
//! too. A snapshot closes every vsock connection (Firecracker resets the
//! device), so a computer is suspended between tasks, never during one.
//!
//! The helper reaches sockets deep in a computer's folder through an open
//! descriptor of their folder: a socket address holds about 107 bytes. It
//! runs as root next to a jail an escaped guest could write to, so it
//! follows no link there, and replaces only a socket where it makes its
//! forwarding socket.

use std::io::{self, BufRead, Read, Write};
use std::net::Shutdown;
use std::ops::Range;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;

use glass_platform::sandbox::AGENT_SOCKET_PATH;
use serde::{Deserialize, Serialize};

use crate::hypervisor::TaskRun;

/// The guest's exec service listens here.
pub const EXEC_PORT: u32 = 10_000;

/// Host ports a task's socket is forwarded from, one per task running.
pub const FORWARD_PORTS: Range<u32> = 20_000..30_000;

/// The longest request line read.
pub const MAX_REQUEST: usize = 1024 * 1024;
/// The most output one `out` or `err` frame carries.
pub const MAX_FRAME: usize = 1024 * 1024;
/// The longest frame head (`exit` and `fail` are all head).
const MAX_HEAD: usize = 64 * 1024;

/// The one line the helper sends the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    /// As the guest sees it.
    pub program: String,
    pub args: Vec<String>,
    /// The complete environment, `[name, value]` pairs in order.
    pub env: Vec<(String, String)>,
    /// The task's folder in the guest: `/work/<task>`.
    pub workdir: String,
    pub socket: SocketForward,
}

/// The task socket, as the guest provides it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketForward {
    /// Where the process finds it: [`AGENT_SOCKET_PATH`].
    pub path: String,
    /// The host port its connections go to.
    pub port: u32,
}

impl ExecRequest {
    /// The request for `run`, its socket forwarded from host port `port`.
    pub fn new(run: &TaskRun, port: u32) -> Result<Self, String> {
        let utf8 = |path: &Path, what: &str| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("The task's {what} isn't UTF-8: {}.", path.display()))
        };
        Ok(Self {
            program: utf8(&run.process.program, "program")?,
            args: run.process.args.clone(),
            env: run.process.env.clone(),
            workdir: utf8(&run.workdir, "folder")?,
            socket: SocketForward {
                path: AGENT_SOCKET_PATH.into(),
                port,
            },
        })
    }

    /// The request as its line, without the `\n`.
    pub fn line(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("Couldn't write a task's request: {e}."))
    }
}

/// The helper's command: `<vmexec> --uds <uds> --port <EXEC_PORT> --socket
/// <task socket> -- <request>`.
pub fn helper_command(
    vmexec: &Path,
    uds: &Path,
    task_socket: &Path,
    request: &ExecRequest,
) -> Result<Command, String> {
    let mut command = Command::new(vmexec);
    command
        .arg("--uds")
        .arg(uds)
        .arg("--port")
        .arg(EXEC_PORT.to_string())
        .arg("--socket")
        .arg(task_socket)
        .arg("--")
        .arg(request.line()?);
    Ok(command)
}

/// What the guest sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Out(Vec<u8>),
    Err(Vec<u8>),
    /// It ended: the last frame.
    Exit(Ended),
    /// It couldn't be started, and why: the last frame.
    Fail(String),
}

/// How a task's process ended: `{"code":0}` or `{"signal":9}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ended {
    Code(i32),
    Signal(i32),
}

impl Ended {
    /// The helper's exit code: the process's, or 128 + its signal.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Code(code) => code.rem_euclid(256) as u8,
            Self::Signal(signal) => (128 + signal).clamp(0, 255) as u8,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Failure {
    error: String,
}

/// Writes one frame, whole, and flushes it.
pub fn write_frame(to: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let bytes = match frame {
        Frame::Out(data) | Frame::Err(data) => {
            let kind = if matches!(frame, Frame::Out(_)) {
                "out"
            } else {
                "err"
            };
            let mut bytes = format!("{kind} {}\n", data.len()).into_bytes();
            bytes.extend_from_slice(data);
            bytes
        }
        Frame::Exit(ended) => format!("exit {}\n", json(ended)?).into_bytes(),
        Frame::Fail(error) => format!(
            "fail {}\n",
            json(&Failure {
                error: error.clone()
            })?
        )
        .into_bytes(),
    };
    to.write_all(&bytes)?;
    to.flush()
}

/// Reads one frame; `None` if the connection ended between frames.
pub fn read_frame(from: &mut impl BufRead) -> io::Result<Option<Frame>> {
    let Some(head) = read_line(from, MAX_HEAD)? else {
        return Ok(None);
    };
    let Some((kind, rest)) = head.split_once(' ') else {
        return Err(invalid(format!("a frame {head:?}")));
    };
    let frame = match kind {
        "out" | "err" => {
            let length: usize = rest
                .parse()
                .map_err(|_| invalid(format!("a frame {head:?}")))?;
            if length > MAX_FRAME {
                return Err(invalid(format!("a frame of {length} bytes")));
            }
            let mut data = vec![0; length];
            from.read_exact(&mut data)?;
            if kind == "out" {
                Frame::Out(data)
            } else {
                Frame::Err(data)
            }
        }
        "exit" => Frame::Exit(
            serde_json::from_str(rest).map_err(|_| invalid(format!("a frame {head:?}")))?,
        ),
        "fail" => Frame::Fail(
            serde_json::from_str::<Failure>(rest)
                .map_err(|_| invalid(format!("a frame {head:?}")))?
                .error,
        ),
        _ => return Err(invalid(format!("a frame {head:?}"))),
    };
    Ok(Some(frame))
}

/// Reads the request line.
pub fn read_request(from: &mut impl BufRead) -> io::Result<ExecRequest> {
    let line = read_line(from, MAX_REQUEST)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the connection ended before a request",
        )
    })?;
    serde_json::from_str(&line).map_err(|e| invalid(format!("a request: {e}")))
}

/// One line without its `\n`, at most `max` bytes; `None` at the end.
pub(crate) fn read_line(from: &mut impl BufRead, max: usize) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    from.take(max as u64 + 1).read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.pop() != Some(b'\n') {
        return Err(if line.len() >= max {
            invalid(format!("a line longer than {max} bytes"))
        } else {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the connection ended mid-line",
            )
        });
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| invalid("a line that isn't UTF-8".into()))
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {what}"))
}

/// Copies both ways between two connections until both are done, each
/// side told when the other has no more to say.
pub fn splice(a: UnixStream, b: UnixStream) {
    let (Ok(a_in), Ok(b_out)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let forth = std::thread::spawn(move || {
        copy(&mut &a_in, &mut &b_out);
        let _ = b_out.shutdown(Shutdown::Write);
    });
    copy(&mut &b, &mut &a);
    let _ = a.shutdown(Shutdown::Write);
    let _ = forth.join();
}

/// Copies until `from` ends or either side fails, each piece passed on as
/// it comes. Not `io::copy`: between a socket and a pipe it splices in the
/// kernel, which here held what it read from a socket instead of putting it
/// in the pipe, and the process at the other end waited forever.
pub fn copy(from: &mut impl Read, to: &mut impl Write) {
    let mut buf = vec![0; 64 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if to.write_all(&buf[..n]).and_then(|()| to.flush()).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn json(value: &impl Serialize) -> io::Result<String> {
    serde_json::to_string(value).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adr0065_frames_read_back_as_written() {
        let frames = [
            Frame::Out(b"two\nlines\n".to_vec()),
            Frame::Err(vec![0, 255, b'\n']),
            Frame::Out(vec![]),
            Frame::Exit(Ended::Signal(9)),
        ];
        let mut wire = Vec::new();
        for frame in &frames {
            write_frame(&mut wire, frame).unwrap();
        }
        write_frame(&mut wire, &Frame::Fail("no \"agent\" here".into())).unwrap();
        let text = String::from_utf8_lossy(&wire);
        assert!(text.starts_with("out 10\ntwo\nlines\nerr 3\n"), "{text}");
        assert!(text.contains("exit {\"signal\":9}\n"), "{text}");
        assert!(
            text.ends_with("fail {\"error\":\"no \\\"agent\\\" here\"}\n"),
            "{text}"
        );

        let mut from = &wire[..];
        for frame in &frames {
            assert_eq!(read_frame(&mut from).unwrap().as_ref(), Some(frame));
        }
        assert_eq!(
            read_frame(&mut from).unwrap(),
            Some(Frame::Fail("no \"agent\" here".into()))
        );
        assert_eq!(read_frame(&mut from).unwrap(), None);

        for bad in [
            "out 5\nabc",
            "out x\n",
            "exit {\"cod\":1}\n",
            "what 1\n",
            "exit",
            "out 2000000\n",
        ] {
            assert!(read_frame(&mut bad.as_bytes()).is_err(), "{bad:?}");
        }
        assert_eq!(Ended::Code(3).exit_code(), 3);
        assert_eq!(Ended::Signal(15).exit_code(), 143);
    }

    #[test]
    fn adr0065_a_request_is_one_line() {
        let request = ExecRequest {
            program: "/bin/echo".into(),
            args: vec!["a\nb".into()],
            env: vec![("A".into(), "1".into())],
            workdir: "/work/t-1".into(),
            socket: SocketForward {
                path: AGENT_SOCKET_PATH.into(),
                port: 20_000,
            },
        };
        let wire = format!("{}\n", request.line().unwrap());
        assert_eq!(wire.matches('\n').count(), 1);
        assert_eq!(read_request(&mut wire.as_bytes()).unwrap(), request);
        assert!(read_request(&mut &b""[..]).is_err());
        assert!(read_request(&mut &b"{\"program\":1}\n"[..]).is_err());
        let long = vec![b'x'; MAX_REQUEST + 10];
        assert!(read_request(&mut &long[..]).is_err());
    }
}
