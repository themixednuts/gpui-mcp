use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AnyElement, App, Window};
use gpui_mcp::{
    Automation, BridgeError, BridgeHandle, ErrorCode, HostError, LiveDocument,
    LiveDocumentDiagnostic, LiveDocumentPreview, LiveDocumentRequest, LiveDocumentResponse,
    LiveDocumentSource, MAX_LABEL_BYTES, MAX_LIVE_DOCUMENT_DIAGNOSTICS,
    MAX_LIVE_DOCUMENT_SOURCE_BYTES,
};
use htmlswap::Severity;

use crate::{
    BindingDocument, BindingDocumentError, ComponentRegistry, HookRegistry, HookRegistryError,
    HtmlUi, HtmlUiError, LiveHtml, SemanticNamespace,
};

/// Shared live renderer and source bundle used by manual UI code and MCP preview tools.
#[derive(Clone)]
pub struct LiveHtmlSession {
    live: Rc<RefCell<LiveHtml>>,
    source: Rc<RefCell<LiveDocumentSource>>,
}

impl LiveHtmlSession {
    /// Compile a complete HTML/CSS/RON bundle and connect it to app hooks.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized input, invalid bindings/source, or unresolved hooks.
    pub fn compile(
        source: LiveDocumentSource,
        automation: Automation,
        hooks: HookRegistry,
    ) -> Result<Self, LiveHtmlSessionError> {
        validate_source(&source)?;
        let ui = compile_source(&source)?;
        let live = LiveHtml::new(ui, automation, hooks)?;
        Ok(Self {
            live: Rc::new(RefCell::new(live)),
            source: Rc::new(RefCell::new(source)),
        })
    }

    /// Install application custom-element factories.
    #[must_use]
    pub fn with_components(self, components: ComponentRegistry) -> Self {
        self.live.borrow_mut().set_components(components);
        self
    }

    /// Render this session as a namespaced child of another live HTML document.
    #[must_use]
    pub fn embedded(self, namespace: SemanticNamespace) -> Self {
        self.live.borrow_mut().set_embedded_namespace(namespace);
        self
    }

    /// Render the currently active live document.
    #[must_use]
    pub fn render(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        self.live.borrow().render(window, cx)
    }

    /// Render with an embedded logical viewport used for CSS media-query evaluation.
    #[must_use]
    pub fn render_for_viewport(
        &self,
        width: f32,
        height: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.live
            .borrow()
            .render_for_viewport(width, height, window, cx)
    }

    /// Current monotonic document revision.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.live.borrow().revision()
    }

    /// Return the complete active source and diagnostics.
    #[must_use]
    pub fn document(&self) -> LiveDocument {
        active_document(&self.live.borrow(), &self.source.borrow())
    }

    /// Register this session as the bridge's sole opt-in live-document host.
    ///
    /// # Errors
    ///
    /// Returns an error when the bridge capability is disabled or already has a host.
    pub fn serve_mcp(&self, bridge: &BridgeHandle) -> Result<(), HostError> {
        let session = self.clone();
        bridge.on_document(move |request, _, _| session.handle(request))
    }

    /// Apply the same revision-checked in-memory preview path used by MCP.
    ///
    /// Manual Studio edits and filesystem reloads can use this method so every
    /// producer shares identical validation, last-good, and diagnostic semantics.
    ///
    /// # Errors
    ///
    /// Returns a stale-revision error when another producer updated the session first.
    pub fn preview_source(
        &self,
        expected_revision: u64,
        source: LiveDocumentSource,
    ) -> Result<LiveDocumentPreview, BridgeError> {
        let response = self.handle(LiveDocumentRequest::Preview {
            expected_revision,
            source,
        })?;
        let LiveDocumentResponse::Preview(preview) = response else {
            return Err(BridgeError::new(
                ErrorCode::Internal,
                "live document session returned the wrong response",
            ));
        };
        Ok(preview)
    }

    fn handle(&self, request: LiveDocumentRequest) -> Result<LiveDocumentResponse, BridgeError> {
        match request {
            LiveDocumentRequest::Get => Ok(LiveDocumentResponse::Document(self.document())),
            LiveDocumentRequest::Preview {
                expected_revision,
                source,
            } => {
                let active_revision = self.revision();
                if expected_revision != active_revision {
                    return Err(BridgeError::new(
                        ErrorCode::StaleRevision,
                        format!(
                            "live document revision is {active_revision}; edit was based on {expected_revision}"
                        ),
                    ));
                }
                Ok(LiveDocumentResponse::Preview(self.preview(source)))
            }
        }
    }

    fn preview(&self, source: LiveDocumentSource) -> LiveDocumentPreview {
        let candidate = validate_source(&source)
            .map_err(|error| vec![error_diagnostic(&error.to_string())])
            .and_then(|()| compile_source(&source).map_err(diagnostics_for_compile_error));
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(diagnostics) => {
                return LiveDocumentPreview {
                    applied: false,
                    document: self.document(),
                    diagnostics,
                };
            }
        };

        let reload = self.live.borrow_mut().reload(candidate);
        if let Err(error) = reload {
            return LiveDocumentPreview {
                applied: false,
                document: self.document(),
                diagnostics: vec![error_diagnostic(&error.to_string())],
            };
        }
        *self.source.borrow_mut() = source;
        let document = self.document();
        LiveDocumentPreview {
            applied: true,
            diagnostics: document.diagnostics.clone(),
            document,
        }
    }
}

fn compile_source(source: &LiveDocumentSource) -> Result<HtmlUi, LiveHtmlSessionError> {
    let bindings = BindingDocument::from_ron(&source.bindings_ron)?;
    HtmlUi::compile_with_stylesheet(source.html.clone(), bindings, "app.css", source.css.clone())
        .map_err(LiveHtmlSessionError::Document)
}

fn validate_source(source: &LiveDocumentSource) -> Result<(), LiveHtmlSessionError> {
    if source.html.is_empty() {
        return Err(LiveHtmlSessionError::EmptyHtml);
    }
    if source.byte_len() > MAX_LIVE_DOCUMENT_SOURCE_BYTES {
        return Err(LiveHtmlSessionError::SourceTooLarge {
            found: source.byte_len(),
            maximum: MAX_LIVE_DOCUMENT_SOURCE_BYTES,
        });
    }
    Ok(())
}

fn active_document(live: &LiveHtml, source: &LiveDocumentSource) -> LiveDocument {
    let mut diagnostics = live
        .document()
        .diagnostics()
        .iter()
        .map(|diagnostic| LiveDocumentDiagnostic {
            severity: severity_name(diagnostic.severity).to_owned(),
            message: bounded_message(&diagnostic.message),
        })
        .collect::<Vec<_>>();
    diagnostics.extend(
        live.diagnostics()
            .iter()
            .map(|diagnostic| LiveDocumentDiagnostic {
                severity: "warning".to_owned(),
                message: bounded_message(&format!(
                    "{} [{}]: {}",
                    diagnostic.feature, diagnostic.node_id, diagnostic.message
                )),
            }),
    );
    diagnostics.truncate(MAX_LIVE_DOCUMENT_DIAGNOSTICS);
    LiveDocument {
        revision: live.revision(),
        source: source.clone(),
        diagnostics,
    }
}

fn diagnostics_for_compile_error(error: LiveHtmlSessionError) -> Vec<LiveDocumentDiagnostic> {
    match error {
        LiveHtmlSessionError::Document(HtmlUiError::Source { diagnostics }) => diagnostics
            .into_iter()
            .take(MAX_LIVE_DOCUMENT_DIAGNOSTICS)
            .map(|diagnostic| LiveDocumentDiagnostic {
                severity: severity_name(diagnostic.severity).to_owned(),
                message: bounded_message(&diagnostic.message),
            })
            .collect(),
        other => vec![error_diagnostic(&other.to_string())],
    }
}

fn error_diagnostic(message: &str) -> LiveDocumentDiagnostic {
    LiveDocumentDiagnostic {
        severity: "error".to_owned(),
        message: bounded_message(message),
    }
}

const fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Info | Severity::Warning => "warning",
    }
}

fn bounded_message(message: &str) -> String {
    if message.len() <= MAX_LABEL_BYTES {
        return message.to_owned();
    }
    let mut end = MAX_LABEL_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

/// Failure to create a live HTML preview session.
#[derive(Debug, thiserror::Error)]
pub enum LiveHtmlSessionError {
    /// HTML is required for every complete bundle.
    #[error("live document HTML cannot be empty")]
    EmptyHtml,
    /// Combined source exceeded the protocol-safe bound.
    #[error("live document source is {found} bytes; maximum is {maximum}")]
    SourceTooLarge {
        /// Observed source bytes.
        found: usize,
        /// Maximum accepted source bytes.
        maximum: usize,
    },
    /// RON binding parsing or validation failed.
    #[error(transparent)]
    Bindings(#[from] BindingDocumentError),
    /// HTML/CSS compilation or binding resolution failed.
    #[error(transparent)]
    Document(HtmlUiError),
    /// App hook registration did not satisfy the document bindings.
    #[error(transparent)]
    Hooks(#[from] HookRegistryError),
}

#[cfg(test)]
mod tests {
    use gpui_mcp::{Automation, LiveDocumentRequest, LiveDocumentResponse, LiveDocumentSource};

    use crate::HookRegistry;

    use super::LiveHtmlSession;

    fn source(html: &str) -> Result<LiveDocumentSource, Box<dyn std::error::Error>> {
        Ok(LiveDocumentSource {
            html: html.to_owned(),
            css: "button { color: red; }".to_owned(),
            bindings_ron: crate::BindingDocument::new().to_ron_pretty()?,
        })
    }

    #[test]
    fn preview_applies_complete_valid_bundle_and_increments_revision()
    -> Result<(), Box<dyn std::error::Error>> {
        let session = LiveHtmlSession::compile(
            source("<button id='save'>Save</button>")?,
            Automation::for_test(),
            HookRegistry::new(),
        )?;
        let response = session
            .handle(LiveDocumentRequest::Preview {
                expected_revision: 1,
                source: source("<button id='save'>Updated</button>")?,
            })
            .map_err(|error| std::io::Error::other(error.message))?;
        let LiveDocumentResponse::Preview(preview) = response else {
            return Err("unexpected live document response".into());
        };

        assert!(preview.applied);
        assert_eq!(preview.document.revision, 2);
        assert!(preview.document.source.html.contains("Updated"));
        Ok(())
    }

    #[test]
    fn invalid_candidate_returns_diagnostics_and_keeps_last_good_revision()
    -> Result<(), Box<dyn std::error::Error>> {
        let session = LiveHtmlSession::compile(
            source("<button id='save'>Save</button>")?,
            Automation::for_test(),
            HookRegistry::new(),
        )?;
        let response = session
            .handle(LiveDocumentRequest::Preview {
                expected_revision: 1,
                source: source("<script>alert('no')</script>")?,
            })
            .map_err(|error| std::io::Error::other(error.message))?;
        let LiveDocumentResponse::Preview(preview) = response else {
            return Err("unexpected live document response".into());
        };

        assert!(!preview.applied);
        assert_eq!(preview.document.revision, 1);
        assert!(!preview.diagnostics.is_empty());
        assert!(preview.document.source.html.contains("Save"));
        Ok(())
    }
}
