use std::time::Duration;

use gpui_mcp_capture::{CaptureOptions, capture_window_with_options};
use gpui_mcp_protocol::{Capability, Screenshot, ScreenshotTarget, UiTree};
use tokio::time::timeout;

const CAPTURE_WORKER_DEADLINE: Duration = Duration::from_secs(15);

use crate::client::BridgeClient;

pub(crate) async fn capture(
    client: &BridgeClient,
    tree: &UiTree,
    target: ScreenshotTarget,
    options: CaptureOptions,
) -> Result<Screenshot, String> {
    if !client
        .descriptor()
        .capabilities
        .supports(Capability::Screenshot)
    {
        return Err("native screenshot support was disabled by the application".to_owned());
    }
    let pid = client.descriptor().pid;
    let title = client.descriptor().window_title.clone();
    let worker_title = title.clone();
    let logical_size = logical_window_size(tree);
    timeout(
        CAPTURE_WORKER_DEADLINE,
        tokio::task::spawn_blocking(move || {
            capture_after_worker_reopen(|| {
                capture_window_with_options(pid, &worker_title, target, logical_size, None, options)
            })
        }),
    )
    .await
    .map_err(|_| "screenshot capture exceeded the 15 second deadline".to_owned())?
    .map_err(|error| {
        tracing::warn!(%error, "screenshot worker failed");
        "screenshot worker failed".to_owned()
    })?
    .map_err(|error| {
        if let gpui_mcp_capture::CaptureFailure::TargetNotFound {
            window_count,
            pid_matches,
            title_matches,
        } = error
        {
            tracing::warn!(
                pid,
                title,
                window_count,
                pid_matches,
                title_matches,
                "could not match native screenshot window"
            );
        }
        error.to_string()
    })
}

fn capture_after_worker_reopen<T, E>(mut capture: impl FnMut() -> Result<T, E>) -> Result<T, E> {
    #[cfg(target_os = "windows")]
    {
        // The first complete external WGC capture prompts DWM to republish all GPUI surfaces.
        // Release that worker session in full before opening the capture returned to the client.
        drop(capture()?);
        std::thread::sleep(Duration::from_millis(16));
    }

    capture()
}

fn logical_window_size(tree: &UiTree) -> Option<(f32, f32)> {
    tree.roots
        .iter()
        .filter_map(|id| tree.nodes.get(id))
        .filter_map(|node| node.bounds)
        .max_by(|left, right| (left.width * left.height).total_cmp(&(right.width * right.height)))
        .map(|rect| (rect.width, rect.height))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use gpui_mcp_protocol::{NodeState, Rect, Role, UiNode, UiTree};

    use super::{capture_after_worker_reopen, logical_window_size};

    #[test]
    fn uses_largest_semantic_root_for_capture_scaling() {
        let mut tree = UiTree::default();
        for (id, width, height) in [("small", 200.0, 100.0), ("large", 900.0, 700.0)] {
            tree.roots.push(id.to_owned());
            tree.nodes.insert(
                id.to_owned(),
                UiNode {
                    id: id.to_owned(),
                    parent: None,
                    children: Vec::new(),
                    role: Role::Window,
                    label: None,
                    description: None,
                    bounds: Some(Rect {
                        width,
                        height,
                        ..Rect::default()
                    }),
                    state: NodeState::default(),
                    actions: Vec::new(),
                    text: None,
                    value: None,
                    metadata: BTreeMap::new(),
                },
            );
        }
        assert_eq!(logical_window_size(&tree), Some((900.0, 700.0)));
    }

    #[test]
    fn windows_reopens_the_complete_external_capture_worker() {
        let calls = Cell::new(0_u8);
        let captured = capture_after_worker_reopen(|| {
            calls.set(calls.get() + 1);
            Ok::<_, ()>(calls.get())
        });

        assert_eq!(
            captured,
            Ok(if cfg!(target_os = "windows") { 2 } else { 1 })
        );
        assert_eq!(calls.get(), if cfg!(target_os = "windows") { 2 } else { 1 });
    }
}
