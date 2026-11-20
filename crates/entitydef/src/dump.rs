//! The `tools/process_defs` dump contract.
//!
//! `process_defs` builds a dict of every parsed entity description plus
//! `constants.digest`; the capture harness serialises it with
//! `json.dump(payload, sort_keys=True, indent=1)` and a trailing newline.
//! This module reproduces that byte-for-byte from the Rust descriptions so a
//! differential test can compare the two implementations field for field
//! (BW-04-01-01/03/04/05, BW-04-04-03).

use std::path::Path;

use crate::constants::{ALIASES_FILE, ENTITIES_FILE};
use crate::data_description::DataDescription;
use crate::data_type::TypeContext;
use crate::diagnostics::Diagnostics;
use crate::entity_description::{DefResolver, EntityDescription, FsResolver};
use crate::entity_map::{EntityDescriptionMap, MessageRanges};
use crate::flags::{CLIENT_DATA, FROM_BASE_TO_CLIENT_DATA, FROM_CELL_TO_CLIENT_DATA};
use crate::method::{MethodArgs, MethodDescription};

/// Just enough JSON to reproduce the harness' output bytes.
#[derive(Debug, Clone)]
pub enum JsonValue {
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// Serialise like Python's `json.dump(..., sort_keys=True, indent=1)`.
    pub fn to_json_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Self::Bool(true) => out.push_str("true"),
            Self::Bool(false) => out.push_str("false"),
            Self::Int(value) => out.push_str(&value.to_string()),
            Self::Str(value) => write_json_string(out, value),
            Self::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    push_indent(out, depth + 1);
                    item.write(out, depth + 1);
                }
                out.push('\n');
                push_indent(out, depth);
                out.push(']');
            }
            Self::Object(entries) => {
                if entries.is_empty() {
                    out.push_str("{}");
                    return;
                }
                // sort_keys=True.
                let mut sorted = entries.clone();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                out.push_str("{\n");
                for (i, (key, value)) in sorted.iter().enumerate() {
                    if i > 0 {
                        out.push_str(",\n");
                    }
                    push_indent(out, depth + 1);
                    write_json_string(out, key);
                    out.push_str(": ");
                    value.write(out, depth + 1);
                }
                out.push('\n');
                push_indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push(' ');
    }
}

fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `createPropertyDescription`.
fn dump_property(desc: &DataDescription) -> JsonValue {
    let flags = desc.data_flags;
    JsonValue::Object(vec![
        ("type".into(), JsonValue::Str(desc.data_type.type_name())),
        ("name".into(), JsonValue::Str(desc.name().into())),
        ("index".into(), JsonValue::Int(desc.index as i64)),
        (
            "clientServerFullIndex".into(),
            JsonValue::Int(desc.client_server_full_index as i64),
        ),
        (
            "isGhostedData".into(),
            JsonValue::Bool(flags.is_ghosted_data()),
        ),
        (
            "isOtherClientData".into(),
            JsonValue::Bool(flags.is_other_client_data()),
        ),
        (
            "isOwnClientData".into(),
            JsonValue::Bool(flags.is_own_client_data()),
        ),
        ("isCellData".into(), JsonValue::Bool(flags.is_cell_data())),
        ("isBaseData".into(), JsonValue::Bool(flags.is_base_data())),
        (
            "isClientServerData".into(),
            JsonValue::Bool(flags.is_client_server_data()),
        ),
        (
            "isPersistent".into(),
            JsonValue::Bool(flags.is_persistent()),
        ),
        (
            "isIdentifier".into(),
            JsonValue::Bool(flags.is_identifier()),
        ),
        (
            "isIndexed".into(),
            JsonValue::Bool(desc.database_indexing.is_indexed()),
        ),
        (
            "isUnique".into(),
            JsonValue::Bool(desc.database_indexing.is_unique()),
        ),
        (
            "streamSize".into(),
            JsonValue::Int(desc.stream_size() as i64),
        ),
        ("isConst".into(), JsonValue::Bool(desc.data_type.is_const())),
    ])
}

/// `argumentTypesAsScript` / `returnValueTypesAsScript` shape:
/// a sequence of `[argName, typeName]` pairs.
fn dump_args(args: &MethodArgs) -> JsonValue {
    JsonValue::Array(
        args.args()
            .iter()
            .map(|(component, data_type)| {
                JsonValue::Array(vec![
                    JsonValue::Str(component.clone()),
                    JsonValue::Str(data_type.type_name()),
                ])
            })
            .collect(),
    )
}

/// `createMethodDescriptions`.
fn dump_method(desc: &MethodDescription) -> JsonValue {
    let mut entries = vec![
        ("name".into(), JsonValue::Str(desc.name().into())),
        ("isExposed".into(), JsonValue::Bool(desc.is_exposed())),
        (
            "internalIndex".into(),
            JsonValue::Int(desc.internal_index() as i64),
        ),
        (
            "exposedIndex".into(),
            JsonValue::Int(desc.exposed_index() as i64),
        ),
        ("args".into(), dump_args(desc.args())),
    ];
    if desc.has_return_values() {
        entries.push(("returnValues".into(), dump_args(desc.return_values())));
    }
    entries.push((
        "streamSize".into(),
        JsonValue::Int(desc.stream_size(true) as i64),
    ));
    JsonValue::Object(entries)
}

/// `createEntityDescription`. The `has*Script` keys alias the `canBeOn*`
/// getters, exactly as the reference's ADD_PROPERTY_NAMED does.
fn dump_entity(desc: &EntityDescription) -> JsonValue {
    let methods = |table: &crate::method::EntityMethodDescriptions| {
        JsonValue::Array(table.internal_methods().iter().map(dump_method).collect())
    };
    let ordered_properties = |domains: u32| {
        let mut items = Vec::new();
        desc.visit(domains, &mut |property| {
            items.push(dump_property(property));
            true
        });
        JsonValue::Array(items)
    };

    JsonValue::Object(vec![
        ("name".into(), JsonValue::Str(desc.name.clone())),
        ("index".into(), JsonValue::Int(desc.index() as i64)),
        (
            "clientIndex".into(),
            JsonValue::Int(desc.client_index() as i64),
        ),
        (
            "hasClientScript".into(),
            JsonValue::Bool(desc.can_be_on_client()),
        ),
        (
            "hasBaseScript".into(),
            JsonValue::Bool(desc.can_be_on_base()),
        ),
        (
            "hasCellScript".into(),
            JsonValue::Bool(desc.can_be_on_cell()),
        ),
        (
            "canBeOnClient".into(),
            JsonValue::Bool(desc.can_be_on_client()),
        ),
        ("canBeOnBase".into(), JsonValue::Bool(desc.can_be_on_base())),
        ("canBeOnCell".into(), JsonValue::Bool(desc.can_be_on_cell())),
        ("isService".into(), JsonValue::Bool(desc.is_service())),
        ("isPersistent".into(), JsonValue::Bool(desc.is_persistent())),
        ("clientMethods".into(), methods(desc.client())),
        ("baseMethods".into(), methods(desc.base())),
        ("cellMethods".into(), methods(desc.cell())),
        (
            "allProperties".into(),
            JsonValue::Array(desc.properties().iter().map(dump_property).collect()),
        ),
        ("clientProperties".into(), ordered_properties(CLIENT_DATA)),
        (
            "baseToClientProperties".into(),
            ordered_properties(FROM_BASE_TO_CLIENT_DATA),
        ),
        (
            "cellToClientProperties".into(),
            ordered_properties(FROM_CELL_TO_CLIENT_DATA),
        ),
    ])
}

/// The whole payload: entity types in name order (the reference iterates its
/// `std::map`), then the constants block with the definition digest.
pub fn dump_map(map: &EntityDescriptionMap) -> String {
    let entity_types = JsonValue::Array(
        map.name_to_index_entries()
            .iter()
            .map(|(_, index)| {
                dump_entity(
                    map.entity_description(*index)
                        .expect("index from the map is valid"),
                )
            })
            .collect(),
    );

    let payload = JsonValue::Object(vec![
        (
            "constants".into(),
            JsonValue::Object(vec![
                ("digest".into(), JsonValue::Str(map.digest().into())),
                (
                    "maxClientServerPropertyCount".into(),
                    JsonValue::Int(map.max_client_server_property_count() as i64),
                ),
                (
                    "maxExposedBaseMethodCount".into(),
                    JsonValue::Int(map.max_exposed_base_method_count() as i64),
                ),
                (
                    "maxExposedCellMethodCount".into(),
                    JsonValue::Int(map.max_exposed_cell_method_count() as i64),
                ),
                (
                    "maxExposedClientMethodCount".into(),
                    JsonValue::Int(map.max_exposed_client_method_count() as i64),
                ),
            ]),
        ),
        ("entityTypes".into(), entity_types),
    ]);

    let mut text = payload.to_json_text();
    text.push('\n');
    text
}

/// The `process_defs` flow: aliases, then `entities.xml` with the
/// dump-equivalent (all-`None`) message ranges.
pub fn load_and_dump(root: &Path, diag: &mut Diagnostics) -> Result<String, String> {
    let resolver = FsResolver::new(root);

    let mut ctx = TypeContext::default();
    ctx.init_aliases(|| resolver.open_section(ALIASES_FILE), diag);

    let entities = resolver
        .open_section(ENTITIES_FILE)
        .ok_or_else(|| format!("Failed to open {ENTITIES_FILE} under {}", root.display()))?;

    let mut map = EntityDescriptionMap::new();
    if !map.parse(
        Some(&entities),
        &resolver,
        &ctx,
        diag,
        &MessageRanges::default(),
    ) {
        return Err("Failed to parse .def files".to_string());
    }

    Ok(dump_map(&map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_text_matches_the_python_reference_shape() {
        let value = JsonValue::Object(vec![
            ("b".into(), JsonValue::Int(1)),
            ("a".into(), JsonValue::Object(Vec::new())),
            (
                "list".into(),
                JsonValue::Array(vec![
                    JsonValue::Bool(true),
                    JsonValue::Str("s\"\\\n".into()),
                    JsonValue::Array(Vec::new()),
                ]),
            ),
        ]);
        let expected = [
            "{\n",
            " \"a\": {},\n",
            " \"b\": 1,\n",
            " \"list\": [\n",
            "  true,\n",
            "  \"s\\\"\\\\\\n\",\n",
            "  []\n",
            " ]\n",
            "}",
        ]
        .concat();
        assert_eq!(value.to_json_text(), expected);
    }

    #[test]
    fn empty_containers_stay_on_one_line() {
        assert_eq!(JsonValue::Object(Vec::new()).to_json_text(), "{}");
        assert_eq!(JsonValue::Array(Vec::new()).to_json_text(), "[]");
    }

    #[test]
    fn control_characters_are_escaped_like_python() {
        assert_eq!(
            JsonValue::Str("line\r\u{1f}end".into()).to_json_text(),
            "\"line\\r\\u001fend\""
        );
        assert_eq!(
            JsonValue::Str("a\tb\nc".into()).to_json_text(),
            "\"a\\tb\\nc\""
        );
    }

    fn scratch_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("entitydef_dump_test_{}", std::process::id()))
    }

    #[test]
    fn load_and_dump_reports_a_missing_entities_file() {
        let dir = scratch_root().join("no_entities");
        std::fs::create_dir_all(&dir).unwrap();

        let mut diag = Diagnostics::new();
        let err = load_and_dump(&dir, &mut diag).unwrap_err();
        assert!(err.contains("Failed to open scripts/entities.xml"), "{err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_and_dump_reports_an_unparseable_definition_set() {
        let dir = scratch_root().join("broken");
        std::fs::create_dir_all(dir.join("scripts/entity_defs")).unwrap();
        std::fs::write(
            dir.join("scripts/entities.xml"),
            "<root> <ClientServerEntities> <Ghost/> </ClientServerEntities> \
             <ServerOnlyEntities/> </root>",
        )
        .unwrap();
        // No entity_defs/Ghost.def: the parse fails.

        let mut diag = Diagnostics::new();
        let err = load_and_dump(&dir, &mut diag).unwrap_err();
        assert_eq!(err, "Failed to parse .def files");
        assert!(!diag.errors().is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_two_way_method_dumps_its_return_values() {
        let dir = scratch_root().join("two_way");
        std::fs::create_dir_all(dir.join("scripts/entity_defs")).unwrap();
        std::fs::create_dir_all(dir.join("scripts/base")).unwrap();
        std::fs::write(
            dir.join("scripts/entities.xml"),
            "<root> <ClientServerEntities> <Actor/> </ClientServerEntities> \
             <ServerOnlyEntities/> </root>",
        )
        .unwrap();
        std::fs::write(
            dir.join("scripts/entity_defs/Actor.def"),
            "<root> <BaseMethods> \
                 <query> <ReturnValues> <answer> INT32 </answer> </ReturnValues> </query> \
             </BaseMethods> </root>",
        )
        .unwrap();
        std::fs::write(dir.join("scripts/base/Actor.py"), "# base script").unwrap();

        let mut diag = Diagnostics::new();
        let text = load_and_dump(&dir, &mut diag).expect("parses and dumps");
        assert!(text.contains("\"returnValues\""), "{text}");
        assert!(!diag.has_errors());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
