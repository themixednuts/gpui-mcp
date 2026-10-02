//! CSS transitions and `@keyframes` for the live renderer.
//!
//! On the `gpui-pre` backend (GPUI Kit) they run on GPUI Kit's motion runtime
//! (`gpui_base::motion`), the same runtime generated GPUI Kit code uses, so
//! the Studio canvas and an exported app animate identically. The values
//! come from htmlswap's typed lowering: `AnimatableProperty` says what a rule
//! animates and `AnimatedValue` how values blend. Without the feature (the
//! Zed GPUI backend), properties take their end values at once.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId as GpuiElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Window, point,
};
use htmlswap::computed::{ComputedStyle, LengthPercentage, Underlying};
use htmlswap::{RenderMotionPlan, StyleDeclaration};

use crate::cascade::Computed;

/// What one element animates this frame.
#[cfg_attr(not(feature = "gpui-pre"), allow(dead_code))]
pub(crate) struct Inputs<'a> {
    /// The element's runtime id, which keys its motion state.
    pub(crate) key: &'a str,
    pub(crate) computed: &'a Computed,
    /// The style to animate toward: the computed style with bound values.
    pub(crate) target: &'a ComputedStyle,
    /// Whether this is the element's first frame, when `@starting-style`
    /// applies.
    pub(crate) entering: bool,
    pub(crate) plan: &'a RenderMotionPlan,
    pub(crate) underlying: Underlying,
    /// Computes a keyframe's declarations in the element's scope.
    pub(crate) compute_keyframe: &'a dyn Fn(&[StyleDeclaration]) -> ComputedStyle,
}

/// What an element's motion produced.
#[derive(Debug, Default)]
pub(crate) struct Output {
    /// Values to draw over the element's computed style.
    pub(crate) style: ComputedStyle,
    /// Whether anything still moves, so another frame is needed.
    pub(crate) moving: bool,
}

/// Whether an element's styles involve motion at all.
pub(crate) fn has_motion(computed: &Computed) -> bool {
    computed
        .transitions
        .iter()
        .any(htmlswap::motion::Transition::is_active)
        || computed
            .animations
            .iter()
            .any(|animation| animation.name.is_some())
}

#[cfg(feature = "gpui-pre")]
pub(crate) use kit::{animate, duration, easing, frames, keyframes, timing};

/// Without GPUI Kit's motion runtime, values take their end state at once.
#[cfg(not(feature = "gpui-pre"))]
pub(crate) fn animate(_: &Inputs<'_>, _: &mut Window, _: &mut App) -> Output {
    Output::default()
}

#[cfg(feature = "gpui-pre")]
mod kit {
    use std::time::Duration;

    use gpui::{App, SharedString, Window};
    use gpui_base::animation::Lerp;
    use gpui_base::motion::{
        Easing as KitEasing, Interpolate, IterationCount, Keyframe, Keyframes, MotionStatus,
        PlaybackDirection, SignedDuration, StepPosition as KitStep, Timing,
        Transition as KitTransition, animate_keyframes, transition_with_status,
    };
    use htmlswap::computed::{AnimatableProperty, AnimatedValue, ComputedStyle};
    use htmlswap::motion::{
        Animation, AnimationDirection, Easing, FillMode, Iterations, StepPosition, Transition,
    };
    use htmlswap::{RenderKeyframes, StyleDeclaration};

    use super::{Inputs, Output};

    /// An animated value for GPUI Kit's runtime, blended as CSS specifies.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Value(AnimatedValue);

    impl Lerp for Value {
        fn lerp(&self, target: &Self, t: f32) -> Self {
            Self(self.0.mix(target.0, t))
        }
    }

    pub(crate) fn easing(easing: Easing) -> KitEasing {
        match easing {
            Easing::Linear => KitEasing::Linear,
            Easing::CubicBezier(x1, y1, x2, y2) => {
                KitEasing::cubic_bezier(x1, y1, x2, y2).unwrap_or(KitEasing::Linear)
            }
            Easing::Steps(count, position) => KitEasing::steps(
                count,
                match position {
                    StepPosition::JumpStart => KitStep::JumpStart,
                    StepPosition::JumpEnd => KitStep::JumpEnd,
                    StepPosition::JumpNone => KitStep::JumpNone,
                    StepPosition::JumpBoth => KitStep::JumpBoth,
                },
            )
            .unwrap_or(KitEasing::Linear),
        }
    }

    pub(crate) fn duration(ms: f32) -> Duration {
        Duration::from_secs_f32(ms.max(0.0) / 1000.0)
    }

    fn delay(ms: f32) -> SignedDuration {
        if ms < 0.0 {
            SignedDuration::negative(duration(-ms))
        } else {
            SignedDuration::positive(duration(ms))
        }
    }

    fn policy(transition: &Transition) -> KitTransition {
        KitTransition::new(duration(transition.duration_ms))
            .delay(delay(transition.delay_ms))
            .easing(easing(transition.easing))
    }

    fn channel(inputs: &Inputs<'_>, suffix: &str) -> (SharedString, SharedString) {
        (
            SharedString::from(format!("html-motion:{}", inputs.key)),
            SharedString::from(suffix.to_owned()),
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn animate(inputs: &Inputs<'_>, window: &mut Window, cx: &mut App) -> Output {
        let mut output = Output::default();
        let computed = inputs.computed;
        // Entry transitions start from @starting-style (CSS Transitions 2
        // §3.1): the first frame creates each value there and retargets it
        // to the element's own value at once, so the transition starts on
        // the frame the element first gets a style.
        let starting = computed.starting.as_ref().filter(|_| inputs.entering);
        for property in AnimatableProperty::ALL {
            let Some(transition) =
                Transition::for_property(&computed.transitions, property.css_name())
                    .filter(|transition| transition.is_active())
            else {
                continue;
            };
            let explicit = property.get(inputs.target);
            let Some(target) = explicit.or_else(|| property.initial(&inputs.underlying)) else {
                continue;
            };
            if let Some(from) = starting.and_then(|starting| property.get(starting)) {
                transition_with_status(
                    channel(inputs, property.css_name()),
                    Value(from),
                    policy(transition),
                    window,
                    cx,
                );
            }
            let sample = transition_with_status(
                channel(inputs, property.css_name()),
                Value(target),
                policy(transition),
                window,
                cx,
            );
            let moving = matches!(sample.status, MotionStatus::Running | MotionStatus::Delayed);
            if explicit.is_some() || moving {
                property.set(&mut output.style, sample.value.0);
            }
            output.moving |= moving;
        }
        for (index, animation) in computed.animations.iter().enumerate() {
            let Some(name) = animation.name.as_deref() else {
                continue;
            };
            let Some(source) = inputs.plan.keyframes(name) else {
                continue;
            };
            let frames = frames(source, animation.easing, inputs.compute_keyframe);
            let timing = timing(animation);
            for property in AnimatableProperty::ALL {
                if frames
                    .iter()
                    .all(|(_, style, _)| property.get(style).is_none())
                {
                    continue;
                }
                let underlying = property
                    .get(&output.style)
                    .or_else(|| property.get(inputs.target))
                    .or_else(|| property.initial(&inputs.underlying));
                let Some(keyframes) =
                    keyframes(&frames, property, underlying, |value| Some(Value(value)))
                else {
                    continue;
                };
                let sample = animate_keyframes(
                    channel(inputs, &format!("{index}:{name}:{}", property.css_name())),
                    &keyframes,
                    timing.clone(),
                    window,
                    cx,
                );
                let applies = match sample.status {
                    MotionStatus::Running => true,
                    MotionStatus::Delayed | MotionStatus::Idle => {
                        matches!(animation.fill_mode, FillMode::Backwards | FillMode::Both)
                    }
                    MotionStatus::Finished => {
                        matches!(animation.fill_mode, FillMode::Forwards | FillMode::Both)
                    }
                };
                if applies {
                    property.set(&mut output.style, sample.value.0);
                }
                output.moving |=
                    matches!(sample.status, MotionStatus::Running | MotionStatus::Delayed);
            }
        }
        output
    }

    /// A `@keyframes` rule's frames as typed styles, with each frame's
    /// easing (its `animation-timing-function`, else the animation's).
    pub(crate) fn frames(
        source: &RenderKeyframes,
        easing: Easing,
        compute: &dyn Fn(&[StyleDeclaration]) -> ComputedStyle,
    ) -> Vec<(f32, ComputedStyle, Easing)> {
        source
            .frames
            .iter()
            .map(|frame| {
                let easing = frame
                    .declarations
                    .iter()
                    .rev()
                    .find(|d| d.property.as_str() == "animation-timing-function")
                    .and_then(|d| Easing::parse(d.value.as_str()))
                    .unwrap_or(easing);
                (frame.offset, compute(&frame.declarations), easing)
            })
            .collect()
    }

    /// An animation's timing. Easing is per keyframe, as in CSS, so the
    /// timing itself is linear.
    pub(crate) fn timing(animation: &Animation) -> Timing {
        Timing::new(duration(animation.duration_ms))
            .delay(delay(animation.delay_ms))
            .iterations(match animation.iterations {
                Iterations::Infinite => IterationCount::Infinite,
                // GPUI Kit plays whole iterations; a fractional count
                // finishes its last iteration.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Iterations::Count(count) => IterationCount::Finite(count.max(0.0).ceil() as u64),
            })
            .direction(match animation.direction {
                AnimationDirection::Normal => PlaybackDirection::Normal,
                AnimationDirection::Reverse => PlaybackDirection::Reverse,
                AnimationDirection::Alternate => PlaybackDirection::Alternate,
                AnimationDirection::AlternateReverse => PlaybackDirection::AlternateReverse,
            })
    }

    /// One property's keyframes, with missing `0%` and `100%` keyframes
    /// taking the underlying value (CSS Animations 1 §3), each value
    /// converted by `value`.
    pub(crate) fn keyframes<T: Interpolate>(
        frames: &[(f32, ComputedStyle, Easing)],
        property: AnimatableProperty,
        underlying: Option<AnimatedValue>,
        value: impl Fn(AnimatedValue) -> Option<T>,
    ) -> Option<Keyframes<T>> {
        let mut list = frames
            .iter()
            .filter_map(|(offset, style, ease)| {
                Some(Keyframe::new(*offset, value(property.get(style)?)?).ease(easing(*ease)))
            })
            .collect::<Vec<_>>();
        if list.first().is_none_or(|frame| frame.offset > 0.0) {
            let ease = frames.first().map_or(Easing::Linear, |frame| frame.2);
            list.insert(
                0,
                Keyframe::new(0.0, value(underlying?)?).ease(easing(ease)),
            );
        }
        if list.last().is_none_or(|frame| frame.offset < 1.0) {
            list.push(Keyframe::new(1.0, value(underlying?)?));
        }
        Keyframes::try_new(list).ok()
    }
}

/// Paints its child moved by a `translate`, resolved against the child's own
/// laid-out size. The move happens in prepaint, so layout, siblings and
/// scroll extents are unaffected, while hit testing and semantic bounds
/// follow the painted position.
pub(crate) struct Translated {
    child: AnyElement,
    x: LengthPercentage,
    y: LengthPercentage,
}

impl Translated {
    pub(crate) fn new(child: AnyElement, x: LengthPercentage, y: LengthPercentage) -> Self {
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
            gpui::px(self.x.resolve(f32::from(bounds.size.width))),
            gpui::px(self.y.resolve(f32::from(bounds.size.height))),
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

/// A translation that moves nothing.
pub(crate) fn is_zero(translate: (LengthPercentage, LengthPercentage)) -> bool {
    translate.0 == LengthPercentage::ZERO && translate.1 == LengthPercentage::ZERO
}
