# Example Plugin: Snapshot Logger

This is a reference implementation demonstrating the full plugin API: `on_snapshot` to observe value changes, `show_window` to render a UI, and `on_context_menu` to add right-click actions.

## What It Does

The Snapshot Logger watches every memory snapshot and records which field values changed. It:

1. **Observes changes** in `on_snapshot()` by comparing current field values to the last observed values.
2. **Logs changes** to a bounded ring buffer (max 200 lines), keeping the most recent changes visible.
3. **Displays the log** in an egui window showing when each field changed and what the new value is.
4. **Provides a context-menu entry** `Mark` to manually add a separator to the log, useful for marking significant events in a session.

## Build

```sh
cargo build -p reclass-example-plugin --release
```

The result is `target/release/libreclass_example_plugin.so` (Linux) or `.dylib` (macOS).

## Install

Drop the `.so` into one of the plugin discovery directories:

```sh
# Per-user (preferred for development)
mkdir -p ~/.config/reclass-rs/plugins
cp target/release/libreclass_example_plugin.so ~/.config/reclass-rs/plugins/

# Or next to the reclass binary (bundled release)
cp target/release/libreclass_example_plugin.so /path/to/reclass-binary/plugins/
```

Launch reclass-rs. The plugin loads on startup. Open *View → Plugins* to see it in the list, and toggle **Snapshot Logger** to enable it. Click **Snapshot Logger** again to open its window.

## How It Works

### Detecting Changes

```rust
fn on_snapshot(&mut self, rows: &[Row], _state: &AppState) -> Vec<PluginAction> {
    for row in rows {
        let key = Self::key(row);  // (root, path) tuple uniquely identifies a field
        if let Some(prev) = self.last.get(&key) && prev != &row.value {
            let line = format!("{}: {} -> {}", row.name, prev, row.value);
            self.push_log(line);
        }
        self.last.insert(key, row.value.clone());
    }
    Vec::new()  // Read-only observer; no mutations
}
```

Each frame, the hook receives `rows` — all visible fields from all open classes. A field is identified by its `(root, path)` pair (the class it belongs to and the nesting path), not its display name (two fields named `next` in different classes are different pointers).

When a field's value changes, we log it. When a field is no longer in the snapshot, its entry is eventually dropped from the map (triggered by a `push_log` that exceeds `MAX_LOG` and evicts old entries).

### Rendering the Window

```rust
fn show_window(&mut self, ctx: &egui::Context, _state: &AppState, open: &mut bool) {
    egui::Window::new("Snapshot Logger")
        .open(open)
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{} changes", self.log.len()));
                if ui.button("Clear").clicked() {
                    self.log.clear();
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.log.is_empty() {
                    ui.label("No value changes observed yet.");
                }
                for line in self.log.iter().rev() {  // newest first
                    ui.label(line);
                }
            });
        });
}
```

The window displays a count of logged changes, a **Clear** button, and a scrollable list of entries (newest at the top). The `open` bool is managed by the host: it reflects whether the user closed the window and persists across sessions.

### Context Menu Entry

```rust
fn context_menu_entries(&self) -> &[(&str, &str)] {
    &[("mark", "Log: mark this field")]
}

fn on_context_menu(
    &mut self,
    id: &str,
    _class: ClassId,
    idx: usize,
    _state: &AppState,
) -> Vec<PluginAction> {
    if id == "mark" {
        self.push_log(format!("-- marked node #{idx} --"));
    }
    Vec::new()
}
```

When you right-click a field and select **Log: mark this field**, it inserts a separator line with the node index. Useful for correlating the log with edits or external events.

## Settings

The logger has no persistent configuration — `save_settings()` and `load_settings()` are not implemented, so the plugin starts fresh each session. To make it persistent, derive `serde` on the struct and use `save_json` / `load_json` (see [`docs/plugins.md`](../../docs/plugins.md#settings-persistence) for an example).

## Key Patterns

- **`HostPlugin::name()` and `version()`** — required; metadata only.
- **`on_snapshot()` returns `Vec<PluginAction>`** — read-only observation. Return an empty vec if you're not requesting mutations.
- **`has_window()` + `show_window()`** — implement both to contribute a window. The host handles open/close state.
- **`context_menu_entries()` + `on_context_menu()`** — register right-click actions.
- **`push_log()` helper** — demonstrates a bounded ring buffer (drop oldest when `MAX_LOG` is exceeded).

## API Reference

See [`docs/plugins.md`](../../docs/plugins.md) for:
- The full `HostPlugin` trait and `PluginAction` enum
- `Row` and `AppState` types
- Settings persistence patterns
- Building your own plugin from scratch
- Troubleshooting

## Build Contract

This crate depends on `reclass` by path with the `gui` feature. **The plugin MUST be built with the same `rustc` and dependencies as the `reclass` binary it will load into.** This is enforced by the ABI fingerprint check at load time. If you update your Rust toolchain, rebuild both the host and this plugin.
