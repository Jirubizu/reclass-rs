//! End-to-end test of the dynamic plugin load path: build the reference plugin
//! cdylib, `dlopen` it through `PluginManager`, and drive its hooks across the
//! library boundary. Proves the C-ABI entry point + same-toolchain contract
//! actually round-trips a `dyn HostPlugin`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use reclass::plugin::{AppState, PluginError, PluginManager, PluginSettings};

/// Platform library file name for the reference plugin crate.
fn plugin_lib_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "reclass_example_plugin.dll"
    } else if cfg!(target_os = "macos") {
        "libreclass_example_plugin.dylib"
    } else {
        "libreclass_example_plugin.so"
    }
}

/// Build the reference plugin and return the path to its dynamic library.
fn build_reference_plugin() -> PathBuf {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "reclass-example-plugin"])
        .status()
        .expect("spawn cargo build");
    assert!(status.success(), "building the reference plugin failed");

    // The test binary lives at <target>/<profile>/deps/<name>; the cdylib is at
    // <target>/<profile>/<lib>.
    let profile_dir = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile dir")
        .to_path_buf();
    let so = profile_dir.join(plugin_lib_name());
    assert!(so.exists(), "plugin library not found at {}", so.display());
    so
}

#[test]
fn loads_reference_plugin_and_runs_hooks() {
    let so = build_reference_plugin();

    let mut mgr = PluginManager::new();
    mgr.load_file(&so).expect("load plugin .so");

    let infos = mgr.infos();
    assert_eq!(infos.len(), 1, "one plugin loaded");
    let info = &infos[0];
    assert_eq!(info.name, "Snapshot Logger");
    assert_eq!(info.version, (0, 1));
    assert!(info.enabled);
    assert!(info.has_window);
    assert!(info.error.is_none());
    assert_eq!(
        info.menu_entries,
        vec![("mark".to_string(), "Log: mark this field".to_string())]
    );

    // Hooks run across the FFI boundary without panicking.
    let mut state = AppState::new();
    let cid = state.add_class("Target");
    assert!(mgr.on_snapshot(&[], &state).is_empty());
    assert!(mgr.on_pre_apply(&state).is_empty());
    assert!(mgr.on_context_menu(0, "mark", cid, 0, &state).is_empty());

    // Reload (unload + fresh load) succeeds and preserves identity.
    mgr.reload(0).expect("reload plugin");
    assert_eq!(mgr.infos()[0].name, "Snapshot Logger");

    // Dropping `mgr` here unloads the library after the plugin box is dropped;
    // a crash on unload would fail the test.
}

fn official_bundle_lib_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "reclass_official_plugins.dll"
    } else if cfg!(target_os = "macos") {
        "libreclass_official_plugins.dylib"
    } else {
        "libreclass_official_plugins.so"
    }
}

#[test]
fn loads_official_bundle_with_all_plugins() {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "reclass-official-plugins"])
        .status()
        .expect("spawn cargo build");
    assert!(status.success(), "building the official bundle failed");

    let profile_dir = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile dir")
        .to_path_buf();
    let so = profile_dir.join(official_bundle_lib_name());
    assert!(so.exists(), "bundle library not found at {}", so.display());

    let mut mgr = PluginManager::new();
    mgr.load_file(&so).expect("load bundle .so");

    let infos = mgr.infos();
    assert_eq!(infos.len(), 8, "bundle should register 8 plugins");

    let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"Pointer Summary"));
    assert!(names.contains(&"Sentinel Watch"));
    assert!(names.contains(&"Auto-attach"));
    assert!(names.contains(&"Scheduled Sampler"));
    assert!(names.contains(&"Structure Diff"));
    assert!(names.contains(&"Hex Dump"));
    assert!(names.contains(&"Copy As"));
    assert!(names.contains(&"Cheat Table Exporter"));

    // All enabled, no errors.
    for info in &infos {
        assert!(info.enabled, "{} should be enabled", info.name);
        assert!(
            info.error.is_none(),
            "{} has error: {:?}",
            info.name,
            info.error
        );
    }

    // Sentinel Watch has a context-menu entry.
    let sw = infos.iter().find(|i| i.name == "Sentinel Watch").unwrap();
    assert_eq!(
        sw.menu_entries,
        vec![("mark_sentinel".to_string(), "Mark as sentinel".to_string())]
    );

    // Copy As has 4 context-menu entries.
    let ca = infos.iter().find(|i| i.name == "Copy As").unwrap();
    assert_eq!(ca.menu_entries.len(), 4);

    // Hooks run without panicking.
    let mut state = AppState::new();
    state.add_class("Target");
    assert!(mgr.on_snapshot(&[], &state).is_empty());
    assert!(mgr.on_pre_apply(&state).is_empty());

    // Persisted state round-trips through the real plugins: a blob they
    // accept is applied and handed straight back.
    let mut saved: BTreeMap<String, PluginSettings> = [
        (
            "Auto-attach".to_string(),
            PluginSettings {
                enabled: true,
                window_open: true,
                config: Some(r#"{"target":"mygame"}"#.to_string()),
            },
        ),
        // Wrong type for `addr_input` — a blob from an incompatible build.
        (
            "Hex Dump".to_string(),
            PluginSettings {
                enabled: false,
                window_open: false,
                config: Some(r#"{"addr_input":42}"#.to_string()),
            },
        ),
        // Not in this bundle; must survive untouched for a later install.
        (
            "Not Installed".to_string(),
            PluginSettings {
                enabled: false,
                window_open: true,
                config: Some("opaque".to_string()),
            },
        ),
    ]
    .into_iter()
    .collect();
    mgr.apply_settings(&mut saved);

    let snap = mgr.settings_snapshot();
    assert_eq!(
        snap["Auto-attach"].config.as_deref(),
        Some(r#"{"target":"mygame"}"#)
    );
    assert!(snap["Auto-attach"].window_open);
    // The rejected blob is dropped, and Hex Dump is back on its defaults.
    assert_eq!(saved["Hex Dump"].config, None);
    assert_eq!(
        snap["Hex Dump"].config.as_deref(),
        Some(r#"{"addr_input":"","rows_input":""}"#)
    );
    assert!(!snap["Hex Dump"].enabled);
    // Host-owned flags still applied despite the bad blob, and the unknown
    // entry is neither applied nor forgotten.
    assert!(!snap.contains_key("Not Installed"));
    assert_eq!(saved["Not Installed"].config.as_deref(), Some("opaque"));

    // Reload the bundle — replaces all 8 plugins with fresh instances, and
    // carries their state across rather than resetting it.
    mgr.reload(0).expect("reload bundle");
    let after = mgr.infos();
    assert_eq!(after.len(), 8);
    assert_eq!(
        after.iter().filter(|i| i.name == "Pointer Summary").count(),
        1
    );
    let after_snap = mgr.settings_snapshot();
    assert_eq!(
        after_snap["Auto-attach"].config.as_deref(),
        Some(r#"{"target":"mygame"}"#)
    );
    assert!(!after_snap["Hex Dump"].enabled);
}

/// Compile `src` as a standalone cdylib with plain `rustc` — deliberately not
/// linking `reclass`, so the library exports exactly the symbols written here
/// and nothing else.
fn build_bare_cdylib(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("reclass_abi_test_{name}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let rs = dir.join("lib.rs");
    std::fs::write(&rs, src).expect("write source");
    let status = Command::new("rustc")
        .args(["--edition", "2024", "--crate-type", "cdylib"])
        .arg("--crate-name")
        .arg(name)
        .arg("-o")
        .arg(dir.join(plugin_lib_name().replace("reclass_example_plugin", name)))
        .arg(&rs)
        .status()
        .expect("spawn rustc");
    assert!(status.success(), "building the bare cdylib failed");
    dir.join(plugin_lib_name().replace("reclass_example_plugin", name))
}

#[test]
fn rejects_plugin_without_an_abi_fingerprint() {
    // `reclass_plugin_create` returns null here, so if the loader called it the
    // error would be `NullPlugin`. Getting `MissingAbi` proves the gate runs
    // *before* any Rust-ABI symbol is touched — the whole point of it.
    let so = build_bare_cdylib(
        "no_abi_plugin",
        r#"
        #[unsafe(no_mangle)]
        pub extern "C" fn reclass_plugin_create() -> *mut u8 { ::std::ptr::null_mut() }
        "#,
    );
    let mut mgr = PluginManager::new();
    let err = mgr.load_file(&so).expect_err("must be rejected");
    assert!(
        matches!(err, PluginError::MissingAbi { .. }),
        "expected MissingAbi, got {err:?}"
    );
    assert!(mgr.is_empty());
}

#[test]
fn rejects_plugin_built_by_a_different_toolchain() {
    let so = build_bare_cdylib(
        "skewed_plugin",
        r#"
        #[unsafe(no_mangle)]
        pub extern "C" fn reclass_plugin_abi() -> *const ::std::os::raw::c_char {
            c"reclass 0.0.0; rustc 1.0.0 (deadbeef 1970-01-01)".as_ptr()
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn reclass_plugin_create() -> *mut u8 { ::std::ptr::null_mut() }
        "#,
    );
    let mut mgr = PluginManager::new();
    let err = mgr.load_file(&so).expect_err("must be rejected");
    match err {
        PluginError::AbiMismatch { host, plugin, .. } => {
            assert_eq!(plugin, "reclass 0.0.0; rustc 1.0.0 (deadbeef 1970-01-01)");
            assert_eq!(host, reclass::plugin::abi_fingerprint());
            assert_ne!(host, plugin);
        }
        other => panic!("expected AbiMismatch, got {other:?}"),
    }
    assert!(mgr.is_empty());
}
