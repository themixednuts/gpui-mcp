# Pure HTML visual-builder contract

The builder's durable source is a small project bundle, not a serialized GPUI
element tree:

```text
ui/
  app.html             standard structural and semantic HTML
  app.css              local standard CSS
  app.bindings.ron     exact event and state connections
src/
  main.rs              GPUI host, hook registry, and custom components
```

This shape supports both ways of adopting the system:

1. **Hook into an existing GPUI application.** Compile an `HtmlUi`, register
   application callbacks/state in `HookRegistry`, optionally register custom
   elements in `ComponentRegistry`, and retain `LiveHtml` plus `BridgeHandle`
   in the owning view.
2. **Create a project.** A host such as GPUI Studio calls the
   `gpui-mcp-html` project-generation API. It produces the bundle and a working
   GPUI/MCP host without overwriting an existing path.

The renderer runs on Zed's GPUI (the default `zed` feature) and on the
`gpui-pre` release GPUI Kit uses (`default-features = false, features =
["gpui-pre", "json", "ron"]`). Generated projects use the Zed backend.

## Why HTML, RON, and JSON each have a separate job

HTML is the visual document language. It already has useful structure, form
semantics, IDs, labels, ARIA roles, and a mature parser. `htmlswap` parses it
and lowers it into a target-neutral `RenderPlan`; `gpui-mcp-html` interprets
that plan live instead of generating Rust source.

RON is the checked-in behavior document because enum variants and newtypes are
readable without JSON's object tagging noise. JSON remains available for tools
and import/export. MCP continues to use JSON-RPC on stdio. Choosing RON for a
project file does not create a second runtime protocol: both RON and JSON feed
the same versioned `BindingDocument`, validation rules, and hook registry.

## Source policy

The GPUI integration always selects `SourcePolicy::pure_html()` and callers
cannot weaken it through `HtmlUi`. Its file and network resolvers are disabled;
stylesheets must be supplied explicitly by the host. The compiler rejects:

- every `data-htmlswap-*` attribute;
- `<script>`, `<iframe>`, `<object>`, and `<embed>`;
- inline `on*` attributes and `javascript:` URLs;
- HTTP(S) and protocol-relative element resources;
- remote or scripted compiler assets.

Validation is fail closed: an error prevents construction of `HtmlUi`; the
compiler does not return a partially trusted render plan for execution.

The live CSS subset covers flex layout, CSS grid (track lists with lengths,
percentages, `fr`, `auto`, `min-content`, `max-content`, `minmax()`,
`fit-content()` and `repeat(n | auto-fill | auto-fit, …)` for
`grid-template-columns` and `grid-template-rows`; `grid-auto-columns`,
`grid-auto-rows` and `grid-auto-flow`; line and span placement with
`grid-column`, `grid-row` and their `-start`/`-end` longhands; `row-gap` and
`column-gap`), spacing, pixel dimensions, background/text/border colors, font
family, size/weight/line height, and border radius. Single `:hover`, `:focus`, and
`:active` variants map to GPUI's native interactive refinements. Standard
`<details>` elements retain open/closed disclosure state and publish it through
the semantic tree. Unsupported properties, lengths, named grid lines and areas,
combined conditions, dynamic rules, and pseudo-elements remain in the document
and produce `RenderDiagnostic` entries rather than disappearing silently. A
builder should show these diagnostics next to the source.

## Mapping the canvas back to source

An editor needs to go from a rendered element to its markup and back, and most
elements have no authored `id`. `LiveHtml::source_map()` maps every rendered
element of the active revision to its semantic id (the id in the MCP tree,
including any embedding namespace), its document id (authored, or a generated
`html-node-…` id derived from its position), tag, position path, parent, and
the byte range, line and column of its markup in the HTML source.
`SourceMap::get(semantic_id)` resolves a selection on the canvas, and
`SourceMap::at_offset(byte)` resolves an editor caret to the innermost element.

Each rendered node also carries `source_span` metadata (`start..end`) in the
semantic tree, so an MCP agent can map `get_ui_tree` nodes back to source
without the Rust API. Generated ids change when an element moves; spans are
per revision. Give an element an `id` when its identity must survive edits.

## Binding document

Version 1 has two binding kinds:

```ron
(
    version: 1,
    bindings: [
        Event(
            target: Id("save"),
            event: Click,
            handler: "save_document",
        ),
        Property(
            target: Id("title"),
            property: Value,
            source: "document_title",
            mode: TwoWay,
        ),
    ],
)
```

Event kinds are `Click`, `Focus`, `Hover`, `Change`, `Input`, and `Submit`.
Bindable properties are `Text`, `Value`, `Checked`, `Selected`, `Disabled`,
and `Visible`. Property flow is `OneWay` by default or explicitly `TwoWay`.

Bindings use exact `id` targets. CSS selectors are intentionally excluded from
action routing because a selector can change meaning when the document changes.
The compiler rejects duplicate IDs, missing targets, bindings incompatible with
the target element, duplicate event/property slots, excessive documents, bad
identifiers, unknown schema versions, and IDs reserved for renderer-generated
semantic nodes.

The RON/JSON document names capabilities; it never embeds Rust, JavaScript, or
an expression language. Every handler and state symbol must exist in
`HookRegistry`, and every two-way property needs a writer, before `LiveHtml`
can be created.

## Components and elements

Ordinary HTML maps to semantic GPUI containers and MCP roles. Reusable native
widgets use standard custom-element syntax:

```html
<document-card id="active-document" aria-label="Active document">
  <h2>Quarterly report</h2>
</document-card>
```

The Rust application registers the matching tag:

```rust,ignore
components.register("document-card", |node, children, window, cx| {
    render_document_card(node, children, window, cx)
})?;
```

Custom names must be lowercase and contain a hyphen, following the web custom
element convention. The factory receives an owned ID/tag/attribute snapshot,
already-rendered children, and foreground-thread GPUI contexts. It returns a
normal `AnyElement`, while the outer host retains the document's stable MCP
identity, styles, bindings, and semantic metadata.

This boundary lets a builder create and rearrange pure HTML while the
application owns privileged behavior and native component implementation.
Unknown custom elements still render their children, so source remains
inspectable while a component is being implemented.

## Motion

Motion is written in standard CSS and runs on GPUI's animation frames, on
both backends. Elements that declare no motion do no per-frame motion work,
and rules are parsed once per element and interaction state.

**Transitions.** `transition` (and its longhands) animates `color`,
`background-color`, `border-color`, `opacity`, `width`, `height` and
`translate` (or `transform: translate(…)`) whenever their computed value
changes: on `:hover`, `:focus`, `:active`, media changes, or a bound
`width`/`height`. A transition interrupted part-way reverses from the value on
screen, as in browsers. `@starting-style` gives entry transitions on an
element's first render. Easing supports the keywords, `cubic-bezier()` and
`steps()`. `translate` moves an element in prepaint, so layout is unchanged
while hit testing and semantic bounds follow it.

**Animations.** `animation` (and its longhands) plays `@keyframes` over the
same properties, with delays, iteration counts, `infinite`, direction and
fill mode. Keyframe selectors may set `animation-timing-function`.

**Reduced motion.** `@media (prefers-reduced-motion: reduce)` follows GPUI's
reduce-motion setting.

**View transitions.** Elements with a `view-transition-name` animate between
two states: a group moves each one from its old box to its new box, while the
old and new images cross-fade, and everything else (`root`) cross-fades as
well. `view-transition-name: none` opts out, and `auto`/`match-element` name
an element by its id. A transition starts in one of two ways:

```rust,ignore
// Same document: capture, then change state; the next frame animates.
live.start_view_transition(["slide"], window, cx);
app_state.update(cx, |state, _| state.page = Page::Detail);

// Navigation: both documents declare `@view-transition { navigation: auto; }`.
live.reload(next_document)?;
```

Calling `reload` while a same-document transition is pending makes the new
document its incoming state. While the transition runs, its types match
`:active-view-transition-type()` in any selector, for example
`main:active-view-transition-type(slide) .title`; `@view-transition { types: … }` adds types to navigations.
`skip_view_transition` ends one at once, and `view_transition_running` reports
whether one is pending or running.

The pseudo-elements take author rules as in browsers:
`::view-transition-group(name)`, `::view-transition-old(name)` and
`::view-transition-new(name)`, with `*`,
`root` and `.class` (from `view-transition-class`) selectors. Group timing
defaults to `250ms ease`; `animation: none` on a group makes it jump. Old and
new images play `@keyframes` such as fades and slides (`opacity` and
`translate`), inheriting the group's timing unless they set their own.

GPUI keeps no pixels between frames, so the old state is drawn by rendering the
previous document again, inertly: same styles and bound values, but no ids,
handlers or focus, and hidden from semantic snapshots. The new state is the
live document, which stays interactive throughout. A group moves its element
rather than scaling it; an old image is stretched to the group's box, and the
new element keeps its own size. A name used by more than one element is not
transitioned, while the rest of the transition proceeds. Custom components
draw only their children in old images.

## Builder operations

A visual editor should modify the source bundle through structured operations,
then recompile and publish diagnostics after each transaction:

- insert, move, remove, or replace an HTML element;
- set/remove a standard attribute, class, text node, or CSS declaration;
- add/remove an exact binding;
- list registered component tags and hook symbols supplied by the host;
- preview, inspect the MCP semantic tree, and undo the source transaction.

Those operations should preserve formatting and use revision checks so stale
edits fail instead of overwriting newer source. Filesystem writes should stay
in a project root explicitly selected by the user. They are an authoring API,
separate from the existing runtime MCP control tools, which deliberately cannot
read or write arbitrary host paths.

The key rule is that the HTML/RON bundle remains authoritative. GPUI elements
are regenerated views of it, and MCP tree snapshots are observations—not a
second document model to reconcile.

## Live development contract

`LiveHtml::reload` validates and indexes a complete candidate before changing
the active renderer. Successful replacements increment a document revision,
retain focus/disclosure/hover caches for stable HTML IDs, and prune deleted IDs.
A failed hook validation leaves the previous document and revision untouched.

`ProjectWatcher` watches the canonical `ui/` directory non-recursively through
the platform-recommended backend so editor atomic-save renames work on Windows,
Linux, and macOS. It filters events to the three exact project files, uses a
bounded queue, converts overflow into a full rescan, and revalidates path
containment before each read. Invalid changed bundles leave the last-good UI.

An app may explicitly enable the bridge's `live_document` capability and
register `LiveHtmlSession`. MCP then exposes `get_live_document` and
`preview_live_document`. Preview accepts a bounded complete HTML/CSS/RON bundle
and `expected_revision`, never writes files, and returns structured candidate
diagnostics. Stale revisions conflict instead of overwriting another manual,
filesystem, or AI edit.

The concrete Studio design built on this contract is in
[`gpui-studio.md`](gpui-studio.md).
