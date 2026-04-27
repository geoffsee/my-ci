use std::collections::HashSet;

use anyhow::{Result, bail};

use crate::config::{WorkflowConfig, WorkflowFile, get_workflow};

pub fn topological_order(config: &WorkflowFile) -> Result<Vec<String>> {
    let mut names = HashSet::new();
    for wf in &config.workflow {
        if !names.insert(wf.name.clone()) {
            bail!("duplicate workflow name '{}'", wf.name);
        }
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut ordered = Vec::new();
    for wf in &config.workflow {
        dfs_visit(wf, config, &mut visiting, &mut visited, &mut ordered)?;
    }
    Ok(ordered)
}

pub fn resolve_build_plan(config: &WorkflowFile, target: &str) -> Result<Vec<String>> {
    let root = get_workflow(config, target)?;
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut ordered = Vec::new();
    dfs_visit(root, config, &mut visiting, &mut visited, &mut ordered)?;
    Ok(ordered)
}

fn dfs_visit(
    wf: &WorkflowConfig,
    config: &WorkflowFile,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    ordered: &mut Vec<String>,
) -> Result<()> {
    if visited.contains(&wf.name) {
        return Ok(());
    }
    if !visiting.insert(wf.name.clone()) {
        bail!("dependency cycle detected at '{}'", wf.name);
    }

    for dep in &wf.depends_on {
        let dep_wf = get_workflow(config, dep)?;
        dfs_visit(dep_wf, config, visiting, visited, ordered)?;
    }

    visiting.remove(&wf.name);
    visited.insert(wf.name.clone());
    ordered.push(wf.name.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn wf(name: &str, deps: &[&str]) -> WorkflowConfig {
        WorkflowConfig {
            name: name.to_string(),
            instructions: String::new(),
            context: PathBuf::new(),
            image: None,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            env: vec![],
            command: None,
        }
    }

    fn cfg(workflows: Vec<WorkflowConfig>) -> WorkflowFile {
        WorkflowFile {
            name: "test".into(),
            env_file: None,
            workflow: workflows,
        }
    }

    #[test]
    fn topological_order_respects_dependencies() {
        let c = cfg(vec![
            wf("publish", &["validate"]),
            wf("validate", &["transform"]),
            wf("transform", &["extract"]),
            wf("extract", &[]),
        ]);
        let order = topological_order(&c).unwrap();
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("extract") < pos("transform"));
        assert!(pos("transform") < pos("validate"));
        assert!(pos("validate") < pos("publish"));
    }

    #[test]
    fn topological_order_detects_cycle() {
        let c = cfg(vec![wf("a", &["b"]), wf("b", &["a"])]);
        let err = topological_order(&c).unwrap_err().to_string();
        assert!(err.contains("cycle"), "got: {err}");
    }

    #[test]
    fn topological_order_detects_duplicate_names() {
        let c = cfg(vec![wf("a", &[]), wf("a", &[])]);
        let err = topological_order(&c).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn resolve_build_plan_returns_ancestors_then_target() {
        let c = cfg(vec![
            wf("c", &["b"]),
            wf("b", &["a"]),
            wf("a", &[]),
            wf("unrelated", &[]),
        ]);
        let plan = resolve_build_plan(&c, "c").unwrap();
        assert_eq!(plan, vec!["a", "b", "c"]);
    }

    #[test]
    fn resolve_build_plan_unknown_target_errors() {
        let c = cfg(vec![wf("a", &[])]);
        assert!(resolve_build_plan(&c, "missing").is_err());
    }

    #[test]
    fn resolve_build_plan_missing_dep_errors() {
        let c = cfg(vec![wf("a", &["ghost"])]);
        assert!(resolve_build_plan(&c, "a").is_err());
    }
}
