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
| Pixels | `screenshot`, `screenshot_element`, `compare_screenshots`, `start_video_recording` |
| Annotate | `annotate_elements`, `remove_annotations`, `list_annotations`, `highlight_elements`, `clear_highlights` |
| Talk to the app | `read_messages`, `wait_for_messages`, `send_message`, the `gpui://messages` resource |
| Performance | `mark_frames`, `get_frame_report`, `record_performance` |
| Live edit | `get_live_document`, `preview_live_document` |

Prefer the element tools over coordinates. All coordinates are logical pixels
relative to the window.

Clients that support the MCP tasks extension (SEP-2663) get the tools that can
block for seconds (`wait_for_messages`, `wait_for_element`, `wait_for_state`,
`record_performance`) as tasks they poll with `tasks/get`, so a long wait does
not hold the client. Other clients get the same result from an ordinary call.

## Annotations

An annotation marks a semantic element, or a fixed rectangle, with an outline
or fill and an optional label drawn as a tag. Node annotations are resolved
again on every frame, so they follow the element through layout changes,
scrolling and zoom, and hide while it is absent, hidden, or scrolled out of
its scroll container. Each has an id (reuse it to update the annotation), an
optional group, and an optional lifetime that starts at the first frame that
draws it. Agents use `annotate_elements`; `highlight_elements` is a thin
wrapper that outlines elements in the `highlights` group.

The application shares the same set. It can add its own annotations, observe
the agent's, and draw them itself:

```rust,ignore
use gpui_mcp::{AnnotationSpec, AnnotationStyle};

let automation = bridge.automation();
automation.annotate(
    AnnotationSpec::node("save")
        .with_label("Unsaved changes")
        .with_style(AnnotationStyle::OutlineFill)
        .with_color("#FFB020FF")
        .with_ttl_ms(5_000),
    window,
)?;

// Called after every change by the agent, the app, or expiry.
bridge.on_annotations(|event, _window, cx| {
    // event.annotations: the full set; event.changed_by: Agent, App or Bridge
})?;

// Draw them yourself instead; bounds are still resolved every frame.
automation.paint_annotations(false, window);
let drawn = automation.annotations(); // each with `resolved` bounds
```

## Talking to the agent from your app

There is no model in the bridge. An in-app chat box talks to the person's own
MCP agent (Claude Code, Codex, or any other client) through a bounded message
log: messages have monotonic ids and timestamps, either side reads by id, and
each side may have at most 64 messages the other has not read.

```rust,ignore
use gpui_mcp::NewMessage;

// Receive the agent's messages on the GPUI thread, once each, in order.
// Registering this also tells the agent that the app reads messages.
bridge.on_message(|message, _window, cx| {
    // show message.text; message.reply_to names the app message it answers
})?;

// Post what the person typed.
bridge.post_message(NewMessage::text("Make the toolbar denser"))?;
```

The agent reads the app's messages with `read_messages` (pass the returned
`latest_id` as `since` next time) and replies with `send_message`, naming the
message it answers in `reply_to`. To wait for the person's next message:

- any client can call `wait_for_messages` in a loop; each call blocks for up
  to 30 seconds and returns an empty page with `timed_out: true` when nothing
  arrived;
- clients that support MCP tasks run that wait as a task instead of a
  blocking call;
- clients that subscribe to resources can subscribe to `gpui://messages`
  (with `resources/subscribe` or `subscriptions/listen`) and receive
  `notifications/resources/updated` whenever the app posts a message.

For a chat loop, tell the agent something like: "Call `wait_for_messages` with
the latest id you have seen, act on each message from the app, and answer it
with `send_message`, then wait again."

## GPUI Kit, `gpui-pre` and `gpui-ce`

Besides Zed's own GPUI, the bridge works with two GPUI releases on crates.io:

| GPUI crate | Used by | Supported versions | Bridge feature |
| --- | --- | --- | --- |
| `gpui-pre` | GPUI Kit, `gpui-component` | 0.3.5, 0.3.6, 0.3.7 | `gpui-pre` |
| `gpui-ce` | The community fork and apps built on it | 0.2.2 | `gpui-ce` |

Each needs a small set of GPUI patches, which this repository provides. The
recipes below pull in a patched copy from here.

### GPUI Kit 0.7.0 (`gpui-pre` 0.3.7)

Add this to your workspace's `Cargo.toml`:

```toml
[dependencies]
gpui-kit = "=0.7.0"
gpui-mcp = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main", default-features = false, features = ["gpui-pre"] }

[patch.crates-io]
gpui-pre = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main" }
```

Call `gpui_kit::init(cx)`, open the window with `gpui_kit::open_window`, and
install the bridge as usual. The bridge accepts Kit's `Window` and `App`
directly. To see it working, run the [Kit demo](examples/gpui-kit):

```console
cargo run --manifest-path examples/gpui-kit/Cargo.toml
```

### `gpui-ce` 0.2.2

Add this to your workspace's `Cargo.toml`:

```toml
[dependencies]
gpui-ce = "=0.2.2"
gpui_ce_platform = "=0.1.0"
gpui-mcp = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main", default-features = false, features = ["gpui-ce"] }

[patch.crates-io]
gpui-ce = { git = "https://github.com/themixednuts/gpui-mcp", branch = "main" }
```

`gpui-ce` is imported as `gpui`, so install the bridge as usual when you open
a window.

### Another version, or your own GPUI patches

The `[patch]` recipes above always give you the version this repository
vendors. If your app is on an older supported version, or you carry GPUI
patches of your own, keep your own patched copy instead:

1. From a checkout of this repository, write a patched copy into your app.
   Pass `--crate gpui-ce` for `gpui-ce`.

   ```console
   cargo xtask vendor --crate gpui-pre --version 0.3.5 --output /path/to/your-app/vendor/gpui-pre
   ```

2. Apply your own patches to that copy, if you have any.
3. Point your workspace at it:

   ```toml
   [patch.crates-io]
   gpui-pre = { path = "vendor/gpui-pre" }
   ```

The patches are in [`vendor/patches/`](vendor/patches), one folder per crate
and version. Each version has three:

- `automation.patch` has everything the bridge needs.
- `font-fallback.patch` is an unrelated font fix. Add `--without font-fallback`
  to skip it.
- `grid.patch` exposes CSS grid track lists (`grid_template_columns`,
  `grid_template_rows`, `grid_auto_rows`, `grid_auto_flow`, `grid_column`,
  `grid_row`, with `px`, `%`, `fr`, `auto`, `min-content`, `max-content`,
  `minmax()`, `fit-content()` and `repeat(n | auto-fill | auto-fit, …)`).
  The bridge doesn't need it, but `gpui-mcp-html` does. Add
  `--without grid` to skip it.

CI builds and tests the bridge against every supported version, so these
patches are kept working.

### Things to know

- Use one backend per app. Every `gpui-mcp` dependency in a GPUI Kit or
  `gpui-ce` app needs `default-features = false`. Zed apps that turn off default
  features must add `features = ["zed"]`.
- Kit components that publish accessibility info work with all the tools.
  Custom-drawn components only show what they annotate.
- In Kit 0.7.0, disabled controls report `enabled: true` and read-only inputs
  don't report `read_only`. Kit doesn't publish these states yet.
- `gpui-mcp-html` works with GPUI Kit too: add it with
  `default-features = false, features = ["gpui-pre", "json", "ron"]`. Its
  `grid-template-*` support needs the `grid.patch` described below, which the
  `[patch]` recipe already includes.

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
tree, building the observed frame, and painting annotations. The semantic tree
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
