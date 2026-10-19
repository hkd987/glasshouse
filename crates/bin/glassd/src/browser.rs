//! The browser's settings from policy (UX-1, ADR 0029): glassd writes them
//! as the engine's own policy files, `managed/org.json` for what IT locked
//! and `recommended/org.json` for IT's defaults, which a person can change
//! in the browser. The image ships both with the computer's defaults, and
//! `glasshouse.json` beside them never sets the keys these files own: the
//! engine leaves it undefined which file wins when two set the same key.

use std::fs;
use std::io;
use std::path::Path;

use glass_policy::document::SettingValue;
use glass_policy::settings::Resolved;
use serde_json::{Map, Value, json};

/// Internal pages nobody needs, some of which name the base system (BR-1).
/// Always blocked, before any site IT adds.
pub const BASE_BLOCKED: &[&str] = &[
    "chrome://version",
    "chrome://gpu",
    "chrome://system",
    "chrome://sandbox",
];

/// The engine's name for a site IT blocked: it matches subdomains itself,
/// so `*.games.example` is `games.example`.
fn blocklist_entry(site: &str) -> &str {
    site.strip_prefix("*.").unwrap_or(site)
}

/// The two files' contents: (managed, recommended).
pub fn policies(device: &[Resolved]) -> (Value, Value) {
    let get = |id: &str| device.iter().find(|r| r.id == id);
    let mut managed = Map::new();
    let mut recommended = Map::new();
    let mut blocked: Vec<String> = BASE_BLOCKED.iter().map(|s| (*s).to_owned()).collect();
    if let Some(Resolved {
        value: SettingValue::List(sites),
        ..
    }) = get("browser.blocked_sites")
    {
        // Blocked sites are always enforced: a default anyone could undo
        // blocks nothing.
        blocked.extend(sites.iter().map(|s| blocklist_entry(s).to_owned()));
    }
    managed.insert("URLBlocklist".into(), json!(blocked));
    let mut put = |locked: bool, key: &str, value: Value| {
        if locked {
            managed.insert(key.to_owned(), value);
        } else {
            recommended.insert(key.to_owned(), value);
        }
    };

    let home = get("browser.home_page");
    let home_url = match home.map(|r| &r.value) {
        Some(SettingValue::Text(url)) if !url.is_empty() => Some(url.clone()),
        _ => None,
    };
    if let (Some(url), Some(r)) = (&home_url, home) {
        put(r.locked, "HomepageLocation", json!(url));
        put(r.locked, "HomepageIsNewTabPage", json!(false));
        put(r.locked, "ShowHomeButton", json!(true));
    }

    let restore = get("browser.restore_tabs");
    let restoring = !matches!(restore.map(|r| &r.value), Some(SettingValue::Bool(false)));
    let locked = restore.is_some_and(|r| r.locked);
    // 1: last session's tabs; 4: the home page; 5: a new tab.
    match (&home_url, restoring) {
        (_, true) => put(locked, "RestoreOnStartup", json!(1)),
        (Some(url), false) => {
            put(locked, "RestoreOnStartup", json!(4));
            put(locked, "RestoreOnStartupURLs", json!([url]));
        }
        (None, false) => put(locked, "RestoreOnStartup", json!(5)),
    }

    for (id, key) in [
        ("browser.camera", "VideoCaptureAllowed"),
        ("browser.microphone", "AudioCaptureAllowed"),
    ] {
        if let Some(Resolved {
            value: SettingValue::Bool(on),
            locked,
            ..
        }) = get(id)
        {
            put(*locked, key, json!(on));
        }
    }
    (Value::Object(managed), Value::Object(recommended))
}

/// Writes both files under `dir` (`managed/`, `recommended/`), each whole.
pub fn write(dir: &Path, device: &[Resolved]) -> io::Result<()> {
    let (managed, recommended) = policies(device);
    for (sub, value) in [("managed", managed), ("recommended", recommended)] {
        let folder = dir.join(sub);
        fs::create_dir_all(&folder)?;
        let tmp = folder.join(".org.json.new");
        let mut text = serde_json::to_string_pretty(&value).map_err(io::Error::other)?;
        text.push('\n');
        fs::write(&tmp, text)?;
        fs::rename(&tmp, folder.join("org.json"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use glass_policy::Policy;

    use super::*;

    fn device(extra: &str) -> Vec<Resolved> {
        let mut policy = String::from(glass_policy::DEFAULT_POLICY);
        // The settings go on the default policy's everyone group.
        policy = policy.replacen(
            "apps = [\"browser\"",
            &format!("settings = {{ {extra} }}\napps = [\"browser\""),
            1,
        );
        Policy::from_toml(&policy).unwrap().device_settings()
    }

    #[test]
    fn ux1_the_computers_defaults_block_only_the_internal_pages() {
        let (managed, recommended) = policies(&device(""));
        assert_eq!(managed, json!({ "URLBlocklist": BASE_BLOCKED }));
        assert_eq!(
            recommended,
            json!({ "RestoreOnStartup": 1, "VideoCaptureAllowed": true, "AudioCaptureAllowed": true })
        );
    }

    #[test]
    fn ux1_it_locks_go_to_managed_and_defaults_to_recommended() {
        let (managed, recommended) = policies(&device(
            r#""browser.home_page" = { value = "https://intranet.acme.example", locked = true }, "browser.blocked_sites" = { value = ["games.example", "*.social.example"] }, "browser.restore_tabs" = { value = false }, "browser.camera" = { value = false, locked = true }"#,
        ));
        assert_eq!(managed["HomepageLocation"], "https://intranet.acme.example");
        assert_eq!(managed["ShowHomeButton"], true);
        assert_eq!(managed["VideoCaptureAllowed"], false);
        let blocked = managed["URLBlocklist"].as_array().unwrap();
        assert!(blocked.contains(&json!("chrome://version")), "BR-1 stays");
        assert!(blocked.contains(&json!("games.example")));
        assert!(blocked.contains(&json!("social.example")));
        assert_eq!(recommended["RestoreOnStartup"], 4);
        assert_eq!(
            recommended["RestoreOnStartupURLs"],
            json!(["https://intranet.acme.example"])
        );
        assert_eq!(recommended["AudioCaptureAllowed"], true);
        assert!(recommended.get("HomepageLocation").is_none());

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &device("")).unwrap();
        let written: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("managed/org.json")).unwrap())
                .unwrap();
        assert_eq!(written["URLBlocklist"], json!(BASE_BLOCKED));
    }

    #[test]
    fn br1_the_images_own_browser_policy_never_sets_a_key_glassd_owns() {
        let shipped = |sub: &str| -> Value {
            serde_json::from_str(
                &fs::read_to_string(format!(
                    "{}/../../../os/freebsd/overlay/usr/local/etc/chromium/policies/{sub}/glasshouse.json",
                    env!("CARGO_MANIFEST_DIR")
                ))
                .unwrap(),
            )
            .unwrap()
        };
        let every = device(
            r#""browser.home_page" = { value = "https://x.example", locked = true }, "browser.blocked_sites" = { value = ["a.example"], locked = true }, "browser.restore_tabs" = { value = false, locked = true }, "browser.camera" = { value = false, locked = true }, "browser.microphone" = { value = false, locked = true }"#,
        );
        // What the image ships before glassd first writes is what glassd
        // writes for a computer nobody set.
        let (defaults_managed, defaults_recommended) = policies(&device(""));
        let read = |sub: &str, name: &str| -> Value {
            serde_json::from_str(
                &fs::read_to_string(format!(
                    "{}/../../../os/freebsd/overlay/usr/local/etc/chromium/policies/{sub}/{name}",
                    env!("CARGO_MANIFEST_DIR")
                ))
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(read("managed", "org.json"), defaults_managed);
        assert_eq!(read("recommended", "org.json"), defaults_recommended);
        let (managed, _) = policies(&every);
        let mut ours: Vec<String> = managed.as_object().unwrap().keys().cloned().collect();
        ours.push("RestoreOnStartup".into());
        for sub in ["managed", "recommended"] {
            let theirs = shipped(sub);
            for key in &ours {
                assert!(
                    theirs.get(key).is_none(),
                    "{sub}/glasshouse.json sets {key}, which glassd's org.json owns"
                );
            }
        }
    }
}
