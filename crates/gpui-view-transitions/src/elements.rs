//! The elements a transition draws with.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Style, Window, deferred, point, size,
};

use crate::{Translate, resolve};

/// Deferred-draw priorities: lifted new elements, then old images over them.
const NEW_LAYER: usize = 1;
const OLD_LAYER: usize = 2;

/// An element's measured layout box, shared with the frame that drew it.
pub(crate) type Measured = Rc<Cell<Option<Bounds<Pixels>>>>;

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
    offset: Translate,
    measured: Measured,
    shift: Shift,
}

pub(crate) struct OldRoot {
    pub(crate) image: AnyElement,
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) translate: Translate,
}

impl Stage {
    pub(crate) fn new(content: AnyElement, measured: Measured, shift: Shift) -> Self {
        Self {
            content,
            old_root: None,
            offset: Translate::default(),
            measured,
            shift,
        }
    }

    pub(crate) fn transition(mut self, offset: Translate, old_root: OldRoot) -> Self {
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

    fn id(&self) -> Option<ElementId> {
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
    pub(crate) translate: Translate,
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

    fn id(&self) -> Option<ElementId> {
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
    translate: Translate,
}

impl OldImage {
    /// The image, drawn above the lifted new elements.
    pub(crate) fn lifted(
        child: AnyElement,
        from: Bounds<Pixels>,
        to: Option<Measured>,
        progress: f32,
        translate: Translate,
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

    fn id(&self) -> Option<ElementId> {
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
