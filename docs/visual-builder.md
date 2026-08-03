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
2. **Create a project.** Run `gpui-mcp new <name>`. The scaffold produces the
   bundle and a working GPUI/MCP host without overwriting an existing path.

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

The initial live CSS subset covers flex layout, fixed-count equal-track grid,
spacing, pixel dimensions, background/text/border colors, font family,
size/weight/line height, and border radius. Single `:hover`, `:focus`, and
`:active` variants map to GPUI's native interactive refinements. Standard
`<details>` elements retain open/closed disclosure state and publish it through
the semantic tree. Unsupported properties, lengths, unequal grid tracks,
combined conditions, dynamic rules, and pseudo-elements remain in the document
and produce `RenderDiagnostic` entries rather than disappearing silently. A
builder should show these diagnostics next to the source.

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
