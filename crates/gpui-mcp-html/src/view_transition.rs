//! CSS view transitions for the live renderer.
//!
//! The transition itself (capture, timing, the pseudo-element tree and its
//! drawing) is `gpui-view-transitions`, the crate generated GPUI Kit code
//! uses too. This module holds what is specific to HTML documents: which
//! elements are named, how author CSS styles each name's pseudo-elements,
//! and the outgoing document the renderer draws again inertly.
//!
//! On the Zed backend, which GPUI Kit does not support, transitions apply
//! their end state at once, as transitions and animations do there.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::SharedString;
use htmlswap::StyleDeclaration;

use crate::{Binding, ElementId, HtmlUi, StateValue, UiProperty};

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
) -> Option<SharedString> {
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
        "auto" | "match-element" => Some(element_id.to_owned().into()),
        name => Some(name.to_owned().into()),
    }
}

/// `view-transition-class` names.
pub(crate) fn transition_classes<'a>(
    declarations: impl IntoIterator<Item = &'a StyleDeclaration>,
) -> Vec<SharedString> {
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
                .map(|class| SharedString::from(class.to_owned()))
                .collect()
        })
        .unwrap_or_default()
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

/// The outgoing document of a transition, which the renderer draws again.
pub(crate) struct OldState {
    pub(crate) ui: Rc<HtmlUi>,
    pub(crate) bindings: HashMap<ElementId, Rc<[Binding]>>,
    /// Bound property values at capture time, or `None` to read the hooks
    /// live (a document swap leaves application state unchanged).
    pub(crate) properties: Option<PropertySnapshot>,
    pub(crate) disclosures: HashMap<ElementId, bool>,
}

#[cfg(feature = "gpui-pre")]
pub(crate) use kit::DocumentTransitions;
#[cfg(not(feature = "gpui-pre"))]
pub(crate) use off::DocumentTransitions;

/// Without GPUI Kit's motion runtime, a transition applies its end state at
/// once: nothing is captured, and the incoming state draws as is.
#[cfg(not(feature = "gpui-pre"))]
mod off {
    use std::rc::Rc;
    use std::time::Instant;

    use gpui::{AnyElement, IntoElement, SharedString, Styled, Window};
    use htmlswap::computed::MediaEnvironment;

    use super::OldState;
    use crate::HtmlUi;

    #[derive(Clone, Default)]
    pub(crate) struct DocumentTransitions(());

    // The same methods as the GPUI Kit version, so the renderer has one path.
    #[allow(clippy::unused_self)]
    impl DocumentTransitions {
        pub(crate) fn set_document(&self, _: Rc<HtmlUi>) {}

        pub(crate) fn begin_frame(&self, _: Instant, _: MediaEnvironment) {}

        pub(crate) fn end_frame(&self, _: &mut Window) {}

        pub(crate) fn start(&self, _: Vec<SharedString>, _: OldState) -> bool {
            false
        }

        pub(crate) fn skip(&self) {}

        pub(crate) fn is_active(&self) -> bool {
            false
        }

        pub(crate) fn is_pending(&self) -> bool {
            false
        }

        pub(crate) fn types(&self) -> Option<Vec<SharedString>> {
            None
        }

        pub(crate) fn add_types(&self, _: impl IntoIterator<Item = SharedString>) {}

        pub(crate) fn named<E: IntoElement + Styled>(
            &self,
            _: SharedString,
            _: &[SharedString],
            _: Vec<usize>,
            element: E,
        ) -> AnyElement {
            element.into_any_element()
        }

        pub(crate) fn stage<E: IntoElement + Styled>(
            &self,
            root: E,
            _: impl FnMut(&OldState, Option<&[usize]>, &dyn Fn(&[usize]) -> bool) -> Option<AnyElement>,
        ) -> AnyElement {
            root.into_any_element()
        }
    }
}

#[cfg(feature = "gpui-pre")]
mod kit {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Instant;

    use gpui::{AnyElement, IntoElement, SharedString, Styled, Window};
    use gpui_view_transitions::{
        Fill, ImageAnimation, NameStyle, Offset, OldPart, Translate, ViewTransitions,
    };
    use htmlswap::computed::{
        AnimatableProperty, AnimatedValue, ComputedScope, LengthPercentage, MediaEnvironment,
    };
    use htmlswap::motion::{Animation, FillMode, animations};
    use htmlswap::{
        CompactString, RenderMotionPlan, RenderStyleCondition, StyleDeclaration, ViewTransitionPart,
    };

    use super::OldState;
    use crate::HtmlUi;
    use crate::cascade::{self, Environment};
    use crate::motion::{duration, easing, frames, keyframes, timing};

    const GROUP: &str = "-ua-view-transition-group";
    const FADE_OUT: &str = "-ua-view-transition-fade-out";
    const FADE_IN: &str = "-ua-view-transition-fade-in";

    /// A document's view transitions, with named elements keyed by their
    /// child-index paths.
    #[derive(Clone, Default)]
    pub(crate) struct DocumentTransitions {
        inner: ViewTransitions<OldState, Vec<usize>>,
        /// The incoming document, whose CSS styles the pseudo-elements.
        incoming: Rc<RefCell<Option<Rc<HtmlUi>>>>,
        /// The media the current frame renders for.
        media: Rc<Cell<MediaEnvironment>>,
    }

    impl DocumentTransitions {
        /// Use `ui` as the incoming document.
        pub(crate) fn set_document(&self, ui: Rc<HtmlUi>) {
            *self.incoming.borrow_mut() = Some(ui);
        }

        pub(crate) fn begin_frame(&self, now: Instant, media: MediaEnvironment) {
            self.media.set(media);
            self.inner.begin_frame(now);
        }

        pub(crate) fn end_frame(&self, window: &mut Window) {
            self.inner.end_frame(window);
        }

        /// Start a transition from the document as last drawn.
        pub(crate) fn start(&self, types: Vec<SharedString>, old: OldState) -> bool {
            let incoming = self.incoming.clone();
            let media = self.media.clone();
            self.inner.start(
                types,
                old,
                Box::new(move |name, classes, types| {
                    let Some(ui) = incoming.borrow().clone() else {
                        return NameStyle::default();
                    };
                    let environment = Environment {
                        media: media.get(),
                        view_transition_types: Some(types),
                    };
                    let holds = |condition: &RenderStyleCondition| {
                        cascade::condition_holds(condition, &environment)
                    };
                    name_style(name, classes, types, &ui.plan().motion, &holds)
                }),
            )
        }

        pub(crate) fn skip(&self) {
            self.inner.skip();
        }

        pub(crate) fn is_active(&self) -> bool {
            self.inner.is_active()
        }

        pub(crate) fn is_pending(&self) -> bool {
            self.inner.is_pending()
        }

        pub(crate) fn types(&self) -> Option<Vec<SharedString>> {
            self.inner.types()
        }

        pub(crate) fn add_types(&self, types: impl IntoIterator<Item = SharedString>) {
            self.inner.add_types(types);
        }

        pub(crate) fn named<E: IntoElement + Styled>(
            &self,
            name: SharedString,
            classes: &[SharedString],
            path: Vec<usize>,
            element: E,
        ) -> AnyElement {
            self.inner.named(name, classes, path, element)
        }

        /// The document root, with the outgoing document over it while a
        /// transition runs. `draw_old` draws the old document's root (path
        /// `None`), leaving out the elements `is_named` accepts, or the old
        /// element at a path.
        pub(crate) fn stage<E: IntoElement + Styled>(
            &self,
            root: E,
            mut draw_old: impl FnMut(
                &OldState,
                Option<&[usize]>,
                &dyn Fn(&[usize]) -> bool,
            ) -> Option<AnyElement>,
        ) -> AnyElement {
            self.inner.stage(root, |old, part| match part {
                OldPart::Root { named } => draw_old(old, None, &|path| {
                    named.iter().any(|captured| captured.key == path)
                }),
                OldPart::Named(captured) => draw_old(old, Some(&captured.key), &|_| false),
            })
        }
    }

    /// How author CSS styles one name's pseudo-elements: the user-agent
    /// style, then `::view-transition-group/old/new` rules for the name, its
    /// classes and the transition's types.
    pub(crate) fn name_style(
        name: &str,
        classes: &[SharedString],
        types: &[SharedString],
        motion: &RenderMotionPlan,
        media: &dyn Fn(&RenderStyleCondition) -> bool,
    ) -> NameStyle {
        let compact = |values: &[SharedString]| {
            values
                .iter()
                .map(|value| CompactString::from(value.as_ref()))
                .collect::<Vec<_>>()
        };
        let (classes, types) = (compact(classes), compact(types));
        let declarations = |part| {
            motion
                .view_transition
                .declarations_for(part, name, &classes, &types, media)
        };
        let group_ua = user_agent(GROUP);
        let group =
            animations(std::iter::once(&group_ua).chain(declarations(ViewTransitionPart::Group)))
                .into_iter()
                .next()
                .filter(|animation| animation.name.is_some());
        let mut style = match &group {
            Some(group) => {
                let mut style = NameStyle::user_agent(
                    duration(group.duration_ms),
                    duration(group.delay_ms),
                    easing(group.easing),
                );
                style.group = Some(timing(group).ease(easing(group.easing)));
                style
            }
            None => NameStyle {
                group: None,
                ..NameStyle::default()
            },
        };
        // The image pair inherits the group's timing, as the pseudo-elements
        // do in browsers; author rules then replace the default fades.
        let inherited = group.as_ref().map(inherited_timing).unwrap_or_default();
        for (part, ua_name, list) in [
            (ViewTransitionPart::Old, FADE_OUT, &mut style.old),
            (ViewTransitionPart::New, FADE_IN, &mut style.new),
        ] {
            let default = user_agent(ua_name);
            let authored = animations(
                std::iter::once(&default)
                    .chain(&inherited)
                    .chain(declarations(part)),
            );
            if authored
                .iter()
                .all(|animation| animation.name.as_deref() == Some(ua_name))
            {
                continue;
            }
            list.clear();
            for animation in &authored {
                match animation.name.as_deref() {
                    None => {}
                    Some(name) if name == ua_name => list.push(fade(animation, name == FADE_OUT)),
                    Some(name) => list.extend(image_animation(animation, name, motion)),
                }
            }
        }
        style
    }

    /// A user-agent animation declaration, at the default group timing.
    fn user_agent(name: &str) -> StyleDeclaration {
        StyleDeclaration::new(
            "animation",
            format!(
                "{}ms ease both {name}",
                gpui_view_transitions::DEFAULT_DURATION.as_millis()
            ),
            false,
            None,
        )
    }

    /// A user-agent fade, at the timing the cascade gave it.
    fn fade(animation: &Animation, out: bool) -> ImageAnimation {
        let (length, delay, ease) = (
            duration(animation.duration_ms),
            duration(animation.delay_ms),
            easing(animation.easing),
        );
        if out {
            ImageAnimation::fade_out(length, delay, ease)
        } else {
            ImageAnimation::fade_in(length, delay, ease)
        }
    }

    /// An author `@keyframes` animation of an image, over the properties an
    /// image animates: `opacity` and `translate`.
    fn image_animation(
        animation: &Animation,
        name: &str,
        motion: &RenderMotionPlan,
    ) -> Option<ImageAnimation> {
        let source = motion.keyframes(name)?;
        let scope = ComputedScope::default();
        let frames = frames(source, animation.easing, &|declarations| {
            let declarations = declarations.iter().collect::<Vec<_>>();
            cascade::typed(&scope, &scope, &declarations)
        });
        let fill = Fill {
            backwards: matches!(animation.fill_mode, FillMode::Backwards | FillMode::Both),
            forwards: matches!(animation.fill_mode, FillMode::Forwards | FillMode::Both),
        };
        let mut image = ImageAnimation::new(timing(animation), fill);
        let sets = |property: AnimatableProperty| {
            frames
                .iter()
                .any(|(_, style, _)| property.get(style).is_some())
        };
        if sets(AnimatableProperty::Opacity)
            && let Some(opacity) = keyframes(
                &frames,
                AnimatableProperty::Opacity,
                Some(AnimatedValue::Number(1.0)),
                AnimatedValue::number,
            )
        {
            image = image.opacity(opacity);
        }
        if sets(AnimatableProperty::Translate)
            && let Some(translate) = keyframes(
                &frames,
                AnimatableProperty::Translate,
                Some(AnimatedValue::Translate(
                    LengthPercentage::ZERO,
                    LengthPercentage::ZERO,
                )),
                |value| {
                    let (x, y) = value.translate()?;
                    Some(Translate {
                        x: offset(x),
                        y: offset(y),
                    })
                },
            )
        {
            image = image.translate(translate);
        }
        Some(image)
    }

    const fn offset(length: LengthPercentage) -> Offset {
        Offset {
            px: length.px,
            fraction: length.fraction,
        }
    }

    /// Longhands that hand the group's timing down to its image pair.
    fn inherited_timing(group: &Animation) -> Vec<StyleDeclaration> {
        [
            ("animation-duration", format!("{}ms", group.duration_ms)),
            ("animation-delay", format!("{}ms", group.delay_ms)),
            ("animation-timing-function", group.easing.to_string()),
        ]
        .into_iter()
        .map(|(property, value)| StyleDeclaration::new(property, value, false, None))
        .collect()
    }

    #[cfg(test)]
    mod tests {
        use std::time::Duration;

        use htmlswap::{RenderMotionPlan, RenderStyleCondition};

        use super::name_style;

        #[test]
        fn without_author_rules_names_get_the_user_agent_cross_fade() {
            let motion = RenderMotionPlan::default();
            let no_media = |_: &RenderStyleCondition| false;
            let style = name_style("root", &[], &[], &motion, &no_media);
            let ms = Duration::from_millis;
            let group = style.group(ms(125));
            assert!(group.running && group.progress > 0.0 && group.progress < 1.0);
            assert!((style.old(ms(0)).opacity - 1.0).abs() < 1e-3);
            assert!(style.new_image(ms(0), false).opacity.abs() < 1e-3);
            let done = style.new_image(ms(400), false);
            assert!(!done.running && (done.opacity - 1.0).abs() < 1e-3);
            assert!(!style.group(ms(400)).running);
        }
    }
}

#[cfg(test)]
mod tests {
    use htmlswap::StyleDeclaration;

    use super::{transition_classes, transition_name};

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
}
