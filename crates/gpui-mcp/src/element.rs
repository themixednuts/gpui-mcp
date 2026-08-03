use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    App, BorderStyle, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window, outline, point, px, rgba, size,
};
use gpui_mcp_protocol::{
    ActionOutcome, MouseButton, NodeAction, NodeState, Point, Role, TextInfo, UiNode, ValueInfo,
};

use crate::Automation;
use crate::registry::rect_from_gpui;

/// Application-authored semantics attached to a GPUI element.
#[derive(Clone)]
pub struct NodeSpec {
    id: String,
    parent: Option<String>,
    role: Role,
    label: Option<String>,
    description: Option<String>,
    state: NodeState,
    actions: Vec<NodeAction>,
    text: Option<TextInfo>,
    value: Option<ValueInfo>,
    metadata: BTreeMap<String, String>,
    root: bool,
    handler: Option<NodeHandler>,
}

pub(crate) type NodeHandler = Rc<dyn Fn(&NodeEvent, &mut Window, &mut App) -> ActionOutcome>;

/// Pointer event routed to an annotated semantic node on GPUI's foreground executor.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeEvent {
    /// Click or double-click activation.
    Click {
        /// Requested mouse button.
        button: MouseButton,
        /// Click count.
        count: u8,
    },
    /// Move keyboard focus to this semantic node.
    Focus,
    /// Pointer hover.
    Hover {
        /// Window-relative point.
        point: Point,
    },
    /// Semantic drag gesture.
    Drag {
        /// Window-relative start point.
        from: Point,
        /// Window-relative end point.
        to: Point,
        /// Requested interpolation steps.
        steps: u8,
    },
    /// One interpolated position of an in-progress semantic drag, delivered
    /// before the final [`NodeEvent::Drag`].
    DragMove {
        /// Window-relative interpolated pointer position.
        at: Point,
        /// Gesture completion in the inclusive range 0.0..=1.0.
        progress: f32,
    },
    /// Semantic scroll gesture.
    Scroll {
        /// Horizontal logical-pixel delta.
        delta_x: f32,
        /// Vertical logical-pixel delta.
        delta_y: f32,
    },
    /// Direct semantic text replacement.
    SetText {
        /// Replacement UTF-8 text.
        text: String,
    },
    /// Direct semantic value replacement.
    SetValue {
        /// Replacement display value.
        value: String,
    },
}

impl NodeSpec {
    /// Create semantics with a stable identifier and role.
    #[must_use]
    pub fn new(id: impl Into<String>, role: Role) -> Self {
        Self {
            id: id.into(),
            parent: None,
            role,
            label: None,
            description: None,
            state: NodeState::default(),
            actions: Vec::new(),
            text: None,
            value: None,
            metadata: BTreeMap::new(),
            root: false,
            handler: None,
        }
    }

    /// Mark this element as the single annotation root for a window.
    #[must_use]
    pub fn root(mut self) -> Self {
        self.root = true;
        self
    }

    /// Override the automatically inferred parent.
    #[must_use]
    pub fn parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Set the accessible label.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set the accessible description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set dynamic state.
    #[must_use]
    pub fn state(mut self, state: NodeState) -> Self {
        self.state = state;
        self
    }

    /// Advertise an accepted action.
    #[must_use]
    pub fn action(mut self, action: NodeAction) -> Self {
        if !self.actions.contains(&action) {
            self.actions.push(action);
        }
        self
    }

    /// Attach non-secret editable text metadata.
    #[must_use]
    pub fn text(mut self, text: TextInfo) -> Self {
        self.text = Some(text);
        self
    }

    /// Attach value metadata.
    #[must_use]
    pub fn value(mut self, value: ValueInfo) -> Self {
        self.value = Some(value);
        self
    }

    /// Attach a small application-defined metadata field.
    #[must_use]
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Register a GPUI-thread handler for pointer automation on this node.
    ///
    /// The closure may capture weak entity handles and update application state. It
    /// never leaves the foreground executor and is discarded at the next root frame.
    #[must_use]
    pub fn on_event(
        mut self,
        handler: impl Fn(&NodeEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handler = Some(Rc::new(move |event, window, cx| {
            handler(event, window, cx);
            ActionOutcome::Handled
        }));
        self
    }

    /// Register a GPUI-thread handler that can explicitly reject an action.
    #[must_use]
    pub fn on_event_result(
        mut self,
        handler: impl Fn(&NodeEvent, &mut Window, &mut App) -> ActionOutcome + 'static,
    ) -> Self {
        self.handler = Some(Rc::new(handler));
        self
    }

    fn to_node(&self, bounds: Bounds<Pixels>) -> UiNode {
        UiNode {
            id: self.id.clone(),
            parent: self.parent.clone(),
            children: Vec::new(),
            role: self.role,
            label: self.label.clone(),
            description: self.description.clone(),
            bounds: Some(rect_from_gpui(bounds)),
            state: self.state.clone(),
            actions: self.actions.clone(),
            text: self.text.clone(),
            value: self.value.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

/// GPUI element wrapper that records actual post-layout bounds and semantics.
pub struct SemanticElement<E: Element> {
    inner: E,
    automation: Automation,
    spec: NodeSpec,
}

impl<E: Element> IntoElement for SemanticElement<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: Element> Element for SemanticElement<E> {
    type RequestLayoutState = E::RequestLayoutState;
    type PrepaintState = E::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        self.inner.id()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.inner.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.inner.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if self.spec.root {
            self.automation.state.begin_frame();
            let mut content_bounds = window.bounds();
            // `Window::bounds` carries the global native-window origin on Windows, but its size
            // includes the non-client frame. Semantic element coordinates are relative to GPUI's
            // drawable viewport, so publish that size and let the capture mapper derive the
            // decoration insets from the captured native image.
            content_bounds.size = window.viewport_size();
            self.automation
                .state
                .set_window_geometry(rect_from_gpui(content_bounds), window.scale_factor());
        }
        let recorded = self.automation.state.record(self.spec.to_node(bounds));
        if recorded {
            if let Some(handler) = self.spec.handler.clone() {
                self.automation
                    .state
                    .record_handler(self.spec.id.clone(), handler);
            }
            self.automation.state.push_parent(&self.spec.id);
        }
        let state = self
            .inner
            .prepaint(id, inspector_id, bounds, request_layout, window, cx);
        if recorded {
            self.automation.state.pop_parent();
        }
        state
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.spec.root {
            self.automation.state.finish_prepaint();
            self.automation.state.finish_frame();
            self.automation.state.begin_root_paint();
        }
        self.inner.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
        if self.spec.root {
            self.automation.state.finish_root_paint();
            for highlight in self.automation.state.highlights() {
                if let Some(color) = parse_color(&highlight.color) {
                    let rect = highlight.rect;
                    window.paint_quad(outline(
                        Bounds::new(
                            point(px(rect.x), px(rect.y)),
                            size(px(rect.width), px(rect.height)),
                        ),
                        rgba(color),
                        BorderStyle::Solid,
                    ));
                }
            }
        }
    }
}

/// Extension trait for annotating any GPUI element.
pub trait McpElementExt: IntoElement + Sized {
    /// Wrap this element with MCP semantics.
    fn mcp_node(self, automation: &Automation, spec: NodeSpec) -> SemanticElement<Self::Element> {
        SemanticElement {
            inner: self.into_element(),
            automation: automation.clone(),
            spec,
        }
    }
}

impl<T: IntoElement> McpElementExt for T {}

fn parse_color(color: &str) -> Option<u32> {
    let value = color.strip_prefix('#')?;
    if value.len() != 8 {
        return None;
    }
    u32::from_str_radix(value, 16).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        Context, IntoElement, ParentElement as _, Render, TestAppContext, Window, deferred, div,
        point, px, size,
    };
    use gpui_mcp_protocol::{ActionOutcome, MouseButton, NodeAction, Role, SemanticAction};

    use super::{McpElementExt as _, NodeEvent, NodeSpec};
    use crate::Automation;
    use crate::registry::SharedState;

    struct DeferredView {
        automation: Automation,
    }

    impl Render for DeferredView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(div().mcp_node(
                    &self.automation,
                    NodeSpec::new("child", Role::Text).label("Child"),
                ))
                .child(deferred(div().mcp_node(
                    &self.automation,
                    NodeSpec::new("overlay", Role::Dialog).label("Overlay"),
                )))
                .mcp_node(
                    &self.automation,
                    NodeSpec::new("root", Role::Application).root(),
                )
        }
    }

    #[gpui::test]
    fn real_gpui_layout_captures_root_children_and_deferred_overlays(cx: &mut TestAppContext) {
        let state = SharedState::new(10);
        let automation = Automation {
            state: state.clone(),
        };
        let (_view, visual) = cx.add_window_view(|_, _| DeferredView { automation });
        visual.run_until_parked();

        let tree = state.tree();
        assert!(tree.nodes.contains_key("root"));
        assert!(tree.nodes.contains_key("child"));
        assert!(tree.nodes.contains_key("overlay"));
        assert_eq!(tree.nodes["child"].parent.as_deref(), Some("root"));
        assert!(tree.nodes.values().all(|node| node.bounds.is_some()));
    }

    #[gpui::test]
    fn generation_checked_semantic_actions_target_exact_ids_and_preserve_focus(
        cx: &mut TestAppContext,
    ) {
        let state = SharedState::new(11);
        let automation = Automation {
            state: state.clone(),
        };
        let events = Rc::new(RefCell::new(Vec::new()));
        let first_events = events.clone();
        let second_events = events.clone();
        let visual = cx.add_empty_window();
        visual.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(200.0)),
            |_, _| {
                div()
                    .child(
                        div().mcp_node(
                            &automation,
                            NodeSpec::new("first", Role::Button)
                                .action(NodeAction::Click)
                                .on_event_result(move |event, _, _| {
                                    if matches!(event, NodeEvent::Click { .. }) {
                                        first_events.borrow_mut().push("first-click");
                                    }
                                    ActionOutcome::Handled
                                }),
                        ),
                    )
                    .child(
                        div().mcp_node(
                            &automation,
                            NodeSpec::new("second", Role::TextInput)
                                .action(NodeAction::Focus)
                                .on_event_result(move |event, _, _| {
                                    if matches!(event, NodeEvent::Focus) {
                                        second_events.borrow_mut().push("second-focus");
                                    }
                                    ActionOutcome::Handled
                                }),
                        ),
                    )
                    .mcp_node(&automation, NodeSpec::new("root", Role::Application).root())
            },
        );

        let generation = state.tree().generation;
        visual.update(|window, cx| {
            let focus =
                state.dispatch_action("second", generation, &SemanticAction::Focus, window, cx);
            assert_eq!(focus, Ok(ActionOutcome::Handled));
            let click = state.dispatch_action(
                "first",
                generation,
                &SemanticAction::Click {
                    button: MouseButton::Left,
                    count: 1,
                },
                window,
                cx,
            );
            assert_eq!(click, Ok(ActionOutcome::Handled));
        });
        assert_eq!(&*events.borrow(), &["second-focus", "first-click"]);

        visual.draw(
            point(px(0.0), px(0.0)),
            size(px(300.0), px(200.0)),
            |_, _| {
                div().mcp_node(
                    &automation,
                    NodeSpec::new("changed", Role::Application).root(),
                )
            },
        );
        visual.update(|window, cx| {
            let stale_result = state.dispatch_action(
                "first",
                generation,
                &SemanticAction::Click {
                    button: MouseButton::Left,
                    count: 1,
                },
                window,
                cx,
            );
            assert!(stale_result.is_err());
        });
    }
}
