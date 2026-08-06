use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AlignItems, AlignSelf, AnyElement, App, AppContext as _, BoxShadow, Context, DefiniteLength,
    Div, Entity, FocusHandle, FontFallbacks, FontWeight, FrameAction, GridPlacement,
    InteractiveElement as _, IntoElement, Length, Overflow, ParentElement as _, Render,
    Role as AccessibleRole, ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled, Toggled, Window, div, point, px, relative, rgba,
};
use gpui_mcp::{Automation, MAX_LABEL_BYTES, MAX_TEXT_BYTES};
use htmlswap::{
    RenderElement, RenderNode, RenderPlan, RenderStyleCondition, RenderStyleVariant,
    StyleDeclaration, StyleProperty, UiRole,
};

use crate::components::{ComponentNode, ComponentRegistry};
use crate::document::{attribute, is_text_editable};
use crate::input::{RuntimeTextInput, RuntimeTextInputOptions};
use crate::{
    Binding, BindingMode, ElementId, HandlerId, HookEvent, HookOutcome, HookRegistry,
    HookRegistryError, HtmlUi, StateValue, UiEvent, UiProperty,
};

/// Minimal hover tooltip view that renders an element's `title` attribute text.
struct TitleTooltip {
    text: SharedString,
}

#[derive(Clone, Debug)]
struct ElementState {
    visible: bool,
    enabled: bool,
    checked: Option<bool>,
    selected: Option<bool>,
    expanded: Option<bool>,
}

impl Default for ElementState {
    fn default() -> Self {
        Self {
            visible: true,
            enabled: true,
            checked: None,
            selected: None,
            expanded: None,
        }
    }
}

struct ElementText {
    text: String,
    redacted: bool,
    editable: bool,
}

struct ElementValue {
    value: String,
    editable: bool,
}

impl Render for TitleTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .bg(rgba(0x1f24_30f2))
            .text_color(rgba(0xf5f7_faff))
            .border_1()
            .border_color(rgba(0x0000_0055))
            .rounded_md()
            .px_2()
            .py_1()
            .text_sm()
            .child(self.text.clone())
    }
}

/// One unsupported or invalid live-rendering feature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderDiagnostic {
    /// Semantic node identifier.
    pub node_id: String,
    /// CSS property or rendering feature.
    pub feature: String,
    /// Human-readable explanation.
    pub message: String,
}

/// State-retention details for one successful live-document replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReloadReport {
    /// Revision that was active before the replacement.
    pub previous_revision: u64,
    /// Revision assigned to the replacement.
    pub revision: u64,
    /// Cached focus handles retained because their stable element IDs still exist.
    pub retained_focus_handles: usize,
    /// Cached focus handles removed with deleted elements.
    pub pruned_focus_handles: usize,
    /// Disclosure states retained because their stable element IDs still exist.
    pub retained_disclosures: usize,
    /// Disclosure states removed with deleted elements.
    pub pruned_disclosures: usize,
    /// Whether the explicitly hovered element survived the replacement.
    pub hovered_element_retained: bool,
}

/// Failure to atomically replace a live document.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReloadError {
    /// The candidate document references a hook unavailable to this live application.
    #[error(transparent)]
    Hooks(#[from] HookRegistryError),
    /// A `u64` revision cannot be allocated after the current one.
    #[error("live document revision space is exhausted")]
    RevisionExhausted,
}

/// Valid semantic-ID prefix used when a live document is embedded in another
/// instrumented document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticNamespace(String);

impl SemanticNamespace {
    /// Validate a short lowercase ASCII namespace such as `project-canvas`.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, or non-kebab-case input.
    pub fn new(value: impl Into<String>) -> Result<Self, SemanticNamespaceError> {
        let value = value.into();
        let valid_length = !value.is_empty() && value.len() <= 64;
        let mut characters = value.chars();
        let valid_start = characters
            .next()
            .is_some_and(|character| character.is_ascii_lowercase());
        let valid_rest = characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
        if valid_length && valid_start && valid_rest {
            Ok(Self(value))
        } else {
            Err(SemanticNamespaceError { value })
        }
    }

    fn scope(&self, id: &str) -> String {
        format!("{}--{id}", self.0)
    }
}

/// Invalid embedded semantic namespace.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("`{value}` is not a valid lowercase semantic namespace")]
pub struct SemanticNamespaceError {
    value: String,
}

/// Compiled pure HTML ready to render inside an instrumented GPUI window.
#[derive(Clone)]
pub struct LiveHtml {
    ui: Rc<HtmlUi>,
    revision: u64,
    automation: Automation,
    hooks: HookRegistry,
    components: ComponentRegistry,
    bindings: HashMap<ElementId, Rc<[Binding]>>,
    diagnostics: Vec<RenderDiagnostic>,
    focus_handles: Rc<RefCell<HashMap<ElementId, FocusHandle>>>,
    scroll_handles: Rc<RefCell<HashMap<ElementId, ScrollHandle>>>,
    text_inputs: Rc<RefCell<HashMap<ElementId, Entity<RuntimeTextInput>>>>,
    hovered_element: Rc<RefCell<Option<ElementId>>>,
    disclosures: Rc<RefCell<HashMap<ElementId, bool>>>,
    embedded_namespace: Option<SemanticNamespace>,
    viewport_override: Cell<Option<MediaViewport>>,
    available_fonts: RefCell<Option<Rc<HashSet<String>>>>,
}

#[derive(Clone, Copy)]
struct ElementRuntime<'a> {
    element_id: &'a ElementId,
    bindings: &'a [Binding],
    properties: &'a HashMap<UiProperty, StateValue>,
    enabled: bool,
}

impl LiveHtml {
    /// Connect a compiled document to application hooks and MCP automation.
    ///
    /// # Errors
    ///
    /// Every symbolic event/state reference must be registered, and two-way
    /// properties must use a writable state hook.
    pub fn new(
        ui: HtmlUi,
        automation: Automation,
        hooks: HookRegistry,
    ) -> Result<Self, HookRegistryError> {
        hooks.validate(ui.bindings())?;
        let bindings = index_bindings(ui.bindings().bindings.iter());
        let diagnostics = collect_render_diagnostics(ui.plan());
        Ok(Self {
            ui: Rc::new(ui),
            revision: 1,
            automation,
            hooks,
            components: ComponentRegistry::new(),
            bindings,
            diagnostics,
            focus_handles: Rc::default(),
            scroll_handles: Rc::default(),
            text_inputs: Rc::default(),
            hovered_element: Rc::default(),
            disclosures: Rc::default(),
            embedded_namespace: None,
            viewport_override: Cell::new(None),
            available_fonts: RefCell::new(None),
        })
    }

    /// Render this document below another document's single MCP semantic root.
    ///
    /// Every runtime and semantic element ID is prefixed with `namespace`, while
    /// authored HTML IDs continue to resolve bindings and retained state. Embedded
    /// documents deliberately do not begin or finish their own semantic frame.
    #[must_use]
    pub fn embedded(mut self, namespace: SemanticNamespace) -> Self {
        self.embedded_namespace = Some(namespace);
        self
    }

    pub(crate) fn set_embedded_namespace(&mut self, namespace: SemanticNamespace) {
        self.embedded_namespace = Some(namespace);
    }

    /// Install application custom-element factories.
    #[must_use]
    pub fn with_components(mut self, components: ComponentRegistry) -> Self {
        self.components = components;
        self
    }

    /// Replace application custom-element factories without changing the document.
    pub fn set_components(&mut self, components: ComponentRegistry) {
        self.components = components;
    }

    /// Return the currently active compiled document.
    #[must_use]
    pub fn document(&self) -> &HtmlUi {
        &self.ui
    }

    /// Monotonically increasing in-process document revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Atomically replace the compiled document while retaining UI state for
    /// stable element IDs.
    ///
    /// The candidate is completely validated and indexed before any active
    /// document or runtime state is changed. Deleted-node caches are pruned;
    /// application hooks and custom-component registrations remain installed.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the active document when a candidate
    /// binding references an unavailable hook or the revision counter is exhausted.
    pub fn reload(&mut self, ui: HtmlUi) -> Result<ReloadReport, ReloadError> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(ReloadError::RevisionExhausted)?;
        self.hooks.validate(ui.bindings())?;
        let bindings = index_bindings(ui.bindings().bindings.iter());
        let diagnostics = collect_render_diagnostics(ui.plan());
        let element_ids = collect_element_ids(ui.plan());

        let previous_focus_handles = self.focus_handles.borrow().len();
        self.focus_handles
            .borrow_mut()
            .retain(|element_id, _| element_ids.contains(element_id));
        self.scroll_handles
            .borrow_mut()
            .retain(|element_id, _| element_ids.contains(element_id));
        let retained_focus_handles = self.focus_handles.borrow().len();
        self.text_inputs
            .borrow_mut()
            .retain(|element_id, _| element_ids.contains(element_id));

        let previous_disclosures = self.disclosures.borrow().len();
        self.disclosures
            .borrow_mut()
            .retain(|element_id, _| element_ids.contains(element_id));
        let retained_disclosures = self.disclosures.borrow().len();

        let hovered_element_retained = self
            .hovered_element
            .borrow()
            .as_ref()
            .is_some_and(|element_id| element_ids.contains(element_id));
        if !hovered_element_retained {
            self.hovered_element.borrow_mut().take();
        }

        let previous_revision = self.revision;
        self.ui = Rc::new(ui);
        self.bindings = bindings;
        self.diagnostics = diagnostics;
        self.revision = revision;

        Ok(ReloadReport {
            previous_revision,
            revision,
            retained_focus_handles,
            pruned_focus_handles: previous_focus_handles - retained_focus_handles,
            retained_disclosures,
            pruned_disclosures: previous_disclosures - retained_disclosures,
            hovered_element_retained,
        })
    }

    /// Build a live GPUI element tree. Call this from the owning view's `Render` implementation.
    #[must_use]
    pub fn render(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        self.render_with_media_viewport(MediaViewport::from_window(window), window, cx)
    }

    /// Render with an explicit logical viewport for an embedded responsive-preview surface.
    ///
    /// GPUI still lays the tree out within its containing element, while CSS media queries use
    /// `width` and `height`. Invalid dimensions safely fall back to the window viewport.
    #[must_use]
    pub fn render_for_viewport(
        &self,
        width: f32,
        height: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let viewport = if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
            MediaViewport { width, height }
        } else {
            MediaViewport::from_window(window)
        };
        self.render_with_media_viewport(viewport, window, cx)
    }

    fn render_with_media_viewport(
        &self,
        viewport: MediaViewport,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.automation.attach(window);
        self.viewport_override.set(Some(viewport));
        let available_fonts = self.available_fonts(cx);
        let children = self
            .ui
            .plan()
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                self.render_node(node, &[index], None, &available_fonts, window, cx)
            })
            .collect::<Vec<_>>();
        let root = apply_declarations(
            children.into_iter().fold(div(), gpui::ParentElement::child),
            self.ui.plan().root.styles.iter(),
            &available_fonts,
        );
        let root = apply_media_variants(
            root,
            &self.ui.plan().root.style_variants,
            viewport,
            &available_fonts,
        );
        let rendered = root
            .id(SharedString::from(self.scoped_id("html-root")))
            .role(AccessibleRole::Application)
            .into_any_element();
        self.viewport_override.set(None);
        rendered
    }

    fn available_fonts(&self, cx: &App) -> Rc<HashSet<String>> {
        if let Some(fonts) = self.available_fonts.borrow().as_ref() {
            return fonts.clone();
        }
        let fonts = Rc::new(available_fonts(cx));
        *self.available_fonts.borrow_mut() = Some(fonts.clone());
        fonts
    }

    fn media_viewport(&self, window: &Window) -> MediaViewport {
        self.viewport_override
            .get()
            .unwrap_or_else(|| MediaViewport::from_window(window))
    }

    /// Unsupported CSS/features retained for visual-builder diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &[RenderDiagnostic] {
        &self.diagnostics
    }

    fn render_node(
        &self,
        node: &RenderNode,
        path: &[usize],
        disclosure_owner: Option<&ElementId>,
        available_fonts: &HashSet<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        match node {
            RenderNode::Text(text) => text.value.clone().into_any_element(),
            RenderNode::Raw(raw) => raw.html.clone().into_any_element(),
            RenderNode::Element(element) => {
                self.render_element(element, path, disclosure_owner, available_fonts, window, cx)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn render_element(
        &self,
        element: &RenderElement,
        path: &[usize],
        disclosure_owner: Option<&ElementId>,
        available_fonts: &HashSet<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let id = attribute(element, "id").map_or_else(|| generated_id(path), str::to_owned);
        let runtime_id = self.scoped_id(&id);
        let element_id = ElementId::new(id.clone());
        let bindings = self.bindings.get(&element_id).cloned().unwrap_or_default();
        let property_values = read_properties(&bindings, &self.hooks, window, cx);
        let is_disclosure = element.source_tag == "details";
        let disclosure_open = self.disclosure_open(element, &element_id, is_disclosure);
        let mut state = element_state(element, &property_values);
        state.expanded = disclosure_open;
        let runtime = ElementRuntime {
            element_id: &element_id,
            bindings: &bindings,
            properties: &property_values,
            enabled: state.enabled,
        };
        let (children, text_input) = self.render_element_children(
            element,
            path,
            runtime,
            disclosure_open,
            available_fonts,
            window,
            cx,
        );
        let mut host = self.render_styled_host(element, &id, children, available_fonts, window, cx);
        host = apply_native_state(host, element, &state);
        if !state.visible {
            host = host.hidden();
        }
        if let Some(width) = property_values
            .get(&UiProperty::Width)
            .and_then(StateValue::as_pixels)
        {
            host = host.w(px(width));
        }
        if let Some(height) = property_values
            .get(&UiProperty::Height)
            .and_then(StateValue::as_pixels)
        {
            host = host.h(px(height));
        }
        let scroll_axes = element_scroll_axes(element, self.media_viewport(window));
        let scroll_handle = scroll_axes.any().then(|| {
            self.scroll_handles
                .borrow_mut()
                .entry(element_id.clone())
                .or_default()
                .clone()
        });
        let toggle = semantic_toggle(element, &state, &bindings);
        let mut host = install_pointer_hooks(
            host,
            &runtime_id,
            &element_id,
            &bindings,
            &self.hooks,
            state.enabled,
            toggle,
        );
        if let Some(scroll_handle) = &scroll_handle {
            host = host.track_scroll(scroll_handle);
        }
        if state.enabled
            && let Some(disclosure_id) = disclosure_owner.cloned()
        {
            let disclosures = self.disclosures.clone();
            host = host.on_click(move |_, window, _| {
                toggle_disclosure(&disclosures, &disclosure_id);
                window.refresh();
            });
        }

        let hoverable = has_interactive_style(element, InteractiveStyle::Hover);
        let focus_handle = self.resolve_focus_handle(element, runtime, text_input.as_ref(), cx);
        if let Some(focus_handle) = &focus_handle {
            host = host.track_focus(focus_handle);
        }
        let forced_hover = self.hovered_element.borrow().as_ref() == Some(&element_id);
        host = apply_interactive_styles(
            host,
            element,
            forced_hover,
            self.media_viewport(window),
            available_fonts,
        );
        if hoverable {
            // GPUI's style-only hover hook does not itself retain enough state for a
            // runtime document to reproduce the hovered cascade on every refreshed frame.
            // Mirror native hit-test transitions into the same state used by semantic hover,
            // so physical input, MCP PlatformInput, and semantic automation resolve one CSS
            // :hover state instead of taking separate rendering paths.
            let hovered_element = self.hovered_element.clone();
            let hovered_id = element_id.clone();
            host = host.on_hover(move |hovered, window, _| {
                update_hovered_element(&hovered_element, &hovered_id, *hovered);
                window.refresh();
            });
        }

        let role = accessible_role(element);
        if let Some(role) = role {
            host = host.role(role);
        }
        host = host
            .aria_hidden(!state.visible)
            .aria_disabled(!state.enabled)
            .frame_metadata("html_tag", element.source_tag.to_string());
        if let UiRole::Heading(level) = element.role {
            host = host.aria_level(level.into());
        }
        if let Some(checked) = state.checked {
            host = host.aria_toggled(if checked {
                Toggled::True
            } else {
                Toggled::False
            });
        }
        if let Some(selected) = state.selected {
            host = host.aria_selected(selected);
        }
        if let Some(expanded) = state.expanded {
            host = host.aria_expanded(expanded);
        }
        if let Some(authored_id) = attribute(element, "id") {
            host = host.frame_metadata("authored_id", authored_id);
        }
        if let Some(component_id) = attribute(element, "component") {
            host = host.frame_metadata("component_id", component_id);
        }
        if let Some(label) = accessible_label(element, &property_values) {
            host = host.aria_label(label);
        }
        if let Some(title) = attribute(element, "title") {
            host = host.aria_description(title);
            let tooltip_text = SharedString::from(title.to_owned());
            host = host.tooltip(move |_window, cx| {
                cx.new(|_| TitleTooltip {
                    text: tooltip_text.clone(),
                })
                .into()
            });
        }
        if let Some(text) = element_text(element, &property_values, &bindings) {
            if text.redacted {
                host = host.frame_redacted(true);
            } else if is_editable_role(role) {
                host = host.aria_value(text.text);
            }
            if text.editable && !text.redacted {
                host = host.frame_action(FrameAction::SetText);
            }
        }
        if let Some(value) = element_value(element, &property_values, &bindings) {
            if !is_editable_role(role) {
                host = host.aria_value(value.value);
            }
            if value.editable {
                host = host.frame_action(FrameAction::SetValue);
            }
        }

        host.into_any_element()
    }

    fn disclosure_open(
        &self,
        element: &RenderElement,
        element_id: &ElementId,
        is_disclosure: bool,
    ) -> Option<bool> {
        is_disclosure.then(|| {
            *self
                .disclosures
                .borrow_mut()
                .entry(element_id.clone())
                .or_insert_with(|| attribute(element, "open").is_some())
        })
    }

    fn scoped_id(&self, id: &str) -> String {
        self.embedded_namespace
            .as_ref()
            .map_or_else(|| id.to_owned(), |namespace| namespace.scope(id))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_children(
        &self,
        element: &RenderElement,
        path: &[usize],
        text: Option<&StateValue>,
        disclosure_open: Option<bool>,
        available_fonts: &HashSet<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        if let Some(value) = text {
            return vec![value.display().into_any_element()];
        }
        let disclosure_owner = disclosure_open.map(|_| {
            ElementId::new(
                attribute(element, "id").map_or_else(|| generated_id(path), str::to_owned),
            )
        });
        element
            .children
            .iter()
            .enumerate()
            .filter(|(_, child)| disclosure_open != Some(false) || is_summary_element(child))
            .map(|(index, child)| {
                let mut child_path = path.to_vec();
                child_path.push(index);
                let child_disclosure_owner = disclosure_owner
                    .as_ref()
                    .filter(|_| is_summary_element(child));
                self.render_node(
                    child,
                    &child_path,
                    child_disclosure_owner,
                    available_fonts,
                    window,
                    cx,
                )
            })
            .collect()
    }

    fn render_text_input(
        &self,
        element: &RenderElement,
        runtime: ElementRuntime<'_>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<RuntimeTextInput> {
        let value = runtime
            .properties
            .get(&UiProperty::Value)
            .map(StateValue::display)
            .or_else(|| {
                element
                    .form_control
                    .as_ref()
                    .and_then(|control| control.value.as_ref())
                    .map(ToString::to_string)
            })
            .or_else(|| attribute(element, "value").map(str::to_owned))
            .unwrap_or_else(|| {
                if element.source_tag == "textarea" {
                    element_text_content(element)
                } else {
                    String::new()
                }
            });
        let placeholder = attribute(element, "placeholder")
            .unwrap_or_default()
            .to_owned();
        let multiline = element.source_tag == "textarea";
        let masked = is_password(element);
        let disabled = !runtime.enabled;
        let cached_input = self
            .text_inputs
            .borrow()
            .get(runtime.element_id)
            .filter(|input| input.read(cx).is_compatible(multiline, masked))
            .cloned();
        if let Some(input) = cached_input {
            let needs_sync =
                input
                    .read(cx)
                    .needs_sync(self.revision, &value, &placeholder, disabled, cx);
            if needs_sync {
                let options = RuntimeTextInputOptions {
                    value,
                    placeholder,
                    multiline,
                    masked,
                    disabled,
                    document_revision: self.revision,
                    element_id: runtime.element_id.clone(),
                    bindings: runtime.bindings.to_vec(),
                    hooks: self.hooks.clone(),
                };
                input.update(cx, |input, cx| input.sync(options, window, cx));
            }
            return input;
        }
        let options = RuntimeTextInputOptions {
            value,
            placeholder,
            multiline,
            masked,
            disabled,
            document_revision: self.revision,
            element_id: runtime.element_id.clone(),
            bindings: runtime.bindings.to_vec(),
            hooks: self.hooks.clone(),
        };
        let input = cx.new(|cx| RuntimeTextInput::new(options, window, cx));
        self.text_inputs
            .borrow_mut()
            .insert(runtime.element_id.clone(), input.clone());
        input
    }

    #[allow(clippy::too_many_arguments)]
    fn render_element_children(
        &self,
        element: &RenderElement,
        path: &[usize],
        runtime: ElementRuntime<'_>,
        disclosure_open: Option<bool>,
        available_fonts: &HashSet<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> (Vec<AnyElement>, Option<Entity<RuntimeTextInput>>) {
        let text_input =
            is_text_editable(element).then(|| self.render_text_input(element, runtime, window, cx));
        let children = text_input.as_ref().map_or_else(
            || {
                self.render_children(
                    element,
                    path,
                    runtime.properties.get(&UiProperty::Text),
                    disclosure_open,
                    available_fonts,
                    window,
                    cx,
                )
            },
            |input| vec![input.clone().into_any_element()],
        );
        (children, text_input)
    }

    fn resolve_focus_handle(
        &self,
        element: &RenderElement,
        runtime: ElementRuntime<'_>,
        text_input: Option<&Entity<RuntimeTextInput>>,
        cx: &mut App,
    ) -> Option<FocusHandle> {
        let focusable = has_interactive_style(element, InteractiveStyle::Focus)
            || runtime.bindings.iter().any(|binding| {
                matches!(
                    binding,
                    Binding::Event {
                        event: UiEvent::Focus,
                        ..
                    }
                )
            });
        text_input
            .map(|input| input.read(cx).focus_handle(cx))
            .or_else(|| {
                focusable.then(|| {
                    self.focus_handles
                        .borrow_mut()
                        .entry(runtime.element_id.clone())
                        .or_insert_with(|| cx.focus_handle())
                        .clone()
                })
            })
    }

    fn render_styled_host(
        &self,
        element: &RenderElement,
        id: &str,
        children: Vec<AnyElement>,
        available_fonts: &HashSet<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        let viewport = self.media_viewport(window);
        apply_styles(
            apply_native_defaults(self.render_host(element, id, children, window, cx), element),
            element,
            viewport,
            available_fonts,
        )
    }

    fn render_host(
        &self,
        element: &RenderElement,
        id: &str,
        children: Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        let custom_node = ComponentNode::new(
            id.to_owned(),
            element.source_tag.to_string(),
            element
                .attributes
                .iter()
                .map(|attribute| (attribute.name.to_string(), attribute.value.to_string()))
                .collect::<BTreeMap<_, _>>(),
        );
        if let Some(factory) = self.components.factory(custom_node.tag()) {
            div().child(factory(&custom_node, children, window, cx))
        } else {
            children.into_iter().fold(div(), gpui::ParentElement::child)
        }
    }
}

fn apply_native_defaults(host: Div, element: &RenderElement) -> Div {
    if element.source_tag != "input" {
        return host;
    }
    match attribute(element, "type") {
        Some("checkbox") => host
            .flex_none()
            .size(px(16.))
            .items_center()
            .justify_center()
            .rounded(px(3.))
            .border_1()
            .border_color(rgba(0x6873_8499)),
        Some("radio") => host
            .flex_none()
            .size(px(16.))
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(rgba(0x6873_8499)),
        _ => host,
    }
}

fn apply_native_state(host: Div, element: &RenderElement, state: &ElementState) -> Div {
    if !state.checked.unwrap_or(false) || element.source_tag != "input" {
        return host;
    }
    match attribute(element, "type") {
        Some("checkbox") => host
            .bg(rgba(0x4f7d_ffff))
            .text_color(rgba(0xffff_ffff))
            .text_size(px(12.))
            .child("✓"),
        Some("radio") => host
            .text_color(rgba(0x4f7d_ffff))
            .text_size(px(10.))
            .child("●"),
        _ => host,
    }
}

fn is_summary_element(node: &RenderNode) -> bool {
    matches!(node, RenderNode::Element(element) if element.source_tag == "summary")
}

fn toggle_disclosure(disclosures: &Rc<RefCell<HashMap<ElementId, bool>>>, element_id: &ElementId) {
    let mut disclosures = disclosures.borrow_mut();
    let open = disclosures.entry(element_id.clone()).or_default();
    *open = !*open;
}

fn index_bindings<'a>(
    bindings: impl Iterator<Item = &'a Binding>,
) -> HashMap<ElementId, Rc<[Binding]>> {
    let mut index = HashMap::<ElementId, Vec<Binding>>::new();
    for binding in bindings {
        index
            .entry(binding.element_id().clone())
            .or_default()
            .push(binding.clone());
    }
    index
        .into_iter()
        .map(|(element_id, bindings)| (element_id, Rc::from(bindings)))
        .collect()
}

fn generated_id(path: &[usize]) -> String {
    let suffix = path
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join("-");
    format!("html-node-{suffix}")
}

fn collect_element_ids(plan: &RenderPlan) -> HashSet<ElementId> {
    fn collect(nodes: &[RenderNode], parent_path: &[usize], ids: &mut HashSet<ElementId>) {
        for (index, node) in nodes.iter().enumerate() {
            let RenderNode::Element(element) = node else {
                continue;
            };
            let mut path = parent_path.to_vec();
            path.push(index);
            let id = attribute(element, "id").map_or_else(|| generated_id(&path), str::to_owned);
            ids.insert(ElementId::new(id));
            collect(&element.children, &path, ids);
        }
    }

    let mut ids = HashSet::new();
    collect(&plan.nodes, &[], &mut ids);
    ids
}

fn read_properties(
    bindings: &[Binding],
    hooks: &HookRegistry,
    window: &mut Window,
    cx: &mut App,
) -> HashMap<UiProperty, StateValue> {
    bindings
        .iter()
        .filter_map(|binding| {
            let Binding::Property {
                property, source, ..
            } = binding
            else {
                return None;
            };
            hooks
                .read(source, window, cx)
                .map(|value| (*property, value))
        })
        .collect()
}

fn element_state(
    element: &RenderElement,
    properties: &HashMap<UiProperty, StateValue>,
) -> ElementState {
    let disabled = properties
        .get(&UiProperty::Disabled)
        .and_then(StateValue::as_boolean)
        .unwrap_or_else(|| {
            element
                .form_control
                .as_ref()
                .is_some_and(|control| control.disabled)
        });
    ElementState {
        visible: properties
            .get(&UiProperty::Visible)
            .and_then(StateValue::as_boolean)
            .unwrap_or(true),
        enabled: !disabled,
        checked: properties
            .get(&UiProperty::Checked)
            .and_then(StateValue::as_boolean)
            .or_else(|| attribute(element, "checked").map(|_| true)),
        selected: properties
            .get(&UiProperty::Selected)
            .and_then(StateValue::as_boolean)
            .or_else(|| attribute(element, "selected").map(|_| true)),
        ..ElementState::default()
    }
}

fn accessible_role(element: &RenderElement) -> Option<AccessibleRole> {
    if let Some(role) = attribute(element, "role") {
        return aria_role(role);
    }
    if attribute(element, "tabindex").is_some() {
        return Some(AccessibleRole::Group);
    }
    if element.source_tag == "input" {
        match attribute(element, "type") {
            Some("checkbox") => return Some(AccessibleRole::CheckBox),
            Some("radio") => return Some(AccessibleRole::RadioButton),
            Some("search") => return Some(AccessibleRole::SearchInput),
            Some("password") => return Some(AccessibleRole::PasswordInput),
            _ => {}
        }
    }
    match element.source_tag.as_ref() {
        "article" => return Some(AccessibleRole::Article),
        "aside" => return Some(AccessibleRole::Complementary),
        "details" => return Some(AccessibleRole::Details),
        "footer" => return Some(AccessibleRole::Footer),
        "header" => return Some(AccessibleRole::Header),
        "main" => return Some(AccessibleRole::Main),
        "nav" => return Some(AccessibleRole::Navigation),
        "section" => return Some(AccessibleRole::Section),
        "summary" => return Some(AccessibleRole::DisclosureTriangle),
        _ => {}
    }
    Some(match element.role {
        UiRole::Container | UiRole::Unknown => return None,
        UiRole::Inline | UiRole::Label => AccessibleRole::Label,
        UiRole::Form => AccessibleRole::Form,
        UiRole::Fieldset => AccessibleRole::Group,
        UiRole::Select => AccessibleRole::ComboBox,
        UiRole::Paragraph => AccessibleRole::Paragraph,
        UiRole::Heading(_) => AccessibleRole::Heading,
        UiRole::Legend => AccessibleRole::Legend,
        UiRole::Button => AccessibleRole::Button,
        UiRole::TextInput => {
            if element.source_tag == "textarea" {
                AccessibleRole::MultilineTextInput
            } else {
                AccessibleRole::TextInput
            }
        }
        UiRole::Option => AccessibleRole::ListBoxOption,
        UiRole::ListItem => AccessibleRole::ListItem,
        UiRole::Link => AccessibleRole::Link,
        UiRole::Image => AccessibleRole::Image,
        UiRole::List { .. } => AccessibleRole::List,
    })
}

fn aria_role(role: &str) -> Option<AccessibleRole> {
    Some(match role.trim().to_ascii_lowercase().as_str() {
        "application" => AccessibleRole::Application,
        "alert" => AccessibleRole::Alert,
        "button" => AccessibleRole::Button,
        "checkbox" => AccessibleRole::CheckBox,
        "combobox" => AccessibleRole::ComboBox,
        "dialog" => AccessibleRole::Dialog,
        "group" => AccessibleRole::Group,
        "img" => AccessibleRole::Image,
        "link" => AccessibleRole::Link,
        "list" => AccessibleRole::List,
        "listbox" => AccessibleRole::ListBox,
        "listitem" => AccessibleRole::ListItem,
        "menu" => AccessibleRole::Menu,
        "menubar" => AccessibleRole::MenuBar,
        "menuitem" => AccessibleRole::MenuItem,
        "menuitemcheckbox" => AccessibleRole::MenuItemCheckBox,
        "menuitemradio" => AccessibleRole::MenuItemRadio,
        "option" => AccessibleRole::ListBoxOption,
        "progressbar" => AccessibleRole::ProgressIndicator,
        "radio" => AccessibleRole::RadioButton,
        "scrollbar" => AccessibleRole::ScrollBar,
        "searchbox" => AccessibleRole::SearchInput,
        "separator" => AccessibleRole::Splitter,
        "slider" => AccessibleRole::Slider,
        "switch" => AccessibleRole::Switch,
        "tab" => AccessibleRole::Tab,
        "table" => AccessibleRole::Table,
        "grid" => AccessibleRole::Grid,
        "treegrid" => AccessibleRole::TreeGrid,
        "tablist" => AccessibleRole::TabList,
        "toolbar" => AccessibleRole::Toolbar,
        "tooltip" => AccessibleRole::Tooltip,
        "tree" => AccessibleRole::Tree,
        "treeitem" => AccessibleRole::TreeItem,
        _ => return None,
    })
}

fn accessible_label(
    element: &RenderElement,
    properties: &HashMap<UiProperty, StateValue>,
) -> Option<String> {
    element
        .accessibility
        .as_ref()
        .and_then(|accessibility| accessibility.label.as_ref())
        .map(ToString::to_string)
        .or_else(|| {
            element
                .form_control
                .as_ref()?
                .label
                .as_ref()
                .map(ToString::to_string)
        })
        .or_else(|| attribute(element, "alt").map(str::to_owned))
        .or_else(|| {
            matches!(element.role, UiRole::Button | UiRole::Link | UiRole::Option)
                .then(|| {
                    properties
                        .get(&UiProperty::Text)
                        .map_or_else(|| element_text_content(element), StateValue::display)
                })
                .filter(|text| !text.is_empty())
        })
        .map(|label| bounded_utf8(label, MAX_LABEL_BYTES))
}

fn element_text(
    element: &RenderElement,
    properties: &HashMap<UiProperty, StateValue>,
    bindings: &[Binding],
) -> Option<ElementText> {
    let editable = is_text_editable(element) && has_writable_text_binding(bindings);
    if is_password(element) {
        return Some(ElementText {
            text: String::new(),
            redacted: true,
            editable,
        });
    }

    let text = properties
        .get(&UiProperty::Text)
        .or_else(|| {
            matches!(element.role, UiRole::TextInput)
                .then(|| properties.get(&UiProperty::Value))
                .flatten()
        })
        .map(StateValue::display)
        .or_else(|| {
            matches!(element.role, UiRole::TextInput)
                .then(|| attribute(element, "value").map(str::to_owned))
                .flatten()
        })
        .or_else(|| is_text_role(&element.role).then(|| element_text_content(element)))?;
    let text = bounded_utf8(text, MAX_TEXT_BYTES);
    (!text.is_empty() || matches!(element.role, UiRole::TextInput)).then_some(ElementText {
        text,
        redacted: false,
        editable,
    })
}

fn element_value(
    element: &RenderElement,
    properties: &HashMap<UiProperty, StateValue>,
    bindings: &[Binding],
) -> Option<ElementValue> {
    if is_password(element) {
        return None;
    }
    properties
        .get(&UiProperty::Value)
        .map(|value| ElementValue {
            value: bounded_utf8(value.display(), MAX_TEXT_BYTES),
            editable: is_text_editable(element) && has_writable_text_binding(bindings),
        })
        .or_else(|| {
            element
                .form_control
                .as_ref()?
                .value
                .as_ref()
                .map(|value| ElementValue {
                    value: bounded_utf8(value.to_string(), MAX_TEXT_BYTES),
                    editable: is_text_editable(element) && has_writable_text_binding(bindings),
                })
        })
}

fn has_writable_text_binding(bindings: &[Binding]) -> bool {
    bindings.iter().any(|binding| {
        matches!(
            binding,
            Binding::Event {
                event: UiEvent::Input | UiEvent::Change,
                ..
            } | Binding::Property {
                property: UiProperty::Text | UiProperty::Value,
                mode: BindingMode::TwoWay,
                ..
            }
        )
    })
}

fn is_password(element: &RenderElement) -> bool {
    element.source_tag == "input" && attribute(element, "type") == Some("password")
}

const fn is_editable_role(role: Option<AccessibleRole>) -> bool {
    matches!(
        role,
        Some(
            AccessibleRole::TextInput
                | AccessibleRole::MultilineTextInput
                | AccessibleRole::SearchInput
                | AccessibleRole::EmailInput
                | AccessibleRole::PasswordInput
                | AccessibleRole::PhoneNumberInput
                | AccessibleRole::UrlInput
        )
    )
}

fn is_text_role(role: &UiRole) -> bool {
    matches!(
        role,
        UiRole::Inline | UiRole::Paragraph | UiRole::Heading(_) | UiRole::Label | UiRole::Legend
    )
}

fn element_text_content(element: &RenderElement) -> String {
    fn collect(nodes: &[RenderNode], text: &mut String) {
        for node in nodes {
            match node {
                RenderNode::Text(value) => {
                    for word in value.value.split_whitespace() {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(word);
                    }
                }
                RenderNode::Element(element) => collect(&element.children, text),
                RenderNode::Raw(_) => {}
            }
        }
    }

    let mut text = String::new();
    collect(&element.children, &mut text);
    bounded_utf8(text, MAX_TEXT_BYTES)
}

fn bounded_utf8(mut value: String, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn install_pointer_hooks(
    host: Div,
    runtime_id: &str,
    element_id: &ElementId,
    bindings: &[Binding],
    hooks: &HookRegistry,
    enabled: bool,
    toggle: Option<ToggleBinding>,
) -> Stateful<Div> {
    let mut host = host.id(SharedString::from(runtime_id.to_owned()));
    if !enabled {
        return host;
    }
    let click = event_handlers(bindings, &[UiEvent::Click, UiEvent::Submit]);
    let double_click = event_handlers(bindings, &[UiEvent::DoubleClick]);
    if !click.is_empty() || !double_click.is_empty() || toggle.is_some() {
        let hooks = hooks.clone();
        let element_id = element_id.clone();
        let bindings = bindings.to_vec();
        host = host.on_click(move |pointer, window, cx| {
            for (event, handler) in &click {
                let hook_event = HookEvent::new(element_id.clone(), *event, None);
                let _ = hooks.invoke(handler, &hook_event, window, cx);
            }
            if pointer.click_count() >= 2 {
                for (event, handler) in &double_click {
                    let hook_event = HookEvent::new(element_id.clone(), *event, None);
                    let _ = hooks.invoke(handler, &hook_event, window, cx);
                }
            }
            if let Some(toggle) = toggle {
                for binding in &bindings {
                    let Binding::Property {
                        property,
                        source,
                        mode: BindingMode::TwoWay,
                        ..
                    } = binding
                    else {
                        continue;
                    };
                    if *property == toggle.property {
                        let _ = hooks.write(source, StateValue::Boolean(toggle.value), window, cx);
                    }
                }
            }
        });
    }
    let hover = event_handlers(bindings, &[UiEvent::Hover]);
    if !hover.is_empty() {
        let hooks = hooks.clone();
        let element_id = element_id.clone();
        host = host.on_hover(move |hovered, window, cx| {
            if *hovered {
                for (event, handler) in &hover {
                    let hook_event = HookEvent::new(element_id.clone(), *event, None);
                    let _ = hooks.invoke(handler, &hook_event, window, cx);
                }
            }
        });
    }
    host
}

#[derive(Clone, Copy)]
struct ToggleBinding {
    property: UiProperty,
    value: bool,
}

fn semantic_toggle(
    element: &RenderElement,
    state: &ElementState,
    bindings: &[Binding],
) -> Option<ToggleBinding> {
    let (property, value) = match accessible_role(element) {
        Some(AccessibleRole::CheckBox | AccessibleRole::Switch) => {
            (UiProperty::Checked, !state.checked.unwrap_or(false))
        }
        Some(AccessibleRole::RadioButton) => (UiProperty::Checked, true),
        Some(AccessibleRole::ListBoxOption | AccessibleRole::MenuListOption) => {
            (UiProperty::Selected, true)
        }
        _ => return None,
    };
    bindings
        .iter()
        .any(|binding| {
            matches!(
                binding,
                Binding::Property {
                    property: bound_property,
                    mode: BindingMode::TwoWay,
                    ..
                } if *bound_property == property
            )
        })
        .then_some(ToggleBinding { property, value })
}

fn event_handlers(bindings: &[Binding], events: &[UiEvent]) -> Vec<(UiEvent, HandlerId)> {
    bindings
        .iter()
        .filter_map(|binding| {
            let Binding::Event { event, handler, .. } = binding else {
                return None;
            };
            events.contains(event).then(|| (*event, handler.clone()))
        })
        .collect()
}

pub(crate) fn dispatch_input_change(
    hooks: &HookRegistry,
    element_id: &ElementId,
    bindings: &[Binding],
    text: String,
    window: &mut Window,
    cx: &mut App,
) -> HookOutcome {
    let value = StateValue::Text(text);

    let mut handled = false;
    for binding in bindings {
        let Binding::Property {
            property: UiProperty::Text | UiProperty::Value,
            source,
            mode: BindingMode::TwoWay,
            ..
        } = binding
        else {
            continue;
        };
        let outcome = hooks.write(source, value.clone(), window, cx);
        if let HookOutcome::Rejected { .. } = outcome {
            return outcome;
        }
        handled = true;
    }
    for (bound_event, handler) in event_handlers(bindings, &[UiEvent::Input, UiEvent::Change]) {
        let hook_event = HookEvent::new(element_id.clone(), bound_event, Some(value.clone()));
        let outcome = hooks.invoke(&handler, &hook_event, window, cx);
        if let HookOutcome::Rejected { .. } = outcome {
            return outcome;
        }
        handled = true;
    }
    if handled {
        HookOutcome::Handled
    } else {
        HookOutcome::Rejected {
            reason: "no compatible binding handled the action".to_owned(),
        }
    }
}

fn apply_styles(
    host: Div,
    element: &RenderElement,
    viewport: MediaViewport,
    available_fonts: &HashSet<String>,
) -> Div {
    let host = apply_declarations(
        host,
        element
            .stylesheet_declarations
            .iter()
            .chain(&element.styles),
        available_fonts,
    );
    apply_media_variants(host, &element.style_variants, viewport, available_fonts)
}

fn apply_media_variants<T: Styled>(
    host: T,
    variants: &[RenderStyleVariant],
    viewport: MediaViewport,
    available_fonts: &HashSet<String>,
) -> T {
    variants
        .iter()
        .filter(|variant| media_only_variant_matches(variant, viewport))
        .fold(host, |host, variant| {
            apply_declarations(host, &variant.declarations, available_fonts)
        })
}

fn apply_declarations<'a, T: Styled>(
    mut host: T,
    declarations: impl IntoIterator<Item = &'a StyleDeclaration>,
    available_fonts: &HashSet<String>,
) -> T {
    let declarations = declarations.into_iter().collect::<Vec<_>>();
    for declaration in &declarations {
        host = apply_style(host, declaration, available_fonts);
    }

    if declarations
        .iter()
        .any(|declaration| declaration.property == StyleProperty::BorderWidth)
    {
        host = match effective_border_style(&declarations) {
            BorderStyle::None => host.border(px(0.)),
            BorderStyle::Solid => host,
            BorderStyle::Dashed => host.border_dashed(),
        };
    }
    host
}

#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "the live renderer deliberately implements a documented CSS subset"
)]
fn apply_style<T: Styled>(
    host: T,
    declaration: &StyleDeclaration,
    available_fonts: &HashSet<String>,
) -> T {
    macro_rules! with_value {
        ($value:expr, |$binding:ident| $expression:expr) => {
            match $value {
                Some($binding) => $expression,
                None => host,
            }
        };
    }
    let raw_value = declaration.value.as_str().trim();
    let value = raw_value.to_ascii_lowercase();
    if layout_property(&declaration.property) {
        return apply_layout_style(host, &declaration.property, &value);
    }
    match declaration.property {
        StyleProperty::Background | StyleProperty::BackgroundColor => {
            with_value!(color(&value), |value| host.bg(rgba(value)))
        }
        StyleProperty::Color => {
            with_value!(color(&value), |value| host.text_color(rgba(value)))
        }
        StyleProperty::FontSize => {
            with_value!(length_px(&value), |value| host.text_size(px(value)))
        }
        StyleProperty::FontFamily => {
            with_value!(font_family(raw_value, available_fonts), |value| {
                apply_font_family(host, value)
            })
        }
        StyleProperty::FontWeight => {
            with_value!(font_weight(&value), |value| host.font_weight(value))
        }
        StyleProperty::LineHeight => match line_height(&value) {
            Some(LineHeight::Pixels(value)) => host.line_height(px(value)),
            Some(LineHeight::Relative(value)) => host.line_height(relative(value)),
            Some(LineHeight::Normal) | None => host,
        },
        StyleProperty::WhiteSpace => match value.as_str() {
            "nowrap" | "pre" => host.whitespace_nowrap(),
            // GPUI currently exposes wrapping rather than CSS's full whitespace-collapse matrix.
            "normal" | "pre-wrap" | "pre-line" | "break-spaces" => host.whitespace_normal(),
            _ => host,
        },
        StyleProperty::TextAlign => match value.as_str() {
            "left" | "start" => host.text_left(),
            "center" => host.text_center(),
            "right" | "end" => host.text_right(),
            _ => host,
        },
        StyleProperty::TextOverflow if value == "ellipsis" => host.text_ellipsis(),
        StyleProperty::Opacity => with_value!(opacity(&value), |value| host.opacity(value)),
        StyleProperty::BoxShadow => {
            with_value!(box_shadows(&value), |value| host.shadow(value))
        }
        StyleProperty::Cursor => apply_cursor(host, &value),
        // GPUI's definite dimensions already use border-box sizing.
        StyleProperty::BoxSizing if value == "border-box" => host,
        StyleProperty::BorderWidth => {
            with_value!(length_px(&value), |value| host.border(px(value)))
        }
        StyleProperty::Border => with_value!(border_value(&value), |value| {
            apply_border(host, value, BorderSide::All)
        }),
        StyleProperty::BorderTop => with_value!(border_value(&value), |value| {
            apply_border(host, value, BorderSide::Top)
        }),
        StyleProperty::BorderRight => with_value!(border_value(&value), |value| {
            apply_border(host, value, BorderSide::Right)
        }),
        StyleProperty::BorderBottom => with_value!(border_value(&value), |value| {
            apply_border(host, value, BorderSide::Bottom)
        }),
        StyleProperty::BorderLeft => with_value!(border_value(&value), |value| {
            apply_border(host, value, BorderSide::Left)
        }),
        StyleProperty::BorderColor => {
            with_value!(color(&value), |value| host.border_color(rgba(value)))
        }
        StyleProperty::BorderRadius => {
            with_value!(length_px(&value), |value| host.rounded(px(value)))
        }
        StyleProperty::Outline if matches!(value.as_str(), "none" | "0") => host,
        _ => host,
    }
}

fn layout_property(property: &StyleProperty) -> bool {
    matches!(
        property,
        StyleProperty::Display
            | StyleProperty::FlexDirection
            | StyleProperty::FlexWrap
            | StyleProperty::AlignItems
            | StyleProperty::AlignSelf
            | StyleProperty::JustifyContent
            | StyleProperty::GridTemplateColumns
            | StyleProperty::GridColumn
            | StyleProperty::Position
            | StyleProperty::Top
            | StyleProperty::Right
            | StyleProperty::Bottom
            | StyleProperty::Left
            | StyleProperty::Inset
            | StyleProperty::Gap
            | StyleProperty::Padding
            | StyleProperty::PaddingTop
            | StyleProperty::PaddingRight
            | StyleProperty::PaddingBottom
            | StyleProperty::PaddingLeft
            | StyleProperty::Margin
            | StyleProperty::MarginTop
            | StyleProperty::MarginRight
            | StyleProperty::MarginBottom
            | StyleProperty::MarginLeft
            | StyleProperty::Width
            | StyleProperty::Height
            | StyleProperty::MinWidth
            | StyleProperty::MinHeight
            | StyleProperty::MaxWidth
            | StyleProperty::MaxHeight
            | StyleProperty::Flex
            | StyleProperty::FlexBasis
            | StyleProperty::FlexGrow
            | StyleProperty::FlexShrink
            | StyleProperty::Overflow
            | StyleProperty::OverflowX
            | StyleProperty::OverflowY
    )
}

#[allow(
    clippy::too_many_lines,
    clippy::wildcard_enum_match_arm,
    reason = "keeping the CSS-to-GPUI layout mapping together makes its supported subset auditable"
)]
fn apply_layout_style<T: Styled>(host: T, property: &StyleProperty, value: &str) -> T {
    macro_rules! with_value {
        ($value:expr, |$binding:ident| $expression:expr) => {
            match $value {
                Some($binding) => $expression,
                None => host,
            }
        };
    }
    match property {
        StyleProperty::Display => match value {
            "flex" | "inline-flex" => host.flex(),
            "grid" => host.grid(),
            "block" | "inline" | "inline-block" => host.block(),
            "none" => host.hidden(),
            _ => host,
        },
        StyleProperty::FlexDirection => match value {
            "column" => host.flex_col(),
            "column-reverse" => host.flex_col_reverse(),
            "row-reverse" => host.flex_row_reverse(),
            _ => host.flex_row(),
        },
        StyleProperty::FlexWrap => match value {
            "wrap" => host.flex_wrap(),
            "wrap-reverse" => host.flex_wrap_reverse(),
            _ => host.flex_nowrap(),
        },
        StyleProperty::AlignItems => match value {
            "start" | "flex-start" => host.items_start(),
            "end" | "flex-end" => host.items_end(),
            "center" => host.items_center(),
            "baseline" => host.items_baseline(),
            "stretch" => apply_align_items(host, AlignItems::Stretch),
            _ => host,
        },
        StyleProperty::AlignSelf => apply_align_self(host, value),
        StyleProperty::JustifyContent => match value {
            "start" | "flex-start" => host.justify_start(),
            "end" | "flex-end" => host.justify_end(),
            "center" => host.justify_center(),
            "space-between" => host.justify_between(),
            "space-around" => host.justify_around(),
            _ => host,
        },
        StyleProperty::GridTemplateColumns => {
            with_value!(grid_column_count(value), |value| host.grid_cols(value))
        }
        StyleProperty::GridColumn => {
            with_value!(grid_column(value), |value| apply_grid_column(host, value))
        }
        StyleProperty::Position => match value {
            "relative" => host.relative(),
            "absolute" => host.absolute(),
            _ => host,
        },
        StyleProperty::Top => with_value!(length(value), |value| host.top(value)),
        StyleProperty::Right => with_value!(length(value), |value| host.right(value)),
        StyleProperty::Bottom => with_value!(length(value), |value| host.bottom(value)),
        StyleProperty::Left => with_value!(length(value), |value| host.left(value)),
        StyleProperty::Inset => with_value!(box_values(value, length), |value| {
            host.top(value[0])
                .right(value[1])
                .bottom(value[2])
                .left(value[3])
        }),
        StyleProperty::Gap => with_value!(definite_length(value), |value| host.gap(value)),
        StyleProperty::Padding => with_value!(box_values(value, definite_length), |value| {
            host.pt(value[0]).pr(value[1]).pb(value[2]).pl(value[3])
        }),
        StyleProperty::PaddingTop => with_value!(definite_length(value), |value| host.pt(value)),
        StyleProperty::PaddingRight => with_value!(definite_length(value), |value| host.pr(value)),
        StyleProperty::PaddingBottom => with_value!(definite_length(value), |value| host.pb(value)),
        StyleProperty::PaddingLeft => with_value!(definite_length(value), |value| host.pl(value)),
        StyleProperty::Margin => with_value!(box_values(value, length), |value| {
            host.mt(value[0]).mr(value[1]).mb(value[2]).ml(value[3])
        }),
        StyleProperty::MarginTop => with_value!(length(value), |value| host.mt(value)),
        StyleProperty::MarginRight => with_value!(length(value), |value| host.mr(value)),
        StyleProperty::MarginBottom => with_value!(length(value), |value| host.mb(value)),
        StyleProperty::MarginLeft => with_value!(length(value), |value| host.ml(value)),
        StyleProperty::Width => with_value!(length(value), |value| host.w(value)),
        StyleProperty::Height => with_value!(length(value), |value| host.h(value)),
        StyleProperty::MinWidth => with_value!(length(value), |value| host.min_w(value)),
        StyleProperty::MinHeight => with_value!(length(value), |value| host.min_h(value)),
        StyleProperty::MaxWidth => with_value!(length(value), |value| host.max_w(value)),
        StyleProperty::MaxHeight => with_value!(length(value), |value| host.max_h(value)),
        StyleProperty::Flex => {
            with_value!(flex_value(value), |value| apply_flex_value(host, value))
        }
        StyleProperty::FlexBasis => with_value!(length(value), |value| host.flex_basis(value)),
        StyleProperty::FlexGrow => {
            with_value!(flex_factor(value), |value| apply_flex_grow(host, value))
        }
        StyleProperty::FlexShrink => {
            with_value!(flex_factor(value), |value| apply_flex_shrink(host, value))
        }
        StyleProperty::Overflow => with_value!(overflow(value), |value| {
            apply_overflow(host, value, OverflowAxis::Both)
        }),
        StyleProperty::OverflowX => with_value!(overflow(value), |value| {
            apply_overflow(host, value, OverflowAxis::X)
        }),
        StyleProperty::OverflowY => with_value!(overflow(value), |value| {
            apply_overflow(host, value, OverflowAxis::Y)
        }),
        _ => host,
    }
}

fn grid_column_count(value: &str) -> Option<u16> {
    let tracks = if let Some(inner) = value
        .strip_prefix("repeat(")
        .and_then(|value| value.strip_suffix(')'))
    {
        let (count, track) = inner.split_once(',')?;
        if track.trim() != "1fr" {
            return None;
        }
        return count.trim().parse().ok().filter(|count| *count > 0);
    } else {
        value.split_whitespace().collect::<Vec<_>>()
    };
    let count = u16::try_from(tracks.len()).ok()?;
    (count > 0 && tracks.iter().all(|track| *track == "1fr")).then_some(count)
}

fn grid_column(value: &str) -> Option<Range<GridPlacement>> {
    let (start, end) = value
        .split_once('/')
        .map_or((value.trim(), "auto"), |(start, end)| {
            (start.trim(), end.trim())
        });
    Some(grid_placement(start)?..grid_placement(end)?)
}

fn grid_placement(value: &str) -> Option<GridPlacement> {
    if value == "auto" {
        return Some(GridPlacement::Auto);
    }
    if let Some(span) = value.strip_prefix("span ") {
        return span
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|span| *span > 0)
            .map(GridPlacement::Span);
    }
    value
        .parse::<i16>()
        .ok()
        .filter(|line| *line != 0)
        .map(GridPlacement::Line)
}

fn apply_grid_column<T: Styled>(mut host: T, value: Range<GridPlacement>) -> T {
    host.style().grid_location_mut().column = value;
    host
}

fn apply_align_self<T: Styled>(mut host: T, value: &str) -> T {
    host.style().align_self = match value {
        "auto" => None,
        "start" => Some(AlignSelf::Start),
        "end" => Some(AlignSelf::End),
        "flex-start" => Some(AlignSelf::FlexStart),
        "flex-end" => Some(AlignSelf::FlexEnd),
        "center" => Some(AlignSelf::Center),
        "baseline" => Some(AlignSelf::Baseline),
        "stretch" => Some(AlignSelf::Stretch),
        _ => return host,
    };
    host
}

fn apply_align_items<T: Styled>(mut host: T, value: AlignItems) -> T {
    host.style().align_items = Some(value);
    host
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MediaViewport {
    width: f32,
    height: f32,
}

impl MediaViewport {
    fn from_window(window: &Window) -> Self {
        let size = window.viewport_size();
        Self {
            width: size.width.into(),
            height: size.height.into(),
        }
    }
}

fn media_only_variant_supported(variant: &RenderStyleVariant) -> bool {
    !variant.conditions.is_empty()
        && variant.conditions.iter().all(|condition| {
            let RenderStyleCondition::Media(query) = condition else {
                return false;
            };
            media_query_matches(
                query,
                MediaViewport {
                    width: 1024.0,
                    height: 768.0,
                },
            )
            .is_some()
        })
}

fn media_only_variant_matches(variant: &RenderStyleVariant, viewport: MediaViewport) -> bool {
    media_only_variant_supported(variant) && media_conditions_match(variant, viewport)
}

fn media_conditions_match(variant: &RenderStyleVariant, viewport: MediaViewport) -> bool {
    variant.conditions.iter().all(|condition| match condition {
        RenderStyleCondition::Media(query) => media_query_matches(query, viewport).unwrap_or(false),
        RenderStyleCondition::PseudoClass(_) => true,
        RenderStyleCondition::PseudoElement(_)
        | RenderStyleCondition::Supports(_)
        | RenderStyleCondition::Container(_) => false,
    })
}

fn media_query_matches(query: &str, viewport: MediaViewport) -> Option<bool> {
    query
        .to_ascii_lowercase()
        .split(',')
        .map(|branch| media_query_branch_matches(branch.trim(), viewport))
        .collect::<Option<Vec<_>>>()
        .map(|branches| branches.into_iter().any(|matches| matches))
}

fn media_query_branch_matches(query: &str, viewport: MediaViewport) -> Option<bool> {
    if query.is_empty() {
        return None;
    }
    let query = query.strip_prefix("only ").unwrap_or(query);
    if query.starts_with("not ") {
        return None;
    }
    query
        .split(" and ")
        .map(str::trim)
        .filter(|part| !matches!(*part, "all" | "screen"))
        .map(|part| media_query_part_matches(part, viewport))
        .collect::<Option<Vec<_>>>()
        .map(|parts| parts.into_iter().all(|matches| matches))
}

fn media_query_part_matches(part: &str, viewport: MediaViewport) -> Option<bool> {
    let feature = part.strip_prefix('(')?.strip_suffix(')')?.trim();
    if let Some(matches) = media_range_matches(feature, viewport) {
        return Some(matches);
    }
    let (name, value) = feature.split_once(':')?;
    let name = name.trim();
    let value = value.trim();
    match name {
        "min-width" => Some(viewport.width >= media_length(value)?),
        "max-width" => Some(viewport.width <= media_length(value)?),
        "width" => Some((viewport.width - media_length(value)?).abs() < f32::EPSILON),
        "min-height" => Some(viewport.height >= media_length(value)?),
        "max-height" => Some(viewport.height <= media_length(value)?),
        "height" => Some((viewport.height - media_length(value)?).abs() < f32::EPSILON),
        "orientation" if value == "landscape" => Some(viewport.width >= viewport.height),
        "orientation" if value == "portrait" => Some(viewport.height > viewport.width),
        // GPUI Studio targets desktop windows, where hover and a fine pointer are available.
        "hover" | "any-hover" => Some(value == "hover"),
        "pointer" | "any-pointer" => Some(value == "fine"),
        _ => None,
    }
}

fn media_range_matches(feature: &str, viewport: MediaViewport) -> Option<bool> {
    for operator in ["<=", ">=", "<", ">", "="] {
        let Some((left, right)) = feature.split_once(operator) else {
            continue;
        };
        let left = left.trim();
        let right = right.trim();
        if let Some(actual) = media_dimension(left, viewport) {
            return Some(compare_media_range(actual, media_length(right)?, operator));
        }
        if let Some(actual) = media_dimension(right, viewport) {
            return Some(compare_media_range(media_length(left)?, actual, operator));
        }
        return None;
    }
    None
}

fn media_dimension(value: &str, viewport: MediaViewport) -> Option<f32> {
    match value {
        "width" => Some(viewport.width),
        "height" => Some(viewport.height),
        _ => None,
    }
}

fn compare_media_range(left: f32, right: f32, operator: &str) -> bool {
    match operator {
        "<=" => left <= right,
        ">=" => left >= right,
        "<" => left < right,
        ">" => left > right,
        "=" => (left - right).abs() < f32::EPSILON,
        _ => false,
    }
}

fn media_length(value: &str) -> Option<f32> {
    value
        .strip_suffix("px")
        .and_then(|value| value.trim().parse::<f32>().ok())
        .or_else(|| {
            value
                .strip_suffix("rem")
                .or_else(|| value.strip_suffix("em"))
                .and_then(|value| value.trim().parse::<f32>().ok())
                .map(|value| value * 16.0)
        })
        .filter(|value| value.is_finite() && *value >= 0.0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InteractiveStyle {
    Hover,
    Focus,
    Active,
}

fn interactive_style(variant: &RenderStyleVariant) -> Option<InteractiveStyle> {
    let mut pseudo_class = None;
    for condition in &variant.conditions {
        match condition {
            RenderStyleCondition::PseudoClass(value) if pseudo_class.is_none() => {
                pseudo_class = Some(value.as_str());
            }
            RenderStyleCondition::Media(query)
                if media_query_matches(
                    query,
                    MediaViewport {
                        width: 1024.0,
                        height: 768.0,
                    },
                )
                .is_some() => {}
            _ => return None,
        }
    }
    match pseudo_class? {
        "hover" => Some(InteractiveStyle::Hover),
        // GPUI exposes a single focus style hook. Preserve `:focus-visible`
        // through that hook instead of dropping the authored keyboard-focus
        // treatment from the live runtime.
        "focus" | "focus-visible" => Some(InteractiveStyle::Focus),
        "active" => Some(InteractiveStyle::Active),
        _ => None,
    }
}

fn has_interactive_style(element: &RenderElement, style: InteractiveStyle) -> bool {
    element
        .style_variants
        .iter()
        .any(|variant| interactive_style(variant) == Some(style))
}

fn apply_interactive_styles(
    mut host: Stateful<Div>,
    element: &RenderElement,
    forced_hover: bool,
    viewport: MediaViewport,
    available_fonts: &HashSet<String>,
) -> Stateful<Div> {
    let hover_variants = interactive_variants(element, InteractiveStyle::Hover, viewport);
    let focus_variants = interactive_variants(element, InteractiveStyle::Focus, viewport);
    let active_variants = interactive_variants(element, InteractiveStyle::Active, viewport);
    if forced_hover {
        host = apply_variant_declarations(host, &hover_variants, available_fonts);
    }
    if !hover_variants.is_empty() {
        host =
            host.hover(|style| apply_variant_declarations(style, &hover_variants, available_fonts));
    }
    if !focus_variants.is_empty() {
        host =
            host.focus(|style| apply_variant_declarations(style, &focus_variants, available_fonts));
    }
    if !active_variants.is_empty() {
        host = host
            .active(|style| apply_variant_declarations(style, &active_variants, available_fonts));
    }
    host
}

fn interactive_variants(
    element: &RenderElement,
    style: InteractiveStyle,
    viewport: MediaViewport,
) -> Vec<&RenderStyleVariant> {
    element
        .style_variants
        .iter()
        .filter(|variant| {
            interactive_style(variant) == Some(style) && media_conditions_match(variant, viewport)
        })
        .collect()
}

fn update_hovered_element(
    hovered_element: &Rc<RefCell<Option<ElementId>>>,
    element_id: &ElementId,
    hovered: bool,
) {
    let mut current = hovered_element.borrow_mut();
    if hovered {
        *current = Some(element_id.clone());
    } else if current.as_ref() == Some(element_id) {
        current.take();
    }
}

fn apply_variant_declarations<T: Styled>(
    host: T,
    variants: &[&RenderStyleVariant],
    available_fonts: &HashSet<String>,
) -> T {
    variants.iter().fold(host, |host, variant| {
        apply_declarations(host, &variant.declarations, available_fonts)
    })
}

fn length_px(value: &str) -> Option<f32> {
    if value == "0" {
        return Some(0.0);
    }
    value.strip_suffix("px")?.trim().parse().ok()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LineHeight {
    Normal,
    Pixels(f32),
    Relative(f32),
}

fn line_height(value: &str) -> Option<LineHeight> {
    let value = value.trim();
    if value == "normal" {
        return Some(LineHeight::Normal);
    }
    if let Some(value) = length_px(value) {
        return Some(LineHeight::Pixels(value));
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.)
            .map(|value| LineHeight::Relative(value / 100.));
    }
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.)
        .map(LineHeight::Relative)
}

fn definite_length(value: &str) -> Option<DefiniteLength> {
    if value == "0" {
        return Some(px(0.).into());
    }
    value.try_into().ok()
}

fn length(value: &str) -> Option<Length> {
    if value == "auto" {
        return Some(Length::Auto);
    }
    definite_length(value).map(Into::into)
}

fn box_values<T: Copy>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Option<[T; 4]> {
    let values = value
        .split_whitespace()
        .map(parse)
        .collect::<Option<Vec<_>>>()?;
    match values.as_slice() {
        [all] => Some([*all; 4]),
        [vertical, horizontal] => Some([*vertical, *horizontal, *vertical, *horizontal]),
        [top, horizontal, bottom] => Some([*top, *horizontal, *bottom, *horizontal]),
        [top, right, bottom, left] => Some([*top, *right, *bottom, *left]),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FlexValue {
    grow: f32,
    shrink: f32,
    basis: Length,
}

fn flex_value(value: &str) -> Option<FlexValue> {
    match value {
        "none" => Some(FlexValue {
            grow: 0.,
            shrink: 0.,
            basis: Length::Auto,
        }),
        "auto" => Some(FlexValue {
            grow: 1.,
            shrink: 1.,
            basis: Length::Auto,
        }),
        "initial" => Some(FlexValue {
            grow: 0.,
            shrink: 1.,
            basis: Length::Auto,
        }),
        _ => {
            let tokens = value.split_whitespace().collect::<Vec<_>>();
            match tokens.as_slice() {
                [grow] => Some(FlexValue {
                    grow: flex_factor(grow)?,
                    shrink: 1.,
                    basis: definite_length("0%").map(Into::into)?,
                }),
                [grow, second] => {
                    let grow = flex_factor(grow)?;
                    if let Some(shrink) = flex_factor(second) {
                        Some(FlexValue {
                            grow,
                            shrink,
                            basis: definite_length("0%").map(Into::into)?,
                        })
                    } else {
                        Some(FlexValue {
                            grow,
                            shrink: 1.,
                            basis: length(second)?,
                        })
                    }
                }
                [grow, shrink, basis] => Some(FlexValue {
                    grow: flex_factor(grow)?,
                    shrink: flex_factor(shrink)?,
                    basis: length(basis)?,
                }),
                _ => None,
            }
        }
    }
}

fn flex_factor(value: &str) -> Option<f32> {
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.)
}

fn apply_flex_value<T: Styled>(mut host: T, value: FlexValue) -> T {
    host.style().flex_grow = Some(value.grow);
    host.style().flex_shrink = Some(value.shrink);
    host.style().flex_basis = Some(value.basis);
    host
}

fn apply_flex_grow<T: Styled>(mut host: T, value: f32) -> T {
    host.style().flex_grow = Some(value);
    host
}

fn apply_flex_shrink<T: Styled>(mut host: T, value: f32) -> T {
    host.style().flex_shrink = Some(value);
    host
}

fn opacity(value: &str) -> Option<f32> {
    value
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
}

fn apply_cursor<T: Styled>(host: T, value: &str) -> T {
    match value {
        "auto" | "default" => host.cursor_default(),
        "pointer" => host.cursor_pointer(),
        "text" => host.cursor_text(),
        "move" => host.cursor_move(),
        "not-allowed" | "no-drop" => host.cursor_not_allowed(),
        "context-menu" => host.cursor_context_menu(),
        "crosshair" => host.cursor_crosshair(),
        "vertical-text" => host.cursor_vertical_text(),
        "alias" => host.cursor_alias(),
        "copy" => host.cursor_copy(),
        "grab" => host.cursor_grab(),
        "grabbing" => host.cursor_grabbing(),
        "ew-resize" | "e-resize" | "w-resize" => host.cursor_ew_resize(),
        "ns-resize" | "n-resize" | "s-resize" => host.cursor_ns_resize(),
        "nesw-resize" | "ne-resize" | "sw-resize" => host.cursor_nesw_resize(),
        "nwse-resize" | "nw-resize" | "se-resize" => host.cursor_nwse_resize(),
        _ => host,
    }
}

fn cursor_supported(value: &str) -> bool {
    matches!(
        value,
        "auto"
            | "default"
            | "pointer"
            | "text"
            | "move"
            | "not-allowed"
            | "no-drop"
            | "context-menu"
            | "crosshair"
            | "vertical-text"
            | "alias"
            | "copy"
            | "grab"
            | "grabbing"
            | "ew-resize"
            | "e-resize"
            | "w-resize"
            | "ns-resize"
            | "n-resize"
            | "s-resize"
            | "nesw-resize"
            | "ne-resize"
            | "sw-resize"
            | "nwse-resize"
            | "nw-resize"
            | "se-resize"
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverflowAxis {
    Both,
    X,
    Y,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ScrollAxes {
    x: bool,
    y: bool,
}

impl ScrollAxes {
    const fn any(self) -> bool {
        self.x || self.y
    }
}

fn element_scroll_axes(element: &RenderElement, viewport: MediaViewport) -> ScrollAxes {
    let mut axes = ScrollAxes::default();
    for declaration in element
        .stylesheet_declarations
        .iter()
        .chain(&element.styles)
        .chain(
            element
                .style_variants
                .iter()
                .filter(|variant| media_only_variant_matches(variant, viewport))
                .flat_map(|variant| &variant.declarations),
        )
    {
        let is_scroll = matches!(
            overflow(&declaration.value.as_str().trim().to_ascii_lowercase()),
            Some(Overflow::Scroll)
        );
        match declaration.property {
            StyleProperty::Overflow => {
                axes = ScrollAxes {
                    x: is_scroll,
                    y: is_scroll,
                }
            }
            StyleProperty::OverflowX => axes.x = is_scroll,
            StyleProperty::OverflowY => axes.y = is_scroll,
            _ => {}
        }
    }
    axes
}

fn overflow(value: &str) -> Option<Overflow> {
    match value {
        "visible" => Some(Overflow::Visible),
        "clip" => Some(Overflow::Clip),
        "hidden" => Some(Overflow::Hidden),
        "auto" | "scroll" => Some(Overflow::Scroll),
        _ => None,
    }
}

fn apply_overflow<T: Styled>(mut host: T, value: Overflow, axis: OverflowAxis) -> T {
    if matches!(axis, OverflowAxis::Both | OverflowAxis::X) {
        host.style().overflow.x = Some(value);
    }
    if matches!(axis, OverflowAxis::Both | OverflowAxis::Y) {
        host.style().overflow.y = Some(value);
    }
    host
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BorderSide {
    All,
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BorderValue {
    width: f32,
    style: BorderStyle,
    color: Option<u32>,
}

fn border_value(value: &str) -> Option<BorderValue> {
    if matches!(value, "none" | "hidden") {
        return Some(BorderValue {
            width: 0.,
            style: BorderStyle::None,
            color: None,
        });
    }

    let mut width = None;
    let mut style = None;
    let mut parsed_color = None;
    for token in value.split_whitespace() {
        if let Some(value) = length_px(token) {
            width = Some(value);
        } else if let Some(value) = match token {
            "none" | "hidden" => Some(BorderStyle::None),
            "solid" => Some(BorderStyle::Solid),
            "dashed" => Some(BorderStyle::Dashed),
            _ => None,
        } {
            style = Some(value);
        } else if let Some(value) = color(token) {
            parsed_color = Some(value);
        } else {
            return None;
        }
    }

    let width = width?;
    Some(BorderValue {
        width,
        style: style.unwrap_or(BorderStyle::None),
        color: parsed_color,
    })
}

fn apply_border<T: Styled>(mut host: T, value: BorderValue, side: BorderSide) -> T {
    let width = if value.style == BorderStyle::None {
        0.
    } else {
        value.width
    };
    host = match side {
        BorderSide::All => host.border(px(width)),
        BorderSide::Top => host.border_t(px(width)),
        BorderSide::Right => host.border_r(px(width)),
        BorderSide::Bottom => host.border_b(px(width)),
        BorderSide::Left => host.border_l(px(width)),
    };
    if let Some(value) = value.color {
        host = host.border_color(rgba(value));
    }
    if value.style == BorderStyle::Dashed {
        host = host.border_dashed();
    }
    host
}

fn box_shadows(value: &str) -> Option<Vec<BoxShadow>> {
    if matches!(value.trim(), "none" | "0") {
        return Some(Vec::new());
    }
    split_top_level(value, ',')
        .into_iter()
        .map(single_box_shadow)
        .collect()
}

fn single_box_shadow(value: &str) -> Option<BoxShadow> {
    let mut lengths = Vec::new();
    let mut parsed_color = None;
    for token in split_top_level_whitespace(value) {
        if token.eq_ignore_ascii_case("inset") {
            return None;
        }
        if let Some(value) = length_px(token) {
            lengths.push(value);
        } else if parsed_color.is_none() {
            parsed_color = color(token);
            parsed_color?;
        } else {
            return None;
        }
    }
    if !(2..=4).contains(&lengths.len()) {
        return None;
    }
    Some(BoxShadow {
        color: rgba(parsed_color.unwrap_or(0x0000_00ff)).into(),
        offset: point(px(lengths[0]), px(lengths[1])),
        blur_radius: px(lengths.get(2).copied().unwrap_or(0.)),
        spread_radius: px(lengths.get(3).copied().unwrap_or(0.)),
        inset: false,
    })
}

fn split_top_level(value: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0u32;
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            _ if character == delimiter && depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(value[start..].trim());
    parts
}

fn split_top_level_whitespace(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut depth = 0u32;
    for (index, character) in value.char_indices() {
        match character {
            '(' => {
                depth = depth.saturating_add(1);
                start.get_or_insert(index);
            }
            ')' => depth = depth.saturating_sub(1),
            _ if character.is_whitespace() && depth == 0 => {
                if let Some(start_index) = start.take() {
                    parts.push(&value[start_index..index]);
                }
            }
            _ => {
                start.get_or_insert(index);
            }
        }
    }
    if let Some(start) = start {
        parts.push(&value[start..]);
    }
    parts
}

fn color(value: &str) -> Option<u32> {
    let value = value.trim();
    if let Some(arguments) = value
        .strip_prefix("rgba(")
        .and_then(|value| value.strip_suffix(')'))
    {
        let channels = arguments.split(',').map(str::trim).collect::<Vec<_>>();
        let [red, green, blue, alpha] = channels.as_slice() else {
            return None;
        };
        let channel = |value: &str| value.parse::<u8>().ok();
        let alpha = alpha
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))?;
        let alpha = format!("{:.0}", alpha * 255.).parse::<u8>().ok()?;
        return Some(
            (u32::from(channel(red)?) << 24)
                | (u32::from(channel(green)?) << 16)
                | (u32::from(channel(blue)?) << 8)
                | u32::from(alpha),
        );
    }
    let hex = value.strip_prefix('#');
    match hex.map(str::len) {
        Some(3) => {
            let value = hex?;
            let mut expanded = String::with_capacity(8);
            for character in value.chars() {
                expanded.push(character);
                expanded.push(character);
            }
            expanded.push_str("ff");
            u32::from_str_radix(&expanded, 16).ok()
        }
        Some(4) => {
            let value = hex?;
            let mut expanded = String::with_capacity(8);
            for character in value.chars() {
                expanded.push(character);
                expanded.push(character);
            }
            u32::from_str_radix(&expanded, 16).ok()
        }
        Some(6) => u32::from_str_radix(&format!("{}ff", hex?), 16).ok(),
        Some(8) => u32::from_str_radix(hex?, 16).ok(),
        _ => match value {
            "transparent" => Some(0x0000_0000),
            "black" => Some(0x0000_00ff),
            "white" => Some(0xffff_ffff),
            "red" => Some(0xff00_00ff),
            "green" => Some(0x0080_00ff),
            "blue" => Some(0x0000_ffff),
            _ => None,
        },
    }
}

fn font_weight(value: &str) -> Option<FontWeight> {
    match value {
        "normal" => Some(FontWeight::NORMAL),
        "bold" => Some(FontWeight::BOLD),
        _ => value
            .parse::<f32>()
            .ok()
            .filter(|value| (100.0..=900.0).contains(value))
            .map(FontWeight),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FontFamily {
    primary: String,
    fallbacks: Vec<String>,
}

fn available_fonts(cx: &App) -> HashSet<String> {
    cx.text_system()
        .all_font_names()
        .into_iter()
        .map(|family| family.to_ascii_lowercase())
        .collect()
}

fn font_family(value: &str, available_fonts: &HashSet<String>) -> Option<FontFamily> {
    let families = value
        .split(',')
        .map(str::trim)
        .map(|family| family.trim_matches(['\'', '"']))
        .filter(|family| !family.is_empty())
        .map(normalize_font_family)
        .collect::<Vec<_>>();
    if families.is_empty() {
        return None;
    }
    let selected = families
        .iter()
        .position(|family| available_fonts.contains(&family.to_ascii_lowercase()));
    let primary_index = selected.unwrap_or(families.len());
    let primary = selected.map_or_else(
        || ".SystemUIFont".to_owned(),
        |index| families[index].clone(),
    );
    let fallbacks = families
        .into_iter()
        .skip(primary_index.saturating_add(1))
        .collect();
    Some(FontFamily { primary, fallbacks })
}

fn normalize_font_family(family: &str) -> String {
    if matches!(
        family.to_ascii_lowercase().as_str(),
        "system-ui"
            | "ui-sans-serif"
            | "ui-serif"
            | "ui-monospace"
            | "sans-serif"
            | "serif"
            | "monospace"
    ) {
        ".SystemUIFont".to_owned()
    } else {
        family.to_owned()
    }
}

fn apply_font_family<T: Styled>(mut host: T, family: FontFamily) -> T {
    let text_style = host.text_style();
    text_style.font_family = Some(SharedString::from(family.primary));
    text_style.font_fallbacks =
        (!family.fallbacks.is_empty()).then(|| FontFallbacks::from_fonts(family.fallbacks));
    host
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BorderStyle {
    None,
    Solid,
    Dashed,
}

fn effective_border_style(declarations: &[&StyleDeclaration]) -> BorderStyle {
    declarations
        .iter()
        .rev()
        .find(|declaration| declaration.property == StyleProperty::BorderStyle)
        .map_or(BorderStyle::None, |declaration| {
            match declaration
                .value
                .as_str()
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "solid" => BorderStyle::Solid,
                "dashed" => BorderStyle::Dashed,
                _ => BorderStyle::None,
            }
        })
}

fn collect_render_diagnostics(plan: &RenderPlan) -> Vec<RenderDiagnostic> {
    let mut diagnostics = Vec::new();
    collect_declaration_diagnostics("html-root", &plan.root.styles, &mut diagnostics);
    for variant in &plan.root.style_variants {
        if media_only_variant_supported(variant) {
            collect_declaration_diagnostics("html-root", &variant.declarations, &mut diagnostics);
        } else {
            diagnostics.push(RenderDiagnostic {
                node_id: "html-root".to_owned(),
                feature: "conditional CSS".to_owned(),
                message:
                    "root style condition is preserved but unsupported by the live GPUI renderer"
                        .to_owned(),
            });
        }
    }
    collect_node_diagnostics(&plan.nodes, &mut Vec::new(), &mut diagnostics);
    diagnostics
}

fn collect_node_diagnostics(
    nodes: &[RenderNode],
    path: &mut Vec<usize>,
    diagnostics: &mut Vec<RenderDiagnostic>,
) {
    for (index, node) in nodes.iter().enumerate() {
        let RenderNode::Element(element) = node else {
            continue;
        };
        path.push(index);
        let id = attribute(element, "id").map_or_else(|| generated_id(path), str::to_owned);
        let declarations = element
            .stylesheet_declarations
            .iter()
            .chain(&element.styles)
            .cloned()
            .collect::<Vec<_>>();
        collect_declaration_diagnostics(&id, &declarations, diagnostics);
        for variant in &element.style_variants {
            if interactive_style(variant).is_some() || media_only_variant_supported(variant) {
                collect_declaration_diagnostics(&id, &variant.declarations, diagnostics);
            } else {
                diagnostics.push(RenderDiagnostic {
                    node_id: id.clone(),
                    feature: "conditional CSS".to_owned(),
                    message: format!(
                        "live renderer does not support style conditions {:?}",
                        variant.conditions
                    ),
                });
            }
        }
        if !element.dynamic_styles.is_empty() || !element.pseudo_elements.is_empty() {
            diagnostics.push(RenderDiagnostic {
                node_id: id.clone(),
                feature: "dynamic or pseudo-element CSS".to_owned(),
                message: "dynamic styles and pseudo-elements are preserved but unsupported by the live GPUI renderer"
                    .to_owned(),
            });
        }
        collect_node_diagnostics(&element.children, path, diagnostics);
        path.pop();
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "ordered validation mirrors the renderer's explicit supported CSS subset"
)]
fn collect_declaration_diagnostics(
    node_id: &str,
    declarations: &[StyleDeclaration],
    diagnostics: &mut Vec<RenderDiagnostic>,
) {
    for declaration in declarations {
        let value = declaration.value.as_str().trim();
        let normalized = value.to_ascii_lowercase();
        if !supported_property(&declaration.property) {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message:
                    "CSS property is preserved in the document but unsupported by the live GPUI renderer"
                        .to_owned(),
            });
        } else if declaration.property == StyleProperty::BoxSizing
            && !value.eq_ignore_ascii_case("border-box")
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer supports only border-box sizing".to_owned(),
            });
        } else if declaration.property == StyleProperty::BorderStyle
            && !matches!(normalized.as_str(), "none" | "hidden" | "solid" | "dashed")
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer supports none, hidden, solid, and dashed borders"
                    .to_owned(),
            });
        } else if declaration.property == StyleProperty::GridTemplateColumns
            && grid_column_count(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer supports fixed counts of equal 1fr grid columns".to_owned(),
            });
        } else if declaration.property == StyleProperty::GridColumn
            && grid_column(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts auto, grid line numbers, and span counts"
                    .to_owned(),
            });
        } else if declaration.property == StyleProperty::Position
            && !matches!(normalized.as_str(), "relative" | "absolute")
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer supports relative and absolute positioning".to_owned(),
            });
        } else if declaration.property == StyleProperty::AlignSelf
            && !matches!(
                normalized.as_str(),
                "auto"
                    | "start"
                    | "end"
                    | "flex-start"
                    | "flex-end"
                    | "center"
                    | "baseline"
                    | "stretch"
            )
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer does not support this align-self value".to_owned(),
            });
        } else if declaration.property == StyleProperty::WhiteSpace
            && !matches!(
                normalized.as_str(),
                "normal" | "nowrap" | "pre" | "pre-wrap" | "pre-line" | "break-spaces"
            )
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer does not support this white-space value".to_owned(),
            });
        } else if declaration.property == StyleProperty::TextOverflow && normalized != "ellipsis" {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer supports ellipsis text overflow".to_owned(),
            });
        } else if declaration.property == StyleProperty::TextAlign
            && !matches!(
                normalized.as_str(),
                "left" | "start" | "center" | "right" | "end"
            )
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message:
                    "live renderer supports left, start, center, right, and end text alignment"
                        .to_owned(),
            });
        } else if declaration.property == StyleProperty::Cursor && !cursor_supported(&normalized) {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer does not support this cursor value".to_owned(),
            });
        } else if declaration.property == StyleProperty::Opacity && opacity(&normalized).is_none() {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts opacity from 0 through 1".to_owned(),
            });
        } else if declaration.property == StyleProperty::BoxShadow
            && box_shadows(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts non-inset pixel box shadows with CSS colors"
                    .to_owned(),
            });
        } else if declaration.property == StyleProperty::LineHeight
            && line_height(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts normal, unitless, percentage, zero, or pixel line heights"
                    .to_owned(),
            });
        } else if declaration.property == StyleProperty::Outline
            && !matches!(normalized.as_str(), "none" | "0")
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer supports only disabling the native outline".to_owned(),
            });
        } else if responsive_length_property(&declaration.property) && length(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts auto, zero, pixel, rem, or percentage lengths for this property"
                    .to_owned(),
            });
        } else if definite_length_property(&declaration.property)
            && definite_length(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts zero, pixel, rem, or percentage lengths for this property"
                    .to_owned(),
            });
        } else if pixel_length_property(&declaration.property) && length_px(&normalized).is_none() {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer currently accepts zero or pixel lengths for this property"
                    .to_owned(),
            });
        } else if (declaration.property == StyleProperty::Padding
            && box_values(&normalized, definite_length).is_none())
            || (matches!(
                declaration.property,
                StyleProperty::Margin | StyleProperty::Inset
            ) && box_values(&normalized, length).is_none())
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts one to four CSS box lengths".to_owned(),
            });
        } else if declaration.property == StyleProperty::Flex && flex_value(&normalized).is_none() {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: format!(
                    "live renderer accepts none, auto, initial, a grow factor, or grow shrink basis; received `{value}`"
                ),
            });
        } else if matches!(
            declaration.property,
            StyleProperty::FlexGrow | StyleProperty::FlexShrink
        ) && flex_factor(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts finite, non-negative flex factors".to_owned(),
            });
        } else if matches!(
            declaration.property,
            StyleProperty::Overflow | StyleProperty::OverflowX | StyleProperty::OverflowY
        ) && overflow(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: "live renderer accepts visible, clip, hidden, auto, or scroll overflow"
                    .to_owned(),
            });
        } else if border_shorthand_property(&declaration.property)
            && border_value(&normalized).is_none()
        {
            diagnostics.push(RenderDiagnostic {
                node_id: node_id.to_owned(),
                feature: declaration.property.to_string(),
                message: format!(
                    "live renderer accepts none or a pixel width, solid/dashed style, and color; received `{value}`"
                ),
            });
        }
    }
}

fn supported_property(property: &StyleProperty) -> bool {
    matches!(
        property,
        StyleProperty::Display
            | StyleProperty::FlexDirection
            | StyleProperty::FlexWrap
            | StyleProperty::Flex
            | StyleProperty::FlexBasis
            | StyleProperty::FlexGrow
            | StyleProperty::FlexShrink
            | StyleProperty::AlignItems
            | StyleProperty::AlignSelf
            | StyleProperty::JustifyContent
            | StyleProperty::GridTemplateColumns
            | StyleProperty::GridColumn
            | StyleProperty::Position
            | StyleProperty::Top
            | StyleProperty::Right
            | StyleProperty::Bottom
            | StyleProperty::Left
            | StyleProperty::Inset
            | StyleProperty::Gap
            | StyleProperty::Padding
            | StyleProperty::PaddingTop
            | StyleProperty::PaddingRight
            | StyleProperty::PaddingBottom
            | StyleProperty::PaddingLeft
            | StyleProperty::Margin
            | StyleProperty::MarginTop
            | StyleProperty::MarginRight
            | StyleProperty::MarginBottom
            | StyleProperty::MarginLeft
            | StyleProperty::Width
            | StyleProperty::Height
            | StyleProperty::MinWidth
            | StyleProperty::MinHeight
            | StyleProperty::MaxWidth
            | StyleProperty::MaxHeight
            | StyleProperty::Overflow
            | StyleProperty::OverflowX
            | StyleProperty::OverflowY
            | StyleProperty::Background
            | StyleProperty::BackgroundColor
            | StyleProperty::BoxSizing
            | StyleProperty::Color
            | StyleProperty::FontFamily
            | StyleProperty::FontSize
            | StyleProperty::FontWeight
            | StyleProperty::LineHeight
            | StyleProperty::WhiteSpace
            | StyleProperty::TextAlign
            | StyleProperty::TextOverflow
            | StyleProperty::Cursor
            | StyleProperty::Opacity
            | StyleProperty::BoxShadow
            | StyleProperty::BorderWidth
            | StyleProperty::BorderStyle
            | StyleProperty::BorderColor
            | StyleProperty::Border
            | StyleProperty::BorderTop
            | StyleProperty::BorderRight
            | StyleProperty::BorderBottom
            | StyleProperty::BorderLeft
            | StyleProperty::BorderRadius
            | StyleProperty::Outline
    )
}

fn responsive_length_property(property: &StyleProperty) -> bool {
    matches!(
        property,
        StyleProperty::MarginTop
            | StyleProperty::MarginRight
            | StyleProperty::MarginBottom
            | StyleProperty::MarginLeft
            | StyleProperty::Top
            | StyleProperty::Right
            | StyleProperty::Bottom
            | StyleProperty::Left
            | StyleProperty::Width
            | StyleProperty::Height
            | StyleProperty::MinWidth
            | StyleProperty::MinHeight
            | StyleProperty::MaxWidth
            | StyleProperty::MaxHeight
            | StyleProperty::FlexBasis
    )
}

fn definite_length_property(property: &StyleProperty) -> bool {
    matches!(
        property,
        StyleProperty::Gap
            | StyleProperty::PaddingTop
            | StyleProperty::PaddingRight
            | StyleProperty::PaddingBottom
            | StyleProperty::PaddingLeft
    )
}

fn pixel_length_property(property: &StyleProperty) -> bool {
    matches!(
        property,
        StyleProperty::FontSize | StyleProperty::BorderWidth | StyleProperty::BorderRadius
    )
}

fn border_shorthand_property(property: &StyleProperty) -> bool {
    matches!(
        property,
        StyleProperty::Border
            | StyleProperty::BorderTop
            | StyleProperty::BorderRight
            | StyleProperty::BorderBottom
            | StyleProperty::BorderLeft
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    use gpui::{GridPlacement, Role as AccessibleRole, px};
    use gpui_mcp::Automation;
    use htmlswap::{RenderNode, StyleDeclaration, StyleProperty};

    use crate::{
        Binding, BindingDocument, BindingTarget, ElementId, HandlerId, HookRegistry, HtmlUi,
        StateValue, UiEvent, UiProperty,
    };

    use super::{
        BorderStyle, MediaViewport, ReloadError, SemanticNamespace, accessible_label, aria_role,
        border_value, box_shadows, box_values, collect_declaration_diagnostics, color,
        cursor_supported, definite_length, effective_border_style, flex_value, font_family,
        grid_column, grid_column_count, length, line_height, media_query_matches, opacity,
        overflow, update_hovered_element,
    };

    #[test]
    fn gpui_hover_transitions_share_the_semantic_hover_state() {
        let hovered = Rc::new(RefCell::new(None));
        let first = ElementId::new("first");
        let second = ElementId::new("second");

        update_hovered_element(&hovered, &first, true);
        assert_eq!(hovered.borrow().as_ref(), Some(&first));
        update_hovered_element(&hovered, &second, true);
        assert_eq!(hovered.borrow().as_ref(), Some(&second));
        update_hovered_element(&hovered, &first, false);
        assert_eq!(hovered.borrow().as_ref(), Some(&second));
        update_hovered_element(&hovered, &second, false);
        assert!(hovered.borrow().is_none());
    }

    #[test]
    fn aria_roles_preserve_tree_and_floating_surface_semantics() {
        assert_eq!(aria_role("tree"), Some(AccessibleRole::Tree));
        assert_eq!(aria_role("TREEITEM"), Some(AccessibleRole::TreeItem));
        assert_eq!(
            aria_role("menuitemcheckbox"),
            Some(AccessibleRole::MenuItemCheckBox)
        );
        assert_eq!(aria_role("combobox"), Some(AccessibleRole::ComboBox));
        assert_eq!(aria_role("option"), Some(AccessibleRole::ListBoxOption));
        assert_eq!(aria_role("presentation"), None);
    }

    #[test]
    fn embedded_documents_scope_runtime_ids_without_changing_authored_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let ui = HtmlUi::compile("<button id='save'>Save</button>", BindingDocument::new())?;
        let live = super::LiveHtml::new(ui, Automation::for_test(), HookRegistry::new())?
            .embedded(SemanticNamespace::new("project-canvas")?);

        assert_eq!(live.scoped_id("save"), "project-canvas--save");
        assert!(super::collect_element_ids(live.ui.plan()).contains(&ElementId::new("save")));
        Ok(())
    }

    #[test]
    fn focus_visible_is_a_supported_live_interaction() -> Result<(), Box<dyn std::error::Error>> {
        let ui = HtmlUi::compile_with_stylesheet(
            "<button id='save'>Save</button>",
            BindingDocument::new(),
            "focus.css",
            "#save:focus-visible { color: #6e7bff; }",
        )?;
        let live = super::LiveHtml::new(ui, Automation::for_test(), HookRegistry::new())?;

        assert!(live.diagnostics().is_empty(), "{:?}", live.diagnostics());
        Ok(())
    }

    #[test]
    fn bound_button_text_is_the_current_accessible_label() -> Result<(), Box<dyn std::error::Error>>
    {
        let ui = HtmlUi::compile(
            "<button id='theme'>Foundry dark</button>",
            BindingDocument::new(),
        )?;
        let Some(RenderNode::Element(button)) = ui.plan().nodes.first() else {
            return Err("button render element is missing".into());
        };
        let properties =
            HashMap::from([(UiProperty::Text, StateValue::Text("Paper light".to_owned()))]);

        assert_eq!(
            accessible_label(button, &properties).as_deref(),
            Some("Paper light")
        );
        Ok(())
    }

    #[test]
    fn semantic_namespaces_are_bounded_kebab_case() {
        assert!(SemanticNamespace::new("project-canvas-2").is_ok());
        assert!(SemanticNamespace::new("").is_err());
        assert!(SemanticNamespace::new("Project").is_err());
        assert!(SemanticNamespace::new("project_canvas").is_err());
        assert!(SemanticNamespace::new("x".repeat(65)).is_err());
    }

    #[test]
    fn borders_default_to_none_and_accept_supported_styles() {
        let width = StyleDeclaration::new(StyleProperty::BorderWidth, "1px", false, None);
        let solid = StyleDeclaration::new(StyleProperty::BorderStyle, "solid", false, None);
        let dashed = StyleDeclaration::new(StyleProperty::BorderStyle, "dashed", false, None);
        let hidden = StyleDeclaration::new(StyleProperty::BorderStyle, "hidden", false, None);

        assert_eq!(effective_border_style(&[&width]), BorderStyle::None);
        assert_eq!(
            effective_border_style(&[&width, &solid]),
            BorderStyle::Solid
        );
        assert_eq!(
            effective_border_style(&[&width, &dashed]),
            BorderStyle::Dashed
        );
        assert_eq!(
            effective_border_style(&[&width, &solid, &hidden]),
            BorderStyle::None
        );
    }

    #[test]
    fn explicit_font_family_preserves_case() {
        let available = HashSet::from(["segoe ui".to_owned(), ".systemuifont".to_owned()]);
        assert_eq!(
            font_family("'Segoe UI', sans-serif", &available),
            Some(super::FontFamily {
                primary: "Segoe UI".to_owned(),
                fallbacks: vec![".SystemUIFont".to_owned()],
            })
        );
        assert_eq!(
            font_family("SYSTEM-UI", &available),
            Some(super::FontFamily {
                primary: ".SystemUIFont".to_owned(),
                fallbacks: Vec::new(),
            })
        );
        assert_eq!(
            font_family("'Missing Font', sans-serif", &available),
            Some(super::FontFamily {
                primary: ".SystemUIFont".to_owned(),
                fallbacks: Vec::new(),
            })
        );
    }

    #[test]
    fn grid_columns_require_equal_fractional_tracks() {
        assert_eq!(grid_column_count("repeat(3, 1fr)"), Some(3));
        assert_eq!(grid_column_count("1fr 1fr"), Some(2));
        assert_eq!(grid_column_count("repeat(2, 120px)"), None);
        assert_eq!(grid_column_count("1fr 2fr"), None);
        assert_eq!(grid_column_count("repeat(0, 1fr)"), None);
    }

    #[test]
    fn grid_column_accepts_line_and_span_placement() {
        assert_eq!(
            grid_column("1 / span 2"),
            Some(GridPlacement::Line(1)..GridPlacement::Span(2))
        );
        assert_eq!(
            grid_column("1 / -1"),
            Some(GridPlacement::Line(1)..GridPlacement::Line(-1))
        );
        assert_eq!(
            grid_column("auto"),
            Some(GridPlacement::Auto..GridPlacement::Auto)
        );
        assert_eq!(grid_column("0 / span 2"), None);
        assert_eq!(grid_column("1 / span 0"), None);
    }

    #[test]
    fn responsive_layout_css_is_a_supported_live_renderer_contract() {
        let declarations = [
            (StyleProperty::Width, "100%"),
            (StyleProperty::Height, "100%"),
            (StyleProperty::MinWidth, "0"),
            (StyleProperty::Flex, "1 1 0%"),
            (StyleProperty::FlexGrow, "1"),
            (StyleProperty::FlexShrink, "0"),
            (StyleProperty::FlexBasis, "240px"),
            (StyleProperty::Overflow, "hidden"),
            (StyleProperty::Padding, "10px 14px"),
            (StyleProperty::Margin, "0 auto"),
            (StyleProperty::BorderBottom, "1px solid #393b31"),
            (StyleProperty::Position, "absolute"),
            (StyleProperty::Inset, "0 12px"),
            (StyleProperty::AlignSelf, "center"),
            (StyleProperty::GridColumn, "1 / span 2"),
            (StyleProperty::WhiteSpace, "nowrap"),
            (StyleProperty::TextAlign, "center"),
            (StyleProperty::TextOverflow, "ellipsis"),
            (StyleProperty::Cursor, "pointer"),
            (StyleProperty::Opacity, "0.76"),
            (StyleProperty::BoxShadow, "0 30px 80px rgba(0, 0, 0, 0.6)"),
            (StyleProperty::LineHeight, "1.6"),
        ]
        .into_iter()
        .map(|(property, value)| StyleDeclaration::new(property, value, false, None))
        .collect::<Vec<_>>();
        let mut diagnostics = Vec::new();

        collect_declaration_diagnostics("responsive-shell", &declarations, &mut diagnostics);

        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    }

    #[test]
    fn responsive_value_parsers_reject_invalid_or_ambiguous_values() {
        assert!(length("100%").is_some());
        assert!(length("auto").is_some());
        assert!(length("12vw").is_none());
        assert!(box_values("1px 2px 3px 4px", definite_length).is_some());
        assert!(box_values("1px 2px 3px 4px 5px", definite_length).is_none());
        assert!(flex_value("1 1 0%").is_some());
        assert!(flex_value("0 250px").is_some());
        assert!(flex_value("0 auto").is_some());
        assert!(flex_value("1 0").is_some());
        assert!(flex_value("grow please").is_none());
        assert!(overflow("hidden").is_some());
        assert_eq!(overflow("auto"), Some(gpui::Overflow::Scroll));
        assert!(border_value("1px solid #393b31").is_some());
        assert!(border_value("0 solid #0000").is_some());
        assert!(border_value("wavy 1px red").is_none());
        assert!(cursor_supported("crosshair"));
        assert!(!cursor_supported("magic"));
        assert_eq!(opacity("0.5"), Some(0.5));
        assert_eq!(opacity("2"), None);
        assert_eq!(definite_length("2px"), Some(px(2.).into()));
        assert_eq!(line_height("1.6"), Some(super::LineHeight::Relative(1.6)));
        assert_eq!(line_height("150%"), Some(super::LineHeight::Relative(1.5)));
        assert_eq!(line_height("18px"), Some(super::LineHeight::Pixels(18.)));
        assert_eq!(line_height("normal"), Some(super::LineHeight::Normal));
        assert_eq!(line_height("-1"), None);
        assert_eq!(color("rgba(0, 0, 0, 0.6)"), Some(0x0000_0099));
        assert_eq!(
            box_shadows("0 30px 80px rgba(0, 0, 0, 0.6)").map(|value| value.len()),
            Some(1)
        );
        assert!(box_shadows("inset 0 1px black").is_none());
    }

    #[test]
    fn desktop_media_queries_follow_the_live_gpui_viewport() {
        let wide = MediaViewport {
            width: 1280.0,
            height: 900.0,
        };
        let compact = MediaViewport {
            width: 900.0,
            height: 650.0,
        };

        assert_eq!(
            media_query_matches("(max-width: 1120px)", wide),
            Some(false)
        );
        assert_eq!(
            media_query_matches("(max-width: 1120px)", compact),
            Some(true)
        );
        assert_eq!(
            media_query_matches("(width <= 1120px)", compact),
            Some(true)
        );
        assert_eq!(
            media_query_matches("screen and (max-height: 720px)", compact),
            Some(true)
        );
        assert_eq!(
            media_query_matches("(min-width: 60rem) and (orientation: landscape)", wide),
            Some(true)
        );
        assert_eq!(
            media_query_matches("(prefers-reduced-motion: reduce)", wide),
            None
        );
    }

    #[test]
    fn reload_preserves_stable_state_and_prunes_deleted_nodes()
    -> Result<(), Box<dyn std::error::Error>> {
        let initial = HtmlUi::compile(
            "<details id='kept'><summary>Kept</summary></details><details id='gone'><summary>Gone</summary></details>",
            BindingDocument::new(),
        )?;
        let mut live = super::LiveHtml::new(initial, Automation::for_test(), HookRegistry::new())?;
        live.disclosures
            .borrow_mut()
            .insert(ElementId::new("kept"), true);
        live.disclosures
            .borrow_mut()
            .insert(ElementId::new("gone"), false);
        *live.hovered_element.borrow_mut() = Some(ElementId::new("kept"));

        let candidate = HtmlUi::compile(
            "<section><details id='kept'><summary>Still here</summary></details></section>",
            BindingDocument::new(),
        )?;
        let report = live.reload(candidate)?;

        assert_eq!(report.previous_revision, 1);
        assert_eq!(report.revision, 2);
        assert_eq!(report.retained_disclosures, 1);
        assert_eq!(report.pruned_disclosures, 1);
        assert!(report.hovered_element_retained);
        assert_eq!(
            live.disclosures.borrow().get(&ElementId::new("kept")),
            Some(&true)
        );
        assert!(
            !live
                .disclosures
                .borrow()
                .contains_key(&ElementId::new("gone"))
        );
        Ok(())
    }

    #[test]
    fn rejected_reload_keeps_the_last_good_document() -> Result<(), Box<dyn std::error::Error>> {
        let initial = HtmlUi::compile("<button id='save'>Save</button>", BindingDocument::new())?;
        let mut live = super::LiveHtml::new(initial, Automation::for_test(), HookRegistry::new())?;
        let candidate = HtmlUi::compile(
            "<button id='save'>Changed</button>",
            BindingDocument::new().with_binding(Binding::Event {
                target: BindingTarget::Id(ElementId::new("save")),
                event: UiEvent::Click,
                handler: HandlerId::new("missing_handler"),
            }),
        )?;

        let result = live.reload(candidate);

        assert!(matches!(result, Err(ReloadError::Hooks(_))));
        assert_eq!(live.revision(), 1);
        assert!(live.document().source().contains("Save"));
        assert!(!live.document().source().contains("Changed"));
        Ok(())
    }
}
