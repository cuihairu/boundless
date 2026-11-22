//! BW-27-01-05: jump-to-definition for an entity reference.
//!
//! A script names an entity (and often one of its properties) and the editor
//! jumps to where that is declared. `definition_of` resolves the reference
//! against the index: the `.def` file that declares the entity, and — when the
//! reference names a property — the range that property occupies in it. An
//! entity the index does not know, or a property that entity does not declare,
//! has nowhere to jump to.

use crate::entitydef_diagnostics::{declaration_range, tag_range};
use crate::entitydef_index::EntityDefIndex;

/// Where a definition lives: the file it is declared in, and the range it
/// occupies there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The `.def` file the definition lives in.
    pub path: String,
    /// The property's name, when the reference names a property.
    pub property: Option<String>,
    /// The 0-based line the definition starts on.
    pub line: usize,
    /// The 0-based column the definition starts at.
    pub column: usize,
    /// The 0-based line the definition's range ends on.
    pub end_line: usize,
    /// The 0-based column just past the definition's range.
    pub end_column: usize,
}

/// The definition a script reference jumps to. `property` selects a field of
/// the entity — its declared span in that entity's file — while a `None`
/// property is the entity itself, at its root element. An entity declared in
/// several files (a rename, a second root) does not jump by name alone: that
/// would mix the same-stem files, so it needs the open path to be sure.
pub fn definition_of(
    index: &EntityDefIndex,
    entity: &str,
    property: Option<&str>,
) -> Option<Definition> {
    crate::entitydef_lookup::resolve(index, entity, "")
        .and_then(|path| definition_of_file(index, &path, property))
}

/// The definition in one named file: what the editor of that file jumps to,
/// and nothing of a same-stem file in another root.
pub fn definition_of_file(
    index: &EntityDefIndex,
    path: &str,
    property: Option<&str>,
) -> Option<Definition> {
    let file = index.file(path)?;
    let lines = &file.lines;
    let (line, column, end_line, end_column) = match property {
        None => tag_range(lines, 0),
        Some(name) => {
            let property = file.properties.iter().find(|p| p.name == name)?;
            declaration_range(lines, property)
        }
    };
    Some(Definition {
        path: file.path.clone(),
        property: property.map(str::to_string),
        line,
        column,
        end_line,
        end_column,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVATAR: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tUINT32\t</Type>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tUINT32\t</Type>\n\t\t</hp>\n\t</Properties>\n</root>\n";

    const AVATAR_EDITED: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t\t<Flags>\tOTHER_CLIENTS\t</Flags>\n\t\t</name>\n\t\t<mood>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</mood>\n\t\t<hp>\n\t\t\t<Type>\tUINT32\t</Type>\n\t\t</hp>\n\t</Properties>\n</root>\n";

    const PATH: &str = "res/scripts/entity_defs/Avatar.def";

    fn index_with(content: &str) -> EntityDefIndex {
        let mut index = EntityDefIndex::new();
        index.refresh(PATH, content);
        index
    }

    #[test]
    fn a_property_reference_jumps_to_its_field_range() {
        let index = index_with(AVATAR);
        let jump = definition_of(&index, "Avatar", Some("hp")).expect("hp is declared");
        assert_eq!(jump.path, PATH);
        assert_eq!(jump.property.as_deref(), Some("hp"));
        assert_eq!((jump.line, jump.column), (5, 2));
        assert_eq!((jump.end_line, jump.end_column), (7, 7));
    }

    #[test]
    fn an_entity_reference_jumps_to_its_root_element() {
        let index = index_with(AVATAR);
        let jump = definition_of(&index, "Avatar", None).expect("Avatar is declared");
        assert_eq!(jump.property, None);
        assert_eq!((jump.line, jump.column), (0, 0));
        assert_eq!((jump.end_line, jump.end_column), (0, 6));
    }

    #[test]
    fn an_unknown_entity_or_property_jumps_nowhere() {
        let index = index_with(AVATAR);
        assert!(definition_of(&index, "Monster", None).is_none());
        assert!(definition_of(&index, "Monster", Some("hp")).is_none());
        assert!(definition_of(&index, "Avatar", Some("mood")).is_none());
    }

    #[test]
    fn a_jump_follows_the_refreshed_content() {
        let mut index = index_with(AVATAR);
        // Before the edit hp is the second declaration.
        let first = definition_of(&index, "Avatar", Some("hp")).expect("hp before edit");
        assert_eq!((first.line, first.column), (5, 2));

        // The editor edits the file: a longer first declaration pushes hp down,
        // and the jump lands where it is now.
        index.refresh(PATH, AVATAR_EDITED);
        let second = definition_of(&index, "Avatar", Some("hp")).expect("hp after edit");
        assert_eq!((second.line, second.column), (9, 2));
        assert_eq!((second.end_line, second.end_column), (11, 7));
        assert_eq!(
            definition_of(&index, "Avatar", Some("mood"))
                .expect("mood is declared after edit")
                .line,
            6
        );
    }
}
