//! End-to-end test of the dynamic plugin load path: build the reference plugin
//! cdylib, `dlopen` it through `PluginManager`, and drive its hooks across the
//! library boundary. Proves the C-ABI entry point + same-toolchain contract
//! actually round-trips a `dyn HostPlugin`.
#![cfg(feature = "gui")]

use std::path::PathBuf;
use std::process::Command;

use reclass::plugin::{AppState, PluginManager};

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

    // Reload the bundle — replaces all 8 plugins with fresh instances.
    mgr.reload(0).expect("reload bundle");
    let after = mgr.infos();
    assert_eq!(after.len(), 8);
    assert_eq!(
        after.iter().filter(|i| i.name == "Pointer Summary").count(),
        1
    );
}
