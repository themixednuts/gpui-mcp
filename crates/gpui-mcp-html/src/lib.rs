//! Pure HTML authoring and live GPUI rendering for [`gpui_mcp`].
//!
//! [`HtmlUi::compile`] accepts standard local HTML/CSS through `htmlswap`'s
//! fail-closed pure-HTML policy. Bindings live in a separate, versioned document
//! and resolve to application-owned hooks rather than executable source strings.

mod binding;
#[cfg(feature = "runtime")]
mod components;
#[cfg(feature = "runtime")]
mod document;
#[cfg(feature = "runtime")]
mod hooks;
#[cfg(feature = "runtime")]
mod input;
#[cfg(feature = "dev-watch")]
mod project;
#[cfg(feature = "runtime")]
mod render;
#[cfg(feature = "ron")]
mod scaffold;
#[cfg(all(feature = "runtime", feature = "ron"))]
mod session;

pub use binding::{
    BINDING_DOCUMENT_VERSION, Binding, BindingDocument, BindingDocumentError, BindingMode,
    BindingTarget, BindingViolation, ElementId, HandlerId, StateBindingId, UiEvent, UiProperty,
};
#[cfg(feature = "runtime")]
pub use components::{ComponentNode, ComponentRegistry, ComponentRegistryError};
#[cfg(feature = "runtime")]
pub use document::{HtmlDiagnostic, HtmlUi, HtmlUiError};
#[cfg(feature = "runtime")]
pub use hooks::{HookEvent, HookOutcome, HookRegistry, HookRegistryError, StateValue};
#[cfg(feature = "runtime")]
pub use input::init;
#[cfg(feature = "dev-watch")]
pub use project::{
    ProjectChange, ProjectError, ProjectFile, ProjectPaths, ProjectReload, ProjectReloadError,
    ProjectSnapshot, ProjectWatcher,
};
#[cfg(feature = "runtime")]
pub use render::{
    LiveHtml, ReloadError, ReloadReport, RenderDiagnostic, SemanticNamespace,
    SemanticNamespaceError,
};
#[cfg(feature = "ron")]
pub use scaffold::{Decorations, ProjectSpec, ScaffoldError, generate};
#[cfg(all(feature = "runtime", feature = "ron"))]
pub use session::{LiveHtmlSession, LiveHtmlSessionError};
