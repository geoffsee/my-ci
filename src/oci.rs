use std::path::Path;
use std::process::{Command as StdCommand, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};
use bollard::{API_DEFAULT_VERSION, Docker};
use clap::ValueEnum;
use my_ci_macros::trace;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{debug, info, warn};

const SOCKET_PROVIDERS: [OciProvider; 2] = [OciProvider::Docker, OciProvider::Podman];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OciProvider {
    Docker,
    Podman,
    AppleContainer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeChoice {
    Auto,
    Docker,
    Podman,
    AppleContainer,
}

#[derive(Clone)]
pub enum OciRuntime {
    DockerSocket { client: Docker },
    AppleContainer,
}

pub fn select_oci_provider(runtime: RuntimeChoice) -> OciProvider {
    match runtime {
        RuntimeChoice::Auto => {
            info!("runtime selection: auto mode requested");
            detect_oci_provider().unwrap_or_else(|| {
                info!(
                    provider = ?OciProvider::Docker,
                    "runtime selection: auto detection found no provider; using Docker default"
                );
                OciProvider::Docker
            })
        }
        RuntimeChoice::Docker => {
            info!(
                provider = ?OciProvider::Docker,
                "runtime selection: provider explicitly requested"
            );
            OciProvider::Docker
        }
        RuntimeChoice::Podman => {
            info!(
                provider = ?OciProvider::Podman,
                "runtime selection: provider explicitly requested"
            );
            OciProvider::Podman
        }
        RuntimeChoice::AppleContainer => {
            info!(
                provider = ?OciProvider::AppleContainer,
                "runtime selection: provider explicitly requested"
            );
            OciProvider::AppleContainer
        }
    }
}

#[trace(level = "debug", ret)]
pub fn detect_oci_provider() -> Option<OciProvider> {
    info!("runtime auto-selection: starting provider detection");

    if cfg!(target_os = "macos") {
        info!("runtime auto-selection: macOS detected; checking Apple container first");
        if command_exists("container") {
            info!("runtime auto-selection: Apple container CLI found; checking service state");
            match apple_container_system_running() {
                Ok(true) => {
                    info!(
                        provider = ?OciProvider::AppleContainer,
                        "runtime auto-selection: Apple container service is running; selecting provider"
                    );
                    return Some(OciProvider::AppleContainer);
                }
                Ok(false) => {
                    warn!(
                        "runtime auto-selection: Apple container service is not running; run `container system start` to use it"
                    );
                    info!("runtime auto-selection: falling back to Docker/Podman socket detection");
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "runtime auto-selection: failed to check Apple container service; falling back to Docker/Podman socket detection"
                    );
                }
            }
        } else {
            info!(
                "runtime auto-selection: Apple container CLI was not found in PATH; skipping Apple container"
            );
        }
    } else {
        info!(
            os = std::env::consts::OS,
            "runtime auto-selection: non-macOS platform; skipping Apple container"
        );
    }

    for &provider in &SOCKET_PROVIDERS {
        let socket = get_oci_socket_addr(provider).expect("socket provider");
        info!(
            ?provider,
            socket, "runtime auto-selection: checking OCI socket"
        );
        if Path::new(socket).exists() {
            info!(
                ?provider,
                socket, "runtime auto-selection: socket exists; selecting provider"
            );
            return Some(provider);
        }
        info!(
            ?provider,
            socket, "runtime auto-selection: socket does not exist; continuing"
        );
    }

    warn!("runtime auto-selection: no runtime provider detected");
    None
}

#[trace(level = "debug", err, fields(provider = ?provider))]
pub fn connect_oci(provider: OciProvider) -> Result<OciRuntime> {
    match provider {
        OciProvider::Docker | OciProvider::Podman => {
            let socket = get_oci_socket_addr(provider).expect("socket provider");
            debug!(?provider, socket, "connecting to OCI socket");
            let client = Docker::connect_with_unix(socket, 120, API_DEFAULT_VERSION)
                .with_context(|| format!("failed to connect to socket at {socket}"))?;
            Ok(OciRuntime::DockerSocket { client })
        }
        OciProvider::AppleContainer => {
            debug!("validating Apple container CLI");
            if !cfg!(target_os = "macos") {
                bail!("Apple container is only supported on macOS");
            }
            if !command_exists("container") {
                bail!("Apple container CLI not found in PATH");
            }
            ensure_apple_container_system_running()?;
            Ok(OciRuntime::AppleContainer)
        }
    }
}

pub fn get_oci_socket_addr(oci_provider: OciProvider) -> Option<&'static str> {
    match oci_provider {
        OciProvider::Docker => Some("/var/run/docker.sock"),
        OciProvider::Podman => Some("/var/run/podman/podman.sock"),
        OciProvider::AppleContainer => None,
    }
}

pub fn describe_oci_target(oci_provider: OciProvider) -> &'static str {
    match oci_provider {
        OciProvider::Docker => "/var/run/docker.sock",
        OciProvider::Podman => "/var/run/podman/podman.sock",
        OciProvider::AppleContainer => "Apple container CLI",
    }
}

#[cfg(test)]
pub fn provider_name(oci_provider: OciProvider) -> &'static str {
    match oci_provider {
        OciProvider::Docker => "docker",
        OciProvider::Podman => "podman",
        OciProvider::AppleContainer => "apple-container",
    }
}

pub fn apple_container_command() -> Command {
    Command::new("container")
}

#[trace(level = "debug", skip_all, err)]
pub async fn run_streaming_command(
    mut command: Command,
    emit: impl Fn(String),
) -> Result<ExitStatus> {
    debug!("spawning streaming runtime command");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().context("failed to spawn command")?;
    let mut stdout = child.stdout.take().context("failed to capture stdout")?;
    let mut stderr = child.stderr.take().context("failed to capture stderr")?;

    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut stdout_buf = vec![0; 8192];
    let mut stderr_buf = vec![0; 8192];

    while !stdout_done || !stderr_done {
        tokio::select! {
            read = stdout.read(&mut stdout_buf), if !stdout_done => {
                let n = read.context("failed to read command stdout")?;
                if n == 0 {
                    stdout_done = true;
                } else {
                    let message = String::from_utf8_lossy(&stdout_buf[..n]).to_string();
                    tracing::trace!(stream = "stdout", bytes = n, output = %message, "runtime command output");
                    emit(message);
                }
            }
            read = stderr.read(&mut stderr_buf), if !stderr_done => {
                let n = read.context("failed to read command stderr")?;
                if n == 0 {
                    stderr_done = true;
                } else {
                    let message = String::from_utf8_lossy(&stderr_buf[..n]).to_string();
                    tracing::trace!(stream = "stderr", bytes = n, output = %message, "runtime command output");
                    emit(message);
                }
            }
        }
    }

    let status = child.wait().await.context("failed to wait for command")?;
    debug!(status = %exit_status_label(status), success = status.success(), "runtime command finished");
    Ok(status)
}

pub fn exit_status_label(status: ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| status.to_string())
}

#[trace(level = "debug", err)]
fn ensure_apple_container_system_running() -> Result<()> {
    debug!("checking Apple container system service");
    if !apple_container_system_running()? {
        bail!("Apple container system service is not running; run `container system start`");
    }
    Ok(())
}

#[trace(level = "trace", ret, err)]
fn apple_container_system_running() -> Result<bool> {
    let output = StdCommand::new("container")
        .args(["system", "status"])
        .output()
        .context("failed to check Apple container system status")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let status_output = format!("{stderr}{stdout}");
    if status_output.contains("not running") {
        return Ok(false);
    }
    if !output.status.success() {
        bail!("failed to check Apple container system status: {status_output}");
    }
    Ok(true)
}

#[trace(level = "trace", ret)]
fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(command);
            candidate.is_file()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_addr_for_docker() {
        assert_eq!(
            get_oci_socket_addr(OciProvider::Docker),
            Some("/var/run/docker.sock")
        );
    }

    #[test]
    fn socket_addr_for_podman() {
        assert_eq!(
            get_oci_socket_addr(OciProvider::Podman),
            Some("/var/run/podman/podman.sock")
        );
    }

    #[test]
    fn apple_container_has_no_socket_addr() {
        assert_eq!(get_oci_socket_addr(OciProvider::AppleContainer), None);
    }

    #[test]
    fn provider_names_are_cli_stable() {
        assert_eq!(provider_name(OciProvider::Docker), "docker");
        assert_eq!(provider_name(OciProvider::Podman), "podman");
        assert_eq!(
            provider_name(OciProvider::AppleContainer),
            "apple-container"
        );
    }

    #[test]
    fn detect_returns_none_when_no_sockets() {
        let docker_exists = Path::new("/var/run/docker.sock").exists();
        let podman_exists = Path::new("/var/run/podman/podman.sock").exists();
        let apple_running = cfg!(target_os = "macos")
            && command_exists("container")
            && apple_container_system_running().unwrap_or(false);
        let detected = detect_oci_provider();
        if !docker_exists && !podman_exists && !apple_running {
            assert!(detected.is_none());
        } else {
            assert!(detected.is_some());
        }
    }
}
