use std::sync::Arc;

use gpui_mcp_protocol::{BridgeError, ErrorCode, Screenshot, ScreenshotTarget};

use crate::registry::SharedState;

#[cfg(feature = "native-screenshot")]
pub(crate) fn capture(
    pid: u32,
    title: &str,
    target: ScreenshotTarget,
    state: &Arc<SharedState>,
) -> Result<Screenshot, BridgeError> {
    let content_geometry =
        state
            .window_geometry()
            .map(|geometry| gpui_mcp_capture::CaptureGeometry {
                content_bounds: geometry.content_bounds,
                scale_factor: geometry.scale_factor,
            });
    let result = gpui_mcp_capture::capture_window(
        pid,
        title,
        target,
        logical_window_size(state),
        content_geometry,
    );
    result.map_err(|error| {
        let code = match error {
            gpui_mcp_capture::CaptureFailure::InvalidRegion
            | gpui_mcp_capture::CaptureFailure::InvalidStabilityDeadline
            | gpui_mcp_capture::CaptureFailure::RegionOutsideWindow => ErrorCode::InvalidRequest,
            gpui_mcp_capture::CaptureFailure::TargetNotFound {
                window_count,
                pid_matches,
                title_matches,
            } => {
                state.add_log(
                    "warn",
                    &format!(
                        "screenshot target match failed: {window_count} windows, {pid_matches} PID matches, {title_matches} title matches"
                    ),
                );
                ErrorCode::NotFound
            }
            gpui_mcp_capture::CaptureFailure::Encoding => ErrorCode::Internal,
            gpui_mcp_capture::CaptureFailure::UnstableFrame => ErrorCode::Timeout,
            gpui_mcp_capture::CaptureFailure::WindowEnumeration
            | gpui_mcp_capture::CaptureFailure::CaptureUnavailable
            | gpui_mcp_capture::CaptureFailure::DimensionsOutOfBounds
            | gpui_mcp_capture::CaptureFailure::EncodedImageTooLarge => ErrorCode::Unsupported,
        };
        tracing::warn!(%error, "native screenshot capture failed");
        BridgeError::new(code, error.to_string())
    })
}

#[cfg(not(feature = "native-screenshot"))]
pub(crate) fn capture(
    _pid: u32,
    _title: &str,
    _target: ScreenshotTarget,
    _state: &Arc<SharedState>,
) -> Result<Screenshot, BridgeError> {
    Err(BridgeError::new(
        ErrorCode::Unsupported,
        "native screenshot support was disabled at build time",
    ))
}

#[cfg(feature = "native-screenshot")]
fn logical_window_size(state: &SharedState) -> Option<(f32, f32)> {
    let tree = state.tree();
    tree.roots
        .iter()
        .filter_map(|id| tree.nodes.get(id))
        .filter_map(|node| node.bounds)
        .max_by(|left, right| (left.width * left.height).total_cmp(&(right.width * right.height)))
        .map(|rect| (rect.width, rect.height))
}
