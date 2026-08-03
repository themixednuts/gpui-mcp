use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use directories::BaseDirs;
use gpui::{App, Window};
use gpui_mcp_protocol::{
    ActionOutcome, ApplicationCommandDescriptor, ApplicationCommandResult, BridgeError,
    BridgeResult, Capabilities, Capability, ContextResource, ContextResourceDescriptor,
    EndpointDescriptor, ErrorCode, Highlight, LiveDocument, LiveDocumentPreview,
    LiveDocumentSource, LocalEndpoint, MAX_APPLICATION_COMMAND_OUTPUT_BYTES,
    MAX_APPLICATION_COMMAND_SCHEMA_BYTES, MAX_APPLICATION_COMMANDS, MAX_CONTEXT_RESOURCE_BYTES,
    MAX_CONTEXT_RESOURCE_URI_BYTES, MAX_CONTEXT_RESOURCES, MAX_ID_BYTES, MAX_LABEL_BYTES,
    MAX_LIVE_DOCUMENT_DIAGNOSTICS, MAX_LIVE_DOCUMENT_SOURCE_BYTES, MAX_REQUEST_BYTES,
    MAX_RESPONSE_BYTES, MAX_WAIT_MS, Operation, PROTOCOL_VERSION, ScreenshotTarget, WireRequest,
    WireResponse,
};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, Name, ToFsName as _, ToNsName as _,
    tokio::{Listener as LocalSocketListener, Stream as LocalSocketStream, prelude::*},
};
use rand::RngCore as _;
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::Automation;
use crate::capture;
use crate::input;
use crate::registry::SharedState;

const MAX_CONNECTIONS: usize = 8;
const COMMAND_CAPACITY: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_mins(30);

/// Configuration for one GPUI window bridge.
#[derive(Clone, Debug)]
pub struct BridgeConfig {
    app_id: String,
    app_name: String,
    window_title: String,
    endpoint_dir: Option<PathBuf>,
    operation_timeout: Duration,
    live_document: bool,
    context_resources: bool,
    application_commands: bool,
}

impl BridgeConfig {
    /// Create configuration with a developer-chosen stable application ID.
    ///
    /// `app_id` is declared once in Rust and discovered automatically by the
    /// MCP server; users do not repeat it in MCP client configuration. Multiple
    /// running bridges with the same logical ID receive independent automatic
    /// instance IDs. The native window title defaults to `app_name`; use
    /// [`Self::window_title`] when they differ.
    #[must_use]
    pub fn new(app_id: impl Into<String>, app_name: impl Into<String>) -> Self {
        let app_name = app_name.into();
        Self {
            app_id: app_id.into(),
            window_title: app_name.clone(),
            app_name,
            endpoint_dir: None,
            operation_timeout: Duration::from_secs(5),
            live_document: false,
            context_resources: false,
            application_commands: false,
        }
    }

    /// Override the exact native title used to match this window for capture.
    #[must_use]
    pub fn window_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = title.into();
        self
    }

    /// Override the private endpoint descriptor directory.
    #[must_use]
    pub fn endpoint_dir(mut self, directory: impl Into<PathBuf>) -> Self {
        self.endpoint_dir = Some(directory.into());
        self
    }

    /// Override the UI operation deadline. Values are clamped to 100 ms through 30 seconds.
    #[must_use]
    pub fn operation_timeout(mut self, duration: Duration) -> Self {
        self.operation_timeout =
            duration.clamp(Duration::from_millis(100), Duration::from_secs(30));
        self
    }

    /// Opt into revisioned in-memory live-document operations for this endpoint.
    ///
    /// A document handler must also be registered on the installed [`BridgeHandle`].
    #[must_use]
    pub const fn enable_live_document(mut self) -> Self {
        self.live_document = true;
        self
    }

    /// Opt into bounded application-owned MCP context resources for this endpoint.
    ///
    /// A resource handler must also be registered on the installed [`BridgeHandle`].
    #[must_use]
    pub const fn enable_context_resources(mut self) -> Self {
        self.context_resources = true;
        self
    }

    /// Opt into bounded application-owned structured commands for this endpoint.
    ///
    /// A command handler must also be registered on the installed [`BridgeHandle`].
    #[must_use]
    pub const fn enable_application_commands(mut self) -> Self {
        self.application_commands = true;
        self
    }
}

/// App-side request delivered to an opt-in live-document handler on the GPUI thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveDocumentRequest {
    /// Return the active document without changing it.
    Get,
    /// Compile and preview a complete candidate against one base revision.
    Preview {
        /// Active revision the caller based its edit on.
        expected_revision: u64,
        /// Complete candidate source bundle.
        source: LiveDocumentSource,
    },
}

type LiveDocumentHandler = Rc<
    dyn Fn(LiveDocumentRequest, &mut Window, &mut App) -> Result<LiveDocumentResponse, BridgeError>,
>;

/// Typed result returned by an application live-document handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveDocumentResponse {
    /// Response to [`LiveDocumentRequest::Get`].
    Document(LiveDocument),
    /// Response to [`LiveDocumentRequest::Preview`].
    Preview(LiveDocumentPreview),
}

struct LiveDocumentHost {
    enabled: bool,
    handler: RefCell<Option<LiveDocumentHandler>>,
}

impl LiveDocumentHost {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            handler: RefCell::new(None),
        }
    }

    fn handle(
        &self,
        request: LiveDocumentRequest,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<LiveDocumentResponse, BridgeError> {
        if !self.enabled {
            return Err(BridgeError::new(
                ErrorCode::Unsupported,
                "live document preview is not enabled for this application",
            ));
        }
        let handler = self.handler.borrow().clone().ok_or_else(|| {
            BridgeError::new(
                ErrorCode::Unsupported,
                "live document host is not registered",
            )
        })?;
        handler(request, window, cx)
    }
}

/// Failure to register an application live-document host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LiveDocumentHostError {
    /// The bridge was not configured to expose document operations.
    #[error("live document preview is not enabled in BridgeConfig")]
    Disabled,
    /// One handler is already registered for this window.
    #[error("a live document handler is already registered")]
    AlreadyRegistered,
}

/// App-side request delivered to an opt-in context-resource handler on the GPUI thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextResourceRequest {
    /// List discoverable resource metadata without reading every resource body.
    List,
    /// Resolve the current contents of one exact resource URI.
    Read {
        /// Exact URI previously returned by [`ContextResourceRequest::List`].
        uri: String,
    },
}

/// Typed result returned by an application context-resource handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextResourceResponse {
    /// Response to [`ContextResourceRequest::List`].
    List(Vec<ContextResourceDescriptor>),
    /// Response to [`ContextResourceRequest::Read`].
    Resource(ContextResource),
}

type ContextResourceHandler = Rc<
    dyn Fn(
        ContextResourceRequest,
        &mut Window,
        &mut App,
    ) -> Result<ContextResourceResponse, BridgeError>,
>;

struct ContextResourceHost {
    enabled: bool,
    handler: RefCell<Option<ContextResourceHandler>>,
}

impl ContextResourceHost {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            handler: RefCell::new(None),
        }
    }

    fn handle(
        &self,
        request: ContextResourceRequest,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<ContextResourceResponse, BridgeError> {
        if !self.enabled {
            return Err(BridgeError::new(
                ErrorCode::Unsupported,
                "context resources are not enabled for this application",
            ));
        }
        let handler = self.handler.borrow().clone().ok_or_else(|| {
            BridgeError::new(
                ErrorCode::Unsupported,
                "context resource host is not registered",
            )
        })?;
        handler(request, window, cx)
    }
}

/// Failure to register an application context-resource host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ContextResourceHostError {
    /// The bridge was not configured to expose context resources.
    #[error("context resources are not enabled in BridgeConfig")]
    Disabled,
    /// One handler is already registered for this window.
    #[error("a context resource handler is already registered")]
    AlreadyRegistered,
}

/// App-side request delivered to an opt-in structured command handler on the GPUI thread.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplicationCommandRequest {
    /// List discoverable command metadata.
    List,
    /// Execute one exact command with JSON arguments.
    Execute {
        /// Name previously returned by [`ApplicationCommandRequest::List`].
        name: String,
        /// Bounded JSON arguments.
        arguments: serde_json::Value,
    },
}

/// Typed result returned by an application command handler.
#[derive(Clone, Debug, PartialEq)]
pub enum ApplicationCommandResponse {
    /// Response to [`ApplicationCommandRequest::List`].
    List(Vec<ApplicationCommandDescriptor>),
    /// Response to [`ApplicationCommandRequest::Execute`].
    Result(ApplicationCommandResult),
}

type ApplicationCommandHandler = Rc<
    dyn Fn(
        ApplicationCommandRequest,
        &mut Window,
        &mut App,
    ) -> Result<ApplicationCommandResponse, BridgeError>,
>;

struct ApplicationCommandHost {
    enabled: bool,
    handler: RefCell<Option<ApplicationCommandHandler>>,
}

impl ApplicationCommandHost {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            handler: RefCell::new(None),
        }
    }

    fn handle(
        &self,
        request: ApplicationCommandRequest,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<ApplicationCommandResponse, BridgeError> {
        if !self.enabled {
            return Err(BridgeError::new(
                ErrorCode::Unsupported,
                "application commands are not enabled for this application",
            ));
        }
        let handler = self.handler.borrow().clone().ok_or_else(|| {
            BridgeError::new(
                ErrorCode::Unsupported,
                "application command host is not registered",
            )
        })?;
        handler(request, window, cx)
    }
}

/// Failure to register an application command host.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ApplicationCommandHostError {
    /// The bridge was not configured to expose application commands.
    #[error("application commands are not enabled in BridgeConfig")]
    Disabled,
    /// One handler is already registered for this window.
    #[error("an application command handler is already registered")]
    AlreadyRegistered,
}

/// Failure while installing the local bridge.
#[derive(Debug, Error)]
pub enum StartError {
    /// Configuration did not meet hardening constraints.
    #[error("invalid bridge configuration: {0}")]
    InvalidConfig(&'static str),
    /// Local IPC or endpoint-file setup failed.
    #[error("could not initialize local bridge: {0}")]
    Io(#[from] io::Error),
    /// Endpoint serialization failed.
    #[error("could not serialize endpoint descriptor: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Owned service lifetime. Dropping it stops the listener and removes its descriptor.
pub struct BridgeHandle {
    automation: Automation,
    endpoint_path: PathBuf,
    token: String,
    cancellation: CancellationToken,
    network_thread: Option<thread::JoinHandle<()>>,
    live_document_host: Rc<LiveDocumentHost>,
    context_resource_host: Rc<ContextResourceHost>,
    application_command_host: Rc<ApplicationCommandHost>,
}

impl BridgeHandle {
    /// Install automation for a GPUI window.
    ///
    /// The returned handle must be retained for as long as the window should be controllable.
    ///
    /// # Errors
    ///
    /// Returns [`StartError`] when configuration validation, the private endpoint
    /// directory, native local listener, descriptor serialization, or runtime setup fails.
    pub fn install(window: &Window, cx: &App, config: BridgeConfig) -> Result<Self, StartError> {
        validate_config(&config)?;

        let mut token_bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut token_bytes);
        let token = encode_hex(&token_bytes);
        let instance_id: u64 = rand::random();
        let state = SharedState::new(instance_id);
        let automation = Automation {
            state: state.clone(),
        };
        let pid = std::process::id();
        let live_document_host = Rc::new(LiveDocumentHost::new(config.live_document));
        let context_resource_host = Rc::new(ContextResourceHost::new(config.context_resources));
        let application_command_host =
            Rc::new(ApplicationCommandHost::new(config.application_commands));
        let endpoint_dir = config
            .endpoint_dir
            .clone()
            .unwrap_or_else(default_endpoint_dir);
        secure_directory(&endpoint_dir)?;
        let endpoint_dir = endpoint_dir.canonicalize()?;
        #[cfg(unix)]
        let owner_uid = endpoint_owner_uid(&endpoint_dir)?;
        #[cfg(windows)]
        let owner_uid = None;
        let endpoint_path =
            endpoint_dir.join(format!("{}-{pid}-{instance_id:016x}.json", config.app_id));
        let endpoint = make_local_endpoint(&endpoint_dir, &config.app_id, pid, instance_id);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("gpui-mcp-ipc")
            .enable_all()
            .build()?;
        let listener = {
            let _guard = runtime.enter();
            create_listener(&endpoint)?
        };

        let descriptor = EndpointDescriptor {
            protocol_version: PROTOCOL_VERSION,
            app_id: config.app_id.clone(),
            app_name: config.app_name.clone(),
            instance_id,
            pid,
            endpoint,
            token: token.clone(),
            window_title: config.window_title.clone(),
            capabilities: endpoint_capabilities(&config),
        };
        write_descriptor(&endpoint_path, &descriptor)?;

        let (command_tx, command_rx) = async_channel::bounded(COMMAND_CAPACITY);
        spawn_ui_pump(
            window,
            cx,
            command_rx,
            state.clone(),
            live_document_host.clone(),
            context_resource_host.clone(),
            application_command_host.clone(),
        );

        let cancellation = CancellationToken::new();
        let thread_cancellation = cancellation.clone();
        let listener_state = state;
        let listener_token = token.clone();
        let title = config.window_title;
        let operation_timeout = config.operation_timeout;
        let network_thread = thread::Builder::new()
            .name("gpui-mcp-listener".to_owned())
            .spawn(move || {
                runtime.block_on(run_listener(
                    listener,
                    command_tx,
                    listener_state,
                    listener_token,
                    pid,
                    title,
                    config.app_id,
                    operation_timeout,
                    owner_uid,
                    thread_cancellation,
                ));
            });
        let network_thread = match network_thread {
            Ok(handle) => handle,
            Err(error) => {
                let _ = fs::remove_file(&endpoint_path);
                return Err(StartError::Io(error));
            }
        };

        Ok(Self {
            automation,
            endpoint_path,
            token,
            cancellation,
            network_thread: Some(network_thread),
            live_document_host,
            context_resource_host,
            application_command_host,
        })
    }

    /// Clone the annotation and logging handle.
    #[must_use]
    pub fn automation(&self) -> Automation {
        self.automation.clone()
    }

    /// Return the descriptor path that MCP clients should use.
    #[must_use]
    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint_path
    }

    /// Stop the bridge and remove its endpoint descriptor.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.cancellation.cancel();
        if let Some(thread) = self.network_thread.take() {
            let _ = thread.join();
        }
        remove_owned_descriptor(&self.endpoint_path, &self.token);
    }

    /// Register the sole app-side live-document handler for this window.
    ///
    /// The callback always runs on GPUI's foreground executor. Registering does
    /// not grant filesystem access; source persistence remains an application decision.
    ///
    /// # Errors
    ///
    /// Returns an error when the capability was not enabled or a handler already exists.
    pub fn register_live_document_handler(
        &self,
        handler: impl Fn(
            LiveDocumentRequest,
            &mut Window,
            &mut App,
        ) -> Result<LiveDocumentResponse, BridgeError>
        + 'static,
    ) -> Result<(), LiveDocumentHostError> {
        if !self.live_document_host.enabled {
            return Err(LiveDocumentHostError::Disabled);
        }
        let mut slot = self.live_document_host.handler.borrow_mut();
        if slot.is_some() {
            return Err(LiveDocumentHostError::AlreadyRegistered);
        }
        *slot = Some(Rc::new(handler));
        Ok(())
    }

    /// Register the sole app-side context-resource handler for this window.
    ///
    /// The callback runs on GPUI's foreground executor. The bridge validates and
    /// bounds all returned metadata and text before it reaches an MCP client.
    ///
    /// # Errors
    ///
    /// Returns an error when the capability was not enabled or a handler already exists.
    pub fn register_context_resource_handler(
        &self,
        handler: impl Fn(
            ContextResourceRequest,
            &mut Window,
            &mut App,
        ) -> Result<ContextResourceResponse, BridgeError>
        + 'static,
    ) -> Result<(), ContextResourceHostError> {
        if !self.context_resource_host.enabled {
            return Err(ContextResourceHostError::Disabled);
        }
        let mut slot = self.context_resource_host.handler.borrow_mut();
        if slot.is_some() {
            return Err(ContextResourceHostError::AlreadyRegistered);
        }
        *slot = Some(Rc::new(handler));
        Ok(())
    }

    /// Register the sole app-side structured command handler for this window.
    ///
    /// The callback runs on GPUI's foreground executor. The bridge bounds descriptors,
    /// schemas, arguments, and results before they cross the authenticated local endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the capability was not enabled or a handler already exists.
    pub fn register_application_command_handler(
        &self,
        handler: impl Fn(
            ApplicationCommandRequest,
            &mut Window,
            &mut App,
        ) -> Result<ApplicationCommandResponse, BridgeError>
        + 'static,
    ) -> Result<(), ApplicationCommandHostError> {
        if !self.application_command_host.enabled {
            return Err(ApplicationCommandHostError::Disabled);
        }
        let mut slot = self.application_command_host.handler.borrow_mut();
        if slot.is_some() {
            return Err(ApplicationCommandHostError::AlreadyRegistered);
        }
        *slot = Some(Rc::new(handler));
        Ok(())
    }
}

fn endpoint_capabilities(config: &BridgeConfig) -> Capabilities {
    Capabilities::new(
        [
            Capability::SemanticTree,
            Capability::Input,
            Capability::Highlight,
            Capability::Performance,
            Capability::Logs,
        ]
        .into_iter()
        .chain(cfg!(feature = "native-screenshot").then_some(Capability::Screenshot))
        .chain(config.live_document.then_some(Capability::LiveDocument))
        .chain(
            config
                .context_resources
                .then_some(Capability::ContextResources),
        )
        .chain(
            config
                .application_commands
                .then_some(Capability::ApplicationCommands),
        ),
    )
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

struct UiCommand {
    operation: Operation,
    response: oneshot::Sender<Result<BridgeResult, BridgeError>>,
}

fn spawn_ui_pump(
    window: &Window,
    cx: &App,
    receiver: Receiver<UiCommand>,
    state: Arc<SharedState>,
    live_document_host: Rc<LiveDocumentHost>,
    context_resource_host: Rc<ContextResourceHost>,
    application_command_host: Rc<ApplicationCommandHost>,
) {
    window
        .spawn(cx, async move |cx| {
            while let Ok(command) = receiver.recv().await {
                let result = cx
                    .update(|window, cx| {
                        handle_ui_operation(
                            command.operation,
                            &state,
                            &live_document_host,
                            &context_resource_host,
                            &application_command_host,
                            window,
                            cx,
                        )
                    })
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, "GPUI window rejected an automation update");
                        Err(BridgeError::new(
                            ErrorCode::Internal,
                            "application window is no longer available",
                        ))
                    });
                let _ = command.response.send(result);
            }
        })
        .detach();
}

#[allow(clippy::too_many_lines)]
fn handle_ui_operation(
    operation: Operation,
    state: &SharedState,
    live_document_host: &LiveDocumentHost,
    context_resource_host: &ContextResourceHost,
    application_command_host: &ApplicationCommandHost,
    window: &mut Window,
    cx: &mut App,
) -> Result<BridgeResult, BridgeError> {
    match operation {
        Operation::NativeInput { command } => {
            input::dispatch_native(&command, window, cx)?;
            // Each primitive native event completes through the same painted-frame contract as
            // semantic input. Compound gestures issue one operation per event, so callers can
            // deterministically settle a frame between drag moves without sleeping.
            window.refresh();
            Ok(BridgeResult::Ack)
        }
        Operation::Input { command } => {
            match &command {
                gpui_mcp_protocol::InputCommand::Key { .. }
                | gpui_mcp_protocol::InputCommand::TypeText { .. }
                | gpui_mcp_protocol::InputCommand::ReplaceText { .. } => {
                    input::dispatch_keyboard(command, window, cx)?;
                }
                _ => match state.dispatch_pointer(&command, window, cx)? {
                    ActionOutcome::Handled => {}
                    ActionOutcome::Rejected { reason } => {
                        return Err(BridgeError::new(ErrorCode::Rejected, reason));
                    }
                },
            }
            // Raw coordinate and keyboard input has the same completion contract as semantic
            // actions: callers may observe the fully painted result once the operation returns.
            window.refresh();
            Ok(BridgeResult::Ack)
        }
        Operation::Refresh => {
            let completed = state.frame_stats();
            window.refresh();
            Ok(BridgeResult::FrameStats(completed))
        }
        Operation::GetPointerLocation => Ok(BridgeResult::PointerLocation(
            input::pointer_location(window),
        )),
        Operation::ClearHighlights => {
            window.refresh();
            Ok(BridgeResult::Ack)
        }
        Operation::Invoke {
            node_id,
            expected_generation,
            action,
        } => {
            let outcome =
                state.dispatch_action(&node_id, expected_generation, &action, window, cx)?;
            if matches!(outcome, ActionOutcome::Handled) {
                // A successful semantic mutation is not complete for automation until GPUI has
                // been asked to publish the resulting frame. Application handlers may refresh
                // too; duplicate refresh requests are coalesced by GPUI.
                window.refresh();
            }
            Ok(BridgeResult::Action(outcome))
        }
        Operation::GetLiveDocument => {
            let response = live_document_host.handle(LiveDocumentRequest::Get, window, cx)?;
            let LiveDocumentResponse::Document(document) = response else {
                return Err(invalid_live_document_host_result());
            };
            validate_live_document_result(document).map(BridgeResult::LiveDocument)
        }
        Operation::PreviewLiveDocument {
            expected_revision,
            source,
        } => {
            let response = live_document_host.handle(
                LiveDocumentRequest::Preview {
                    expected_revision,
                    source,
                },
                window,
                cx,
            )?;
            let LiveDocumentResponse::Preview(preview) = response else {
                return Err(invalid_live_document_host_result());
            };
            let preview = validate_live_document_preview(preview)?;
            if preview.applied {
                window.refresh();
            }
            Ok(BridgeResult::LiveDocumentPreview(preview))
        }
        Operation::ListContextResources => {
            let response =
                context_resource_host.handle(ContextResourceRequest::List, window, cx)?;
            let ContextResourceResponse::List(resources) = response else {
                return Err(invalid_context_resource_host_result());
            };
            validate_context_resource_list(resources).map(BridgeResult::ContextResources)
        }
        Operation::ReadContextResource { uri } => {
            let response = context_resource_host.handle(
                ContextResourceRequest::Read { uri: uri.clone() },
                window,
                cx,
            )?;
            let ContextResourceResponse::Resource(resource) = response else {
                return Err(invalid_context_resource_host_result());
            };
            if resource.descriptor.uri != uri {
                return Err(invalid_context_resource_host_result());
            }
            validate_context_resource(resource).map(BridgeResult::ContextResource)
        }
        Operation::ListApplicationCommands => {
            let response =
                application_command_host.handle(ApplicationCommandRequest::List, window, cx)?;
            let ApplicationCommandResponse::List(commands) = response else {
                return Err(invalid_application_command_host_result());
            };
            validate_application_command_list(commands).map(BridgeResult::ApplicationCommands)
        }
        Operation::ExecuteApplicationCommand { name, arguments } => {
            let response = application_command_host.handle(
                ApplicationCommandRequest::Execute {
                    name: name.clone(),
                    arguments,
                },
                window,
                cx,
            )?;
            let ApplicationCommandResponse::Result(result) = response else {
                return Err(invalid_application_command_host_result());
            };
            if result.name != name {
                return Err(invalid_application_command_host_result());
            }
            let result = validate_application_command_result(result)?;
            window.refresh();
            Ok(BridgeResult::ApplicationCommand(result))
        }
        _ => Err(BridgeError::new(
            ErrorCode::Internal,
            "operation was routed to the wrong executor",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_listener(
    listener: LocalSocketListener,
    command_tx: Sender<UiCommand>,
    state: Arc<SharedState>,
    token: String,
    pid: u32,
    window_title: String,
    app_id: String,
    operation_timeout: Duration,
    owner_uid: Option<u32>,
    cancellation: CancellationToken,
) {
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            () = cancellation.cancelled() => break,
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(%error, "local IPC accept failed");
                        continue;
                    }
                };
                if let Err(error) = verify_peer(&stream, owner_uid) {
                    tracing::warn!(%error, "rejected a local IPC peer");
                    continue;
                }
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    tracing::warn!(max_connections = MAX_CONNECTIONS, "bridge connection limit reached");
                    continue;
                };
                let context = ConnectionContext {
                    command_tx: command_tx.clone(),
                    state: state.clone(),
                    token: token.clone(),
                    pid,
                    window_title: window_title.clone(),
                    app_id: app_id.clone(),
                    operation_timeout,
                };
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_connection(stream, context).await {
                        tracing::debug!(%error, "bridge connection closed with an I/O error");
                    }
                });
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::debug!(%error, "bridge connection task failed");
                }
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

#[derive(Clone)]
struct ConnectionContext {
    command_tx: Sender<UiCommand>,
    state: Arc<SharedState>,
    token: String,
    pid: u32,
    window_title: String,
    app_id: String,
    operation_timeout: Duration,
}

async fn handle_connection(
    mut stream: LocalSocketStream,
    context: ConnectionContext,
) -> Result<(), io::Error> {
    let mut first_request = true;
    loop {
        let read_timeout = if first_request {
            IO_TIMEOUT
        } else {
            CONNECTION_IDLE_TIMEOUT
        };
        let Some(request) = read_request(&mut stream, read_timeout).await? else {
            return Ok(());
        };
        first_request = false;
        let response = process_request(request, context.clone()).await;
        let close_after_response = response.error.as_ref().is_some_and(|error| {
            matches!(
                error.code,
                ErrorCode::Unauthorized | ErrorCode::ProtocolMismatch
            )
        });
        write_response(&mut stream, &response).await?;
        if close_after_response {
            return Ok(());
        }
    }
}

async fn read_request(
    stream: &mut LocalSocketStream,
    read_timeout: Duration,
) -> io::Result<Option<WireRequest>> {
    let mut length_bytes = [0_u8; 4];
    match timeout(read_timeout, stream.read_exact(&mut length_bytes)).await {
        Err(_) => return Ok(None),
        Ok(Err(error))
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
            ) =>
        {
            return Ok(None);
        }
        Ok(Err(error)) => return Err(error),
        Ok(Ok(_)) => {}
    }
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > MAX_REQUEST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request frame size is invalid",
        ));
    }
    let mut payload = vec![0_u8; length];
    timeout(IO_TIMEOUT, stream.read_exact(&mut payload))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request read timed out"))??;
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request JSON is invalid"))
}

async fn process_request(request: WireRequest, context: ConnectionContext) -> WireResponse {
    if request.protocol_version != PROTOCOL_VERSION {
        return WireResponse::failure(
            request.request_id,
            BridgeError::new(
                ErrorCode::ProtocolMismatch,
                format!("protocol version {PROTOCOL_VERSION} is required"),
            ),
        );
    }
    if request.token.len() != context.token.len()
        || !bool::from(request.token.as_bytes().ct_eq(context.token.as_bytes()))
    {
        return WireResponse::failure(
            request.request_id,
            BridgeError::new(ErrorCode::Unauthorized, "endpoint authentication failed"),
        );
    }
    if let Err(error) = validate_operation(&request.operation) {
        return WireResponse::failure(request.request_id, error);
    }

    let result = match request.operation {
        Operation::Ping => Ok(BridgeResult::Pong {
            app_id: context.app_id,
            pid: context.pid,
            protocol_version: PROTOCOL_VERSION,
        }),
        Operation::GetTree => Ok(BridgeResult::Tree(context.state.tree())),
        Operation::WaitForTree {
            after_generation,
            timeout_ms,
        } => context
            .state
            .wait_for_tree(after_generation, Duration::from_millis(timeout_ms))
            .await
            .map(BridgeResult::Tree),
        Operation::WaitForFrame {
            after_frame_count,
            timeout_ms,
        } => context
            .state
            .wait_for_frame(after_frame_count, Duration::from_millis(timeout_ms))
            .await
            .map(BridgeResult::FrameStats),
        Operation::Screenshot { target } => capture_after_refresh(target, &context).await,
        Operation::SetHighlights { highlights } => {
            context.state.set_highlights(highlights);
            request_ui_refresh(&context.command_tx, context.operation_timeout).await
        }
        Operation::ClearHighlights => {
            context.state.set_highlights(Vec::new());
            request_ui_refresh(&context.command_tx, context.operation_timeout).await
        }
        Operation::GetFrameStats => Ok(BridgeResult::FrameStats(context.state.frame_stats())),
        Operation::GetPointerLocation => {
            dispatch_to_ui(
                Operation::GetPointerLocation,
                &context.command_tx,
                context.operation_timeout,
            )
            .await
        }
        Operation::GetLogs { limit, min_level } => Ok(BridgeResult::Logs(
            context.state.logs(limit, min_level.as_deref()),
        )),
        Operation::ClearLogs => {
            context.state.clear_logs();
            Ok(BridgeResult::Ack)
        }
        operation @ (Operation::Input { .. }
        | Operation::NativeInput { .. }
        | Operation::Invoke { .. }
        | Operation::Refresh
        | Operation::GetLiveDocument
        | Operation::PreviewLiveDocument { .. }
        | Operation::ListContextResources
        | Operation::ReadContextResource { .. }
        | Operation::ListApplicationCommands
        | Operation::ExecuteApplicationCommand { .. }) => {
            dispatch_to_ui(operation, &context.command_tx, context.operation_timeout).await
        }
    };
    match result {
        Ok(result) => WireResponse::success(request.request_id, result),
        Err(error) => WireResponse::failure(request.request_id, error),
    }
}

async fn capture_after_refresh(
    target: ScreenshotTarget,
    context: &ConnectionContext,
) -> Result<BridgeResult, BridgeError> {
    // A mutation can commit the changed GPUI surface before unchanged shell surfaces are
    // re-presented to the native compositor. Require two completed root paints before capture:
    // the first flushes the mutation and the second presents one coherent complete window.
    for _ in 0..2 {
        let BridgeResult::FrameStats(before_refresh) = dispatch_to_ui(
            Operation::Refresh,
            &context.command_tx,
            context.operation_timeout,
        )
        .await?
        else {
            return Err(BridgeError::new(
                ErrorCode::Internal,
                "refresh returned the wrong completed-frame token",
            ));
        };
        context
            .state
            .wait_for_frame(before_refresh.frame_count, context.operation_timeout)
            .await?;
    }

    let state = context.state.clone();
    let title = context.window_title.clone();
    let pid = context.pid;
    match timeout(
        context.operation_timeout,
        tokio::task::spawn_blocking(move || {
            capture::capture(pid, &title, target, &state).map(BridgeResult::Screenshot)
        }),
    )
    .await
    {
        Err(_) => Err(BridgeError::new(
            ErrorCode::Timeout,
            "screenshot worker timed out",
        )),
        Ok(Err(error)) => {
            tracing::warn!(%error, "screenshot worker failed");
            Err(BridgeError::new(
                ErrorCode::Internal,
                "screenshot worker failed",
            ))
        }
        Ok(Ok(result)) => result,
    }
}

async fn request_ui_refresh(
    command_tx: &Sender<UiCommand>,
    operation_timeout: Duration,
) -> Result<BridgeResult, BridgeError> {
    let (response_tx, response_rx) = oneshot::channel();
    command_tx
        .try_send(UiCommand {
            operation: Operation::ClearHighlights,
            response: response_tx,
        })
        .map_err(|_| BridgeError::new(ErrorCode::Busy, "UI command queue is full"))?;
    timeout(operation_timeout, response_rx)
        .await
        .map_err(|_| BridgeError::new(ErrorCode::Timeout, "UI operation timed out"))?
        .map_err(|_| BridgeError::new(ErrorCode::Internal, "UI command pump stopped"))?
}

async fn dispatch_to_ui(
    operation: Operation,
    command_tx: &Sender<UiCommand>,
    operation_timeout: Duration,
) -> Result<BridgeResult, BridgeError> {
    let (response_tx, response_rx) = oneshot::channel();
    command_tx
        .try_send(UiCommand {
            operation,
            response: response_tx,
        })
        .map_err(|_| BridgeError::new(ErrorCode::Busy, "UI command queue is full"))?;
    timeout(operation_timeout, response_rx)
        .await
        .map_err(|_| BridgeError::new(ErrorCode::Timeout, "UI operation timed out"))?
        .map_err(|_| BridgeError::new(ErrorCode::Internal, "UI command pump stopped"))?
}

fn validate_operation(operation: &Operation) -> Result<(), BridgeError> {
    match operation {
        Operation::Input { command } => input::validate(command),
        Operation::NativeInput { command } => input::validate_native(command),
        Operation::Invoke {
            node_id, action, ..
        } => {
            if node_id.is_empty()
                || node_id.len() > MAX_ID_BYTES
                || node_id.chars().any(char::is_control)
            {
                return Err(invalid("semantic node identifier is invalid"));
            }
            input::validate_semantic(action)
        }
        Operation::WaitForTree { timeout_ms, .. }
            if *timeout_ms == 0 || *timeout_ms > MAX_WAIT_MS =>
        {
            Err(invalid("tree wait timeout must be between 1 and 30000 ms"))
        }
        Operation::WaitForFrame { timeout_ms, .. }
            if *timeout_ms == 0 || *timeout_ms > MAX_WAIT_MS =>
        {
            Err(invalid("frame wait timeout must be between 1 and 30000 ms"))
        }
        Operation::SetHighlights { highlights } => validate_highlights(highlights),
        Operation::GetLogs { limit, min_level } => {
            if *limit > 512 {
                return Err(invalid("log limit cannot exceed 512"));
            }
            if let Some(level) = min_level
                && !matches!(
                    level.as_str(),
                    "trace" | "debug" | "info" | "warn" | "error"
                )
            {
                return Err(invalid("minimum log level is invalid"));
            }
            Ok(())
        }
        Operation::Screenshot {
            target: gpui_mcp_protocol::ScreenshotTarget::Region { rect },
        } if !rect.is_valid() => Err(invalid("screenshot rectangle is invalid")),
        Operation::PreviewLiveDocument {
            expected_revision,
            source,
        } => {
            if *expected_revision == 0 {
                return Err(invalid("expected live document revision must be nonzero"));
            }
            validate_live_document_source(source)
        }
        Operation::ReadContextResource { uri } => validate_context_resource_uri(uri),
        Operation::ExecuteApplicationCommand { name, arguments } => {
            validate_application_command_name(name)?;
            if serde_json::to_vec(arguments).map_or(true, |value| {
                value.len() > MAX_APPLICATION_COMMAND_OUTPUT_BYTES
            }) {
                return Err(invalid(
                    "application command arguments are invalid or too large",
                ));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_application_command_name(name: &str) -> Result<(), BridgeError> {
    if name.is_empty()
        || name.len() > 128
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
    {
        return Err(invalid("application command name is invalid"));
    }
    Ok(())
}

fn validate_context_resource_uri(uri: &str) -> Result<(), BridgeError> {
    if uri.is_empty()
        || uri.len() > MAX_CONTEXT_RESOURCE_URI_BYTES
        || uri.chars().any(char::is_control)
        || !uri.contains("://")
    {
        return Err(invalid("context resource URI is invalid"));
    }
    Ok(())
}

fn validate_context_resource_descriptor(
    descriptor: &ContextResourceDescriptor,
) -> Result<(), BridgeError> {
    validate_context_resource_uri(&descriptor.uri)?;
    if descriptor.name.is_empty()
        || descriptor.name.len() > MAX_LABEL_BYTES
        || descriptor.name.chars().any(char::is_control)
        || descriptor.title.as_ref().is_some_and(|value| {
            value.len() > MAX_LABEL_BYTES || value.chars().any(char::is_control)
        })
        || descriptor.description.as_ref().is_some_and(|value| {
            value.len() > MAX_LABEL_BYTES || value.chars().any(char::is_control)
        })
        || descriptor.mime_type.is_empty()
        || descriptor.mime_type.len() > 128
        || descriptor.mime_type.chars().any(char::is_control)
        || descriptor
            .size
            .is_some_and(|size| size > MAX_CONTEXT_RESOURCE_BYTES as u64)
    {
        return Err(invalid_context_resource_host_result());
    }
    Ok(())
}

fn validate_context_resource_list(
    resources: Vec<ContextResourceDescriptor>,
) -> Result<Vec<ContextResourceDescriptor>, BridgeError> {
    if resources.len() > MAX_CONTEXT_RESOURCES {
        return Err(invalid_context_resource_host_result());
    }
    let mut uris = std::collections::BTreeSet::new();
    for descriptor in &resources {
        validate_context_resource_descriptor(descriptor)?;
        if !uris.insert(descriptor.uri.as_str()) {
            return Err(invalid_context_resource_host_result());
        }
    }
    Ok(resources)
}

fn validate_context_resource(resource: ContextResource) -> Result<ContextResource, BridgeError> {
    validate_context_resource_descriptor(&resource.descriptor)?;
    if resource.text.len() > MAX_CONTEXT_RESOURCE_BYTES
        || resource
            .descriptor
            .size
            .is_some_and(|size| size != resource.text.len() as u64)
    {
        return Err(invalid_context_resource_host_result());
    }
    Ok(resource)
}

fn invalid_context_resource_host_result() -> BridgeError {
    BridgeError::new(
        ErrorCode::Internal,
        "context resource host returned an invalid result",
    )
}

fn validate_application_command_list(
    commands: Vec<ApplicationCommandDescriptor>,
) -> Result<Vec<ApplicationCommandDescriptor>, BridgeError> {
    if commands.len() > MAX_APPLICATION_COMMANDS {
        return Err(invalid_application_command_host_result());
    }
    let mut names = std::collections::BTreeSet::new();
    for command in &commands {
        validate_application_command_name(&command.name)
            .map_err(|_| invalid_application_command_host_result())?;
        let schema_size = serde_json::to_vec(&command.input_schema)
            .map_err(|_| invalid_application_command_host_result())?
            .len();
        if command.title.is_empty()
            || command.title.len() > MAX_LABEL_BYTES
            || command.description.len() > MAX_LABEL_BYTES
            || !command.input_schema.is_object()
            || schema_size > MAX_APPLICATION_COMMAND_SCHEMA_BYTES
            || !names.insert(command.name.as_str())
        {
            return Err(invalid_application_command_host_result());
        }
    }
    Ok(commands)
}

fn validate_application_command_result(
    result: ApplicationCommandResult,
) -> Result<ApplicationCommandResult, BridgeError> {
    validate_application_command_name(&result.name)
        .map_err(|_| invalid_application_command_host_result())?;
    if serde_json::to_vec(&result.output).map_or(true, |value| {
        value.len() > MAX_APPLICATION_COMMAND_OUTPUT_BYTES
    }) {
        return Err(invalid_application_command_host_result());
    }
    Ok(result)
}

fn invalid_application_command_host_result() -> BridgeError {
    BridgeError::new(
        ErrorCode::Internal,
        "application command host returned an invalid result",
    )
}

fn validate_live_document_source(source: &LiveDocumentSource) -> Result<(), BridgeError> {
    if source.byte_len() > MAX_LIVE_DOCUMENT_SOURCE_BYTES {
        return Err(invalid("live document source exceeds 768 KiB"));
    }
    if source.html.is_empty() {
        return Err(invalid("live document HTML cannot be empty"));
    }
    Ok(())
}

fn validate_live_document_result(document: LiveDocument) -> Result<LiveDocument, BridgeError> {
    if document.revision == 0
        || validate_live_document_source(&document.source).is_err()
        || document.diagnostics.len() > MAX_LIVE_DOCUMENT_DIAGNOSTICS
        || document.diagnostics.iter().any(|diagnostic| {
            !matches!(diagnostic.severity.as_str(), "warning" | "error")
                || diagnostic.message.len() > MAX_LABEL_BYTES
        })
    {
        return Err(invalid_live_document_host_result());
    }
    Ok(document)
}

fn validate_live_document_preview(
    mut preview: LiveDocumentPreview,
) -> Result<LiveDocumentPreview, BridgeError> {
    preview.document = validate_live_document_result(preview.document)?;
    if preview.diagnostics.len() > MAX_LIVE_DOCUMENT_DIAGNOSTICS
        || preview.diagnostics.iter().any(|diagnostic| {
            !matches!(diagnostic.severity.as_str(), "warning" | "error")
                || diagnostic.message.len() > MAX_LABEL_BYTES
        })
    {
        return Err(invalid_live_document_host_result());
    }
    Ok(preview)
}

fn invalid_live_document_host_result() -> BridgeError {
    BridgeError::new(
        ErrorCode::Internal,
        "live document host returned an invalid result",
    )
}

fn validate_highlights(highlights: &[Highlight]) -> Result<(), BridgeError> {
    if highlights.len() > 64 {
        return Err(invalid("no more than 64 highlights are allowed"));
    }
    for highlight in highlights {
        if !highlight.rect.is_valid() {
            return Err(invalid("highlight rectangle is invalid"));
        }
        let color = highlight.color.strip_prefix('#').unwrap_or_default();
        if color.len() != 8 || !color.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid("highlight color must use #RRGGBBAA"));
        }
        if highlight
            .label
            .as_ref()
            .is_some_and(|label| label.len() > 128)
        {
            return Err(invalid("highlight label exceeds 128 bytes"));
        }
    }
    Ok(())
}

async fn write_response(
    stream: &mut LocalSocketStream,
    response: &WireResponse,
) -> Result<(), io::Error> {
    let mut payload = serde_json::to_vec(response).map_err(io::Error::other)?;
    if payload.len() > MAX_RESPONSE_BYTES {
        payload = serde_json::to_vec(&WireResponse::failure(
            response.request_id,
            BridgeError::new(ErrorCode::Internal, "response exceeded the safety bound"),
        ))
        .map_err(io::Error::other)?;
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response is too large"))?;
    timeout(IO_TIMEOUT, async {
        stream.write_all(&length.to_be_bytes()).await?;
        stream.write_all(&payload).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response write timed out"))?
}

fn make_local_endpoint(
    endpoint_dir: &Path,
    app_id: &str,
    pid: u32,
    instance_id: u64,
) -> LocalEndpoint {
    #[cfg(unix)]
    {
        let _ = (app_id, pid);
        LocalEndpoint::Filesystem {
            path: endpoint_dir.join(format!("mcp-{instance_id:016x}.sock")),
        }
    }
    #[cfg(windows)]
    {
        let _ = endpoint_dir;
        LocalEndpoint::Namespaced {
            name: format!("gpui-mcp-{app_id}-{pid}-{instance_id:016x}"),
        }
    }
}

fn endpoint_name(endpoint: &LocalEndpoint) -> io::Result<Name<'_>> {
    match endpoint {
        LocalEndpoint::Filesystem { path } => path.as_path().to_fs_name::<GenericFilePath>(),
        LocalEndpoint::Namespaced { name } => name.as_str().to_ns_name::<GenericNamespaced>(),
    }
}

fn create_listener(endpoint: &LocalEndpoint) -> io::Result<LocalSocketListener> {
    let options = ListenerOptions::new()
        .name(endpoint_name(endpoint)?)
        .reclaim_name(true)
        .try_overwrite(false)
        .max_spin_time(Duration::from_millis(100));

    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))]
    let options = {
        use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
        options.mode(0o600)
    };

    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
        options.security_descriptor(owner_only_security_descriptor()?)
    };

    let listener = options.create_tokio()?;
    #[cfg(all(
        unix,
        not(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))
    ))]
    secure_socket_permissions(endpoint)?;
    Ok(listener)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))
))]
fn secure_socket_permissions(endpoint: &LocalEndpoint) -> io::Result<()> {
    if let LocalEndpoint::Filesystem { path } = endpoint {
        use std::os::unix::fs::PermissionsExt as _;
        // The containing directory is already 0700, so post-bind chmod has no cross-user race.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(windows)]
fn owner_only_security_descriptor()
-> io::Result<interprocess::os::windows::security_descriptor::SecurityDescriptor> {
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;

    // Protected DACL: the pipe owner and LocalSystem receive full access. No inherited ACEs.
    let sddl = U16CString::from_str("D:P(A;;GA;;;OW)(A;;GA;;;SY)")
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    SecurityDescriptor::deserialize(&sddl)
}

fn verify_peer(stream: &LocalSocketStream, owner_uid: Option<u32>) -> io::Result<()> {
    let credentials = stream.peer_creds()?;
    let _ = owner_uid;
    #[cfg(unix)]
    {
        if credentials.euid() != owner_uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "peer effective user does not own the bridge",
            ));
        }
    }
    #[cfg(windows)]
    let _ = credentials.pid();
    Ok(())
}

#[cfg(unix)]
fn endpoint_owner_uid(path: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::MetadataExt as _;
    fs::metadata(path).map(|metadata| Some(metadata.uid()))
}

fn validate_config(config: &BridgeConfig) -> Result<(), StartError> {
    if config.app_id.is_empty()
        || config.app_id.len() > 64
        || !config
            .app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(StartError::InvalidConfig(
            "app_id must contain 1-64 ASCII letters, digits, '.', '_' or '-'",
        ));
    }
    if invalid_display_text(&config.app_name) || invalid_display_text(&config.window_title) {
        return Err(StartError::InvalidConfig(
            "app name and window title must be 1-256 bytes without control characters",
        ));
    }
    if config
        .endpoint_dir
        .as_ref()
        .is_some_and(|directory| !directory.is_absolute())
    {
        return Err(StartError::InvalidConfig(
            "custom endpoint directory must be absolute",
        ));
    }
    Ok(())
}

fn invalid_display_text(value: &str) -> bool {
    value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
}

fn default_endpoint_dir() -> PathBuf {
    if let Some(base) = BaseDirs::new() {
        return base
            .runtime_dir()
            .unwrap_or_else(|| base.data_local_dir())
            .join("gpui-mcp")
            .join("run");
    }
    std::env::temp_dir().join("gpui-mcp").join("run")
}

fn secure_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_descriptor(path: &Path, descriptor: &EndpointDescriptor) -> Result<(), StartError> {
    let payload = serde_json::to_vec_pretty(descriptor)?;
    let temporary = path.with_extension(format!("{}.tmp", descriptor.pid));
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&payload)?;
    file.sync_all()?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(StartError::Io(error));
    }
    Ok(())
}

fn remove_owned_descriptor(path: &Path, token: &str) {
    let owned = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<EndpointDescriptor>(&bytes).ok())
        .is_some_and(|descriptor| descriptor.token == token);
    if owned {
        let _ = fs::remove_file(path);
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn invalid(message: &'static str) -> BridgeError {
    BridgeError::new(ErrorCode::InvalidRequest, message)
}

#[cfg(test)]
mod tests {
    use gpui_mcp_protocol::{ContextResource, ContextResourceDescriptor};

    use super::{
        BridgeConfig, encode_hex, validate_config, validate_context_resource,
        validate_context_resource_list,
    };

    #[test]
    fn application_identifier_blocks_path_traversal() {
        let config = BridgeConfig::new("../unsafe", "App");
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn application_name_is_the_default_window_title() {
        let config = BridgeConfig::new("app", "My App");
        assert_eq!(config.window_title, "My App");
    }

    #[test]
    fn window_title_can_be_overridden() {
        let config = BridgeConfig::new("app", "My App").window_title("My App — Settings");
        assert_eq!(config.app_name, "My App");
        assert_eq!(config.window_title, "My App — Settings");
    }

    #[test]
    fn token_encoding_is_fixed_width() {
        assert_eq!(encode_hex(&[0, 15, 16, 255]), "000f10ff");
    }

    #[test]
    fn context_resources_require_unique_uris_and_exact_sizes() {
        let descriptor = ContextResourceDescriptor {
            uri: "test-app://selection".to_owned(),
            name: "selection".to_owned(),
            title: Some("Selection".to_owned()),
            description: None,
            mime_type: "application/json".to_owned(),
            size: Some(2),
        };
        assert!(validate_context_resource_list(vec![descriptor.clone()]).is_ok());
        assert!(
            validate_context_resource_list(vec![descriptor.clone(), descriptor.clone()]).is_err()
        );
        assert!(
            validate_context_resource(ContextResource {
                descriptor: descriptor.clone(),
                text: "{}".to_owned(),
            })
            .is_ok()
        );
        assert!(
            validate_context_resource(ContextResource {
                descriptor,
                text: "oversized relative to descriptor".to_owned(),
            })
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_owner_only_pipe_descriptor_is_valid() {
        assert!(super::owner_only_security_descriptor().is_ok());
    }
}
