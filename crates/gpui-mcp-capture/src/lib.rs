//! Native window capture for an exact process-and-title pair.

use std::io::Cursor;
use std::time::Duration;
#[cfg(target_os = "windows")]
use std::time::Instant;

use base64::Engine as _;
use gpui_mcp_protocol::{Rect, Screenshot, ScreenshotTarget};
use image::{DynamicImage, ImageFormat, RgbaImage};

const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_ENCODED_BYTES: usize = 16 * 1024 * 1024;
#[cfg(any(target_os = "windows", test))]
const COMPOSITOR_POLL_INTERVAL: Duration = Duration::from_millis(16);
const MIN_STABILITY_DEADLINE: Duration = Duration::from_millis(32);
const MAX_STABILITY_DEADLINE: Duration = Duration::from_secs(2);
#[cfg(any(target_os = "windows", test))]
const MIN_FRESHNESS_SAMPLES: u8 = 3;

/// Bounded Windows Graphics Capture freshness policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureOptions {
    compositor_stability_deadline: Duration,
}

impl CaptureOptions {
    /// Configure the maximum time Windows capture spends obtaining fresh compositor samples.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureFailure::InvalidStabilityDeadline`] unless `deadline` is
    /// between 32 milliseconds and 2 seconds, inclusive.
    pub fn new(compositor_stability_deadline: Duration) -> Result<Self, CaptureFailure> {
        if !(MIN_STABILITY_DEADLINE..=MAX_STABILITY_DEADLINE)
            .contains(&compositor_stability_deadline)
        {
            return Err(CaptureFailure::InvalidStabilityDeadline);
        }
        Ok(Self {
            compositor_stability_deadline,
        })
    }

    /// Return the configured compositor freshness deadline.
    #[must_use]
    pub const fn compositor_stability_deadline(self) -> Duration {
        self.compositor_stability_deadline
    }
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            compositor_stability_deadline: Duration::from_secs(1),
        }
    }
}

/// Native window pixels plus display metadata reported by the operating system.
///
/// The image is bounded by this crate's safety limits and, on Windows, has
/// passed the same compositor-freshness policy used by MCP screenshots.
#[derive(Clone, Debug)]
pub struct NativeWindowCapture {
    /// Captured native RGBA pixels, including any frame retained by the OS API.
    pub image: RgbaImage,
    /// Native global window origin when the platform reports one.
    pub origin: Option<(i32, i32)>,
    /// Physical pixels per logical display pixel from the native monitor API.
    pub scale_factor: f32,
    /// Window dimensions reported by the native window API, when available.
    pub reported_size: Option<(u32, u32)>,
}

/// GPUI client-area geometry used to map logical element coordinates into a decorated native
/// window capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptureGeometry {
    /// Drawable client-area size paired with GPUI's native-window global origin. Some platforms
    /// report the outer-window origin for a decorated window; the mapper detects that case from
    /// the captured image and derives the decoration insets without platform constants.
    pub content_bounds: Rect,
    /// Physical pixels per GPUI logical pixel for the target window.
    pub scale_factor: f32,
}

/// A sanitized capture failure safe to return across the MCP boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureFailure {
    /// The requested logical region was invalid.
    InvalidRegion,
    /// The compositor stability deadline was outside the supported range.
    InvalidStabilityDeadline,
    /// Native windows could not be enumerated.
    WindowEnumeration,
    /// No native window matched both the exact process ID and title.
    TargetNotFound {
        /// Number of enumerated candidate windows.
        window_count: usize,
        /// Number of candidates with the requested process ID.
        pid_matches: usize,
        /// Number of candidates with the requested title.
        title_matches: usize,
    },
    /// The operating system could not capture the matched window.
    CaptureUnavailable,
    /// The compositor did not produce enough fresh samples before the configured deadline.
    UnstableFrame,
    /// Captured dimensions exceeded the safety bound.
    DimensionsOutOfBounds,
    /// The requested region did not intersect the captured window.
    RegionOutsideWindow,
    /// PNG encoding failed.
    Encoding,
    /// The encoded PNG exceeded the safety bound.
    EncodedImageTooLarge,
}

impl std::fmt::Display for CaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRegion => "screenshot region must be finite and non-empty",
            Self::InvalidStabilityDeadline => {
                "capture stability deadline must be between 32 milliseconds and 2 seconds"
            }
            Self::WindowEnumeration | Self::CaptureUnavailable => {
                "native screenshot capture is unavailable; verify desktop-session support and OS permission"
            }
            Self::TargetNotFound { .. } => "application window was not available for capture",
            Self::UnstableFrame => {
                "native screenshot capture did not produce a fresh frame before the deadline"
            }
            Self::DimensionsOutOfBounds => {
                "captured image dimensions are outside the safety bound"
            }
            Self::RegionOutsideWindow => "screenshot region is outside the application window",
            Self::Encoding => "screenshot encoding failed",
            Self::EncodedImageTooLarge => "encoded screenshot exceeds the 16 MiB safety bound",
        })
    }
}

impl std::error::Error for CaptureFailure {}

/// Captures the native window that matches both `pid` and `title` exactly.
///
/// `logical_size` is the semantic root size in logical pixels and is used to
/// scale a logical region into the captured image's physical pixels.
///
/// # Errors
///
/// Returns a sanitized [`CaptureFailure`] when validation, native capture, or
/// bounded PNG encoding fails.
// GPUI regions are floating-point logical pixels while image crops use u32
// physical pixels. Values are validated, clamped, and images are capped before
// these intentional conversions.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn capture_window(
    pid: u32,
    title: &str,
    target: ScreenshotTarget,
    logical_size: Option<(f32, f32)>,
    content_geometry: Option<CaptureGeometry>,
) -> Result<Screenshot, CaptureFailure> {
    capture_window_with_options(
        pid,
        title,
        target,
        logical_size,
        content_geometry,
        CaptureOptions::default(),
    )
}

/// Captures an exact native window with a caller-selected bounded freshness policy.
///
/// # Errors
///
/// Returns a sanitized [`CaptureFailure`] when validation, native capture,
/// compositor freshness sampling, cropping, or bounded PNG encoding fails.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub fn capture_window_with_options(
    pid: u32,
    title: &str,
    target: ScreenshotTarget,
    logical_size: Option<(f32, f32)>,
    content_geometry: Option<CaptureGeometry>,
    options: CaptureOptions,
) -> Result<Screenshot, CaptureFailure> {
    validate_target(target)?;
    let native = capture_native_window_with_options(pid, title, options)?;
    let native_origin = native.origin;
    let mut image = native.image;

    if let ScreenshotTarget::Region { rect } = target {
        let mapping = region_mapping(
            image.width(),
            image.height(),
            logical_size,
            native_origin,
            content_geometry,
        );
        let left = (mapping.offset_x + rect.x * mapping.scale_x)
            .floor()
            .max(0.0) as u32;
        let top = (mapping.offset_y + rect.y * mapping.scale_y)
            .floor()
            .max(0.0) as u32;
        let right = (mapping.offset_x + (rect.x + rect.width) * mapping.scale_x)
            .ceil()
            .clamp(0.0, image.width() as f32) as u32;
        let bottom = (mapping.offset_y + (rect.y + rect.height) * mapping.scale_y)
            .ceil()
            .clamp(0.0, image.height() as f32) as u32;
        if right <= left || bottom <= top || left >= image.width() || top >= image.height() {
            return Err(CaptureFailure::RegionOutsideWindow);
        }
        image = image::imageops::crop_imm(&image, left, top, right - left, bottom - top).to_image();
    }

    let width = image.width();
    let height = image.height();
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(|_| CaptureFailure::Encoding)?;
    let bytes = bytes.into_inner();
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(CaptureFailure::EncodedImageTooLarge);
    }
    Ok(Screenshot {
        mime_type: "image/png".to_owned(),
        base64_data: base64::engine::general_purpose::STANDARD.encode(bytes),
        width,
        height,
    })
}

/// Capture bounded native RGBA pixels for an exact process-and-title pair.
///
/// This is the common platform abstraction used by MCP screenshots and visual
/// parity tests. It uses the platform backend selected by `xcap`: Windows
/// Graphics Capture on Windows, CoreGraphics on macOS, and X11 window capture
/// on Linux. Native Wayland compositors intentionally do not expose unattended
/// cross-process window capture by process ID and title.
///
/// # Errors
///
/// Returns a sanitized [`CaptureFailure`] when the window cannot be found,
/// native display metadata is unavailable, or capture does not settle.
pub fn capture_native_window(pid: u32, title: &str) -> Result<NativeWindowCapture, CaptureFailure> {
    capture_native_window_with_options(pid, title, CaptureOptions::default())
}

/// Capture native RGBA pixels with a caller-selected bounded freshness policy.
///
/// # Errors
///
/// Returns a sanitized [`CaptureFailure`] when window enumeration, display
/// metadata, native capture, or compositor settlement fails.
pub fn capture_native_window_with_options(
    pid: u32,
    title: &str,
    options: CaptureOptions,
) -> Result<NativeWindowCapture, CaptureFailure> {
    let windows = xcap::Window::all().map_err(|_| CaptureFailure::WindowEnumeration)?;
    let native_window = find_native_window(windows, pid, title)?;
    let origin = native_window.x().ok().zip(native_window.y().ok());
    let scale_factor = native_window
        .current_monitor()
        .and_then(|monitor| monitor.scale_factor())
        .map_err(|_| CaptureFailure::CaptureUnavailable)?;
    if !scale_factor.is_finite() || !(0.25..=8.0).contains(&scale_factor) {
        return Err(CaptureFailure::CaptureUnavailable);
    }
    let reported_size = native_window.width().ok().zip(native_window.height().ok());
    let image = capture_after_compositor_settle(
        || {
            native_window
                .capture_image()
                .map_err(|_| CaptureFailure::CaptureUnavailable)
        },
        options,
    )?;
    if image.width() == 0
        || image.height() == 0
        || image.width() > MAX_IMAGE_DIMENSION
        || image.height() > MAX_IMAGE_DIMENSION
    {
        return Err(CaptureFailure::DimensionsOutOfBounds);
    }
    Ok(NativeWindowCapture {
        image,
        origin,
        scale_factor,
        reported_size,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RegionMapping {
    offset_x: f32,
    offset_y: f32,
    scale_x: f32,
    scale_y: f32,
}

#[allow(clippy::cast_precision_loss)]
fn region_mapping(
    image_width: u32,
    image_height: u32,
    logical_size: Option<(f32, f32)>,
    native_origin: Option<(i32, i32)>,
    content_geometry: Option<CaptureGeometry>,
) -> RegionMapping {
    if let Some(geometry) = content_geometry {
        let bounds = geometry.content_bounds;
        let scale = geometry.scale_factor;
        let global_offsets = native_origin.map(|(native_x, native_y)| {
            (
                bounds.x.mul_add(scale, -(native_x as f32)),
                bounds.y.mul_add(scale, -(native_y as f32)),
            )
        });
        // The published semantic root is the authoritative drawable area. On Windows, GPUI's
        // platform `viewport_size` can temporarily include non-client chrome while the semantic
        // root remains correctly client-relative. Prefer the root dimensions when available and
        // retain the geometry dimensions as a fallback for native-only callers.
        let (content_logical_width, content_logical_height) = logical_size
            .filter(|(width, height)| *width > 0.0 && *height > 0.0)
            .unwrap_or((bounds.width, bounds.height));
        let tolerance = 2.0;
        // WGC can exclude a few resize-border pixels horizontally even though GPUI reports the
        // requested logical window width. Preserve the platform scale when the content fits, and
        // only compress an axis when its predicted client area is larger than the capture.
        let scale_x = if content_logical_width * scale > image_width as f32 + tolerance {
            image_width as f32 / content_logical_width
        } else {
            scale
        };
        let scale_y = if content_logical_height * scale > image_height as f32 + tolerance {
            image_height as f32 / content_logical_height
        } else {
            scale
        };
        let content_width = content_logical_width * scale_x;
        let content_height = content_logical_height * scale_y;
        let horizontal_inset = ((image_width as f32 - content_width) / 2.0).max(0.0);
        let top_inset = (image_height as f32 - content_height).max(0.0);
        let offset_x = global_offsets
            .filter(|(global_offset_x, _)| {
                (*global_offset_x + content_width - image_width as f32).abs() <= tolerance
            })
            .map_or(horizontal_inset, |(global_offset_x, _)| global_offset_x);
        let offset_y = global_offsets
            .filter(|(_, global_offset_y)| {
                (*global_offset_y + content_height - image_height as f32).abs() <= tolerance
            })
            .map_or(top_inset, |(_, global_offset_y)| global_offset_y);
        let content_right = content_width + offset_x;
        let content_bottom = content_height + offset_y;
        if bounds.is_valid()
            && scale.is_finite()
            && scale > 0.0
            && offset_x >= -tolerance
            && offset_y >= -tolerance
            && content_right <= image_width as f32 + tolerance
            && content_bottom <= image_height as f32 + tolerance
        {
            return RegionMapping {
                offset_x: offset_x.max(0.0),
                offset_y: offset_y.max(0.0),
                scale_x,
                scale_y,
            };
        }
    }

    if let Some((logical_width, logical_height)) =
        logical_size.filter(|(width, height)| *width > 0.0 && *height > 0.0)
    {
        // Portable decorated-window fallback. A semantic root is client-relative, while native
        // capture APIs commonly return the complete decorated frame. The horizontal ratio is the
        // best available physical scale because title bars add height but not material width;
        // any remaining vertical pixels are therefore the top decoration inset. If the captured
        // image is shorter instead, constrain the vertical scale and do not invent an inset.
        let scale_x = image_width as f32 / logical_width;
        let predicted_content_height = logical_height * scale_x;
        let (offset_y, scale_y) = if predicted_content_height <= image_height as f32 {
            (image_height as f32 - predicted_content_height, scale_x)
        } else {
            (0.0, image_height as f32 / logical_height)
        };
        return RegionMapping {
            offset_x: 0.0,
            offset_y,
            scale_x,
            scale_y,
        };
    }

    RegionMapping {
        offset_x: 0.0,
        offset_y: 0.0,
        scale_x: 1.0,
        scale_y: 1.0,
    }
}

fn capture_after_compositor_settle<T>(
    capture: impl FnMut() -> Result<T, CaptureFailure>,
    options: CaptureOptions,
) -> Result<T, CaptureFailure> {
    #[cfg(target_os = "windows")]
    {
        capture_fresh_frame(
            capture,
            options,
            Instant::now,
            std::thread::sleep,
            CaptureFailure::UnstableFrame,
        )
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = options;
        let mut capture = capture;
        capture()
    }
}

#[cfg(any(target_os = "windows", test))]
fn capture_fresh_frame<T, E: Clone>(
    mut capture: impl FnMut() -> Result<T, E>,
    options: CaptureOptions,
    mut now: impl FnMut() -> std::time::Instant,
    mut wait: impl FnMut(Duration),
    unstable_error: E,
) -> Result<T, E> {
    // Windows Graphics Capture can initially return an older composited frame. Take a bounded
    // sequence of ordered samples and return the newest one. Requiring byte equality is incorrect:
    // native capture borders and legitimate animation may change between every sample.
    let started = now();
    let Some(deadline) = started.checked_add(options.compositor_stability_deadline()) else {
        return Err(unstable_error.clone());
    };
    let mut latest = capture()?;
    for _ in 1..MIN_FRESHNESS_SAMPLES {
        let current_time = now();
        let Some(remaining) = deadline.checked_duration_since(current_time) else {
            return Err(unstable_error.clone());
        };
        if remaining.is_zero() {
            return Err(unstable_error.clone());
        }
        wait(COMPOSITOR_POLL_INTERVAL.min(remaining));
        if now() >= deadline {
            return Err(unstable_error.clone());
        }
        latest = capture()?;
    }
    Ok(latest)
}

fn validate_target(target: ScreenshotTarget) -> Result<(), CaptureFailure> {
    if let ScreenshotTarget::Region { rect } = target
        && (!rect.is_valid() || rect.width < 1.0 || rect.height < 1.0)
    {
        return Err(CaptureFailure::InvalidRegion);
    }
    Ok(())
}

fn find_native_window(
    windows: Vec<xcap::Window>,
    pid: u32,
    title: &str,
) -> Result<xcap::Window, CaptureFailure> {
    let window_count = windows.len();
    let mut pid_matches = 0_usize;
    let mut title_matches = 0_usize;
    for window in windows {
        let pid_matches_window = window.pid().ok() == Some(pid);
        let title_matches_window = window.title().ok().as_deref() == Some(title);
        pid_matches += usize::from(pid_matches_window);
        title_matches += usize::from(title_matches_window);
        if pid_matches_window && title_matches_window {
            return Ok(window);
        }
    }
    Err(CaptureFailure::TargetNotFound {
        window_count,
        pid_matches,
        title_matches,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use gpui_mcp_protocol::{Rect, ScreenshotTarget};

    use super::{
        CaptureFailure, CaptureGeometry, CaptureOptions, RegionMapping, capture_fresh_frame,
        region_mapping, validate_target,
    };

    #[test]
    fn rejects_empty_and_non_finite_regions_before_native_access() {
        for rect in [
            Rect {
                width: 0.0,
                height: 10.0,
                ..Rect::default()
            },
            Rect {
                width: f32::NAN,
                height: 10.0,
                ..Rect::default()
            },
        ] {
            assert_eq!(
                validate_target(ScreenshotTarget::Region { rect }),
                Err(CaptureFailure::InvalidRegion)
            );
        }
    }

    #[test]
    fn freshness_sampling_returns_the_third_ordered_frame() -> Result<(), CaptureFailure> {
        let calls = Cell::new(0_usize);
        let elapsed = Cell::new(Duration::ZERO);
        let started = Instant::now();
        let frames = [1_u8, 1, 2];
        let captured = capture_fresh_frame(
            || {
                let call = calls.get();
                calls.set(call.saturating_add(1));
                Ok::<_, CaptureFailure>(
                    frames
                        .get(call)
                        .copied()
                        .or_else(|| frames.last().copied())
                        .unwrap_or(2),
                )
            },
            CaptureOptions::new(Duration::from_millis(100))?,
            || started + elapsed.get(),
            |duration| elapsed.set(elapsed.get().saturating_add(duration)),
            CaptureFailure::UnstableFrame,
        )?;

        assert_eq!(captured, 2);
        assert_eq!(calls.get(), 3);
        Ok(())
    }

    #[test]
    fn freshness_sampling_honors_the_deadline() -> Result<(), CaptureFailure> {
        let calls = Cell::new(0_u8);
        let elapsed = Cell::new(Duration::ZERO);
        let started = Instant::now();
        let captured = capture_fresh_frame(
            || {
                let frame = calls.get();
                calls.set(frame.saturating_add(1));
                Ok::<_, CaptureFailure>(frame)
            },
            CaptureOptions::new(Duration::from_millis(32))?,
            || started + elapsed.get(),
            |duration| elapsed.set(elapsed.get().saturating_add(duration)),
            CaptureFailure::UnstableFrame,
        );

        assert_eq!(captured, Err(CaptureFailure::UnstableFrame));
        assert_eq!(calls.get(), 2);
        Ok(())
    }

    #[test]
    fn capture_stability_deadline_is_bounded() {
        assert!(CaptureOptions::new(Duration::from_millis(31)).is_err());
        assert!(CaptureOptions::new(Duration::from_millis(32)).is_ok());
        assert!(CaptureOptions::new(Duration::from_secs(2)).is_ok());
        assert!(CaptureOptions::new(Duration::from_millis(2_001)).is_err());
    }

    #[test]
    fn client_geometry_offsets_regions_below_native_title_chrome() {
        assert_eq!(
            region_mapping(
                1_435,
                932,
                Some((1_440.0, 900.0)),
                Some((100, 100)),
                Some(CaptureGeometry {
                    content_bounds: Rect {
                        x: 100.0,
                        y: 132.0,
                        width: 1_435.0,
                        height: 900.0,
                    },
                    scale_factor: 1.0,
                }),
            ),
            RegionMapping {
                offset_x: 0.0,
                offset_y: 32.0,
                scale_x: 1_435.0 / 1_440.0,
                scale_y: 1.0,
            }
        );
    }

    #[test]
    fn client_size_derives_title_chrome_when_global_origin_is_outer_origin() {
        assert_eq!(
            region_mapping(
                1_435,
                932,
                Some((1_440.0, 900.0)),
                Some((100, 100)),
                Some(CaptureGeometry {
                    content_bounds: Rect {
                        x: 100.0,
                        y: 100.0,
                        width: 1_435.0,
                        height: 900.0,
                    },
                    scale_factor: 1.0,
                }),
            ),
            RegionMapping {
                offset_x: 0.0,
                offset_y: 32.0,
                scale_x: 1_435.0 / 1_440.0,
                scale_y: 1.0,
            }
        );
    }

    #[test]
    fn client_size_derives_title_chrome_without_native_origin() {
        assert_eq!(
            region_mapping(
                1_435,
                932,
                Some((1_440.0, 900.0)),
                None,
                Some(CaptureGeometry {
                    content_bounds: Rect {
                        width: 1_440.0,
                        height: 932.0,
                        ..Rect::default()
                    },
                    scale_factor: 1.0,
                }),
            ),
            RegionMapping {
                offset_x: 0.0,
                offset_y: 32.0,
                scale_x: 1_435.0 / 1_440.0,
                scale_y: 1.0,
            }
        );
    }

    #[test]
    fn semantic_root_derives_decorated_capture_without_native_geometry() {
        assert_eq!(
            region_mapping(1_435, 932, Some((1_440.0, 900.0)), None, None),
            RegionMapping {
                offset_x: 0.0,
                offset_y: 932.0 - 900.0 * (1_435.0 / 1_440.0),
                scale_x: 1_435.0 / 1_440.0,
                scale_y: 1_435.0 / 1_440.0,
            }
        );
    }
}
