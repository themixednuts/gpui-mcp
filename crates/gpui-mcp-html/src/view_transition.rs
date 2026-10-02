//! CSS view transitions for the live renderer.
//!
//! A transition captures the outgoing state, then animates to the incoming
//! one through the standard pseudo-element tree:
//!
//! - `::view-transition-group(name)` moves each named element from its old
//!   box to its new one;
//! - `::view-transition-old(name)` shows the outgoing element, by default
//!   fading out;
//! - `::view-transition-new(name)` shows the incoming element, by default
//!   fading in;
//! - `root` names the rest of the document.
//!
//! GPUI keeps neither pixels nor elements across frames, so the outgoing
//! state is drawn by rendering the retained old document inertly (no ids,
//! handlers or focus). The incoming state is the live document itself, so
//! it stays interactive throughout. Paint order follows the pseudo-element
//! tree, which sits in the top layer: the new document, then the old root
//! image, then each named element lifted above it, then the old named
//! images. Because each old image is drawn over its new counterpart, the
//! default cross-fade is exact without `plus-lighter` blending: the new side
//! stays opaque and the old side fades out over it.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId as GpuiElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Style, Window, deferred, point, size,
};
use htmlswap::motion::{Animation, Easing, StepPosition, animations};
use htmlswap::{
    CompactString, RenderMotionPlan, RenderStyleCondition, StyleDeclaration, ViewTransitionPart,
};

use crate::motion::{Animated, Frame, MotionValue, Offset, apply_keyframes, elapsed_ms};
use crate::{Binding, ElementId, HtmlUi, StateValue, UiProperty};

/// Duration of the user-agent group animation when no author rule sets one.
const DEFAULT_DURATION_MS: f32 = 250.0;
const FADE_OUT: &str = "-ua-view-transition-fade-out";
const FADE_IN: &str = "-ua-view-transition-fade-in";
const GROUP: &str = "-ua-view-transition-group";

/// Deferred-draw priorities: lifted new elements, then old images over them.
const NEW_LAYER: usize = 1;
const OLD_LAYER: usize = 2;

/// Whether an element can take part in a transition: it declares
/// `view-transition-name` somewhere, so the name is worth computing.
pub(crate) fn declares_name(element: &htmlswap::RenderElement) -> bool {
    element
        .stylesheet_declarations
        .iter()
        .chain(&element.styles)
        .chain(
            element
                .style_variants
                .iter()
                .flat_map(|variant| &variant.declarations),
        )
        .any(|declaration| declaration.property.as_str() == "view-transition-name")
}

/// The name `view-transition-name` computes to on an element, if any.
pub(crate) fn transition_name<'a>(
    declarations: impl IntoIterator<Item = &'a StyleDeclaration>,
    element_id: &str,
) -> Option<CompactString> {
    let value = declarations
        .into_iter()
        .filter(|declaration| declaration.property.as_str() == "view-transition-name")
        .last()?
        .value
        .as_str()
        .trim();
    match value {
        "" | "none" => None,
        // Both name the element by its identity, which here is its document id.
        "auto" | "match-element" => Some(element_id.into()),
        name => Some(name.into()),
    }
}

/// `view-transition-class` names.
pub(crate) fn transition_classes<'a>(
    declarations: impl IntoIterator<Item = &'a StyleDeclaration>,
) -> Vec<CompactString> {
    declarations
        .into_iter()
        .filter(|declaration| declaration.property.as_str() == "view-transition-class")
        .last()
        .map(|declaration| {
            declaration
                .value
                .as_str()
                .split_whitespace()
                .filter(|class| *class != "none")
                .map(CompactString::from)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) type Measured = Rc<Cell<Option<Bounds<Pixels>>>>;

/// Named elements of the latest render, with their measured boxes.
#[derive(Default)]
pub(crate) struct NamedSlots {
    slots: HashMap<CompactString, Slot>,
}

struct Slot {
    path: Vec<usize>,
    classes: Vec<CompactString>,
    measured: Measured,
    /// Elements carrying the name in the current pass.
    seen: u32,
}

impl NamedSlots {
    pub(crate) fn begin(&mut self) {
        for slot in self.slots.values_mut() {
            slot.seen = 0;
        }
    }

    pub(crate) fn end(&mut self) {
        self.slots.retain(|_, slot| slot.seen > 0);
    }

    /// Record a named element, returning where to measure it, or `None` when
    /// the name is already taken this pass. As in browsers, a duplicate name
    /// does not transition; unlike them, the rest of the transition proceeds.
    pub(crate) fn visit(
        &mut self,
        name: &CompactString,
        path: &[usize],
        classes: Vec<CompactString>,
    ) -> Option<Measured> {
        let slot = match self.slots.get_mut(name) {
            Some(slot) => slot,
            None => self.slots.entry(name.clone()).or_insert_with(|| Slot {
                path: Vec::new(),
                classes: Vec::new(),
                measured: Rc::default(),
                seen: 0,
            }),
        };
        slot.seen += 1;
        if slot.seen > 1 {
            return None;
        }
        if slot.path != path {
            slot.path.clear();
            slot.path.extend_from_slice(path);
        }
        slot.classes = classes;
        Some(slot.measured.clone())
    }

    pub(crate) fn classes(&self, name: &str) -> Option<&[CompactString]> {
        self.slots.get(name).map(|slot| slot.classes.as_slice())
    }

    /// The measured box of the unique element with this name this pass.
    pub(crate) fn incoming(&self, name: &str) -> Option<Measured> {
        self.slots
            .get(name)
            .filter(|slot| slot.seen == 1)
            .map(|slot| slot.measured.clone())
    }

    fn capture(&self) -> HashMap<CompactString, Captured> {
        self.slots
            .iter()
            .filter(|(_, slot)| slot.seen == 1)
            .filter_map(|(name, slot)| {
                Some((
                    name.clone(),
                    Captured {
                        bounds: slot.measured.get()?,
                        path: slot.path.clone(),
                        classes: slot.classes.clone(),
                    },
                ))
            })
            .collect()
    }
}

/// One named element's capture.
#[derive(Clone, Debug)]
pub(crate) struct Captured {
    /// Window-relative border box, without transient transition offsets.
    pub(crate) bounds: Bounds<Pixels>,
    /// Child-index path in its document, to render it again.
    pub(crate) path: Vec<usize>,
    pub(crate) classes: Vec<CompactString>,
}

/// The element at a child-index path.
pub(crate) fn element_at<'a>(
    plan: &'a htmlswap::RenderPlan,
    path: &[usize],
) -> Option<&'a htmlswap::RenderElement> {
    let (first, rest) = path.split_first()?;
    let htmlswap::RenderNode::Element(first) = plan.nodes.get(*first)? else {
        return None;
    };
    let mut element: &htmlswap::RenderElement = first;
    for index in rest {
        let htmlswap::RenderNode::Element(child) = element.children.get(*index)? else {
            return None;
        };
        element = child;
    }
    Some(element)
}

/// Bound property values by element.
pub(crate) type PropertySnapshot = HashMap<ElementId, HashMap<UiProperty, StateValue>>;

/// The outgoing state of a transition.
pub(crate) struct OldState {
    pub(crate) ui: Rc<HtmlUi>,
    pub(crate) bindings: HashMap<ElementId, Rc<[Binding]>>,
    /// Bound property values at capture time, or `None` to read the hooks
    /// live (a document swap leaves application state unchanged).
    pub(crate) properties: Option<PropertySnapshot>,
    pub(crate) disclosures: HashMap<ElementId, bool>,
    /// The document root's window-relative box.
    pub(crate) root: Bounds<Pixels>,
    /// Named elements by `view-transition-name`.
    pub(crate) named: HashMap<CompactString, Captured>,
}

impl OldState {
    pub(crate) fn capture(
        ui: Rc<HtmlUi>,
        bindings: HashMap<ElementId, Rc<[Binding]>>,
        properties: Option<PropertySnapshot>,
        disclosures: HashMap<ElementId, bool>,
        root: Bounds<Pixels>,
        named: &NamedSlots,
    ) -> Self {
        Self {
            ui,
            bindings,
            properties,
            disclosures,
            root,
            named: named.capture(),
        }
    }

    /// Whether an element of the old document is a named one, which the
    /// root image leaves out.
    pub(crate) fn is_named(&self, path: &[usize]) -> bool {
        self.named.values().any(|captured| captured.path == path)
    }
}

/// A running view transition.
pub(crate) struct ViewTransition {
    pub(crate) types: Vec<CompactString>,
    pub(crate) old: OldState,
    /// Set by the first frame drawn after the transition starts.
    started: Option<Instant>,
    /// Milliseconds since the first frame, for the frame being drawn.
    pub(crate) elapsed: f32,
    /// Whether any part was still animating in the frame being drawn.
    pub(crate) running: bool,
    timings: HashMap<CompactString, NameTiming>,
}

impl ViewTransition {
    pub(crate) fn new(types: Vec<CompactString>, old: OldState) -> Self {
        Self {
            types,
            old,
            started: None,
            elapsed: 0.0,
            running: false,
            timings: HashMap::new(),
        }
    }

    /// Whether the transition has been captured but not drawn yet.
    pub(crate) const fn pending(&self) -> bool {
        self.started.is_none()
    }

    /// Advance to the frame drawn at `now`, starting the clock if needed.
    pub(crate) fn begin_frame(&mut self, now: Instant) {
        self.elapsed = elapsed_ms(*self.started.get_or_insert(now), now);
        self.running = false;
    }

    /// The pseudo-element timing of `name`, resolved once per transition.
    pub(crate) fn timing(
        &mut self,
        name: &str,
        classes: &[CompactString],
        motion: &RenderMotionPlan,
        media: &dyn Fn(&RenderStyleCondition) -> bool,
    ) -> &NameTiming {
        if !self.timings.contains_key(name) {
            let timing = NameTiming::resolve(name, classes, &self.types, motion, media);
            self.timings.insert(name.into(), timing);
        }
        &self.timings[name]
    }
}

/// Sampled animation of one image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PartFrame {
    pub(crate) opacity: f32,
    pub(crate) translate: (Offset, Offset),
    /// Whether the part is still animating.
    pub(crate) running: bool,
}

/// Sampled group geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GroupFrame {
    /// Eased progress from the old box (0) to the new box (1).
    pub(crate) progress: f32,
    pub(crate) running: bool,
}

/// One side of an image pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Side {
    Old,
    New,
}

/// How the pseudo-elements of one name animate.
pub(crate) struct NameTiming {
    group: Option<Animation>,
    old: Vec<Animation>,
    new: Vec<Animation>,
    /// Whether author rules animate that side beyond the default fade.
    authored: [bool; 2],
}

impl NameTiming {
    pub(crate) fn resolve(
        name: &str,
        classes: &[CompactString],
        types: &[CompactString],
        motion: &RenderMotionPlan,
        media: &dyn Fn(&RenderStyleCondition) -> bool,
    ) -> Self {
        let declarations = |part| {
            motion
                .view_transition
                .declarations_for(part, name, classes, types, media)
        };
        let group_ua = default_animation(GROUP);
        let group =
            animations(std::iter::once(&group_ua).chain(declarations(ViewTransitionPart::Group)))
                .into_iter()
                .next()
                .filter(|animation| animation.name.is_some());
        // The image pair inherits the group's timing, as the
        // pseudo-elements do in browsers.
        let inherited = group.as_ref().map(inherited_timing).unwrap_or_default();
        let side = |ua: &str, part| {
            let default = default_animation(ua);
            let list = animations(
                std::iter::once(&default)
                    .chain(&inherited)
                    .chain(declarations(part)),
            );
            let authored = list
                .iter()
                .any(|animation| animation.name.as_deref() != Some(ua));
            (list, authored)
        };
        let (old, old_authored) = side(FADE_OUT, ViewTransitionPart::Old);
        let (new, new_authored) = side(FADE_IN, ViewTransitionPart::New);
        Self {
            group,
            old,
            new,
            authored: [old_authored, new_authored],
        }
    }

    pub(crate) fn group(&self, elapsed: f32) -> GroupFrame {
        let Some(animation) = &self.group else {
            return GroupFrame {
                progress: 1.0,
                running: false,
            };
        };
        match animation.phase(elapsed) {
            Some(phase) if !phase.finished => GroupFrame {
                progress: animation.easing.sample(phase.progress),
                running: true,
            },
            _ if elapsed < animation.delay_ms => GroupFrame {
                progress: 0.0,
                running: true,
            },
            _ => GroupFrame {
                progress: 1.0,
                running: false,
            },
        }
    }

    /// Whether a side only plays the default fade.
    pub(crate) const fn default_fade(&self, side: Side) -> bool {
        !self.authored[side as usize]
    }

    /// The old or new image's animation at `elapsed`.
    pub(crate) fn part(&self, side: Side, elapsed: f32, motion: &RenderMotionPlan) -> PartFrame {
        let list = match side {
            Side::Old => &self.old,
            Side::New => &self.new,
        };
        let mut frame = Frame::default();
        frame.values[Animated::Opacity.index()] = Some(MotionValue::Number(1.0));
        let mut running = false;
        for animation in list {
            let Some(name) = animation.name.as_deref() else {
                continue;
            };
            running |= animation.end_ms().is_none_or(|end| elapsed < end);
            let Some(phase) = animation.phase(elapsed) else {
                continue;
            };
            let opacity = match name {
                FADE_OUT => Some(1.0 - animation.easing.sample(phase.progress)),
                FADE_IN => Some(animation.easing.sample(phase.progress)),
                name => {
                    if let Some(keyframes) = motion.keyframes(name) {
                        apply_keyframes(&mut frame, keyframes, animation.easing, phase.progress);
                    }
                    None
                }
            };
            if let Some(opacity) = opacity {
                frame.values[Animated::Opacity.index()] = Some(MotionValue::Number(opacity));
            }
        }
        PartFrame {
            opacity: frame
                .get(Animated::Opacity)
                .and_then(MotionValue::number)
                .unwrap_or(1.0)
                .clamp(0.0, 1.0),
            translate: frame
                .get(Animated::Translate)
                .and_then(MotionValue::translate)
                .unwrap_or((Offset::ZERO, Offset::ZERO)),
            running,
        }
    }
}

fn default_animation(name: &str) -> StyleDeclaration {
    StyleDeclaration::new(
        "animation",
        format!("{DEFAULT_DURATION_MS}ms ease both {name}"),
        false,
        None,
    )
}

/// Longhands that hand the group's timing down to its image pair.
fn inherited_timing(group: &Animation) -> Vec<StyleDeclaration> {
    let easing = match group.easing {
        Easing::Linear => "linear".to_owned(),
        Easing::CubicBezier(x1, y1, x2, y2) => format!("cubic-bezier({x1}, {y1}, {x2}, {y2})"),
        Easing::Steps(count, position) => format!(
            "steps({count}, {})",
            match position {
                StepPosition::JumpStart => "jump-start",
                StepPosition::JumpEnd => "jump-end",
                StepPosition::JumpNone => "jump-none",
                StepPosition::JumpBoth => "jump-both",
            }
        ),
    };
    vec![
        StyleDeclaration::new(
            "animation-duration",
            format!("{}ms", group.duration_ms),
            false,
            None,
        ),
        StyleDeclaration::new(
            "animation-delay",
            format!("{}ms", group.delay_ms),
            false,
            None,
        ),
        StyleDeclaration::new("animation-timing-function", easing, false, None),
    ]
}

fn resolve(translate: (Offset, Offset), size: gpui::Size<Pixels>) -> Point<Pixels> {
    point(
        translate.0.resolve(size.width),
        translate.1.resolve(size.height),
    )
}

fn lerp_bounds(from: Bounds<Pixels>, to: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let lerp = |a: Pixels, b: Pixels| a + (b - a) * t;
    Bounds::new(
        point(
            lerp(from.origin.x, to.origin.x),
            lerp(from.origin.y, to.origin.y),
        ),
        size(
            lerp(from.size.width, to.size.width),
            lerp(from.size.height, to.size.height),
        ),
    )
}

/// Offset applied to the subtree being prepainted by transition elements,
/// which measurements subtract to report layout boxes.
pub(crate) type Shift = Rc<Cell<Point<Pixels>>>;

/// The document, with the old root image over it while a transition runs.
pub(crate) struct Stage {
    content: AnyElement,
    /// The old root image, laid out at the old root's box.
    old_root: Option<OldRoot>,
    /// The new root's `translate`.
    offset: (Offset, Offset),
    measured: Measured,
    shift: Shift,
}

pub(crate) struct OldRoot {
    pub(crate) image: AnyElement,
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) translate: (Offset, Offset),
}

impl Stage {
    pub(crate) fn new(content: AnyElement, measured: Measured, shift: Shift) -> Self {
        Self {
            content,
            old_root: None,
            offset: (Offset::ZERO, Offset::ZERO),
            measured,
            shift,
        }
    }

    pub(crate) fn transition(mut self, offset: (Offset, Offset), old_root: OldRoot) -> Self {
        self.offset = offset;
        self.old_root = Some(old_root);
        self
    }
}

impl IntoElement for Stage {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Stage {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<GpuiElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let outer = self.shift.get();
        self.measured
            .set(Some(Bounds::new(bounds.origin - outer, bounds.size)));
        let offset = resolve(self.offset, bounds.size);
        self.shift.set(outer + offset);
        window.with_element_offset(offset, |window| self.content.prepaint(window, cx));
        self.shift.set(outer);
        if let Some(old_root) = &mut self.old_root {
            let space = size(
                AvailableSpace::Definite(old_root.bounds.size.width),
                AvailableSpace::Definite(old_root.bounds.size.height),
            );
            let origin = old_root.bounds.origin + resolve(old_root.translate, old_root.bounds.size);
            old_root.image.prepaint_as_root(origin, space, window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        (): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.paint(window, cx);
        if let Some(old_root) = &mut self.old_root {
            old_root.image.paint(window, cx);
        }
    }
}

/// A named element of the live document. Always measures the element for a
/// future capture; during a transition it also lifts the element above the
/// old root image and moves it along its group's path from the old box.
pub(crate) struct Morph {
    child: AnyElement,
    measured: Measured,
    shift: Shift,
    motion: Option<MorphMotion>,
}

#[derive(Clone, Copy)]
pub(crate) struct MorphMotion {
    /// The old box's origin, when the name existed before.
    pub(crate) from: Option<Point<Pixels>>,
    pub(crate) progress: f32,
    pub(crate) translate: (Offset, Offset),
}

impl Morph {
    pub(crate) fn new(
        child: AnyElement,
        measured: Measured,
        shift: Shift,
        motion: Option<MorphMotion>,
    ) -> Self {
        let child = if motion.is_some() {
            deferred(child).with_priority(NEW_LAYER).into_any_element()
        } else {
            child
        };
        Self {
            child,
            measured,
            shift,
            motion,
        }
    }
}

impl IntoElement for Morph {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Morph {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<GpuiElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let outer = self.shift.get();
        let layout = Bounds::new(bounds.origin - outer, bounds.size);
        self.measured.set(Some(layout));
        let Some(motion) = self.motion else {
            self.child.prepaint(window, cx);
            return;
        };
        // A group is positioned on its own, not by its ancestors' transition
        // offsets, so those are undone here.
        let travel = motion.from.map_or_else(Point::default, |from| {
            (from - layout.origin) * (1.0 - motion.progress)
        });
        let target = travel + resolve(motion.translate, bounds.size);
        self.shift.set(target);
        window.with_element_offset(target - outer, |window| self.child.prepaint(window, cx));
        self.shift.set(outer);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        (): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// An old named image: the outgoing element, stretched over its group's
/// box as the group moves from the old box to the new one.
pub(crate) struct OldImage {
    child: AnyElement,
    from: Bounds<Pixels>,
    to: Option<Measured>,
    progress: f32,
    translate: (Offset, Offset),
}

impl OldImage {
    /// The image, drawn above the lifted new elements.
    pub(crate) fn lifted(
        child: AnyElement,
        from: Bounds<Pixels>,
        to: Option<Measured>,
        progress: f32,
        translate: (Offset, Offset),
    ) -> AnyElement {
        deferred(Self {
            child,
            from,
            to,
            progress,
            translate,
        })
        .with_priority(OLD_LAYER)
        .into_any_element()
    }
}

impl IntoElement for OldImage {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for OldImage {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<GpuiElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Out of flow: the image is laid out on its own when drawn, once the
        // new box has been measured.
        let style = Style {
            position: gpui::Position::Absolute,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let to = self
            .to
            .as_ref()
            .and_then(|measured| measured.get())
            .unwrap_or(self.from);
        let rect = lerp_bounds(self.from, to, self.progress);
        let origin = rect.origin + resolve(self.translate, rect.size);
        let space = size(
            AvailableSpace::Definite(rect.size.width),
            AvailableSpace::Definite(rect.size.height),
        );
        self.child.prepaint_as_root(origin, space, window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        (): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use htmlswap::{RenderMotionPlan, RenderStyleCondition, StyleDeclaration};

    use super::{NameTiming, Side, transition_classes, transition_name};

    fn declarations(pairs: &[(&str, &str)]) -> Vec<StyleDeclaration> {
        pairs
            .iter()
            .map(|(property, value)| StyleDeclaration::new(*property, *value, false, None))
            .collect()
    }

    #[test]
    fn names_and_classes_compute_like_css() {
        let hero = declarations(&[
            ("view-transition-name", "card"),
            ("view-transition-name", "hero"),
            ("view-transition-class", "photo big"),
        ]);
        assert_eq!(transition_name(&hero, "x").as_deref(), Some("hero"));
        assert_eq!(transition_classes(&hero), ["photo", "big"]);
        let none = declarations(&[("view-transition-name", "none")]);
        assert_eq!(transition_name(&none, "x"), None);
        let auto = declarations(&[("view-transition-name", "match-element")]);
        assert_eq!(transition_name(&auto, "card-7").as_deref(), Some("card-7"));
    }

    #[test]
    fn default_parts_cross_fade_over_the_group_duration() {
        let motion = RenderMotionPlan::default();
        let no_media = |_: &RenderStyleCondition| false;
        let timing = NameTiming::resolve("root", &[], &[], &motion, &no_media);
        assert!(timing.default_fade(Side::Old) && timing.default_fade(Side::New));
        let group = timing.group(125.0);
        assert!(group.running && group.progress > 0.0 && group.progress < 1.0);
        let old = timing.part(Side::Old, 0.0, &motion);
        let new = timing.part(Side::New, 0.0, &motion);
        assert!((old.opacity - 1.0).abs() < 1e-3);
        assert!(new.opacity.abs() < 1e-3);
        let done = timing.part(Side::New, 400.0, &motion);
        assert!(!done.running && (done.opacity - 1.0).abs() < 1e-3);
        assert!(!timing.group(400.0).running);
    }
}
