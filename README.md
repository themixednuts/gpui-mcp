# gpui-mcp

Give MCP agents eyes and hands inside a
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) app.

Agents read the live UI as a semantic tree, then click, type, hover, drag,
scroll and wait on real state. They can also take screenshots and diffs, record
video, measure frame cost, and edit HTML-authored interfaces while the app runs.
It works on Windows 11, macOS, and Linux.

## Setup

Install the MCP server and add it to your MCP client:

```console
cargo install --git https://github.com/themixednuts/gpui-mcp --locked gpui-mcp-server
```

```json
{ "mcpServers": { "gpui": { "command": "gpui-mcp" } } }
```

Add the bridge and its GPUI build to your app:

```toml
[dependencies]
gpui = "=0.2.2"
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "16c9aa7ea6d897a8044d9501cde1b295256722f2", features = ["font-kit", "wayland", "x11"] }
gpui-mcp = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main" }

[patch.crates-io]
gpui = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main" }

[patch."https://github.com/zed-industries/zed"]
gpui = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main" }
```

Both patches are required. They keep your app, `gpui_platform`, and the bridge
on one GPUI build. They can go away once the additions in the
[vendor patch inventory](vendor/gpui/PATCHES.md) land upstream.

Install the bridge when you open a window:

```rust,ignore
use gpui_mcp::{AppId, BridgeConfig, BridgeHandle};

let bridge = BridgeHandle::install(
    window,
    cx,
    BridgeConfig::new(AppId::new("my-app")?, "My App"),
)?;
```

That is the whole integration. The client discovers running apps on its own.
See the [demo](examples/demo/src/main.rs) for a complete window.

## Making your UI agent-ready

The tree is built from your rendered elements and their accessibility data.
Most problems come from the items below.

1. **Keep the `BridgeHandle` alive.** Store it in your root view. Dropping it
   stops the bridge.
2. **Give elements an `.id(...)`.** Only elements with an id become nodes.
   Text inside an element without one becomes part of its nearest ancestor's
   label, and that text cannot be clicked, waited on or asserted on separately.
   Clickable elements already need an id in GPUI.
3. **Keep ids unique among siblings.** When ids collide, the nodes get longer
   identities qualified by their path, and exact duplicates are dropped with a
   `DuplicateId` diagnostic. Find children through `parent`; don't rely on the
   shape of an id.
4. **Name controls that have no text.** An icon button's label is otherwise its
   glyph, such as `⚙`. Use `.aria_label("Settings")`, and use `.role(...)` when
   the role can't be inferred, as with tabs.
5. **State disabled and read-only explicitly.** `enabled` comes only from
   `.aria_disabled(true)`. A grey control with no click handler still reports
   `enabled: true`. Mark read-only inputs with `.aria_read_only(true)`.
6. **Redact secrets.** `.frame_redacted(true)` on an element with an id
   withholds its text and value from the bridge, and from any labels derived
   from them.
7. **Check the diagnostics.** `get_ui_tree` returns a `diagnostics` list that
   reports omitted, duplicate and orphaned nodes.

```rust,ignore
div()
    .id("settings")
    .aria_label("Settings")
    .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx)))
    .child(svg().path("icons/settings.svg").size_4())
```

For HTML-authored interfaces that agents can also edit live, see the
[visual builder guide](docs/visual-builder.md) and the
[showcase](examples/runtime-showcase).

## What agents can do

| Area | Tools |
| --- | --- |
| Discover | `list_apps`, `select_app`, `get_ui_tree`, `find_elements`, `get_element` |
| Act | `click_element`, `type_text`, `set_text`, `set_value`, `keyboard`, `hover_element`, `drag_element`, `scroll`, `pointer_*` |
| Verify | `wait_for_element`, `wait_for_state`, `get_element_state`, `save_ui_snapshot`, `diff_current_ui` |
| Pixels | `screenshot`, `screenshot_element`, `compare_screenshots`, `highlight_elements`, `start_video_recording` |
| Performance | `mark_frames`, `get_frame_report`, `record_performance` |
| Live edit | `get_live_document`, `preview_live_document` |

Prefer the element tools over coordinates. All coordinates are logical pixels
relative to the window.

## GPUI Kit

For [GPUI Kit](https://github.com/longbridge/gpui-kit) 0.7.0, select the
`gpui-pre` backend and patch its GPUI snapshot in your workspace root:

```toml
[dependencies]
gpui-kit = "=0.7.0"
gpui-mcp = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main", default-features = false, features = ["gpui-pre"] }

[patch.crates-io]
gpui-pre = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main" }
```

Call `gpui_kit::init(cx)`, open the window with `gpui_kit::open_window`, and
install the bridge as above. The bridge accepts Kit's `Window` and `App`
directly. Run the [Kit demo](examples/gpui-kit) with
`cargo run --manifest-path examples/gpui-kit/Cargo.toml`.

- Choose exactly one backend per app. Cargo features are additive, so every
  dependency on `gpui-mcp` in a Kit app must disable default features. Zed
  integrations that disable defaults must add `features = ["zed"]`.
- Kit's annotated components work with the standard tools. Custom-drawn
  components expose only the semantics they annotate.
- Kit 0.7.0's disabled controls don't set the disabled flag, so they report
  `enabled: true`. Its read-only inputs don't publish `read_only`.
- `gpui-mcp-html` currently uses the Zed backend. Newer `gpui-pre` pins need a
  vendor update; see [vendor/README.md](vendor/README.md).

## Measuring frame cost

Injected input costs the same as real input. The server waits for the frames
that input caused and adds none of its own.

Call `mark_frames`, perform the interaction, then call `get_frame_report`. For
every frame since the mark it gives:

- the full `Window::draw` time (`draw_ms`), split into the app's share
  (`app_draw_ms`) and the bridge's (`bridge_ms`);
- p50, p95 and max for each;
- every view that rendered and why, plus the cached views that replayed instead.

The render causes are:

- `notified`: the view, or a view inside it, called `cx.notify()`
- `ancestor_rendered`: a cached view around it rendered
- `refresh`: the window was refreshed
- `first_draw`: the view had nothing cached yet
- `layout_changed`: its bounds, content mask, or text style changed
- `uncached`: it is not embedded with `.cached(...)`

A hover inside an `Entity::cached` region should show that region `notified`
and its siblings replaying. Anything else shows where a caching boundary leaks.
`get_frame_stats` averages over the same frames. `record_performance` reports a
fixed interval. In process, use `Automation::mark_frames` and
`Automation::frame_report`.

`bridge_ms` is the work the bridge adds to a draw: finishing the accessibility
tree, building the observed frame, and painting highlights. The semantic tree
is converted off the UI thread when a client reads it.

## Platform notes

- **Linux:** exact-window capture currently requires X11.
- **Windows:** screenshots take a short burst of compositor samples and return
  the newest, because Windows Graphics Capture can return a stale frame first.
  The one-second deadline covers waiting on the compositor, not readback, so
  even very large windows capture from debug builds.

## Security

Only enable automation in development, testing, or another trusted
environment. See [SECURITY.md](SECURITY.md).

## License

Apache-2.0.
