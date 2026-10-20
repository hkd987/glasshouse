//! A line-oriented connection to a mail server: TCP, then TLS from the
//! first byte or after STARTTLS, verified like every connection this crate
//! makes (the base system's trust store and the server's name, ADR 0018).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use glass_managed::mail::{LinkError, Server};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a server may take to answer one command.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest line a server may send us (literals are read apart).
const MAX_LINE: usize = 64 * 1024;

/// How this link reaches servers: TLS, and in tests a certificate to trust
/// and an address for a name.
#[derive(Debug, Clone)]
pub(crate) struct Dialer {
    tls: Result<native_tls::TlsConnector, String>,
    /// `(host, address)`: connect to `address` when policy names `host`,
    /// still checking the certificate against `host`.
    resolve: Vec<(String, std::net::IpAddr)>,
}

impl Dialer {
    pub(crate) fn new(roots: &[Vec<u8>], resolve: Vec<(String, std::net::IpAddr)>) -> Self {
        let tls = (|| {
            let mut builder = native_tls::TlsConnector::builder();
            builder.min_protocol_version(Some(native_tls::Protocol::Tlsv12));
            for pem in roots {
                builder.add_root_certificate(
                    native_tls::Certificate::from_pem(pem).map_err(|e| e.to_string())?,
                );
            }
            builder.build().map_err(|e| e.to_string())
        })();
        Self { tls, resolve }
    }

    /// A connection to `server`, in TLS already when it takes TLS from the
    /// first byte.
    pub(crate) fn dial(&self, server: &Server) -> Result<Conn, LinkError> {
        let addrs: Vec<SocketAddr> = match self
            .resolve
            .iter()
            .find(|(host, _)| host.eq_ignore_ascii_case(&server.host))
        {
            Some((_, ip)) => vec![SocketAddr::new(*ip, server.port)],
            None => (server.host.as_str(), server.port)
                .to_socket_addrs()
                .map_err(|_| unreachable(&server.host, "its name doesn't resolve"))?
                .collect(),
        };
        let mut last = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(tcp) => {
                    tcp.set_read_timeout(Some(IO_TIMEOUT))
                        .and_then(|()| tcp.set_write_timeout(Some(IO_TIMEOUT)))
                        .map_err(|e| io_error(&server.host, &e))?;
                    let mut conn = Conn {
                        stream: Stream::Plain(tcp),
                        buf: Vec::new(),
                        host: server.host.clone(),
                    };
                    if server.implicit_tls {
                        conn.start_tls(self)?;
                    }
                    return Ok(conn);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(match last {
            Some(e) => io_error(&server.host, &e),
            None => unreachable(&server.host, "it has no address"),
        })
    }

    fn connector(&self) -> Result<&native_tls::TlsConnector, LinkError> {
        self.tls.as_ref().map_err(|e| {
            LinkError::Unreachable(format!("TLS isn't available on this computer: {e}"))
        })
    }
}

fn unreachable(host: &str, why: &str) -> LinkError {
    LinkError::Unreachable(format!("{host} can't be reached: {why}"))
}

pub(crate) fn io_error(host: &str, e: &std::io::Error) -> LinkError {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            unreachable(host, "it didn't answer in time")
        }
        std::io::ErrorKind::ConnectionRefused => unreachable(host, "it refused the connection"),
        std::io::ErrorKind::UnexpectedEof => unreachable(host, "it closed the connection"),
        _ => unreachable(host, &e.kind().to_string()),
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<native_tls::TlsStream<TcpStream>>),
    /// Mid-upgrade, or after a failed one.
    Gone,
}

/// One connection, read a line or a counted run of bytes at a time.
pub(crate) struct Conn {
    stream: Stream,
    /// Read but not yet taken.
    buf: Vec<u8>,
    pub(crate) host: String,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

impl Conn {
    /// Upgrades to TLS (STARTTLS, or the first byte). Nothing may have
    /// been sent past the server's go-ahead.
    pub(crate) fn start_tls(&mut self, dialer: &Dialer) -> Result<(), LinkError> {
        if !self.buf.is_empty() {
            return Err(LinkError::Refused(format!(
                "{} sent data before TLS started",
                self.host
            )));
        }
        let Stream::Plain(tcp) = std::mem::replace(&mut self.stream, Stream::Gone) else {
            return Err(LinkError::Refused("TLS already started".into()));
        };
        let tls = dialer.connector()?.connect(&self.host, tcp).map_err(|e| {
            LinkError::Unreachable(format!(
                "{} can't be reached securely: TLS failed ({})",
                self.host,
                first_line(&e.to_string())
            ))
        })?;
        self.stream = Stream::Tls(Box::new(tls));
        Ok(())
    }

    pub(crate) fn is_tls(&self) -> bool {
        matches!(self.stream, Stream::Tls(_))
    }

    pub(crate) fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), LinkError> {
        let tcp = match &self.stream {
            Stream::Plain(tcp) => tcp,
            Stream::Tls(tls) => tls.get_ref(),
            Stream::Gone => return Ok(()),
        };
        tcp.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
            .map_err(|e| io_error(&self.host, &e))
    }

    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> Result<(), LinkError> {
        let host = self.host.clone();
        let result = match &mut self.stream {
            Stream::Plain(tcp) => tcp.write_all(bytes).and_then(|()| tcp.flush()),
            Stream::Tls(tls) => tls.write_all(bytes).and_then(|()| tls.flush()),
            Stream::Gone => return Err(unreachable(&host, "the connection is closed")),
        };
        result.map_err(|e| io_error(&host, &e))
    }

    fn fill(&mut self) -> std::io::Result<usize> {
        let mut chunk = [0u8; 16 * 1024];
        let n = match &mut self.stream {
            Stream::Plain(tcp) => tcp.read(&mut chunk)?,
            Stream::Tls(tls) => tls.read(&mut chunk)?,
            Stream::Gone => 0,
        };
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(n)
    }

    /// The next line, without its CRLF; a raw I/O error when nothing came.
    pub(crate) fn read_line_io(&mut self) -> std::io::Result<Vec<u8>> {
        let mut searched = 0;
        loop {
            if let Some(at) = self.buf[searched..].windows(2).position(|w| w == b"\r\n") {
                let at = searched + at;
                let line = self.buf[..at].to_vec();
                self.buf.drain(..at + 2);
                return Ok(line);
            }
            searched = self.buf.len().saturating_sub(1);
            if self.buf.len() > MAX_LINE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "line too long",
                ));
            }
            self.fill()?;
        }
    }

    pub(crate) fn read_line(&mut self) -> Result<Vec<u8>, LinkError> {
        self.read_line_io().map_err(|e| match e.kind() {
            std::io::ErrorKind::InvalidData => {
                LinkError::Refused(format!("{} sent a line that's too long", self.host))
            }
            _ => io_error(&self.host, &e),
        })
    }

    /// Exactly `n` bytes.
    pub(crate) fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, LinkError> {
        while self.buf.len() < n {
            self.fill().map_err(|e| io_error(&self.host, &e))?;
        }
        Ok(self.buf.drain(..n).collect())
    }
}

/// Text from a server, safe to show: one line, printable, short.
pub(crate) fn tidy(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    clean.trim().to_owned()
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect()
}
