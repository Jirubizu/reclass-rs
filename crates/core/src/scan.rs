//! Pointer scanning: find static paths to a runtime address.
//!
//! Given an address you found once — a player struct, an entity list — this
//! answers the question that makes it useful next session: *which
//! `<module>+0xBASE -> +0xOFF -> …` chain leads here?* The result feeds straight
//! into a class's address bar via [`PointerPath::to_expr`].
//!
//! The approach is the standard reverse pointer map. One pass over readable
//! memory records every pointer-aligned slot whose value lands in mapped
//! memory, keyed by the value; a breadth-first walk *backwards* from the target
//! then asks "what holds a pointer to within `max_offset` of here?" until it
//! lands in a module mapping, which is the part that survives a restart.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::backend::{MemError, MemoryBackend, Region};

/// Limits on a [`scan_pointers`] run.
///
/// Every field is a bound rather than a target: a pointer scan over a real
/// process is inherently open-ended, and the useful chains are short and
/// close-offset. Widening these costs time and memory superlinearly.
#[derive(Clone, Debug)]
pub struct ScanConfig {
    /// Maximum pointer hops in a chain (`offsets.len()`).
    pub max_depth: usize,
    /// Largest `+off` allowed at each hop.
    pub max_offset: u64,
    /// Stop once this many paths are found.
    pub max_results: usize,
    /// Target pointer width; see [`crate::class::ClassRegistry::pointer_bytes`].
    pub pointer_bytes: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            max_depth: 4,
            max_offset: 0x1000,
            max_results: 64,
            pointer_bytes: 8,
        }
    }
}

impl ScanConfig {
    /// Pointer width clamped to something a `u64` can hold.
    fn ptr(&self) -> usize {
        self.pointer_bytes.clamp(1, 8)
    }
}

/// One static path to the scanned address.
///
/// Read as: start at `module + base_offset`, dereference, then add each offset
/// in turn — dereferencing between them, but not after the last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerPath {
    /// File name of the module the chain starts in.
    pub module: String,
    /// Offset of the root pointer slot within that module.
    pub base_offset: u64,
    /// Offsets applied after each dereference, outermost first.
    pub offsets: Vec<u64>,
}

impl PointerPath {
    /// Render as an address-bar expression, e.g.
    /// `[[<game.so> + 0x1234] + 0x10] + 0x8`.
    ///
    /// Parses back through [`crate::expr::AddrExpr`] — the whole point of a
    /// found path is that it can be pasted into a class's address bar.
    #[must_use]
    pub fn to_expr(&self) -> String {
        let mut s = format!("[<{}> + 0x{:X}]", self.module, self.base_offset);
        let Some((last, rest)) = self.offsets.split_last() else {
            return s;
        };
        for off in rest {
            s = format!("[{s} + 0x{off:X}]");
        }
        // A trailing `+ 0x0` is noise, and the expression means the same
        // without it.
        if *last != 0 {
            s.push_str(&format!(" + 0x{last:X}"));
        }
        s
    }
}

/// A readable region plus, when it is file-backed, its module identity.
struct Mapped {
    start: u64,
    end: u64,
    module: Option<(String, u64)>,
}

/// Bytes read per chunk while building the reverse map. Bounded so a
/// multi-gigabyte mapping does not become one allocation.
const CHUNK: usize = 1 << 20;

/// Pointer slots recorded before the scan stops enlarging the map.
///
/// A pointer-dense process can hold tens of millions of slots; at ~40 bytes per
/// entry an unbounded map is gigabytes. Hitting the cap degrades the result
/// (some chains are missed) rather than the machine.
const MAX_SLOTS: usize = 8_000_000;

/// Find static pointer paths to `target`.
///
/// Returns at most `cfg.max_results` paths, shortest chain first. An
/// unreachable target yields an empty vec — not every address has a static
/// path, and that is an answer, not a failure.
///
/// Only [`MemoryBackend::regions`] failing aborts the scan: a per-chunk read
/// error is skipped, because a region can be unmapped by the target while the
/// scan walks it.
pub fn scan_pointers<B: MemoryBackend + ?Sized>(
    be: &B,
    target: u64,
    cfg: &ScanConfig,
) -> Result<Vec<PointerPath>, MemError> {
    let mapped = readable_regions(&be.regions()?);
    if mapped.is_empty() || cfg.max_results == 0 {
        return Ok(Vec::new());
    }
    let rev = build_reverse_map(be, &mapped, cfg);
    Ok(walk_back(target, &mapped, &rev, cfg))
}

/// Readable regions, sorted by start, each tagged with its module identity.
///
/// A module's base is the *lowest* mapped start sharing its path: a shared
/// object maps as several regions (text, rodata, data) and only the first is
/// the load base an address expression can name.
fn readable_regions(regions: &[Region]) -> Vec<Mapped> {
    let mut bases: HashMap<&str, u64> = HashMap::new();
    for r in regions.iter().filter(|r| r.perms.read && !r.is_empty()) {
        if let Some(p) = &r.path {
            let e = bases.entry(p.as_str()).or_insert(r.start);
            *e = (*e).min(r.start);
        }
    }
    let mut out: Vec<Mapped> = regions
        .iter()
        .filter(|r| r.perms.read && !r.is_empty())
        .map(|r| Mapped {
            start: r.start,
            end: r.end,
            module: r.path.as_ref().and_then(|p| {
                // Anonymous and special maps ("[stack]", "[heap]") have no
                // load base an expression could name.
                let name = p.rsplit('/').next().filter(|n| !n.starts_with('['))?;
                Some((name.to_string(), *bases.get(p.as_str())?))
            }),
        })
        .collect();
    out.sort_unstable_by_key(|m| m.start);
    out
}

/// Whether `addr` falls in any mapped region. Binary search over the
/// start-sorted list, so the value filter costs `O(log n)` per slot rather than
/// a scan of every region.
fn is_mapped(mapped: &[Mapped], addr: u64) -> bool {
    match mapped.binary_search_by_key(&addr, |m| m.start) {
        Ok(_) => true,
        Err(0) => false,
        Err(i) => addr < mapped[i - 1].end,
    }
}

/// The module a holding address belongs to, if it is file-backed.
fn module_of(mapped: &[Mapped], addr: u64) -> Option<&(String, u64)> {
    let i = match mapped.binary_search_by_key(&addr, |m| m.start) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    let m = &mapped[i];
    (addr < m.end).then_some(m.module.as_ref()).flatten()
}

/// `pointee -> addresses holding it`, over every pointer-aligned slot in
/// readable memory whose value is itself mapped.
///
/// The mapped-value filter is what keeps this tractable: most words are not
/// pointers, and recording them would bloat the map by orders of magnitude for
/// keys no chain can ever reach.
fn build_reverse_map<B: MemoryBackend + ?Sized>(
    be: &B,
    mapped: &[Mapped],
    cfg: &ScanConfig,
) -> HashMap<u64, Vec<u64>> {
    let ptr = cfg.ptr();
    let mut rev: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut buf = vec![0u8; CHUNK];
    let mut slots = 0usize;

    for m in mapped {
        let mut addr = m.start;
        while addr < m.end {
            if slots >= MAX_SLOTS {
                return rev;
            }
            let len = CHUNK.min((m.end - addr) as usize);
            let chunk = &mut buf[..len];
            if be.read(addr, chunk).is_err() {
                // The target can unmap a region mid-scan; skipping the chunk
                // loses chains through it, aborting loses the whole scan.
                addr += len as u64;
                continue;
            }
            for (i, word) in chunk.chunks_exact(ptr).enumerate() {
                let mut v = [0u8; 8];
                v[..ptr].copy_from_slice(word);
                let value = u64::from_le_bytes(v);
                if value != 0 && is_mapped(mapped, value) {
                    rev.entry(value).or_default().push(addr + (i * ptr) as u64);
                    slots += 1;
                }
            }
            addr += len as u64;
        }
    }
    rev
}

/// Breadth-first walk backwards from `target` to module-backed roots.
///
/// BFS rather than DFS so results come out shortest-chain-first, which is what
/// a person wants: a two-hop chain is far likelier to survive a game update
/// than a five-hop one.
///
// ponytail: each visited address probes `max_offset / ptr` map keys — 512 at
// the defaults. Fine at these bounds; if `max_offset` ever needs to be large,
// switch the reverse map to a sorted Vec and range-scan it instead.
fn walk_back(
    target: u64,
    mapped: &[Mapped],
    rev: &HashMap<u64, Vec<u64>>,
    cfg: &ScanConfig,
) -> Vec<PointerPath> {
    let ptr = cfg.ptr() as u64;
    let mut out = Vec::new();
    let mut seen: HashSet<u64> = HashSet::from([target]);
    // (address to reach, offsets collected so far, nearest-to-target last)
    let mut queue: VecDeque<(u64, Vec<u64>)> = VecDeque::from([(target, Vec::new())]);

    while let Some((addr, tail)) = queue.pop_front() {
        if out.len() >= cfg.max_results {
            break;
        }
        // A hop is only worth taking if the chain still has room for it.
        if tail.len() >= cfg.max_depth {
            continue;
        }
        let mut off = 0u64;
        while off <= cfg.max_offset {
            let Some(value) = addr.checked_sub(off) else {
                break;
            };
            if let Some(holders) = rev.get(&value) {
                for &holder in holders {
                    // Offsets are discovered target-first, so each new hop goes
                    // in front of the ones already found.
                    let mut offsets = Vec::with_capacity(tail.len() + 1);
                    offsets.push(off);
                    offsets.extend_from_slice(&tail);

                    if let Some((module, base)) = module_of(mapped, holder) {
                        out.push(PointerPath {
                            module: module.clone(),
                            base_offset: holder - base,
                            offsets,
                        });
                        if out.len() >= cfg.max_results {
                            return out;
                        }
                    } else if seen.insert(holder) {
                        queue.push_back((holder, offsets));
                    }
                }
            }
            off += ptr;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{MockBackend, Perms};
    use crate::expr::AddrExpr;

    const MODULE: u64 = 0x40_0000;
    const MODULE_LEN: usize = 0x1000;
    const HEAP: u64 = 0x10_0000_0000;
    /// Wider than `ScanConfig::max_offset`, so a test can place two addresses
    /// far enough apart that no accidental short chain links them.
    const HEAP_LEN: usize = 0x10000;

    fn perms() -> Perms {
        Perms {
            read: true,
            write: true,
            execute: false,
            shared: false,
        }
    }

    /// A backend with a file-backed module region and an anonymous heap region,
    /// both fully readable.
    fn backend() -> MockBackend {
        let m = MockBackend::new();
        m.put_region(Region {
            start: MODULE,
            end: MODULE + MODULE_LEN as u64,
            perms: perms(),
            path: Some("/usr/lib/game.so".into()),
        });
        m.put_region(Region {
            start: HEAP,
            end: HEAP + HEAP_LEN as u64,
            perms: perms(),
            path: None,
        });
        m.put(MODULE, vec![0u8; MODULE_LEN]);
        m.put(HEAP, vec![0u8; HEAP_LEN]);
        m
    }

    fn block_len(base: u64) -> usize {
        if base == MODULE { MODULE_LEN } else { HEAP_LEN }
    }

    /// Overwrite the 8 bytes at `addr` with `value`, in place.
    fn poke(m: &MockBackend, base: u64, addr: u64, value: u64) {
        let mut block = vec![0u8; block_len(base)];
        let _ = m.read(base, &mut block);
        let off = (addr - base) as usize;
        block[off..off + 8].copy_from_slice(&value.to_le_bytes());
        m.put(base, block);
    }

    #[test]
    fn a_module_slot_pointing_straight_at_the_target_is_one_hop() {
        let m = backend();
        let target = HEAP + 0x200;
        poke(&m, MODULE, MODULE + 0x40, target);

        let paths = scan_pointers(&m, target, &ScanConfig::default()).unwrap();
        assert_eq!(paths.len(), 1, "{paths:#?}");
        assert_eq!(paths[0].module, "game.so");
        assert_eq!(paths[0].base_offset, 0x40);
        assert_eq!(paths[0].offsets, [0]);
        // the whole point: it goes in an address bar
        assert_eq!(paths[0].to_expr(), "[<game.so> + 0x40]");
        AddrExpr::parse(&paths[0].to_expr()).expect("expression parses");
    }

    #[test]
    fn a_two_hop_chain_reproduces_the_target_when_walked() {
        let m = backend();
        let target = HEAP + 0x300;
        let mid = HEAP + 0x100;
        // module+0x80 -> mid ; mid+0x18 -> target
        poke(&m, MODULE, MODULE + 0x80, mid);
        poke(&m, HEAP, mid + 0x18, target);

        let paths = scan_pointers(&m, target, &ScanConfig::default()).unwrap();
        let two: Vec<_> = paths.iter().filter(|p| p.offsets.len() == 2).collect();
        assert!(!two.is_empty(), "{paths:#?}");
        let p = two[0];
        assert_eq!(p.offsets, [0x18, 0]);

        // walk it by hand against the same backend
        let mut cur = MODULE + p.base_offset;
        let mut buf = [0u8; 8];
        m.read(cur, &mut buf).unwrap();
        cur = u64::from_le_bytes(buf);
        for (i, off) in p.offsets.iter().enumerate() {
            cur += off;
            if i + 1 < p.offsets.len() {
                m.read(cur, &mut buf).unwrap();
                cur = u64::from_le_bytes(buf);
            }
        }
        assert_eq!(cur, target);
        assert_eq!(p.to_expr(), "[[<game.so> + 0x80] + 0x18]");
        AddrExpr::parse(&p.to_expr()).expect("expression parses");
    }

    #[test]
    fn a_chain_longer_than_max_depth_is_not_returned() {
        let m = backend();
        // Each link sits more than `max_offset` (0x1000) from the next, so the
        // only way from the module to the target is all three hops — otherwise
        // the scanner legitimately finds a shorter chain and the test is
        // measuring the fixture, not the depth cap.
        let a = HEAP + 0x2000;
        let b = HEAP + 0x6000;
        let target = HEAP + 0xA000;
        poke(&m, MODULE, MODULE + 0x100, a);
        poke(&m, HEAP, a, b);
        poke(&m, HEAP, b, target);

        let deep = ScanConfig {
            max_depth: 3,
            ..Default::default()
        };
        let found = scan_pointers(&m, target, &deep).unwrap();
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].offsets, [0, 0, 0]);

        for depth in [0, 1, 2] {
            let shallow = ScanConfig {
                max_depth: depth,
                ..Default::default()
            };
            assert!(
                scan_pointers(&m, target, &shallow).unwrap().is_empty(),
                "a 3-hop chain surfaced at max_depth {depth}"
            );
        }
    }

    #[test]
    fn an_offset_beyond_max_offset_is_not_returned() {
        let m = backend();
        let target = HEAP + 0x800;
        // the module slot points 0x400 short of the target
        poke(&m, MODULE, MODULE + 0x40, target - 0x400);

        let wide = ScanConfig {
            max_offset: 0x800,
            ..Default::default()
        };
        assert_eq!(
            scan_pointers(&m, target, &wide).unwrap()[0].offsets,
            [0x400]
        );

        let narrow = ScanConfig {
            max_offset: 0x100,
            ..Default::default()
        };
        assert!(scan_pointers(&m, target, &narrow).unwrap().is_empty());
    }

    #[test]
    fn max_results_bounds_the_output() {
        let m = backend();
        let target = HEAP + 0x500;
        for i in 0..10u64 {
            poke(&m, MODULE, MODULE + 0x200 + i * 8, target);
        }
        let cfg = ScanConfig {
            max_results: 3,
            ..Default::default()
        };
        assert_eq!(scan_pointers(&m, target, &cfg).unwrap().len(), 3);
        assert_eq!(
            scan_pointers(&m, target, &ScanConfig::default())
                .unwrap()
                .len(),
            10
        );
    }

    #[test]
    fn an_unreachable_target_is_an_empty_result_not_an_error() {
        let m = backend();
        let paths = scan_pointers(&m, HEAP + 0x600, &ScanConfig::default()).unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn a_region_that_cannot_be_read_does_not_abort_the_scan() {
        let m = backend();
        // A region with no backing block: every read of it fails.
        m.put_region(Region {
            start: 0x20_0000_0000,
            end: 0x20_0000_1000,
            perms: perms(),
            path: None,
        });
        let target = HEAP + 0x200;
        poke(&m, MODULE, MODULE + 0x40, target);

        let paths = scan_pointers(&m, target, &ScanConfig::default()).unwrap();
        assert_eq!(paths.len(), 1, "the unreadable gap swallowed the scan");
    }

    #[test]
    fn only_module_backed_holders_terminate_a_chain() {
        // A pointer living only on the anonymous heap is not a static path: it
        // moves every run, so it must not be reported as a root.
        let m = backend();
        let target = HEAP + 0x700;
        poke(&m, HEAP, HEAP + 0x30, target);
        assert!(
            scan_pointers(&m, target, &ScanConfig::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn results_come_out_shortest_chain_first() {
        let m = backend();
        let target = HEAP + 0x900;
        let mid = HEAP + 0x150;
        poke(&m, MODULE, MODULE + 0x300, target); // 1 hop
        poke(&m, MODULE, MODULE + 0x308, mid); // 2 hops
        poke(&m, HEAP, mid + 0x8, target);

        let paths = scan_pointers(&m, target, &ScanConfig::default()).unwrap();
        assert!(paths.len() >= 2, "{paths:#?}");
        assert!(
            paths
                .windows(2)
                .all(|w| w[0].offsets.len() <= w[1].offsets.len()),
            "{paths:#?}"
        );
    }

    #[test]
    fn a_pointer_cycle_terminates() {
        // Two heap slots pointing at each other; the visited set must stop the
        // walk instead of queueing them forever.
        let m = backend();
        let a = HEAP + 0x40;
        let b = HEAP + 0x48;
        poke(&m, HEAP, a, b);
        poke(&m, HEAP, b, a);
        let paths = scan_pointers(&m, a, &ScanConfig::default()).unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn expressions_render_and_parse_at_every_depth() {
        for k in 0..4usize {
            let p = PointerPath {
                module: "game.so".into(),
                base_offset: 0x1234,
                offsets: (0..k).map(|i| (i as u64 + 1) * 0x10).collect(),
            };
            let e = p.to_expr();
            AddrExpr::parse(&e).unwrap_or_else(|err| panic!("{e} did not parse: {err}"));
        }
        assert_eq!(
            PointerPath {
                module: "a.so".into(),
                base_offset: 0,
                offsets: vec![0x10, 0x20, 0x8],
            }
            .to_expr(),
            "[[[<a.so> + 0x0] + 0x10] + 0x20] + 0x8"
        );
    }

    #[test]
    fn a_32_bit_target_reads_four_byte_slots() {
        let m = MockBackend::new();
        m.put_region(Region {
            start: MODULE,
            end: MODULE + 0x100,
            perms: perms(),
            path: Some("/game32".into()),
        });
        m.put_region(Region {
            start: 0x0800_0000,
            end: 0x0800_0100,
            perms: perms(),
            path: None,
        });
        let mut module = vec![0u8; 0x100];
        let target = 0x0800_0040u32;
        module[0x20..0x24].copy_from_slice(&target.to_le_bytes());
        m.put(MODULE, module);
        m.put(0x0800_0000, vec![0u8; 0x100]);

        let cfg = ScanConfig {
            pointer_bytes: 4,
            ..Default::default()
        };
        let paths = scan_pointers(&m, u64::from(target), &cfg).unwrap();
        assert_eq!(paths.len(), 1, "{paths:#?}");
        assert_eq!(paths[0].base_offset, 0x20);
    }
}
