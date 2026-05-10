use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};

use crate::events::{EventKind, PipelineEvent, WorkflowPhase, WorkflowStatus};

#[derive(Clone)]
pub struct HistoryStore {
    pool: Pool<Sqlite>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct RunRecord {
    pub id: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub mode: String,
    pub workflow: Option<String>,
    pub status: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EventRecord {
    pub id: i64,
    pub run_id: i64,
    pub timestamp_ms: i64,
    pub kind: String,
    pub workflow: Option<String>,
    pub phase: Option<String>,
    pub status: Option<String>,
    pub message: String,
}

impl HistoryStore {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .with_context(|| format!("failed to open history database at {}", path.display()))?;

        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at INTEGER NOT NULL,
                finished_at INTEGER,
                mode TEXT NOT NULL,
                workflow TEXT,
                status TEXT NOT NULL DEFAULT 'running'
            )"#,
        )
        .execute(&pool)
        .await
        .context("failed to ensure runs table")?;

        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS run_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id INTEGER NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                kind TEXT NOT NULL,
                workflow TEXT,
                phase TEXT,
                status TEXT,
                message TEXT NOT NULL,
                FOREIGN KEY(run_id) REFERENCES runs(id)
            )"#,
        )
        .execute(&pool)
        .await
        .context("failed to ensure run_events table")?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_run_events_run_id ON run_events(run_id)")
            .execute(&pool)
            .await
            .context("failed to ensure run_events index")?;

        Ok(Self { pool })
    }

    pub async fn create_run(&self, mode: &str, workflow: Option<&str>) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "INSERT INTO runs (started_at, mode, workflow, status) VALUES (?, ?, ?, 'running') RETURNING id",
        )
        .bind(now_ms())
        .bind(mode)
        .bind(workflow)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert run")?;
        Ok(row.0)
    }

    pub async fn finish_run(&self, run_id: i64, status: &str) -> Result<()> {
        sqlx::query("UPDATE runs SET finished_at = ?, status = ? WHERE id = ?")
            .bind(now_ms())
            .bind(status)
            .bind(run_id)
            .execute(&self.pool)
            .await
            .context("failed to finalize run")?;
        Ok(())
    }

    pub async fn append_event(&self, run_id: i64, event: &PipelineEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO run_events (run_id, timestamp_ms, kind, workflow, phase, status, message) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id)
        .bind(event.timestamp_ms as i64)
        .bind(kind_str(&event.kind))
        .bind(event.workflow.as_deref())
        .bind(event.phase.as_ref().map(phase_str))
        .bind(event.status.as_ref().map(status_str))
        .bind(&event.message)
        .execute(&self.pool)
        .await
        .context("failed to insert run event")?;
        Ok(())
    }

    pub async fn list_runs(&self, limit: i64) -> Result<Vec<RunRecord>> {
        let rows = sqlx::query_as::<_, RunRecord>(
            "SELECT id, started_at, finished_at, mode, workflow, status \
             FROM runs ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch runs")?;
        Ok(rows)
    }

    pub async fn list_events(&self, run_id: i64) -> Result<Vec<EventRecord>> {
        let rows = sqlx::query_as::<_, EventRecord>(
            "SELECT id, run_id, timestamp_ms, kind, workflow, phase, status, message \
             FROM run_events WHERE run_id = ? ORDER BY id ASC",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch run events")?;
        Ok(rows)
    }

    /// Drop older runs and their events, keeping the `keep` most recent runs by primary key.
    pub async fn prune_runs_keep_recent(&self, keep: i64) -> Result<()> {
        if keep < 1 {
            return Ok(());
        }
        let cut_id: Option<i64> = sqlx::query_scalar(
            "SELECT MIN(id) FROM (SELECT id FROM runs ORDER BY id DESC LIMIT ?)",
        )
        .bind(keep)
        .fetch_one(&self.pool)
        .await
        .context("failed to resolve run ids to retain")?;

        let Some(cut_id) = cut_id else {
            return Ok(());
        };

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin prune transaction")?;
        sqlx::query("DELETE FROM run_events WHERE run_id < ?")
            .bind(cut_id)
            .execute(&mut *tx)
            .await
            .context("failed to prune old run events")?;
        sqlx::query("DELETE FROM runs WHERE id < ?")
            .bind(cut_id)
            .execute(&mut *tx)
            .await
            .context("failed to prune old runs")?;
        tx.commit()
            .await
            .context("failed to commit prune transaction")?;
        Ok(())
    }
}

fn kind_str(k: &EventKind) -> &'static str {
    match k {
        EventKind::PipelineStarted => "pipeline-started",
        EventKind::PipelineFinished => "pipeline-finished",
        EventKind::PipelineCancelled => "pipeline-cancelled",
        EventKind::WorkflowStatus => "workflow-status",
        EventKind::Log => "log",
        EventKind::Error => "error",
    }
}

fn phase_str(p: &WorkflowPhase) -> &'static str {
    match p {
        WorkflowPhase::Build => "build",
        WorkflowPhase::Run => "run",
    }
}

fn status_str(s: &WorkflowStatus) -> &'static str {
    match s {
        WorkflowStatus::Pending => "pending",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Succeeded => "succeeded",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Skipped => "skipped",
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prune_runs_keep_recent_drops_old_rows() {
        let path = std::env::temp_dir().join(format!(
            "my-ci-hist-prune-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&path);
        let store = HistoryStore::open(&path).await.unwrap();
        let id1 = store.create_run("build", None).await.unwrap();
        store.finish_run(id1, "succeeded").await.unwrap();
        let id2 = store.create_run("build", None).await.unwrap();
        store.finish_run(id2, "succeeded").await.unwrap();
        let id3 = store.create_run("build", None).await.unwrap();
        store.finish_run(id3, "succeeded").await.unwrap();
        assert_eq!(store.list_runs(10).await.unwrap().len(), 3);
        store.prune_runs_keep_recent(1).await.unwrap();
        let runs = store.list_runs(10).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, id3);
        let _ = std::fs::remove_file(&path);
    }
}
