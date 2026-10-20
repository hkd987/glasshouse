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
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m).min(MAX_EDITS as isize);
    let offset = max + 1;
    let mut v = vec![0isize; (2 * max + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = false;
    for d in 0..=max {
        trace.push(v.clone());
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
                found = true;
                break;
            }
            k += 2;
        }
        if found {
            trace.push(v.clone());
            break;
        }
    }
    if !found {
        return None;
    }
    // Walk back through the saved fronts.
    let mut edits = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len() as isize - 1).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let at = |k: isize| (k + offset) as usize;
        let prev_k = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[at(prev_k)];
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
    Some(edits)
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

    #[test]
    fn the_same_files_differ_in_nothing() {
        let same = files(&[("src/main.ts", "a\nb\n")]);
        assert!(changes(&same, &same).is_empty());
    }
}
