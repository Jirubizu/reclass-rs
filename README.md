# reclass-rs
---
![docs/example.png](docs/example.png)

---
A native-Linux, [ReClass.NET](https://github.com/ReClassNET/ReClass.NET)-style **live memory inspector** for reconstructing the in-memory layout of a running process — written in Rust, with no Mono/WinForms.

You define a *class* as an ordered list of typed *fields*; reclass-rs resolves a base address, re-reads the target's memory a few times a second, and renders each field's **offset / address / type / name / value / raw bytes** with inline editing. Point it at a process, build up structs interactively, follow pointer chains, and export the result as C / C++ / Rust.

> ⚠️ **Linux host, x86-64 only. Userspace only.** 32-bit *targets* are supported; the host build is x86-64. This is a research/RE tool. Only use it on processes you own or are authorized to inspect. See [Legal & ethics](#legal--ethics).

---

## Documentation

| Page | What's in it |
|---|---|
| **[Getting started](docs/getting-started.md)** | Prerequisites, ptrace permission, install, build, CLI flags |
| **[Playground tour](examples/playground/README.md)** | Guided first session against a purpose-built C target |
| **[User guide](docs/user-guide.md)** | Every node type, the UI, address expressions, keys, search, pointer scan, ReClass.NET interop, settings |
| **[MCP server](docs/mcp.md)** | Driving reclass-rs from an AI agent (pairs with IDA Pro MCP) |
| **[Plugin authoring](docs/plugins.md)** | ABI contract, discovery, hooks, `PluginAction`, a working minimal plugin |
| **[Architecture](docs/architecture.md)** | Crate layout, the `MemoryBackend` seam, feature flags |
| **[Development](docs/development.md)** | Tests, benches, lints, docs, CI, releases |
| **[Changelog](CHANGELOG.md)** | Notable changes per release |

API reference: `cargo doc --workspace --all-features --no-deps --open`.

---

## Highlights

- **Live, batched reads.** One scatter read (`process_vm_readv`) per pointer-chain level per tick — never one syscall per field. Partial reads are tolerated, so a class that overruns its mapping still shows the mapped prefix.
- **Full ReClass-style node set** — hex, signed/unsigned ints, floats, `Bool`, `Vec2/3/4`, `Text`/`WText` (followed and shown inline), `Pointer`, `FunctionPtr`, `Array[N]`, inline `ClassInstance`, `ClassPtr`, `Padding`, `Unknown`, `Enum` with a named-variant table, and `Bits8/16/32/64` binary views. [Details](docs/user-guide.md#node-types).
- **32-bit targets** — pointer width is a property of the project: reads, edits, offsets, and codegen all narrow. [Details](docs/user-guide.md#32-bit-targets).
- **Address expressions** — `<module.so> + 0x10`, `[0xADDR]`, `[<module> + 0x10] + 0x20`, with `+ - * /`.
- **Two front-ends over one core** — an egui desktop UI (default) with a virtualized table, collapsible pointers/arrays/instances, multi-select, inline editing and value-change flashing; and a ratatui terminal UI (`--tui`).
- **Undo/redo, copy/paste** over every structural edit — including edits made by plugins and MCP agents, which mutate the same state. [Details](docs/user-guide.md#undo-copy-and-paste).
- **Find and Go to** — filter by name/type/value/comment/address, or jump to the field *containing* an address. [Details](docs/user-guide.md#find-and-go-to).
- **Pointer scanner** — given an address, find the `<module>+0xBASE -> +0xOFF -> …` chains that reach it, shortest first. [Details](docs/user-guide.md#pointer-scanner).
- **ReClass.NET interop** — reads and writes real `.rcnet` files, with every lossy approximation listed rather than applied silently. [Details](docs/user-guide.md#reclassnet-interop).
- **Code generation** to C, C++, and Rust (`#[repr(C, packed)]`) with offsets as comments; generated Rust's `size_of`/`offset_of` match the model.
- **Plugins** — native `.so`s that observe every snapshot and request mutations, with an enforced same-toolchain ABI check. Eight ship in the box. [Details](docs/plugins.md).
- **MCP control server** (loopback, off by default) — hands an AI agent full control, so it can read code in IDA and **build the matching structs here, live**. [Details](docs/mcp.md).
- **In-app updates**, **process picker**, **memory-map view**, RON **project save/load** with auto-reattach, and a persisted **settings** window.

## Try it — the playground

A self-contained C target with a live-mutating `Player` struct (and a `Weapon` it points to) lives under [`examples/playground`](examples/playground/), with a full **[guided tour](examples/playground/README.md)**. Build it, attach, and rebuild the struct live — no game, no anti-cheat, default ptrace settings:

![reclass-rs inspecting the playground](examples/playground/img/typed.png)

## Quick start

```sh
# from a release tarball
tar xzf reclass-linux-x86_64.tar.gz && ./reclass

# from source
cargo run --release -p reclass -- --pid 1234 --addr 0x5A3518
cargo run --release -p reclass -- --tui --pid 1234
```

Reading another process needs ptrace permission — for development,
`sudo sysctl -w kernel.yama.ptrace_scope=0`. Full detail in
[Getting started](docs/getting-started.md).

---

## Legal & ethics

reclass-rs reads and **writes** another process's memory. Doing so can corrupt or crash the target. Only use it on software you own or have explicit permission to analyze, and respect the EULA/Terms of the programs you inspect. This project is for reverse-engineering, debugging, and education — not for cheating in online games or any unauthorized tampering. You are responsible for how you use it.

## License

MIT — see [`LICENSE`](LICENSE). The `vmem` dependency is dual-licensed MIT OR Apache-2.0.

## Acknowledgements

- UX inspired by [ReClass.NET](https://github.com/ReClassNET/ReClass.NET).
- Memory backend powered by [`vmem`](https://github.com/Jirubizu/vmem).
