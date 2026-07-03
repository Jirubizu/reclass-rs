//! The egui-independent application core: attach, resolve expressions, drive
//! the render engine, and apply edits. Unit-tested against `MockBackend`.

use reclass_core::backend::Region;
use reclass_core::codegen::{Language, generate};
use reclass_core::project::{Project, View};
use reclass_core::{
    AddrExpr, AddrInfo, ClassId, Engine, ExpandState, IntWidth, MemError, MemoryBackend, Node,
    NodeKind, PathSeg, Root, Row,
};
use std::collections::HashMap;

/// Resolves an address to a `module+offset` / region label for pointer display.
pub struct AddrResolver<'a> {
    regions: &'a [Region],
}

impl AddrInfo for AddrResolver<'_> {
    fn describe(&self, addr: u64) -> Option<String> {
        let r = self.regions.iter().find(|r| r.contains(addr))?;
        match &r.path {
            Some(p) => {
                let base = p.rsplit('/').next().unwrap_or(p);
                Some(format!("{base}+0x{:X}", addr - r.start))
            }
            None => Some(format!("{} 0x{:X}", r.perms, addr)),
        }
    }
}

/// Bounded static walker used by `expand_all` / `collapse_all` to enumerate the
/// aggregate and `ClassPtr` node paths of a class without live reads.
struct Walk<'a> {
    reg: &'a reclass_core::ClassRegistry,
    follow_ptrs: bool,
    visited: std::collections::HashSet<ClassId>,
    aggs: Vec<Vec<PathSeg>>,
    ptrs: Vec<Vec<PathSeg>>,
}

impl Walk<'_> {
    const MAX_DEPTH: usize = 16;
    const ELEM_CAP: usize = 64;

    fn class(&mut self, class: ClassId, base: Vec<PathSeg>, depth: usize) {
        let Some(c) = self.reg.get(class) else { return };
        for (i, node) in c.nodes.iter().enumerate() {
            let mut p = base.clone();
            p.push(PathSeg::Node(i));
            let kind = node.kind.clone();
            self.kind(&kind, p, depth);
        }
    }

    fn kind(&mut self, kind: &NodeKind, path: Vec<PathSeg>, depth: usize) {
        match kind {
            NodeKind::ClassInstance { class_id } => {
                self.aggs.push(path.clone());
                if depth < Self::MAX_DEPTH && self.visited.insert(*class_id) {
                    self.class(*class_id, path, depth + 1);
                    self.visited.remove(class_id);
                }
            }
            NodeKind::ClassPtr { class_id } => {
                self.ptrs.push(path.clone());
                if self.follow_ptrs && depth < Self::MAX_DEPTH && self.visited.insert(*class_id) {
                    self.class(*class_id, path, depth + 1);
                    self.visited.remove(class_id);
                }
            }
            NodeKind::Array { element, count } => {
                self.aggs.push(path.clone());
                if matches!(
                    element.as_ref(),
                    NodeKind::ClassInstance { .. }
                        | NodeKind::ClassPtr { .. }
                        | NodeKind::Array { .. }
                ) {
                    for e in 0..(*count).min(Self::ELEM_CAP) {
                        let mut ep = path.clone();
                        ep.push(PathSeg::Elem(e));
                        self.kind(element, ep, depth);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Per-view resolved-base outcome for this tick.
#[derive(Clone, Debug, Default)]
pub struct ViewStatus {
    /// Resolved base address (0 if unresolved).
    pub base: u64,
    /// Error message if the expression failed to resolve.
    pub error: Option<String>,
}

/// The whole application state.
pub struct AppState {
    /// Classes, views, window settings.
    pub project: Project,
    /// Attached target (None when detached / offline).
    pub backend: Option<Box<dyn MemoryBackend>>,
    /// Expansion state for `ClassPtr` nodes.
    pub expand: ExpandState,
    /// Render engine (holds reusable buffers).
    pub engine: Engine,
    /// Cached regions for the memory-map view + pointer annotation.
    pub regions: Vec<Region>,
    /// Index of the currently selected view.
    pub selected_view: usize,
    /// Per-view resolve status (parallel to `project.views`).
    pub view_status: Vec<ViewStatus>,
    /// Human-readable status line.
    pub status: String,
    /// Parsed address expressions, keyed by class id. A stored entry is reused
    /// only while its source string still matches the class's `address_expr`,
    /// so editing the expression transparently re-parses.
    expr_cache: HashMap<ClassId, (String, Result<AddrExpr, String>)>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// An empty, detached state.
    pub fn new() -> Self {
        AppState {
            project: Project::default(),
            backend: None,
            expand: ExpandState::new(),
            engine: Engine::new(),
            regions: Vec::new(),
            selected_view: 0,
            view_status: Vec::new(),
            status: "detached".to_string(),
            expr_cache: HashMap::new(),
        }
    }

    /// Replace the backend (e.g. after attaching) and refresh regions.
    pub fn set_backend(&mut self, backend: Box<dyn MemoryBackend>) {
        self.backend = Some(backend);
        self.refresh_regions();
    }

    /// Whether a backend is attached.
    pub fn attached(&self) -> bool {
        self.backend.is_some()
    }

    /// Re-read the target's mapped regions.
    pub fn refresh_regions(&mut self) {
        self.regions = match &self.backend {
            Some(b) => b.regions().unwrap_or_default(),
            None => Vec::new(),
        };
    }

    // -- classes / views ---------------------------------------------------

    /// Create a class and open it in a new view; returns its id.
    pub fn add_class(&mut self, name: impl Into<String>) -> ClassId {
        let id = self.project.registry.add_class(name);
        self.open_view(id);
        id
    }

    /// Open `class_id` in a view (selecting it). No-op if already open.
    pub fn open_view(&mut self, class_id: ClassId) {
        if let Some(i) = self
            .project
            .views
            .iter()
            .position(|v| v.class_id == class_id)
        {
            self.selected_view = i;
        } else {
            self.project.views.push(View { class_id });
            self.selected_view = self.project.views.len() - 1;
        }
        self.view_status
            .resize(self.project.views.len(), ViewStatus::default());
    }

    /// Close a view by index.
    pub fn close_view(&mut self, idx: usize) {
        if idx < self.project.views.len() {
            self.project.views.remove(idx);
            self.view_status.truncate(self.project.views.len());
            if self.selected_view >= self.project.views.len() {
                self.selected_view = self.project.views.len().saturating_sub(1);
            }
            self.expand.drop_root(idx);
        }
    }

    /// Remove a class and close any views showing it. References to it from
    /// other classes become dangling (rendered as `class#id`); that's allowed.
    pub fn remove_class(&mut self, id: ClassId) {
        self.project.registry.remove_class(id);
        self.expr_cache.remove(&id);
        if let Some(idx) = self.project.views.iter().position(|v| v.class_id == id) {
            self.project.views.remove(idx);
            self.view_status.truncate(self.project.views.len());
            if self.selected_view >= self.project.views.len() {
                self.selected_view = self.project.views.len().saturating_sub(1);
            }
            // expansion is keyed by view position; shift higher views down.
            self.expand.drop_root(idx);
        }
    }

    /// The class id of the selected view, if any.
    pub fn selected_class(&self) -> Option<ClassId> {
        self.project
            .views
            .get(self.selected_view)
            .map(|v| v.class_id)
    }

    // -- live read ---------------------------------------------------------

    /// Resolve every view's base address and produce the full row set
    /// (`Row::root` == view index). Updates `view_status`.
    pub fn compute_rows(&mut self) -> Vec<Row> {
        let n = self.project.views.len();
        self.view_status.resize(n, ViewStatus::default());

        // Snapshot (class_id, expr) first so resolution can take `&mut self` to
        // populate the parsed-expression cache without aliasing the views.
        let views: Vec<(ClassId, String)> = self
            .project
            .views
            .iter()
            .map(|v| {
                let expr = self
                    .project
                    .registry
                    .get(v.class_id)
                    .map(|c| c.address_expr.clone())
                    .unwrap_or_default();
                (v.class_id, expr)
            })
            .collect();

        let mut roots = Vec::with_capacity(n);
        for (i, (class_id, expr)) in views.iter().enumerate() {
            let (base, error) = self.resolve_cached(*class_id, expr);
            self.view_status[i] = ViewStatus { base, error };
            roots.push(Root {
                class_id: *class_id,
                base,
            });
        }

        let Some(backend) = &self.backend else {
            return Vec::new();
        };
        let resolver = AddrResolver {
            regions: &self.regions,
        };
        self.engine.snapshot(
            backend.as_ref(),
            &self.project.registry,
            &roots,
            &self.expand,
            Some(&resolver),
        )
    }

    /// Resolve a class's address expression against the backend, caching the
    /// parsed AST per class so only `eval` (which may deref live memory) runs
    /// each tick; the parse happens once per expression edit.
    fn resolve_cached(&mut self, class_id: ClassId, expr: &str) -> (u64, Option<String>) {
        if expr.trim().is_empty() {
            self.expr_cache.remove(&class_id);
            return (0, None);
        }
        let fresh =
            matches!(self.expr_cache.get(&class_id), Some((src, _)) if src.as_str() == expr);
        if !fresh {
            let ast = AddrExpr::parse(expr).map_err(|e| e.to_string());
            self.expr_cache.insert(class_id, (expr.to_string(), ast));
        }
        let parsed = match self.expr_cache.get(&class_id) {
            Some((_, ast)) => ast.clone(),
            None => return (0, None),
        };
        let Some(backend) = &self.backend else {
            return (0, Some("not attached".to_string()));
        };
        match parsed {
            Ok(ast) => match ast.eval(backend.as_ref()) {
                Ok(a) => (a, None),
                Err(e) => (0, Some(e.to_string())),
            },
            Err(e) => (0, Some(e)),
        }
    }

    /// Whether `addr` lies in a mapped, readable region.
    pub fn addr_is_readable(&self, addr: u64) -> bool {
        self.regions
            .iter()
            .any(|r| r.contains(addr) && r.perms.read)
    }

    // -- editing -----------------------------------------------------------

    /// Toggle expansion of an expandable (`ClassPtr`) row.
    pub fn toggle_expand(&mut self, root: usize, path: Vec<PathSeg>) {
        self.expand.toggle(root, path);
    }

    /// Toggle collapse of an aggregate (`Array`/`ClassInstance`) row.
    pub fn toggle_collapse(&mut self, root: usize, path: Vec<PathSeg>) {
        self.expand.toggle_collapse(root, path);
    }

    /// Expand every aggregate and follow every `ClassPtr` in the selected view
    /// (bounded by depth and a per-branch class-visited guard to avoid cycles).
    pub fn expand_all(&mut self) {
        let Some(class) = self.selected_class() else {
            return;
        };
        let root = self.selected_view;
        let mut aggs = Vec::new();
        let mut ptrs = Vec::new();
        self.collect_expandables(class, true, &mut aggs, &mut ptrs);
        self.expand.clear_root(root); // un-collapse all aggregates
        for p in ptrs {
            self.expand.expand(root, p);
        }
    }

    /// Collapse every aggregate and un-follow every `ClassPtr` in the view.
    pub fn collapse_all(&mut self) {
        let Some(class) = self.selected_class() else {
            return;
        };
        let root = self.selected_view;
        let mut aggs = Vec::new();
        let mut ptrs = Vec::new();
        self.collect_expandables(class, false, &mut aggs, &mut ptrs);
        self.expand.clear_root(root); // drop expanded pointers
        for p in aggs {
            self.expand.mark_collapsed(root, p);
        }
    }

    /// Statically walk a class collecting aggregate paths and `ClassPtr` paths.
    /// `follow_ptrs` descends into pointer targets too (for "expand all").
    fn collect_expandables(
        &self,
        class: ClassId,
        follow_ptrs: bool,
        aggs: &mut Vec<Vec<PathSeg>>,
        ptrs: &mut Vec<Vec<PathSeg>>,
    ) {
        let mut w = Walk {
            reg: &self.project.registry,
            follow_ptrs,
            visited: std::collections::HashSet::from([class]),
            aggs: std::mem::take(aggs),
            ptrs: std::mem::take(ptrs),
        };
        w.class(class, Vec::new(), 0);
        *aggs = w.aggs;
        *ptrs = w.ptrs;
    }

    /// Append an array of `count` × `element` to a class.
    pub fn add_array(
        &mut self,
        class: ClassId,
        element: NodeKind,
        count: usize,
    ) -> Result<(), String> {
        if self.project.registry.kind_would_cycle(class, &element) {
            return Err("would create an inline class cycle".to_string());
        }
        let off = self.project.registry.size_of(class);
        self.push_node(
            class,
            Node::new(
                format!("arr_{off:X}"),
                NodeKind::Array {
                    element: Box::new(element),
                    count,
                },
            ),
        )
    }

    /// Expand a plain `Pointer` node by creating a backing class (16 Hex64
    /// fields) and converting the node to a `ClassPtr` over it, then marking it
    /// expanded. Mirrors ReClass auto-creating a class for a pointer target.
    pub fn expand_pointer(
        &mut self,
        owner: ClassId,
        idx: usize,
        root: usize,
        path: Vec<PathSeg>,
    ) -> Result<(), String> {
        let reg = &mut self.project.registry;
        let name = format!("Auto{}", reg.len());
        let target = reg.add_class(name);
        reg.push_nodes(
            target,
            (0..16).map(|i| Node::new(format!("field_{:X}", i * 8), NodeKind::Hex(IntWidth::W64))),
        )
        .map_err(|e| e.to_string())?;
        reg.set_kind(owner, idx, NodeKind::ClassPtr { class_id: target })
            .map_err(|e| e.to_string())?;
        self.expand.expand(root, path);
        Ok(())
    }

    /// Write a new value to a scalar node (parsed by its kind).
    pub fn write_value(&mut self, addr: u64, kind: &NodeKind, input: &str) -> Result<(), String> {
        let bytes = kind.parse_edit(input).map_err(|e| e.to_string())?;
        let backend = self.backend.as_ref().ok_or("not attached")?;
        backend
            .write(addr, &bytes)
            .map_err(|e: MemError| e.to_string())
    }

    /// Resolve a row path to the `(owning class, node index)` it identifies.
    pub fn resolve_owner(&self, root_class: ClassId, path: &[PathSeg]) -> Option<(ClassId, usize)> {
        let reg = &self.project.registry;
        let mut class = root_class;
        let mut owner = (root_class, 0usize);
        let mut cur_kind: Option<NodeKind> = None;
        for seg in path {
            match seg {
                PathSeg::Node(i) => {
                    let node = reg.get(class)?.nodes.get(*i)?;
                    owner = (class, *i);
                    cur_kind = Some(node.kind.clone());
                    if let NodeKind::ClassInstance { class_id } | NodeKind::ClassPtr { class_id } =
                        &node.kind
                    {
                        class = *class_id;
                    }
                }
                PathSeg::Elem(_) => {
                    let k = cur_kind.take()?;
                    if let NodeKind::Array { element, .. } = k {
                        if let NodeKind::ClassInstance { class_id }
                        | NodeKind::ClassPtr { class_id } = element.as_ref()
                        {
                            class = *class_id;
                        }
                        cur_kind = Some(*element);
                    } else {
                        return None;
                    }
                }
            }
        }
        Some(owner)
    }

    /// Append a node to a class.
    pub fn push_node(&mut self, class: ClassId, node: Node) -> Result<(), String> {
        self.project
            .registry
            .push_node(class, node)
            .map_err(|e| e.to_string())
    }

    /// Append `n` bytes worth of fields to a class: as many `Hex64` rows as fit,
    /// then `Hex8` rows for any remainder. Lets the user grow a class in bulk
    /// (e.g. 1024 bytes) instead of one field at a time.
    pub fn add_bytes(&mut self, class: ClassId, n: usize) -> Result<(), String> {
        let mut off = self.project.registry.size_of(class);
        let mut nodes = Vec::with_capacity(n.div_ceil(8));
        let mut remaining = n;
        while remaining >= 8 {
            nodes.push(Node::new(
                format!("field_{off:X}"),
                NodeKind::Hex(IntWidth::W64),
            ));
            off += 8;
            remaining -= 8;
        }
        while remaining > 0 {
            nodes.push(Node::new(
                format!("field_{off:X}"),
                NodeKind::Hex(IntWidth::W8),
            ));
            off += 1;
            remaining -= 1;
        }
        self.project
            .registry
            .push_nodes(class, nodes)
            .map_err(|e| e.to_string())
    }

    /// Insert a node after `idx` in `class`.
    pub fn insert_after(&mut self, class: ClassId, idx: usize, node: Node) -> Result<(), String> {
        self.project
            .registry
            .insert_node(class, idx + 1, node)
            .map_err(|e| e.to_string())
    }

    /// Delete node `idx` from `class`.
    pub fn delete_node(&mut self, class: ClassId, idx: usize) -> Result<(), String> {
        self.project
            .registry
            .remove_node(class, idx)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Delete several nodes at once. Sorts by class then by descending index so
    /// removing earlier entries doesn't shift the indices of later ones.
    pub fn delete_many(&mut self, targets: &[(ClassId, usize)]) {
        let mut t = targets.to_vec();
        t.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        t.dedup();
        for (cls, idx) in t {
            let _ = self.delete_node(cls, idx);
        }
    }

    /// Change a node's kind, guarding against inline cycles.
    pub fn change_kind(
        &mut self,
        class: ClassId,
        idx: usize,
        kind: NodeKind,
    ) -> Result<(), String> {
        if self.project.registry.kind_would_cycle(class, &kind) {
            return Err("would create an inline class cycle".to_string());
        }
        self.project
            .registry
            .set_kind(class, idx, kind)
            .map_err(|e| e.to_string())
    }

    /// Set the element count of an array node.
    pub fn set_array_count(
        &mut self,
        class: ClassId,
        idx: usize,
        count: usize,
    ) -> Result<(), String> {
        self.project
            .registry
            .set_array_count(class, idx, count)
            .map_err(|e| e.to_string())
    }

    /// Rename a node.
    pub fn rename_node(&mut self, class: ClassId, idx: usize, name: String) -> Result<(), String> {
        self.project
            .registry
            .rename_node(class, idx, name)
            .map_err(|e| e.to_string())
    }

    /// Set a node's comment.
    pub fn set_comment(
        &mut self,
        class: ClassId,
        idx: usize,
        comment: String,
    ) -> Result<(), String> {
        self.project
            .registry
            .set_comment(class, idx, comment)
            .map_err(|e| e.to_string())
    }

    /// Rename a class.
    pub fn rename_class(&mut self, id: ClassId, name: String) -> Result<(), String> {
        self.project
            .registry
            .rename_class(id, name)
            .map_err(|e| e.to_string())
    }

    /// Set the address expression of a class.
    pub fn set_address_expr(&mut self, id: ClassId, expr: String) -> Result<(), String> {
        self.project
            .registry
            .set_address_expr(id, expr)
            .map_err(|e| e.to_string())
    }

    // -- project / codegen -------------------------------------------------

    /// Generated source for the whole registry.
    pub fn codegen(&self, lang: Language) -> String {
        generate(&self.project.registry, lang)
    }

    /// Save the project to a RON file.
    pub fn save(&self, path: &str) -> Result<(), String> {
        self.project.save(path).map_err(|e| e.to_string())
    }

    /// Load a project from a RON file (replaces state).
    pub fn load(&mut self, path: &str) -> Result<(), String> {
        let project = Project::load(path).map_err(|e| e.to_string())?;
        self.project = project;
        self.expand = ExpandState::new();
        self.selected_view = 0;
        self.view_status = vec![ViewStatus::default(); self.project.views.len()];
        self.expr_cache.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reclass_core::backend::{MockBackend, Perms, Region};
    use reclass_core::node::IntWidth;

    fn attached_state() -> AppState {
        let mut st = AppState::new();
        let m = MockBackend::new();
        m.put_module("game", 0x4000);
        // Player @ resolved base 0x5000
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(&100i32.to_le_bytes());
        bytes[4..8].copy_from_slice(&1.5f32.to_le_bytes());
        m.put(0x5000, bytes);
        m.put_region(Region {
            start: 0x5000,
            end: 0x6000,
            perms: Perms {
                read: true,
                write: true,
                execute: false,
                shared: false,
            },
            path: Some("/game".to_string()),
        });
        st.set_backend(Box::new(m));
        st
    }

    #[test]
    fn compute_rows_resolves_expr_and_reads() {
        let mut st = attached_state();
        let player = st.add_class("Player");
        st.push_node(player, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        st.push_node(player, Node::new("speed", NodeKind::Float32))
            .unwrap();
        st.set_address_expr(player, "<game> + 0x1000".to_string())
            .unwrap();

        let rows = st.compute_rows();
        assert_eq!(st.view_status[0].base, 0x5000);
        assert!(st.view_status[0].error.is_none());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].value, "100");
        assert_eq!(rows[0].address, 0x5000);
        assert_eq!(rows[1].value, "1.5");
    }

    #[test]
    fn seeded_hex_class_renders_rows() {
        // Regression: a class with default Hex64 fields over a readable address
        // must produce one row per field (an empty class produced none, which
        // looked like "nothing is coming through").
        let mut st = attached_state();
        let c = st.add_class("Class1");
        for i in 0..16 {
            st.push_node(c, Node::new(format!("f{i}"), NodeKind::Hex(IntWidth::W64)))
                .unwrap();
        }
        st.set_address_expr(c, "0x5000".to_string()).unwrap();
        let rows = st.compute_rows();
        assert_eq!(rows.len(), 16);
        // the 32-byte block covers the first 4 Hex64 fields; the rest overrun
        // the mapping and render "???" rather than blanking the whole table.
        assert!(rows[0].readable && rows[0].value.starts_with("0x"));
        assert_eq!(rows[0].offset, 0);
        assert_eq!(rows[1].address, 0x5008);
        assert!(rows[..4].iter().all(|r| r.readable));
        assert!(rows[4..].iter().all(|r| !r.readable && r.value == "???"));
    }

    #[test]
    fn expand_pointer_creates_and_follows_target() {
        let mut st = AppState::new();
        let m = MockBackend::new();
        // C @ 0x5000 has a pointer -> 0x7000; target holds 128 bytes.
        m.put(0x5000, 0x7000u64.to_le_bytes().to_vec());
        m.put(0x7000, (0u8..128).collect::<Vec<_>>());
        for (s, e) in [(0x5000u64, 0x5100u64), (0x7000, 0x7100)] {
            m.put_region(Region {
                start: s,
                end: e,
                perms: Perms {
                    read: true,
                    write: true,
                    execute: false,
                    shared: false,
                },
                path: None,
            });
        }
        st.set_backend(Box::new(m));
        let c = st.add_class("C");
        st.push_node(c, Node::new("ptr", NodeKind::Pointer))
            .unwrap();
        st.set_address_expr(c, "0x5000".to_string()).unwrap();

        // before expansion: a single expandable pointer row, still a Pointer
        let rows = st.compute_rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].expandable && !rows[0].expanded);
        assert!(matches!(rows[0].kind, NodeKind::Pointer));
        // expand: converts to ClassPtr over a fresh class and follows it
        st.expand_pointer(c, 0, 0, vec![PathSeg::Node(0)]).unwrap();
        let rows = st.compute_rows();
        assert!(matches!(rows[0].kind, NodeKind::ClassPtr { .. }));
        assert!(rows[0].expanded);
        assert!(rows.len() > 1, "pointer did not expand into target fields");
        // first nested field reads the target's first 8 bytes
        assert_eq!(rows[1].address, 0x7000);
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn delete_many_removes_descending_without_shift_bugs() {
        let mut st = AppState::new();
        let c = st.add_class("C");
        for i in 0..6 {
            st.push_node(c, Node::new(format!("f{i}"), NodeKind::Hex(IntWidth::W8)))
                .unwrap();
        }
        // delete indices 1, 3, 4 (order/dupes shouldn't matter)
        st.delete_many(&[(c, 3), (c, 1), (c, 4), (c, 3)]);
        let names: Vec<String> = st
            .project
            .registry
            .get(c)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.name.clone())
            .collect();
        assert_eq!(names, vec!["f0", "f2", "f5"]);
    }

    #[test]
    fn add_bytes_grows_class_in_bulk() {
        let mut st = AppState::new();
        let c = st.add_class("C");
        st.add_bytes(c, 20).unwrap(); // 2 x Hex64 (16) + 4 x Hex8 (4)
        assert_eq!(st.project.registry.size_of(c), 20);
        assert_eq!(st.project.registry.get(c).unwrap().nodes.len(), 6);
        st.add_bytes(c, 1024).unwrap();
        assert_eq!(st.project.registry.size_of(c), 20 + 1024);
    }

    #[test]
    fn add_array_appends_one_collapsible_node() {
        let mut st = AppState::new();
        let c = st.add_class("C");
        st.add_array(c, NodeKind::Hex(IntWidth::W8), 72).unwrap();
        let class = st.project.registry.get(c).unwrap();
        assert_eq!(class.nodes.len(), 1);
        assert!(matches!(
            class.nodes[0].kind,
            NodeKind::Array { count: 72, .. }
        ));
        assert_eq!(st.project.registry.size_of(c), 72);
    }

    #[test]
    fn expand_all_and_collapse_all_toggle_aggregates() {
        let mut st = attached_state();
        let c = st.add_class("C");
        st.push_node(
            c,
            Node::new(
                "arr",
                NodeKind::Array {
                    element: Box::new(NodeKind::Int(IntWidth::W32)),
                    count: 3,
                },
            ),
        )
        .unwrap();
        st.set_address_expr(c, "0x5000".to_string()).unwrap();

        // default expanded: header + 3 elements
        assert_eq!(st.compute_rows().len(), 4);

        st.collapse_all();
        let rows = st.compute_rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].expandable && !rows[0].expanded);

        st.expand_all();
        let rows = st.compute_rows();
        assert_eq!(rows.len(), 4);
        assert!(rows[0].expanded);
    }

    #[test]
    fn change_kind_converts_field_to_array() {
        let mut st = AppState::new();
        let c = st.add_class("C");
        st.push_node(c, Node::new("blob", NodeKind::Hex(IntWidth::W64)))
            .unwrap();
        st.change_kind(
            c,
            0,
            NodeKind::Array {
                element: Box::new(NodeKind::Hex(IntWidth::W8)),
                count: 13,
            },
        )
        .unwrap();
        let class = st.project.registry.get(c).unwrap();
        assert!(matches!(
            class.nodes[0].kind,
            NodeKind::Array { count: 13, .. }
        ));
        assert_eq!(st.project.registry.size_of(c), 13);
    }

    #[test]
    fn remove_class_drops_class_and_its_views() {
        let mut st = AppState::new();
        let a = st.add_class("A");
        let b = st.add_class("B");
        assert_eq!(st.project.registry.len(), 2);
        assert_eq!(st.project.views.len(), 2);
        st.remove_class(a);
        assert_eq!(st.project.registry.len(), 1);
        assert!(st.project.registry.name_of(a).is_none());
        assert_eq!(st.project.registry.name_of(b), Some("B"));
        // the view showing A is gone; B's view remains and selection stays valid
        assert_eq!(st.project.views.len(), 1);
        assert_eq!(st.selected_class(), Some(b));
    }

    #[test]
    fn write_value_roundtrips() {
        let mut st = attached_state();
        let player = st.add_class("Player");
        st.push_node(player, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        st.set_address_expr(player, "0x5000".to_string()).unwrap();

        st.write_value(0x5000, &NodeKind::Int(IntWidth::W32), "777")
            .unwrap();
        let rows = st.compute_rows();
        assert_eq!(rows[0].value, "777");
    }

    #[test]
    fn bad_expr_sets_error_no_panic() {
        let mut st = attached_state();
        let c = st.add_class("C");
        st.set_address_expr(c, "<missing> + 1".to_string()).unwrap();
        let _ = st.compute_rows();
        assert!(st.view_status[0].error.is_some());
    }

    #[test]
    fn resolve_owner_through_nested_and_array() {
        let mut st = AppState::new();
        let inner = st.project.registry.add_class("Inner");
        st.push_node(inner, Node::new("x", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        let outer = st.add_class("Outer");
        st.push_node(outer, Node::new("a", NodeKind::Hex(IntWidth::W32)))
            .unwrap();
        st.push_node(
            outer,
            Node::new("inner", NodeKind::ClassInstance { class_id: inner }),
        )
        .unwrap();

        // path to Outer.inner.x  =>  [Node(1), Node(0)] resolves to (inner, 0)
        let owner = st.resolve_owner(outer, &[PathSeg::Node(1), PathSeg::Node(0)]);
        assert_eq!(owner, Some((inner, 0)));
        // path to Outer.a => (outer, 0)
        assert_eq!(
            st.resolve_owner(outer, &[PathSeg::Node(0)]),
            Some((outer, 0))
        );
    }

    #[test]
    fn change_kind_rejects_cycle() {
        let mut st = AppState::new();
        let a = st.add_class("A");
        st.push_node(a, Node::new("self", NodeKind::Hex(IntWidth::W32)))
            .unwrap();
        let err = st.change_kind(a, 0, NodeKind::ClassInstance { class_id: a });
        assert!(err.is_err());
    }

    #[test]
    fn addr_readability_check() {
        let st = attached_state();
        assert!(st.addr_is_readable(0x5500));
        assert!(!st.addr_is_readable(0x9999));
    }

    #[test]
    fn close_view_keeps_selection_valid() {
        let mut st = AppState::new();
        let a = st.add_class("A");
        let b = st.add_class("B");
        assert_eq!(st.project.views.len(), 2);
        st.close_view(0);
        assert_eq!(st.project.views.len(), 1);
        assert_eq!(st.selected_class(), Some(b));
        let _ = a;
    }

    #[test]
    fn add_array_rejects_inline_cycle() {
        let mut st = AppState::new();
        let a = st.add_class("A");
        // array of inline-A inside A would recurse forever — must be refused
        assert!(
            st.add_array(a, NodeKind::ClassInstance { class_id: a }, 4)
                .is_err()
        );
        // a ClassPtr element is a read boundary and is allowed
        assert!(
            st.add_array(a, NodeKind::ClassPtr { class_id: a }, 4)
                .is_ok()
        );
    }

    #[test]
    fn expr_cache_reparses_after_edit() {
        let mut st = AppState::new();
        let m = MockBackend::new();
        m.put_module("game", 0x4000);
        st.set_backend(Box::new(m));
        let c = st.add_class("C");
        st.push_node(c, Node::new("f", NodeKind::Hex(IntWidth::W64)))
            .unwrap();
        let _ = st.set_address_expr(c, "<game> + 0x10".to_string());
        let _ = st.compute_rows();
        assert_eq!(st.view_status[0].base, 0x4010);
        // editing the expression must discard the cached AST and re-parse
        let _ = st.set_address_expr(c, "<game> + 0x20".to_string());
        let _ = st.compute_rows();
        assert_eq!(st.view_status[0].base, 0x4020);
    }
}
