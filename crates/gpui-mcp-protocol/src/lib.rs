//! Shared data model and bounded local wire protocol for `gpui-mcp`.
//!
//! The protocol intentionally contains no platform-specific types. Coordinates are
//! logical GPUI pixels relative to the target window.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Current wire protocol version.
pub const PROTOCOL_VERSION: u16 = 9;
/// Maximum accepted request frame, including its four-byte length prefix.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Maximum accepted response frame. Screenshots are base64 encoded inside it.
pub const MAX_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
/// Maximum length of user-supplied text or a keystroke string.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Maximum number of nodes retained in one semantic tree.
pub const MAX_TREE_NODES: usize = 50_000;
/// Maximum length of a semantic node identifier.
pub const MAX_ID_BYTES: usize = 256;
/// Maximum length of a label or description.
pub const MAX_LABEL_BYTES: usize = 4 * 1024;
/// Maximum metadata fields accepted on one semantic node.
pub const MAX_METADATA_FIELDS: usize = 32;
/// Maximum metadata key length.
pub const MAX_METADATA_KEY_BYTES: usize = 128;
/// Maximum metadata value length.
pub const MAX_METADATA_VALUE_BYTES: usize = 4 * 1024;
/// Maximum server-side wait duration.
pub const MAX_WAIT_MS: u64 = 30_000;
/// Maximum combined UTF-8 bytes in one live HTML/CSS/RON source bundle.
pub const MAX_LIVE_DOCUMENT_SOURCE_BYTES: usize = 768 * 1024;
/// Maximum diagnostics returned with one live document.
pub const MAX_LIVE_DOCUMENT_DIAGNOSTICS: usize = 256;
/// Maximum number of application-owned context resources exposed by one endpoint.
pub const MAX_CONTEXT_RESOURCES: usize = 64;
/// Maximum UTF-8 bytes returned by one application-owned context resource.
pub const MAX_CONTEXT_RESOURCE_BYTES: usize = 256 * 1024;
/// Maximum length of an application-owned context resource URI.
pub const MAX_CONTEXT_RESOURCE_URI_BYTES: usize = 512;
/// Maximum application commands exposed by one endpoint.
pub const MAX_APPLICATION_COMMANDS: usize = 64;
/// Maximum serialized JSON Schema bytes for one command.
pub const MAX_APPLICATION_COMMAND_SCHEMA_BYTES: usize = 32 * 1024;
/// Maximum serialized output bytes returned by one command.
pub const MAX_APPLICATION_COMMAND_OUTPUT_BYTES: usize = 256 * 1024;

/// A point in logical window pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

impl Point {
    /// Returns whether both coordinates are finite.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// A rectangle in logical window pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Rect {
    /// Left coordinate.
    pub x: f32,
    /// Top coordinate.
    pub y: f32,
    /// Width, which must be non-negative.
    pub width: f32,
    /// Height, which must be non-negative.
    pub height: f32,
}

impl Rect {
    /// Returns whether the rectangle is finite and non-negative.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= 0.0
            && self.height >= 0.0
    }

    /// Returns the center of this rectangle.
    #[must_use]
    pub fn center(self) -> Point {
        Point {
            x: self.x + self.width / 2.0,
            y: self.y + self.height / 2.0,
        }
    }

    /// Returns whether this rectangle contains the supplied point.
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x <= self.x + self.width
            && point.y <= self.y + self.height
    }
}

/// Semantic role of a GPUI element.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Generic element with no stronger semantics.
    #[default]
    Generic,
    /// Root application region.
    Application,
    /// Top-level window.
    Window,
    /// Group or container.
    Group,
    /// Button.
    Button,
    /// Checkbox.
    Checkbox,
    /// Radio button.
    Radio,
    /// Switch or toggle.
    Switch,
    /// Link.
    Link,
    /// Static text.
    Text,
    /// Editable text field.
    TextInput,
    /// Search field.
    SearchInput,
    /// Slider.
    Slider,
    /// Progress indicator.
    Progress,
    /// Image.
    Image,
    /// List.
    List,
    /// List row or item.
    ListItem,
    /// Hierarchical tree.
    Tree,
    /// Item in a hierarchical tree.
    TreeItem,
    /// Table.
    Table,
    /// Table row.
    Row,
    /// Table cell.
    Cell,
    /// Menu.
    Menu,
    /// Menu item.
    MenuItem,
    /// Select-like popup trigger with an editable or read-only value.
    Combobox,
    /// Option in a listbox or combobox popup.
    Option,
    /// Non-interactive separator between related groups.
    Separator,
    /// Contextual description shown for a control.
    Tooltip,
    /// Tab list.
    TabList,
    /// Tab.
    Tab,
    /// Toolbar.
    Toolbar,
    /// Dialog.
    Dialog,
    /// Alert.
    Alert,
    /// Scrollable region.
    ScrollArea,
}

/// Automation action advertised by a node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeAction {
    /// Click or activate the node.
    Click,
    /// Move focus to the node by clicking its center.
    Focus,
    /// Deliver a semantic hover event.
    Hover,
    /// Deliver a semantic drag event.
    Drag,
    /// Replace editable text.
    SetText,
    /// Set a numeric or string value.
    SetValue,
    /// Scroll at the node's center.
    Scroll,
}

/// Optional editable text metadata supplied by the application.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TextInfo {
    /// Current text. Applications may omit secret values.
    pub text: String,
    /// UTF-8 byte index of the caret.
    pub caret: Option<usize>,
    /// Selected UTF-8 byte range `[start, end)`.
    pub selection: Option<TextRange>,
    /// Whether the value is intentionally redacted.
    pub redacted: bool,
}

/// A validated half-open UTF-8 byte range.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TextRange {
    /// Inclusive start byte index.
    pub start: usize,
    /// Exclusive end byte index.
    pub end: usize,
}

/// Optional numeric value metadata supplied by the application.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ValueInfo {
    /// Current display value.
    pub value: String,
    /// Optional numeric minimum.
    pub min: Option<f64>,
    /// Optional numeric maximum.
    pub max: Option<f64>,
    /// Optional numeric increment.
    pub step: Option<f64>,
}

/// Dynamic state of a semantic node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeState {
    /// Whether the application considers the node visible.
    pub visible: bool,
    /// Whether the node accepts input.
    pub enabled: bool,
    /// Whether the node owns keyboard focus.
    pub focused: bool,
    /// Optional checked state.
    pub checked: Option<bool>,
    /// Optional selected state.
    pub selected: Option<bool>,
    /// Optional expanded state.
    pub expanded: Option<bool>,
}

impl Default for NodeState {
    fn default() -> Self {
        Self {
            visible: true,
            enabled: true,
            focused: false,
            checked: None,
            selected: None,
            expanded: None,
        }
    }
}

/// One annotated element in the latest GPUI frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UiNode {
    /// Stable application-provided identifier.
    pub id: String,
    /// Parent identifier, or `None` for a root.
    pub parent: Option<String>,
    /// Ordered direct child identifiers.
    pub children: Vec<String>,
    /// Semantic role.
    pub role: Role,
    /// Human-readable accessible label.
    pub label: Option<String>,
    /// Additional accessible description.
    pub description: Option<String>,
    /// Bounds in logical pixels for the current frame.
    pub bounds: Option<Rect>,
    /// Current state.
    pub state: NodeState,
    /// Actions accepted by the node.
    pub actions: Vec<NodeAction>,
    /// Optional text metadata.
    pub text: Option<TextInfo>,
    /// Optional value metadata.
    pub value: Option<ValueInfo>,
    /// Small, non-secret application-defined metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

/// A stable snapshot of the latest annotated GPUI frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UiTree {
    /// Monotonically increasing frame generation.
    pub generation: u64,
    /// Ordered root node identifiers.
    pub roots: Vec<String>,
    /// Nodes keyed by identifier for efficient lookup.
    pub nodes: BTreeMap<String, UiNode>,
    /// Validation problems observed while collecting this generation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<SemanticDiagnostic>,
}

/// Stable code for a semantic-tree validation problem.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDiagnosticCode {
    /// A node failed field validation and was omitted.
    InvalidNode,
    /// Two rendered nodes supplied the same identifier.
    DuplicateId,
    /// A node named a parent that was not present.
    MissingParent,
    /// A parent relationship formed a cycle and was broken.
    ParentCycle,
    /// The bounded tree capacity was exceeded.
    CapacityExceeded,
}

/// One sanitized semantic-tree validation diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SemanticDiagnostic {
    /// Machine-readable diagnostic category.
    pub code: SemanticDiagnosticCode,
    /// Related node identifier when one was available and safe.
    pub node_id: Option<String>,
    /// Bounded explanation suitable for application developers.
    pub message: String,
}

/// Mouse button used by pointer input.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    /// Primary pointer button.
    #[default]
    Left,
    /// Secondary pointer button.
    Right,
    /// Middle pointer button.
    Middle,
}

/// Delta carried by a synthetic native scroll-wheel event.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum NativeScrollDelta {
    /// Exact logical-pixel delta.
    Pixels {
        /// Horizontal logical-pixel delta.
        delta_x: f32,
        /// Vertical logical-pixel delta.
        delta_y: f32,
    },
    /// Inexact line delta interpreted by GPUI.
    Lines {
        /// Horizontal line delta.
        delta_x: f32,
        /// Vertical line delta.
        delta_y: f32,
    },
}

/// One synthetic platform event dispatched through GPUI's native pointer pipeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeInputCommand {
    /// Move the native pointer state to a window-relative logical point.
    MouseMove {
        /// Target point.
        point: Point,
        /// Button held during the move, when this is part of a drag.
        pressed_button: Option<MouseButton>,
    },
    /// Press a native mouse button at a window-relative logical point.
    MouseDown {
        /// Target point.
        point: Point,
        /// Pointer button.
        button: MouseButton,
        /// Click count from one through three.
        click_count: u8,
    },
    /// Release a native mouse button at a window-relative logical point.
    MouseUp {
        /// Target point.
        point: Point,
        /// Pointer button.
        button: MouseButton,
        /// Click count from one through three.
        click_count: u8,
    },
    /// Dispatch a native scroll-wheel event at a window-relative logical point.
    ScrollWheel {
        /// Pointer position used for native hit testing.
        point: Point,
        /// Pixel or line delta passed to GPUI.
        delta: NativeScrollDelta,
    },
}

/// Semantic input command dispatched on the GPUI foreground executor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputCommand {
    /// Click a window-relative point.
    Click {
        /// Target point.
        point: Point,
        /// Pointer button.
        button: MouseButton,
        /// One for click, two for double-click.
        count: u8,
    },
    /// Hover a window-relative point.
    Hover {
        /// Target point.
        point: Point,
    },
    /// Drag between two points.
    Drag {
        /// Start point.
        from: Point,
        /// End point.
        to: Point,
        /// Number of interpolation steps, from 1 through 120.
        steps: u8,
    },
    /// Dispatch one GPUI keystroke string.
    Key {
        /// GPUI keystroke syntax, such as `ctrl-a` or `enter`.
        keystroke: String,
    },
    /// Insert text using GPUI key events.
    TypeText {
        /// Text to type.
        text: String,
    },
    /// Select all in the currently focused field and type replacement text.
    ReplaceText {
        /// Text to insert.
        text: String,
    },
    /// Scroll at a point by a logical-pixel delta.
    Scroll {
        /// Pointer position used for hit testing.
        point: Point,
        /// Horizontal pixel delta.
        delta_x: f32,
        /// Vertical pixel delta.
        delta_y: f32,
    },
}

/// Exact semantic action invoked against one stable node identifier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticAction {
    /// Click or activate the node.
    Click {
        /// Requested pointer button.
        button: MouseButton,
        /// Click count from one through three.
        count: u8,
    },
    /// Move keyboard focus to the node.
    Focus,
    /// Notify the node of semantic hover.
    Hover,
    /// Drag from the node to a window-relative destination.
    Drag {
        /// Window-relative destination.
        to: Point,
        /// Requested interpolation steps from one through 120.
        steps: u8,
    },
    /// Scroll the node by logical-pixel deltas.
    Scroll {
        /// Horizontal logical-pixel delta.
        delta_x: f32,
        /// Vertical logical-pixel delta.
        delta_y: f32,
    },
    /// Replace editable text through application logic.
    SetText {
        /// Replacement UTF-8 text.
        text: String,
    },
    /// Replace a value through application logic.
    SetValue {
        /// Replacement display value.
        value: String,
    },
}

impl SemanticAction {
    /// Return the capability that a target node must advertise.
    #[must_use]
    pub const fn required_node_action(&self) -> NodeAction {
        match self {
            Self::Click { .. } => NodeAction::Click,
            Self::Focus => NodeAction::Focus,
            Self::Hover => NodeAction::Hover,
            Self::Drag { .. } => NodeAction::Drag,
            Self::Scroll { .. } => NodeAction::Scroll,
            Self::SetText { .. } => NodeAction::SetText,
            Self::SetValue { .. } => NodeAction::SetValue,
        }
    }
}

/// Application-reported result of a semantic action handler.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActionOutcome {
    /// The application accepted and handled the action.
    Handled,
    /// The application deliberately declined the action.
    Rejected {
        /// Bounded, non-sensitive explanation.
        reason: String,
    },
}

/// Requested screenshot scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScreenshotTarget {
    /// Capture the whole application window.
    #[default]
    Window,
    /// Capture a logical-pixel region of the window.
    Region {
        /// Region to capture.
        rect: Rect,
    },
}

/// Highlight description rendered by the GPUI integration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Highlight {
    /// Logical-pixel rectangle.
    pub rect: Rect,
    /// Eight-digit `#RRGGBBAA` color.
    pub color: String,
    /// Optional short label.
    pub label: Option<String>,
}

/// One retained diagnostic log entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LogEntry {
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: u64,
    /// Lowercase severity name.
    pub level: String,
    /// Sanitized log message.
    pub message: String,
}

/// Frame timing summary produced by the application integration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FrameStats {
    /// Monotonic token of the most recently completed root-paint frame.
    pub frame_count: u64,
    /// Number of retained interval samples.
    pub sample_count: u32,
    /// Mean time between semantic-root prepaint starts.
    pub frame_interval_average_ms: f64,
    /// Longest retained time between semantic-root prepaint starts.
    pub frame_interval_max_ms: f64,
    /// Mean elapsed prepaint phase from the semantic root through deferred prepaint.
    pub prepaint_average_ms: f64,
    /// Longest retained semantic prepaint phase.
    pub prepaint_max_ms: f64,
    /// Mean time spent painting the semantic root subtree.
    pub root_paint_average_ms: f64,
    /// Longest retained semantic root-subtree paint time.
    pub root_paint_max_ms: f64,
    /// Observed repaint cadence from retained frame intervals.
    ///
    /// This is not rendering capacity for an event-driven UI: an idle application intentionally
    /// has a low value because it does not continuously request frames.
    pub estimated_fps: f64,
}

/// A PNG screenshot returned without touching the filesystem.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Screenshot {
    /// MIME type, currently always `image/png`.
    pub mime_type: String,
    /// Base64-encoded PNG bytes.
    pub base64_data: String,
    /// Physical image width.
    pub width: u32,
    /// Physical image height.
    pub height: u32,
}

/// One bridge feature advertised in the endpoint descriptor.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Semantic-tree discovery.
    SemanticTree,
    /// GPUI input dispatch.
    Input,
    /// Screenshot capture.
    Screenshot,
    /// Visual highlighting.
    Highlight,
    /// Frame metrics.
    Performance,
    /// Bounded diagnostic logs.
    Logs,
    /// Revisioned, in-memory live HTML document preview.
    LiveDocument,
    /// Bounded application-owned MCP context resources.
    ContextResources,
    /// Bounded application-owned structured commands.
    ApplicationCommands,
}

/// Discoverable application command exposed as a generic MCP mutation tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApplicationCommandDescriptor {
    /// Stable lowercase command name.
    pub name: String,
    /// Human-readable title.
    pub title: String,
    /// Bounded explanation of behavior and side effects.
    pub description: String,
    /// JSON Schema for the command arguments.
    pub input_schema: serde_json::Value,
    /// Whether executing the command can mutate application state.
    pub mutating: bool,
}

/// Validated result returned by one application command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApplicationCommandResult {
    /// Command name that executed.
    pub name: String,
    /// Current application-owned revision after execution, when applicable.
    pub revision: Option<u64>,
    /// Structured, bounded result payload.
    pub output: serde_json::Value,
}

/// Discoverable metadata for one application-owned MCP context resource.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContextResourceDescriptor {
    /// Stable URI used by MCP `resources/read`.
    pub uri: String,
    /// Stable programmatic resource name.
    pub name: String,
    /// Optional human-readable display title.
    pub title: Option<String>,
    /// Optional bounded explanation for an MCP client or model.
    pub description: Option<String>,
    /// MIME type of the returned UTF-8 text.
    pub mime_type: String,
    /// Current UTF-8 content size, when cheaply known by the application.
    pub size: Option<u64>,
}

/// One bounded UTF-8 application-owned MCP context resource.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContextResource {
    /// Discoverable metadata matching the requested resource.
    pub descriptor: ContextResourceDescriptor,
    /// Current resource contents, resolved when read rather than cached by the bridge.
    pub text: String,
}

/// Complete source bundle for one live pure-HTML document revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LiveDocumentSource {
    /// Standard HTML structure and semantics.
    pub html: String,
    /// Standard local CSS presentation.
    pub css: String,
    /// Versioned exact hook/state bindings encoded as RON.
    pub bindings_ron: String,
}

impl LiveDocumentSource {
    /// Combined UTF-8 source byte length.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.html
            .len()
            .saturating_add(self.css.len())
            .saturating_add(self.bindings_ron.len())
    }
}

/// Sanitized compiler or renderer diagnostic for a live source revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LiveDocumentDiagnostic {
    /// `warning` or `error`.
    pub severity: String,
    /// Human-readable bounded diagnostic message.
    pub message: String,
}

/// Active in-memory live document returned by an application host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LiveDocument {
    /// Monotonic in-process revision used for optimistic concurrency.
    pub revision: u64,
    /// Complete source for this revision.
    pub source: LiveDocumentSource,
    /// Non-fatal compiler and renderer diagnostics.
    pub diagnostics: Vec<LiveDocumentDiagnostic>,
}

/// Outcome of compiling one candidate against the active live document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LiveDocumentPreview {
    /// Whether the candidate became the active document.
    pub applied: bool,
    /// Active document after the attempt (the last-good revision on rejection).
    pub document: LiveDocument,
    /// Candidate-specific compile, binding, or hook diagnostics.
    pub diagnostics: Vec<LiveDocumentDiagnostic>,
}

/// Bridge capability set advertised in the endpoint descriptor.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    /// Available features.
    pub available: BTreeSet<Capability>,
}

impl Capabilities {
    /// Create a capability set from an iterator.
    #[must_use]
    pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            available: capabilities.into_iter().collect(),
        }
    }

    /// Return whether one feature is available.
    #[must_use]
    pub fn supports(&self, capability: Capability) -> bool {
        self.available.contains(&capability)
    }
}

/// Private endpoint descriptor written by the GPUI process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EndpointDescriptor {
    /// Wire protocol version.
    pub protocol_version: u16,
    /// Validated application identifier.
    pub app_id: String,
    /// Human-readable application name.
    pub app_name: String,
    /// Random, non-secret identifier for this bridge lifetime.
    ///
    /// Unlike `app_id`, this distinguishes concurrently running windows of the
    /// same application. It is safe to expose to MCP clients and must never be
    /// used as an authentication credential.
    pub instance_id: u64,
    /// Process identifier that owns the endpoint.
    pub pid: u32,
    /// Owner-restricted local socket or named-pipe endpoint.
    pub endpoint: LocalEndpoint,
    /// Random per-process bearer secret.
    pub token: String,
    /// Exact native title used for screenshot window matching.
    pub window_title: String,
    /// Supported operations.
    pub capabilities: Capabilities,
}

/// Cross-platform local IPC address.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocalEndpoint {
    /// Filesystem Unix-domain socket inside the private endpoint directory.
    Filesystem {
        /// Absolute socket path.
        path: PathBuf,
    },
    /// Namespaced local endpoint, implemented as a Windows named pipe.
    Namespaced {
        /// Validated platform-local name without a transport prefix.
        name: String,
    },
}

/// One authenticated request sent to the embedded bridge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireRequest {
    /// Must equal [`PROTOCOL_VERSION`].
    pub protocol_version: u16,
    /// Caller-selected nonzero correlation identifier.
    pub request_id: u64,
    /// Endpoint bearer secret.
    pub token: String,
    /// Requested operation.
    pub operation: Operation,
}

/// Operation accepted by the embedded bridge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    /// Check liveness and identity.
    Ping,
    /// Return the latest semantic tree.
    GetTree,
    /// Dispatch semantic input on the UI thread.
    Input {
        /// Semantic input command.
        command: InputCommand,
    },
    /// Dispatch one synthetic platform event through GPUI's native input pipeline.
    NativeInput {
        /// Native platform event command.
        command: NativeInputCommand,
    },
    /// Invoke one exact, generation-checked semantic node handler.
    Invoke {
        /// Stable target node identifier.
        node_id: String,
        /// Tree generation used to select the node.
        expected_generation: u64,
        /// Semantic action to invoke.
        action: SemanticAction,
    },
    /// Wait without polling until a newer semantic tree is published.
    WaitForTree {
        /// Return immediately when the current generation is newer than this value.
        after_generation: u64,
        /// Maximum wait duration from one through 30,000 milliseconds.
        timeout_ms: u64,
    },
    /// Wait without polling until a newer frame finishes root paint.
    WaitForFrame {
        /// Return immediately when the completed-frame token is newer than this value.
        after_frame_count: u64,
        /// Maximum wait duration from one through 30,000 milliseconds.
        timeout_ms: u64,
    },
    /// Request a new GPUI frame and return the last completed-frame token observed before the
    /// refresh was scheduled.
    Refresh,
    /// Capture a screenshot.
    Screenshot {
        /// Screenshot target.
        target: ScreenshotTarget,
    },
    /// Replace the current highlight set.
    SetHighlights {
        /// Bounded highlight list.
        highlights: Vec<Highlight>,
    },
    /// Clear visual highlights.
    ClearHighlights,
    /// Return current frame timing statistics.
    GetFrameStats,
    /// Return the current window-relative logical pointer position from GPUI's input pipeline.
    GetPointerLocation,
    /// Return retained application logs.
    GetLogs {
        /// Maximum entries to return.
        limit: u16,
        /// Optional minimum severity.
        min_level: Option<String>,
    },
    /// Clear retained application logs.
    ClearLogs,
    /// Return the active in-memory pure-HTML document.
    GetLiveDocument,
    /// Compile and preview a complete in-memory document bundle.
    PreviewLiveDocument {
        /// Active revision the caller based its edit on.
        expected_revision: u64,
        /// Complete candidate bundle; partial updates are intentionally unsupported.
        source: LiveDocumentSource,
    },
    /// List application-owned MCP context resources.
    ListContextResources,
    /// Read one application-owned MCP context resource by exact URI.
    ReadContextResource {
        /// Exact URI returned by [`Operation::ListContextResources`].
        uri: String,
    },
    /// List structured commands owned by the application.
    ListApplicationCommands,
    /// Execute one exact application command on the GPUI thread.
    ExecuteApplicationCommand {
        /// Name returned by [`Operation::ListApplicationCommands`].
        name: String,
        /// Command arguments validated by the application against its advertised schema.
        arguments: serde_json::Value,
    },
}

/// Successful result from an embedded bridge operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum BridgeResult {
    /// Liveness information.
    Pong {
        /// Application identifier.
        app_id: String,
        /// Owning process identifier.
        pid: u32,
        /// Protocol version.
        protocol_version: u16,
    },
    /// Latest semantic tree.
    Tree(UiTree),
    /// Operation completed without a richer result.
    Ack,
    /// Result returned by an exact semantic handler.
    Action(ActionOutcome),
    /// Captured PNG.
    Screenshot(Screenshot),
    /// Frame timing statistics.
    FrameStats(FrameStats),
    /// Current window-relative logical pointer position.
    PointerLocation(Point),
    /// Retained log entries.
    Logs(Vec<LogEntry>),
    /// Active or newly previewed live document.
    LiveDocument(LiveDocument),
    /// Result of an attempted atomic live-document preview.
    LiveDocumentPreview(LiveDocumentPreview),
    /// Discoverable application-owned MCP context resource metadata.
    ContextResources(Vec<ContextResourceDescriptor>),
    /// Current contents of one application-owned MCP context resource.
    ContextResource(ContextResource),
    /// Discoverable application command metadata.
    ApplicationCommands(Vec<ApplicationCommandDescriptor>),
    /// Structured application command outcome.
    ApplicationCommand(ApplicationCommandResult),
}

/// Stable error code safe to expose to MCP clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Authentication failed.
    Unauthorized,
    /// Protocol versions differ.
    ProtocolMismatch,
    /// Request data is malformed or outside a bound.
    InvalidRequest,
    /// A requested semantic node was not present.
    NotFound,
    /// The target semantic tree changed after the caller selected it.
    StaleGeneration,
    /// The live document changed after the caller selected its base revision.
    StaleRevision,
    /// The application rejected an otherwise valid semantic action.
    Rejected,
    /// The operation is not available in this build or environment.
    Unsupported,
    /// The bridge is at its configured capacity.
    Busy,
    /// The UI thread did not complete before its deadline.
    Timeout,
    /// The application-side operation failed.
    Internal,
}

/// Sanitized bridge error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BridgeError {
    /// Stable machine-readable code.
    pub code: ErrorCode,
    /// Human-readable, non-sensitive explanation.
    pub message: String,
}

impl BridgeError {
    /// Construct a new sanitized error.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Correlated response from the embedded bridge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireResponse {
    /// Protocol version used by the bridge.
    pub protocol_version: u16,
    /// Request correlation identifier.
    pub request_id: u64,
    /// Successful result, mutually exclusive with `error`.
    pub result: Option<BridgeResult>,
    /// Sanitized error, mutually exclusive with `result`.
    pub error: Option<BridgeError>,
}

impl WireResponse {
    /// Construct a successful response.
    #[must_use]
    pub fn success(request_id: u64, result: BridgeResult) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: Some(result),
            error: None,
        }
    }

    /// Construct an error response.
    #[must_use]
    pub fn failure(request_id: u64, error: BridgeError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: None,
            error: Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Point, Rect, WireResponse};

    #[test]
    fn rectangle_validation_rejects_non_finite_and_negative_values() {
        assert!(
            Rect {
                x: 0.0,
                y: 1.0,
                width: 2.0,
                height: 3.0,
            }
            .is_valid()
        );
        assert!(
            !Rect {
                x: f32::NAN,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            }
            .is_valid()
        );
        assert!(
            !Rect {
                x: 0.0,
                y: 0.0,
                width: -1.0,
                height: 1.0,
            }
            .is_valid()
        );
    }

    #[test]
    fn response_has_exactly_one_outcome() {
        let response = WireResponse::failure(
            42,
            super::BridgeError::new(super::ErrorCode::NotFound, "missing"),
        );
        assert_eq!(response.request_id, 42);
        assert!(response.result.is_none());
        assert!(response.error.is_some());
    }

    #[test]
    fn point_validation_rejects_infinity() {
        assert!(Point { x: 1.0, y: 2.0 }.is_valid());
        assert!(
            !Point {
                x: f32::INFINITY,
                y: 2.0,
            }
            .is_valid()
        );
    }
}
