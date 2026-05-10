use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::container::{
    AttachContainerOptions, Config as ContainerConfig, CreateContainerOptions,
    DownloadFromContainerOptions, LogOutput, RemoveContainerOptions, StartContainerOptions,
    WaitContainerOptions,
};
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use futures_util::{StreamExt, TryStreamExt};
use my_ci_macros::trace;
use tracing::{debug, error, info};

use crate::artifacts::{artifact_subdir_label, unpack_docker_copy_archive};
use crate::config::{WorkflowConfig, WorkflowFile, image_tag};
use crate::events::{PipelineEvent, WorkflowPhase, WorkflowStatus};
use crate::oci::{OciRuntime, apple_container_command, exit_status_label, run_streaming_command};
use std::path::Path;

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
    let container_env = if wf.env.is_empty() {
        None
    } else {
        Some(wf.env.clone())
    };

    let artifact_host_dir = config.artifacts_dir.join(&wf.name);
    if wf.artifact_bind.is_some() || !wf.artifacts.is_empty() {
        tokio::fs::create_dir_all(&artifact_host_dir)
            .await
            .with_context(|| format!("create artifact dir {}", artifact_host_dir.display()))?;
    }

    let bind_host = if wf.artifact_bind.is_some() {
        Some(std::fs::canonicalize(&artifact_host_dir).with_context(|| {
            format!(
                "resolve artifact host directory {}",
                artifact_host_dir.display()
            )
        })?)
    } else {
        None
    };

    let delayed_removal = !wf.artifacts.is_empty();
    let mounts = match (&wf.artifact_bind, &bind_host) {
        (Some(target), Some(host)) => Some(vec![Mount {
            target: Some(target.clone()),
            source: Some(host.to_string_lossy().to_string()),
            typ: Some(MountTypeEnum::BIND),
            ..Default::default()
        }]),
        (Some(_), None) => {
            bail!(
                "workflow '{}': could not prepare host bind mount under {}",
                wf.name,
                artifact_host_dir.display()
            );
        }
        _ => None,
    };

    let host_config = HostConfig {
        auto_remove: Some(!delayed_removal),
        mounts,
        ..Default::default()
    };

    let container_config = ContainerConfig {
        image: Some(image.clone()),
        cmd: Some(cmd.clone()),
        env: container_env.clone(),
        host_config: Some(host_config),
        ..Default::default()
    };

    let container_name = format!("my-ci-{}", wf.name);
    info!(
        workflow = %wf.name,
        image = %image,
        container = %container_name,
        argv = ?cmd,
        env_count = wf.env.len(),
        artifact_paths = wf.artifacts.len(),
        artifact_bind = ?wf.artifact_bind,
        "starting Docker-compatible workflow container"
    );

    println!("Running '{}' as container '{}'", wf.name, container_name);
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Run,
        WorkflowStatus::Running,
        format!("Running as container '{container_name}'"),
    ));

    if let Some(bind) = &wf.artifact_bind {
        emit(PipelineEvent::log(
            wf.name.clone(),
            WorkflowPhase::Run,
            format!(
                "Host directory {} is mounted at {bind}\n",
                artifact_host_dir.display()
            ),
        ));
    }

    let create = docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name.clone(),
                platform: None,
            }),
            container_config.clone(),
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
                    container_config,
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

    let exit_code = if let Some(wait_result) = wait_stream.next().await {
        wait_result
            .context("failed while waiting for container")?
            .status_code
    } else {
        error!(
            workflow = %wf.name,
            container = %container_name,
            "wait_container returned no exit record"
        );
        -1
    };

    debug!(
        workflow = %wf.name,
        container = %container_name,
        status = exit_code,
        "workflow container exited"
    );

    let success = exit_code == 0;

    if success && !wf.artifacts.is_empty() {
        copy_workflow_artifacts_from_container(
            docker,
            &container_name,
            wf,
            &artifact_host_dir,
            &emit,
        )
        .await?;
    }

    if delayed_removal {
        let _ = docker
            .remove_container(
                &container_name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
    }

    if !success {
        error!(
            workflow = %wf.name,
            container = %container_name,
            status = exit_code,
            "workflow container failed"
        );
        emit(PipelineEvent::error(
            wf.name.clone(),
            WorkflowPhase::Run,
            format!("Exited with status {exit_code}"),
        ));
        bail!("workflow '{}' exited with status {}", wf.name, exit_code);
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

async fn copy_workflow_artifacts_from_container(
    docker: &Docker,
    container_name: &str,
    wf: &WorkflowConfig,
    artifact_host_dir: &Path,
    emit: &impl Fn(PipelineEvent),
) -> Result<()> {
    for path in &wf.artifacts {
        let dest_subdir = artifact_host_dir.join(artifact_subdir_label(path));
        tokio::fs::create_dir_all(&dest_subdir)
            .await
            .with_context(|| format!("create artifact subdirectory {}", dest_subdir.display()))?;
        let bytes = docker
            .download_from_container(
                container_name,
                Some(DownloadFromContainerOptions { path: path.clone() }),
            )
            .try_fold(Vec::new(), |mut acc, chunk| async move {
                acc.extend_from_slice(&chunk);
                Ok(acc)
            })
            .await
            .with_context(|| format!("download files from container path '{path}'"))?;
        unpack_docker_copy_archive(&bytes, &dest_subdir).with_context(|| {
            format!(
                "unpack artifact archive for container path '{path}' into {}",
                dest_subdir.display()
            )
        })?;
        info!(
            workflow = %wf.name,
            container_path = %path,
            host_path = %dest_subdir.display(),
            "wrote workflow artifact"
        );
        emit(PipelineEvent::log(
            wf.name.clone(),
            WorkflowPhase::Run,
            format!("Saved artifact `{path}` -> {}\n", dest_subdir.display()),
        ));
    }
    Ok(())
}

#[trace(level = "debug", skip(config, wf, emit), err, fields(workflow = %wf.name))]
async fn run_workflow_with_apple_container(
    config: &WorkflowFile,
    wf: &WorkflowConfig,
    emit: impl Fn(PipelineEvent),
) -> Result<()> {
    if !wf.artifacts.is_empty() {
        bail!(
            "workflow '{}' lists `artifacts` paths; copying from the container requires Docker or Podman. \
Use `artifact_bind` so the task writes files into a mounted host directory, or choose a Docker-compatible runtime.",
            wf.name
        );
    }

    let image = image_tag(config, wf);
    let cmd = wf
        .command
        .clone()
        .ok_or_else(|| anyhow!("workflow '{}' has no command configured", wf.name))?;
    let container_name = format!("my-ci-{}", wf.name);
    let artifact_host_dir = config.artifacts_dir.join(&wf.name);

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

    let bind_mount = if let Some(bind) = wf.artifact_bind.as_ref() {
        tokio::fs::create_dir_all(&artifact_host_dir)
            .await
            .with_context(|| format!("create artifact dir {}", artifact_host_dir.display()))?;
        let host_abs = std::fs::canonicalize(&artifact_host_dir).with_context(|| {
            format!(
                "resolve artifact host directory {}",
                artifact_host_dir.display()
            )
        })?;
        emit(PipelineEvent::log(
            wf.name.clone(),
            WorkflowPhase::Run,
            format!(
                "Host directory {} is mounted at {bind}\n",
                artifact_host_dir.display()
            ),
        ));
        Some((bind.clone(), host_abs))
    } else {
        None
    };

    let mut command = apple_container_command();
    command
        .arg("run")
        .arg("--rm")
        .arg("--name")
        .arg(&container_name);
    for env in &wf.env {
        command.arg("-e").arg(env);
    }
    if let Some((bind, host_abs)) = &bind_mount {
        command
            .arg("-v")
            .arg(format!("{}:{bind}", host_abs.display()));
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
