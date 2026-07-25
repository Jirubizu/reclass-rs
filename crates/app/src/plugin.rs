//! Dynamic plugin system (GUI builds only).
//!
//! ## Model — dynamic `.so`/`.dylib`, **same-toolchain contract**
//!
//! Plugins are native dynamic libraries dropped into `plugins/` next to the
//! binary. Each exports one C-ABI entry point, [`CREATE_SYMBOL`], produced by
//! the [`reclass_plugin_create!`] macro, returning a boxed [`HostPlugin`]
//! trait object.
//!
//! Rust has **no stable ABI**. Everything flowing across the library boundary
//! — the `dyn HostPlugin` fat pointer, [`Row`], [`NodeKind`], [`AppState`],
//! `Vec`/`String`, `egui::Context` — has a layout the compiler is free to
//! change between versions. This system is therefore sound **only** when a
//! plugin is built with the *identical* toolchain (same `rustc`, same
//! dependency versions, same codegen flags) as the host. That is the deal:
//! you get drop-in `.so` reloading in exchange for a strict build contract.
//!
//! The contract is **enforced at load**: every plugin exports
//! [`ABI_SYMBOL`], a `*const c_char` fingerprint of the crate version and the
//! `rustc` that built it (see `build.rs`). The loader compares it against its
//! own [`ABI_FINGERPRINT`] before reading any other symbol, so a skewed build
//! is a [`PluginError::AbiMismatch`], not undefined behaviour. That check is
//! itself skew-proof: a C string has a C layout.
//!
//! It does **not** cover a `#[global_allocator]` difference. The host calls
//! `Box::from_raw` on memory the plugin allocated, so host and plugin must
//! also share an allocator; the default system allocator on both sides
//! satisfies this, and nothing detects a divergence.
//! [`HostPlugin::version`] is metadata for the manager UI — the ABI gate, not
//! the version number, is what rejects an incompatible build.
//!
//! ## Safety boundary
//!
//! - Hooks receive `&AppState` / `&[Row]` — read-only. Plugins never hold a
//!   mutable host reference.
//! - Mutations are deferred: hooks return [`PluginAction`]s, applied by the
//!   host in its own phase (the same pattern as user actions and MCP calls).
//! - Every hook runs inside [`std::panic::catch_unwind`]; a panicking plugin
//!   is disabled and its error recorded — it never takes down the session.
//! - The one `extern "C"` symbol only *creates* the object; the macro wraps
//!   its body in `catch_unwind` so a panic there yields null, not an abort.
//!
//! ## Authoring a plugin
//!
//! Depend on `reclass` (this crate, `gui` feature) and:
//!
//! ```ignore
//! use reclass::plugin::*;
//!
//! #[derive(Default)]
//! struct MyPlugin;
//!
//! impl HostPlugin for MyPlugin {
//!     fn name(&self) -> &str { "My Plugin" }
//!     fn version(&self) -> (u32, u32) { (1, 0) }
//! }
//!
//! reclass_plugin_create!(MyPlugin);
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use libloading::{Library, Symbol};
use parking_lot::Mutex;

// Re-exports so a plugin author needs only `use reclass::plugin::*`.
pub use crate::app_state::AppState;
pub use eframe::egui;
pub use reclass_core::codegen::{Language, generate};
pub use reclass_core::{
    Class, ClassId, ClassRegistry, IntWidth, MemError, MemoryBackend, Node, NodeKind, PathSeg,
    Perms, Region, Row,
};

/// The C-ABI symbol every plugin exports (see [`reclass_plugin_create!`]).
pub const CREATE_SYMBOL: &[u8] = b"reclass_plugin_create";

/// Signature of the plugin entry point. Returns a freshly boxed trait object
/// (leaked to a raw pointer); the host takes ownership. Returns null on a
/// construction panic.
///
/// Not FFI-safe in the C sense (the return is a Rust fat pointer) — sound only
/// under the same-toolchain contract documented on this module.
#[allow(improper_ctypes_definitions)]
pub type CreateFn = unsafe extern "C" fn() -> *mut Box<dyn HostPlugin>;

/// The optional C-ABI symbol a *bundle* library exports to register many
/// plugins from one file (see [`reclass_plugin_create_all!`]). The loader tries
/// this before [`CREATE_SYMBOL`].
pub const CREATE_ALL_SYMBOL: &[u8] = b"reclass_plugin_create_all";

/// Signature of the bundle entry point: a boxed vector of plugins. Same
/// FFI/soundness caveats as [`CreateFn`].
#[allow(improper_ctypes_definitions)]
pub type CreateAllFn = unsafe extern "C" fn() -> *mut Vec<Box<dyn HostPlugin>>;

/// The C-ABI symbol carrying a plugin's toolchain fingerprint, emitted by
/// [`reclass_plugin_create!`] / [`reclass_plugin_create_all!`]. The loader
/// reads it *before* touching any other symbol.
pub const ABI_SYMBOL: &[u8] = b"reclass_plugin_abi";

/// Signature of the fingerprint entry point: a NUL-terminated C string.
///
/// Unlike [`CreateFn`], this one is genuinely FFI-safe — a `*const c_char`
/// has a C layout by definition. That is the point: the gate must stay
/// readable across exactly the toolchain skew it exists to detect.
pub type AbiFn = unsafe extern "C" fn() -> *const std::os::raw::c_char;

/// This build's ABI fingerprint: crate version plus the full `rustc --verbose
/// --version` of the compiler that produced it (see `build.rs`).
///
/// A plugin links its own copy of this crate, so the constant baked into a
/// `.so` records *that* build's toolchain. Comparing the two turns the
/// module's unenforceable same-toolchain contract into a load error.
///
/// Trailing NUL so it can be handed out as a C string with no allocation.
pub const ABI_FINGERPRINT: &str = concat!(
    "reclass ",
    env!("CARGO_PKG_VERSION"),
    "; ",
    env!("RECLASS_ABI_RUSTC"),
    "\0"
);

/// [`ABI_FINGERPRINT`] without its terminating NUL, for comparison and display.
#[must_use]
pub fn abi_fingerprint() -> &'static str {
    ABI_FINGERPRINT.trim_end_matches('\0')
}

/// A mutation a plugin asks the host to perform. Mirrors the verbs
/// [`AppState`] already exposes; the host applies these in its mutation phase.
pub enum PluginAction {
    /// Create a class and open it in a view.
    AddClass {
        /// Display name for the new class.
        name: String,
    },
    /// Append a node to a class.
    PushNode {
        /// Class to append to.
        class: ClassId,
        /// Type of the new node.
        kind: NodeKind,
        /// Field name.
        name: String,
    },
    /// Insert a node after `after_idx` in a class.
    InsertNode {
        /// Class to insert into.
        class: ClassId,
        /// The new node lands at `after_idx + 1`.
        after_idx: usize,
        /// Type of the new node.
        kind: NodeKind,
        /// Field name.
        name: String,
    },
    /// Remove node `idx` from a class.
    RemoveNode {
        /// Class to remove from.
        class: ClassId,
        /// Node index.
        idx: usize,
    },
    /// Change node `idx`'s kind.
    SetKind {
        /// Owning class.
        class: ClassId,
        /// Node index.
        idx: usize,
        /// Replacement type.
        kind: NodeKind,
    },
    /// Set an array node's element count.
    SetArrayCount {
        /// Owning class.
        class: ClassId,
        /// Node index; must already be an `Array`.
        idx: usize,
        /// New element count.
        count: usize,
    },
    /// Rename node `idx`.
    RenameNode {
        /// Owning class.
        class: ClassId,
        /// Node index.
        idx: usize,
        /// New field name.
        name: String,
    },
    /// Set node `idx`'s comment.
    SetComment {
        /// Owning class.
        class: ClassId,
        /// Node index.
        idx: usize,
        /// New comment (empty clears it).
        comment: String,
    },
    /// Set a class's address expression.
    SetAddressExpr {
        /// Class to retarget.
        class: ClassId,
        /// Expression source, parsed by [`reclass_core::expr`].
        expr: String,
    },
    /// Attach to a process by pid.
    AttachPid(
        /// Target process id.
        i32,
    ),
    /// Write a value to an address, parsed by `kind`.
    WriteValue {
        /// Absolute address in the target.
        addr: u64,
        /// Type used to parse `text` into bytes.
        kind: NodeKind,
        /// User-facing value text.
        text: String,
    },
    /// Save the project to a RON file.
    SaveProject {
        /// Destination path.
        path: String,
    },
    /// Load a project from a RON file.
    LoadProject {
        /// Source path.
        path: String,
    },
    /// Copy `text` to the system clipboard (host flushes it via egui).
    SetClipboard(
        /// Text to place on the clipboard.
        String,
    ),
}

/// Handle passed to a plugin at [`HostPlugin::init`]. `Send + Sync` so a plugin
/// may clone it into a background thread and enqueue work from there; the host
/// drains the queue each tick.
pub trait PluginHost: Send + Sync {
    /// Enqueue an action for the host to apply next tick.
    fn enqueue(&self, action: PluginAction);
}

/// A loadable plugin. Every method except [`name`](Self::name) /
/// [`version`](Self::version) has a no-op default, so a plugin implements only
/// the hooks it needs.
pub trait HostPlugin {
    /// Human-readable name for the plugin menu.
    fn name(&self) -> &str;

    /// `(major, minor)` version — metadata for the manager UI (see module docs:
    /// this is not an ABI guard).
    fn version(&self) -> (u32, u32);

    /// Called once after loading, with a handle for background/async work.
    fn init(&mut self, _host: Arc<dyn PluginHost>) {}

    /// After every snapshot, before the render pass. May annotate/collect and
    /// return actions for the host to apply.
    fn on_snapshot(&mut self, _rows: &[Row], _state: &AppState) -> Vec<PluginAction> {
        Vec::new()
    }

    /// Between the render pass and the host's apply phase. May inject
    /// mutations into the batch.
    fn on_pre_apply(&mut self, _state: &AppState) -> Vec<PluginAction> {
        Vec::new()
    }

    /// Whether this plugin contributes a window.
    fn has_window(&self) -> bool {
        false
    }

    /// Render the plugin's window during the egui render pass.
    fn show_window(&mut self, _ctx: &egui::Context, _state: &AppState, _open: &mut bool) {}

    /// Right-click context-menu entries: `(id, label)`.
    fn context_menu_entries(&self) -> &[(&str, &str)] {
        &[]
    }

    /// Handle a context-menu activation on the node at `(class, idx)`.
    fn on_context_menu(
        &mut self,
        _id: &str,
        _class: ClassId,
        _idx: usize,
        _state: &AppState,
    ) -> Vec<PluginAction> {
        Vec::new()
    }
}

/// Errors from loading a plugin library.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// `dlopen`/`LoadLibrary` failed.
    #[error("load {path}: {source}")]
    Load {
        /// The library path.
        path: String,
        /// The underlying loader error.
        source: libloading::Error,
    },
    /// The library has no `reclass_plugin_create` symbol.
    #[error("{path}: missing '{sym}' symbol", sym = String::from_utf8_lossy(CREATE_SYMBOL))]
    MissingSymbol {
        /// The library path.
        path: String,
        /// The underlying loader error.
        source: libloading::Error,
    },
    /// The entry point returned null (construction panicked).
    #[error("{path}: plugin construction returned null")]
    NullPlugin {
        /// The library path.
        path: String,
    },
    /// The library has no `reclass_plugin_abi` symbol, so its toolchain
    /// cannot be verified. Predates the ABI gate, or was not built with the
    /// `reclass_plugin_create!` macro.
    #[error(
        "{path}: missing '{sym}' symbol — rebuild the plugin against this host",
        sym = String::from_utf8_lossy(ABI_SYMBOL)
    )]
    MissingAbi {
        /// The library path.
        path: String,
    },
    /// The plugin was built by a different toolchain or `reclass` version.
    #[error("{path}: ABI mismatch\n  host:   {host}\n  plugin: {plugin}")]
    AbiMismatch {
        /// The library path.
        path: String,
        /// The host's fingerprint.
        host: String,
        /// The plugin's fingerprint.
        plugin: String,
    },
}

/// Metadata + UI state snapshot for one loaded plugin, cloned for the manager
/// window and context menu so they never borrow into live plugin state.
#[derive(Clone)]
pub struct PluginInfo {
    /// Index in the manager's plugin list.
    pub idx: usize,
    /// Plugin-reported name.
    pub name: String,
    /// Plugin-reported `(major, minor)` version.
    pub version: (u32, u32),
    /// Source library path.
    pub path: PathBuf,
    /// Whether hooks fire for this plugin.
    pub enabled: bool,
    /// Whether the plugin contributes a window.
    pub has_window: bool,
    /// Whether the plugin's window is currently open.
    pub window_open: bool,
    /// Last panic/error message, if the plugin misbehaved.
    pub error: Option<String>,
    /// Context-menu entries `(id, label)`, owned copies.
    pub menu_entries: Vec<(String, String)>,
}

/// Reject a library whose toolchain fingerprint does not match this build.
///
/// Reading `reclass_plugin_abi` is safe under skew because it returns a
/// `*const c_char`. Everything the loader does afterwards is not, which is
/// why this runs first.
fn check_abi(lib: &Library, path: &str) -> Result<(), PluginError> {
    let Ok(abi) = (unsafe { lib.get::<AbiFn>(ABI_SYMBOL) }) else {
        return Err(PluginError::MissingAbi {
            path: path.to_string(),
        });
    };
    // SAFETY: macro-generated; returns a pointer to a `'static` NUL-terminated
    // string constant in the plugin image, which stays mapped for `lib`'s life.
    let raw = unsafe { abi() };
    if raw.is_null() {
        return Err(PluginError::MissingAbi {
            path: path.to_string(),
        });
    }
    let plugin = unsafe { std::ffi::CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    if plugin == abi_fingerprint() {
        Ok(())
    } else {
        Err(PluginError::AbiMismatch {
            path: path.to_string(),
            host: abi_fingerprint().to_string(),
            plugin,
        })
    }
}

/// One loaded plugin plus the library that must outlive it.
struct Loaded {
    // `plugin` MUST be declared before `_lib`: fields drop in declaration
    // order, so the trait object is dropped while its library is still mapped.
    plugin: Box<dyn HostPlugin>,
    // The backing library, kept mapped for the plugin's lifetime. `None` only
    // for in-process test plugins with no dynamic library.
    _lib: Option<Arc<Library>>,
    name: String,
    version: (u32, u32),
    path: PathBuf,
    enabled: bool,
    window_open: bool,
    error: Option<String>,
}

/// The queue backing [`PluginHost::enqueue`], shared with plugin threads.
#[derive(Default)]
struct HostQueue {
    queue: Mutex<Vec<PluginAction>>,
}

impl PluginHost for HostQueue {
    fn enqueue(&self, action: PluginAction) {
        self.queue.lock().push(action);
    }
}

/// Owns every loaded plugin and drives their hooks. Lives in `ReClassApp`.
pub struct PluginManager {
    plugins: Vec<Loaded>,
    host: Arc<HostQueue>,
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginManager {
    /// An empty manager.
    pub fn new() -> Self {
        PluginManager {
            plugins: Vec::new(),
            host: Arc::new(HostQueue::default()),
        }
    }

    /// Whether any plugin is loaded.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Load every `.so`/`.dylib`/`.dll` in `dir`. Missing dir is not an error.
    /// Returns the per-file errors (so callers can surface them); successfully
    /// loaded plugins are retained.
    pub fn load_dir(&mut self, dir: &Path) -> Vec<PluginError> {
        let mut errors = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return errors;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_lib = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "so" | "dylib" | "dll"));
            if is_lib && let Err(e) = self.load_file(&path) {
                errors.push(e);
            }
        }
        errors
    }

    /// Load one plugin library (single or bundle) and run each plugin's `init`.
    pub fn load_file(&mut self, path: &Path) -> Result<(), PluginError> {
        let loaded = Self::open(path)?;
        let start = self.plugins.len();
        self.plugins.extend(loaded);
        let host: Arc<dyn PluginHost> = self.host.clone();
        for p in &mut self.plugins[start..] {
            Self::guarded(&mut p.error, &p.name, || p.plugin.init(host.clone()));
        }
        Ok(())
    }

    /// Open a library and construct its plugin(s). A bundle library exporting
    /// [`CREATE_ALL_SYMBOL`] yields many; otherwise [`CREATE_SYMBOL`] yields
    /// one. All plugins from one library share an `Arc<Library>`, so the library
    /// stays mapped until the last of them is dropped.
    fn open(path: &Path) -> Result<Vec<Loaded>, PluginError> {
        let disp = path.display().to_string();
        // SAFETY: loading arbitrary native code — see module-level contract.
        let lib = unsafe { Library::new(path) }.map_err(|source| PluginError::Load {
            path: disp.clone(),
            source,
        })?;

        // Verify the toolchain BEFORE touching `reclass_plugin_create`. Every
        // other symbol here hands Rust types across the boundary and is only
        // meaningful under the same-toolchain contract; this one is a plain
        // `*const c_char`, so it stays readable across exactly the skew it
        // exists to detect.
        check_abi(&lib, &disp)?;

        // Prefer the bundle entry point; fall back to the single-plugin one.
        let created: Vec<Box<dyn HostPlugin>> = if let Ok(create_all) =
            unsafe { lib.get::<CreateAllFn>(CREATE_ALL_SYMBOL) }
        {
            // SAFETY: macro-generated, `catch_unwind`-wrapped; null on panic.
            let raw = unsafe { create_all() };
            if raw.is_null() {
                return Err(PluginError::NullPlugin { path: disp });
            }
            // SAFETY: `raw` came from `Box::into_raw(Box::new(vec))`.
            *unsafe { Box::from_raw(raw) }
        } else {
            let create: Symbol<'_, CreateFn> =
                unsafe { lib.get(CREATE_SYMBOL) }.map_err(|source| PluginError::MissingSymbol {
                    path: disp.clone(),
                    source,
                })?;
            // SAFETY: macro-generated, `catch_unwind`-wrapped; null on panic.
            let raw = unsafe { create() };
            if raw.is_null() {
                return Err(PluginError::NullPlugin { path: disp });
            }
            // SAFETY: `raw` came from `Box::into_raw(Box::new(Box<dyn ...>))`.
            vec![*unsafe { Box::from_raw(raw) }]
        };

        let lib = Arc::new(lib);
        let loaded = created
            .into_iter()
            .map(|plugin| {
                let name = plugin.name().to_string();
                let version = plugin.version();
                Loaded {
                    plugin,
                    _lib: Some(lib.clone()),
                    name,
                    version,
                    path: path.to_path_buf(),
                    enabled: true,
                    window_open: false,
                    error: None,
                }
            })
            .collect();
        Ok(loaded)
    }

    /// Run `f` under `catch_unwind`; on panic, record the message in `error`.
    /// Returns whether `f` completed without panicking.
    fn guarded<R>(error: &mut Option<String>, name: &str, f: impl FnOnce() -> R) -> Option<R> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(r) => Some(r),
            Err(e) => {
                let msg = e
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| e.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panicked".to_string());
                *error = Some(format!("panic in plugin '{name}': {msg}"));
                None
            }
        }
    }

    /// Run `hook` on every enabled plugin, disabling any that panic, and
    /// collect the actions they return (plus anything enqueued from threads).
    fn run_hook(
        &mut self,
        mut hook: impl FnMut(&mut Box<dyn HostPlugin>) -> Vec<PluginAction>,
    ) -> Vec<PluginAction> {
        let mut out = self.drain_enqueued();
        for p in self.plugins.iter_mut().filter(|p| p.enabled) {
            let name = p.name.clone();
            match Self::guarded(&mut p.error, &name, || hook(&mut p.plugin)) {
                Some(actions) => out.extend(actions),
                None => p.enabled = false, // disable a panicking plugin
            }
        }
        out
    }

    /// HOOK 1 — post-snapshot.
    pub fn on_snapshot(&mut self, rows: &[Row], state: &AppState) -> Vec<PluginAction> {
        self.run_hook(|p| p.on_snapshot(rows, state))
    }

    /// HOOK 2 — pre-apply.
    pub fn on_pre_apply(&mut self, state: &AppState) -> Vec<PluginAction> {
        self.run_hook(|p| p.on_pre_apply(state))
    }

    /// Render every enabled plugin window, honoring its persisted open state.
    pub fn show_windows(&mut self, ctx: &egui::Context, state: &AppState) {
        for p in self
            .plugins
            .iter_mut()
            .filter(|p| p.enabled && p.window_open)
        {
            if !p.plugin.has_window() {
                continue;
            }
            let mut open = p.window_open;
            let name = p.name.clone();
            Self::guarded(&mut p.error, &name, || {
                p.plugin.show_window(ctx, state, &mut open)
            });
            p.window_open = open;
        }
    }

    /// Deliver a context-menu activation to plugin `idx`.
    pub fn on_context_menu(
        &mut self,
        idx: usize,
        id: &str,
        class: ClassId,
        node_idx: usize,
        state: &AppState,
    ) -> Vec<PluginAction> {
        let Some(p) = self.plugins.get_mut(idx) else {
            return Vec::new();
        };
        if !p.enabled {
            return Vec::new();
        }
        let name = p.name.clone();
        Self::guarded(&mut p.error, &name, || {
            p.plugin.on_context_menu(id, class, node_idx, state)
        })
        .unwrap_or_default()
    }

    /// Drain actions enqueued from plugin threads since the last tick.
    pub fn drain_enqueued(&self) -> Vec<PluginAction> {
        std::mem::take(&mut *self.host.queue.lock())
    }

    /// Snapshot of every plugin for the manager window / context menu.
    pub fn infos(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .enumerate()
            .map(|(idx, p)| PluginInfo {
                idx,
                name: p.name.clone(),
                version: p.version,
                path: p.path.clone(),
                enabled: p.enabled,
                has_window: p.plugin.has_window(),
                window_open: p.window_open,
                error: p.error.clone(),
                menu_entries: if p.enabled {
                    p.plugin
                        .context_menu_entries()
                        .iter()
                        .map(|(i, l)| (i.to_string(), l.to_string()))
                        .collect()
                } else {
                    Vec::new()
                },
            })
            .collect()
    }

    /// Enable or disable plugin `idx`. Clears the recorded error when
    /// re-enabling so a recovered plugin can run again.
    pub fn set_enabled(&mut self, idx: usize, enabled: bool) {
        if let Some(p) = self.plugins.get_mut(idx) {
            p.enabled = enabled;
            if enabled {
                p.error = None;
            }
        }
    }

    /// Open or close plugin `idx`'s window.
    pub fn set_window_open(&mut self, idx: usize, open: bool) {
        if let Some(p) = self.plugins.get_mut(idx) {
            p.window_open = open;
        }
    }

    /// Reload the library backing plugin `idx` from disk (unload + load fresh),
    /// replacing every plugin that came from the same file. For plugin authors
    /// iterating on a `.so`.
    pub fn reload(&mut self, idx: usize) -> Result<(), PluginError> {
        let Some(path) = self.plugins.get(idx).map(|p| p.path.clone()) else {
            return Ok(());
        };
        // Remember open windows by plugin name to restore them after reload.
        let open: Vec<(String, bool)> = self
            .plugins
            .iter()
            .filter(|p| p.path == path)
            .map(|p| (p.name.clone(), p.window_open))
            .collect();
        self.plugins.retain(|p| p.path != path);
        self.load_file(&path)?;
        for p in &mut self.plugins {
            if p.path == path
                && let Some((_, was)) = open.iter().find(|(n, _)| n == &p.name)
            {
                p.window_open = *was;
            }
        }
        Ok(())
    }
}

/// Emit the `reclass_plugin_abi` symbol carrying this build's toolchain
/// fingerprint. Invoked by both entry-point macros; a plugin never calls it
/// directly, and must not emit it twice.
#[macro_export]
macro_rules! reclass_plugin_abi {
    () => {
        /// Toolchain fingerprint. See [`reclass::plugin::ABI_FINGERPRINT`].
        #[unsafe(no_mangle)]
        pub extern "C" fn reclass_plugin_abi() -> *const ::std::os::raw::c_char {
            // The constant is NUL-terminated and `'static`, so this is a plain
            // C string pointer into the plugin image — no allocation, and no
            // Rust type crosses the boundary.
            $crate::plugin::ABI_FINGERPRINT
                .as_ptr()
                .cast::<::std::os::raw::c_char>()
        }
    };
}

/// Generate the C-ABI entry point for a plugin type. The type must implement
/// [`HostPlugin`] and [`Default`]. Place once in the plugin crate's `lib.rs`.
#[macro_export]
macro_rules! reclass_plugin_create {
    ($ty:ty) => {
        $crate::reclass_plugin_abi!();

        /// Plugin entry point. See [`reclass::plugin`].
        #[unsafe(no_mangle)]
        #[allow(improper_ctypes_definitions)]
        pub extern "C" fn reclass_plugin_create()
        -> *mut ::std::boxed::Box<dyn $crate::plugin::HostPlugin> {
            // A panic must not unwind across the `extern "C"` boundary. The
            // trait object is double-boxed so the returned pointer is thin
            // (a fat pointer has no null form).
            let built = ::std::panic::catch_unwind(|| {
                ::std::boxed::Box::new(::std::boxed::Box::new(
                    <$ty as ::std::default::Default>::default(),
                )
                    as ::std::boxed::Box<dyn $crate::plugin::HostPlugin>)
            });
            match built {
                ::std::result::Result::Ok(b) => ::std::boxed::Box::into_raw(b),
                ::std::result::Result::Err(_) => ::std::ptr::null_mut(),
            }
        }
    };
}

/// Generate the bundle entry point registering many plugin types from one
/// library. Each type must implement [`HostPlugin`] and [`Default`]. Use this
/// instead of [`reclass_plugin_create!`] when a library ships several plugins.
#[macro_export]
macro_rules! reclass_plugin_create_all {
    ($($ty:ty),+ $(,)?) => {
        $crate::reclass_plugin_abi!();

        /// Bundle entry point. See [`reclass::plugin`].
        #[unsafe(no_mangle)]
        #[allow(improper_ctypes_definitions)]
        pub extern "C" fn reclass_plugin_create_all()
        -> *mut ::std::vec::Vec<::std::boxed::Box<dyn $crate::plugin::HostPlugin>> {
            let built = ::std::panic::catch_unwind(|| {
                let mut v: ::std::vec::Vec<::std::boxed::Box<dyn $crate::plugin::HostPlugin>> =
                    ::std::vec::Vec::new();
                $(
                    v.push(::std::boxed::Box::new(
                        <$ty as ::std::default::Default>::default(),
                    ) as ::std::boxed::Box<dyn $crate::plugin::HostPlugin>);
                )+
                v
            });
            match built {
                ::std::result::Result::Ok(v) => {
                    ::std::boxed::Box::into_raw(::std::boxed::Box::new(v))
                }
                ::std::result::Result::Err(_) => ::std::ptr::null_mut(),
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-process plugin that records which hooks fired, in order, into a
    /// shared log. Exercises the manager's hook orchestration without a `.so`.
    #[derive(Default)]
    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
        panic_on_snapshot: bool,
    }

    impl HostPlugin for Recorder {
        fn name(&self) -> &str {
            "Recorder"
        }
        fn version(&self) -> (u32, u32) {
            (1, 0)
        }
        fn init(&mut self, host: Arc<dyn PluginHost>) {
            self.log.lock().push("init".into());
            host.enqueue(PluginAction::AddClass {
                name: "FromInit".into(),
            });
        }
        fn on_snapshot(&mut self, _rows: &[Row], _state: &AppState) -> Vec<PluginAction> {
            if self.panic_on_snapshot {
                panic!("boom");
            }
            self.log.lock().push("on_snapshot".into());
            vec![PluginAction::AddClass {
                name: "FromSnapshot".into(),
            }]
        }
        fn on_pre_apply(&mut self, _state: &AppState) -> Vec<PluginAction> {
            self.log.lock().push("on_pre_apply".into());
            Vec::new()
        }
        fn context_menu_entries(&self) -> &[(&str, &str)] {
            &[("greet", "Greet")]
        }
        fn on_context_menu(
            &mut self,
            id: &str,
            _class: ClassId,
            _idx: usize,
            _state: &AppState,
        ) -> Vec<PluginAction> {
            self.log.lock().push(format!("ctx:{id}"));
            Vec::new()
        }
    }

    /// Push a `Recorder` directly, bypassing the `.so` load path, so hook logic
    /// is testable without building a library.
    fn with_recorder(panic_on_snapshot: bool) -> (PluginManager, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut mgr = PluginManager::new();
        let host: Arc<dyn PluginHost> = mgr.host.clone();
        let mut plugin = Recorder {
            log: log.clone(),
            panic_on_snapshot,
        };
        plugin.init(host);
        mgr.plugins.push(Loaded {
            plugin: Box::new(plugin),
            _lib: None,
            name: "Recorder".into(),
            version: (1, 0),
            path: PathBuf::from("<test>"),
            enabled: true,
            window_open: false,
            error: None,
        });
        (mgr, log)
    }

    #[test]
    fn hooks_fire_in_order() {
        let (mut mgr, log) = with_recorder(false);
        let state = AppState::new();
        let snap = mgr.on_snapshot(&[], &state);
        let pre = mgr.on_pre_apply(&state);
        let ctx = mgr.on_context_menu(0, "greet", 0, 0, &state);

        assert_eq!(
            *log.lock(),
            vec!["init", "on_snapshot", "on_pre_apply", "ctx:greet"]
        );
        // init enqueued one action; it surfaces on the next hook drain.
        assert!(matches!(
            snap.as_slice(),
            [
                PluginAction::AddClass { name: n1 },
                PluginAction::AddClass { name: n2 }
            ] if n1 == "FromInit" && n2 == "FromSnapshot"
        ));
        assert!(pre.is_empty() && ctx.is_empty());
    }

    #[test]
    fn panicking_hook_is_caught_and_disables_plugin() {
        let (mut mgr, _log) = with_recorder(true);
        let state = AppState::new();
        let out = mgr.on_snapshot(&[], &state); // must not unwind
        assert!(
            out.iter()
                .any(|a| matches!(a, PluginAction::AddClass { .. }))
        ); // the init enqueue still drained
        let info = &mgr.infos()[0];
        assert!(!info.enabled, "panicking plugin should be disabled");
        assert!(info.error.as_deref().unwrap().contains("boom"));
        // once disabled, its hooks no longer fire.
        assert!(mgr.on_snapshot(&[], &state).is_empty());
    }

    #[test]
    fn context_menu_entries_surface_in_infos() {
        let (mgr, _log) = with_recorder(false);
        let entries = &mgr.infos()[0].menu_entries;
        assert_eq!(entries, &[("greet".to_string(), "Greet".to_string())]);
    }
}
