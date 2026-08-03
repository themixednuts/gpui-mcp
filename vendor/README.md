# Vendored dependencies

`gpui/` is GPUI 0.2.2 from Zed commit
`69e2130295c2649963eb639fc70b4f2ee8ea1624`. It carries one downstream source
change: `DispatchEventResult` is public so callers can use GPUI's already-public
`Window::dispatch_event` method. The crate's Apache-2.0 license is retained in
`gpui/LICENSE-APACHE`.

Remove the patch in the workspace `Cargo.toml` and this directory once an
upstream GPUI release exposes that return type.
