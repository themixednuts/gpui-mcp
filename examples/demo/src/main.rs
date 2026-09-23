//! Small instrumented GPUI application used to exercise the MCP bridge.

use gpui::{
    App, Bounds, Context, Div, Entity, FocusHandle, IntoElement, Render, Role, Stateful,
    StatefulInteractiveElement as _, StyleRefinement, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use gpui_mcp::{Automation, BridgeConfig, BridgeHandle};

const TITLE: &str = "GPUI MCP Demo";

/// `--endpoint-dir <absolute path>` keeps a driving test's discovery private to
/// that test rather than sharing the developer's live endpoint directory.
fn endpoint_dir() -> Option<std::path::PathBuf> {
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--endpoint-dir" {
            return arguments.next().map(std::path::PathBuf::from);
        }
    }
    None
}

struct Demo {
    count: usize,
    locked: bool,
    search: FocusHandle,
    filter: FocusHandle,
    probes: [Entity<ProbeRegion>; 2],
    automation: Automation,
    _bridge: BridgeHandle,
}

/// One region drawn as its own cached view, the way a workbench draws each of
/// its panels. Hovering its control notifies this view alone, so a frame report
/// taken across the hover should show this region rendering and its sibling
/// replaying from cache. Anything that renders both, such as a window refresh,
/// is visible in the report as the cause of the sibling's render.
struct ProbeRegion {
    id: &'static str,
    target: &'static str,
    label: &'static str,
}

impl Render for ProbeRegion {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().id(self.id).size_full().flex().items_center().child(
            div()
                .id(self.target)
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(0x22_28_33))
                .hover(|style| style.bg(rgb(0x39_42_53)))
                .child(self.label)
                .aria_label(self.label),
        )
    }
}

/// One keyboard-focusable field carrying the two shapes of visual change a
/// region crop has to survive: focus draws a one-pixel, high-contrast ring on
/// the field's own bounds, and hover repaints its whole interior a single step
/// lighter. A crop that is stale fails the first; a crop that refreshes on
/// layout but not on paint can still pass the second, so both are needed.
///
/// The focus treatment is a plain conditional border and deliberately not
/// GPUI's `focus_visible`, which additionally requires
/// `Window::last_input_was_keyboard()`. A programmatic focus dispatches no
/// platform input and a programmatic pointer move claims Mouse modality, so a
/// `focus_visible` ring here would never paint for a driving test that parks
/// the pointer first, and the region capture test would pass while measuring
/// nothing. Change this to `focus_visible` only alongside driving the field
/// with a real keystroke.
fn field(
    id: &'static str,
    label: &'static str,
    handle: &FocusHandle,
    focused: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .track_focus(handle)
        .w(px(240.0))
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(if focused {
            rgb(0x16_77_ff)
        } else {
            rgb(0x39_42_53)
        })
        .bg(rgb(0x0b_0e_14))
        .hover(|style| style.bg(rgb(0x12_16_1e)))
        .child(label)
        .role(Role::TextInput)
        .aria_label(label)
}

impl Demo {
    fn increment(&mut self, cx: &mut Context<Self>) {
        self.count = self.count.saturating_add(1);
        self.automation
            .log("info", &format!("counter changed to {}", self.count));
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.count = 0;
        self.automation.log("info", "counter reset");
        cx.notify();
    }

    fn toggle_lock(&mut self, cx: &mut Context<Self>) {
        self.locked = !self.locked;
        self.automation.log(
            "info",
            if self.locked {
                "the locked action refuses input"
            } else {
                "the locked action accepts input"
            },
        );
        cx.notify();
    }

    /// A control that refuses input, and the toggle that gives it back.
    ///
    /// The locked control states its own disabled state with `aria_disabled`,
    /// which is the only thing the UI tree's `enabled` is derived from: a widget
    /// that merely withholds its click handler and paints itself grey — which
    /// this one also does — still reports `enabled: true`, so the tree would
    /// assert a falsehood rather than admit it does not know. The toggle exists
    /// so the attribute can be measured flipping on one node rather than read
    /// once, which is the only way to tell a carried value from a constant.
    fn lock_row(&self, cx: &mut Context<Self>) -> Div {
        let locked = self.locked;
        div()
            .flex()
            .gap_3()
            .child(
                div()
                    .id("locked-action")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(if locked {
                        rgb(0x22_28_33)
                    } else {
                        rgb(0x16_77_ff)
                    })
                    .text_color(if locked {
                        rgb(0x6b_74_85)
                    } else {
                        rgb(0xe8_ee_f7)
                    })
                    .child("Locked action")
                    .role(Role::Button)
                    .aria_label("Locked action")
                    .aria_disabled(locked)
                    .when(!locked, |this| {
                        this.cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.increment(cx)))
                    }),
            )
            .child(
                div()
                    .id("lock-toggle")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(0x39_42_53))
                    .cursor_pointer()
                    .child(if locked { "Unlock" } else { "Lock" })
                    .role(Role::Button)
                    .aria_label(if locked { "Unlock" } else { "Lock" })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_lock(cx))),
            )
    }
}

impl Render for Demo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let counter = self.count;
        let searching = self.search.is_focused(window);
        let filtering = self.filter.is_focused(window);
        div()
            .id("demo-root")
            .flex()
            .flex_col()
            .size_full()
            .gap_4()
            .p_8()
            .bg(rgb(0x10_14_1c))
            .text_color(rgb(0xe8_ee_f7))
            .child(
                div()
                    .id("heading")
                    .text_2xl()
                    .child("GPUI MCP cross-platform demo"),
            )
            .child(
                div()
                    .id("count")
                    .text_lg()
                    .child(format!("Count: {counter}")),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(field("search", "Search", &self.search, searching))
                    .child(field("filter", "Filter", &self.filter, filtering)),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .id("increment")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x16_77_ff))
                            .cursor_pointer()
                            .child("Increment")
                            .on_click(cx.listener(|this, _, _, cx| this.increment(cx))),
                    )
                    .child(
                        div()
                            .id("reset")
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(0x39_42_53))
                            .cursor_pointer()
                            .child("Reset")
                            .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
                    ),
            )
            .child(self.lock_row(cx))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .children(self.probes.iter().map(|probe| {
                        probe
                            .clone()
                            .cached(StyleRefinement::default().w(px(200.0)).h(px(40.0)))
                    })),
            )
            .role(Role::Application)
            .aria_label(TITLE)
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.0), px(420.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            },
            |window, cx| {
                window.set_window_title(TITLE);
                let app_id = match gpui_mcp::AppId::new("gpui-mcp-demo") {
                    Ok(app_id) => app_id,
                    Err(error) => {
                        eprintln!("invalid GPUI MCP application ID: {error}");
                        std::process::exit(1);
                    }
                };
                let mut config = BridgeConfig::new(app_id, TITLE);
                if let Some(directory) = endpoint_dir() {
                    config = match config.endpoint_dir(directory) {
                        Ok(config) => config,
                        Err(error) => {
                            eprintln!("invalid endpoint directory: {error}");
                            std::process::exit(1);
                        }
                    };
                }
                let bridge = match BridgeHandle::install(window, cx, config) {
                    Ok(bridge) => bridge,
                    Err(error) => {
                        eprintln!("could not install GPUI MCP bridge: {error}");
                        std::process::exit(1);
                    }
                };
                let automation = bridge.automation();
                cx.new(|cx| Demo {
                    count: 0,
                    locked: true,
                    search: cx.focus_handle(),
                    filter: cx.focus_handle(),
                    probes: [
                        cx.new(|_| ProbeRegion {
                            id: "probe-left",
                            target: "probe-left-target",
                            label: "Left probe",
                        }),
                        cx.new(|_| ProbeRegion {
                            id: "probe-right",
                            target: "probe-right-target",
                            label: "Right probe",
                        }),
                    ],
                    automation,
                    _bridge: bridge,
                })
            },
        );
        if let Err(error) = opened {
            eprintln!("could not open demo window: {error}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
