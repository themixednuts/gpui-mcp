//! Project scaffolding command for pure-HTML GPUI MCP applications.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use gpui_mcp_html::{ProjectOptions, scaffold_project};

#[derive(Debug, Parser)]
#[command(name = "gpui-mcp", about = "Pure HTML project tools for GPUI MCP")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a new pure-HTML GPUI application.
    New {
        /// Cargo package and MCP application identifier.
        name: String,
        /// New directory; defaults to the package name.
        #[arg(long)]
        destination: Option<PathBuf>,
        /// Explicit local gpui-mcp checkout for offline or unpublished work.
        #[arg(long)]
        gpui_mcp_workspace: Option<PathBuf>,
    },
}

fn main() {
    let result = match Cli::parse().command {
        Command::New {
            name,
            destination,
            gpui_mcp_workspace,
        } => {
            let destination = destination.unwrap_or_else(|| PathBuf::from(&name));
            let mut options = ProjectOptions::new(name, destination);
            if let Some(workspace) = gpui_mcp_workspace {
                options = options.with_local_workspace(workspace);
            }
            scaffold_project(&options)
        }
    };
    match result {
        Ok(path) => println!("created {}", path.display()),
        Err(error) => {
            eprintln!("could not create project: {error}");
            std::process::exit(1);
        }
    }
}
