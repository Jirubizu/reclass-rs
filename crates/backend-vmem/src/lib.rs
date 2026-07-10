//! [`MemoryBackend`] implemented over the `vmem` crate.
//!
//! See `docs/vmem-api.md` for the capability → signature mapping. Addresses are
//! `u64` in `core`'s trait and `usize` in `vmem`; on the only supported target
//! (x86-64 Linux) those are the same width, and we cast at this boundary.
// `unsafe` lives in `select_backend` (env var) and the `tracker` module
// (ptrace); each call is SAFETY-noted.

use reclass_core::{MemError, MemoryBackend, Perms, Region, ScatterReq};
use vmem::Process;

#[cfg(feature = "access-tracker")]
pub mod tracker;

/// A discovered process for the UI picker.
#[derive(Clone, Debug)]
pub struct ProcInfo {
    /// Process id.
    pub pid: i32,
    /// `comm` name (kernel-truncated to 15 bytes).
    pub name: String,
}

/// List user-visible processes by scanning `/proc` (pid + `comm`), ascending.
pub fn list_processes() -> Vec<ProcInfo> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out: Vec<ProcInfo> = dir
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let name = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            Some(ProcInfo { pid, name })
        })
        .collect();
    out.sort_by_key(|p| p.pid);
    out
}

/// The `comm` name of a pid, if readable.
pub fn process_name(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The `/dev/vmem` char device path (kernel driver).
const VMEM_DEVICE: &str = "/dev/vmem";

/// Check whether the vmem kernel module is loaded and the device is usable.
///
/// Opens `/dev/vmem` read-write; drops the fd immediately.
#[must_use]
pub fn kernel_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(VMEM_DEVICE)
        .is_ok()
}

/// Select the vmem backend for this process.
///
/// Must be called once at startup, before any `VmemBackend` is created and
/// while the process is still single-threaded. If `use_kernel` is true, sets
/// `VMEM_BACKEND=kernel` (vmem probes `/dev/vmem` and falls back to syscalls
/// if unavailable). If false, sets `VMEM_BACKEND=syscall` to force the
/// userspace path regardless of whether the device exists.
///
/// # Safety
/// Not thread-safe. Call exactly once before spawning threads or touching
/// memory backends.
pub unsafe fn select_backend(use_kernel: bool) {
    if use_kernel {
        // SAFETY: caller's contract (§Safety) guarantees single-threaded startup.
        unsafe { std::env::set_var("VMEM_BACKEND", "kernel") };
    } else {
        // SAFETY: same contract.
        unsafe { std::env::set_var("VMEM_BACKEND", "syscall") };
    }
}

/// A live target process exposed as a [`MemoryBackend`].
#[derive(Clone, Debug)]
pub struct VmemBackend {
    proc: Process,
}

impl VmemBackend {
    /// Attach by pid.
    ///
    /// # Errors
    /// [`MemError::NoProcess`] if no process has this pid, or
    /// [`MemError::Permission`] if access is denied.
    pub fn by_pid(pid: i32) -> Result<Self, MemError> {
        Process::by_pid(pid)
            .map(|proc| VmemBackend { proc })
            .map_err(map_err)
    }

    /// Attach to the first process matching `name`.
    ///
    /// # Errors
    /// [`MemError::NoProcess`] if nothing matches `name`, or
    /// [`MemError::Permission`] if access is denied.
    pub fn by_name(name: &str) -> Result<Self, MemError> {
        Process::by_name(name)
            .map(|proc| VmemBackend { proc })
            .map_err(map_err)
    }

    /// Every pid currently matching `name`, ascending.
    #[must_use]
    pub fn pids_by_name(name: &str) -> Vec<i32> {
        Process::all_by_name(name)
    }

    /// The underlying pid.
    #[must_use]
    pub fn pid(&self) -> i32 {
        self.proc.pid()
    }

    /// The underlying `vmem` handle (for advanced/stretch features).
    #[must_use]
    pub fn process(&self) -> Process {
        self.proc
    }
}

fn map_err(e: vmem::Error) -> MemError {
    match e {
        vmem::Error::Permission { .. } => MemError::Permission,
        vmem::Error::ProcessNotFound(_) => MemError::NoProcess,
        vmem::Error::Unmapped { addr, len } => MemError::Unmapped {
            addr: addr as u64,
            len,
        },
        vmem::Error::Partial {
            addr,
            wanted,
            moved,
        } => MemError::Unmapped {
            // `moved` bytes were readable; the hole starts after them.
            addr: addr as u64 + moved as u64,
            len: wanted.saturating_sub(moved),
        },
        vmem::Error::ModuleNotFound { module, .. } => {
            MemError::Backend(format!("module '{module}' not found"))
        }
        other => MemError::Backend(other.to_string()),
    }
}

impl MemoryBackend for VmemBackend {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.proc.read_bytes(addr as usize, buf).map_err(map_err)
    }

    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemError> {
        self.proc.write_bytes(addr as usize, data).map_err(map_err)
    }

    fn read_scatter(&self, reqs: &mut [ScatterReq<'_>]) -> Result<(), MemError> {
        if reqs.is_empty() {
            return Ok(());
        }
        // One `process_vm_readv` (auto-chunked past IOV_MAX inside vmem).
        let mut scatter = self.proc.scatter();
        for req in reqs.iter() {
            scatter.add(req.addr as usize, req.buf.len());
        }
        let bufs = scatter.run().map_err(map_err)?;
        for (req, data) in reqs.iter_mut().zip(bufs) {
            // vmem returns one buffer per request, each exactly the requested
            // length; assert it so a future contract change fails loudly.
            debug_assert_eq!(req.buf.len(), data.len());
            req.buf.copy_from_slice(&data);
        }
        Ok(())
    }

    fn regions(&self) -> Result<Vec<Region>, MemError> {
        let maps = self.proc.maps().map_err(map_err)?;
        Ok(maps
            .into_iter()
            .map(|m| Region {
                start: m.start as u64,
                end: m.end as u64,
                perms: Perms::parse(&m.perms),
                path: m.path,
            })
            .collect())
    }

    fn module_base(&self, name: &str) -> Option<u64> {
        self.proc.module(name).ok().map(|m| m.base as u64)
    }
}
