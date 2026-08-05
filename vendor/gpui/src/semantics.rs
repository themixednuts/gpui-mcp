//! Semantic information derived from GPUI's rendered element tree.
//!
//! GPUI elements are rebuilt every frame, so semantics are collected alongside
//! layout and prepaint. Consumers receive a stable, element-ID-backed snapshot
//! after prepaint has completed.

use crate::{App, Bounds, FocusHandle, GlobalElementId, InteractiveElement, Pixels, Window};
use std::collections::{BTreeMap, HashMap};

/// The purpose of an element in the user interface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SemanticRole {
    /// An element without a more specific semantic role.
    #[default]
    Generic,
    /// The application root.
    Application,
    /// Static text.
    Text,
    /// A control that activates an action.
    Button,
    /// An editable text field.
    TextInput,
    /// An editable search field.
    SearchInput,
    /// A checkable control.
    Checkbox,
    /// A mutually exclusive checkable control.
    Radio,
    /// A binary on/off control.
    Switch,
    /// A control that chooses from a list.
    Combobox,
    /// A control that selects a value from a range.
    Slider,
    /// A progress indicator.
    Progress,
    /// A hyperlink.
    Link,
    /// An image.
    Image,
    /// A related group of elements.
    Group,
    /// A list.
    List,
    /// An item in a list.
    ListItem,
    /// A table or grid.
    Table,
    /// A tree.
    Tree,
    /// An item in a tree.
    TreeItem,
    /// A menu.
    Menu,
    /// An item in a menu.
    MenuItem,
    /// An option in a listbox or combobox.
    Option,
    /// A non-interactive separator.
    Separator,
    /// Contextual help shown for another element.
    Tooltip,
    /// A list of tabs.
    TabList,
    /// A tab.
    Tab,
    /// A toolbar.
    Toolbar,
    /// A dialog.
    Dialog,
    /// An alert.
    Alert,
    /// A scrollable region.
    ScrollArea,
}

/// An interaction supported by a semantic element.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticAction {
    /// Activate the element through its normal click handling.
    Click,
    /// Move keyboard focus to the element.
    Focus,
    /// Move the pointer over the element.
    Hover,
    /// Start a drag from the element.
    Drag,
    /// Replace editable text using the active input handler.
    SetText,
    /// Change a value-bearing control.
    SetValue,
    /// Scroll the element.
    Scroll,
}

/// Dynamic state associated with a semantic element.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticState {
    /// Whether the element is visible.
    pub visible: bool,
    /// Whether the element accepts input.
    pub enabled: bool,
    /// Whether the element owns keyboard focus.
    pub focused: bool,
    /// The checked state of a checkable element.
    pub checked: Option<bool>,
    /// The selection state of a selectable element.
    pub selected: Option<bool>,
    /// The expansion state of a disclosure element.
    pub expanded: Option<bool>,
}

impl Default for SemanticState {
    fn default() -> Self {
        Self {
            visible: true,
            enabled: true,
            focused: false,
            checked: None,
            selected: None,
            expanded: None,
        }
    }
}

/// Editable text exposed by an element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticText {
    /// Current text. Secret values should be omitted.
    pub text: String,
    /// UTF-8 byte index of the caret.
    pub caret: Option<usize>,
    /// Selected UTF-8 byte range.
    pub selection: Option<std::ops::Range<usize>>,
    /// Whether the value is intentionally redacted.
    pub redacted: bool,
    /// Whether the active input handler accepts text replacement.
    pub editable: bool,
}

/// A value exposed by a semantic element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticValue {
    /// Current display value.
    pub value: String,
    /// Optional numeric minimum.
    pub min: Option<f64>,
    /// Optional numeric maximum.
    pub max: Option<f64>,
    /// Optional numeric increment.
    pub step: Option<f64>,
    /// Whether the value can be changed through normal element input.
    pub editable: bool,
}

/// Application-provided semantic meaning that GPUI cannot infer from behavior.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Semantics {
    /// The element's role.
    pub role: SemanticRole,
    /// Explicit accessible name. When absent, controls use descendant text.
    pub label: Option<String>,
    /// Additional accessible description.
    pub description: Option<String>,
    /// Dynamic state.
    pub state: SemanticState,
    /// Editable or readable text information.
    pub text: Option<SemanticText>,
    /// Value information.
    pub value: Option<SemanticValue>,
    /// Small application-defined metadata fields.
    pub metadata: BTreeMap<String, String>,
}

/// Extension methods for adding semantics GPUI cannot infer from behavior.
pub trait SemanticElementExt: InteractiveElement + Sized {
    /// Set the element's semantic role.
    fn semantic_role(mut self, role: SemanticRole) -> Self {
        self.interactivity().semantics.role = role;
        self
    }

    /// Set an explicit accessible name.
    fn accessible_name(mut self, name: impl Into<String>) -> Self {
        self.interactivity().semantics.label = Some(name.into());
        self
    }

    /// Set an accessible description.
    fn accessible_description(mut self, description: impl Into<String>) -> Self {
        self.interactivity().semantics.description = Some(description.into());
        self
    }

    /// Set whether the element participates visibly in the current UI.
    fn semantic_visible(mut self, visible: bool) -> Self {
        self.interactivity().semantics.state.visible = visible;
        self
    }

    /// Set whether the element accepts input.
    fn semantic_enabled(mut self, enabled: bool) -> Self {
        self.interactivity().semantics.state.enabled = enabled;
        self
    }

    /// Set the checked state of a checkable element.
    fn semantic_checked(mut self, checked: bool) -> Self {
        self.interactivity().semantics.state.checked = Some(checked);
        self
    }

    /// Set the selection state of a selectable element.
    fn semantic_selected(mut self, selected: bool) -> Self {
        self.interactivity().semantics.state.selected = Some(selected);
        self
    }

    /// Set the expansion state of a disclosure element.
    fn semantic_expanded(mut self, expanded: bool) -> Self {
        self.interactivity().semantics.state.expanded = Some(expanded);
        self
    }

    /// Attach text information.
    fn semantic_text(mut self, text: SemanticText) -> Self {
        self.interactivity().semantics.text = Some(text);
        self
    }

    /// Attach value information.
    fn semantic_value(mut self, value: SemanticValue) -> Self {
        self.interactivity().semantics.value = Some(value);
        self
    }

    /// Attach a non-secret metadata field.
    fn semantic_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.interactivity()
            .semantics
            .metadata
            .insert(key.into(), value.into());
        self
    }
}

impl<T: InteractiveElement> SemanticElementExt for T {}

/// Stable identity for one semantic node in a rendered frame.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SemanticNodeId(String);

impl SemanticNodeId {
    pub(crate) fn from_global_id(id: &GlobalElementId) -> Self {
        Self(
            id.0.last()
                .map(ToString::to_string)
                .unwrap_or_else(|| id.to_string()),
        )
    }

    /// Return the stable rendered element path.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One semantic element from a completed GPUI prepaint pass.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticNode {
    /// Stable element-path identity.
    pub id: SemanticNodeId,
    /// Nearest semantic ancestor.
    pub parent: Option<SemanticNodeId>,
    /// Post-layout bounds in window-relative logical pixels.
    pub bounds: Bounds<Pixels>,
    /// Semantic meaning and state.
    pub semantics: Semantics,
    /// Interactions derived from the element's actual GPUI behavior.
    pub actions: Vec<SemanticAction>,
    focus_handle: Option<FocusHandle>,
    content_text: String,
}

/// Semantic snapshot collected from one GPUI frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticFrame {
    nodes: Vec<SemanticNode>,
    indices: HashMap<SemanticNodeId, usize>,
    parents: Vec<SemanticNodeId>,
    enabled: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SemanticCheckpoint {
    node_count: usize,
    content: Vec<(SemanticNodeId, String)>,
}

impl SemanticFrame {
    pub(crate) fn begin(&mut self, enabled: bool) {
        self.nodes.clear();
        self.indices.clear();
        self.parents.clear();
        self.enabled = enabled;
    }

    pub(crate) fn enable(&mut self) {
        self.enabled = true;
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn enter(
        &mut self,
        global_id: Option<&GlobalElementId>,
        semantics: Option<(Semantics, Vec<SemanticAction>)>,
        focus_handle: Option<FocusHandle>,
        bounds: Bounds<Pixels>,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let (global_id, (mut semantics, actions)) = match (global_id, semantics) {
            (Some(global_id), Some(semantics)) => (global_id, semantics),
            _ => return false,
        };
        let id = SemanticNodeId::from_global_id(global_id);
        semantics.state.visible &= !bounds.is_empty();
        let node = SemanticNode {
            id: id.clone(),
            parent: self.parents.last().cloned(),
            bounds,
            semantics,
            actions,
            focus_handle,
            content_text: String::new(),
        };
        let index = self.nodes.len();
        self.nodes.push(node);
        self.indices.entry(id.clone()).or_insert(index);
        self.parents.push(id);
        true
    }

    pub(crate) fn exit(&mut self, entered: bool) {
        if entered {
            self.parents.pop();
        }
    }

    pub(crate) fn add_text(&mut self, text: impl AsRef<str>) {
        let normalized = text
            .as_ref()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if normalized.is_empty() {
            return;
        }
        for parent in self.parents.clone() {
            self.append_content(&parent, &normalized);
        }
    }

    pub(crate) fn current_parent(&self) -> Option<SemanticNodeId> {
        self.parents.last().cloned()
    }

    pub(crate) fn checkpoint(&self) -> SemanticCheckpoint {
        SemanticCheckpoint {
            node_count: self.nodes.len(),
            content: self
                .nodes
                .iter()
                .map(|node| (node.id.clone(), node.content_text.clone()))
                .collect(),
        }
    }

    pub(crate) fn restore(&mut self, checkpoint: &SemanticCheckpoint) {
        self.nodes.truncate(checkpoint.node_count);
        for (node, (_, content)) in self.nodes.iter_mut().zip(&checkpoint.content) {
            node.content_text.clone_from(content);
        }
        self.indices
            .retain(|_, index| *index < self.nodes.len());
    }

    pub(crate) fn reuse(
        &mut self,
        rendered: &SemanticFrame,
        start: &SemanticCheckpoint,
        end: &SemanticCheckpoint,
    ) {
        for ((id, before), (after_id, after)) in start.content.iter().zip(&end.content) {
            if id == after_id
                && let Some(delta) = after.strip_prefix(before)
                && !delta.is_empty()
            {
                self.append_content(id, delta.trim_start());
            }
        }
        for node in rendered.nodes[start.node_count..end.node_count]
            .iter()
            .cloned()
        {
            let index = self.nodes.len();
            self.indices.entry(node.id.clone()).or_insert(index);
            self.nodes.push(node);
        }
    }

    pub(crate) fn push_parent(&mut self, parent: Option<SemanticNodeId>) -> bool {
        let Some(parent) = parent else {
            return false;
        };
        self.parents.push(parent);
        true
    }

    pub(crate) fn finish(&mut self) {
        self.parents.clear();
        for node in &mut self.nodes {
            let explicit_text = node
                .semantics
                .text
                .as_ref()
                .map(|text| text.text.trim())
                .filter(|text| !text.is_empty());
            let content_text = (!node.content_text.is_empty()).then_some(node.content_text.as_str());
            let text = content_text.or(explicit_text);
            if node.semantics.label.is_none()
                && role_uses_name_from_contents(node.semantics.role, &node.actions)
            {
                node.semantics.label = text.map(ToOwned::to_owned);
            }
            if node.semantics.role == SemanticRole::Generic
                && node.actions.is_empty()
                && text.is_some()
            {
                node.semantics.role = SemanticRole::Text;
                if node.semantics.text.is_none() {
                    node.semantics.text = content_text.map(|text| SemanticText {
                        text: text.to_owned(),
                        ..SemanticText::default()
                    });
                }
            }
        }
    }

    /// Return semantic nodes in GPUI prepaint order.
    pub fn nodes(&self) -> &[SemanticNode] {
        &self.nodes
    }

    pub(crate) fn focus_handle(&self, id: &str) -> Option<FocusHandle> {
        let mut matches = self
            .nodes
            .iter()
            .filter(|node| node.id.as_str() == id);
        let handle = matches.next()?.focus_handle.clone()?;
        if matches.next().is_some() {
            return None;
        }
        Some(handle)
    }

    fn append_content(&mut self, id: &SemanticNodeId, text: &str) {
        let Some(index) = self.indices.get(id).copied() else {
            return;
        };
        let Some(node) = self.nodes.get_mut(index) else {
            return;
        };
        if !node.content_text.is_empty()
            && !text.chars().next().is_some_and(char::is_whitespace)
        {
            node.content_text.push(' ');
        }
        node.content_text.push_str(text);
    }
}

fn role_uses_name_from_contents(role: SemanticRole, actions: &[SemanticAction]) -> bool {
    matches!(
        role,
        SemanticRole::Button
            | SemanticRole::Checkbox
            | SemanticRole::Radio
            | SemanticRole::Switch
            | SemanticRole::Link
            | SemanticRole::MenuItem
            | SemanticRole::Option
            | SemanticRole::Tab
    ) || (role == SemanticRole::Generic && actions.contains(&SemanticAction::Click))
}

/// Receives frame semantics and may paint a final overlay.
///
/// Observers are retained weakly by [`Window`]. The owner must retain the
/// corresponding [`Arc`] for as long as observation should continue.
pub trait FrameObserver: Send + Sync + 'static {
    /// Called immediately before GPUI begins constructing a new frame.
    fn frame_started(&self, _window: &Window) {}

    /// Called after all root and deferred elements have completed prepaint.
    fn semantics_updated(&self, _frame: &SemanticFrame) {}

    /// Called immediately before the paint pass.
    fn paint_started(&self) {}

    /// Paint an overlay after normal elements have painted.
    fn paint_overlay(&self, _window: &mut Window, _cx: &mut App) {}

    /// Called after paint and observer overlays have completed.
    fn frame_finished(&self) {}
}

impl InteractivitySemanticsExt for crate::Interactivity {
    fn semantic_node(
        &self,
        window: &Window,
        default_role: SemanticRole,
    ) -> Option<(Semantics, Vec<SemanticAction>)> {
        self.element_id.as_ref()?;
        let mut semantics = self.semantics.clone();
        semantics.state.focused = self
            .tracked_focus_handle
            .as_ref()
            .is_some_and(|handle| handle.is_focused(window));

        let mut actions = Vec::new();
        add_action(
            &mut actions,
            !self.click_listeners.is_empty(),
            SemanticAction::Click,
        );
        add_action(
            &mut actions,
            self.focusable || self.tracked_focus_handle.is_some(),
            SemanticAction::Focus,
        );
        add_action(
            &mut actions,
            self.hover_style.is_some()
                || self.group_hover_style.is_some()
                || self.hover_listener.is_some()
                || !self.mouse_move_listeners.is_empty()
                || self.tooltip_builder.is_some(),
            SemanticAction::Hover,
        );
        add_action(
            &mut actions,
            self.drag_listener.is_some(),
            SemanticAction::Drag,
        );
        add_action(
            &mut actions,
            self.scroll_offset.is_some()
                || self.tracked_scroll_handle.is_some()
                || !self.scroll_wheel_listeners.is_empty(),
            SemanticAction::Scroll,
        );
        add_action(
            &mut actions,
            semantics
                .text
                .as_ref()
                .is_some_and(|text| text.editable && !text.redacted),
            SemanticAction::SetText,
        );
        add_action(
            &mut actions,
            semantics
                .value
                .as_ref()
                .is_some_and(|value| value.editable),
            SemanticAction::SetValue,
        );
        if semantics.role == SemanticRole::Generic
            && actions.contains(&SemanticAction::Click)
        {
            semantics.role = SemanticRole::Button;
        } else if semantics.role == SemanticRole::Generic {
            semantics.role = default_role;
        }
        Some((semantics, actions))
    }
}

pub(crate) trait InteractivitySemanticsExt {
    fn semantic_node(
        &self,
        window: &Window,
        default_role: SemanticRole,
    ) -> Option<(Semantics, Vec<SemanticAction>)>;
}

fn add_action(actions: &mut Vec<SemanticAction>, condition: bool, action: SemanticAction) {
    if condition && !actions.contains(&action) {
        actions.push(action);
    }
}
