use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use my_ci_macros::trace;
use serde::Deserialize;
use tracing::debug;

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowFile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    /// Host directory for workflow outputs (copied paths and bind mounts). Relative paths resolve against the config file directory.
    #[serde(default = "default_artifacts_dir")]
    pub artifacts_dir: PathBuf,
    #[serde(default)]
    pub workflow: Vec<WorkflowConfig>,
}

fn default_artifacts_dir() -> PathBuf {
    PathBuf::from("artifacts")
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowConfig {
    pub name: String,
    pub instructions: String,
    #[serde(default)]
    pub context: PathBuf,
    pub image: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    pub command: Option<Vec<String>>,
    /// POSIX paths inside the container to copy into `{artifacts_dir}/{workflow}/…` after a successful run (Docker/Podman).
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// If set, `{artifacts_dir}/{workflow}` on the host is bind-mounted here for every runtime (write-through artifacts).
    pub artifact_bind: Option<String>,
}

#[trace(level = "debug", err, fields(path = %path.display()))]
pub fn load_config(path: &Path) -> Result<WorkflowFile> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file at {}", path.display()))?;
    debug!(bytes = raw.len(), "read workflow config file");
    let mut parsed: WorkflowFile = toml::from_str(&raw)
        .with_context(|| format!("failed to parse TOML file at {}", path.display()))?;
    if parsed.workflow.is_empty() {
        bail!("config contains no [[workflow]] entries");
    }
    debug!(
        workflow_count = parsed.workflow.len(),
        env_file = ?parsed.env_file,
        "parsed workflow config"
    );

    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    if let Some(env_file) = &parsed.env_file {
        let resolved = if env_file.is_absolute() {
            env_file.clone()
        } else {
            config_dir.join(env_file)
        };
        debug!(env_file = %resolved.display(), "loading env file");
        dotenvy::from_path(&resolved)
            .with_context(|| format!("failed to load env_file at {}", resolved.display()))?;
    }

    parsed.artifacts_dir = if parsed.artifacts_dir.is_absolute() {
        parsed.artifacts_dir.clone()
    } else {
        config_dir.join(&parsed.artifacts_dir)
    };
    for workflow in &mut parsed.workflow {
        debug!(workflow = %workflow.name, "hydrating workflow instructions");
        hydrate_instructions_from_containerfile(config_dir, workflow)?;
    }

    Ok(parsed)
}

#[trace(level = "debug", skip(workflow), err, fields(config_dir = %config_dir.display(), workflow = %workflow.name))]
pub fn hydrate_instructions_from_containerfile(
    config_dir: &Path,
    workflow: &mut WorkflowConfig,
) -> Result<()> {
    let candidate = workflow.instructions.trim();
    if candidate.is_empty() || candidate.contains('\n') {
        debug!("workflow already has inline instructions");
        return Ok(());
    }

    let candidate_path = Path::new(candidate);
    let resolved = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        config_dir.join(candidate_path)
    };

    if !resolved.is_file() {
        debug!(candidate = %resolved.display(), "workflow instructions are not a readable file path");
        return Ok(());
    }

    let is_containerfile = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".Containerfile"));
    if !is_containerfile {
        debug!(candidate = %resolved.display(), "workflow instructions file is not a Containerfile");
        return Ok(());
    }

    debug!(containerfile = %resolved.display(), "reading workflow Containerfile");
    workflow.instructions = std::fs::read_to_string(&resolved).with_context(|| {
        format!(
            "failed to read Containerfile for workflow '{}' at {}",
            workflow.name,
            resolved.display()
        )
    })?;
    Ok(())
}

#[trace(level = "trace", skip(config), err, fields(workflow = %name))]
pub fn get_workflow<'a>(config: &'a WorkflowFile, name: &str) -> Result<&'a WorkflowConfig> {
    config
        .workflow
        .iter()
        .find(|wf| wf.name == name)
        .ok_or_else(|| anyhow!("unknown workflow '{name}'"))
}

#[trace(level = "trace", ret, fields(context = %context.display()))]
pub fn normalize_context(context: &Path) -> PathBuf {
    if context.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        context.to_path_buf()
    }
}

#[trace(level = "trace", skip(config, wf), ret, fields(project = %config.name, workflow = %wf.name))]
pub fn image_tag(config: &WorkflowFile, wf: &WorkflowConfig) -> String {
    let project = if config.name.trim().is_empty() {
        "my-ci"
    } else {
        config.name.trim()
    };
    wf.image
        .clone()
        .unwrap_or_else(|| format!("{project}:{}", wf.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf(name: &str) -> WorkflowConfig {
        WorkflowConfig {
            name: name.to_string(),
            instructions: String::new(),
            context: PathBuf::new(),
            image: None,
            depends_on: vec![],
            env: vec![],
            command: None,
            artifacts: vec![],
            artifact_bind: None,
        }
    }

    #[test]
    fn normalize_context_defaults_to_dot() {
        assert_eq!(normalize_context(Path::new("")), PathBuf::from("."));
        assert_eq!(normalize_context(Path::new("ctx")), PathBuf::from("ctx"));
    }

    #[test]
    fn image_tag_uses_project_name_then_workflow_name() {
        let cfg = WorkflowFile {
            name: "proj".into(),
            env_file: None,
            artifacts_dir: PathBuf::from("/tmp/a"),
            workflow: vec![wf("build")],
        };
        assert_eq!(image_tag(&cfg, &cfg.workflow[0]), "proj:build");
    }

    #[test]
    fn image_tag_falls_back_to_my_ci_when_name_blank() {
        let cfg = WorkflowFile {
            name: "  ".into(),
            env_file: None,
            artifacts_dir: PathBuf::from("/tmp/b"),
            workflow: vec![wf("step")],
        };
        assert_eq!(image_tag(&cfg, &cfg.workflow[0]), "my-ci:step");
    }

    #[test]
    fn image_tag_respects_explicit_override() {
        let mut w = wf("step");
        w.image = Some("custom:tag".into());
        let cfg = WorkflowFile {
            name: "proj".into(),
            env_file: None,
            artifacts_dir: PathBuf::from("/tmp/c"),
            workflow: vec![w],
        };
        assert_eq!(image_tag(&cfg, &cfg.workflow[0]), "custom:tag");
    }

    #[test]
    fn get_workflow_finds_by_name() {
        let cfg = WorkflowFile {
            name: "p".into(),
            env_file: None,
            artifacts_dir: PathBuf::from("/tmp/d"),
            workflow: vec![wf("a"), wf("b")],
        };
        assert_eq!(get_workflow(&cfg, "b").unwrap().name, "b");
        assert!(get_workflow(&cfg, "missing").is_err());
    }

    #[test]
    fn hydrate_inlines_containerfile_path() {
        let dir = tempdir();
        let cf_path = dir.join("step.Containerfile");
        std::fs::write(&cf_path, "FROM busybox:latest\n").unwrap();
        let mut w = wf("step");
        w.instructions = "step.Containerfile".into();
        hydrate_instructions_from_containerfile(&dir, &mut w).unwrap();
        assert!(w.instructions.contains("FROM busybox:latest"));
    }

    #[test]
    fn hydrate_leaves_inline_dockerfile_untouched() {
        let dir = tempdir();
        let mut w = wf("step");
        w.instructions = "FROM alpine\nRUN echo hi\n".into();
        hydrate_instructions_from_containerfile(&dir, &mut w).unwrap();
        assert!(w.instructions.starts_with("FROM alpine"));
    }

    #[test]
    fn hydrate_ignores_non_containerfile_paths() {
        let dir = tempdir();
        let p = dir.join("notes.txt");
        std::fs::write(&p, "ignored").unwrap();
        let mut w = wf("step");
        w.instructions = "notes.txt".into();
        hydrate_instructions_from_containerfile(&dir, &mut w).unwrap();
        assert_eq!(w.instructions, "notes.txt");
    }

    #[test]
    fn load_config_resolves_artifacts_dir_relative_to_config_file() {
        let dir = tempdir();
        let cfg_path = dir.join("nested").join("workflows.toml");
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(
            &cfg_path,
            r#"
name = "demo"
artifacts_dir = "build-products"

[[workflow]]
name = "a"
instructions = "FROM busybox:latest\n"
"#,
        )
        .unwrap();
        let cfg = load_config(&cfg_path).unwrap();
        let expected = cfg_path.parent().unwrap().join("build-products");
        assert_eq!(cfg.artifacts_dir, expected);
    }

    #[test]
    fn load_config_reads_toml_and_hydrates() {
        let dir = tempdir();
        std::fs::write(dir.join("a.Containerfile"), "FROM busybox\n").unwrap();
        let cfg_path = dir.join("workflows.toml");
        std::fs::write(
            &cfg_path,
            r#"
name = "demo"

[[workflow]]
name = "a"
instructions = "a.Containerfile"
"#,
        )
        .unwrap();
        let cfg = load_config(&cfg_path).unwrap();
        assert_eq!(cfg.name, "demo");
        assert_eq!(cfg.workflow.len(), 1);
        assert!(cfg.workflow[0].instructions.contains("FROM busybox"));
    }

    #[test]
    fn load_config_errors_when_no_workflows() {
        let dir = tempdir();
        let cfg_path = dir.join("workflows.toml");
        std::fs::write(&cfg_path, "name = \"empty\"\n").unwrap();
        assert!(load_config(&cfg_path).is_err());
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "my-ci-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
