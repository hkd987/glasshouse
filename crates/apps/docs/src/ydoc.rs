//! The Docs schema in a Yjs document (ADR 0035 §1): the ProseMirror
//! document y-prosemirror keeps in the `prosemirror` XML fragment, read
//! into [`Block`]s and written from them. Elements are named after nodes
//! with their attributes; text carries marks as formatting attributes,
//! each mark's attributes as a map.

use std::collections::HashMap;
use std::sync::Arc;

use yrs::types::Attrs;
use yrs::types::text::YChange;
use yrs::updates::decoder::Decode;
use yrs::{
    Any, Doc, GetString, Number, Out, ReadTxn, StateVector, Text, Transact, TransactionMut, Update,
    Xml, XmlElementPrelim, XmlElementRef, XmlFragment, XmlOut, XmlTextPrelim,
};

use crate::markdown::{Block, Inline, MAX_DEPTH, Mark, Span, TONES};

/// The fragment y-prosemirror binds.
pub const FRAGMENT: &str = "prosemirror";

/// A document from Yjs updates, as they were posted.
pub fn doc_from(updates: &[Vec<u8>]) -> Result<Doc, String> {
    let doc = Doc::new();
    apply(&doc, updates)?;
    Ok(doc)
}

/// Applies Yjs v1 updates to `doc`.
pub fn apply(doc: &Doc, updates: &[Vec<u8>]) -> Result<(), String> {
    let mut txn = doc.transact_mut();
    for bytes in updates {
        let update =
            Update::decode_v1(bytes).map_err(|e| format!("an update doesn't decode: {e}"))?;
        txn.apply_update(update)
            .map_err(|e| format!("an update doesn't apply: {e}"))?;
    }
    Ok(())
}

/// Whether `bytes` is a Yjs v1 update.
pub fn is_update(bytes: &[u8]) -> bool {
    Update::decode_v1(bytes).is_ok()
}

/// The whole document as one update.
pub fn state(doc: &Doc) -> Vec<u8> {
    doc.transact()
        .encode_state_as_update_v1(&StateVector::default())
}

// ---- reading -------------------------------------------------------------

fn text_attr<T: ReadTxn>(el: &XmlElementRef, txn: &T, name: &str) -> Option<String> {
    match el.get_attribute(txn, name)? {
        Out::Any(Any::String(s)) => Some(s.to_string()),
        Out::Any(Any::Number(Number::Int(n))) => Some(n.to_string()),
        Out::Any(Any::Number(Number::Float(n))) => Some(n.to_string()),
        Out::Any(Any::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn number_attr<T: ReadTxn>(el: &XmlElementRef, txn: &T, name: &str) -> Option<i64> {
    match el.get_attribute(txn, name)? {
        Out::Any(Any::Number(Number::Int(n))) => Some(n),
        Out::Any(Any::Number(Number::Float(n))) => Some(n as i64),
        Out::Any(Any::String(s)) => s.parse().ok(),
        _ => None,
    }
}

fn mark_of(name: &str, value: &Any) -> Option<Mark> {
    let field = |key: &str| match value {
        Any::Map(map) => match map.get(key) {
            Some(Any::String(s)) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    };
    Some(match name {
        "strong" => Mark::Strong,
        "em" => Mark::Em,
        "code" => Mark::Code,
        "strike" => Mark::Strike,
        "link" => Mark::Link(field("href")?),
        "ai" => Mark::Ai(field("by").unwrap_or_default()),
        _ => return None,
    })
}

/// An element's text, with its marks; a hard break is a space.
fn inline<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Inline {
    inline_at(txn, el, 0)
}

/// [`inline`], `depth` elements down. The schema nests none in a run of
/// text, but another computer's update can nest any number: past
/// [`MAX_DEPTH`] they are left out, since each is a call deep.
fn inline_at<T: ReadTxn>(txn: &T, el: &XmlElementRef, depth: usize) -> Inline {
    let mut spans = Vec::new();
    for child in el.children(txn) {
        match child {
            XmlOut::Text(text) => {
                for diff in text.diff(txn, YChange::identity) {
                    let Out::Any(Any::String(s)) = diff.insert else {
                        continue;
                    };
                    let mut marks: Vec<Mark> = diff
                        .attributes
                        .iter()
                        .flat_map(|a| a.iter())
                        .filter_map(|(k, v)| mark_of(k, v))
                        .collect();
                    marks.sort();
                    spans.push(Span {
                        text: s.to_string(),
                        marks,
                    });
                }
            }
            XmlOut::Element(e) if e.tag().as_ref() == "hard_break" => spans.push(Span {
                text: " ".into(),
                marks: Vec::new(),
            }),
            XmlOut::Element(e) if depth < MAX_DEPTH => {
                spans.extend(inline_at(txn, &e, depth + 1));
            }
            XmlOut::Element(_) => {}
            XmlOut::Fragment(_) => {}
        }
    }
    let mut merged: Inline = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(last) if last.marks == span.marks => last.text.push_str(&span.text),
            _ => merged.push(span),
        }
    }
    merged
}

fn elements<T: ReadTxn>(txn: &T, parent: &impl XmlFragment) -> Vec<XmlElementRef> {
    parent
        .children(txn)
        .filter_map(|c| match c {
            XmlOut::Element(e) => Some(e),
            _ => None,
        })
        .collect()
}

/// The blocks in `parent`, itself `depth` blocks down. A doc is what every
/// member's computer posted, so its nesting is theirs to choose: blocks
/// past [`MAX_DEPTH`] are left out instead of read a call deeper each.
fn children<T: ReadTxn>(txn: &T, parent: &impl XmlFragment, depth: usize) -> Vec<Block> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }
    elements(txn, parent)
        .iter()
        .filter_map(|e| block(txn, e, depth))
        .collect()
}

/// A list item's or a cell's text: its first paragraph's, or its own.
fn first_text<T: ReadTxn>(txn: &T, el: &XmlElementRef) -> Inline {
    elements(txn, el)
        .into_iter()
        .find(|e| e.tag().as_ref() == "paragraph")
        .map_or_else(|| inline(txn, el), |p| inline(txn, &p))
}

fn block<T: ReadTxn>(txn: &T, el: &XmlElementRef, depth: usize) -> Option<Block> {
    Some(match el.tag().as_ref() {
        "paragraph" => Block::Paragraph(inline(txn, el)),
        "heading" => Block::Heading {
            level: number_attr(el, txn, "level").unwrap_or(1).clamp(1, 3) as u8,
            text: inline(txn, el),
        },
        "blockquote" => Block::Quote(children(txn, el, depth + 1)),
        "bullet_list" => Block::Bullets(
            elements(txn, el)
                .iter()
                .map(|item| children(txn, item, depth + 1))
                .collect(),
        ),
        "ordered_list" => Block::Ordered {
            start: number_attr(el, txn, "order").unwrap_or(1).max(1) as u32,
            items: elements(txn, el)
                .iter()
                .map(|item| children(txn, item, depth + 1))
                .collect(),
        },
        "task_list" => Block::Tasks(
            elements(txn, el)
                .iter()
                .map(|item| {
                    let checked = text_attr(item, txn, "checked").is_some_and(|c| c == "true");
                    (checked, first_text(txn, item))
                })
                .collect(),
        ),
        "code_block" => Block::Code {
            language: text_attr(el, txn, "language").unwrap_or_default(),
            text: el
                .children(txn)
                .filter_map(|c| match c {
                    XmlOut::Text(t) => Some(t.get_string(txn)),
                    _ => None,
                })
                .collect(),
        },
        "callout" => Block::Callout {
            tone: text_attr(el, txn, "tone")
                .filter(|t| TONES.contains(&t.as_str()))
                .unwrap_or_else(|| "info".into()),
            blocks: children(txn, el, depth + 1),
        },
        "horizontal_rule" => Block::Rule,
        "table" => Block::Table(
            elements(txn, el)
                .iter()
                .map(|row| {
                    elements(txn, row)
                        .iter()
                        .map(|cell| first_text(txn, cell))
                        .collect()
                })
                .collect(),
        ),
        "artifact" => Block::Artifact {
            id: text_attr(el, txn, "id")?,
            title: text_attr(el, txn, "title").unwrap_or_default(),
        },
        _ => return None,
    })
}

/// The document's blocks.
pub fn blocks(doc: &Doc) -> Vec<Block> {
    let fragment = doc.get_or_insert_xml_fragment(FRAGMENT);
    let txn = doc.transact();
    children(&txn, &fragment, 0)
}

// ---- writing -------------------------------------------------------------

fn mark_attrs(marks: &[Mark], ai: Option<&str>) -> Attrs {
    let map = |pairs: &[(&str, &str)]| {
        Any::Map(Arc::new(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), Any::String((*v).into())))
                .collect::<HashMap<_, _>>(),
        ))
    };
    let mut attrs = Attrs::new();
    for mark in marks {
        let (name, value) = match mark {
            Mark::Strong => ("strong", map(&[])),
            Mark::Em => ("em", map(&[])),
            Mark::Code => ("code", map(&[])),
            Mark::Strike => ("strike", map(&[])),
            Mark::Link(href) => ("link", map(&[("href", href)])),
            Mark::Ai(by) => ("ai", map(&[("by", by)])),
        };
        attrs.insert(name.into(), value);
    }
    if let Some(by) = ai {
        attrs.insert("ai".into(), map(&[("by", by)]));
    }
    attrs
}

fn element(
    txn: &mut TransactionMut<'_>,
    parent: &impl XmlFragment,
    index: u32,
    tag: &str,
    attrs: &[(&str, Any)],
) -> XmlElementRef {
    let el = parent.insert(txn, index, XmlElementPrelim::empty(tag));
    for (key, value) in attrs {
        el.insert_attribute(txn, *key, value.clone());
    }
    el
}

fn write_inline(
    txn: &mut TransactionMut<'_>,
    el: &XmlElementRef,
    spans: &Inline,
    ai: Option<&str>,
) {
    let text = el.push_back(txn, XmlTextPrelim::new(""));
    let mut at = 0;
    for span in spans {
        text.insert_with_attributes(txn, at, &span.text, mark_attrs(&span.marks, ai));
        at += span.text.encode_utf16().count() as u32;
    }
}

fn paragraph(
    txn: &mut TransactionMut<'_>,
    parent: &impl XmlFragment,
    index: u32,
    text: &Inline,
    ai: Option<&str>,
) {
    let p = element(txn, parent, index, "paragraph", &[]);
    write_inline(txn, &p, text, ai);
}

fn write_items(
    txn: &mut TransactionMut<'_>,
    list: &XmlElementRef,
    items: &[Vec<Block>],
    ai: Option<&str>,
) {
    for (n, item) in items.iter().enumerate() {
        let li = element(txn, list, n as u32, "list_item", &[]);
        for (m, b) in item.iter().enumerate() {
            write_block(txn, &li, m as u32, b, ai);
        }
        if item.is_empty() {
            paragraph(txn, &li, 0, &Vec::new(), ai);
        }
    }
}

/// Writes `block` as child `index` of `parent`, its text marked as
/// written by `ai` when given.
pub fn write_block(
    txn: &mut TransactionMut<'_>,
    parent: &impl XmlFragment,
    index: u32,
    block: &Block,
    ai: Option<&str>,
) {
    let int = |n: i64| Any::Number(Number::Int(n));
    match block {
        Block::Paragraph(text) => paragraph(txn, parent, index, text, ai),
        Block::Heading { level, text } => {
            let h = element(
                txn,
                parent,
                index,
                "heading",
                &[("level", int(i64::from(*level)))],
            );
            write_inline(txn, &h, text, ai);
        }
        Block::Quote(blocks) => {
            let q = element(txn, parent, index, "blockquote", &[]);
            for (n, b) in blocks.iter().enumerate() {
                write_block(txn, &q, n as u32, b, ai);
            }
        }
        Block::Callout { tone, blocks } => {
            let c = element(
                txn,
                parent,
                index,
                "callout",
                &[("tone", Any::String(tone.as_str().into()))],
            );
            for (n, b) in blocks.iter().enumerate() {
                write_block(txn, &c, n as u32, b, ai);
            }
        }
        Block::Bullets(items) => {
            let list = element(txn, parent, index, "bullet_list", &[]);
            write_items(txn, &list, items, ai);
        }
        Block::Ordered { start, items } => {
            let list = element(
                txn,
                parent,
                index,
                "ordered_list",
                &[("order", int(i64::from(*start)))],
            );
            write_items(txn, &list, items, ai);
        }
        Block::Tasks(items) => {
            let list = element(txn, parent, index, "task_list", &[]);
            for (n, (done, text)) in items.iter().enumerate() {
                let item = element(
                    txn,
                    &list,
                    n as u32,
                    "task_item",
                    &[("checked", Any::Bool(*done))],
                );
                paragraph(txn, &item, 0, text, ai);
            }
        }
        Block::Code { language, text } => {
            let code = element(
                txn,
                parent,
                index,
                "code_block",
                &[("language", Any::String(language.as_str().into()))],
            );
            let t = code.push_back(txn, XmlTextPrelim::new(""));
            t.insert(txn, 0, text);
        }
        Block::Rule => {
            element(txn, parent, index, "horizontal_rule", &[]);
        }
        Block::Table(rows) => {
            let table = element(txn, parent, index, "table", &[]);
            for (r, row) in rows.iter().enumerate() {
                let tr = element(txn, &table, r as u32, "table_row", &[]);
                for (c, cell) in row.iter().enumerate() {
                    let tag = if r == 0 { "table_header" } else { "table_cell" };
                    let td = element(txn, &tr, c as u32, tag, &[]);
                    paragraph(txn, &td, 0, cell, ai);
                }
            }
        }
        Block::Artifact { id, title } => {
            element(
                txn,
                parent,
                index,
                "artifact",
                &[
                    ("id", Any::String(id.as_str().into())),
                    ("title", Any::String(title.as_str().into())),
                ],
            );
        }
    }
}

/// Where an edit goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    /// After everything.
    End,
    /// In place of everything.
    Replace,
    /// After the section a heading starts: before the next heading of the
    /// same or a higher level.
    AfterSection(String),
}

/// Writes `blocks` into `doc` at `at`, marked as `ai`'s: the update it
/// made, or why it couldn't.
pub fn write(doc: &Doc, blocks: &[Block], at: &At, ai: &str) -> Result<Vec<u8>, String> {
    let fragment = doc.get_or_insert_xml_fragment(FRAGMENT);
    let before = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let existing = elements(&txn, &fragment);
        let index = match at {
            At::End => existing.len() as u32,
            At::Replace => {
                let len = fragment.len(&txn);
                if len > 0 {
                    fragment.remove_range(&mut txn, 0, len);
                }
                0
            }
            At::AfterSection(heading) => {
                let wanted = heading.trim().to_lowercase();
                let level = |e: &XmlElementRef, txn: &TransactionMut<'_>| {
                    (e.tag().as_ref() == "heading")
                        .then(|| number_attr(e, txn, "level").unwrap_or(1))
                };
                let start = existing.iter().position(|e| {
                    level(e, &txn).is_some()
                        && crate::markdown::render_inline(&inline(&txn, e))
                            .trim()
                            .to_lowercase()
                            == wanted
                });
                let Some(start) = start else {
                    return Err(format!("the doc has no heading \"{heading}\""));
                };
                let own = level(&existing[start], &txn).unwrap_or(1);
                let end = existing[start + 1..]
                    .iter()
                    .position(|e| level(e, &txn).is_some_and(|l| l <= own))
                    .map_or(existing.len(), |n| start + 1 + n);
                end as u32
            }
        };
        for (n, block) in blocks.iter().enumerate() {
            write_block(&mut txn, &fragment, index + n as u32, block, Some(ai));
        }
    }
    Ok(doc.transact().encode_state_as_update_v1(&before))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{parse, render};

    const MD: &str = "# Q3 plan\n\nShip **Northwind** by *Friday*.\n\n- one\n- two\n\n- [x] done\n- [ ] open\n\n\
                      > [!info]\n> Heads up\n\n| Account | Q3 |\n| --- | --- |\n| Northwind | 42 |\n\n\
                      ## Risks\n\nNone yet.\n\n## Next\n\nTBD\n";

    #[test]
    fn dc6_markdown_written_into_a_doc_reads_back_the_same_from_another_computer() {
        let a = Doc::new();
        let update = write(&a, &parse(MD), &At::End, "claude-code for dana").unwrap();
        // Another computer applies the update it posted.
        let b = doc_from(&[update]).unwrap();
        let read = blocks(&b);
        let plain: Vec<Block> = parse(MD);
        // The text is the same; the agent's marks are on it.
        assert_eq!(render(&read), render(&plain));
        let Block::Paragraph(spans) = &read[1] else {
            panic!("{read:?}")
        };
        assert!(
            spans
                .iter()
                .all(|s| s.marks.contains(&Mark::Ai("claude-code for dana".into())))
        );
    }

    #[test]
    fn dc6_an_edit_lands_after_the_section_it_names() {
        let doc = doc_from(&[]).unwrap();
        write(&doc, &parse(MD), &At::End, "a").unwrap();
        write(
            &doc,
            &parse("- Supplier delay"),
            &At::AfterSection("Risks".into()),
            "b",
        )
        .unwrap();
        let md = render(&blocks(&doc));
        let risks = md.find("## Risks").unwrap();
        let delay = md.find("Supplier delay").unwrap();
        let next = md.find("## Next").unwrap();
        assert!(risks < delay && delay < next, "{md}");
        assert!(write(&doc, &parse("x"), &At::AfterSection("Nope".into()), "b").is_err());
        // Replace leaves only what it wrote.
        write(&doc, &parse("# Fresh"), &At::Replace, "c").unwrap();
        assert_eq!(render(&blocks(&doc)), "# Fresh\n");
    }

    #[test]
    fn dc3_concurrent_edits_from_two_computers_merge() {
        let base = Doc::new();
        let first = write(&base, &parse("# Plan\n\nIntro"), &At::End, "a").unwrap();
        let a = doc_from(std::slice::from_ref(&first)).unwrap();
        let b = doc_from(&[first]).unwrap();
        let ua = write(&a, &parse("From A"), &At::End, "a").unwrap();
        let ub = write(&b, &parse("From B"), &At::End, "b").unwrap();
        apply(&a, std::slice::from_ref(&ub)).unwrap();
        apply(&b, std::slice::from_ref(&ua)).unwrap();
        assert_eq!(render(&blocks(&a)), render(&blocks(&b)));
        let md = render(&blocks(&a));
        assert!(md.contains("From A") && md.contains("From B"), "{md}");
        // Applying an update twice changes nothing.
        apply(&a, &[ua]).unwrap();
        assert_eq!(render(&blocks(&a)), render(&blocks(&b)));
        assert!(!is_update(b"not yjs"));
    }

    #[test]
    fn dc6_a_doc_nested_past_the_limit_reads_without_its_deepest_blocks() {
        // What another member's computer can post: quotes nested far past
        // anything the editor makes. Reading it was a call per level.
        let doc = Doc::new();
        let fragment = doc.get_or_insert_xml_fragment(FRAGMENT);
        {
            let mut txn = doc.transact_mut();
            let mut parent = element(&mut txn, &fragment, 0, "blockquote", &[]);
            for _ in 0..2_000 {
                parent = element(&mut txn, &parent, 0, "blockquote", &[]);
            }
            paragraph(&mut txn, &parent, 0, &parse_text("bottom"), None);
            paragraph(&mut txn, &fragment, 1, &parse_text("top"), None);
        }
        // Read on another computer, from the update.
        let read = blocks(&doc_from(&[state(&doc)]).unwrap());
        let mut deep = 0;
        let mut at = &read;
        while let Some(Block::Quote(inner)) = at.first() {
            deep += 1;
            at = inner;
        }
        assert_eq!(deep, MAX_DEPTH);
        let md = render(&read);
        assert!(md.contains("top") && !md.contains("bottom"), "{md}");
    }

    fn parse_text(text: &str) -> Inline {
        crate::markdown::parse_inline(text)
    }
}
