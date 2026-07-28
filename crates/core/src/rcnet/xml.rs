//! A tolerant, allocation-light XML reader and writer for `Data.xml`.
//!
//! Scoped to exactly what ReClass.NET writes: elements, attributes, comments,
//! the five predefined entities, and numeric character references. No
//! namespaces, no DTDs, no processing instructions beyond the declaration, no
//! mixed content — the file is a machine-generated attribute tree.
//!
//! Hand-written rather than pulled from a crate for the same reason
//! `official-plugins/src/cheat_table.rs` hand-writes its Cheat Engine XML: the
//! surface is small, single-vendor, and stable, and `reclass-core` otherwise
//! has four dependencies.

use std::fmt::Write as _;

use super::RcnetError;

/// A parsed element: tag, attributes, and child elements.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Element {
    /// Tag name.
    pub tag: String,
    /// Attributes in document order.
    pub attrs: Vec<(String, String)>,
    /// Child elements; text content is discarded, as the format has none.
    pub children: Vec<Element>,
}

impl Element {
    /// The value of `name`, if present.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// `attr` or the empty string — the format treats a missing optional
    /// attribute and an empty one identically.
    pub fn attr_or_empty(&self, name: &str) -> &str {
        self.attr(name).unwrap_or_default()
    }

    /// `attr` parsed as a decimal integer, or `None` when absent or malformed.
    pub fn attr_num<T: std::str::FromStr>(&self, name: &str) -> Option<T> {
        self.attr(name)?.trim().parse().ok()
    }

    /// Direct children with tag `tag`.
    pub fn children_named<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.tag == tag)
    }

    /// The first direct child with tag `tag`.
    pub fn child(&self, tag: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.tag == tag)
    }
}

/// Parse a document, returning its root element.
pub(super) fn parse(src: &str) -> Result<Element, RcnetError> {
    let b = src.as_bytes();
    let mut p = 0usize;
    let mut stack: Vec<Element> = Vec::new();

    // Everything that is not a tag is skipped: the format has no meaningful
    // text content, so inter-tag bytes are whitespace.
    while let Some(lt) = memchr(b, p, b'<') {
        p = lt + 1;

        match b.get(p) {
            // <!-- comment --> or <![CDATA[…]]>; both are skipped wholesale.
            Some(b'!') => {
                p = skip_bang(b, p).ok_or_else(|| unterminated(lt))?;
                continue;
            }
            // <?xml …?>
            Some(b'?') => {
                p = find(b, p, b"?>").ok_or_else(|| unterminated(lt))? + 2;
                continue;
            }
            // closing tag
            Some(b'/') => {
                p += 1;
                let end = memchr(b, p, b'>').ok_or_else(|| unterminated(lt))?;
                let name = src[p..end].trim();
                let done = stack.pop().ok_or_else(|| RcnetError::Xml {
                    msg: format!("closing </{name}> with no open element"),
                    pos: lt,
                })?;
                if done.tag != name {
                    return Err(RcnetError::Xml {
                        msg: format!("</{name}> closes <{}>", done.tag),
                        pos: lt,
                    });
                }
                p = end + 1;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    // The root closed; anything after it is trailing junk.
                    None => return Ok(done),
                }
            }
            _ => {
                let (el, next, self_closing) = parse_open(src, b, p, lt)?;
                p = next;
                if self_closing {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => return Ok(el),
                    }
                } else {
                    stack.push(el);
                }
            }
        }
    }
    Err(RcnetError::Xml {
        msg: if stack.is_empty() {
            "no root element".into()
        } else {
            format!("unclosed <{}>", stack[0].tag)
        },
        pos: src.len(),
    })
}

fn unterminated(pos: usize) -> RcnetError {
    RcnetError::Xml {
        msg: "unterminated tag".into(),
        pos,
    }
}

/// Skip `<!-- … -->`, `<![CDATA[ … ]]>`, or `<!DOCTYPE …>`, returning the
/// position just past it.
fn skip_bang(b: &[u8], p: usize) -> Option<usize> {
    if b[p..].starts_with(b"!--") {
        return Some(find(b, p, b"-->")? + 3);
    }
    if b[p..].starts_with(b"![CDATA[") {
        return Some(find(b, p, b"]]>")? + 3);
    }
    Some(memchr(b, p, b'>')? + 1)
}

/// Parse an opening tag starting at `p` (just past `<`).
///
/// Returns the element, the position after the tag, and whether it was
/// self-closing.
fn parse_open(
    src: &str,
    b: &[u8],
    mut p: usize,
    lt: usize,
) -> Result<(Element, usize, bool), RcnetError> {
    let name_start = p;
    while p < b.len() && !matches!(b[p], b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>') {
        p += 1;
    }
    let mut el = Element {
        tag: src[name_start..p].to_string(),
        ..Default::default()
    };
    if el.tag.is_empty() {
        return Err(RcnetError::Xml {
            msg: "empty tag name".into(),
            pos: lt,
        });
    }

    loop {
        while p < b.len() && b[p].is_ascii_whitespace() {
            p += 1;
        }
        match b.get(p) {
            None => return Err(unterminated(lt)),
            Some(b'>') => return Ok((el, p + 1, false)),
            Some(b'/') => {
                if b.get(p + 1) != Some(&b'>') {
                    return Err(unterminated(lt));
                }
                return Ok((el, p + 2, true));
            }
            _ => {}
        }
        let key_start = p;
        while p < b.len() && !matches!(b[p], b'=' | b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/') {
            p += 1;
        }
        let key = src[key_start..p].to_string();
        while p < b.len() && b[p].is_ascii_whitespace() {
            p += 1;
        }
        if b.get(p) != Some(&b'=') {
            // A valueless attribute is not valid here; treat it as empty rather
            // than rejecting a file over one stray token.
            el.attrs.push((key, String::new()));
            continue;
        }
        p += 1;
        while p < b.len() && b[p].is_ascii_whitespace() {
            p += 1;
        }
        let quote = *b.get(p).ok_or_else(|| unterminated(lt))?;
        if quote != b'"' && quote != b'\'' {
            return Err(RcnetError::Xml {
                msg: format!("unquoted value for attribute '{key}'"),
                pos: p,
            });
        }
        p += 1;
        let end = memchr(b, p, quote).ok_or_else(|| unterminated(lt))?;
        el.attrs.push((key, unescape(&src[p..end])));
        p = end + 1;
    }
}

fn memchr(b: &[u8], from: usize, needle: u8) -> Option<usize> {
    b.get(from..)?
        .iter()
        .position(|&c| c == needle)
        .map(|i| i + from)
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// Expand the five predefined entities and numeric character references.
///
/// An unrecognized `&…;` is kept verbatim: a class named `Health&Armor` written
/// by a tool that did not escape it should survive, not become mojibake.
pub(super) fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';').filter(|&i| i <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let body = &rest[1..semi];
        let replacement = match body {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => body
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escape a string for use as an attribute value.
pub(super) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters are not legal in XML 1.0 and would make the
            // file unloadable in ReClass.NET; drop them rather than emit them.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Incremental element writer with automatic indentation and closing.
pub(super) struct Writer {
    out: String,
    open: Vec<String>,
}

impl Writer {
    /// A new document with the XML declaration.
    pub fn new() -> Self {
        Writer {
            out: "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n".to_string(),
            open: Vec::new(),
        }
    }

    fn indent(&mut self) {
        for _ in 0..self.open.len() {
            self.out.push_str("  ");
        }
    }

    /// Open `tag` with `attrs`; must be matched by [`end`](Self::end).
    pub fn start(&mut self, tag: &str, attrs: &[(&str, String)]) {
        self.indent();
        let _ = write!(self.out, "<{tag}");
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", escape(v));
        }
        self.out.push_str(">\n");
        self.open.push(tag.to_string());
    }

    /// Write a self-closing `tag` with `attrs`.
    pub fn leaf(&mut self, tag: &str, attrs: &[(&str, String)]) {
        self.indent();
        let _ = write!(self.out, "<{tag}");
        for (k, v) in attrs {
            let _ = write!(self.out, " {k}=\"{}\"", escape(v));
        }
        self.out.push_str(" />\n");
    }

    /// Close the innermost open element.
    pub fn end(&mut self) {
        if let Some(tag) = self.open.pop() {
            self.indent();
            let _ = writeln!(self.out, "</{tag}>");
        }
    }

    /// Finish the document. Any still-open elements are closed, so a partial
    /// write never produces malformed XML.
    pub fn finish(mut self) -> String {
        while !self.open.is_empty() {
            self.end();
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_attributes_children_and_self_closing_tags() {
        let doc = r#"<?xml version="1.0"?>
            <!-- a comment -->
            <reclass version="65537" type="x64">
              <classes>
                <class uuid="u1" name="Player">
                  <node type="Hex32Node" name="hp" />
                </class>
              </classes>
            </reclass>"#;
        let root = parse(doc).unwrap();
        assert_eq!(root.tag, "reclass");
        assert_eq!(root.attr("version"), Some("65537"));
        assert_eq!(root.attr_num::<u32>("version"), Some(65537));
        let class = root.child("classes").unwrap().child("class").unwrap();
        assert_eq!(class.attr("name"), Some("Player"));
        assert_eq!(class.children.len(), 1);
        assert_eq!(class.children[0].attr("type"), Some("Hex32Node"));
    }

    #[test]
    fn entities_round_trip_through_escape_and_unescape() {
        let raw = "a<b>&\"c\"'d'";
        assert_eq!(unescape(&escape(raw)), raw);
        assert_eq!(unescape("&#65;&#x42;"), "AB");
        // an entity the writer never emits is left alone rather than mangled
        assert_eq!(unescape("Health&Armor"), "Health&Armor");
        assert_eq!(unescape("&nbsp;"), "&nbsp;");
    }

    #[test]
    fn control_characters_are_dropped_not_emitted() {
        // XML 1.0 forbids them; emitting one makes the file unloadable.
        assert_eq!(escape("a\u{1}b"), "ab");
        assert_eq!(escape("keep\tthese\n"), "keep\tthese\n");
    }

    #[test]
    fn malformed_documents_error_instead_of_panicking() {
        for bad in [
            "",
            "no tags here",
            "<open>",
            "</close>",
            "<a></b>",
            "<a attr=unquoted>",
            "<a",
            "<!-- never closed",
            "<>",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn single_quoted_attributes_and_odd_whitespace_parse() {
        let root = parse("<a  b = 'x'\n\tc=\"y\" />").unwrap();
        assert_eq!(root.attr("b"), Some("x"));
        assert_eq!(root.attr("c"), Some("y"));
    }

    #[test]
    fn a_written_document_parses_back() {
        let mut w = Writer::new();
        w.start("reclass", &[("version", "65537".into())]);
        w.start("classes", &[]);
        w.leaf("class", &[("name", "A \"quoted\" & <odd>".into())]);
        w.end();
        let doc = w.finish();
        let root = parse(&doc).unwrap();
        assert_eq!(
            root.child("classes")
                .unwrap()
                .child("class")
                .unwrap()
                .attr("name"),
            Some("A \"quoted\" & <odd>")
        );
    }

    #[test]
    fn finish_closes_elements_left_open() {
        let mut w = Writer::new();
        w.start("a", &[]);
        w.start("b", &[]);
        assert!(parse(&w.finish()).is_ok());
    }

    #[test]
    fn a_missing_attribute_reads_as_empty() {
        let root = parse("<a />").unwrap();
        assert_eq!(root.attr_or_empty("name"), "");
        assert_eq!(root.attr_num::<u32>("count"), None);
    }
}
