# Stack architecture: who owns what

Four projects make up the HTML-authored GPUI stack. Each concern has exactly
one owner; the others call it rather than re-implementing it.

```text
                         ┌──────────────────────────────┐
                         │          gpui-studio          │  visual builder (app)
                         │  component graph, inspector,  │
                         │  canvas, export               │
                         └───┬───────────┬───────────┬──┘
              renders canvas │   MCP     │  exports  │ uses components
              and its shell  │           │           │
                ┌────────────▼──┐  ┌─────▼─────┐  ┌──▼───────────────────┐
                │ gpui-mcp-html │  │  gpui-mcp │  │ htmlswap GPUI Kit    │
                │  (LiveHtml)   │  │  (bridge) │  │ code generator       │
                └──┬─────────┬──┘  └───────────┘  └──┬───────────────────┘
       compiles,   │         │ drives motion         │ emits calls to
       lowers CSS  │         │                       │
          ┌────────▼──────┐  │   ┌───────────────────▼──────────────────┐
          │   htmlswap    │  └──►│ GPUI Kit (gpui-base, gpui-component)  │
          │  (compiler)   │      │ + gpui-view-transitions               │
          └───────────────┘      └───────────────────┬──────────────────┘
                                                     │
                                           ┌─────────▼─────────┐
                                           │  GPUI (gpui-pre)  │ layout, text,
                                           └───────────────────┘ paint, input
```

## Owners

| Concern | Owner | Notes |
|---|---|---|
| Layout, text, painting, input, focus, accessibility | **GPUI** (gpui-pre for GPUI Kit) | Nobody else lays out or paints. |
| Components (buttons, menus, dialogs, …), theme model | **GPUI Kit** (`gpui-component`) | Studio and exported apps use them directly. |
| Motion primitives: value transitions, keyframes, easing, springs, presence | **GPUI Kit** (`gpui_base::motion`) | Called by both `LiveHtml` and generated code; no second engine. |
| View transitions (capture, named-element morphs, cross-fade, pseudo-element timing) | **`gpui-view-transitions`** (gpui-mcp repo) | The one motion gap in GPUI Kit; built on `gpui_base::motion`, independent of HTML (callers draw the outgoing state and style each name), and a candidate to upstream. |
| HTML parsing, CSS parsing, selectors and the cascade, bindings validation | **htmlswap** | Uses lightningcss; resolves at compile time what can be static. |
| CSS → GPUI-shaped typed lowering (lengths and units, colors, `var()`, `light-dark()`, state styles, motion plan) | **htmlswap** | One lowering, consumed by both back ends below, so preview and export cannot drift. |
| Rust code generation for exported apps | **htmlswap** (GPUI Kit adapter) | Emits GPUI Kit builder chains and `gpui_base::motion` calls; no interpreter ships. |
| Interpreting the lowering at runtime (hot reload, canvas, agents editing live) | **gpui-mcp-html** (`LiveHtml`) | Maps the typed lowering onto GPUI styles each frame; binds hooks; publishes semantics. |
| MCP automation, semantic tree, annotations, messaging | **gpui-mcp** (bridge) | Unchanged by this document. |
| Component graph, projections, editing, export UX | **gpui-studio** | Projects the graph to HTML/CSS/RON (rendered by `LiveHtml`) and to GPUI Kit code (via htmlswap). |

## Who calls whom

- **gpui-studio** → `LiveHtml`/`LiveHtmlSession` (canvas and its own shell UI),
  → the gpui-mcp bridge (MCP), → GPUI Kit components, → htmlswap's GPUI Kit
  code generator (export).
- **gpui-mcp-html** → htmlswap (compile and lower), → `gpui_base::motion` and
  `gpui-view-transitions` (motion; the default on the `gpui-pre` backend), →
  gpui-mcp (automation and semantics).
- **gpui-view-transitions** → `gpui_base::motion` and GPUI only. It knows no
  HTML or CSS: callers hand it each name's `NameStyle` and draw the outgoing
  state themselves.
- **htmlswap** → lightningcss only. It never depends on GPUI; generated code
  names GPUI Kit APIs as text.
- **Exported apps** → GPUI Kit and `gpui-view-transitions` at runtime. They do
  not depend on htmlswap or gpui-mcp-html; bindings become direct calls to the
  application's Rust hooks.

## Rules that keep the stack correct

1. **One lowering.** Any CSS feature is implemented once, in htmlswap's
   lowering. `LiveHtml` and the code generator only map its typed output to
   GPUI. A feature the code generator cannot emit is not supported by
   `LiveHtml` either, and both report the same diagnostic.
2. **One motion runtime.** Transitions, animations and view transitions run
   on `gpui_base::motion` in both back ends; it is the default wherever GPUI
   Kit is. On the Zed GPUI backend, which GPUI Kit does not support,
   `LiveHtml` applies end states without animating.
3. **GPUI-expressible only.** The lowering covers what GPUI can draw: no
   general transforms beyond translation, no gradients or filters until GPUI
   has them. Unsupported CSS is a diagnostic, never a silent approximation.
4. **Spec behavior over convenience.** Selectors, cascade, units and colors
   follow the CSS specifications; state pseudo-classes apply to the element
   they are written on (`.card:hover .title` lowers to a GPUI group hover).
5. **Version ranges, not pins.** Dependencies accept every release they work
   with (`gpui-pre >=0.3.5, <=0.3.7`, `gpui-base >=0.6.4, <0.8`), and CI tests
   each supported release. A pin is only for a vendored tree or an upstream
   that pins itself.

## Migration status

| Before | Now | Status |
|---|---|---|
| `LiveHtml` parsed CSS strings every frame | Maps htmlswap's typed lowering (`gpui_style`), cached per element and state | Done |
| `LiveHtml` had its own transition and animation engine | `gpui_base::motion` on the `gpui-pre` backend; end states on Zed | Done |
| htmlswap attached dynamic pseudo-classes to the subject element | State attached to the element written on (`ElementState { ancestor }`) | Done |
| View-transition elements inside `LiveHtml` | `gpui-view-transitions` crate on `gpui_base::motion`; `LiveHtml` keeps only CSS-to-style mapping and inert drawing of the old document | Done |
| `gpui-mcp-html` pinned one GPUI Kit release | `gpui-base >=0.6.4, <0.8`: a Kit release for every gpui-pre the bridge supports (0.3.5–0.3.7), each tested in CI | Done |
| htmlswap code generators target `gpui` 0.2.2 and `gpui-component` 0.5.1 | Add a GPUI Kit 0.7 (gpui-pre 0.3.7) target | Planned |
| gpui-studio on Zed `gpui` 0.2.2, themes via generated CSS strings | GPUI Kit on gpui-pre; themes as CSS custom properties | Planned |
