//! Live HTML runtime showcase for responsive layout, interaction, and hot reload.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, AppContext, Application, Bounds, Context, IntoElement, Render, Timer, Window,
    WindowBounds, WindowOptions, px, size,
};
use gpui_mcp::{ActionOutcome, BridgeConfig, BridgeHandle};
use gpui_mcp_html::{
    HandlerId, HookRegistry, LiveHtmlSession, ProjectPaths, ProjectSnapshot, ProjectWatcher,
    StateBindingId, StateValue,
};

const APP_ID: &str = "runtime-showcase";
const TITLE: &str = "Runtime Studio - HTML to GPUI";

struct AppView {
    session: LiveHtmlSession,
    watcher: ProjectWatcher,
    bridge: BridgeHandle,
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
            Ok(snapshot) => snapshot.into_live_document_source(),
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
    build_count: Rc<Cell<u32>>,
    status: Rc<RefCell<String>>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            build_count: Rc::new(Cell::new(12)),
            status: Rc::new(RefCell::new(
                "Ready - document compiled into native GPUI elements.".to_owned(),
            )),
        }
    }
}

fn runtime_hooks(state: &RuntimeState) -> Result<HookRegistry, String> {
    let mut hooks = HookRegistry::new();
    let build_count = state.build_count.clone();
    hooks
        .register_state(StateBindingId::new("build_count"), move |_, _| {
            StateValue::Number(f64::from(build_count.get()))
        })
        .map_err(|error| error.to_string())?;

    let status = state.status.clone();
    hooks
        .register_state(StateBindingId::new("status_message"), move |_, _| {
            StateValue::Text(status.borrow().clone())
        })
        .map_err(|error| error.to_string())?;

    let build_count = state.build_count.clone();
    let status = state.status.clone();
    hooks
        .register_event(HandlerId::new("run_build"), move |event, window, _| {
            let next_build = build_count.get() + 1;
            build_count.set(next_build);
            *status.borrow_mut() =
                format!("Build #{next_build} complete - HTML -> RenderPlan -> GPUI.");
            eprintln!(
                "{} completed build #{next_build}",
                event.element_id().as_str()
            );
            window.refresh();
            ActionOutcome::Handled
        })
        .map_err(|error| error.to_string())?;

    let build_count = state.build_count.clone();
    let status = state.status.clone();
    hooks
        .register_event(HandlerId::new("reset_build"), move |event, window, _| {
            build_count.set(0);
            "Session reset - runtime is ready.".clone_into(&mut status.borrow_mut());
            eprintln!("{} reset the runtime session", event.element_id().as_str());
            window.refresh();
            ActionOutcome::Handled
        })
        .map_err(|error| error.to_string())?;
    Ok(hooks)
}

fn build_live(window: &Window, cx: &App) -> Result<AppView, String> {
    let bridge = BridgeHandle::install(
        window,
        cx,
        BridgeConfig::new(APP_ID, TITLE).enable_live_document(),
    )
    .map_err(|error| error.to_string())?;
    let paths = ProjectPaths::open(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .map_err(|error| error.to_string())?;
    let source = ProjectSnapshot::load(&paths)
        .map_err(|error| error.to_string())?
        .into_live_document_source();
    let hooks = runtime_hooks(&RuntimeState::default())?;
    let session = LiveHtmlSession::compile(source, bridge.automation(), hooks)
        .map_err(|error| error.to_string())?;
    session
        .register_mcp_preview(&bridge)
        .map_err(|error| error.to_string())?;
    let watcher = ProjectWatcher::new(paths).map_err(|error| error.to_string())?;
    Ok(AppView {
        session,
        watcher,
        bridge,
    })
}

fn main() {
    Application::new().run(|cx: &mut App| {
        gpui_mcp_html::init(cx);
        let bounds = Bounds::centered(None, size(px(1040.0), px(720.0)), cx);
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
                eprintln!(
                    "GPUI MCP endpoint: {}",
                    app.bridge.endpoint_path().display()
                );
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
                view
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
