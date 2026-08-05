use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use gpui::{App, Window};

use crate::{Binding, BindingDocument, BindingMode, ElementId, HandlerId, StateBindingId, UiEvent};

type EventHook = Rc<dyn Fn(&HookEvent, &mut Window, &mut App) -> HookOutcome>;
type StateReader = Rc<dyn Fn(&mut Window, &mut App) -> StateValue>;
type StateWriter = Rc<dyn Fn(StateValue, &mut Window, &mut App) -> HookOutcome>;

/// Result returned by an application-owned event or state hook.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HookOutcome {
    /// The hook accepted and handled the request.
    Handled,
    /// The hook deliberately declined the request.
    Rejected {
        /// Non-sensitive explanation suitable for diagnostics.
        reason: String,
    },
}

/// Runtime value exposed by an application-owned state binding.
#[derive(Clone, Debug, PartialEq)]
pub enum StateValue {
    /// UTF-8 text.
    Text(String),
    /// Boolean state.
    Boolean(bool),
    /// Finite or application-defined numeric state.
    Number(f64),
    /// Missing or intentionally empty state.
    Empty,
}

impl StateValue {
    pub(crate) fn display(&self) -> String {
        match self {
            Self::Text(value) => value.clone(),
            Self::Boolean(value) => value.to_string(),
            Self::Number(value) => value.to_string(),
            Self::Empty => String::new(),
        }
    }

    pub(crate) fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            Self::Text(value) if value.eq_ignore_ascii_case("true") => Some(true),
            Self::Text(value) if value.eq_ignore_ascii_case("false") => Some(false),
            Self::Text(_) | Self::Number(_) | Self::Empty => None,
        }
    }

    /// Interpret this value as a finite, non-negative pixel length, accepting
    /// a `Number` or a numeric `Text`. Returns `None` for empty, negative,
    /// non-finite, or unparseable values.
    // A `Number` is application-owned `f64` state narrowed to the `f32` pixel
    // lengths GPUI styling expects; any precision beyond `f32` is irrelevant
    // to layout, so truncation here is intentional.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn as_pixels(&self) -> Option<f32> {
        let value = match self {
            Self::Number(value) => *value as f32,
            Self::Text(text) => text.trim().parse::<f32>().ok()?,
            Self::Boolean(_) | Self::Empty => return None,
        };
        (value.is_finite() && value >= 0.0).then_some(value)
    }
}

/// Event delivered to a registered application hook on the GPUI foreground thread.
#[derive(Clone, Debug, PartialEq)]
pub struct HookEvent {
    element_id: ElementId,
    event: UiEvent,
    value: Option<StateValue>,
}

impl HookEvent {
    pub(crate) fn new(element_id: ElementId, event: UiEvent, value: Option<StateValue>) -> Self {
        Self {
            element_id,
            event,
            value,
        }
    }

    /// Exact standard-HTML target ID.
    #[must_use]
    pub fn element_id(&self) -> &ElementId {
        &self.element_id
    }

    /// Declarative event type.
    #[must_use]
    pub fn event(&self) -> UiEvent {
        self.event
    }

    /// Replacement value for input/change events.
    #[must_use]
    pub fn value(&self) -> Option<&StateValue> {
        self.value.as_ref()
    }
}

#[derive(Clone)]
struct StateHook {
    reader: StateReader,
    writer: Option<StateWriter>,
}

/// Explicit registry that resolves declarative symbols to typed Rust callbacks.
#[derive(Clone, Default)]
pub struct HookRegistry {
    events: HashMap<HandlerId, EventHook>,
    states: HashMap<StateBindingId, StateHook>,
}

impl HookRegistry {
    /// Create an empty hook registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one event callback without replacing an existing symbol.
    ///
    /// # Errors
    ///
    /// Returns an error when the event symbol is already registered.
    pub fn register_event(
        &mut self,
        id: HandlerId,
        hook: impl Fn(&HookEvent, &mut Window, &mut App) -> HookOutcome + 'static,
    ) -> Result<(), HookRegistryError> {
        if self.events.contains_key(&id) {
            return Err(HookRegistryError::DuplicateEvent {
                id: id.as_str().to_owned(),
            });
        }
        self.events.insert(id, Rc::new(hook));
        Ok(())
    }

    /// Register a read-only property source.
    ///
    /// # Errors
    ///
    /// Returns an error when the state symbol is already registered.
    pub fn register_state(
        &mut self,
        id: StateBindingId,
        read: impl Fn(&mut Window, &mut App) -> StateValue + 'static,
    ) -> Result<(), HookRegistryError> {
        self.register_state_hook(id, Rc::new(read), None)
    }

    /// Register a property source that can participate in two-way bindings.
    ///
    /// # Errors
    ///
    /// Returns an error when the state symbol is already registered.
    pub fn register_state_mut(
        &mut self,
        id: StateBindingId,
        read: impl Fn(&mut Window, &mut App) -> StateValue + 'static,
        write: impl Fn(StateValue, &mut Window, &mut App) -> HookOutcome + 'static,
    ) -> Result<(), HookRegistryError> {
        self.register_state_hook(id, Rc::new(read), Some(Rc::new(write)))
    }

    fn register_state_hook(
        &mut self,
        id: StateBindingId,
        reader: StateReader,
        writer: Option<StateWriter>,
    ) -> Result<(), HookRegistryError> {
        if self.states.contains_key(&id) {
            return Err(HookRegistryError::DuplicateState {
                id: id.as_str().to_owned(),
            });
        }
        self.states.insert(id, StateHook { reader, writer });
        Ok(())
    }

    pub(crate) fn validate(&self, document: &BindingDocument) -> Result<(), HookRegistryError> {
        let mut missing_events = BTreeSet::new();
        let mut missing_states = BTreeSet::new();
        let mut read_only_states = BTreeSet::new();
        for binding in &document.bindings {
            match binding {
                Binding::Event { handler, .. } if !self.events.contains_key(handler) => {
                    missing_events.insert(handler.as_str().to_owned());
                }
                Binding::Property { source, mode, .. } => {
                    let Some(state) = self.states.get(source) else {
                        missing_states.insert(source.as_str().to_owned());
                        continue;
                    };
                    if *mode == BindingMode::TwoWay && state.writer.is_none() {
                        read_only_states.insert(source.as_str().to_owned());
                    }
                }
                Binding::Event { .. } => {}
            }
        }
        if missing_events.is_empty() && missing_states.is_empty() && read_only_states.is_empty() {
            Ok(())
        } else {
            Err(HookRegistryError::Unresolved {
                missing_events: missing_events.into_iter().collect(),
                missing_states: missing_states.into_iter().collect(),
                read_only_states: read_only_states.into_iter().collect(),
            })
        }
    }

    pub(crate) fn invoke(
        &self,
        handler: &HandlerId,
        event: &HookEvent,
        window: &mut Window,
        cx: &mut App,
    ) -> HookOutcome {
        self.events.get(handler).map_or_else(
            || Self::missing_hook_outcome("binding handler is unavailable"),
            |hook| hook(event, window, cx),
        )
    }

    pub(crate) fn read(
        &self,
        source: &StateBindingId,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<StateValue> {
        self.states
            .get(source)
            .map(|state| (state.reader)(window, cx))
    }

    pub(crate) fn write(
        &self,
        source: &StateBindingId,
        value: StateValue,
        window: &mut Window,
        cx: &mut App,
    ) -> HookOutcome {
        self.states
            .get(source)
            .and_then(|state| state.writer.as_ref())
            .map_or_else(
                || Self::missing_hook_outcome("state binding is read-only"),
                |writer| writer(value, window, cx),
            )
    }

    fn missing_hook_outcome(reason: &str) -> HookOutcome {
        HookOutcome::Rejected {
            reason: reason.to_owned(),
        }
    }
}

/// Registration or binding-resolution failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HookRegistryError {
    /// An event hook already owns the symbol.
    #[error("event hook `{id}` is already registered")]
    DuplicateEvent {
        /// Duplicate symbol.
        id: String,
    },
    /// A state hook already owns the symbol.
    #[error("state hook `{id}` is already registered")]
    DuplicateState {
        /// Duplicate symbol.
        id: String,
    },
    /// One or more binding symbols could not be resolved safely.
    #[error(
        "unresolved hooks: missing events {missing_events:?}, missing states {missing_states:?}, read-only states used two-way {read_only_states:?}"
    )]
    Unresolved {
        /// Every missing event symbol, sorted and deduplicated.
        missing_events: Vec<String>,
        /// Every missing state symbol, sorted and deduplicated.
        missing_states: Vec<String>,
        /// Every read-only state symbol used by a two-way binding.
        read_only_states: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use crate::{
        Binding, BindingDocument, BindingMode, BindingTarget, ElementId, HandlerId, StateBindingId,
        UiEvent, UiProperty,
    };

    use super::{HookRegistry, HookRegistryError, StateValue};

    fn unresolved_document() -> BindingDocument {
        BindingDocument::new()
            .with_binding(Binding::Event {
                target: BindingTarget::Id(ElementId::new("run")),
                event: UiEvent::Click,
                handler: HandlerId::new("run_project"),
            })
            .with_binding(Binding::Property {
                target: BindingTarget::Id(ElementId::new("title")),
                property: UiProperty::Value,
                source: StateBindingId::new("project_title"),
                mode: BindingMode::TwoWay,
            })
    }

    #[test]
    fn strict_registry_rejects_unknown_project_hooks() {
        assert!(matches!(
            HookRegistry::new().validate(&unresolved_document()),
            Err(HookRegistryError::Unresolved { .. })
        ));
    }

    #[test]
    fn as_pixels_accepts_numbers_and_numeric_text() {
        assert_eq!(StateValue::Number(240.0).as_pixels(), Some(240.0));
        assert_eq!(StateValue::Text("180".to_owned()).as_pixels(), Some(180.0));
    }

    #[test]
    fn as_pixels_rejects_negative_non_finite_and_unparseable_values() {
        assert_eq!(StateValue::Number(-1.0).as_pixels(), None);
        assert_eq!(StateValue::Number(f64::NAN).as_pixels(), None);
        assert_eq!(StateValue::Number(f64::INFINITY).as_pixels(), None);
        assert_eq!(StateValue::Text("abc".to_owned()).as_pixels(), None);
    }

    #[test]
    fn as_pixels_rejects_boolean_and_empty() {
        assert_eq!(StateValue::Boolean(true).as_pixels(), None);
        assert_eq!(StateValue::Empty.as_pixels(), None);
    }
}
