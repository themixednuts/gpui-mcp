//! How the pseudo-elements of one transition name animate.
//!
//! A name has three parts, as in CSS: a group, which moves the element from
//! its old box to its new one, and an old and a new image inside it. Their
//! timing uses GPUI Kit's motion types, so `animation` longhands map onto
//! [`Timing`] and `@keyframes` onto [`Keyframes`].

use std::time::Duration;

use gpui_base::animation::Lerp;
use gpui_base::motion::{Easing, Keyframe, Keyframes, MotionPhase, Timing};

/// Duration of the user-agent group animation (`250ms`).
pub const DEFAULT_DURATION: Duration = Duration::from_millis(250);

/// A length along one axis of an image: pixels plus a fraction of the
/// image's own size, as a `translate` percentage resolves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Offset {
    /// Absolute pixels.
    pub px: f32,
    /// A fraction of the image's size on this axis (`50%` is `0.5`).
    pub fraction: f32,
}

impl Offset {
    /// No offset.
    pub const ZERO: Self = Self {
        px: 0.0,
        fraction: 0.0,
    };

    /// The length along an axis of `size` pixels.
    #[must_use]
    pub fn resolve(self, size: f32) -> f32 {
        self.px + self.fraction * size
    }
}

impl Lerp for Offset {
    fn lerp(&self, target: &Self, t: f32) -> Self {
        Self {
            px: self.px.lerp(&target.px, t),
            fraction: self.fraction.lerp(&target.fraction, t),
        }
    }
}

/// A `translate` on both axes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Translate {
    /// Horizontal, positive to the right.
    pub x: Offset,
    /// Vertical, positive downwards.
    pub y: Offset,
}

impl Lerp for Translate {
    fn lerp(&self, target: &Self, t: f32) -> Self {
        Self {
            x: self.x.lerp(&target.x, t),
            y: self.y.lerp(&target.y, t),
        }
    }
}

/// Which sides of the active interval an animation applies outside of
/// (`animation-fill-mode`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Fill {
    /// Apply the first keyframe during the delay.
    pub backwards: bool,
    /// Keep the last keyframe once finished.
    pub forwards: bool,
}

impl Fill {
    /// `none`.
    pub const NONE: Self = Self {
        backwards: false,
        forwards: false,
    };
    /// `both`.
    pub const BOTH: Self = Self {
        backwards: true,
        forwards: true,
    };
}

/// One animation of an old or new image: `@keyframes` for the properties
/// GPUI can animate on an image, played with a timing.
///
/// Keyframes are per property, as CSS keyframes are, and must include
/// their `0%` and `100%` frames; a rule that leaves one out takes the
/// image's underlying value there (opacity `1`, no translation).
#[derive(Clone, Debug)]
pub struct ImageAnimation {
    timing: Timing,
    fill: Fill,
    opacity: Option<Keyframes<f32>>,
    translate: Option<Keyframes<Translate>>,
    /// The user-agent cross-fade, which [`NameStyle`] draws exactly.
    fade: Option<Fade>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fade {
    Out,
    In,
}

impl ImageAnimation {
    /// An animation with `timing` that animates nothing until keyframes are
    /// added. Per-segment easing belongs on the keyframes, as in CSS, so
    /// `timing` is normally linear.
    #[must_use]
    pub fn new(timing: Timing, fill: Fill) -> Self {
        Self {
            timing,
            fill,
            opacity: None,
            translate: None,
            fade: None,
        }
    }

    /// Animate `opacity` through `keyframes`.
    #[must_use]
    pub fn opacity(mut self, keyframes: Keyframes<f32>) -> Self {
        self.opacity = Some(keyframes);
        self
    }

    /// Animate `translate` through `keyframes`.
    #[must_use]
    pub fn translate(mut self, keyframes: Keyframes<Translate>) -> Self {
        self.translate = Some(keyframes);
        self
    }

    /// `-ua-view-transition-fade-out` over `duration` after `delay`.
    #[must_use]
    pub fn fade_out(duration: Duration, delay: Duration, easing: Easing) -> Self {
        Self::fade(Fade::Out, duration, delay, easing)
    }

    /// `-ua-view-transition-fade-in` over `duration` after `delay`.
    #[must_use]
    pub fn fade_in(duration: Duration, delay: Duration, easing: Easing) -> Self {
        Self::fade(Fade::In, duration, delay, easing)
    }

    fn fade(fade: Fade, duration: Duration, delay: Duration, easing: Easing) -> Self {
        let (from, to) = match fade {
            Fade::Out => (1.0, 0.0),
            Fade::In => (0.0, 1.0),
        };
        let keyframes = Keyframes::try_new([
            Keyframe::new(0.0, from).ease(easing),
            Keyframe::new(1.0, to),
        ]);
        let mut animation = Self::new(Timing::new(duration).delay(delay.into()), Fill::BOTH);
        animation.opacity = keyframes.ok();
        animation.fade = Some(fade);
        animation
    }

    /// Whether the animation is still to play or playing at `elapsed`.
    fn running(&self, elapsed: Duration) -> bool {
        !self.timing.sample(elapsed).finished
    }

    /// The animation's progress at `elapsed`, when it applies then.
    fn progress(&self, elapsed: Duration) -> Option<f32> {
        let sample = self.timing.sample(elapsed);
        let applies = match sample.phase {
            MotionPhase::Before => self.fill.backwards,
            MotionPhase::Active => true,
            MotionPhase::After => self.fill.forwards,
        };
        applies.then_some(sample.directed_progress)
    }
}

/// An image's animated values in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageFrame {
    /// The image's opacity, in `0..=1`.
    pub opacity: f32,
    /// The image's translation.
    pub translate: Translate,
    /// Whether any of its animations is still to play or playing.
    pub running: bool,
}

impl Default for ImageFrame {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            translate: Translate::default(),
            running: false,
        }
    }
}

/// The group's progress in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupFrame {
    /// Eased progress from the old box (`0`) to the new box (`1`).
    pub progress: f32,
    /// Whether the group is still to move or moving.
    pub running: bool,
}

/// How one name's group and images animate.
#[derive(Clone, Debug)]
pub struct NameStyle {
    /// The group's timing (with its easing), or `None` for `animation:
    /// none`, which places the group at the new box at once.
    pub group: Option<Timing>,
    /// `::view-transition-old(name)` animations, in `animation-name` order.
    pub old: Vec<ImageAnimation>,
    /// `::view-transition-new(name)` animations, in `animation-name` order.
    pub new: Vec<ImageAnimation>,
}

impl Default for NameStyle {
    /// The user-agent style: the group moves over `250ms ease`, and the
    /// images cross-fade with the same timing.
    fn default() -> Self {
        Self::user_agent(DEFAULT_DURATION, Duration::ZERO, Easing::Ease)
    }
}

impl NameStyle {
    /// The user-agent style with the group timing an author set, which the
    /// images inherit, as the pseudo-elements do in browsers.
    #[must_use]
    pub fn user_agent(duration: Duration, delay: Duration, easing: Easing) -> Self {
        Self {
            group: Some(
                Timing::new(duration)
                    .delay(delay.into())
                    .ease(easing.clone()),
            ),
            old: vec![ImageAnimation::fade_out(duration, delay, easing.clone())],
            new: vec![ImageAnimation::fade_in(duration, delay, easing)],
        }
    }

    /// The group at `elapsed`. The group fills both ways.
    #[must_use]
    pub fn group(&self, elapsed: Duration) -> GroupFrame {
        let Some(timing) = &self.group else {
            return GroupFrame {
                progress: 1.0,
                running: false,
            };
        };
        let sample = timing.sample(elapsed);
        GroupFrame {
            progress: sample.directed_progress,
            running: !sample.finished,
        }
    }

    /// The old image at `elapsed`.
    #[must_use]
    pub fn old(&self, elapsed: Duration) -> ImageFrame {
        sample(&self.old, elapsed)
    }

    /// The new image at `elapsed`, drawn under an old image when
    /// `under_old` (the name existed before).
    ///
    /// When both images only cross-fade, the old one is drawn over the new
    /// one, so the new side stays opaque under it and the blend is exact
    /// without `plus-lighter`.
    #[must_use]
    pub fn new_image(&self, elapsed: Duration, under_old: bool) -> ImageFrame {
        let mut frame = sample(&self.new, elapsed);
        if under_old && self.cross_fade_only() {
            frame.opacity = 1.0;
        }
        frame
    }

    fn cross_fade_only(&self) -> bool {
        let only = |list: &[ImageAnimation], fade| {
            list.iter().all(|animation| animation.fade == Some(fade))
        };
        only(&self.old, Fade::Out) && only(&self.new, Fade::In)
    }
}

/// Later animations replace earlier ones' values for the same property,
/// as CSS composites `replace` animations.
fn sample(list: &[ImageAnimation], elapsed: Duration) -> ImageFrame {
    let mut frame = ImageFrame::default();
    for animation in list {
        frame.running |= animation.running(elapsed);
        let Some(progress) = animation.progress(elapsed) else {
            continue;
        };
        if let Some(opacity) = &animation.opacity {
            frame.opacity = opacity.sample(progress);
        }
        if let Some(translate) = &animation.translate {
            frame.translate = translate.sample(progress);
        }
    }
    frame.opacity = frame.opacity.clamp(0.0, 1.0);
    frame
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui_base::motion::{Easing, Keyframe, Keyframes, Timing};

    use super::{Fill, ImageAnimation, NameStyle, Offset, Translate};

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn close(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 1e-3
    }

    #[test]
    fn the_user_agent_style_cross_fades_over_the_group_duration() {
        let style = NameStyle::default();
        let group = style.group(ms(125));
        assert!(group.running && group.progress > 0.0 && group.progress < 1.0);
        assert!(close(style.old(ms(0)).opacity, 1.0));
        assert!(
            close(style.new_image(ms(0), true).opacity, 1.0),
            "the new side stays opaque under the old one"
        );
        assert!(
            close(style.new_image(ms(0), false).opacity, 0.0),
            "a new name fades in"
        );
        let done = style.old(ms(400));
        assert!(!done.running && close(done.opacity, 0.0));
        assert!(!style.group(ms(400)).running);
    }

    #[test]
    fn author_animations_fill_as_declared_and_replace_earlier_values() {
        let slide = Keyframes::try_new([
            Keyframe::new(0.0, Translate::default()),
            Keyframe::new(
                1.0,
                Translate {
                    x: Offset {
                        px: 0.0,
                        fraction: -1.0,
                    },
                    y: Offset::ZERO,
                },
            ),
        ])
        .ok();
        let Some(slide) = slide else {
            return;
        };
        let timing = Timing::new(ms(100)).delay(ms(100).into());
        let mut style = NameStyle::user_agent(ms(200), Duration::ZERO, Easing::Linear);
        style
            .old
            .push(ImageAnimation::new(timing, Fill::NONE).translate(slide));

        assert!(
            close(style.old(ms(50)).translate.x.fraction, 0.0),
            "no backwards fill"
        );
        assert!(close(style.old(ms(150)).translate.x.fraction, -0.5));
        assert!(
            close(style.old(ms(150)).opacity, 0.25),
            "the fade keeps running"
        );
        assert!(
            close(style.old(ms(250)).translate.x.fraction, 0.0),
            "no forwards fill"
        );
        assert!(
            !close(style.new_image(ms(100), true).opacity, 1.0),
            "an authored side breaks the exact cross-fade"
        );
    }
}
