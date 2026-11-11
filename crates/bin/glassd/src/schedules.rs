//! Where people's schedules are kept (ADR 0069): a DynamoDB table every
//! agent host shares, written by the stack (deploy/aws), or a file in
//! glassd's state on a laptop or development host.
//!
//! The table's key is the person's key in the store (`owner`) and the
//! schedule's `id`. A schedule that's on carries `queue = "due"`, which
//! puts it in the sparse index `due` by `next_run`: what every host asks
//! for every half minute. Each item keeps the whole schedule in `body`,
//! with its `version` beside it for the write that claims a run.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use glass_gateway_providers::dynamodb::{DynamoDb, DynamoError, Expression};
use glass_runtime::{ScheduleStore, StoreError, StoredSchedule};
use serde::Deserialize;
use serde_json::{Value, json};

/// The index of what's due, and its partition's one value.
const DUE_INDEX: &str = "due";
const QUEUE: &str = "due";

/// `[schedules]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulesConfig {
    pub store: SchedulesStoreKind,
    /// The DynamoDB table, for `store = "dynamodb"`.
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    /// Another DynamoDB endpoint: a VPC endpoint's, or a stand-in on this
    /// machine (deploy/local).
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SchedulesStoreKind {
    File,
    Dynamodb,
}

impl SchedulesConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.store == SchedulesStoreKind::Dynamodb
            && (self.table.is_none() || self.region.is_none())
        {
            anyhow::bail!("[schedules] store = \"dynamodb\" needs table and region");
        }
        Ok(())
    }
}

/// The store `settings` names: the file at `file` when there are none.
pub fn store(
    settings: Option<&SchedulesConfig>,
    file: std::path::PathBuf,
) -> anyhow::Result<Arc<dyn ScheduleStore>> {
    let Some(settings) = settings.filter(|s| s.store == SchedulesStoreKind::Dynamodb) else {
        return Ok(Arc::new(glass_runtime::FileSchedules::new(file)));
    };
    let credentials =
        glass_gateway_providers::s3::S3Credentials::from_host().map_err(anyhow::Error::msg)?;
    let client = DynamoDb::new(
        settings.region.as_deref().unwrap_or_default(),
        settings.endpoint.as_deref(),
        credentials,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(Arc::new(DynamoSchedules {
        client,
        table: settings.table.clone().unwrap_or_default(),
        unread: Unread::default(),
    }))
}

#[derive(Debug)]
pub struct DynamoSchedules {
    client: DynamoDb,
    table: String,
    unread: Unread,
}

/// The items this host couldn't read and has said so about: once each,
/// not every half minute.
#[derive(Debug, Default)]
struct Unread(Mutex<HashSet<String>>);

/// More than this many remembered, and they are forgotten (and said
/// again): the table is the stack's, not this host's to bound.
const UNREAD_REMEMBERED: usize = 1024;

impl Unread {
    /// The schedules among `items` this host can read.
    ///
    /// One it can't is left out and logged, where it used to fail the
    /// whole answer (audit 4.11). The table is shared by hosts that may be
    /// a version apart while an update goes round, and one schedule
    /// written by the newer (who it runs as, of a kind this host doesn't
    /// know) made "what is due" fail on every older host every half
    /// minute: they ran nothing, for anyone, and the person's own list
    /// failed with it. An item damaged by hand did the same for good.
    ///
    /// Left out, it is as it was: a write is an item's own, and only over
    /// a version this host read. A host that can read it runs it.
    fn readable(&self, items: &[Value]) -> Vec<StoredSchedule> {
        items
            .iter()
            .filter_map(|item| match from_item(item) {
                Ok(schedule) => Some(schedule),
                Err(why) => {
                    self.say_once(item, &why);
                    None
                }
            })
            .collect()
    }

    fn say_once(&self, item: &Value, why: &str) {
        let part = |name: &str, kind: &str| item[name][kind].as_str().unwrap_or("?").to_owned();
        let (owner, id, version) = (part("owner", "S"), part("id", "S"), part("version", "N"));
        let mut said = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if said.len() >= UNREAD_REMEMBERED {
            said.clear();
        }
        if said.insert(format!("{owner}/{id}/{version}")) {
            // Not its body: that is the person's own words.
            tracing::warn!(
                owner,
                schedule = id,
                version,
                "left a schedule out, this host can't read it (written by a newer one, or damaged): {why}"
            );
        }
    }
}

fn s(text: &str) -> Value {
    json!({ "S": text })
}

fn n(number: impl ToString) -> Value {
    json!({ "N": number.to_string() })
}

fn key(owner: &str, id: &str) -> Value {
    json!({ "owner": s(owner), "id": s(id) })
}

fn item(schedule: &StoredSchedule) -> Result<Value, String> {
    let body = serde_json::to_string(schedule).map_err(|e| e.to_string())?;
    let mut item = json!({
        "owner": s(&schedule.owner),
        "id": s(&schedule.id),
        "version": n(schedule.version),
        "next_run": n(schedule.next_run),
        "body": s(&body),
    });
    if schedule.queued() {
        item["queue"] = s(QUEUE);
    }
    Ok(item)
}

fn from_item(item: &Value) -> Result<StoredSchedule, String> {
    let body = item["body"]["S"]
        .as_str()
        .ok_or("a schedule in the table has no body")?;
    serde_json::from_str(body).map_err(|e| format!("a schedule in the table isn't readable: {e}"))
}

fn names(pairs: &[(&str, &str)]) -> Value {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Value::from(*v)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn changed(e: DynamoError) -> StoreError {
    match e {
        DynamoError::ConditionFailed => StoreError::Changed,
        DynamoError::Failed(why) => StoreError::Failed(why),
    }
}

impl ScheduleStore for DynamoSchedules {
    fn make(&self, schedule: &StoredSchedule) -> Result<(), StoreError> {
        let item = item(schedule).map_err(StoreError::Failed)?;
        self.client
            .put(
                &self.table,
                &item,
                &Expression {
                    text: "attribute_not_exists(#id)",
                    names: names(&[("#id", "id")]),
                    values: json!({}),
                },
            )
            .map_err(changed)
    }

    fn of(&self, owner: &str) -> Result<Vec<StoredSchedule>, String> {
        let items = self
            .client
            .query(
                &self.table,
                None,
                &Expression {
                    text: "#o = :o",
                    names: names(&[("#o", "owner")]),
                    values: json!({ ":o": s(owner) }),
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(self.unread.readable(&items))
    }

    fn replace(&self, schedule: &StoredSchedule, seen: u64) -> Result<(), StoreError> {
        let item = item(schedule).map_err(StoreError::Failed)?;
        self.client
            .put(
                &self.table,
                &item,
                &Expression {
                    text: "#v = :seen",
                    names: names(&[("#v", "version")]),
                    values: json!({ ":seen": n(seen) }),
                },
            )
            .map_err(changed)
    }

    fn remove(&self, owner: &str, id: &str) -> Result<bool, String> {
        match self.client.delete(
            &self.table,
            &key(owner, id),
            &Expression {
                text: "attribute_exists(#id)",
                names: names(&[("#id", "id")]),
                values: json!({}),
            },
        ) {
            Ok(()) => Ok(true),
            Err(DynamoError::ConditionFailed) => Ok(false),
            Err(DynamoError::Failed(why)) => Err(why),
        }
    }

    fn due(&self, now: i64) -> Result<Vec<StoredSchedule>, String> {
        let items = self
            .client
            .query(
                &self.table,
                Some(DUE_INDEX),
                &Expression {
                    text: "#q = :q AND next_run <= :now",
                    names: names(&[("#q", "queue")]),
                    values: json!({ ":q": s(QUEUE), ":now": n(now) }),
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(self.unread.readable(&items))
    }
}

/// Starts what's due every half minute, as long as glassd runs.
pub fn run(runtime: Arc<glass_runtime::Runtime>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            let runtime = Arc::clone(&runtime);
            let turns = tokio::task::spawn_blocking(move || {
                runtime.run_due_schedules(glass_types::Timestamp::now().unix_seconds())
            })
            .await
            .unwrap_or_default();
            for (schedule, turn) in turns {
                match turn {
                    glass_runtime::Turn::Started(task) => {
                        tracing::info!(schedule, %task, "started a scheduled run");
                    }
                    glass_runtime::Turn::Lost => {
                        tracing::debug!(schedule, "another host claimed the run");
                    }
                    glass_runtime::Turn::Refused(why) => {
                        tracing::warn!(schedule, "a scheduled run couldn't start: {why}");
                    }
                    other => tracing::info!(schedule, ?other, "a schedule's slot passed"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(enabled: bool) -> StoredSchedule {
        StoredSchedule {
            owner: "p0123".into(),
            id: "s0a1b2c3d4e5f".into(),
            version: 3,
            holder: glass_runtime::Holder::Person {
                sub: "sub-1".into(),
                name: "Ana".into(),
                email: "ana@acme.example".into(),
            },
            persona: "reconciler".into(),
            prompt: "Match the invoices".into(),
            when: "weekdays 09:00".into(),
            zone: "Europe/London".into(),
            enabled,
            disabled_why: None,
            next_run: 1_793_178_000,
            failures: 0,
            last: None,
            created_at: glass_types::Timestamp::now(),
            other: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn adr0069_only_a_schedule_thats_on_is_in_the_due_index() {
        let kept = schedule(true);
        let on = item(&kept).unwrap();
        assert_eq!(on["queue"]["S"], "due");
        assert_eq!(on["next_run"]["N"], "1793178000");
        assert_eq!(on["version"]["N"], "3");
        assert_eq!(from_item(&on).unwrap(), kept);
        let off = item(&schedule(false)).unwrap();
        assert!(off.get("queue").is_none(), "{off}");
        assert!(from_item(&json!({ "owner": s("p") })).is_err());
    }

    /// `made`'s body with who it runs as of a kind this version doesn't
    /// know, as a host a version on might write it.
    fn from_a_newer_host(made: &StoredSchedule, id: &str) -> String {
        let mut body = serde_json::to_value(made).unwrap();
        body["id"] = id.into();
        body["holder"] = json!({ "kind": "team", "team": "finance" });
        body.to_string()
    }

    // Audit 4.11: one item this host can't read failed the whole answer,
    // so "what is due" failed every half minute and the host ran nothing.
    #[test]
    fn a_schedule_this_host_cant_read_is_left_out_and_the_rest_are_answered() {
        let kept = schedule(true);
        let mut other = schedule(true);
        other.id = "s111111111111".into();
        let mut newer = item(&kept).unwrap();
        newer["id"] = s("s222222222222");
        newer["body"] = s(&from_a_newer_host(&kept, "s222222222222"));
        let no_body = json!({ "owner": s("p0123"), "id": s("s333333333333") });
        let items = [
            item(&kept).unwrap(),
            newer.clone(),
            no_body,
            item(&other).unwrap(),
        ];
        let unread = Unread::default();
        assert_eq!(unread.readable(&items), vec![kept.clone(), other.clone()]);
        // Said once each, however often it is asked: every half minute.
        let said = || {
            let mut said: Vec<String> = unread.0.lock().unwrap().iter().cloned().collect();
            said.sort();
            said
        };
        let once = said();
        assert_eq!(
            once,
            ["p0123/s222222222222/3", "p0123/s333333333333/?"],
            "by owner, id and version; never by what the schedule says"
        );
        assert_eq!(unread.readable(&items).len(), 2);
        assert_eq!(said(), once);
        // A version on, it is said again: it may be readable now, or not.
        newer["version"] = n(4);
        unread.readable(&[newer]);
        assert_eq!(said().len(), 3);
        // What is remembered has an end.
        for i in 0..UNREAD_REMEMBERED + 5 {
            unread.readable(&[json!({ "owner": s("p"), "id": s(&format!("s{i}")) })]);
        }
        assert!(unread.0.lock().unwrap().len() <= UNREAD_REMEMBERED);
    }

    #[test]
    fn adr0069_dynamodb_needs_a_table_and_a_region() {
        let parse = |text: &str| toml::from_str::<SchedulesConfig>(text).unwrap();
        assert!(parse("store = \"dynamodb\"").validate().is_err());
        assert!(
            parse("store = \"dynamodb\"\ntable = \"t\"\nregion = \"us-east-1\"")
                .validate()
                .is_ok()
        );
        assert!(parse("store = \"file\"").validate().is_ok());
        assert!(toml::from_str::<SchedulesConfig>("store = \"redis\"").is_err());
    }

    /// Against the local AWS simulation's DynamoDB (deploy/local): two
    /// hosts claim one run; one holds.
    #[test]
    #[ignore = "needs `make aws-local`"]
    fn adr0069_local_aws_one_claim_holds() {
        let Ok(endpoint) = std::env::var("GLASS_LOCAL_DYNAMODB") else {
            panic!("GLASS_LOCAL_DYNAMODB isn't set: run `make aws-local` first");
        };
        let table = std::env::var("GLASS_LOCAL_SCHEDULES_TABLE")
            .unwrap_or_else(|_| "glasshouse-agents-schedules".into());
        let region = std::env::var("GLASS_LOCAL_REGION").unwrap_or_else(|_| "us-east-1".into());
        let host = || DynamoSchedules {
            client: DynamoDb::new(
                &region,
                Some(&endpoint),
                glass_gateway_providers::s3::S3Credentials::new("local", "local", None),
            )
            .unwrap(),
            table: table.clone(),
            unread: Unread::default(),
        };
        let (a, b) = (host(), host());
        let mut made = schedule(true);
        made.id = format!("s{:012x}", rand_id());
        made.version = 1;
        // With something a version on might add to it.
        made.other.insert("catch_up".into(), true.into());
        a.make(&made).unwrap();
        assert_eq!(
            b.make(&made),
            Err(StoreError::Changed),
            "an id is taken once"
        );
        // Audit 4.11: beside it, the same person's schedule as a newer
        // host might write it, due too. This host can't read who it runs
        // as; it answers with the one it can, and the other is untouched.
        let mut newer = item(&made).unwrap();
        let newer_id = format!("s{:012x}", rand_id() ^ 0xffff);
        newer["id"] = s(&newer_id);
        newer["body"] = s(&from_a_newer_host(&made, &newer_id));
        let unconditional = Expression {
            text: "attribute_not_exists(#id)",
            names: names(&[("#id", "id")]),
            values: json!({}),
        };
        a.client.put(&table, &newer, &unconditional).unwrap();
        let mine = a.of(&made.owner).unwrap();
        assert!(mine.iter().any(|s| s.id == made.id), "{mine:?}");
        assert!(!mine.iter().any(|s| s.id == newer_id));
        let due = b.due(made.next_run).unwrap();
        assert!(due.iter().any(|s| s.id == made.id), "{due:?}");
        assert!(!due.iter().any(|s| s.id == newer_id));
        let kept = a.client.get(&table, &key(&made.owner, &newer_id)).unwrap();
        assert_eq!(kept.as_ref(), Some(&newer), "as the newer host wrote it");
        assert!(a.remove(&made.owner, &newer_id).unwrap());
        assert!(a.of(&made.owner).unwrap().iter().any(|s| s.id == made.id));
        let due = b.due(made.next_run).unwrap();
        assert!(due.iter().any(|s| s.id == made.id), "{due:?}");
        // The claim is what a host read, a version on: with it goes what
        // this version doesn't know (audit 4.11).
        let mut claim = due.into_iter().find(|s| s.id == made.id).unwrap();
        claim.version = 2;
        claim.next_run += 86_400;
        a.replace(&claim, 1).unwrap();
        assert_eq!(
            b.replace(&claim, 1),
            Err(StoreError::Changed),
            "one claim holds"
        );
        let raw = a.client.get(&table, &key(&made.owner, &made.id)).unwrap();
        let body: Value =
            serde_json::from_str(raw.unwrap()["body"]["S"].as_str().unwrap()).unwrap();
        assert_eq!(
            (&body["version"], &body["catch_up"]),
            (&json!(2), &json!(true))
        );
        let mut off = claim.clone();
        off.version = 3;
        off.enabled = false;
        a.replace(&off, 2).unwrap();
        let due = a.due(i64::MAX / 2).unwrap();
        assert!(
            !due.iter().any(|s| s.id == made.id),
            "off, it leaves the index"
        );
        assert!(a.remove(&made.owner, &made.id).unwrap());
        assert!(!b.remove(&made.owner, &made.id).unwrap());
    }

    fn rand_id() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| {
                u64::try_from(d.as_nanos() & 0xffff_ffff_ffff).unwrap_or(0)
            })
    }
}
