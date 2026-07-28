//! `.rcnet` conversion tests.
//!
//! The XML fixtures are written the way ReClass.NET writes them — attribute
//! order, `hidden` flags, the project-level enum table — so a change that only
//! works against our own exporter fails here.

use super::*;

/// A document in ReClass.NET's exact shape, with `$CLASSES` substituted.
fn doc(platform: &str, enums: &str, classes: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!--ReClass.NET 1.2 by KN4CK3R-->
<reclass version="65537" type="{platform}">
  <custom_data />
  <type_mapping />
  <enums>{enums}</enums>
  <classes>{classes}</classes>
</reclass>"#
    )
}

const U1: &str = "11111111-1111-1111-1111-111111111111";
const U2: &str = "22222222-2222-2222-2222-222222222222";

fn kinds(reg: &ClassRegistry, id: ClassId) -> Vec<NodeKind> {
    reg.get(id)
        .map(|c| c.nodes.iter().map(|n| n.kind.clone()).collect())
        .unwrap_or_default()
}

#[test]
fn imports_scalars_names_comments_and_the_address_formula() {
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="Player" comment="" address="game.exe+0x1234">
                 <node type="Int32Node" name="hp" comment="health" hidden="false" />
                 <node type="FloatNode" name="speed" comment="" hidden="false" />
                 <node type="BoolNode" name="alive" comment="" hidden="false" />
                 <node type="Hex64Node" name="pad" comment="" hidden="false" />
               </class>"#
        ),
    ))
    .unwrap();

    assert!(report.is_exact(), "{:?}", report.notes);
    assert_eq!(report.classes, 1);
    assert_eq!(report.nodes, 4);
    let id = reg.ids()[0];
    assert_eq!(reg.name_of(id), Some("Player"));
    assert_eq!(reg.get(id).unwrap().address_expr, "game.exe+0x1234");
    assert_eq!(reg.get(id).unwrap().nodes[0].comment, "health");
    assert_eq!(
        kinds(&reg, id),
        [
            NodeKind::Int(IntWidth::W32),
            NodeKind::Float32,
            NodeKind::Bool,
            NodeKind::Hex(IntWidth::W64),
        ]
    );
}

#[test]
fn a_class_reference_resolves_across_declaration_order() {
    // The referenced class is declared *after* the one referencing it, which
    // the format allows and a single-pass importer would get wrong.
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="Player" comment="" address="">
                 <node type="ClassInstanceNode" name="w" comment="" hidden="false" reference="{U2}" />
                 <node type="PointerNode" name="wp" comment="" hidden="false">
                   <node type="ClassInstanceNode" name="" comment="" hidden="false" reference="{U2}" />
                 </node>
               </class>
               <class uuid="{U2}" name="Weapon" comment="" address="">
                 <node type="Int32Node" name="ammo" comment="" hidden="false" />
               </class>"#
        ),
    ))
    .unwrap();

    assert!(report.is_exact(), "{:?}", report.notes);
    let player = reg.ids()[0];
    let weapon = reg.ids()[1];
    assert_eq!(reg.name_of(weapon), Some("Weapon"));
    assert_eq!(
        kinds(&reg, player),
        [
            NodeKind::ClassInstance { class_id: weapon },
            NodeKind::ClassPtr { class_id: weapon },
        ]
    );
    // and the layout the references imply actually computes
    assert_eq!(reg.size_of(player), 4 + 8);
}

#[test]
fn an_enum_is_inlined_from_the_project_table() {
    let (reg, report) = import_xml(&doc(
        "x64",
        r#"<enum name="State" size="TwoBytes" flags="false">
             <item name="Idle" value="0" />
             <item name="Dead" value="-1" />
           </enum>"#,
        &format!(
            r#"<class uuid="{U1}" name="E" comment="" address="">
                 <node type="EnumNode" name="state" comment="" hidden="false" reference="State" />
               </class>"#
        ),
    ))
    .unwrap();

    assert!(report.is_exact(), "{:?}", report.notes);
    let NodeKind::Enum { width, variants } = &kinds(&reg, reg.ids()[0])[0] else {
        panic!("not an enum");
    };
    assert_eq!(*width, IntWidth::W16);
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[1].name, "Dead");
    assert_eq!(variants[1].value, -1);
}

#[test]
fn an_enum_reference_with_no_table_entry_degrades_and_says_so() {
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="E" comment="" address="">
                 <node type="EnumNode" name="state" comment="" hidden="false" reference="Ghost" />
               </class>"#
        ),
    ))
    .unwrap();
    assert_eq!(kinds(&reg, reg.ids()[0]), [NodeKind::UInt(IntWidth::W32)]);
    assert!(
        report.notes.iter().any(|n| n.contains("Ghost")),
        "{:?}",
        report.notes
    );
}

#[test]
fn the_platform_attribute_sets_pointer_width() {
    let body = format!(
        r#"<class uuid="{U1}" name="C" comment="" address="">
             <node type="PointerNode" name="p" comment="" hidden="false" />
             <node type="NIntNode" name="n" comment="" hidden="false" />
           </class>"#
    );
    let (x64, _) = import_xml(&doc("x64", "", &body)).unwrap();
    assert_eq!(x64.ptr_width(), PtrWidth::P64);
    assert_eq!(x64.size_of(x64.ids()[0]), 16);

    let (x86, _) = import_xml(&doc("x86", "", &body)).unwrap();
    assert_eq!(x86.ptr_width(), PtrWidth::P32);
    // both the pointer and the native-width int narrow
    assert_eq!(x86.size_of(x86.ids()[0]), 8);
}

#[test]
fn matrices_import_as_rows_of_vectors_with_the_same_layout() {
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="M" comment="" address="">
                 <node type="Matrix3x3Node" name="a" comment="" hidden="false" />
                 <node type="Matrix3x4Node" name="b" comment="" hidden="false" />
                 <node type="Matrix4x4Node" name="c" comment="" hidden="false" />
               </class>"#
        ),
    ))
    .unwrap();
    assert!(report.is_exact(), "{:?}", report.notes);
    // 36 + 48 + 64 — the byte layout ReClass.NET uses
    assert_eq!(reg.size_of(reg.ids()[0]), 36 + 48 + 64);
}

#[test]
fn text_nodes_carry_their_length_and_encoding() {
    let (reg, _) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="T" comment="" address="">
                 <node type="Utf8TextNode" name="a" comment="" hidden="false" length="32" />
                 <node type="Utf16TextNode" name="b" comment="" hidden="false" length="16" />
                 <node type="Utf8TextPtrNode" name="c" comment="" hidden="false" />
               </class>"#
        ),
    ))
    .unwrap();
    assert_eq!(
        kinds(&reg, reg.ids()[0]),
        [
            NodeKind::Text {
                encoding: TextEncoding::Utf8,
                len: 32
            },
            NodeKind::Text {
                encoding: TextEncoding::Utf16,
                len: 16
            },
            NodeKind::PtrText {
                encoding: TextEncoding::Utf8,
                max: 64
            },
        ]
    );
    assert_eq!(reg.size_of(reg.ids()[0]), 32 + 32 + 8);
}

#[test]
fn unrepresentable_types_are_approximated_and_reported_not_dropped_silently() {
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="X" comment="" address="">
                 <node type="VirtualMethodTableNode" name="vt" comment="" hidden="false">
                   <method name="m0" comment="" hidden="false" />
                   <method name="m1" comment="" hidden="false" />
                 </node>
                 <node type="UnionNode" name="u" comment="" hidden="false">
                   <node type="Int32Node" name="i" comment="" hidden="false" />
                   <node type="Hex64Node" name="h" comment="" hidden="false" />
                 </node>
                 <node type="Utf32TextNode" name="t" comment="" hidden="false" length="4" />
               </class>"#
        ),
    ))
    .unwrap();

    let k = kinds(&reg, reg.ids()[0]);
    // vtable -> that many function pointers, union -> its largest member's bytes
    assert_eq!(
        k[0],
        NodeKind::Array {
            element: Box::new(NodeKind::FunctionPtr),
            count: 2
        }
    );
    assert_eq!(k[1], NodeKind::Unknown(8));
    assert_eq!(k[2], NodeKind::Unknown(16));
    assert_eq!(report.notes.len(), 3, "{:?}", report.notes);
    assert!(report.notes.iter().any(|n| n.contains("vtable")));
    assert!(report.notes.iter().any(|n| n.contains("union")));
    assert!(report.notes.iter().any(|n| n.contains("UTF-32")));
}

#[test]
fn an_unknown_node_type_is_skipped_and_named() {
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="X" comment="" address="">
                 <node type="Int32Node" name="keep" comment="" hidden="false" />
                 <node type="SomeFuturePluginNode" name="drop" comment="" hidden="false" />
               </class>"#
        ),
    ))
    .unwrap();
    assert_eq!(kinds(&reg, reg.ids()[0]), [NodeKind::Int(IntWidth::W32)]);
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("SomeFuturePluginNode")),
        "{:?}",
        report.notes
    );
}

#[test]
fn a_reference_to_a_class_that_is_not_in_the_file_is_skipped() {
    // Dropping the field is the only safe option: keeping it would put a
    // dangling class id in the registry, which `validate` rejects on save.
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="X" comment="" address="">
                 <node type="ClassInstanceNode" name="ghost" comment="" hidden="false" reference="{U2}" />
               </class>"#
        ),
    ))
    .unwrap();
    assert!(kinds(&reg, reg.ids()[0]).is_empty());
    assert!(reg.validate().is_ok(), "import left a dangling reference");
    assert!(report.notes.iter().any(|n| n.contains("unknown class")));
}

#[test]
fn arrays_import_with_their_element_type() {
    let (reg, _) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="A" comment="" address="">
                 <node type="ArrayNode" name="xs" comment="" hidden="false" count="8">
                   <node type="Int32Node" name="" comment="" hidden="false" />
                 </node>
               </class>"#
        ),
    ))
    .unwrap();
    assert_eq!(
        kinds(&reg, reg.ids()[0]),
        [NodeKind::Array {
            element: Box::new(NodeKind::Int(IntWidth::W32)),
            count: 8
        }]
    );
    assert_eq!(reg.size_of(reg.ids()[0]), 32);
}

#[test]
fn a_duplicate_uuid_keeps_the_first_class_and_reports_it() {
    let (reg, report) = import_xml(&doc(
        "x64",
        "",
        &format!(
            r#"<class uuid="{U1}" name="First" comment="" address="" />
               <class uuid="{U1}" name="Second" comment="" address="" />"#
        ),
    ))
    .unwrap();
    assert_eq!(reg.len(), 1);
    assert_eq!(reg.name_of(reg.ids()[0]), Some("First"));
    assert!(report.notes.iter().any(|n| n.contains("duplicate")));
}

#[test]
fn a_document_that_is_not_a_project_is_rejected() {
    assert!(matches!(
        import_xml("<notreclass><classes/></notreclass>"),
        Err(RcnetError::NotAProject(_))
    ));
    assert!(matches!(
        import_xml(r#"<reclass version="65537"/>"#),
        Err(RcnetError::NotAProject(_))
    ));
    assert!(matches!(import_xml("<<<"), Err(RcnetError::Xml { .. })));
}

#[test]
fn a_file_from_a_newer_major_version_is_refused() {
    // Matching ReClass.NET's own critical-mask check: a bumped major means the
    // schema changed and guessing would corrupt the import.
    let newer = doc("x64", "", "").replace("version=\"65537\"", "version=\"131073\"");
    assert!(matches!(
        import_xml(&newer),
        Err(RcnetError::UnsupportedVersion(0x0002_0001))
    ));
    // a bumped minor is still readable
    let minor = doc("x64", "", "").replace("version=\"65537\"", "version=\"65539\"");
    assert!(import_xml(&minor).is_ok());
}

// ---------------------------------------------------------------------------
// export + round trip
// ---------------------------------------------------------------------------

/// A registry exercising every kind that has an exact `.rcnet` equivalent.
fn sample() -> (ClassRegistry, ClassId, ClassId) {
    let mut reg = ClassRegistry::new();
    let weapon = reg.add_class("Weapon");
    reg.push_node(weapon, Node::new("ammo", NodeKind::Int(IntWidth::W32)))
        .unwrap();

    let player = reg.add_class("Player");
    for node in [
        Node::new("hp", NodeKind::Int(IntWidth::W32)),
        Node::new("speed", NodeKind::Float32),
        Node::new("alive", NodeKind::Bool),
        Node::new("pos", NodeKind::Vec3),
        Node::new(
            "name",
            NodeKind::Text {
                encoding: TextEncoding::Utf8,
                len: 24,
            },
        ),
        Node::new(
            "label",
            NodeKind::PtrText {
                encoding: TextEncoding::Utf16,
                max: 64,
            },
        ),
        Node::new("flags", NodeKind::Bitfield(IntWidth::W16)),
        Node::new(
            "state",
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
                ],
            },
        ),
        Node::new("weapon", NodeKind::ClassPtr { class_id: weapon }),
        Node::new("inline", NodeKind::ClassInstance { class_id: weapon }),
        Node::new(
            "scores",
            NodeKind::Array {
                element: Box::new(NodeKind::UInt(IntWidth::W16)),
                count: 4,
            },
        ),
        Node::new("fn", NodeKind::FunctionPtr),
        Node::new("raw", NodeKind::Pointer),
    ] {
        reg.push_node(player, node).unwrap();
    }
    reg.set_address_expr(player, "<game.so> + 0x10".to_string())
        .unwrap();
    (reg, player, weapon)
}

#[test]
fn a_registry_survives_a_full_round_trip_through_the_archive() {
    let (reg, player, weapon) = sample();
    let before = (reg.size_of(player), reg.size_of(weapon));

    let (bytes, out) = export(&reg).unwrap();
    let (back, r#in) = import(&bytes).unwrap();

    assert!(out.is_exact(), "export: {:?}", out.notes);
    assert!(r#in.is_exact(), "import: {:?}", r#in.notes);
    assert_eq!(back.len(), reg.len());

    // Ids are reassigned on import, so compare by name.
    let find = |r: &ClassRegistry, name: &str| r.iter().find(|c| c.name == name).map(|c| c.id);
    let p2 = find(&back, "Player").expect("Player survived");
    let w2 = find(&back, "Weapon").expect("Weapon survived");
    assert_eq!(
        (back.size_of(p2), back.size_of(w2)),
        before,
        "layout drifted"
    );
    assert_eq!(back.get(p2).unwrap().address_expr, "<game.so> + 0x10");

    let orig: Vec<_> = reg.get(player).unwrap().nodes.iter().collect();
    let round: Vec<_> = back.get(p2).unwrap().nodes.iter().collect();
    assert_eq!(orig.len(), round.len());
    for (a, b) in orig.iter().zip(&round) {
        assert_eq!(a.name, b.name, "field name drifted");
        // class references are re-pointed to the new ids, so compare by shape
        match (&a.kind, &b.kind) {
            (NodeKind::ClassPtr { .. }, NodeKind::ClassPtr { class_id }) => {
                assert_eq!(*class_id, w2)
            }
            (NodeKind::ClassInstance { .. }, NodeKind::ClassInstance { class_id }) => {
                assert_eq!(*class_id, w2);
            }
            (x, y) => assert_eq!(x, y, "field '{}' drifted", a.name),
        }
    }
}

#[test]
fn a_32_bit_project_round_trips_as_32_bit() {
    let (mut reg, player, _) = sample();
    reg.set_ptr_width(PtrWidth::P32);
    let size = reg.size_of(player);

    let (bytes, _) = export(&reg).unwrap();
    let (back, _) = import(&bytes).unwrap();
    assert_eq!(back.ptr_width(), PtrWidth::P32);
    let p2 = back.iter().find(|c| c.name == "Player").unwrap().id;
    assert_eq!(back.size_of(p2), size);
}

#[test]
fn exporting_twice_produces_identical_bytes() {
    // Deterministic output is what makes a project file diffable and a
    // round-trip test meaningful.
    let (reg, _, _) = sample();
    assert_eq!(export(&reg).unwrap().0, export(&reg).unwrap().0);
}

#[test]
fn raw_blocks_export_as_byte_arrays_of_the_same_size() {
    let mut reg = ClassRegistry::new();
    let c = reg.add_class("P");
    reg.push_node(c, Node::new("pad", NodeKind::Padding(3)))
        .unwrap();
    reg.push_node(c, Node::new("gap", NodeKind::Unknown(5)))
        .unwrap();
    let size = reg.size_of(c);

    let (bytes, out) = export(&reg).unwrap();
    assert_eq!(out.notes.len(), 2, "{:?}", out.notes);
    let (back, _) = import(&bytes).unwrap();
    let id = back.ids()[0];
    // the shape is lost but the layout is not, which is the property that matters
    assert_eq!(back.size_of(id), size);
    assert_eq!(
        kinds(&back, id)[0],
        NodeKind::Array {
            element: Box::new(NodeKind::Hex(IntWidth::W8)),
            count: 3
        }
    );
}

#[test]
fn enum_names_do_not_collide_across_classes() {
    // The rcnet enum table is keyed by name; two fields called `state` in
    // different classes would otherwise silently share one table entry.
    let mut reg = ClassRegistry::new();
    for (cls, val) in [("A", 1i64), ("B", 2)] {
        let id = reg.add_class(cls);
        reg.push_node(
            id,
            Node::new(
                "state",
                NodeKind::Enum {
                    width: IntWidth::W32,
                    variants: vec![EnumVariant {
                        value: val,
                        name: format!("V{val}"),
                    }],
                },
            ),
        )
        .unwrap();
    }
    let (bytes, _) = export(&reg).unwrap();
    let (back, _) = import(&bytes).unwrap();
    let a = back.iter().find(|c| c.name == "A").unwrap().id;
    let b = back.iter().find(|c| c.name == "B").unwrap().id;
    let variant = |r: &ClassRegistry, id| match &kinds(r, id)[0] {
        NodeKind::Enum { variants, .. } => variants[0].name.clone(),
        other => panic!("not an enum: {other:?}"),
    };
    assert_eq!(variant(&back, a), "V1");
    assert_eq!(variant(&back, b), "V2");
}

#[test]
fn exported_xml_looks_like_reclass_net_wrote_it() {
    let (reg, _, _) = sample();
    let (xml, _) = export_xml(&reg);
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
    assert!(xml.contains("<reclass version=\"65537\" type=\"x64\">"));
    assert!(xml.contains("<enums>"));
    assert!(xml.contains("<classes>"));
    assert!(xml.contains("type=\"Int32Node\""));
    assert!(xml.contains("hidden=\"false\""));
    // uuid must be GUID-shaped or ReClass.NET's parser throws
    assert!(xml.contains("uuid=\"00000000-0000-0000-0000-000000000000\""));
    // and it must parse as the XML it claims to be
    xml::parse(&xml).expect("our own output parses");
}

#[test]
fn names_needing_escapes_survive_a_round_trip() {
    let mut reg = ClassRegistry::new();
    let c = reg.add_class("A & B <C>");
    reg.push_node(
        c,
        Node {
            name: "\"quoted\"".into(),
            comment: "it's <fine> & ok".into(),
            kind: NodeKind::Bool,
        },
    )
    .unwrap();
    let (bytes, _) = export(&reg).unwrap();
    let (back, _) = import(&bytes).unwrap();
    let id = back.ids()[0];
    assert_eq!(back.name_of(id), Some("A & B <C>"));
    assert_eq!(back.get(id).unwrap().nodes[0].name, "\"quoted\"");
    assert_eq!(back.get(id).unwrap().nodes[0].comment, "it's <fine> & ok");
}

#[test]
fn an_empty_registry_round_trips() {
    let reg = ClassRegistry::new();
    let (bytes, _) = export(&reg).unwrap();
    let (back, report) = import(&bytes).unwrap();
    assert!(back.is_empty());
    assert!(report.is_exact());
}

#[test]
fn importing_something_that_is_not_an_archive_errors() {
    assert!(matches!(import(b"not a zip"), Err(RcnetError::NotAZip)));
}
