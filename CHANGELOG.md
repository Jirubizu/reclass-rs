# Changelog

Notable changes per release. Older entries are the generated release notes on
[GitHub Releases](https://github.com/Jirubizu/reclass-rs/releases); this file
starts at 0.6.0.

Versions follow semver against the `reclass-core` public API. The `.ron`
project format is forward-compatible: a file written by an older version loads
in a newer one, with new fields taking their defaults.

## 0.7.1

### Changed

- **`vmem` bumped to `v0.1.13`** (was the unreleased rev `29c5cf94`), and the
  dependency is now pinned to a tag instead of a raw revision. Upstream split
  its monolithic `lib.rs` into modules, added an optional `static` feature
  (offline ELF/AOB inspection, which we do not enable), hardened the
  `write_force` fallback and ptrace stepping, and grew three `Error` variants.
  No API we use changed; the new variants fall through to
  `MemError::Backend` as before.
- Generated projects (**Project generator**) pin `vmem` to `tag = "v0.1.13"`
  rather than tracking the default branch, so a generated `Cargo.toml` keeps
  building when upstream moves.

## 0.7.0

### Removed

- **The ratatui terminal front-end (`--tui`).** It was a second view over
  `AppState` that had to be kept in step with the egui one for every model
  change, while never reaching parity — no find/go-to, pointer scan, plugins, or
  MCP. `reclass` now builds one front-end; the `tui` feature and the `ratatui` /
  `crossterm` dependencies are gone. The last version with it is on the `tui`
  branch.
- **The `gui` feature.** With the TUI gone it gated the only front-end: a
  `--no-default-features` build compiled a binary that refused to start. `eframe`,
  `egui`, `libloading` and `parking_lot` are now unconditional dependencies, and
  a plugin's `Cargo.toml` is one line shorter — `reclass = { path = "..." }`,
  with no `default-features = false, features = ["gui"]` incantation.

### Changed

- **Documentation split out of the README** into [`docs/`](docs/README.md):
  getting started, user guide, MCP server, plugin authoring, architecture, and
  development. The README is now an overview plus an index; nothing was dropped.
- `reclass-core` sets `#![warn(missing_docs)]`, so all three library surfaces do.

### Fixed

- Ten broken or private rustdoc intra-doc links (`reclass_plugin_create!`,
  `ClassRegistry`, the private `gui` submodules, the `smoke` usage block).
  CI now runs `cargo doc --workspace --all-features` with `-D warnings`, so a
  broken link fails the build instead of silently shipping.

## 0.6.1

### Fixed

- **Scrollbars parked mid-window** in the ReClass.NET conversion-notes window
  and the pointer-scan results list. Both scroll areas kept egui's default
  `auto_shrink([true, true])`, so they sized to their widest row instead of the
  window, which also clipped the text behind the misplaced scrollbar. Now
  `[false, true]`, matching every other list in the app.

### Changed

- CI runs tests through `cargo nextest run` instead of `cargo test`.

## 0.6.0

### Added

- **Three node kinds.** `Enum` (an integer with a named-variant table, edited
  in the Type menu), `Bits8/16/32/64` (an integer shown as MSB-first binary
  octets), and `Text*`/`WText*` (a `char*`/`char16_t*` the engine follows to
  show the string inline). Every followed string in a tick batches into one
  extra scatter read.
- **32-bit target support.** *View → Target pointer width* switches the project
  between 32- and 64-bit pointers. Pointer reads, value writes, layout, and
  codegen all follow it; the setting persists with the project.
- **Undo / redo** (`Ctrl+Z` / `Ctrl+Shift+Z`, or the Edit menu) over every
  structural edit, including multi-select delete as a single step. Also exposed
  as `undo` / `redo` MCP tools.
- **Copy / paste fields** (`Ctrl+C` / `Ctrl+V`) between classes, with
  `copy_nodes` / `paste_nodes` MCP tools.
- **Find and Go to.** `Ctrl+F` filters the table across every column; Go to
  scrolls to the field containing an address.
- **Pointer scanner** (*View → Pointer scan*): finds `<module>+0xBASE -> +0xOFF`
  chains to an address and writes the winner into a class's address bar.
- **ReClass.NET `.rcnet` import and export** (*File* menu, and `import_rcnet` /
  `export_rcnet` MCP tools). Approximations are reported, never silent.
- **`set_pointer_width` MCP tool.**
- **Plugin authoring guide** at [`docs/plugins.md`](docs/plugins.md), plus a
  README for `crates/example-plugin`.
- **Benchmarks** for undo snapshots (`benches/history.rs`) and pointer scanning
  (`benches/scan.rs`).

### Fixed

- A partially-read `ClassPtr` slot was followed. The frame buffer is zero-filled
  past the readable prefix, so a pointer with two mapped bytes assembled an
  address from two real bytes and two padding zeros, then scatter-read wherever
  that landed. Pointer reads now require the whole slot inside the readable
  prefix.

### Changed

- `NodeKind::fixed_size` and `NodeKind::parse_edit` take the target's pointer
  width. **Breaking** for direct `reclass-core` users; the app and plugins go
  through `NodeKind::size` and `AppState`, which thread it for you.
- Codegen emits a fixed-width integer instead of `void*` / `*mut T` when the
  target's pointer width differs from the host's, so generated struct offsets
  still match the live layout.
- `reclass-core` gained an optional, off-by-default `rcnet` feature (its only
  new dependency, `flate2`, is behind it).

### Known gaps

- `crates/app/src/gui/table.rs` and `panels.rs` have no tests; they are pure
  egui rendering. The logic behind them lives in tested modules.
- Find / Go to are egui-only; the `--tui` front-end does not have them.
- The pointer scanner has no cancellation — an abandoned run finishes and its
  result is discarded.
- `.rcnet` conversion is verified against hand-built fixtures matching the
  documented format, not against a file from a real ReClass.NET install.
- The address-bar `[…]` deref always reads 8 bytes, so on a 32-bit target it
  resolves correctly only when the 4 bytes after the pointer are zero. Pointer
  *fields* do honour the project's width.
