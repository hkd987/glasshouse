//! Redaction (DP-3): secrets and configured PII are replaced with
//! placeholders such as `[redacted:aws-access-key#1]` before a prompt leaves
//! the machine, and PII placeholders are put back in the reply.
//!
//! Secrets are always redacted and never restored: credential material never
//! makes a round trip through a model, and a reply that mentions one keeps the
//! placeholder. PII is restored, because an agent that sent a customer's
//! address needs it back in a draft. Only placeholders this call issued are
//! restored, so a reply can't pull in anything the agent didn't send.
//!
//! The matchers are hand-written (the workspace avoids `regex`, see
//! docs/dev/build-performance.md) and err toward redacting too much.

use std::collections::BTreeMap;

use glass_policy::document::{PiiKind, Redaction};

/// Built-in secret kinds, in placeholder names.
pub const SECRET_KINDS: [&str; 5] = [
    "private-key",
    "aws-access-key",
    "api-key",
    "token",
    "credential",
];

/// Finds and replaces what must not leave, per policy.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    pii: Vec<PiiKind>,
    patterns: Vec<(String, Vec<Tok>)>,
}

/// What one call redacted, so the reply can be restored and the audit record
/// can say how much was removed.
#[derive(Debug, Default)]
pub struct Vault {
    entries: Vec<Entry>,
    counts: BTreeMap<String, usize>,
}

#[derive(Debug)]
struct Entry {
    kind: String,
    original: String,
    placeholder: String,
    restore: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
    kind: String,
    restore: bool,
}

/// One element of a configured pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Digit,
    Letter,
    Alnum,
    Lit(char),
}

impl Redactor {
    pub fn new(config: &Redaction) -> Self {
        Self {
            pii: config.pii.clone(),
            patterns: config
                .patterns
                .iter()
                .map(|p| (p.name.clone(), compile(&p.pattern)))
                .collect(),
        }
    }

    /// `text` with every secret and configured PII replaced. The same value
    /// gets the same placeholder throughout the call.
    pub fn redact(&self, text: &str, vault: &mut Vault) -> String {
        let mut spans = secrets(text);
        for kind in &self.pii {
            let found = match kind {
                PiiKind::Email => emails(text),
                PiiKind::UsSsn => ssns(text),
                PiiKind::CardNumber => cards(text),
            };
            spans.extend(found.into_iter().map(|(start, end, kind)| Span {
                start,
                end,
                kind: kind.into(),
                restore: true,
            }));
        }
        for (name, toks) in &self.patterns {
            spans.extend(custom(text, toks).into_iter().map(|(start, end)| Span {
                start,
                end,
                kind: name.clone(),
                restore: true,
            }));
        }
        // Earliest first, then longest; overlaps go to the first.
        spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for span in spans {
            if span.start < at {
                continue;
            }
            out.push_str(&text[at..span.start]);
            out.push_str(&vault.placeholder(&span, &text[span.start..span.end]));
            at = span.end;
        }
        out.push_str(&text[at..]);
        out
    }
}

impl Vault {
    fn placeholder(&mut self, span: &Span, original: &str) -> String {
        *self.counts.entry(span.kind.clone()).or_default() += 1;
        if let Some(e) = self
            .entries
            .iter()
            .find(|e| e.kind == span.kind && e.original == original)
        {
            return e.placeholder.clone();
        }
        let n = self.entries.iter().filter(|e| e.kind == span.kind).count() + 1;
        let placeholder = format!("[redacted:{}#{n}]", span.kind);
        self.entries.push(Entry {
            kind: span.kind.clone(),
            original: original.to_owned(),
            placeholder: placeholder.clone(),
            restore: span.restore,
        });
        placeholder
    }

    /// How many occurrences of each kind were replaced.
    pub fn counts(&self) -> &BTreeMap<String, usize> {
        &self.counts
    }

    /// `reply` with this call's PII placeholders put back. Secret
    /// placeholders stay.
    pub fn restore(&self, reply: &str) -> String {
        let mut text = reply.to_owned();
        for e in self.entries.iter().filter(|e| e.restore) {
            if text.contains(&e.placeholder) {
                text = text.replace(&e.placeholder, &e.original);
            }
        }
        text
    }
}

fn compile(pattern: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        toks.push(match c {
            '#' => Tok::Digit,
            '@' => Tok::Letter,
            '?' => Tok::Alnum,
            '\\' => match chars.next() {
                Some(next) => Tok::Lit(next),
                None => break,
            },
            other => Tok::Lit(other),
        });
    }
    toks
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// The end of the run of bytes from `from` that satisfy `ok`.
fn run(bytes: &[u8], from: usize, ok: impl Fn(u8) -> bool) -> usize {
    bytes[from.min(bytes.len())..]
        .iter()
        .position(|b| !ok(*b))
        .map_or(bytes.len(), |n| from + n)
}

fn secret(start: usize, end: usize, kind: &str) -> Span {
    Span {
        start,
        end,
        kind: kind.into(),
        restore: false,
    }
}

/// Every built-in secret in `text`.
fn secrets(text: &str) -> Vec<Span> {
    let mut spans = private_keys(text);
    spans.extend(aws_keys(text));
    spans.extend(prefixed_keys(text));
    spans.extend(jwts(text));
    spans.extend(bearer_tokens(text));
    spans.extend(assignments(text));
    spans
}

/// PEM private key blocks, through their END line (or the end of the text,
/// since a truncated key is still a key).
fn private_keys(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("-----BEGIN ") {
        let start = from + found;
        let line_end = text[start..].find('\n').map_or(text.len(), |n| start + n);
        if !text[start..line_end].contains("PRIVATE KEY") {
            from = line_end;
            continue;
        }
        let end = match text[line_end..].find("-----END ") {
            Some(n) => {
                let after = line_end + n + "-----END ".len();
                text[after..]
                    .find("-----")
                    .map_or(text.len(), |m| after + m + "-----".len())
            }
            None => text.len(),
        };
        spans.push(secret(start, end, "private-key"));
        from = end;
    }
    spans
}

/// AWS access key ids: a known four-letter prefix and 16 more capitals or
/// digits.
fn aws_keys(text: &str) -> Vec<Span> {
    const PREFIXES: [&[u8; 4]; 12] = [
        b"AKIA", b"ASIA", b"ABIA", b"ACCA", b"AGPA", b"AIDA", b"AIPA", b"ANPA", b"ANVA", b"AROA",
        b"APKA", b"ASCA",
    ];
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i + 20 <= bytes.len() {
        let boundary = i == 0 || !word(bytes[i - 1]);
        let keyed = boundary
            && PREFIXES.iter().any(|p| &bytes[i..i + 4] == *p)
            && bytes[i + 4..i + 20]
                .iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            && bytes.get(i + 20).is_none_or(|b| !word(*b));
        if keyed {
            spans.push(secret(i, i + 20, "aws-access-key"));
            i += 20;
        } else {
            i += 1;
        }
    }
    spans
}

/// API keys and tokens recognizable by their prefix: Anthropic and OpenAI
/// (`sk-`), GitHub, Slack, Google, Stripe.
fn prefixed_keys(text: &str) -> Vec<Span> {
    /// A prefix, how many key characters must follow it, and which.
    type Shape = (&'static str, usize, fn(u8) -> bool);
    let alnum: fn(u8) -> bool = |b| b.is_ascii_alphanumeric();
    let dashed: fn(u8) -> bool = |b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let underscored: fn(u8) -> bool = |b| b.is_ascii_alphanumeric() || b == b'_';
    let slack: fn(u8) -> bool = |b| b.is_ascii_alphanumeric() || b == b'-';
    let shapes: [Shape; 13] = [
        ("sk-", 20, dashed),
        ("ghp_", 30, alnum),
        ("gho_", 30, alnum),
        ("ghu_", 30, alnum),
        ("ghs_", 30, alnum),
        ("ghr_", 30, alnum),
        ("github_pat_", 30, underscored),
        ("xoxb-", 10, slack),
        ("xoxp-", 10, slack),
        ("xoxa-", 10, slack),
        ("AIza", 35, dashed),
        ("sk_live_", 16, alnum),
        ("rk_live_", 16, alnum),
    ];
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    for (prefix, min, ok) in shapes {
        let mut from = 0;
        while let Some(found) = text[from..].find(prefix) {
            let start = from + found;
            let tail = start + prefix.len();
            let end = run(bytes, tail, ok);
            let boundary = start == 0 || !dashed(bytes[start - 1]);
            if boundary && end - tail >= min {
                spans.push(secret(start, end, "api-key"));
            }
            from = end.max(start + 1);
        }
    }
    spans
}

/// JSON Web Tokens: three base64url parts, the first two starting `eyJ`.
fn jwts(text: &str) -> Vec<Span> {
    let b64 = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("eyJ") {
        let start = from + found;
        from = start + 3;
        if start > 0 && b64(bytes[start - 1]) {
            continue;
        }
        let one = run(bytes, start, b64);
        if one - start < 10 || bytes.get(one) != Some(&b'.') || !text[one + 1..].starts_with("eyJ")
        {
            continue;
        }
        let two = run(bytes, one + 1, b64);
        if two - one < 10 || bytes.get(two) != Some(&b'.') {
            continue;
        }
        let three = run(bytes, two + 1, b64);
        if three - two > 8 {
            spans.push(secret(start, three, "token"));
            from = three;
        }
    }
    spans
}

/// The token after `Bearer `.
fn bearer_tokens(text: &str) -> Vec<Span> {
    let token = |b: u8| b.is_ascii_alphanumeric() || b"._~+/=-".contains(&b);
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(found) = lower[from..].find("bearer ") {
        let start = run(bytes, from + found + "bearer".len(), |b| b == b' ');
        let end = run(bytes, start, token);
        if end - start >= 16 {
            spans.push(secret(start, end, "token"));
        }
        from = end.max(start + 1);
    }
    spans
}

/// Values assigned to something named like a credential:
/// `password = hunter2hunter2`, `"api_key": "…"`, `aws_secret_access_key=…`.
fn assignments(text: &str) -> Vec<Span> {
    const NAMES: [&str; 8] = [
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "apikey",
        "api-key",
        "private_key",
    ];
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.';
    let quote = |b: u8| b == b'"' || b == b'\'';
    let value = |b: u8| !b.is_ascii_whitespace() && !b"\"',;".contains(&b);
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'=' && *b != b':' {
            continue;
        }
        // The name to the left, past spaces and a closing quote.
        let mut j = i;
        while j > 0 && bytes[j - 1] == b' ' {
            j -= 1;
        }
        if j > 0 && quote(bytes[j - 1]) {
            j -= 1;
        }
        let name_end = j;
        while j > 0 && ident(bytes[j - 1]) {
            j -= 1;
        }
        let name = text[j..name_end].to_ascii_lowercase();
        if name.is_empty() || !NAMES.iter().any(|n| name.contains(n)) {
            continue;
        }
        // The value to the right, past spaces and an opening quote.
        let mut start = run(bytes, i + 1, |b| b == b' ');
        if bytes.get(start).is_some_and(|b| quote(*b)) {
            start += 1;
        }
        if text[start..].starts_with("[redacted:") {
            continue;
        }
        let end = run(bytes, start, value);
        if end - start >= 8 {
            spans.push(secret(start, end, "credential"));
        }
    }
    spans
}

/// Email addresses.
fn emails(text: &str) -> Vec<(usize, usize, &'static str)> {
    let local = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
    let domain = |b: u8| b.is_ascii_alphanumeric() || b == b'.' || b == b'-';
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for (at, _) in text.match_indices('@') {
        let mut start = at;
        while start > 0 && local(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = run(bytes, at + 1, domain);
        while end > at + 1 && bytes[end - 1] == b'.' {
            end -= 1;
        }
        let host = &text[at + 1..end];
        let tld_ok = host.rsplit_once('.').is_some_and(|(name, tld)| {
            !name.is_empty() && tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic())
        });
        if start < at && tld_ok {
            found.push((start, end, "email"));
        }
    }
    found
}

/// US Social Security numbers written `ddd-dd-dddd`, minus numbers the SSA
/// never issues.
fn ssns(text: &str) -> Vec<(usize, usize, &'static str)> {
    let bytes = text.as_bytes();
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    let mut found = Vec::new();
    let mut i = 0;
    while i + 11 <= bytes.len() {
        let s = &bytes[i..i + 11];
        let shaped = digits(&s[0..3])
            && s[3] == b'-'
            && digits(&s[4..6])
            && s[6] == b'-'
            && digits(&s[7..11]);
        let bounded = (i == 0 || !(word(bytes[i - 1]) || bytes[i - 1] == b'-'))
            && bytes.get(i + 11).is_none_or(|b| !(word(*b) || *b == b'-'));
        let issued = shaped
            && &s[0..3] != b"000"
            && &s[0..3] != b"666"
            && s[0] != b'9'
            && &s[4..6] != b"00"
            && &s[7..11] != b"0000";
        if bounded && issued {
            found.push((i, i + 11, "us-ssn"));
            i += 11;
        } else {
            i += 1;
        }
    }
    found
}

/// Payment card numbers: 13 to 19 digits, optionally grouped with single
/// spaces or hyphens, that pass the Luhn check.
fn cards(text: &str) -> Vec<(usize, usize, &'static str)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let starts = bytes[i].is_ascii_digit() && (i == 0 || !word(bytes[i - 1]));
        if !starts {
            i += 1;
            continue;
        }
        let mut digits = Vec::new();
        let mut j = i;
        let mut end = i;
        while j < bytes.len() {
            if bytes[j].is_ascii_digit() {
                digits.push(bytes[j] - b'0');
                j += 1;
                end = j;
            } else if (bytes[j] == b' ' || bytes[j] == b'-')
                && bytes.get(j + 1).is_some_and(u8::is_ascii_digit)
                && bytes[j - 1].is_ascii_digit()
            {
                j += 1;
            } else {
                break;
            }
        }
        let bounded = bytes.get(end).is_none_or(|b| !word(*b));
        if bounded && (13..=19).contains(&digits.len()) && luhn(&digits) {
            found.push((i, end, "card-number"));
        }
        i = end.max(i + 1);
    }
    found
}

fn luhn(digits: &[u8]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let d = u32::from(*d);
            if i % 2 == 1 {
                let doubled = d * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// Matches of a configured pattern that don't run on into letters or
/// digits.
fn custom(text: &str, toks: &[Tok]) -> Vec<(usize, usize)> {
    if toks.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte_at = |k: usize| chars.get(k).map_or(text.len(), |(b, _)| *b);
    let mut found = Vec::new();
    let mut k = 0;
    while k + toks.len() <= chars.len() {
        let bounded_before = k == 0 || !chars[k - 1].1.is_alphanumeric();
        let matches = bounded_before
            && toks.iter().zip(&chars[k..]).all(|(t, (_, c))| match t {
                Tok::Digit => c.is_ascii_digit(),
                Tok::Letter => c.is_alphabetic(),
                Tok::Alnum => c.is_alphanumeric(),
                Tok::Lit(l) => c == l,
            });
        let after = k + toks.len();
        if matches && chars.get(after).is_none_or(|(_, c)| !c.is_alphanumeric()) {
            found.push((byte_at(k), byte_at(after)));
            k = after;
        } else {
            k += 1;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use glass_policy::document::RedactPattern;

    use super::*;

    // AWS's documentation example credentials, not real ones.
    const AWS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const AWS_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    fn all() -> Redactor {
        Redactor::new(&Redaction {
            pii: vec![PiiKind::Email, PiiKind::UsSsn, PiiKind::CardNumber],
            patterns: vec![RedactPattern {
                name: "employee-id".into(),
                pattern: "EMP-######".into(),
            }],
        })
    }

    fn redact(text: &str) -> (String, Vault) {
        let mut vault = Vault::default();
        let out = all().redact(text, &mut vault);
        (out, vault)
    }

    #[test]
    fn dp3_secrets_never_leave() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmU=\n-----END OPENSSH PRIVATE KEY-----";
        let text = format!(
            "key {AWS_KEY} and\naws_secret_access_key = {AWS_SECRET}\n{pem}\n\
             Authorization: Bearer abcdefghijklmnop.qrstuvwx\n\
             export ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123\n\
             gh: ghp_abcdefghijklmnopqrstuvwxyz0123456789\n\
             jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U\n\
             \"password\": \"correct-horse-battery\""
        );
        let (out, vault) = redact(&text);
        for secret in [
            AWS_KEY,
            AWS_SECRET,
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmU",
            "abcdefghijklmnop.qrstuvwx",
            "sk-ant-api03",
            "ghp_",
            "dozjgNryP4J3jVmNHl0w5N",
            "correct-horse-battery",
        ] {
            assert!(!out.contains(secret), "{secret} leaked in:\n{out}");
        }
        assert!(out.contains("key [redacted:aws-access-key#1] and"), "{out}");
        assert!(out.contains("[redacted:private-key#1]"), "{out}");
        assert!(out.contains("Bearer [redacted:token#1]"), "{out}");
        let counts = vault.counts();
        assert_eq!(counts["aws-access-key"], 1);
        assert_eq!(counts["api-key"], 2, "{counts:?}");
        assert_eq!(counts["credential"], 2, "{counts:?}");
    }

    #[test]
    fn dp3_pii_is_replaced_and_restored_in_the_reply() {
        let text = "Write to ana.diaz@example.com about card 4111 1111 1111 1111, SSN 123-45-6789, \
                    employee EMP-004211. Again: ana.diaz@example.com";
        let (out, vault) = redact(text);
        assert_eq!(
            out,
            "Write to [redacted:email#1] about card [redacted:card-number#1], SSN [redacted:us-ssn#1], \
             employee [redacted:employee-id#1]. Again: [redacted:email#1]"
        );
        assert_eq!(vault.counts()["email"], 2);
        let reply = "Dear [redacted:email#1], we charged [redacted:card-number#1]. [redacted:email#2] is unknown.";
        assert_eq!(
            vault.restore(reply),
            "Dear ana.diaz@example.com, we charged 4111 1111 1111 1111. [redacted:email#2] is unknown.",
            "only placeholders this call issued come back"
        );
    }

    #[test]
    fn dp3_secret_placeholders_stay_in_the_reply() {
        let (out, vault) = redact(&format!("rotate {AWS_KEY} for bo@example.com"));
        assert_eq!(
            out,
            "rotate [redacted:aws-access-key#1] for [redacted:email#1]"
        );
        assert_eq!(
            vault.restore("Rotate [redacted:aws-access-key#1]; tell [redacted:email#1]."),
            "Rotate [redacted:aws-access-key#1]; tell bo@example.com."
        );
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        let text = "Invoice 2026-10-17 for $1,250.00, PO 4111-22, phone 555-123-4567, \
                    token count: 42, a@b, user@localhost, ASIA travel, sk-short, \
                    secret = short, see https://example.com/path.";
        let (out, vault) = redact(text);
        assert_eq!(out, text, "{:?}", vault.counts());
        assert!(vault.counts().is_empty());
    }

    #[test]
    fn pii_detectors_are_off_unless_policy_turns_them_on() {
        let mut vault = Vault::default();
        let out = Redactor::new(&Redaction::default())
            .redact("mail ana@example.com, key AKIAIOSFODNN7EXAMPLE", &mut vault);
        assert_eq!(out, "mail ana@example.com, key [redacted:aws-access-key#1]");
    }

    #[test]
    fn card_numbers_must_pass_luhn_and_patterns_must_stand_alone() {
        let (out, _) = redact("4111 1111 1111 1112 and EMP-0042111 and XEMP-004211 and EMP-004211");
        assert_eq!(
            out,
            "4111 1111 1111 1112 and EMP-0042111 and XEMP-004211 and [redacted:employee-id#1]"
        );
        let (out, _) = redact("ünïcode EMP-123456 ✓ ana@example.com");
        assert_eq!(out, "ünïcode [redacted:employee-id#1] ✓ [redacted:email#1]");
    }

    #[test]
    fn a_truncated_private_key_is_still_redacted() {
        let (out, _) = redact("here: -----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA");
        assert_eq!(out, "here: [redacted:private-key#1]");
        let (cert, _) = redact("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----");
        assert!(cert.contains("MIIB"), "certificates are public");
    }
}
