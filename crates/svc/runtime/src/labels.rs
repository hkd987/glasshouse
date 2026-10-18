//! Data labels for a host path: folder rules from policy, plus labels stored
//! on the file and on every folder above it up to the user's home (a label on
//! a folder covers what's inside). The most sensitive label wins.

use std::path::Path;

use glass_platform::xattr;
use glass_policy::EffectivePolicy;
use glass_types::Label;

pub(crate) fn label_of(
    policy: &EffectivePolicy,
    home: &Path,
    path: &Path,
) -> Result<Option<Label>, String> {
    let mut label = policy.folder_label(path);
    for ancestor in path.ancestors() {
        if ancestor.symlink_metadata().is_ok() {
            let stored = xattr::read_label(ancestor).map_err(|e| e.to_string())?;
            label = label.max(stored);
        }
        if ancestor == home || !ancestor.starts_with(home) {
            break;
        }
    }
    Ok(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_rules_apply_without_any_stored_label() {
        let policy = glass_policy::Policy::from_toml(glass_policy::DEFAULT_POLICY)
            .unwrap()
            .effective("demo", &"invoice-demo".parse().unwrap())
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert_eq!(
            label_of(&policy, home, &home.join("Invoices/a.txt")).unwrap(),
            None
        );
        // `/home/*/Finance` only matches under /home, so use a real path there.
        let finance = Path::new("/home/demo/Finance/q3.csv");
        assert_eq!(
            label_of(&policy, Path::new("/home/demo"), finance).unwrap(),
            Some(Label::Confidential)
        );
    }
}
