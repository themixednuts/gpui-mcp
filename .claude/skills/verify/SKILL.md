# Verify gpui-mcp recording

1. Build `gpui-mcp-server`, `gpui-mcp-call`, and `gpui-mcp-demo`.
2. From the repository root, launch `cargo run -p gpui-mcp-demo` and wait for the `GPUI MCP endpoint` line.
3. Create a JSON batch containing `start_video_recording` in continuous mode, optional `capture_video_frame` checkpoints, and `stop_video_recording`.
4. Run `target/debug/gpui-mcp-call --server target/debug/gpui-mcp-server --app-id gpui-mcp-demo --artifact-dir <private-test-dir> --batch <batch.json>` (append `.exe` to the two binary paths on Windows).
5. Confirm every response has the same session ID, frame counts increase without explicit checkpoints, `stop_video_recording` reports the expected MP4, and open the MP4 to inspect the captured demo window.
6. Probe duplicate `start_video_recording` and an invalid traversal artifact name in one batch, then confirm a valid `stop_video_recording` still succeeds for that session.
7. Stop `gpui-mcp-demo.exe` after verification.

On Windows, the first demo build may take about a minute. Use one persistent `--batch` invocation because recording state belongs to the child MCP server process.
