//! What the console's pages show, as JSON: one view per page, computed
//! from the state and the directory on each request.

use std::collections::BTreeSet;

use glass_managed::{CatalogItem, InstallMode, Ring};
use glass_policy::ActionPattern;
use glass_policy::document::{PolicyDocument, SupportRole, SupportTool};
use glass_types::{Label, Tier, Timestamp};
use serde_json::{Value, json};

use crate::access::Access;
use crate::directory::{ADMINS, Directory, EVERYONE};
use crate::impact::Titles;
use crate::state::{ConsoleItem, Review, State};

/// The approval tiers IT sets, by action class (AD-2): each row sets the
/// tier of every pattern in it.
pub const CLASSES: &[(&str, &str, &[&str])] = &[
    (
        "read",
        "Read files, mail and records in scope",
        &[
            "files.list",
            "files.read",
            "mail.list",
            "mail.read",
            "calendar.list",
            "calendar.read",
        ],
    ),
    (
        "drafts",
        "Save drafts and calendar holds",
        &["mail.draft", "calendar.hold", "records.save_draft"],
    ),
    (
        "change",
        "Change, move or delete files",
        &["files.write", "files.move", "files.delete"],
    ),
    (
        "send",
        "Send email and invitations",
        &["mail.send", "calendar.invite"],
    ),
    ("submit", "Post or submit invoices", &["records.submit"]),
    (
        "payments",
        "Payments and vendor bank details",
        &["payments.*", "vendors.bank_details.*"],
    ),
    (
        "export",
        "Export more than 500 records",
        &["data.export_bulk"],
    ),
];

/// A device answered within this long: it's online.
pub const ONLINE_SECS: i64 = 120;

fn tier_name(t: Tier) -> &'static str {
    match t {
        Tier::Auto => "auto",
        Tier::Notify => "notify",
        Tier::Ask => "ask",
        Tier::Never => "never",
    }
}

fn group_json(directory: &dyn Directory, name: &str) -> Value {
    let group = directory.group(name);
    json!({
        "name": name,
        "display": directory.group_display(name),
        "source": group.map(|g| g.source.clone()).unwrap_or_default(),
        "users": directory.members(name).len(),
    })
}

/// Every group IT can set policy for: everyone, then the directory's.
pub fn groups(directory: &dyn Directory) -> Vec<Value> {
    std::iter::once(EVERYONE)
        .chain(directory.groups().iter().map(|g| g.name.as_str()))
        .map(|g| group_json(directory, g))
        .collect()
}

/// A class's tier for `group`: the group's own rule if it has one, else
/// the org's; the strictest across the class's patterns.
fn class_tier(doc: &PolicyDocument, group: &str, patterns: &[&str]) -> (Option<Tier>, bool) {
    let rules = doc.groups.iter().find(|g| g.name == group);
    let mut strictest: Option<Tier> = None;
    let mut inherited = true;
    for p in patterns {
        let Ok(pattern) = p.parse::<ActionPattern>() else {
            continue;
        };
        let own = rules.and_then(|g| g.tiers.get(&pattern)).copied();
        if own.is_some() {
            inherited = false;
        }
        if let Some(t) = own.or_else(|| doc.org.tiers.get(&pattern).copied()) {
            strictest = Some(strictest.map_or(t, |s| s.max(t)));
        }
    }
    (strictest, inherited)
}

/// Where each label's data may go (DP-2), in words.
fn routes(doc: &PolicyDocument) -> Vec<Value> {
    let name = |id: &str| {
        doc.models
            .models
            .iter()
            .find(|m| m.id == id)
            .map_or_else(|| id.to_owned(), |m| m.name.clone())
    };
    Label::ALL
        .iter()
        .map(|label| {
            let route = if *label == Label::Restricted {
                "Blocked".to_owned()
            } else if let Some(route) = doc.models.routes.get(label).filter(|r| !r.is_empty()) {
                route
                    .iter()
                    .map(|m| name(m))
                    .collect::<Vec<_>>()
                    .join(" or ")
            } else if let Some(default) = doc
                .models
                .default
                .as_deref()
                .filter(|_| *label <= Label::Internal)
            {
                name(default)
            } else {
                "No model approved".to_owned()
            };
            json!({ "label": label, "route": route })
        })
        .collect()
}

/// The live panel for a group (AD-4): from its members' devices' reports.
pub fn live(state: &State, directory: &dyn Directory, group: &str) -> Value {
    let members: BTreeSet<&str> = directory.members(group).into_iter().collect();
    let (mut running, mut waiting, mut blocked) = (0usize, 0usize, 0u64);
    for device in state.fleet.devices.values() {
        let Some(report) = &device.report else {
            continue;
        };
        let theirs: Vec<_> = report
            .agents
            .iter()
            .filter(|a| members.contains(a.user.as_str()))
            .collect();
        running += theirs.len();
        waiting += theirs.iter().filter(|a| a.waiting).count();
        if report.users.iter().any(|u| members.contains(u.as_str())) {
            blocked += u64::from(report.blocked_24h);
        }
    }
    let week = Timestamp::now().unix_seconds() - 7 * 24 * 60 * 60;
    let rollbacks = state
        .policy
        .versions
        .iter()
        .filter(|v| v.rolls_back_to.is_some() && v.at.unix_seconds() >= week)
        .count();
    json!({
        "group": directory.group_display(group),
        "running": running,
        "waiting": waiting,
        "blocked_24h": blocked,
        "rollbacks_7d": rollbacks,
    })
}

/// The agent policy page for `group` (AD-2, AD-3).
pub fn policy(
    state: &State,
    directory: &dyn Directory,
    titles: &Titles,
    group: &str,
) -> anyhow::Result<Value> {
    let doc = state.draft()?;
    let rules = doc.groups.iter().find(|g| g.name == group);
    let classes: Vec<Value> = CLASSES
        .iter()
        .map(|(id, label, patterns)| {
            let (tier, inherited) = class_tier(&doc, group, patterns);
            json!({
                "id": id,
                "label": label,
                "tier": tier.map(tier_name),
                "inherited": inherited,
                "actions": patterns.iter().filter_map(|p| p.parse::<ActionPattern>().ok()).map(|p| titles.of(&doc, &p)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let catalog_agent = |id: &str| -> Option<&CatalogItem> {
        state
            .catalog
            .items
            .iter()
            .map(|i| &i.item)
            .find(|i| i.id.as_str() == id)
    };
    let agents: Vec<Value> = doc
        .agents
        .iter()
        .map(|a| {
            let item = catalog_agent(a.id.as_str());
            json!({
                "id": a.id,
                "name": item.map_or_else(|| a.id.to_string(), |i| i.name.clone()),
                "version": item.map(|i| i.version.clone()),
                "allowed": rules.is_some_and(|g| g.agents.contains(&a.id)),
                "everyone": doc.groups.iter().any(|g| g.name == EVERYONE && g.agents.contains(&a.id)),
                "actions": a.allowed_actions.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "egress": a.egress,
                "max_label": a.max_label,
                "models": a.models,
            })
        })
        .collect();
    let pilot = state.in_force(Ring::Pilot).map(|v| v.text.clone());
    let draft_text = state.draft_for_devices(directory)?;
    let versions: Vec<Value> = state
        .policy
        .versions
        .iter()
        .rev()
        .take(12)
        .map(|v| {
            let rings: Vec<&str> = Ring::ALL
                .iter()
                .filter(|r| state.policy.rings.get(r) == Some(&v.version))
                .map(|r| r.as_str())
                .collect();
            json!({
                "version": v.version,
                "note": v.note,
                "by": directory.display_name(&v.by),
                "at": v.at,
                "rolls_back_to": v.rolls_back_to,
                "rings": rings,
            })
        })
        .collect();
    Ok(json!({
        "group": group_json(directory, group),
        "groups": groups(directory),
        "in_policy": rules.is_some(),
        "draft_version": state.latest_version() + 1,
        "draft_changed": pilot.as_deref() != Some(draft_text.as_str()),
        "classes": classes,
        "agents": agents,
        "apps": rules.map(|g| g.apps.clone()).unwrap_or_default(),
        "apps_on": rules.map(|g| g.apps_on.clone()).unwrap_or_default(),
        "apps_off": rules.map(|g| g.apps_off.clone()).unwrap_or_default(),
        "org_apps_off": doc.org.apps_off,
        "org_apps": doc
            .groups
            .iter()
            .find(|g| g.name == glass_policy::EVERYONE)
            .map(|g| g.apps.clone())
            .unwrap_or_default(),
        "max_label": rules.and_then(|g| g.max_label),
        "org_max_label": doc.org.max_label,
        "routes": routes(&doc),
        "versions": versions,
        "rings": state.policy.rings,
        "live": live(state, directory, group),
    }))
}

fn mode_users(directory: &dyn Directory, item: &CatalogItem) -> Vec<Value> {
    item.groups
        .iter()
        .map(|(group, mode)| {
            json!({
                "group": group,
                "display": directory.group_display(group),
                "users": directory.members(group).len(),
                "mode": mode,
            })
        })
        .collect()
}

/// A catalog item as the catalog page shows it (DV-3: what it declares).
/// The Mail page (NV-3): the org's mail service in IT's draft, and in the
/// version each ring runs.
pub fn mail(state: &State) -> anyhow::Result<Value> {
    let draft = state.draft()?;
    let rings: Vec<Value> = Ring::ALL
        .iter()
        .map(|&ring| {
            let version = state.in_force(ring);
            let mail = version
                .and_then(|v| toml::from_str::<PolicyDocument>(&v.text).ok())
                .and_then(|doc| doc.mail);
            json!({ "ring": ring, "version": version.map(|v| v.version), "mail": mail })
        })
        .collect();
    let changed = state.in_force(Ring::Pilot).is_none_or(|v| {
        toml::from_str::<PolicyDocument>(&v.text).map_or(true, |doc| doc.mail != draft.mail)
    });
    Ok(json!({
        "draft": draft.mail,
        "draft_version": state.latest_version() + 1,
        "changed": changed,
        "rings": rings,
        "people": state.people.released.as_ref().map(|s| s.version),
    }))
}

/// The Chat page (ADR 0030 §4): the compliance key every computer seals
/// to, by its fingerprint, and how long chat is kept (CF-61).
pub fn chat(state: &State) -> Value {
    let release = state.chat.release.as_ref();
    let fingerprint = release
        .and_then(|s| serde_json::from_str::<glass_managed::ChatRelease>(&s.payload).ok())
        .map(|r| r.fingerprint);
    json!({
        "version": release.map(|s| s.version),
        "fingerprint": fingerprint,
        "kept_for": state.chat.kept_for,
        "set": state.chat.kept_set,
        "max_days": glass_managed::chat::KeptFor::MAX_DAYS,
    })
}

pub fn item(state: &State, directory: &dyn Directory, titles: &Titles, id: &str) -> Option<Value> {
    let doc = state.draft().ok()?;
    let entry = state
        .catalog
        .items
        .iter()
        .find(|i| i.item.id.as_str() == id)?;
    Some(entry_view(
        entry,
        directory,
        titles,
        &doc,
        &patch_counts(state),
    ))
}

/// On how many computers each kernel patch is kept, being tried, or went
/// back (ADR 0057): what their reports say, by patch id.
fn patch_counts(state: &State) -> std::collections::BTreeMap<String, [u32; 3]> {
    let mut counts = std::collections::BTreeMap::<String, [u32; 3]>::new();
    for device in state.fleet.devices.values() {
        if device.revoked.is_some() {
            continue;
        }
        for p in device.report.iter().flat_map(|r| &r.patches) {
            let slot = match p.state.as_str() {
                "kept" => 0,
                "staging" | "staged" | "probation" => 1,
                "failed" | "rolled_back" => 2,
                _ => continue,
            };
            counts.entry(p.id.clone()).or_default()[slot] += 1;
        }
    }
    counts
}

/// One catalog item, with IT's draft (for the actions' titles) parsed once
/// by the caller: the catalog page shows hundreds.
fn entry_view(
    entry: &ConsoleItem,
    directory: &dyn Directory,
    titles: &Titles,
    doc: &PolicyDocument,
    patches: &std::collections::BTreeMap<String, [u32; 3]>,
) -> Value {
    let i = &entry.item;
    let actions: Vec<Value> = i
        .declares
        .actions
        .iter()
        .map(|a| {
            let title = a
                .parse::<ActionPattern>()
                .map_or_else(|_| a.clone(), |p| titles.of(doc, &p));
            json!({
                "action": a,
                "title": title,
                "tier": i.declares.default_tiers.get(a),
            })
        })
        .collect();
    json!({
        "id": i.id,
        "kind": i.kind,
        "kind_name": i.kind.as_str(),
        "name": i.name,
        "version": i.version,
        "publisher": i.publisher,
        "verified": i.publisher_verified,
        "internal": i.internal,
        "summary": i.summary,
        "review": entry.review,
        "requested_by": entry.requested_by,
        "decided_by": entry.decided_by.as_deref().map(|u| directory.display_name(u)),
        "checks": entry.checks,
        "pinned": i.pinned,
        "groups": mode_users(directory, i),
        "declares": {
            "actions": actions,
            "max_label": i.declares.max_label,
            "egress": i.declares.egress,
        },
        "public_store": i.public_store_check().err().map(|e| e.to_string()),
        "approved_version": entry.approved.as_ref().map(|a| &a.version),
        "shared": entry.shared.as_ref().map(|s| shared(s, directory, titles, doc, i)),
        "hardware": entry.hardware.as_ref().map(|h| hardware(h, directory, i, patches)),
    })
}

/// A kernel patch's review (ADR 0057, slice 3): who asked for it on which
/// computer, its diff, its build, and the computers that run it.
fn hardware(
    h: &crate::state::PatchShared,
    directory: &dyn Directory,
    item: &CatalogItem,
    patches: &std::collections::BTreeMap<String, [u32; 3]>,
) -> Value {
    let [kept, trying, went_back] = patches.get(&h.request.id).copied().unwrap_or_default();
    json!({
        "person": directory.display_name(&h.person),
        "person_login": h.person,
        "device": h.device,
        "computer": h.computer,
        "asked": h.asked,
        "patch": h.request.id,
        "release": h.request.release,
        "targets": h.request.targets,
        "modules": h.request.modules.iter().map(|m| &m.file).collect::<Vec<_>>(),
        "kernel": h.request.kernel,
        "origin": h.request.origin,
        "diff": h.request.diff,
        "build": h.build,
        "submitted_at": h.submitted_at,
        "bundle_sha256": item.patch.as_ref().map(|p| &p.bundle_sha256),
        "computers": { "kept": kept, "trying": trying, "went_back": went_back },
        "agent_review": h.agent_review.as_ref().map(|r| agent_review(r, directory)),
    })
}

/// An agent's review of a kernel patch, with who asked for it by name.
pub fn agent_review(review: &crate::hardware::AgentReview, directory: &dyn Directory) -> Value {
    let mut view = json!(review);
    view["by"] = json!(directory.display_name(&review.by));
    view
}

/// A shared app's review (ADR 0032, R2; GA-8): who made it from what, the
/// console's checks, what it may do beyond its base, and whether a person
/// must review it (DV-4).
fn shared(
    s: &crate::state::Shared,
    directory: &dyn Directory,
    titles: &Titles,
    doc: &PolicyDocument,
    item: &CatalogItem,
) -> Value {
    let titled = |actions: &[glass_types::ActionName]| -> Vec<Value> {
        actions
            .iter()
            .map(|a| {
                let title = a
                    .as_str()
                    .parse::<ActionPattern>()
                    .map_or_else(|_| a.to_string(), |p| titles.of(doc, &p));
                json!({ "action": a, "title": title })
            })
            .collect()
    };
    let (to, group) = match &s.share {
        glass_managed::studio::ShareTo::Team { group } => ("team", Some(group)),
        glass_managed::studio::ShareTo::Org => ("org", None),
    };
    json!({
        "maker": directory.display_name(&s.maker),
        "maker_login": s.maker,
        "maker_version": s.maker_version,
        "device": s.device,
        "to": to,
        "group": group,
        "group_name": group.map(|g| directory.group_display(g)),
        "note": s.note,
        "based_on": s.based_on.as_ref().map(|b| json!({ "id": b.id, "version": b.version })),
        "device_notes": s.device_notes,
        "checks": s.checks,
        "passed": s.checks.iter().all(|c| c.ok),
        "beyond_base": titled(&s.beyond_base),
        "irreversible": titled(&s.irreversible),
        "needs_person": !s.irreversible.is_empty(),
        "submitted_at": s.submitted_at,
        "bundle_sha256": item.app.as_ref().map(|a| &a.bundle_sha256),
    })
}

/// The catalog page (AD-5, DV-10, DV-11).
pub fn catalog(state: &State, directory: &dyn Directory, titles: &Titles) -> Value {
    // The draft parsed once for the page, not once an item.
    let patches = patch_counts(state);
    let items: Vec<Value> = state
        .draft()
        .map(|doc| {
            state
                .catalog
                .items
                .iter()
                .map(|entry| entry_view(entry, directory, titles, &doc, &patches))
                .collect()
        })
        .unwrap_or_default();
    let count = |r: Review| state.catalog.items.iter().filter(|i| i.review == r).count();
    json!({
        "version": state.catalog.released.as_ref().map(|s| s.version),
        "counts": {
            "waiting": count(Review::Waiting),
            "approved": count(Review::Approved),
            "blocked": count(Review::Blocked),
        },
        "groups": groups(directory),
        "modes": [InstallMode::Required, InstallMode::Available, InstallMode::Blocked],
        "items": items,
    })
}

/// The fleet page (AD-4; AD-10's enrollment, as a stub).
pub fn fleet(state: &State, directory: &dyn Directory, console_url: &str) -> Value {
    let now = Timestamp::now().unix_seconds();
    let mut totals = (0usize, 0usize, 0usize, 0u64);
    let mut users = BTreeSet::new();
    // Every app people made, on every computer (GA-10, ADR 0032): who made
    // it, from what, and what it does beyond that.
    let mut made: Vec<Value> = Vec::new();
    // Most recently heard from first: the devices IT is looking at now.
    let mut sorted: Vec<_> = state.fleet.devices.values().collect();
    sorted.sort_by(|a, b| {
        b.last_seen
            .unix_seconds()
            .cmp(&a.last_seen.unix_seconds())
            .then_with(|| a.id.cmp(&b.id))
    });
    let devices: Vec<Value> = sorted
        .into_iter()
        .map(|d| {
            // A removed computer is never counted online (ADR 0030 P).
            let online = d.revoked.is_none() && now - d.last_seen.unix_seconds() <= ONLINE_SECS;
            let r = d.report.as_ref();
            let agents = r.map_or(0, |r| r.agents.len());
            let waiting = r.map_or(0, |r| r.agents.iter().filter(|a| a.waiting).count());
            let blocked = r.map_or(0, |r| r.blocked_24h);
            totals.0 += usize::from(online);
            totals.1 += agents;
            totals.2 += waiting;
            totals.3 += u64::from(blocked);
            if let Some(r) = r {
                users.extend(r.users.iter().cloned());
                made.extend(r.made_here.iter().map(|m| {
                    json!({
                        "app": m.app,
                        "name": m.name,
                        "owner": directory.display_name(&m.owner),
                        "device": r.name,
                        "version": m.version,
                        "based_on": m.based_on,
                        "actions": m.actions,
                        "beyond_base": m.beyond_base,
                        "saved_at": m.saved_at,
                    })
                }));
            }
            json!({
                "id": d.id,
                "name": r.map_or_else(|| d.id.clone(), |r| r.name.clone()),
                "ring": d.ring,
                "kind": d.kind,
                "online": online,
                "last_seen": d.last_seen,
                "users": r.map(|r| r.users.iter().map(|u| directory.display_name(u)).collect::<Vec<_>>()).unwrap_or_default(),
                "os_version": r.map(|r| r.os_version.clone()),
                "policy_version": r.and_then(|r| r.policy_version),
                "catalog_version": r.and_then(|r| r.catalog_version),
                "agents": agents,
                "waiting": waiting,
                "blocked_24h": blocked,
                "last_error": r.and_then(|r| r.last_error.clone()),
                // ADR 0030 slice P: whether it signs with a key of its own,
                // and when IT removed it.
                "registered": d.key.is_some(),
                "removed": d.revoked,
            })
        })
        .collect();
    let rings: Vec<Value> = Ring::ALL
        .iter()
        .map(|ring| {
            json!({
                "ring": ring,
                "devices": state.fleet.devices.values().filter(|d| d.ring == *ring).count(),
                "policy_version": state.policy.rings.get(ring),
            })
        })
        .collect();
    // The kinds a device can be: those roles reach unattended, and any
    // a device already has.
    let mut kinds: BTreeSet<String> = state
        .support_in_force()
        .roles
        .into_iter()
        .flat_map(|r| r.unattended)
        .collect();
    kinds.extend(state.fleet.devices.values().filter_map(|d| d.kind.clone()));
    json!({
        "kinds": kinds,
        "totals": {
            "devices": devices.len(),
            "online": totals.0,
            "running": totals.1,
            "waiting": totals.2,
            "blocked_24h": totals.3,
            "users": users.len(),
        },
        "rings": rings,
        "catalog_version": state.catalog.released.as_ref().map(|s| s.version),
        "devices": devices,
        "made": made,
        "enroll": {
            "url": console_url,
            "org": state.org.id,
            "org_key": state.public_key(),
            "admins": directory.members(ADMINS).len(),
        },
    })
}

/// The Support page (CF-80, CF-81): the person's roles and tools, which
/// role has the tools they don't, and the computers their roles reach,
/// with only the people on them it reaches, most recently heard from
/// first. Seeing and controlling a screen come with sessions (ADR 0028,
/// slice b): every tool says it isn't ready yet. `None` if `user` holds no
/// role. Everything on it is from one read of the policy in force, the
/// one their access was decided by (SupportAccess.tla).
pub fn support(state: &State, directory: &dyn Directory, user: &str) -> Option<Value> {
    let support = state.support_in_force();
    let access = Access::of(user, directory, &support);
    if access.roles.is_empty() {
        return None;
    }
    let mine = access.tools();
    let tools: Vec<Value> = SupportTool::ALL
        .iter()
        .map(|&tool| {
            let roles: Vec<&str> = support
                .roles
                .iter()
                .filter(|r| r.tools.contains(&tool))
                .map(|r| r.name.as_str())
                .collect();
            json!({
                "id": tool,
                "title": tool.title(),
                "mine": mine.contains(&tool),
                "roles": roles,
                "ready": false,
            })
        })
        .collect();
    let everyone = access
        .roles
        .iter()
        .any(|r| r.people.iter().any(|g| g == EVERYONE));
    let reach: BTreeSet<&str> = access
        .roles
        .iter()
        .flat_map(|r| &r.people)
        .flat_map(|g| directory.members(g))
        .collect();
    let now = Timestamp::now().unix_seconds();
    let mut devices: Vec<_> = state
        .fleet
        .devices
        .values()
        .filter(|d| {
            everyone
                || d.report
                    .as_ref()
                    .is_some_and(|r| r.users.iter().any(|u| reach.contains(u.as_str())))
        })
        .collect();
    devices.sort_by(|a, b| {
        b.last_seen
            .unix_seconds()
            .cmp(&a.last_seen.unix_seconds())
            .then_with(|| a.id.cmp(&b.id))
    });
    let devices: Vec<Value> = devices
        .into_iter()
        .map(|d| {
            let r = d.report.as_ref();
            let unattended = d.kind.as_ref().is_some_and(|k| {
                access.roles.iter().any(|role| role.unattended.contains(k))
            });
            json!({
                "id": d.id,
                "name": r.map_or_else(|| d.id.clone(), |r| r.name.clone()),
                "kind": d.kind,
                "unattended": unattended,
                "online": now - d.last_seen.unix_seconds() <= ONLINE_SECS,
                "last_seen": d.last_seen,
                // Only the people the role may help: others on the same
                // computer aren't theirs to see (CF-81).
                "users": r.map(|r| r.users.iter().filter(|u| everyone || reach.contains(u.as_str())).map(|u| directory.display_name(u)).collect::<Vec<_>>()).unwrap_or_default(),
                "os_version": r.map(|r| r.os_version.clone()),
            })
        })
        .collect();
    let roles: Vec<Value> = access
        .roles
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "tools": r.tools,
                "people": r.people.iter().map(|g| directory.group_display(g)).collect::<Vec<_>>(),
                "unattended": r.unattended,
                "max_minutes": r.max_minutes,
            })
        })
        .collect();
    Some(json!({
        "roles": roles,
        "tools": tools,
        "devices": devices,
        "answer_within_secs": support.answer_within_secs,
        "policy_version": state.in_force(Ring::Pilot).map(|v| v.version),
    }))
}

/// The Remote help page (CF-81): the draft's support roles, who holds
/// each, and whether they differ from what the pilot ring runs.
pub fn roles(state: &State, directory: &dyn Directory) -> anyhow::Result<Value> {
    let draft = state.draft()?.support;
    let role = |r: &SupportRole| {
        let holders: BTreeSet<&str> = r.groups.iter().flat_map(|g| directory.members(g)).collect();
        json!({
            "id": r.id,
            "name": r.name,
            "groups": r.groups,
            "tools": r.tools,
            "people": r.people,
            "unattended": r.unattended,
            "max_minutes": r.max_minutes,
            "holders": holders.iter().take(12).map(|u| directory.display_name(u)).collect::<Vec<_>>(),
            "holder_count": holders.len(),
        })
    };
    let groups: Vec<Value> = std::iter::once(json!({ "name": EVERYONE, "display": "Everyone" }))
        .chain(
            directory
                .groups()
                .iter()
                .map(|g| json!({ "name": g.name, "display": directory.group_display(&g.name) })),
        )
        .collect();
    Ok(json!({
        "answer_within_secs": draft.answer_within_secs,
        "roles": draft.roles.iter().map(role).collect::<Vec<_>>(),
        "changed": draft != state.support_in_force(),
        "pilot_version": state.in_force(Ring::Pilot).map(|v| v.version),
        "tools": SupportTool::ALL.iter().map(|t| json!({ "id": t, "title": t.title() })).collect::<Vec<_>>(),
        "groups": groups,
    }))
}

/// The Settings page (UX-1): every device setting, what `group` sets in
/// the draft, what it would get otherwise, and what the pilot ring runs.
/// Groups higher in `order` win for people in two that disagree.
pub fn settings(state: &State, directory: &dyn Directory, group: &str) -> anyhow::Result<Value> {
    let draft = state.draft()?;
    let pilot = state
        .in_force(Ring::Pilot)
        .and_then(|v| toml::from_str::<PolicyDocument>(&v.text).ok());
    let of = |doc: &PolicyDocument, name: &str| {
        doc.groups
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.settings.clone())
            .unwrap_or_default()
    };
    let mine = of(&draft, group);
    let everyone = of(&draft, EVERYONE);
    let in_pilot = pilot.as_ref().map(|d| of(d, group)).unwrap_or_default();
    let defs: Vec<Value> = glass_policy::settings::table()
        .iter()
        .map(|def| {
            json!({
                "id": def.id,
                "section": def.section,
                "title": def.title,
                "help": def.help,
                "kind": def.kind,
                "scope": def.scope,
                "default": def.default,
                "set": mine.get(def.id),
                "everyone": (group != EVERYONE).then(|| everyone.get(def.id)).flatten(),
                "pilot": in_pilot.get(def.id),
            })
        })
        .collect();
    let order: Vec<&str> = draft
        .groups
        .iter()
        .map(|g| g.name.as_str())
        .filter(|n| *n != EVERYONE)
        .collect();
    // Everyone, then the policy's groups in order, then the directory's
    // other groups (setting one adds it to the policy, last).
    let mut names: Vec<&str> = vec![EVERYONE];
    names.extend(order.iter().copied());
    names.extend(
        directory
            .groups()
            .iter()
            .map(|g| g.name.as_str())
            .filter(|n| !names.contains(n) && *n != EVERYONE)
            .collect::<Vec<_>>(),
    );
    let groups: Vec<Value> = names
        .iter()
        .map(|n| {
            json!({
                "name": n,
                "display": directory.group_display(n),
                "people": directory.members(n).len(),
                "sets": of(&draft, n).len(),
            })
        })
        .collect();
    let changed = pilot.as_ref().is_none_or(|p| {
        let all = |d: &PolicyDocument| {
            d.groups
                .iter()
                .map(|g| (g.name.clone(), g.settings.clone()))
                .collect::<Vec<_>>()
        };
        all(p) != all(&draft)
    });
    Ok(json!({
        "group": { "name": group, "display": directory.group_display(group), "people": directory.members(group).len() },
        "groups": groups,
        "order": order,
        "settings": defs,
        "changed": changed,
        "pilot_version": state.in_force(Ring::Pilot).map(|v| v.version),
    }))
}
