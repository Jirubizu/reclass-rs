# Plugin Authoring Guide

Plugins are native dynamic libraries (`.so` / `.dylib` / `.dll`) that observe memory snapshots and request mutations. They load at startup from user config directories or bundled next to the binary, and run inside the host process with read-only access to the current state.

> ⚠️ A plugin is native code running in-process with full access to the target's memory. Only load ones you trust.

Already have one and just want to run it? Drop it in a [discovery directory](#plugin-discovery) and manage it in *View → Plugins*: enable/disable, open its window, or reload it after a rebuild. A plugin that panics is disabled with its message recorded rather than taking down the session. The eight [bundled plugins](#bundled-plugins) ship pre-installed in the release tarball.

## The ABI Contract

Rust has **no stable ABI**. Everything flowing across the library boundary — the `dyn HostPlugin` fat pointer, `Row`, `NodeKind`, `AppState`, `Vec`, `String`, `egui::Context` — has a layout the compiler is free to change between versions. A plugin is sound **only** when built with the *identical* toolchain as the host (same `rustc`, same dependency versions, same codegen flags).

The loader enforces this at load time by comparing a `reclass_plugin_abi` symbol — a C-string fingerprint encoding the crate version and the `rustc --verbose --version` that built the library — against its own `ABI_FINGERPRINT`. A mismatched toolchain produces `PluginError::AbiMismatch` *before* any other symbol is read, preventing undefined behavior. The check is itself skew-proof: a C string has a C layout.

## Plugin Discovery

The host scans two directories, in this order:

1. `$XDG_CONFIG_HOME/reclass-rs/plugins/`, or `$HOME/.config/reclass-rs/plugins/` when `$XDG_CONFIG_HOME` is unset (`./.reclass-rs/plugins/` when neither is set)
2. `plugins/` next to the binary (what the release tarball ships)

Both are scanned — the config directory first — and a missing one is skipped silently. Plugins are discovered by file extension (`.so`, `.dylib`, `.dll`). Loading happens in `gui` builds only; a build without that feature has no plugin host.

## Lifecycle Hooks

Each frame, the host calls two plugin hooks in this order:

1. **HOOK 1: `on_snapshot`** — After memory reads, before rendering. Receives `&[Row]` (all visible fields from all open classes) and `&AppState` (read-only session state). May observe changes and enqueue mutations.
2. **HOOK 2: `on_pre_apply`** — After rendering, before the host applies mutations. Receives only `&AppState`. Used to inject final mutations into the batch.

Every hook runs inside `std::panic::catch_unwind`. A panicking plugin is disabled and its error recorded; it never takes down the session.

## Required Exports

A plugin library must export one C-ABI symbol:

- **`reclass_plugin_create`** — Entry point, returns a boxed `Box<dyn HostPlugin>`. Use the `reclass_plugin_create!` macro.
  
Alternatively, a bundle library exporting multiple plugins uses:

- **`reclass_plugin_create_all`** — Returns a `Vec<Box<dyn HostPlugin>>`. Use `reclass_plugin_create_all!`.

Both also emit the **`reclass_plugin_abi`** symbol automatically (a `*const c_char` carrying the fingerprint).

## HostPlugin Trait

Implement this trait. All methods except `name()` and `version()` have no-op defaults.

```rust
pub trait HostPlugin {
    /// Human-readable name for the plugin menu.
    fn name(&self) -> &str;
    
    /// (major, minor) version — metadata for the UI, not an ABI guard.
    fn version(&self) -> (u32, u32);
    
    /// Called once after loading, with a handle for background/async work.
    fn init(&mut self, host: Arc<dyn PluginHost>) {}
    
    /// After every snapshot, before rendering.
    fn on_snapshot(&mut self, rows: &[Row], state: &AppState) -> Vec<PluginAction> {
        Vec::new()
    }
    
    /// Between rendering and mutation apply.
    fn on_pre_apply(&mut self, state: &AppState) -> Vec<PluginAction> {
        Vec::new()
    }
    
    /// Whether this plugin contributes a window.
    fn has_window(&self) -> bool { false }
    
    /// Render the plugin's window during the egui render pass.
    fn show_window(&mut self, ctx: &egui::Context, state: &AppState, open: &mut bool) {}
    
    /// Right-click context-menu entries: (id, label).
    fn context_menu_entries(&self) -> &[(&str, &str)] { &[] }
    
    /// Handle a context-menu activation on node (class, idx).
    fn on_context_menu(
        &mut self,
        id: &str,
        class: ClassId,
        idx: usize,
        state: &AppState,
    ) -> Vec<PluginAction> {
        Vec::new()
    }
    
    /// Serialize configuration for persistence (opaque blob).
    fn save_settings(&self) -> Option<String> { None }
    
    /// Restore previously saved configuration.
    fn load_settings(&mut self, data: &str) -> bool { false }
}
```

## PluginAction — Mutations

Hooks return a `Vec<PluginAction>`. The host applies each action in its own mutation phase, using the same code path as user actions and MCP calls. Actions are:

- **`AddClass { name: String }`** — Create a class and open it in a view.
- **`PushNode { class: ClassId, kind: NodeKind, name: String }`** — Append a node to a class.
- **`InsertNode { class: ClassId, after_idx: usize, kind: NodeKind, name: String }`** — Insert a node after index.
- **`RemoveNode { class: ClassId, idx: usize }`** — Remove node by index.
- **`SetKind { class: ClassId, idx: usize, kind: NodeKind }`** — Change a node's type.
- **`SetArrayCount { class: ClassId, idx: usize, count: usize }`** — Resize an array node.
- **`RenameNode { class: ClassId, idx: usize, name: String }`** — Rename a node.
- **`SetComment { class: ClassId, idx: usize, comment: String }`** — Set or clear a node's comment.
- **`SetAddressExpr { class: ClassId, expr: String }`** — Change a class's address expression.
- **`AttachPid(i32)`** — Attach to a process by pid.
- **`WriteValue { addr: u64, kind: NodeKind, text: String }`** — Write a value to an address.
- **`SaveProject { path: String }`** — Save the project to a RON file.
- **`LoadProject { path: String }`** — Load a project from a RON file.
- **`SetClipboard(String)`** — Copy text to the system clipboard.

## Settings Persistence

Configuration is saved to `~/.config/reclass-rs/settings.ron` under a `plugins:` section, keyed by plugin name.

For each plugin, the host stores:
- `enabled` — whether the plugin's hooks fire
- `window_open` — whether its window is open
- `config` — an opaque blob from `save_settings()`

When a plugin loads, the host calls `load_settings()` with its saved blob. Implement it using the `load_json` helper for common cases:

```rust
use reclass::plugin::*;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct MyPlugin {
    threshold: u32,
    #[serde(skip)]
    transient_data: Vec<String>,
}

impl HostPlugin for MyPlugin {
    fn name(&self) -> &str { "My Plugin" }
    fn version(&self) -> (u32, u32) { (1, 0) }
    
    fn save_settings(&self) -> Option<String> {
        save_json(self)  // serializes public fields, skips #[serde(skip)]
    }
    
    fn load_settings(&mut self, data: &str) -> bool {
        load_json(self, data)  // deserializes, fills missing fields with Default
    }
}
```

Mark transient fields `#[serde(skip)]` and the struct `#[serde(default)]`. Blobs written by older builds still load with new fields defaulted. Return `false` from `load_settings()` if the format changed and the blob is unrecoverable — the host discards it and starts with defaults. Entries for uninstalled plugins are preserved in `settings.ron` so reinstalling them restores their configuration.

## Minimal Example

```rust
use reclass::plugin::*;

#[derive(Default)]
struct HelloPlugin;

impl HostPlugin for HelloPlugin {
    fn name(&self) -> &str { "Hello" }
    fn version(&self) -> (u32, u32) { (1, 0) }
}

// Path-qualified: `#[macro_export]` puts the macro at the crate root, so the
// `plugin::*` glob above does not bring it into scope.
reclass::reclass_plugin_create!(HelloPlugin);
```

`name` and `version` are the only required methods; every hook has a default.
This compiles as written — it is the same shape as
`crates/example-plugin/src/lib.rs`.

## `Cargo.toml`

```toml
[package]
name = "my-reclass-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
reclass = { path = "../reclass/crates/app", default-features = false, features = ["gui"] }
serde = { version = "1.0", features = ["derive"] }
```

Key requirements:
- **`crate-type = ["cdylib"]`** — produces a native library, not an rlib.
- **`path` dependency on `reclass`** with the `gui` feature — this is what makes the ABI fingerprint match. `reclass` is not published on crates.io, and even if it were, a registry copy would be a different `reclass` build than the binary you are loading into, which the fingerprint check rejects.

Build with `cargo build --release`. The result is `target/release/libmy_reclass_plugin.so` (Linux).

## Window Rendering

Plugins can render immediate-mode UI using `egui`:

```rust
impl HostPlugin for MyPlugin {
    fn has_window(&self) -> bool { true }
    
    fn show_window(&mut self, ctx: &egui::Context, state: &AppState, open: &mut bool) {
        egui::Window::new("My Window")
            .open(open)
            .show(ctx, |ui| {
                ui.label("Hello from a plugin!");
                if ui.button("Do something").clicked() {
                    // Return mutations from a hook, not from here
                }
            });
    }
}
```

The window is rendered during the egui render pass. Layout and event handling follow egui semantics. The `open` bool reflects whether the user closed the window; set it to control visibility.

## Context Menu Entries

Right-click a field to add custom actions:

```rust
impl HostPlugin for MyPlugin {
    fn context_menu_entries(&self) -> &[(&str, &str)] {
        &[
            ("copy_hex", "Copy as hex"),
            ("copy_asm", "Copy as x86 asm"),
        ]
    }
    
    fn on_context_menu(
        &mut self,
        id: &str,
        class: ClassId,
        idx: usize,
        state: &AppState,
    ) -> Vec<PluginAction> {
        match id {
            "copy_hex" => {
                let rows = state.compute_rows();  // all visible rows
                if let Some(row) = rows.iter().find(|r| r.class == class && r.idx == idx) {
                    return vec![PluginAction::SetClipboard(format!("0x{:X}", row.value))];
                }
            }
            "copy_asm" => {
                // ...
            }
            _ => {}
        }
        Vec::new()
    }
}
```

## Background Threads

Pass the `PluginHost` handle (from `init()`) to background threads. Call `enqueue()` to inject actions onto the host's work queue:

```rust
impl HostPlugin for MyPlugin {
    fn init(&mut self, host: Arc<dyn PluginHost>) {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            host.enqueue(PluginAction::AddClass { 
                name: "Auto-added".to_string() 
            });
        });
    }
}
```

The host drains the queue each tick and applies actions in the mutation phase.

## Troubleshooting

**Plugin not appearing**
- Check the plugin's filename ends in `.so` (Linux), `.dylib` (macOS), or `.dll` (Windows).
- Verify the directory: `~/.config/reclass-rs/plugins/` exists or `plugins/` next to the binary.
- Check *View → Plugins* for load errors (the manager window shows why a plugin failed to load).

**ABI mismatch error**
- The plugin was built with a different `rustc` than the host binary. Rebuild the plugin using the same toolchain.
- Ensure the plugin's `Cargo.toml` depends on `reclass` by path, pointing at the same checkout the host binary was built from.
- If you built reclass-rs from a fresh git clone, rebuild the plugin too.

**Plugin panics or crashes the session**
- Panics inside hooks are caught and recorded (the plugin is disabled). Check *View → Plugins* for the error message.
- A plugin that segfaults will crash the session — this indicates memory corruption at the boundary, usually from a toolchain mismatch or unsafe code.

**Window doesn't appear**
- Ensure `has_window()` returns `true`.
- Verify the window opens in *View → Plugins* (check the checkbox).
- If the window renders but is empty, check `show_window()` is pushing UI to the context.

## Bundled plugins

All eight ship in one cdylib (`libreclass_official_plugins.so`), built from
[`crates/official-plugins`](../crates/official-plugins/) and included in the
release tarball:

| Plugin | What it does |
|---|---|
| Pointer Summary | For every `ClassPtr` row, which class it targets and whether it moved since the last tick |
| Sentinel Watch | Flags fields whose value must never change — magic-number / integrity detection |
| Structure Diff | Freezes a baseline snapshot and diffs every later one against it |
| Hex Dump | Raw hex viewer for an arbitrary address, read through the live backend |
| Copy As | Context-menu entries to copy a field's declaration as C, Rust, Python, or JSON |
| Cheat Table Exporter | Writes the current scalar rows out as a Cheat Engine `.CT` file |
| Auto-attach | Polls `/proc` for a process by name and attaches when it appears |
| Scheduled Sampler | Saves a timestamped project snapshot every N ticks |

## Real Examples

The bundled plugins in `crates/official-plugins/src/` demonstrate:
- `pointer_summary.rs` — read-only observation and windowed display
- `auto_attach.rs` — configuration with settings persistence
- `sentinel_watch.rs` — per-field change detection and flagging
- `structure_diff.rs` — snapshot comparison
- `hex_dump.rs` — raw memory view at an arbitrary address
- `copy_as.rs` — context-menu entries and clipboard
- `cheat_table.rs` — export to external format
- `scheduled_sampler.rs` — periodic mutations via background thread

The `crates/example-plugin/` is a commented walk-through of hooks and state: `on_snapshot()` to diff values, `show_window()` to render a log, and `on_context_menu()` to mark fields. Build and install it to see how the pieces fit together.
