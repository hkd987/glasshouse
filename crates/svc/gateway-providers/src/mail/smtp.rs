//! SMTP submission (RFC 6409, 8314): TLS from the first byte (465) or
//! STARTTLS before anything else (587), AUTH PLAIN, one message per
//! transaction. The reply that ends DATA is what the audit record keeps.

use glass_managed::mail::{Account, LinkError, Outgoing, Submitted};

use super::wire::{Conn, Dialer, tidy};

/// A reply: its code and text, lines joined.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reply {
    code: u16,
    text: String,
}

impl Reply {
    fn ok(&self) -> bool {
        (200..400).contains(&self.code)
    }

    fn line(&self) -> String {
        format!("{} {}", self.code, self.text)
    }
}

pub(crate) struct Submission {
    conn: Conn,
    extensions: Vec<String>,
}

impl Submission {
    /// Connects and signs in as `account`.
    pub(crate) fn open(dialer: &Dialer, account: &Account) -> Result<Submission, LinkError> {
        let conn = dialer.dial(&account.outgoing)?;
        let mut s = Submission {
            conn,
            extensions: Vec::new(),
        };
        let greeting = s.read_reply()?;
        if greeting.code != 220 {
            return Err(LinkError::Unreachable(format!(
                "{} isn't taking mail now: {}",
                s.conn.host,
                greeting.line()
            )));
        }
        s.ehlo()?;
        if !s.conn.is_tls() {
            if !s.has("STARTTLS") {
                return Err(LinkError::Refused(format!(
                    "{} doesn't offer STARTTLS, so mail can't be sent through it securely",
                    s.conn.host
                )));
            }
            let reply = s.send("STARTTLS")?;
            if reply.code != 220 {
                return Err(s.refused("STARTTLS", &reply));
            }
            s.conn.start_tls(dialer)?;
            s.ehlo()?;
        }
        if !s
            .extensions
            .iter()
            .any(|e| e.starts_with("AUTH ") && e.split(' ').any(|m| m == "PLAIN"))
        {
            return Err(LinkError::Refused(format!(
                "{} doesn't take a password sign-in for sending",
                s.conn.host
            )));
        }
        let mut plain = Vec::new();
        plain.push(0);
        plain.extend_from_slice(account.user.as_bytes());
        plain.push(0);
        plain.extend_from_slice(account.secret.expose().as_bytes());
        let reply = s.send(&format!(
            "AUTH PLAIN {}",
            glass_managed::base64::encode(&plain)
        ))?;
        if reply.code != 235 {
            return Err(LinkError::SignIn(format!(
                "{} didn't accept the password for {}: {}",
                s.conn.host,
                account.user,
                reply.line()
            )));
        }
        Ok(s)
    }

    fn has(&self, extension: &str) -> bool {
        self.extensions
            .iter()
            .any(|e| e.split(' ').next() == Some(extension))
    }

    fn refused(&self, doing: &str, reply: &Reply) -> LinkError {
        LinkError::Refused(format!(
            "{} refused {doing}: {}",
            self.conn.host,
            reply.line()
        ))
    }

    fn ehlo(&mut self) -> Result<(), LinkError> {
        // Our name: an address literal, which needs no DNS of its own.
        let reply = self.send("EHLO [127.0.0.1]")?;
        if reply.code != 250 {
            return Err(self.refused("our greeting", &reply));
        }
        self.extensions = reply
            .text
            .split('\n')
            .skip(1)
            .map(|l| l.trim().to_ascii_uppercase())
            .collect();
        Ok(())
    }

    fn send(&mut self, line: &str) -> Result<Reply, LinkError> {
        self.conn.write_all(format!("{line}\r\n").as_bytes())?;
        self.read_reply()
    }

    fn read_reply(&mut self) -> Result<Reply, LinkError> {
        let mut lines = Vec::new();
        loop {
            let line = self.conn.read_line()?;
            if line.len() < 3 || lines.len() > 100 {
                return Err(LinkError::Refused(format!(
                    "{} sent a reply we couldn't read",
                    self.conn.host
                )));
            }
            let code: u16 = std::str::from_utf8(&line[..3])
                .ok()
                .and_then(|c| c.parse().ok())
                .ok_or_else(|| {
                    LinkError::Refused(format!("{} sent a reply we couldn't read", self.conn.host))
                })?;
            let more = line.get(3) == Some(&b'-');
            lines.push(tidy(line.get(4..).unwrap_or_default()));
            if !more {
                return Ok(Reply {
                    code,
                    text: lines.join("\n"),
                });
            }
        }
    }

    /// One message, to everyone in its envelope.
    pub(crate) fn submit(&mut self, outgoing: &Outgoing<'_>) -> Result<Submitted, LinkError> {
        let path = |addr: &str| {
            if addr
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>'))
            {
                Err(LinkError::Refused(format!("{addr:?} is not an address")))
            } else {
                Ok(format!("<{addr}>"))
            }
        };
        let reply = self.send(&format!("MAIL FROM:{}", path(outgoing.from)?))?;
        if !reply.ok() {
            return Err(self.refused(&format!("sending as {}", outgoing.from), &reply));
        }
        for to in outgoing.to {
            let reply = self.send(&format!("RCPT TO:{}", path(to)?))?;
            if !reply.ok() {
                let _ = self.send("RSET");
                return Err(self.refused(&format!("sending to {to}"), &reply));
            }
        }
        let reply = self.send("DATA")?;
        if reply.code != 354 {
            return Err(self.refused("the message", &reply));
        }
        self.conn.write_all(&dot_stuffed(outgoing.message))?;
        // The whole message went: without the answer, the server may have
        // taken it, so this is neither sent nor not (ADR 0051).
        let reply = self.read_reply().map_err(|e| {
            LinkError::Unknown(format!(
                "{} took the whole message but its answer never came ({e})",
                self.conn.host
            ))
        })?;
        if !reply.ok() {
            return Err(self.refused("the message", &reply));
        }
        Ok(Submitted {
            reply: reply.line().replace('\n', " "),
        })
    }

    /// Signs out; best effort.
    pub(crate) fn quit(mut self) {
        let _ = self.send("QUIT");
    }
}

/// The message as DATA carries it: CRLF line ends, a dot doubled at a
/// line's start, and the closing `.` line.
fn dot_stuffed(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 64);
    let mut at_start = true;
    let mut i = 0;
    while i < message.len() {
        let b = message[i];
        if at_start && b == b'.' {
            out.push(b'.');
        }
        match b {
            b'\r' if message.get(i + 1) == Some(&b'\n') => {
                out.extend_from_slice(b"\r\n");
                i += 1;
                at_start = true;
            }
            b'\n' | b'\r' => {
                out.extend_from_slice(b"\r\n");
                at_start = true;
            }
            _ => {
                out.push(b);
                at_start = false;
            }
        }
        i += 1;
    }
    if !at_start {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_doubles_leading_dots_and_ends_with_a_dot_line() {
        assert_eq!(
            dot_stuffed(b"Subject: x\r\n\r\n.hidden\n..two\r\nend"),
            b"Subject: x\r\n\r\n..hidden\r\n...two\r\nend\r\n.\r\n"
        );
        assert_eq!(dot_stuffed(b"a\r\n"), b"a\r\n.\r\n");
    }
}
