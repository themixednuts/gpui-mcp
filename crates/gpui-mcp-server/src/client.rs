use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use directories::BaseDirs;
use gpui_mcp_protocol::{
    BridgeResult, EndpointDescriptor, LocalEndpoint, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    Operation, PROTOCOL_VERSION, WireRequest, WireResponse,
};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, Name, ToFsName as _, ToNsName as _,
    tokio::{Stream as LocalSocketStream, prelude::*},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DESCRIPTOR_BYTES: u64 = 16 * 1024;
const MAX_IDLE_CONNECTIONS: usize = 4;

struct ConnectionPool {
    idle: Mutex<Vec<LocalSocketStream>>,
    permits: Semaphore,
}

impl ConnectionPool {
    fn new() -> Self {
        Self {
            idle: Mutex::new(Vec::with_capacity(MAX_IDLE_CONNECTIONS)),
            permits: Semaphore::new(MAX_IDLE_CONNECTIONS),
        }
    }

    async fn take(&self, endpoint: &LocalEndpoint) -> Result<LocalSocketStream, String> {
        if let Some(stream) = self.idle.lock().await.pop() {
            return Ok(stream);
        }
        connect(endpoint).await
    }

    async fn put(&self, stream: LocalSocketStream) {
        let mut idle = self.idle.lock().await;
        if idle.len() < MAX_IDLE_CONNECTIONS {
            idle.push(stream);
        }
    }
}

#[derive(Clone)]
pub(crate) struct BridgeClient {
    descriptor: Arc<EndpointDescriptor>,
    next_request_id: Arc<AtomicU64>,
    pool: Arc<ConnectionPool>,
}

impl BridgeClient {
    pub(crate) async fn discover(
        endpoint: Option<&Path>,
        app_id: Option<&str>,
        endpoint_dir: Option<&Path>,
    ) -> Result<Self> {
        let path = if let Some(endpoint) = endpoint {
            endpoint.to_path_buf()
        } else {
            discover_descriptor(app_id, endpoint_dir).await?
        };
        let descriptor = read_descriptor(&path)?;
        if let Some(expected) = app_id
            && descriptor.app_id != expected
        {
            bail!(
                "endpoint application id is {:?}, not {:?}",
                descriptor.app_id,
                expected
            );
        }
        validate_descriptor(&descriptor, &path)?;
        Ok(Self {
            descriptor: Arc::new(descriptor),
            next_request_id: Arc::new(AtomicU64::new(1)),
            pool: Arc::new(ConnectionPool::new()),
        })
    }

    pub(crate) fn descriptor(&self) -> &EndpointDescriptor {
        &self.descriptor
    }

    pub(crate) async fn call(&self, operation: Operation) -> Result<BridgeResult, String> {
        let response_timeout = match &operation {
            Operation::WaitForTree { timeout_ms, .. }
            | Operation::WaitForFrame { timeout_ms, .. } => {
                Duration::from_millis(*timeout_ms).saturating_add(IO_TIMEOUT)
            }
            _ => IO_TIMEOUT,
        };
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed).max(1);
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            token: self.descriptor.token.clone(),
            operation,
        };
        let payload = serde_json::to_vec(&request)
            .map_err(|_| "could not encode the bridge request".to_owned())?;
        if payload.len() > MAX_REQUEST_BYTES {
            return Err("request exceeds the 1 MiB safety bound".to_owned());
        }

        let _permit = self
            .pool
            .permits
            .acquire()
            .await
            .map_err(|_| "bridge connection pool stopped".to_owned())?;
        let mut stream = self.pool.take(&self.descriptor.endpoint).await?;
        let response = exchange(&mut stream, &payload, response_timeout).await;
        if response.is_ok() {
            self.pool.put(stream).await;
        }
        let response = response?;

        if response.protocol_version != PROTOCOL_VERSION {
            return Err("the bridge returned an incompatible protocol version".to_owned());
        }
        if response.request_id != request_id {
            return Err("the bridge returned a mismatched request id".to_owned());
        }
        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(format!("{:?}: {}", error.code, error.message)),
            _ => Err("the bridge response has an invalid outcome".to_owned()),
        }
    }
}

async fn exchange(
    stream: &mut LocalSocketStream,
    payload: &[u8],
    response_timeout: Duration,
) -> Result<WireResponse, String> {
    let length = u32::try_from(payload.len()).map_err(|_| "request exceeds the wire limit")?;
    timeout(IO_TIMEOUT, async {
        stream.write_all(&length.to_be_bytes()).await?;
        stream.write_all(payload).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| "timed out writing the bridge request".to_owned())?
    .map_err(|_| "could not write the bridge request".to_owned())?;

    let mut length_bytes = [0_u8; 4];
    timeout(response_timeout, stream.read_exact(&mut length_bytes))
        .await
        .map_err(|_| "timed out waiting for the GPUI application".to_owned())?
        .map_err(|_| "the GPUI application closed the connection".to_owned())?;
    let response_length = u32::from_be_bytes(length_bytes) as usize;
    if response_length == 0 || response_length > MAX_RESPONSE_BYTES {
        return Err("bridge response size is invalid".to_owned());
    }
    let mut response_payload = vec![0_u8; response_length];
    timeout(IO_TIMEOUT, stream.read_exact(&mut response_payload))
        .await
        .map_err(|_| "timed out reading the bridge response".to_owned())?
        .map_err(|_| "the bridge response was truncated".to_owned())?;
    serde_json::from_slice(&response_payload)
        .map_err(|_| "the bridge returned invalid JSON".to_owned())
}

async fn connect(endpoint: &LocalEndpoint) -> Result<LocalSocketStream, String> {
    let name = endpoint_name(endpoint).map_err(|_| "the local endpoint name is invalid")?;
    timeout(CONNECT_TIMEOUT, LocalSocketStream::connect(name))
        .await
        .map_err(|_| "timed out connecting to the GPUI application".to_owned())?
        .map_err(|_| "could not connect to the GPUI application".to_owned())
}

fn endpoint_name(endpoint: &LocalEndpoint) -> std::io::Result<Name<'_>> {
    match endpoint {
        LocalEndpoint::Filesystem { path } => path.as_path().to_fs_name::<GenericFilePath>(),
        LocalEndpoint::Namespaced { name } => name.as_str().to_ns_name::<GenericNamespaced>(),
    }
}

async fn discover_descriptor(app_id: Option<&str>, endpoint_dir: Option<&Path>) -> Result<PathBuf> {
    let directory = endpoint_dir.map_or_else(default_endpoint_dir, Path::to_path_buf);
    let entries = fs::read_dir(&directory).with_context(|| {
        format!(
            "could not read endpoint directory {}; start the GPUI app first or pass --endpoint",
            directory.display()
        )
    })?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Ok(descriptor) = read_descriptor(&path) else {
            continue;
        };
        candidates.push((path, descriptor));
    }
    let liveness = ProcessLiveness::query(candidates.iter().map(|(_, descriptor)| descriptor.pid));
    let mut matches = Vec::new();
    for (path, descriptor) in candidates {
        if liveness.is_stale(descriptor.pid, &path) {
            // The owning process is gone; drop its descriptor so stale files
            // never accumulate and future discovery stays unambiguous.
            let _ = fs::remove_file(&path);
            continue;
        }
        if app_id.is_none_or(|expected| descriptor.app_id == expected)
            && validate_descriptor(&descriptor, &path).is_ok()
            && timeout(Duration::from_millis(200), connect(&descriptor.endpoint))
                .await
                .is_ok_and(|result| result.is_ok())
        {
            matches.push((path, descriptor));
        }
    }
    match matches.as_slice() {
        [] => Err(anyhow!(
            "no matching GPUI MCP endpoint found in {}",
            directory.display()
        )),
        [(path, _)] => Ok(path.clone()),
        _ => {
            let listing = matches
                .iter()
                .map(|(_, descriptor)| format!("{} (pid {})", descriptor.app_id, descriptor.pid))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "multiple matching GPUI MCP endpoints found [{listing}]; pass --endpoint explicitly"
            )
        }
    }
}

/// Once a process that wrote a descriptor could plausibly have started this
/// long after the descriptor file's timestamp, treat the pid as reused.
const PID_REUSE_SLACK_SECS: u64 = 10;

/// Snapshot of which descriptor-owning processes are still running.
struct ProcessLiveness {
    system: sysinfo::System,
}

impl ProcessLiveness {
    fn query(pids: impl Iterator<Item = u32>) -> Self {
        let pids: Vec<sysinfo::Pid> = pids.map(sysinfo::Pid::from_u32).collect();
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pids), true);
        Self { system }
    }

    /// True when the descriptor's owning process no longer exists, or when its
    /// pid now belongs to a process started after the descriptor was written
    /// (the pid was reused, so the original owner is dead).
    fn is_stale(&self, pid: u32, descriptor_path: &Path) -> bool {
        let Some(process) = self.system.process(sysinfo::Pid::from_u32(pid)) else {
            return true;
        };
        let started = process.start_time();
        if started == 0 {
            return false;
        }
        let written = fs::symlink_metadata(descriptor_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        matches!(
            written,
            Some(written) if started > written.saturating_add(PID_REUSE_SLACK_SECS)
        )
    }
}

fn read_descriptor(path: &Path) -> Result<EndpointDescriptor> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect endpoint descriptor {}", path.display()))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_DESCRIPTOR_BYTES
    {
        bail!("endpoint descriptor must be a regular, non-symlink file smaller than 16 KiB");
    }
    #[cfg(unix)]
    validate_descriptor_permissions(&metadata)?;
    let bytes = fs::read(path)
        .with_context(|| format!("could not read endpoint descriptor {}", path.display()))?;
    serde_json::from_slice(&bytes).context("endpoint descriptor contains invalid JSON")
}

fn validate_descriptor(descriptor: &EndpointDescriptor, descriptor_path: &Path) -> Result<()> {
    if descriptor.protocol_version != PROTOCOL_VERSION {
        bail!(
            "endpoint protocol is {}, but server requires {}",
            descriptor.protocol_version,
            PROTOCOL_VERSION
        );
    }
    validate_local_endpoint(&descriptor.endpoint, descriptor_path)?;
    if descriptor.token.len() != 64
        || !descriptor
            .token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("endpoint authentication token is malformed");
    }
    if descriptor.app_id.is_empty()
        || descriptor.app_id.len() > 64
        || !descriptor
            .app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("endpoint application identifier is malformed");
    }
    Ok(())
}

fn validate_local_endpoint(endpoint: &LocalEndpoint, descriptor_path: &Path) -> Result<()> {
    let _ = descriptor_path;
    match endpoint {
        LocalEndpoint::Filesystem { path } => {
            let _ = path;
            #[cfg(not(unix))]
            bail!("filesystem local endpoints are unsupported on this platform");
            #[cfg(unix)]
            {
                use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};

                if !path.is_absolute() {
                    bail!("filesystem local endpoint must be absolute");
                }
                let descriptor_parent = descriptor_path
                    .parent()
                    .ok_or_else(|| anyhow!("descriptor has no parent directory"))?
                    .canonicalize()?;
                let endpoint_parent = path
                    .parent()
                    .ok_or_else(|| anyhow!("filesystem local endpoint has no parent directory"))?
                    .canonicalize()?;
                if endpoint_parent != descriptor_parent {
                    bail!("filesystem local endpoint must share the descriptor directory");
                }
                let socket_metadata = fs::symlink_metadata(path)?;
                if socket_metadata.file_type().is_symlink()
                    || !socket_metadata.file_type().is_socket()
                {
                    bail!("filesystem local endpoint is not a Unix-domain socket");
                }
                if socket_metadata.permissions().mode() & 0o077 != 0 {
                    bail!("filesystem local endpoint is accessible by group/other users");
                }
                let descriptor_metadata = fs::symlink_metadata(descriptor_path)?;
                if socket_metadata.uid() != descriptor_metadata.uid() {
                    bail!("descriptor and local endpoint have different owners");
                }
            }
        }
        LocalEndpoint::Namespaced { name } => {
            #[cfg(not(windows))]
            bail!("namespaced local endpoints are unsupported on this platform");
            #[cfg(windows)]
            if name.is_empty()
                || name.len() > 256
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                bail!("namespaced local endpoint is malformed");
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_descriptor_permissions(metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!("refusing endpoint descriptor readable or writable by group/other users");
    }
    Ok(())
}

pub(crate) fn default_runtime_root() -> PathBuf {
    if let Some(base) = BaseDirs::new() {
        return base
            .runtime_dir()
            .unwrap_or_else(|| base.data_local_dir())
            .join("gpui-mcp");
    }
    std::env::temp_dir().join("gpui-mcp")
}

fn default_endpoint_dir() -> PathBuf {
    default_runtime_root().join("run")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use anyhow::Result;
    use gpui_mcp_protocol::{
        BridgeResult, Capabilities, EndpointDescriptor, LocalEndpoint, Operation, PROTOCOL_VERSION,
        WireRequest, WireResponse,
    };
    use interprocess::local_socket::{
        GenericFilePath, GenericNamespaced, ListenerOptions, ToFsName as _, ToNsName as _,
        tokio::prelude::*,
    };
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::BridgeClient;

    #[tokio::test]
    async fn authenticated_persistent_connection_round_trips_twice() -> Result<()> {
        let directory = tempdir()?;
        let endpoint = test_endpoint(directory.path());
        let name = match &endpoint {
            LocalEndpoint::Filesystem { path } => path.as_path().to_fs_name::<GenericFilePath>()?,
            LocalEndpoint::Namespaced { name } => {
                name.as_str().to_ns_name::<GenericNamespaced>()?
            }
        };
        let listener = ListenerOptions::new().name(name).create_tokio()?;
        #[cfg(unix)]
        if let LocalEndpoint::Filesystem { path } = &endpoint {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        let token = "ab".repeat(32);
        let server_token = token.clone();
        let task = tokio::spawn(async move {
            let mut stream = listener.accept().await?;
            for _ in 0..2 {
                let mut length = [0_u8; 4];
                stream.read_exact(&mut length).await?;
                let mut payload = vec![0_u8; u32::from_be_bytes(length) as usize];
                stream.read_exact(&mut payload).await?;
                let request: WireRequest = serde_json::from_slice(&payload)?;
                assert_eq!(request.token, server_token);
                let response = WireResponse::success(
                    request.request_id,
                    BridgeResult::Pong {
                        app_id: "test".to_owned(),
                        pid: 7,
                        protocol_version: PROTOCOL_VERSION,
                    },
                );
                let payload = serde_json::to_vec(&response)?;
                let response_length = u32::try_from(payload.len())?;
                stream.write_all(&response_length.to_be_bytes()).await?;
                stream.write_all(&payload).await?;
            }
            anyhow::Ok(())
        });

        let path = directory.path().join("endpoint.json");
        let descriptor = EndpointDescriptor {
            protocol_version: PROTOCOL_VERSION,
            app_id: "test".to_owned(),
            app_name: "Test".to_owned(),
            pid: 7,
            endpoint,
            token,
            window_title: "Test".to_owned(),
            capabilities: Capabilities::default(),
        };
        fs::write(&path, serde_json::to_vec(&descriptor)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }

        let client = BridgeClient::discover(Some(&path), None, None).await?;
        for _ in 0..2 {
            let result = client
                .call(Operation::Ping)
                .await
                .map_err(anyhow::Error::msg)?;
            assert!(matches!(result, BridgeResult::Pong { pid: 7, .. }));
        }
        task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn discovery_ignores_and_deletes_descriptors_of_dead_processes() -> Result<()> {
        let directory = tempdir()?;
        let live_endpoint = test_endpoint(directory.path());
        let name = match &live_endpoint {
            LocalEndpoint::Filesystem { path } => path.as_path().to_fs_name::<GenericFilePath>()?,
            LocalEndpoint::Namespaced { name } => {
                name.as_str().to_ns_name::<GenericNamespaced>()?
            }
        };
        let listener = ListenerOptions::new().name(name).create_tokio()?;
        #[cfg(unix)]
        if let LocalEndpoint::Filesystem { path } = &live_endpoint {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        let live_path = directory.path().join("live.json");
        write_test_descriptor(&live_path, std::process::id(), live_endpoint)?;

        #[cfg(unix)]
        let dead_endpoint = LocalEndpoint::Filesystem {
            path: directory.path().join("dead.sock"),
        };
        #[cfg(windows)]
        let dead_endpoint = LocalEndpoint::Namespaced {
            name: format!("gpui-mcp-test-dead-{}", std::process::id()),
        };
        let dead_path = directory.path().join("dead.json");
        write_test_descriptor(&dead_path, exited_process_id()?, dead_endpoint)?;

        let discovered =
            super::discover_descriptor(Some("test-app"), Some(directory.path())).await?;
        assert_eq!(discovered, live_path);
        assert!(live_path.exists());
        assert!(!dead_path.exists(), "stale descriptor should be deleted");
        drop(listener);
        Ok(())
    }

    #[test]
    fn liveness_distinguishes_running_and_exited_processes() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("descriptor.json");
        fs::write(&path, b"{}")?;
        let live_pid = std::process::id();
        let dead_pid = exited_process_id()?;
        let liveness = super::ProcessLiveness::query([live_pid, dead_pid].into_iter());
        assert!(!liveness.is_stale(live_pid, &path));
        assert!(liveness.is_stale(dead_pid, &path));
        Ok(())
    }

    #[test]
    fn liveness_treats_reused_pid_as_stale() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("descriptor.json");
        fs::write(&path, b"{}")?;
        let past = std::time::SystemTime::now() - std::time::Duration::from_hours(1);
        fs::File::options()
            .write(true)
            .open(&path)?
            .set_modified(past)?;
        let mut child = long_running_process()?;
        let pid = child.id();
        let liveness = super::ProcessLiveness::query(std::iter::once(pid));
        let stale = liveness.is_stale(pid, &path);
        child.kill()?;
        child.wait()?;
        assert!(
            stale,
            "a process started after the descriptor was written cannot own it"
        );
        Ok(())
    }

    fn write_test_descriptor(path: &Path, pid: u32, endpoint: LocalEndpoint) -> Result<()> {
        let descriptor = EndpointDescriptor {
            protocol_version: PROTOCOL_VERSION,
            app_id: "test-app".to_owned(),
            app_name: "Test".to_owned(),
            pid,
            endpoint,
            token: "ab".repeat(32),
            window_title: "Test".to_owned(),
            capabilities: Capabilities::default(),
        };
        fs::write(path, serde_json::to_vec(&descriptor)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    fn exited_process_id() -> Result<u32> {
        #[cfg(windows)]
        let mut command = std::process::Command::new("cmd");
        #[cfg(windows)]
        command.args(["/C", "exit"]);
        #[cfg(unix)]
        let command = std::process::Command::new("true");
        #[cfg(unix)]
        let mut command = command;
        let mut child = command.spawn()?;
        let pid = child.id();
        child.wait()?;
        Ok(pid)
    }

    fn long_running_process() -> Result<std::process::Child> {
        #[cfg(windows)]
        let child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()?;
        #[cfg(unix)]
        let child = std::process::Command::new("sleep").arg("30").spawn()?;
        Ok(child)
    }

    fn test_endpoint(directory: &Path) -> LocalEndpoint {
        #[cfg(unix)]
        {
            LocalEndpoint::Filesystem {
                path: directory.join("test.sock"),
            }
        }
        #[cfg(windows)]
        {
            let _ = directory;
            LocalEndpoint::Namespaced {
                name: format!(
                    "gpui-mcp-test-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos())
                ),
            }
        }
    }
}
