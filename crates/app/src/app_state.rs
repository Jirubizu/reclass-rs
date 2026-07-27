//! The egui-independent application core: attach, resolve expressions, drive
//! the render engine, and apply edits. Unit-tested against `MockBackend`.

use reclass_core::backend::Region;
use reclass_core::codegen::{Language, generate, generate_project};
use reclass_core::project::{Project, ProjectError, View};
use reclass_core::{
    AddrExpr, AddrInfo, ClassId, ClassRegistry, EditErr, Engine, ExpandState, IntWidth, MemError,
    MemoryBackend, Node, NodeKind, PathSeg, RegistryError, Root, Row,
};
use std::collections::HashMap;

/// Error from an [`AppState`] edit, write, or project IO operation.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A registry / layout edit failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Parsing a value for its node kind failed.
    #[error(transparent)]
    Edit(#[from] EditErr),
    /// A memory read/write failed.
    #[error(transparent)]
    Mem(#[from] MemError),
    /// Saving or loading a project failed.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// The edit would create an inline `ClassInstance` cycle.
    #[error("would create an inline class cycle")]
    Cycle,
    /// No target process is attached.
    #[error("not attached")]
    NotAttached,
    /// A filesystem error while writing a generated project, with the path.
    #[error("{path}: {source}")]
    Io {
        /// The path being written.
        path: String,
        /// The underlying IO error.
        source: std::io::Error,
    },
}

/// Bridge to the MCP/JSON-RPC layer and status lines, whose error type is a
/// plain display string.
impl From<AppError> for String {
    fn from(e: AppError) -> Self {
        e.to_string()
    }
}

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
    /// Element cap for arrays-of-class, matching the engine's render cap so
    /// expand/collapse-all covers exactly the elements that get rendered.
    elem_cap: usize,
}

impl Walk<'_> {
    const MAX_DEPTH: usize = 16;

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
                    for e in 0..(*count).min(self.elem_cap) {
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

/// Bounded undo/redo stack of whole-[`Project`] snapshots.
///
/// Snapshotting the project rather than journalling inverse operations is the
/// only cheap way to be correct here: `remove_class` rewrites references across
/// every other class, and `change_kind` on an array destroys its element count.
///
/// Bounded on two axes, because one is not enough (`benches/history.rs`):
/// cloning a 256-class × 64-field project costs ~1.6 ms — fine once per edit,
/// next to the ~0.2 ms of size/offset recompute the same edit already forces —
/// but it is also ~1.7 MB, so a depth-only cap would pin ~100 MB of history for
/// the session. [`DEPTH`](Self::DEPTH) bounds small projects, where the clone is
/// microseconds and depth is what a user wants; [`MAX_NODES`](Self::MAX_NODES)
/// bounds large ones, trading undo depth for memory exactly where a snapshot
/// gets expensive.
#[derive(Default)]
struct History {
    /// `(snapshot, node count)` — the count is cached so trimming does not
    /// re-walk every retained project.
    undo: std::collections::VecDeque<(Project, usize)>,
    redo: Vec<(Project, usize)>,
    /// Nodes across every snapshot in `undo`.
    nodes: usize,
}

impl History {
    /// Snapshots kept, whatever their size.
    const DEPTH: usize = 64;

    /// Node budget across the whole stack; the binding cap on large projects.
    /// Roughly 50 MB at ~100 bytes per node (two `String`s plus a `NodeKind`).
    const MAX_NODES: usize = 500_000;

    /// Nodes across every class of `p`.
    fn count_nodes(p: &Project) -> usize {
        p.registry.iter().map(|c| c.nodes.len()).sum()
    }

    /// Record `before` as an undo point, invalidating any redo branch.
    fn push(&mut self, before: Project) {
        let n = Self::count_nodes(&before);
        self.undo.push_back((before, n));
        self.nodes += n;
        // Always keep the newest snapshot, even if it alone blows the budget:
        // dropping it would make the edit that just happened unundoable.
        while self.undo.len() > 1 && (self.undo.len() > Self::DEPTH || self.nodes > Self::MAX_NODES)
        {
            if let Some((_, dropped)) = self.undo.pop_front() {
                self.nodes -= dropped;
            }
        }
        self.redo.clear();
    }

    /// Forget everything (a project load starts a new timeline).
    fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.nodes = 0;
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
    /// Undo/redo snapshots.
    history: History,
    /// Nodes copied from a class, waiting to be pasted. Owned by the model, not
    /// the front-end, so MCP and plugin callers share one clipboard with the UI.
    clipboard: Vec<Node>,
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
            history: History::default(),
            clipboard: Vec::new(),
        }
    }

    // -- undo / redo -------------------------------------------------------

    /// Record the current project as an undo point.
    ///
    /// Every public mutator calls this first. Mutators that compose (e.g.
    /// `delete_many`) go straight to the registry instead of through their
    /// single-item sibling, so one user action is exactly one undo step.
    fn snapshot(&mut self) {
        self.history.push(self.project.clone());
    }

    /// Whether there is an edit to undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    /// Whether there is an undone edit to redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// Depth of the undo stack (for tests and status display).
    #[must_use]
    pub fn undo_depth(&self) -> usize {
        self.history.undo.len()
    }

    /// Step back one edit. Returns whether anything changed.
    pub fn undo(&mut self) -> bool {
        let Some((prev, nodes)) = self.history.undo.pop_back() else {
            return false;
        };
        self.history.nodes -= nodes;
        let current = std::mem::replace(&mut self.project, prev);
        let n = History::count_nodes(&current);
        self.history.redo.push((current, n));
        self.resync_after_restore();
        true
    }

    /// Step forward one undone edit. Returns whether anything changed.
    ///
    /// Re-entering the undo stack past its caps is allowed: the snapshot came
    /// from that stack a moment ago, and dropping it here would strand the user
    /// mid-timeline with no way back.
    pub fn redo(&mut self) -> bool {
        let Some((next, _)) = self.history.redo.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.project, next);
        let n = History::count_nodes(&current);
        self.history.undo.push_back((current, n));
        self.history.nodes += n;
        self.resync_after_restore();
        true
    }

    /// Bring the derived, non-persisted state back in line with a restored
    /// project: the view cursor and status vector are indexed by view position,
    /// and a cached expression may belong to a class that no longer exists.
    ///
    /// Expansion state is keyed by view position and node path, both of which a
    /// restore can invalidate, so it is dropped rather than half-applied.
    fn resync_after_restore(&mut self) {
        self.view_status = vec![ViewStatus::default(); self.project.views.len()];
        self.selected_view = self
            .selected_view
            .min(self.project.views.len().saturating_sub(1));
        self.expand = ExpandState::new();
        self.expr_cache.clear();
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

    /// The attached memory backend, if any. `&dyn` (not a generic) because the
    /// backend is type-erased at runtime (`VmemBackend`/`MockBackend`); there is
    /// no concrete type to monomorphize over. For read-only plugin access.
    pub fn backend(&self) -> Option<&dyn MemoryBackend> {
        self.backend.as_deref()
    }

    /// The target's mapped regions (as of the last [`refresh_regions`](Self::refresh_regions)).
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// The class registry (read-only), for plugins inspecting structure.
    pub fn registry(&self) -> &ClassRegistry {
        &self.project.registry
    }

    /// The class id shown by view/root `root`, if it exists.
    pub fn view_class(&self, root: usize) -> Option<ClassId> {
        self.project.views.get(root).map(|v| v.class_id)
    }

    // -- classes / views ---------------------------------------------------

    /// Create a class and open it in a new view; returns its id.
    pub fn add_class(&mut self, name: impl Into<String>) -> ClassId {
        self.snapshot();
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
        self.snapshot();
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
            elem_cap: self.engine.array_limit(),
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
    ) -> Result<(), AppError> {
        if self.project.registry.kind_would_cycle(class, &element) {
            return Err(AppError::Cycle);
        }
        self.snapshot();
        let off = self.project.registry.size_of(class);
        // straight to the registry: `push_node` would take a second snapshot
        // and split one user action across two undo steps
        self.project
            .registry
            .push_node(
                class,
                Node::new(
                    format!("arr_{off:X}"),
                    NodeKind::Array {
                        element: Box::new(element),
                        count,
                    },
                ),
            )
            .map_err(AppError::from)
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
    ) -> Result<(), AppError> {
        self.snapshot();
        let reg = &mut self.project.registry;
        let name = format!("Auto{}", reg.len());
        let target = reg.add_class(name);
        // One field per pointer-width word, so the auto class's rows line up
        // with the target's natural slot size on both 32- and 64-bit targets.
        let (word, step) = if reg.pointer_bytes() == 4 {
            (IntWidth::W32, 4)
        } else {
            (IntWidth::W64, 8)
        };
        reg.push_nodes(
            target,
            (0..16).map(|i| Node::new(format!("field_{:X}", i * step), NodeKind::Hex(word))),
        )
        .map_err(AppError::from)?;
        reg.set_kind(owner, idx, NodeKind::ClassPtr { class_id: target })
            .map_err(AppError::from)?;
        self.expand.expand(root, path);
        Ok(())
    }

    /// Write a new value to a scalar node (parsed by its kind).
    pub fn write_value(&mut self, addr: u64, kind: &NodeKind, input: &str) -> Result<(), AppError> {
        let bytes = kind.parse_edit(input, self.project.registry.pointer_bytes())?;
        let backend = self.backend.as_ref().ok_or(AppError::NotAttached)?;
        backend.write(addr, &bytes).map_err(AppError::from)
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

    // -- clipboard ---------------------------------------------------------

    /// Copy the nodes at `targets` into the clipboard, in class-then-index
    /// order so a multi-row selection pastes back in its original layout order
    /// regardless of how the user clicked it. Returns how many were copied.
    ///
    /// A stale index is skipped rather than failing the whole copy: a selection
    /// made before an MCP call shrank the class should still copy what is left.
    pub fn copy_nodes(&mut self, targets: &[(ClassId, usize)]) -> usize {
        let mut t = targets.to_vec();
        t.sort_unstable();
        t.dedup();
        self.clipboard = t
            .iter()
            .filter_map(|&(cls, idx)| self.project.registry.get(cls)?.nodes.get(idx).cloned())
            .collect();
        self.clipboard.len()
    }

    /// Nodes currently on the clipboard.
    #[must_use]
    pub fn clipboard(&self) -> &[Node] {
        &self.clipboard
    }

    /// Insert the clipboard into `class`, after node `after` (or appended when
    /// `None`). Returns how many nodes were inserted.
    ///
    /// Rejected wholesale — before any node lands — when a pasted node would
    /// create an inline cycle, or references a class that has since been
    /// deleted. A partial paste would leave the class in a shape the user never
    /// asked for, and undoing it is a worse recovery than never starting.
    pub fn paste_nodes(&mut self, class: ClassId, after: Option<usize>) -> Result<usize, AppError> {
        if self.clipboard.is_empty() {
            return Ok(0);
        }
        let reg = &self.project.registry;
        if reg.get(class).is_none() {
            return Err(AppError::Registry(RegistryError::NotFound(class)));
        }
        for node in &self.clipboard {
            if reg.kind_would_cycle(class, &node.kind) {
                return Err(AppError::Cycle);
            }
            if let Some(missing) = first_missing_ref(reg, &node.kind) {
                return Err(AppError::Registry(RegistryError::DanglingRef {
                    class,
                    idx: 0,
                    target: missing,
                }));
            }
        }
        self.snapshot();
        let nodes = self.clipboard.clone();
        let n = nodes.len();
        let reg = &mut self.project.registry;
        match after {
            // reverse, so each insert lands before the previous one and the
            // block keeps its order
            Some(idx) => {
                for node in nodes.into_iter().rev() {
                    reg.insert_node(class, idx + 1, node)?;
                }
            }
            None => reg.push_nodes(class, nodes)?,
        }
        Ok(n)
    }

    /// Append a node to a class.
    pub fn push_node(&mut self, class: ClassId, node: Node) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .push_node(class, node)
            .map_err(AppError::from)
    }

    /// Append `n` bytes worth of fields to a class: as many `Hex64` rows as fit,
    /// then `Hex8` rows for any remainder. Lets the user grow a class in bulk
    /// (e.g. 1024 bytes) instead of one field at a time.
    pub fn add_bytes(&mut self, class: ClassId, n: usize) -> Result<(), AppError> {
        self.snapshot();
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
            .map_err(AppError::from)
    }

    /// Insert a node after `idx` in `class`.
    pub fn insert_after(&mut self, class: ClassId, idx: usize, node: Node) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .insert_node(class, idx + 1, node)
            .map_err(AppError::from)
    }

    /// Delete node `idx` from `class`.
    pub fn delete_node(&mut self, class: ClassId, idx: usize) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .remove_node(class, idx)
            .map(|_| ())
            .map_err(AppError::from)
    }

    /// Delete several nodes at once. Sorts by class then by descending index so
    /// removing earlier entries doesn't shift the indices of later ones.
    ///
    /// Returns the first failure. Deletion continues past it: the targets are
    /// independent, and a stale index — a selection made before an MCP call
    /// mutated the same class — should not silently drop the rest.
    pub fn delete_many(&mut self, targets: &[(ClassId, usize)]) -> Result<(), AppError> {
        let mut t = targets.to_vec();
        t.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        t.dedup();
        self.snapshot();
        let mut first_err = None;
        for (cls, idx) in t {
            // straight to the registry: one multi-select delete is one undo step
            if let Err(e) = self.project.registry.remove_node(cls, idx) {
                first_err.get_or_insert(AppError::from(e));
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Change a node's kind, guarding against inline cycles.
    pub fn change_kind(
        &mut self,
        class: ClassId,
        idx: usize,
        kind: NodeKind,
    ) -> Result<(), AppError> {
        if self.project.registry.kind_would_cycle(class, &kind) {
            return Err(AppError::Cycle);
        }
        self.snapshot();
        self.project
            .registry
            .set_kind(class, idx, kind)
            .map_err(AppError::from)
    }

    /// Set the element count of an array node.
    pub fn set_array_count(
        &mut self,
        class: ClassId,
        idx: usize,
        count: usize,
    ) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .set_array_count(class, idx, count)
            .map_err(AppError::from)
    }

    /// Rename a node.
    pub fn rename_node(
        &mut self,
        class: ClassId,
        idx: usize,
        name: String,
    ) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .rename_node(class, idx, name)
            .map_err(AppError::from)
    }

    /// Set a node's comment.
    pub fn set_comment(
        &mut self,
        class: ClassId,
        idx: usize,
        comment: String,
    ) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .set_comment(class, idx, comment)
            .map_err(AppError::from)
    }

    /// Rename a class.
    pub fn rename_class(&mut self, id: ClassId, name: String) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .rename_class(id, name)
            .map_err(AppError::from)
    }

    /// Set the address expression of a class.
    pub fn set_address_expr(&mut self, id: ClassId, expr: String) -> Result<(), AppError> {
        self.snapshot();
        self.project
            .registry
            .set_address_expr(id, expr)
            .map_err(AppError::from)
    }

    // -- project / codegen -------------------------------------------------

    /// Generated source for the whole registry.
    pub fn codegen(&self, lang: Language) -> String {
        generate(&self.project.registry, lang)
    }

    /// Generate a standalone `vmem`-backed Cargo project mirroring the current
    /// classes into `dir` (created if needed). Returns the number of files
    /// written. The crate is named after `dir`'s final component.
    pub fn generate_project(&self, dir: &str) -> Result<usize, AppError> {
        use std::path::Path;
        let root = Path::new(dir);
        let crate_name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "reclass_project".to_string());
        let files = generate_project(
            &self.project.registry,
            &crate_name,
            self.project.attach_name.as_deref(),
        );
        for (rel, contents) in &files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| AppError::Io {
                    path: parent.display().to_string(),
                    source: e,
                })?;
            }
            std::fs::write(&path, contents).map_err(|e| AppError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
        }
        Ok(files.len())
    }

    /// Save the project to a RON file.
    pub fn save(&self, path: &str) -> Result<(), AppError> {
        self.project.save(path).map_err(AppError::from)
    }

    /// Load a project from a RON file (replaces state).
    ///
    /// Clears the undo history: the snapshots describe a different project, so
    /// undoing across a load would splice one project's classes into another's
    /// views.
    pub fn load(&mut self, path: &str) -> Result<(), AppError> {
        let project = Project::load(path)?;
        self.project = project;
        self.expand = ExpandState::new();
        self.selected_view = 0;
        self.view_status = vec![ViewStatus::default(); self.project.views.len()];
        self.expr_cache.clear();
        self.history.clear();
        Ok(())
    }
}

/// The first class id `kind` references that is no longer in `reg`, if any.
///
/// A node copied while its target class existed can outlive it: the registry
/// rewrites references on `remove_class`, but a clipboard entry is outside the
/// registry and keeps the dead id.
fn first_missing_ref(reg: &ClassRegistry, kind: &NodeKind) -> Option<ClassId> {
    match kind {
        NodeKind::ClassInstance { class_id } | NodeKind::ClassPtr { class_id } => {
            reg.get(*class_id).is_none().then_some(*class_id)
        }
        NodeKind::Array { element, .. } => first_missing_ref(reg, element),
        _ => None,
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
        st.delete_many(&[(c, 3), (c, 1), (c, 4), (c, 3)]).unwrap();
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

    fn field_names(st: &AppState, c: ClassId) -> Vec<String> {
        st.registry()
            .get(c)
            .map(|cl| cl.nodes.iter().map(|n| n.name.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn undo_reverses_one_edit_and_redo_reapplies_it() {
        let mut st = AppState::new();
        let c = st.add_class("Player");
        st.push_node(c, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        st.push_node(c, Node::new("mp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        assert_eq!(field_names(&st, c), ["hp", "mp"]);

        assert!(st.undo());
        assert_eq!(field_names(&st, c), ["hp"]);
        assert!(st.undo());
        assert_eq!(field_names(&st, c), Vec::<String>::new());

        assert!(st.redo());
        assert_eq!(field_names(&st, c), ["hp"]);
        assert!(st.redo());
        assert_eq!(field_names(&st, c), ["hp", "mp"]);
        assert!(!st.redo(), "redo past the tip must be a no-op");
    }

    #[test]
    fn a_multi_select_delete_is_one_undo_step() {
        // `delete_many` used to loop through `delete_node`; each iteration would
        // have taken its own snapshot, so undoing a 3-row delete would restore
        // one row per Ctrl+Z.
        let mut st = AppState::new();
        let c = st.add_class("S");
        for i in 0..4 {
            st.push_node(c, Node::new(format!("f{i}"), NodeKind::Hex(IntWidth::W8)))
                .unwrap();
        }
        let before = st.undo_depth();
        st.delete_many(&[(c, 0), (c, 1), (c, 2)]).unwrap();
        assert_eq!(field_names(&st, c), ["f3"]);
        assert_eq!(st.undo_depth(), before + 1, "one action, one undo step");
        assert!(st.undo());
        assert_eq!(field_names(&st, c), ["f0", "f1", "f2", "f3"]);
    }

    #[test]
    fn add_array_is_one_undo_step() {
        let mut st = AppState::new();
        let c = st.add_class("S");
        let before = st.undo_depth();
        st.add_array(c, NodeKind::Hex(IntWidth::W32), 8).unwrap();
        assert_eq!(st.undo_depth(), before + 1);
        assert!(st.undo());
        assert!(field_names(&st, c).is_empty());
    }

    #[test]
    fn undo_restores_references_a_class_removal_rewrote() {
        // `remove_class` rewrites every reference to the dead class across the
        // whole registry, which is why undo snapshots the project rather than
        // journalling an inverse operation.
        let mut st = AppState::new();
        let inner = st.add_class("Inner");
        st.push_node(inner, Node::new("x", NodeKind::Hex(IntWidth::W32)))
            .unwrap();
        let outer = st.add_class("Outer");
        st.push_node(
            outer,
            Node::new("i", NodeKind::ClassInstance { class_id: inner }),
        )
        .unwrap();
        assert_eq!(st.registry().size_of(outer), 4);

        st.remove_class(inner);
        assert!(st.registry().get(inner).is_none());
        // the inline instance became same-size Unknown, preserving layout
        assert_eq!(st.registry().size_of(outer), 4);

        assert!(st.undo());
        assert!(st.registry().get(inner).is_some());
        assert_eq!(
            st.registry().get(outer).unwrap().nodes[0].kind,
            NodeKind::ClassInstance { class_id: inner },
            "the rewritten reference came back"
        );
    }

    #[test]
    fn a_new_edit_discards_the_redo_branch() {
        let mut st = AppState::new();
        let c = st.add_class("S");
        st.push_node(c, Node::new("a", NodeKind::Hex(IntWidth::W8)))
            .unwrap();
        assert!(st.undo());
        assert!(st.can_redo());
        st.push_node(c, Node::new("b", NodeKind::Hex(IntWidth::W8)))
            .unwrap();
        assert!(!st.can_redo(), "branching forward must drop the old future");
        assert_eq!(field_names(&st, c), ["b"]);
    }

    #[test]
    fn undo_on_a_fresh_state_is_a_no_op() {
        let mut st = AppState::new();
        assert!(!st.can_undo());
        assert!(!st.undo());
        assert!(!st.redo());
    }

    #[test]
    fn a_large_project_trades_undo_depth_for_memory() {
        // Depth alone is not a memory bound: 64 snapshots of a big project is
        // ~100 MB (benches/history.rs). Once the node budget binds, the stack
        // must stop growing well short of DEPTH.
        let mut st = AppState::new();
        let c = st.add_class("Big");
        let wide = History::MAX_NODES / 8;
        st.add_bytes(c, wide * 8).unwrap();
        assert!(st.registry().get(c).unwrap().nodes.len() >= wide);

        for i in 0..20 {
            st.rename_node(c, 0, format!("n{i}")).unwrap();
        }
        assert!(
            st.undo_depth() < History::DEPTH,
            "node budget never bound: depth {}",
            st.undo_depth()
        );
        // the most recent edit is always undoable, however big the project
        assert!(st.can_undo());
        assert!(st.undo());
        assert_eq!(st.registry().get(c).unwrap().nodes[0].name, "n18");
    }

    #[test]
    fn one_oversized_snapshot_is_still_undoable() {
        // A single project bigger than the whole budget must not trim itself
        // away: the edit that just happened would become unundoable.
        let mut st = AppState::new();
        let c = st.add_class("Huge");
        st.add_bytes(c, (History::MAX_NODES + 1000) * 8).unwrap();
        st.rename_node(c, 0, "renamed".into()).unwrap();
        assert_eq!(st.undo_depth(), 1);
        assert!(st.undo());
        assert_ne!(st.registry().get(c).unwrap().nodes[0].name, "renamed");
    }

    #[test]
    fn the_history_depth_is_bounded() {
        let mut st = AppState::new();
        let c = st.add_class("S");
        for i in 0..(History::DEPTH * 2) {
            st.push_node(c, Node::new(format!("f{i}"), NodeKind::Hex(IntWidth::W8)))
                .unwrap();
        }
        assert_eq!(st.undo_depth(), History::DEPTH);
        // unwinding the whole stack must not panic or restore a half-state
        while st.undo() {}
        assert!(!st.can_undo());
        assert_eq!(field_names(&st, c).len(), History::DEPTH);
    }

    #[test]
    fn undo_keeps_the_view_cursor_in_range() {
        // `add_class` opens a view; undoing it removes that view, and a stale
        // `selected_view` would index past the end.
        let mut st = AppState::new();
        st.add_class("A");
        st.add_class("B");
        assert_eq!(st.selected_view, 1);
        assert!(st.undo());
        assert_eq!(st.project.views.len(), 1);
        assert_eq!(st.selected_view, 0);
        assert!(st.selected_class().is_some());
        let _ = st.compute_rows();
    }

    #[test]
    fn loading_a_project_clears_the_history() {
        let dir = std::env::temp_dir().join("reclass_undo_load.ron");
        let mut st = AppState::new();
        let c = st.add_class("S");
        st.push_node(c, Node::new("a", NodeKind::Hex(IntWidth::W8)))
            .unwrap();
        st.save(dir.to_str().unwrap()).unwrap();

        let mut other = AppState::new();
        other.add_class("Different");
        other.load(dir.to_str().unwrap()).unwrap();
        // undoing across a load would splice one project's classes into
        // another's views
        assert!(!other.can_undo());
        assert!(!other.can_redo());
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn copy_and_paste_moves_fields_between_classes() {
        let mut st = AppState::new();
        let src = st.add_class("Src");
        st.push_node(src, Node::new("hp", NodeKind::Int(IntWidth::W32)))
            .unwrap();
        st.push_node(src, Node::new("mp", NodeKind::Float32))
            .unwrap();
        let dst = st.add_class("Dst");
        st.push_node(dst, Node::new("head", NodeKind::Hex(IntWidth::W8)))
            .unwrap();

        assert_eq!(st.copy_nodes(&[(src, 0), (src, 1)]), 2);
        assert_eq!(st.paste_nodes(dst, Some(0)).unwrap(), 2);
        assert_eq!(field_names(&st, dst), ["head", "hp", "mp"]);
        // the source is untouched — this is copy, not move
        assert_eq!(field_names(&st, src), ["hp", "mp"]);
        // and the kinds came across, not just the names
        assert_eq!(
            st.registry().get(dst).unwrap().nodes[2].kind,
            NodeKind::Float32
        );
    }

    #[test]
    fn paste_appends_when_no_anchor_is_given() {
        let mut st = AppState::new();
        let c = st.add_class("S");
        st.push_node(c, Node::new("a", NodeKind::Hex(IntWidth::W8)))
            .unwrap();
        st.copy_nodes(&[(c, 0)]);
        st.paste_nodes(c, None).unwrap();
        assert_eq!(field_names(&st, c), ["a", "a"]);
    }

    #[test]
    fn a_copied_block_keeps_its_layout_order() {
        // Selection is a HashSet, so the copy must impose order itself or a
        // three-row block pastes back scrambled.
        let mut st = AppState::new();
        let c = st.add_class("S");
        for n in ["a", "b", "c"] {
            st.push_node(c, Node::new(n, NodeKind::Hex(IntWidth::W8)))
                .unwrap();
        }
        // targets deliberately out of order
        assert_eq!(st.copy_nodes(&[(c, 2), (c, 0), (c, 1)]), 3);
        st.paste_nodes(c, Some(2)).unwrap();
        assert_eq!(field_names(&st, c), ["a", "b", "c", "a", "b", "c"]);
    }

    #[test]
    fn paste_is_one_undo_step() {
        let mut st = AppState::new();
        let c = st.add_class("S");
        for n in ["a", "b", "c"] {
            st.push_node(c, Node::new(n, NodeKind::Hex(IntWidth::W8)))
                .unwrap();
        }
        st.copy_nodes(&[(c, 0), (c, 1), (c, 2)]);
        let before = st.undo_depth();
        st.paste_nodes(c, None).unwrap();
        assert_eq!(st.undo_depth(), before + 1);
        assert!(st.undo());
        assert_eq!(field_names(&st, c), ["a", "b", "c"]);
    }

    #[test]
    fn pasting_a_self_instance_is_refused_whole() {
        let mut st = AppState::new();
        let a = st.add_class("A");
        let b = st.add_class("B");
        st.push_node(b, Node::new("x", NodeKind::Hex(IntWidth::W8)))
            .unwrap();
        // Not a cycle yet: A holds nothing. B holds an inline A; copying that
        // out of B and pasting it into A is what closes the loop.
        st.push_node(b, Node::new("a", NodeKind::ClassInstance { class_id: a }))
            .unwrap();
        st.copy_nodes(&[(b, 0), (b, 1)]);
        let before = field_names(&st, a);
        assert!(matches!(st.paste_nodes(a, None), Err(AppError::Cycle)));
        // nothing landed: a partial paste is worse than none
        assert_eq!(field_names(&st, a), before);
    }

    #[test]
    fn pasting_a_reference_to_a_deleted_class_is_refused() {
        // The clipboard lives outside the registry, so `remove_class`'s
        // reference rewrite cannot reach it.
        let mut st = AppState::new();
        let gone = st.add_class("Gone");
        let holder = st.add_class("Holder");
        st.push_node(
            holder,
            Node::new("p", NodeKind::ClassPtr { class_id: gone }),
        )
        .unwrap();
        st.copy_nodes(&[(holder, 0)]);
        st.remove_class(gone);

        let dst = st.add_class("Dst");
        assert!(matches!(
            st.paste_nodes(dst, None),
            Err(AppError::Registry(RegistryError::DanglingRef { .. }))
        ));
        assert!(field_names(&st, dst).is_empty());
    }

    #[test]
    fn copying_a_stale_index_skips_it_instead_of_failing() {
        let mut st = AppState::new();
        let c = st.add_class("S");
        st.push_node(c, Node::new("only", NodeKind::Hex(IntWidth::W8)))
            .unwrap();
        assert_eq!(st.copy_nodes(&[(c, 0), (c, 99), (404, 0)]), 1);
        assert_eq!(st.clipboard()[0].name, "only");
    }

    #[test]
    fn pasting_an_empty_clipboard_changes_nothing() {
        let mut st = AppState::new();
        let c = st.add_class("S");
        let before = st.undo_depth();
        assert_eq!(st.paste_nodes(c, None).unwrap(), 0);
        assert_eq!(st.undo_depth(), before, "no-op must not burn an undo step");
    }
}
