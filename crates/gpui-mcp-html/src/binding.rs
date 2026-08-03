use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Current on-disk binding document version.
pub const BINDING_DOCUMENT_VERSION: u16 = 1;
const MAX_BINDINGS: usize = 4_096;
const MAX_ID_BYTES: usize = 128;
const MAX_SYMBOL_BYTES: usize = 256;

/// Stable standard-HTML element ID used for exact binding resolution.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ElementId(String);

impl ElementId {
    /// Construct an element ID. Full validation occurs when the document is validated.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Application-owned hook symbol.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HandlerId(String);

impl HandlerId {
    /// Construct a handler symbol. Full validation occurs with its document.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the handler symbol.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Application-owned state binding symbol.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StateBindingId(String);

impl StateBindingId {
    /// Construct a state symbol. Full validation occurs with its document.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the state symbol.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact binding target. Selector matching is deliberately excluded from action routing.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum BindingTarget {
    /// A standard HTML `id` attribute.
    Id(ElementId),
}

impl BindingTarget {
    /// Return the exact HTML element ID.
    #[must_use]
    pub fn element_id(&self) -> &ElementId {
        match self {
            Self::Id(id) => id,
        }
    }
}

/// UI event that can invoke an application hook.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum UiEvent {
    /// Pointer or semantic activation.
    Click,
    /// Two consecutive pointer activations on the same element.
    DoubleClick,
    /// Keyboard focus request.
    Focus,
    /// Pointer entered the element.
    Hover,
    /// Committed form-control change.
    Change,
    /// Immediate editable input.
    Input,
    /// Form submission.
    Submit,
}

/// Rendered property connected to application state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum UiProperty {
    /// Displayed child text.
    Text,
    /// Form-control value.
    Value,
    /// Checkbox/radio checked state.
    Checked,
    /// Selection state.
    Selected,
    /// Disabled state.
    Disabled,
    /// Visibility state.
    Visible,
    /// Overriding pixel width from application state.
    Width,
    /// Overriding pixel height from application state.
    Height,
}

/// Direction of a property binding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum BindingMode {
    /// Application state flows into the UI.
    #[default]
    OneWay,
    /// Application state flows both into and out of the UI.
    TwoWay,
}

/// Declarative connection between one exact HTML element and application behavior.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Binding {
    /// Invoke a named application handler for an event.
    Event {
        /// Exact target.
        target: BindingTarget,
        /// Event to observe.
        event: UiEvent,
        /// Registered application handler.
        handler: HandlerId,
    },
    /// Connect a rendered property to application-owned state.
    Property {
        /// Exact target.
        target: BindingTarget,
        /// Rendered property.
        property: UiProperty,
        /// Registered state source.
        source: StateBindingId,
        /// Data-flow direction.
        #[serde(default)]
        mode: BindingMode,
    },
}

impl Binding {
    /// Exact target element ID.
    #[must_use]
    pub fn element_id(&self) -> &ElementId {
        match self {
            Self::Event { target, .. } | Self::Property { target, .. } => target.element_id(),
        }
    }
}

/// Versioned, format-independent binding graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BindingDocument {
    /// Schema version.
    pub version: u16,
    /// Ordered exact bindings.
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

impl Default for BindingDocument {
    fn default() -> Self {
        Self::new()
    }
}

impl BindingDocument {
    /// Create an empty document using the current schema version.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            version: BINDING_DOCUMENT_VERSION,
            bindings: Vec::new(),
        }
    }

    /// Add a binding.
    #[must_use]
    pub fn with_binding(mut self, binding: Binding) -> Self {
        self.bindings.push(binding);
        self
    }

    /// Validate version, resource bounds, identifiers, and uniqueness.
    ///
    /// # Errors
    ///
    /// Returns every bounded semantic violation found in the document.
    pub fn validate(&self) -> Result<(), BindingDocumentError> {
        let mut violations = Vec::new();
        if self.version != BINDING_DOCUMENT_VERSION {
            violations.push(BindingViolation::UnsupportedVersion {
                found: self.version,
                supported: BINDING_DOCUMENT_VERSION,
            });
        }
        if self.bindings.len() > MAX_BINDINGS {
            violations.push(BindingViolation::TooManyBindings {
                found: self.bindings.len(),
                maximum: MAX_BINDINGS,
            });
        }

        let mut events = HashSet::new();
        let mut properties = HashSet::new();
        for (index, binding) in self.bindings.iter().enumerate().take(MAX_BINDINGS) {
            validate_identifier(
                index,
                "element ID",
                binding.element_id().as_str(),
                MAX_ID_BYTES,
                &mut violations,
            );
            match binding {
                Binding::Event {
                    target,
                    event,
                    handler,
                } => {
                    validate_symbol(index, "handler", handler.as_str(), &mut violations);
                    if !events.insert((target.clone(), *event)) {
                        violations.push(BindingViolation::DuplicateEvent { index });
                    }
                }
                Binding::Property {
                    target,
                    property,
                    source,
                    ..
                } => {
                    validate_symbol(index, "state source", source.as_str(), &mut violations);
                    if !properties.insert((target.clone(), *property)) {
                        violations.push(BindingViolation::DuplicateProperty { index });
                    }
                }
            }
        }

        if violations.is_empty() {
            Ok(())
        } else {
            Err(BindingDocumentError::Invalid { violations })
        }
    }

    /// Parse and validate a RON binding document.
    ///
    /// # Errors
    ///
    /// Returns a parse error or semantic validation failures.
    #[cfg(feature = "ron")]
    pub fn from_ron(source: &str) -> Result<Self, BindingDocumentError> {
        let document: Self = ron::from_str(source).map_err(BindingDocumentError::RonParse)?;
        document.validate()?;
        Ok(document)
    }

    /// Serialize a validated document as canonical pretty RON.
    ///
    /// # Errors
    ///
    /// Returns semantic validation failures or a serialization error.
    #[cfg(feature = "ron")]
    pub fn to_ron_pretty(&self) -> Result<String, BindingDocumentError> {
        self.validate()?;
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(BindingDocumentError::RonSerialize)
    }

    /// Parse and validate a JSON binding document.
    ///
    /// # Errors
    ///
    /// Returns a parse error or semantic validation failures.
    #[cfg(feature = "json")]
    pub fn from_json(source: &str) -> Result<Self, BindingDocumentError> {
        let document: Self =
            serde_json::from_str(source).map_err(BindingDocumentError::JsonParse)?;
        document.validate()?;
        Ok(document)
    }

    /// Serialize a validated document as pretty JSON for MCP and external tools.
    ///
    /// # Errors
    ///
    /// Returns semantic validation failures or a serialization error.
    #[cfg(feature = "json")]
    pub fn to_json_pretty(&self) -> Result<String, BindingDocumentError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(BindingDocumentError::JsonSerialize)
    }
}

fn validate_identifier(
    index: usize,
    field: &'static str,
    value: &str,
    maximum: usize,
    violations: &mut Vec<BindingViolation>,
) {
    let mut characters = value.chars();
    let valid = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':')
        });
    if value.is_empty() || value.len() > maximum || !valid {
        violations.push(BindingViolation::InvalidIdentifier {
            index,
            field,
            maximum,
        });
    }
}

fn validate_symbol(
    index: usize,
    field: &'static str,
    value: &str,
    violations: &mut Vec<BindingViolation>,
) {
    validate_identifier(index, field, value, MAX_SYMBOL_BYTES, violations);
}

/// One stable binding validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingViolation {
    /// The document uses an unsupported schema version.
    UnsupportedVersion {
        /// Parsed version.
        found: u16,
        /// Current supported version.
        supported: u16,
    },
    /// The document exceeds its bounded binding capacity.
    TooManyBindings {
        /// Parsed count.
        found: usize,
        /// Maximum accepted count.
        maximum: usize,
    },
    /// An ID or symbol is empty, oversized, or contains unsupported characters.
    InvalidIdentifier {
        /// Binding index.
        index: usize,
        /// Field name.
        field: &'static str,
        /// Maximum accepted byte length.
        maximum: usize,
    },
    /// A target/event pair was declared more than once.
    DuplicateEvent {
        /// Later duplicate binding index.
        index: usize,
    },
    /// A target/property pair was declared more than once.
    DuplicateProperty {
        /// Later duplicate binding index.
        index: usize,
    },
}

impl fmt::Display for BindingViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found, supported } => {
                write!(
                    formatter,
                    "unsupported binding version {found}; expected {supported}"
                )
            }
            Self::TooManyBindings { found, maximum } => {
                write!(formatter, "binding count {found} exceeds maximum {maximum}")
            }
            Self::InvalidIdentifier {
                index,
                field,
                maximum,
            } => write!(
                formatter,
                "binding {index} has invalid {field}; expected at most {maximum} bytes of identifier characters"
            ),
            Self::DuplicateEvent { index } => {
                write!(formatter, "binding {index} duplicates an event binding")
            }
            Self::DuplicateProperty { index } => {
                write!(formatter, "binding {index} duplicates a property binding")
            }
        }
    }
}

/// Parse, serialization, or semantic validation error for a binding document.
#[derive(Debug, thiserror::Error)]
pub enum BindingDocumentError {
    /// RON syntax was invalid.
    #[cfg(feature = "ron")]
    #[error("parse RON binding document")]
    RonParse(#[source] ron::error::SpannedError),
    /// RON serialization failed.
    #[cfg(feature = "ron")]
    #[error("serialize RON binding document")]
    RonSerialize(#[source] ron::Error),
    /// JSON syntax was invalid.
    #[cfg(feature = "json")]
    #[error("parse JSON binding document")]
    JsonParse(#[source] serde_json::Error),
    /// JSON serialization failed.
    #[cfg(feature = "json")]
    #[error("serialize JSON binding document")]
    JsonSerialize(#[source] serde_json::Error),
    /// Parsed data violated the binding schema.
    #[error("invalid binding document: {violations:?}")]
    Invalid {
        /// All bounded validation failures.
        violations: Vec<BindingViolation>,
    },
}

impl BindingDocumentError {
    /// Return semantic violations when parsing succeeded but validation failed.
    #[must_use]
    pub fn violations(&self) -> Option<&[BindingViolation]> {
        match self {
            Self::Invalid { violations } => Some(violations),
            #[cfg(feature = "ron")]
            Self::RonParse(_) | Self::RonSerialize(_) => None,
            #[cfg(feature = "json")]
            Self::JsonParse(_) | Self::JsonSerialize(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Binding, BindingDocument, BindingDocumentError, BindingTarget, ElementId, HandlerId,
        UiEvent,
    };

    #[test]
    fn ron_and_json_round_trip_the_same_domain_document() -> Result<(), BindingDocumentError> {
        let document = BindingDocument::new().with_binding(Binding::Event {
            target: BindingTarget::Id(ElementId::new("save")),
            event: UiEvent::Click,
            handler: HandlerId::new("save_document"),
        });

        let ron = document.to_ron_pretty()?;
        let json = document.to_json_pretty()?;

        assert_eq!(BindingDocument::from_ron(&ron)?, document);
        assert_eq!(BindingDocument::from_json(&json)?, document);
        Ok(())
    }

    #[test]
    fn duplicate_exact_event_bindings_are_rejected() {
        let binding = Binding::Event {
            target: BindingTarget::Id(ElementId::new("save")),
            event: UiEvent::Click,
            handler: HandlerId::new("save_document"),
        };
        let document = BindingDocument::new()
            .with_binding(binding.clone())
            .with_binding(binding);

        assert!(matches!(
            document.validate(),
            Err(BindingDocumentError::Invalid { violations }) if violations.len() == 1
        ));
    }
}
