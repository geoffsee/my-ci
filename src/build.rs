use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bollard::Docker;
use bollard::image::BuildImageOptions;
use bytes::Bytes;
use futures_util::StreamExt;
use my_ci_macros::trace;
use tar::{Builder, Header};
use tracing::{debug, error, info};
use walkdir::WalkDir;

use crate::config::{WorkflowConfig, WorkflowFile, image_tag, normalize_context};
use crate::events::{PipelineEvent, WorkflowPhase, WorkflowStatus};
use crate::oci::{OciRuntime, apple_container_command, exit_status_label, run_streaming_command};

#[trace(level = "debug", skip(runtime, config, wf), err, fields(workflow = %wf.name))]
pub async fn build_workflow(
    runtime: &OciRuntime,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
) -> Result<()> {
    build_workflow_with_events(runtime, config, wf, |event| {
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
pub async fn build_workflow_with_events(
    runtime: &OciRuntime,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
    emit: impl Fn(PipelineEvent),
) -> Result<()> {
    match runtime {
        OciRuntime::DockerSocket { client } => {
            build_workflow_with_docker(client, config, wf, emit).await
        }
        OciRuntime::AppleContainer => build_workflow_with_apple_container(config, wf, emit).await,
    }
}

#[trace(level = "debug", skip(docker, config, wf, emit), err, fields(workflow = %wf.name))]
async fn build_workflow_with_docker(
    docker: &Docker,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
    emit: impl Fn(PipelineEvent),
) -> Result<()> {
    let context = normalize_context(&wf.context);
    let image_tag = image_tag(config, wf);
    info!(
        workflow = %wf.name,
        image = %image_tag,
        context = %context.display(),
        "starting Docker-compatible build"
    );
    println!("Building '{}' from {}", wf.name, context.display());
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Build,
        WorkflowStatus::Running,
        format!("Building from {}", context.display()),
    ));

    let tar_path = write_temp_build_context(&context, &wf.instructions)?;
    debug!(workflow = %wf.name, tar_path = %tar_path.display(), "created build context archive");
    let archive_bytes = std::fs::read(&tar_path)
        .with_context(|| format!("failed to read temp build context {}", tar_path.display()))?;
    debug!(
        workflow = %wf.name,
        image = %image_tag,
        bytes = archive_bytes.len(),
        "submitting build context to runtime"
    );
    let body = Bytes::from(archive_bytes);

    let options = BuildImageOptions {
        dockerfile: "Dockerfile".to_string(),
        t: image_tag.clone(),
        rm: true,
        forcerm: true,
        pull: true,
        ..Default::default()
    };

    let mut output = docker.build_image(options, None, Some(body));
    while let Some(item) = output.next().await {
        let chunk = item.context("docker build stream failed")?;
        if let Some(error) = chunk.error {
            error!(workflow = %wf.name, error = %error, "runtime build stream returned an error");
            emit(PipelineEvent::error(
                wf.name.clone(),
                WorkflowPhase::Build,
                error.clone(),
            ));
            bail!("build failed for '{}': {error}", wf.name);
        }
        if let Some(stream) = chunk.stream {
            tracing::trace!(workflow = %wf.name, output = %stream, "runtime build output");
            emit(PipelineEvent::log(
                wf.name.clone(),
                WorkflowPhase::Build,
                stream.clone(),
            ));
        }
    }

    std::fs::remove_file(&tar_path).ok();
    info!(workflow = %wf.name, image = %image_tag, "Docker-compatible build completed");
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Build,
        WorkflowStatus::Succeeded,
        format!("Built {image_tag}"),
    ));
    Ok(())
}

#[trace(level = "debug", skip(config, wf, emit), err, fields(workflow = %wf.name))]
async fn build_workflow_with_apple_container(
    config: &WorkflowFile,
    wf: &WorkflowConfig,
    emit: impl Fn(PipelineEvent),
) -> Result<()> {
    let context = normalize_context(&wf.context);
    let image_tag = image_tag(config, wf);
    info!(
        workflow = %wf.name,
        image = %image_tag,
        context = %context.display(),
        "starting Apple container build"
    );
    println!("Building '{}' from {}", wf.name, context.display());
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Build,
        WorkflowStatus::Running,
        format!("Building from {}", context.display()),
    ));

    let build_dir = write_temp_build_directory(&context, &wf.instructions)?;
    let dockerfile = build_dir.join("Dockerfile");
    debug!(
        workflow = %wf.name,
        build_dir = %build_dir.display(),
        dockerfile = %dockerfile.display(),
        "created filesystem build context for Apple container"
    );
    let mut command = apple_container_command();
    command
        .arg("build")
        .arg("--progress")
        .arg("plain")
        .arg("-f")
        .arg(&dockerfile)
        .arg("-t")
        .arg(&image_tag)
        .arg(&build_dir);

    let status = run_streaming_command(command, |message| {
        emit(PipelineEvent::log(
            wf.name.clone(),
            WorkflowPhase::Build,
            message,
        ));
    })
    .await;

    fs::remove_dir_all(&build_dir).ok();
    let status = status.context("container build failed")?;
    if !status.success() {
        let status = exit_status_label(status);
        error!(workflow = %wf.name, status = %status, "Apple container build failed");
        emit(PipelineEvent::error(
            wf.name.clone(),
            WorkflowPhase::Build,
            format!("container build exited with status {status}"),
        ));
        bail!(
            "build failed for '{}': container build exited with status {status}",
            wf.name
        );
    }

    info!(workflow = %wf.name, image = %image_tag, "Apple container build completed");
    emit(PipelineEvent::workflow(
        wf.name.clone(),
        WorkflowPhase::Build,
        WorkflowStatus::Succeeded,
        format!("Built {image_tag}"),
    ));
    Ok(())
}

#[trace(level = "debug", err, fields(context = %context.display(), dockerfile_bytes = dockerfile.len()))]
fn write_temp_build_context(context: &Path, dockerfile: &str) -> Result<PathBuf> {
    let mut archive_path = std::env::temp_dir();
    archive_path.push(format!(
        "my-ci-{}.tar",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let tar_file = File::create(&archive_path)
        .with_context(|| format!("failed to create {}", archive_path.display()))?;
    let mut builder = Builder::new(tar_file);
    append_directory_to_tar(context, &mut builder)?;
    append_virtual_file(&mut builder, "Dockerfile", dockerfile.as_bytes())?;
    builder
        .finish()
        .context("failed to finalize build archive")?;
    debug!(archive_path = %archive_path.display(), "finished build context archive");
    Ok(archive_path)
}

#[trace(level = "trace", skip(builder), err, fields(context = %context.display()))]
fn append_directory_to_tar(context: &Path, builder: &mut Builder<File>) -> Result<()> {
    for entry in WalkDir::new(context) {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(context).with_context(|| {
            format!(
                "failed to strip context prefix '{}' from '{}'",
                context.display(),
                path.display()
            )
        })?;
        if should_skip_context_entry(rel) {
            continue;
        }

        if entry.file_type().is_dir() {
            tracing::trace!(path = %path.display(), rel = %rel.display(), "adding directory to build archive");
            builder
                .append_dir(rel, path)
                .with_context(|| format!("failed to append directory {}", path.display()))?;
            continue;
        }

        if entry.file_type().is_file() {
            tracing::trace!(path = %path.display(), rel = %rel.display(), "adding file to build archive");
            let mut file = File::open(path)
                .with_context(|| format!("failed to open context file {}", path.display()))?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)
                .with_context(|| format!("failed to read context file {}", path.display()))?;
            append_virtual_file(builder, rel, &data)?;
        }
    }
    Ok(())
}

#[trace(level = "debug", err, fields(context = %context.display(), dockerfile_bytes = dockerfile.len()))]
fn write_temp_build_directory(context: &Path, dockerfile: &str) -> Result<PathBuf> {
    let mut build_dir = std::env::temp_dir();
    build_dir.push(format!(
        "my-ci-build-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir(&build_dir)
        .with_context(|| format!("failed to create {}", build_dir.display()))?;

    if let Err(err) = copy_directory_to_build_context(context, &build_dir) {
        error!(build_dir = %build_dir.display(), error = %err, "failed to copy build context");
        fs::remove_dir_all(&build_dir).ok();
        return Err(err);
    }

    let dockerfile_path = build_dir.join("Dockerfile");
    if let Err(err) = fs::write(&dockerfile_path, dockerfile)
        .with_context(|| format!("failed to write {}", dockerfile_path.display()))
    {
        error!(build_dir = %build_dir.display(), error = %err, "failed to write generated Dockerfile");
        fs::remove_dir_all(&build_dir).ok();
        return Err(err);
    }

    debug!(build_dir = %build_dir.display(), "finished filesystem build context");
    Ok(build_dir)
}

#[trace(level = "trace", err, fields(context = %context.display(), build_dir = %build_dir.display()))]
fn copy_directory_to_build_context(context: &Path, build_dir: &Path) -> Result<()> {
    for entry in WalkDir::new(context) {
        let entry = entry?;
        let path = entry.path();
        if path.starts_with(build_dir) {
            continue;
        }
        let rel = path.strip_prefix(context).with_context(|| {
            format!(
                "failed to strip context prefix '{}' from '{}'",
                context.display(),
                path.display()
            )
        })?;
        if should_skip_context_entry(rel) {
            continue;
        }

        let dest = build_dir.join(rel);
        if entry.file_type().is_dir() {
            tracing::trace!(source = %path.display(), dest = %dest.display(), "copying build context directory");
            fs::create_dir_all(&dest)
                .with_context(|| format!("failed to create directory {}", dest.display()))?;
            continue;
        }

        if entry.file_type().is_file() {
            tracing::trace!(source = %path.display(), dest = %dest.display(), "copying build context file");
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }
            fs::copy(path, &dest).with_context(|| {
                format!(
                    "failed to copy context file '{}' to '{}'",
                    path.display(),
                    dest.display()
                )
            })?;
        }
    }
    Ok(())
}

#[trace(level = "trace", ret, fields(rel = %rel.display()))]
fn should_skip_context_entry(rel: &Path) -> bool {
    rel.as_os_str().is_empty() || rel.starts_with(".git") || rel.starts_with("target")
}

#[trace(level = "trace", skip(builder, contents), err, fields(rel = %rel.as_ref().display(), bytes = contents.len()))]
fn append_virtual_file(
    builder: &mut Builder<File>,
    rel: impl AsRef<Path>,
    contents: &[u8],
) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    let mut cursor = Cursor::new(contents);
    builder
        .append_data(&mut header, rel, &mut cursor)
        .context("failed to append file to tar")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Read;
    use tar::Archive;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "my-ci-build-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn read_archive(path: &Path) -> HashMap<String, Vec<u8>> {
        let mut map = HashMap::new();
        let file = File::open(path).unwrap();
        let mut archive = Archive::new(file);
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().display().to_string();
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).unwrap();
            map.insert(name, buf);
        }
        map
    }

    #[test]
    fn tar_includes_dockerfile_and_context_files() {
        let dir = tempdir();
        std::fs::write(dir.join("hello.txt"), b"hi").unwrap();
        std::fs::create_dir(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/data.txt"), b"data").unwrap();

        let tar = write_temp_build_context(&dir, "FROM busybox\n").unwrap();
        let entries = read_archive(&tar);

        assert_eq!(
            entries.get("Dockerfile").map(|v| v.as_slice()),
            Some(b"FROM busybox\n".as_slice())
        );
        assert_eq!(
            entries.get("hello.txt").map(|v| v.as_slice()),
            Some(b"hi".as_slice())
        );
        assert_eq!(
            entries.get("nested/data.txt").map(|v| v.as_slice()),
            Some(b"data".as_slice())
        );
        std::fs::remove_file(&tar).ok();
    }

    #[test]
    fn tar_skips_target_and_git() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/blob"), b"x").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), b"x").unwrap();
        std::fs::write(dir.join("keep.txt"), b"keep").unwrap();

        let tar = write_temp_build_context(&dir, "FROM busybox\n").unwrap();
        let entries = read_archive(&tar);

        assert!(entries.contains_key("keep.txt"));
        assert!(entries.keys().all(|k| !k.starts_with("target")));
        assert!(entries.keys().all(|k| !k.starts_with(".git")));
        std::fs::remove_file(&tar).ok();
    }

    #[test]
    fn build_directory_includes_dockerfile_and_context_files() {
        let dir = tempdir();
        std::fs::write(dir.join("hello.txt"), b"hi").unwrap();
        std::fs::create_dir(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested/data.txt"), b"data").unwrap();

        let build_dir = write_temp_build_directory(&dir, "FROM busybox\n").unwrap();

        assert_eq!(
            std::fs::read(build_dir.join("Dockerfile")).unwrap(),
            b"FROM busybox\n"
        );
        assert_eq!(std::fs::read(build_dir.join("hello.txt")).unwrap(), b"hi");
        assert_eq!(
            std::fs::read(build_dir.join("nested/data.txt")).unwrap(),
            b"data"
        );
        std::fs::remove_dir_all(&build_dir).ok();
    }

    #[test]
    fn build_directory_skips_target_and_git() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("target/debug/blob"), b"x").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), b"x").unwrap();
        std::fs::write(dir.join("keep.txt"), b"keep").unwrap();

        let build_dir = write_temp_build_directory(&dir, "FROM busybox\n").unwrap();

        assert!(build_dir.join("keep.txt").is_file());
        assert!(!build_dir.join("target").exists());
        assert!(!build_dir.join(".git").exists());
        std::fs::remove_dir_all(&build_dir).ok();
    }
}
