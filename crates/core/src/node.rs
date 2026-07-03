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

/// Text encoding for a [`NodeKind::Text`] node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TextEncoding {
    /// One byte per code unit.
    Utf8,
    /// Two (little-endian) bytes per code unit.
    Utf16,
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
            other => other.fixed_size(),
        }
    }

    /// Size of every non-recursive kind; `0` for `ClassInstance`/`Array` (use
    /// [`size`](Self::size) with a registry for those).
    #[must_use]
    pub fn fixed_size(&self) -> usize {
        match self {
            NodeKind::Hex(w) | NodeKind::Int(w) | NodeKind::UInt(w) => w.bytes(),
            NodeKind::Float32 => 4,
            NodeKind::Float64 => 8,
            NodeKind::Bool => 1,
            NodeKind::Vec2 => 8,
            NodeKind::Vec3 => 12,
            NodeKind::Vec4 => 16,
            NodeKind::Text { encoding, len } => match encoding {
                TextEncoding::Utf8 => *len,
                TextEncoding::Utf16 => len * 2,
            },
            NodeKind::Pointer | NodeKind::ClassPtr { .. } | NodeKind::FunctionPtr => 8,
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
            NodeKind::Text { encoding, len } => match encoding {
                TextEncoding::Utf8 => format!("Text[{len}]"),
                TextEncoding::Utf16 => format!("WText[{len}]"),
            },
            NodeKind::Pointer => "Ptr".into(),
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
                    fmt_float(f64::from(read_f32(bytes))),
                    fmt_float(f64::from(read_f32(&bytes[4..])))
                )
            }
            NodeKind::Vec3 => format!(
                "({}, {}, {})",
                fmt_float(f64::from(read_f32(bytes))),
                fmt_float(f64::from(read_f32(&bytes[4..]))),
                fmt_float(f64::from(read_f32(&bytes[8..]))),
            ),
            NodeKind::Vec4 => format!(
                "({}, {}, {}, {})",
                fmt_float(f64::from(read_f32(bytes))),
                fmt_float(f64::from(read_f32(&bytes[4..]))),
                fmt_float(f64::from(read_f32(&bytes[8..]))),
                fmt_float(f64::from(read_f32(&bytes[12..]))),
            ),
            NodeKind::Text { encoding, .. } => format_text(bytes, *encoding),
            NodeKind::Pointer | NodeKind::FunctionPtr => {
                let target = le_unsigned(&bytes[..8.min(bytes.len())]);
                format_ptr(target, ctx)
            }
            NodeKind::ClassPtr { class_id } => {
                let target = le_unsigned(&bytes[..8.min(bytes.len())]);
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
    pub fn parse_edit(&self, input: &str) -> Result<Vec<u8>, EditErr> {
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
            NodeKind::Text { encoding, len } => Ok(encode_text(input, *encoding, *len)),
            NodeKind::Pointer | NodeKind::ClassPtr { .. } | NodeKind::FunctionPtr => {
                Ok(parse_addr(input)?.to_le_bytes().to_vec())
            }
            NodeKind::Array { .. }
            | NodeKind::ClassInstance { .. }
            | NodeKind::Padding(_)
            | NodeKind::Unknown(_) => Err(EditErr::NotEditable),
        }
    }
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

fn format_text(bytes: &[u8], encoding: TextEncoding) -> String {
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
        assert_eq!(NodeKind::Hex(IntWidth::W32).fixed_size(), 4);
        assert_eq!(NodeKind::Int(IntWidth::W64).fixed_size(), 8);
        assert_eq!(NodeKind::Bool.fixed_size(), 1);
        assert_eq!(NodeKind::Vec3.fixed_size(), 12);
        assert_eq!(NodeKind::Pointer.fixed_size(), 8);
        assert_eq!(
            NodeKind::Text {
                encoding: TextEncoding::Utf16,
                len: 8
            }
            .fixed_size(),
            16
        );
        assert_eq!(NodeKind::Padding(5).fixed_size(), 5);
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
            NodeKind::Int(IntWidth::W32).parse_edit("-5").unwrap(),
            (-5i32).to_le_bytes()
        );
        assert_eq!(
            NodeKind::UInt(IntWidth::W16).parse_edit("0x1234").unwrap(),
            0x1234u16.to_le_bytes()
        );
        assert_eq!(
            NodeKind::Hex(IntWidth::W8).parse_edit("255").unwrap(),
            vec![255]
        );
        // out of range
        assert_eq!(
            NodeKind::UInt(IntWidth::W8).parse_edit("256"),
            Err(EditErr::OutOfRange)
        );
        assert_eq!(
            NodeKind::Int(IntWidth::W8).parse_edit("128"),
            Err(EditErr::OutOfRange)
        );
        assert_eq!(
            NodeKind::Int(IntWidth::W8).parse_edit("-128").unwrap(),
            vec![0x80]
        );
    }

    #[test]
    fn parse_edit_float_bool_vec() {
        assert_eq!(
            NodeKind::Float32.parse_edit("1.5").unwrap(),
            1.5f32.to_le_bytes()
        );
        assert_eq!(NodeKind::Bool.parse_edit("true").unwrap(), vec![1]);
        assert_eq!(NodeKind::Bool.parse_edit("0").unwrap(), vec![0]);
        let v = NodeKind::Vec2.parse_edit("1.0, 2.0").unwrap();
        assert_eq!(&v[..4], &1.0f32.to_le_bytes());
        assert_eq!(&v[4..], &2.0f32.to_le_bytes());
        assert_eq!(
            NodeKind::Vec3.parse_edit("1,2"),
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
        assert_eq!(txt.parse_edit("hello").unwrap(), b"hell".to_vec());
        assert_eq!(txt.parse_edit("hi").unwrap(), b"hi\0\0".to_vec());
    }

    #[test]
    fn parse_edit_not_editable() {
        assert_eq!(
            NodeKind::Padding(4).parse_edit("x"),
            Err(EditErr::NotEditable)
        );
        assert_eq!(
            NodeKind::ClassInstance { class_id: 1 }.parse_edit("x"),
            Err(EditErr::NotEditable)
        );
    }

    #[test]
    fn pointer_roundtrip_edit() {
        let bytes = NodeKind::Pointer.parse_edit("0x7fff1234").unwrap();
        assert_eq!(u64::from_le_bytes(bytes.try_into().unwrap()), 0x7fff_1234);
    }
}
