//! Typed nodes: the fields of a class.
//!
//! A [`Node`] is a `name`/`comment` plus a [`NodeKind`]. The kind knows its byte
//! [`size`](NodeKind::size), how to [`format`](NodeKind::format) a byte slice
//! into a display value, and how to [`parse_edit`](NodeKind::parse_edit) user
//! input back into bytes for write-back.

use crate::class::{ClassId, ClassRegistry};
use std::fmt::Write as _;

/// Width of an integer / hex node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum IntWidth {
    /// 1 byte.
    W8,
    /// 2 bytes.
    W16,
    /// 4 bytes.
    W32,
    /// 8 bytes.
    W64,
}

impl IntWidth {
    /// Width in bytes.
    #[inline]
    #[must_use]
    pub fn bytes(self) -> usize {
        match self {
            IntWidth::W8 => 1,
            IntWidth::W16 => 2,
            IntWidth::W32 => 4,
            IntWidth::W64 => 8,
        }
    }
    /// Number of bits.
    #[inline]
    #[must_use]
    pub fn bits(self) -> u32 {
        self.bytes() as u32 * 8
    }
}

/// Text encoding for a [`NodeKind::Text`] / [`NodeKind::PtrText`] node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TextEncoding {
    /// One byte per code unit.
    Utf8,
    /// Two (little-endian) bytes per code unit.
    Utf16,
}

impl TextEncoding {
    /// Bytes occupied by `units` code units.
    #[inline]
    #[must_use]
    pub fn bytes_for(self, units: usize) -> usize {
        match self {
            TextEncoding::Utf8 => units,
            TextEncoding::Utf16 => units.saturating_mul(2),
        }
    }
}

/// One named value of a [`NodeKind::Enum`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EnumVariant {
    /// Numeric value as stored in memory (sign-extended to the node's width).
    pub value: i64,
    /// Display / codegen name.
    pub name: String,
}

/// The type of a node — what determines its size and rendering.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NodeKind {
    /// Raw bytes shown as a hex word.
    Hex(IntWidth),
    /// Signed integer.
    Int(IntWidth),
    /// Unsigned integer.
    UInt(IntWidth),
    /// 32-bit float.
    Float32,
    /// 64-bit float.
    Float64,
    /// Boolean (one byte; nonzero is true).
    Bool,
    /// Integer whose value is looked up in a table of named variants.
    ///
    /// Not emitted as a real `enum` by codegen: a foreign process can hold any
    /// bit pattern here, and a Rust/C++ enum with an out-of-range discriminant
    /// is undefined behaviour to materialize. Codegen emits the underlying
    /// integer and lists the variants as a comment.
    Enum {
        /// Storage width.
        width: IntWidth,
        /// Known values, searched in order; the first match wins.
        variants: Vec<EnumVariant>,
    },
    /// Integer displayed as grouped binary, MSB first.
    ///
    /// Individual bits are unnamed on purpose — naming them would duplicate
    /// the node's comment field for no layout benefit.
    Bitfield(IntWidth),
    /// 2 × f32.
    Vec2,
    /// 3 × f32.
    Vec3,
    /// 4 × f32.
    Vec4,
    /// Inline string of `len` code units.
    Text {
        /// Code-unit encoding.
        encoding: TextEncoding,
        /// Number of code units (chars for UTF-8, u16s for UTF-16).
        len: usize,
    },
    /// Generic 8-byte pointer; the engine can annotate its target.
    Pointer,
    /// Pointer to a NUL-terminated string, read through by the engine.
    ///
    /// The node itself occupies one pointer; `max` bounds how many code units
    /// are read at the target so a garbage pointer cannot request a huge read.
    PtrText {
        /// Code-unit encoding at the target.
        encoding: TextEncoding,
        /// Maximum code units read at the target.
        max: usize,
    },
    /// `count` repetitions of `element`, laid out contiguously.
    Array {
        /// Element type.
        element: Box<NodeKind>,
        /// Repetition count.
        count: usize,
    },
    /// Another class embedded inline (recurses into the registry).
    ClassInstance {
        /// Target class id.
        class_id: ClassId,
    },
    /// 8-byte pointer to another class (a read boundary, not inline).
    ClassPtr {
        /// Target class id.
        class_id: ClassId,
    },
    /// 8-byte function pointer; the engine can resolve a symbol.
    FunctionPtr,
    /// `n` bytes of explicit padding.
    Padding(usize),
    /// `n` bytes of not-yet-classified memory.
    Unknown(usize),
}

/// A single field in a class.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Node {
    /// Field name (display + codegen identifier).
    pub name: String,
    /// Free-form comment.
    pub comment: String,
    /// The field's type.
    pub kind: NodeKind,
}

impl Node {
    /// A node with a name and kind and no comment.
    pub fn new(name: impl Into<String>, kind: NodeKind) -> Self {
        Node {
            name: name.into(),
            comment: String::new(),
            kind,
        }
    }

    /// Byte size of this node (recurses through the registry).
    #[inline]
    pub fn size(&self, reg: &ClassRegistry) -> usize {
        self.kind.size(reg)
    }
}

/// Address-info resolver: maps an address to a short human label (module+off,
/// region, or symbol). Implemented by the app over live `regions()`.
pub trait AddrInfo {
    /// A short label describing what lives at `addr`, if known.
    fn describe(&self, addr: u64) -> Option<String>;
}

/// Context passed to [`NodeKind::format`].
pub struct FmtCtx<'a> {
    /// Registry, for class-name lookups.
    pub registry: &'a ClassRegistry,
    /// Address of the node being formatted (for pointer display).
    pub node_addr: u64,
    /// Optional resolver to annotate pointer targets.
    pub info: Option<&'a dyn AddrInfo>,
}

impl<'a> FmtCtx<'a> {
    /// A bare context with no address resolver.
    pub fn new(registry: &'a ClassRegistry) -> Self {
        FmtCtx {
            registry,
            node_addr: 0,
            info: None,
        }
    }
}

/// Error from [`NodeKind::parse_edit`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditErr {
    /// This node kind cannot be edited inline (aggregate / structural).
    #[error("this node type is not editable")]
    NotEditable,
    /// The input could not be parsed for this type.
    #[error("could not parse '{0}' for this type")]
    Parse(String),
    /// The parsed value does not fit the node's width.
    #[error("value out of range for this type")]
    OutOfRange,
    /// A vector type needs exactly `expected` components.
    #[error("expected {expected} components, got {got}")]
    WrongArity {
        /// Required component count.
        expected: usize,
        /// Supplied component count.
        got: usize,
    },
}

// ---------------------------------------------------------------------------
// little-endian helpers
// ---------------------------------------------------------------------------

fn le_unsigned(bytes: &[u8]) -> u64 {
    let mut v = 0u64;
    for (i, &b) in bytes.iter().take(8).enumerate() {
        v |= u64::from(b) << (i * 8);
    }
    v
}

fn le_signed(bytes: &[u8], width: IntWidth) -> i64 {
    let u = le_unsigned(&bytes[..width.bytes().min(bytes.len())]);
    let bits = width.bits();
    if bits == 64 {
        u as i64
    } else {
        // sign-extend from `bits`
        let shift = 64 - bits;
        ((u << shift) as i64) >> shift
    }
}

fn read_f32(bytes: &[u8]) -> f32 {
    let mut b = [0u8; 4];
    let n = bytes.len().min(4);
    b[..n].copy_from_slice(&bytes[..n]);
    f32::from_le_bytes(b)
}

/// Read an f32 at byte offset `off`, tolerating a short/absent slice (missing
/// bytes read as zero) so a truncated read never panics.
fn read_f32_at(bytes: &[u8], off: usize) -> f32 {
    read_f32(bytes.get(off..).unwrap_or(&[]))
}

fn read_f64(bytes: &[u8]) -> f64 {
    let mut b = [0u8; 8];
    let n = bytes.len().min(8);
    b[..n].copy_from_slice(&bytes[..n]);
    f64::from_le_bytes(b)
}

fn int_to_le(value: i128, width: IntWidth, signed: bool) -> Result<Vec<u8>, EditErr> {
    let bytes = width.bytes();
    if signed {
        let bits = width.bits();
        let (min, max) = if bits == 64 {
            (i128::from(i64::MIN), i128::from(i64::MAX))
        } else {
            let max = (1i128 << (bits - 1)) - 1;
            (-(1i128 << (bits - 1)), max)
        };
        if value < min || value > max {
            return Err(EditErr::OutOfRange);
        }
    } else {
        let bits = width.bits();
        let max = if bits == 64 {
            i128::from(u64::MAX)
        } else {
            (1i128 << bits) - 1
        };
        if value < 0 || value > max {
            return Err(EditErr::OutOfRange);
        }
    }
    let le = value.to_le_bytes();
    Ok(le[..bytes].to_vec())
}

fn parse_int(input: &str) -> Result<i128, EditErr> {
    let s = input.trim();
    let parse_err = || EditErr::Parse(input.to_string());
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i128::from_str_radix(hex, 16).map_err(|_| parse_err());
    }
    if let Some(hex) = s.strip_prefix("-0x").or_else(|| s.strip_prefix("-0X")) {
        return i128::from_str_radix(hex, 16)
            .map(|v| -v)
            .map_err(|_| parse_err());
    }
    s.parse::<i128>().map_err(|_| parse_err())
}

fn parse_addr(input: &str) -> Result<u64, EditErr> {
    let s = input.trim();
    let parse_err = || EditErr::Parse(input.to_string());
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|_| parse_err())
    } else {
        // bare numbers are interpreted as hex for addresses (ReClass habit)
        u64::from_str_radix(s, 16)
            .or_else(|_| s.parse::<u64>())
            .map_err(|_| parse_err())
    }
}

impl NodeKind {
    /// Byte size of this kind. Recurses for `ClassInstance`/`Array`; cycle-safe
    /// because [`ClassRegistry::size_of`] guards against re-entrancy.
    pub fn size(&self, reg: &ClassRegistry) -> usize {
        match self {
            NodeKind::ClassInstance { class_id } => reg.size_of(*class_id),
            NodeKind::Array { element, count } => element.size(reg).saturating_mul(*count),
            other => other.fixed_size(reg.pointer_bytes()),
        }
    }

    /// Size of every non-recursive kind; `0` for `ClassInstance`/`Array` (use
    /// [`size`](Self::size) with a registry for those).
    ///
    /// `ptr_bytes` is the target's pointer width (see
    /// [`ClassRegistry::pointer_bytes`]). It is a parameter rather than a
    /// constant because a 32-bit target lays pointers out in 4 bytes, which
    /// shifts the offset of every field after one.
    #[must_use]
    pub fn fixed_size(&self, ptr_bytes: usize) -> usize {
        match self {
            NodeKind::Hex(w) | NodeKind::Int(w) | NodeKind::UInt(w) => w.bytes(),
            NodeKind::Float32 => 4,
            NodeKind::Float64 => 8,
            NodeKind::Bool => 1,
            NodeKind::Vec2 => 8,
            NodeKind::Vec3 => 12,
            NodeKind::Vec4 => 16,
            NodeKind::Enum { width, .. } | NodeKind::Bitfield(width) => width.bytes(),
            NodeKind::Text { encoding, len } => encoding.bytes_for(*len),
            NodeKind::Pointer
            | NodeKind::PtrText { .. }
            | NodeKind::ClassPtr { .. }
            | NodeKind::FunctionPtr => ptr_bytes,
            NodeKind::Padding(n) | NodeKind::Unknown(n) => *n,
            // recursive kinds have no fixed size
            NodeKind::ClassInstance { .. } | NodeKind::Array { .. } => 0,
        }
    }

    /// Short type label for the "type" column / codegen.
    pub fn label(&self, reg: &ClassRegistry) -> String {
        match self {
            NodeKind::Hex(w) => format!("Hex{}", w.bits()),
            NodeKind::Int(w) => format!("Int{}", w.bits()),
            NodeKind::UInt(w) => format!("UInt{}", w.bits()),
            NodeKind::Float32 => "Float".into(),
            NodeKind::Float64 => "Double".into(),
            NodeKind::Bool => "Bool".into(),
            NodeKind::Vec2 => "Vec2".into(),
            NodeKind::Vec3 => "Vec3".into(),
            NodeKind::Vec4 => "Vec4".into(),
            NodeKind::Enum { width, .. } => format!("Enum{}", width.bits()),
            NodeKind::Bitfield(w) => format!("Bits{}", w.bits()),
            NodeKind::Text { encoding, len } => match encoding {
                TextEncoding::Utf8 => format!("Text[{len}]"),
                TextEncoding::Utf16 => format!("WText[{len}]"),
            },
            NodeKind::Pointer => "Ptr".into(),
            NodeKind::PtrText { encoding, max } => match encoding {
                TextEncoding::Utf8 => format!("Text*[{max}]"),
                TextEncoding::Utf16 => format!("WText*[{max}]"),
            },
            NodeKind::Array { element, count } => format!("{}[{count}]", element.label(reg)),
            NodeKind::ClassInstance { class_id } => reg.name_of(*class_id).map_or_else(
                || format!("class#{class_id}"),
                std::string::ToString::to_string,
            ),
            NodeKind::ClassPtr { class_id } => reg
                .name_of(*class_id)
                .map_or_else(|| format!("class#{class_id}*"), |n| format!("{n}*")),
            NodeKind::FunctionPtr => "FnPtr".into(),
            NodeKind::Padding(n) => format!("Padding[{n}]"),
            NodeKind::Unknown(n) => format!("Unknown[{n}]"),
        }
    }

    /// Whether this kind holds a single editable scalar/value (vs an aggregate).
    #[must_use]
    pub fn is_editable(&self) -> bool {
        !matches!(
            self,
            NodeKind::Array { .. }
                | NodeKind::ClassInstance { .. }
                | NodeKind::Padding(_)
                | NodeKind::Unknown(_)
        )
    }

    /// Format a byte slice into a one-line display value. For aggregate kinds
    /// the result is a summary; per-element rows are produced by the engine.
    #[must_use]
    pub fn format(&self, bytes: &[u8], ctx: &FmtCtx<'_>) -> String {
        match self {
            NodeKind::Hex(w) => {
                let v = le_unsigned(&bytes[..w.bytes().min(bytes.len())]);
                format!("0x{:0width$X}", v, width = w.bytes() * 2)
            }
            NodeKind::Int(w) => le_signed(bytes, *w).to_string(),
            NodeKind::UInt(w) => le_unsigned(&bytes[..w.bytes().min(bytes.len())]).to_string(),
            NodeKind::Float32 => fmt_float(f64::from(read_f32(bytes))),
            NodeKind::Float64 => fmt_float(read_f64(bytes)),
            NodeKind::Bool => if bytes.iter().any(|&b| b != 0) {
                "true"
            } else {
                "false"
            }
            .into(),
            NodeKind::Vec2 => {
                format!(
                    "({}, {})",
                    fmt_float(f64::from(read_f32_at(bytes, 0))),
                    fmt_float(f64::from(read_f32_at(bytes, 4)))
                )
            }
            NodeKind::Vec3 => format!(
                "({}, {}, {})",
                fmt_float(f64::from(read_f32_at(bytes, 0))),
                fmt_float(f64::from(read_f32_at(bytes, 4))),
                fmt_float(f64::from(read_f32_at(bytes, 8))),
            ),
            NodeKind::Vec4 => format!(
                "({}, {}, {}, {})",
                fmt_float(f64::from(read_f32_at(bytes, 0))),
                fmt_float(f64::from(read_f32_at(bytes, 4))),
                fmt_float(f64::from(read_f32_at(bytes, 8))),
                fmt_float(f64::from(read_f32_at(bytes, 12))),
            ),
            NodeKind::Enum { width, variants } => {
                let v = le_signed(bytes, *width);
                match variants.iter().find(|e| e.value == v) {
                    Some(e) => format!("{} ({v})", e.name),
                    None => format!("{v}"),
                }
            }
            NodeKind::Bitfield(w) => format_bits(bytes, *w),
            NodeKind::Text { encoding, .. } => format_text(bytes, *encoding),
            NodeKind::Pointer | NodeKind::FunctionPtr | NodeKind::PtrText { .. } => {
                format_ptr(read_ptr(bytes, ctx.registry.pointer_bytes()), ctx)
            }
            NodeKind::ClassPtr { class_id } => {
                let target = read_ptr(bytes, ctx.registry.pointer_bytes());
                let name = ctx.registry.name_of(*class_id).map_or_else(
                    || format!("class#{class_id}"),
                    std::string::ToString::to_string,
                );
                format!("-> {} {}", format_ptr(target, ctx), name)
            }
            NodeKind::ClassInstance { class_id } => ctx
                .registry
                .name_of(*class_id)
                .map_or_else(|| format!("<class#{class_id}>"), |n| format!("<{n}>")),
            NodeKind::Array { element, count } => {
                format!("{}[{count}]", element.label(ctx.registry))
            }
            NodeKind::Padding(n) => format!("(padding {n})"),
            NodeKind::Unknown(_) => hex_dump(bytes, bytes.len()),
        }
    }

    /// Parse user input into the bytes to write back. Errors with
    /// [`EditErr::NotEditable`] for aggregate / structural kinds.
    ///
    /// `ptr_bytes` is the target's pointer width (see
    /// [`ClassRegistry::pointer_bytes`]); writing 8 bytes for a pointer on a
    /// 32-bit target would clobber the next field.
    pub fn parse_edit(&self, input: &str, ptr_bytes: usize) -> Result<Vec<u8>, EditErr> {
        match self {
            NodeKind::Hex(w) | NodeKind::UInt(w) => int_to_le(parse_int(input)?, *w, false),
            NodeKind::Int(w) => int_to_le(parse_int(input)?, *w, true),
            NodeKind::Float32 => input
                .trim()
                .parse::<f32>()
                .map(|f| f.to_le_bytes().to_vec())
                .map_err(|_| EditErr::Parse(input.to_string())),
            NodeKind::Float64 => input
                .trim()
                .parse::<f64>()
                .map(|f| f.to_le_bytes().to_vec())
                .map_err(|_| EditErr::Parse(input.to_string())),
            NodeKind::Bool => match input.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => Ok(vec![1]),
                "false" | "0" | "no" => Ok(vec![0]),
                _ => Err(EditErr::Parse(input.to_string())),
            },
            NodeKind::Vec2 => parse_vec(input, 2),
            NodeKind::Vec3 => parse_vec(input, 3),
            NodeKind::Vec4 => parse_vec(input, 4),
            NodeKind::Enum { width, variants } => {
                let t = input.trim();
                match variants.iter().find(|e| e.name.eq_ignore_ascii_case(t)) {
                    Some(e) => int_to_le(i128::from(e.value), *width, true),
                    // A name that is not a known variant still has to fail as a
                    // name, not silently write a nearby number.
                    None => int_to_le(parse_int(input)?, *width, true),
                }
            }
            NodeKind::Bitfield(w) => int_to_le(parse_bits(input)?, *w, false),
            NodeKind::Text { encoding, len } => Ok(encode_text(input, *encoding, *len)),
            NodeKind::Pointer
            | NodeKind::ClassPtr { .. }
            | NodeKind::FunctionPtr
            | NodeKind::PtrText { .. } => {
                let addr = parse_addr(input)?;
                let n = ptr_bytes.clamp(1, 8);
                if n < 8 && addr > (u64::MAX >> ((8 - n) * 8)) {
                    return Err(EditErr::OutOfRange);
                }
                Ok(addr.to_le_bytes()[..n].to_vec())
            }
            NodeKind::Array { .. }
            | NodeKind::ClassInstance { .. }
            | NodeKind::Padding(_)
            | NodeKind::Unknown(_) => Err(EditErr::NotEditable),
        }
    }
}
/// Read a little-endian pointer of `ptr_bytes` width, tolerating a short slice
/// (a truncated read yields the mapped prefix, zero-extended).
#[inline]
pub(crate) fn read_ptr(bytes: &[u8], ptr_bytes: usize) -> u64 {
    le_unsigned(&bytes[..ptr_bytes.clamp(1, 8).min(bytes.len())])
}

fn fmt_float(f: f64) -> String {
    if f == 0.0 {
        // normalize -0.0 to "0"
        "0".to_string()
    } else {
        // Display is the shortest round-trip representation (incl. inf/NaN).
        format!("{f}")
    }
}

fn format_ptr(target: u64, ctx: &FmtCtx<'_>) -> String {
    if target == 0 {
        return "NULL".into();
    }
    match ctx.info.and_then(|i| i.describe(target)) {
        Some(label) => format!("0x{target:X} ({label})"),
        None => format!("0x{target:X}"),
    }
}

/// Render a NUL-terminated string from `bytes`. Shared with the engine, which
/// formats [`NodeKind::PtrText`] from a separately-read target buffer.
pub(crate) fn format_text(bytes: &[u8], encoding: TextEncoding) -> String {
    match encoding {
        TextEncoding::Utf8 => {
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            format!("\"{}\"", String::from_utf8_lossy(&bytes[..end]))
        }
        TextEncoding::Utf16 => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            format!("\"{}\"", String::from_utf16_lossy(&units))
        }
    }
}

/// Render `width` bytes as binary, MSB first, in space-separated octets.
///
/// Missing bytes (a truncated read) render as zeros rather than shifting the
/// remaining bits into the wrong column.
fn format_bits(bytes: &[u8], width: IntWidth) -> String {
    let v = le_unsigned(&bytes[..width.bytes().min(bytes.len())]);
    let mut s = String::with_capacity(width.bytes() * 9);
    for byte in (0..width.bytes()).rev() {
        if !s.is_empty() {
            s.push(' ');
        }
        let _ = write!(s, "{:08b}", (v >> (byte * 8)) as u8);
    }
    s
}

/// Parse a bitfield edit: binary (`0b1010`, or bare digits with separators),
/// hex (`0x…`), or decimal.
///
/// Bare `10` is binary here, not decimal — the field is displayed as binary, so
/// echoing back what is on screen has to mean the same value.
fn parse_bits(input: &str) -> Result<i128, EditErr> {
    let t = input.trim();
    if t.starts_with("0x") || t.starts_with("0X") {
        return parse_int(t);
    }
    let digits: String = t
        .strip_prefix("0b")
        .or_else(|| t.strip_prefix("0B"))
        .unwrap_or(t)
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect();
    if !digits.is_empty() && digits.chars().all(|c| c == '0' || c == '1') {
        return i128::from_str_radix(&digits, 2).map_err(|_| EditErr::Parse(input.to_string()));
    }
    parse_int(t)
}

fn hex_dump(bytes: &[u8], max: usize) -> String {
    let mut s = String::with_capacity(max.min(bytes.len()) * 3);
    for (i, b) in bytes.iter().take(max).enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{b:02X}");
    }
    if bytes.len() > max {
        s.push_str(" …");
    }
    s
}

fn parse_vec(input: &str, n: usize) -> Result<Vec<u8>, EditErr> {
    let parts: Vec<&str> = input
        .split([',', ' ', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() != n {
        return Err(EditErr::WrongArity {
            expected: n,
            got: parts.len(),
        });
    }
    let mut out = Vec::with_capacity(n * 4);
    for p in parts {
        let f: f32 = p.parse().map_err(|_| EditErr::Parse(p.to_string()))?;
        out.extend_from_slice(&f.to_le_bytes());
    }
    Ok(out)
}

fn encode_text(input: &str, encoding: TextEncoding, len: usize) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => {
            let mut buf = vec![0u8; len];
            let src = input.as_bytes();
            let n = src.len().min(len);
            buf[..n].copy_from_slice(&src[..n]);
            // ensure NUL terminator if there is room
            if n < len {
                buf[n] = 0;
            }
            buf
        }
        TextEncoding::Utf16 => {
            let mut buf = vec![0u8; len * 2];
            for (i, u) in input.encode_utf16().take(len).enumerate() {
                buf[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
            }
            buf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::ClassRegistry;

    fn ctx(reg: &ClassRegistry) -> FmtCtx<'_> {
        FmtCtx::new(reg)
    }

    #[test]
    fn fixed_sizes() {
        assert_eq!(NodeKind::Hex(IntWidth::W32).fixed_size(8), 4);
        assert_eq!(NodeKind::Int(IntWidth::W64).fixed_size(8), 8);
        assert_eq!(NodeKind::Bool.fixed_size(8), 1);
        assert_eq!(NodeKind::Vec3.fixed_size(8), 12);
        assert_eq!(NodeKind::Pointer.fixed_size(8), 8);
        assert_eq!(
            NodeKind::Text {
                encoding: TextEncoding::Utf16,
                len: 8
            }
            .fixed_size(8),
            16
        );
        assert_eq!(NodeKind::Padding(5).fixed_size(8), 5);
    }

    #[test]
    fn format_scalars() {
        let reg = ClassRegistry::new();
        let c = ctx(&reg);
        assert_eq!(
            NodeKind::Hex(IntWidth::W32).format(&0x2Au32.to_le_bytes(), &c),
            "0x0000002A"
        );
        assert_eq!(
            NodeKind::Int(IntWidth::W32).format(&(-5i32).to_le_bytes(), &c),
            "-5"
        );
        assert_eq!(NodeKind::UInt(IntWidth::W8).format(&[200], &c), "200");
        assert_eq!(NodeKind::Int(IntWidth::W8).format(&[200], &c), "-56");
        assert_eq!(NodeKind::Bool.format(&[0], &c), "false");
        assert_eq!(NodeKind::Bool.format(&[7], &c), "true");
        assert_eq!(NodeKind::Float32.format(&1.5f32.to_le_bytes(), &c), "1.5");
    }

    #[test]
    fn format_vec_and_text() {
        let reg = ClassRegistry::new();
        let c = ctx(&reg);
        let mut b = Vec::new();
        b.extend_from_slice(&1.0f32.to_le_bytes());
        b.extend_from_slice(&2.0f32.to_le_bytes());
        b.extend_from_slice(&3.0f32.to_le_bytes());
        assert_eq!(NodeKind::Vec3.format(&b, &c), "(1, 2, 3)");

        let txt = NodeKind::Text {
            encoding: TextEncoding::Utf8,
            len: 8,
        };
        let mut tb = b"hi\0junk!".to_vec();
        tb.truncate(8);
        assert_eq!(txt.format(&tb, &c), "\"hi\"");
    }

    #[test]
    fn format_pointer_null_and_value() {
        let reg = ClassRegistry::new();
        let c = ctx(&reg);
        assert_eq!(NodeKind::Pointer.format(&0u64.to_le_bytes(), &c), "NULL");
        assert_eq!(
            NodeKind::Pointer.format(&0xDEADu64.to_le_bytes(), &c),
            "0xDEAD"
        );
    }

    #[test]
    fn parse_edit_ints() {
        assert_eq!(
            NodeKind::Int(IntWidth::W32).parse_edit("-5", 8).unwrap(),
            (-5i32).to_le_bytes()
        );
        assert_eq!(
            NodeKind::UInt(IntWidth::W16)
                .parse_edit("0x1234", 8)
                .unwrap(),
            0x1234u16.to_le_bytes()
        );
        assert_eq!(
            NodeKind::Hex(IntWidth::W8).parse_edit("255", 8).unwrap(),
            vec![255]
        );
        // out of range
        assert_eq!(
            NodeKind::UInt(IntWidth::W8).parse_edit("256", 8),
            Err(EditErr::OutOfRange)
        );
        assert_eq!(
            NodeKind::Int(IntWidth::W8).parse_edit("128", 8),
            Err(EditErr::OutOfRange)
        );
        assert_eq!(
            NodeKind::Int(IntWidth::W8).parse_edit("-128", 8).unwrap(),
            vec![0x80]
        );
    }

    #[test]
    fn parse_edit_float_bool_vec() {
        assert_eq!(
            NodeKind::Float32.parse_edit("1.5", 8).unwrap(),
            1.5f32.to_le_bytes()
        );
        assert_eq!(NodeKind::Bool.parse_edit("true", 8).unwrap(), vec![1]);
        assert_eq!(NodeKind::Bool.parse_edit("0", 8).unwrap(), vec![0]);
        let v = NodeKind::Vec2.parse_edit("1.0, 2.0", 8).unwrap();
        assert_eq!(&v[..4], &1.0f32.to_le_bytes());
        assert_eq!(&v[4..], &2.0f32.to_le_bytes());
        assert_eq!(
            NodeKind::Vec3.parse_edit("1,2", 8),
            Err(EditErr::WrongArity {
                expected: 3,
                got: 2
            })
        );
    }

    #[test]
    fn parse_edit_text_truncates_and_pads() {
        let txt = NodeKind::Text {
            encoding: TextEncoding::Utf8,
            len: 4,
        };
        assert_eq!(txt.parse_edit("hello", 8).unwrap(), b"hell".to_vec());
        assert_eq!(txt.parse_edit("hi", 8).unwrap(), b"hi\0\0".to_vec());
    }

    #[test]
    fn parse_edit_not_editable() {
        assert_eq!(
            NodeKind::Padding(4).parse_edit("x", 8),
            Err(EditErr::NotEditable)
        );
        assert_eq!(
            NodeKind::ClassInstance { class_id: 1 }.parse_edit("x", 8),
            Err(EditErr::NotEditable)
        );
    }

    #[test]
    fn pointer_roundtrip_edit() {
        let bytes = NodeKind::Pointer.parse_edit("0x7fff1234", 8).unwrap();
        assert_eq!(u64::from_le_bytes(bytes.try_into().unwrap()), 0x7fff_1234);
    }

    #[test]
    fn vec_format_tolerates_short_slice() {
        // A truncated read must render, not panic on `&bytes[4..]` etc.
        let reg = ClassRegistry::new();
        let ctx = FmtCtx::new(&reg);
        for kind in [NodeKind::Vec2, NodeKind::Vec3, NodeKind::Vec4] {
            for len in 0..=kind.fixed_size(8) {
                let _ = kind.format(&vec![0u8; len], &ctx);
            }
        }
    }

    fn hp_enum() -> NodeKind {
        NodeKind::Enum {
            width: IntWidth::W32,
            variants: vec![
                EnumVariant {
                    value: 0,
                    name: "Idle".into(),
                },
                EnumVariant {
                    value: 2,
                    name: "Dead".into(),
                },
                EnumVariant {
                    value: -1,
                    name: "Invalid".into(),
                },
            ],
        }
    }

    #[test]
    fn enum_formats_known_and_unknown_values() {
        let reg = ClassRegistry::new();
        let c = ctx(&reg);
        let k = hp_enum();
        assert_eq!(k.fixed_size(8), 4);
        assert_eq!(k.format(&0i32.to_le_bytes(), &c), "Idle (0)");
        assert_eq!(k.format(&2i32.to_le_bytes(), &c), "Dead (2)");
        // negative variants match through sign extension, not raw bits
        assert_eq!(k.format(&(-1i32).to_le_bytes(), &c), "Invalid (-1)");
        // an unnamed value must still show its number, not a blank cell
        assert_eq!(k.format(&7i32.to_le_bytes(), &c), "7");
    }

    #[test]
    fn enum_parses_variant_names_case_insensitively_and_numbers() {
        let k = hp_enum();
        assert_eq!(k.parse_edit("Dead", 8).unwrap(), 2i32.to_le_bytes());
        assert_eq!(k.parse_edit("  dead ", 8).unwrap(), 2i32.to_le_bytes());
        assert_eq!(k.parse_edit("Invalid", 8).unwrap(), (-1i32).to_le_bytes());
        assert_eq!(k.parse_edit("7", 8).unwrap(), 7i32.to_le_bytes());
        assert_eq!(k.parse_edit("0x10", 8).unwrap(), 16i32.to_le_bytes());
        // a name that is not a variant is a parse error, not a silent zero
        assert!(matches!(k.parse_edit("Nope", 8), Err(EditErr::Parse(_))));
    }

    #[test]
    fn enum_with_no_variants_is_a_plain_integer() {
        let reg = ClassRegistry::new();
        let k = NodeKind::Enum {
            width: IntWidth::W8,
            variants: Vec::new(),
        };
        assert_eq!(k.format(&[200], &ctx(&reg)), "-56");
        assert_eq!(k.parse_edit("-56", 8).unwrap(), vec![200]);
    }

    #[test]
    fn bitfield_formats_msb_first_in_octets() {
        let reg = ClassRegistry::new();
        let c = ctx(&reg);
        assert_eq!(
            NodeKind::Bitfield(IntWidth::W8).format(&[0b0000_1010], &c),
            "00001010"
        );
        assert_eq!(
            NodeKind::Bitfield(IntWidth::W16).format(&0x0102u16.to_le_bytes(), &c),
            "00000001 00000010"
        );
        assert_eq!(NodeKind::Bitfield(IntWidth::W32).fixed_size(8), 4);
    }

    #[test]
    fn bitfield_short_read_renders_zeros_without_shifting() {
        let reg = ClassRegistry::new();
        let c = ctx(&reg);
        // Only the low byte is present; the high byte must read as 0 in its own
        // column rather than sliding the low byte left.
        assert_eq!(
            NodeKind::Bitfield(IntWidth::W16).format(&[0xFF], &c),
            "00000000 11111111"
        );
    }

    #[test]
    fn bitfield_parses_binary_hex_and_decimal() {
        let k = NodeKind::Bitfield(IntWidth::W8);
        // bare digits are binary, matching what the field displays
        assert_eq!(k.parse_edit("1010", 8).unwrap(), vec![0b1010]);
        assert_eq!(k.parse_edit("0b1111_0000", 8).unwrap(), vec![0xF0]);
        assert_eq!(k.parse_edit("0000 0011", 8).unwrap(), vec![3]);
        assert_eq!(k.parse_edit("0xF0", 8).unwrap(), vec![0xF0]);
        // a value with a digit outside {0,1} falls back to decimal
        assert_eq!(k.parse_edit("42", 8).unwrap(), vec![42]);
        assert_eq!(k.parse_edit("256", 8), Err(EditErr::OutOfRange));
    }

    #[test]
    fn bitfield_round_trips_through_its_own_display() {
        let reg = ClassRegistry::new();
        let c = ctx(&reg);
        let k = NodeKind::Bitfield(IntWidth::W32);
        for v in [0u32, 1, 0xDEAD_BEEF, u32::MAX] {
            let shown = k.format(&v.to_le_bytes(), &c);
            assert_eq!(k.parse_edit(&shown, 8).unwrap(), v.to_le_bytes(), "{shown}");
        }
    }

    #[test]
    fn ptr_text_is_one_pointer_wide_and_edits_as_an_address() {
        let reg = ClassRegistry::new();
        let k = NodeKind::PtrText {
            encoding: TextEncoding::Utf8,
            max: 64,
        };
        assert_eq!(k.fixed_size(8), 8);
        // the node holds the pointer, so an edit writes an address here — the
        // string lives at the target and is not editable through this field
        let bytes = k.parse_edit("0x7fff1234", 8).unwrap();
        assert_eq!(u64::from_le_bytes(bytes.try_into().unwrap()), 0x7fff_1234);
        assert_eq!(k.format(&0u64.to_le_bytes(), &ctx(&reg)), "NULL");
    }

    #[test]
    fn utf16_ptr_text_doubles_its_read_length() {
        assert_eq!(TextEncoding::Utf8.bytes_for(64), 64);
        assert_eq!(TextEncoding::Utf16.bytes_for(64), 128);
        // saturating: an absurd max must not wrap the read size to something small
        assert_eq!(TextEncoding::Utf16.bytes_for(usize::MAX), usize::MAX);
    }
}

#[cfg(test)]
mod ptr_width_tests {
    use super::*;
    use crate::class::{ClassRegistry, PtrWidth};

    #[test]
    fn a_32_bit_pointer_edit_writes_four_bytes() {
        // Writing 8 would clobber the two fields that follow on a 32-bit target.
        let bytes = NodeKind::Pointer.parse_edit("0x08048000", 4).unwrap();
        assert_eq!(bytes, 0x0804_8000u32.to_le_bytes());
        assert_eq!(NodeKind::Pointer.parse_edit("0x1000", 8).unwrap().len(), 8);
    }

    #[test]
    fn an_address_too_wide_for_the_target_is_rejected() {
        // Silently truncating would write a different address than the user typed.
        assert_eq!(
            NodeKind::Pointer.parse_edit("0x100000000", 4),
            Err(EditErr::OutOfRange)
        );
        assert!(NodeKind::Pointer.parse_edit("0xFFFFFFFF", 4).is_ok());
    }

    #[test]
    fn a_32_bit_pointer_formats_from_four_bytes() {
        let mut reg = ClassRegistry::new();
        reg.set_ptr_width(PtrWidth::P32);
        let ctx = FmtCtx::new(&reg);
        // eight bytes on the wire, but only the low four are the pointer
        let raw = 0xAABB_CCDD_0804_8000u64.to_le_bytes();
        assert_eq!(NodeKind::Pointer.format(&raw, &ctx), "0x8048000");

        reg.set_ptr_width(PtrWidth::P64);
        let ctx = FmtCtx::new(&reg);
        assert_eq!(NodeKind::Pointer.format(&raw, &ctx), "0xAABBCCDD08048000");
    }
}
