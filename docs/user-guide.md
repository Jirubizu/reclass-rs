# User guide

reclass-rs re-reads the target's memory a few times a second and renders each
field's **offset / address / type / name / value / raw bytes**, with inline
editing that writes back. This page covers the model, the UI, and every feature
in detail. For a first, hands-on walkthrough use the
[playground tour](../examples/playground/README.md).

- [The basic loop](#the-basic-loop)
- [Address expression syntax](#address-expression-syntax)
- [Node types](#node-types)
- [32-bit targets](#32-bit-targets)
- [Mouse and keys](#mouse-and-keys)
- [Undo, copy and paste](#undo-copy-and-paste)
- [Find and Go to](#find-and-go-to)
- [Pointer scanner](#pointer-scanner)
- [ReClass.NET interop](#reclassnet-interop)
- [Projects](#projects)
- [Code generation](#code-generation)
- [Settings](#settings)
- [Terminal front-end](#terminal-front-end)
- [Access tracker](#access-tracker)

## The basic loop

1. **Attach** — type a PID and click *Attach*, or pick a process from the list
   (filter by name).
2. **Set an address** — type an expression in the address bar (see below). The
   `= 0x…` indicator turns **green** when it resolves into a readable region,
   **yellow** if unmapped, **red** on a parse/deref error.
3. **Build the class** — use *Add field* / *Add bytes* / the *Array* builder, or
   **left-click a field's Type** to change it. Memory shows live; **changed
   values flash** and fade.
4. **Edit** — double-click a value/name/comment to edit it; value edits are
   written back to the target.
5. **Follow pointers** — expand a `Ptr`/`ClassPtr` (▶) to follow it; right-click
   a pointer → *Add bytes to target* to grow the pointed-to class without
   opening it.
6. **Save/Load** — *File → Save / Save as… / Open project…* open an in-app file
   browser (filters to `*.ron`); *File → Open recent* lists your last projects.
7. **Export** — *View → Code generation* dumps the registry as C / C++ / Rust.

**Reads are batched.** The render loop gathers every visible address and issues
**one** scatter read per pointer-chain level (`process_vm_readv`) — never one
syscall per field. Partial reads are tolerated, so a class that overruns its
mapping still shows the mapped prefix. Each class reads at most 1 MiB per tick;
past that a field shows `???` rather than letting one mistyped array count stall
the UI.

Derived offsets recompute and re-cache on every structural edit. Inline
`ClassInstance` cycles are detected and rejected; `ClassPtr` cycles are fine —
they are a read boundary.

## Address expression syntax

| Expression | Meaning |
|---|---|
| `0x5A3518` | absolute address |
| `<module.so> + 0x10` | module load base + offset |
| `[0xADDR]` | pointer-sized dereference |
| `[<module> + 0x10] + 0x20` | nested deref then offset |
| `+ - * /` | integer arithmetic |

> **PIE vs non-PIE:** for a position-independent binary, IDA addresses are RVAs
> → use `<module> + rva`. For a fixed-base (`ET_EXEC`) binary, IDA shows
> absolute addresses → use them directly (`[0x5A3518]`) or subtract the image
> base (`0x400000`) before adding the module base.

## Node types

The full ReClass-style set: `Hex8/16/32/64`, signed/unsigned ints, `Float`,
`Double`, `Bool`, `Vec2/3/4`, `Text`/`WText`, `Pointer`, `FunctionPtr`,
`Array[N]`, inline `ClassInstance`, `ClassPtr`, `Padding`, `Unknown`, plus the
assembly size keywords (`byte/word/dword/qword/tword/oword/yword/zword`).

Three deserve their own note:

- **`Enum`** — an integer with a named-variant table, edited in the Type menu
  (`NAME = VALUE` per line, decimal or `0x`). Values show as `Idle (0)`; typing a
  variant name writes its value. Codegen emits the storage integer plus a
  `// enum:` comment, never a real `enum`: a foreign process can hold any bit
  pattern, and materializing an out-of-range discriminant is undefined
  behaviour.
- **`Bits8/16/32/64`** — an integer displayed as MSB-first binary octets
  (`00000001 00000010`). Edits accept binary, `0x` hex, or decimal; bare digits
  are binary, so retyping what is on screen means the same value.
- **`Text*`/`WText*`** — a `char*` / `char16_t*`. The engine follows it and shows
  the string inline (`0x2000 -> "Player One"`), batching every followed string in
  the tick into one extra scatter. `max` bounds the read so a garbage pointer
  cannot request a huge one.

## 32-bit targets

*View → Target pointer width* switches the project between 32- and 64-bit
pointers. It is a property of the target, not the app: every
`Pointer`/`ClassPtr`/`FunctionPtr`/`Text*` narrows to 4 bytes and every offset
after one shifts. The engine reads pointers at that width, edits write at that
width (an address that does not fit is rejected rather than truncated), and
codegen emits a fixed-width integer instead of a host-width `void*`/`*mut T`, so
the generated struct's offsets still match the live layout. Persisted with the
project; a project written before this existed loads as 64-bit.

> **Known gap:** the address bar's `[…]` deref always reads 8 bytes, so on a
> 32-bit target it only resolves correctly when the 4 bytes following the pointer
> happen to be zero. Type the pointer as a `Pointer`/`ClassPtr` field and expand
> it instead — those do honour the width.

## Mouse and keys

- **Click offset cell** — select row · **Ctrl-click** toggle · **Shift-click**
  range · **Delete** removes selected.
- **Left-click Type** — change type · **Right-click offset** — rename / insert /
  delete / add-bytes-to-target.
- **▶/▼** — expand/collapse arrays, class instances, and pointers.
- **Expand all / Collapse all** in the toolbar; the **View** menu hides the
  Classes panel to focus on memory.
- Multi-select also works on the class list. The table is virtualized, so it
  stays smooth with thousands of fields, and scrolls horizontally.

## Undo, copy and paste

**Undo / redo** (`Ctrl+Z` / `Ctrl+Shift+Z`, or the **Edit** menu) covers every
structural edit — including a multi-select delete, which is one step, not one per
row. Plugins and MCP agents mutate through the same `AppState`, so their changes
are undoable too.

It is implemented as whole-project snapshots rather than inverse operations:
deleting a class rewrites references across every other class, so nothing smaller
reverses one edit reliably. Bounded by both snapshot count and total node count,
because a large project's snapshot is ~1.7 MB (see
[`crates/core/benches/history.rs`](../crates/core/benches/history.rs)).

**Copy / paste fields** (`Ctrl+C` / `Ctrl+V`, the **Edit** menu, or the row
context menu) moves fields between classes, keeping layout order however the
multi-select was clicked. A paste that would create an inline class cycle, or
that carries a reference to a class deleted since the copy, is refused whole
rather than landing halfway — the clipboard lives outside the registry, so
`remove_class`'s reference rewrite cannot reach it.

## Find and Go to

`Ctrl+F` filters the table by name, type, value, comment, or a rendered `0x`
offset/address. Matches keep their ancestor rows, so a hit inside an expanded
pointer is still shown under the field it belongs to.

**Go to** takes an address (or a bare-hex offset, or `123d` for decimal) and
scrolls to the field that *contains* it, not just an exact row match. With a
filter active it resolves against the filtered rows, so it never jumps to
something hidden.

egui front-end only — the `--tui` front-end does not have it.

## Pointer scanner

*View → Pointer scan*: given an address you found once, finds the
`<module>+0xBASE -> +0xOFF -> …` chains that lead to it, shortest first, and
pastes the winner straight into a class's address bar.

Chains terminate only on file-backed mappings — a pointer that lives on the
anonymous heap moves every run and is not a static path.

It runs on a worker thread that re-attaches by pid (`MemoryBackend` is not
`Send`), because it is not fast:
[`crates/core/benches/scan.rs`](../crates/core/benches/scan.rs) measures ~66 ms
over 1 MiB of pointer-dense heap at depth 4 and ~660 ms at depth 8 — each extra
hop multiplies the search, not the memory pass. Verified end to end against a
live process by
[`crates/backend-vmem/tests/live_scan.rs`](../crates/backend-vmem/tests/live_scan.rs).

## ReClass.NET interop

*File → Import / Export ReClass.NET (.rcnet)* reads and writes the real format —
a ZIP holding `Data.xml` — so the existing corpus of community structs is usable
here. The platform tag sets pointer width, the project-level enum table is
inlined onto the fields that use it, and class references resolve by GUID
regardless of declaration order.

**The mapping is not lossless and says so:** a union becomes raw bytes of the
same size, a vtable becomes that many function pointers, UTF-32 text becomes a
raw block. Every approximation is listed in a conversion-notes window rather than
applied silently. Import is undoable, so a mis-aimed one is a single `Ctrl+Z`.

## Projects

Projects are RON. The attached process **name** is saved and reconnected
automatically on load. The **process picker** and **memory-map view** are under
the same menus.

## Code generation

*View → Code generation* emits C, C++, or Rust (`#[repr(C, packed)]`), with
offsets as comments. Generated Rust's `size_of`/`offset_of` match the model —
there is a test that asserts it.

## Settings

*View → Settings*, persisted to `~/.config/reclass-rs/settings.ron`:

| Setting | Effect |
|---|---|
| Value-change highlight | colour, fade duration, on/off |
| Default field type | e.g. `Hex64` → `Int64` for new fields |
| Seed rows | how many rows a new class starts with |
| Max array elements | render cap for large arrays |
| Kernel backend | use `/dev/vmem` instead of `process_vm_readv`; ticking it without the module loaded says so and reverts |
| MCP control server | enable + port — see [MCP server](mcp.md) |
| Plugins | per-plugin enabled flag, window state, and each plugin's own configuration — see [Plugin authoring](plugins.md#settings-persistence) |

## Terminal front-end

`--tui` runs a ratatui UI over the same core. Keys: `q` quit · `↑/↓` move ·
`←/→` switch class tab · `space` expand/collapse a `ClassPtr` · `e` edit the
selected value · `a` edit the address expression · `m` toggle the memory map ·
`r` refresh regions.

It is deliberately a subset: no find/go-to, pointer scan, plugins, or MCP.

## Access tracker

Optional, off by default: build `reclass-backend-vmem` with the
`access-tracker` feature for "what instruction wrote/accessed this address" via
x86-64 hardware breakpoints (ptrace).

```sh
cargo build -p reclass-backend-vmem --features access-tracker
```
