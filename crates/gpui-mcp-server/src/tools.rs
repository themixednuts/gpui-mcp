use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use gpui_mcp_capture::CaptureOptions;
use gpui_mcp_protocol::{
    ActionOutcome, BridgeResult, Capability, ContextResourceDescriptor, FrameStats, Highlight,
    InputCommand, LiveDocumentSource, MouseButton, NativeInputCommand, NativeScrollDelta,
    NodeAction, NodeState, Operation, Point, Rect, Role, Screenshot, ScreenshotTarget,
    SemanticAction, UiNode, UiTree, ValueInfo,
};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use rmcp::{
    Json, RoleServer, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorData, Implementation, ListResourcesResult,
        PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResult, Resource,
        ResourceContents, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

use crate::client::BridgeClient;
use crate::recording::{ArtifactStore, RecordingState};

mod application_commands;
mod connection;
mod diagnostics;
mod input;
mod live_document;
mod tree;
mod visual;

const MAX_TREE_SNAPSHOTS: usize = 32;
const MAX_IMAGE_SNAPSHOTS: usize = 8;
const MAX_WAIT_MS: u64 = 30_000;

#[derive(Default)]
struct SnapshotStore {
    trees: BTreeMap<String, UiTree>,
    images: BTreeMap<String, Screenshot>,
}

/// MCP tool suite backed by one authenticated GPUI application endpoint.
#[derive(Clone)]
pub(crate) struct GpuiMcp {
    client: BridgeClient,
    tool_router: ToolRouter<Self>,
    snapshots: Arc<RwLock<SnapshotStore>>,
    recording: Arc<Mutex<RecordingState>>,
    recording_task: Arc<Mutex<Option<RecordingTask>>>,
    artifacts: ArtifactStore,
    capture_options: CaptureOptions,
}

struct RecordingTask {
    cancellation: CancellationToken,
    join: JoinHandle<()>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ObjectOutput {
    #[serde(flatten)]
    fields: BTreeMap<String, JsonValue>,
}

type Value = ObjectOutput;

#[derive(Debug, Deserialize, JsonSchema)]
struct ElementArgs {
    /// Stable semantic node identifier.
    id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FindArgs {
    /// Optional label substring (case-insensitive unless exact is true).
    query: Option<String>,
    /// Optional semantic role filter.
    role: Option<Role>,
    /// Match the full label exactly, case-sensitively.
    #[serde(default)]
    exact: bool,
    /// Return visible nodes only.
    #[serde(default = "default_true")]
    visible_only: bool,
    /// Maximum matches, capped at 200.
    #[serde(default = "default_result_limit")]
    limit: u16,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ClickElementArgs {
    /// Stable semantic node identifier.
    id: String,
    /// `left`, `right`, or `middle`.
    #[serde(default)]
    button: MouseButton,
    /// Click count from 1 through 3.
    #[serde(default = "default_click_count")]
    count: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ClickPointArgs {
    /// Window-relative logical x coordinate.
    x: f32,
    /// Window-relative logical y coordinate.
    y: f32,
    /// `left`, `right`, or `middle`.
    #[serde(default)]
    button: MouseButton,
    /// Click count from 1 through 3.
    #[serde(default = "default_click_count")]
    count: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NativeClickPointArgs {
    /// Window-relative logical x coordinate.
    x: f32,
    /// Window-relative logical y coordinate.
    y: f32,
    /// `left`, `right`, or `middle`.
    #[serde(default)]
    button: MouseButton,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PointerMoveArgs {
    /// Window-relative logical x coordinate.
    x: f32,
    /// Window-relative logical y coordinate.
    y: f32,
    /// Button held during the move, when this is part of a manually controlled drag.
    held_button: Option<MouseButton>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PointerButtonArgs {
    /// Window-relative logical x coordinate.
    x: f32,
    /// Window-relative logical y coordinate.
    y: f32,
    /// `left`, `right`, or `middle`.
    #[serde(default)]
    button: MouseButton,
    /// Click count from 1 through 3.
    #[serde(default = "default_click_count")]
    count: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DragElementArgs {
    /// Source semantic node identifier.
    from_id: String,
    /// Destination semantic node identifier.
    to_id: String,
    /// Gesture interpolation steps, from 1 through 120.
    #[serde(default = "default_drag_steps")]
    steps: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DragPointArgs {
    /// Source window-relative x coordinate.
    from_x: f32,
    /// Source window-relative y coordinate.
    from_y: f32,
    /// Destination window-relative x coordinate.
    to_x: f32,
    /// Destination window-relative y coordinate.
    to_y: f32,
    /// Gesture interpolation steps, from 1 through 120.
    #[serde(default = "default_drag_steps")]
    steps: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct KeyArgs {
    /// GPUI keystroke syntax, for example `ctrl-a`, `secondary-s`, or `enter`.
    keystroke: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TypeTextArgs {
    /// UTF-8 text to insert into the currently focused input.
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetTextArgs {
    /// Editable semantic node identifier.
    id: String,
    /// Replacement UTF-8 text.
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetValueArgs {
    /// Value-bearing semantic node identifier.
    id: String,
    /// Replacement value, validated against annotated numeric bounds when present.
    value: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ScrollArgs {
    /// Optional semantic node identifier. Its center is used when supplied.
    id: Option<String>,
    /// Window-relative x coordinate when `id` is omitted.
    x: Option<f32>,
    /// Window-relative y coordinate when `id` is omitted.
    y: Option<f32>,
    /// Horizontal logical-pixel delta.
    #[serde(default)]
    delta_x: f32,
    /// Vertical logical-pixel delta.
    delta_y: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NativeScrollElementArgs {
    /// Stable semantic node identifier whose live bounds center receives the native wheel event.
    id: String,
    /// Horizontal logical-pixel delta.
    #[serde(default)]
    delta_x: f32,
    /// Vertical logical-pixel delta.
    #[serde(default)]
    delta_y: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NativeScrollPointArgs {
    /// Window-relative logical x coordinate.
    x: f32,
    /// Window-relative logical y coordinate.
    y: f32,
    /// Horizontal logical-pixel delta.
    #[serde(default)]
    delta_x: f32,
    /// Vertical logical-pixel delta.
    #[serde(default)]
    delta_y: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RegionArgs {
    /// Left logical-pixel coordinate.
    x: f32,
    /// Top logical-pixel coordinate.
    y: f32,
    /// Logical-pixel width.
    width: f32,
    /// Logical-pixel height.
    height: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WaitElementArgs {
    /// Label query.
    query: String,
    /// Optional semantic role filter.
    role: Option<Role>,
    /// Match the full label exactly.
    #[serde(default)]
    exact: bool,
    /// Deadline in milliseconds, capped at 30000.
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WaitStateArgs {
    /// Stable semantic node identifier.
    id: String,
    /// Expected visibility, if specified.
    visible: Option<bool>,
    /// Expected enabled state, if specified.
    enabled: Option<bool>,
    /// Expected focus state, if specified.
    focused: Option<bool>,
    /// Expected checked state, if specified.
    checked: Option<bool>,
    /// Expected selected state, if specified.
    selected: Option<bool>,
    /// Expected expanded or collapsed state, if specified.
    expanded: Option<bool>,
    /// Deadline in milliseconds, capped at 30000.
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct HighlightArgs {
    /// One through 64 stable semantic node identifiers.
    ids: Vec<String>,
    /// Eight-digit `#RRGGBBAA` outline color.
    #[serde(default = "default_highlight_color")]
    color: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotArgs {
    /// In-memory snapshot name: 1-64 ASCII letters, digits, `.`, `_`, or `-`.
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DiffSnapshotsArgs {
    /// Left/base in-memory snapshot name.
    left: String,
    /// Right/target in-memory snapshot name.
    right: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DiffCurrentArgs {
    /// Saved in-memory tree snapshot compared with the current UI.
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CaptureNamedArgs {
    /// In-memory image snapshot name.
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StopVideoRecordingArgs {
    /// Portable MP4 filename written inside the server-configured artifact directory.
    /// Must be one 1-128 byte ASCII filename ending in lowercase `.mp4`; paths,
    /// separators, traversal, hidden names, and Windows device names are rejected.
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[A-Za-z0-9][A-Za-z0-9._-]*[.]mp4$")
    )]
    artifact_name: String,
    /// Replace an existing regular artifact with the same name. Defaults to false.
    /// Symlinks and non-regular files are always rejected.
    #[serde(default)]
    overwrite: bool,
    /// Optional per-frame duration in milliseconds (20..=10000). This is primarily
    /// for checkpoint recordings. Continuous recordings default to the duration
    /// derived from their configured `frames_per_second`.
    #[serde(default)]
    #[schemars(range(min = 20, max = 10000))]
    frame_duration_ms: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum VideoCaptureMode {
    /// Capture only when `capture_video_frame` is called.
    Checkpoint,
    /// Continuously capture rendered GPUI frames at the requested bounded rate.
    #[default]
    Continuous,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StartVideoRecordingArgs {
    /// Draw the current GPUI window-relative pointer into every captured frame. This is enabled
    /// by default and works identically on Windows, Linux, and macOS without global OS cursor access.
    #[serde(default = "default_true")]
    include_pointer: bool,
    /// Capture continuously like a browser screencast, or only on explicit checkpoints.
    #[serde(default)]
    mode: VideoCaptureMode,
    /// Target capture cadence for continuous mode (1..=30; default 10). The encoder
    /// uses the matching deterministic frame duration.
    #[serde(default = "default_video_fps")]
    #[schemars(range(min = 1, max = 30))]
    frames_per_second: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompareImagesArgs {
    /// Left/base in-memory image snapshot name.
    left: String,
    /// Right/target in-memory image snapshot name.
    right: String,
    /// Per-channel difference threshold from 0 through 255.
    #[serde(default)]
    tolerance: u8,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RecordPerformanceArgs {
    /// Sampling interval in milliseconds, capped at 30000.
    duration_ms: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LogsArgs {
    /// Maximum entries, capped at 512.
    #[serde(default = "default_log_limit")]
    limit: u16,
    /// Optional minimum level: trace, debug, info, warn, or error.
    min_level: Option<String>,
}

#[tool_router(router = core_router)]
impl GpuiMcp {
    pub(crate) fn new(
        client: BridgeClient,
        artifacts: ArtifactStore,
        capture_options: CaptureOptions,
    ) -> Self {
        Self {
            client,
            tool_router: Self::production_router(),
            snapshots: Arc::new(RwLock::new(SnapshotStore::default())),
            recording: Arc::new(Mutex::new(RecordingState::default())),
            recording_task: Arc::new(Mutex::new(None)),
            artifacts,
            capture_options,
        }
    }

    fn production_router() -> ToolRouter<Self> {
        let mut router = Self::core_router();
        router.merge(connection::router());
        router.merge(application_commands::router());
        router.merge(diagnostics::router());
        router.merge(input::router());
        router.merge(live_document::router());
        router.merge(tree::router());
        router.merge(visual::router());
        router
    }

    async fn tree(&self) -> Result<UiTree, String> {
        let result = self.client.call(Operation::GetTree).await?;
        match result {
            BridgeResult::Tree(tree) => Ok(tree),
            _ => Err("bridge returned the wrong result for the semantic tree".to_owned()),
        }
    }

    async fn ack(&self, operation: Operation) -> Result<(), String> {
        match self.client.call(operation).await? {
            BridgeResult::Ack => Ok(()),
            _ => Err("bridge returned the wrong acknowledgement".to_owned()),
        }
    }

    async fn ack_after_frame(&self, operation: Operation) -> Result<(), String> {
        self.ack(operation).await?;
        self.settle_after_refresh(Duration::from_secs(2)).await?;
        Ok(())
    }

    async fn dispatch_input(&self, command: InputCommand) -> Result<(), String> {
        self.ack_after_frame(Operation::Input { command }).await
    }

    async fn dispatch_native_input(&self, command: NativeInputCommand) -> Result<(), String> {
        self.ack_after_frame(Operation::NativeInput { command })
            .await
    }

    async fn current_pointer_location(&self) -> Result<Point, String> {
        match self.client.call(Operation::GetPointerLocation).await? {
            BridgeResult::PointerLocation(point) => Ok(point),
            _ => Err("bridge returned the wrong result for pointer location".to_owned()),
        }
    }

    async fn native_click_at(
        &self,
        point: Point,
        button: MouseButton,
        count: u8,
    ) -> Result<(), String> {
        validate_native_point(point)?;
        if !(1..=3).contains(&count) {
            return Err("native click count must be between 1 and 3".to_owned());
        }
        for click_count in 1..=count {
            self.dispatch_native_input(NativeInputCommand::MouseDown {
                point,
                button,
                click_count,
            })
            .await?;
            if let Err(error) = self
                .dispatch_native_input(NativeInputCommand::MouseUp {
                    point,
                    button,
                    click_count,
                })
                .await
            {
                return Err(self
                    .native_release_error(point, button, click_count, error)
                    .await);
            }
        }
        Ok(())
    }

    async fn native_drag_between(&self, from: Point, to: Point, steps: u8) -> Result<(), String> {
        validate_native_point(from)?;
        validate_native_point(to)?;
        if !(1..=120).contains(&steps) {
            return Err("native drag steps must be between 1 and 120".to_owned());
        }
        let distance = (to.x - from.x).hypot(to.y - from.y);
        if distance <= 2.0 {
            return Err(
                "native drag endpoints must be more than 2 logical pixels apart".to_owned(),
            );
        }

        self.dispatch_native_input(NativeInputCommand::MouseDown {
            point: from,
            button: MouseButton::Left,
            click_count: 1,
        })
        .await?;

        let mut last_point = from;
        for step in 1..=steps {
            let progress = f32::from(step) / f32::from(steps);
            let point = Point {
                x: from.x + (to.x - from.x) * progress,
                y: from.y + (to.y - from.y) * progress,
            };
            if let Err(error) = self
                .dispatch_native_input(NativeInputCommand::MouseMove {
                    point,
                    pressed_button: Some(MouseButton::Left),
                })
                .await
            {
                return Err(self
                    .native_release_error(last_point, MouseButton::Left, 1, error)
                    .await);
            }
            last_point = point;
        }

        self.dispatch_native_input(NativeInputCommand::MouseUp {
            point: to,
            button: MouseButton::Left,
            click_count: 1,
        })
        .await
    }

    async fn native_scroll_at(
        &self,
        point: Point,
        delta_x: f32,
        delta_y: f32,
    ) -> Result<(), String> {
        validate_native_point(point)?;
        validate_native_scroll_delta(delta_x, delta_y)?;
        self.dispatch_native_input(NativeInputCommand::ScrollWheel {
            point,
            delta: NativeScrollDelta::Pixels { delta_x, delta_y },
        })
        .await
    }

    async fn native_release_error(
        &self,
        point: Point,
        button: MouseButton,
        click_count: u8,
        error: String,
    ) -> String {
        match self
            .dispatch_native_input(NativeInputCommand::MouseUp {
                point,
                button,
                click_count,
            })
            .await
        {
            Ok(()) => error,
            Err(release_error) => {
                format!("{error}; native mouse release also failed: {release_error}")
            }
        }
    }

    async fn click_node(&self, id: &str, button: MouseButton, count: u8) -> Result<(), String> {
        let tree = self.tree().await?;
        self.invoke_node(&tree, id, SemanticAction::Click { button, count })
            .await
    }

    async fn invoke_node(
        &self,
        tree: &UiTree,
        id: &str,
        action: SemanticAction,
    ) -> Result<(), String> {
        let mut fresh_tree = None;
        for attempt in 0..=1 {
            let current_tree = fresh_tree.as_ref().unwrap_or(tree);
            let node = get_node(current_tree, id)?;
            if !node.state.visible || !node.state.enabled {
                return Err(format!("element {id:?} is not visible and enabled"));
            }
            let required_action = action.required_node_action();
            if !node.actions.contains(&required_action) {
                return Err(format!(
                    "element {id:?} does not advertise the {required_action:?} action"
                ));
            }
            let result = self
                .client
                .call(Operation::Invoke {
                    node_id: id.to_owned(),
                    expected_generation: current_tree.generation,
                    action: action.clone(),
                })
                .await;
            match result {
                Err(error) if attempt == 0 && is_stale_generation_error(&error) => {
                    fresh_tree = Some(self.tree().await?);
                }
                Err(error) => return Err(error),
                Ok(BridgeResult::Action(ActionOutcome::Handled)) => {
                    // Semantic generations intentionally remain stable when only pixels change.
                    // Wait for the completed repaint instead so every following operation observes
                    // the action's fully painted result.
                    self.settle_after_refresh(Duration::from_secs(2)).await?;
                    return Ok(());
                }
                Ok(BridgeResult::Action(ActionOutcome::Rejected { reason })) => {
                    return Err(reason);
                }
                Ok(_) => {
                    return Err("bridge returned the wrong result for semantic action".to_owned());
                }
            }
        }
        Err("semantic action retry was exhausted".to_owned())
    }

    async fn wait_for_tree(&self, generation: u64, wait: Duration) -> Result<UiTree, String> {
        let timeout_ms = u64::try_from(wait.as_millis())
            .unwrap_or(MAX_WAIT_MS)
            .clamp(1, MAX_WAIT_MS);
        match self
            .client
            .call(Operation::WaitForTree {
                after_generation: generation,
                timeout_ms,
            })
            .await?
        {
            BridgeResult::Tree(tree) => Ok(tree),
            _ => Err("bridge returned the wrong result for semantic tree wait".to_owned()),
        }
    }

    async fn wait_for_frame(&self, frame_count: u64, wait: Duration) -> Result<FrameStats, String> {
        let timeout_ms = u64::try_from(wait.as_millis())
            .unwrap_or(MAX_WAIT_MS)
            .clamp(1, MAX_WAIT_MS);
        match self
            .client
            .call(Operation::WaitForFrame {
                after_frame_count: frame_count,
                timeout_ms,
            })
            .await?
        {
            BridgeResult::FrameStats(stats) => Ok(stats),
            _ => Err("bridge returned the wrong result for frame wait".to_owned()),
        }
    }

    async fn settle_after_refresh(&self, wait: Duration) -> Result<FrameStats, String> {
        settle_refresh_frames(wait, |operation| self.client.call(operation)).await
    }

    async fn capture(&self, target: ScreenshotTarget) -> Result<Screenshot, String> {
        self.settle_after_refresh(Duration::from_secs(2)).await?;
        let tree = self.tree().await?;
        crate::capture::capture(&self.client, &tree, target, self.capture_options).await
    }

    async fn stored_images(
        &self,
        left: &str,
        right: &str,
    ) -> Result<(Screenshot, Screenshot), String> {
        let snapshots = self.snapshots.read().await;
        let left = snapshots
            .images
            .get(left)
            .cloned()
            .ok_or_else(|| format!("image snapshot {left:?} was not found"))?;
        let right = snapshots
            .images
            .get(right)
            .cloned()
            .ok_or_else(|| format!("image snapshot {right:?} was not found"))?;
        Ok((left, right))
    }

    async fn frame_stats(&self) -> Result<FrameStats, String> {
        match self.client.call(Operation::GetFrameStats).await? {
            BridgeResult::FrameStats(stats) => Ok(stats),
            _ => Err("bridge returned the wrong result for frame statistics".to_owned()),
        }
    }
}

async fn settle_refresh_frames<F, Fut>(wait: Duration, mut call: F) -> Result<FrameStats, String>
where
    F: FnMut(Operation) -> Fut,
    Fut: Future<Output = Result<BridgeResult, String>>,
{
    let timeout_ms = u64::try_from(wait.as_millis())
        .unwrap_or(MAX_WAIT_MS)
        .clamp(1, MAX_WAIT_MS);
    let mut completed = None;
    for _ in 0..2 {
        let BridgeResult::FrameStats(before_refresh) = call(Operation::Refresh).await? else {
            return Err("bridge returned the wrong completed-frame token for refresh".to_owned());
        };
        let BridgeResult::FrameStats(stats) = call(Operation::WaitForFrame {
            after_frame_count: before_refresh.frame_count,
            timeout_ms,
        })
        .await?
        else {
            return Err("bridge returned the wrong result for frame wait".to_owned());
        };
        completed = Some(stats);
    }
    completed.ok_or_else(|| "frame settlement did not request a refresh".to_owned())
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GpuiMcp {
    fn get_info(&self) -> ServerInfo {
        let capabilities = if self
            .client
            .descriptor()
            .capabilities
            .supports(Capability::ContextResources)
        {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build()
        } else {
            ServerCapabilities::builder().enable_tools().build()
        };
        ServerInfo::new(capabilities)
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Inspect and automate one explicitly instrumented GPUI window. Prefer semantic element tools over coordinates. Pointer actions require app-provided NodeSpec::on_event handlers; keyboard input uses GPUI directly. Screenshots and snapshots remain in memory, and all coordinates are logical pixels relative to the target window. Video recording is a continuous bounded screencast by default: keep one MCP transport open for start_video_recording and stop_video_recording; capture_video_frame is an optional explicit checkpoint. The recording artifact is an H.264 MP4 with an optional GPUI pointer overlay; it never controls or reads the global OS cursor. Video tools accept only a portable MP4 artifact name inside the server-configured artifact directory; overwrite is opt-in."
                    .to_owned(),
            )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let result = self
            .client
            .call(Operation::ListContextResources)
            .await
            .map_err(context_resource_error)?;
        let BridgeResult::ContextResources(resources) = result else {
            return Err(ErrorData::internal_error(
                "bridge returned the wrong result for context resources",
                None,
            ));
        };
        Ok(ListResourcesResult::with_all_items(
            resources.into_iter().map(mcp_resource).collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let result = self
            .client
            .call(Operation::ReadContextResource {
                uri: request.uri.clone(),
            })
            .await
            .map_err(context_resource_error)?;
        let BridgeResult::ContextResource(resource) = result else {
            return Err(ErrorData::internal_error(
                "bridge returned the wrong result for a context resource",
                None,
            ));
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(resource.text, resource.descriptor.uri)
                .with_mime_type(resource.descriptor.mime_type),
        ]))
    }
}

fn mcp_resource(descriptor: ContextResourceDescriptor) -> Resource {
    let mut resource =
        Resource::new(descriptor.uri, descriptor.name).with_mime_type(descriptor.mime_type);
    if let Some(title) = descriptor.title {
        resource = resource.with_title(title);
    }
    if let Some(description) = descriptor.description {
        resource = resource.with_description(description);
    }
    if let Some(size) = descriptor.size {
        resource = resource.with_size(size);
    }
    resource
}

fn context_resource_error(message: String) -> ErrorData {
    if message.starts_with("NotFound:") {
        ErrorData::resource_not_found(message, None)
    } else if message.starts_with("InvalidRequest:") {
        ErrorData::invalid_params(message, None)
    } else {
        ErrorData::internal_error(message, None)
    }
}

fn default_true() -> bool {
    true
}

fn default_result_limit() -> u16 {
    50
}

fn default_click_count() -> u8 {
    1
}

fn default_video_fps() -> u8 {
    10
}

fn default_drag_steps() -> u8 {
    12
}

fn default_timeout_ms() -> u64 {
    5_000
}

fn default_highlight_color() -> String {
    "#00A8FFFF".to_owned()
}

fn default_log_limit() -> u16 {
    100
}

fn validate_native_point(point: Point) -> Result<(), String> {
    if !point.is_valid() {
        return Err("native input coordinates must be finite".to_owned());
    }
    if point.x.abs() > 1_000_000.0 || point.y.abs() > 1_000_000.0 {
        return Err("native input coordinates exceed the safety bound".to_owned());
    }
    Ok(())
}

fn validate_native_scroll_delta(delta_x: f32, delta_y: f32) -> Result<(), String> {
    if !delta_x.is_finite() || !delta_y.is_finite() {
        return Err("native scroll deltas must be finite".to_owned());
    }
    if delta_x.abs() > 100_000.0 || delta_y.abs() > 100_000.0 {
        return Err("native scroll delta exceeds the safety bound".to_owned());
    }
    Ok(())
}

fn find_nodes<'a>(tree: &'a UiTree, args: &FindArgs) -> Vec<&'a UiNode> {
    let limit = usize::from(args.limit.clamp(1, 200));
    let query_lower = args.query.as_ref().map(|query| query.to_lowercase());
    tree.nodes
        .values()
        .filter(|node| !args.visible_only || node.state.visible)
        .filter(|node| args.role.is_none_or(|role| node.role == role))
        .filter(|node| {
            let Some(query) = args.query.as_deref() else {
                return true;
            };
            let label = node.label.as_deref().unwrap_or_default();
            if args.exact {
                label == query
            } else {
                label
                    .to_lowercase()
                    .contains(query_lower.as_deref().unwrap_or_default())
            }
        })
        .take(limit)
        .collect()
}

fn get_node<'a>(tree: &'a UiTree, id: &str) -> Result<&'a UiNode, String> {
    tree.nodes
        .get(id)
        .ok_or_else(|| format!("semantic element {id:?} was not found"))
}

fn require_bounds(node: &UiNode) -> Result<Rect, String> {
    node.bounds
        .filter(|bounds| bounds.is_valid())
        .ok_or_else(|| format!("element {:?} has no valid current bounds", node.id))
}

fn validate_value(input: &str, annotation: &ValueInfo) -> Result<(), String> {
    if annotation.min.is_none() && annotation.max.is_none() && annotation.step.is_none() {
        return Ok(());
    }
    let number: f64 = input
        .parse()
        .map_err(|_| "annotated numeric value requires a finite number".to_owned())?;
    if !number.is_finite() {
        return Err("annotated numeric value requires a finite number".to_owned());
    }
    if annotation.min.is_some_and(|min| number < min) {
        return Err(format!(
            "value is below the annotated minimum {min}",
            min = annotation.min.unwrap_or_default()
        ));
    }
    if annotation.max.is_some_and(|max| number > max) {
        return Err(format!(
            "value is above the annotated maximum {max}",
            max = annotation.max.unwrap_or_default()
        ));
    }
    Ok(())
}

fn validate_timeout(timeout_ms: u64) -> Result<(), String> {
    if timeout_ms == 0 || timeout_ms > MAX_WAIT_MS {
        return Err("timeout/duration must be between 1 and 30000 milliseconds".to_owned());
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "snapshot name must contain 1-64 ASCII letters, digits, '.', '_' or '-'".to_owned(),
        );
    }
    Ok(())
}

fn state_matches(state: &NodeState, args: &WaitStateArgs) -> bool {
    args.visible
        .is_none_or(|expected| state.visible == expected)
        && args
            .enabled
            .is_none_or(|expected| state.enabled == expected)
        && args
            .focused
            .is_none_or(|expected| state.focused == expected)
        && args
            .checked
            .is_none_or(|expected| state.checked == Some(expected))
        && args
            .selected
            .is_none_or(|expected| state.selected == Some(expected))
        && args
            .expanded
            .is_none_or(|expected| state.expanded == Some(expected))
}

fn tree_diff(left: &UiTree, right: &UiTree) -> JsonValue {
    let left_ids: BTreeSet<_> = left.nodes.keys().cloned().collect();
    let right_ids: BTreeSet<_> = right.nodes.keys().cloned().collect();
    let added: Vec<_> = right_ids.difference(&left_ids).cloned().collect();
    let removed: Vec<_> = left_ids.difference(&right_ids).cloned().collect();
    let changed: Vec<_> = left_ids
        .intersection(&right_ids)
        .filter(|id| left.nodes.get(*id) != right.nodes.get(*id))
        .cloned()
        .collect();
    json!({
        "left_generation": left.generation,
        "right_generation": right.generation,
        "added": added,
        "removed": removed,
        "changed": changed,
        "identical": added.is_empty() && removed.is_empty() && changed.is_empty(),
    })
}

fn image_result(screenshot: Screenshot) -> CallToolResult {
    let metadata = json!({
        "mime_type": screenshot.mime_type,
        "width": screenshot.width,
        "height": screenshot.height,
    });
    let mut result = CallToolResult::success(vec![
        ContentBlock::text(metadata.to_string()),
        ContentBlock::image(screenshot.base64_data, screenshot.mime_type),
    ]);
    result.structured_content = Some(metadata);
    result
}

// Counts are bounded to 64 megapixels above, so conversion to f64 is well
// inside the exact-integer range needed for deterministic comparison metrics.
#[allow(clippy::cast_precision_loss)]
fn compare_images(
    left: &Screenshot,
    right: &Screenshot,
    tolerance: u8,
) -> Result<(JsonValue, Screenshot), String> {
    let left_image = decode_image(left)?;
    let right_image = decode_image(right)?;
    if left_image.dimensions() != right_image.dimensions() {
        return Err(format!(
            "image dimensions differ: {}x{} versus {}x{}",
            left_image.width(),
            left_image.height(),
            right_image.width(),
            right_image.height()
        ));
    }
    let pixel_count = u64::from(left_image.width()) * u64::from(left_image.height());
    if pixel_count > 64_000_000 {
        return Err("image comparison exceeds the 64 megapixel safety bound".to_owned());
    }
    let mut changed_pixels = 0_u64;
    let mut absolute_difference = 0_u64;
    let mut diff = RgbaImage::new(left_image.width(), left_image.height());
    for (x, y, left_pixel) in left_image.enumerate_pixels() {
        let right_pixel = right_image.get_pixel(x, y);
        let differences = [
            left_pixel[0].abs_diff(right_pixel[0]),
            left_pixel[1].abs_diff(right_pixel[1]),
            left_pixel[2].abs_diff(right_pixel[2]),
            left_pixel[3].abs_diff(right_pixel[3]),
        ];
        absolute_difference += differences
            .iter()
            .map(|value| u64::from(*value))
            .sum::<u64>();
        let changed = differences.iter().any(|value| *value > tolerance);
        if changed {
            changed_pixels = changed_pixels.saturating_add(1);
            diff.put_pixel(x, y, Rgba([255, differences[1], differences[2], 255]));
        } else {
            let gray = u8::try_from(
                (u16::from(right_pixel[0]) + u16::from(right_pixel[1]) + u16::from(right_pixel[2]))
                    / 6,
            )
            .unwrap_or(u8::MAX);
            diff.put_pixel(x, y, Rgba([gray, gray, gray, 255]));
        }
    }
    let channel_total = pixel_count.saturating_mul(4).saturating_mul(255);
    let similarity = if channel_total == 0 {
        1.0
    } else {
        1.0 - absolute_difference as f64 / channel_total as f64
    };
    let metrics = json!({
        "width": left_image.width(),
        "height": left_image.height(),
        "pixel_count": pixel_count,
        "changed_pixels": changed_pixels,
        "changed_ratio": if pixel_count == 0 { 0.0 } else { changed_pixels as f64 / pixel_count as f64 },
        "mean_absolute_channel_difference": if pixel_count == 0 { 0.0 } else { absolute_difference as f64 / (pixel_count * 4) as f64 },
        "similarity": similarity,
        "tolerance": tolerance,
    });
    let screenshot = encode_image(diff)?;
    Ok((metrics, screenshot))
}

fn decode_image(screenshot: &Screenshot) -> Result<RgbaImage, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&screenshot.base64_data)
        .map_err(|_| "stored screenshot base64 is invalid".to_owned())?;
    image::load_from_memory(&bytes)
        .map(DynamicImage::into_rgba8)
        .map_err(|_| "stored screenshot PNG is invalid".to_owned())
}

fn encode_image(image: RgbaImage) -> Result<Screenshot, String> {
    let width = image.width();
    let height = image.height();
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|_| "could not encode the screenshot diff".to_owned())?;
    Ok(Screenshot {
        mime_type: "image/png".to_owned(),
        base64_data: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
        width,
        height,
    })
}

fn performance_assessment(stats: &FrameStats) -> &'static str {
    if stats.sample_count == 0 {
        "no frame samples have been observed"
    } else if stats.prepaint_average_ms + stats.root_paint_average_ms <= 16.67 {
        "average measured render work is within a 60 FPS frame budget"
    } else if stats.prepaint_average_ms + stats.root_paint_average_ms <= 33.33 {
        "average measured render work is within a 30 FPS frame budget but above a 60 FPS budget"
    } else {
        "average measured render work is above a 30 FPS frame budget"
    }
}

fn is_stale_generation_error(error: &str) -> bool {
    error.starts_with("StaleGeneration:")
}

fn ack_json(action: &'static str) -> Json<Value> {
    object_output(json!({ "ok": true, "action": action }))
}

fn object_output(value: JsonValue) -> Json<ObjectOutput> {
    let fields = match value {
        JsonValue::Object(fields) => fields.into_iter().collect(),
        other => BTreeMap::from([("value".to_owned(), other)]),
    };
    Json(ObjectOutput { fields })
}

fn encode_error(_error: serde_json::Error) -> String {
    "could not encode the tool result".to_owned()
}

fn map_wait_error(error: String, subject: &str) -> String {
    if error.starts_with("Timeout:") {
        format!("timed out waiting for the {subject}")
    } else {
        error
    }
}

#[cfg(test)]
fn default_result_limit_for_test() -> u16 {
    default_result_limit()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use gpui_mcp_protocol::{BridgeResult, FrameStats, NodeState, Operation, UiNode};
    use serde_json::json;

    use crate::recording::validate_frame_delay;

    use super::{
        FindArgs, Role, StartVideoRecordingArgs, UiTree, VideoCaptureMode, WaitStateArgs,
        default_result_limit_for_test, find_nodes, is_stale_generation_error,
        settle_refresh_frames, state_matches, tree_diff,
    };

    #[test]
    fn semantic_retry_matches_only_typed_stale_generation_errors() {
        assert!(is_stale_generation_error(
            "StaleGeneration: semantic tree changed"
        ));
        assert!(!is_stale_generation_error(
            "NotFound: StaleGeneration is only message text"
        ));
    }

    #[tokio::test]
    async fn mutation_settlement_waits_from_each_refresh_token() -> Result<(), String> {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let responses = Arc::new(Mutex::new(VecDeque::from([
            Ok(BridgeResult::FrameStats(FrameStats {
                frame_count: 11,
                ..FrameStats::default()
            })),
            Ok(BridgeResult::FrameStats(FrameStats {
                frame_count: 12,
                ..FrameStats::default()
            })),
            Ok(BridgeResult::FrameStats(FrameStats {
                frame_count: 14,
                ..FrameStats::default()
            })),
            Ok(BridgeResult::FrameStats(FrameStats {
                frame_count: 15,
                ..FrameStats::default()
            })),
        ])));
        let recorded = operations.clone();
        let queued = responses.clone();
        let settled = settle_refresh_frames(Duration::from_secs(2), move |operation| {
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(operation);
            let response = queued
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(|| Err("test response queue exhausted".to_owned()));
            async move { response }
        })
        .await?;

        assert_eq!(settled.frame_count, 15);
        let operations = operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(matches!(operations[0], Operation::Refresh));
        assert!(matches!(
            operations[1],
            Operation::WaitForFrame {
                after_frame_count: 11,
                ..
            }
        ));
        assert!(matches!(operations[2], Operation::Refresh));
        assert!(matches!(
            operations[3],
            Operation::WaitForFrame {
                after_frame_count: 14,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn find_is_case_insensitive_by_default() {
        let node = UiNode {
            id: "save".to_owned(),
            parent: None,
            children: Vec::new(),
            role: Role::Button,
            label: Some("Save Document".to_owned()),
            description: None,
            bounds: None,
            state: NodeState::default(),
            actions: Vec::new(),
            text: None,
            value: None,
            metadata: BTreeMap::new(),
        };
        let tree = UiTree {
            generation: 1,
            roots: vec!["save".to_owned()],
            nodes: BTreeMap::from([("save".to_owned(), node)]),
            diagnostics: Vec::new(),
        };
        let found = find_nodes(
            &tree,
            &FindArgs {
                query: Some("document".to_owned()),
                role: Some(Role::Button),
                exact: false,
                visible_only: true,
                limit: default_result_limit_for_test(),
            },
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn state_wait_matches_expanded_disclosures() {
        let state = NodeState {
            expanded: Some(true),
            ..NodeState::default()
        };
        let expanded = WaitStateArgs {
            id: "details".to_owned(),
            visible: None,
            enabled: None,
            focused: None,
            checked: None,
            selected: None,
            expanded: Some(true),
            timeout_ms: 1_000,
        };
        assert!(state_matches(&state, &expanded));

        let collapsed = WaitStateArgs {
            expanded: Some(false),
            ..expanded
        };
        assert!(!state_matches(&state, &collapsed));
    }

    #[test]
    fn tree_diff_reports_added_ids() {
        let left = UiTree::default();
        let right = UiTree {
            generation: 1,
            roots: vec!["new".to_owned()],
            nodes: BTreeMap::from([(
                "new".to_owned(),
                UiNode {
                    id: "new".to_owned(),
                    parent: None,
                    children: Vec::new(),
                    role: Role::Generic,
                    label: None,
                    description: None,
                    bounds: None,
                    state: NodeState::default(),
                    actions: Vec::new(),
                    text: None,
                    value: None,
                    metadata: BTreeMap::new(),
                },
            )]),
            diagnostics: Vec::new(),
        };
        assert_eq!(tree_diff(&left, &right)["added"], json!(["new"]));
    }

    #[test]
    fn complete_tool_suite_is_registered() {
        let names: BTreeSet<String> = super::GpuiMcp::production_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        let expected = BTreeSet::from(
            [
                "ping",
                "check_connection",
                "list_app_commands",
                "execute_app_command",
                "get_frame_stats",
                "record_performance",
                "get_performance_report",
                "get_logs",
                "clear_logs",
                "click_element",
                "double_click_element",
                "click_coordinates",
                "hover_element",
                "drag_element",
                "drag_coordinates",
                "pointer_location",
                "pointer_move",
                "pointer_down",
                "pointer_up",
                "pointer_click",
                "pointer_drag",
                "pointer_scroll",
                "native_click",
                "native_double_click",
                "native_hover",
                "native_drag",
                "native_scroll",
                "native_click_coordinates",
                "native_drag_coordinates",
                "native_scroll_coordinates",
                "keyboard",
                "type_text",
                "focus_element",
                "get_text_info",
                "set_text",
                "get_value",
                "set_value",
                "get_selection_count",
                "get_element_state",
                "scroll",
                "get_live_document",
                "preview_live_document",
                "get_ui_tree",
                "find_elements",
                "get_element",
                "get_element_bounds",
                "wait_for_element",
                "wait_for_state",
                "save_ui_snapshot",
                "load_ui_snapshot",
                "diff_ui_snapshots",
                "diff_current_ui",
                "screenshot",
                "screenshot_region",
                "screenshot_element",
                "highlight_elements",
                "clear_highlights",
                "capture_screenshot_snapshot",
                "compare_screenshots",
                "diff_screenshots",
                "start_video_recording",
                "capture_video_frame",
                "stop_video_recording",
            ]
            .map(str::to_owned),
        );
        assert_eq!(names, expected);
    }

    #[test]
    fn default_video_recording_uses_continuous_screencast_mode() -> Result<(), String> {
        let args = serde_json::from_value::<StartVideoRecordingArgs>(json!({}))
            .map_err(|error| error.to_string())?;

        assert_eq!(args.mode, VideoCaptureMode::Continuous);
        assert_eq!(args.frames_per_second, 10);
        validate_frame_delay(1_000 / u32::from(args.frames_per_second))
    }

    #[test]
    fn stop_video_recording_schema_exposes_input_bounds() -> Result<(), String> {
        let router = super::GpuiMcp::production_router();
        let Some(tool) = router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "stop_video_recording")
        else {
            return Err("stop_video_recording was not registered".to_owned());
        };
        let properties = tool
            .input_schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| "stop_video_recording schema has no properties".to_owned())?;
        let delay = properties
            .get("frame_duration_ms")
            .ok_or_else(|| "frame_duration_ms schema is missing".to_owned())?;
        assert_eq!(delay.get("minimum"), Some(&json!(20)));
        assert_eq!(delay.get("maximum"), Some(&json!(10_000)));
        let artifact_name = properties
            .get("artifact_name")
            .ok_or_else(|| "artifact_name schema is missing".to_owned())?;
        assert_eq!(artifact_name.get("minLength"), Some(&json!(1)));
        assert_eq!(artifact_name.get("maxLength"), Some(&json!(128)));
        assert_eq!(
            artifact_name.get("pattern"),
            Some(&json!(r"^[A-Za-z0-9][A-Za-z0-9._-]*[.]mp4$"))
        );
        Ok(())
    }
}
