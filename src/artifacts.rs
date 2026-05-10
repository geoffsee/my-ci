//! Helpers for copying workflow outputs from container archives onto the host.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};

/// Stable subdirectory name under `{artifacts_dir}/{workflow}` for one configured container path.
pub fn artifact_subdir_label(container_path: &str) -> String {
    container_path.trim_start_matches('/').replace('/', "__")
}

/// Unpack a Docker `GET /containers/archive` response (uncompressed tar) into `dest`.
pub fn unpack_docker_copy_archive(bytes: &[u8], dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    let cursor = Cursor::new(bytes);
    let mut archive = tar::Archive::new(cursor);
    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("tar entry")?;
        entry
            .unpack_in(dest)
            .with_context(|| format!("unpack into {}", dest.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_subdir_label_strips_leading_slash_and_escapes() {
        assert_eq!(artifact_subdir_label("/a/b/c"), "a__b__c");
        assert_eq!(
            artifact_subdir_label("var/log/app.log"),
            "var__log__app.log"
        );
    }

    #[test]
    fn unpack_writes_files() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_path("hello.txt").unwrap();
            header.set_size(5);
            header.set_cksum();
            builder.append(&header, "world".as_bytes()).unwrap();
            builder.finish().unwrap();
        }
        let mut dir = std::env::temp_dir();
        dir.push(format!("my-ci-artifacts-unpack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out");
        unpack_docker_copy_archive(&tar_bytes, &out).unwrap();
        let content = std::fs::read_to_string(out.join("hello.txt")).unwrap();
        assert_eq!(content, "world");
        std::fs::remove_dir_all(&dir).ok();
    }
}
