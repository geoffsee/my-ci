use std::net::IpAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::oci::RuntimeChoice;

#[derive(Parser, Debug)]
#[command(name = "my-ci")]
#[command(about = "Run local CI/CD workflows through Docker, Podman, or Apple container")]
pub struct Cli {
    #[arg(short, long, default_value = "my-ci/workflows.toml")]
    pub config: PathBuf,
    #[arg(long, value_enum, default_value_t = RuntimeChoice::Auto)]
    pub runtime: RuntimeChoice,
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
    /// Serve a browser GUI with realtime pipeline status and controls.
    Gui {
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        #[arg(short, long, default_value_t = 7878)]
        port: u16,
    },
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

    #[test]
    fn runtime_defaults_to_auto() {
        let cli = Cli::try_parse_from(["my-ci", "list"]).unwrap();
        assert_eq!(cli.runtime, RuntimeChoice::Auto);
    }

    #[test]
    fn runtime_accepts_apple_container() {
        let cli = Cli::try_parse_from(["my-ci", "--runtime", "apple-container", "run"]).unwrap();
        assert_eq!(cli.runtime, RuntimeChoice::AppleContainer);
    }

    #[test]
    fn gui_defaults_to_localhost_port() {
        let cli = Cli::try_parse_from(["my-ci", "gui"]).unwrap();
        match cli.command {
            Commands::Gui { host, port } => {
                assert_eq!(host, "127.0.0.1".parse::<IpAddr>().unwrap());
                assert_eq!(port, 7878);
            }
            other => panic!("expected Gui, got {other:?}"),
        }
    }
}
