use std::time::Duration;

use tokio::time::{MissedTickBehavior, interval_at, timeout};

use crate::recording::{
    CaptureLease, CaptureOutcome, run_encoding_job, validate_artifact_name, validate_frame_delay,
};

use super::{
    CallToolResult, CaptureNamedArgs, CompareImagesArgs, ElementArgs, GpuiMcp, Highlight,
    HighlightArgs, Json, MAX_IMAGE_SNAPSHOTS, Operation, Parameters, Point, Rect, RegionArgs,
    Screenshot, ScreenshotTarget, StartVideoRecordingArgs, StopVideoRecordingArgs, ToolRouter,
    Value, VideoCaptureMode, ack_json, compare_images, decode_image, encode_image, get_node,
    image_result, object_output, require_bounds, tool, tool_router, validate_name,
};

const ENCODING_RESPONSE_DEADLINE: Duration = Duration::from_mins(2);

#[tool_router(router = visual_router)]
impl GpuiMcp {
    #[tool(description = "Capture the full GPUI application window as an in-memory PNG")]
    async fn screenshot(&self) -> Result<CallToolResult, String> {
        self.capture(ScreenshotTarget::Window)
            .await
            .map(image_result)
    }

    #[tool(description = "Capture a window-relative logical rectangle as an in-memory PNG")]
    async fn screenshot_region(
        &self,
        Parameters(args): Parameters<RegionArgs>,
    ) -> Result<CallToolResult, String> {
        self.capture(ScreenshotTarget::Region {
            rect: Rect {
                x: args.x,
                y: args.y,
                width: args.width,
                height: args.height,
            },
        })
        .await
        .map(image_result)
    }

    #[tool(description = "Capture one semantic element's current bounds as an in-memory PNG")]
    async fn screenshot_element(
        &self,
        Parameters(args): Parameters<ElementArgs>,
    ) -> Result<CallToolResult, String> {
        let tree = self.tree().await?;
        let rect = require_bounds(get_node(&tree, &args.id)?)?;
        self.capture(ScreenshotTarget::Region { rect })
            .await
            .map(image_result)
    }

    #[tool(description = "Outline one or more semantic elements inside the GPUI window")]
    async fn highlight_elements(
        &self,
        Parameters(args): Parameters<HighlightArgs>,
    ) -> Result<Json<Value>, String> {
        if args.ids.is_empty() || args.ids.len() > 64 {
            return Err("ids must contain between 1 and 64 elements".to_owned());
        }
        let tree = self.tree().await?;
        let highlights = args
            .ids
            .iter()
            .map(|id| {
                let node = get_node(&tree, id)?;
                Ok(Highlight {
                    rect: require_bounds(node)?,
                    color: args.color.clone(),
                    label: node.label.clone().or_else(|| Some(id.clone())),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.ack_after_frame(Operation::SetHighlights { highlights })
            .await?;
        Ok(ack_json("highlighted"))
    }

    #[tool(description = "Clear all MCP visual outlines from the GPUI window")]
    async fn clear_highlights(&self) -> Result<Json<Value>, String> {
        self.ack_after_frame(Operation::ClearHighlights).await?;
        Ok(ack_json("highlights_cleared"))
    }

    #[tool(description = "Capture and save a full-window PNG under a bounded in-memory name")]
    async fn capture_screenshot_snapshot(
        &self,
        Parameters(args): Parameters<CaptureNamedArgs>,
    ) -> Result<CallToolResult, String> {
        validate_name(&args.name)?;
        let screenshot = self.capture(ScreenshotTarget::Window).await?;
        let mut snapshots = self.snapshots.write().await;
        if !snapshots.images.contains_key(&args.name)
            && snapshots.images.len() >= MAX_IMAGE_SNAPSHOTS
        {
            return Err("image snapshot capacity (8) has been reached".to_owned());
        }
        snapshots.images.insert(args.name, screenshot.clone());
        Ok(image_result(screenshot))
    }

    #[tool(
        description = "Compare two in-memory PNG snapshots and return deterministic pixel metrics"
    )]
    async fn compare_screenshots(
        &self,
        Parameters(args): Parameters<CompareImagesArgs>,
    ) -> Result<Json<Value>, String> {
        let (left, right) = self.stored_images(&args.left, &args.right).await?;
        let metrics = compare_images(&left, &right, args.tolerance)?.0;
        Ok(object_output(metrics))
    }

    #[tool(description = "Render an in-memory PNG diff for two saved screenshot snapshots")]
    async fn diff_screenshots(
        &self,
        Parameters(args): Parameters<CompareImagesArgs>,
    ) -> Result<CallToolResult, String> {
        let (left, right) = self.stored_images(&args.left, &args.right).await?;
        let (metrics, diff) = compare_images(&left, &right, args.tolerance)?;
        let mut result = image_result(diff);
        result.structured_content = Some(metrics);
        Ok(result)
    }

    #[tool(
        description = "Begin an H.264/MP4 video recording session. Continuous mode is the default: it captures settled GPUI render frames at the requested bounded cadence, like a browser screencast. Checkpoint mode captures only when capture_video_frame is called. Keep this initialized MCP transport open until stop_video_recording; recording state is not shared by separate one-shot server processes."
    )]
    async fn start_video_recording(
        &self,
        Parameters(args): Parameters<StartVideoRecordingArgs>,
    ) -> Result<Json<Value>, String> {
        if !(1..=30).contains(&args.frames_per_second) {
            return Err("frames_per_second must be between 1 and 30".to_owned());
        }
        let _transition = self.target_transition.lock().await;
        let target = self.client().await?;
        let frame_duration_ms = 1_000 / u32::from(args.frames_per_second);
        let mut task = self
            .recording_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if task.is_some() {
            return Err("a continuous video recording task is still active".to_owned());
        }
        let session_id = self
            .recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .start_with_config(args.include_pointer, frame_duration_ms)?;
        if matches!(args.mode, VideoCaptureMode::Continuous) {
            let cancellation = super::CancellationToken::new();
            let worker = self.clone();
            let worker_cancellation = cancellation.clone();
            let frames_per_second = args.frames_per_second;
            *task = Some(super::RecordingTask {
                cancellation,
                join: tokio::spawn(async move {
                    continuous_capture(worker, worker_cancellation, session_id, frames_per_second)
                        .await;
                }),
            });
        }
        Ok(object_output(serde_json::json!({
            "ok": true,
            "action": "video_recording_started",
            "session_id": session_id,
            "target_id": target.target_id(),
            "format": "mp4",
            "codec": "h264",
            "pointer_overlay": args.include_pointer,
            "coordinate_space": "window_logical_pixels",
            "capture_mode": capture_mode_name(args.mode),
            "frames_per_second": args.frames_per_second,
            "frame_duration_ms": frame_duration_ms,
            "artifact_directory": self.artifacts.directory().display().to_string(),
        })))
    }

    #[tool(
        description = "Capture one complete frame for the active MP4 recording. When enabled at start, the current GPUI pointer position is drawn into the frame, so synthetic pointer_move/click/drag actions appear in the video consistently on Windows, Linux, and macOS."
    )]
    async fn capture_video_frame(&self) -> Result<Json<Value>, String> {
        let (outcome, pointer, pointer_drawn) = self.capture_video_checkpoint().await?;
        Ok(object_output(serde_json::json!({
            "session_id": outcome.session_id,
            "frames": outcome.frame_count,
            "width": outcome.width,
            "height": outcome.height,
            "stored_base64_bytes": outcome.base64_bytes,
            "decoded_pixels": outcome.decoded_pixels,
            "pointer": pointer.map(|point| serde_json::json!({
                "x": point.x,
                "y": point.y,
                "coordinate_space": "window_logical_pixels",
                "drawn": pointer_drawn,
            })),
        })))
    }

    async fn capture_video_checkpoint(
        &self,
    ) -> Result<(CaptureOutcome, Option<Point>, bool), String> {
        let capture = CaptureLease::begin(self.recording.clone())?;
        let pointer = if capture.includes_pointer_overlay() {
            Some(self.current_pointer_location().await?)
        } else {
            None
        };
        let screenshot = self.capture(ScreenshotTarget::Window).await?;
        let tree = self.tree().await?;
        let (screenshot, pointer_drawn) = match pointer {
            Some(pointer) => overlay_pointer(screenshot, pointer, &tree)?,
            None => (screenshot, false),
        };
        let outcome = capture.complete(screenshot)?;
        Ok((outcome, pointer, pointer_drawn))
    }

    #[tool(
        description = "Stop the active recording and atomically encode it as an H.264 MP4 inside the server-configured artifact directory. artifact_name is a single validated lowercase-.mp4 filename, never a path. Existing files are preserved unless overwrite=true; symlinks and non-regular targets are always rejected. Encoding uses a same-directory temporary file and restores the recording for retry on failure."
    )]
    async fn stop_video_recording(
        &self,
        Parameters(args): Parameters<StopVideoRecordingArgs>,
    ) -> Result<Json<Value>, String> {
        validate_artifact_name(&args.artifact_name)?;
        self.stop_continuous_capture().await?;
        let job = self
            .recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_encoding()?;
        let delay_ms = args
            .frame_duration_ms
            .unwrap_or_else(|| job.frame_duration_ms());
        validate_frame_delay(delay_ms)?;
        let session_id = job.session_id();
        let state = self.recording.clone();
        let artifacts = self.artifacts.clone();
        let artifact_name = args.artifact_name;
        let overwrite = args.overwrite;
        let mut worker = tokio::task::spawn_blocking(move || {
            run_encoding_job(state, &artifacts, job, &artifact_name, delay_ms, overwrite)
        });
        let artifact = match timeout(ENCODING_RESPONSE_DEADLINE, &mut worker).await {
            Ok(Ok(result)) => result?,
            Ok(Err(error)) => {
                tracing::warn!(%error, session_id, "recording encoder worker failed");
                return Err(
                    "recording encoder worker failed; captured frames were restored".to_owned(),
                );
            }
            Err(_) => {
                return Err(
                    "recording encoding exceeded the 120 second response deadline and is still finishing"
                        .to_owned(),
                );
            }
        };
        Ok(object_output(serde_json::json!({
            "path": artifact.path.display().to_string(),
            "artifact_name": artifact
                .path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            "session_id": session_id,
            "frames": artifact.frame_count,
            "frame_duration_ms": artifact.delay_ms,
            "format": "mp4",
            "mime_type": "video/mp4",
            "codec": "h264",
            "width": artifact.width,
            "height": artifact.height,
            "dropped_frames": artifact.dropped_frames,
            "bytes": artifact.bytes,
            "overwritten": overwrite,
        })))
    }

    async fn stop_continuous_capture(&self) -> Result<(), String> {
        let task = self
            .recording_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(mut task) = task else {
            return Ok(());
        };
        task.cancellation.cancel();
        match timeout(Duration::from_secs(5), &mut task.join).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("continuous video capture task failed: {error}")),
            Err(_) => {
                task.join.abort();
                Err("continuous video capture did not stop within five seconds".to_owned())
            }
        }
    }
}

async fn continuous_capture(
    mcp: GpuiMcp,
    cancellation: super::CancellationToken,
    session_id: u64,
    frames_per_second: u8,
) {
    let period = Duration::from_millis(1_000 / u64::from(frames_per_second));
    let mut ticker = interval_at(tokio::time::Instant::now(), period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = cancellation.cancelled() => return,
            _ = ticker.tick() => match mcp.capture_video_checkpoint().await {
                Ok(_) => {}
                Err(error) => {
                    let keep_running = mcp
                        .recording
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .note_continuous_capture_failure(session_id, error);
                    if !keep_running {
                        return;
                    }
                }
            },
        }
    }
}

const fn capture_mode_name(mode: VideoCaptureMode) -> &'static str {
    match mode {
        VideoCaptureMode::Checkpoint => "checkpoint",
        VideoCaptureMode::Continuous => "continuous",
    }
}

fn overlay_pointer(
    screenshot: Screenshot,
    pointer: Point,
    tree: &super::UiTree,
) -> Result<(Screenshot, bool), String> {
    let Some(root) = tree
        .roots
        .iter()
        .filter_map(|id| tree.nodes.get(id))
        .filter_map(|node| node.bounds)
        .max_by(|left, right| (left.width * left.height).total_cmp(&(right.width * right.height)))
    else {
        return Ok((screenshot, false));
    };
    if root.width <= 0.0 || root.height <= 0.0 {
        return Ok((screenshot, false));
    }
    let mut image = decode_image(&screenshot)?;
    let scale = f64::from(image.width()) / f64::from(root.width);
    let top_inset = (f64::from(image.height()) - f64::from(root.height) * scale).max(0.0);
    let x = rounded_i32(f64::from(pointer.x - root.x) * scale);
    let y = rounded_i32(top_inset + f64::from(pointer.y - root.y) * scale);
    let drawn = draw_pointer_marker(&mut image, x, y);
    Ok((encode_image(image)?, drawn))
}

fn draw_pointer_marker(image: &mut image::RgbaImage, x: i32, y: i32) -> bool {
    let Ok(width) = i32::try_from(image.width()) else {
        return false;
    };
    let Ok(height) = i32::try_from(image.height()) else {
        return false;
    };
    if !(0..width).contains(&x) || !(0..height).contains(&y) {
        return false;
    }
    for (offset_x, offset_y, color) in [
        (0, 0, image::Rgba([255, 255, 255, 255])),
        (-1, 0, image::Rgba([16, 18, 24, 255])),
        (1, 0, image::Rgba([16, 18, 24, 255])),
        (0, -1, image::Rgba([16, 18, 24, 255])),
        (0, 1, image::Rgba([16, 18, 24, 255])),
    ] {
        let pixel_x = x + offset_x;
        let pixel_y = y + offset_y;
        if (0..width).contains(&pixel_x) && (0..height).contains(&pixel_y) {
            let Ok(pixel_x) = u32::try_from(pixel_x) else {
                continue;
            };
            let Ok(pixel_y) = u32::try_from(pixel_y) else {
                continue;
            };
            image.put_pixel(pixel_x, pixel_y, color);
        }
    }
    true
}

#[allow(clippy::cast_possible_truncation)]
fn rounded_i32(value: f64) -> i32 {
    if value.is_nan() {
        return 0;
    }
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

pub(super) fn router() -> ToolRouter<GpuiMcp> {
    GpuiMcp::visual_router()
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::draw_pointer_marker;

    #[test]
    fn pointer_marker_is_visible_and_stays_inside_the_frame() {
        let mut image = RgbaImage::from_pixel(5, 5, Rgba([1, 2, 3, 255]));

        assert!(draw_pointer_marker(&mut image, 2, 2));
        assert_eq!(image.get_pixel(2, 2), &Rgba([255, 255, 255, 255]));
        assert_eq!(image.get_pixel(1, 2), &Rgba([16, 18, 24, 255]));
        assert!(!draw_pointer_marker(&mut image, -1, 2));
        assert_eq!(image.get_pixel(0, 0), &Rgba([1, 2, 3, 255]));
    }
}
