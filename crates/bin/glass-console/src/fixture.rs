//! A fleet to look at and to measure against: `--fixture 500,50` gives a
//! fresh console 500 enrolled devices with their people and reports, and
//! 50 catalog items, a few of them waiting for review. Deterministic, so
//! screenshots and budgets compare run to run.

use std::collections::BTreeMap;

use glass_managed::{
    CatalogItem, Declared, DeviceReport, InstallMode, ItemKind, Provenance, Ring, RunningAgent,
};
use glass_types::{Label, Tier, Timestamp};

use crate::directory::{ADMINS, Directory, Group, LocalDirectory, STARTER_DOMAIN, User};
use crate::state::{ConsoleItem, Device, Review, State};

const FIRST: &[&str] = &[
    "Ana", "Ben", "Chloe", "Dev", "Elena", "Femi", "Grace", "Hiro", "Ines", "Jon", "Kai", "Lena",
    "Marco", "Nadia", "Omar", "Paula", "Quinn", "Rosa", "Sven", "Tara", "Uma", "Victor", "Wen",
    "Yara", "Zane",
];
const LAST: &[&str] = &[
    "Abara", "Berg", "Costa", "Dahl", "Eze", "Fischer", "Gomez", "Haddad", "Ito", "Jensen",
    "Kowalski", "Lund", "Moreau", "Novak", "Okoro", "Park", "Quist", "Rossi", "Sato", "Tan",
];

const TITLES: &[&str] = &["Analyst", "Specialist", "Manager", "Associate", "Lead"];

/// A small deterministic generator, so every run draws the same fleet.
struct Draw(u64);

impl Draw {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n.max(1)
    }
}

/// The directory for `devices` people, one device each: AP Operations
/// (214 at full size), Legal Operations, Finance, and IT's admins and
/// support tiers.
pub fn directory(devices: usize, admin: &str) -> LocalDirectory {
    let mut dir = LocalDirectory::starter(admin);
    for n in 0..devices {
        let name = format!("u{:04}", n + 1);
        let (first, last) = (FIRST[n % FIRST.len()], LAST[(n / FIRST.len()) % LAST.len()]);
        let display = format!("{first} {last}");
        // first.last@, numbered once the names come round again.
        let round = n / (FIRST.len() * LAST.len());
        let email = format!(
            "{}.{}{}@{STARTER_DOMAIN}",
            first.to_lowercase(),
            last.to_lowercase(),
            if round == 0 {
                String::new()
            } else {
                round.to_string()
            }
        );
        dir.users.push(User {
            name,
            display,
            email,
            title: TITLES[n % TITLES.len()].into(),
            department: String::new(),
        });
    }
    let people: Vec<String> = (0..devices).map(|n| format!("u{:04}", n + 1)).collect();
    let take = |from: usize, count: usize| -> Vec<String> {
        people.iter().skip(from).take(count).cloned().collect()
    };
    let ap = devices * 214 / 500;
    let legal = devices * 38 / 500;
    let finance = devices * 60 / 500;
    for group in &mut dir.groups {
        match group.name.as_str() {
            "ap-ops" => group.members.extend(take(0, ap)),
            "legal-ops" => group.members.extend(take(ap, legal)),
            ADMINS => group.members.extend(take(ap + legal + finance, 4)),
            "it-helpdesk" => group.members.extend(take(ap + legal + finance + 4, 6)),
            "desktop-support" => group.members.extend(take(ap + legal + finance + 10, 3)),
            "it-security" => group.members.extend(take(ap + legal + finance + 13, 2)),
            _ => {}
        }
    }
    dir.groups.push(Group {
        name: "finance".into(),
        display: "Finance".into(),
        source: "Entra ID group Finance".into(),
        members: take(ap + legal, finance),
    });
    // Each person's department, from the group they're in.
    for (group, department) in [
        ("ap-ops", "Accounts payable"),
        ("legal-ops", "Legal"),
        ("finance", "Finance"),
        (ADMINS, "IT"),
        ("it-helpdesk", "IT"),
        ("desktop-support", "IT"),
        ("it-security", "IT"),
    ] {
        let members = dir
            .members(group)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for user in dir.users.iter_mut().filter(|u| members.contains(&u.name)) {
            user.department = department.into();
        }
    }
    dir
}

fn item(
    id: &str,
    kind: ItemKind,
    name: &str,
    publisher: &str,
    summary: &str,
    actions: &[&str],
) -> CatalogItem {
    CatalogItem {
        id: id.parse().expect("fixture ids are valid"),
        kind,
        name: name.into(),
        version: "1.0.0".into(),
        publisher: publisher.into(),
        publisher_verified: true,
        internal: publisher == "Acme IT",
        summary: summary.into(),
        keywords: Vec::new(),
        declares: Declared {
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            default_tiers: BTreeMap::new(),
            max_label: Label::Internal,
            egress: Vec::new(),
        },
        groups: BTreeMap::new(),
        pinned: false,
        provenance: Provenance::default(),
        app: None,
    }
}

/// The catalog: Contract Review waiting for Legal's approval, as on the
/// mockup, and 49 more items reviewed or waiting.
fn catalog(count: usize) -> Vec<ConsoleItem> {
    let mut contract = item(
        "contract-review",
        ItemKind::Agent,
        "Contract Review agent",
        "Clause Labs",
        "Reads contracts in the folders you choose and drafts a redline and a summary",
        &["files.read", "files.write"],
    );
    contract.version = "1.4.0".into();
    contract.keywords = vec!["contract".into(), "redline".into(), "legal".into()];
    contract.declares.max_label = Label::Confidential;
    contract.declares.default_tiers = [
        ("files.read".to_owned(), Tier::Auto),
        ("files.write".to_owned(), Tier::Notify),
    ]
    .into();
    let mut items = vec![ConsoleItem {
        item: contract,
        review: Review::Waiting,
        requested_by: "Legal Operations".into(),
        checks: vec![
            "Malware scan clean".into(),
            "No critical CVEs".into(),
            "Behavior matches declaration".into(),
        ],
        decided_by: None,
        shared: None,
        approved: None,
    }];
    let kinds = [ItemKind::App, ItemKind::Agent, ItemKind::Flow];
    let topics = [
        (
            "vendor-disputes",
            "Vendor Disputes",
            "Track disputes with vendors over invoices, with overdue reminders",
            &["records.read", "mail.draft"][..],
        ),
        (
            "month-end-close",
            "Month-end Close",
            "A checklist for the month-end close, with owners and due dates",
            &["records.read"][..],
        ),
        (
            "expense-check",
            "Expense Check",
            "Checks expense reports against the travel policy",
            &["files.read", "records.read"][..],
        ),
        (
            "invoice-intake",
            "Invoice Intake",
            "Reads invoices from the AP inbox and drafts entries",
            &["mail.read", "records.save_draft"][..],
        ),
        (
            "onboarding",
            "New Hire Onboarding",
            "Sets up accounts and a first-week plan for new hires",
            &["mail.draft", "calendar.hold"][..],
        ),
        (
            "meeting-notes",
            "Meeting Notes",
            "Turns meeting recordings into notes and follow-ups",
            &["files.read", "mail.draft"][..],
        ),
        (
            "travel-desk",
            "Travel Desk",
            "Books travel within policy and holds the dates",
            &["calendar.hold"][..],
        ),
        (
            "forecast-pack",
            "Forecast Pack",
            "Builds the quarterly forecast pack from the ledgers",
            &["records.read", "files.write"][..],
        ),
    ];
    let groups = ["ap-ops", "finance", "legal-ops", "everyone"];
    let mut draw = Draw(7);
    for n in 1..count {
        let (id, name, summary, actions) = topics[n % topics.len()];
        let round = n / topics.len();
        let (id, name) = if round == 0 {
            (id.to_owned(), name.to_owned())
        } else {
            (
                format!("{id}-{}", round + 1),
                format!("{name} {}", round + 1),
            )
        };
        let publisher = if n % 3 == 0 {
            "Acme IT"
        } else {
            "Northwind Software"
        };
        let mut it = item(
            &id,
            kinds[n % kinds.len()],
            &name,
            publisher,
            summary,
            actions,
        );
        it.version = format!("{}.{}.0", 1 + n % 3, n % 7);
        let review = match n % 17 {
            5 | 11 => Review::Waiting,
            13 => Review::Blocked,
            _ => Review::Approved,
        };
        if review == Review::Approved {
            let group = groups[usize::try_from(draw.next(4)).unwrap_or(0)];
            let mode = if draw.next(4) == 0 {
                InstallMode::Required
            } else {
                InstallMode::Available
            };
            it.groups.insert(group.to_owned(), mode);
        }
        items.push(ConsoleItem {
            item: it,
            review,
            requested_by: ["AP Operations", "Finance", "Legal Operations"][n % 3].into(),
            checks: vec!["Malware scan clean".into(), "No critical CVEs".into()],
            decided_by: (review != Review::Waiting).then(|| "pat".to_owned()),
            shared: None,
            approved: None,
        });
    }
    items
}

/// Fills a fresh console: the AP and Legal groups' rules in policy (as a
/// published version every ring runs), the catalog and the fleet.
pub fn fill(
    state: &mut State,
    dir: &LocalDirectory,
    devices: usize,
    items: usize,
) -> anyhow::Result<()> {
    if !state.fleet.devices.is_empty() || !state.catalog.items.is_empty() {
        return Ok(());
    }
    // Policy: AP runs the invoice agent and the user's harness; Legal the
    // harness; sends ask for AP.
    let mut doc = state.draft()?;
    for (name, agents) in [
        ("ap-ops", &["invoice-demo", "claude-code"][..]),
        ("legal-ops", &["claude-code"][..]),
    ] {
        doc.groups.push(glass_policy::document::GroupPolicy {
            name: name.into(),
            members: Vec::new(),
            agents: agents.iter().map(|a| a.parse().expect("valid")).collect(),
            apps: Vec::new(),
            max_label: None,
            tiers: Default::default(),
            models: None,
            settings: Default::default(),
        });
    }
    state
        .save_draft(&doc)
        .map_err(|e| anyhow::anyhow!("fixture policy: {e:?}"))?;
    let version = state
        .publish(dir, "AP and Legal get their agents", "pat")
        .map_err(|e| anyhow::anyhow!("fixture policy: {e:?}"))?;
    state
        .promote(Ring::Early)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    state
        .promote(Ring::Broad)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    for item in catalog(items) {
        state
            .submit(item)
            .map_err(|e| anyhow::anyhow!("fixture catalog: {e:?}"))?;
    }
    let catalog_version = state
        .release_catalog()
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let mut draw = Draw(42);
    let now = Timestamp::now();
    let agents = ["invoice-demo", "claude-code", "it-agent"];
    for n in 0..devices {
        let user = format!("u{:04}", n + 1);
        let id = format!("NW-LT-{:04}", n + 1);
        let ring = match n % 20 {
            0 => Ring::Pilot,
            1..=4 => Ring::Early,
            _ => Ring::Broad,
        };
        let running: Vec<RunningAgent> = (0..draw.next(3))
            .map(|_| RunningAgent {
                agent: agents[usize::try_from(draw.next(3)).unwrap_or(0)].into(),
                user: user.clone(),
                waiting: draw.next(6) == 0,
            })
            .collect();
        let blocked = u32::try_from(draw.next(10).saturating_sub(7)).unwrap_or(0);
        state.fleet.devices.insert(
            id.clone(),
            Device {
                id: id.clone(),
                ring,
                kind: None,
                enrolled: now,
                last_seen: now,
                key: None,
                revoked: None,
                report: Some(DeviceReport {
                    device: id.clone(),
                    name: id,
                    os_version: format!("Glasshouse {}", env!("CARGO_PKG_VERSION")),
                    users: vec![user],
                    policy_version: Some(version),
                    catalog_version: Some(catalog_version),
                    directory_version: None,
                    approvals_pending: u32::try_from(running.iter().filter(|a| a.waiting).count())
                        .unwrap_or(0),
                    agents: running,
                    blocked_24h: blocked,
                    last_error: None,
                    made_here: Vec::new(),
                    reported_at: now,
                }),
            },
        );
    }
    state.fleet_dirty = true;
    state.save_fleet()
}
