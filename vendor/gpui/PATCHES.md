# GPUI patch inventory

This directory tracks Zed GPUI at commit
`16c9aa7ea6d897a8044d9501cde1b295256722f2` (`gpui` 0.2.2).

gpui-mcp carries nine focused changes that are not available from upstream
GPUI yet. The first six were last checked against `zed-industries/zed` `main`
on 2026-09-22, at the commit named above; the last three were written against
the same commit on 2026-09-23:

- read-only observation of each completed rendered AccessKit tree, with stable
  GPUI element paths, frame-unique node identities, bounds, text provenance,
  and an overlay paint pass (`FrameObserver`, `AccessibilityFrame`, `FrameNode`,
  `Window::observe_frames`, `Window::focus_observed_element`). A node keeps its
  own element id while that id names it alone, and otherwise takes the shortest
  trailing run of its element path that separates it, so identities are
  deliberately **not uniform in shape**: two siblings can look nothing alike
  because one collided and the other did not, and adding a widget to one
  container can change the shape of identities in a container nobody touched.
  Consumers must group nodes by `parent`, which is exact and resolved from
  paths, and never by the form of the identity itself — a matcher keyed on an
  identity pattern selects exactly the nodes that collided and silently skips
  the ones that did not;
- rendered-frame provenance that AccessKit does not model: the
  `Element::frame_node` and `Element::frame_text` hooks, the `frame_metadata`,
  `frame_action`, and `frame_redacted` builders, hover/drag/scroll interaction
  inference from an element's listeners, and a `Div` with click listeners
  reporting `Role::Button`. Redacted frame text and values are withheld from
  the bridge, including labels derived from content;
- `aria_disabled`, `aria_hidden`, and `aria_read_only` builders that forward to
  the corresponding AccessKit node states. An ID-bearing, hidden `Div` with no
  other role reports `Role::Group` so the hidden state covers its descendants;
- programmatic focus and text replacement through GPUI's active input handler
  (`Window::insert_input_text`, `Window::replace_input_text`);
- pointer ownership that prevents a stale native mouse position from cancelling
  a synthetic hover before the physical mouse actually moves;
- font-family fallback that preserves the requested weight, style, and OpenType
  features;
- frame cost and view-cache observation. `FrameObserver::frame_drawn` receives
  a `DrawnFrame` when `Window::draw` returns. It carries the draw's wall time,
  the same interval the profiler records as `FrameTiming::draw_duration`, but
  without the `profiler` feature. It also carries the part of that time spent
  only because the frame was observed, and a `ViewDraw` for every view: whether
  it rendered or replayed its previous output, and, for a render, the
  `ViewRenderCause`. `FrameObserver::accessibility_frame` hands observers a
  shared `Arc<AccessibilityFrame>`, so they can keep it and convert it later,
  off the UI thread. `AccessibilityFrame::accessibility_node` indexes the tree
  on first use instead of scanning it for every node. An observed frame's
  prepaint checkpoints record only the nodes that are open. Those are the only
  existing nodes a later range can add text to, so a cached view's two
  checkpoints no longer copy every node drawn so far. Text is added to open
  nodes by position rather than by searching for their path;
- `Window::request_frame`, which schedules a frame without the view-cache
  invalidation of `Window::refresh`, and `Window::frame_pending`, which reports
  whether the window is invalidated; and
- view-scoped redraws for an element's own interaction state. Pressing,
  releasing, and the active state of an element with click or drag handlers,
  and showing its tooltip, notify the view that painted the element. Upstream
  refreshes the whole window, which renders every cached view again. That state
  is read only while its view prepaints and paints the element, so the view is
  enough. An element painted outside any view still refreshes. Hiding a tooltip
  requests a frame without notifying anything: a replayed element re-registers
  its old tooltip request, and that request's visibility check reports the
  tooltip hidden. This is the one change here that alters GPUI's behavior
  rather than adding to it. Starting a drag, dropping, changing the window's
  hover status, and switching between keyboard and pointer input still refresh
  the window: a drag is drawn at window level, any view may read
  `Window::is_window_hovered`, and input modality changes hover and
  focus-visible styling everywhere.

Two earlier additions are no longer needed and are not carried:

- `DispatchEventResult` was made public so callers could use `Window::
  dispatch_event`; upstream now exposes both publicly.
- `Window::native_window_id`; `gpui-mcp` obtains native window identity through
  `raw-window-handle` instead.

The bridge uses GPUI's standard `Role` and `aria_*` APIs, extended only by the
builders above. It does not maintain a second semantic tree.

Every item should be removed here as soon as an equivalent upstream API is
available. The rest of this directory is an unmodified snapshot of that Zed
commit, except that `Cargo.toml` has Zed workspace inheritance unwound (and
`[dev-dependencies]` dropped) so the crate can build outside the Zed workspace,
and two blank doc comments in `_accessibility.rs` have trailing spaces removed.
The manifest rewrite carries no behavior change.
