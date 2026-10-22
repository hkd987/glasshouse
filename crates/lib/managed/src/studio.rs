//! Sharing an app back to the org's store (ADR 0032, slice R2; GA-8,
//! GA-13): what a computer submits, what an approved app is in the signed
//! catalog, and the bundle computers fetch and check against it.
//!
//! The console takes nothing on trust: it checks a submission's source
//! itself and builds the bundle itself. What it signs into the catalog is
//! the SHA-256 of that bundle ([`Bundle::digest`]); a computer serves an
//! org app only from a bundle whose digest matches.

use std::collections::BTreeMap;

use glass_api::AppManifest;
use glass_types::AgentId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Who a shared app is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum ShareTo {
    /// A directory group: the maker's team.
    Team { group: String },
    /// Everyone in the org, through the catalog.
    Org,
}

/// The installed app a shared app remixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedBase {
    pub id: AgentId,
    pub version: String,
}

/// One of a person's own apps, sent to the console for review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    /// The maker's own id for it (`my-…`).
    pub app: AgentId,
    pub name: String,
    /// Whether the person chose `name`: a new version of an app already in
    /// the store keeps the name it has there unless they did.
    #[serde(default)]
    pub named: bool,
    pub version: u32,
    /// The login of whoever made it, on the computer that sends it.
    pub maker: String,
    pub device: String,
    pub manifest: AppManifest,
    /// Its source, path → text (`src/main.ts`).
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub based_on: Option<SharedBase>,
    /// What it's for and what changed, in the maker's words.
    pub note: String,
    pub share: ShareTo,
    /// What the computer's own checks noted.
    #[serde(default)]
    pub notes: Vec<String>,
}

/// The console's answer to a submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submitted {
    /// The id it will have in the org's catalog.
    pub id: AgentId,
    /// What happens next, in words for the person.
    pub text: String,
}

/// An app people made, as the signed catalog carries it once IT approved
/// it: computers install exactly this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgApp {
    /// Its manifest, with its org id and version.
    pub manifest: AppManifest,
    /// SHA-256 of the bundle the console built ([`Bundle::digest`]).
    pub bundle_sha256: String,
    /// SHA-256 of the source it was built from.
    pub source_sha256: String,
    /// The login of whoever made it.
    pub made_by: String,
    /// What it remixes (`mail@0.1.0`); none for an app made new.
    #[serde(default)]
    pub based_on: Option<String>,
}

/// What an org app serves: its built files, by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub app: AgentId,
    pub version: String,
    pub files: BTreeMap<String, String>,
}

impl Bundle {
    /// The SHA-256 the catalog signs: every file's name and text, in name
    /// order, each ended by a zero byte.
    pub fn digest(&self) -> String {
        digest(&self.files)
    }
}

/// SHA-256 of files, path → text, as [`Bundle::digest`] and source digests
/// are taken.
pub fn digest(files: &BTreeMap<String, String>) -> String {
    let mut hash = Sha256::new();
    for (name, text) in files {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update(text.as_bytes());
        hash.update([0]);
    }
    hex::encode(hash.finalize())
}

/// What a submission asks for a bundle: the app and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleRequest {
    pub app: AgentId,
    pub version: String,
}

/// How a computer reaches the console for sharing: submit, and fetch the
/// bundles of the org apps the catalog lists. glassd's is the console
/// client; tests use their own.
pub trait ShareLink: Send + Sync + std::fmt::Debug {
    fn submit(&self, submission: &Submission) -> Result<Submitted, String>;
    fn bundle(&self, request: &BundleRequest) -> Result<Bundle, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ga13_a_bundles_digest_covers_every_name_and_byte() {
        let mut files = BTreeMap::from([
            ("app.js".to_owned(), "a".to_owned()),
            ("index.html".to_owned(), "b".to_owned()),
        ]);
        let bundle = |files: &BTreeMap<String, String>| Bundle {
            app: "mail-unread".parse().unwrap(),
            version: "1".into(),
            files: files.clone(),
        };
        let first = bundle(&files).digest();
        assert_eq!(first.len(), 64);
        files.insert("app.js".into(), "a ".into());
        assert_ne!(bundle(&files).digest(), first);
        // Names and text can't trade bytes.
        let moved = BTreeMap::from([("app.jsa".to_owned(), String::new())]);
        let kept = BTreeMap::from([("app.js".to_owned(), "a".to_owned())]);
        assert_ne!(digest(&moved), digest(&kept));
    }

    #[test]
    fn a_submission_round_trips() {
        let submission = Submission {
            app: "my-mail".parse().unwrap(),
            name: "Mail, with Write".into(),
            named: true,
            version: 2,
            maker: "dana".into(),
            device: "dev-1".into(),
            manifest: serde_json::from_value(serde_json::json!({
                "id": "my-mail", "name": "Mail, with Write", "version": "2",
                "actions": ["mail.list"], "labels": ["internal"],
            }))
            .unwrap(),
            files: BTreeMap::from([("src/main.ts".to_owned(), "x".to_owned())]),
            based_on: Some(SharedBase {
                id: "mail".parse().unwrap(),
                version: "0.1.0".into(),
            }),
            note: "Write instead of New message".into(),
            share: ShareTo::Team {
                group: "ap-ops".into(),
            },
            notes: Vec::new(),
        };
        let json = serde_json::to_string(&submission).unwrap();
        assert!(json.contains(r#""to":"team""#));
        assert_eq!(
            serde_json::from_str::<Submission>(&json).unwrap(),
            submission
        );
    }
}
