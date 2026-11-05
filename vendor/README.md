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
