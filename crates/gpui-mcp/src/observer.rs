use std::sync::{Arc, Weak};

use gpui::{
    AccessibilityFrame, App, BorderStyle, FrameAction, FrameNode, FrameObserver, Window,
    accesskit::{self, Action, Role as AccessibleRole, Toggled},
    outline, point, px, rgba, size,
};
use gpui_mcp_protocol::{NodeAction, NodeState, Role, TextInfo, UiNode, ValueInfo};

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

    fn accessibility_updated(&self, frame: &AccessibilityFrame) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut nodes = frame.nodes().map(|(_, node)| node).collect::<Vec<_>>();
        nodes.sort_by(|left, right| left.path().cmp(right.path()));
        state.publish_frame(nodes.into_iter().map(|node| to_node(frame, node)));
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

fn to_node(frame: &AccessibilityFrame, rendered: &FrameNode) -> UiNode {
    let accessible = frame.accessibility_node(rendered);
    let accessible_role =
        accessible.map_or_else(|| rendered.fallback_role(), accesskit::Node::role);
    let focused = frame.tree().focus == rendered.accessibility_id();
    let mut metadata = rendered.metadata().clone();
    if rendered.path() != rendered.id() {
        metadata.insert("gpui_path".to_owned(), rendered.path().to_owned());
    }

    let value = accessible.and_then(accesskit::Node::value);
    let text_value = if rendered.is_redacted() {
        Some(String::new())
    } else {
        value.map(ToOwned::to_owned).or_else(|| {
            (!rendered.content_text().is_empty()).then(|| rendered.content_text().to_owned())
        })
    };
    let text = is_text_role(accessible_role).then(|| TextInfo {
        text: text_value.clone().unwrap_or_default(),
        caret: None,
        selection: None,
        redacted: rendered.is_redacted(),
    });
    let numeric_value = accessible.and_then(accesskit::Node::numeric_value);
    let value = (numeric_value.is_some() || value.is_some()).then(|| ValueInfo {
        value: numeric_value
            .map(|value| value.to_string())
            .or(text_value)
            .unwrap_or_default(),
        min: accessible.and_then(accesskit::Node::min_numeric_value),
        max: accessible.and_then(accesskit::Node::max_numeric_value),
        step: accessible.and_then(accesskit::Node::numeric_value_step),
    });

    UiNode {
        id: rendered.id().to_owned(),
        parent: rendered.parent().map(ToOwned::to_owned),
        children: Vec::new(),
        role: role(accessible_role),
        label: accessible
            .and_then(accesskit::Node::label)
            .map(ToOwned::to_owned)
            .or_else(|| label_from_content(accessible_role, rendered.content_text())),
        description: accessible
            .and_then(accesskit::Node::description)
            .map(ToOwned::to_owned),
        bounds: Some(rect_from_gpui(rendered.bounds())),
        state: NodeState {
            visible: !rendered.bounds().is_empty()
                && !accessible.is_some_and(accesskit::Node::is_hidden),
            enabled: !accessible.is_some_and(accesskit::Node::is_disabled),
            focused,
            checked: accessible.and_then(|node| match node.toggled() {
                Some(Toggled::True) => Some(true),
                Some(Toggled::False) => Some(false),
                Some(Toggled::Mixed) | None => None,
            }),
            selected: accessible.and_then(accesskit::Node::is_selected),
            expanded: accessible.and_then(accesskit::Node::is_expanded),
        },
        actions: actions(accessible, rendered),
        text,
        value,
        metadata,
    }
}

fn actions(accessible: Option<&accesskit::Node>, rendered: &FrameNode) -> Vec<NodeAction> {
    let mut actions = Vec::new();
    let supports = |action| accessible.is_some_and(|node| node.supports_action(action));
    push_action(&mut actions, supports(Action::Click), NodeAction::Click);
    push_action(&mut actions, supports(Action::Focus), NodeAction::Focus);
    push_action(
        &mut actions,
        supports(Action::ReplaceSelectedText)
            || (is_editable_text_role(
                accessible.map_or_else(|| rendered.fallback_role(), accesskit::Node::role),
            ) && supports(Action::SetValue)),
        NodeAction::SetText,
    );
    push_action(
        &mut actions,
        supports(Action::SetValue),
        NodeAction::SetValue,
    );
    push_action(
        &mut actions,
        [
            Action::ScrollDown,
            Action::ScrollLeft,
            Action::ScrollRight,
            Action::ScrollUp,
            Action::ScrollIntoView,
            Action::ScrollToPoint,
            Action::SetScrollOffset,
        ]
        .into_iter()
        .any(supports),
        NodeAction::Scroll,
    );
    for action in rendered.actions() {
        let action = match action {
            FrameAction::Hover => NodeAction::Hover,
            FrameAction::Drag => NodeAction::Drag,
            FrameAction::Scroll => NodeAction::Scroll,
            FrameAction::SetText => NodeAction::SetText,
            FrameAction::SetValue => NodeAction::SetValue,
        };
        push_action(&mut actions, true, action);
    }
    actions
}

fn push_action(actions: &mut Vec<NodeAction>, condition: bool, action: NodeAction) {
    if condition && !actions.contains(&action) {
        actions.push(action);
    }
}

fn label_from_content(role: AccessibleRole, content: &str) -> Option<String> {
    (!content.is_empty()
        && matches!(
            role,
            AccessibleRole::Button
                | AccessibleRole::DefaultButton
                | AccessibleRole::CheckBox
                | AccessibleRole::RadioButton
                | AccessibleRole::Switch
                | AccessibleRole::Link
                | AccessibleRole::MenuItem
                | AccessibleRole::MenuItemCheckBox
                | AccessibleRole::MenuItemRadio
                | AccessibleRole::ListBoxOption
                | AccessibleRole::MenuListOption
                | AccessibleRole::Tab
        ))
    .then(|| content.to_owned())
}

const fn is_text_role(role: AccessibleRole) -> bool {
    matches!(
        role,
        AccessibleRole::TextInput
            | AccessibleRole::MultilineTextInput
            | AccessibleRole::SearchInput
            | AccessibleRole::EmailInput
            | AccessibleRole::PasswordInput
            | AccessibleRole::PhoneNumberInput
            | AccessibleRole::UrlInput
            | AccessibleRole::Label
            | AccessibleRole::TextRun
            | AccessibleRole::Paragraph
            | AccessibleRole::Heading
            | AccessibleRole::Legend
            | AccessibleRole::Caption
            | AccessibleRole::FigureCaption
            | AccessibleRole::Term
            | AccessibleRole::Code
            | AccessibleRole::Emphasis
            | AccessibleRole::Strong
    )
}

const fn is_editable_text_role(role: AccessibleRole) -> bool {
    matches!(
        role,
        AccessibleRole::TextInput
            | AccessibleRole::MultilineTextInput
            | AccessibleRole::SearchInput
            | AccessibleRole::EmailInput
            | AccessibleRole::PasswordInput
            | AccessibleRole::PhoneNumberInput
            | AccessibleRole::UrlInput
    )
}

const fn role(role: AccessibleRole) -> Role {
    match role {
        AccessibleRole::Application => Role::Application,
        AccessibleRole::Window | AccessibleRole::RootWebArea => Role::Window,
        AccessibleRole::Button
        | AccessibleRole::DefaultButton
        | AccessibleRole::DisclosureTriangle => Role::Button,
        AccessibleRole::CheckBox => Role::Checkbox,
        AccessibleRole::RadioButton => Role::Radio,
        AccessibleRole::Switch => Role::Switch,
        AccessibleRole::Link => Role::Link,
        AccessibleRole::Label
        | AccessibleRole::TextRun
        | AccessibleRole::Paragraph
        | AccessibleRole::Heading
        | AccessibleRole::Legend
        | AccessibleRole::Caption
        | AccessibleRole::FigureCaption
        | AccessibleRole::Term
        | AccessibleRole::Code
        | AccessibleRole::Emphasis
        | AccessibleRole::Strong => Role::Text,
        AccessibleRole::TextInput
        | AccessibleRole::MultilineTextInput
        | AccessibleRole::EmailInput
        | AccessibleRole::PasswordInput
        | AccessibleRole::PhoneNumberInput
        | AccessibleRole::UrlInput => Role::TextInput,
        AccessibleRole::SearchInput | AccessibleRole::Search => Role::SearchInput,
        AccessibleRole::Slider | AccessibleRole::SpinButton => Role::Slider,
        AccessibleRole::ProgressIndicator | AccessibleRole::Meter => Role::Progress,
        AccessibleRole::Image | AccessibleRole::GraphicsSymbol => Role::Image,
        AccessibleRole::List | AccessibleRole::ListBox => Role::List,
        AccessibleRole::ListItem => Role::ListItem,
        AccessibleRole::Tree => Role::Tree,
        AccessibleRole::TreeItem => Role::TreeItem,
        AccessibleRole::Table
        | AccessibleRole::Grid
        | AccessibleRole::TreeGrid
        | AccessibleRole::ListGrid => Role::Table,
        AccessibleRole::Row | AccessibleRole::LayoutTableRow => Role::Row,
        AccessibleRole::Cell
        | AccessibleRole::GridCell
        | AccessibleRole::LayoutTableCell
        | AccessibleRole::RowHeader
        | AccessibleRole::ColumnHeader => Role::Cell,
        AccessibleRole::Menu | AccessibleRole::MenuBar | AccessibleRole::MenuListPopup => {
            Role::Menu
        }
        AccessibleRole::MenuItem
        | AccessibleRole::MenuItemCheckBox
        | AccessibleRole::MenuItemRadio => Role::MenuItem,
        AccessibleRole::ComboBox | AccessibleRole::EditableComboBox => Role::Combobox,
        AccessibleRole::ListBoxOption | AccessibleRole::MenuListOption => Role::Option,
        AccessibleRole::Splitter => Role::Separator,
        AccessibleRole::Tooltip => Role::Tooltip,
        AccessibleRole::TabList => Role::TabList,
        AccessibleRole::Tab => Role::Tab,
        AccessibleRole::Toolbar => Role::Toolbar,
        AccessibleRole::Dialog | AccessibleRole::AlertDialog => Role::Dialog,
        AccessibleRole::Alert => Role::Alert,
        AccessibleRole::ScrollBar | AccessibleRole::ScrollView => Role::ScrollArea,
        AccessibleRole::Group
        | AccessibleRole::Pane
        | AccessibleRole::RadioGroup
        | AccessibleRole::TabPanel => Role::Group,
        _ => Role::Generic,
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
        Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Role,
        StatefulInteractiveElement as _, Styled as _, StyledText, TestAppContext, Window, div, px,
    };
    use gpui_mcp_protocol::{MouseButton, NodeAction, Point, PointerCommand, Role as McpRole};

    use crate::{Automation, input::dispatch_pointer};

    struct SemanticFixture {
        clicked: Rc<Cell<bool>>,
    }

    impl Render for SemanticFixture {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("root")
                .role(Role::Application)
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
        assert_eq!(tree.nodes["root"].role, McpRole::Application);
        assert_eq!(tree.nodes["save"].parent.as_deref(), Some("root"));
        assert_eq!(tree.nodes["save-label"].parent.as_deref(), Some("save"));
        assert_eq!(tree.nodes["save"].role, McpRole::Button);
        assert_eq!(tree.nodes["save"].label.as_deref(), Some("Save"));
        assert!(tree.nodes["save"].actions.contains(&NodeAction::Click));
        assert!(
            tree.nodes["hover-target"]
                .actions
                .contains(&NodeAction::Hover)
        );
        assert_eq!(tree.nodes["status"].label.as_deref(), None);

        assert!(tree.nodes["save"].bounds.is_some());
        let save = tree.nodes["save"].bounds.unwrap_or_default();
        visual.update(|window, cx| {
            let point = save.center();
            assert_eq!(
                dispatch_pointer(
                    &PointerCommand::MouseDown {
                        point: Point {
                            x: point.x,
                            y: point.y
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
                            y: point.y
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
