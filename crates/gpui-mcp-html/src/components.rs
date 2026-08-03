use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use gpui::{AnyElement, App, Window};

pub(crate) type ComponentFactory =
    Rc<dyn Fn(&ComponentNode, Vec<AnyElement>, &mut Window, &mut App) -> AnyElement>;

/// Owned custom-element input supplied to an application component factory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentNode {
    id: String,
    tag: String,
    attributes: BTreeMap<String, String>,
}

impl ComponentNode {
    pub(crate) fn new(id: String, tag: String, attributes: BTreeMap<String, String>) -> Self {
        Self {
            id,
            tag,
            attributes,
        }
    }

    /// Stable standard-HTML ID, or a deterministic generated preview ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Lowercase custom-element tag.
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Ordinary HTML attributes supplied to this element.
    #[must_use]
    pub fn attributes(&self) -> &BTreeMap<String, String> {
        &self.attributes
    }

    /// Return one ordinary HTML attribute.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }
}

/// Application-owned custom-element factories for live previews and embedded UIs.
#[derive(Clone, Default)]
pub struct ComponentRegistry {
    factories: HashMap<String, ComponentFactory>,
}

impl ComponentRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a valid custom-element name such as `project-card`.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid names or an already-registered tag.
    pub fn register(
        &mut self,
        tag: impl Into<String>,
        factory: impl Fn(&ComponentNode, Vec<AnyElement>, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Result<(), ComponentRegistryError> {
        let tag = tag.into();
        if !is_custom_element_name(&tag) {
            return Err(ComponentRegistryError::InvalidName { tag });
        }
        if self.factories.contains_key(&tag) {
            return Err(ComponentRegistryError::Duplicate { tag });
        }
        self.factories.insert(tag, Rc::new(factory));
        Ok(())
    }

    pub(crate) fn factory(&self, tag: &str) -> Option<&ComponentFactory> {
        self.factories.get(tag)
    }
}

fn is_custom_element_name(tag: &str) -> bool {
    let mut characters = tag.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && tag.contains('-')
        && !matches!(
            tag,
            "annotation-xml"
                | "color-profile"
                | "font-face"
                | "font-face-src"
                | "font-face-uri"
                | "font-face-format"
                | "font-face-name"
                | "missing-glyph"
        )
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_' | '.')
        })
}

/// Custom-element registration failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ComponentRegistryError {
    /// Custom-element names must use lowercase HTML custom-element syntax.
    #[error("`{tag}` is not a valid custom-element name")]
    InvalidName {
        /// Rejected tag.
        tag: String,
    },
    /// A factory already owns this tag.
    #[error("custom element `{tag}` is already registered")]
    Duplicate {
        /// Already registered tag.
        tag: String,
    },
}

#[cfg(test)]
mod tests {
    use gpui::{IntoElement as _, div};

    use super::{ComponentRegistry, ComponentRegistryError};

    #[test]
    fn registry_requires_standard_custom_element_names() -> Result<(), ComponentRegistryError> {
        let mut registry = ComponentRegistry::new();
        assert!(matches!(
            registry.register("Button", |_, _, _, _| div().into_any_element()),
            Err(ComponentRegistryError::InvalidName { .. })
        ));
        assert!(matches!(
            registry.register("Project-card", |_, _, _, _| div().into_any_element()),
            Err(ComponentRegistryError::InvalidName { .. })
        ));
        assert!(matches!(
            registry.register("font-face", |_, _, _, _| div().into_any_element()),
            Err(ComponentRegistryError::InvalidName { .. })
        ));
        registry.register("project-card", |_, _, _, _| div().into_any_element())?;
        Ok(())
    }
}
