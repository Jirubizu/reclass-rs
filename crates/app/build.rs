//! Bakes the compiling toolchain's identity into the crate so the plugin
//! loader can reject a `.so` built by a different one.
//!
//! Rust has no stable ABI, so a plugin and the host must be built by the same
//! `rustc`. That invariant is unenforceable at runtime unless both sides carry
//! a fingerprint of the compiler that produced them — which is what this emits.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--verbose")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(
            || "unknown".to_string(),
            |v| {
                // `--verbose --version` includes the commit hash and host
                // triple, so a nightly respin or a cross-target build is a
                // different fingerprint even at the same version number.
                v.split('\n')
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect::<Vec<_>>()
                    .join("; ")
            },
        );

    println!("cargo:rustc-env=RECLASS_ABI_RUSTC={version}");
}
