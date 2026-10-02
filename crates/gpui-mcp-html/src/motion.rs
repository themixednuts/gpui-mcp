//! CSS transitions and `@keyframes` animations for the live renderer.
//!
//! Each element keeps one track per animated property. A track remembers
//! where the property started, where it is going, and when, on the window
//! executor's clock. When an element's computed value changes the track
//! restarts from the value currently on screen, so an interrupted
//! transition reverses smoothly, as in a browser. Sampling a track
//! allocates nothing.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{
    AnyElement, App, Bounds, DefiniteLength, Element, ElementId as GpuiElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Window, point, px,
};
use htmlswap::motion::{Animation, Easing, Transition};
use htmlswap::{RenderKeyframes, RenderMotionPlan, StyleDeclaration};

use crate::ElementId;

/// A property the renderer can interpolate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Animated {
    BackgroundColor,
    Color,
    BorderColor,
    Opacity,
    Width,
    Height,
    Translate,
}

impl Animated {
    pub(crate) const ALL: [Self; 7] = [
        Self::BackgroundColor,
        Self::Color,
        Self::BorderColor,
        Self::Opacity,
        Self::Width,
        Self::Height,
        Self::Translate,
    ];

    pub(crate) const fn css_name(self) -> &'static str {
        match self {
            Self::BackgroundColor => "background-color",
            Self::Color => "color",
            Self::BorderColor => "border-color",
            Self::Opacity => "opacity",
            Self::Width => "width",
            Self::Height => "height",
            Self::Translate => "translate",
        }
    }

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    /// Parse a declaration of this property, including the shorthands and
    /// equivalent forms that set it.
    fn parse(self, declaration: &StyleDeclaration) -> Option<MotionValue> {
        let name = declaration.property.as_str();
        let value = declaration.value.as_str().trim();
        let lowered = value.to_ascii_lowercase();
        match self {
            Self::BackgroundColor if matches!(name, "background-color" | "background") => {
                crate::render::color(&lowered).map(MotionValue::color)
            }
            Self::Color if name == "color" => {
                crate::render::color(&lowered).map(MotionValue::color)
            }
            Self::BorderColor if name == "border-color" => {
                crate::render::color(&lowered).map(MotionValue::color)
            }
            Self::Opacity if name == "opacity" => {
                crate::render::opacity(&lowered).map(MotionValue::Number)
            }
            Self::Width if name == "width" => length(&lowered),
            Self::Height if name == "height" => length(&lowered),
            Self::Translate if name == "translate" => translate(&lowered),
            Self::Translate if name == "transform" => transform_translate(&lowered),
            _ => None,
        }
    }

    /// The value an element has when nothing sets the property.
    const fn initial(self) -> Option<MotionValue> {
        match self {
            Self::BackgroundColor => Some(MotionValue::Color([0.0; 4])),
            Self::Opacity => Some(MotionValue::Number(1.0)),
            Self::Translate => Some(MotionValue::Translate(Offset::ZERO, Offset::ZERO)),
            // Inherited colors and `auto` sizes are not interpolated.
            Self::Color | Self::BorderColor | Self::Width | Self::Height => None,
        }
    }
}

/// One component of a translation: `px + fraction × own size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Offset {
    pub(crate) px: f32,
    pub(crate) fraction: f32,
}

impl Offset {
    pub(crate) const ZERO: Self = Self {
        px: 0.0,
        fraction: 0.0,
    };

    fn lerp(self, to: Self, t: f32) -> Self {
        Self {
            px: lerp(self.px, to.px, t),
            fraction: lerp(self.fraction, to.fraction, t),
        }
    }

    pub(crate) fn resolve(self, size: Pixels) -> Pixels {
        px(self.px + self.fraction * f32::from(size))
    }
}

/// An interpolable computed value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MotionValue {
    /// Premultiplied linear-in-sRGB RGBA in `0..=1`, which is how browsers
    /// interpolate legacy colors without darkening fades through transparent.
    Color([f32; 4]),
    Number(f32),
    Pixels(f32),
    /// A percentage of the containing block, as `0..=1`.
    Fraction(f32),
    Translate(Offset, Offset),
}

impl MotionValue {
    fn color(rgba: u32) -> Self {
        let [r, g, b, a] = rgba.to_be_bytes().map(|byte| f32::from(byte) / 255.0);
        Self::Color([r * a, g * a, b * a, a])
    }

    /// `0xRRGGBBAA` for a color value.
    pub(crate) fn rgba(self) -> Option<u32> {
        let Self::Color([r, g, b, a]) = self else {
            return None;
        };
        // Clamped to 0..=255 first, so the cast cannot truncate or wrap.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
        let unpremultiply = |channel: f32| if a > 0.0 { channel / a } else { 0.0 };
        Some(
            byte(unpremultiply(r)) << 24
                | byte(unpremultiply(g)) << 16
                | byte(unpremultiply(b)) << 8
                | byte(a),
        )
    }

    pub(crate) fn number(self) -> Option<f32> {
        match self {
            Self::Number(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn length(self) -> Option<DefiniteLength> {
        match self {
            Self::Pixels(value) => Some(px(value).into()),
            Self::Fraction(value) => Some(DefiniteLength::Fraction(value)),
            _ => None,
        }
    }

    pub(crate) fn translate(self) -> Option<(Offset, Offset)> {
        match self {
            Self::Translate(x, y) => Some((x, y)),
            _ => None,
        }
    }

    /// Interpolate, or `None` when the values have no common form (such as
    /// pixels and percentages, or `auto`), in which case CSS switches at once.
    fn lerp(self, to: Self, t: f32) -> Option<Self> {
        Some(match (self, to) {
            (Self::Color(from), Self::Color(to)) => {
                Self::Color(std::array::from_fn(|index| lerp(from[index], to[index], t)))
            }
            (Self::Number(from), Self::Number(to)) => Self::Number(lerp(from, to, t)),
            (Self::Pixels(from), Self::Pixels(to)) => Self::Pixels(lerp(from, to, t)),
            (Self::Fraction(from), Self::Fraction(to)) => Self::Fraction(lerp(from, to, t)),
            (Self::Translate(from_x, from_y), Self::Translate(to_x, to_y)) => {
                Self::Translate(from_x.lerp(to_x, t), from_y.lerp(to_y, t))
            }
            _ => return None,
        })
    }
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

fn length(value: &str) -> Option<MotionValue> {
    if value == "0" {
        return Some(MotionValue::Pixels(0.0));
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .map(|p| MotionValue::Fraction(p / 100.0));
    }
    if let Some(pixels) = value.strip_suffix("px") {
        return pixels.trim().parse().ok().map(MotionValue::Pixels);
    }
    if let Some(rems) = value.strip_suffix("rem") {
        return rems
            .trim()
            .parse::<f32>()
            .ok()
            .map(|r| MotionValue::Pixels(r * 16.0));
    }
    None
}

fn offset(value: &str) -> Option<Offset> {
    match length(value)? {
        MotionValue::Pixels(px) => Some(Offset { px, fraction: 0.0 }),
        MotionValue::Fraction(fraction) => Some(Offset { px: 0.0, fraction }),
        _ => None,
    }
}

/// `translate: x [y [z]]`.
pub(crate) fn translate(value: &str) -> Option<MotionValue> {
    if value == "none" {
        return Some(MotionValue::Translate(Offset::ZERO, Offset::ZERO));
    }
    let mut parts = value.split_whitespace();
    let x = offset(parts.next()?)?;
    let y = parts.next().map_or(Some(Offset::ZERO), offset)?;
    Some(MotionValue::Translate(x, y))
}

/// The translation in `transform: translate(…)`, `translateX(…)` or
/// `translateY(…)`; other transform functions are not animated.
fn transform_translate(value: &str) -> Option<MotionValue> {
    if value == "none" {
        return Some(MotionValue::Translate(Offset::ZERO, Offset::ZERO));
    }
    let (function, arguments) = value.strip_suffix(')')?.split_once('(')?;
    let arguments: Vec<&str> = arguments.split(',').map(str::trim).collect();
    let (x, y) = match (function.trim(), arguments.as_slice()) {
        ("translate" | "translatex", [x]) => (offset(x)?, Offset::ZERO),
        ("translate", [x, y]) => (offset(x)?, offset(y)?),
        ("translatey", [y]) => (Offset::ZERO, offset(y)?),
        _ => return None,
    };
    Some(MotionValue::Translate(x, y))
}

/// Computed values for every animated property, from declarations in
/// cascade order (later wins).
pub(crate) fn computed<'a>(
    declarations: impl IntoIterator<Item = &'a StyleDeclaration>,
) -> [Option<MotionValue>; 7] {
    let mut values = [None; 7];
    let mut important = [false; 7];
    for declaration in declarations {
        for property in Animated::ALL {
            if let Some(value) = property.parse(declaration) {
                let index = property.index();
                if declaration.important || !important[index] {
                    values[index] = Some(value);
                    important[index] |= declaration.important;
                }
            }
        }
    }
    values
}

#[derive(Clone, Debug)]
struct Track {
    from: MotionValue,
    to: MotionValue,
    started: Instant,
    transition: Option<Transition>,
}

impl Track {
    fn settled(value: MotionValue, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            started: now,
            transition: None,
        }
    }

    /// The value at `now`, and whether it is still moving.
    fn sample(&self, now: Instant) -> (MotionValue, bool) {
        let Some(transition) = &self.transition else {
            return (self.to, false);
        };
        let elapsed = elapsed_ms(self.started, now);
        match transition.progress(elapsed) {
            Some(t) => (self.from.lerp(self.to, t).unwrap_or(self.to), true),
            None => (self.to, false),
        }
    }
}

#[derive(Clone, Debug)]
struct RunningAnimation {
    name: htmlswap::CompactString,
    started: Instant,
}

#[derive(Debug, Default)]
struct ElementMotion {
    tracks: [Option<Track>; 7],
    animations: Vec<RunningAnimation>,
    touched: u64,
}

/// What an element's motion produced for one frame.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Frame {
    pub(crate) values: [Option<MotionValue>; 7],
    pub(crate) moving: bool,
}

impl Frame {
    pub(crate) fn get(&self, property: Animated) -> Option<MotionValue> {
        self.values[property.index()]
    }
}

/// Inputs for one element's motion this frame.
pub(crate) struct Inputs<'a> {
    /// Computed values after the cascade, interaction state and bindings.
    pub(crate) targets: [Option<MotionValue>; 7],
    /// Values from `@starting-style`, used the first time the element is seen.
    pub(crate) starting: [Option<MotionValue>; 7],
    pub(crate) transitions: &'a [Transition],
    pub(crate) animations: &'a [Animation],
    pub(crate) keyframes: &'a RenderMotionPlan,
}

/// Motion for every rendered element of one live document.
#[derive(Debug, Default)]
pub(crate) struct MotionState {
    elements: HashMap<ElementId, ElementMotion>,
    generation: u64,
}

impl MotionState {
    /// Start a render pass.
    pub(crate) fn begin(&mut self) {
        self.generation += 1;
    }

    /// Forget elements that the last render pass did not draw.
    pub(crate) fn end(&mut self) {
        let generation = self.generation;
        self.elements
            .retain(|_, motion| motion.touched == generation);
    }

    /// Advance one element and return the values to draw.
    pub(crate) fn frame(&mut self, id: &ElementId, inputs: &Inputs<'_>, now: Instant) -> Frame {
        let generation = self.generation;
        let seen = self.elements.contains_key(id);
        if !seen {
            self.elements.insert(id.clone(), ElementMotion::default());
        }
        let Some(motion) = self.elements.get_mut(id) else {
            return Frame::default();
        };
        motion.touched = generation;
        let mut frame = Frame::default();

        for property in Animated::ALL {
            let index = property.index();
            let target = inputs.targets[index].or_else(|| property.initial());
            let Some(target) = target else {
                motion.tracks[index] = None;
                continue;
            };
            let transition =
                Transition::for_property(inputs.transitions, property.css_name()).cloned();
            let track = match motion.tracks[index].take() {
                // An entry transition from @starting-style.
                None if !seen => match (inputs.starting[index], &transition) {
                    (Some(start), Some(_)) => Track {
                        from: start,
                        to: target,
                        started: now,
                        transition,
                    },
                    _ => Track::settled(target, now),
                },
                None => Track::settled(target, now),
                Some(track) if track.to == target => track,
                Some(track) => {
                    let (current, _) = track.sample(now);
                    match transition {
                        Some(transition) if current.lerp(target, 0.0).is_some() => Track {
                            from: current,
                            to: target,
                            started: now,
                            transition: Some(transition),
                        },
                        _ => Track::settled(target, now),
                    }
                }
            };
            let (value, moving) = track.sample(now);
            // An initial value is drawn only while it animates, so it never
            // overrides styling the motion engine does not model, such as a
            // gradient background.
            if inputs.targets[index].is_some() || moving {
                frame.values[index] = Some(value);
            }
            frame.moving |= moving;
            motion.tracks[index] = Some(track);
        }

        sync_animations(&mut motion.animations, inputs.animations, now);
        for (animation, running) in inputs.animations.iter().zip(&motion.animations) {
            let Some(keyframes) = animation
                .name
                .as_deref()
                .and_then(|name| inputs.keyframes.keyframes(name))
            else {
                continue;
            };
            let elapsed = elapsed_ms(running.started, now);
            frame.moving |= animation.end_ms().is_none_or(|end| elapsed < end);
            if let Some(phase) = animation.phase(elapsed) {
                apply_keyframes(&mut frame, keyframes, animation.easing, phase.progress);
            }
        }
        frame
    }
}

/// Keep each running animation's start time while its name is unchanged.
fn sync_animations(running: &mut Vec<RunningAnimation>, wanted: &[Animation], now: Instant) {
    let same = running.len() == wanted.len()
        && running
            .iter()
            .zip(wanted)
            .all(|(running, wanted)| wanted.name.as_deref() == Some(running.name.as_str()));
    if same {
        return;
    }
    let previous = std::mem::take(running);
    running.extend(wanted.iter().map(|animation| {
        let name = animation.name.clone().unwrap_or_default();
        let started = previous
            .iter()
            .find(|running| running.name == name)
            .map_or(now, |running| running.started);
        RunningAnimation { name, started }
    }));
}

/// Overlay keyframe values at `progress` onto the frame's underlying values.
pub(crate) fn apply_keyframes(
    frame: &mut Frame,
    keyframes: &RenderKeyframes,
    easing: Easing,
    progress: f32,
) {
    for property in Animated::ALL {
        let Some(interval) = keyframes.interval(property.css_name(), progress) else {
            continue;
        };
        let index = property.index();
        let underlying = frame.values[index].or_else(|| property.initial());
        let parse = |declaration: Option<&StyleDeclaration>| match declaration {
            Some(declaration) => property.parse(declaration),
            None => underlying,
        };
        let (Some(from), Some(to)) = (parse(interval.from), parse(interval.to)) else {
            continue;
        };
        let easing = interval
            .start
            .and_then(|frame| {
                frame
                    .declarations
                    .iter()
                    .rev()
                    .find(|d| d.property.as_str() == "animation-timing-function")
            })
            .and_then(|declaration| Easing::parse(declaration.value.as_str()))
            .unwrap_or(easing);
        let t = easing.sample(interval.progress);
        frame.values[index] = Some(from.lerp(to, t).unwrap_or(if t < 0.5 { from } else { to }));
    }
}

pub(crate) fn elapsed_ms(started: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(started).as_secs_f32() * 1000.0
}

/// Paints its child moved by a `translate`, resolved against the child's own
/// laid-out size. The move happens in prepaint, so layout, siblings and
/// scroll extents are unaffected, while hit testing and semantic bounds
/// follow the painted position.
pub(crate) struct Translated {
    child: AnyElement,
    x: Offset,
    y: Offset,
}

impl Translated {
    pub(crate) fn new(child: AnyElement, x: Offset, y: Offset) -> Self {
        Self { child, x, y }
    }
}

impl IntoElement for Translated {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Translated {
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
        let offset = point(
            self.x.resolve(bounds.size.width),
            self.y.resolve(bounds.size.height),
        );
        window.with_element_offset(offset, |window| self.child.prepaint(window, cx));
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
    use std::time::{Duration, Instant};

    use htmlswap::StyleDeclaration;
    use htmlswap::motion::{Easing, Transition, TransitionProperty};

    use super::{Animated, Inputs, MotionState, MotionValue, Offset, computed, translate};
    use crate::ElementId;

    fn declarations(pairs: &[(&str, &str)]) -> Vec<StyleDeclaration> {
        pairs
            .iter()
            .map(|(property, value)| StyleDeclaration::new(*property, *value, false, None))
            .collect()
    }

    fn linear(property: &str, duration_ms: f32) -> Transition {
        Transition {
            property: TransitionProperty::Property(property.into()),
            duration_ms,
            delay_ms: 0.0,
            easing: Easing::Linear,
        }
    }

    #[test]
    fn computed_values_cover_shorthands_and_importance() {
        let values = computed(&declarations(&[
            ("background", "#ff0000"),
            ("opacity", "0.5"),
            ("transform", "translateX(-100%)"),
            ("width", "120px"),
        ]));
        assert_eq!(
            values[Animated::BackgroundColor as usize].and_then(MotionValue::rgba),
            Some(0xff00_00ff)
        );
        assert_eq!(
            values[Animated::Opacity as usize],
            Some(MotionValue::Number(0.5))
        );
        assert_eq!(
            values[Animated::Width as usize],
            Some(MotionValue::Pixels(120.0))
        );
        assert_eq!(
            values[Animated::Translate as usize],
            Some(MotionValue::Translate(
                Offset {
                    px: 0.0,
                    fraction: -1.0
                },
                Offset::ZERO
            ))
        );
        assert_eq!(translate("10px"), translate("10px 0"));
    }

    #[test]
    fn transitions_retarget_from_the_value_on_screen() {
        let mut state = MotionState::default();
        let id = ElementId::new("box");
        let transitions = [linear("width", 1000.0)];
        let keyframes = htmlswap::RenderMotionPlan::default();
        let inputs = |width: f32| Inputs {
            targets: {
                let mut targets = [None; 7];
                targets[Animated::Width as usize] = Some(MotionValue::Pixels(width));
                targets
            },
            starting: [None; 7],
            transitions: &transitions,
            animations: &[],
            keyframes: &keyframes,
        };
        let start = Instant::now();
        let width = |frame: super::Frame| frame.get(Animated::Width);

        state.begin();
        assert_eq!(
            width(state.frame(&id, &inputs(100.0), start)),
            Some(MotionValue::Pixels(100.0))
        );
        state.begin();
        let half = state.frame(&id, &inputs(200.0), start);
        assert!(!half.moving || width(half) == Some(MotionValue::Pixels(100.0)));
        state.begin();
        let mid = state.frame(&id, &inputs(200.0), start + Duration::from_millis(500));
        assert_eq!(width(mid), Some(MotionValue::Pixels(150.0)));
        assert!(mid.moving);
        // Reversing half-way starts from 150px, not from 200px.
        state.begin();
        let reversed = state.frame(&id, &inputs(100.0), start + Duration::from_millis(500));
        assert_eq!(width(reversed), Some(MotionValue::Pixels(150.0)));
        state.begin();
        let done = state.frame(&id, &inputs(100.0), start + Duration::from_millis(1600));
        assert_eq!(width(done), Some(MotionValue::Pixels(100.0)));
        assert!(!done.moving);
        state.end();
        state.begin();
        state.end();
        assert!(state.elements.is_empty(), "undrawn elements are forgotten");
    }

    #[test]
    fn colors_fade_through_transparent_without_darkening() {
        let from = MotionValue::color(0xff00_00ff);
        let to = MotionValue::color(0xff00_0000);
        let mid = from.lerp(to, 0.5).and_then(MotionValue::rgba);
        assert_eq!(mid.map(|rgba| rgba >> 8), Some(0x00ff_0000));
    }
}
