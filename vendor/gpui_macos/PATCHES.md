# gpui_macos patch inventory

This directory is Zed's `gpui_macos` at commit
`27bbbc1cc480a7500cec122f120e796d1fb8500c`, the same commit as
[`../gpui/`](../gpui/PATCHES.md), with one change.

## A display link that stops answering no longer stops the window

Since zed-industries/zed@0373366 (2026-10-29), each window on macOS 14 and
later gets its frames from a `CAMetalDisplayLink`. GPUI draws only from the
link's updates, and the renderer draws into the drawable each update hands
out. A link calls back only when it can hand out a drawable.

On GitHub's `macos-latest` runners the link sometimes stopped calling back.
The bridge's integration tests then timed out waiting for a frame, at a
different step each run. Diagnostic counters on the link, run on
gpui-mcp#16, showed the same state each time. GPUI had asked for a frame and
the link was running (`isPaused()` false). The window was visible and
invalidated. No update had arrived for over two seconds, and none followed.
In one run the link stopped after its first three drawn frames. Keeping the
link permanently unpaused did not help, so the link's idle pause is not the
cause.

The frame waker now checks that the link answers. When GPUI wakes the link and
no update arrives within 250 ms while GPUI still wants a frame, the window
draws that frame the way `displayLayer:` and activation already do:
detached from the link, from the layer's `nextDrawable`, then with a fresh
link attached. `nextDrawable` waits up to a second for a drawable and returns
none after that, so GPUI's frame runs either way, as it did with
`CVDisplayLink` before that commit. The fallback logs a warning each time it
runs.

As in `../gpui/`, `Cargo.toml` has Zed workspace inheritance unwound and
`[dev-dependencies]` dropped so the crate builds outside the Zed workspace.
Drop this change once Zed's display link recovers on its own.
