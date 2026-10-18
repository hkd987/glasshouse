//! A small client for driving glassd the way a user would, with waits and
//! assertions that fail with useful messages.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use glass_api::{
    AnswerApproval, ApprovalAnswer, ApprovalScope, ApprovalView, ScopeRequest, TaskDetail, TaskRef,
    TaskRequest, TaskView, TileState, UndoReport, UndoRequest, methods,
};
use glass_audit::AuditRecord;
use glass_rpc::Client;
use glass_types::TaskId;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

pub struct Harness {
    client: Client,
    pub home: PathBuf,
}

impl Harness {
    pub async fn connect() -> anyhow::Result<Self> {
        let socket = std::env::var(glass_api::CONTROL_SOCKET_ENV)
            .unwrap_or_else(|_| glass_api::DEFAULT_CONTROL_SOCKET.to_owned());
        let client = Client::connect(&socket)
            .await
            .with_context(|| format!("connecting to {socket}"))?;
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        Ok(Self { client, home })
    }

    pub async fn call<R: DeserializeOwned>(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> anyhow::Result<R> {
        self.client
            .call(method, params)
            .await
            .map_err(|e| anyhow::anyhow!("{method}: {e}"))
    }

    /// The raw error from a call expected to fail.
    pub async fn call_err(&self, method: &str, params: impl Serialize) -> anyhow::Result<String> {
        match self.client.call::<Value>(method, params).await {
            Ok(v) => bail!("{method} unexpectedly succeeded: {v}"),
            Err(e) => Ok(e.to_string()),
        }
    }

    pub async fn start(
        &self,
        agent: &str,
        prompt: &str,
        read: &[&Path],
        write: &[&Path],
    ) -> anyhow::Result<TaskId> {
        let request = TaskRequest {
            agent: agent.parse().map_err(|e| anyhow::anyhow!("{e}"))?,
            prompt: prompt.into(),
            scope: ScopeRequest {
                read: read.iter().map(|p| p.to_path_buf()).collect(),
                write: write.iter().map(|p| p.to_path_buf()).collect(),
                restrict: Vec::new(),
            },
        };
        let view: TaskView = self.call(methods::TASK_START, request).await?;
        Ok(view.id)
    }

    pub async fn detail(&self, task: TaskId) -> anyhow::Result<TaskDetail> {
        self.call(methods::TASK_GET, TaskRef { task }).await
    }

    pub async fn act(&self, method: &str, task: TaskId) -> anyhow::Result<TaskView> {
        self.call(method, TaskRef { task }).await
    }

    /// Waits for the task to reach `state` (or to end, which fails unless
    /// `state` is an end state).
    pub async fn wait_for(
        &self,
        task: TaskId,
        state: TileState,
        timeout: Duration,
    ) -> anyhow::Result<TaskDetail> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let detail = self.detail(task).await?;
            if detail.view.state == state {
                return Ok(detail);
            }
            if !detail.view.state.is_live() {
                bail!(
                    "task ended {:?} while waiting for {state:?}: {}",
                    detail.view.state,
                    detail
                        .view
                        .failure
                        .as_deref()
                        .unwrap_or(&detail.view.caption)
                );
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "timed out waiting for {state:?}; task is {:?}: {}",
                    detail.view.state,
                    detail.now
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn pending(&self) -> anyhow::Result<Vec<ApprovalView>> {
        self.call(methods::APPROVAL_LIST, Value::Null).await
    }

    pub async fn the_ask(&self, task: TaskId) -> anyhow::Result<ApprovalView> {
        let asks: Vec<ApprovalView> = self
            .pending()
            .await?
            .into_iter()
            .filter(|a| a.task == task)
            .collect();
        ensure!(
            asks.len() == 1,
            "expected one pending ask on the task, found {}",
            asks.len()
        );
        Ok(asks.into_iter().next().expect("checked length"))
    }

    pub async fn answer(
        &self,
        ask: &ApprovalView,
        answer: ApprovalAnswer,
    ) -> anyhow::Result<ApprovalView> {
        self.call(
            methods::APPROVAL_ANSWER,
            AnswerApproval {
                approval: ask.id,
                answer,
            },
        )
        .await
    }

    pub async fn approve_once(&self, ask: &ApprovalView) -> anyhow::Result<ApprovalView> {
        self.answer(
            ask,
            ApprovalAnswer::Approve {
                scope: ApprovalScope::Once,
            },
        )
        .await
    }

    pub async fn undo(&self, task: TaskId, dry_run: bool) -> anyhow::Result<UndoReport> {
        self.undo_changes(task, Vec::new(), dry_run).await
    }

    /// Undoes only these changes (`ChangeEntry::id`); empty undoes all.
    pub async fn undo_changes(
        &self,
        task: TaskId,
        changes: Vec<u32>,
        dry_run: bool,
    ) -> anyhow::Result<UndoReport> {
        self.call(
            methods::TASK_UNDO,
            UndoRequest {
                task,
                to: None,
                changes,
                dry_run,
            },
        )
        .await
    }

    pub async fn audit(&self, task: TaskId) -> anyhow::Result<Vec<AuditRecord>> {
        self.call(methods::AUDIT_TASK, TaskRef { task }).await
    }

    /// A fresh workspace in the user's home with the demo invoices.
    pub fn workspace(&self, name: &str) -> anyhow::Result<Workspace> {
        let root = self.home.join(format!("e2e-{name}-{}", std::process::id()));
        let invoices = root.join("Invoices");
        let reports = root.join("Reports");
        std::fs::create_dir_all(&invoices)?;
        std::fs::create_dir_all(&reports)?;
        let seed = Path::new(SEED_INVOICES);
        if seed.is_dir() {
            for entry in std::fs::read_dir(seed)?.flatten() {
                std::fs::copy(entry.path(), invoices.join(entry.file_name()))?;
            }
        } else {
            for (i, (vendor, status)) in [
                ("Acme", "unpaid"),
                ("Globex", "paid"),
                ("Initech", "unpaid"),
            ]
            .iter()
            .enumerate()
            {
                std::fs::write(
                    invoices.join(format!("inv-{i}.txt")),
                    format!(
                        "Invoice: INV-{i}\nVendor: {vendor}\nEmail: ar@{}.example\nAmount: 100.00\nDue: 2026-10-01\nStatus: {status}\n",
                        vendor.to_lowercase()
                    ),
                )?;
            }
        }
        Ok(Workspace {
            root,
            invoices,
            reports,
        })
    }
}

const SEED_INVOICES: &str = "/usr/local/share/glasshouse/demo/Invoices";

pub struct Workspace {
    pub root: PathBuf,
    pub invoices: PathBuf,
    pub reports: PathBuf,
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn mail_count(home: &Path, folder: &str) -> usize {
    std::fs::read_dir(home.join("Mail").join(folder))
        .map(|d| d.count())
        .unwrap_or(0)
}
