//! Apps people made that IT approved into the org's catalog (ADR 0032,
//! R2), on this computer. The signed catalog names each one's manifest and
//! the SHA-256 of its bundle; glassd fetches the bundle from the console,
//! keeps it only if the digest matches, root-only under the state
//! directory, and serves it over the app socket (`app.files`). A bundle the
//! catalog no longer lists is removed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use glass_api::AppManifest;
use glass_app_catalog::CatalogHandle;
use glass_managed::studio::{Bundle, BundleRequest, OrgApp, ShareLink};
use glass_types::AgentId;

/// The org's apps on this computer: `<dir>/<id>/<version>/` holds the
/// manifest and the bundle's files.
#[derive(Debug, Clone)]
pub(crate) struct OrgAppStore {
    dir: PathBuf,
    catalog: CatalogHandle,
    link: Arc<OnceLock<Arc<dyn ShareLink>>>,
}

/// What a fetch did, for the record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fetched {
    Installed { app: AgentId, version: String },
    Refused { app: AgentId, why: String },
}

impl OrgAppStore {
    pub(crate) fn new(dir: PathBuf, catalog: CatalogHandle) -> Self {
        Self {
            dir,
            catalog,
            link: Arc::new(OnceLock::new()),
        }
    }

    /// Bundles come from the org's console through `link`.
    pub(crate) fn connect(&self, link: Arc<dyn ShareLink>) {
        let _ = self.link.set(link);
    }

    fn version_dir(&self, app: &AgentId, version: &str) -> Option<PathBuf> {
        // A version is a manifest's one-line text; as a folder name it
        // must be plain.
        let plain = !version.is_empty()
            && version.len() <= 32
            && version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            && !version.starts_with('.');
        plain.then(|| self.dir.join(app.as_str()).join(version))
    }

    /// The catalog's org apps.
    fn listed(&self) -> Vec<OrgApp> {
        self.catalog
            .current()
            .map(|applied| {
                applied
                    .release
                    .items
                    .iter()
                    .filter_map(|item| item.app.clone())
                    .filter(|app| app.manifest.check().is_ok() && !app.manifest.id.is_personal())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Fetches what the catalog lists and this computer lacks, keeps only
    /// what matches the signed digest, and removes what it no longer lists.
    pub(crate) fn fetch(&self) -> Vec<Fetched> {
        let mut done = Vec::new();
        let listed = self.listed();
        for app in &listed {
            let (id, version) = (&app.manifest.id, &app.manifest.version);
            let Some(dir) = self.version_dir(id, version) else {
                continue;
            };
            if dir.join("manifest.json").exists() {
                continue;
            }
            let Some(link) = self.link.get() else {
                break;
            };
            let refused = |why: String| Fetched::Refused {
                app: id.clone(),
                why,
            };
            match link.bundle(&BundleRequest {
                app: id.clone(),
                version: version.clone(),
            }) {
                Err(e) => done.push(refused(format!("fetching it: {e}"))),
                Ok(bundle) => match self.keep(app, &bundle, &dir) {
                    Ok(()) => done.push(Fetched::Installed {
                        app: id.clone(),
                        version: version.clone(),
                    }),
                    Err(why) => done.push(refused(why)),
                },
            }
        }
        // Gone from the catalog: gone from the computer.
        let keep: Vec<&AgentId> = listed.iter().map(|a| &a.manifest.id).collect();
        for entry in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let listed = name
                .to_str()
                .and_then(|n| n.parse::<AgentId>().ok())
                .is_some_and(|id| keep.contains(&&id));
            if !listed {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        done
    }

    /// Checks `bundle` against what the signed catalog says of `app`, then
    /// writes it whole or not at all.
    fn keep(&self, app: &OrgApp, bundle: &Bundle, dir: &Path) -> Result<(), String> {
        if bundle.app != app.manifest.id || bundle.version != app.manifest.version {
            return Err(format!(
                "the console sent {} {} for {} {}",
                bundle.app, bundle.version, app.manifest.id, app.manifest.version
            ));
        }
        if bundle.digest() != app.bundle_sha256 {
            return Err("its bundle doesn't match the signed catalog's digest".into());
        }
        let plain = |name: &str| {
            !name.is_empty()
                && !name.starts_with('.')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        };
        if let Some(bad) = bundle.files.keys().find(|n| !plain(n)) {
            return Err(format!("its bundle names a file {bad:?}"));
        }
        let write = || -> std::io::Result<()> {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(&self.dir)?;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
            let staging = self.dir.join(format!(".new-{}", app.manifest.id));
            let _ = std::fs::remove_dir_all(&staging);
            std::fs::create_dir_all(staging.join("files"))?;
            std::fs::write(
                staging.join("manifest.json"),
                serde_json::to_vec_pretty(&app.manifest).map_err(std::io::Error::other)?,
            )?;
            for (name, text) in &bundle.files {
                std::fs::write(staging.join("files").join(name), text)?;
            }
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&staging, dir)
        };
        write().map_err(|e| format!("keeping it: {e}"))
    }
}

impl glass_runtime::OrgApps for OrgAppStore {
    fn apps(&self) -> Vec<AppManifest> {
        self.listed()
            .into_iter()
            .filter(|app| {
                self.version_dir(&app.manifest.id, &app.manifest.version)
                    .is_some_and(|dir| dir.join("manifest.json").exists())
            })
            .map(|app| app.manifest)
            .collect()
    }

    fn files(&self, app: &AgentId) -> Option<(String, BTreeMap<String, String>)> {
        let listed = self.listed().into_iter().find(|a| &a.manifest.id == app)?;
        let version = listed.manifest.version;
        let dir = self.version_dir(app, &version)?.join("files");
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let name = entry.file_name().to_str()?.to_owned();
            files.insert(name, std::fs::read_to_string(entry.path()).ok()?);
        }
        Some((version, files))
    }
}

#[cfg(test)]
mod tests {
    use glass_app_catalog::Applied;
    use glass_managed::{CatalogItem, CatalogRelease, Declared, ItemKind};
    use glass_runtime::OrgApps;

    use super::*;

    fn bundle(text: &str) -> Bundle {
        Bundle {
            app: "mail-with-write".parse().unwrap(),
            version: "1".into(),
            files: BTreeMap::from([
                (
                    "index.html".to_owned(),
                    "<title>Mail, with Write</title>".to_owned(),
                ),
                ("app.js".to_owned(), text.to_owned()),
            ]),
        }
    }

    fn catalog(digest: String) -> CatalogHandle {
        let manifest: AppManifest = serde_json::from_value(serde_json::json!({
            "id": "mail-with-write", "name": "Mail, with Write", "version": "1",
            "actions": ["mail.list"], "labels": ["internal"],
        }))
        .unwrap();
        let handle = CatalogHandle::new();
        handle.set(Applied {
            version: 3,
            release: CatalogRelease {
                items: vec![CatalogItem {
                    id: "mail-with-write".parse().unwrap(),
                    kind: ItemKind::App,
                    name: "Mail, with Write".into(),
                    version: "1".into(),
                    publisher: "Dana Ruiz".into(),
                    publisher_verified: false,
                    internal: true,
                    summary: String::new(),
                    keywords: Vec::new(),
                    declares: Declared::default(),
                    groups: BTreeMap::new(),
                    pinned: false,
                    provenance: Default::default(),
                    app: Some(OrgApp {
                        manifest,
                        bundle_sha256: digest,
                        source_sha256: String::new(),
                        made_by: "dana".into(),
                        based_on: Some("mail@0.1.0".into()),
                    }),
                    patch: None,
                }],
            },
        });
        handle
    }

    /// The console, sending whatever bundle the test says.
    #[derive(Debug)]
    struct Console(Bundle);

    impl ShareLink for Console {
        fn submit(
            &self,
            _: &glass_managed::studio::Submission,
        ) -> Result<glass_managed::studio::Submitted, String> {
            Err("no".into())
        }
        fn bundle(&self, _: &BundleRequest) -> Result<Bundle, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn ga13_an_org_app_is_kept_only_as_the_signed_catalog_says() {
        let dir = tempfile::tempdir().unwrap();
        let signed = bundle("console.log('approved')");
        // The console sends something else than was signed: refused.
        let store = OrgAppStore::new(dir.path().join("org"), catalog(signed.digest()));
        store.connect(Arc::new(Console(bundle("console.log('swapped')"))));
        assert!(
            matches!(&store.fetch()[0], Fetched::Refused { why, .. } if why.contains("digest"))
        );
        assert!(store.apps().is_empty());

        let store = OrgAppStore::new(dir.path().join("org"), catalog(signed.digest()));
        store.connect(Arc::new(Console(signed.clone())));
        assert_eq!(
            store.fetch(),
            [Fetched::Installed {
                app: "mail-with-write".parse().unwrap(),
                version: "1".into()
            }]
        );
        assert_eq!(store.apps()[0].name, "Mail, with Write");
        let (version, files) = store.files(&"mail-with-write".parse().unwrap()).unwrap();
        assert_eq!(
            (version.as_str(), files["app.js"].as_str()),
            ("1", "console.log('approved')")
        );
        // Fetched once.
        assert!(store.fetch().is_empty());

        // Out of the catalog, off the computer.
        store.catalog.set(Applied {
            version: 4,
            release: CatalogRelease { items: Vec::new() },
        });
        store.fetch();
        assert!(!dir.path().join("org/mail-with-write").exists());
    }
}
