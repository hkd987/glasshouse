//! Skills (ADR 0034): a version's bundle and its evals, how an eval's
//! answer is graded, and the scores a run records. `glass-app-skills`
//! checks and keeps them; the `skill-evals` agent runs them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A skill version's blob, as `Part::Version` and `Part::Proposal` name it.
pub const SKILL_MIME: &str = "application/vnd.glasshouse.skill+json";
/// The skill itself: front matter naming and describing it, then what it
/// teaches.
pub const SKILL_FILE: &str = "SKILL.md";
/// Its evals.
pub const EVALS_FILE: &str = "evals.json";

pub const MAX_FILES: usize = 200;
pub const MAX_PATH: usize = 200;
pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 300;
pub const MAX_CASES: usize = 50;
pub const MAX_GRADERS: usize = 10;
pub const MAX_RUNS: u32 = 5;
pub const DEFAULT_RUNS: u32 = 3;
/// The most a case's answer may run to, so a run's cost is known.
pub const MAX_TOKENS: u32 = 4096;
/// A grader's text or a pattern.
const MAX_GRADER_TEXT: usize = 2000;

/// A skill version's files: text only, by plain relative path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub files: BTreeMap<String, String>,
}

/// What a bundle that checks out says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub name: String,
    pub description: String,
    pub evals: Evals,
}

/// Whether `path` is one a bundle may hold: relative, plain characters,
/// no `..`, no dot files, not too long.
pub fn check_path(path: &str) -> Result<(), String> {
    let plain = path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
    let parts_ok = path
        .split('/')
        .all(|part| !part.is_empty() && !part.starts_with('.'));
    if path.is_empty() || path.len() > MAX_PATH || !plain || !parts_ok {
        return Err(format!(
            "{path:?} isn't a plain relative path (letters, digits, . _ - and /; no dot files; at most {MAX_PATH} characters)"
        ));
    }
    Ok(())
}

/// The `name:` and `description:` lines of `SKILL.md`'s front matter.
fn front_matter(text: &str) -> Result<(String, String), String> {
    let bad = || {
        format!(
            "{SKILL_FILE} starts with front matter: a --- line, name: and description: lines, and a --- line"
        )
    };
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err(bad());
    }
    let (mut name, mut description) = (None, None);
    for line in lines.by_ref() {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim() {
                "name" => name = Some(value),
                "description" => description = Some(value),
                _ => {}
            }
        }
    }
    let (Some(name), Some(description)) = (name, description) else {
        return Err(bad());
    };
    if name.is_empty()
        || name.chars().count() > MAX_NAME
        || description.is_empty()
        || description.chars().count() > MAX_DESCRIPTION
    {
        return Err(format!(
            "a skill's name is 1 to {MAX_NAME} characters and its description 1 to {MAX_DESCRIPTION}"
        ));
    }
    Ok((name, description))
}

impl Bundle {
    /// Whether it is a skill: plain paths, text within the blob limit, a
    /// `SKILL.md` that names and describes it, and evals that check out.
    pub fn check(&self) -> Result<Checked, String> {
        if self.files.len() > MAX_FILES {
            return Err(format!("a skill has at most {MAX_FILES} files"));
        }
        for path in self.files.keys() {
            check_path(path)?;
        }
        let size = self.to_bytes().len();
        if size > crate::shared::MAX_BLOB_BYTES {
            return Err(format!(
                "the skill is {} KB; a version is {} KB at most",
                size.div_ceil(1024),
                crate::shared::MAX_BLOB_BYTES / 1024
            ));
        }
        let skill = self
            .files
            .get(SKILL_FILE)
            .ok_or_else(|| format!("a skill has a {SKILL_FILE}"))?;
        let (name, description) = front_matter(skill)?;
        let evals: Evals = serde_json::from_str(
            self.files
                .get(EVALS_FILE)
                .ok_or_else(|| format!("a skill has its evals in {EVALS_FILE} (SK-2)"))?,
        )
        .map_err(|e| format!("{EVALS_FILE}: {e}"))?;
        evals.check()?;
        Ok(Checked {
            name,
            description,
            evals,
        })
    }

    /// As its blob holds it: JSON, its files in path order.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("a skill's bundle doesn't parse: {e}"))
    }

    /// SHA-256 of its blob, hex: what scores are recorded against.
    pub fn sha256(&self) -> String {
        hex::encode(glass_crypto::sha256(&self.to_bytes()))
    }

    /// The instructions and the message one eval case sends: the skill
    /// and its other files as instructions, the case's prompt and files as
    /// the message.
    pub fn case_request(&self, case: &Case) -> (String, String) {
        let mut system =
            String::from("You have this skill. Follow it to answer the request that follows.\n\n");
        for (path, text) in &self.files {
            if path != EVALS_FILE {
                system.push_str(&format!("<file path=\"{path}\">\n{text}\n</file>\n"));
            }
        }
        let mut user = case.prompt.clone();
        for (name, text) in &case.files {
            user.push_str(&format!("\n\n<file name=\"{name}\">\n{text}\n</file>"));
        }
        (system, user)
    }
}

fn default_runs() -> u32 {
    DEFAULT_RUNS
}

/// A skill's evals (SK-2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evals {
    /// How many times each case runs; it passes on a majority.
    #[serde(default = "default_runs")]
    pub runs: u32,
    pub cases: Vec<Case>,
}

/// One eval case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub prompt: String,
    /// Text the case gives the model with its prompt.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    pub graders: Vec<Grader>,
}

/// How an answer is graded. Every grader must pass for the case to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Grader {
    /// The answer contains `text`, ignoring case.
    Contains { text: String },
    /// The answer doesn't contain `text`, ignoring case.
    NotContains { text: String },
    /// The answer is `text`, trimmed, ignoring case.
    Equals { text: String },
    /// The trimmed answer matches `pattern`, ignoring case: `*` is any run
    /// of characters and `?` any one.
    Matches { pattern: String },
    /// The answer is JSON whose value at `path` (dotted keys and indexes,
    /// such as `items.0.total`) is `equals`.
    JsonPath { path: String, equals: Value },
    /// A second model call judges the answer against `criteria`.
    Rubric { criteria: String },
}

impl Evals {
    pub fn check(&self) -> Result<(), String> {
        if !(1..=MAX_RUNS).contains(&self.runs) {
            return Err(format!("runs is 1 to {MAX_RUNS}"));
        }
        if self.cases.is_empty() || self.cases.len() > MAX_CASES {
            return Err(format!("a skill has 1 to {MAX_CASES} eval cases"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for case in &self.cases {
            let plain = !case.id.is_empty()
                && case.id.len() <= MAX_NAME
                && case
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
            if !plain || !ids.insert(case.id.as_str()) {
                return Err(format!(
                    "case ids are distinct, 1 to {MAX_NAME} letters, digits, - and _ ({:?})",
                    case.id
                ));
            }
            if case.prompt.trim().is_empty() {
                return Err(format!("case {} has no prompt", case.id));
            }
            if case.graders.is_empty() || case.graders.len() > MAX_GRADERS {
                return Err(format!("case {} has 1 to {MAX_GRADERS} graders", case.id));
            }
            for grader in &case.graders {
                let text = match grader {
                    Grader::Contains { text }
                    | Grader::NotContains { text }
                    | Grader::Equals { text } => text,
                    Grader::Matches { pattern } => pattern,
                    Grader::JsonPath { path, .. } => path,
                    Grader::Rubric { criteria } => criteria,
                };
                if text.is_empty() || text.chars().count() > MAX_GRADER_TEXT {
                    return Err(format!(
                        "case {}: a grader's text is 1 to {MAX_GRADER_TEXT} characters",
                        case.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// The cases a comparison runs (ADR 0034 §6): these evals' cases as
    /// they are, and only the cases `change` adds. A change can add cases
    /// but can't rewrite one to pass.
    pub fn with_new_cases(&self, change: &Evals) -> Evals {
        let mut evals = self.clone();
        for case in &change.cases {
            if !evals.cases.iter().any(|c| c.id == case.id) && evals.cases.len() < MAX_CASES {
                evals.cases.push(case.clone());
            }
        }
        evals
    }

    /// How many model calls a run makes, at most: each case's runs, each
    /// with an answer and its rubric graders.
    pub fn calls(&self) -> u32 {
        self.cases
            .iter()
            .map(|c| {
                let rubrics = c
                    .graders
                    .iter()
                    .filter(|g| matches!(g, Grader::Rubric { .. }))
                    .count() as u32;
                self.runs * (1 + rubrics)
            })
            .sum()
    }
}

/// Whether `text` matches the glob `pattern`, ignoring case.
fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |v, key| match v {
        Value::Array(items) => key.parse::<usize>().ok().and_then(|i| items.get(i)),
        Value::Object(map) => map.get(key),
        _ => None,
    })
}

/// The JSON in an answer: the whole answer, or a fenced block in it.
fn json_in(answer: &str) -> Option<Value> {
    let trimmed = answer.trim();
    serde_json::from_str(trimmed).ok().or_else(|| {
        let start = trimmed.find("```")?;
        let rest = &trimmed[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let end = rest.find("```")?;
        serde_json::from_str(rest[..end].trim()).ok()
    })
}

impl Grader {
    /// Grades `answer`: Some for the graders that need no model, None for
    /// a rubric.
    pub fn grade(&self, answer: &str) -> Option<bool> {
        let lower = answer.to_lowercase();
        Some(match self {
            Grader::Contains { text } => lower.contains(&text.to_lowercase()),
            Grader::NotContains { text } => !lower.contains(&text.to_lowercase()),
            Grader::Equals { text } => answer.trim().eq_ignore_ascii_case(text.trim()),
            Grader::Matches { pattern } => glob(pattern, answer.trim()),
            Grader::JsonPath { path, equals } => {
                json_in(answer).is_some_and(|v| at_path(&v, path) == Some(equals))
            }
            Grader::Rubric { .. } => return None,
        })
    }
}

/// The instructions and message that ask a model to judge `answer` to
/// `prompt` against `criteria`.
pub fn rubric_request(criteria: &str, prompt: &str, answer: &str) -> (String, String) {
    (
        "You grade an answer against criteria. Think briefly, then end with a line that is \
         exactly PASS or exactly FAIL."
            .into(),
        format!(
            "Criteria:\n{criteria}\n\nRequest:\n{prompt}\n\nAnswer:\n{answer}\n\nDoes the answer meet the criteria?"
        ),
    )
}

/// A rubric's verdict: its reply's last line is PASS.
pub fn rubric_passed(reply: &str) -> bool {
    reply
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.trim_matches(|c: char| !c.is_ascii_alphabetic()) == "PASS")
}

/// How one case did in a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseScore {
    pub id: String,
    pub passed: u32,
    pub runs: u32,
}

impl CaseScore {
    /// Passed on a majority of its runs.
    pub fn ok(&self) -> bool {
        self.runs > 0 && self.passed * 2 > self.runs
    }
}

/// A run's scores for one bundle (SK-2): what it ran on, and each case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scores {
    /// The run: the eval task's id. Before and after scores in a proposal
    /// come from one run.
    pub run: String,
    /// The model that answered.
    pub model: String,
    /// The eval runner and its version.
    pub runner: String,
    /// Whose computer ran it.
    pub by: String,
    pub at: String,
    /// The bundle scored.
    pub sha256: String,
    pub cases: Vec<CaseScore>,
}

impl Scores {
    pub fn passed(&self) -> usize {
        self.cases.iter().filter(|c| c.ok()).count()
    }

    /// "4 of 5 cases (80%)".
    pub fn summary(&self) -> String {
        let total = self.cases.len();
        let passed = self.passed();
        format!(
            "{passed} of {total} case{} ({}%)",
            if total == 1 { "" } else { "s" },
            (passed * 100).checked_div(total).unwrap_or(0)
        )
    }

    /// Cases that pass here and fail in `after` (SK-3).
    pub fn broken_by(&self, after: &Scores) -> Vec<String> {
        self.cases
            .iter()
            .filter(|c| c.ok())
            .filter(|c| !after.cases.iter().any(|a| a.id == c.id && a.ok()))
            .map(|c| c.id.clone())
            .collect()
    }

    pub fn check(&self) -> Result<(), String> {
        let fits = |s: &str| !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control);
        if ![&self.run, &self.model, &self.runner, &self.by, &self.at]
            .iter()
            .all(|s| fits(s))
            || self.sha256.len() != 64
            || self.cases.len() > MAX_CASES
            || self
                .cases
                .iter()
                .any(|c| c.runs == 0 || c.runs > MAX_RUNS || c.passed > c.runs || !fits(&c.id))
        {
            return Err("scores are malformed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = "---\nname: Refunds\ndescription: Answers questions about refunds\n---\nRefunds take 14 days.\n";

    fn bundle(skill: &str) -> Bundle {
        let evals = serde_json::json!({
            "cases": [{
                "id": "window",
                "prompt": "How long do refunds take?",
                "graders": [{ "type": "contains", "text": "30 days" }]
            }]
        });
        Bundle {
            files: [
                (SKILL_FILE.to_owned(), skill.to_owned()),
                (EVALS_FILE.to_owned(), evals.to_string()),
            ]
            .into(),
        }
    }

    #[test]
    fn sk1_a_bundle_names_itself_and_carries_evals() {
        let checked = bundle(SKILL).check().unwrap();
        assert_eq!(checked.name, "Refunds");
        assert_eq!(checked.evals.runs, DEFAULT_RUNS);
        assert_eq!(checked.evals.calls(), 3);
        let mut no_evals = bundle(SKILL);
        no_evals.files.remove(EVALS_FILE);
        assert!(no_evals.check().unwrap_err().contains("SK-2"));
        assert!(
            bundle("Refunds take 14 days.").check().is_err(),
            "front matter"
        );
        assert_eq!(bundle(SKILL).sha256(), bundle(SKILL).sha256());
        // A change adds cases but can't rewrite one.
        let mut change = checked.evals.clone();
        change.cases[0].graders = vec![Grader::Contains { text: "".into() }];
        change.cases.push(Case {
            id: "new".into(),
            ..change.cases[0].clone()
        });
        let both = checked.evals.with_new_cases(&change);
        assert_eq!(both.cases.len(), 2);
        assert_eq!(both.cases[0], checked.evals.cases[0]);
        assert_ne!(
            bundle(SKILL).sha256(),
            bundle("---\nname: R\ndescription: d\n---\n").sha256()
        );
    }

    #[test]
    fn sk1_paths_are_plain_and_relative() {
        for good in ["SKILL.md", "examples/one.md", "a-b_c.txt"] {
            check_path(good).unwrap();
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            ".env",
            "a/.git/x",
            "a//b",
            "a b",
            "a\\b",
        ] {
            assert!(check_path(bad).is_err(), "{bad:?}");
        }
        let mut b = bundle(SKILL);
        b.files.insert("../escape".into(), "x".into());
        assert!(b.check().is_err());
    }

    #[test]
    fn sk2_graders_grade_and_a_case_passes_on_a_majority() {
        let g = |json: serde_json::Value| -> Grader { serde_json::from_value(json).unwrap() };
        let answer = "Refunds take 30 Days.";
        assert_eq!(
            g(serde_json::json!({"type":"contains","text":"30 days"})).grade(answer),
            Some(true)
        );
        assert_eq!(
            g(serde_json::json!({"type":"not_contains","text":"14 days"})).grade(answer),
            Some(true)
        );
        assert_eq!(
            g(serde_json::json!({"type":"equals","text":"refunds take 30 days."})).grade(answer),
            Some(true)
        );
        assert_eq!(
            g(serde_json::json!({"type":"matches","pattern":"refunds*30 day?."})).grade(answer),
            Some(true)
        );
        assert_eq!(
            g(serde_json::json!({"type":"matches","pattern":"*14*"})).grade(answer),
            Some(false)
        );
        let json = "Here:\n```json\n{\"items\": [{\"total\": 42}]}\n```";
        assert_eq!(
            g(serde_json::json!({"type":"json_path","path":"items.0.total","equals":42}))
                .grade(json),
            Some(true)
        );
        assert_eq!(
            g(serde_json::json!({"type":"rubric","criteria":"polite"})).grade(answer),
            None
        );
        assert!(rubric_passed("It is polite.\nPASS"));
        assert!(rubric_passed("**PASS**\n"));
        assert!(!rubric_passed("PASS or FAIL?\nFAIL"));
        assert!(!rubric_passed("Scripted reply ... end with PASS or FAIL"));
        assert!(
            CaseScore {
                id: "a".into(),
                passed: 2,
                runs: 3
            }
            .ok()
        );
        assert!(
            !CaseScore {
                id: "a".into(),
                passed: 1,
                runs: 2
            }
            .ok()
        );
    }

    #[test]
    fn sk3_a_change_that_breaks_a_case_is_named() {
        let scores = |cases: &[(&str, u32)]| Scores {
            run: "t".into(),
            model: "m".into(),
            runner: "skill-evals 1".into(),
            by: "dana".into(),
            at: "2026-10-20T10:00:00Z".into(),
            sha256: "0".repeat(64),
            cases: cases
                .iter()
                .map(|(id, passed)| CaseScore {
                    id: (*id).into(),
                    passed: *passed,
                    runs: 3,
                })
                .collect(),
        };
        let before = scores(&[("a", 3), ("b", 0), ("c", 2)]);
        let after = scores(&[("a", 3), ("b", 3), ("c", 1)]);
        assert_eq!(before.summary(), "2 of 3 cases (66%)");
        assert_eq!(after.summary(), "2 of 3 cases (66%)");
        assert_eq!(before.broken_by(&after), ["c"]);
        assert!(after.check().is_ok());
    }
}
