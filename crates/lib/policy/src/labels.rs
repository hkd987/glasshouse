use std::path::{Component, Path};

use glass_types::Label;

use crate::document::FolderLabel;

/// The label folder rules assign to `path`, if any rule covers it (DP-10).
///
/// When several rules cover a path the most sensitive label wins, so a broad
/// rule can never weaken a narrower, stricter one.
pub fn folder_label(rules: &[FolderLabel], path: &Path) -> Option<Label> {
    rules
        .iter()
        .filter(|rule| covers(&rule.path, path))
        .map(|rule| rule.label)
        .max()
}

/// Whether the rule path (with `*` segment wildcards) is `path` or one of its
/// ancestors.
fn covers(rule: &str, path: &Path) -> bool {
    let rule_segments: Vec<&str> = rule.split('/').filter(|s| !s.is_empty()).collect();
    let mut path_segments = path.components().filter_map(|c| match c {
        Component::Normal(s) => Some(s.to_string_lossy()),
        _ => None,
    });
    rule_segments.iter().all(|want| match path_segments.next() {
        Some(have) => *want == "*" || *want == have,
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(path: &str, label: Label) -> FolderLabel {
        FolderLabel {
            path: path.into(),
            label,
        }
    }

    #[test]
    fn rules_cover_descendants_and_wildcard_segments() {
        let rules = [
            rule("/home/*/Finance", Label::Confidential),
            rule("/home/*/Finance/Board", Label::Restricted),
            rule("/srv/shared", Label::Internal),
        ];
        let label = |p: &str| folder_label(&rules, Path::new(p));
        assert_eq!(
            label("/home/demo/Finance/q3.xlsx"),
            Some(Label::Confidential)
        );
        assert_eq!(label("/home/demo/Finance"), Some(Label::Confidential));
        assert_eq!(
            label("/home/demo/Finance/Board/minutes.pdf"),
            Some(Label::Restricted)
        );
        assert_eq!(label("/home/demo/Invoices/a.txt"), None);
        assert_eq!(label("/home/demo/FinanceOld/a.txt"), None);
        assert_eq!(label("/srv/shared/x"), Some(Label::Internal));
        assert_eq!(label("/home"), None);
    }
}
