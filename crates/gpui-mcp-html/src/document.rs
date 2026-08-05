use std::collections::{HashMap, HashSet};

use htmlswap::{
    Compilation, CompileAssets, Compiler, CompilerBuildError, CompilerOptions,
    CompilerResourceOptions, Diagnostic, RenderElement, RenderNode, RenderPlan, Severity,
    SourcePolicy, Span, UiRole,
};

use crate::{Binding, BindingDocument, BindingDocumentError, BindingMode, UiEvent, UiProperty};

/// Source diagnostic retained for visual-builder display.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HtmlDiagnostic {
    /// Severity reported by htmlswap or binding resolution.
    pub severity: Severity,
    /// Human-readable explanation.
    pub message: String,
    /// Original HTML/CSS source span when available.
    pub span: Option<Span>,
}

impl From<Diagnostic> for HtmlDiagnostic {
    fn from(diagnostic: Diagnostic) -> Self {
        Self {
            severity: diagnostic.severity,
            message: diagnostic.message,
            span: diagnostic.span,
        }
    }
}

/// Validated pure-HTML document and its target-neutral render plan.
#[derive(Clone, Debug)]
pub struct HtmlUi {
    source: String,
    plan: RenderPlan,
    bindings: BindingDocument,
    diagnostics: Vec<HtmlDiagnostic>,
}

impl HtmlUi {
    /// Compile pure HTML with no additional stylesheet assets.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bindings, forbidden source features, duplicate
    /// IDs, missing binding targets, or incompatible event/property bindings.
    pub fn compile(
        source: impl Into<String>,
        bindings: BindingDocument,
    ) -> Result<Self, HtmlUiError> {
        Self::compile_with_assets(source, bindings, &CompileAssets::new())
    }

    /// Compile pure HTML with one trusted local stylesheet.
    ///
    /// # Errors
    ///
    /// Returns an error when source, bindings, or their relationship is invalid.
    pub fn compile_with_stylesheet(
        source: impl Into<String>,
        bindings: BindingDocument,
        name: impl Into<String>,
        stylesheet: impl Into<String>,
    ) -> Result<Self, HtmlUiError> {
        let assets = CompileAssets::new().with_stylesheet(Some(name.into()), stylesheet.into());
        Self::compile_with_assets(source, bindings, &assets)
    }

    /// Compile pure HTML with caller-supplied local stylesheet assets.
    ///
    /// Script assets and remote stylesheet references are rejected by policy.
    ///
    /// # Errors
    ///
    /// Returns an error when source, assets, bindings, or their relationship is invalid.
    pub fn compile_with_assets(
        source: impl Into<String>,
        bindings: BindingDocument,
        assets: &CompileAssets,
    ) -> Result<Self, HtmlUiError> {
        bindings.validate().map_err(HtmlUiError::Bindings)?;
        let source = source.into();
        let compiler = Compiler::try_with_options(
            CompilerOptions::new()
                .with_source_policy(SourcePolicy::pure_html())
                .with_resources(
                    CompilerResourceOptions::new()
                        .with_remote_resolution(false)
                        .with_file_resolution(false),
                ),
        )
        .map_err(HtmlUiError::BuildCompiler)?;
        let Compilation {
            value: render_output,
            diagnostics,
        } = compiler.compile_fragment(source.clone(), assets);
        let diagnostics = diagnostics
            .into_iter()
            .map(HtmlDiagnostic::from)
            .collect::<Vec<_>>();
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Err(HtmlUiError::Source { diagnostics });
        }

        let mut resolution = validate_binding_targets(&render_output.plan, &bindings);
        if resolution
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Err(HtmlUiError::Source {
                diagnostics: resolution,
            });
        }
        resolution.extend(diagnostics);

        Ok(Self {
            source,
            plan: render_output.plan,
            bindings,
            diagnostics: resolution,
        })
    }

    /// Original pure HTML source.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Target-neutral htmlswap render plan.
    #[must_use]
    pub fn plan(&self) -> &RenderPlan {
        &self.plan
    }

    /// Validated external binding graph.
    #[must_use]
    pub fn bindings(&self) -> &BindingDocument {
        &self.bindings
    }

    /// Non-fatal compiler diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &[HtmlDiagnostic] {
        &self.diagnostics
    }
}

fn validate_binding_targets(plan: &RenderPlan, bindings: &BindingDocument) -> Vec<HtmlDiagnostic> {
    let mut elements = HashMap::new();
    let mut duplicates = HashSet::new();
    let mut mutation_channels = HashSet::new();
    collect_identified_elements(&plan.nodes, &mut elements, &mut duplicates);
    let mut diagnostics = duplicates
        .into_iter()
        .map(|id| HtmlDiagnostic {
            severity: Severity::Error,
            message: format!("duplicate HTML id `{id}` cannot be bound exactly"),
            span: None,
        })
        .collect::<Vec<_>>();

    for reserved in elements
        .keys()
        .filter(|id| id.as_str() == "html-root" || id.starts_with("html-node-"))
    {
        diagnostics.push(HtmlDiagnostic {
            severity: Severity::Error,
            message: format!(
                "HTML id `{reserved}` is reserved for gpui-mcp renderer-generated nodes"
            ),
            span: elements.get(reserved).and_then(|element| element.span),
        });
    }

    for binding in &bindings.bindings {
        let id = binding.element_id().as_str();
        let Some(element) = elements.get(id) else {
            diagnostics.push(HtmlDiagnostic {
                severity: Severity::Error,
                message: format!("binding target `{id}` does not exist in the HTML document"),
                span: None,
            });
            continue;
        };
        if !binding_is_compatible(binding, element) {
            diagnostics.push(HtmlDiagnostic {
                severity: Severity::Error,
                message: format!(
                    "binding for `{id}` is incompatible with `<{}>` semantics",
                    element.source_tag
                ),
                span: element.span,
            });
            continue;
        }
        if let Some(channel) = mutation_channel(binding, element)
            && !mutation_channels.insert((id.to_owned(), channel))
        {
            diagnostics.push(HtmlDiagnostic {
                severity: Severity::Error,
                message: format!(
                    "multiple two-way bindings for `{id}` resolve to the same input channel"
                ),
                span: element.span,
            });
        }
    }

    diagnostics
}

fn collect_identified_elements<'a>(
    nodes: &'a [RenderNode],
    elements: &mut HashMap<String, &'a RenderElement>,
    duplicates: &mut HashSet<String>,
) {
    for node in nodes {
        let RenderNode::Element(element) = node else {
            continue;
        };
        if let Some(id) = attribute(element, "id")
            && elements.insert(id.to_owned(), element).is_some()
        {
            duplicates.insert(id.to_owned());
        }
        collect_identified_elements(&element.children, elements, duplicates);
    }
}

fn binding_is_compatible(binding: &Binding, element: &RenderElement) -> bool {
    match binding {
        Binding::Event { event, .. } => match event {
            UiEvent::Click | UiEvent::DoubleClick | UiEvent::Focus | UiEvent::Hover => true,
            UiEvent::Change | UiEvent::Input => element.form_control.is_some(),
            UiEvent::Submit => element.source_tag == "form",
        },
        Binding::Property { property, mode, .. } => {
            if *mode == BindingMode::TwoWay
                && matches!(
                    property,
                    UiProperty::Disabled
                        | UiProperty::Visible
                        | UiProperty::Width
                        | UiProperty::Height
                )
            {
                false
            } else {
                match property {
                    UiProperty::Text
                    | UiProperty::Disabled
                    | UiProperty::Visible
                    | UiProperty::Width
                    | UiProperty::Height => true,
                    UiProperty::Value => element.form_control.is_some(),
                    UiProperty::Checked => attribute(element, "type")
                        .is_some_and(|value| matches!(value, "checkbox" | "radio")),
                    UiProperty::Selected => {
                        matches!(element.source_tag.as_str(), "option" | "button")
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum MutationChannel {
    Text,
    Value,
}

fn mutation_channel(binding: &Binding, element: &RenderElement) -> Option<MutationChannel> {
    let Binding::Property {
        property,
        mode: BindingMode::TwoWay,
        ..
    } = binding
    else {
        return None;
    };
    match property {
        UiProperty::Text => Some(MutationChannel::Text),
        UiProperty::Value if is_text_editable(element) => Some(MutationChannel::Text),
        UiProperty::Value | UiProperty::Checked | UiProperty::Selected => {
            Some(MutationChannel::Value)
        }
        UiProperty::Disabled | UiProperty::Visible | UiProperty::Width | UiProperty::Height => None,
    }
}

pub(crate) fn is_text_editable(element: &RenderElement) -> bool {
    matches!(element.role, UiRole::TextInput)
        && !attribute(element, "type").is_some_and(|kind| matches!(kind, "checkbox" | "radio"))
}

pub(crate) fn attribute<'a>(element: &'a RenderElement, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.name == name)
        .map(|attribute| attribute.value.as_str())
}

/// Failure to compile or validate a pure HTML UI.
#[derive(Debug, thiserror::Error)]
pub enum HtmlUiError {
    /// htmlswap compiler construction failed.
    #[error("build pure HTML compiler")]
    BuildCompiler(#[source] CompilerBuildError),
    /// External bindings were invalid.
    #[error("validate binding document")]
    Bindings(#[source] BindingDocumentError),
    /// HTML/CSS or binding-to-document resolution failed.
    #[error("pure HTML source is invalid")]
    Source {
        /// All source validation failures.
        diagnostics: Vec<HtmlDiagnostic>,
    },
}

impl HtmlUiError {
    /// Return source diagnostics when compilation reached the source-validation phase.
    #[must_use]
    pub fn diagnostics(&self) -> Option<&[HtmlDiagnostic]> {
        match self {
            Self::Source { diagnostics } => Some(diagnostics),
            Self::BuildCompiler(_) | Self::Bindings(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Binding, BindingDocument, BindingMode, BindingTarget, ElementId, HandlerId, HtmlUi,
        HtmlUiError, StateBindingId, UiEvent, UiProperty,
    };

    #[test]
    fn pure_document_resolves_binding_ids() -> Result<(), HtmlUiError> {
        let bindings = BindingDocument::new().with_binding(Binding::Event {
            target: BindingTarget::Id(ElementId::new("save")),
            event: UiEvent::Click,
            handler: HandlerId::new("save_document"),
        });
        let ui = HtmlUi::compile("<button id=\"save\">Save</button>", bindings)?;

        assert_eq!(ui.plan().nodes.len(), 1);
        Ok(())
    }

    #[test]
    fn missing_binding_target_is_rejected() {
        let bindings = BindingDocument::new().with_binding(Binding::Event {
            target: BindingTarget::Id(ElementId::new("missing")),
            event: UiEvent::Click,
            handler: HandlerId::new("save_document"),
        });

        assert!(matches!(
            HtmlUi::compile("<button id=\"save\">Save</button>", bindings),
            Err(HtmlUiError::Source { diagnostics }) if diagnostics.len() == 1
        ));
    }

    #[test]
    fn renderer_node_ids_are_reserved() {
        for id in ["html-root", "html-node-0"] {
            assert!(matches!(
                HtmlUi::compile(format!("<div id=\"{id}\"></div>"), BindingDocument::new()),
                Err(HtmlUiError::Source { diagnostics })
                    if diagnostics.iter().any(|diagnostic| diagnostic.message.contains("reserved"))
            ));
        }
    }

    #[test]
    fn two_way_properties_cannot_share_one_input_channel() {
        let bindings = BindingDocument::new()
            .with_binding(Binding::Property {
                target: BindingTarget::Id(ElementId::new("title")),
                property: UiProperty::Text,
                source: StateBindingId::new("title_text"),
                mode: BindingMode::TwoWay,
            })
            .with_binding(Binding::Property {
                target: BindingTarget::Id(ElementId::new("title")),
                property: UiProperty::Value,
                source: StateBindingId::new("title_value"),
                mode: BindingMode::TwoWay,
            });

        assert!(matches!(
            HtmlUi::compile("<input id=\"title\" type=\"text\">", bindings),
            Err(HtmlUiError::Source { diagnostics })
                if diagnostics.iter().any(|diagnostic| diagnostic.message.contains("same input channel"))
        ));
    }

    #[test]
    fn non_mutable_properties_reject_two_way_bindings() {
        let bindings = BindingDocument::new().with_binding(Binding::Property {
            target: BindingTarget::Id(ElementId::new("save")),
            property: UiProperty::Disabled,
            source: StateBindingId::new("save_disabled"),
            mode: BindingMode::TwoWay,
        });

        assert!(matches!(
            HtmlUi::compile("<button id=\"save\">Save</button>", bindings),
            Err(HtmlUiError::Source { diagnostics })
                if diagnostics.iter().any(|diagnostic| diagnostic.message.contains("incompatible"))
        ));
    }

    #[test]
    fn one_way_width_binding_on_any_element_validates() -> Result<(), HtmlUiError> {
        let bindings = BindingDocument::new().with_binding(Binding::Property {
            target: BindingTarget::Id(ElementId::new("panel")),
            property: UiProperty::Width,
            source: StateBindingId::new("panel_width"),
            mode: BindingMode::OneWay,
        });
        let ui = HtmlUi::compile("<div id=\"panel\"></div>", bindings)?;

        assert_eq!(ui.plan().nodes.len(), 1);
        Ok(())
    }

    #[test]
    fn two_way_width_binding_is_rejected() {
        let bindings = BindingDocument::new().with_binding(Binding::Property {
            target: BindingTarget::Id(ElementId::new("panel")),
            property: UiProperty::Width,
            source: StateBindingId::new("panel_width"),
            mode: BindingMode::TwoWay,
        });

        assert!(matches!(
            HtmlUi::compile("<div id=\"panel\"></div>", bindings),
            Err(HtmlUiError::Source { diagnostics })
                if diagnostics.iter().any(|diagnostic| diagnostic.message.contains("incompatible"))
        ));
    }
}
