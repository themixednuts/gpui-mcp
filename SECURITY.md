# Security policy

## Scope

`gpui-mcp` is a local automation bridge. Anyone who can authenticate to it can invoke annotated application actions and keyboard input, inspect application-published semantics/logs, and request screenshots of the configured application window.

Do not ship the bridge enabled in a production application unless local automation is an intentional, documented product feature. Prefer a development-only Cargo feature in downstream applications.

## Supported versions

Until the first stable release, only the current main branch is supported with security fixes.

## Reporting

Do not open a public issue for a vulnerability that exposes credentials, enables unauthenticated control, crosses the target-window boundary, bypasses a configured limit, or writes/reads unintended files. Use [GitHub's private vulnerability reporting form](https://github.com/themixednuts/gpui-mcp/security/advisories/new) and include:

- affected commit and operating system;
- minimal reproduction;
- expected and observed security boundary;
- whether credentials or user data were exposed.

## Threat model

The bridge defends against unauthorized local IPC clients, cross-user endpoint access, unbounded resource consumption through protocol fields, path traversal and symlink endpoint or recording-artifact substitution, stale endpoint or semantic-generation selection, arbitrary screenshot or recording paths, and UI mutation from a background thread. Video output is confined to a server-configured directory; tool callers supply only a bounded portable `.mp4` filename, and overwrite of an existing regular file is opt-in. The pointer tools drive GPUI's in-process input pipeline and never inspect or move the global operating-system cursor.

Video recording is state owned by one MCP server process. `start_video_recording` and `stop_video_recording` must run over the same initialized MCP transport. Continuous mode captures settled GPUI frames at a bounded rate; `capture_video_frame` is an optional checkpoint. Session IDs bind in-flight captures and encoding to the recording that created them; duplicate starts, concurrent captures, and calls made while encoding are rejected. Repeated capture failures stop the continuous worker instead of retrying unboundedly. Failed or cancelled capture requests release only their own session reservation. Failed encoding restores captured frames so the same session can be retried. The output is a real H.264/MP4 file; optional cursor overlays use GPUI's window-relative pointer state.

The following are outside its boundary:

- a process already running with the same user's privileges and able to read that user's private runtime/local-data files;
- a compromised instrumented application;
- secrets deliberately published by application code in labels, text/value annotations, metadata, or logs;
- operating-system screenshot implementations and desktop portals after the user grants permission;
- denial of service by the owning user terminating either process.

## Deployment checklist

- Use a dedicated downstream Cargo feature and disable it in release builds.
- Keep the endpoint descriptor in the default private directory or an equally protected directory.
- Keep recording artifacts in the default MCP-server-session runtime directory or an explicitly private `--artifact-dir`; never point it at a shared or privileged directory.
- Drive a recording through one long-lived MCP client session (or one bounded `gpui-mcp-call --batch` invocation); separate one-shot tool processes do not share recording state.
- Keep Unix socket, descriptor, and artifact-directory modes owner-only; do not weaken the Windows named-pipe DACL.
- Never copy the descriptor or token into logs, chat messages, CI artifacts, or shared directories.
- Give each application a clear logical ID in Rust. Use the non-secret IDs from `list_apps` with `select_app` when multiple instances run; never copy endpoint descriptors or bearer tokens into MCP configuration.
- Mark secret inputs redacted and publish no secret log content.
- Review requested macOS Screen Recording and Linux portal permissions.
- Keep `gpui`, `rmcp`, `xcap`, Rust, and transitive dependencies current.

## Audited upstream exceptions

`.cargo/audit.toml` temporarily ignores `RUSTSEC-2026-0194` and
`RUSTSEC-2026-0195`. The affected `quick-xml` versions are build dependencies
of the latest `wayland-scanner` and `xcb`; they parse bundled/trusted protocol
XML while compiling Linux support and are not reachable from MCP or application
input at runtime. As of 2026-07-15, neither upstream has a release using the
fixed `quick-xml >=0.41`. The weekly audit remains blocking for every other
vulnerability, and these exceptions should be removed on the first compatible
upstream release.
