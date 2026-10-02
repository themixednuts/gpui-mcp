//! End-to-end pure HTML rendering through a real GPUI test window and MCP semantics.

#[cfg(feature = "gpui-pre")]
extern crate gpui_pre as gpui;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    App, Context, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PlatformInput, Render, ScrollDelta, ScrollWheelEvent, Styled as _,
    TestAppContext, TouchPhase, Window, div, point, px, size,
};
use gpui_mcp::{Automation, NodeAction, Role, UiTree, ValueInfo};
use gpui_mcp_html::{
    Binding, BindingDocument, BindingMode, BindingTarget, ComponentRegistry, ElementId, HandlerId,
    HookOutcome, HookRegistry, HtmlUi, LiveHtml, SemanticNamespace, StateBindingId, StateValue,
    UiEvent, UiProperty,
};

const HTML: &str = r#"<!doctype html>
<html>
  <body>
    <main id="workspace">
      <h1 id="heading">Runtime harness</h1>
      <label for="title">Title</label>
      <input id="title" type="text">
      <label for="secret">Secret</label>
      <input id="secret" type="password" value="must-not-leak">
      <label for="published">Published</label>
      <input id="published" type="checkbox">
      <button id="save" type="button">Save</button>
      <project-card id="preview">
        <span id="status">default status</span>
      </project-card>
    </main>
  </body>
</html>"#;

const CSS: &str = r"
body {
  display: flex;
  flex-direction: column;
  width: 640px;
  padding: 12px;
  background-color: #10141c;
  color: #e8eef7;
}

#workspace {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

project-card {
  display: block;
  padding: 4px;
  border-width: 1px;
  border-style: solid;
  border-color: #445066;
}

button:hover {
  background-color: #22314a;
}

#save:hover {
  border-color: #5d8cff;
}
";

const BEHAVIORS_HTML: &str = include_str!("../../../visual-tests/fixtures/behaviors.html");
const COMPLEX_LAYOUT_HTML: &str =
    include_str!("../../../visual-tests/fixtures/complex-layout.html");

const RESPONSIVE_HEIGHT_HTML: &str = r#"<!doctype html>
<html>
  <body>
    <main id="responsive-shell">
      <header id="fixed-header">Header</header>
      <section id="flex-content">Content</section>
      <footer id="fixed-footer">Footer</footer>
    </main>
  </body>
</html>"#;

const RESPONSIVE_HEIGHT_CSS: &str = r"
html, body, #responsive-shell {
  width: 100%;
  height: 100%;
  margin: 0;
}

#responsive-shell {
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow: hidden;
}

#fixed-header {
  height: 40px;
  flex-shrink: 0;
}

#flex-content {
  min-height: 0;
  flex: 1 1 0%;
}

#fixed-footer {
  height: 20px;
  flex-shrink: 0;
}
";

const EMBEDDED_HEIGHT_HTML: &str = r#"<!doctype html>
<html>
  <body>
    <main id="embedded-shell">
      <header id="embedded-header">Header</header>
      <studio-canvas id="embedded-canvas"></studio-canvas>
      <footer id="embedded-footer">Footer</footer>
    </main>
  </body>
</html>"#;

const EMBEDDED_HEIGHT_CSS: &str = r"
html, body, #embedded-shell {
  width: 100%;
  height: 100%;
  margin: 0;
}

#embedded-shell {
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow: hidden;
}

#embedded-header {
  height: 40px;
  flex-shrink: 0;
}

#embedded-canvas {
  min-height: 0;
  flex: 1 1 0%;
  overflow: hidden;
}

#embedded-footer {
  height: 20px;
  flex-shrink: 0;
}
";

const INNER_HEIGHT_HTML: &str = r#"<!doctype html>
<html>
  <body>
    <main id="inner-shell">Inner project</main>
  </body>
</html>"#;

const INNER_HEIGHT_CSS: &str = r"
html, body, #inner-shell {
  width: 100%;
  height: 100%;
  margin: 0;
}
";

const SCROLL_HTML: &str = r#"<!doctype html>
<html>
  <body>
    <main id="scroller">
      <section id="scroll-top">Top</section>
      <section id="scroll-bottom">Bottom</section>
    </main>
  </body>
</html>"#;

const SCROLL_CSS: &str = r"
#scroller {
  display: flex;
  flex-direction: column;
  width: 200px;
  height: 100px;
  overflow-y: auto;
}

#scroll-top, #scroll-bottom {
  height: 100px;
  flex-shrink: 0;
}
";

struct RuntimeView {
    live: LiveHtml,
}

impl Render for RuntimeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.live.render(window, cx)
    }
}

struct Fixture {
    live: LiveHtml,
    automation: Automation,
    state: TestState,
}

#[derive(Clone, Debug)]
enum TestInput {
    Click { button: MouseButton, count: u8 },
    Focus,
    Hover,
    Scroll { delta_x: f32, delta_y: f32 },
    SetValue { value: String },
}

fn dispatch_test_input(
    automation: &Automation,
    node_id: &str,
    action: &TestInput,
    window: &mut Window,
    cx: &mut App,
) -> Result<HookOutcome, gpui_mcp::BridgeError> {
    let tree = automation.snapshot();
    let node = tree.nodes.get(node_id).ok_or_else(|| {
        gpui_mcp::BridgeError::new(gpui_mcp::ErrorCode::NotFound, "semantic node was not found")
    })?;
    let required = match action {
        TestInput::Click { .. } | TestInput::SetValue { .. } => NodeAction::Click,
        TestInput::Focus => NodeAction::Focus,
        TestInput::Hover => NodeAction::Hover,
        TestInput::Scroll { .. } => NodeAction::Scroll,
    };
    if !node.actions.contains(&required) {
        return Err(gpui_mcp::BridgeError::new(
            gpui_mcp::ErrorCode::Unsupported,
            "semantic node does not support the requested input",
        ));
    }
    if let TestInput::Focus = action {
        return window
            .focus_observed_element(node_id, cx)
            .then_some(HookOutcome::Handled)
            .ok_or_else(|| {
                gpui_mcp::BridgeError::new(gpui_mcp::ErrorCode::NotFound, "node is not focusable")
            });
    }
    if let TestInput::SetValue { value } = action {
        let requested = match value.as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Ok(HookOutcome::Rejected {
                    reason: "checked and selected values accept only `true` or `false`".to_owned(),
                });
            }
        };
        if node.state.checked == Some(requested) {
            return Ok(HookOutcome::Handled);
        }
    }
    let bounds = node.bounds.ok_or_else(|| {
        gpui_mcp::BridgeError::new(gpui_mcp::ErrorCode::NotFound, "semantic node has no bounds")
    })?;
    let center = point(px(bounds.center().x), px(bounds.center().y));
    dispatch_pointer_test_input(action, center, window, cx);
    window.refresh();
    Ok(HookOutcome::Handled)
}

fn dispatch_pointer_test_input(
    action: &TestInput,
    center: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    match action {
        TestInput::Click { button, count } => {
            for click_count in 1..=*count {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: *button,
                        position: center,
                        modifiers: Modifiers::default(),
                        click_count: usize::from(click_count),
                        first_mouse: false,
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: *button,
                        position: center,
                        modifiers: Modifiers::default(),
                        click_count: usize::from(click_count),
                    }),
                    cx,
                );
            }
        }
        TestInput::Hover => {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: center,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
        }
        TestInput::Scroll { delta_x, delta_y } => {
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: center,
                    delta: ScrollDelta::Pixels(point(px(-*delta_x), px(-*delta_y))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                }),
                cx,
            );
        }
        TestInput::SetValue { .. } => {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: center,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: center,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        }
        TestInput::Focus => {}
    }
}

#[derive(Clone)]
struct TestState {
    title: Rc<RefCell<String>>,
    published: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<String>>>,
    component_renders: Rc<Cell<usize>>,
}

fn bindings() -> BindingDocument {
    BindingDocument::new()
        .with_binding(Binding::Property {
            target: BindingTarget::Id(ElementId::new("title")),
            property: UiProperty::Value,
            source: StateBindingId::new("document_title"),
            mode: BindingMode::TwoWay,
        })
        .with_binding(Binding::Event {
            target: BindingTarget::Id(ElementId::new("save")),
            event: UiEvent::Click,
            handler: HandlerId::new("save_document"),
        })
        .with_binding(Binding::Event {
            target: BindingTarget::Id(ElementId::new("save")),
            event: UiEvent::DoubleClick,
            handler: HandlerId::new("open_document"),
        })
        .with_binding(Binding::Property {
            target: BindingTarget::Id(ElementId::new("status")),
            property: UiProperty::Text,
            source: StateBindingId::new("status_message"),
            mode: BindingMode::OneWay,
        })
        .with_binding(Binding::Property {
            target: BindingTarget::Id(ElementId::new("published")),
            property: UiProperty::Checked,
            source: StateBindingId::new("is_published"),
            mode: BindingMode::TwoWay,
        })
}

fn expect_ok<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> Option<T> {
    assert!(result.is_ok(), "{context}: {:?}", result.as_ref().err());
    result.ok()
}

fn build_hooks(state: &TestState) -> Option<HookRegistry> {
    let mut hooks = HookRegistry::new();
    let title_reader = state.title.clone();
    let title_writer = state.title.clone();
    expect_ok(
        hooks.register_state_mut(
            StateBindingId::new("document_title"),
            move |_, _| StateValue::Text(title_reader.borrow().clone()),
            move |value, window, _| {
                let StateValue::Text(value) = value else {
                    return HookOutcome::Rejected {
                        reason: "title requires text".to_owned(),
                    };
                };
                *title_writer.borrow_mut() = value;
                window.refresh();
                HookOutcome::Handled
            },
        ),
        "title hook should register",
    )?;
    expect_ok(
        hooks.register_state(StateBindingId::new("status_message"), |_, _| {
            StateValue::Text("Ready".to_owned())
        }),
        "status hook should register",
    )?;

    register_published_hook(&mut hooks, state)?;
    let recorded_events = state.events.clone();
    expect_ok(
        hooks.register_event(HandlerId::new("save_document"), move |event, _, _| {
            recorded_events.borrow_mut().push(format!(
                "{:?}:{}",
                event.event(),
                event.element_id().as_str()
            ));
            HookOutcome::Handled
        }),
        "save hook should register",
    )?;
    let recorded_events = state.events.clone();
    expect_ok(
        hooks.register_event(HandlerId::new("open_document"), move |event, _, _| {
            recorded_events.borrow_mut().push(format!(
                "{:?}:{}",
                event.event(),
                event.element_id().as_str()
            ));
            HookOutcome::Handled
        }),
        "double-click hook should register",
    )?;
    Some(hooks)
}

fn register_published_hook(hooks: &mut HookRegistry, state: &TestState) -> Option<()> {
    let published_reader = state.published.clone();
    let published_writer = state.published.clone();
    expect_ok(
        hooks.register_state_mut(
            StateBindingId::new("is_published"),
            move |_, _| StateValue::Boolean(published_reader.get()),
            move |value, window, _| {
                let StateValue::Boolean(value) = value else {
                    return HookOutcome::Rejected {
                        reason: "published requires a boolean".to_owned(),
                    };
                };
                published_writer.set(value);
                window.refresh();
                HookOutcome::Handled
            },
        ),
        "published hook should register",
    )
}

fn build_components(state: &TestState) -> Option<ComponentRegistry> {
    let render_count = state.component_renders.clone();
    let mut components = ComponentRegistry::new();
    expect_ok(
        components.register("project-card", move |_, children, _, _| {
            render_count.set(render_count.get() + 1);
            children
                .into_iter()
                .fold(div().p(px(6.0)), gpui::ParentElement::child)
                .into_any_element()
        }),
        "component should register",
    )?;
    Some(components)
}

fn build_fixture() -> Option<Fixture> {
    let ui = expect_ok(
        HtmlUi::compile_with_stylesheet(HTML, bindings(), "runtime.css", CSS),
        "runtime fixture should compile",
    )?;
    assert!(ui.diagnostics().is_empty(), "{:?}", ui.diagnostics());
    let state = TestState {
        title: Rc::new(RefCell::new("Draft title".to_owned())),
        published: Rc::new(Cell::new(false)),
        events: Rc::new(RefCell::new(Vec::new())),
        component_renders: Rc::new(Cell::new(0)),
    };
    let hooks = build_hooks(&state)?;
    let components = build_components(&state)?;
    let automation = Automation::for_test();
    let live = expect_ok(
        LiveHtml::new(ui, automation.clone(), hooks),
        "live renderer should resolve hooks",
    )?
    .with_components(components);
    assert!(live.diagnostics().is_empty(), "{:?}", live.diagnostics());

    Some(Fixture {
        live,
        automation,
        state,
    })
}

fn assert_initial_tree(tree: &UiTree, state: &TestState) {
    assert_eq!(tree.roots, ["html-root"]);
    assert_eq!(tree.nodes["html-root"].parent, None);
    assert_eq!(tree.nodes["workspace"].parent.as_deref(), Some("html-root"));
    assert_eq!(
        tree.nodes["workspace"]
            .metadata
            .get("authored_id")
            .map(String::as_str),
        Some("workspace")
    );
    assert_eq!(tree.nodes["heading"].role, Role::Text);
    assert_eq!(
        tree.nodes["heading"]
            .text
            .as_ref()
            .map(|text| text.text.as_str()),
        Some("Runtime harness")
    );
    assert_eq!(tree.nodes["title"].role, Role::TextInput);
    assert_eq!(
        tree.nodes["title"].value.as_ref(),
        Some(&ValueInfo {
            value: "Draft title".to_owned(),
            ..ValueInfo::default()
        })
    );
    assert_eq!(tree.nodes["save"].label.as_deref(), Some("Save"));
    assert_eq!(tree.nodes["secret"].role, Role::TextInput);
    assert_eq!(
        tree.nodes["secret"]
            .text
            .as_ref()
            .map(|text| (text.text.as_str(), text.redacted)),
        Some(("", true))
    );
    assert!(tree.nodes["secret"].value.is_none());
    assert_eq!(tree.nodes["published"].role, Role::Checkbox);
    assert_eq!(tree.nodes["published"].state.checked, Some(false));
    assert!(tree.nodes["published"].actions.contains(&NodeAction::Click));
    assert_eq!(
        tree.nodes["status"]
            .text
            .as_ref()
            .map(|text| text.text.as_str()),
        Some("Ready")
    );
    assert!(tree.nodes["save"].actions.contains(&NodeAction::Click));
    assert!(tree.nodes.values().all(|node| node.bounds.is_some()));
    assert!(state.component_renders.get() > 0);
    assert!(
        tree.nodes["html-root"]
            .bounds
            .as_ref()
            .is_some_and(|bounds| bounds.width >= 640.0),
        "HTML root should have styled GPUI layout bounds"
    );
    assert!(
        tree.nodes["html-root"]
            .bounds
            .as_ref()
            .zip(tree.nodes["workspace"].bounds.as_ref())
            .is_some_and(|(root, workspace)| workspace.x >= root.x + 12.0),
        "workspace should reflect the root padding"
    );
}

fn dispatch_actions(automation: &Automation, window: &mut Window, cx: &mut App) {
    let click = TestInput::Click {
        button: MouseButton::Left,
        count: 1,
    };
    let invalid_published = TestInput::SetValue {
        value: "yes".to_owned(),
    };
    let publish = TestInput::SetValue {
        value: "true".to_owned(),
    };
    let mut dispatch =
        |node_id, action| dispatch_test_input(automation, node_id, action, window, cx);
    assert_eq!(dispatch("save", &click), Ok(HookOutcome::Handled));
    assert_eq!(
        dispatch(
            "save",
            &TestInput::Click {
                button: MouseButton::Left,
                count: 2,
            }
        ),
        Ok(HookOutcome::Handled)
    );
    assert_eq!(
        dispatch("published", &invalid_published),
        Ok(HookOutcome::Rejected {
            reason: "checked and selected values accept only `true` or `false`".to_owned(),
        })
    );
    assert_eq!(dispatch("published", &publish), Ok(HookOutcome::Handled));
    assert_eq!(
        dispatch("title", &TestInput::Focus),
        Ok(HookOutcome::Handled)
    );
}

fn assert_updated_tree(tree: &UiTree, old_generation: u64) {
    assert_eq!(
        tree.nodes["title"]
            .value
            .as_ref()
            .map(|value| value.value.as_str()),
        Some("Published title")
    );
    assert!(tree.generation > old_generation);
    assert_eq!(tree.nodes["published"].state.checked, Some(true));
}

#[gpui::test]
fn html_renders_to_gpui_and_uses_real_input(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let Some(fixture) = build_fixture() else {
        return;
    };
    let Fixture {
        live,
        automation,
        state,
    } = fixture;
    let (view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();

    let tree = automation.snapshot();
    assert_initial_tree(&tree, &state);
    visual.update(|window, cx| dispatch_actions(&automation, window, cx));
    assert_eq!(
        &*state.events.borrow(),
        &["Click:save", "Click:save", "Click:save", "DoubleClick:save"]
    );
    assert!(state.published.get());

    view.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();
    visual.update(|window, cx| {
        assert!(window.replace_input_text("Published title", cx));
    });
    assert_eq!(&*state.title.borrow(), "Published title");
    view.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();
    assert_updated_tree(&automation.snapshot(), tree.generation);
}

#[gpui::test]
fn complex_layout_and_interactive_states_round_trip(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let Some(complex) = expect_ok(
        HtmlUi::compile(COMPLEX_LAYOUT_HTML, BindingDocument::new()),
        "complex layout should compile",
    ) else {
        return;
    };
    assert!(
        complex.diagnostics().is_empty(),
        "{:?}",
        complex.diagnostics()
    );
    let complex_live = expect_ok(
        LiveHtml::new(complex, Automation::for_test(), HookRegistry::new()),
        "complex layout should connect",
    );
    assert!(
        complex_live
            .as_ref()
            .is_some_and(|live| live.diagnostics().is_empty()),
        "complex grid declarations should be supported"
    );

    let Some(ui) = expect_ok(
        HtmlUi::compile(BEHAVIORS_HTML, BindingDocument::new()),
        "behavior fixture should compile",
    ) else {
        return;
    };
    assert!(ui.diagnostics().is_empty(), "{:?}", ui.diagnostics());
    let automation = Automation::for_test();
    let Some(live) = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "behavior fixture should connect",
    ) else {
        return;
    };
    assert!(live.diagnostics().is_empty(), "{:?}", live.diagnostics());

    let (view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();
    let initial = automation.snapshot();
    assert_eq!(initial.nodes["dropdown"].state.expanded, Some(false));
    assert!(!initial.nodes.contains_key("menu"));
    let disclosure_control = initial
        .nodes
        .values()
        .find(|node| {
            node.parent.as_deref() == Some("dropdown") && node.actions.contains(&NodeAction::Click)
        })
        .map(|node| node.id.clone());
    assert!(disclosure_control.is_some());
    let disclosure_control = disclosure_control.unwrap_or_default();
    assert!(
        initial.nodes["hover-card"]
            .actions
            .contains(&NodeAction::Hover)
    );
    assert!(
        initial.nodes["focus-card"]
            .actions
            .contains(&NodeAction::Focus)
    );

    visual.update(|window, cx| {
        assert_eq!(
            dispatch_test_input(&automation, "hover-card", &TestInput::Hover, window, cx,),
            Ok(HookOutcome::Handled)
        );
        assert_eq!(
            dispatch_test_input(&automation, "focus-card", &TestInput::Focus, window, cx,),
            Ok(HookOutcome::Handled)
        );
        assert_eq!(
            dispatch_test_input(
                &automation,
                &disclosure_control,
                &TestInput::Click {
                    button: MouseButton::Left,
                    count: 1,
                },
                window,
                cx,
            ),
            Ok(HookOutcome::Handled)
        );
    });
    view.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();

    let updated = automation.snapshot();
    assert!(updated.generation > initial.generation);
    assert_eq!(updated.nodes["dropdown"].state.expanded, Some(true));
    assert!(updated.nodes.contains_key("menu"));
    assert!(updated.nodes["focus-card"].state.focused);
}

#[gpui::test]
fn disclosure_only_toggles_from_its_summary(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let Some(ui) = expect_ok(
        HtmlUi::compile_with_stylesheet(
            "<details id='folder' open><summary id='folder-summary'>src</summary><button id='file'>main.rs</button></details>",
            BindingDocument::new(),
            "disclosure.css",
            "body { width: 300px; height: 200px; } details, summary, button { display: flex; width: 200px; height: 32px; }",
        ),
        "disclosure fixture should compile",
    ) else {
        return;
    };
    let automation = Automation::for_test();
    let Some(live) = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "disclosure fixture should connect",
    ) else {
        return;
    };
    let (_view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();

    let initial = automation.snapshot();
    assert_eq!(initial.nodes["folder"].state.expanded, Some(true));
    let file = initial.nodes["file"].bounds.unwrap_or_default();
    visual.simulate_click(
        point(
            px(file.x + file.width / 2.0),
            px(file.y + file.height / 2.0),
        ),
        Modifiers::default(),
    );
    visual.run_until_parked();

    let after_file_click = automation.snapshot();
    assert_eq!(after_file_click.nodes["folder"].state.expanded, Some(true));
    assert!(after_file_click.nodes.contains_key("file"));

    let summary = after_file_click.nodes["folder-summary"]
        .bounds
        .unwrap_or_default();
    visual.simulate_click(
        point(
            px(summary.x + summary.width / 2.0),
            px(summary.y + summary.height / 2.0),
        ),
        Modifiers::default(),
    );
    visual.run_until_parked();

    let collapsed = automation.snapshot();
    assert_eq!(collapsed.nodes["folder"].state.expanded, Some(false));
    assert!(!collapsed.nodes.contains_key("file"));
}

#[gpui::test]
fn overflow_elements_expose_and_handle_semantic_scroll(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let Some(ui) = expect_ok(
        HtmlUi::compile_with_stylesheet(
            SCROLL_HTML,
            BindingDocument::new(),
            "scroll.css",
            SCROLL_CSS,
        ),
        "scroll fixture should compile",
    ) else {
        return;
    };
    let automation = Automation::for_test();
    let Some(live) = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "scroll fixture should connect",
    ) else {
        return;
    };
    let (view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();

    let initial = automation.snapshot();
    assert!(
        initial.nodes["scroller"]
            .actions
            .contains(&NodeAction::Scroll)
    );
    let initial_bottom = initial.nodes["scroll-bottom"].bounds.unwrap_or_default();
    assert!(initial.nodes["scroll-bottom"].bounds.is_some());
    let initial_bottom_y = initial_bottom.y;

    visual.update(|window, cx| {
        assert_eq!(
            dispatch_test_input(
                &automation,
                "scroller",
                &TestInput::Scroll {
                    delta_x: 0.0,
                    delta_y: 80.0,
                },
                window,
                cx,
            ),
            Ok(HookOutcome::Handled)
        );
    });
    view.update(visual, |_, cx| cx.notify());
    visual.run_until_parked();

    let updated = automation.snapshot();
    let updated_bottom = updated.nodes["scroll-bottom"].bounds.unwrap_or_default();
    assert!(updated.nodes["scroll-bottom"].bounds.is_some());
    let updated_bottom_y = updated_bottom.y;
    assert!((updated_bottom_y - (initial_bottom_y - 80.0)).abs() < f32::EPSILON);
}

#[gpui::test]
fn percentage_height_and_flex_content_track_window_resizes(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let Some(ui) = expect_ok(
        HtmlUi::compile_with_stylesheet(
            RESPONSIVE_HEIGHT_HTML,
            BindingDocument::new(),
            "responsive-height.css",
            RESPONSIVE_HEIGHT_CSS,
        ),
        "responsive height fixture should compile",
    ) else {
        return;
    };
    assert!(ui.diagnostics().is_empty(), "{:?}", ui.diagnostics());
    let automation = Automation::for_test();
    let Some(live) = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "responsive height fixture should connect",
    ) else {
        return;
    };
    let (view, visual) = cx.add_window_view(|_, _| RuntimeView { live });

    for height in [600.0, 420.0, 760.0] {
        visual.simulate_resize(size(px(800.), px(height)));
        view.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        let tree = automation.snapshot();
        let bounds = |id: &str| tree.nodes.get(id).and_then(|node| node.bounds);

        assert_eq!(bounds("html-root").map(|rect| rect.height), Some(height));
        assert_eq!(
            bounds("responsive-shell").map(|rect| rect.height),
            Some(height)
        );
        assert_eq!(
            bounds("flex-content").map(|rect| rect.height),
            Some(height - 60.0)
        );
        assert_eq!(
            bounds("fixed-footer").map(|rect| rect.y),
            Some(height - 20.0)
        );
    }
}

#[gpui::test]
fn embedded_component_height_tracks_its_flex_host(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let automation = Automation::for_test();
    let Some(inner_ui) = expect_ok(
        HtmlUi::compile_with_stylesheet(
            INNER_HEIGHT_HTML,
            BindingDocument::new(),
            "inner-height.css",
            INNER_HEIGHT_CSS,
        ),
        "inner height fixture should compile",
    ) else {
        return;
    };
    let Some(namespace) = expect_ok(
        SemanticNamespace::new("embedded-project"),
        "semantic namespace should validate",
    ) else {
        return;
    };
    let Some(inner) = expect_ok(
        LiveHtml::new(inner_ui, automation.clone(), HookRegistry::new()),
        "inner height fixture should connect",
    )
    .map(|live| Rc::new(live.embedded(namespace))) else {
        return;
    };
    let mut components = ComponentRegistry::new();
    let Some(()) = expect_ok(
        components.register("studio-canvas", move |_, _, window, cx| {
            inner.render(window, cx)
        }),
        "embedded canvas should register",
    ) else {
        return;
    };
    let Some(outer_ui) = expect_ok(
        HtmlUi::compile_with_stylesheet(
            EMBEDDED_HEIGHT_HTML,
            BindingDocument::new(),
            "embedded-height.css",
            EMBEDDED_HEIGHT_CSS,
        ),
        "embedded height fixture should compile",
    ) else {
        return;
    };
    let Some(outer) = expect_ok(
        LiveHtml::new(outer_ui, automation.clone(), HookRegistry::new()),
        "embedded height shell should connect",
    )
    .map(|live| live.with_components(components)) else {
        return;
    };
    let (view, visual) = cx.add_window_view(|_, _| RuntimeView { live: outer });

    for height in [600.0, 420.0, 760.0] {
        visual.simulate_resize(size(px(800.), px(height)));
        view.update(visual, |_, cx| cx.notify());
        visual.run_until_parked();
        let tree = automation.snapshot();
        let bounds = |id: &str| tree.nodes.get(id).and_then(|node| node.bounds);
        let expected_canvas_height = height - 60.0;

        assert_eq!(
            bounds("embedded-shell").map(|rect| rect.height),
            Some(height)
        );
        assert_eq!(
            bounds("embedded-canvas").map(|rect| rect.height),
            Some(expected_canvas_height)
        );
        assert_eq!(
            bounds("embedded-project--html-root").map(|rect| rect.height),
            Some(expected_canvas_height)
        );
        assert_eq!(
            bounds("embedded-project--inner-shell").map(|rect| rect.height),
            Some(expected_canvas_height)
        );
    }
}

fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < 0.5,
        "{what}: expected {expected}, got {actual}"
    );
}

#[gpui::test]
fn css_grid_track_lists_lay_out_like_css(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let html = r#"<main id="page">
  <section id="layout">
    <div id="sidebar"></div><div id="content"></div><div id="aside"></div>
    <div id="footer"></div>
  </section>
  <section id="cards">
    <div id="card-0" class="card"></div><div id="card-1" class="card"></div>
    <div id="card-2" class="card"></div><div id="card-3" class="card"></div>
    <div id="card-4" class="card"></div>
  </section>
</main>"#;
    let css = "body { width: 600px; height: 400px; }
#page { display: flex; flex-direction: column; }
#layout {
  display: grid;
  width: 400px;
  grid-template-columns: 100px 1fr 2fr;
  grid-template-rows: 30px;
  grid-auto-rows: 15px;
}
#footer { grid-column: 2 / span 2; }
#cards { display: grid; width: 200px; grid-template-columns: repeat(auto-fill, 50px); row-gap: 5px; }
.card { height: 10px; }";
    let Some(ui) = expect_ok(
        HtmlUi::compile_with_stylesheet(html, BindingDocument::new(), "grid.css", css),
        "grid fixture should compile",
    ) else {
        return;
    };
    let automation = Automation::for_test();
    let Some(live) = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "grid fixture should connect",
    ) else {
        return;
    };
    assert!(live.diagnostics().is_empty(), "{:?}", live.diagnostics());
    let (_view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();

    let tree = automation.snapshot();
    let bounds = |id: &str| tree.nodes[id].bounds.unwrap_or_default();
    let origin = bounds("layout");
    let column = |id: &str| (bounds(id).x - origin.x, bounds(id).width);
    assert_eq!(column("sidebar"), (0.0, 100.0));
    assert_eq!(column("content"), (100.0, 100.0));
    assert_eq!(column("aside"), (200.0, 200.0));
    assert_close(bounds("sidebar").height, 30.0, "explicit row height");
    // The spanning footer lands in an implicit row sized by grid-auto-rows.
    assert_eq!(column("footer"), (100.0, 300.0));
    assert_close(bounds("footer").height, 15.0, "implicit row height");
    assert_close(bounds("footer").y - origin.y, 30.0, "implicit row offset");

    // repeat(auto-fill, 50px) in 200px makes four columns, so the fifth card wraps.
    let first = bounds("card-0");
    assert_close(
        bounds("card-3").x - first.x,
        150.0,
        "fourth auto-fill column",
    );
    assert_close(bounds("card-4").x, first.x, "wrapped card column");
    assert_close(
        bounds("card-4").y - first.y,
        15.0,
        "one 10px row plus a 5px row gap",
    );
}

#[gpui::test]
fn rendered_elements_map_back_to_their_markup(cx: &mut TestAppContext) {
    cx.update(gpui_mcp_html::init);
    let html = "<main id=\"app\">\n  <p>Intro</p>\n  <button id=\"go\">Go</button>\n</main>";
    let Some(ui) = expect_ok(
        HtmlUi::compile(html, BindingDocument::new()),
        "source map fixture should compile",
    ) else {
        return;
    };
    let automation = Automation::for_test();
    let Some(live) = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "source map fixture should connect",
    ) else {
        return;
    };
    let Some(namespace) = expect_ok(SemanticNamespace::new("editor"), "valid namespace") else {
        return;
    };
    let live = live.embedded(namespace);
    let map = live.source_map();
    let (_view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();
    let tree = automation.snapshot();

    let markup = |node: &gpui_mcp_html::SourceNode| {
        node.span
            .clone()
            .map(|span| &html[span])
            .unwrap_or_default()
    };
    let by_tag = |tag: &str| map.nodes().iter().find(|node| node.tag == tag);

    // The paragraph has no authored id, yet maps to its markup through a generated one.
    let paragraph = by_tag("p");
    assert_eq!(paragraph.map(markup), Some("<p>Intro</p>"));
    assert_eq!(paragraph.and_then(|p| p.authored_id.clone()), None);
    assert_eq!(
        paragraph.map(|p| (p.line, p.column)),
        Some((Some(2), Some(3)))
    );
    assert!(paragraph.is_some_and(|p| p.element_id.as_str().starts_with("html-node-")));

    let button = by_tag("button");
    assert_eq!(button.map(|b| b.semantic_id.as_str()), Some("editor--go"));
    assert_eq!(
        button.and_then(|b| map.get(&b.semantic_id)).map(markup),
        Some("<button id=\"go\">Go</button>")
    );
    assert_eq!(
        button.and_then(|b| b.parent.as_deref()),
        by_tag("main").map(|main| main.semantic_id.as_str())
    );

    // Every mapped element is in the live tree under its semantic id, with its span.
    for node in map.nodes() {
        let rendered = tree.nodes.get(&node.semantic_id);
        assert!(rendered.is_some(), "{} was not rendered", node.semantic_id);
        let span = node
            .span
            .clone()
            .map(|span| format!("{}..{}", span.start, span.end));
        assert_eq!(
            rendered.and_then(|rendered| rendered.metadata.get("source_span").cloned()),
            span,
            "{}",
            node.semantic_id
        );
    }

    // An offset inside the button's text resolves to the button, not <main>.
    let offset = html.find("Go<").unwrap_or_default();
    assert_eq!(
        map.at_offset(offset).map(|node| node.tag.as_str()),
        Some("button")
    );
}

/// Draw the frame GPUI's animation loop would draw next, `advance` later.
fn next_frame(visual: &mut gpui::VisualTestContext, advance: std::time::Duration) {
    visual.executor().advance_clock(advance);
    visual.update(|window, cx| {
        window.simulate_next_frame(cx);
    });
    visual.run_until_parked();
}

fn mount<'a>(
    html: &str,
    css: &str,
    cx: &'a mut TestAppContext,
) -> Option<(Automation, &'a mut gpui::VisualTestContext)> {
    cx.update(gpui_mcp_html::init);
    let ui = expect_ok(
        HtmlUi::compile_with_stylesheet(html, BindingDocument::new(), "motion.css", css),
        "motion fixture should compile",
    )?;
    let automation = Automation::for_test();
    let live = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "motion fixture should connect",
    )?;
    let (_view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();
    Some((automation, visual))
}

fn bounds_of(automation: &Automation, id: &str) -> gpui_mcp::Rect {
    automation.snapshot().nodes[id].bounds.unwrap_or_default()
}

#[gpui::test]
fn css_transitions_interpolate_interaction_changes(cx: &mut TestAppContext) {
    let css = "body { width: 600px; height: 400px; }
.box { width: 100px; height: 20px; transition: width 1s linear, translate 1s linear; }
.box:hover { width: 200px; translate: 100px 0; }";
    let Some((automation, visual)) = mount(
        r#"<main id="page"><div id="box" class="box">Box</div></main>"#,
        css,
        cx,
    ) else {
        return;
    };
    let start = bounds_of(&automation, "box");
    assert_close(start.width, 100.0, "starting width");

    let ms = std::time::Duration::from_millis;
    visual.simulate_mouse_move(
        point(px(start.x + 10.0), px(start.y + 10.0)),
        None,
        Modifiers::default(),
    );
    visual.run_until_parked();
    next_frame(visual, ms(500));
    let half = bounds_of(&automation, "box");
    assert_close(half.width, 150.0, "width half-way through the transition");
    assert_close(
        half.x - start.x,
        50.0,
        "translate half-way, without moving layout",
    );

    // Leaving half-way reverses from the value on screen.
    visual.simulate_mouse_move(point(px(590.0), px(390.0)), None, Modifiers::default());
    visual.run_until_parked();
    next_frame(visual, ms(250));
    assert_close(
        bounds_of(&automation, "box").width,
        137.5,
        "a reversed transition starts from where it was",
    );
    next_frame(visual, ms(2000));
    let settled = bounds_of(&automation, "box");
    assert_close(settled.width, 100.0, "settled width");
    assert_close(settled.x, start.x, "settled position");
}

#[gpui::test]
fn starting_style_and_keyframes_animate_on_first_render(cx: &mut TestAppContext) {
    let css = "body { width: 600px; height: 400px; }
#page { display: flex; flex-direction: column; }
.grow { width: 100px; height: 10px; transition: width 1s linear; }
@starting-style { .grow { width: 0px; } }
@keyframes stretch { from { width: 0px; } to { width: 200px; } }
.pulse { width: 100px; height: 10px; animation: stretch 1s linear; }";
    let Some((automation, visual)) = mount(
        r#"<main id="page"><div id="grow" class="grow"></div><div id="pulse" class="pulse"></div></main>"#,
        css,
        cx,
    ) else {
        return;
    };
    assert_close(
        bounds_of(&automation, "grow").width,
        0.0,
        "entry starts from @starting-style",
    );
    assert_close(
        bounds_of(&automation, "pulse").width,
        0.0,
        "animation starts at its first keyframe",
    );

    let ms = std::time::Duration::from_millis;
    next_frame(visual, ms(250));
    assert_close(
        bounds_of(&automation, "grow").width,
        25.0,
        "entry transition",
    );
    assert_close(bounds_of(&automation, "pulse").width, 50.0, "keyframes");
    next_frame(visual, ms(1000));
    assert_close(
        bounds_of(&automation, "grow").width,
        100.0,
        "entry finished",
    );
    assert_close(
        bounds_of(&automation, "pulse").width,
        100.0,
        "without fill-mode the animation releases the element's own value",
    );
}

fn compile_motion(html: &str, css: &str) -> Option<HtmlUi> {
    expect_ok(
        HtmlUi::compile_with_stylesheet(html, BindingDocument::new(), "motion.css", css),
        "view-transition fixture should compile",
    )
}

fn mount_view<'a>(
    html: &str,
    css: &str,
    cx: &'a mut TestAppContext,
) -> Option<(
    Automation,
    gpui::Entity<RuntimeView>,
    &'a mut gpui::VisualTestContext,
)> {
    cx.update(gpui_mcp_html::init);
    let ui = compile_motion(html, css)?;
    let automation = Automation::for_test();
    let live = expect_ok(
        LiveHtml::new(ui, automation.clone(), HookRegistry::new()),
        "view-transition fixture should connect",
    )?;
    let (view, visual) = cx.add_window_view(|_, _| RuntimeView { live });
    visual.run_until_parked();
    Some((automation, view, visual))
}

const HERO_HTML: &str = r#"<main id="page"><div id="hero" class="hero">Hero</div></main>"#;

fn hero_css(navigation: bool, margin: f32, group: &str) -> String {
    let navigation = if navigation {
        "@view-transition { navigation: auto; }"
    } else {
        ""
    };
    format!(
        "{navigation}
body {{ width: 600px; height: 400px; }}
.hero {{ view-transition-name: hero; width: 100px; height: 20px; margin-left: {margin}px; }}
::view-transition-group(hero) {{ {group} }}"
    )
}

#[gpui::test]
fn navigation_view_transitions_move_named_elements(cx: &mut TestAppContext) {
    let linear = "animation-duration: 1s; animation-timing-function: linear;";
    let Some((automation, view, visual)) = mount_view(HERO_HTML, &hero_css(true, 0.0, linear), cx)
    else {
        return;
    };
    let start = bounds_of(&automation, "hero");
    let Some(next) = compile_motion(HERO_HTML, &hero_css(true, 200.0, linear)) else {
        return;
    };
    view.update(visual, |view, cx| {
        assert!(view.live.reload(next).is_ok());
        cx.notify();
    });
    visual.run_until_parked();
    assert_close(
        bounds_of(&automation, "hero").x,
        start.x,
        "the group starts at the old box",
    );
    assert!(
        automation
            .snapshot()
            .nodes
            .contains_key("html-view-transition"),
        "the old image is drawn while the transition runs"
    );

    let ms = std::time::Duration::from_millis;
    next_frame(visual, ms(500));
    assert_close(
        bounds_of(&automation, "hero").x - start.x,
        100.0,
        "half-way along the group's path",
    );
    next_frame(visual, ms(600));
    next_frame(visual, ms(16));
    assert_close(
        bounds_of(&automation, "hero").x - start.x,
        200.0,
        "the new box",
    );
    assert!(!view.read_with(visual, |view, _| view.live.view_transition_running()));
    assert!(
        !automation
            .snapshot()
            .nodes
            .contains_key("html-view-transition"),
        "the old image is gone once the transition ends"
    );

    // Both documents must opt in for a swap to transition.
    let Some(plain) = compile_motion(HERO_HTML, &hero_css(false, 0.0, linear)) else {
        return;
    };
    view.update(visual, |view, cx| {
        assert!(view.live.reload(plain).is_ok());
        assert!(!view.live.view_transition_running());
        cx.notify();
    });
    visual.run_until_parked();
    assert_close(bounds_of(&automation, "hero").x, start.x, "no transition");
}

#[gpui::test]
fn same_document_view_transitions_activate_their_types(cx: &mut TestAppContext) {
    let css = "body { width: 600px; height: 400px; }
#page { display: flex; flex-direction: column; }
.hero { view-transition-name: hero; width: 100px; height: 20px; }
.badge { width: 40px; height: 10px; }
main:active-view-transition-type(slide) .badge { width: 80px; }
::view-transition-group(*) { animation: none; }
::view-transition-old(root) { animation: 400ms linear both fade; }
@keyframes fade { to { opacity: 0; } }";
    let html = r#"<main id="page"><div id="hero" class="hero">Hero</div><div id="badge" class="badge"></div></main>"#;
    let Some((automation, view, visual)) = mount_view(html, css, cx) else {
        return;
    };
    let hero = bounds_of(&automation, "hero");
    let moved = html.replace(
        r#"<div id="hero" class="hero">Hero</div><div id="badge" class="badge"></div>"#,
        r#"<div id="badge" class="badge"></div><div id="hero" class="hero">Hero</div>"#,
    );
    let Some(next) = compile_motion(&moved, css) else {
        return;
    };
    view.update_in(visual, |view, window, cx| {
        assert!(view.live.start_view_transition(["slide"], window, cx));
        // A document swap while the transition is pending becomes its new state.
        assert!(view.live.reload(next).is_ok());
        cx.notify();
    });
    visual.run_until_parked();
    assert_close(
        bounds_of(&automation, "badge").width,
        80.0,
        ":active-view-transition-type() matches while the transition runs",
    );
    assert_close(
        bounds_of(&automation, "hero").y - hero.y,
        10.0,
        "`animation: none` on the group jumps to the new box",
    );

    let ms = std::time::Duration::from_millis;
    next_frame(visual, ms(200));
    assert!(view.read_with(visual, |view, _| view.live.view_transition_running()));
    next_frame(visual, ms(300));
    next_frame(visual, ms(16));
    assert!(!view.read_with(visual, |view, _| view.live.view_transition_running()));
    assert_close(
        bounds_of(&automation, "badge").width,
        40.0,
        "types stop matching when the transition ends",
    );
}

#[gpui::test]
fn duplicate_and_missing_names_do_not_break_transitions(cx: &mut TestAppContext) {
    let css = "body { width: 600px; height: 400px; }
.card { view-transition-name: card; width: 50px; height: 20px; }
.solo { view-transition-name: solo; width: 50px; height: 20px; }";
    let Some((automation, view, visual)) = mount_view(
        r#"<main id="page"><div id="a" class="card"></div><div id="b" class="card"></div><div id="gone" class="solo"></div></main>"#,
        css,
        cx,
    ) else {
        return;
    };
    let Some(next) = compile_motion(
        r#"<main id="page"><div id="a" class="card"></div><div id="fresh" class="hero"></div></main>"#,
        &format!("{css} .hero {{ view-transition-name: fresh; width: 10px; height: 10px; }}"),
    ) else {
        return;
    };
    view.update_in(visual, |view, window, cx| {
        assert!(
            view.live
                .start_view_transition(Vec::<&str>::new(), window, cx)
        );
        assert!(view.live.reload(next).is_ok());
        cx.notify();
    });
    visual.run_until_parked();
    assert!(automation.snapshot().nodes.contains_key("fresh"));
    next_frame(visual, std::time::Duration::from_millis(400));
    next_frame(visual, std::time::Duration::from_millis(16));
    assert!(!view.read_with(visual, |view, _| view.live.view_transition_running()));
    assert_close(bounds_of(&automation, "a").width, 50.0, "settled");
}
