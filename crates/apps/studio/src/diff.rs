//! What changed between an app and the app it remixes, file by file, as a
//! reviewer reads it (ADR 0032, R2): unified hunks with three lines of
//! context, from a Myers diff of the lines. Small apps only (the studio's
//! limits); past [`MAX_EDITS`] a file reads as replaced whole.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Lines of context around each change.
const CONTEXT: usize = 3;
/// More edits than this in one file and it is shown replaced whole.
const MAX_EDITS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Removed,
    Changed,
}

/// One file that differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: Status,
    pub added: usize,
    pub removed: usize,
    /// Unified hunks: `@@ -a,b +c,d @@`, then lines starting ` `, `-`, `+`.
    pub unified: String,
}

/// Every file that differs from `base` in `new`, by path.
pub fn changes(base: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Vec<FileChange> {
    let mut out = Vec::new();
    let paths: std::collections::BTreeSet<&String> = base.keys().chain(new.keys()).collect();
    for path in paths {
        let (old, now) = (base.get(path), new.get(path));
        if old == now {
            continue;
        }
        let status = match (old, now) {
            (None, _) => Status::Added,
            (_, None) => Status::Removed,
            _ => Status::Changed,
        };
        let (unified, added, removed) = unified(
            old.map_or("", String::as_str),
            now.map_or("", String::as_str),
        );
        out.push(FileChange {
            path: path.clone(),
            status,
            added,
            removed,
            unified,
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Same(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// The shortest edit script from `a` to `b` (Myers, 1986), or `None` past
/// [`MAX_EDITS`].
fn script(a: &[&str], b: &[&str]) -> Option<Vec<Edit>> {
    script_counted(a, b).map(|(edits, _)| edits)
}

/// [`script`], and how many numbers the search kept to walk back through.
///
/// The search saves where it stood after each step. It saved the whole of
/// its working row every time, 8,003 numbers of eight bytes whatever the
/// step: 256 MB for one file at the limit, and 190 KB for a file with
/// three changed lines (audit 2.10). A step `d` only ever reads the
/// `2d - 1` numbers the step before it wrote, so those are what is kept,
/// as `u32` (a line number).
fn script_counted(a: &[&str], b: &[&str]) -> Option<(Vec<Edit>, usize)> {
    // A file added or removed whole: nothing to search for, and it was the
    // costliest search there is, every line an edit.
    if a.is_empty() || b.is_empty() {
        let edits = (0..a.len())
            .map(Edit::Delete)
            .chain((0..b.len()).map(Edit::Insert))
            .collect();
        return Some((edits, 0));
    }
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m).min(MAX_EDITS as isize);
    let offset = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    // `fronts[d]`: the row as step `d` found it, diagonals `-(d - 1)` to
    // `d - 1`.
    let mut fronts: Vec<Vec<u32>> = Vec::new();
    let front = |v: &[isize], d: isize| -> Vec<u32> {
        if d == 0 {
            return Vec::new();
        }
        let (from, to) = ((offset - (d - 1)) as usize, (offset + (d - 1)) as usize);
        v[from..=to]
            .iter()
            .map(|x| u32::try_from(*x).unwrap_or(u32::MAX))
            .collect()
    };
    let mut found = None;
    for d in 0..=max {
        fronts.push(front(&v, d));
        let mut k = -d;
        while k <= d {
            let at = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[at - 1] < v[at + 1]) {
                v[at + 1]
            } else {
                v[at - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at] = x;
            if x >= n && y >= m {
                found = Some(d);
                break;
            }
            k += 2;
        }
        if found.is_some() {
            break;
        }
    }
    let found = found?;
    let kept = fronts.iter().map(Vec::len).sum();
    // Walk back through the saved fronts.
    let mut edits = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=found).rev() {
        let row = &fronts[d as usize];
        let at = |k: isize| row[(k + d - 1) as usize] as isize;
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = at(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            edits.push(Edit::Same(x as usize, y as usize));
        }
        if x == prev_x {
            y -= 1;
            edits.push(Edit::Insert(y as usize));
        } else {
            x -= 1;
            edits.push(Edit::Delete(x as usize));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        edits.push(Edit::Same(x as usize, y as usize));
    }
    edits.reverse();
    Some((edits, kept))
}

/// `old` to `new` as unified hunks: the text, lines added, lines removed.
fn unified(old: &str, new: &str) -> (String, usize, usize) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let edits = script(&a, &b).unwrap_or_else(|| {
        (0..a.len())
            .map(Edit::Delete)
            .chain((0..b.len()).map(Edit::Insert))
            .collect()
    });
    let added = edits
        .iter()
        .filter(|e| matches!(e, Edit::Insert(_)))
        .count();
    let removed = edits
        .iter()
        .filter(|e| matches!(e, Edit::Delete(_)))
        .count();
    // Hunks: runs of changes with up to CONTEXT unchanged lines either side,
    // joined when closer than twice that.
    let changed: Vec<usize> = edits
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e, Edit::Same(..)))
        .map(|(i, _)| i)
        .collect();
    let mut out = String::new();
    let mut i = 0;
    while i < changed.len() {
        let start = changed[i].saturating_sub(CONTEXT);
        let mut end = changed[i];
        while i + 1 < changed.len() && changed[i + 1] <= end + 2 * CONTEXT + 1 {
            i += 1;
            end = changed[i];
        }
        let end = (end + CONTEXT + 1).min(edits.len());
        let hunk = &edits[start..end];
        let first_old = hunk.iter().find_map(|e| match e {
            Edit::Same(x, _) | Edit::Delete(x) => Some(*x),
            Edit::Insert(_) => None,
        });
        let first_new = hunk.iter().find_map(|e| match e {
            Edit::Same(_, y) | Edit::Insert(y) => Some(*y),
            Edit::Delete(_) => None,
        });
        let old_len = hunk
            .iter()
            .filter(|e| !matches!(e, Edit::Insert(_)))
            .count();
        let new_len = hunk
            .iter()
            .filter(|e| !matches!(e, Edit::Delete(_)))
            .count();
        out.push_str(&format!(
            "@@ -{},{old_len} +{},{new_len} @@\n",
            first_old.map_or(0, |x| x + 1),
            first_new.map_or(0, |y| y + 1),
        ));
        for edit in hunk {
            match *edit {
                Edit::Same(x, _) => out.push_str(&format!(" {}\n", a[x])),
                Edit::Delete(x) => out.push_str(&format!("-{}\n", a[x])),
                Edit::Insert(y) => out.push_str(&format!("+{}\n", b[y])),
            }
        }
        i += 1;
    }
    (out, added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect()
    }

    #[test]
    fn ga8_a_reviewer_sees_each_changed_line_with_its_context() {
        let base: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let new = base
            .replace("line 10\n", "line ten\n")
            .replace("line 19\n", "");
        let out = changes(
            &files(&[("src/main.ts", &base), ("src/gone.ts", "x\n")]),
            &files(&[("src/main.ts", &new), ("src/new.ts", "y\n")]),
        );
        let paths: Vec<_> = out.iter().map(|c| (c.path.as_str(), c.status)).collect();
        assert_eq!(
            paths,
            [
                ("src/gone.ts", Status::Removed),
                ("src/main.ts", Status::Changed),
                ("src/new.ts", Status::Added),
            ]
        );
        let main = &out[1];
        assert_eq!((main.added, main.removed), (1, 2));
        assert!(main.unified.starts_with("@@ -7,"), "{}", main.unified);
        assert!(
            main.unified.contains("-line 10\n+line ten\n"),
            "{}",
            main.unified
        );
        assert!(
            main.unified.contains(" line 13\n"),
            "context: {}",
            main.unified
        );
        assert!(
            !main.unified.contains(" line 2\n"),
            "no more than 3 lines around: {}",
            main.unified
        );
        assert!(main.unified.contains("-line 19\n"), "{}", main.unified);
        assert_eq!(out[2].unified, "@@ -0,0 +1,1 @@\n+y\n");
    }

    /// `edits` applied to `a`: what they make of it.
    fn applied<'a>(edits: &[Edit], a: &[&'a str], b: &[&'a str]) -> Vec<&'a str> {
        edits
            .iter()
            .filter_map(|edit| match *edit {
                Edit::Same(x, y) => {
                    assert_eq!(a[x], b[y]);
                    Some(a[x])
                }
                Edit::Insert(y) => Some(b[y]),
                Edit::Delete(_) => None,
            })
            .collect()
    }

    /// The longest run of lines `a` and `b` share in order, the slow way.
    fn shared(a: &[&str], b: &[&str]) -> usize {
        let mut row = vec![0usize; b.len() + 1];
        for x in a {
            let mut diagonal = 0;
            for (j, y) in b.iter().enumerate() {
                let up = row[j + 1];
                row[j + 1] = if x == y { diagonal + 1 } else { up.max(row[j]) };
                diagonal = up;
            }
        }
        row[b.len()]
    }

    // Audit 2.10: the search kept its whole working row at every step,
    // 256 MB for one file at the limit.
    #[test]
    fn ga8_the_search_keeps_only_what_it_reads_back() {
        // Every pair of short files over three kinds of line: the edits
        // make the new file of the old, and none fewer would.
        let lines = ["a", "b", "c"];
        let mut files: Vec<Vec<&str>> = vec![Vec::new()];
        for len in 1..=5 {
            for n in 0..3usize.pow(len) {
                files.push((0..len).map(|i| lines[n / 3usize.pow(i) % 3]).collect());
            }
        }
        for a in &files {
            for b in files.iter().step_by(7) {
                let (edits, _) = script_counted(a, b).unwrap();
                assert_eq!(&applied(&edits, a, b), b, "{a:?} to {b:?}");
                let changed = edits
                    .iter()
                    .filter(|e| !matches!(e, Edit::Same(..)))
                    .count();
                assert_eq!(
                    changed,
                    a.len() + b.len() - 2 * shared(a, b),
                    "{a:?} to {b:?}"
                );
            }
        }

        // Three changed lines in a long file: a few numbers kept, where it
        // was four rows of 8,003.
        let old: Vec<String> = (0..6000).map(|n| format!("line {n}")).collect();
        let mut new = old.clone();
        new[10] = "changed".into();
        new.remove(3000);
        let (a, b): (Vec<&str>, Vec<&str>) = (
            old.iter().map(String::as_str).collect(),
            new.iter().map(String::as_str).collect(),
        );
        let (edits, kept) = script_counted(&a, &b).unwrap();
        assert_eq!(applied(&edits, &a, &b), b);
        assert_eq!(
            edits
                .iter()
                .filter(|e| !matches!(e, Edit::Same(..)))
                .count(),
            3
        );
        assert!(kept <= 9, "{kept} numbers kept for three edits");

        // At the limit: every line of 2,000 replaced. A sixteenth of what
        // it kept, in numbers half the size.
        let other: Vec<String> = (0..2000).map(|n| format!("other {n}")).collect();
        let (a, b): (Vec<&str>, Vec<&str>) = (
            old[..2000].iter().map(String::as_str).collect(),
            other.iter().map(String::as_str).collect(),
        );
        let (edits, kept) = script_counted(&a, &b).unwrap();
        assert_eq!(applied(&edits, &a, &b), b);
        assert!(kept <= 4000 * 4000, "{kept}");
        // One more edit than the limit reads as replaced whole.
        let (a, b): (Vec<&str>, Vec<&str>) = (
            old[..2001].iter().map(String::as_str).collect(),
            other.iter().map(String::as_str).collect(),
        );
        assert_eq!(script_counted(&a, &b), None);

        // A file added or removed whole is searched for nothing, however
        // long: it was the costliest search of all.
        let (edits, kept) = script_counted(&[], &b).unwrap();
        assert_eq!((edits.len(), kept), (2000, 0));
        assert!(edits.iter().all(|e| matches!(e, Edit::Insert(_))));
        let long: Vec<&str> = old.iter().map(String::as_str).collect();
        let (edits, kept) = script_counted(&long, &[]).unwrap();
        assert_eq!((edits.len(), kept), (6000, 0));
        assert!(edits.iter().all(|e| matches!(e, Edit::Delete(_))));
    }

    #[test]
    fn the_same_files_differ_in_nothing() {
        let same = files(&[("src/main.ts", "a\nb\n")]);
        assert!(changes(&same, &same).is_empty());
    }
}
