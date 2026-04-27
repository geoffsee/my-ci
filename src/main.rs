mod build;
mod cli;
mod config;
mod graph;
mod init;
mod oci;
mod run;

use anyhow::{Context, Result};
use clap::Parser;

use crate::build::build_workflow;
use crate::cli::{Cli, Commands};
use crate::config::{get_workflow, load_config};
use crate::graph::{resolve_build_plan, topological_order};
use crate::init::scaffold_init;
use crate::oci::{OciProvider, connect_oci, detect_oci_provider, get_oci_socket_addr};
use crate::run::run_workflow;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Commands::Init { path, force } = &cli.command {
        return scaffold_init(path, *force);
    }

    let config = load_config(&cli.config)?;

    let detected_provider = detect_oci_provider().unwrap_or(OciProvider::Docker);

    let oci_provider = connect_oci(detected_provider).context(format!(
        "failed to connect to an OCI socket at {}",
        get_oci_socket_addr(detected_provider)
    ))?;

    match cli.command {
        Commands::Build { workflow } => {
            if let Some(name) = workflow {
                for target in resolve_build_plan(&config, &name)? {
                    let wf = get_workflow(&config, &target)?;
                    build_workflow(&oci_provider, &config, wf).await?;
                }
            } else {
                for name in topological_order(&config)? {
                    let wf = get_workflow(&config, &name)?;
                    build_workflow(&oci_provider, &config, wf).await?;
                }
            }
        }
        Commands::Run { workflow } => {
            let targets = match workflow {
                Some(name) => vec![name],
                None => topological_order(&config)?,
            };
            for target in &targets {
                for dep in resolve_build_plan(&config, target)? {
                    let wf = get_workflow(&config, &dep)?;
                    build_workflow(&oci_provider, &config, wf).await?;
                }
            }
            for target in &targets {
                let wf = get_workflow(&config, target)?;
                if wf.command.is_some() {
                    run_workflow(&oci_provider, &config, wf).await?;
                }
            }
        }
        Commands::List => {
            for wf in &config.workflow {
                println!("{}", wf.name);
            }
        }
        Commands::Init { .. } => unreachable!("init handled before config load"),
    }

    Ok(())
}
