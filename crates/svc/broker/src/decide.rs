use std::path::Path;

use glass_policy::EffectivePolicy;
use glass_types::{ActionName, Label, Tier};

/// What happens to one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Runs now at `tier` (Auto or Notify).
    Run { tier: Tier, rule: String },
    /// Needs the user (Ask): the task goes to "Needs you".
    Ask { rule: String },
    /// Refused. `reason` is shown to the agent and the user.
    Block { reason: String, rule: String },
}

impl Verdict {
    /// The tier recorded in the audit log.
    pub fn tier(&self) -> Tier {
        match self {
            Verdict::Run { tier, .. } => *tier,
            Verdict::Ask { .. } => Tier::Ask,
            Verdict::Block { .. } => Tier::Never,
        }
    }

    pub fn rule(&self) -> &str {
        match self {
            Verdict::Run { rule, .. } | Verdict::Ask { rule } | Verdict::Block { rule, .. } => rule,
        }
    }
}

/// Finds the label of a host path: folder rules, labels stored with the file,
/// or both. Errors (an unreadable label) block the call rather than guess.
pub trait LabelLookup {
    fn label_of(&self, path: &Path) -> Result<Option<Label>, String>;
}

impl<F: Fn(&Path) -> Result<Option<Label>, String>> LabelLookup for F {
    fn label_of(&self, path: &Path) -> Result<Option<Label>, String> {
        self(path)
    }
}

/// Decides one call: the action's tier under `policy`, then the labels of
/// every path it reads or writes (data protection holds even for actions
/// policy otherwise allows).
pub fn decide<'a>(
    policy: &EffectivePolicy,
    action: &ActionName,
    app_default: Tier,
    paths: impl IntoIterator<Item = &'a Path>,
    labels: &impl LabelLookup,
) -> Verdict {
    let decision = policy.tier_for(action, app_default);
    let rule = decision.source.to_string();
    if decision.tier == Tier::Never {
        return Verdict::Block {
            reason: format!("{action} is blocked for this task ({rule})"),
            rule,
        };
    }
    for path in paths {
        match labels.label_of(path) {
            Ok(Some(label)) if !policy.may_open(label) => {
                return Verdict::Block {
                    reason: format!(
                        "{} is labeled {label}; this agent may only open data up to {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        policy.max_label()
                    ),
                    rule: format!("data label {label}"),
                };
            }
            Ok(_) => {}
            Err(e) => {
                return Verdict::Block {
                    reason: format!("can't read the data label: {e}"),
                    rule: "data label unreadable".into(),
                };
            }
        }
    }
    match decision.tier {
        Tier::Ask => Verdict::Ask { rule },
        tier => Verdict::Run { tier, rule },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use glass_policy::Policy;

    use super::*;

    fn policy() -> EffectivePolicy {
        Policy::from_toml(glass_policy::DEFAULT_POLICY)
            .unwrap()
            .effective("demo", &"invoice-demo".parse().unwrap())
            .unwrap()
    }

    fn no_labels(_: &Path) -> Result<Option<Label>, String> {
        Ok(None)
    }

    fn name(s: &str) -> ActionName {
        s.parse().unwrap()
    }

    #[test]
    fn tiers_map_to_verdicts() {
        let p = policy();
        let none: [&Path; 0] = [];
        assert!(matches!(
            decide(&p, &name("files.read"), Tier::Auto, none, &no_labels),
            Verdict::Run {
                tier: Tier::Auto,
                ..
            }
        ));
        assert!(matches!(
            decide(&p, &name("files.write"), Tier::Notify, none, &no_labels),
            Verdict::Run {
                tier: Tier::Notify,
                ..
            }
        ));
        assert!(matches!(
            decide(&p, &name("mail.send"), Tier::Ask, none, &no_labels),
            Verdict::Ask { .. }
        ));
        let blocked = decide(&p, &name("payments.move"), Tier::Auto, none, &no_labels);
        assert_eq!(blocked.tier(), Tier::Never);
    }

    #[test]
    fn labels_above_the_limit_block_even_auto_reads() {
        let p = policy();
        let finance = PathBuf::from("/home/demo/Finance/q3.csv");
        let by_folder = |path: &Path| Ok(p.folder_label(path));
        let verdict = decide(
            &p,
            &name("files.read"),
            Tier::Auto,
            [finance.as_path()],
            &by_folder,
        );
        let Verdict::Block { reason, rule } = verdict else {
            panic!("expected a block")
        };
        assert!(reason.contains("labeled confidential"), "{reason}");
        assert_eq!(rule, "data label confidential");

        let stored = |_: &Path| Ok(Some(Label::Restricted));
        let other = PathBuf::from("/home/demo/Invoices/a.txt");
        assert_eq!(
            decide(
                &p,
                &name("files.read"),
                Tier::Auto,
                [other.as_path()],
                &stored
            )
            .tier(),
            Tier::Never
        );
    }

    #[test]
    fn unreadable_labels_fail_closed() {
        let p = policy();
        let broken = |_: &Path| Err("garbage label".to_string());
        let path = PathBuf::from("/home/demo/x");
        assert_eq!(
            decide(
                &p,
                &name("files.read"),
                Tier::Auto,
                [path.as_path()],
                &broken
            )
            .tier(),
            Tier::Never
        );
    }
}
