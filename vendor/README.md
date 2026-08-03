# Vendored dependencies

`gpui/` is GPUI 0.2.2 from Zed commit
`69e2130295c2649963eb639fc70b4f2ee8ea1624`. It carries two downstream source
changes:

- `DispatchEventResult` is public so callers can use GPUI's already-public
  `Window::dispatch_event` method.
- Font fallback changes only the family while preserving the requested
  features, weight, and style. This keeps fallback text consistent with the
  caller's typography request and the Chromium/GPUI visual-parity fixtures.

The crate's Apache-2.0 license is retained in `gpui/LICENSE-APACHE`.

Remove the patch in the workspace `Cargo.toml` and this directory once an
upstream GPUI release includes both behaviors. Either change can be dropped
independently after its upstream equivalent ships.
