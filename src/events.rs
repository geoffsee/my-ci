use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct PipelineEvent {
    pub kind: EventKind,
    pub timestamp_ms: u128,
    pub workflow: Option<String>,
    pub phase: Option<WorkflowPhase>,
    pub status: Option<WorkflowStatus>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    PipelineStarted,
    PipelineFinished,
    PipelineCancelled,
    WorkflowStatus,
    Log,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowPhase {
    Build,
    Run,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl PipelineEvent {
    pub fn pipeline(kind: EventKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            timestamp_ms: now_ms(),
            workflow: None,
            phase: None,
            status: None,
            message: message.into(),
        }
    }

    pub fn workflow(
        workflow: impl Into<String>,
        phase: WorkflowPhase,
        status: WorkflowStatus,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: EventKind::WorkflowStatus,
            timestamp_ms: now_ms(),
            workflow: Some(workflow.into()),
            phase: Some(phase),
            status: Some(status),
            message: message.into(),
        }
    }

    pub fn log(
        workflow: impl Into<String>,
        phase: WorkflowPhase,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: EventKind::Log,
            timestamp_ms: now_ms(),
            workflow: Some(workflow.into()),
            phase: Some(phase),
            status: None,
            message: message.into(),
        }
    }

    pub fn error(
        workflow: impl Into<String>,
        phase: WorkflowPhase,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind: EventKind::Error,
            timestamp_ms: now_ms(),
            workflow: Some(workflow.into()),
            phase: Some(phase),
            status: Some(WorkflowStatus::Failed),
            message: message.into(),
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
