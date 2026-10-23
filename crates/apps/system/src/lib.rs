//! The `system` app: what the IT self-service agent may check and fix on
//! this computer (AE-4).
//!
//! These are the dedicated broker role the PRD asks for: fixed-function
//! operations glassd runs with its own privileges on the user's behalf. An
//! agent can't name a path or a command, only pick an action, and policy
//! allows `system.*` only for the IT agent. Checks read; fixes come from
//! IT's approved list, run through the same tiers, snapshots and audit as
//! every action, and stay undoable where they change anything. Security
//! settings are not here at all, and policy makes `security.*` Never.

use std::fmt;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use glass_api::ChangeKind;
use glass_platform::fs::ConfinedDir;
use glass_platform::system::{self, FsUsage, Load, NetworkStatus, Process};
use glass_types::{ActionName, SideEffect, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, TaskContext, parse_input,
};

/// The user's cache folder: safe to empty, and it grows quietly.
const CACHE: &str = ".cache";
/// A cache this big is worth offering to clean.
const CACHE_WORTH_CLEANING: u64 = 1024 * 1024;
const DISK_NEARLY_FULL: u64 = 90;

/// The machine, as the system actions see it. Production uses
/// [`HostSystem`]; tests substitute their own.
pub trait SystemOps: Send + Sync + fmt::Debug {
    fn network(&self) -> Result<NetworkStatus, String>;
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String>;
    fn filesystem_usage(&self, path: &Path) -> Result<FsUsage, String>;
    fn load(&self) -> Result<Load, String>;
    fn top_processes(&self, uid: u32) -> Result<Vec<Process>, String>;
    fn service_enabled(&self, name: &str) -> bool;
    fn restart_service(&self, name: &str) -> Result<(), String>;
}

/// This computer, through `glass_platform::system`.
#[derive(Debug, Default)]
pub struct HostSystem;

impl SystemOps for HostSystem {
    fn network(&self) -> Result<NetworkStatus, String> {
        system::network().map_err(|e| e.to_string())
    }
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        system::resolve(host, Duration::from_secs(3))
    }
    fn filesystem_usage(&self, path: &Path) -> Result<FsUsage, String> {
        system::filesystem_usage(path).map_err(|e| e.to_string())
    }
    fn load(&self) -> Result<Load, String> {
        system::load().map_err(|e| e.to_string())
    }
    fn top_processes(&self, uid: u32) -> Result<Vec<Process>, String> {
        system::top_processes(uid, 5).map_err(|e| e.to_string())
    }
    fn service_enabled(&self, name: &str) -> bool {
        system::service_enabled(name)
    }
    fn restart_service(&self, name: &str) -> Result<(), String> {
        system::restart_service(name).map_err(|e| e.to_string())
    }
}

#[derive(Debug)]
pub struct SystemProvider {
    ops: Arc<dyn SystemOps>,
    /// Services IT lets an agent restart (policy `[fixes] services`).
    services: Vec<String>,
    /// A name the network check looks up to test DNS.
    probe_host: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceInput {
    service: String,
}

impl SystemProvider {
    pub fn new(ops: Arc<dyn SystemOps>, services: Vec<String>) -> Self {
        Self {
            ops,
            services,
            probe_host: "example.com".into(),
        }
    }

    fn home(ctx: &TaskContext) -> Result<ConfinedDir, ActionError> {
        ConfinedDir::open(&ctx.home).map_err(|e| ActionError::io("home folder", e))
    }

    /// The cache's size in bytes and files; zero when there is none (or it
    /// is a symlink, which the cleanup won't touch).
    fn cache_usage(home: &ConfinedDir) -> Result<(u64, u64), ActionError> {
        if !home.is_real_dir(Path::new(CACHE)) {
            return Ok((0, 0));
        }
        home.disk_usage(Path::new(CACHE))
            .map_err(|e| ActionError::io("cache folder", e))
    }

    fn network_check(&self) -> Result<ActionOutput, ActionError> {
        let net = self.ops.network().map_err(ActionError::Failed)?;
        let dns = self.ops.resolve(&self.probe_host);
        let mut findings = Vec::new();
        let up: Vec<_> = net.interfaces.iter().filter(|i| i.up).collect();
        if up.is_empty() {
            findings.push("No network connection: every network interface is down".to_owned());
        } else if up.iter().all(|i| i.addresses.is_empty()) {
            findings.push("Connected, but the network gave this computer no address".to_owned());
        }
        if net.gateway.is_none() {
            findings.push("No route to the internet (no default gateway)".to_owned());
        }
        if let Err(e) = &dns {
            findings.push(format!("Names don't resolve: DNS isn't answering ({e})"));
        }
        let text = if findings.is_empty() {
            let first = up.first().map_or(String::new(), |i| {
                format!(
                    "{} {}",
                    i.name,
                    i.addresses.first().map_or("", String::as_str)
                )
            });
            format!(
                "Network looks healthy: {first}, gateway {}, DNS answering",
                net.gateway.as_deref().unwrap_or("?")
            )
        } else {
            findings.join(". ")
        };
        Ok(ActionOutput {
            text,
            data: json!({
                "healthy": findings.is_empty(),
                "findings": findings,
                "interfaces": net.interfaces,
                "gateway": net.gateway,
                "nameservers": net.nameservers,
                "dns": match dns {
                    Ok(addrs) => json!({ "host": self.probe_host, "ok": true, "addresses": addrs }),
                    Err(e) => json!({ "host": self.probe_host, "ok": false, "error": e }),
                },
            }),
            changes: Vec::new(),
        })
    }

    fn storage_check(&self, ctx: &TaskContext) -> Result<ActionOutput, ActionError> {
        let usage = self
            .ops
            .filesystem_usage(&ctx.home)
            .map_err(ActionError::Failed)?;
        let (cache_bytes, cache_files) = Self::cache_usage(&Self::home(ctx)?)?;
        let mut findings = Vec::new();
        if usage.percent_used() >= DISK_NEARLY_FULL {
            findings.push(format!("Your disk is {}% full", usage.percent_used()));
        }
        if cache_bytes >= CACHE_WORTH_CLEANING {
            findings.push(format!(
                "{} of cache files can be cleaned up",
                human(cache_bytes)
            ));
        }
        let text = format!(
            "{}% of your disk is used, {} free{}",
            usage.percent_used(),
            human(usage.available_kb * 1024),
            if findings.is_empty() {
                String::new()
            } else {
                format!(". {}", findings.join(". "))
            }
        );
        Ok(ActionOutput {
            text,
            data: json!({
                "healthy": findings.is_empty(),
                "findings": findings,
                "percent_used": usage.percent_used(),
                "free_bytes": usage.available_kb * 1024,
                "cache_bytes": cache_bytes,
                "cache_files": cache_files,
            }),
            changes: Vec::new(),
        })
    }

    fn performance_check(&self, ctx: &TaskContext) -> Result<ActionOutput, ActionError> {
        let load = self.ops.load().map_err(ActionError::Failed)?;
        let top = self
            .ops
            .top_processes(ctx.owner.uid)
            .map_err(ActionError::Failed)?;
        let mut findings = Vec::new();
        if load.average[0] > f64::from(load.cpus) * 1.5 {
            findings.push(format!(
                "The computer is overloaded: load {:.1} on {} CPUs",
                load.average[0], load.cpus
            ));
        }
        if load.memory_total_mb > 0 && load.memory_free_mb * 10 < load.memory_total_mb {
            findings.push(format!(
                "Memory is nearly full: {} MB free of {} MB",
                load.memory_free_mb, load.memory_total_mb
            ));
        }
        if let Some(busiest) = top.iter().find(|p| p.cpu_percent >= 50.0) {
            findings.push(format!(
                "{} is using {:.0}% CPU",
                busiest.command, busiest.cpu_percent
            ));
        }
        let text = if findings.is_empty() {
            format!(
                "Nothing is slowing the computer down: load {:.1} on {} CPUs, {} MB memory free",
                load.average[0], load.cpus, load.memory_free_mb
            )
        } else {
            findings.join(". ")
        };
        Ok(ActionOutput {
            text,
            data: json!({
                "healthy": findings.is_empty(),
                "findings": findings,
                "load": load,
                "top_processes": top,
            }),
            changes: Vec::new(),
        })
    }

    fn set_up(&self, service: &str) -> Result<(), ActionError> {
        if self.ops.service_enabled(service) {
            Ok(())
        } else {
            Err(ActionError::Failed(format!(
                "{service} isn't set up on this computer"
            )))
        }
    }

    fn approved(&self, service: &str) -> Result<(), ActionError> {
        if self.services.iter().any(|s| s == service) {
            Ok(())
        } else {
            Err(ActionError::OutOfScope(format!(
                "{service} isn't on IT's list of services an agent may restart ({})",
                self.services.join(", ")
            )))
        }
    }
}

impl ActionProvider for SystemProvider {
    fn actions(&self) -> Vec<ActionDef> {
        let none = json!({ "type": "object", "properties": {}, "additionalProperties": false });
        let def =
            |name: &str, title: &str, description: &str, side_effect, tier, schema: &Value| {
                ActionDef {
                    name: name.parse().expect("static name"),
                    title: title.into(),
                    description: description.into(),
                    input_schema: schema.clone(),
                    side_effect,
                    default_tier: tier,
                    ask: Default::default(),
                }
            };
        vec![
            def(
                "system.network.check",
                "Check the network",
                "Check network interfaces, the route to the internet and DNS. Reads only.",
                SideEffect::None,
                Tier::Auto,
                &none,
            ),
            def(
                "system.storage.check",
                "Check storage",
                "Check free disk space and how much cache could be cleaned up. Reads only.",
                SideEffect::None,
                Tier::Auto,
                &none,
            ),
            def(
                "system.performance.check",
                "Check what's slowing things down",
                "Check load, memory and the user's busiest programs. Reads only.",
                SideEffect::None,
                Tier::Auto,
                &none,
            ),
            def(
                "system.storage.clean_cache",
                "Clean up cache files",
                "Empty the user's cache folder (~/.cache) to free space. Undoable: the folder \
                 is snapshotted first.",
                SideEffect::LocalReversible,
                Tier::Notify,
                &none,
            )
            .asking("Review cleanup", "Approve and clean up"),
            def(
                "system.service.restart",
                "Restart a service from IT's list",
                "Restart a system service, such as the VPN client. Only services on IT's \
                 approved list.",
                SideEffect::LocalReversible,
                Tier::Ask,
                &json!({
                    "type": "object",
                    "properties": { "service": { "type": "string", "enum": self.services } },
                    "required": ["service"],
                    "additionalProperties": false
                }),
            )
            .asking("Review details", "Approve and restart"),
        ]
    }

    fn returns(&self, action: &ActionName) -> Option<Value> {
        let strings = json!({ "type": "array", "items": { "type": "string" } });
        // Every check says whether all is well and, in plain words, what isn't.
        let check = |extra: Value, required: &[&str]| {
            let mut properties = json!({
                "healthy": { "type": "boolean", "description": "true when nothing was found wrong" },
                "findings": { "type": "array", "items": { "type": "string" }, "description": "what's wrong, in plain words; empty when healthy" }
            });
            if let (Some(all), Some(extra)) = (properties.as_object_mut(), extra.as_object()) {
                all.extend(extra.clone());
            }
            let mut required: Vec<&str> = required.to_vec();
            required.extend(["healthy", "findings"]);
            json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            })
        };
        Some(match action.as_str() {
            "system.network.check" => check(
                json!({
                    "interfaces": {
                        "type": "array",
                        "description": "network interfaces other than loopback",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "up": { "type": "boolean" },
                                "addresses": { "type": "array", "items": { "type": "string" }, "description": "IP addresses" }
                            },
                            "required": ["name", "up", "addresses"],
                            "additionalProperties": false
                        }
                    },
                    "gateway": { "type": ["string", "null"], "description": "the default gateway's address; null when there is none" },
                    "nameservers": strings,
                    "dns": {
                        "type": "object",
                        "description": "looking up a well-known name",
                        "properties": {
                            "host": { "type": "string", "description": "the name looked up" },
                            "ok": { "type": "boolean" },
                            "addresses": { "type": "array", "items": { "type": "string" }, "description": "what it resolved to, when ok" },
                            "error": { "type": "string", "description": "why it didn't resolve, when not ok" }
                        },
                        "required": ["host", "ok"],
                        "additionalProperties": false
                    }
                }),
                &["interfaces", "gateway", "nameservers", "dns"],
            ),
            "system.storage.check" => check(
                json!({
                    "percent_used": { "type": "integer", "description": "of the disk the home folder is on" },
                    "free_bytes": { "type": "integer" },
                    "cache_bytes": { "type": "integer", "description": "what cleaning the cache would free" },
                    "cache_files": { "type": "integer" }
                }),
                &["percent_used", "free_bytes", "cache_bytes", "cache_files"],
            ),
            "system.performance.check" => check(
                json!({
                    "load": {
                        "type": "object",
                        "properties": {
                            "average": { "type": "array", "items": { "type": "number" }, "description": "the 1, 5 and 15 minute load averages" },
                            "cpus": { "type": "integer" },
                            "memory_total_mb": { "type": "integer" },
                            "memory_free_mb": { "type": "integer" }
                        },
                        "required": ["average", "cpus", "memory_total_mb", "memory_free_mb"],
                        "additionalProperties": false
                    },
                    "top_processes": {
                        "type": "array",
                        "description": "the person's busiest programs, busiest first",
                        "items": {
                            "type": "object",
                            "properties": {
                                "pid": { "type": "integer" },
                                "cpu_percent": { "type": "number" },
                                "memory_mb": { "type": "integer" },
                                "command": { "type": "string" }
                            },
                            "required": ["pid", "cpu_percent", "memory_mb", "command"],
                            "additionalProperties": false
                        }
                    }
                }),
                &["load", "top_processes"],
            ),
            "system.storage.clean_cache" => json!({
                "type": "object",
                "properties": {
                    "freed_bytes": { "type": "integer" },
                    "files": { "type": "integer", "description": "how many files were removed" }
                },
                "required": ["freed_bytes", "files"],
                "additionalProperties": false
            }),
            "system.service.restart" => json!({
                "type": "object",
                "properties": {
                    "service": { "type": "string" },
                    "restarted": { "type": "boolean" }
                },
                "required": ["service", "restarted"],
                "additionalProperties": false
            }),
            _ => return None,
        })
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        let check = |summary: &str| Preview {
            summary: summary.into(),
            ..Preview::default()
        };
        match action.as_str() {
            "system.network.check" => Ok(check("Check network")),
            "system.storage.check" => Ok(check("Check storage")),
            "system.performance.check" => Ok(check("Check performance")),
            "system.storage.clean_cache" => {
                let home = Self::home(ctx)?;
                if !home.is_real_dir(Path::new(CACHE)) {
                    return Err(ActionError::NotFound(
                        "there is no cache folder to clean".into(),
                    ));
                }
                let (bytes, files) = Self::cache_usage(&home)?;
                let root = ctx.home.join(CACHE);
                Ok(Preview {
                    summary: format!("Clean up cache files ({})", human(bytes)),
                    targets: vec![format!("~/{CACHE}")],
                    items: vec![format!(
                        "{files} cache files, {} in ~/{CACHE}",
                        human(bytes)
                    )],
                    texts: Vec::new(),
                    reads: Vec::new(),
                    writes: vec![root.clone()],
                    snapshot_roots: vec![root],
                    label: None,
                })
            }
            "system.service.restart" => {
                let ServiceInput { service } = parse_input(input)?;
                self.approved(&service)?;
                // Fail before anyone is asked to approve a restart that can't work.
                self.set_up(&service)?;
                Ok(Preview {
                    summary: format!("Restart the {service} service"),
                    targets: vec![service.clone()],
                    items: vec![format!("Restart {service} (on IT's approved list)")],
                    ..Preview::default()
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "system has no action {action}"
            ))),
        }
    }

    fn invoke(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<ActionOutput, ActionError> {
        match action.as_str() {
            "system.network.check" => self.network_check(),
            "system.storage.check" => self.storage_check(ctx),
            "system.performance.check" => self.performance_check(ctx),
            "system.storage.clean_cache" => {
                let home = Self::home(ctx)?;
                // Checked again here, not only in preview: a cache folder that
                // is a symlink would send the cleanup somewhere else.
                if !home.is_real_dir(Path::new(CACHE)) {
                    return Err(ActionError::NotFound(
                        "there is no cache folder to clean".into(),
                    ));
                }
                let (bytes, files) = Self::cache_usage(&home)?;
                let cache = Path::new(CACHE);
                for entry in home
                    .list(cache)
                    .map_err(|e| ActionError::io("cache folder", e))?
                {
                    let rel = cache.join(&entry.name);
                    let removed = if entry.is_dir {
                        home.remove_dir_all(&rel)
                    } else {
                        home.remove_file(&rel)
                    };
                    removed.map_err(|e| ActionError::io(format!("~/{}", rel.display()), e))?;
                }
                Ok(ActionOutput {
                    text: format!("Cleaned up {} of cache files ({files} files)", human(bytes)),
                    data: json!({ "freed_bytes": bytes, "files": files }),
                    changes: vec![Change {
                        kind: ChangeKind::Deleted,
                        target: format!("Cache files · {}", human(bytes)),
                        reversible: true,
                        paths: vec![ctx.home.join(CACHE)],
                    }],
                })
            }
            "system.service.restart" => {
                let ServiceInput { service } = parse_input(input)?;
                self.approved(&service)?;
                self.set_up(&service)?;
                self.ops
                    .restart_service(&service)
                    .map_err(ActionError::Failed)?;
                // A restart leaves nothing behind to undo; the audit log and
                // the activity feed record it.
                Ok(ActionOutput {
                    text: format!("Restarted {service}"),
                    data: json!({ "service": service, "restarted": true }),
                    changes: Vec::new(),
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "system has no action {action}"
            ))),
        }
    }
}

/// Bytes as people say them: "12.4 MB".
pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use glass_platform::fs::Owner;
    use glass_platform::system::Interface;
    use glass_types::{AgentPrincipal, TaskId, UserRef};

    use super::*;

    #[derive(Debug, Default)]
    struct Fake {
        dns_down: bool,
        restarted: Mutex<Vec<String>>,
    }

    impl SystemOps for Fake {
        fn network(&self) -> Result<NetworkStatus, String> {
            Ok(NetworkStatus {
                interfaces: vec![Interface {
                    name: "em0".into(),
                    up: true,
                    addresses: vec!["192.0.2.10".into()],
                }],
                gateway: Some("192.0.2.1".into()),
                nameservers: vec!["192.0.2.53".into()],
            })
        }
        fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
            if self.dns_down {
                Err("timed out".into())
            } else {
                Ok(vec!["192.0.2.80".parse().unwrap()])
            }
        }
        fn filesystem_usage(&self, _path: &Path) -> Result<FsUsage, String> {
            Ok(FsUsage {
                total_kb: 1000,
                used_kb: 950,
                available_kb: 50,
            })
        }
        fn load(&self) -> Result<Load, String> {
            Ok(Load {
                average: [0.2, 0.1, 0.1],
                cpus: 4,
                memory_total_mb: 8192,
                memory_free_mb: 4096,
            })
        }
        fn top_processes(&self, _uid: u32) -> Result<Vec<Process>, String> {
            Ok(vec![Process {
                pid: 7,
                cpu_percent: 93.0,
                memory_mb: 300,
                command: "spinner".into(),
            }])
        }
        fn service_enabled(&self, name: &str) -> bool {
            name == "ntpd"
        }
        fn restart_service(&self, name: &str) -> Result<(), String> {
            self.restarted.lock().unwrap().push(name.into());
            Ok(())
        }
    }

    fn ctx(home: PathBuf) -> TaskContext {
        TaskContext {
            principal: AgentPrincipal {
                agent: "it-agent".parse().unwrap(),
                on_behalf_of: UserRef {
                    name: "dana".into(),
                    uid: 1001,
                },
                task: TaskId::new(),
            },
            owner: Owner {
                uid: 1001,
                gid: 1001,
            },
            home,
            grants: Vec::new(),
            groups: Vec::new(),
            data_label: None,
        }
    }

    fn provider(fake: Fake) -> (Arc<Fake>, SystemProvider) {
        let fake = Arc::new(fake);
        let ops: Arc<dyn SystemOps> = fake.clone();
        (
            fake,
            SystemProvider::new(ops, vec!["ntpd".into(), "cupsd".into()]),
        )
    }

    fn name(s: &str) -> ActionName {
        s.parse().unwrap()
    }

    #[test]
    fn checks_report_findings_in_plain_words() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".cache")).unwrap();
        std::fs::write(home.path().join(".cache/blob"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        let (_, p) = provider(Fake {
            dns_down: true,
            ..Fake::default()
        });
        let c = ctx(home.path().into());
        let net = p
            .invoke(&c, &name("system.network.check"), &Value::Null)
            .unwrap();
        assert_eq!(net.data["healthy"], json!(false));
        assert!(net.text.contains("DNS isn't answering"), "{}", net.text);
        let storage = p
            .invoke(&c, &name("system.storage.check"), &Value::Null)
            .unwrap();
        assert!(storage.text.contains("95% full"), "{}", storage.text);
        assert!(storage.text.contains("2.0 MB of cache"), "{}", storage.text);
        let perf = p
            .invoke(&c, &name("system.performance.check"), &Value::Null)
            .unwrap();
        assert!(
            perf.text.contains("spinner is using 93% CPU"),
            "{}",
            perf.text
        );
    }

    #[test]
    fn cache_cleanup_is_snapshotted_and_never_follows_a_symlinked_cache() {
        let home = tempfile::tempdir().unwrap();
        let (_, p) = provider(Fake::default());
        let c = ctx(home.path().into());
        let clean = name("system.storage.clean_cache");
        assert!(matches!(
            p.preview(&c, &clean, &Value::Null),
            Err(ActionError::NotFound(_))
        ));

        std::fs::create_dir_all(home.path().join(".cache/thumbs")).unwrap();
        std::fs::write(home.path().join(".cache/thumbs/a.png"), b"png").unwrap();
        std::fs::write(home.path().join(".cache/b.bin"), b"bin").unwrap();
        let preview = p.preview(&c, &clean, &Value::Null).unwrap();
        assert_eq!(preview.snapshot_roots, [home.path().join(".cache")]);
        let out = p.invoke(&c, &clean, &Value::Null).unwrap();
        assert_eq!(out.changes[0].paths, [home.path().join(".cache")]);
        assert!(home.path().join(".cache").is_dir(), "the folder stays");
        assert_eq!(
            std::fs::read_dir(home.path().join(".cache"))
                .unwrap()
                .count(),
            0
        );

        // A cache that points elsewhere is not emptied.
        std::fs::remove_dir(home.path().join(".cache")).unwrap();
        std::fs::create_dir(home.path().join("Documents")).unwrap();
        std::fs::write(home.path().join("Documents/keep.txt"), b"keep").unwrap();
        std::os::unix::fs::symlink(home.path().join("Documents"), home.path().join(".cache"))
            .unwrap();
        assert!(p.preview(&c, &clean, &Value::Null).is_err());
        assert!(p.invoke(&c, &clean, &Value::Null).is_err());
        assert!(home.path().join("Documents/keep.txt").exists());
    }

    #[test]
    fn only_services_on_its_list_restart() {
        let home = tempfile::tempdir().unwrap();
        let (fake, p) = provider(Fake::default());
        let c = ctx(home.path().into());
        let restart = name("system.service.restart");
        let sshd = json!({ "service": "sshd" });
        assert!(matches!(
            p.preview(&c, &restart, &sshd),
            Err(ActionError::OutOfScope(_))
        ));
        assert!(matches!(
            p.invoke(&c, &restart, &sshd),
            Err(ActionError::OutOfScope(_))
        ));
        let cupsd = json!({ "service": "cupsd" });
        assert!(
            matches!(p.preview(&c, &restart, &cupsd), Err(ActionError::Failed(_))),
            "no approval is asked for a restart that can't work"
        );
        assert!(
            matches!(p.invoke(&c, &restart, &cupsd), Err(ActionError::Failed(_))),
            "listed but not set up here"
        );
        let ntpd = json!({ "service": "ntpd" });
        assert_eq!(
            p.invoke(&c, &restart, &ntpd).unwrap().text,
            "Restarted ntpd"
        );
        assert_eq!(*fake.restarted.lock().unwrap(), ["ntpd"]);
    }

    #[test]
    fn ga1_what_each_action_returns_is_what_it_declares() {
        // The checks run on a stand-in for the computer (`Fake`): the real
        // one needs the base system's commands. What they return is built
        // the same way from what either says.
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".cache")).unwrap();
        std::fs::write(home.path().join(".cache/blob"), b"blob").unwrap();
        let c = ctx(home.path().into());
        for dns_down in [false, true] {
            let (_, p) = provider(Fake {
                dns_down,
                ..Fake::default()
            });
            let mut seen = Vec::new();
            for def in p.actions() {
                let input = match def.name.as_str() {
                    "system.service.restart" => json!({ "service": "ntpd" }),
                    _ => json!({}),
                };
                let mut data = p.invoke(&c, &def.name, &input).unwrap().data;
                if let Some(data) = data.as_object_mut() {
                    data.remove(glass_actions::AUDIT_DETAIL);
                }
                let shape = p.returns(&def.name).unwrap();
                glass_actions::shape::conforms(&data, &shape)
                    .unwrap_or_else(|e| panic!("{}: {e}", def.name));
                seen.push(def.name.to_string());
            }
            assert_eq!(seen.len(), 5, "{seen:?}");
        }
    }

    #[test]
    fn sizes_read_like_people_say_them() {
        assert_eq!(human(512), "512 bytes");
        assert_eq!(human(2 * 1024 * 1024), "2.0 MB");
    }
}
