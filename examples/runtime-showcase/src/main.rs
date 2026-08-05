//! Live HTML runtime showcase for responsive layout, interaction, and hot reload.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext, Application, Bounds, Context, IntoElement, Render, Timer, Window,
    WindowBounds, WindowOptions, px, size,
};
use gpui_mcp::{AppId, BridgeConfig, BridgeHandle};
use gpui_mcp_html::{
    HandlerId, HookOutcome, HookRegistry, LiveHtmlSession, ProjectPaths, ProjectSnapshot,
    ProjectWatcher, StateBindingId, StateValue,
};

const APP_ID: &str = "zed-showcase";
const TITLE: &str = "Ember — GPUI Code Editor";

struct AppView {
    session: LiveHtmlSession,
    watcher: ProjectWatcher,
    _bridge: BridgeHandle,
}

impl AppView {
    fn poll_project(&mut self) {
        let change = match self.watcher.poll() {
            Ok(change) => change,
            Err(error) => {
                eprintln!("project watcher error: {error}");
                return;
            }
        };
        let Some(change) = change else {
            return;
        };
        let source = match ProjectSnapshot::load(self.watcher.paths()) {
            Ok(snapshot) => snapshot.into_document(),
            Err(error) => {
                eprintln!("could not read changed project: {error}");
                return;
            }
        };
        match self.session.preview_source(self.session.revision(), source) {
            Ok(preview) if preview.applied => eprintln!(
                "hot reloaded revision {} from {:?}",
                preview.document.revision,
                change.files()
            ),
            Ok(preview) => eprintln!("hot reload rejected: {:?}", preview.diagnostics),
            Err(error) => eprintln!("hot reload conflict: {}", error.message),
        }
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.session.render(window, cx)
    }
}

#[derive(Clone)]
struct RuntimeState {
    explorer_visible: Rc<Cell<bool>>,
    panel_visible: Rc<Cell<bool>>,
    active_file: Rc<RefCell<String>>,
    editor_value: Rc<RefCell<String>>,
    status: Rc<RefCell<String>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            explorer_visible: Rc::new(Cell::new(true)),
            panel_visible: Rc::new(Cell::new(true)),
            active_file: Rc::new(RefCell::new("main.rs".to_owned())),
            editor_value: Rc::new(RefCell::new(
                "use gpui::{div, prelude::*, rgb, App, Context, IntoElement, Render, Window};\n\npub struct Workspace {\n    project_name: SharedString,\n    panel_open: bool,\n}\n\nimpl Render for Workspace {\n    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>)\n        -> impl IntoElement\n    {\n        div()\n            .flex()\n            .flex_col()\n            .size_full()\n            .bg(rgb(0x181818))\n            .child(editor_header(&self.project_name))\n            .child(editor_surface())\n    }\n}".to_owned(),
            )),
            status: Rc::new(RefCell::new("GPUI MCP connected · ready".to_owned())),
        }
    }
}

fn runtime_hooks(state: &RuntimeState) -> Result<HookRegistry, String> {
    let mut hooks = HookRegistry::new();
    register_state_hooks(&mut hooks, state)?;
    register_action_hooks(&mut hooks, state)?;
    Ok(hooks)
}

fn register_state_hooks(hooks: &mut HookRegistry, state: &RuntimeState) -> Result<(), String> {
    let explorer_visible = state.explorer_visible.clone();
    hooks
        .register_state(StateBindingId::new("explorer_visible"), move |_, _| {
            StateValue::Boolean(explorer_visible.get())
        })
        .map_err(|error| error.to_string())?;

    let panel_visible = state.panel_visible.clone();
    hooks
        .register_state(StateBindingId::new("panel_visible"), move |_, _| {
            StateValue::Boolean(panel_visible.get())
        })
        .map_err(|error| error.to_string())?;

    let active_file = state.active_file.clone();
    hooks
        .register_state(StateBindingId::new("active_file"), move |_, _| {
            StateValue::Text(active_file.borrow().clone())
        })
        .map_err(|error| error.to_string())?;

    let editor_reader = state.editor_value.clone();
    let editor_writer = state.editor_value.clone();
    hooks
        .register_state_mut(
            StateBindingId::new("editor_value"),
            move |_, _| StateValue::Text(editor_reader.borrow().clone()),
            move |value, window, _| {
                let StateValue::Text(value) = value else {
                    return HookOutcome::Rejected {
                        reason: "editor value must be text".to_owned(),
                    };
                };
                *editor_writer.borrow_mut() = value;
                window.refresh();
                HookOutcome::Handled
            },
        )
        .map_err(|error| error.to_string())?;

    let status = state.status.clone();
    hooks
        .register_state(StateBindingId::new("status_message"), move |_, _| {
            StateValue::Text(status.borrow().clone())
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn register_action_hooks(hooks: &mut HookRegistry, state: &RuntimeState) -> Result<(), String> {
    let explorer_visible = state.explorer_visible.clone();
    let status = state.status.clone();
    hooks
        .register_event(HandlerId::new("toggle_explorer"), move |_, window, _| {
            explorer_visible.set(!explorer_visible.get());
            *status.borrow_mut() = if explorer_visible.get() {
                "Explorer dock opened".to_owned()
            } else {
                "Explorer dock closed".to_owned()
            };
            window.refresh();
            HookOutcome::Handled
        })
        .map_err(|error| error.to_string())?;

    let panel_visible = state.panel_visible.clone();
    let status = state.status.clone();
    hooks
        .register_event(HandlerId::new("toggle_panel"), move |_, window, _| {
            panel_visible.set(!panel_visible.get());
            *status.borrow_mut() = if panel_visible.get() {
                "Terminal dock opened".to_owned()
            } else {
                "Terminal dock closed".to_owned()
            };
            window.refresh();
            HookOutcome::Handled
        })
        .map_err(|error| error.to_string())?;

    register_file_event(
        hooks,
        HandlerId::new("open_main"),
        state,
        "main.rs",
        "use gpui::{div, prelude::*, rgb, App, Context, IntoElement, Render, Window};\n\npub struct Workspace {\n    project_name: SharedString,\n    panel_open: bool,\n}\n\nimpl Render for Workspace {\n    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>)\n        -> impl IntoElement\n    {\n        div()\n            .flex()\n            .flex_col()\n            .size_full()\n            .bg(rgb(0x181818))\n            .child(editor_header(&self.project_name))\n            .child(editor_surface())\n    }\n}",
    )?;
    register_file_event(
        hooks,
        HandlerId::new("open_theme"),
        state,
        "theme.rs",
        "pub struct EmberTheme {\n    pub background: Hsla,\n    pub surface: Hsla,\n    pub accent: Hsla,\n}\n\nimpl EmberTheme {\n    pub fn midnight() -> Self {\n        Self {\n            background: hsla(0.0, 0.0, 0.09, 1.0),\n            surface: hsla(0.0, 0.0, 0.12, 1.0),\n            accent: hsla(0.58, 0.72, 0.61, 1.0),\n        }\n    }\n}",
    )?;
    register_file_event(
        hooks,
        HandlerId::new("open_readme"),
        state,
        "README.md",
        "# Ember\n\nA small native editor built with GPUI.\n\n- HTML-authored shell\n- Native GPUI runtime\n- Semantic MCP automation\n- Live, incremental video capture\n",
    )?;

    for (handler, message) in [
        ("run_project", "cargo run finished · 0 errors · 412ms"),
        ("select_midnight", "Theme changed to Midnight"),
        ("select_ayu", "Theme changed to Ayu Mirage"),
        ("clear_terminal", "Terminal cleared"),
    ] {
        let status = state.status.clone();
        hooks
            .register_event(HandlerId::new(handler), move |_, window, _| {
                message.clone_into(&mut status.borrow_mut());
                window.refresh();
                HookOutcome::Handled
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn register_file_event(
    hooks: &mut HookRegistry,
    handler: HandlerId,
    state: &RuntimeState,
    file: &'static str,
    source: &'static str,
) -> Result<(), String> {
    let active_file = state.active_file.clone();
    let editor_value = state.editor_value.clone();
    let status = state.status.clone();
    hooks
        .register_event(handler, move |_, window, _| {
            file.clone_into(&mut active_file.borrow_mut());
            source.clone_into(&mut editor_value.borrow_mut());
            *status.borrow_mut() = format!("Opened {file}");
            window.refresh();
            HookOutcome::Handled
        })
        .map_err(|error| error.to_string())
}

fn build_live(window: &mut Window, cx: &App) -> Result<AppView, String> {
    let bridge = BridgeHandle::install(
        window,
        cx,
        BridgeConfig::new(
            AppId::new(APP_ID).map_err(|error| error.to_string())?,
            TITLE,
        ),
    )
    .map_err(|error| error.to_string())?;
    let paths = ProjectPaths::open(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .map_err(|error| error.to_string())?;
    let source = ProjectSnapshot::load(&paths)
        .map_err(|error| error.to_string())?
        .into_document();
    let hooks = runtime_hooks(&RuntimeState::default())?;
    let session = LiveHtmlSession::compile(source, bridge.automation(), hooks)
        .map_err(|error| error.to_string())?;
    session
        .serve_mcp(&bridge)
        .map_err(|error| error.to_string())?;
    let watcher = ProjectWatcher::new(paths).map_err(|error| error.to_string())?;
    Ok(AppView {
        session,
        watcher,
        _bridge: bridge,
    })
}

fn main() {
    Application::new().run(|cx: &mut App| {
        gpui_mcp_html::init(cx);
        let bounds = Bounds::centered(None, size(px(1200.0), px(760.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            },
            |window, cx| {
                window.set_window_title(TITLE);
                let app = build_live(window, cx).unwrap_or_else(|error| {
                    eprintln!("could not initialize application: {error}");
                    std::process::exit(1);
                });
                let view = cx.new(|_| app);
                let weak_view = view.downgrade();
                window
                    .spawn(cx, async move |cx| {
                        loop {
                            Timer::after(Duration::from_millis(50)).await;
                            if weak_view
                                .update(cx, |view, cx| {
                                    view.poll_project();
                                    cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                cx.new(|cx| gpui_mcp_html::NativeRoot::new(view, window, cx))
            },
        );
        if let Err(error) = opened {
            eprintln!("could not open application window: {error}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
