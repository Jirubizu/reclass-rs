//! End-to-end acceptance for the pointer scanner against a real process.
//!
//! Spawns the `target` helper, which holds a heap array whose address is also
//! stored in a *static* slot in its own binary. The scanner must find that
//! static slot and produce an address expression that resolves back to the
//! array — the whole workflow the feature exists for, over `/proc` and
//! `process_vm_readv` rather than a mock.
//!
//! Self-skips when the environment forbids ptrace, matching `live_read.rs`.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use reclass_backend_vmem::VmemBackend;
use reclass_core::scan::{ScanConfig, scan_pointers};
use reclass_core::{AddrExpr, MemError};

#[test]
fn finds_a_static_path_to_a_heap_allocation() {
    let exe = env!("CARGO_BIN_EXE_target");
    let mut child = Command::new(exe)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn target");

    let mut reader = BufReader::new(child.stdout.take().expect("target stdout"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read target line");
    let fields: Vec<&str> = line.split_whitespace().collect();
    let arr_addr = u64::from_str_radix(fields[2].trim_start_matches("0x"), 16).expect("arr addr");

    let outcome = (|| -> Result<Option<String>, MemError> {
        let be = VmemBackend::by_pid(child.id() as i32)?;
        // Depth 1 is enough: the static slot points straight at the array. A
        // deeper scan of a whole live process is slow and would find the same
        // path plus noise.
        let cfg = ScanConfig {
            max_depth: 1,
            max_offset: 0x40,
            max_results: 16,
            ..Default::default()
        };
        let paths = scan_pointers(&be, arr_addr, &cfg)?;
        let target_name = std::path::Path::new(exe)
            .file_name()
            .and_then(|s| s.to_str())
            .expect("target file name")
            .to_string();

        let found = paths.iter().find(|p| p.module == target_name);
        let Some(found) = found else {
            panic!(
                "no path through {target_name} to {arr_addr:#x}; got {:#?}",
                paths
            );
        };
        let expr = found.to_expr();
        // The expression is the deliverable: it has to parse and resolve back
        // to the address we scanned for.
        let resolved =
            AddrExpr::resolve(&expr, &be).unwrap_or_else(|e| panic!("{expr} did not resolve: {e}"));
        assert_eq!(resolved, arr_addr, "{expr} resolved to the wrong address");
        Ok(Some(expr))
    })();

    let _ = child.kill();
    let _ = child.wait();

    match outcome {
        Ok(Some(expr)) => println!("resolved static path: {expr}"),
        Ok(None) => {}
        Err(MemError::Permission) => {
            eprintln!("skipping: ptrace not permitted in this environment");
        }
        Err(e) => panic!("scan failed: {e}"),
    }
}
