//! The live read loop.
//!
//! Given a set of [`Root`]s (a class + a resolved base address) and an
//! [`ExpandState`], the engine:
//!
//! 1. reads every visible class buffer for the current depth in **one**
//!    [`MemoryBackend::read_scatter`] call,
//! 2. discovers the targets of expanded [`NodeKind::ClassPtr`]s from those
//!    bytes and batches the next depth's reads,
//! 3. formats every node from its slice into a flat list of [`Row`]s in tree
//!    order.
//!
//! So a flat class costs one scatter; a depth-`d` pointer chain costs `d`
//! scatters (one per level), never one syscall per node. Class byte buffers are
//! pooled and reused across ticks to keep the hot path allocation-light.

use std::collections::HashMap;

use crate::backend::{MemoryBackend, ScatterReq};
use crate::class::{ClassId, ClassRegistry};
use crate::node::{AddrInfo, FmtCtx, NodeKind};

/// One open class view: render `class_id` as if it lives at `base`.
#[derive(Clone, Copy, Debug)]
pub struct Root {
    /// Class to render.
    pub class_id: ClassId,
    /// Already-resolved base address (see [`crate::expr`]).
    pub base: u64,
}

/// A step in a node path. `ClassInstance`/`ClassPtr` descents push `Node`,
/// array-element descents push `Elem`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PathSeg {
    /// Descend into node index `usize` of the current class.
    Node(usize),
    /// Descend into array element `usize`.
    Elem(usize),
}

/// Which expandable nodes (`ClassPtr`s) are currently followed. Keyed by
/// `(root index, root-relative node path)`.
#[derive(Clone, Debug, Default)]
pub struct ExpandState {
    /// Followed `ClassPtr`s (default collapsed; presence == expanded).
    set: std::collections::HashSet<(usize, Vec<PathSeg>)>,
    /// Collapsed aggregates — `Array`/`ClassInstance` (default expanded;
    /// presence == collapsed).
    collapsed: std::collections::HashSet<(usize, Vec<PathSeg>)>,
}

impl ExpandState {
    /// New, all collapsed.
    pub fn new() -> Self {
        Self::default()
    }
    /// Whether `path` under `root` is expanded.
    pub fn is_expanded(&self, root: usize, path: &[PathSeg]) -> bool {
        self.set.contains(&(root, path.to_vec()))
    }
    /// Mark expanded.
    pub fn expand(&mut self, root: usize, path: Vec<PathSeg>) {
        self.set.insert((root, path));
    }
    /// Mark collapsed.
    pub fn collapse(&mut self, root: usize, path: &[PathSeg]) {
        self.set.remove(&(root, path.to_vec()));
    }
    /// Flip expansion.
    pub fn toggle(&mut self, root: usize, path: Vec<PathSeg>) {
        let key = (root, path);
        if self.set.contains(&key) {
            self.set.remove(&key);
        } else {
            self.set.insert(key);
        }
    }

    /// Whether an aggregate (`Array`/`ClassInstance`) at `path` is collapsed
    /// (they default to expanded).
    pub fn is_collapsed(&self, root: usize, path: &[PathSeg]) -> bool {
        self.collapsed.contains(&(root, path.to_vec()))
    }
    /// Flip the collapsed state of an aggregate.
    pub fn toggle_collapse(&mut self, root: usize, path: Vec<PathSeg>) {
        let key = (root, path);
        if self.collapsed.contains(&key) {
            self.collapsed.remove(&key);
        } else {
            self.collapsed.insert(key);
        }
    }

    /// Mark an aggregate collapsed (used by "collapse all").
    pub fn mark_collapsed(&mut self, root: usize, path: Vec<PathSeg>) {
        self.collapsed.insert((root, path));
    }
    /// Remove every expand/collapse entry for a root (used by "expand all").
    pub fn clear_root(&mut self, root: usize) {
        self.set.retain(|(r, _)| *r != root);
        self.collapsed.retain(|(r, _)| *r != root);
    }
}

/// One rendered field.
#[derive(Clone, Debug)]
pub struct Row {
    /// Indentation depth (0 = top level of a root).
    pub depth: u32,
    /// Which root this row belongs to.
    pub root: usize,
    /// Offset within the immediate parent class / array.
    pub offset: usize,
    /// Absolute address of this node in the target.
    pub address: u64,
    /// Short type label.
    pub type_label: String,
    /// Field name.
    pub name: String,
    /// Formatted value (or summary for aggregates).
    pub value: String,
    /// Hex preview of the node's first bytes.
    pub hex: String,
    /// The node's kind (for inline editing / type menus).
    pub kind: NodeKind,
    /// Comment.
    pub comment: String,
    /// Whether this node can be expanded/collapsed (a `ClassPtr`).
    pub expandable: bool,
    /// Whether it is currently expanded.
    pub expanded: bool,
    /// Root-relative path to this node (for toggling expansion / editing).
    pub path: Vec<PathSeg>,
    /// Whether the bytes were read successfully.
    pub readable: bool,
}

struct Frame {
    class_id: ClassId,
    base: u64,
    depth: u32,
    root: usize,
    base_path: Vec<PathSeg>,
    buf: Vec<u8>,
    readable: bool,
    /// Bytes successfully read from the start of `buf` (partial reads tolerated).
    readable_len: usize,
    children: HashMap<Vec<PathSeg>, usize>,
}

struct DiscSpec {
    parent_fi: usize,
    ptr_path: Vec<PathSeg>,
    class_id: ClassId,
    target: u64,
}

struct Ctx<'a> {
    reg: &'a ClassRegistry,
    expand: &'a ExpandState,
    info: Option<&'a dyn AddrInfo>,
    array_limit: usize,
}

/// The render engine. Holds reusable buffers; create one and call
/// [`snapshot`](Self::snapshot) each tick.
#[derive(Debug, Default)]
pub struct Engine {
    buf_pool: Vec<Vec<u8>>,
    array_limit: usize,
    last_levels: usize,
}

impl Engine {
    /// A fresh engine with a default array-expansion cap of 256 elements.
    pub fn new() -> Self {
        Engine {
            buf_pool: Vec::new(),
            array_limit: 256,
            last_levels: 0,
        }
    }

    /// Maximum array elements rendered per array node.
    pub fn set_array_limit(&mut self, limit: usize) {
        self.array_limit = limit.max(1);
    }

    /// Number of read levels (scatter calls on the happy path) the last
    /// [`snapshot`](Self::snapshot) issued.
    pub fn last_read_levels(&self) -> usize {
        self.last_levels
    }

    fn take_buf(&mut self) -> Vec<u8> {
        self.buf_pool.pop().unwrap_or_default()
    }

    /// Render the visible tree, reading live memory level-by-level.
    pub fn snapshot(
        &mut self,
        backend: &dyn MemoryBackend,
        reg: &ClassRegistry,
        roots: &[Root],
        expand: &ExpandState,
        info: Option<&dyn AddrInfo>,
    ) -> Vec<Row> {
        self.last_levels = 0;
        let array_limit = if self.array_limit == 0 {
            256
        } else {
            self.array_limit
        };
        let ctx = Ctx {
            reg,
            expand,
            info,
            array_limit,
        };

        // -- build root frames --
        let mut frames: Vec<Frame> = Vec::with_capacity(roots.len());
        for (ri, r) in roots.iter().enumerate() {
            let buf = self.take_buf();
            frames.push(Frame {
                class_id: r.class_id,
                base: r.base,
                depth: 0,
                root: ri,
                base_path: Vec::new(),
                buf,
                readable: false,
                readable_len: 0,
                children: HashMap::new(),
            });
        }

        // -- BFS over depth, one batched read per level --
        let mut wave: Vec<usize> = (0..frames.len()).collect();
        Self::read_level(backend, reg, &mut frames, &wave, &mut self.last_levels);

        loop {
            let mut specs: Vec<DiscSpec> = Vec::new();
            for &fi in &wave {
                if frames[fi].readable {
                    discover_frame(&frames[fi], &ctx, fi, &mut specs);
                }
            }
            if specs.is_empty() {
                break;
            }
            let mut new_wave = Vec::with_capacity(specs.len());
            for spec in specs {
                let idx = frames.len();
                let mut base_path = spec.ptr_path.clone();
                // child frame nodes live under the ClassPtr's own path
                let root = frames[spec.parent_fi].root;
                let depth = frames[spec.parent_fi].depth + 1;
                frames[spec.parent_fi]
                    .children
                    .insert(spec.ptr_path.clone(), idx);
                base_path.shrink_to_fit();
                let buf = self.take_buf();
                frames.push(Frame {
                    class_id: spec.class_id,
                    base: spec.target,
                    depth,
                    root,
                    base_path,
                    buf,
                    readable: false,
                    readable_len: 0,
                    children: HashMap::new(),
                });
                new_wave.push(idx);
            }
            Self::read_level(backend, reg, &mut frames, &new_wave, &mut self.last_levels);
            wave = new_wave;
        }

        // -- format DFS in tree order --
        let mut rows = Vec::new();
        let n_roots = roots.len();
        for fi in 0..n_roots {
            format_class(
                &frames,
                fi,
                frames[fi].class_id,
                0,
                0,
                frames[fi].base_path.clone(),
                &ctx,
                &mut rows,
            );
        }

        // -- return buffers to the pool --
        for mut f in frames {
            f.buf.clear();
            self.buf_pool.push(std::mem::take(&mut f.buf));
        }
        rows
    }

    fn read_level(
        backend: &dyn MemoryBackend,
        reg: &ClassRegistry,
        frames: &mut [Frame],
        wave: &[usize],
        levels: &mut usize,
    ) {
        if wave.is_empty() {
            return;
        }
        // size each frame's buffer
        for &fi in wave {
            let sz = reg.size_of(frames[fi].class_id);
            frames[fi].buf.clear();
            frames[fi].buf.resize(sz, 0);
        }
        // collect indices that actually need a read (non-empty)
        let to_read: Vec<usize> = wave
            .iter()
            .copied()
            .filter(|&fi| !frames[fi].buf.is_empty())
            .collect();
        if to_read.is_empty() {
            for &fi in wave {
                frames[fi].readable = true;
                frames[fi].readable_len = 0;
            }
            return;
        }

        *levels += 1;
        // Fast path: one batched read fills every frame in the wave fully.
        if scatter_into(backend, frames, &to_read) {
            for &fi in wave {
                frames[fi].readable = true;
                frames[fi].readable_len = frames[fi].buf.len();
            }
            return;
        }

        // Slow path: isolate failures and tolerate partial reads, so the mapped
        // prefix of a class that overruns its region still renders.
        for &fi in wave {
            let len = frames[fi].buf.len();
            if len == 0 {
                frames[fi].readable = true;
                frames[fi].readable_len = 0;
                continue;
            }
            let base = frames[fi].base;
            if backend.read(base, &mut frames[fi].buf).is_ok() {
                frames[fi].readable = true;
                frames[fi].readable_len = len;
            } else {
                let got = read_partial(backend, base, &mut frames[fi].buf);
                frames[fi].readable = got > 0;
                frames[fi].readable_len = got;
                for b in &mut frames[fi].buf[got..] {
                    *b = 0;
                }
            }
        }
    }
}

/// Read as much of `buf` as is mapped, from `base`, returning the byte count of
/// the readable prefix. Coarse chunks, then byte-granular at the boundary.
fn read_partial(backend: &dyn MemoryBackend, base: u64, buf: &mut [u8]) -> usize {
    const CHUNK: usize = 256;
    let mut got = 0;
    while got < buf.len() {
        let end = (got + CHUNK).min(buf.len());
        if backend.read(base + got as u64, &mut buf[got..end]).is_ok() {
            got = end;
            continue;
        }
        // narrow to the exact boundary one byte at a time
        while got < buf.len() {
            let mut one = [0u8; 1];
            if backend.read(base + got as u64, &mut one).is_ok() {
                buf[got] = one[0];
                got += 1;
            } else {
                return got;
            }
        }
    }
    got
}

/// Issue one `read_scatter` filling the buffers of the listed frames. Returns
/// whether the batched read succeeded.
fn scatter_into(backend: &dyn MemoryBackend, frames: &mut [Frame], to_read: &[usize]) -> bool {
    // Gather disjoint &mut to the selected frames' buffers.
    // `to_read` indices are distinct, so we can collect mutable refs safely by
    // walking the slice once and matching indices.
    let mut bufs: Vec<(u64, &mut [u8])> = Vec::with_capacity(to_read.len());
    // Build a set for membership testing.
    let want: std::collections::HashSet<usize> = to_read.iter().copied().collect();
    for (i, f) in frames.iter_mut().enumerate() {
        if want.contains(&i) {
            let addr = f.base;
            bufs.push((addr, f.buf.as_mut_slice()));
        }
    }
    let mut reqs: Vec<ScatterReq<'_>> = bufs
        .iter_mut()
        .map(|(addr, buf)| ScatterReq::new(*addr, buf))
        .collect();
    backend.read_scatter(&mut reqs).is_ok()
}

fn contains_class_ref(kind: &NodeKind) -> bool {
    match kind {
        NodeKind::ClassInstance { .. } | NodeKind::ClassPtr { .. } => true,
        NodeKind::Array { element, .. } => contains_class_ref(element),
        _ => false,
    }
}

// -- discovery -------------------------------------------------------------

fn discover_frame(frame: &Frame, ctx: &Ctx<'_>, fi: usize, out: &mut Vec<DiscSpec>) {
    discover_class(
        frame,
        ctx,
        fi,
        frame.class_id,
        0,
        frame.base_path.clone(),
        out,
    );
}

fn discover_class(
    frame: &Frame,
    ctx: &Ctx<'_>,
    fi: usize,
    class_id: ClassId,
    buf_off: usize,
    base_path: Vec<PathSeg>,
    out: &mut Vec<DiscSpec>,
) {
    let Some(class) = ctx.reg.get(class_id) else {
        return;
    };
    let offsets = ctx.reg.offsets(class_id);
    for (i, node) in class.nodes.iter().enumerate() {
        let node_off = buf_off + offsets.get(i).copied().unwrap_or(0);
        let mut p = base_path.clone();
        p.push(PathSeg::Node(i));
        discover_kind(frame, ctx, fi, &node.kind, node_off, p, out);
    }
}

fn discover_kind(
    frame: &Frame,
    ctx: &Ctx<'_>,
    fi: usize,
    kind: &NodeKind,
    off: usize,
    path: Vec<PathSeg>,
    out: &mut Vec<DiscSpec>,
) {
    match kind {
        NodeKind::ClassInstance { class_id } if !ctx.expand.is_collapsed(frame.root, &path) => {
            discover_class(frame, ctx, fi, *class_id, off, path, out);
        }
        NodeKind::Array { element, count }
            if contains_class_ref(element) && !ctx.expand.is_collapsed(frame.root, &path) =>
        {
            let esz = element.size(ctx.reg);
            for e in 0..(*count).min(ctx.array_limit) {
                let mut ep = path.clone();
                ep.push(PathSeg::Elem(e));
                discover_kind(frame, ctx, fi, element, off + e * esz, ep, out);
            }
        }
        NodeKind::ClassPtr { class_id } if ctx.expand.is_expanded(frame.root, &path) => {
            if let Some(target) = read_u64(&frame.buf, off) {
                out.push(DiscSpec {
                    parent_fi: fi,
                    ptr_path: path,
                    class_id: *class_id,
                    target,
                });
            }
        }
        _ => {}
    }
}

// -- formatting ------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn format_class(
    frames: &[Frame],
    fi: usize,
    class_id: ClassId,
    buf_off: usize,
    depth: u32,
    base_path: Vec<PathSeg>,
    ctx: &Ctx<'_>,
    out: &mut Vec<Row>,
) {
    let Some(class) = ctx.reg.get(class_id) else {
        return;
    };
    let offsets = ctx.reg.offsets(class_id);
    for (i, node) in class.nodes.iter().enumerate() {
        let local_off = offsets.get(i).copied().unwrap_or(0);
        let node_off = buf_off + local_off;
        let mut p = base_path.clone();
        p.push(PathSeg::Node(i));
        format_kind(
            frames,
            fi,
            &node.kind,
            node_off,
            local_off,
            depth,
            p,
            &node.name,
            &node.comment,
            ctx,
            out,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn format_kind(
    frames: &[Frame],
    fi: usize,
    kind: &NodeKind,
    off: usize,
    local_off: usize,
    depth: u32,
    path: Vec<PathSeg>,
    name: &str,
    comment: &str,
    ctx: &Ctx<'_>,
    out: &mut Vec<Row>,
) {
    let frame = &frames[fi];
    let addr = frame.base.wrapping_add(off as u64);
    let size = kind.size(ctx.reg);
    let slice = frame.buf.get(off..off + size);
    let readable = frame.readable && slice.is_some() && off + size <= frame.readable_len;

    match kind {
        NodeKind::ClassInstance { class_id } => {
            let expanded = !ctx.expand.is_collapsed(frame.root, &path);
            out.push(Row {
                depth,
                root: frame.root,
                offset: local_off,
                address: addr,
                type_label: kind.label(ctx.reg),
                name: name.to_string(),
                value: value_of(kind, slice, addr, readable, ctx),
                hex: String::new(),
                kind: kind.clone(),
                comment: comment.to_string(),
                expandable: true,
                expanded,
                path: path.clone(),
                readable,
            });
            if expanded {
                format_class(frames, fi, *class_id, off, depth + 1, path, ctx, out);
            }
        }
        NodeKind::Array { element, count } => {
            let expanded = !ctx.expand.is_collapsed(frame.root, &path);
            out.push(Row {
                depth,
                root: frame.root,
                offset: local_off,
                address: addr,
                type_label: kind.label(ctx.reg),
                name: name.to_string(),
                value: format!("{}[{count}]", element.label(ctx.reg)),
                hex: String::new(),
                kind: kind.clone(),
                comment: comment.to_string(),
                expandable: true,
                expanded,
                path: path.clone(),
                readable,
            });
            if !expanded {
                return;
            }
            let esz = element.size(ctx.reg).max(1);
            let shown = (*count).min(ctx.array_limit);
            for e in 0..shown {
                let eoff = off + e * esz;
                let mut ep = path.clone();
                ep.push(PathSeg::Elem(e));
                format_kind(
                    frames,
                    fi,
                    element,
                    eoff,
                    e * esz,
                    depth + 1,
                    ep,
                    &format!("[{e}]"),
                    "",
                    ctx,
                    out,
                );
            }
            if *count > shown {
                out.push(Row {
                    depth: depth + 1,
                    root: frame.root,
                    offset: shown * esz,
                    address: frame.base.wrapping_add((off + shown * esz) as u64),
                    type_label: String::new(),
                    name: format!("… {} more", *count - shown),
                    value: String::new(),
                    hex: String::new(),
                    kind: NodeKind::Padding(0),
                    comment: String::new(),
                    expandable: false,
                    expanded: false,
                    path: path.clone(),
                    readable,
                });
            }
        }
        NodeKind::ClassPtr { class_id } => {
            let expanded = ctx.expand.is_expanded(frame.root, &path);
            out.push(Row {
                depth,
                root: frame.root,
                offset: local_off,
                address: addr,
                type_label: kind.label(ctx.reg),
                name: name.to_string(),
                value: value_of(kind, slice, addr, readable, ctx),
                hex: hex_preview(slice),
                kind: kind.clone(),
                comment: comment.to_string(),
                expandable: true,
                expanded,
                path: path.clone(),
                readable,
            });
            if expanded && let Some(&child_fi) = frame.children.get(&path) {
                format_class(frames, child_fi, *class_id, 0, depth + 1, path, ctx, out);
            }
        }
        NodeKind::Pointer => {
            // A plain pointer is expandable in the UI: expanding it converts the
            // node to a ClassPtr over an auto-created class (ReClass behaviour).
            out.push(Row {
                depth,
                root: frame.root,
                offset: local_off,
                address: addr,
                type_label: kind.label(ctx.reg),
                name: name.to_string(),
                value: value_of(kind, slice, addr, readable, ctx),
                hex: hex_preview(slice),
                kind: kind.clone(),
                comment: comment.to_string(),
                expandable: true,
                expanded: false,
                path,
                readable,
            });
        }
        _ => {
            out.push(Row {
                depth,
                root: frame.root,
                offset: local_off,
                address: addr,
                type_label: kind.label(ctx.reg),
                name: name.to_string(),
                value: value_of(kind, slice, addr, readable, ctx),
                hex: hex_preview(slice),
                kind: kind.clone(),
                comment: comment.to_string(),
                expandable: false,
                expanded: false,
                path,
                readable,
            });
        }
    }
}

fn value_of(
    kind: &NodeKind,
    slice: Option<&[u8]>,
    addr: u64,
    readable: bool,
    ctx: &Ctx<'_>,
) -> String {
    match slice {
        Some(bytes) if readable => {
            let fmt = FmtCtx {
                registry: ctx.reg,
                node_addr: addr,
                info: ctx.info,
            };
            kind.format(bytes, &fmt)
        }
        _ => "???".to_string(),
    }
}

fn hex_preview(slice: Option<&[u8]>) -> String {
    match slice {
        Some(bytes) => {
            let mut s = String::with_capacity(bytes.len() * 3);
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                s.push_str(&format!("{b:02X}"));
            }
            s
        }
        None => String::new(),
    }
}

fn read_u64(buf: &[u8], off: usize) -> Option<u64> {
    buf.get(off..off + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

#[cfg(all(test, feature = "mock"))]
mod tests {
    use super::*;
    use crate::backend::MockBackend;
    use crate::node::{IntWidth, Node, NodeKind};

    fn h32() -> NodeKind {
        NodeKind::Hex(IntWidth::W32)
    }

    #[test]
    fn flat_class_one_scatter_correct_values() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("S");
        reg.push_node(c, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap(); // off 0
        reg.push_node(c, Node::new("mp", NodeKind::Float32))
            .unwrap(); // off 4
        reg.push_node(c, Node::new("flag", NodeKind::Bool)).unwrap(); // off 8

        let m = MockBackend::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&123i32.to_le_bytes());
        bytes.extend_from_slice(&2.5f32.to_le_bytes());
        bytes.push(1);
        bytes.resize(16, 0);
        m.put(0x1000, bytes);

        let mut eng = Engine::new();
        let rows = eng.snapshot(
            &m,
            &reg,
            &[Root {
                class_id: c,
                base: 0x1000,
            }],
            &ExpandState::new(),
            None,
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].value, "123");
        assert_eq!(rows[0].address, 0x1000);
        assert_eq!(rows[1].value, "2.5");
        assert_eq!(rows[1].address, 0x1004);
        assert_eq!(rows[2].value, "true");
        // single batched read
        assert_eq!(eng.last_read_levels(), 1);
        assert_eq!(m.scatter_calls(), 1);
        assert_eq!(m.read_calls(), 0);
    }

    #[test]
    fn nested_class_instance_inline() {
        let mut reg = ClassRegistry::new();
        let inner = reg.add_class("Inner");
        reg.push_node(inner, Node::new("x", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        let outer = reg.add_class("Outer");
        reg.push_node(outer, Node::new("a", h32())).unwrap(); // off 0
        reg.push_node(
            outer,
            Node::new("inner", NodeKind::ClassInstance { class_id: inner }),
        )
        .unwrap(); // off 4

        let m = MockBackend::new();
        let mut bytes = vec![0u8; 16];
        bytes[4..8].copy_from_slice(&77i32.to_le_bytes());
        m.put(0x2000, bytes);

        let mut eng = Engine::new();
        let rows = eng.snapshot(
            &m,
            &reg,
            &[Root {
                class_id: outer,
                base: 0x2000,
            }],
            &ExpandState::new(),
            None,
        );
        // a, inner(header), inner.x
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].name, "x");
        assert_eq!(rows[2].value, "77");
        assert_eq!(rows[2].address, 0x2004);
        assert_eq!(eng.last_read_levels(), 1); // inline = same buffer
    }

    #[test]
    fn class_ptr_follow_two_levels() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C"); // leaf: one i32 "val"
        reg.push_node(c, Node::new("val", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        let b = reg.add_class("B"); // has ptr to C
        reg.push_node(b, Node::new("toC", NodeKind::ClassPtr { class_id: c }))
            .unwrap();
        let a = reg.add_class("A"); // has ptr to B
        reg.push_node(a, Node::new("toB", NodeKind::ClassPtr { class_id: b }))
            .unwrap();

        let m = MockBackend::new();
        // A @ 0x1000 -> ptr to B @ 0x2000
        m.put(0x1000, 0x2000u64.to_le_bytes().to_vec());
        // B @ 0x2000 -> ptr to C @ 0x3000
        m.put(0x2000, 0x3000u64.to_le_bytes().to_vec());
        // C @ 0x3000 -> val = 999
        m.put(0x3000, 999i32.to_le_bytes().to_vec());

        let mut expand = ExpandState::new();
        expand.expand(0, vec![PathSeg::Node(0)]); // expand A.toB
        expand.expand(0, vec![PathSeg::Node(0), PathSeg::Node(0)]); // expand B.toC

        let mut eng = Engine::new();
        let rows = eng.snapshot(
            &m,
            &reg,
            &[Root {
                class_id: a,
                base: 0x1000,
            }],
            &expand,
            None,
        );
        // A.toB (expandable), B.toC (expandable), C.val = 999
        let val_row = rows.iter().find(|r| r.name == "val").unwrap();
        assert_eq!(val_row.value, "999");
        assert_eq!(val_row.address, 0x3000);
        // 3 levels: A, B, C
        assert_eq!(eng.last_read_levels(), 3);
        // each level one scatter
        assert_eq!(m.scatter_calls(), 3);
    }

    #[test]
    fn collapsed_class_ptr_is_one_level() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(c, Node::new("val", h32())).unwrap();
        let a = reg.add_class("A");
        reg.push_node(a, Node::new("toC", NodeKind::ClassPtr { class_id: c }))
            .unwrap();
        let m = MockBackend::new();
        m.put(0x1000, 0x2000u64.to_le_bytes().to_vec());
        m.put(0x2000, 7u32.to_le_bytes().to_vec());
        let mut eng = Engine::new();
        let rows = eng.snapshot(
            &m,
            &reg,
            &[Root {
                class_id: a,
                base: 0x1000,
            }],
            &ExpandState::new(),
            None,
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].expandable);
        assert!(!rows[0].expanded);
        assert_eq!(eng.last_read_levels(), 1);
    }

    #[test]
    fn scalar_array_expands_inline() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(
            c,
            Node::new(
                "scores",
                NodeKind::Array {
                    element: Box::new(NodeKind::Int(IntWidth::W32)),
                    count: 3,
                },
            ),
        )
        .unwrap();
        let m = MockBackend::new();
        let mut bytes = Vec::new();
        for v in [10i32, 20, 30] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        m.put(0x1000, bytes);
        let mut eng = Engine::new();
        let rows = eng.snapshot(
            &m,
            &reg,
            &[Root {
                class_id: c,
                base: 0x1000,
            }],
            &ExpandState::new(),
            None,
        );
        // header + 3 elements
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].value, "10");
        assert_eq!(rows[2].value, "20");
        assert_eq!(rows[3].value, "30");
        assert_eq!(rows[3].address, 0x1008);
        assert_eq!(eng.last_read_levels(), 1);
    }

    #[test]
    fn array_collapses_to_header_only() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(
            c,
            Node::new(
                "scores",
                NodeKind::Array {
                    element: Box::new(NodeKind::Int(IntWidth::W32)),
                    count: 3,
                },
            ),
        )
        .unwrap();
        reg.push_node(c, Node::new("tail", h32())).unwrap();
        let m = MockBackend::new();
        m.put(0x1000, vec![0u8; 32]);
        let mut eng = Engine::new();
        let roots = [Root {
            class_id: c,
            base: 0x1000,
        }];

        // expanded by default: header + 3 elements + tail = 5
        let rows = eng.snapshot(&m, &reg, &roots, &ExpandState::new(), None);
        assert_eq!(rows.len(), 5);
        assert!(rows[0].expandable && rows[0].expanded);

        // collapse the array: header + tail = 2
        let mut expand = ExpandState::new();
        expand.toggle_collapse(0, vec![PathSeg::Node(0)]);
        let rows = eng.snapshot(&m, &reg, &roots, &expand, None);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].expandable && !rows[0].expanded);
        assert_eq!(rows[1].name, "tail");
    }

    #[test]
    fn unreadable_pointer_target_degrades_gracefully() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(c, Node::new("val", h32())).unwrap();
        let a = reg.add_class("A");
        reg.push_node(a, Node::new("toC", NodeKind::ClassPtr { class_id: c }))
            .unwrap();
        let m = MockBackend::new();
        // ptr points to unmapped 0xDEAD0000
        m.put(0x1000, 0xDEAD0000u64.to_le_bytes().to_vec());
        let mut expand = ExpandState::new();
        expand.expand(0, vec![PathSeg::Node(0)]);
        let mut eng = Engine::new();
        let rows = eng.snapshot(
            &m,
            &reg,
            &[Root {
                class_id: a,
                base: 0x1000,
            }],
            &expand,
            None,
        );
        let val_row = rows.iter().find(|r| r.name == "val").unwrap();
        assert_eq!(val_row.value, "???");
        assert!(!val_row.readable);
    }

    #[test]
    fn partial_read_renders_mapped_prefix() {
        // Class is 16 bytes (4 x Int32) but only 12 bytes are mapped; the first
        // three fields must render, the fourth shows "???".
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        for i in 0..4 {
            reg.push_node(c, Node::new(format!("f{i}"), NodeKind::Int(IntWidth::W32)))
                .unwrap();
        }
        let m = MockBackend::new();
        let mut bytes = Vec::new();
        for v in [11i32, 22, 33] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        m.put(0x1000, bytes); // only 12 bytes mapped
        let mut eng = Engine::new();
        let rows = eng.snapshot(
            &m,
            &reg,
            &[Root {
                class_id: c,
                base: 0x1000,
            }],
            &ExpandState::new(),
            None,
        );
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].value, "11");
        assert_eq!(rows[2].value, "33");
        assert!(rows[..3].iter().all(|r| r.readable));
        assert_eq!(rows[3].value, "???");
        assert!(!rows[3].readable);
    }
}
