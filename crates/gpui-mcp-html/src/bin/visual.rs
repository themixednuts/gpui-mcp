//! Native GPUI fixture and exact-window capture helper for visual parity tests.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use gpui::{
    App, AppContext as _, Application, Bounds, Context, IntoElement, Render, Timer, Window,
    WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowOptions, px, size,
};
use gpui_mcp::{Automation, MouseButton, SemanticAction};
use gpui_mcp_html::{BindingDocument, HookRegistry, HtmlUi, LiveHtml};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

const PARITY_HTML: &str = include_str!("../../../../visual-tests/fixtures/parity.html");
const COMPLEX_LAYOUT_HTML: &str =
    include_str!("../../../../visual-tests/fixtures/complex-layout.html");
const BEHAVIORS_HTML: &str = include_str!("../../../../visual-tests/fixtures/behaviors.html");
const WINDOW_TITLE: &str = "gpui-mcp visual parity fixture";
const VIEWPORT_WIDTH: u32 = 640;
const VIEWPORT_HEIGHT: u32 = 480;
const FIXTURE_BACKGROUND: Rgba<u8> = Rgba([16, 20, 28, 255]);
const FIXTURE_READY_TIMEOUT: Duration = Duration::from_secs(5);
const FRAME_POLL_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Debug, Parser)]
#[command(about = "Run and capture the gpui-mcp HTML visual parity fixture")]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the deterministic frameless GPUI fixture window.
    Fixture {
        #[arg(long, value_enum, default_value_t = FixtureName::Parity)]
        fixture: FixtureName,
        #[arg(long, value_enum, default_value_t = FixtureState::Baseline)]
        state: FixtureState,
    },
    /// Capture an exact PID/title window and normalize it to CSS pixels.
    Capture {
        #[arg(long)]
        pid: u32,
        #[arg(long, default_value = WINDOW_TITLE)]
        title: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        raw_output: Option<PathBuf>,
        #[arg(long, default_value_t = VIEWPORT_WIDTH)]
        width: u32,
        #[arg(long, default_value_t = VIEWPORT_HEIGHT)]
        height: u32,
        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = 300)]
        settle_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum FixtureName {
    Parity,
    ComplexLayout,
    Behaviors,
}

impl FixtureName {
    fn html(self) -> &'static str {
        match self {
            Self::Parity => PARITY_HTML,
            Self::ComplexLayout => COMPLEX_LAYOUT_HTML,
            Self::Behaviors => BEHAVIORS_HTML,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum FixtureState {
    #[default]
    Baseline,
    Hover,
    Focus,
    DropdownOpen,
}

impl FixtureState {
    fn action(self) -> Option<(&'static str, SemanticAction)> {
        match self {
            Self::Baseline => None,
            Self::Hover => Some(("hover-card", SemanticAction::Hover)),
            Self::Focus => Some(("focus-card", SemanticAction::Focus)),
            Self::DropdownOpen => Some((
                "dropdown",
                SemanticAction::Click {
                    button: MouseButton::Left,
                    count: 1,
                },
            )),
        }
    }
}

struct FixtureView {
    live: LiveHtml,
}

impl Render for FixtureView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.live.render(window, cx)
    }
}

fn main() -> Result<()> {
    match Arguments::parse().command {
        Command::Fixture { fixture, state } => run_fixture(fixture, state),
        Command::Capture {
            pid,
            title,
            output,
            raw_output,
            width,
            height,
            timeout_ms,
            settle_ms,
        } => capture_window(&CaptureRequest {
            pid,
            title,
            output,
            raw_output,
            width,
            height,
            timeout: Duration::from_millis(timeout_ms),
            settle: Duration::from_millis(settle_ms),
        }),
    }
}

fn run_fixture(fixture: FixtureName, state: FixtureState) -> Result<()> {
    ensure!(
        state == FixtureState::Baseline || fixture == FixtureName::Behaviors,
        "interactive states are available only for the behaviors fixture"
    );
    let ui = HtmlUi::compile(fixture.html(), BindingDocument::new())
        .context("compile visual parity fixture")?;
    ensure!(
        ui.diagnostics().is_empty(),
        "fixture compiler diagnostics: {:?}",
        ui.diagnostics()
    );
    let automation = Automation::for_test();
    let live = LiveHtml::new(ui, automation.clone(), HookRegistry::new())
        .context("connect visual parity fixture")?;
    ensure!(
        live.diagnostics().is_empty(),
        "fixture renderer diagnostics: {:?}",
        live.diagnostics()
    );

    Application::new().run(move |cx: &mut App| {
        gpui_mcp_html::init(cx);
        let bounds = Bounds::centered(None, fixture_window_size(), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: true,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Opaque,
                window_decorations: Some(WindowDecorations::Client),
                ..WindowOptions::default()
            },
            |window, cx| {
                window.set_window_title(WINDOW_TITLE);
                let automation = automation.clone();
                window
                    .spawn(cx, async move |cx| {
                        if let Err(error) = wait_for_completed_frame(&automation, 0).await {
                            eprintln!("could not present fixture window: {error:#}");
                            let _ = cx.update(|_, cx| cx.quit());
                            return;
                        }

                        if let Some((node_id, action)) = state.action() {
                            let completed_frame = automation.completed_test_frame_count();
                            let result = cx.update(|window, cx| {
                                let generation = automation.snapshot().generation;
                                let result = automation
                                    .dispatch_test_action(node_id, generation, &action, window, cx);
                                if result.is_ok() {
                                    window.refresh();
                                }
                                result
                            });
                            match result {
                                Ok(Ok(_)) => {}
                                Ok(Err(error)) => {
                                    eprintln!("could not apply fixture state: {}", error.message);
                                    let _ = cx.update(|_, cx| cx.quit());
                                    return;
                                }
                                Err(error) => {
                                    eprintln!("could not update fixture window: {error}");
                                    return;
                                }
                            }
                            if let Err(error) =
                                wait_for_completed_frame(&automation, completed_frame).await
                            {
                                eprintln!("could not present fixture state: {error:#}");
                                let _ = cx.update(|_, cx| cx.quit());
                                return;
                            }
                        }
                        print_ready();
                    })
                    .detach();
                cx.new(|_| FixtureView { live })
            },
        );
        if let Err(error) = opened {
            eprintln!("could not open visual parity fixture: {error:#}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
    Ok(())
}

async fn wait_for_completed_frame(automation: &Automation, after_frame: u64) -> Result<()> {
    let started = Instant::now();
    while automation.completed_test_frame_count() <= after_frame {
        ensure!(
            started.elapsed() < FIXTURE_READY_TIMEOUT,
            "fixture did not complete a root-paint frame within {FIXTURE_READY_TIMEOUT:?}"
        );
        Timer::after(FRAME_POLL_INTERVAL).await;
    }
    Ok(())
}

fn print_ready() {
    println!("READY {} {WINDOW_TITLE}", std::process::id());
    let _ = std::io::stdout().flush();
}

struct CaptureRequest {
    pid: u32,
    title: String,
    output: PathBuf,
    raw_output: Option<PathBuf>,
    width: u32,
    height: u32,
    timeout: Duration,
    settle: Duration,
}

fn capture_window(request: &CaptureRequest) -> Result<()> {
    ensure!(
        request.width > 0 && request.height > 0,
        "capture size must be non-zero"
    );
    ensure!(
        request.width <= 16_384 && request.height <= 16_384,
        "capture size exceeds 16384 pixels"
    );
    let started = Instant::now();
    let native_window = loop {
        let windows = xcap::Window::all().context("enumerate native windows")?;
        if let Some(window) = windows.into_iter().find(|window| {
            window.pid().ok() == Some(request.pid)
                && window.title().ok().as_deref() == Some(request.title.as_str())
        }) {
            break window;
        }
        if started.elapsed() >= request.timeout {
            bail!(
                "window with pid {} and title {:?} was not available within {:?}",
                request.pid,
                request.title,
                request.timeout
            );
        }
        thread::sleep(Duration::from_millis(50));
    };

    thread::sleep(request.settle);
    let image = native_window
        .capture_image()
        .context("capture GPUI fixture window")?;
    ensure!(
        image.width() > 0 && image.height() > 0,
        "native capture was empty"
    );
    if let Some(path) = &request.raw_output {
        save_png(&image, path)?;
    }
    let normalized = normalize_capture(&image, request.width, request.height)?;
    save_png(&normalized, &request.output)?;
    let scale = f64::from(normalized.width()) / f64::from(request.width);
    println!(
        "CAPTURED {}x{} -> {}x{} at {scale:.2}x",
        native_window.width().unwrap_or(normalized.width()),
        native_window.height().unwrap_or(normalized.height()),
        normalized.width(),
        normalized.height()
    );
    Ok(())
}

fn fixture_window_size() -> gpui::Size<gpui::Pixels> {
    // Native compositors retain small non-client edges even with GPUI client
    // decorations. Compensate when requesting the window so the rendered HTML
    // surface remains exactly 640x480 CSS pixels.
    #[cfg(target_os = "windows")]
    let requested = (646.0, 480.0);
    #[cfg(target_os = "macos")]
    let requested = (642.0, 482.0);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let requested = (640.0, 480.0);
    size(px(requested.0), px(requested.1))
}

fn normalize_capture(image: &RgbaImage, width: u32, height: u32) -> Result<RgbaImage> {
    let horizontal_probe = image.height().saturating_mul(3) / 4;
    let vertical_probe = image.width() / 2;
    let left = (0..image.width())
        .find(|x| *image.get_pixel(*x, horizontal_probe) == FIXTURE_BACKGROUND)
        .unwrap_or(0);
    let right = (0..image.width())
        .rev()
        .find(|x| *image.get_pixel(*x, horizontal_probe) == FIXTURE_BACKGROUND)
        .map_or(image.width(), |x| x.saturating_add(1));
    let top = (0..image.height())
        .find(|y| *image.get_pixel(vertical_probe, *y) == FIXTURE_BACKGROUND)
        .unwrap_or(0);
    let bottom = (0..image.height())
        .rev()
        .find(|y| *image.get_pixel(vertical_probe, *y) == FIXTURE_BACKGROUND)
        .map_or(image.height(), |y| y.saturating_add(1));
    ensure!(
        right > left && bottom > top,
        "native capture contains no fixture surface"
    );
    let content_width = right - left;
    let content_height = bottom - top;
    let minimum_width = width.saturating_mul(3) / 4;
    let maximum_width = width.saturating_mul(4);
    ensure!(
        (minimum_width..=maximum_width).contains(&content_width),
        "native client width {content_width} is outside the supported 0.75x to 4x scale range for logical width {width}"
    );
    let scaled_width = content_width;
    let scaled_height = u32::try_from(
        (u64::from(content_width) * u64::from(height) + u64::from(width / 2)) / u64::from(width),
    )
    .context("scaled capture height exceeds u32")?;
    let frame_allowance = 8_u32.saturating_mul(content_width.div_ceil(width).max(1));
    ensure!(
        content_height.abs_diff(scaled_height) <= frame_allowance,
        "native client surface {content_width}x{content_height} does not match scaled target {scaled_width}x{scaled_height} within the frame allowance"
    );

    let cropped = image::imageops::crop_imm(
        image,
        left,
        top,
        content_width.min(scaled_width),
        content_height.min(scaled_height),
    )
    .to_image();
    let mut normalized = RgbaImage::from_pixel(scaled_width, scaled_height, FIXTURE_BACKGROUND);
    image::imageops::replace(&mut normalized, &cropped, 0, 0);
    Ok(normalized)
}

fn save_png(image: &RgbaImage, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create screenshot directory {}", parent.display()))?;
    }
    DynamicImage::ImageRgba8(image.clone())
        .save_with_format(path, ImageFormat::Png)
        .with_context(|| format!("write PNG {}", path.display()))
}

#[cfg(test)]
mod tests {
    use image::{Rgba, RgbaImage};

    use super::{FIXTURE_BACKGROUND, normalize_capture};

    #[test]
    fn native_frame_is_trimmed_without_resampling_content() {
        let mut native = RgbaImage::from_pixel(9, 8, Rgba([0, 0, 0, 255]));
        for y in 1..7 {
            for x in 1..9 {
                native.put_pixel(x, y, FIXTURE_BACKGROUND);
            }
        }
        native.put_pixel(2, 2, Rgba([255, 0, 0, 255]));
        let result = normalize_capture(&native, 8, 6);
        assert!(result.is_ok(), "normalization failed: {:?}", result.err());
        let Some(result) = result.ok() else {
            return;
        };
        assert_eq!(result.dimensions(), (8, 6));
        assert_eq!(*result.get_pixel(1, 1), Rgba([255, 0, 0, 255]));
    }

    #[test]
    fn high_density_capture_preserves_native_pixels() {
        let mut native = RgbaImage::from_pixel(18, 14, Rgba([0, 0, 0, 255]));
        for y in 1..13 {
            for x in 1..17 {
                native.put_pixel(x, y, FIXTURE_BACKGROUND);
            }
        }
        native.put_pixel(3, 3, Rgba([255, 0, 0, 255]));
        let result = normalize_capture(&native, 8, 6);
        assert!(result.is_ok(), "normalization failed: {:?}", result.err());
        let Some(result) = result.ok() else {
            return;
        };
        assert_eq!(result.dimensions(), (16, 12));
        assert_eq!(*result.get_pixel(2, 2), Rgba([255, 0, 0, 255]));
    }
}
