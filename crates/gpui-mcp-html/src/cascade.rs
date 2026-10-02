//! Which of an element's lowered declarations apply in its current state,
//! and what they compute to.
//!
//! htmlswap resolves selectors and the cascade at compile time; what remains
//! is conditional on runtime state: media features, an active view
//! transition, and interaction states on the element or an ancestor. This
//! module evaluates those conditions and runs htmlswap's typed lowering
//! (`htmlswap::computed`) over the result, so the renderer only maps typed
//! values onto GPUI.

use std::borrow::Cow;

use htmlswap::computed::{ComputedScope, ComputedStyle, MediaEnvironment, Unsupported};
use htmlswap::motion::{Animation, Transition, animations, transitions};
use htmlswap::{
    CompactString, RenderElement, RenderStyleCondition, RenderStyleVariant, StyleDeclaration,
};

/// Interaction states of one element.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct Interaction {
    pub(crate) hovered: bool,
    pub(crate) focused: bool,
    pub(crate) active: bool,
}

impl Interaction {
    pub(crate) const fn bits(self) -> u8 {
        self.hovered as u8 | (self.focused as u8) << 1 | (self.active as u8) << 2
    }

    /// Whether a state pseudo-class holds, or `None` for one the renderer
    /// does not track.
    fn holds(self, pseudo: &str) -> Option<bool> {
        Some(match pseudo {
            "hover" => self.hovered,
            "active" => self.active,
            // GPUI has one focus state, which also stands for keyboard focus.
            "focus" | "focus-visible" => self.focused,
            _ => return None,
        })
    }
}

/// Interaction states an element's styles, or its descendants' styles,
/// depend on, so the renderer tracks them and redraws when they change.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct StateNeeds {
    pub(crate) hover: bool,
    pub(crate) focus: bool,
    pub(crate) active: bool,
}

impl StateNeeds {
    pub(crate) fn add(&mut self, pseudo: &str) {
        match pseudo {
            "hover" => self.hover = true,
            "focus" | "focus-visible" => self.focus = true,
            "active" => self.active = true,
            _ => {}
        }
    }
}

/// What conditions are evaluated against while rendering.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Environment<'a> {
    pub(crate) media: MediaEnvironment,
    /// Types of the running view transition, if one is running.
    pub(crate) view_transition_types: Option<&'a [gpui::SharedString]>,
}

impl Environment<'_> {
    /// A key identifying everything conditions depend on, for caches.
    pub(crate) fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.media.width.to_bits().hash(&mut hasher);
        self.media.height.to_bits().hash(&mut hasher);
        self.media.font_size.to_bits().hash(&mut hasher);
        (self.media.color_scheme == htmlswap::computed::ColorScheme::Dark).hash(&mut hasher);
        self.media.reduced_motion.hash(&mut hasher);
        self.view_transition_types.hash(&mut hasher);
        hasher.finish()
    }
}

/// Whether a running transition has any of `types`.
fn active_type(active: &[gpui::SharedString], types: &[CompactString]) -> bool {
    types
        .iter()
        .any(|kind| active.iter().any(|active| active.as_ref() == kind.as_str()))
}

/// Whether a variant applies now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Applies {
    Yes,
    No,
    /// It depends on something the renderer cannot evaluate.
    Unsupported,
}

/// Evaluate a variant. `ancestors` lists the ancestors' interaction states,
/// nearest last. `starting` selects `@starting-style` variants instead of
/// excluding them.
pub(crate) fn applies(
    variant: &RenderStyleVariant,
    environment: &Environment<'_>,
    own: Interaction,
    ancestors: &[Interaction],
    starting: bool,
) -> Applies {
    let mut starting_seen = false;
    for condition in &variant.conditions {
        let holds = match condition {
            RenderStyleCondition::Media(query) => match environment.media.matches(query) {
                Some(matches) => matches,
                None => return Applies::Unsupported,
            },
            RenderStyleCondition::PseudoClass(pseudo) => match own.holds(pseudo) {
                Some(holds) => holds,
                None => return Applies::Unsupported,
            },
            RenderStyleCondition::ElementState {
                pseudo,
                ancestor,
                negated,
            } => {
                let element = match usize::from(*ancestor) {
                    0 => Some(own),
                    up => ancestors
                        .len()
                        .checked_sub(up)
                        .map(|index| ancestors[index]),
                };
                match element.map(|element| element.holds(pseudo)) {
                    Some(Some(holds)) => holds != *negated,
                    // An ancestor above the rendered document never matches.
                    None => false,
                    Some(None) => return Applies::Unsupported,
                }
            }
            RenderStyleCondition::ActiveViewTransitionType(types) => environment
                .view_transition_types
                .is_some_and(|active| active_type(active, types)),
            RenderStyleCondition::StartingStyle => {
                starting_seen = true;
                true
            }
            RenderStyleCondition::PseudoElement(_)
            | RenderStyleCondition::Supports(_)
            | RenderStyleCondition::Container(_) => return Applies::Unsupported,
        };
        if !holds {
            return Applies::No;
        }
    }
    if starting_seen && !starting {
        Applies::No
    } else {
        Applies::Yes
    }
}

/// Whether the renderer can evaluate every condition of a variant.
pub(crate) fn supported(variant: &RenderStyleVariant) -> bool {
    variant.conditions.iter().all(|condition| match condition {
        RenderStyleCondition::Media(query) => MediaEnvironment::default().matches(query).is_some(),
        RenderStyleCondition::PseudoClass(pseudo)
        | RenderStyleCondition::ElementState { pseudo, .. } => {
            Interaction::default().holds(pseudo).is_some()
        }
        RenderStyleCondition::ActiveViewTransitionType(_) | RenderStyleCondition::StartingStyle => {
            true
        }
        RenderStyleCondition::PseudoElement(_)
        | RenderStyleCondition::Supports(_)
        | RenderStyleCondition::Container(_) => false,
    })
}

/// Whether a condition holds in an environment, for rules outside the
/// element tree (view-transition pseudo-elements). Interaction states do
/// not apply there.
#[cfg(feature = "gpui-pre")]
pub(crate) fn condition_holds(
    condition: &RenderStyleCondition,
    environment: &Environment<'_>,
) -> bool {
    match condition {
        RenderStyleCondition::Media(query) => environment.media.matches(query).unwrap_or(false),
        RenderStyleCondition::ActiveViewTransitionType(types) => environment
            .view_transition_types
            .is_some_and(|active| active_type(active, types)),
        _ => false,
    }
}

/// The element's declarations that apply now, in cascade order.
pub(crate) fn declarations<'a>(
    element: &'a RenderElement,
    environment: &Environment<'_>,
    own: Interaction,
    ancestors: &[Interaction],
) -> Vec<&'a StyleDeclaration> {
    collect(element, environment, own, ancestors, false)
}

/// The document root's declarations that apply now.
pub(crate) fn root_declarations<'a>(
    root: &'a htmlswap::RenderRoot,
    environment: &Environment<'_>,
) -> Vec<&'a StyleDeclaration> {
    let mut declarations: Vec<&StyleDeclaration> = root.styles.iter().collect();
    for variant in &root.style_variants {
        if applies(variant, environment, Interaction::default(), &[], false) == Applies::Yes {
            declarations.extend(&variant.declarations);
        }
    }
    declarations
}

/// The element's declarations including `@starting-style`, for the values an
/// entry transition starts from.
pub(crate) fn starting_declarations<'a>(
    element: &'a RenderElement,
    environment: &Environment<'_>,
    own: Interaction,
    ancestors: &[Interaction],
) -> Option<Vec<&'a StyleDeclaration>> {
    let has_starting = element.style_variants.iter().any(|variant| {
        variant
            .conditions
            .contains(&RenderStyleCondition::StartingStyle)
    });
    has_starting.then(|| collect(element, environment, own, ancestors, true))
}

fn collect<'a>(
    element: &'a RenderElement,
    environment: &Environment<'_>,
    own: Interaction,
    ancestors: &[Interaction],
    starting: bool,
) -> Vec<&'a StyleDeclaration> {
    let mut declarations: Vec<&StyleDeclaration> = element
        .stylesheet_declarations
        .iter()
        .chain(&element.styles)
        .collect();
    for variant in &element.style_variants {
        if applies(variant, environment, own, ancestors, starting) == Applies::Yes {
            declarations.extend(&variant.declarations);
        }
    }
    declarations
}

/// Whether an element's styles depend on an ancestor's interaction state.
pub(crate) fn depends_on_ancestors(element: &RenderElement) -> bool {
    element.style_variants.iter().any(|variant| {
        variant.conditions.iter().any(|condition| {
            matches!(condition, RenderStyleCondition::ElementState { ancestor, .. } if *ancestor > 0)
        })
    })
}

/// The interaction states an element's own styles depend on.
pub(crate) fn own_state_needs(element: &RenderElement) -> StateNeeds {
    let mut needs = StateNeeds::default();
    for variant in &element.style_variants {
        for condition in &variant.conditions {
            match condition {
                RenderStyleCondition::PseudoClass(pseudo)
                | RenderStyleCondition::ElementState {
                    pseudo,
                    ancestor: 0,
                    ..
                } => needs.add(pseudo),
                _ => {}
            }
        }
    }
    needs
}

/// What one element computes to in its context.
#[derive(Clone, Debug)]
pub(crate) struct Computed {
    /// The element's scope, which its children inherit.
    pub(crate) scope: ComputedScope,
    pub(crate) style: ComputedStyle,
    pub(crate) transitions: Vec<Transition>,
    pub(crate) animations: Vec<Animation>,
    /// The typed style with `@starting-style` applied, when the element has
    /// one. Only GPUI Kit's motion runtime animates from it.
    #[cfg_attr(not(feature = "gpui-pre"), allow(dead_code))]
    pub(crate) starting: Option<ComputedStyle>,
}

/// Compute an element's scope and typed style from its applicable
/// declarations.
pub(crate) fn compute(
    parent: &ComputedScope,
    declarations: &[&StyleDeclaration],
    starting: Option<&[&StyleDeclaration]>,
) -> Computed {
    let scope = parent.child(declarations.iter().copied());
    let style = typed(&scope, parent, declarations);
    let resolved = resolve(&scope, declarations);
    let transitions = transitions(resolved.iter().map(Cow::as_ref));
    let animations = animations(resolved.iter().map(Cow::as_ref));
    let starting = starting.map(|declarations| {
        let scope = parent.child(declarations.iter().copied());
        typed(&scope, parent, declarations)
    });
    Computed {
        scope,
        style,
        transitions,
        animations,
        starting,
    }
}

/// Typed styles for declarations in a scope, ignoring what does not compute.
pub(crate) fn typed(
    scope: &ComputedScope,
    parent: &ComputedScope,
    declarations: &[&StyleDeclaration],
) -> ComputedStyle {
    let resolved = resolve(scope, declarations);
    ComputedStyle::compute(
        resolved.iter().map(Cow::as_ref),
        &scope.style_context(parent),
        |_, _: Unsupported| {},
    )
}

/// Substitute `var()`, `light-dark()` and `currentColor`. A declaration
/// invalid at computed-value time is dropped.
fn resolve<'d>(
    scope: &ComputedScope,
    declarations: &[&'d StyleDeclaration],
) -> Vec<Cow<'d, StyleDeclaration>> {
    declarations
        .iter()
        .filter_map(|declaration| scope.resolve(declaration))
        .collect()
}

#[cfg(test)]
mod tests {
    use htmlswap::computed::MediaEnvironment;
    use htmlswap::{RenderStyleCondition, RenderStyleVariant};

    use super::{Applies, Environment, Interaction, applies};

    fn variant(conditions: Vec<RenderStyleCondition>) -> RenderStyleVariant {
        RenderStyleVariant {
            conditions,
            selector: "x".into(),
            declarations: Vec::new(),
            span: None,
        }
    }

    #[test]
    fn ancestor_and_negated_states_evaluate_against_the_right_element() {
        let environment = Environment {
            media: MediaEnvironment::default(),
            view_transition_types: None,
        };
        let hovered = Interaction {
            hovered: true,
            ..Interaction::default()
        };
        let idle = Interaction::default();
        let card_hover = variant(vec![RenderStyleCondition::ElementState {
            pseudo: "hover".into(),
            ancestor: 1,
            negated: false,
        }]);
        assert_eq!(
            applies(&card_hover, &environment, idle, &[hovered], false),
            Applies::Yes
        );
        assert_eq!(
            applies(&card_hover, &environment, hovered, &[idle], false),
            Applies::No
        );
        assert_eq!(
            applies(&card_hover, &environment, idle, &[], false),
            Applies::No
        );
        let not_hovered = variant(vec![RenderStyleCondition::ElementState {
            pseudo: "hover".into(),
            ancestor: 0,
            negated: true,
        }]);
        assert_eq!(
            applies(&not_hovered, &environment, idle, &[], false),
            Applies::Yes
        );
        assert_eq!(
            applies(&not_hovered, &environment, hovered, &[], false),
            Applies::No
        );
        let narrow = variant(vec![RenderStyleCondition::Media(
            "(max-width: 600px)".into(),
        )]);
        assert_eq!(
            applies(&narrow, &environment, idle, &[], false),
            Applies::No
        );
        let starting = variant(vec![RenderStyleCondition::StartingStyle]);
        assert_eq!(
            applies(&starting, &environment, idle, &[], false),
            Applies::No
        );
        assert_eq!(
            applies(&starting, &environment, idle, &[], true),
            Applies::Yes
        );
    }
}
