//! Small instrumented GPUI application used to exercise the MCP bridge.

use gpui::{
    App, Application, Bounds, Context, IntoElement, Render, SemanticRole, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_mcp::{Automation, BridgeConfig, BridgeHandle};

const TITLE: &str = "GPUI MCP Demo";

struct Demo {
    count: usize,
    automation: Automation,
    _bridge: BridgeHandle,
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
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let counter = self.count;
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
            .semantic_role(SemanticRole::Application)
            .accessible_name(TITLE)
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    Application::new().run(|cx: &mut App| {
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
                let bridge =
                    match BridgeHandle::install(window, cx, BridgeConfig::new(app_id, TITLE)) {
                        Ok(bridge) => bridge,
                        Err(error) => {
                            eprintln!("could not install GPUI MCP bridge: {error}");
                            std::process::exit(1);
                        }
                    };
                let automation = bridge.automation();
                cx.new(|_| Demo {
                    count: 0,
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
