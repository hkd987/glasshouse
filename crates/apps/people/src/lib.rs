//! The `people` app's action (AD-1): `people.search` looks colleagues up in
//! the org's directory as this device last applied it (ADR 0023), so Mail
//! and Calendar can offer them as people type an address, and agents can
//! find who to write to.
//!
//! Read only, at Auto, audited like every action, and capped at
//! [`MAX_LIMIT`] people a call, so reading the directory is visible and
//! slow to scrape. It never decides who can be written to: anyone can
//! still be reached by typing their address in full.

use glass_actions::{ActionDef, ActionError, ActionOutput, ActionProvider, Preview, TaskContext};
use glass_api::{AskLabels, display_line};
use glass_managed::DirectoryHandle;
use glass_managed::directory::search;
use glass_types::{ActionName, SideEffect, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

pub const SEARCH: &str = "people.search";
/// The most people one call returns.
pub const MAX_LIMIT: usize = 20;
const DEFAULT_LIMIT: usize = 8;
/// The longest query.
const MAX_QUERY: usize = 100;

#[derive(Debug, Clone)]
pub struct PeopleProvider {
    directory: DirectoryHandle,
    /// This device is managed by an org (ADR 0023): with no directory yet,
    /// one is on its way; unmanaged, none will come.
    managed: bool,
}

impl PeopleProvider {
    pub fn new(directory: DirectoryHandle) -> Self {
        Self {
            directory,
            managed: false,
        }
    }

    pub fn managed(mut self, managed: bool) -> Self {
        self.managed = managed;
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

impl SearchInput {
    fn parse(input: &Value) -> Result<Self, ActionError> {
        let input: Self = glass_actions::parse_input(input)?;
        let query = input.query.trim();
        if query.is_empty() || query.chars().count() > MAX_QUERY {
            return Err(ActionError::InvalidInput(format!(
                "the query must be 1 to {MAX_QUERY} characters"
            )));
        }
        if input.limit.is_some_and(|l| l == 0 || l > MAX_LIMIT) {
            return Err(ActionError::InvalidInput(format!(
                "limit must be 1 to {MAX_LIMIT}"
            )));
        }
        Ok(input)
    }
}

impl ActionProvider for PeopleProvider {
    fn actions(&self) -> Vec<ActionDef> {
        vec![ActionDef {
            name: SEARCH.parse().expect("static name"),
            title: "Look up people in the org's directory".into(),
            description: "Find colleagues by name, email, title or department in the directory \
                          IT's console sent this computer, for their email address. Anyone can \
                          still be written to by their full address; this only finds them."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Part of a name, an email, a title or a department, such as \"pri\" or \"legal counsel\"", "maxLength": MAX_QUERY },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            side_effect: SideEffect::None,
            default_tier: Tier::Auto,
            ask: AskLabels::default(),
        }]
    }

    fn preview(
        &self,
        _ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        if action.as_str() != SEARCH {
            return Err(ActionError::InvalidInput(format!(
                "people has no action {action}"
            )));
        }
        let input = SearchInput::parse(input)?;
        let query: String = display_line(input.query.trim()).chars().take(40).collect();
        Ok(Preview {
            summary: format!("Look up \"{query}\" in the org's directory"),
            targets: vec!["Org directory".into()],
            ..Preview::default()
        })
    }

    fn invoke(
        &self,
        _ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        if action.as_str() != SEARCH {
            return Err(ActionError::InvalidInput(format!(
                "people has no action {action}"
            )));
        }
        let input = SearchInput::parse(input)?;
        let Some(directory) = self.directory.current() else {
            let text = if self.managed {
                "No directory from IT has reached this computer yet. Addresses can still be \
                 typed in full."
            } else {
                "This computer isn't managed by an organization, so there's no directory of \
                 people. Addresses can still be typed in full."
            };
            return Ok(ActionOutput {
                text: text.into(),
                data: json!({ "directory_version": null, "people": [] }),
                changes: Vec::new(),
            });
        };
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT);
        // Everyone who matches: an org without mail yet still shares docs
        // and chats by login. Mail and Calendar keep only those with an
        // address.
        let found: Vec<_> = search(&directory.release.people, &input.query, limit);
        let people: Vec<Value> = found
            .iter()
            .map(|p| {
                json!({
                    "login": p.login,
                    "name": p.name,
                    "email": p.email,
                    "title": p.title,
                    "department": p.department,
                })
            })
            .collect();
        let query = display_line(input.query.trim());
        let text = match found.len() {
            0 => format!(
                "Nobody in the org's directory matches \"{query}\". An address typed in full \
                 still works."
            ),
            n => {
                let listed: Vec<String> = found
                    .iter()
                    .map(|p| {
                        let about: Vec<&str> = [p.title.as_str(), p.department.as_str()]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect();
                        let who = if p.email.is_empty() {
                            format!("{} (login {}, no email)", p.name, p.login)
                        } else {
                            format!("{} <{}>", p.name, p.email)
                        };
                        if about.is_empty() {
                            who
                        } else {
                            format!("{who} ({})", about.join(", "))
                        }
                    })
                    .collect();
                format!(
                    "{n} {} in the org's directory match{} \"{query}\": {}",
                    if n == 1 { "person" } else { "people" },
                    if n == 1 { "es" } else { "" },
                    listed.join("; ")
                )
            }
        };
        Ok(ActionOutput {
            text,
            data: json!({ "directory_version": directory.version, "people": people }),
            changes: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use glass_actions::testing::context;
    use glass_managed::directory::Applied;
    use glass_managed::{DirectoryRelease, Person};

    use super::*;

    fn provider() -> PeopleProvider {
        let handle = DirectoryHandle::new();
        let person = |login: &str, name: &str, email: &str, title: &str, department: &str| Person {
            login: login.into(),
            name: name.into(),
            email: email.into(),
            title: title.into(),
            department: department.into(),
        };
        handle.set(Applied {
            version: 4,
            release: DirectoryRelease {
                people: vec![
                    person(
                        "dana",
                        "Dana Ruiz",
                        "dana@acme.example",
                        "AP lead",
                        "Finance",
                    ),
                    person(
                        "priya",
                        "Priya Shah",
                        "priya@acme.example",
                        "Counsel",
                        "Legal",
                    ),
                    person("prim", "Prim Ode", "", "Contractor", ""),
                    person("sam", "Sam Lee", "sam@acme.example", "", ""),
                ],
            },
        });
        PeopleProvider::new(handle).managed(true)
    }

    fn search_for(p: &PeopleProvider, input: Value) -> Result<ActionOutput, ActionError> {
        let ctx = context(std::path::Path::new("/home/demo"));
        p.invoke(&ctx, &SEARCH.parse().unwrap(), &input)
    }

    #[test]
    fn ad1_search_finds_by_name_email_title_and_department() {
        let p = provider();
        let out = search_for(&p, json!({ "query": "pri" })).unwrap();
        // Prim has no address but is found, to share a doc or a chat with;
        // Mail and Calendar leave such people out themselves.
        let people = out.data["people"].as_array().unwrap();
        assert_eq!(people.len(), 2);
        let prim = people.iter().find(|x| x["login"] == "prim").unwrap();
        assert_eq!(prim["email"], "");
        assert!(
            out.text.contains("Prim Ode (login prim, no email)"),
            "{}",
            out.text
        );
        let priya = people.iter().find(|x| x["login"] == "priya").unwrap();
        assert_eq!(priya["email"], "priya@acme.example");
        assert_eq!(out.data["directory_version"], 4);
        assert!(
            out.text
                .contains("Priya Shah <priya@acme.example> (Counsel, Legal)")
        );
        for query in ["legal", "counsel", "priya@acme.example", "shah"] {
            let out = search_for(&p, json!({ "query": query })).unwrap();
            assert_eq!(out.data["people"][0]["name"], "Priya Shah", "{query}");
        }
        let all = search_for(&p, json!({ "query": "acme", "limit": 2 })).unwrap();
        assert_eq!(all.data["people"].as_array().unwrap().len(), 2);
        let none = search_for(&p, json!({ "query": "zz" })).unwrap();
        assert!(none.text.contains("Nobody"));
    }

    #[test]
    fn ad1_with_no_directory_yet_it_says_so() {
        let p = PeopleProvider::new(DirectoryHandle::new()).managed(true);
        let out = search_for(&p, json!({ "query": "dana" })).unwrap();
        assert!(out.text.contains("No directory from IT"));
        assert_eq!(out.data["people"], json!([]));
        let unmanaged = PeopleProvider::new(DirectoryHandle::new());
        let out = search_for(&unmanaged, json!({ "query": "dana" })).unwrap();
        assert!(out.text.contains("isn't managed"));
    }

    #[test]
    fn bad_input_is_refused_and_the_preview_is_display_safe() {
        let p = provider();
        for bad in [
            json!({}),
            json!({ "query": "" }),
            json!({ "query": "x".repeat(101) }),
            json!({ "query": "dana", "limit": 0 }),
            json!({ "query": "dana", "limit": 21 }),
            json!({ "query": "dana", "who": "me" }),
        ] {
            assert!(search_for(&p, bad.clone()).is_err(), "{bad}");
        }
        let ctx = context(std::path::Path::new("/home/demo"));
        let preview = p
            .preview(
                &ctx,
                &SEARCH.parse().unwrap(),
                &json!({ "query": "da\u{202E}na\nx" }),
            )
            .unwrap();
        assert!(!preview.summary.contains('\u{202E}') && !preview.summary.contains('\n'));
        assert_eq!(preview.targets, ["Org directory"]);
        assert!(preview.writes.is_empty() && preview.reads.is_empty());
    }
}
