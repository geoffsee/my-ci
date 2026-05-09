use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::container::{
    AttachContainerOptions, Config as ContainerConfig, CreateContainerOptions, LogOutput,
    RemoveContainerOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::models::HostConfig;
use futures_util::StreamExt;
use my_ci_macros::trace;
use tracing::{debug, error, info};

use crate::config::{WorkflowConfig, WorkflowFile, image_tag};
use crate::events::{PipelineEvent, WorkflowPhase, WorkflowStatus};
use crate::oci::{OciRuntime, apple_container_command, exit_status_label, run_streaming_command};

#[trace(level = "debug", skip(runtime, config, wf), err, fields(workflow = %wf.name))]
pub async fn run_workflow(
    runtime: &OciRuntime,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
) -> Result<()> {
    run_workflow_with_events(runtime, config, wf, |event| {
        if let PipelineEvent {
            kind: crate::events::EventKind::Log,
            message,
            ..
        } = event
        {
            print!("{message}");
        }
    })
    .await
}

#[trace(level = "debug", skip(runtime, config, wf, emit), err, fields(workflow = %wf.name))]
pub async fn run_workflow_with_events(
    runtime: &OciRuntime,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
    emit: impl Fn(PipelineEvent),
) -> Result<()> {
    match runtime {
        OciRuntime::DockerSocket { client } => {
            run_workflow_with_docker(client, config, wf, emit).await
        }
        OciRuntime::AppleContainer => run_workflow_with_apple_container(config, wf, emit).await,
    }
}

#[trace(level = "debug", skip(docker, config, wf, emit), err, fields(workflow = %wf.name))]
async fn run_workflow_with_docker(
    docker: &Docker,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
    emit: impl Fn(PipelineEvent),
) -> Result<()> {
    let image = image_tag(config, wf);
    let cmd = wf
        .command
        .clone()
        .ok_or_else(|| anyhow!("workflow '{}' has no command configured", wf.name))?;
    let env = if wf.env.is_empty() {
        None
    } else {
        Some(wf.env.clone())
    };
    let container_name = format!("my-ci-{}", wf.name);
    info!(
        workflow = %wf.name,
        image = %image,
        container = %container_name,
        argv = ?cmd,
        env_count = wf.env.len(),
        "starting Docker-compatible workflow container"
    );

    println!("Running '{}' as container '{}'", wf.name, container_name);
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Run,
        WorkflowStatus::Running,
        format!("Running as container '{container_name}'"),
    ));

    let create = docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name.clone(),
                platform: None,
            }),
            ContainerConfig {
                image: Some(image.clone()),
                cmd: Some(cmd),
                env,
                host_config: Some(HostConfig {
                    auto_remove: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

    let id = match create {
        Ok(resp) => {
            debug!(workflow = %wf.name, container = %container_name, id = %resp.id, "created workflow container");
            resp.id
        }
        Err(err) => {
            debug!(
                workflow = %wf.name,
                container = %container_name,
                error = %err,
                "container create failed; attempting cleanup and retry"
            );
            let _ = docker
                .remove_container(
                    &container_name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            let retried = docker
                .create_container(
                    Some(CreateContainerOptions {
                        name: container_name.clone(),
                        platform: None,
                    }),
                    ContainerConfig {
                        image: Some(image),
                        cmd: Some(wf.command.clone().unwrap_or_default()),
                        env: if wf.env.is_empty() {
                            None
                        } else {
                            Some(wf.env.clone())
                        },
                        host_config: Some(HostConfig {
                            auto_remove: Some(true),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )
                .await
                .with_context(|| format!("failed to create container after cleanup: {err}"))?;
            debug!(
                workflow = %wf.name,
                container = %container_name,
                id = %retried.id,
                "created workflow container after cleanup"
            );
            retried.id
        }
    };

    let mut attach_stream = docker
        .attach_container(
            &id,
            Some(AttachContainerOptions::<String> {
                stdout: Some(true),
                stderr: Some(true),
                stream: Some(true),
                logs: Some(true),
                stdin: Some(false),
                detach_keys: None,
            }),
        )
        .await
        .context("failed to attach to container logs")?;

    docker
        .start_container(&id, None::<StartContainerOptions<String>>)
        .await
        .context("failed to start container")?;
    debug!(workflow = %wf.name, container = %container_name, id = %id, "started workflow container");

    while let Some(output) = attach_stream.output.next().await {
        let output = output.context("container attach stream failed")?;
        emit_log_output(&wf.name, output, &emit);
    }

    let mut wait_stream = docker.wait_container(
        &id,
        Some(WaitContainerOptions {
            condition: "not-running",
        }),
    );

    if let Some(wait_result) = wait_stream.next().await {
        let status = wait_result.context("failed while waiting for container")?;
        debug!(
            workflow = %wf.name,
            container = %container_name,
            status = status.status_code,
            "workflow container exited"
        );
        if status.status_code != 0 {
            error!(
                workflow = %wf.name,
                container = %container_name,
                status = status.status_code,
                "workflow container failed"
            );
            emit(PipelineEvent::error(
                wf.name.clone(),
                WorkflowPhase::Run,
                format!("Exited with status {}", status.status_code),
            ));
            bail!(
                "workflow '{}' exited with status {}",
                wf.name,
                status.status_code
            );
        }
    }

    info!(workflow = %wf.name, container = %container_name, "workflow container completed");
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Run,
        WorkflowStatus::Succeeded,
        "Run completed",
    ));
    Ok(())
}

#[trace(level = "debug", skip(config, wf, emit), err, fields(workflow = %wf.name))]
async fn run_workflow_with_apple_container(
    config: &WorkflowFile,
    wf: &WorkflowConfig,
    emit: impl Fn(PipelineEvent),
) -> Result<()> {
    let image = image_tag(config, wf);
    let cmd = wf
        .command
        .clone()
        .ok_or_else(|| anyhow!("workflow '{}' has no command configured", wf.name))?;
    let container_name = format!("my-ci-{}", wf.name);
    info!(
        workflow = %wf.name,
        image = %image,
        container = %container_name,
        argv = ?cmd,
        env_count = wf.env.len(),
        "starting Apple container workflow"
    );

    println!("Running '{}' as container '{}'", wf.name, container_name);
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Run,
        WorkflowStatus::Running,
        format!("Running as container '{container_name}'"),
    ));

    let mut command = apple_container_command();
    command
        .arg("run")
        .arg("--rm")
        .arg("--name")
        .arg(&container_name);
    for env in &wf.env {
        command.arg("-e").arg(env);
    }
    command.arg(&image);
    command.args(cmd);

    let status = run_streaming_command(command, |message| {
        emit(PipelineEvent::log(
            wf.name.clone(),
            WorkflowPhase::Run,
            message,
        ));
    })
    .await
    .context("container run failed")?;

    if !status.success() {
        let status = exit_status_label(status);
        error!(
            workflow = %wf.name,
            container = %container_name,
            status = %status,
            "Apple container workflow failed"
        );
        emit(PipelineEvent::error(
            wf.name.clone(),
            WorkflowPhase::Run,
            format!("Exited with status {status}"),
        ));
        bail!("workflow '{}' exited with status {status}", wf.name);
    }

    info!(workflow = %wf.name, container = %container_name, "Apple container workflow completed");
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Run,
        WorkflowStatus::Succeeded,
        "Run completed",
    ));
    Ok(())
}

#[cfg(test)]
fn print_log_output(output: LogOutput) {
    match output {
        LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
            print!("{}", String::from_utf8_lossy(&message))
        }
        LogOutput::StdIn { .. } | LogOutput::Console { .. } => {}
    }
}

fn emit_log_output(workflow: &str, output: LogOutput, emit: &impl Fn(PipelineEvent)) {
    match output {
        LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
            let message = String::from_utf8_lossy(&message).to_string();
            tracing::trace!(workflow, output = %message, "runtime container output");
            emit(PipelineEvent::log(
                workflow.to_string(),
                WorkflowPhase::Run,
                message,
            ));
        }
        LogOutput::StdIn { .. } | LogOutput::Console { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn print_log_output_handles_all_streams() {
        // Smoke test that all variants are accepted without panic.
        print_log_output(LogOutput::StdOut {
            message: Bytes::from_static(b"hello"),
        });
        print_log_output(LogOutput::StdErr {
            message: Bytes::from_static(b"err"),
        });
        print_log_output(LogOutput::StdIn {
            message: Bytes::from_static(b"in"),
        });
        print_log_output(LogOutput::Console {
            message: Bytes::from_static(b"console"),
        });
    }
}
