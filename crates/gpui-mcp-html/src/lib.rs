//! Pure HTML authoring and live GPUI rendering for [`gpui_mcp`].
//!
//! [`HtmlUi::compile`] accepts standard local HTML/CSS through `htmlswap`'s
//! fail-closed pure-HTML policy. Bindings live in a separate, versioned document
//! and resolve to application-owned hooks rather than executable source strings.

mod binding;
mod components;
mod document;
mod hooks;
mod input;
#[cfg(feature = "dev-watch")]
mod project;
mod render;
#[cfg(feature = "ron")]
mod scaffold;
#[cfg(feature = "ron")]
mod session;

pub use binding::{
    BINDING_DOCUMENT_VERSION, Binding, BindingDocument, BindingDocumentError, BindingMode,
    BindingTarget, BindingViolation, ElementId, HandlerId, StateBindingId, UiEvent, UiProperty,
};
pub use components::{ComponentNode, ComponentRegistry, ComponentRegistryError};
pub use document::{HtmlDiagnostic, HtmlUi, HtmlUiError};
/// Native window root required by the shared text-input backend.
///
/// Wrap the application view in this root when a live document can render
/// `input` or `textarea` elements. The project scaffolder does this by default.
pub use gpui_component::Root as NativeRoot;
pub use hooks::{HookEvent, HookRegistry, HookRegistryError, StateValue};
pub use input::init;
#[cfg(feature = "dev-watch")]
pub use project::{
    ProjectChange, ProjectError, ProjectFile, ProjectPaths, ProjectReload, ProjectReloadError,
    ProjectSnapshot, ProjectWatcher,
};
pub use render::{
    LiveHtml, ReloadError, ReloadReport, RenderDiagnostic, SemanticNamespace,
    SemanticNamespaceError,
};
#[cfg(feature = "ron")]
pub use scaffold::{OutputWindowDecorations, ProjectOptions, ScaffoldError, scaffold_project};
#[cfg(feature = "ron")]
pub use session::{LiveHtmlSession, LiveHtmlSessionError};
