//! A doc as Markdown, both ways (ADR 0035 §5): how agents read docs and
//! write into them. The blocks are the Docs schema's nodes; what Markdown
//! can't say (a callout's tone, an embedded artifact) has a small,
//! readable form of its own.

/// A mark on a run of text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mark {
    Strong,
    Em,
    Code,
    Strike,
    Link(String),
    /// Written by an agent or the AI: who.
    Ai(String),
}

/// A run of text with its marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub marks: Vec<Mark>,
}

pub type Inline = Vec<Span>;

/// One block of the Docs schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Inline),
    Heading {
        level: u8,
        text: Inline,
    },
    Quote(Vec<Block>),
    Bullets(Vec<Vec<Block>>),
    Ordered {
        start: u32,
        items: Vec<Vec<Block>>,
    },
    Tasks(Vec<(bool, Inline)>),
    Code {
        language: String,
        text: String,
    },
    Callout {
        tone: String,
        blocks: Vec<Block>,
    },
    Rule,
    /// The first row is the header.
    Table(Vec<Vec<Inline>>),
    Artifact {
        id: String,
        title: String,
    },
}

pub const TONES: [&str; 3] = ["info", "warn", "success"];

// ---- reading Markdown ---------------------------------------------------

/// Inline Markdown: **strong**, *em* or _em_, `code`, ~~strike~~ and
/// [text](href). Anything unclosed is text.
pub fn parse_inline(text: &str) -> Inline {
    let mut out = Vec::new();
    inline_into(text, &[], &mut out);
    merge(out)
}

fn push(out: &mut Inline, text: &str, marks: &[Mark]) {
    if !text.is_empty() {
        out.push(Span {
            text: text.to_owned(),
            marks: marks.to_vec(),
        });
    }
}

fn with(marks: &[Mark], mark: Mark) -> Vec<Mark> {
    let mut all = marks.to_vec();
    if !all.contains(&mark) {
        all.push(mark);
        all.sort();
    }
    all
}

/// Where `close` ends the run that starts `rest` after `open`: not
/// escaped, and not next to whitespace inside.
fn closing(rest: &str, open: &str, close: &str) -> Option<(String, usize)> {
    let inner = rest.strip_prefix(open)?;
    if inner.starts_with(char::is_whitespace) {
        return None;
    }
    let mut from = 0;
    while let Some(at) = inner[from..].find(close) {
        let end = from + at;
        let escaped = inner[..end].ends_with('\\');
        let spaced = inner[..end].ends_with(char::is_whitespace);
        if end > 0 && !escaped && !spaced {
            return Some((inner[..end].to_owned(), open.len() + end + close.len()));
        }
        from = end + close.len();
    }
    None
}

fn inline_into(text: &str, marks: &[Mark], out: &mut Inline) {
    let mut plain = String::new();
    let mut rest = text;
    let mut prev: Option<char> = None;
    while let Some(c) = rest.chars().next() {
        let word = prev.is_some_and(char::is_alphanumeric);
        let found = if rest.starts_with("**") {
            closing(rest, "**", "**").map(|(t, n)| (t, n, Some(Mark::Strong)))
        } else if rest.starts_with("~~") {
            closing(rest, "~~", "~~").map(|(t, n)| (t, n, Some(Mark::Strike)))
        } else if c == '`' {
            rest[1..]
                .find('`')
                .filter(|end| *end > 0)
                .map(|end| (rest[1..1 + end].to_owned(), end + 2, Some(Mark::Code)))
        } else if (c == '*' || c == '_') && !(c == '_' && word) {
            let d = &rest[..1];
            closing(rest, d, d).map(|(t, n)| (t, n, Some(Mark::Em)))
        } else if c == '[' {
            link(rest).map(|(t, href, n)| (t, n, Some(Mark::Link(href))))
        } else if c == '\\' && rest.len() > 1 {
            let next = rest[1..].chars().next().map_or(1, char::len_utf8);
            Some((rest[1..1 + next].to_owned(), 1 + next, None))
        } else {
            None
        };
        match found {
            Some((inner, used, mark)) => {
                push(out, &plain, marks);
                plain.clear();
                match mark {
                    Some(Mark::Code) => push(out, &inner, &with(marks, Mark::Code)),
                    Some(mark) => inline_into(&inner, &with(marks, mark), out),
                    None => push(out, &inner, marks),
                }
                prev = rest[..used].chars().last();
                rest = &rest[used..];
            }
            None => {
                plain.push(c);
                prev = Some(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    push(out, &plain, marks);
}

/// `[text](href)` at the start of `rest`: its text, href and length.
fn link(rest: &str) -> Option<(String, String, usize)> {
    let close = rest.find("](")?;
    let end = rest[close + 2..].find(')')?;
    let text = rest[1..close].to_owned();
    let href = rest[close + 2..close + 2 + end].trim().to_owned();
    (!text.is_empty() && !href.is_empty()).then_some((text, href, close + 3 + end))
}

/// Neighbouring spans with the same marks, joined.
fn merge(spans: Inline) -> Inline {
    let mut out: Inline = Vec::new();
    for span in spans {
        match out.last_mut() {
            Some(last) if last.marks == span.marks => last.text.push_str(&span.text),
            _ => out.push(span),
        }
    }
    out
}

fn list_item(line: &str) -> Option<(Option<u32>, &str)> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        return Some((None, rest));
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && digits < 10 {
        let rest = t[digits..].strip_prefix(". ")?;
        return Some((t[..digits].parse().ok(), rest));
    }
    None
}

fn task(text: &str) -> Option<(bool, &str)> {
    if let Some(rest) = text.strip_prefix("[ ] ") {
        Some((false, rest))
    } else {
        text.strip_prefix("[x] ")
            .or_else(|| text.strip_prefix("[X] "))
            .map(|rest| (true, rest))
    }
}

fn table_row(line: &str) -> Option<Vec<String>> {
    let t = line.trim();
    let inner = t.strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').map(|c| c.trim().to_owned()).collect())
}

fn is_rule_row(cells: &[String]) -> bool {
    cells.iter().all(|c| {
        !c.is_empty() && c.trim_matches(':').chars().all(|ch| ch == '-') && c.contains('-')
    })
}

/// Markdown blocks.
pub fn parse(markdown: &str) -> Vec<Block> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            i += 1;
            continue;
        }
        // Fenced code.
        if let Some(language) = t.strip_prefix("```") {
            let mut text = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != "```" {
                text.push(lines[i]);
                i += 1;
            }
            i += 1;
            blocks.push(Block::Code {
                language: language.trim().to_owned(),
                text: text.join("\n"),
            });
            continue;
        }
        if t == "---" || t == "***" {
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }
        if let Some(level) = (1..=3).find(|n| t.starts_with(&format!("{} ", "#".repeat(*n)))) {
            blocks.push(Block::Heading {
                level: level as u8,
                text: parse_inline(t[level + 1..].trim()),
            });
            i += 1;
            continue;
        }
        // An embedded artifact: [artifact: Title](#cv-…) alone on a line.
        if let Some(rest) = t.strip_prefix("[artifact: ")
            && let Some((title, href, used)) = link(&format!("[{rest}"))
            && used == rest.len() + 1
            && let Some(id) = href.strip_prefix('#')
            && glass_managed::chat::is_conversation_id(id)
        {
            blocks.push(Block::Artifact {
                id: id.to_owned(),
                title,
            });
            i += 1;
            continue;
        }
        if t.starts_with('>') {
            let mut inner = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('>') {
                let l = lines[i].trim().trim_start_matches('>');
                inner.push(l.strip_prefix(' ').unwrap_or(l));
                i += 1;
            }
            // A callout: its first line names its tone, as > [!warn].
            let tone = inner.first().and_then(|l| {
                let name = l.trim().strip_prefix("[!")?.strip_suffix(']')?;
                TONES.contains(&name).then(|| name.to_owned())
            });
            match tone {
                Some(tone) => blocks.push(Block::Callout {
                    tone,
                    blocks: parse(&inner[1..].join("\n")),
                }),
                None => blocks.push(Block::Quote(parse(&inner.join("\n")))),
            }
            continue;
        }
        if let Some(header) = table_row(t)
            && i + 1 < lines.len()
            && table_row(lines[i + 1]).is_some_and(|r| is_rule_row(&r))
        {
            let mut rows = vec![header.iter().map(|c| parse_inline(c)).collect::<Vec<_>>()];
            i += 2;
            while i < lines.len() {
                let Some(row) = table_row(lines[i]) else {
                    break;
                };
                rows.push(row.iter().map(|c| parse_inline(c)).collect());
                i += 1;
            }
            blocks.push(Block::Table(rows));
            continue;
        }
        if let Some((number, first)) = list_item(t) {
            let tasks = number.is_none() && task(first).is_some();
            let mut items: Vec<(Option<u32>, String)> = Vec::new();
            while i < lines.len() {
                match list_item(lines[i].trim()) {
                    Some((n, text)) if n.is_some() == number.is_some() => {
                        items.push((n, text.to_owned()));
                    }
                    // A continuation line belongs to the item before it.
                    None if !lines[i].trim().is_empty() && lines[i].starts_with("  ") => {
                        if let Some(last) = items.last_mut() {
                            last.1.push(' ');
                            last.1.push_str(lines[i].trim());
                        }
                    }
                    _ => break,
                }
                i += 1;
            }
            if tasks && items.iter().all(|(_, text)| task(text).is_some()) {
                blocks.push(Block::Tasks(
                    items
                        .iter()
                        .filter_map(|(_, text)| task(text))
                        .map(|(done, text)| (done, parse_inline(text)))
                        .collect(),
                ));
            } else {
                let content: Vec<Vec<Block>> = items
                    .iter()
                    .map(|(_, text)| vec![Block::Paragraph(parse_inline(text))])
                    .collect();
                blocks.push(match number {
                    Some(start) => Block::Ordered {
                        start,
                        items: content,
                    },
                    None => Block::Bullets(content),
                });
            }
            continue;
        }
        // A paragraph runs to a blank line or the next block.
        let mut text = vec![t];
        i += 1;
        while i < lines.len() {
            let n = lines[i].trim();
            if n.is_empty()
                || n.starts_with('#')
                || n.starts_with('>')
                || n.starts_with("```")
                || n == "---"
                || list_item(n).is_some()
                || table_row(n).is_some()
            {
                break;
            }
            text.push(n);
            i += 1;
        }
        blocks.push(Block::Paragraph(parse_inline(&text.join(" "))));
    }
    blocks
}

// ---- writing Markdown ---------------------------------------------------

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '*' | '_' | '`' | '[' | ']' | '~' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn opener(mark: &Mark) -> &'static str {
    match mark {
        Mark::Strong => "**",
        Mark::Em => "_",
        Mark::Strike => "~~",
        Mark::Link(_) => "[",
        Mark::Code | Mark::Ai(_) => "",
    }
}

fn closer(mark: &Mark) -> String {
    match mark {
        Mark::Strong => "**".into(),
        Mark::Em => "_".into(),
        Mark::Strike => "~~".into(),
        Mark::Link(href) => format!("]({href})"),
        Mark::Code | Mark::Ai(_) => String::new(),
    }
}

/// Inline Markdown, marks opened and closed across runs so they nest. An
/// AI mark has no Markdown; it shows as the text.
pub fn render_inline(spans: &Inline) -> String {
    let mut out = String::new();
    let mut open: Vec<Mark> = Vec::new();
    let close_all = |out: &mut String, open: &mut Vec<Mark>| {
        while let Some(mark) = open.pop() {
            out.push_str(&closer(&mark));
        }
    };
    for span in spans {
        if span.marks.contains(&Mark::Code) {
            close_all(&mut out, &mut open);
            out.push('`');
            out.push_str(&span.text);
            out.push('`');
            continue;
        }
        let wanted: Vec<&Mark> = span
            .marks
            .iter()
            .filter(|m| !matches!(m, Mark::Code | Mark::Ai(_)))
            .collect();
        while !open.iter().all(|m| wanted.contains(&m)) {
            if let Some(mark) = open.pop() {
                out.push_str(&closer(&mark));
            }
        }
        for mark in wanted {
            if !open.contains(mark) {
                out.push_str(opener(mark));
                open.push(mark.clone());
            }
        }
        out.push_str(&escape(&span.text));
    }
    close_all(&mut out, &mut open);
    out
}

fn indent(text: &str, first: &str, rest: &str) -> String {
    let mut out = String::new();
    for (n, line) in text.lines().enumerate() {
        out.push_str(if n == 0 { first } else { rest });
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Markdown for `blocks`, separated by blank lines.
pub fn render(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(render_block)
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_block(block: &Block) -> String {
    match block {
        Block::Paragraph(text) => format!("{}\n", render_inline(text)),
        Block::Heading { level, text } => {
            format!(
                "{} {}\n",
                "#".repeat(usize::from(*level)),
                render_inline(text)
            )
        }
        Block::Quote(blocks) => indent(&render(blocks), "> ", "> "),
        Block::Callout { tone, blocks } => {
            format!("> [!{tone}]\n{}", indent(&render(blocks), "> ", "> "))
        }
        Block::Bullets(items) => items
            .iter()
            .map(|item| indent(&render(item).replace("\n\n", "\n"), "- ", "  "))
            .collect(),
        Block::Ordered { start, items } => items
            .iter()
            .enumerate()
            .map(|(n, item)| {
                let marker = format!("{}. ", *start as usize + n);
                indent(&render(item).replace("\n\n", "\n"), &marker, "   ")
            })
            .collect(),
        Block::Tasks(items) => items
            .iter()
            .map(|(done, text)| {
                format!(
                    "- [{}] {}\n",
                    if *done { "x" } else { " " },
                    render_inline(text)
                )
            })
            .collect(),
        Block::Code { language, text } => format!("```{language}\n{text}\n```\n"),
        Block::Rule => "---\n".into(),
        Block::Table(rows) => {
            let mut out = String::new();
            for (n, row) in rows.iter().enumerate() {
                let cells: Vec<String> = row
                    .iter()
                    .map(|c| render_inline(c).replace('|', "\\|"))
                    .collect();
                out.push_str(&format!("| {} |\n", cells.join(" | ")));
                if n == 0 {
                    out.push_str(&format!("|{}\n", " --- |".repeat(row.len().max(1))));
                }
            }
            out
        }
        Block::Artifact { id, title } => format!("[artifact: {}](#{id})\n", escape(title)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        vec![Span {
            text: s.into(),
            marks: Vec::new(),
        }]
    }

    #[test]
    fn dc6_markdown_reads_every_block_the_editor_has() {
        let md = "# Q3 plan\n\nShip **Northwind** by *Friday*, see [the forecast](#cv-0123456789abcdef0123456789abcdef).\n\n\
                  - one\n- two\n\n1. first\n2. second\n\n- [x] done\n- [ ] open\n\n> quoted\n\n> [!warn]\n> Careful\n\n\
                  ```rust\nfn main() {}\n```\n\n---\n\n| Account | Q3 |\n| --- | --- |\n| Northwind | 42 |\n\n\
                  [artifact: Q3 forecast](#cv-0123456789abcdef0123456789abcdef)\n";
        let blocks = parse(md);
        assert_eq!(blocks.len(), 11, "{blocks:#?}");
        assert_eq!(
            blocks[0],
            Block::Heading {
                level: 1,
                text: text("Q3 plan")
            }
        );
        let Block::Paragraph(spans) = &blocks[1] else {
            panic!()
        };
        assert_eq!(spans[1].marks, [Mark::Strong]);
        assert_eq!(spans[3].marks, [Mark::Em]);
        assert_eq!(
            spans[5].marks,
            [Mark::Link("#cv-0123456789abcdef0123456789abcdef".into())]
        );
        assert!(matches!(&blocks[2], Block::Bullets(items) if items.len() == 2));
        assert!(matches!(&blocks[3], Block::Ordered { start: 1, items } if items.len() == 2));
        assert_eq!(
            blocks[4],
            Block::Tasks(vec![(true, text("done")), (false, text("open"))])
        );
        assert!(matches!(&blocks[5], Block::Quote(_)));
        assert!(matches!(&blocks[6], Block::Callout { tone, .. } if tone == "warn"));
        assert_eq!(
            blocks[7],
            Block::Code {
                language: "rust".into(),
                text: "fn main() {}".into()
            }
        );
        assert_eq!(blocks[8], Block::Rule);
        assert!(matches!(&blocks[9], Block::Table(rows) if rows.len() == 2 && rows[0].len() == 2));
        assert!(matches!(&blocks[10], Block::Artifact { title, .. } if title == "Q3 forecast"));
        // And back: the same blocks.
        assert_eq!(parse(&render(&blocks)), blocks);
    }

    #[test]
    fn dc6_inline_marks_nest_and_unclosed_ones_stay_text() {
        let spans = parse_inline("a **b *c* d** `d*e` 2 * 3 [x](y) \\*f");
        let rendered = render_inline(&spans);
        assert_eq!(parse_inline(&rendered), spans);
        assert!(
            spans
                .iter()
                .any(|s| s.text == "c" && s.marks == [Mark::Strong, Mark::Em])
        );
        assert!(
            spans
                .iter()
                .any(|s| s.text == "d*e" && s.marks == [Mark::Code])
        );
        assert!(spans.iter().any(|s| s.text.contains("2 * 3")));
        assert!(spans.iter().any(|s| s.text.contains("*f")));
    }
}
