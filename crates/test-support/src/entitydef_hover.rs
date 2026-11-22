//! BW-27-01-04: hover information for an EntityDef property type.
//!
//! Hovering a property's type means the editor knows a file and a cursor line
//! and expects an answer about the declaration under it. `hover` resolves the
//! declaration whose span contains the line — the same spans the index and the
//! diagnostics use — then pairs its declared `Type` with one line of
//! documentation for it. Lines outside every declaration (the root element,
//! the block closers, the gaps) hover to nothing.

use crate::entitydef_index::EntityDefIndex;

/// What a hover over a property's type shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    /// The property whose type is hovered.
    pub property: String,
    /// The EntityDef type the property declares.
    pub type_name: String,
    /// One line of documentation for that type.
    pub documentation: String,
}

/// Hover one line of one indexed file: the declaration that spans the line,
/// with its type and that type's documentation.
pub fn hover(index: &EntityDefIndex, path: &str, line: usize) -> Option<Hover> {
    let file = index.file(path)?;
    let property = file
        .properties
        .iter()
        .find(|p| line >= p.line && line < p.end_line)?;
    Some(Hover {
        property: property.name.clone(),
        type_name: property.type_name.clone(),
        documentation: type_documentation(&property.type_name),
    })
}

/// One line of documentation per EntityDef type. An unrecognised type still
/// hovers, naming itself as the reason.
fn type_documentation(type_name: &str) -> String {
    match type_name {
        "ARRAY" => "variable-length list of one element type".to_string(),
        "ENTITY_TYPE" => "id of an entity type".to_string(),
        "FIXED_DICT" => "fixed set of named fields".to_string(),
        "FLOAT32" => "32-bit floating-point number".to_string(),
        "FLOAT64" => "64-bit floating-point number".to_string(),
        "MAILBOX" => "reference to an entity elsewhere in the world".to_string(),
        "PYTHON" => "arbitrary Python object; not replicated to clients".to_string(),
        "STRING" => "UTF-8 string, replicated as a whole".to_string(),
        "VECTOR2" => "two-component floating-point vector".to_string(),
        "VECTOR3" => "three-component floating-point vector".to_string(),
        "VECTOR4" => "four-component floating-point vector".to_string(),
        other if other.starts_with("INT") => "signed integer".to_string(),
        other if other.starts_with("UINT") => "unsigned integer".to_string(),
        _ => format!("unknown type '{type_name}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVATAR: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tUINT32\t</Type>\n\t\t</hp>\n\t</Properties>\n</root>\n";

    const FIXED_DICT: &str = "<root>\n\t<Properties>\n\t\t<pos>\n\t\t\t<Type>\tFIXED_DICT\n\t\t\t\t<Properties>\n\t\t\t\t\t<x>\n\t\t\t\t\t\t<Type>\tFLOAT32\t</Type>\n\t\t\t\t\t</x>\n\t\t\t\t</Properties>\n\t\t\t</Type>\n\t\t\t<Flags>\tCELL_PUBLIC_AND_OWN\t</Flags>\n\t\t</pos>\n\t</Properties>\n</root>\n";

    const PATH: &str = "res/scripts/entity_defs/Avatar.def";

    fn index_with(content: &str) -> EntityDefIndex {
        let mut index = EntityDefIndex::new();
        index.refresh(PATH, content);
        index
    }

    #[test]
    fn hovering_a_type_line_shows_the_declared_type_and_its_documentation() {
        let index = index_with(AVATAR);

        // The `<Type>` line sits inside the declaration it belongs to.
        let name = hover(&index, PATH, 3).expect("name's type line");
        assert_eq!(name.property, "name");
        assert_eq!(name.type_name, "STRING");
        assert_eq!(name.documentation, "UTF-8 string, replicated as a whole");

        let hp = hover(&index, PATH, 6).expect("hp's type line");
        assert_eq!(hp.property, "hp");
        assert_eq!(hp.type_name, "UINT32");
        assert_eq!(hp.documentation, "unsigned integer");
    }

    #[test]
    fn hover_resolves_the_declaration_under_the_cursor() {
        let index = index_with(AVATAR);

        // The open tag is inside the span as much as the type line.
        assert_eq!(hover(&index, PATH, 2).unwrap().property, "name");
        assert_eq!(hover(&index, PATH, 7).unwrap().property, "hp");
    }

    #[test]
    fn a_nested_type_hovers_as_the_type_it_belongs_to() {
        let index = index_with(FIXED_DICT);

        // The nested <Properties> is the type's structure: hovering anywhere in
        // the declaration, on a field's type included, hovers pos and FIXED_DICT.
        assert_eq!(hover(&index, PATH, 3).unwrap().type_name, "FIXED_DICT");
        assert_eq!(hover(&index, PATH, 6).unwrap().type_name, "FIXED_DICT");
        assert_eq!(
            hover(&index, PATH, 3).unwrap().documentation,
            "fixed set of named fields"
        );
    }

    #[test]
    fn lines_outside_every_declaration_do_not_hover() {
        let index = index_with(AVATAR);

        // The root element, the block closers and a gap hover to nothing.
        assert!(hover(&index, PATH, 0).is_none());
        assert!(hover(&index, PATH, 8).is_none());
        assert!(hover(&index, PATH, 4).is_some());
        assert!(hover(&index, PATH, 500).is_none());
        assert!(hover(&index, "res/scripts/entity_defs/Other.def", 3).is_none());
    }
}
