//! Hardware-breakpoint access tracker (Phase 10 stretch; x86-64 Linux).
//!
//! Answers "what instruction writes/accesses this address" by `ptrace`-attaching
//! to the target, arming debug register **DR0** with a data watchpoint via DR7,
//! and recording the faulting RIP each time it traps. Gated behind the
//! `access-tracker` feature; this module holds the crate's only `unsafe`.
//!
//! Limitations: the watchpoint is armed on the attached (main) thread only, so
//! accesses from other threads are not seen. A watchdog `SIGSTOP` bounds the
//! wait so a quiet address cannot hang the call.

use std::os::raw::c_void;
use std::time::{Duration, Instant};

/// Which access to trap on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Trap on writes only.
    Write,
    /// Trap on reads and writes.
    ReadWrite,
}

/// One captured access: the instruction pointer just after the access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessHit {
    /// RIP reported at the trap (the instruction following the access).
    pub rip: u64,
}

/// Tracker error.
#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    /// A `ptrace` request failed.
    #[error("ptrace {op} failed (errno {errno})")]
    Ptrace {
        /// The failing request name.
        op: &'static str,
        /// `errno` at failure.
        errno: i32,
    },
    /// Watch size must be 1, 2, 4, or 8 bytes.
    #[error("unsupported watch size {0} (must be 1, 2, 4, or 8)")]
    Size(usize),
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// SAFETY: thin wrapper over the variadic `ptrace`. All pointer arguments are
/// either null or valid for the duration of the call.
unsafe fn ptrace(req: libc::c_uint, pid: libc::pid_t, addr: usize, data: usize) -> libc::c_long {
    libc::ptrace(req, pid, addr as *mut c_void, data as *mut c_void)
}

/// Watch `addr` (`size` bytes) for `access` on `pid` for up to `duration`,
/// stopping early after `max_hits`. Returns the captured instruction pointers.
///
/// Requires ptrace rights over `pid` (same UID + `ptrace_scope <= 1`, or root).
pub fn watch(
    pid: i32,
    addr: u64,
    size: usize,
    access: Access,
    duration: Duration,
    max_hits: usize,
) -> Result<Vec<AccessHit>, TrackError> {
    let len_bits: u64 = match size {
        1 => 0b00,
        2 => 0b01,
        4 => 0b11,
        8 => 0b10,
        other => return Err(TrackError::Size(other)),
    };
    let rw_bits: u64 = match access {
        Access::Write => 0b01,
        Access::ReadWrite => 0b11,
    };
    // DR7: enable local breakpoint 0 (bit 0), set RW0 (bits 16-17) and LEN0 (18-19).
    let dr7: u64 = 1 | (rw_bits << 16) | (len_bits << 18);
    let dbg = std::mem::offset_of!(libc::user, u_debugreg);

    // SAFETY: every ptrace call below targets `pid`, which we attach to at the
    // start and always detach from before returning; the GETREGS buffer is a
    // live local. Failures are surfaced as `TrackError` and still detach.
    unsafe {
        if ptrace(libc::PTRACE_ATTACH, pid, 0, 0) < 0 {
            return Err(TrackError::Ptrace {
                op: "ATTACH",
                errno: errno(),
            });
        }
        // wait for the attach-stop
        let mut status: libc::c_int = 0;
        libc::waitpid(pid, &mut status, 0);

        // arm DR0 + DR7
        if ptrace(libc::PTRACE_POKEUSER, pid, dbg, addr as usize) < 0 {
            let e = errno();
            cleanup(pid, dbg);
            return Err(TrackError::Ptrace {
                op: "POKEUSER DR0",
                errno: e,
            });
        }
        if ptrace(libc::PTRACE_POKEUSER, pid, dbg + 7 * 8, dr7 as usize) < 0 {
            let e = errno();
            cleanup(pid, dbg);
            return Err(TrackError::Ptrace {
                op: "POKEUSER DR7",
                errno: e,
            });
        }

        // watchdog: unblock the final waitpid if the address stays quiet
        let deadline = Instant::now() + duration;
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(duration);
            // SAFETY: kill is always safe to call with a pid + signal.
            libc::kill(pid, libc::SIGSTOP);
        });

        let mut hits = Vec::new();
        loop {
            if ptrace(libc::PTRACE_CONT, pid, 0, 0) < 0 {
                break;
            }
            let mut status: libc::c_int = 0;
            if libc::waitpid(pid, &mut status, 0) < 0 {
                break;
            }
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                break;
            }
            if libc::WIFSTOPPED(status) {
                let sig = libc::WSTOPSIG(status);
                if sig == libc::SIGTRAP {
                    let dr6 = ptrace(libc::PTRACE_PEEKUSER, pid, dbg + 6 * 8, 0) as u64;
                    if dr6 & 0xF != 0 {
                        let mut regs: libc::user_regs_struct = std::mem::zeroed();
                        if ptrace(
                            libc::PTRACE_GETREGS,
                            pid,
                            0,
                            &mut regs as *mut libc::user_regs_struct as usize,
                        ) >= 0
                        {
                            hits.push(AccessHit { rip: regs.rip });
                        }
                        // clear DR6 status bits
                        ptrace(libc::PTRACE_POKEUSER, pid, dbg + 6 * 8, 0);
                    }
                    if hits.len() >= max_hits {
                        break;
                    }
                } else if sig == libc::SIGSTOP {
                    // our watchdog (or an external stop): stop tracking
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
        }

        cleanup(pid, dbg);
        let _ = watchdog.join();
        Ok(hits)
    }
}

/// Clear DR7 (disarm) and detach. SAFETY: see [`watch`].
unsafe fn cleanup(pid: i32, dbg: usize) {
    ptrace(libc::PTRACE_POKEUSER, pid, dbg + 7 * 8, 0);
    ptrace(libc::PTRACE_DETACH, pid, 0, 0);
}
