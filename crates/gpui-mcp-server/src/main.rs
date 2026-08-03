//! Stdio MCP server for locally discoverable, instrumented GPUI windows.

mod capture;
mod client;
mod recording;
mod tools;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use clap::Parser;
use gpui_mcp_capture::CaptureOptions;
use rmcp::{ServiceExt as _, transport::stdio};
use tracing_subscriber::EnvFilter;

use client::{BridgeRegistry, default_runtime_root};
use recording::ArtifactStore;
use tools::GpuiMcp;

#[derive(Debug, Parser)]
#[command(
    name = "gpui-mcp-server",
    version,
    about = "Hardened MCP stdio server for an instrumented GPUI application"
)]
struct Args {
    /// Restrict discovery to one exact endpoint descriptor.
    #[arg(long, value_name = "PATH")]
    endpoint: Option<PathBuf>,

    /// Restrict discovery to this Rust-configured application identifier.
    /// Normal MCP setup does not need this option.
    #[arg(long, value_name = "ID", conflicts_with = "endpoint")]
    app_id: Option<String>,

    /// Override the private endpoint discovery directory.
    #[arg(long, value_name = "DIR", conflicts_with = "endpoint")]
    endpoint_dir: Option<PathBuf>,

    /// Directory for recording artifacts. Defaults to a private app/session directory
    /// below the user runtime area (or the process temp area when unavailable).
    #[arg(long, value_name = "DIR")]
    artifact_dir: Option<PathBuf>,

    /// Maximum Windows Graphics Capture freshness wait, in milliseconds (32..=2000).
    #[arg(long, default_value_t = 1_000, value_name = "MS")]
    capture_stability_deadline_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args = Args::parse();
    let registry = BridgeRegistry::new(args.endpoint, args.app_id, args.endpoint_dir);
    let capture_options =
        CaptureOptions::new(Duration::from_millis(args.capture_stability_deadline_ms))
            .map_err(anyhow::Error::msg)?;
    let artifact_dir = args.artifact_dir.unwrap_or_else(|| {
        default_runtime_root()
            .join("artifacts")
            .join(format!("server-{}", std::process::id()))
    });
    let artifacts = ArtifactStore::open(artifact_dir).map_err(anyhow::Error::msg)?;
    tracing::info!(
        artifact_dir = %artifacts.directory().display(),
        capture_stability_deadline_ms = args.capture_stability_deadline_ms,
        "started GPUI MCP discovery server"
    );

    let service = GpuiMcp::new(registry, artifacts, capture_options)
        .serve(stdio())
        .await
        .context("could not start MCP stdio transport")?;
    service
        .waiting()
        .await
        .context("MCP stdio service stopped unexpectedly")?;
    Ok(())
}
