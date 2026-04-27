use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bollard::Docker;
use bollard::image::BuildImageOptions;
use bytes::Bytes;
use futures_util::StreamExt;
use tar::{Builder, Header};
use walkdir::WalkDir;

use crate::config::{WorkflowConfig, WorkflowFile, image_tag, normalize_context};

pub async fn build_workflow(
    docker: &Docker,
    config: &WorkflowFile,
    wf: &WorkflowConfig,
) -> Result<()> {
    let context = normalize_context(&wf.context);
    let image_tag = image_tag(config, wf);
    println!("Building '{}' from {}", wf.name, context.display());

    let tar_path = write_temp_build_context(&context, &wf.instructions)?;
    let archive_bytes = std::fs::read(&tar_path)
        .with_context(|| format!("failed to read temp build context {}", tar_path.display()))?;
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
            bail!("build failed for '{}': {error}", wf.name);
        }
        if let Some(stream) = chunk.stream {
            print!("{stream}");
        }
    }

    std::fs::remove_file(&tar_path).ok();
    Ok(())
}

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
    builder.finish().context("failed to finalize build archive")?;
    Ok(archive_path)
}

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
        if rel.as_os_str().is_empty() || rel.starts_with(".git") || rel.starts_with("target") {
            continue;
        }

        if entry.file_type().is_dir() {
            builder
                .append_dir(rel, path)
                .with_context(|| format!("failed to append directory {}", path.display()))?;
            continue;
        }

        if entry.file_type().is_file() {
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
}
