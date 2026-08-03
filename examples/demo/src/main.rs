//! Small instrumented GPUI application used to exercise the MCP bridge.

use gpui::{
    App, Application, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use gpui_mcp::{
    Automation, BridgeConfig, BridgeHandle, McpElementExt, NodeAction, NodeEvent, NodeSpec, Role,
    TextInfo,
};

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
        let automation = self.automation.clone();
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
                    .text_2xl()
                    .child("GPUI MCP cross-platform demo")
                    .mcp_node(
                        &automation,
                        NodeSpec::new("heading", Role::Text).label("GPUI MCP cross-platform demo"),
                    ),
            )
            .child(
                div().text_lg().child(format!("Count: {counter}")).mcp_node(
                    &automation,
                    NodeSpec::new("count", Role::Text)
                        .label("Counter value")
                        .text(TextInfo {
                            text: counter.to_string(),
                            ..TextInfo::default()
                        }),
                ),
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
                            .on_click(cx.listener(|this, _, _, cx| this.increment(cx)))
                            .mcp_node(
                                &automation,
                                NodeSpec::new("increment", Role::Button)
                                    .label("Increment counter")
                                    .action(NodeAction::Click)
                                    .on_event(cx.listener(|this, event, _, cx| {
                                        if matches!(event, NodeEvent::Click { .. }) {
                                            this.increment(cx);
                                        }
                                    })),
                            ),
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
                            .on_click(cx.listener(|this, _, _, cx| this.reset(cx)))
                            .mcp_node(
                                &automation,
                                NodeSpec::new("reset", Role::Button)
                                    .label("Reset counter")
                                    .action(NodeAction::Click)
                                    .on_event(cx.listener(|this, event, _, cx| {
                                        if matches!(event, NodeEvent::Click { .. }) {
                                            this.reset(cx);
                                        }
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .mt_4()
                    .text_sm()
                    .text_color(rgb(0x9b_a8_bb))
                    .child("The endpoint descriptor path is printed to stderr at startup.")
                    .mcp_node(
                        &automation,
                        NodeSpec::new("help", Role::Text).label("Endpoint descriptor help"),
                    ),
            )
            .mcp_node(
                &automation,
                NodeSpec::new("app", Role::Application).label(TITLE).root(),
            )
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
                let bridge = match BridgeHandle::install(
                    window,
                    cx,
                    BridgeConfig::new("gpui-mcp-demo", TITLE),
                ) {
                    Ok(bridge) => bridge,
                    Err(error) => {
                        eprintln!("could not install GPUI MCP bridge: {error}");
                        std::process::exit(1);
                    }
                };
                eprintln!("GPUI MCP endpoint: {}", bridge.endpoint_path().display());
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
