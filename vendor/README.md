# Local dependency patches

## gpui-pre-linux 0.3.10

Source: the crates.io `gpui-pre-linux` 0.3.10 release (Apache-2.0).
`Cargo.toml`, `LICENSE-APACHE`, and `src/` are copied from that release.

The X11 backend previously discarded `update_ime_position` calls while composing.
Rockstar now requests an IME coordinate update after the focused chat input is
painted. Those updates must also reach XIM while composing, so that wrapping,
scrolling, and resizing cannot leave the candidate window at an old position.

Changes in `src/linux/x11/client.rs`:

- Allow position updates during composition.
- Extract the logical-to-physical XIM spot conversion, preserving signed
  coordinates and saturating at the protocol's signed 16-bit limit.
- Test cursor movement and display scaling in that conversion.

The application-side notification lives in `src/chat/ime.rs`.

Run the backend regression tests with:

```sh
cargo test -p gpui-pre-linux ime_spot
```

Remove this patch when upgrading to a release that allows post-layout IME
position updates during composition, and rerun the X11 integration test.

## gpui-base 0.8.1

Source: the crates.io `gpui-base` 0.8.1 release (Apache-2.0). The original
manifest, license, sources, tests and benches are retained. Cargo's registry
cache is not modified; the root manifest selects this copy with `[patch.crates-io]`.

The native editor is still used for editing, completion, hover, diagnostics and
syntax highlighting. These local extensions provide the missing display layer:

- `EditorAnnotation` / `set_annotations`: inline labels and clickable rows above
  source lines, without inserting text into the rope or undo history.
- Display-map block rows and inline-fragment shaping: cursor, selection, hit
  testing, scrolling and IME coordinates include the annotations. Fileview uses
  unwrapped source lines; annotation-aware soft wrapping is not implemented.
- `AnnotationClick` returns an opaque host action ID. Rockstar resolves lens
  labels during analysis and executes the command only on a click.
- `set_semantic_tokens` accepts host-fetched tokens; Rockstar converts negotiated
  LSP coordinates to the editor's Unicode-scalar columns first.
- Completion acceptance checks the document revision and cursor, validates
  ranges, and applies the primary/additional edits against one source snapshot.
- `is_composing` lets the host avoid displaying analysis for committed text over
  temporary IME preedit. Annotations invalidated by preedit are restored after
  cancellation or refreshed after commit.
- `set_selection_reversed` lets FileView place the caret at the active end of a
  Vim Visual selection, including a row in a block selection, without changing
  the selected text or removing additional selections.

Markdown's inline parser also gives media extensions a complete paired
`<audio>`/`<video>` element, including `<source>` children and fallback text.
It preserves the full source span for selection/copy and retains the previous
HTML handling when no extension claims the element. Rockstar's integration is
in `src/media/markdown.rs`; the documentation media test exercises copying and
source-relative loading through the production renderer.

Application integration and protocol adapters are in `src/lsp/` and
`src/file_view/lsp.rs`. Tests and the reviewed screenshot are described in
`tests/README.md`. Re-run both the production UI suite and
`python3 tests/ime_x11.py --lsp` when updating this dependency. Remove the patch
once equivalent APIs and coordinate handling are available upstream.
