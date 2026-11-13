# gpui_wgpu patch inventory

This directory is Zed's `gpui_wgpu` at commit
`27bbbc1cc480a7500cec122f120e796d1fb8500c`, the same commit as
[`../gpui/`](../gpui/PATCHES.md).

Zed builds it against wgpu from gfx-rs commit
`cddb2f5d80dc0009b2ef6767635140cacf90a007`, a commit after 30.0.1 that carries
two fixes no release had on 2026-11-12 (30.0.2 included): the Vulkan
surface-drop panic fix
([gfx-rs/wgpu#10230](https://github.com/gfx-rs/wgpu/pull/10230)) and
[gfx-rs/wgpu#10348](https://github.com/gfx-rs/wgpu/pull/10348), which ignores
timeouts on wasm and which Zed's manifest cites as its WebGL resize fix.
An app that already uses the wgpu 30 release would otherwise link two wgpus.
This copy builds against the release (`wgpu = "30.0.1"`), which differs from
that commit in two APIs `gpui_wgpu` uses:

- `TextureFormat::has_srgb_suffix` is still called `TextureFormat::is_srgb`
  ([gfx-rs/wgpu#9758](https://github.com/gfx-rs/wgpu/pull/9758) renamed it
  without changing it).
- `DeviceDescriptor` has no `default_queue` field. Zed passes
  `QueueDescriptor::default()`, which only leaves the queue unlabeled, so the
  field is dropped.

Without the two unreleased fixes, dropping a surface whose acquired texture is
still alive can panic on Vulkan, and WebGL resizes behave as they did before
Zed moved off the release. Drop this copy once a wgpu release carries both
fixes and Zed returns to it.

As in `../gpui/`, `Cargo.toml` has Zed workspace inheritance unwound and
`[dev-dependencies]` dropped so the crate builds outside the Zed workspace.
