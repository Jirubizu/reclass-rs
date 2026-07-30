# Development

## Quality bar

- Edition 2024, stable toolchain.
- `#![forbid(unsafe_code)]` in `reclass-core`; every other crate sets
  `#![deny(rust_2018_idioms)]`, and all three library surfaces
  (`reclass-core`, `reclass`, `reclass-backend-vmem`) set
  `#![warn(missing_docs)]` — the rustdoc is part of the build, not an
  afterthought.
- Errors: `thiserror` in libraries, `anyhow` only in the app.
- `cargo fmt --all --check` and
  `cargo clippy --all-targets --all-features -- -D warnings` are clean.
- Every `core` module ships unit tests.

## Tests

```sh
cargo nextest run --workspace --all-features  # full suite, as CI runs it
cargo test --workspace --all-features         # same suite without nextest installed
```

The suite includes live tests that spawn a helper child process and read its
memory for real; they **self-skip** if `ptrace` is denied rather than failing, so
a restricted machine still gets a green run:

| Test | What it proves |
|---|---|
| [`backend-vmem/tests/live_read.rs`](../crates/backend-vmem/tests/live_read.rs) | real reads against a spawned target |
| [`backend-vmem/tests/live_scan.rs`](../crates/backend-vmem/tests/live_scan.rs) | the pointer scanner finds a real static chain |
| [`app/tests/live_app.rs`](../crates/app/tests/live_app.rs) | attach → resolve → render → edit → follow a `ClassPtr`, sans pixels |
| [`app/tests/plugin_load.rs`](../crates/app/tests/plugin_load.rs) | builds the reference cdylib, `dlopen`s it, drives its hooks across the boundary |
| [`app/tests/mcp_smoke.rs`](../crates/app/tests/mcp_smoke.rs) | real JSON-RPC over a real loopback socket |
| [`app/tests/rcnet_roundtrip.rs`](../crates/app/tests/rcnet_roundtrip.rs) | a project survives a `.rcnet` round-trip on disk |

`crates/app/src/gui/table.rs` and `panels.rs` have no tests — they are pure egui
rendering, and the logic behind them lives in tested modules
(`gui/search.rs`, `gui/flash.rs`, `app_state.rs`).

## Benchmarks

```sh
cargo bench -p reclass-core --all-features
```

| Bench | Measures |
|---|---|
| `engine` | that reads are batched — a flat 256-byte / 64-field class costs **one** scatter call per tick; a depth-4 pointer chain costs four (one per level) |
| `history` | an undo snapshot of a large project (~1.7 MB), which is what bounds the undo stack |
| `scan` | the pointer scanner per depth (~66 ms at depth 4, ~660 ms at depth 8 over 1 MiB) |

CI compile-checks every registered bench (`--no-run`): a bench that stops
compiling is a broken test nobody runs until they need it.

## Documentation

Prose lives in [`docs/`](README.md) and is plain Markdown — GitHub renders it,
and the relative links work in an editor too. API documentation is rustdoc:

```sh
cargo doc --workspace --all-features --no-deps --open

# treat broken intra-doc links as errors, as CI does
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Where a doc claims a number or a behaviour, it names the test or bench that
holds it up. Keep that property: a claim without a reference rots silently.

## CI

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml), on every push and PR:

1. `cargo fmt --all --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo nextest run --workspace --all-features` (with
   `kernel.yama.ptrace_scope=0` so the live tests actually run)
4. `cargo doc` with `-D warnings` — broken intra-doc links fail the build
5. `cargo bench --no-run`

## Releases

A pushed `v*` tag additionally runs, gated on the test job passing:

- **release-build** — one `cargo build --release --package reclass --package
  reclass-official-plugins`. One invocation on purpose: the plugin loader
  compares an ABI fingerprint derived from `rustc --verbose --version`, so a
  bundle built in a separate job would refuse to load. Packages
  `reclass-linux-x86_64.tar.gz` (binary + `plugins/`) plus a `.sha256`.
- **create-release** — generates release notes from `git log` between the
  previous `v*` tag and this one, and publishes the artifacts.

Notable changes also go in [`CHANGELOG.md`](../CHANGELOG.md) by hand, under a
version heading with `Added` / `Changed` / `Fixed` / `Known gaps` sections. The
generated notes are the commit list; the changelog is the curated version.
