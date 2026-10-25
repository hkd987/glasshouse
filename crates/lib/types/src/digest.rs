use serde::{Deserialize, Serialize};

/// A file as the signed list of an image's system files names it (ADR
/// 0064, EP-1): a regular file by its SHA-256 and size, a symbolic link by
/// where it points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDigest {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl FileDigest {
    pub fn file(sha256: String, bytes: u64) -> Self {
        Self {
            sha256,
            bytes,
            link: None,
        }
    }

    pub fn link(to: String) -> Self {
        Self {
            sha256: String::new(),
            bytes: 0,
            link: Some(to),
        }
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}
