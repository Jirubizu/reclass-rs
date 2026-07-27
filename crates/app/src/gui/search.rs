//! Row filtering and address navigation for the node table.
//!
//! Both are pure functions over the snapshot the engine already produced, so
//! they cost one pass over the visible rows and need no live reads.

use reclass_core::Row;

/// Whether `row` matches `needle`, which the caller has already lowercased.
///
/// Every column the table shows is searchable, including the rendered value:
/// finding the field that currently reads `1337` is the common way into a
/// struct you do not yet understand.
fn matches(row: &Row, needle: &str) -> bool {
    let hay = [
        row.name.as_str(),
        row.type_label.as_str(),
        row.value.as_str(),
        row.comment.as_str(),
    ];
    if hay.iter().any(|h| h.to_ascii_lowercase().contains(needle)) {
        return true;
    }
    // Offset and address are rendered, not stored, as text — searching for
    // "0x1c" should find the field at that offset.
    format!("0x{:x}", row.offset).contains(needle)
        || format!("0x{:x}", row.address).contains(needle)
}

/// Rows matching `query`, each preceded by its ancestors.
///
/// Ancestors are kept because the table is a tree rendered flat: a matching
/// field inside an expanded `ClassPtr` shown without its parent row is a
/// nameless value at an unexplained indent. An empty or whitespace-only query
/// returns everything, so the filter is off by default.
///
/// Walks backwards tracking the shallowest depth still owed an ancestor, so the
/// whole filter is one pass with no per-row parent lookup.
pub(super) fn filter_rows<'a>(rows: &[&'a Row], query: &str) -> Vec<&'a Row> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return rows.to_vec();
    }
    let mut keep = vec![false; rows.len()];
    // Shallowest depth still owed an ancestor. Starts at 0 — nothing is owed
    // yet, so a non-matching row before any match is dropped.
    let mut needed = 0u32;
    for (i, row) in rows.iter().enumerate().rev() {
        // Either a hit, or an ancestor of one already kept. Both owe the same
        // chain of shallower rows from here.
        if matches(row, &needle) || row.depth < needed {
            keep[i] = true;
            needed = row.depth;
        }
    }
    rows.iter()
        .zip(keep)
        .filter_map(|(r, k)| k.then_some(*r))
        .collect()
}

/// Parse a goto target: `0x`-prefixed or bare hex, or `123d` decimal.
///
/// Bare digits are hex, matching the address bar and the rest of the tool; the
/// `d` suffix is the escape hatch for the rare decimal offset.
pub(super) fn parse_goto(input: &str) -> Option<u64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(dec) = s.strip_suffix(['d', 'D']) {
        return dec.trim().parse::<u64>().ok();
    }
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(hex, 16).ok()
}

/// Index of the row `target` lands in, matching absolute address first and
/// falling back to class-relative offset.
///
/// Address first because that is what a debugger, `/proc/pid/maps`, or a
/// pointer value gives you. The offset fallback covers a detached session,
/// where every address is 0 and only offsets are meaningful.
///
/// "Lands in" means the last row at or before `target`, not an exact hit: an
/// address usually points into the middle of a field, and scrolling to the
/// field that contains it is the useful answer.
pub(super) fn find_row(rows: &[&Row], target: u64) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    // Offsets are per-parent and repeat across a tree, so they are only a
    // sound key when there are no addresses at all — a detached session.
    let key: fn(&Row) -> u64 = if rows.iter().any(|r| r.address != 0) {
        |r| r.address
    } else {
        |r| r.offset as u64
    };
    rows.iter()
        .enumerate()
        .filter(|(_, r)| key(r) <= target)
        .map(|(i, _)| i)
        .next_back()
        // A target below every row still scrolls somewhere useful: the top.
        .or(Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reclass_core::{NodeKind, PathSeg};

    fn row(depth: u32, offset: usize, address: u64, name: &str, value: &str) -> Row {
        Row {
            depth,
            root: 0,
            offset,
            address,
            type_label: "Hex32".into(),
            name: name.into(),
            value: value.into(),
            hex: String::new(),
            kind: NodeKind::Pointer,
            comment: String::new(),
            expandable: false,
            expanded: false,
            path: vec![PathSeg::Node(offset)],
            readable: true,
        }
    }

    fn tree() -> Vec<Row> {
        vec![
            row(0, 0, 0x1000, "hp", "100"),
            row(0, 4, 0x1004, "weapon", "0x2000"),
            row(1, 0, 0x2000, "ammo", "30"),
            row(1, 4, 0x2004, "clip", "7"),
            row(0, 12, 0x100C, "mana", "50"),
        ]
    }

    fn refs(rows: &[Row]) -> Vec<&Row> {
        rows.iter().collect()
    }

    #[test]
    fn an_empty_query_keeps_everything() {
        let t = tree();
        assert_eq!(filter_rows(&refs(&t), "").len(), 5);
        assert_eq!(filter_rows(&refs(&t), "   ").len(), 5);
    }

    #[test]
    fn a_match_brings_its_ancestors_along() {
        // `ammo` is nested under `weapon`; showing it alone would be a nameless
        // value at an unexplained indent.
        let t = tree();
        let got = filter_rows(&refs(&t), "ammo");
        let names: Vec<&str> = got.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["weapon", "ammo"]);
    }

    #[test]
    fn matching_is_case_insensitive_across_every_column() {
        let t = tree();
        assert_eq!(filter_rows(&refs(&t), "HP").len(), 1);
        // by rendered value
        assert_eq!(filter_rows(&refs(&t), "50")[0].name, "mana");
        // by type label
        assert_eq!(filter_rows(&refs(&t), "hex32").len(), 5);
    }

    #[test]
    fn offsets_and_addresses_are_searchable_as_text() {
        let t = tree();
        assert_eq!(filter_rows(&refs(&t), "0x100c")[0].name, "mana");
        // 0xc is the offset of mana; the ancestor rule adds nothing at depth 0
        let by_off = filter_rows(&refs(&t), "0xc");
        assert!(by_off.iter().any(|r| r.name == "mana"), "{by_off:?}");
    }

    #[test]
    fn a_query_matching_nothing_yields_nothing() {
        let t = tree();
        assert!(filter_rows(&refs(&t), "zzzz").is_empty());
    }

    #[test]
    fn a_matching_parent_does_not_drag_in_its_children() {
        // Only ancestors are implied, not descendants: matching `weapon` should
        // not dump its whole subtree back into the results.
        let t = tree();
        let names: Vec<&str> = filter_rows(&refs(&t), "weapon")
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(names, ["weapon"]);
    }

    #[test]
    fn goto_parses_hex_forms_and_decimal_suffix() {
        assert_eq!(parse_goto("0x1004"), Some(0x1004));
        assert_eq!(parse_goto("  0X20 "), Some(0x20));
        assert_eq!(parse_goto("1004"), Some(0x1004), "bare digits are hex");
        assert_eq!(parse_goto("16d"), Some(16), "the d suffix means decimal");
        assert_eq!(parse_goto(""), None);
        assert_eq!(parse_goto("nonsense"), None);
    }

    #[test]
    fn goto_lands_in_the_field_containing_the_address() {
        // 0x1006 is inside `weapon` (0x1004..0x100C), not an exact row address.
        let t = tree();
        let r = refs(&t);
        assert_eq!(
            find_row(&r, 0x1006).map(|i| r[i].name.as_str()),
            Some("weapon")
        );
        assert_eq!(find_row(&r, 0x1000).map(|i| r[i].name.as_str()), Some("hp"));
    }

    #[test]
    fn goto_falls_back_to_offsets_when_detached() {
        // Detached: every address is 0, so only offsets are meaningful.
        let t: Vec<Row> = tree()
            .into_iter()
            .map(|mut r| {
                r.address = 0;
                r
            })
            .collect();
        let r = refs(&t);
        assert_eq!(find_row(&r, 4).map(|i| r[i].name.as_str()), Some("clip"));
        assert_eq!(find_row(&r, 0xC).map(|i| r[i].name.as_str()), Some("mana"));
    }

    #[test]
    fn goto_before_the_first_row_and_on_an_empty_table() {
        let t = tree();
        let r = refs(&t);
        // below every address, but offset 0 still matches the first row
        assert_eq!(find_row(&r, 0).map(|i| r[i].name.as_str()), Some("hp"));
        assert_eq!(find_row(&[], 0x1000), None);
    }
}
