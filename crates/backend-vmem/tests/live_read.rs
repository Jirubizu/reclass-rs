//! Phase-0 acceptance: read live memory of a real process through the trait.
//!
//! Spawns the `target` helper (same UID), parses the sentinel addresses it
//! prints, and reads them back via `VmemBackend`. If the environment forbids
//! ptrace (`MemError::Permission`), the test self-skips rather than failing.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use reclass_backend_vmem::VmemBackend;
use reclass_core::{MemError, MemoryBackend, ScatterReq};

struct Sentinels {
    heap_addr: u64,
    heap_val: u64,
    arr_addr: u64,
    arr: [u32; 4],
}

fn parse(line: &str) -> Sentinels {
    let t: Vec<&str> = line.split_whitespace().collect();
    let hex = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    // <heap_addr> <heap_val> <arr_addr> <a0> <a1> <a2> <a3>
    Sentinels {
        heap_addr: hex(t[0]),
        heap_val: t[1].parse().unwrap(),
        arr_addr: hex(t[2]),
        arr: [
            t[3].parse().unwrap(),
            t[4].parse().unwrap(),
            t[5].parse().unwrap(),
            t[6].parse().unwrap(),
        ],
    }
}

#[test]
fn live_read_and_scatter_against_child() {
    let exe = env!("CARGO_BIN_EXE_target");
    let mut child = Command::new(exe)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn target");

    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).expect("read target line");
    let s = parse(line.trim());

    let result = (|| -> Result<(), MemError> {
        let backend = VmemBackend::by_pid(child.id() as i32)?;

        // single typed read
        let mut buf = [0u8; 8];
        backend.read(s.heap_addr, &mut buf)?;
        assert_eq!(u64::from_le_bytes(buf), s.heap_val, "heap value mismatch");

        // batched scatter read of the four array elements
        let mut b0 = [0u8; 4];
        let mut b1 = [0u8; 4];
        let mut b2 = [0u8; 4];
        let mut b3 = [0u8; 4];
        {
            let mut reqs = [
                ScatterReq::new(s.arr_addr, &mut b0),
                ScatterReq::new(s.arr_addr + 4, &mut b1),
                ScatterReq::new(s.arr_addr + 8, &mut b2),
                ScatterReq::new(s.arr_addr + 12, &mut b3),
            ];
            backend.read_scatter(&mut reqs)?;
        }
        assert_eq!(
            [
                u32::from_le_bytes(b0),
                u32::from_le_bytes(b1),
                u32::from_le_bytes(b2),
                u32::from_le_bytes(b3),
            ],
            s.arr
        );

        // regions enumerate and the heap address lives in a readable one
        let regions = backend.regions()?;
        assert!(!regions.is_empty(), "no regions enumerated");
        assert!(
            regions
                .iter()
                .any(|r| r.contains(s.heap_addr) && r.perms.read),
            "heap addr not in a readable region"
        );
        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();

    match result {
        Ok(()) => {}
        Err(MemError::Permission) => {
            eprintln!("SKIP: ptrace not permitted in this environment");
        }
        Err(e) => panic!("live read failed: {e}"),
    }
}
