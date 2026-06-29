//! Phase-10(b) acceptance: the access tracker catches the instruction writing a
//! watched address. Only compiled/run with the `access-tracker` feature.
#![cfg(feature = "access-tracker")]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

use reclass_backend_vmem::tracker::{watch, Access, TrackError};
use reclass_backend_vmem::VmemBackend;
use reclass_core::MemoryBackend;

#[test]
fn detects_writer_instruction() {
    let exe = env!("CARGO_BIN_EXE_writer");
    let mut child = Command::new(exe)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn writer");
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).expect("read addr");
    let addr = u64::from_str_radix(line.trim().trim_start_matches("0x"), 16).expect("addr");

    let result = watch(
        child.id() as i32,
        addr,
        8,
        Access::Write,
        Duration::from_secs(2),
        4,
    );

    // grab the writer's module range before killing it, for a sanity bound
    let regions = VmemBackend::by_pid(child.id() as i32)
        .ok()
        .and_then(|b| b.regions().ok());

    let _ = child.kill();
    let _ = child.wait();

    match result {
        Ok(hits) => {
            assert!(!hits.is_empty(), "no write access captured");
            for h in &hits {
                assert_ne!(h.rip, 0, "captured a null RIP");
            }
            // each RIP should fall inside some mapped, executable region
            if let Some(regions) = regions {
                let rip = hits[0].rip;
                assert!(
                    regions.iter().any(|r| r.contains(rip) && r.perms.execute),
                    "captured RIP {rip:#x} not in an executable region"
                );
            }
        }
        Err(TrackError::Ptrace { op, errno }) => {
            // some sandboxes forbid debug-register access; don't fail the suite
            eprintln!("SKIP: ptrace {op} errno {errno}");
        }
        Err(e) => panic!("tracker error: {e}"),
    }
}
