//! Minimal first-party MCP client for dogfooding instrumented GPUI applications.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use clap::Parser;
use rmcp::{
    ServiceExt as _,
    model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject},
    transport::TokioChildProcess,
};
use serde::Deserialize;
use tokio::process::Command;

const MAX_BATCH_BYTES: u64 = 1024 * 1024;
const MAX_BATCH_CALLS: usize = 256;

#[derive(Debug, Parser)]
#[command(about = "Call GPUI MCP tools through one initialized child-server transport")]
struct Args {
    /// Path to the gpui-mcp-server executable.
    #[arg(long)]
    server: PathBuf,

    /// Exact private endpoint descriptor emitted by the GPUI application.
    #[arg(long, conflicts_with = "app_id")]
    endpoint: Option<PathBuf>,

    /// Discover the running GPUI application by its stable application ID.
    #[arg(long, conflicts_with = "endpoint")]
    app_id: Option<String>,

    /// Recording artifact directory passed to the child server.
    #[arg(long)]
    artifact_dir: Option<PathBuf>,

    /// Compositor stability deadline passed to the child server, in milliseconds.
    #[arg(long, default_value_t = 250)]
    capture_stability_deadline_ms: u64,

    /// MCP tool name. Omit with --list-tools or --batch.
    #[arg(
        long,
        required_unless_present_any = ["list_tools", "batch"],
        conflicts_with = "batch"
    )]
    tool: Option<String>,

    /// JSON object passed as the tool arguments.
    #[arg(long, default_value = "{}")]
    arguments: String,

    /// List the server's tool names instead of calling a tool.
    #[arg(long, conflicts_with = "batch")]
    list_tools: bool,

    /// Execute a JSON array of tool calls over one persistent MCP transport.
    /// Each entry is `{"tool":"...","arguments":{...},"image_out":"optional/path"}`.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["tool", "list_tools", "image_out"])]
    batch: Option<PathBuf>,

    /// Save the first returned image block to this file.
    #[arg(long, requires = "tool", conflicts_with = "batch")]
    image_out: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct BatchCall {
    tool: String,
    #[serde(default)]
    arguments: JsonObject,
    image_out: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.endpoint.is_none() && args.app_id.is_none() {
        bail!("pass either --endpoint or --app-id");
    }

    let mut command = Command::new(&args.server);
    if let Some(endpoint) = &args.endpoint {
        command.arg("--endpoint").arg(endpoint);
    } else if let Some(app_id) = &args.app_id {
        command.arg("--app-id").arg(app_id);
    }
    if let Some(artifact_dir) = &args.artifact_dir {
        command.arg("--artifact-dir").arg(artifact_dir);
    }
    command
        .arg("--capture-stability-deadline-ms")
        .arg(args.capture_stability_deadline_ms.to_string());
    let transport = TokioChildProcess::new(command).context("start GPUI MCP stdio server")?;
    let mut client = ().serve(transport).await.context("initialize MCP client")?;

    if args.list_tools {
        let tools = client.list_all_tools().await.context("list MCP tools")?;
        let names = tools
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&names)?);
        client.close().await.context("close MCP client")?;
        return Ok(());
    }

    if let Some(batch_path) = args.batch {
        let metadata = std::fs::metadata(&batch_path)
            .with_context(|| format!("inspect MCP batch {}", batch_path.display()))?;
        if !metadata.is_file() || metadata.len() > MAX_BATCH_BYTES {
            bail!("--batch must be a regular JSON file no larger than 1 MiB");
        }
        let bytes = std::fs::read(&batch_path)
            .with_context(|| format!("read MCP batch {}", batch_path.display()))?;
        let calls = serde_json::from_slice::<Vec<BatchCall>>(&bytes)
            .context("--batch must contain a JSON array of tool calls")?;
        if calls.is_empty() || calls.len() > MAX_BATCH_CALLS {
            bail!("--batch must contain between 1 and {MAX_BATCH_CALLS} calls");
        }
        let mut outputs = Vec::with_capacity(calls.len());
        for (index, call) in calls.into_iter().enumerate() {
            if call.tool.is_empty() {
                bail!("batch call {index} has an empty tool name");
            }
            let request =
                CallToolRequestParams::new(call.tool.clone()).with_arguments(call.arguments);
            let result = client
                .call_tool(request)
                .await
                .with_context(|| format!("call MCP batch tool {} at index {index}", call.tool))?;
            outputs.push(serde_json::json!({
                "index": index,
                "tool": call.tool,
                "result": render_result(result, call.image_out.as_deref())?,
            }));
        }
        println!("{}", serde_json::to_string_pretty(&outputs)?);
        client.close().await.context("close MCP client")?;
        return Ok(());
    }

    let arguments = serde_json::from_str::<JsonObject>(&args.arguments)
        .context("--arguments must be a JSON object")?;
    let request =
        CallToolRequestParams::new(args.tool.unwrap_or_default()).with_arguments(arguments);
    let result = client.call_tool(request).await.context("call MCP tool")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&render_result(result, args.image_out.as_deref())?)?
    );
    client.close().await.context("close MCP client")?;
    Ok(())
}

fn render_result(
    result: CallToolResult,
    image_out: Option<&std::path::Path>,
) -> Result<serde_json::Value> {
    let Some(output) = image_out else {
        return serde_json::to_value(result).context("encode MCP tool result");
    };
    let Some(image) = result.content.iter().find_map(ContentBlock::as_image) else {
        bail!("tool result did not contain an image");
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .context("decode returned image")?;
    std::fs::write(output, bytes)
        .with_context(|| format!("write MCP image {}", output.display()))?;
    Ok(serde_json::json!({
        "image_out": output,
        "mime_type": image.mime_type,
        "structured_content": result.structured_content,
        "is_error": result.is_error,
    }))
}
