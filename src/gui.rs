use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use bollard::Docker;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::{ServeDir, ServeFile};

use crate::build::build_workflow_with_events;
use crate::config::{WorkflowFile, get_workflow, image_tag};
use crate::events::{EventKind, PipelineEvent, WorkflowPhase, WorkflowStatus};
use crate::graph::{resolve_build_plan, topological_order};
use crate::run::run_workflow_with_events;

#[derive(Clone)]
struct AppState {
    config: Arc<WorkflowFile>,
    docker: Docker,
    events: broadcast::Sender<PipelineEvent>,
    active: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

#[derive(Debug, Deserialize)]
struct PipelineRequest {
    workflow: Option<String>,
}

#[derive(Debug, Serialize)]
struct PipelineResponse {
    accepted: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct PipelineDescription {
    name: String,
    workflows: Vec<WorkflowSummary>,
}

#[derive(Debug, Serialize)]
struct WorkflowSummary {
    name: String,
    image: String,
    depends_on: Vec<String>,
    command: bool,
}

pub async fn serve_gui(
    host: IpAddr,
    port: u16,
    config: WorkflowFile,
    docker: Docker,
) -> Result<()> {
    if !Path::new("ui/dist/index.html").is_file() {
        bail!("UI assets missing: run `cd ui && npm install && npm run build`");
    }

    let (event_sender, _) = broadcast::channel(512);
    let state = AppState {
        config: Arc::new(config),
        docker,
        events: event_sender,
        active: Arc::new(Mutex::new(None)),
    };

    let ui_assets = ServeDir::new("ui/dist").fallback(ServeFile::new("ui/dist/index.html"));

    let app = Router::new()
        .route("/favicon.ico", get(favicon))
        .route("/api/pipeline", get(pipeline))
        .route("/api/events", get(sse_events))
        .route("/api/build", post(build))
        .route("/api/run", post(run))
        .route("/api/stop", post(stop))
        .fallback_service(ui_assets)
        .with_state(state);

    let addr = SocketAddr::new(host, port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind GUI server at http://{addr}"))?;
    println!("GUI listening at http://{addr}");
    axum::serve(listener, app)
        .await
        .context("GUI server failed")?;
    Ok(())
}

async fn favicon() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn pipeline(State(state): State<AppState>) -> Json<PipelineDescription> {
    Json(PipelineDescription {
        name: if state.config.name.trim().is_empty() {
            "my-ci".to_string()
        } else {
            state.config.name.clone()
        },
        workflows: state
            .config
            .workflow
            .iter()
            .map(|wf| WorkflowSummary {
                name: wf.name.clone(),
                image: image_tag(&state.config, wf),
                depends_on: wf.depends_on.clone(),
                command: wf.command.is_some(),
            })
            .collect(),
    })
}

async fn sse_events(
    State(state): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(state.events.subscribe()).filter_map(|event| async move {
        match event {
            Ok(event) => {
                let data = serde_json::to_string(&event).unwrap_or_else(|err| {
                    format!(r#"{{"kind":"error","message":"failed to encode event: {err}"}}"#)
                });
                Some(Ok(Event::default().event("pipeline").data(data)))
            }
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn build(
    State(state): State<AppState>,
    Json(request): Json<PipelineRequest>,
) -> impl IntoResponse {
    start_pipeline(state, PipelineMode::Build, request.workflow).await
}

async fn run(
    State(state): State<AppState>,
    Json(request): Json<PipelineRequest>,
) -> impl IntoResponse {
    start_pipeline(state, PipelineMode::Run, request.workflow).await
}

async fn stop(State(state): State<AppState>) -> Json<PipelineResponse> {
    let mut active = state.active.lock().await;
    if let Some(handle) = active.take() {
        handle.abort();
        let _ = state.events.send(PipelineEvent::pipeline(
            EventKind::PipelineCancelled,
            "Pipeline cancelled",
        ));
        return Json(PipelineResponse {
            accepted: true,
            message: "Pipeline cancelled".to_string(),
        });
    }

    Json(PipelineResponse {
        accepted: false,
        message: "No active pipeline".to_string(),
    })
}

#[derive(Clone, Copy)]
enum PipelineMode {
    Build,
    Run,
}

async fn start_pipeline(
    state: AppState,
    mode: PipelineMode,
    workflow: Option<String>,
) -> Json<PipelineResponse> {
    let mut active = state.active.lock().await;
    if active.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return Json(PipelineResponse {
            accepted: false,
            message: "A pipeline is already running".to_string(),
        });
    }

    if active.as_ref().is_some_and(|handle| handle.is_finished()) {
        active.take();
    }

    let plan = match plan_for(&state.config, mode, workflow.as_deref()) {
        Ok(plan) => plan,
        Err(err) => {
            return Json(PipelineResponse {
                accepted: false,
                message: err.to_string(),
            });
        }
    };

    let worker_state = state.clone();
    let label = match mode {
        PipelineMode::Build => "Build",
        PipelineMode::Run => "Run",
    };
    let handle = tokio::spawn(async move {
        run_pipeline(worker_state, mode, plan).await;
    });
    *active = Some(handle);

    Json(PipelineResponse {
        accepted: true,
        message: format!("{label} started"),
    })
}

fn plan_for(
    config: &WorkflowFile,
    mode: PipelineMode,
    workflow: Option<&str>,
) -> Result<Vec<String>> {
    match mode {
        PipelineMode::Build => match workflow {
            Some(name) => resolve_build_plan(config, name),
            None => topological_order(config),
        },
        PipelineMode::Run => match workflow {
            Some(name) => {
                get_workflow(config, name)?;
                Ok(vec![name.to_string()])
            }
            None => topological_order(config),
        },
    }
}

async fn run_pipeline(state: AppState, mode: PipelineMode, targets: Vec<String>) {
    let _ = state.events.send(PipelineEvent::pipeline(
        EventKind::PipelineStarted,
        "Pipeline started",
    ));
    mark_pending(&state, &targets, mode);

    let result = match mode {
        PipelineMode::Build => run_build_plan(&state, &targets).await,
        PipelineMode::Run => run_run_plan(&state, &targets).await,
    };

    match result {
        Ok(()) => {
            let _ = state.events.send(PipelineEvent::pipeline(
                EventKind::PipelineFinished,
                "Pipeline finished",
            ));
        }
        Err(err) => {
            let _ = state
                .events
                .send(PipelineEvent::pipeline(EventKind::Error, err.to_string()));
        }
    }

    let mut active = state.active.lock().await;
    active.take();
}

fn mark_pending(state: &AppState, targets: &[String], mode: PipelineMode) {
    let phase = match mode {
        PipelineMode::Build => WorkflowPhase::Build,
        PipelineMode::Run => WorkflowPhase::Run,
    };
    for target in targets {
        let _ = state.events.send(PipelineEvent::workflow(
            target.clone(),
            phase.clone(),
            WorkflowStatus::Pending,
            "Queued",
        ));
    }
}

async fn run_build_plan(state: &AppState, targets: &[String]) -> Result<()> {
    for target in targets {
        let wf = get_workflow(&state.config, target)?;
        build_workflow_with_events(&state.docker, &state.config, wf, |event| {
            let _ = state.events.send(event);
        })
        .await?;
    }
    Ok(())
}

async fn run_run_plan(state: &AppState, targets: &[String]) -> Result<()> {
    for target in targets {
        for dep in resolve_build_plan(&state.config, target)? {
            let wf = get_workflow(&state.config, &dep)?;
            build_workflow_with_events(&state.docker, &state.config, wf, |event| {
                let _ = state.events.send(event);
            })
            .await?;
        }
    }

    for target in targets {
        let wf = get_workflow(&state.config, target)?;
        if wf.command.is_some() {
            run_workflow_with_events(&state.docker, &state.config, wf, |event| {
                let _ = state.events.send(event);
            })
            .await?;
        } else {
            let _ = state.events.send(PipelineEvent::workflow(
                wf.name.clone(),
                WorkflowPhase::Run,
                WorkflowStatus::Skipped,
                "No command configured",
            ));
        }
    }
    Ok(())
}
