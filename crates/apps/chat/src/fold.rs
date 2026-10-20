//! What a chat conversation shows, folded from its messages (ADR 0030
//! C4a, CF-92): edits, deletes, reactions, pins and topics are messages of
//! their own, applied in sequence order. Every computer folds the same
//! messages the same way, judging each by its verified sender and never by
//! anything a body claims.

use std::collections::{BTreeMap, HashMap};

use glass_managed::chat::Body;
use glass_managed::shared::Part;

/// A message this computer opened: its verified header fields and body.
#[derive(Debug, Clone)]
pub(crate) struct Opened {
    pub id: String,
    pub seq: u64,
    /// `Header.sender`, checked against its key when it was kept.
    pub sender: String,
    pub at: String,
    pub body: Body,
}

/// A message as people see it.
#[derive(Debug, Clone)]
pub(crate) struct Shown {
    pub id: String,
    pub seq: u64,
    pub sender: String,
    pub at: String,
    /// The latest edit's text, or "" once deleted.
    pub text: String,
    pub attribution: Option<String>,
    pub reply_to: Option<String>,
    /// An item shared here, as a card (ADR 0033).
    pub shared: Option<Part>,
    pub mentions: Vec<String>,
    pub edited: bool,
    pub deleted: bool,
    /// Emoji in the order first used, each with who has it on now.
    pub reactions: Vec<(String, Vec<String>)>,
    pub pinned: bool,
    /// The thread's root, for a reply.
    pub thread: Option<String>,
    /// For a root: its replies, oldest first, as indexes into
    /// [`Folded::messages`].
    pub replies: Vec<usize>,
}

impl Shown {
    pub fn is_root(&self) -> bool {
        self.thread.is_none()
    }
}

/// A conversation as people see it.
#[derive(Debug, Default)]
pub(crate) struct Folded {
    /// Messages people wrote (not edits, reactions and the like), in
    /// sequence order.
    pub messages: Vec<Shown>,
    pub index: HashMap<String, usize>,
    pub topic: Option<String>,
    /// Pinned messages, most recently pinned first.
    pub pinned: Vec<String>,
    /// The newest sequence number folded, control parts included.
    pub newest: u64,
    /// Messages this computer couldn't open.
    pub sealed: usize,
}

/// An emoji and, per sender, whether theirs is on.
type Reacted = (String, BTreeMap<String, bool>);

/// How deep a reply chain is followed to its root.
const THREAD_DEPTH: usize = 64;

/// Whether `part` changes another message or the conversation rather than
/// being a message people read.
pub(crate) fn is_control(part: Option<&Part>) -> bool {
    matches!(
        part,
        Some(
            Part::Edit { .. }
                | Part::Delete { .. }
                | Part::Reaction { .. }
                | Part::Pin { .. }
                | Part::Topic { .. }
        )
    )
}

impl Folded {
    /// Folds `opened`, which must be in sequence order. `channel` when the
    /// conversation is a channel (only channels have topics).
    pub fn new(opened: &[Opened], channel: bool) -> Self {
        let mut folded = Folded::default();
        for o in opened {
            folded.newest = folded.newest.max(o.seq);
            if is_control(o.body.part.as_ref()) {
                continue;
            }
            let shared = match &o.body.part {
                Some(p @ Part::Shared { .. }) => Some(p.clone()),
                _ => None,
            };
            folded.index.insert(o.id.clone(), folded.messages.len());
            folded.messages.push(Shown {
                id: o.id.clone(),
                seq: o.seq,
                sender: o.sender.clone(),
                at: o.at.clone(),
                text: o.body.text.clone(),
                attribution: o.body.attribution.clone(),
                reply_to: o.body.reply_to.clone(),
                shared,
                mentions: o.body.mentions.clone(),
                edited: false,
                deleted: false,
                reactions: Vec::new(),
                pinned: false,
                thread: None,
                replies: Vec::new(),
            });
        }
        folded.threads();
        // Reactions per message: emoji → sender → on, in first-use order.
        let mut reactions: HashMap<usize, Vec<Reacted>> = HashMap::new();
        let mut pins: Vec<(String, u64)> = Vec::new();
        for o in opened {
            let target = |id: &str| folded.index.get(id).copied();
            match &o.body.part {
                Some(Part::Edit { message, text }) => {
                    if let Some(i) = target(message) {
                        let m = &mut folded.messages[i];
                        if m.sender == o.sender && !m.deleted {
                            m.text.clone_from(text);
                            m.mentions.clone_from(&o.body.mentions);
                            m.edited = true;
                        }
                    }
                }
                Some(Part::Delete { message }) => {
                    if let Some(i) = target(message) {
                        let m = &mut folded.messages[i];
                        if m.sender == o.sender {
                            m.deleted = true;
                            m.text.clear();
                            m.shared = None;
                            m.mentions.clear();
                        }
                    }
                }
                Some(Part::Reaction { to, emoji, off }) => {
                    if let Some(i) = target(to) {
                        let list = reactions.entry(i).or_default();
                        let slot = match list.iter().position(|(e, _)| e == emoji) {
                            Some(at) => at,
                            None => {
                                list.push((emoji.clone(), BTreeMap::new()));
                                list.len() - 1
                            }
                        };
                        list[slot].1.insert(o.sender.clone(), !off);
                    }
                }
                Some(Part::Pin { message, pinned }) => {
                    if target(message).is_some() {
                        pins.retain(|(id, _)| id != message);
                        if *pinned {
                            pins.push((message.clone(), o.seq));
                        }
                    }
                }
                Some(Part::Topic { text }) if channel => {
                    folded.topic = Some(text.clone()).filter(|t| !t.trim().is_empty());
                }
                _ => {}
            }
        }
        for (i, list) in reactions {
            let m = &mut folded.messages[i];
            if m.deleted {
                continue;
            }
            m.reactions = list
                .into_iter()
                .map(|(emoji, by)| {
                    let on: Vec<String> = by
                        .into_iter()
                        .filter(|(_, on)| *on)
                        .map(|(s, _)| s)
                        .collect();
                    (emoji, on)
                })
                .filter(|(_, on)| !on.is_empty())
                .collect();
        }
        pins.retain(|(id, _)| {
            folded
                .index
                .get(id)
                .is_some_and(|&i| !folded.messages[i].deleted)
        });
        pins.reverse();
        for (id, _) in &pins {
            if let Some(&i) = folded.index.get(id) {
                folded.messages[i].pinned = true;
            }
        }
        folded.pinned = pins.into_iter().map(|(id, _)| id).collect();
        folded
    }

    /// A reply's thread is its `reply_to` chain followed to a message with
    /// no `reply_to`. A reply to a message this computer doesn't have shows
    /// on its own, as a root.
    fn threads(&mut self) {
        for i in 0..self.messages.len() {
            let mut at = i;
            let mut root = None;
            for _ in 0..THREAD_DEPTH {
                let Some(parent) = self.messages[at].reply_to.as_deref() else {
                    break;
                };
                match self.index.get(parent) {
                    Some(&p) if p != at => {
                        at = p;
                        if self.messages[p].reply_to.is_none() {
                            root = Some(p);
                            break;
                        }
                    }
                    _ => break,
                }
            }
            if let Some(r) = root
                && r != i
            {
                self.messages[i].thread = Some(self.messages[r].id.clone());
                self.messages[r].replies.push(i);
            }
        }
    }

    /// Thread roots, in order.
    pub fn roots(&self) -> impl Iterator<Item = &Shown> {
        self.messages.iter().filter(|m| m.is_root())
    }

    pub fn get(&self, id: &str) -> Option<&Shown> {
        self.index.get(id).map(|&i| &self.messages[i])
    }
}

/// Members named `@login` in `text`, each once, in order; never `me`.
pub(crate) fn mentions(text: &str, members: &[String], me: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('@') {
        let before = rest[..at].chars().next_back();
        rest = &rest[at + 1..];
        // `name@example.com` is an address, not a mention.
        if before.is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
            .unwrap_or(rest.len());
        let login = rest[..end].trim_end_matches('.');
        if !login.is_empty()
            && login != me
            && members.iter().any(|m| m == login)
            && !found.iter().any(|f| f == login)
        {
            found.push(login.to_owned());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, seq: u64, sender: &str, text: &str) -> Opened {
        Opened {
            id: id.into(),
            seq,
            sender: sender.into(),
            at: format!("2026-10-20T10:{seq:02}:00Z"),
            body: Body {
                text: text.into(),
                attribution: None,
                reply_to: None,
                sent_at: String::new(),
                part: None,
                mentions: Vec::new(),
            },
        }
    }

    fn part(id: &str, seq: u64, sender: &str, part: Part) -> Opened {
        let mut o = msg(id, seq, sender, "");
        o.body.part = Some(part);
        o
    }

    fn reply(id: &str, seq: u64, sender: &str, to: &str) -> Opened {
        let mut o = msg(id, seq, sender, "reply");
        o.body.reply_to = Some(to.into());
        o
    }

    #[test]
    fn cf92_an_edit_or_delete_counts_only_from_the_sender() {
        let opened = vec![
            msg("m1", 1, "dana", "Ship Friday"),
            msg("m2", 2, "priya", "Agreed"),
            part(
                "e1",
                3,
                "priya",
                Part::Edit {
                    message: "m1".into(),
                    text: "Ship Monday".into(),
                },
            ),
            part(
                "e2",
                4,
                "dana",
                Part::Edit {
                    message: "m1".into(),
                    text: "Ship Thursday".into(),
                },
            ),
            part(
                "d1",
                5,
                "dana",
                Part::Delete {
                    message: "m2".into(),
                },
            ),
            part(
                "d2",
                6,
                "priya",
                Part::Delete {
                    message: "m2".into(),
                },
            ),
            part(
                "e3",
                7,
                "priya",
                Part::Edit {
                    message: "m2".into(),
                    text: "back".into(),
                },
            ),
        ];
        let f = Folded::new(&opened, false);
        assert_eq!(f.messages.len(), 2, "control parts aren't messages");
        let m1 = f.get("m1").unwrap();
        assert_eq!((m1.text.as_str(), m1.edited), ("Ship Thursday", true));
        let m2 = f.get("m2").unwrap();
        assert!(m2.deleted && m2.text.is_empty(), "{m2:?}");
        assert!(!m2.edited, "a deleted message isn't edited back");
    }

    #[test]
    fn cf92_reactions_toggle_per_person_and_emoji() {
        let r = |id: &str, seq, by: &str, emoji: &str, off| {
            part(
                id,
                seq,
                by,
                Part::Reaction {
                    to: "m1".into(),
                    emoji: emoji.into(),
                    off,
                },
            )
        };
        let opened = vec![
            msg("m1", 1, "dana", "Lunch?"),
            r("r1", 2, "priya", "👍", false),
            r("r2", 3, "sam", "🎉", false),
            r("r3", 4, "sam", "👍", false),
            r("r4", 5, "priya", "👍", true),
            r("r5", 6, "sam", "🎉", true),
            r("r6", 7, "priya", "👍", false),
            r("r7", 8, "priya", "👍", false),
        ];
        let f = Folded::new(&opened, false);
        let m = f.get("m1").unwrap();
        assert_eq!(
            m.reactions,
            vec![("👍".to_owned(), vec!["priya".to_owned(), "sam".to_owned()])],
            "🎉 is all off, so it's gone; a repeat 'on' counts once"
        );
    }

    #[test]
    fn cf92_a_thread_folds_on_its_root() {
        let opened = vec![
            msg("m1", 1, "dana", "Plan?"),
            reply("m2", 2, "priya", "m1"),
            reply("m3", 3, "sam", "m2"),
            msg("m4", 4, "dana", "Next"),
            reply("m5", 5, "sam", "gone"),
        ];
        let f = Folded::new(&opened, false);
        let roots: Vec<&str> = f.roots().map(|m| m.id.as_str()).collect();
        assert_eq!(
            roots,
            ["m1", "m4", "m5"],
            "a reply to an unknown message shows on its own"
        );
        let m1 = f.get("m1").unwrap();
        assert_eq!(m1.replies, [1, 2]);
        assert_eq!(
            f.get("m3").unwrap().thread.as_deref(),
            Some("m1"),
            "a reply to a reply joins the root's thread"
        );
    }

    #[test]
    fn cf92_parts_before_their_message_apply_when_it_arrives() {
        // A pin and a reaction whose message this computer didn't have yet:
        // folded again with it, they apply.
        let mut opened = vec![
            part(
                "p1",
                2,
                "priya",
                Part::Pin {
                    message: "m1".into(),
                    pinned: true,
                },
            ),
            part(
                "r1",
                3,
                "priya",
                Part::Reaction {
                    to: "m1".into(),
                    emoji: "✅".into(),
                    off: false,
                },
            ),
        ];
        assert!(Folded::new(&opened, false).messages.is_empty());
        opened.insert(0, msg("m1", 1, "dana", "Decision: go"));
        let f = Folded::new(&opened, false);
        assert_eq!(f.pinned, ["m1"]);
        assert!(f.get("m1").unwrap().pinned);
        assert_eq!(f.get("m1").unwrap().reactions.len(), 1);
    }

    #[test]
    fn cf92_pins_and_topics_latest_wins_and_topics_are_for_channels() {
        let opened = vec![
            msg("m1", 1, "dana", "a"),
            msg("m2", 2, "dana", "b"),
            part(
                "p1",
                3,
                "priya",
                Part::Pin {
                    message: "m1".into(),
                    pinned: true,
                },
            ),
            part(
                "p2",
                4,
                "sam",
                Part::Pin {
                    message: "m2".into(),
                    pinned: true,
                },
            ),
            part(
                "p3",
                5,
                "dana",
                Part::Pin {
                    message: "m1".into(),
                    pinned: false,
                },
            ),
            part(
                "p4",
                6,
                "dana",
                Part::Pin {
                    message: "m1".into(),
                    pinned: true,
                },
            ),
            part(
                "t1",
                7,
                "priya",
                Part::Topic {
                    text: "Q3 launch".into(),
                },
            ),
            part(
                "t2",
                8,
                "sam",
                Part::Topic {
                    text: "Q4 launch".into(),
                },
            ),
        ];
        let f = Folded::new(&opened, true);
        assert_eq!(f.pinned, ["m1", "m2"], "most recently pinned first");
        assert_eq!(f.topic.as_deref(), Some("Q4 launch"));
        assert_eq!(Folded::new(&opened, false).topic, None);
    }

    #[test]
    fn cf92_mentions_name_members_once_and_not_addresses() {
        let members = ["dana".to_owned(), "priya".to_owned(), "sam.lee".to_owned()];
        assert_eq!(
            mentions(
                "@priya and @sam.lee, see @priya. Mail ops@priya.example or @mallory",
                &members,
                "dana"
            ),
            ["priya", "sam.lee"]
        );
        assert!(mentions("@dana note to self", &members, "dana").is_empty());
    }
}
