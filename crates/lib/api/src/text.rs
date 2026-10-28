//! Display text: what a view shows that an agent, an action argument or a
//! file name could have written, as one line of plain text.
//!
//! Agents write much of what people read: the progress caption, the files
//! they name, the subjects of the mail they draft, the last line they log
//! before failing. Shown as it came, a control character in that text acts
//! on the reader's terminal (an escape sequence clears the screen, moves the
//! cursor or makes a link; a carriage return hides the start of a line
//! behind its end) and a direction override reorders what any screen shows,
//! so an agent could hide what it did or forge an approval. glassd passes
//! every such string through [`display_line`] where it builds a view
//! ([`DisplaySafe`]), and clients pass what they print through it again, so
//! an older or compromised glassd can't drive them either.
//!
//! The rule: a control that breaks or spaces a line (tab, line feed, vertical
//! tab, form feed, carriage return, next line, line and paragraph separator)
//! becomes a space, so the words on either side stay readable; any other
//! control character (C0, DEL, C1) and any [`invisible`] character becomes
//! U+FFFD, the replacement character, so tampering shows rather than
//! vanishes. Each character becomes exactly one, so length limits in
//! characters still hold. Everything else, emoji and accents included, is
//! left as it is.

use std::borrow::Cow;

use serde_json::Value;

/// What shows in place of a character that would act on the display rather
/// than be displayed.
pub const STAND_IN: char = '\u{FFFD}';

/// `text` as one line that is safe to show: unchanged (and not copied) when
/// nothing in it needs replacing, which is nearly always.
pub fn display_line(text: &str) -> Cow<'_, str> {
    match text.char_indices().find(|&(_, c)| display_char(c) != c) {
        None => Cow::Borrowed(text),
        Some((at, _)) => {
            let mut line = String::with_capacity(text.len() + 2);
            line.push_str(&text[..at]);
            line.extend(text[at..].chars().map(display_char));
            Cow::Owned(line)
        }
    }
}

/// What a display line shows for `c`: a space, [`STAND_IN`], or `c` itself.
pub fn display_char(c: char) -> char {
    match c {
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' => ' ',
        c if c.is_control() || invisible(c) => STAND_IN,
        c => c,
    }
}

/// Characters that change how the text around them shows without showing
/// themselves: every bidirectional control (marks, embeddings, overrides,
/// isolates), zero-width spaces and the byte-order mark, and the line and
/// paragraph separators. The zero-width joiner and non-joiner (U+200D,
/// U+200C) are not among them: emoji sequences and several scripts need
/// them, and they can't reorder or hide text.
pub fn invisible(c: char) -> bool {
    matches!(c,
        '\u{061C}' | '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{2028}' | '\u{2029}' | '\u{2060}'
        | '\u{FEFF}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A view whose text can be made safe to show. Each impl names every field
/// of its type, so a new field doesn't compile until someone decides whether
/// it is display text.
pub trait DisplaySafe {
    /// Passes every string in `self` that could have come from an agent, an
    /// action argument or a file name through [`display_line`].
    fn make_display_safe(&mut self);
}

impl DisplaySafe for String {
    fn make_display_safe(&mut self) {
        if let Cow::Owned(line) = display_line(self) {
            *self = line;
        }
    }
}

impl<T: DisplaySafe> DisplaySafe for Vec<T> {
    fn make_display_safe(&mut self) {
        self.iter_mut().for_each(T::make_display_safe);
    }
}

impl<T: DisplaySafe> DisplaySafe for Option<T> {
    fn make_display_safe(&mut self) {
        if let Some(inner) = self {
            inner.make_display_safe();
        }
    }
}

/// Every string in the value, object keys included: for a client printing
/// whatever a method returned.
impl DisplaySafe for Value {
    fn make_display_safe(&mut self) {
        match self {
            Value::String(text) => text.make_display_safe(),
            Value::Array(items) => items.make_display_safe(),
            Value::Object(map) => {
                if map
                    .keys()
                    .any(|key| matches!(display_line(key), Cow::Owned(_)))
                {
                    *map = std::mem::take(map)
                        .into_iter()
                        .map(|(mut key, value)| {
                            key.make_display_safe();
                            (key, value)
                        })
                        .collect();
                }
                map.values_mut().for_each(Value::make_display_safe);
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }
}

mod views {
    use super::DisplaySafe;
    use crate::{
        ActivityEntry, ApprovalView, AskLabels, ChangeEntry, FeedLine, Receipt, SnapshotMarker,
        TaskDetail, TaskView, UndoReport,
    };

    impl DisplaySafe for TaskView {
        fn make_display_safe(&mut self) {
            // Ids, the agent id and the driver socket are glassd's own and
            // validated; the rest an agent or a file name can reach.
            let TaskView {
                id: _,
                agent: _,
                agent_name,
                user,
                mode: _,
                state: _,
                title,
                caption,
                progress: _,
                feed,
                needs,
                receipt,
                failure,
                started_at: _,
                updated_at: _,
                driver_socket: _,
                continues: _,
                computer: _,
            } = self;
            for text in [agent_name, user, title, caption] {
                text.make_display_safe();
            }
            feed.make_display_safe();
            needs.make_display_safe();
            receipt.make_display_safe();
            failure.make_display_safe();
        }
    }

    impl DisplaySafe for FeedLine {
        fn make_display_safe(&mut self) {
            let FeedLine {
                at: _,
                text,
                verb,
                target,
            } = self;
            for text in [text, verb, target] {
                text.make_display_safe();
            }
        }
    }

    impl DisplaySafe for Receipt {
        fn make_display_safe(&mut self) {
            let Receipt {
                actions: _,
                changes: _,
                undoable: _,
                left_machine: _,
                models,
                flagged,
            } = self;
            models.make_display_safe();
            flagged.make_display_safe();
        }
    }

    impl DisplaySafe for TaskDetail {
        fn make_display_safe(&mut self) {
            let TaskDetail {
                view,
                prompt,
                plan,
                now,
                activity,
                changes,
                cant_touch,
                snapshots,
                answer,
            } = self;
            if let Some(text) = answer {
                *text = super::display_paragraphs(text);
            }
            view.make_display_safe();
            prompt.make_display_safe();
            plan.make_display_safe();
            now.make_display_safe();
            activity.make_display_safe();
            changes.make_display_safe();
            cant_touch.make_display_safe();
            snapshots.make_display_safe();
        }
    }

    impl DisplaySafe for ActivityEntry {
        fn make_display_safe(&mut self) {
            let ActivityEntry {
                at: _,
                kind: _,
                text,
            } = self;
            text.make_display_safe();
        }
    }

    impl DisplaySafe for ChangeEntry {
        fn make_display_safe(&mut self) {
            let ChangeEntry {
                id: _,
                at: _,
                kind: _,
                target,
                action: _,
                reversible: _,
                paths,
                undone: _,
            } = self;
            target.make_display_safe();
            paths.make_display_safe();
        }
    }

    impl DisplaySafe for SnapshotMarker {
        fn make_display_safe(&mut self) {
            let SnapshotMarker {
                id: _,
                at: _,
                reason,
            } = self;
            reason.make_display_safe();
        }
    }

    impl DisplaySafe for UndoReport {
        fn make_display_safe(&mut self) {
            let UndoReport {
                dry_run: _,
                restored,
                removed,
                not_undone,
            } = self;
            for list in [restored, removed, not_undone] {
                list.make_display_safe();
            }
        }
    }

    impl DisplaySafe for ApprovalView {
        fn make_display_safe(&mut self) {
            let ApprovalView {
                id: _,
                task: _,
                agent: _,
                action: _,
                summary,
                items,
                texts,
                rule,
                requested_at: _,
                expires_at: _,
                state: _,
                labels,
            } = self;
            summary.make_display_safe();
            items.make_display_safe();
            texts.make_display_safe();
            rule.make_display_safe();
            labels.make_display_safe();
        }
    }

    impl DisplaySafe for AskLabels {
        fn make_display_safe(&mut self) {
            let AskLabels { review, approve } = self;
            review.make_display_safe();
            approve.make_display_safe();
        }
    }
}

/// Text in paragraphs, such as an agent's answer (ADR 0041): it keeps its
/// line breaks, and each line is display text like any other.
pub(crate) fn display_paragraphs(text: &str) -> String {
    text.lines()
        .map(|line| display_line(line).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use glass_types::{ApprovalId, TaskId, Timestamp};
    use serde_json::json;

    use super::*;
    use crate::{AgentMode, FeedLine, TaskView, TileState};

    fn owned(text: &str) -> String {
        match display_line(text) {
            Cow::Owned(line) => line,
            Cow::Borrowed(_) => panic!("{text:?} came back unchanged"),
        }
    }

    #[test]
    fn escape_sequences_show_as_text_and_never_reach_the_terminal() {
        // Clear screen, cursor home, colors, and a cursor move up a line.
        assert_eq!(owned("Done\x1b[2J\x1b[H"), "Done\u{FFFD}[2J\u{FFFD}[H");
        assert_eq!(owned("\x1b[31mred\x1b[0m"), "\u{FFFD}[31mred\u{FFFD}[0m");
        assert_eq!(owned("a\x1b[1Ab"), "a\u{FFFD}[1Ab");
        // An OSC 8 hyperlink, ended both ways (ST and BEL).
        assert_eq!(
            owned("\x1b]8;;https://evil.example\x1b\\Invoices\x1b]8;;\x07"),
            "\u{FFFD}]8;;https://evil.example\u{FFFD}\\Invoices\u{FFFD}]8;;\u{FFFD}"
        );
        // The single-character CSI (C1), which some terminals act on too.
        assert_eq!(owned("\u{9b}2J"), "\u{FFFD}2J");
    }

    #[test]
    fn line_breaks_become_spaces_so_nothing_hides_behind_them() {
        assert_eq!(
            owned("Approved by you\rDeleted Reports"),
            "Approved by you Deleted Reports"
        );
        assert_eq!(owned("one\ntwo\r\nthree\tfour"), "one two  three four");
        assert_eq!(owned("a\u{85}b\u{2028}c\u{2029}d"), "a b c d");
        assert_eq!(owned("x\u{0B}y\u{0C}z"), "x y z");
        // Backspace, DEL, NUL and BEL can't erase or ring either.
        assert_eq!(
            owned("Send\x08\x08\x08\x08Keep"),
            "Send\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}Keep"
        );
        assert_eq!(owned("a\x7fb\0c\x07"), "a\u{FFFD}b\u{FFFD}c\u{FFFD}");
    }

    #[test]
    fn direction_overrides_and_zero_width_characters_show() {
        // Left in, U+202E makes "Invoice" + override + "fdp.exe" read "Invoiceexe.pdf".
        assert_eq!(owned("Invoice\u{202E}fdp.exe"), "Invoice\u{FFFD}fdp.exe");
        for c in [
            '\u{061C}', '\u{200B}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}',
            '\u{202D}', '\u{2060}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{FEFF}',
        ] {
            assert!(invisible(c), "{c:?}");
            assert_eq!(
                owned(&format!("mail{c}.send")),
                "mail\u{FFFD}.send",
                "{c:?}"
            );
        }
    }

    #[test]
    fn ordinary_text_comes_back_unchanged_without_a_copy() {
        for text in [
            "",
            "Write Reports/summary.csv",
            "Café crème, naïve résumé, Ålesund",
            "Größe · 東京 · שלום · مرحبا",
            "Done 🎉 👍🏽",
            // Joiners stay: a family emoji and Persian need them.
            "👨\u{200D}👩\u{200D}👧 می\u{200C}خواهم",
            "Invoice \u{FFFD} already replaced",
        ] {
            assert!(
                matches!(display_line(text), Cow::Borrowed(same) if std::ptr::eq(same, text)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn replacing_keeps_the_length_and_is_idempotent() {
        let hostile = "Résumé\x1b[2J\r\u{202E}🎉\u{9b}";
        let once = owned(hostile);
        assert_eq!(once.chars().count(), hostile.chars().count());
        assert!(matches!(display_line(&once), Cow::Borrowed(_)));
    }

    #[test]
    fn json_values_are_made_safe_keys_and_all() {
        let mut value = json!({
            "caption": "Done\x1b[2J",
            "feed": [{ "target": "a\u{202E}b" }],
            "count": 3,
            "ok\rkey": null,
        });
        value.make_display_safe();
        assert_eq!(
            value,
            json!({
                "caption": "Done\u{FFFD}[2J",
                "feed": [{ "target": "a\u{FFFD}b" }],
                "count": 3,
                "ok key": null,
            })
        );
    }

    /// Every string a tile shows that an agent can reach is covered (AW-3,
    /// AW-4): the fields are filled with hostile text and none survives.
    #[test]
    fn aw4_every_agent_reachable_string_in_a_task_view_is_made_safe() {
        let hostile = |what: &str| format!("{what}\x1b[2J\u{202E}\r");
        let mut view = TaskView {
            id: TaskId::new(),
            agent: "invoice-demo".parse().unwrap(),
            agent_name: hostile("name"),
            user: hostile("user"),
            mode: AgentMode::Actions,
            state: TileState::Failed,
            title: hostile("title"),
            caption: hostile("caption"),
            progress: None,
            feed: vec![FeedLine {
                at: Timestamp::now(),
                text: hostile("text"),
                verb: hostile("verb"),
                target: hostile("target"),
            }],
            needs: Some(crate::ApprovalView {
                id: ApprovalId::new(),
                task: TaskId::new(),
                agent: "invoice-demo".parse().unwrap(),
                action: "mail.send".parse().unwrap(),
                summary: hostile("summary"),
                items: vec![hostile("item")],
                texts: vec![vec![hostile("text")]],
                rule: hostile("rule"),
                requested_at: Timestamp::now(),
                expires_at: Timestamp::now(),
                state: crate::ApprovalState::Pending,
                labels: crate::AskLabels::new(&hostile("review"), &hostile("approve")),
            }),
            receipt: Some(crate::Receipt {
                models: vec![hostile("provider")],
                flagged: vec![hostile("flag")],
                ..Default::default()
            }),
            failure: Some(hostile("failure")),
            started_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            driver_socket: None,
            continues: None,
            computer: None,
        };
        view.make_display_safe();
        let shown = serde_json::to_string(&view).unwrap();
        assert!(
            !shown.contains("\\u001b") && !shown.contains("\\r") && !shown.contains('\u{202E}'),
            "{shown}"
        );
        assert_eq!(view.caption, "caption\u{FFFD}[2J\u{FFFD} ");
    }

    #[test]
    fn ae3_an_answer_keeps_its_paragraphs_but_no_escapes() {
        assert_eq!(
            display_paragraphs("You have 3 files.\n\nOne is \x1b[2Jnew.\r\nDone\u{202E}"),
            "You have 3 files.\n\nOne is \u{FFFD}[2Jnew.\nDone\u{FFFD}"
        );
    }
}
