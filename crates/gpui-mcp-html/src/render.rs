use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, FocusHandle, FrameAction,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Role as AccessibleRole,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled, Toggled, Window,
    div, px, rgba,
};
use gpui_mcp::{Automation, MAX_LABEL_BYTES, MAX_TEXT_BYTES};
use htmlswap::{
    RenderElement, RenderNode, RenderPlan, RenderStyleCondition, RenderStyleVariant,
    StyleDeclaration, UiRole,
};

use crate::cascade::{self, Environment, Interaction, StateNeeds};
use crate::components::{ComponentNode, ComponentRegistry};
use crate::document::{attribute, is_text_editable};
use crate::gpui_style;
use crate::input::{RuntimeTextInput, RuntimeTextInputOptions};
use crate::motion::{self, Translated};
use crate::view_transition::{
    DocumentTransitions, OldState, PropertySnapshot, declares_name, element_at, transition_classes,
    transition_name,
};
use crate::{
    Binding, BindingMode, ElementId, HandlerId, HookEvent, HookOutcome, HookRegistry,
    HookRegistryError, HtmlUi, StateValue, UiEvent, UiProperty,
};
use htmlswap::computed::{
    ColorScheme, ComputedScope, ComputedStyle, LengthPercentage, MediaEnvironment, Underlying,
    Unsupported,
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
    /// Media-query dimensions for the render in progress, when overridden.
    viewport_override: Cell<Option<(f32, f32)>>,
    available_fonts: RefCell<Option<Rc<HashSet<String>>>>,
    /// Computed styles, reused while an element's context is unchanged.
    styles: Rc<RefCell<StyleCache>>,
    /// Elements whose descendants' styles depend on their interaction state.
    state_anchors: Rc<HashMap<ElementId, StateNeeds>>,
    /// Scopes of the elements being rendered, innermost last.
    scopes: RefCell<Vec<ComputedScope>>,
    /// Interaction states of the elements being rendered, innermost last.
    interactions: RefCell<Vec<Interaction>>,
    /// Elements drawn by the previous and current passes, for entry
    /// transitions from `@starting-style`.
    drawn: Rc<RefCell<DrawnElements>>,
    /// The preferred color scheme, or `None` to follow the window.
    color_scheme: Option<ColorScheme>,
    /// Elements under the pointer, for elements whose interaction styles transition.
    pointer_hovered: Rc<RefCell<HashSet<ElementId>>>,
    /// The element the primary button is pressed on.
    pressed: Rc<RefCell<Option<ElementId>>>,
    /// Per-render clock and motion settings.
    frame: Cell<FrameContext>,
    transitions: DocumentTransitions,
    /// Elements that may carry a `view-transition-name`.
    named_elements: Rc<HashSet<ElementId>>,
}

/// What drawing the outgoing document of a view transition needs.
#[derive(Clone, Copy)]
struct Inert<'a> {
    old: &'a OldState,
    /// The named element being drawn as its own image, which fills its box.
    keep: Option<&'a [usize]>,
    /// Named elements, which the root image leaves out.
    is_named: &'a dyn Fn(&[usize]) -> bool,
    environment: Environment<'a>,
    fonts: &'a HashSet<String>,
}

/// Elements drawn by the previous and the current render pass.
#[derive(Default)]
struct DrawnElements {
    previous: HashSet<ElementId>,
    current: HashSet<ElementId>,
}

impl DrawnElements {
    fn begin(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    /// Record an element, returning whether it is new this pass.
    fn draw(&mut self, element_id: &ElementId) -> bool {
        if !self.current.contains(element_id) {
            self.current.insert(element_id.clone());
        }
        !self.previous.contains(element_id)
    }
}

/// Values fixed for one render pass.
#[derive(Clone, Copy, Debug)]
struct FrameContext {
    reduce_motion: bool,
    /// Whether anything drawn this pass is still moving.
    moving: bool,
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
        let named_elements = Rc::new(collect_named_elements(ui.plan()));
        let state_anchors = Rc::new(collect_state_anchors(ui.plan()));
        let ui = Rc::new(ui);
        let transitions = DocumentTransitions::default();
        transitions.set_document(ui.clone());
        Ok(Self {
            ui,
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
            styles: Rc::default(),
            state_anchors,
            scopes: RefCell::default(),
            interactions: RefCell::default(),
            drawn: Rc::default(),
            color_scheme: None,
            pointer_hovered: Rc::default(),
            pressed: Rc::default(),
            frame: Cell::new(FrameContext {
                reduce_motion: false,
                moving: false,
            }),
            transitions,
            named_elements,
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
        let named_elements = Rc::new(collect_named_elements(ui.plan()));
        let state_anchors = Rc::new(collect_state_anchors(ui.plan()));
        // A swap between two documents that both opt in with
        // `@view-transition { navigation: auto; }` is a navigation.
        let navigation = &ui.plan().motion.view_transition;
        let types = navigation
            .types
            .iter()
            .map(|kind| SharedString::from(kind.to_string()))
            .collect::<Vec<_>>();
        if self.transitions.is_pending() {
            self.transitions.add_types(types);
        } else if navigation.navigation && self.ui.plan().motion.view_transition.navigation {
            self.begin_view_transition(types, None);
        }

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

        self.styles.borrow_mut().elements.clear();
        self.pointer_hovered
            .borrow_mut()
            .retain(|element_id| element_ids.contains(element_id));
        let previous_revision = self.revision;
        self.ui = Rc::new(ui);
        self.transitions.set_document(self.ui.clone());
        self.bindings = bindings;
        self.diagnostics = diagnostics;
        self.named_elements = named_elements;
        self.state_anchors = state_anchors;
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

    /// Start a same-document view transition, like `document.startViewTransition()`.
    ///
    /// Captures the document as last drawn, including bound values read from
    /// the hooks now. Change application state after calling this; the next
    /// frame animates from the captured state to the new one. `types` match
    /// `:active-view-transition-type()` while the transition runs.
    ///
    /// Swapping in a new document with [`LiveHtml::reload`] while the
    /// transition is pending (before the next frame) makes it the incoming
    /// state; when both documents declare `@view-transition { navigation:
    /// auto; }`, `reload` starts a transition on its own.
    ///
    /// Returns `false`, and starts nothing, before the document's first frame.
    pub fn start_view_transition<T: AsRef<str>>(
        &mut self,
        types: impl IntoIterator<Item = T>,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let properties: PropertySnapshot = self
            .bindings
            .iter()
            .map(|(element_id, bindings)| {
                (
                    element_id.clone(),
                    read_properties(bindings, &self.hooks, window, cx),
                )
            })
            .filter(|(_, values)| !values.is_empty())
            .collect();
        let types = types
            .into_iter()
            .map(|kind| SharedString::from(kind.as_ref().to_owned()))
            .collect();
        let started = self.begin_view_transition(types, Some(properties));
        if started {
            window.refresh();
        }
        started
    }

    /// Whether a view transition is pending or running.
    #[must_use]
    pub fn view_transition_running(&self) -> bool {
        self.transitions.is_active()
    }

    /// Finish the current view transition at once, like
    /// `ViewTransition.skipTransition()`.
    pub fn skip_view_transition(&mut self) {
        self.transitions.skip();
    }

    fn begin_view_transition(
        &mut self,
        types: Vec<SharedString>,
        properties: Option<PropertySnapshot>,
    ) -> bool {
        let old = OldState {
            ui: self.ui.clone(),
            bindings: self.bindings.clone(),
            properties,
            disclosures: self.disclosures.borrow().clone(),
        };
        self.transitions.start(types, old)
    }

    /// Prefer a color scheme for `prefers-color-scheme`, `light-dark()` and
    /// system colors, or follow the window's appearance with `None`.
    ///
    /// Hosts following the window should redraw when it changes, for
    /// example with `cx.observe_window_appearance(window, |_, _, cx| cx.notify())`.
    pub fn set_color_scheme(&mut self, scheme: Option<ColorScheme>) {
        if self.color_scheme != scheme {
            self.color_scheme = scheme;
            self.styles.borrow_mut().elements.clear();
        }
    }

    /// Builder form of [`LiveHtml::set_color_scheme`].
    #[must_use]
    pub fn with_color_scheme(mut self, scheme: Option<ColorScheme>) -> Self {
        self.set_color_scheme(scheme);
        self
    }

    /// Build a live GPUI element tree. Call this from the owning view's `Render` implementation.
    #[must_use]
    pub fn render(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        self.render_with_media_viewport(None, window, cx)
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
        let viewport = (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
            .then_some((width, height));
        self.render_with_media_viewport(viewport, window, cx)
    }

    fn render_with_media_viewport(
        &self,
        viewport: Option<(f32, f32)>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.automation.attach(window);
        self.viewport_override.set(viewport);
        let now = cx.background_executor().now();
        self.frame.set(FrameContext {
            reduce_motion: cx.reduce_motion(),
            moving: false,
        });
        self.drawn.borrow_mut().begin();
        // A running transition's types match `:active-view-transition-type()`.
        let types = self.transitions.types();
        let environment = self.environment(window, types.as_deref());
        self.transitions.begin_frame(now, environment.media);
        let available_fonts = self.available_fonts(cx);
        let plan = self.ui.plan();
        let root_declarations = cascade::root_declarations(&plan.root, &environment);
        let initial = ComputedScope::root(&environment.media);
        let root_scope = initial.document_root(root_declarations.iter().copied());
        let root_style = cascade::typed(&root_scope, &initial, &root_declarations);
        self.scopes.borrow_mut().push(root_scope.clone());
        let children = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                self.render_node(node, &[index], None, &available_fonts, window, cx)
            })
            .collect::<Vec<_>>();
        self.scopes.borrow_mut().pop();
        let mut root = gpui_style::apply(
            children.into_iter().fold(div(), gpui::ParentElement::child),
            &root_style,
            &available_fonts,
        );
        // A document that declares its color scheme draws its text in that
        // scheme's CanvasText unless it sets a color; otherwise text keeps
        // inheriting from the host.
        if root_style.color.is_none() && root_scope.declares_color_scheme() {
            root = root.text_color(gpui_style::color(root_scope.color()));
        }
        let root = root
            .id(SharedString::from(self.scoped_id("html-root")))
            .role(AccessibleRole::Application);
        let rendered = self.transitions.stage(root, |old, path, is_named| {
            let image = match path {
                None => self
                    .render_old_root(old, is_named, environment, &available_fonts, window, cx)
                    .size_full()
                    .into_any_element(),
                Some(path) => {
                    let element = element_at(old.ui.plan(), path)?;
                    let inert = Inert {
                        old,
                        keep: Some(path),
                        is_named,
                        environment,
                        fonts: &available_fonts,
                    };
                    self.render_inert_element(&inert, element, &mut path.to_vec(), window, cx)
                }
            };
            // The outgoing document is an image: hidden from semantics.
            let id = match path {
                None => self.scoped_id("html-view-transition-old"),
                Some(path) => {
                    self.scoped_id(&format!("html-view-transition-old-{}", generated_id(path)))
                }
            };
            Some(
                div()
                    .id(SharedString::from(id))
                    .aria_hidden(true)
                    .size_full()
                    .child(image)
                    .into_any_element(),
            )
        });
        self.viewport_override.set(None);
        self.transitions.end_frame(window);
        if self.frame.get().moving {
            window.request_animation_frame();
        }
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

    fn environment<'a>(
        &self,
        window: &Window,
        view_transition_types: Option<&'a [SharedString]>,
    ) -> Environment<'a> {
        let (width, height) = self.viewport_override.get().unwrap_or_else(|| {
            let size = window.viewport_size();
            (size.width.into(), size.height.into())
        });
        let color_scheme = self.color_scheme.unwrap_or(match window.appearance() {
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark => ColorScheme::Dark,
            gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => {
                ColorScheme::Light
            }
        });
        Environment {
            media: MediaEnvironment {
                width,
                height,
                font_size: 16.0,
                color_scheme,
                reduced_motion: self.frame.get().reduce_motion,
                hover: true,
                fine_pointer: true,
            },
            view_transition_types,
        }
    }

    /// Map every rendered element of the active document to its HTML source:
    /// semantic id, document id (authored or generated), tag, position path,
    /// and the byte range, line and column of its markup.
    ///
    /// Each rendered node also carries its byte range as `source_span`
    /// (`start..end`) metadata in the semantic tree, so an MCP client can map
    /// `get_ui_tree` nodes back to source without this API.
    #[must_use]
    pub fn source_map(&self) -> crate::SourceMap {
        crate::SourceMap::build(self.ui.plan(), self.ui.source(), |id| self.scoped_id(id))
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
            RenderNode::Text(text) => {
                let transform = self
                    .scopes
                    .borrow()
                    .last()
                    .map(ComputedScope::text_transform)
                    .unwrap_or_default();
                match transform.apply(&text.value) {
                    std::borrow::Cow::Borrowed(_) => text.value.clone().into_any_element(),
                    std::borrow::Cow::Owned(transformed) => {
                        SharedString::from(transformed).into_any_element()
                    }
                }
            }
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
        let types = self.transitions.types();
        let environment = self.environment(window, types.as_deref());
        let interaction = self.interaction(&element_id, window, cx);
        let needs = {
            let mut needs = cascade::own_state_needs(element);
            if let Some(anchor) = self.state_anchors.get(&element_id) {
                needs.hover |= anchor.hover;
                needs.focus |= anchor.focus;
                needs.active |= anchor.active;
            }
            needs
        };
        let parent = self.scopes.borrow().last().cloned().unwrap_or_default();
        let computed = self.computed(&element_id, element, &environment, interaction, &parent);
        self.scopes.borrow_mut().push(computed.scope.clone());
        self.interactions.borrow_mut().push(interaction);
        let (children, text_input) = self.render_element_children(
            element,
            path,
            runtime,
            disclosure_open,
            available_fonts,
            window,
            cx,
        );
        self.interactions.borrow_mut().pop();
        self.scopes.borrow_mut().pop();

        // Bound sizes override the stylesheet, and are what size transitions
        // move toward.
        let bound = |property: UiProperty| {
            property_values
                .get(&property)
                .and_then(StateValue::as_pixels)
                .map(|pixels| htmlswap::computed::Size::Length(LengthPercentage::px(pixels)))
        };
        let (bound_width, bound_height) = (bound(UiProperty::Width), bound(UiProperty::Height));
        let entering = self.drawn.borrow_mut().draw(&element_id);
        let animated = motion::has_motion(&computed);
        let overridden = bound_width.is_some() || bound_height.is_some();
        let owned;
        let style = if animated || overridden {
            let mut target = computed.style.clone();
            if bound_width.is_some() {
                target.width = bound_width;
            }
            if bound_height.is_some() {
                target.height = bound_height;
            }
            if animated {
                let keyframe_scope = &computed.scope;
                let compute_keyframe = |declarations: &[StyleDeclaration]| {
                    let declarations = declarations.iter().collect::<Vec<_>>();
                    cascade::typed(keyframe_scope, &parent, &declarations)
                };
                let output = motion::animate(
                    &motion::Inputs {
                        key: &runtime_id,
                        computed: &computed,
                        target: &target,
                        entering,
                        plan: &self.ui.plan().motion,
                        underlying: Underlying {
                            current_color: parent.color(),
                            font_size: parent.font_size(),
                        },
                        compute_keyframe: &compute_keyframe,
                    },
                    window,
                    cx,
                );
                if output.moving {
                    let mut context = self.frame.get();
                    context.moving = true;
                    self.frame.set(context);
                }
                target.overlay(&output.style);
            }
            owned = target;
            &owned
        } else {
            &computed.style
        };

        let host = self.render_host(element, &id, children, window, cx);
        let mut host =
            gpui_style::apply(apply_native_defaults(host, element), style, available_fonts);
        if computed.scope.font_features() != parent.font_features() {
            let features = computed
                .scope
                .font_features()
                .features()
                .into_iter()
                .map(|(tag, value)| (tag.to_string(), value))
                .collect::<Vec<_>>();
            host.text_style().font_features =
                Some(gpui::FontFeatures(std::sync::Arc::new(features)));
        }
        host = apply_native_state(host, element, &state);
        if !state.visible {
            host = host.hidden();
        }
        let translate = style
            .translate
            .filter(|translate| !motion::is_zero(*translate));
        let scroll_axes = ScrollAxes::of(style);
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

        let focus_handle = self.resolve_focus_handle(runtime, needs.focus, text_input.as_ref(), cx);
        if let Some(focus_handle) = &focus_handle {
            host = host.track_focus(focus_handle);
        }
        if needs.active {
            let pressed = self.pressed.clone();
            let pressed_id = element_id.clone();
            host = host.on_mouse_down(gpui::MouseButton::Left, move |_, window, _| {
                *pressed.borrow_mut() = Some(pressed_id.clone());
                window.refresh();
            });
            for release in [false, true] {
                let pressed = self.pressed.clone();
                let release_listener =
                    move |_: &gpui::MouseUpEvent, window: &mut Window, _: &mut App| {
                        if pressed.borrow_mut().take().is_some() {
                            window.refresh();
                        }
                    };
                host = if release {
                    host.on_mouse_up_out(gpui::MouseButton::Left, release_listener)
                } else {
                    host.on_mouse_up(gpui::MouseButton::Left, release_listener)
                };
            }
        }
        if needs.hover {
            // Interaction styles are resolved while rendering, from one hover
            // state shared by pointer input, MCP platform input and semantic
            // automation, so every path resolves the same CSS :hover.
            let hovered_element = self.hovered_element.clone();
            let pointer_hovered = self.pointer_hovered.clone();
            let hovered_id = element_id.clone();
            host = host.on_hover(move |hovered, window, _| {
                update_hovered_element(&hovered_element, &hovered_id, *hovered);
                if *hovered {
                    pointer_hovered.borrow_mut().insert(hovered_id.clone());
                } else {
                    pointer_hovered.borrow_mut().remove(&hovered_id);
                }
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
        if let Some(span) = element.span {
            host = host.frame_metadata("source_span", format!("{}..{}", span.start, span.end));
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

        // A named element is captured and moved with its `translate`
        // applied, as browsers capture an element's transformed box.
        let rendered = match self.transition_name_of(element, &element_id, &environment) {
            Some((name, classes)) => self.transitions.named(name, &classes, path.to_vec(), host),
            None => host.into_any_element(),
        };
        match translate {
            Some((x, y)) => Translated::new(rendered, x, y).into_any_element(),
            None => rendered,
        }
    }

    /// The old document's root, with its named elements left out.
    fn render_old_root(
        &self,
        old: &OldState,
        is_named: &dyn Fn(&[usize]) -> bool,
        environment: Environment<'_>,
        available_fonts: &HashSet<String>,
        window: &mut Window,
        cx: &mut App,
    ) -> Div {
        let old_plan = old.ui.plan();
        let root_declarations = cascade::root_declarations(&old_plan.root, &environment);
        let initial = ComputedScope::root(&environment.media);
        let root_scope = initial.document_root(root_declarations.iter().copied());
        let root_style = cascade::typed(&root_scope, &initial, &root_declarations);
        self.scopes.borrow_mut().push(root_scope);
        let inert = Inert {
            old,
            keep: None,
            is_named,
            environment,
            fonts: available_fonts,
        };
        let mut path = Vec::new();
        let children = old_plan
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                path.clear();
                path.push(index);
                self.render_inert(&inert, node, &mut path, window, cx)
            })
            .collect::<Vec<_>>();
        self.scopes.borrow_mut().pop();
        gpui_style::apply(
            children.into_iter().fold(div(), gpui::ParentElement::child),
            &root_style,
            available_fonts,
        )
    }

    /// A named element's `view-transition-name` and classes, if it has one.
    fn transition_name_of(
        &self,
        element: &RenderElement,
        element_id: &ElementId,
        environment: &Environment<'_>,
    ) -> Option<(SharedString, Vec<SharedString>)> {
        if !self.named_elements.contains(element_id) {
            return None;
        }
        let declarations = cascade::declarations(element, environment, Interaction::default(), &[]);
        let name = transition_name(declarations.iter().copied(), element_id.as_str())?;
        Some((name, transition_classes(declarations.iter().copied())))
    }

    /// Render a node of the outgoing document as a static image: its styles
    /// and bound values, without ids, handlers, focus or semantics.
    fn render_inert(
        &self,
        inert: &Inert<'_>,
        node: &RenderNode,
        path: &mut Vec<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        match node {
            RenderNode::Text(text) => text.value.clone().into_any_element(),
            RenderNode::Raw(raw) => raw.html.clone().into_any_element(),
            RenderNode::Element(element) => {
                self.render_inert_element(inert, element, path, window, cx)
            }
        }
    }

    fn render_inert_element(
        &self,
        inert: &Inert<'_>,
        element: &RenderElement,
        path: &mut Vec<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Inert {
            old,
            keep,
            is_named,
            environment: viewport,
            fonts: available_fonts,
        } = *inert;
        let element_id = ElementId::new(
            attribute(element, "id").map_or_else(|| generated_id(path), str::to_owned),
        );
        let live;
        let properties = if let Some(snapshot) = &old.properties {
            snapshot.get(&element_id)
        } else {
            live = old
                .bindings
                .get(&element_id)
                .map(|bindings| read_properties(bindings, &self.hooks, window, cx));
            live.as_ref()
        };
        let empty = HashMap::new();
        let properties = properties.unwrap_or(&empty);
        let state = element_state(element, properties);
        let parent = self.scopes.borrow().last().cloned().unwrap_or_default();
        let declarations = cascade::declarations(element, &viewport, Interaction::default(), &[]);
        let scope = parent.child(declarations.iter().copied());
        let style = cascade::typed(&scope, &parent, &declarations);
        let text = properties
            .get(&UiProperty::Text)
            .or_else(|| properties.get(&UiProperty::Value));
        let children = if let Some(value) = text {
            vec![value.display().into_any_element()]
        } else if is_text_editable(element) {
            attribute(element, "value")
                .map(|value| vec![value.to_owned().into_any_element()])
                .unwrap_or_default()
        } else {
            let open = (element.source_tag == "details").then(|| {
                old.disclosures
                    .get(&element_id)
                    .copied()
                    .unwrap_or_else(|| attribute(element, "open").is_some())
            });
            let mut children = Vec::with_capacity(element.children.len());
            self.scopes.borrow_mut().push(scope.clone());
            for (index, child) in element.children.iter().enumerate() {
                if open == Some(false) && !is_summary_element(child) {
                    continue;
                }
                path.push(index);
                children.push(self.render_inert(inert, child, path, window, cx));
                path.pop();
            }
            self.scopes.borrow_mut().pop();
            children
        };
        let host = children.into_iter().fold(div(), gpui::ParentElement::child);
        let mut host = gpui_style::apply(
            apply_native_defaults(host, element),
            &style,
            available_fonts,
        );
        host = apply_native_state(host, element, &state);
        if !state.visible {
            host = host.hidden();
        }
        if let Some(width) = properties
            .get(&UiProperty::Width)
            .and_then(StateValue::as_pixels)
        {
            host = host.w(px(width));
        }
        if let Some(height) = properties
            .get(&UiProperty::Height)
            .and_then(StateValue::as_pixels)
        {
            host = host.h(px(height));
        }
        if keep == Some(path.as_slice()) {
            // The image of a named element fills its group's box.
            host = host.size_full();
        } else if is_named(path) {
            // Named elements are drawn as their own images.
            host.style().visibility = Some(gpui::Visibility::Hidden);
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
        runtime: ElementRuntime<'_>,
        focus_styles: bool,
        text_input: Option<&Entity<RuntimeTextInput>>,
        cx: &mut App,
    ) -> Option<FocusHandle> {
        let focusable = focus_styles
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

    /// The element's interaction state as of this frame, before its
    /// children render so they can depend on it.
    fn interaction(&self, element_id: &ElementId, window: &Window, cx: &App) -> Interaction {
        let forced_hover = self.hovered_element.borrow().as_ref() == Some(element_id);
        let focused = self
            .focus_handles
            .borrow()
            .get(element_id)
            .is_some_and(|handle| handle.is_focused(window))
            || self
                .text_inputs
                .borrow()
                .get(element_id)
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window));
        Interaction {
            hovered: forced_hover || self.pointer_hovered.borrow().contains(element_id),
            focused,
            active: self.pressed.borrow().as_ref() == Some(element_id),
        }
    }

    /// The element's computed style in its current context, reused while
    /// nothing it depends on has changed.
    fn computed(
        &self,
        element_id: &ElementId,
        element: &RenderElement,
        environment: &Environment<'_>,
        interaction: Interaction,
        parent: &ComputedScope,
    ) -> Rc<cascade::Computed> {
        let ancestors = self.interactions.borrow();
        let ancestors_key = if cascade::depends_on_ancestors(element) {
            ancestors.iter().fold(ancestors.len() as u64, |key, state| {
                key.wrapping_mul(8).wrapping_add(u64::from(state.bits()))
            })
        } else {
            0
        };
        let key = (
            parent.fingerprint(),
            environment.key(),
            interaction.bits(),
            ancestors_key,
        );
        if let Some(cached) = self.styles.borrow().elements.get(element_id)
            && cached.key == key
        {
            return cached.computed.clone();
        }
        let declarations = cascade::declarations(element, environment, interaction, &ancestors);
        let starting =
            cascade::starting_declarations(element, environment, interaction, &ancestors);
        let computed = Rc::new(cascade::compute(parent, &declarations, starting.as_deref()));
        self.styles.borrow_mut().elements.insert(
            element_id.clone(),
            CachedStyle {
                key,
                computed: computed.clone(),
            },
        );
        computed
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

pub(crate) fn generated_id(path: &[usize]) -> String {
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

fn collect_named_elements(plan: &RenderPlan) -> HashSet<ElementId> {
    fn collect(nodes: &[RenderNode], parent_path: &[usize], ids: &mut HashSet<ElementId>) {
        for (index, node) in nodes.iter().enumerate() {
            let RenderNode::Element(element) = node else {
                continue;
            };
            let mut path = parent_path.to_vec();
            path.push(index);
            if declares_name(element) {
                let id =
                    attribute(element, "id").map_or_else(|| generated_id(&path), str::to_owned);
                ids.insert(ElementId::new(id));
            }
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

fn available_fonts(cx: &App) -> HashSet<String> {
    cx.text_system()
        .all_font_names()
        .into_iter()
        .map(|family| family.to_ascii_lowercase())
        .collect()
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

    fn of(style: &ComputedStyle) -> Self {
        use htmlswap::computed::Overflow;
        let scrolls =
            |value: Option<Overflow>| matches!(value, Some(Overflow::Scroll | Overflow::Auto));
        Self {
            x: scrolls(style.overflow_x),
            y: scrolls(style.overflow_y),
        }
    }
}

/// What an element computed to, cached while its context is unchanged.
struct CachedStyle {
    key: (u64, u64, u8, u64),
    computed: Rc<cascade::Computed>,
}

/// Computed styles of the rendered elements of one document.
#[derive(Default)]
struct StyleCache {
    elements: HashMap<ElementId, CachedStyle>,
}

/// Elements whose descendants' styles depend on their interaction state,
/// with the states they depend on.
fn collect_state_anchors(plan: &RenderPlan) -> HashMap<ElementId, StateNeeds> {
    fn collect(
        nodes: &[RenderNode],
        path: &mut Vec<usize>,
        ancestors: &mut Vec<ElementId>,
        anchors: &mut HashMap<ElementId, StateNeeds>,
    ) {
        for (index, node) in nodes.iter().enumerate() {
            let RenderNode::Element(element) = node else {
                continue;
            };
            path.push(index);
            let id = ElementId::new(
                attribute(element, "id").map_or_else(|| generated_id(path), str::to_owned),
            );
            for variant in &element.style_variants {
                for condition in &variant.conditions {
                    if let RenderStyleCondition::ElementState {
                        pseudo, ancestor, ..
                    } = condition
                        && *ancestor > 0
                        && let Some(anchor) = ancestors
                            .len()
                            .checked_sub(usize::from(*ancestor))
                            .map(|index| ancestors[index].clone())
                    {
                        anchors.entry(anchor).or_default().add(pseudo);
                    }
                }
            }
            ancestors.push(id);
            collect(&element.children, path, ancestors, anchors);
            ancestors.pop();
            path.pop();
        }
    }

    let mut anchors = HashMap::new();
    collect(&plan.nodes, &mut Vec::new(), &mut Vec::new(), &mut anchors);
    anchors
}

/// Properties applied outside the typed style: by the inherited scope,
/// motion, and view transitions.
fn handled_outside_style(property: &str) -> bool {
    property.starts_with("--")
        || property.starts_with("transition")
        || property.starts_with("animation")
        || property.starts_with("view-transition")
        || property.starts_with("font-variant")
        || matches!(
            property,
            "color-scheme" | "text-transform" | "font-kerning" | "font-feature-settings"
        )
}

fn collect_render_diagnostics(plan: &RenderPlan) -> Vec<RenderDiagnostic> {
    let mut diagnostics = Vec::new();
    let root = ComputedScope::root(&MediaEnvironment::default());
    let root_declarations = plan.root.styles.iter().collect::<Vec<_>>();
    let scope = root.document_root(root_declarations.iter().copied());
    diagnose_declarations(
        "html-root",
        &root_declarations,
        &scope,
        &root,
        &mut diagnostics,
    );
    for variant in &plan.root.style_variants {
        diagnose_variant("html-root", variant, &scope, &root, &mut diagnostics);
    }
    collect_node_diagnostics(&plan.nodes, &mut Vec::new(), &scope, &mut diagnostics);
    diagnostics
}

fn collect_node_diagnostics(
    nodes: &[RenderNode],
    path: &mut Vec<usize>,
    parent: &ComputedScope,
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
            .collect::<Vec<_>>();
        let scope = parent.child(declarations.iter().copied());
        diagnose_declarations(&id, &declarations, &scope, parent, diagnostics);
        for variant in &element.style_variants {
            diagnose_variant(&id, variant, &scope, parent, diagnostics);
        }
        if !element.dynamic_styles.is_empty() || !element.pseudo_elements.is_empty() {
            diagnostics.push(RenderDiagnostic {
                node_id: id.clone(),
                feature: "dynamic or pseudo-element CSS".to_owned(),
                message: "dynamic styles and pseudo-elements are preserved but unsupported by the live GPUI renderer"
                    .to_owned(),
            });
        }
        collect_node_diagnostics(&element.children, path, &scope, diagnostics);
        path.pop();
    }
}

fn diagnose_variant(
    node_id: &str,
    variant: &RenderStyleVariant,
    scope: &ComputedScope,
    parent: &ComputedScope,
    diagnostics: &mut Vec<RenderDiagnostic>,
) {
    if cascade::supported(variant) {
        let declarations = variant.declarations.iter().collect::<Vec<_>>();
        diagnose_declarations(node_id, &declarations, scope, parent, diagnostics);
    } else {
        diagnostics.push(RenderDiagnostic {
            node_id: node_id.to_owned(),
            feature: "conditional CSS".to_owned(),
            message: format!(
                "live renderer does not support style conditions {:?}",
                variant.conditions
            ),
        });
    }
}

fn diagnose_declarations(
    node_id: &str,
    declarations: &[&StyleDeclaration],
    scope: &ComputedScope,
    parent: &ComputedScope,
    diagnostics: &mut Vec<RenderDiagnostic>,
) {
    let mut push = |feature: &str, message: String| {
        let diagnostic = RenderDiagnostic {
            node_id: node_id.to_owned(),
            feature: feature.to_owned(),
            message,
        };
        if !diagnostics.contains(&diagnostic) {
            diagnostics.push(diagnostic);
        }
    };
    let resolved = declarations
        .iter()
        .filter(|declaration| !handled_outside_style(declaration.property.as_str()))
        .filter_map(|declaration| scope.resolve(declaration))
        .collect::<Vec<_>>();
    let style = ComputedStyle::compute(
        resolved.iter().map(std::borrow::Cow::as_ref),
        &scope.style_context(parent),
        |declaration, reason| {
            let property = declaration.property.as_str();
            let value = declaration.value.as_str().trim();
            if property == "outline" && matches!(value, "none" | "0") {
                return;
            }
            let message = match reason {
                Unsupported::Property => {
                    format!("live renderer does not support the {property} property")
                }
                Unsupported::Invalid => format!("invalid {property} value {value:?}"),
                Unsupported::Value(reason) => reason.to_owned(),
            };
            push(property, message);
        },
    );
    for (property, reason) in gpui_style::limits(&style) {
        push(property, reason.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use gpui::Role as AccessibleRole;
    use gpui_mcp::Automation;
    use htmlswap::computed::{ComputedScope, MediaEnvironment};
    use htmlswap::{RenderNode, StyleDeclaration};

    use crate::{
        Binding, BindingDocument, BindingTarget, ElementId, HandlerId, HookRegistry, HtmlUi,
        StateValue, UiEvent, UiProperty,
    };

    use super::{
        ReloadError, SemanticNamespace, accessible_label, aria_role, diagnose_declarations,
        update_hovered_element,
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

    fn diagnose(declarations: &[(&str, &str)]) -> Vec<super::RenderDiagnostic> {
        let declarations = declarations
            .iter()
            .map(|(property, value)| StyleDeclaration::new(*property, *value, false, None))
            .collect::<Vec<_>>();
        let declarations = declarations.iter().collect::<Vec<_>>();
        let root = ComputedScope::root(&MediaEnvironment::default());
        let scope = root.child(declarations.iter().copied());
        let mut diagnostics = Vec::new();
        diagnose_declarations("test", &declarations, &scope, &root, &mut diagnostics);
        diagnostics
    }

    #[test]
    fn grid_css_is_supported_and_invalid_values_are_diagnosed() {
        let supported = diagnose(&[
            ("grid-template-columns", "240px repeat(2, minmax(0, 1fr))"),
            ("grid-template-rows", "auto 1fr fit-content(120px)"),
            ("grid-auto-rows", "minmax(32px, auto)"),
            ("grid-auto-flow", "row dense"),
            ("grid-row", "2 / span 2"),
            ("grid-column-start", "-1"),
            ("row-gap", "8px"),
            ("column-gap", "12px"),
        ]);
        assert!(supported.is_empty(), "{supported:#?}");

        let rejected = diagnose(&[
            ("grid-column", "0 / span 2"),
            ("grid-auto-flow", "diagonal"),
        ]);
        assert_eq!(rejected.len(), 2, "{rejected:#?}");
    }

    #[test]
    fn responsive_layout_css_is_a_supported_live_renderer_contract() {
        let diagnostics = diagnose(&[
            ("box-sizing", "border-box"),
            ("width", "100%"),
            ("height", "100%"),
            ("min-width", "0"),
            ("max-width", "80vw"),
            ("flex", "1 1 0%"),
            ("flex-grow", "1"),
            ("flex-shrink", "0"),
            ("flex-basis", "240px"),
            ("overflow", "hidden"),
            ("padding", "10px 14px"),
            ("margin", "0 auto"),
            ("border-bottom", "1px solid #393b31"),
            ("position", "absolute"),
            ("inset", "0 12px"),
            ("align-self", "center"),
            ("grid-column", "1 / span 2"),
            ("white-space", "nowrap"),
            ("text-align", "center"),
            ("text-overflow", "ellipsis"),
            ("cursor", "pointer"),
            ("opacity", "0.76"),
            ("box-shadow", "0 30px 80px rgba(0, 0, 0, 0.6)"),
            ("line-height", "1.6"),
            ("color", "light-dark(#111, #eee)"),
            ("outline", "none"),
        ]);

        assert!(diagnostics.is_empty(), "{diagnostics:#?}");
    }

    #[test]
    fn invalid_and_undrawable_values_are_diagnosed_once_each() {
        let diagnostics = diagnose(&[
            ("flex", "grow please"),
            ("border", "wavy 1px red"),
            ("cursor", "magic"),
            ("position", "sticky"),
            ("width", "calc(50% + 10px)"),
            ("border-style", "double"),
            ("letter-spacing", "1px"),
            ("height", "50%"),
            ("padding-top", "4px"),
        ]);
        let properties = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.feature.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            properties,
            [
                "flex",
                "border",
                "cursor",
                "position",
                "width",
                "height",
                "border-style",
                "letter-spacing"
            ],
            "{diagnostics:#?}"
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
