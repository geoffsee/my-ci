use anyhow::{Context, Result, anyhow, bail};
use bollard::Docker;
use bollard::container::{
    AttachContainerOptions, Config as ContainerConfig, CreateContainerOptions, LogOutput,
    RemoveContainerOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::models::HostConfig;
use futures_util::StreamExt;

use crate::config::{WorkflowConfig, WorkflowFile, image_tag};

pub async fn run_workflow(
    docker: &Docker,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
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

    println!("Running '{}' as container '{}'", wf.name, container_name);

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
        Ok(resp) => resp.id,
        Err(err) => {
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

    while let Some(output) = attach_stream.output.next().await {
        let output = output.context("container attach stream failed")?;
        print_log_output(output);
    }

    let mut wait_stream = docker.wait_container(
        &id,
        Some(WaitContainerOptions {
            condition: "not-running",
        }),
    );

    if let Some(wait_result) = wait_stream.next().await {
        let status = wait_result.context("failed while waiting for container")?;
        if status.status_code != 0 {
            bail!(
                "workflow '{}' exited with status {}",
                wf.name,
                status.status_code
            );
        }
    }

    Ok(())
}

fn print_log_output(output: LogOutput) {
    match output {
        LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
            print!("{}", String::from_utf8_lossy(&message))
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
