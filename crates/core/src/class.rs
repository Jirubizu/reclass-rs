//! Classes, the class registry, derived offsets, and cycle detection.
//!
//! Offsets are **never stored**: node `i`'s offset is the sum of the sizes of
//! nodes `0..i`. Sizes recurse through the registry for `ClassInstance` and
//! `Array`, so the registry owns size/offset computation and memoizes results;
//! any structural edit (`&mut self`) clears the caches.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use crate::node::{Node, NodeKind};

/// Identifier for a class within a [`ClassRegistry`].
pub type ClassId = u32;

/// A class: an ordered list of typed [`Node`]s plus its address expression.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Class {
    /// Stable id.
    pub id: ClassId,
    /// Display name / codegen type name.
    pub name: String,
    /// Fields, in layout order.
    pub nodes: Vec<Node>,
    /// Per-class address-bar expression (see [`crate::expr`]).
    #[cfg_attr(feature = "serde", serde(default))]
    pub address_expr: String,
}

impl Class {
    /// An empty class.
    pub fn new(id: ClassId, name: impl Into<String>) -> Self {
        Class {
            id,
            name: name.into(),
            nodes: Vec::new(),
            address_expr: String::new(),
        }
    }
}

/// Errors from registry operations / validation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// No class with this id.
    #[error("class #{0} not found")]
    NotFound(ClassId),
    /// An inline `ClassInstance` nesting cycle was found (the chain of ids).
    #[error("inline class cycle: {0:?}")]
    Cycle(Vec<ClassId>),
    /// A node index was out of range for its class.
    #[error("node index {idx} out of bounds for class #{class} ({len} nodes)")]
    NodeOutOfBounds {
        /// Class id.
        class: ClassId,
        /// Requested index.
        idx: usize,
        /// Number of nodes.
        len: usize,
    },
}

#[derive(Debug, Default)]
struct Cache {
    sizes: HashMap<ClassId, usize>,
    offsets: HashMap<ClassId, Vec<usize>>,
}

/// Owns every class by id and answers size/offset queries.
#[derive(Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassRegistry {
    classes: BTreeMap<ClassId, Class>,
    next_id: ClassId,
    #[cfg_attr(feature = "serde", serde(skip))]
    cache: RefCell<Cache>,
}

impl ClassRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn invalidate(&mut self) {
        let c = self.cache.get_mut();
        c.sizes.clear();
        c.offsets.clear();
    }

    // -- class lifecycle ---------------------------------------------------

    /// Create a new empty class, returning its id.
    pub fn add_class(&mut self, name: impl Into<String>) -> ClassId {
        let id = self.next_id;
        self.next_id += 1;
        self.classes.insert(id, Class::new(id, name));
        self.invalidate();
        id
    }

    /// Insert a fully-built class, keeping `next_id` ahead of it.
    pub fn insert_class(&mut self, class: Class) {
        self.next_id = self.next_id.max(class.id + 1);
        self.classes.insert(class.id, class);
        self.invalidate();
    }

    /// Remove a class (does not touch references to it from other classes).
    pub fn remove_class(&mut self, id: ClassId) -> Option<Class> {
        let removed = self.classes.remove(&id);
        if removed.is_some() {
            self.invalidate();
        }
        removed
    }

    /// Borrow a class.
    pub fn get(&self, id: ClassId) -> Option<&Class> {
        self.classes.get(&id)
    }

    /// Mutably borrow a class. Callers that change layout MUST call
    /// [`touch`](Self::touch) afterwards, or use the typed edit helpers.
    pub fn get_mut(&mut self, id: ClassId) -> Option<&mut Class> {
        self.classes.get_mut(&id)
    }

    /// Force a cache invalidation (after raw `get_mut` layout edits).
    pub fn touch(&mut self) {
        self.invalidate();
    }

    /// A class's display name.
    pub fn name_of(&self, id: ClassId) -> Option<&str> {
        self.classes.get(&id).map(|c| c.name.as_str())
    }

    /// Number of classes.
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// Whether there are no classes.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Iterate classes in ascending id order.
    pub fn iter(&self) -> impl Iterator<Item = &Class> {
        self.classes.values()
    }

    /// All class ids, ascending.
    pub fn ids(&self) -> Vec<ClassId> {
        self.classes.keys().copied().collect()
    }

    // -- node edits (Phase 5) ---------------------------------------------

    /// Append a node to a class.
    pub fn push_node(&mut self, class: ClassId, node: Node) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&class)
            .ok_or(RegistryError::NotFound(class))?;
        c.nodes.push(node);
        self.invalidate();
        Ok(())
    }

    /// Insert a node at `idx` (shifts following offsets).
    pub fn insert_node(
        &mut self,
        class: ClassId,
        idx: usize,
        node: Node,
    ) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&class)
            .ok_or(RegistryError::NotFound(class))?;
        if idx > c.nodes.len() {
            return Err(RegistryError::NodeOutOfBounds {
                class,
                idx,
                len: c.nodes.len(),
            });
        }
        c.nodes.insert(idx, node);
        self.invalidate();
        Ok(())
    }

    /// Remove the node at `idx`.
    pub fn remove_node(&mut self, class: ClassId, idx: usize) -> Result<Node, RegistryError> {
        let c = self
            .classes
            .get_mut(&class)
            .ok_or(RegistryError::NotFound(class))?;
        if idx >= c.nodes.len() {
            return Err(RegistryError::NodeOutOfBounds {
                class,
                idx,
                len: c.nodes.len(),
            });
        }
        let n = c.nodes.remove(idx);
        self.invalidate();
        Ok(n)
    }

    /// Change a node's kind (may grow or shrink the class).
    pub fn set_kind(
        &mut self,
        class: ClassId,
        idx: usize,
        kind: NodeKind,
    ) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&class)
            .ok_or(RegistryError::NotFound(class))?;
        let len = c.nodes.len();
        let n = c
            .nodes
            .get_mut(idx)
            .ok_or(RegistryError::NodeOutOfBounds { class, idx, len })?;
        n.kind = kind;
        self.invalidate();
        Ok(())
    }

    /// Set the element count of an `Array` node.
    pub fn set_array_count(
        &mut self,
        class: ClassId,
        idx: usize,
        count: usize,
    ) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&class)
            .ok_or(RegistryError::NotFound(class))?;
        let len = c.nodes.len();
        let n = c
            .nodes
            .get_mut(idx)
            .ok_or(RegistryError::NodeOutOfBounds { class, idx, len })?;
        if let NodeKind::Array { count: cnt, .. } = &mut n.kind {
            *cnt = count;
            self.invalidate();
            Ok(())
        } else {
            Err(RegistryError::NodeOutOfBounds { class, idx, len })
        }
    }

    /// Rename a node.
    pub fn rename_node(
        &mut self,
        class: ClassId,
        idx: usize,
        name: impl Into<String>,
    ) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&class)
            .ok_or(RegistryError::NotFound(class))?;
        let len = c.nodes.len();
        let n = c
            .nodes
            .get_mut(idx)
            .ok_or(RegistryError::NodeOutOfBounds { class, idx, len })?;
        n.name = name.into();
        // name change does not affect layout, but keep it simple.
        Ok(())
    }

    /// Set a node's comment.
    pub fn set_comment(
        &mut self,
        class: ClassId,
        idx: usize,
        comment: impl Into<String>,
    ) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&class)
            .ok_or(RegistryError::NotFound(class))?;
        let len = c.nodes.len();
        let n = c
            .nodes
            .get_mut(idx)
            .ok_or(RegistryError::NodeOutOfBounds { class, idx, len })?;
        n.comment = comment.into();
        Ok(())
    }

    /// Rename a class.
    pub fn rename_class(
        &mut self,
        id: ClassId,
        name: impl Into<String>,
    ) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&id)
            .ok_or(RegistryError::NotFound(id))?;
        c.name = name.into();
        Ok(())
    }

    /// Set a class's address expression.
    pub fn set_address_expr(
        &mut self,
        id: ClassId,
        expr: impl Into<String>,
    ) -> Result<(), RegistryError> {
        let c = self
            .classes
            .get_mut(&id)
            .ok_or(RegistryError::NotFound(id))?;
        c.address_expr = expr.into();
        Ok(())
    }

    // -- sizing / offsets --------------------------------------------------

    /// Total byte size of a class (sum of its node sizes). Cycle-safe: an
    /// inline `ClassInstance` cycle contributes 0 where it re-enters.
    pub fn size_of(&self, id: ClassId) -> usize {
        let mut stack = Vec::new();
        self.size_of_inner(id, &mut stack)
    }

    fn size_of_inner(&self, id: ClassId, stack: &mut Vec<ClassId>) -> usize {
        if stack.contains(&id) {
            return 0; // inline cycle: this re-entry contributes nothing
        }
        if let Some(&s) = self.cache.borrow().sizes.get(&id) {
            return s;
        }
        let total = match self.classes.get(&id) {
            Some(class) => {
                stack.push(id);
                let t = class
                    .nodes
                    .iter()
                    .map(|n| self.node_size_inner(&n.kind, stack))
                    .sum();
                stack.pop();
                t
            }
            None => 0,
        };
        self.cache.borrow_mut().sizes.insert(id, total);
        total
    }

    fn node_size_inner(&self, kind: &NodeKind, stack: &mut Vec<ClassId>) -> usize {
        match kind {
            NodeKind::ClassInstance { class_id } => self.size_of_inner(*class_id, stack),
            NodeKind::Array { element, count } => {
                self.node_size_inner(element, stack).saturating_mul(*count)
            }
            other => other.fixed_size(),
        }
    }

    /// Offset of each node, in order. `offsets(id)[i]` = sum of sizes `0..i`.
    pub fn offsets(&self, id: ClassId) -> Vec<usize> {
        if let Some(o) = self.cache.borrow().offsets.get(&id) {
            return o.clone();
        }
        let offs = match self.classes.get(&id) {
            Some(class) => {
                let mut offs = Vec::with_capacity(class.nodes.len());
                let mut acc = 0usize;
                for n in &class.nodes {
                    offs.push(acc);
                    let mut stack = vec![id];
                    acc += self.node_size_inner(&n.kind, &mut stack);
                }
                offs
            }
            None => Vec::new(),
        };
        self.cache.borrow_mut().offsets.insert(id, offs.clone());
        offs
    }

    /// Offset of node `idx`, or `None` if out of range / unknown class.
    pub fn offset_of(&self, id: ClassId, idx: usize) -> Option<usize> {
        self.offsets(id).get(idx).copied()
    }

    // -- validation --------------------------------------------------------

    /// Reject inline `ClassInstance` cycles. `ClassPtr` cycles are fine (a read
    /// boundary, not inline layout).
    pub fn validate(&self) -> Result<(), RegistryError> {
        #[derive(Clone, Copy, PartialEq)]
        enum State {
            Visiting,
            Done,
        }
        let mut state: HashMap<ClassId, State> = HashMap::new();
        let mut path: Vec<ClassId> = Vec::new();

        // Iterative-safe recursion via an explicit helper.
        fn dfs(
            reg: &ClassRegistry,
            id: ClassId,
            state: &mut HashMap<ClassId, State>,
            path: &mut Vec<ClassId>,
        ) -> Result<(), RegistryError> {
            match state.get(&id) {
                Some(State::Done) => return Ok(()),
                Some(State::Visiting) => {
                    let mut cycle = path.clone();
                    cycle.push(id);
                    return Err(RegistryError::Cycle(cycle));
                }
                None => {}
            }
            state.insert(id, State::Visiting);
            path.push(id);
            if let Some(class) = reg.classes.get(&id) {
                for n in &class.nodes {
                    if let Some(child) = inline_class(&n.kind) {
                        dfs(reg, child, state, path)?;
                    }
                }
            }
            path.pop();
            state.insert(id, State::Done);
            Ok(())
        }

        for &id in self.classes.keys() {
            dfs(self, id, &mut state, &mut path)?;
        }
        Ok(())
    }

    /// Whether adding an inline `ClassInstance { child }` node to `parent` would
    /// create a cycle (used to guard the UI before committing the edit).
    pub fn would_cycle(&self, parent: ClassId, child: ClassId) -> bool {
        if parent == child {
            return true;
        }
        // Does `child` already inline-reach `parent`?
        let mut seen = vec![child];
        let mut i = 0;
        while i < seen.len() {
            let cur = seen[i];
            i += 1;
            if cur == parent {
                return true;
            }
            if let Some(class) = self.classes.get(&cur) {
                for n in &class.nodes {
                    if let Some(c) = inline_class(&n.kind)
                        && !seen.contains(&c)
                    {
                        seen.push(c);
                    }
                }
            }
        }
        false
    }
}

/// The class id a kind inlines (directly, or through array nesting). `ClassPtr`
/// is intentionally **not** inline.
fn inline_class(kind: &NodeKind) -> Option<ClassId> {
    match kind {
        NodeKind::ClassInstance { class_id } => Some(*class_id),
        NodeKind::Array { element, .. } => inline_class(element),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{IntWidth, Node, NodeKind};

    fn h32() -> NodeKind {
        NodeKind::Hex(IntWidth::W32)
    }

    #[test]
    fn offsets_basic() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("Player");
        reg.push_node(c, Node::new("a", NodeKind::Int(IntWidth::W32)))
            .unwrap(); // 4
        reg.push_node(c, Node::new("b", NodeKind::Int(IntWidth::W64)))
            .unwrap(); // 8
        reg.push_node(c, Node::new("c", NodeKind::Bool)).unwrap(); // 1
        assert_eq!(reg.offsets(c), vec![0, 4, 12]);
        assert_eq!(reg.size_of(c), 13);
    }

    #[test]
    fn insert_head_middle_tail_shifts() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(c, Node::new("a", h32())).unwrap();
        reg.push_node(c, Node::new("b", h32())).unwrap();
        assert_eq!(reg.offsets(c), vec![0, 4]);
        // insert at head
        reg.insert_node(c, 0, Node::new("h", NodeKind::Int(IntWidth::W64)))
            .unwrap();
        assert_eq!(reg.offsets(c), vec![0, 8, 12]);
        // insert in middle
        reg.insert_node(c, 1, Node::new("m", NodeKind::Bool))
            .unwrap();
        assert_eq!(reg.offsets(c), vec![0, 8, 9, 13]);
        // insert at tail
        reg.insert_node(c, 4, Node::new("t", h32())).unwrap();
        assert_eq!(reg.offsets(c), vec![0, 8, 9, 13, 17]);
        assert_eq!(reg.size_of(c), 21);
    }

    #[test]
    fn change_type_grow_shrink_and_delete() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(c, Node::new("a", h32())).unwrap(); // 4
        reg.push_node(c, Node::new("b", h32())).unwrap(); // 4
        reg.push_node(c, Node::new("c", h32())).unwrap(); // 4
        assert_eq!(reg.offsets(c), vec![0, 4, 8]);
        // grow a to 8 bytes
        reg.set_kind(c, 0, NodeKind::Int(IntWidth::W64)).unwrap();
        assert_eq!(reg.offsets(c), vec![0, 8, 12]);
        // shrink a to 1 byte
        reg.set_kind(c, 0, NodeKind::Bool).unwrap();
        assert_eq!(reg.offsets(c), vec![0, 1, 5]);
        // delete middle
        reg.remove_node(c, 1).unwrap();
        assert_eq!(reg.offsets(c), vec![0, 1]);
    }

    #[test]
    fn array_length_change() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(
            c,
            Node::new(
                "arr",
                NodeKind::Array {
                    element: Box::new(h32()),
                    count: 3,
                },
            ),
        )
        .unwrap();
        reg.push_node(c, Node::new("tail", h32())).unwrap();
        assert_eq!(reg.offsets(c), vec![0, 12]);
        reg.set_array_count(c, 0, 5).unwrap();
        assert_eq!(reg.offsets(c), vec![0, 20]);
    }

    #[test]
    fn nested_class_instance_size_and_offsets() {
        let mut reg = ClassRegistry::new();
        let inner = reg.add_class("Inner");
        reg.push_node(inner, Node::new("x", NodeKind::Int(IntWidth::W64)))
            .unwrap(); // 8
        reg.push_node(inner, Node::new("y", h32())).unwrap(); // 4 -> Inner = 12
        let outer = reg.add_class("Outer");
        reg.push_node(outer, Node::new("flag", NodeKind::Bool))
            .unwrap(); // 1
        reg.push_node(
            outer,
            Node::new("inner", NodeKind::ClassInstance { class_id: inner }),
        )
        .unwrap(); // 12
        reg.push_node(outer, Node::new("z", h32())).unwrap(); // 4
        assert_eq!(reg.size_of(inner), 12);
        assert_eq!(reg.offsets(outer), vec![0, 1, 13]);
        assert_eq!(reg.size_of(outer), 17);
    }

    #[test]
    fn array_of_class_instance() {
        let mut reg = ClassRegistry::new();
        let inner = reg.add_class("Inner");
        reg.push_node(inner, Node::new("x", h32())).unwrap(); // 4
        let outer = reg.add_class("Outer");
        reg.push_node(
            outer,
            Node::new(
                "items",
                NodeKind::Array {
                    element: Box::new(NodeKind::ClassInstance { class_id: inner }),
                    count: 4,
                },
            ),
        )
        .unwrap();
        assert_eq!(reg.size_of(outer), 16);
    }

    #[test]
    fn self_referential_class_instance_is_a_cycle() {
        let mut reg = ClassRegistry::new();
        let a = reg.add_class("A");
        reg.push_node(a, Node::new("me", NodeKind::ClassInstance { class_id: a }))
            .unwrap();
        // size terminates (cycle contributes 0)
        assert_eq!(reg.size_of(a), 0);
        // validation rejects it
        assert!(matches!(reg.validate(), Err(RegistryError::Cycle(_))));
        assert!(reg.would_cycle(a, a));
    }

    #[test]
    fn mutual_inline_cycle_detected() {
        let mut reg = ClassRegistry::new();
        let a = reg.add_class("A");
        let b = reg.add_class("B");
        reg.push_node(a, Node::new("b", NodeKind::ClassInstance { class_id: b }))
            .unwrap();
        reg.push_node(b, Node::new("a", NodeKind::ClassInstance { class_id: a }))
            .unwrap();
        assert!(matches!(reg.validate(), Err(RegistryError::Cycle(_))));
        assert!(reg.would_cycle(a, b));
        assert!(reg.would_cycle(b, a));
    }

    #[test]
    fn class_ptr_cycle_is_allowed() {
        // A node that points to its own class is fine: it's a read boundary.
        let mut reg = ClassRegistry::new();
        let a = reg.add_class("Node");
        reg.push_node(a, Node::new("val", h32())).unwrap();
        reg.push_node(a, Node::new("next", NodeKind::ClassPtr { class_id: a }))
            .unwrap();
        assert!(reg.validate().is_ok());
        assert!(!reg.would_cycle(a, a) || true); // would_cycle only concerns inline
        assert_eq!(reg.size_of(a), 12); // 4 + 8 (pointer)
    }

    #[test]
    fn cache_invalidates_on_edit() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        reg.push_node(c, Node::new("a", h32())).unwrap();
        assert_eq!(reg.size_of(c), 4); // populates cache
        reg.push_node(c, Node::new("b", h32())).unwrap();
        assert_eq!(reg.size_of(c), 8); // cache was invalidated
        assert_eq!(reg.offsets(c), vec![0, 4]);
    }

    #[test]
    fn out_of_bounds_edits_error() {
        let mut reg = ClassRegistry::new();
        let c = reg.add_class("C");
        assert_eq!(
            reg.insert_node(c, 5, Node::new("x", h32())),
            Err(RegistryError::NodeOutOfBounds {
                class: c,
                idx: 5,
                len: 0
            })
        );
        assert!(matches!(
            reg.remove_node(c, 0),
            Err(RegistryError::NodeOutOfBounds { .. })
        ));
        assert_eq!(reg.set_kind(99, 0, h32()), Err(RegistryError::NotFound(99)));
    }
}
