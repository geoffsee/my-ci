use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "my-ci")]
#[command(about = "Run local CI/CD workflows through an OCI socket")]
pub struct Cli {
    #[arg(short, long, default_value = "my-ci/workflows.toml")]
    pub config: PathBuf,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Build one workflow (and its dependencies), or all workflows.
    Build { workflow: Option<String> },
    /// Run a workflow container using its configured command. Runs all workflows when no name is given.
    Run { workflow: Option<String> },
    /// List workflow names from config.
    List,
    /// Scaffold the bundled my-ci/ template into the target directory (default: ./my-ci).
    Init {
        #[arg(default_value = "my-ci")]
        path: PathBuf,
        /// Overwrite existing files instead of skipping them.
        #[arg(long)]
        force: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_workflow_is_optional() {
        let cli = Cli::try_parse_from(["my-ci", "run"]).unwrap();
        assert!(matches!(cli.command, Commands::Run { workflow: None }));
    }

    #[test]
    fn run_accepts_workflow_name() {
        let cli = Cli::try_parse_from(["my-ci", "run", "publish"]).unwrap();
        match cli.command {
            Commands::Run { workflow } => assert_eq!(workflow.as_deref(), Some("publish")),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn init_defaults_path_and_force() {
        let cli = Cli::try_parse_from(["my-ci", "init"]).unwrap();
        match cli.command {
            Commands::Init { path, force } => {
                assert_eq!(path, PathBuf::from("my-ci"));
                assert!(!force);
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn init_force_flag_parses() {
        let cli = Cli::try_parse_from(["my-ci", "init", "custom", "--force"]).unwrap();
        match cli.command {
            Commands::Init { path, force } => {
                assert_eq!(path, PathBuf::from("custom"));
                assert!(force);
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn config_default_is_my_ci_workflows_toml() {
        let cli = Cli::try_parse_from(["my-ci", "list"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("my-ci/workflows.toml"));
    }
}
