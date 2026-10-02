//! CSS view transitions for GPUI Kit, on `gpui_base::motion`.
//!
//! A view transition animates a view from one state to the next through the
//! standard pseudo-element tree:
//!
//! - `::view-transition-group(name)` moves each named element from its old
//!   box to its new one;
//! - `::view-transition-old(name)` shows the outgoing element, by default
//!   fading out;
//! - `::view-transition-new(name)` shows the incoming element, by default
//!   fading in;
//! - `root` names the rest of the view.
//!
//! GPUI keeps neither pixels nor elements across frames, so the caller draws
//! the outgoing state again from a snapshot it hands to
//! [`ViewTransitions::start`] (an `Old` value of its choosing). The incoming
//! state is the live view, which stays interactive throughout.
//!
//! Each frame, a view:
//!
//! 1. calls [`ViewTransitions::begin_frame`];
//! 2. wraps every element that has a `view-transition-name` with
//!    [`ViewTransitions::named`], which measures it for future captures and
//!    moves it during a transition;
//! 3. wraps its root with [`ViewTransitions::stage`], which draws the old
//!    images over it;
//! 4. calls [`ViewTransitions::end_frame`].
//!
//! Paint order follows the pseudo-element tree, which sits in the top layer:
//! the new view, then the old root image, then each named element lifted
//! above it, then the old named images.
//!
//! ```rust,ignore
//! struct Gallery {
//!     page: Page,
//!     transitions: ViewTransitions<Page>,
//! }
//!
//! impl Gallery {
//!     fn open(&mut self, page: Page, cx: &mut Context<Self>) {
//!         // Capture what is on screen, then change state.
//!         let old = self.page.clone();
//!         self.transitions.start(["forward".into()], old, Box::new(|_, _, _| NameStyle::default()));
//!         self.page = page;
//!         cx.notify();
//!     }
//! }
//!
//! impl Render for Gallery {
//!     fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
//!         let transitions = self.transitions.clone();
//!         transitions.begin_frame(Instant::now());
//!         let hero = transitions.named("hero", &[], (), hero(&self.page));
//!         let root = transitions.stage(div().size_full().child(hero), |old, part| match part {
//!             OldPart::Root { .. } => Some(div().size_full().into_any_element()),
//!             OldPart::Named(_) => Some(hero(old).into_any_element()),
//!         });
//!         transitions.end_frame(window);
//!         root
//!     }
//! }
//! ```

mod elements;
mod style;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

extern crate gpui_pre as gpui;

use gpui::{AnyElement, Bounds, IntoElement, Pixels, SharedString, Styled, Window, div};
use gpui::{ParentElement, Point};

pub use gpui_base::motion::{Easing, Keyframe, Keyframes, Timing};
pub use style::{
    DEFAULT_DURATION, Fill, GroupFrame, ImageAnimation, ImageFrame, NameStyle, Offset, Translate,
};

use elements::{Measured, Morph, MorphMotion, OldImage, OldRoot, Shift, Stage};

/// How a transition finds the style of each name: given the name, its
/// `view-transition-class` names and the transition's types.
pub type Resolve = Box<dyn Fn(&str, &[SharedString], &[SharedString]) -> NameStyle>;

/// One named element as last drawn before a transition started.
#[derive(Clone, Debug)]
pub struct Captured<Key> {
    /// Its `view-transition-name`.
    pub name: SharedString,
    /// What the caller passed to [`ViewTransitions::named`] for it, to draw
    /// it again.
    pub key: Key,
    /// Its `view-transition-class` names.
    pub classes: Vec<SharedString>,
    /// Window-relative border box, without transition offsets.
    pub bounds: Bounds<Pixels>,
}

/// A part of the outgoing view to draw again.
#[derive(Debug)]
pub enum OldPart<'a, Key> {
    /// The root, leaving out these named elements, which are drawn as their
    /// own images.
    Root {
        /// The named elements to leave out.
        named: &'a [Captured<Key>],
    },
    /// One named element, filling its group's box.
    Named(&'a Captured<Key>),
}

/// View-transition state of one view. Cloning shares it.
pub struct ViewTransitions<Old, Key = ()> {
    state: Rc<RefCell<State<Old, Key>>>,
}

impl<Old, Key> Clone for ViewTransitions<Old, Key> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl<Old, Key> Default for ViewTransitions<Old, Key> {
    fn default() -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                slots: HashMap::new(),
                order: 0,
                root: Rc::default(),
                shift: Rc::default(),
                active: None,
            })),
        }
    }
}

struct State<Old, Key> {
    slots: HashMap<SharedString, Slot<Key>>,
    /// Visits so far this frame, for document order.
    order: u32,
    root: Measured,
    shift: Shift,
    active: Option<Active<Old, Key>>,
}

struct Slot<Key> {
    key: Key,
    classes: Vec<SharedString>,
    measured: Measured,
    order: u32,
    /// Elements carrying the name this frame.
    seen: u32,
}

struct Active<Old, Key> {
    types: Vec<SharedString>,
    old: Rc<Old>,
    root: Bounds<Pixels>,
    /// In document order.
    named: Vec<Captured<Key>>,
    resolve: Resolve,
    styles: HashMap<SharedString, Rc<NameStyle>>,
    /// Set by the first frame drawn after the transition starts.
    started: Option<Instant>,
    elapsed: Duration,
    /// Whether any part was still animating in the frame being drawn.
    running: bool,
}

impl<Old, Key> Active<Old, Key> {
    fn style(&mut self, name: &str, classes: &[SharedString]) -> Rc<NameStyle> {
        if let Some(style) = self.styles.get(name) {
            return style.clone();
        }
        let style = Rc::new((self.resolve)(name, classes, &self.types));
        self.styles.insert(name.into(), style.clone());
        style
    }
}

impl<Old: 'static, Key: Clone + 'static> ViewTransitions<Old, Key> {
    /// No transition, and nothing measured yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a frame drawn at `now`.
    pub fn begin_frame(&self, now: Instant) {
        let mut state = self.state.borrow_mut();
        state.order = 0;
        for slot in state.slots.values_mut() {
            slot.seen = 0;
        }
        if let Some(active) = &mut state.active {
            let started = *active.started.get_or_insert(now);
            active.elapsed = now.saturating_duration_since(started);
            active.running = false;
        }
    }

    /// Finish a frame: forget names no longer drawn, end a transition that
    /// has finished, and keep frames coming while one runs.
    pub fn end_frame(&self, window: &mut Window) {
        let mut state = self.state.borrow_mut();
        state.slots.retain(|_, slot| slot.seen > 0);
        let Some(active) = &state.active else {
            return;
        };
        if active.started.is_some() && !active.running {
            // This frame already shows the end state; the next one drops
            // the old images.
            state.active = None;
        }
        window.request_animation_frame();
    }

    /// Start a transition from the view as last drawn, like
    /// `document.startViewTransition()`: change state after this, and the
    /// next frame animates to it. `old` is what the caller needs to draw the
    /// outgoing state; `types` are the transition's types; `resolve` styles
    /// each name.
    ///
    /// Returns `false`, and starts nothing, before the view's first frame.
    pub fn start(
        &self,
        types: impl IntoIterator<Item = SharedString>,
        old: Old,
        resolve: Resolve,
    ) -> bool {
        let mut state = self.state.borrow_mut();
        let Some(root) = state.root.get() else {
            return false;
        };
        let mut named = state
            .slots
            .iter()
            .filter(|(_, slot)| slot.seen == 1)
            .filter_map(|(name, slot)| {
                Some((
                    slot.order,
                    Captured {
                        name: name.clone(),
                        key: slot.key.clone(),
                        classes: slot.classes.clone(),
                        bounds: slot.measured.get()?,
                    },
                ))
            })
            .collect::<Vec<_>>();
        named.sort_by_key(|(order, _)| *order);
        state.active = Some(Active {
            types: types.into_iter().collect(),
            old: Rc::new(old),
            root,
            named: named.into_iter().map(|(_, captured)| captured).collect(),
            resolve,
            styles: HashMap::new(),
            started: None,
            elapsed: Duration::ZERO,
            running: false,
        });
        true
    }

    /// End the transition at once, like `ViewTransition.skipTransition()`.
    pub fn skip(&self) {
        self.state.borrow_mut().active = None;
    }

    /// Whether a transition is pending or running.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state.borrow().active.is_some()
    }

    /// Whether a transition has started but not been drawn yet.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.state
            .borrow()
            .active
            .as_ref()
            .is_some_and(|active| active.started.is_none())
    }

    /// The running transition's types, for `:active-view-transition-type()`.
    #[must_use]
    pub fn types(&self) -> Option<Vec<SharedString>> {
        let state = self.state.borrow();
        state.active.as_ref().map(|active| active.types.clone())
    }

    /// Add types to the transition, as a navigation's
    /// `@view-transition { types: … }` does to one that is pending.
    pub fn add_types(&self, types: impl IntoIterator<Item = SharedString>) {
        if let Some(active) = &mut self.state.borrow_mut().active {
            for kind in types {
                if !active.types.contains(&kind) {
                    active.types.push(kind);
                }
            }
        }
    }

    /// The outgoing state of the transition, if one is active.
    #[must_use]
    pub fn old(&self) -> Option<Rc<Old>> {
        let state = self.state.borrow();
        state.active.as_ref().map(|active| active.old.clone())
    }

    /// Replace the outgoing state of a pending transition, before its first
    /// frame. Returns `false` when none is pending.
    pub fn replace_old(&self, old: Old) -> bool {
        let mut state = self.state.borrow_mut();
        match &mut state.active {
            Some(active) if active.started.is_none() => {
                active.old = Rc::new(old);
                true
            }
            _ => false,
        }
    }

    /// A named element of the live view. Measures it for future captures;
    /// during a transition it lifts it above the old root image, moves it
    /// along its group from the old box, and applies its new image's
    /// animation. A name already used this frame does not transition, as in
    /// browsers, while the rest of the transition proceeds.
    pub fn named<E>(
        &self,
        name: impl Into<SharedString>,
        classes: &[SharedString],
        key: Key,
        mut element: E,
    ) -> AnyElement
    where
        E: IntoElement + Styled,
    {
        let name = name.into();
        let mut guard = self.state.borrow_mut();
        let state = &mut *guard;
        state.order += 1;
        let order = state.order;
        let slot = state.slots.entry(name.clone()).or_insert_with(|| Slot {
            key: key.clone(),
            classes: Vec::new(),
            measured: Rc::default(),
            order,
            seen: 0,
        });
        slot.seen += 1;
        if slot.seen > 1 {
            return element.into_any_element();
        }
        slot.key = key;
        slot.order = order;
        slot.classes = classes.to_vec();
        let measured = slot.measured.clone();
        let motion = state.active.as_mut().map(|active| {
            let style = active.style(&name, classes);
            let from = active
                .named
                .iter()
                .find(|captured| captured.name == name)
                .map(|captured| captured.bounds.origin);
            let group = style.group(active.elapsed);
            let image = style.new_image(active.elapsed, from.is_some());
            active.running |= group.running || image.running;
            (
                MorphMotion {
                    from,
                    progress: group.progress,
                    translate: image.translate,
                },
                image.opacity,
            )
        });
        let shift = state.shift.clone();
        drop(guard);
        let motion = motion.map(|(motion, opacity)| {
            fade(&mut element, opacity);
            motion
        });
        Morph::new(element.into_any_element(), measured, shift, motion).into_any_element()
    }

    /// The view's root, with the outgoing view drawn over it while a
    /// transition runs. `draw_old` draws each part of the outgoing view:
    /// its root without the named elements, then each named element. It
    /// draws inert content (no ids, handlers or focus), or nothing.
    pub fn stage<E>(
        &self,
        mut root: E,
        mut draw_old: impl FnMut(&Old, OldPart<'_, Key>) -> Option<AnyElement>,
    ) -> AnyElement
    where
        E: IntoElement + Styled,
    {
        let mut guard = self.state.borrow_mut();
        let state = &mut *guard;
        let measured = state.root.clone();
        let shift = state.shift.clone();
        let Some(active) = state.active.as_mut() else {
            drop(guard);
            return Stage::new(root.into_any_element(), measured, shift).into_any_element();
        };
        let elapsed = active.elapsed;
        let root_style = active.style("root", &[]);
        let old_part = root_style.old(elapsed);
        let new_part = root_style.new_image(elapsed, true);
        let mut running = old_part.running || new_part.running;
        let captured_names = active.named.clone();
        let named = captured_names
            .iter()
            .cloned()
            .map(|captured| {
                let style = active.style(&captured.name, &captured.classes);
                let incoming = state
                    .slots
                    .get(&captured.name)
                    .filter(|slot| slot.seen == 1)
                    .map(|slot| slot.measured.clone());
                (captured, style, incoming)
            })
            .collect::<Vec<_>>();
        let old = active.old.clone();
        let old_root_bounds = active.root;
        drop(guard);

        let mut images = Vec::with_capacity(named.len());
        for (captured, style, incoming) in &named {
            let group = style.group(elapsed);
            let part = style.old(elapsed);
            running |= group.running || part.running;
            let Some(child) = draw_old(&old, OldPart::Named(captured)) else {
                continue;
            };
            let child = div()
                .size_full()
                .opacity(part.opacity)
                .child(child)
                .into_any_element();
            images.push(OldImage::lifted(
                child,
                captured.bounds,
                incoming.clone(),
                group.progress,
                part.translate,
            ));
        }
        if let Some(active) = self.state.borrow_mut().active.as_mut() {
            active.running |= running;
        }
        let old_root = draw_old(
            &old,
            OldPart::Root {
                named: &captured_names,
            },
        );
        let image = div()
            .size_full()
            .child(
                div()
                    .size_full()
                    .opacity(old_part.opacity)
                    .children(old_root),
            )
            .children(images)
            .into_any_element();
        fade(&mut root, new_part.opacity);
        Stage::new(root.into_any_element(), measured, shift)
            .transition(
                new_part.translate,
                OldRoot {
                    image,
                    bounds: old_root_bounds,
                    translate: old_part.translate,
                },
            )
            .into_any_element()
    }
}

/// Multiply an element's own opacity by an image's.
fn fade(element: &mut impl Styled, opacity: f32) {
    if opacity < 1.0 {
        let style = element.style();
        style.opacity = Some(style.opacity.unwrap_or(1.0) * opacity);
    }
}

/// The window-relative position of `translate` on a box of `size`.
fn resolve(translate: Translate, size: gpui::Size<Pixels>) -> Point<Pixels> {
    gpui::point(
        gpui::px(translate.x.resolve(f32::from(size.width))),
        gpui::px(translate.y.resolve(f32::from(size.height))),
    )
}
