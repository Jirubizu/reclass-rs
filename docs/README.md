# reclass-rs documentation

Start here. The [top-level README](../README.md) is the overview; these pages are
the detail.

| Page | What's in it |
|---|---|
| [Getting started](getting-started.md) | Prerequisites, ptrace permission, install, build, CLI flags |
| [Playground tour](../examples/playground/README.md) | Guided first session against a purpose-built C target |
| [User guide](user-guide.md) | Every node type, the UI, address expressions, keys, search, pointer scan, ReClass.NET interop, settings |
| [MCP server](mcp.md) | Driving reclass-rs from an AI agent over the Model Context Protocol |
| [Plugin authoring](plugins.md) | The ABI contract, discovery, hooks, `PluginAction`, settings, a working minimal plugin |
| [Architecture](architecture.md) | Crate layout, the `MemoryBackend` trait, the render loop, feature flags |
| [Development](development.md) | Tests, benches, lints, docs, CI, release process |
| [`vmem` API mapping](vmem-api.md) | Which `vmem` call backs each backend capability, and its gotchas |
| [Changelog](../CHANGELOG.md) | Notable changes per release |

## API reference

Every crate is documented inline, and `reclass-core`, `reclass` and
`reclass-backend-vmem` all `#![warn(missing_docs)]`, so the rustdoc is the
authoritative API surface:

```sh
cargo doc --workspace --all-features --no-deps --open
```

`reclass-core` is the entry point for embedding: [`class`], [`node`], [`expr`],
[`engine`], [`codegen`] and [`scan`] are independent of any UI.

[`class`]: ../crates/core/src/class.rs
[`node`]: ../crates/core/src/node.rs
[`expr`]: ../crates/core/src/expr.rs
[`engine`]: ../crates/core/src/engine.rs
[`codegen`]: ../crates/core/src/codegen/
[`scan`]: ../crates/core/src/scan.rs
