//! Task titles: the short name a task goes by in lists such as the command
//! bar's Recent ("Reconcile the October invoices"). The user may give one
//! with the request; otherwise it is made from the prompt, and the agent may
//! rename the task once while it runs (`report_progress` with a `title`).
//! Retry and take over keep the title.

use glass_api::TITLE_MAX_CHARS;

/// About how long a title made from the prompt is, in characters.
const MADE_CHARS: usize = 40;

/// What a title made from a prompt that has no words is called.
const UNTITLED: &str = "Untitled task";

/// Openers that aren't part of what the task is.
const OPENERS: &[&str] = &["please", "can you", "could you", "would you"];

/// Words a shortened title shouldn't end on.
const DANGLING: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "then", "to", "of", "for", "in", "on", "at", "with",
    "from", "by", "into", "as", "my", "your", "our", "their", "its", "this", "that",
];

/// Punctuation a title doesn't end with.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', '-', '–', '—', '…'];

/// A title made from the prompt: the first clause of its first line, cut at
/// a word boundary to about 40 characters, capitalized, with no trailing
/// punctuation.
pub(crate) fn from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(plain)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut clause = first_clause(without_openers(&line));
    if clause.chars().count() > MADE_CHARS {
        clause = shortened(clause);
    }
    let title = clause.trim_end_matches(|c: char| c.is_whitespace() || TRAILING.contains(&c));
    let mut chars = title.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => UNTITLED.into(),
    }
}

/// A title the user or the agent gave, trimmed; or why it can't be one.
pub(crate) fn check(title: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("a title can't be empty".into());
    }
    if title.chars().any(|c| c.is_control() || invisible(c)) {
        return Err("a title is one line of plain text".into());
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(format!("a title is at most {TITLE_MAX_CHARS} characters"));
    }
    Ok(title.to_owned())
}

/// Characters that change how text around them shows without showing
/// themselves, such as direction overrides, or that break the line.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{2028}' | '\u{2029}' | '\u{FEFF}'
        | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// One line with control and invisible characters dropped and runs of
/// spaces collapsed.
fn plain(line: &str) -> String {
    line.chars()
        .map(|c| {
            if c.is_control() || invisible(c) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn without_openers(mut line: &str) -> &str {
    'strip: loop {
        for opener in OPENERS {
            let Some(head) = line.get(..opener.len()) else {
                continue;
            };
            let rest = &line[opener.len()..];
            if head.eq_ignore_ascii_case(opener) && rest.starts_with([' ', ',']) {
                line = rest.trim_start_matches([' ', ',']);
                continue 'strip;
            }
        }
        return line;
    }
}

/// Up to the first `.`, `,`, `;`, `:`, `!` or `?` that ends a word (so
/// "2:00" and "$412.60" stay whole), dash or parenthesis.
fn first_clause(line: &str) -> &str {
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let ends_word = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if matches!(c, '—' | '–' | '(')
            || (matches!(c, '.' | ',' | ';' | ':' | '!' | '?') && ends_word)
            || (c == '-' && line[..at].ends_with(' ') && ends_word)
        {
            return &line[..at];
        }
    }
    line
}

/// At most about [`MADE_CHARS`] characters, ending on a whole word that
/// isn't an article, preposition or conjunction.
fn shortened(clause: &str) -> &str {
    let limit = clause
        .char_indices()
        .nth(MADE_CHARS)
        .map_or(clause.len(), |(at, _)| at);
    // A word that ends right at the limit is whole.
    let mut cut = if clause[limit..].starts_with(' ') {
        &clause[..limit]
    } else {
        clause[..limit]
            .rsplit_once(' ')
            .map_or(&clause[..limit], |(head, _)| head)
    };
    while let Some((head, last)) = cut.trim_end().rsplit_once(' ') {
        if !DANGLING.contains(&last.to_lowercase().as_str()) {
            break;
        }
        cut = head;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_the_first_clause_capitalized() {
        for (prompt, title) in [
            (
                "Reconcile the October invoices",
                "Reconcile the October invoices",
            ),
            (
                "reconcile the October invoices. Email Dana when done.",
                "Reconcile the October invoices",
            ),
            (
                "Reconcile the October invoices, then email Dana a summary",
                "Reconcile the October invoices",
            ),
            ("My VPN won't connect!", "My VPN won't connect"),
            ("Why is my laptop slow?", "Why is my laptop slow"),
            (
                "Onboard Alex Kim — accounts, laptop, mail",
                "Onboard Alex Kim",
            ),
            ("prep me for my 2:00", "Prep me for my 2:00"),
            (
                "Pay INV-0407 for $412.60 today",
                "Pay INV-0407 for $412.60 today",
            ),
            ("\n\n  Draft replies\nto everyone", "Draft replies"),
        ] {
            assert_eq!(from_prompt(prompt), title, "{prompt:?}");
        }
    }

    #[test]
    fn titles_skip_openers() {
        assert_eq!(
            from_prompt("Could you please reconcile the invoices?"),
            "Reconcile the invoices"
        );
        assert_eq!(from_prompt("Please, file my expenses"), "File my expenses");
        assert_eq!(from_prompt("Pleased to meet you"), "Pleased to meet you");
    }

    #[test]
    fn long_titles_end_on_a_whole_meaningful_word() {
        let title = from_prompt(
            "Reconcile every vendor invoice from October against the purchase orders in the ledger",
        );
        assert_eq!(title, "Reconcile every vendor invoice");
        assert!(title.chars().count() <= MADE_CHARS);
        // A word that ends exactly at the limit stays.
        assert_eq!(
            from_prompt("Reconcile the October and November bills by vendor"),
            "Reconcile the October and November bills"
        );
        // One long word is cut where it has to be.
        assert_eq!(from_prompt(&"x".repeat(80)).chars().count(), MADE_CHARS);
    }

    #[test]
    fn titles_from_prompts_without_words_still_read() {
        assert_eq!(from_prompt("?!"), UNTITLED);
        assert_eq!(from_prompt("Check\u{1b}[2J this\u{202E}"), "Check [2J this");
    }

    #[test]
    fn given_titles_are_one_short_line_of_plain_text() {
        assert_eq!(
            check("  Reconcile October invoices "),
            Ok("Reconcile October invoices".into())
        );
        assert!(check(" ").is_err());
        assert!(check("Two\nlines").is_err());
        assert!(check("Paint the screen\u{1b}[2J").is_err());
        assert!(check("Invoices\u{202E}fdp.exe").is_err());
        assert!(check(&"x".repeat(TITLE_MAX_CHARS)).is_ok());
        assert!(check(&"x".repeat(TITLE_MAX_CHARS + 1)).is_err());
    }
}
