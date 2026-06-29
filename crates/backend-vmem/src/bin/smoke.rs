//! Throwaway Phase-0 CLI: attach to a process and dump memory through the
//! `MemoryBackend` trait, proving the `vmem` backend is wired up.
//!
//! Usage:
//!   smoke <pid|name> <hex-addr> [len]
//!   smoke <pid|name> --maps          # list mapped regions
//!   smoke <pid|name> --modules <name>  # print a module base

use reclass_backend_vmem::VmemBackend;
use reclass_core::{MemoryBackend, ScatterReq};

fn attach(target: &str) -> Result<VmemBackend, String> {
    if let Ok(pid) = target.parse::<i32>() {
        VmemBackend::by_pid(pid).map_err(|e| e.to_string())
    } else {
        VmemBackend::by_name(target).map_err(|e| e.to_string())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: smoke <pid|name> <hex-addr> [len] | --maps | --modules <name>");
        std::process::exit(2);
    }
    let backend = match attach(&args[0]) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("attach failed: {e}");
            std::process::exit(1);
        }
    };
    println!("attached pid = {}", backend.pid());

    match args[1].as_str() {
        "--maps" => {
            let regions = backend.regions().unwrap_or_default();
            println!("{} regions", regions.len());
            for r in regions.iter().take(40) {
                println!(
                    "  {:#014x}-{:#014x} {} {}",
                    r.start,
                    r.end,
                    r.perms,
                    r.path.as_deref().unwrap_or("")
                );
            }
        }
        "--modules" => {
            let name = args.get(2).map(String::as_str).unwrap_or("");
            match backend.module_base(name) {
                Some(b) => println!("module {name} base = {b:#x}"),
                None => println!("module {name} not found"),
            }
        }
        addr_str => {
            let addr = match u64::from_str_radix(addr_str.trim_start_matches("0x"), 16) {
                Ok(a) => a,
                Err(_) => {
                    eprintln!("bad address: {addr_str}");
                    std::process::exit(2);
                }
            };
            let len: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(64);
            let mut buf = vec![0u8; len];
            if let Err(e) = backend.read(addr, &mut buf) {
                eprintln!("read failed: {e}");
                std::process::exit(1);
            }
            hexdump(addr, &buf);

            // demonstrate a batched scatter read of the first 8 + last 8 bytes
            if len >= 16 {
                let mut a = [0u8; 8];
                let mut b = [0u8; 8];
                let mut reqs = [
                    ScatterReq::new(addr, &mut a),
                    ScatterReq::new(addr + (len as u64 - 8), &mut b),
                ];
                if backend.read_scatter(&mut reqs).is_ok() {
                    println!(
                        "scatter: head={:#018x} tail={:#018x}",
                        u64::from_le_bytes(a),
                        u64::from_le_bytes(b)
                    );
                }
            }
        }
    }
}

fn hexdump(base: u64, buf: &[u8]) {
    for (i, chunk) in buf.chunks(16).enumerate() {
        let mut hex = String::new();
        let mut ascii = String::new();
        for b in chunk {
            hex.push_str(&format!("{b:02x} "));
            ascii.push(if b.is_ascii_graphic() {
                *b as char
            } else {
                '.'
            });
        }
        println!("{:#014x}  {:<48} {}", base + (i * 16) as u64, hex, ascii);
    }
}
