mod build;
mod cli;
mod config;
mod events;
mod graph;
mod gui;
mod history;
mod init;
mod oci;
mod run;
mod telemetry;
mod ui_assets;

use anyhow::{Context, Result};
use clap::Parser;
use my_ci_macros::trace;
use tracing::{debug, info};

use crate::build::build_workflow;
use crate::cli::{Cli, Commands};
use crate::config::{get_workflow, load_config};
use crate::graph::{resolve_build_plan, topological_order};
use crate::gui::serve_gui;
use crate::init::scaffold_init;
use crate::oci::{
    OciRuntime, RuntimeChoice, connect_oci, describe_oci_target, select_oci_provider,
};
use crate::run::run_workflow;

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init_tracing();
    let cli = Cli::parse();
    run_cli(cli).await
}

#[trace(skip(cli), err, fields(config = %cli.config.display(), command = ?cli.command, runtime = ?cli.runtime))]
async fn run_cli(cli: Cli) -> Result<()> {
    debug!("parsed CLI arguments");
    if let Commands::Init { path, force } = &cli.command {
        info!(path = %path.display(), force, "scaffolding bundled workflow template");
        return scaffold_init(path, *force);
    }

    let config = load_config(&cli.config)?;
    let project_name = if config.name.trim().is_empty() {
        "my-ci"
    } else {
        config.name.trim()
    };
    info!(
        project = %project_name,
        workflow_count = config.workflow.len(),
        "loaded workflow config"
    );

    if let Commands::List = &cli.command {
        debug!("listing workflows without connecting to a runtime");
        for wf in &config.workflow {
            println!("{}", wf.name);
        }
        return Ok(());
    }

    match cli.command {
        Commands::Build { workflow } => {
            let oci_runtime = connect_selected_runtime(cli.runtime)?;
            if let Some(name) = workflow {
                debug!(workflow = %name, "resolving targeted build plan");
                for target in resolve_build_plan(&config, &name)? {
                    let wf = get_workflow(&config, &target)?;
                    build_workflow(&oci_runtime, &config, wf).await?;
                }
            } else {
                debug!("resolving full build plan");
                for name in topological_order(&config)? {
                    let wf = get_workflow(&config, &name)?;
                    build_workflow(&oci_runtime, &config, wf).await?;
                }
            }
        }
        Commands::Run { workflow } => {
            let oci_runtime = connect_selected_runtime(cli.runtime)?;
            let targets = match workflow {
                Some(name) => vec![name],
                None => topological_order(&config)?,
            };
            debug!(targets = ?targets, "resolved run targets");
            for target in &targets {
                debug!(workflow = %target, "building workflow dependencies before run");
                for dep in resolve_build_plan(&config, target)? {
                    let wf = get_workflow(&config, &dep)?;
                    build_workflow(&oci_runtime, &config, wf).await?;
                }
            }
            for target in &targets {
                let wf = get_workflow(&config, target)?;
                if wf.command.is_some() {
                    run_workflow(&oci_runtime, &config, wf).await?;
                }
            }
        }
        Commands::Gui { host, port } => {
            info!(%host, port, default_runtime = ?cli.runtime, "starting GUI");
            serve_gui(host, port, config, cli.runtime).await?;
        }
        Commands::List => unreachable!("list handled before runtime connect"),
        Commands::Init { .. } => unreachable!("init handled before config load"),
    }

    Ok(())
}

fn connect_selected_runtime(runtime: RuntimeChoice) -> Result<OciRuntime> {
    let provider = select_oci_provider(runtime);
    info!(?runtime, provider = ?provider, "selected OCI runtime provider");

    let oci_runtime = connect_oci(provider)
        .with_context(|| format!("failed to connect to {}", describe_oci_target(provider)))?;
    info!(
        target = describe_oci_target(provider),
        "connected to OCI runtime"
    );
    Ok(oci_runtime)
}
