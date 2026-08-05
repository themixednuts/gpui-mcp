# Vendored dependencies

`gpui/` is GPUI 0.2.2 from Zed commit
`69e2130295c2649963eb639fc70b4f2ee8ea1624`. It carries five downstream source
changes:

- `DispatchEventResult` is public so callers can use GPUI's already-public
  `Window::dispatch_event` method.
- Font fallback changes only the family while preserving the requested
  features, weight, and style. This keeps fallback text consistent with the
  caller's typography request and the Chromium/GPUI visual-parity fixtures.
- `Window::native_window_id` exposes the platform window identity already held
  by GPUI so capture can survive application title changes.
- A rendered semantic frame is collected from stable element IDs, real
  interactivity, layout bounds, text, and focus handles. `FrameObserver` exposes
  completed frames and a post-paint overlay without maintaining a second UI
  graph.
- `Window::insert_input_text` and `Window::replace_input_text` route text through
  the focused element's active `InputHandler`, including IME-aware editors.

The crate's Apache-2.0 license is retained in `gpui/LICENSE-APACHE`.

Remove the patch in the workspace `Cargo.toml` and this directory once an
upstream GPUI release includes these behaviors. Each change can be dropped
independently after its upstream equivalent ships.
