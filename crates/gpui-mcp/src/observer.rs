use std::sync::{Arc, Weak};

use gpui::{
    App, BorderStyle, FrameObserver, SemanticAction, SemanticFrame, SemanticRole, Window, outline,
    point, px, rgba, size,
};
use gpui_mcp_protocol::{NodeAction, NodeState, Role, TextInfo, TextRange, UiNode, ValueInfo};

use crate::registry::{SharedState, rect_from_gpui};

pub(crate) struct BridgeObserver {
    state: Weak<SharedState>,
}

impl BridgeObserver {
    pub(crate) fn new(state: &Arc<SharedState>) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::downgrade(state),
        })
    }
}

impl FrameObserver for BridgeObserver {
    fn frame_started(&self, window: &Window) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state.begin_frame();
        let mut content_bounds = window.bounds();
        content_bounds.size = window.viewport_size();
        state.set_window_geometry(rect_from_gpui(content_bounds), window.scale_factor());
    }

    fn semantics_updated(&self, frame: &SemanticFrame) {
        if let Some(state) = self.state.upgrade() {
            state.publish_frame(frame.nodes().iter().map(to_node));
        }
    }

    fn paint_started(&self) {
        if let Some(state) = self.state.upgrade() {
            state.begin_root_paint();
        }
    }

    fn paint_overlay(&self, window: &mut Window, _cx: &mut App) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        for highlight in state.highlights() {
            let Some(color) = parse_color(&highlight.color) else {
                continue;
            };
            let rect = highlight.rect;
            window.paint_quad(outline(
                gpui::Bounds::new(
                    point(px(rect.x), px(rect.y)),
                    size(px(rect.width), px(rect.height)),
                ),
                rgba(color),
                BorderStyle::Solid,
            ));
        }
    }

    fn frame_finished(&self) {
        if let Some(state) = self.state.upgrade() {
            state.finish_root_paint();
        }
    }
}

fn to_node(node: &gpui::SemanticNode) -> UiNode {
    UiNode {
        id: node.id.as_str().to_owned(),
        parent: node
            .parent
            .as_ref()
            .map(|parent| parent.as_str().to_owned()),
        children: Vec::new(),
        role: role(node.semantics.role),
        label: node.semantics.label.clone(),
        description: node.semantics.description.clone(),
        bounds: Some(rect_from_gpui(node.bounds)),
        state: NodeState {
            visible: node.semantics.state.visible,
            enabled: node.semantics.state.enabled,
            focused: node.semantics.state.focused,
            checked: node.semantics.state.checked,
            selected: node.semantics.state.selected,
            expanded: node.semantics.state.expanded,
        },
        actions: node.actions.iter().copied().map(action).collect(),
        text: node.semantics.text.as_ref().map(|text| TextInfo {
            text: text.text.clone(),
            caret: text.caret,
            selection: text.selection.as_ref().map(|selection| TextRange {
                start: selection.start,
                end: selection.end,
            }),
            redacted: text.redacted,
        }),
        value: node.semantics.value.as_ref().map(|value| ValueInfo {
            value: value.value.clone(),
            min: value.min,
            max: value.max,
            step: value.step,
        }),
        metadata: node.semantics.metadata.clone(),
    }
}

const fn role(role: SemanticRole) -> Role {
    match role {
        SemanticRole::Generic => Role::Generic,
        SemanticRole::Application => Role::Application,
        SemanticRole::Text => Role::Text,
        SemanticRole::Button => Role::Button,
        SemanticRole::TextInput => Role::TextInput,
        SemanticRole::SearchInput => Role::SearchInput,
        SemanticRole::Checkbox => Role::Checkbox,
        SemanticRole::Radio => Role::Radio,
        SemanticRole::Switch => Role::Switch,
        SemanticRole::Combobox => Role::Combobox,
        SemanticRole::Slider => Role::Slider,
        SemanticRole::Progress => Role::Progress,
        SemanticRole::Link => Role::Link,
        SemanticRole::Image => Role::Image,
        SemanticRole::Group => Role::Group,
        SemanticRole::List => Role::List,
        SemanticRole::ListItem => Role::ListItem,
        SemanticRole::Table => Role::Table,
        SemanticRole::Tree => Role::Tree,
        SemanticRole::TreeItem => Role::TreeItem,
        SemanticRole::Menu => Role::Menu,
        SemanticRole::MenuItem => Role::MenuItem,
        SemanticRole::Option => Role::Option,
        SemanticRole::Separator => Role::Separator,
        SemanticRole::Tooltip => Role::Tooltip,
        SemanticRole::TabList => Role::TabList,
        SemanticRole::Tab => Role::Tab,
        SemanticRole::Toolbar => Role::Toolbar,
        SemanticRole::Dialog => Role::Dialog,
        SemanticRole::Alert => Role::Alert,
        SemanticRole::ScrollArea => Role::ScrollArea,
    }
}

const fn action(action: SemanticAction) -> NodeAction {
    match action {
        SemanticAction::Click => NodeAction::Click,
        SemanticAction::Focus => NodeAction::Focus,
        SemanticAction::Hover => NodeAction::Hover,
        SemanticAction::Drag => NodeAction::Drag,
        SemanticAction::SetText => NodeAction::SetText,
        SemanticAction::SetValue => NodeAction::SetValue,
        SemanticAction::Scroll => NodeAction::Scroll,
    }
}

fn parse_color(color: &str) -> Option<u32> {
    let value = color.strip_prefix('#')?;
    (value.len() == 8)
        .then(|| u32::from_str_radix(value, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        Context, InteractiveElement as _, IntoElement, ParentElement as _, Render,
        SemanticElementExt as _, SemanticRole, StatefulInteractiveElement as _, Styled as _,
        StyledText, TestAppContext, Window, div, px,
    };
    use gpui_mcp_protocol::{MouseButton, NodeAction, Point, PointerCommand, Role};

    use crate::{Automation, input::dispatch_pointer};

    struct SemanticFixture {
        clicked: Rc<Cell<bool>>,
    }

    impl Render for SemanticFixture {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("root")
                .semantic_role(SemanticRole::Application)
                .size_full()
                .child(
                    div()
                        .id("save")
                        .w(px(100.0))
                        .h(px(40.0))
                        .on_click({
                            let clicked = self.clicked.clone();
                            move |_, _, _| clicked.set(true)
                        })
                        .child(div().id("save-label").child(StyledText::new("Save"))),
                )
                .child(
                    div()
                        .id("hover-target")
                        .on_mouse_move(|_, _, _| {})
                        .child("Hover target"),
                )
                .child(div().id("status").child("Ready"))
        }
    }

    #[gpui::test]
    fn observes_real_gpui_ids_text_hierarchy_and_handlers(cx: &mut TestAppContext) {
        let automation = Automation::isolated();
        let clicked = Rc::new(Cell::new(false));
        let automation_for_window = automation.clone();
        let clicked_by_handler = clicked.clone();
        let (_view, visual) = cx.add_window_view(move |window, _| {
            automation_for_window.attach(window);
            SemanticFixture {
                clicked: clicked_by_handler,
            }
        });
        visual.run_until_parked();

        let tree = automation.snapshot();
        assert_eq!(tree.roots, ["root"]);
        assert_eq!(tree.nodes["root"].role, Role::Application);
        assert_eq!(tree.nodes["save"].parent.as_deref(), Some("root"));
        assert_eq!(tree.nodes["save-label"].parent.as_deref(), Some("save"));
        assert_eq!(tree.nodes["save"].role, Role::Button);
        assert_eq!(tree.nodes["save"].label.as_deref(), Some("Save"));
        assert!(tree.nodes["save"].actions.contains(&NodeAction::Click));
        assert!(
            tree.nodes["hover-target"]
                .actions
                .contains(&NodeAction::Hover)
        );
        assert_eq!(
            tree.nodes["status"]
                .text
                .as_ref()
                .map(|text| text.text.as_str()),
            Some("Ready")
        );

        assert!(tree.nodes["save"].bounds.is_some());
        let save = tree.nodes["save"].bounds.unwrap_or_default();
        visual.update(|window, cx| {
            let point = save.center();
            assert_eq!(
                dispatch_pointer(
                    &PointerCommand::MouseDown {
                        point: Point {
                            x: point.x,
                            y: point.y,
                        },
                        button: MouseButton::Left,
                        click_count: 1,
                    },
                    window,
                    cx,
                ),
                Ok(())
            );
            assert_eq!(
                dispatch_pointer(
                    &PointerCommand::MouseUp {
                        point: Point {
                            x: point.x,
                            y: point.y,
                        },
                        button: MouseButton::Left,
                        click_count: 1,
                    },
                    window,
                    cx,
                ),
                Ok(())
            );
        });
        assert!(clicked.get());
    }
}
