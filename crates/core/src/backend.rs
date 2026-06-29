//! The memory-access seam.
//!
//! [`MemoryBackend`] is the only thing `core` knows about a target process. The
//! real implementation lives in `backend-vmem`; [`MockBackend`] (feature
//! `mock`, on by default) provides an in-memory fake so the whole model and
//! render loop are unit-testable with no live process.

/// Errors any backend can produce. `core` never sees `vmem::Error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemError {
    /// The address range is not mapped (or not readable/writable) in the target.
    #[error("address {addr:#x} (+{len}) is not accessible in the target")]
    Unmapped {
        /// Start of the offending range.
        addr: u64,
        /// Length of the range in bytes.
        len: usize,
    },
    /// Permission denied (ptrace scope, missing capability, …).
    #[error("permission denied accessing the target")]
    Permission,
    /// The process is gone or never existed.
    #[error("target process not available")]
    NoProcess,
    /// Anything the backend could not classify more precisely.
    #[error("backend error: {0}")]
    Backend(String),
}

/// Read/write/execute (and shared) permission bits of a mapped region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Perms {
    /// Region is readable.
    pub read: bool,
    /// Region is writable.
    pub write: bool,
    /// Region is executable.
    pub execute: bool,
    /// Region is shared (vs private/copy-on-write).
    pub shared: bool,
}

impl Perms {
    /// Parse the four `rwxp` characters from `/proc/<pid>/maps`.
    pub fn parse(s: &str) -> Self {
        let b = s.as_bytes();
        Perms {
            read: b.first() == Some(&b'r'),
            write: b.get(1) == Some(&b'w'),
            execute: b.get(2) == Some(&b'x'),
            shared: b.get(3) == Some(&b's'),
        }
    }
}

impl std::fmt::Display for Perms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}{}{}",
            if self.read { 'r' } else { '-' },
            if self.write { 'w' } else { '-' },
            if self.execute { 'x' } else { '-' },
            if self.shared { 's' } else { 'p' },
        )
    }
}

/// One mapped virtual-memory region (a `/proc/<pid>/maps` line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    /// First address (inclusive).
    pub start: u64,
    /// One past the last address (exclusive).
    pub end: u64,
    /// Permission bits.
    pub perms: Perms,
    /// Backing path, or `None` for anonymous / special maps.
    pub path: Option<String>,
}

impl Region {
    /// Length in bytes.
    #[inline]
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
    /// Whether the region is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
    /// Whether `addr` falls within `[start, end)`.
    #[inline]
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.start && addr < self.end
    }
}

/// One slot of a batched [`MemoryBackend::read_scatter`] request: read
/// `buf.len()` bytes from `addr` into `buf`.
#[derive(Debug)]
pub struct ScatterReq<'a> {
    /// Address to read from.
    pub addr: u64,
    /// Destination buffer; its length is the read length.
    pub buf: &'a mut [u8],
}

impl<'a> ScatterReq<'a> {
    /// Build a request to fill `buf` from `addr`.
    #[inline]
    pub fn new(addr: u64, buf: &'a mut [u8]) -> Self {
        ScatterReq { addr, buf }
    }
}

/// Abstract access to a target process's memory.
///
/// `read_scatter` is the performance-critical primitive: the render loop issues
/// **one** call per pointer-chain level instead of one syscall per node.
pub trait MemoryBackend {
    /// Read `buf.len()` bytes from `addr` into `buf`.
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemError>;

    /// Write `data` to `addr`.
    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemError>;

    /// Fill every request's buffer from its address in as few syscalls as
    /// possible. The default routes each request through [`read`](Self::read);
    /// real backends override with a true scatter read.
    fn read_scatter(&self, reqs: &mut [ScatterReq<'_>]) -> Result<(), MemError> {
        for req in reqs.iter_mut() {
            self.read(req.addr, req.buf)?;
        }
        Ok(())
    }

    /// Enumerate mapped regions (`/proc/<pid>/maps`).
    fn regions(&self) -> Result<Vec<Region>, MemError>;

    /// Resolve a module's load base by file basename, if mapped.
    fn module_base(&self, name: &str) -> Option<u64>;
}

// A `&T` and `Box<T>`/`Arc<T>` of a backend are themselves backends, so the app
// can hold `Arc<dyn MemoryBackend>` and pass `&*backend` into `core`.
impl<T: MemoryBackend + ?Sized> MemoryBackend for &T {
    fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        (**self).read(addr, buf)
    }
    fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemError> {
        (**self).write(addr, data)
    }
    fn read_scatter(&self, reqs: &mut [ScatterReq<'_>]) -> Result<(), MemError> {
        (**self).read_scatter(reqs)
    }
    fn regions(&self) -> Result<Vec<Region>, MemError> {
        (**self).regions()
    }
    fn module_base(&self, name: &str) -> Option<u64> {
        (**self).module_base(name)
    }
}

#[cfg(feature = "mock")]
mod mock {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::BTreeMap;

    /// An in-memory [`MemoryBackend`] for tests, benches, and offline UI.
    ///
    /// Memory is a set of byte blocks keyed by start address; a read must fall
    /// entirely within one block. Counters record how many times each access
    /// path ran so tests can assert the render loop batches with `read_scatter`.
    #[derive(Debug, Default)]
    pub struct MockBackend {
        inner: Mutex<Inner>,
    }

    #[derive(Debug, Default)]
    struct Inner {
        blocks: BTreeMap<u64, Vec<u8>>,
        modules: BTreeMap<String, u64>,
        regions: Vec<Region>,
        read_calls: u64,
        scatter_calls: u64,
    }

    impl MockBackend {
        /// Empty backend.
        pub fn new() -> Self {
            Self::default()
        }

        /// Place a block of bytes at `addr` (later writes mutate it in place).
        pub fn put(&self, addr: u64, bytes: impl Into<Vec<u8>>) {
            self.inner.lock().blocks.insert(addr, bytes.into());
        }

        /// Register a module base for `module_base`.
        pub fn put_module(&self, name: impl Into<String>, base: u64) {
            self.inner.lock().modules.insert(name.into(), base);
        }

        /// Register a region for `regions`.
        pub fn put_region(&self, region: Region) {
            self.inner.lock().regions.push(region);
        }

        /// Number of single `read` calls served so far.
        pub fn read_calls(&self) -> u64 {
            self.inner.lock().read_calls
        }

        /// Number of `read_scatter` calls served so far.
        pub fn scatter_calls(&self) -> u64 {
            self.inner.lock().scatter_calls
        }

        /// Reset the access counters.
        pub fn reset_counters(&self) {
            let mut g = self.inner.lock();
            g.read_calls = 0;
            g.scatter_calls = 0;
        }
    }

    impl Inner {
        /// Copy `[addr, addr+len)` out of the single block that contains it.
        fn fetch(&self, addr: u64, len: usize) -> Result<Vec<u8>, MemError> {
            if len == 0 {
                return Ok(Vec::new());
            }
            // Greatest block start <= addr.
            if let Some((&start, bytes)) = self.blocks.range(..=addr).next_back() {
                let off = (addr - start) as usize;
                if off.checked_add(len).is_some_and(|end| end <= bytes.len()) {
                    return Ok(bytes[off..off + len].to_vec());
                }
            }
            Err(MemError::Unmapped { addr, len })
        }
    }

    impl MemoryBackend for MockBackend {
        fn read(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
            let mut g = self.inner.lock();
            g.read_calls += 1;
            let data = g.fetch(addr, buf.len())?;
            buf.copy_from_slice(&data);
            Ok(())
        }

        fn write(&self, addr: u64, data: &[u8]) -> Result<(), MemError> {
            if data.is_empty() {
                return Ok(());
            }
            let mut g = self.inner.lock();
            if let Some((&start, bytes)) = g.blocks.range_mut(..=addr).next_back() {
                let off = (addr - start) as usize;
                if off
                    .checked_add(data.len())
                    .is_some_and(|end| end <= bytes.len())
                {
                    bytes[off..off + data.len()].copy_from_slice(data);
                    return Ok(());
                }
            }
            Err(MemError::Unmapped {
                addr,
                len: data.len(),
            })
        }

        fn read_scatter(&self, reqs: &mut [ScatterReq<'_>]) -> Result<(), MemError> {
            let mut g = self.inner.lock();
            g.scatter_calls += 1;
            // Fetch first (immutable borrow of blocks) then copy back.
            let fetched: Vec<Vec<u8>> = reqs
                .iter()
                .map(|r| g.fetch(r.addr, r.buf.len()))
                .collect::<Result<_, _>>()?;
            for (req, data) in reqs.iter_mut().zip(fetched) {
                req.buf.copy_from_slice(&data);
            }
            Ok(())
        }

        fn regions(&self) -> Result<Vec<Region>, MemError> {
            Ok(self.inner.lock().regions.clone())
        }

        fn module_base(&self, name: &str) -> Option<u64> {
            self.inner.lock().modules.get(name).copied()
        }
    }
}

#[cfg(feature = "mock")]
pub use mock::MockBackend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perms_parse_and_display() {
        let p = Perms::parse("r-xp");
        assert!(p.read && !p.write && p.execute && !p.shared);
        assert_eq!(p.to_string(), "r-xp");
        assert_eq!(Perms::parse("rw-s").to_string(), "rw-s");
    }

    #[cfg(feature = "mock")]
    #[test]
    fn mock_read_write_roundtrip() {
        let m = MockBackend::new();
        m.put(0x1000, vec![0u8; 16]);
        m.write(0x1004, &42u32.to_le_bytes()).unwrap();
        let mut buf = [0u8; 4];
        m.read(0x1004, &mut buf).unwrap();
        assert_eq!(u32::from_le_bytes(buf), 42);
    }

    #[cfg(feature = "mock")]
    #[test]
    fn mock_unmapped_and_oob() {
        let m = MockBackend::new();
        m.put(0x2000, vec![0u8; 8]);
        assert!(matches!(
            m.read(0x9000, &mut [0u8; 4]),
            Err(MemError::Unmapped { .. })
        ));
        // straddling the end of the block
        assert!(matches!(
            m.read(0x2006, &mut [0u8; 4]),
            Err(MemError::Unmapped { .. })
        ));
    }

    #[cfg(feature = "mock")]
    #[test]
    fn mock_scatter_is_one_call() {
        let m = MockBackend::new();
        m.put(0x1000, (0u8..32).collect::<Vec<_>>());
        let mut a = [0u8; 4];
        let mut b = [0u8; 4];
        {
            let mut reqs = [
                ScatterReq::new(0x1000, &mut a),
                ScatterReq::new(0x1010, &mut b),
            ];
            m.read_scatter(&mut reqs).unwrap();
        }
        assert_eq!(a, [0, 1, 2, 3]);
        assert_eq!(b, [16, 17, 18, 19]);
        assert_eq!(m.scatter_calls(), 1);
        assert_eq!(m.read_calls(), 0);
    }
}
