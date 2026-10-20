//! Messages: reading what arrives, whatever its MIME structure, encodings
//! and charsets (mail-parser), and building what glassd sends
//! (mail-builder). Everything read is untrusted text: it reaches agents as
//! text, and the Mail app shows HTML only in a frame with no scripts and no
//! network (ADR 0037 §6).

use glass_managed::Sender;
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::text::Text;
use mail_parser::MessageParser;
use serde::Serialize;

/// The header that carries "by [agent] for [user]" (AU-10).
pub(crate) const ATTRIBUTION_HEADER: &str = "X-Glasshouse-Attribution";
/// The task a message came from.
pub(crate) const TASK_HEADER: &str = "X-Glasshouse-Task";
/// The most of a message's HTML the Mail app is handed.
const MAX_HTML: usize = 2 * 1024 * 1024;
/// A snippet's length, in characters.
const SNIPPET: usize = 140;

/// "by invoice-demo for demo", split into who wrote it and for whom.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Attribution {
    pub text: String,
    pub by: String,
    #[serde(rename = "for")]
    pub for_user: String,
}

impl Attribution {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (by, for_user) = text.strip_prefix("by ")?.rsplit_once(" for ")?;
        Some(Self {
            text: text.to_owned(),
            by: by.to_owned(),
            for_user: for_user.to_owned(),
        })
    }
}

/// A message as listings and reads describe it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Parsed {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    /// RFC 3339, in UTC; empty when the message has no date we can read.
    pub date: String,
    /// Seconds since 1970, for ordering.
    pub when: i64,
    pub snippet: String,
    pub attribution: Option<Attribution>,
    /// The text: the message's text part, or its HTML reduced to text.
    pub body: String,
    /// Its HTML part, if it has one.
    pub html: Option<String>,
    pub message_id: Option<String>,
}

fn mailbox(name: Option<&str>, address: Option<&str>) -> String {
    match (name.map(str::trim).filter(|n| !n.is_empty()), address) {
        (Some(name), Some(address)) => format!("{name} <{address}>"),
        (None, Some(address)) => address.to_owned(),
        (Some(name), None) => name.to_owned(),
        (None, None) => String::new(),
    }
}

fn addresses(address: Option<&mail_parser::Address<'_>>) -> Vec<String> {
    address
        .map(|a| {
            a.iter()
                .map(|addr| mailbox(addr.name(), addr.address()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// One line, printable: what a header or snippet may show.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A date header as RFC 3339 and seconds: RFC 5322, or the RFC 3339 the
/// local store wrote before EM0.
fn date_of(message: &mail_parser::Message<'_>) -> (String, i64) {
    if let Some(date) = message.date().filter(|d| d.is_valid()) {
        let when = date.to_timestamp();
        return (rfc3339(when), when);
    }
    let raw = message.header_raw("Date").unwrap_or_default().trim();
    match time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339) {
        Ok(t) => (rfc3339(t.unix_timestamp()), t.unix_timestamp()),
        Err(_) => (String::new(), 0),
    }
}

fn rfc3339(seconds: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// Reads a stored message. Anything can arrive, so nothing fails: what
/// can't be read comes back empty.
pub(crate) fn parse(raw: &[u8]) -> Parsed {
    let Some(message) = MessageParser::default().parse(raw) else {
        return Parsed {
            from: String::new(),
            to: Vec::new(),
            subject: String::new(),
            date: String::new(),
            when: 0,
            snippet: String::new(),
            attribution: None,
            body: String::from_utf8_lossy(raw).into_owned(),
            html: None,
            message_id: None,
        };
    };
    let (date, when) = date_of(&message);
    let body = message
        .body_text(0)
        .map(|t| t.into_owned())
        .unwrap_or_default();
    let html = (message.html_body_count() > 0)
        .then(|| message.html_part(0))
        .flatten()
        .and_then(|part| match &part.body {
            mail_parser::PartType::Html(html) => Some(html.as_ref().to_owned()),
            _ => None,
        })
        .filter(|h| h.len() <= MAX_HTML);
    let snippet: String = one_line(&body).chars().take(SNIPPET).collect();
    let attribution = message
        .header(ATTRIBUTION_HEADER)
        .and_then(|h| h.as_text())
        .map(one_line)
        .and_then(|t| Attribution::parse(&t));
    Parsed {
        from: one_line(&addresses(message.from()).join(", ")),
        to: addresses(message.to())
            .into_iter()
            .map(|a| one_line(&a))
            .collect(),
        subject: one_line(message.subject().unwrap_or_default()),
        date,
        when,
        snippet,
        attribution,
        body,
        html,
        message_id: message.message_id().map(str::to_owned),
    }
}

/// A message glassd sends or keeps, as the person and agent it is from.
#[derive(Debug, Clone)]
pub(crate) struct Draft<'a> {
    pub from: &'a Sender,
    pub to: &'a [String],
    pub subject: &'a str,
    pub body: &'a str,
    /// "by [agent] for [user]" (AU-10).
    pub attribution: &'a str,
    pub task: &'a str,
    /// `<id@domain>` without the brackets.
    pub message_id: &'a str,
    /// Seconds since 1970.
    pub date: i64,
}

/// A new Message-ID on the sender's domain: time-ordered and random
/// (UUIDv7), so no two computers make the same one.
pub(crate) fn new_message_id(from: &Sender) -> String {
    let domain = from
        .email
        .as_deref()
        .and_then(|e| e.rsplit_once('@'))
        .map_or("glasshouse.invalid", |(_, d)| d);
    format!("{}@{domain}", uuid::Uuid::now_v7().simple())
}

/// The message as it leaves: RFC 5322, CRLF, its body in UTF-8 text.
pub(crate) fn build(draft: &Draft<'_>) -> Vec<u8> {
    let from = match &draft.from.email {
        Some(email) => Address::new_address(Some(draft.from.name.clone()), email.clone()),
        None => Address::new_address(None::<String>, draft.from.name.clone()),
    };
    let to = Address::new_list(
        draft
            .to
            .iter()
            .map(|a| Address::new_address(None::<String>, a.clone()))
            .collect(),
    );
    let mut body = draft.body.replace("\r\n", "\n");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    MessageBuilder::new()
        .from(from)
        .to(to)
        .subject(draft.subject)
        .date(draft.date)
        .message_id(draft.message_id)
        .header(ATTRIBUTION_HEADER, Text::new(draft.attribution))
        .header(TASK_HEADER, Text::new(draft.task))
        .text_body(body)
        .write_to_vec()
        .unwrap_or_default()
}

/// What a message larger than the sync's cap keeps on this computer: its
/// header, and a line saying why that's all.
pub(crate) fn header_only(header: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(header);
    let mut out = String::with_capacity(text.len() + 256);
    // Its own Content-* fields describe a body that isn't here: dropped,
    // with their folded lines.
    let mut dropping = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\r', '\n']);
        if bare.is_empty() {
            break;
        }
        if bare.starts_with([' ', '\t']) {
            if !dropping {
                out.push_str(bare);
                out.push_str("\r\n");
            }
            continue;
        }
        let name = bare.split(':').next().unwrap_or_default().trim();
        dropping = name.to_ascii_lowercase().starts_with("content-");
        if !dropping {
            out.push_str(bare);
            out.push_str("\r\n");
        }
    }
    out.push_str(
        "X-Glasshouse-Truncated: yes\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
         This message is too large to keep on this computer; only its header is here. \
         Open it in another mail app.\r\n",
    );
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv3_html_only_mail_reads_as_text_and_keeps_its_html_apart() {
        let raw = b"From: \"Dana Ruiz\" <dana@northwind.example>\r\nTo: demo@corp.example, \
                    =?utf-8?q?Priya_Sh=C3=A4h?= <priya@corp.example>\r\n\
                    Subject: =?utf-8?b?UmVuZXdhbCB0ZXJtcyDinJM=?=\r\n\
                    Date: Mon, 19 Oct 2026 10:00:00 +0200\r\nMIME-Version: 1.0\r\n\
                    Content-Type: text/html; charset=utf-8\r\n\r\n\
                    <p>Hi,</p><script>steal()</script><p>Can we <b>talk</b>?</p>\
                    <img src=\"https://tracker.example/p.gif\">\r\n";
        let parsed = parse(raw);
        assert_eq!(parsed.from, "Dana Ruiz <dana@northwind.example>");
        assert_eq!(parsed.to[1], "Priya Shäh <priya@corp.example>");
        assert_eq!(parsed.subject, "Renewal terms ✓");
        assert_eq!(parsed.date, "2026-10-19T08:00:00Z");
        assert!(parsed.body.contains("Can we talk?"), "{}", parsed.body);
        assert!(!parsed.body.contains('<'), "{}", parsed.body);
        assert!(parsed.html.as_deref().unwrap().contains("<script>"));
    }

    #[test]
    fn au10_a_built_message_carries_the_by_line_and_reads_back() {
        let from = Sender {
            name: "Demo Person".into(),
            email: Some("demo@corp.example".into()),
        };
        let to = ["dana@northwind.example".to_owned()];
        let raw = build(&Draft {
            from: &from,
            to: &to,
            subject: "Re: Renewal — terms",
            body: "Yes.\n.Friday works.",
            attribution: "by invoice-demo for demo",
            task: "t-1",
            message_id: &new_message_id(&from),
            date: 1_792_400_000,
        });
        let text = String::from_utf8(raw.clone()).unwrap();
        assert!(text.contains("\r\nX-Glasshouse-Attribution: by invoice-demo for demo\r\n"));
        assert!(text.contains("@corp.example>\r\n"), "{text}");
        let back = parse(&raw);
        assert_eq!(back.subject, "Re: Renewal — terms");
        assert_eq!(back.from, "Demo Person <demo@corp.example>");
        assert_eq!(back.attribution.unwrap().by, "invoice-demo");
        assert_eq!(back.body.replace("\r\n", "\n"), "Yes.\n.Friday works.\n");
        assert!(back.message_id.unwrap().ends_with("@corp.example"));
    }

    #[test]
    fn the_local_stores_rfc3339_dates_still_read() {
        let parsed = parse(b"From: a@b.example\nDate: 2026-10-19T08:00:00Z\nSubject: x\n\nbody\n");
        assert_eq!(parsed.date, "2026-10-19T08:00:00Z");
        assert_eq!(parsed.when, 1_792_396_800);
    }
}
