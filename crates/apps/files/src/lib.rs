//! The `files` app: list, read, write, move and delete inside granted
//! folders. All I/O goes through [`ConfinedDir`], so a planted symlink or
//! `..` can never reach outside a grant even though the broker runs as root.

use glass_api::ChangeKind;
use glass_platform::fs::ConfinedDir;
use glass_types::{ActionName, SideEffect, Tier};
use serde::Deserialize;
use serde_json::{Value, json};

use glass_actions::{
    ActionDef, ActionError, ActionOutput, ActionProvider, Change, Preview, Resolved, TaskContext,
    attribute, parse_input,
};

/// Largest file `files.read` returns.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Default)]
pub struct FilesProvider;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveInput {
    from: String,
    to: String,
}

fn def(
    name: &str,
    title: &str,
    description: &str,
    schema: Value,
    side_effect: SideEffect,
    tier: Tier,
) -> ActionDef {
    ActionDef {
        name: name.parse().expect("static action names are valid"),
        title: title.into(),
        description: description.into(),
        input_schema: schema,
        side_effect,
        default_tier: tier,
        ask: Default::default(),
    }
}

fn path_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": { "path": { "type": "string", "description": description } },
        "required": ["path"],
        "additionalProperties": false
    })
}

impl ActionProvider for FilesProvider {
    fn actions(&self) -> Vec<ActionDef> {
        vec![
            def(
                "files.list",
                "List files",
                "List a granted folder. Paths start with the folder name, such as \"Invoices\" or \
                 \"Invoices/2026\". An empty path lists the folders this task was given.",
                json!({
                    "type": "object",
                    "properties": { "path": { "type": "string", "description": "Folder to list; empty for the granted folders" } },
                    "additionalProperties": false
                }),
                SideEffect::None,
                Tier::Auto,
            ),
            def(
                "files.read",
                "Read a file",
                "Read a UTF-8 text file inside a granted folder.",
                path_schema("File to read, such as \"Invoices/inv-001.txt\""),
                SideEffect::None,
                Tier::Auto,
            ),
            def(
                "files.write",
                "Write a file",
                "Create or replace a text file inside a writable granted folder. Missing parent \
                 folders are created. The previous version is kept in a snapshot.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File to write, such as \"Reports/summary.csv\"" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
                SideEffect::LocalReversible,
                Tier::Notify,
            )
            .asking("Review file", "Approve and save"),
            def(
                "files.move",
                "Move a file",
                "Move or rename a file within one writable granted folder.",
                json!({
                    "type": "object",
                    "properties": { "from": { "type": "string" }, "to": { "type": "string" } },
                    "required": ["from", "to"],
                    "additionalProperties": false
                }),
                SideEffect::LocalReversible,
                Tier::Notify,
            )
            .asking("Review move", "Approve and move"),
            def(
                "files.delete",
                "Delete a file",
                "Delete a file inside a writable granted folder. It stays recoverable from the \
                 task's snapshots.",
                path_schema("File to delete"),
                SideEffect::LocalReversible,
                Tier::Ask,
            )
            .asking("Review file", "Approve and delete"),
        ]
    }

    fn preview(
        &self,
        ctx: &TaskContext,
        action: &ActionName,
        input: &Value,
    ) -> Result<Preview, ActionError> {
        match action.as_str() {
            "files.list" => {
                let PathInput { path } = parse_input(input)?;
                if path.trim().trim_matches('/').is_empty() {
                    return Ok(Preview {
                        summary: "List granted folders".into(),
                        ..Preview::default()
                    });
                }
                let r = ctx.resolve(&path)?;
                Ok(read_preview("List", &r))
            }
            "files.read" => {
                let PathInput { path } = parse_input(input)?;
                let r = ctx.resolve(&path)?;
                Ok(read_preview("Read", &r))
            }
            "files.write" => {
                let WriteInput { path, .. } = parse_input(input)?;
                let r = ctx.resolve_writable(&path)?;
                Ok(write_preview("Write", &r))
            }
            "files.delete" => {
                let PathInput { path } = parse_input(input)?;
                let r = ctx.resolve_writable(&path)?;
                Ok(write_preview("Delete", &r))
            }
            "files.move" => {
                let MoveInput { from, to } = parse_input(input)?;
                let (src, dst) = (ctx.resolve_writable(&from)?, ctx.resolve_writable(&to)?);
                if src.grant != dst.grant {
                    return Err(ActionError::InvalidInput(
                        "moves must stay within one folder".into(),
                    ));
                }
                Ok(Preview {
                    summary: format!("Move {} → {}", src.display, dst.display),
                    targets: vec![src.display.clone(), dst.display.clone()],
                    items: vec![format!("{} → {}", src.display, dst.display)],
                    texts: Vec::new(),
                    reads: vec![src.host.clone()],
                    writes: vec![src.host.clone(), dst.host.clone()],
                    snapshot_roots: vec![src.grant.root.clone()],
                    label: None,
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "files has no action {action}"
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
            "files.list" => {
                let PathInput { path } = parse_input(input)?;
                if path.trim().trim_matches('/').is_empty() {
                    let grants: Vec<Value> = ctx
                        .grants
                        .iter()
                        .map(|g| json!({ "name": g.name, "writable": g.writable }))
                        .collect();
                    let text = ctx
                        .grants
                        .iter()
                        .map(|g| {
                            format!(
                                "{}/ ({})",
                                g.name,
                                if g.writable {
                                    "read-write"
                                } else {
                                    "read-only"
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Ok(output(
                        if text.is_empty() {
                            "No folders granted.".into()
                        } else {
                            text
                        },
                        json!({ "folders": grants }),
                    ));
                }
                let r = ctx.resolve(&path)?;
                let entries = open(&r)?
                    .list(&r.rel)
                    .map_err(|e| ActionError::io(&r.display, e))?;
                let text = entries
                    .iter()
                    .map(|e| {
                        if e.is_dir {
                            format!("{}/", e.name)
                        } else {
                            format!("{} ({} bytes)", e.name, e.size)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let data: Vec<Value> = entries
                    .iter()
                    .map(|e| json!({ "name": e.name, "dir": e.is_dir, "size": e.size }))
                    .collect();
                Ok(output(text, json!({ "path": r.display, "entries": data })))
            }
            "files.read" => {
                let PathInput { path } = parse_input(input)?;
                let r = ctx.resolve(&path)?;
                let bytes = open(&r)?
                    .read(&r.rel, MAX_READ_BYTES)
                    .map_err(|e| ActionError::io(&r.display, e))?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| ActionError::Failed(format!("{} is not UTF-8 text", r.display)))?;
                Ok(output(text, Value::Null))
            }
            "files.write" => {
                let WriteInput { path, content } = parse_input(input)?;
                let r = ctx.resolve_writable(&path)?;
                let dir = open(&r)?;
                if let Some(parent) = r.rel.parent().filter(|p| !p.as_os_str().is_empty()) {
                    dir.create_dir_all(parent)
                        .map_err(|e| ActionError::io(&r.display, e))?;
                }
                let created = dir
                    .write(&r.rel, content.as_bytes(), Some(ctx.owner))
                    .map_err(|e| ActionError::io(&r.display, e))?;
                attribute(ctx, &r.host);
                let kind = if created {
                    ChangeKind::Created
                } else {
                    ChangeKind::Modified
                };
                Ok(ActionOutput {
                    text: format!(
                        "{} {} ({} bytes)",
                        if created { "Created" } else { "Updated" },
                        r.display,
                        content.len()
                    ),
                    data: json!({ "path": r.display, "created": created }),
                    changes: vec![Change {
                        kind,
                        target: r.display,
                        reversible: true,
                        paths: vec![r.host],
                    }],
                })
            }
            "files.delete" => {
                let PathInput { path } = parse_input(input)?;
                let r = ctx.resolve_writable(&path)?;
                open(&r)?
                    .remove_file(&r.rel)
                    .map_err(|e| ActionError::io(&r.display, e))?;
                Ok(ActionOutput {
                    text: format!("Deleted {}", r.display),
                    data: json!({ "path": r.display }),
                    changes: vec![Change {
                        kind: ChangeKind::Deleted,
                        target: r.display,
                        reversible: true,
                        paths: vec![r.host],
                    }],
                })
            }
            "files.move" => {
                let MoveInput { from, to } = parse_input(input)?;
                let (src, dst) = (ctx.resolve_writable(&from)?, ctx.resolve_writable(&to)?);
                if src.grant != dst.grant {
                    return Err(ActionError::InvalidInput(
                        "moves must stay within one folder".into(),
                    ));
                }
                let dir = open(&src)?;
                if let Some(parent) = dst.rel.parent().filter(|p| !p.as_os_str().is_empty()) {
                    dir.create_dir_all(parent)
                        .map_err(|e| ActionError::io(&dst.display, e))?;
                }
                dir.rename(&src.rel, &dst.rel)
                    .map_err(|e| ActionError::io(&src.display, e))?;
                attribute(ctx, &dst.host);
                Ok(ActionOutput {
                    text: format!("Moved {} to {}", src.display, dst.display),
                    data: json!({ "from": src.display, "to": dst.display }),
                    changes: vec![Change {
                        kind: ChangeKind::Moved,
                        target: format!("{} → {}", src.display, dst.display),
                        reversible: true,
                        paths: vec![src.host, dst.host],
                    }],
                })
            }
            _ => Err(ActionError::InvalidInput(format!(
                "files has no action {action}"
            ))),
        }
    }
}

fn open(r: &Resolved<'_>) -> Result<ConfinedDir, ActionError> {
    ConfinedDir::open(&r.grant.root).map_err(|e| ActionError::io(&r.grant.name, e))
}

fn output(text: String, data: Value) -> ActionOutput {
    ActionOutput {
        text,
        data,
        changes: Vec::new(),
    }
}

fn read_preview(verb: &str, r: &Resolved<'_>) -> Preview {
    Preview {
        summary: format!("{verb} {}", r.display),
        targets: vec![r.display.clone()],
        items: vec![r.display.clone()],
        reads: vec![r.host.clone()],
        ..Preview::default()
    }
}

fn write_preview(verb: &str, r: &Resolved<'_>) -> Preview {
    Preview {
        summary: format!("{verb} {}", r.display),
        targets: vec![r.display.clone()],
        items: vec![r.display.clone()],
        texts: Vec::new(),
        reads: Vec::new(),
        writes: vec![r.host.clone()],
        snapshot_roots: vec![r.grant.root.clone()],
        label: None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use glass_actions::testing::context;

    fn name(s: &str) -> ActionName {
        s.parse().unwrap()
    }

    fn setup() -> (tempfile::TempDir, TaskContext) {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Invoices")).unwrap();
        fs::create_dir_all(dir.path().join("Reports")).unwrap();
        fs::write(dir.path().join("Invoices/inv-1.txt"), "ACME 100.00").unwrap();
        let mut ctx = context(dir.path());
        let meta = fs::metadata(dir.path()).unwrap();
        ctx.owner = glass_platform::fs::Owner {
            uid: meta.uid(),
            gid: meta.gid(),
        };
        (dir, ctx)
    }

    #[test]
    fn reads_and_lists_granted_folders() {
        let (_dir, ctx) = setup();
        let p = FilesProvider;
        let out = p
            .invoke(
                &ctx,
                &name("files.read"),
                &json!({"path": "Invoices/inv-1.txt"}),
            )
            .unwrap();
        assert_eq!(out.text, "ACME 100.00");
        let out = p.invoke(&ctx, &name("files.list"), &json!({})).unwrap();
        assert!(out.text.contains("Invoices/ (read-only)"));
        assert!(out.text.contains("Reports/ (read-write)"));
        let out = p
            .invoke(&ctx, &name("files.list"), &json!({"path": "Invoices"}))
            .unwrap();
        assert_eq!(out.text, "inv-1.txt (11 bytes)");
    }

    #[test]
    fn writes_report_changes_and_previews_snapshot_roots() {
        let (dir, ctx) = setup();
        let p = FilesProvider;
        let input = json!({"path": "Reports/q3/summary.csv", "content": "total,100"});
        let preview = p.preview(&ctx, &name("files.write"), &input).unwrap();
        assert_eq!(preview.summary, "Write Reports/q3/summary.csv");
        assert_eq!(preview.snapshot_roots, [dir.path().join("Reports")]);
        let out = p.invoke(&ctx, &name("files.write"), &input).unwrap();
        assert_eq!(out.changes[0].kind, ChangeKind::Created);
        assert_eq!(
            fs::read_to_string(dir.path().join("Reports/q3/summary.csv")).unwrap(),
            "total,100"
        );
        let out = p.invoke(&ctx, &name("files.write"), &input).unwrap();
        assert_eq!(out.changes[0].kind, ChangeKind::Modified);
    }

    #[test]
    fn read_only_grants_refuse_changes() {
        let (_dir, ctx) = setup();
        let p = FilesProvider;
        for (action, input) in [
            (
                "files.write",
                json!({"path": "Invoices/x.txt", "content": "x"}),
            ),
            ("files.delete", json!({"path": "Invoices/inv-1.txt"})),
        ] {
            assert!(matches!(
                p.preview(&ctx, &name(action), &input),
                Err(ActionError::OutOfScope(_))
            ));
            assert!(matches!(
                p.invoke(&ctx, &name(action), &input),
                Err(ActionError::OutOfScope(_))
            ));
        }
    }

    #[test]
    fn unknown_input_fields_are_rejected() {
        let (_dir, ctx) = setup();
        let err = FilesProvider.preview(
            &ctx,
            &name("files.read"),
            &json!({"path": "Invoices/inv-1.txt", "sudo": true}),
        );
        assert!(matches!(err, Err(ActionError::InvalidInput(_))));
    }
}
