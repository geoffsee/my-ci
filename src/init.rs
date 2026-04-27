use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "my-ci/"]
#[include = "*.toml"]
#[include = "*.Containerfile"]
#[include = ".env.workflows.example"]
struct ScaffoldAssets;

pub fn scaffold_init(target: &Path, force: bool) -> Result<()> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("failed to create {}", target.display()))?;

    let mut files: Vec<String> = ScaffoldAssets::iter().map(|p| p.into_owned()).collect();
    if files.is_empty() {
        bail!("no scaffold assets embedded in binary");
    }
    files.sort();

    let mut written = 0usize;
    let mut skipped = 0usize;
    for rel in &files {
        let asset = ScaffoldAssets::get(rel)
            .ok_or_else(|| anyhow!("missing embedded asset '{rel}'"))?;
        let dest = target.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        if dest.exists() && !force {
            println!("skip {} (exists; use --force to overwrite)", dest.display());
            skipped += 1;
            continue;
        }
        std::fs::write(&dest, asset.data.as_ref())
            .with_context(|| format!("failed to write {}", dest.display()))?;
        println!("wrote {}", dest.display());
        written += 1;
    }

    println!("init complete ({written} written, {skipped} skipped)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "my-ci-init-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        p
    }

    #[test]
    fn embedded_assets_include_workflow_template() {
        let names: Vec<String> = ScaffoldAssets::iter().map(|p| p.into_owned()).collect();
        assert!(names.iter().any(|n| n == "workflows.toml"));
        assert!(names.iter().any(|n| n.ends_with(".Containerfile")));
    }

    #[test]
    fn scaffold_writes_all_assets_into_fresh_dir() {
        let dir = tempdir();
        scaffold_init(&dir, false).unwrap();
        assert!(dir.join("workflows.toml").exists());
        // cleanup
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scaffold_skips_existing_files_without_force() {
        let dir = tempdir();
        std::fs::create_dir_all(&dir).unwrap();
        let target_file = dir.join("workflows.toml");
        std::fs::write(&target_file, b"USER CONTENT").unwrap();

        scaffold_init(&dir, false).unwrap();
        let after = std::fs::read(&target_file).unwrap();
        assert_eq!(after, b"USER CONTENT");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scaffold_overwrites_with_force() {
        let dir = tempdir();
        std::fs::create_dir_all(&dir).unwrap();
        let target_file = dir.join("workflows.toml");
        std::fs::write(&target_file, b"USER CONTENT").unwrap();

        scaffold_init(&dir, true).unwrap();
        let after = std::fs::read(&target_file).unwrap();
        assert_ne!(after, b"USER CONTENT");
        std::fs::remove_dir_all(&dir).ok();
    }
}
