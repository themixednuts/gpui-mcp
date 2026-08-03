//! End-to-end pure HTML rendering through a real GPUI test window and MCP semantics.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{App, Context, IntoElement, Render, Styled as _, TestAppContext, Window, div, px, size};
use gpui_mcp::{
    ActionOutcome, Automation, MouseButton, NodeAction, Role, SemanticAction, UiTree, ValueInfo,
};
use gpui_mcp_html::{
    Binding, BindingDocument, BindingMode, BindingTarget, ComponentRegistry, ElementId, HandlerId,
    HookRegistry, HtmlUi, LiveHtml, SemanticNamespace, StateBindingId, StateValue, UiEvent,
    UiProperty,
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
        <span id="status">fallback status</span>
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
                    return ActionOutcome::Rejected {
                        reason: "title requires text".to_owned(),
                    };
                };
                *title_writer.borrow_mut() = value;
                window.refresh();
                ActionOutcome::Handled
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
            ActionOutcome::Handled
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
            ActionOutcome::Handled
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
                    return ActionOutcome::Rejected {
                        reason: "published requires a boolean".to_owned(),
                    };
                };
                published_writer.set(value);
                window.refresh();
                ActionOutcome::Handled
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
    assert!(
        tree.nodes["published"]
            .actions
            .contains(&NodeAction::SetValue)
    );
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

fn dispatch_actions(automation: &Automation, generation: u64, window: &mut Window, cx: &mut App) {
    let click = SemanticAction::Click {
        button: MouseButton::Left,
        count: 1,
    };
    let invalid_published = SemanticAction::SetValue {
        value: "yes".to_owned(),
    };
    let publish = SemanticAction::SetValue {
        value: "true".to_owned(),
    };
    let rename = SemanticAction::SetText {
        text: "Published title".to_owned(),
    };
    let mut dispatch =
        |node_id, action| automation.dispatch_test_action(node_id, generation, action, window, cx);
    assert_eq!(dispatch("save", &click), Ok(ActionOutcome::Handled));
    assert_eq!(
        dispatch(
            "save",
            &SemanticAction::Click {
                button: MouseButton::Left,
                count: 2,
            }
        ),
        Ok(ActionOutcome::Handled)
    );
    assert_eq!(
        dispatch("published", &invalid_published),
        Ok(ActionOutcome::Rejected {
            reason: "checked and selected values accept only `true` or `false`".to_owned(),
        })
    );
    assert_eq!(dispatch("published", &publish), Ok(ActionOutcome::Handled));
    assert_eq!(dispatch("title", &rename), Ok(ActionOutcome::Handled));
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
fn html_renders_to_gpui_and_round_trips_semantic_actions(cx: &mut TestAppContext) {
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
    visual.update(|window, cx| dispatch_actions(&automation, tree.generation, window, cx));
    assert_eq!(&*state.events.borrow(), &["Click:save", "DoubleClick:save"]);
    assert_eq!(&*state.title.borrow(), "Published title");
    assert!(state.published.get());

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
    assert!(
        initial.nodes["dropdown"]
            .actions
            .contains(&NodeAction::Click)
    );
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
            automation.dispatch_test_action(
                "hover-card",
                initial.generation,
                &SemanticAction::Hover,
                window,
                cx,
            ),
            Ok(ActionOutcome::Handled)
        );
        assert_eq!(
            automation.dispatch_test_action(
                "focus-card",
                initial.generation,
                &SemanticAction::Focus,
                window,
                cx,
            ),
            Ok(ActionOutcome::Handled)
        );
        assert_eq!(
            automation.dispatch_test_action(
                "dropdown",
                initial.generation,
                &SemanticAction::Click {
                    button: MouseButton::Left,
                    count: 1,
                },
                window,
                cx,
            ),
            Ok(ActionOutcome::Handled)
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
            automation.dispatch_test_action(
                "scroller",
                initial.generation,
                &SemanticAction::Scroll {
                    delta_x: 0.0,
                    delta_y: 80.0,
                },
                window,
                cx,
            ),
            Ok(ActionOutcome::Handled)
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
