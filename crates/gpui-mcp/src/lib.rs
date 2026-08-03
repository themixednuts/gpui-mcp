//! Hardened, cross-platform MCP automation for GPUI applications.
//!
//! Install [`BridgeHandle`] for a window, retain the handle for that window's
//! lifetime, and annotate the rendered element tree with [`McpElementExt`]. All
//! control stays on owner-restricted native local IPC and authenticated requests are dispatched through
//! GPUI's foreground executor.

mod capture;
mod element;
mod input;
mod registry;
mod service;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub use element::{McpElementExt, NodeEvent, NodeSpec, SemanticElement};
pub use gpui_mcp_protocol::{
    ActionOutcome, ApplicationCommandDescriptor, ApplicationCommandResult, BridgeError,
    ContextResource, ContextResourceDescriptor, ErrorCode, LiveDocument, LiveDocumentDiagnostic,
    LiveDocumentPreview, LiveDocumentSource, LogEntry, MAX_LABEL_BYTES,
    MAX_LIVE_DOCUMENT_DIAGNOSTICS, MAX_LIVE_DOCUMENT_SOURCE_BYTES, MAX_TEXT_BYTES, NodeAction,
    NodeState, Point, Rect, Role, TextInfo, TextRange, UiNode, UiTree, ValueInfo,
};
// `MouseButton` is a field of the public `NodeEvent::Click`, so applications
// matching on click buttons need it unconditionally.
pub use gpui_mcp_protocol::MouseButton;
#[cfg(feature = "test-support")]
pub use gpui_mcp_protocol::SemanticAction;
pub use service::{
    ApplicationCommandHostError, ApplicationCommandRequest, ApplicationCommandResponse,
    BridgeConfig, BridgeHandle, ContextResourceHostError, ContextResourceRequest,
    ContextResourceResponse, LiveDocumentHostError, LiveDocumentRequest, LiveDocumentResponse,
    StartError,
};

use registry::SharedState;

static NEXT_ISOLATED_INSTANCE_ID: AtomicU64 = AtomicU64::new(1 << 63);

/// Cloneable application-side handle used when annotating elements and publishing logs.
#[derive(Clone)]
pub struct Automation {
    pub(crate) state: Arc<SharedState>,
}

impl Automation {
    /// Create isolated in-process automation without starting an MCP bridge.
    ///
    /// This is intended for offline application modes and embedded previews that
    /// still want one local semantic tree. No endpoint, listener, descriptor, or
    /// background thread is created.
    #[must_use]
    pub fn isolated() -> Self {
        Self {
            state: SharedState::new(NEXT_ISOLATED_INSTANCE_ID.fetch_add(1, Ordering::Relaxed)),
        }
    }

    /// Create isolated in-process automation without IPC for GPUI runtime tests.
    ///
    /// This constructor is available only with the `test-support` feature and
    /// must not be used as a substitute for [`BridgeHandle`] in an application.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn for_test() -> Self {
        Self::isolated()
    }

    /// Return the most recently completed semantic frame.
    #[must_use]
    pub fn snapshot(&self) -> UiTree {
        self.state.tree()
    }

    /// Return the generation of the most recently completed semantic frame without cloning it.
    ///
    /// Consumers that maintain a small derived view of the semantic tree can use this as a cheap
    /// invalidation guard and call [`Self::snapshot`] only after the generation changes.
    #[must_use]
    pub fn semantic_generation(&self) -> u64 {
        self.state.tree_generation()
    }

    /// Return the count of the most recently completed root-paint frame.
    ///
    /// This is available only to deterministic runtime tests. A frame is not
    /// counted until the annotated root has finished painting.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn completed_test_frame_count(&self) -> u64 {
        self.state.frame_stats().frame_count
    }

    /// Retain a bounded, sanitized diagnostic log entry for MCP inspection.
    ///
    /// Do not pass secrets. Newlines are replaced and messages are capped at 4 KiB.
    pub fn log(&self, level: &str, message: &str) {
        self.state.add_log(level, message);
    }

    /// Most recent log entries in chronological order, filtered to `min_level`
    /// ("debug" < "info" < "warn" < "error") when given, capped at `limit`.
    #[must_use]
    pub fn logs(&self, limit: u16, min_level: Option<&str>) -> Vec<LogEntry> {
        self.state.logs(limit, min_level)
    }

    /// Dispatch one semantic action directly through the GPUI foreground test context.
    ///
    /// # Errors
    ///
    /// Returns the same stale-generation, missing-node, unsupported-action, or
    /// handler failure exposed by the production bridge.
    #[cfg(feature = "test-support")]
    pub fn dispatch_test_action(
        &self,
        node_id: &str,
        expected_generation: u64,
        action: &SemanticAction,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Result<ActionOutcome, BridgeError> {
        self.state
            .dispatch_action(node_id, expected_generation, action, window, cx)
    }
}
