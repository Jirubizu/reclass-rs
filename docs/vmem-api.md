# `vmem` capability → API mapping

Source inspected: `/home/jirubizu/dev/vmem/src/lib.rs` (Linux-only, x86-64).
`vmem` addresses are `usize`; on the only supported target (`x86-64 Linux`) that is
64-bit, so our `MemoryBackend` trait uses `u64` and the `backend-vmem` impl casts
`u64 <-> usize` at the boundary.

All cross-process I/O uses `process_vm_readv` / `process_vm_writev` (no `ptrace`
stop). Permissions: same UID with `ptrace_scope <= 1` (descendants allowed under
scope 1), `cap_sys_ptrace`, or root. Failures surface as `vmem::Error::Permission`.

## Capability checklist (PLAN §1)

| Capability | Required by | `vmem` API | Status |
|---|---|---|---|
| Process discovery / attach | discovery | `Process::by_pid(i32)`, `Process::by_name(&str)`, `Process::all_by_name(&str) -> Vec<i32>`, `Process::pid()` | ✅ present |
| Typed reads/writes (`T: Pod`) | read/write | `Process::read<T>(usize)`, `Process::write<T>(usize, T)`, `read_bytes`, `write_bytes`, `read_vec`, `read_cstring`, `write_force` | ✅ present |
| Pointer chains | expr deref | `Process::pointer(usize) -> Pointer`, `.offset/.offsets/.offset_first/.resolve/.read/.write` | ✅ present |
| Scatter / batched reads | render loop | `Process::scatter() -> Scatter`, `Scatter::add(addr,len)`, `add_typed::<T>(addr)`, `run() -> Vec<Vec<u8>>`, `vmem::pod_at::<T>(&bufs, i)` | ✅ present — **single `process_vm_readv` syscall** (auto-chunked past `IOV_MAX` = 1024) |
| Region enumeration (`/proc/<pid>/maps`) | memory map view | `Process::maps() -> Vec<MapRegion>`, `Process::module(&str) -> Module`, `Process::modules() -> Vec<Module>` | ✅ present — no shim needed |

**No capability gaps.** The performance-critical scatter primitive and region
enumeration are both first-class in `vmem`.

## Type mapping used by `backend-vmem`

| Our trait | `vmem` |
|---|---|
| `MemoryBackend::read(addr: u64, buf)` | `Process::read_bytes(addr as usize, buf)` |
| `MemoryBackend::write(addr: u64, data)` | `Process::write_bytes(addr as usize, data)` |
| `MemoryBackend::read_scatter(&mut [ScatterReq])` | one `Process::scatter()` → `add(addr,len)` per req → `run()` → copy each `Vec<u8>` slot back into the req buffer |
| `MemoryBackend::regions() -> Vec<Region>` | `Process::maps()`; `MapRegion{start,end,perms:String,path}` → `Region{start,end,Perms{r,w,x,shared},path}` |
| `MemoryBackend::module_base(name) -> Option<u64>` | `Process::module(name).ok().map(|m| m.base as u64)` |

### `vmem::Error` → `MemError`

`vmem::Error` is `#[non_exhaustive]`. Mapped variants:
`Permission`, `Unmapped`, `Partial`, `ModuleNotFound`, `ProcessNotFound`, `Io`,
everything else → `MemError::Backend(String)`. We never expose `vmem::Error` from
`core` (which has no `vmem` dependency); only `backend-vmem` converts.

## Notes / gotchas

* `Scatter::run()` consumes `self` and allocates one `Vec<u8>` per request. To keep
  the render loop allocation-light we copy into caller-owned buffers (`ScatterReq.buf`)
  and reuse those across ticks; the per-tick `Vec<Vec<u8>>` churn lives inside `vmem`.
* `read_cstring` stops at NUL / `max` / first unreadable chunk — handy for `Text`
  nodes that point off into other mappings.
* Pointer deref convention is Cheat-Engine style (deref **then** add offset). Our
  address-expression `[..]` operator is a single pointer-sized deref, which we
  implement directly via `read::<u64>` rather than `Pointer` to keep semantics
  explicit and unit-testable against `MockBackend`.
