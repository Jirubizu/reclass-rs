//! ReClass.NET `.rcnet` import and export.
//!
//! A `.rcnet` is a ZIP holding one `Data.xml`: a project-level `<enums>` table
//! plus a flat list of `<class>` elements, each a list of `<node>` elements
//! keyed by a .NET type name. This module maps that onto [`ClassRegistry`] and
//! back, which is what makes the large existing corpus of community structs
//! usable here.
//!
//! **The mapping is not lossless in either direction, by construction.** Some
//! ReClass.NET node types have no equivalent in this model (unions, virtual
//! method tables as a distinct kind, UTF-32 text) and some of this model's do
//! not exist there. Rather than silently reshaping data, every conversion
//! returns a [`Report`] naming what it approximated or dropped, so the caller
//! can put it in front of the user.
//!
//! Structural facts the file format forces:
//!
//! * Classes are identified by GUID, not index. Import maps each GUID to a
//!   fresh [`ClassId`]; export derives a stable GUID from the id.
//! * Enum variants live in a project-level table referenced by name, while this
//!   model stores them on the node. Import inlines the table; export
//!   synthesizes one `<enum>` per enum-typed field.
//! * `PointerNode` is a wrapper around an inner node. A pointer whose inner
//!   node is a class becomes [`NodeKind::ClassPtr`]; anything else becomes a
//!   plain [`NodeKind::Pointer`], because this model cannot express
//!   "pointer to a float".

use std::collections::HashMap;

use crate::class::{Class, ClassId, ClassRegistry, PtrWidth};
use crate::node::{EnumVariant, IntWidth, Node, NodeKind, TextEncoding};

mod xml;
mod zip;

use xml::{Element, Writer};

/// The single entry every `.rcnet` archive contains.
const DATA_ENTRY: &str = "Data.xml";
/// File version ReClass.NET writes; the high 16 bits are its compatibility mask.
const FILE_VERSION: u32 = 0x0001_0001;
/// Versions differing above this mask are unreadable, per ReClass.NET's own check.
const VERSION_CRITICAL_MASK: u32 = 0xFFFF_0000;

/// Why an import or export failed outright.
///
/// Approximations do not appear here — they go in [`Report::notes`], because a
/// file that mostly converted is far more useful than an error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RcnetError {
    /// The bytes are not a ZIP archive.
    #[error("not a ReClass.NET file (no ZIP container)")]
    NotAZip,
    /// The archive has no entry with this name.
    #[error("archive has no '{0}' entry")]
    MissingEntry(String),
    /// An unsupported ZIP compression method.
    #[error("unsupported ZIP compression method {0}")]
    Compression(u16),
    /// The archive or its payload is damaged.
    #[error("damaged archive: {0}")]
    Corrupt(String),
    /// The XML did not parse.
    #[error("malformed XML at byte {pos}: {msg}")]
    Xml {
        /// What went wrong.
        msg: String,
        /// Byte offset into the document.
        pos: usize,
    },
    /// The document parsed but is not a ReClass.NET project.
    #[error("not a ReClass.NET project: {0}")]
    NotAProject(String),
    /// The file was written by a ReClass.NET too new to read.
    #[error("file version {0:#x} is newer than this reader supports")]
    UnsupportedVersion(u32),
    /// Reading or writing the file failed.
    #[error("{path}: {source}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// What a conversion approximated or dropped.
///
/// Always returned alongside a successful conversion — an empty `notes` means
/// the mapping was exact.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// One human-readable line per approximation, in encounter order.
    pub notes: Vec<String>,
    /// Classes converted.
    pub classes: usize,
    /// Fields converted.
    pub nodes: usize,
}

impl Report {
    fn note(&mut self, msg: impl Into<String>) {
        self.notes.push(msg.into());
    }

    /// Whether the conversion was exact.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.notes.is_empty()
    }
}

/// A ReClass.NET enum table entry, while importing.
struct EnumDesc {
    width: IntWidth,
    variants: Vec<EnumVariant>,
}

fn width_from_bytes(bytes: u32) -> IntWidth {
    match bytes {
        1 => IntWidth::W8,
        2 => IntWidth::W16,
        8 => IntWidth::W64,
        _ => IntWidth::W32,
    }
}

/// The smallest width that can hold `bits`.
fn width_for_bits(bits: u32) -> IntWidth {
    match bits {
        0..=8 => IntWidth::W8,
        9..=16 => IntWidth::W16,
        17..=32 => IntWidth::W32,
        _ => IntWidth::W64,
    }
}

fn size_name(w: IntWidth) -> &'static str {
    match w {
        IntWidth::W8 => "OneByte",
        IntWidth::W16 => "TwoBytes",
        IntWidth::W32 => "FourBytes",
        IntWidth::W64 => "EightBytes",
    }
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

/// Parse a `.rcnet` archive into a registry.
///
/// The returned registry is standalone: class ids are freshly allocated and
/// every reference is rewritten to them, so it can be merged into a project or
/// used on its own.
pub fn import(bytes: &[u8]) -> Result<(ClassRegistry, Report), RcnetError> {
    let data = zip::read_entry(bytes, DATA_ENTRY)?;
    let text = String::from_utf8_lossy(&data);
    import_xml(&text)
}

/// Parse a bare `Data.xml` document (the archive's payload).
///
/// Separate from [`import`] so the mapping is testable without building a ZIP,
/// and so a caller holding an already-extracted document need not repack it.
pub fn import_xml(text: &str) -> Result<(ClassRegistry, Report), RcnetError> {
    let root = xml::parse(text)?;
    if root.tag != "reclass" {
        return Err(RcnetError::NotAProject(format!(
            "root element is <{}>, expected <reclass>",
            root.tag
        )));
    }
    let version: u32 = root.attr_num("version").unwrap_or(FILE_VERSION);
    if version & VERSION_CRITICAL_MASK > FILE_VERSION & VERSION_CRITICAL_MASK {
        return Err(RcnetError::UnsupportedVersion(version));
    }
    let classes_el = root
        .child("classes")
        .ok_or_else(|| RcnetError::NotAProject("no <classes> element".into()))?;

    let mut report = Report::default();
    // `type` is ReClass.NET's platform tag. It decides pointer width, which
    // moves every offset after a pointer, so it is worth honouring rather than
    // defaulting.
    let ptr = match root.attr_or_empty("type") {
        "x86" => PtrWidth::P32,
        "" => PtrWidth::P64,
        "x64" => PtrWidth::P64,
        other => {
            report.note(format!("unknown platform '{other}'; assuming 64-bit"));
            PtrWidth::P64
        }
    };

    let enums = read_enums(&root, &mut report);

    // Two passes: every class must exist before any reference to it can be
    // resolved, and the file makes no ordering promise.
    let mut reg = ClassRegistry::new();
    reg.set_ptr_width(ptr);
    let mut by_uuid: HashMap<&str, ClassId> = HashMap::new();
    let mut pending: Vec<(&Element, ClassId)> = Vec::new();
    for el in classes_el.children_named("class") {
        let uuid = el.attr_or_empty("uuid");
        if by_uuid.contains_key(uuid) && !uuid.is_empty() {
            report.note(format!("duplicate class uuid {uuid}; keeping the first"));
            continue;
        }
        let name = el.attr_or_empty("name");
        let id = reg.add_class(if name.is_empty() { "Unnamed" } else { name });
        if let Some(c) = reg.get_mut(id) {
            c.address_expr = el.attr_or_empty("address").to_string();
        }
        by_uuid.insert(uuid, id);
        pending.push((el, id));
    }
    report.classes = pending.len();

    for (el, id) in pending {
        let nodes: Vec<Node> = el
            .children_named("node")
            .filter_map(|n| import_node(n, &by_uuid, &enums, ptr, &mut report))
            .collect();
        report.nodes += nodes.len();
        let _ = reg.push_nodes(id, nodes);
    }
    reg.touch();
    Ok((reg, report))
}

/// Read the project-level `<enums>` table, keyed by name.
fn read_enums(root: &Element, report: &mut Report) -> HashMap<String, EnumDesc> {
    let Some(enums_el) = root.child("enums") else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for e in enums_el.children_named("enum") {
        let name = e.attr_or_empty("name").to_string();
        // ReClass.NET writes the size as a .NET enum *name*, not a number.
        let width = match e.attr_or_empty("size") {
            "OneByte" => IntWidth::W8,
            "TwoBytes" => IntWidth::W16,
            "EightBytes" => IntWidth::W64,
            "FourBytes" | "" => IntWidth::W32,
            other => other.parse::<u32>().map_or(IntWidth::W32, width_from_bytes),
        };
        if e.attr_or_empty("flags") == "true" {
            report.note(format!(
                "enum '{name}' is a flags enum; imported as plain named values"
            ));
        }
        let variants = e
            .children_named("item")
            .map(|i| EnumVariant {
                value: i.attr_num("value").unwrap_or(0),
                name: i.attr_or_empty("name").to_string(),
            })
            .collect();
        out.insert(name, EnumDesc { width, variants });
    }
    out
}

/// Convert one `<node>` element, or `None` when it cannot be represented.
fn import_node(
    el: &Element,
    by_uuid: &HashMap<&str, ClassId>,
    enums: &HashMap<String, EnumDesc>,
    ptr: PtrWidth,
    report: &mut Report,
) -> Option<Node> {
    let name = el.attr_or_empty("name").to_string();
    let kind = import_kind(el, by_uuid, enums, ptr, report, &name)?;
    Some(Node {
        name,
        comment: el.attr_or_empty("comment").to_string(),
        kind,
    })
}

#[allow(clippy::too_many_lines)] // one arm per ReClass.NET node type; a table would hide the special cases
fn import_kind(
    el: &Element,
    by_uuid: &HashMap<&str, ClassId>,
    enums: &HashMap<String, EnumDesc>,
    ptr: PtrWidth,
    report: &mut Report,
    field: &str,
) -> Option<NodeKind> {
    use IntWidth::{W8, W16, W32, W64};
    let ty = el.attr_or_empty("type");
    let ptr_width = width_from_bytes(ptr.bytes() as u32);

    let kind = match ty {
        "Hex8Node" => NodeKind::Hex(W8),
        "Hex16Node" => NodeKind::Hex(W16),
        "Hex32Node" => NodeKind::Hex(W32),
        "Hex64Node" => NodeKind::Hex(W64),
        "Int8Node" => NodeKind::Int(W8),
        "Int16Node" => NodeKind::Int(W16),
        "Int32Node" => NodeKind::Int(W32),
        "Int64Node" => NodeKind::Int(W64),
        "NIntNode" => NodeKind::Int(ptr_width),
        "UInt8Node" => NodeKind::UInt(W8),
        "UInt16Node" => NodeKind::UInt(W16),
        "UInt32Node" => NodeKind::UInt(W32),
        "UInt64Node" => NodeKind::UInt(W64),
        "NUIntNode" => NodeKind::UInt(ptr_width),
        "FloatNode" => NodeKind::Float32,
        "DoubleNode" => NodeKind::Float64,
        "BoolNode" => NodeKind::Bool,
        "Vector2Node" => NodeKind::Vec2,
        "Vector3Node" => NodeKind::Vec3,
        "Vector4Node" => NodeKind::Vec4,
        // No matrix kind here, but a matrix is exactly rows of vectors and the
        // byte layout is identical.
        "Matrix3x3Node" => array_of(NodeKind::Vec3, 3),
        "Matrix3x4Node" => array_of(NodeKind::Vec4, 3),
        "Matrix4x4Node" => array_of(NodeKind::Vec4, 4),
        "BitFieldNode" => NodeKind::Bitfield(width_for_bits(el.attr_num("bits").unwrap_or(32))),
        "EnumNode" => {
            let name = el.attr_or_empty("reference");
            match enums.get(name) {
                Some(e) => NodeKind::Enum {
                    width: e.width,
                    variants: e.variants.clone(),
                },
                None => {
                    report.note(format!(
                        "field '{field}' references unknown enum '{name}'; imported as UInt32"
                    ));
                    NodeKind::UInt(W32)
                }
            }
        }
        "Utf8TextNode" => text(TextEncoding::Utf8, el),
        "Utf16TextNode" => text(TextEncoding::Utf16, el),
        "Utf32TextNode" => {
            let len: usize = el.attr_num("length").unwrap_or(0);
            report.note(format!(
                "field '{field}' is UTF-32 text; imported as {} raw bytes",
                len * 4
            ));
            NodeKind::Unknown(len.saturating_mul(4))
        }
        "Utf8TextPtrNode" => NodeKind::PtrText {
            encoding: TextEncoding::Utf8,
            max: 64,
        },
        "Utf16TextPtrNode" => NodeKind::PtrText {
            encoding: TextEncoding::Utf16,
            max: 64,
        },
        "Utf32TextPtrNode" => {
            report.note(format!(
                "field '{field}' is a UTF-32 string pointer; imported as a plain pointer"
            ));
            NodeKind::Pointer
        }
        "FunctionPtrNode" | "FunctionNode" => NodeKind::FunctionPtr,
        "ClassInstanceNode" => NodeKind::ClassInstance {
            class_id: reference(el, by_uuid, report, field)?,
        },
        // Legacy aliases ReClass.NET still reads.
        "ClassPtrNode" => NodeKind::ClassPtr {
            class_id: reference(el, by_uuid, report, field)?,
        },
        "ClassInstanceArrayNode" => array_of(
            NodeKind::ClassInstance {
                class_id: reference(el, by_uuid, report, field)?,
            },
            el.attr_num("count").unwrap_or(0),
        ),
        "ClassPtrArrayNode" => array_of(
            NodeKind::ClassPtr {
                class_id: reference(el, by_uuid, report, field)?,
            },
            el.attr_num("count").unwrap_or(0),
        ),
        "PointerNode" => {
            // A pointer is a wrapper; only a pointer-to-class survives as one.
            match el.children.first().map(|inner| inner.attr_or_empty("type")) {
                Some("ClassInstanceNode") => NodeKind::ClassPtr {
                    class_id: reference(&el.children[0], by_uuid, report, field)?,
                },
                Some(other) => {
                    report.note(format!(
                        "field '{field}' points to {other}; imported as a plain pointer"
                    ));
                    NodeKind::Pointer
                }
                None => NodeKind::Pointer,
            }
        }
        "ArrayNode" => {
            let count: usize = el.attr_num("count").unwrap_or(0);
            let inner = el.children.first()?;
            let elem = import_kind(inner, by_uuid, enums, ptr, report, field)?;
            array_of(elem, count)
        }
        "VirtualMethodTableNode" => {
            let methods = el.children_named("method").count();
            report.note(format!(
                "field '{field}' is a vtable; imported as {methods} function pointers"
            ));
            array_of(NodeKind::FunctionPtr, methods)
        }
        "UnionNode" => {
            // A union's size is its largest member; this model has no overlap,
            // so the space is preserved as raw bytes and the shape is lost.
            let size = el
                .children
                .iter()
                .filter_map(|c| import_kind(c, by_uuid, enums, ptr, report, field))
                .map(|k| k.fixed_size(ptr.bytes()))
                .max()
                .unwrap_or(0);
            report.note(format!(
                "field '{field}' is a union; imported as {size} raw bytes"
            ));
            NodeKind::Unknown(size)
        }
        other => {
            report.note(format!(
                "field '{field}' has unsupported type '{other}'; skipped"
            ));
            return None;
        }
    };
    Some(kind)
}

fn array_of(element: NodeKind, count: usize) -> NodeKind {
    NodeKind::Array {
        element: Box::new(element),
        count,
    }
}

fn text(encoding: TextEncoding, el: &Element) -> NodeKind {
    NodeKind::Text {
        encoding,
        len: el.attr_num("length").unwrap_or(0),
    }
}

fn reference(
    el: &Element,
    by_uuid: &HashMap<&str, ClassId>,
    report: &mut Report,
    field: &str,
) -> Option<ClassId> {
    let uuid = el.attr_or_empty("reference");
    match by_uuid.get(uuid) {
        Some(id) => Some(*id),
        None => {
            report.note(format!(
                "field '{field}' references unknown class {uuid}; skipped"
            ));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// Serialize a registry as a `.rcnet` archive.
pub fn export(reg: &ClassRegistry) -> Result<(Vec<u8>, Report), RcnetError> {
    let (xml, report) = export_xml(reg);
    Ok((zip::write_entry(DATA_ENTRY, xml.as_bytes())?, report))
}

/// Serialize a registry as a bare `Data.xml` document.
pub fn export_xml(reg: &ClassRegistry) -> (String, Report) {
    let mut report = Report::default();
    let mut w = Writer::new();
    w.start(
        "reclass",
        &[
            ("version", FILE_VERSION.to_string()),
            (
                "type",
                match reg.ptr_width() {
                    PtrWidth::P32 => "x86".to_string(),
                    PtrWidth::P64 => "x64".to_string(),
                },
            ),
        ],
    );

    // Enum variants live on the node here but in a project table there, so one
    // synthetic enum per enum-typed field, named after the field that owns it.
    w.start("enums", &[]);
    let mut enum_names: HashMap<(ClassId, usize), String> = HashMap::new();
    let mut used: Vec<String> = Vec::new();
    for class in reg.iter() {
        for (i, node) in class.nodes.iter().enumerate() {
            let NodeKind::Enum { width, variants } = &node.kind else {
                continue;
            };
            let base = if node.name.is_empty() {
                format!("{}_enum{i}", class.name)
            } else {
                format!("{}_{}", class.name, node.name)
            };
            // Two fields can share a name across classes; the table is keyed by
            // name, so a collision would silently merge two enums.
            let mut name = base.clone();
            let mut n = 2;
            while used.contains(&name) {
                name = format!("{base}_{n}");
                n += 1;
            }
            used.push(name.clone());
            w.start(
                "enum",
                &[
                    ("name", name.clone()),
                    ("size", size_name(*width).to_string()),
                    ("flags", "false".to_string()),
                ],
            );
            for v in variants {
                w.leaf(
                    "item",
                    &[("name", v.name.clone()), ("value", v.value.to_string())],
                );
            }
            w.end();
            enum_names.insert((class.id, i), name);
        }
    }
    w.end();

    w.start("classes", &[]);
    for class in reg.iter() {
        report.classes += 1;
        w.start(
            "class",
            &[
                ("uuid", uuid_for(class.id)),
                ("name", class.name.clone()),
                ("comment", String::new()),
                ("address", class.address_expr.clone()),
            ],
        );
        for (i, node) in class.nodes.iter().enumerate() {
            report.nodes += 1;
            export_node(&mut w, class, i, node, &enum_names, &mut report);
        }
        w.end();
    }
    w.end();
    (w.finish(), report)
}

/// A stable GUID-shaped identifier for a class id.
///
/// ReClass.NET parses either a canonical GUID or 24 base64 characters, and
/// treats it as opaque. Deriving it from the id keeps export deterministic —
/// re-exporting an unchanged project produces byte-identical output.
fn uuid_for(id: ClassId) -> String {
    format!("00000000-0000-0000-0000-{id:012x}")
}

/// Emit one `<node>` for a class field.
fn export_node(
    w: &mut Writer,
    class: &Class,
    idx: usize,
    node: &Node,
    enum_names: &HashMap<(ClassId, usize), String>,
    report: &mut Report,
) {
    let field = if node.name.is_empty() {
        format!("{}[{idx}]", class.name)
    } else {
        node.name.clone()
    };
    emit(
        w,
        &node.kind,
        &node.name,
        &node.comment,
        class,
        idx,
        enum_names,
        report,
        &field,
    );
}

/// Emit a `<node>` for `kind`, recursing into its inner element when it has one.
#[allow(clippy::too_many_arguments)] // every one is threaded context, not config
fn emit(
    w: &mut Writer,
    kind: &NodeKind,
    name: &str,
    comment: &str,
    class: &Class,
    idx: usize,
    enum_names: &HashMap<(ClassId, usize), String>,
    report: &mut Report,
    field: &str,
) {
    let mut attrs: Vec<(&str, String)> = vec![
        ("name", name.to_string()),
        ("comment", comment.to_string()),
        ("hidden", "false".to_string()),
    ];
    let Some((ty, inner)) = export_kind(kind, &mut attrs, enum_names, class, idx, report, field)
    else {
        return;
    };
    attrs.insert(0, ("type", ty.to_string()));
    match inner {
        // A wrapper node's element is a child, not an attribute: ReClass.NET
        // reads the first child element as the wrapped type.
        Some(inner) => {
            w.start("node", &attrs);
            emit(w, &inner, "", "", class, idx, enum_names, report, field);
            w.end();
        }
        None => w.leaf("node", &attrs),
    }
}

/// The ReClass.NET type name for a kind, plus the inner node it wraps.
///
/// Pushes any extra attributes the type needs. `None` means the kind has no
/// representation and the field is dropped.
#[allow(clippy::too_many_arguments)] // same: threaded context
fn export_kind(
    kind: &NodeKind,
    attrs: &mut Vec<(&'static str, String)>,
    enum_names: &HashMap<(ClassId, usize), String>,
    class: &Class,
    idx: usize,
    report: &mut Report,
    field: &str,
) -> Option<(&'static str, Option<NodeKind>)> {
    use IntWidth::{W8, W16, W32, W64};
    let plain = |t: &'static str| Some((t, None));
    match kind {
        NodeKind::Hex(W8) => plain("Hex8Node"),
        NodeKind::Hex(W16) => plain("Hex16Node"),
        NodeKind::Hex(W32) => plain("Hex32Node"),
        NodeKind::Hex(W64) => plain("Hex64Node"),
        NodeKind::Int(W8) => plain("Int8Node"),
        NodeKind::Int(W16) => plain("Int16Node"),
        NodeKind::Int(W32) => plain("Int32Node"),
        NodeKind::Int(W64) => plain("Int64Node"),
        NodeKind::UInt(W8) => plain("UInt8Node"),
        NodeKind::UInt(W16) => plain("UInt16Node"),
        NodeKind::UInt(W32) => plain("UInt32Node"),
        NodeKind::UInt(W64) => plain("UInt64Node"),
        NodeKind::Float32 => plain("FloatNode"),
        NodeKind::Float64 => plain("DoubleNode"),
        NodeKind::Bool => plain("BoolNode"),
        NodeKind::Vec2 => plain("Vector2Node"),
        NodeKind::Vec3 => plain("Vector3Node"),
        NodeKind::Vec4 => plain("Vector4Node"),
        NodeKind::Bitfield(w) => {
            attrs.push(("bits", w.bits().to_string()));
            plain("BitFieldNode")
        }
        NodeKind::Enum { width, .. } => match enum_names.get(&(class.id, idx)) {
            Some(name) => {
                attrs.push(("reference", name.clone()));
                plain("EnumNode")
            }
            None => {
                // Only reachable for an enum nested inside an array, whose
                // entry the table pass never created.
                report.note(format!(
                    "field '{field}' is an enum inside an array; exported as a plain integer"
                ));
                plain(match width {
                    W8 => "UInt8Node",
                    W16 => "UInt16Node",
                    W32 => "UInt32Node",
                    W64 => "UInt64Node",
                })
            }
        },
        NodeKind::Text { encoding, len } => {
            attrs.push(("length", len.to_string()));
            plain(match encoding {
                TextEncoding::Utf8 => "Utf8TextNode",
                TextEncoding::Utf16 => "Utf16TextNode",
            })
        }
        NodeKind::PtrText { encoding, .. } => plain(match encoding {
            TextEncoding::Utf8 => "Utf8TextPtrNode",
            TextEncoding::Utf16 => "Utf16TextPtrNode",
        }),
        NodeKind::Pointer => plain("PointerNode"),
        NodeKind::FunctionPtr => plain("FunctionPtrNode"),
        NodeKind::ClassInstance { class_id } => {
            attrs.push(("reference", uuid_for(*class_id)));
            plain("ClassInstanceNode")
        }
        NodeKind::ClassPtr { class_id } => {
            attrs.push(("reference", uuid_for(*class_id)));
            plain("ClassPtrNode")
        }
        NodeKind::Array { element, count } => {
            attrs.push(("count", count.to_string()));
            Some(("ArrayNode", Some((**element).clone())))
        }
        // ReClass.NET has no padding or unknown-block kind. A byte array is the
        // one representation with the identical layout, which is the property
        // that matters — the bytes stay where they are.
        NodeKind::Padding(n) | NodeKind::Unknown(n) => {
            report.note(format!(
                "field '{field}' is a {n}-byte raw block; exported as a byte array"
            ));
            attrs.push(("count", n.to_string()));
            Some(("ArrayNode", Some(NodeKind::Hex(W8))))
        }
    }
}

#[cfg(test)]
mod tests;
