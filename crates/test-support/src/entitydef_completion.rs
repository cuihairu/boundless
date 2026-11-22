//! BW-27-01-03: property completion for an entity definition.
//!
//! Editing a `.def` file, a field context belongs to one entity: its property
//! names are the candidates. Completion is scoped to that entity — an
//! `Avatar.def` field context never suggests a property only `Npc.def`
//! declares, even when the index holds both files.

use crate::entitydef_index::EntityDefIndex;

/// One completion candidate: a property of the entity being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The property's name.
    pub label: String,
    /// The 0-based line the property is declared on.
    pub line: usize,
}

/// The candidates for an entity's field context, in declaration order and
/// narrowed to those starting with `prefix`. An entity the index does not
/// know — never indexed, or deleted since — completes to nothing, and so does
/// one whose name is declared in several files: guessing would mix their
/// candidates, so completion needs the open path to be sure.
pub fn complete(index: &EntityDefIndex, entity: &str, prefix: &str) -> Vec<Completion> {
    crate::entitydef_lookup::resolve(index, entity, "")
        .map(|path| complete_in_file(index, &path, prefix))
        .unwrap_or_default()
}

/// The candidates of one file's field context: what the editor of that file
/// sees, and nothing of a same-stem file in another root.
pub fn complete_in_file(index: &EntityDefIndex, path: &str, prefix: &str) -> Vec<Completion> {
    index
        .file(path)
        .map(|file| {
            file.properties
                .iter()
                .filter(|property| property.name.starts_with(prefix))
                .map(|property| Completion {
                    label: property.name.clone(),
                    line: property.line,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVATAR: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tHP\t</Type>\n\t\t</hp>\n\t\t<mood>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</mood>\n\t</Properties>\n</root>\n";

    const NPC: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<level>\n\t\t\t<Type>\tUINT32\t</Type>\n\t\t</level>\n\t\t<faction>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</faction>\n\t</Properties>\n</root>\n";

    fn index_with_both() -> EntityDefIndex {
        let mut index = EntityDefIndex::new();
        index.refresh("res/scripts/entity_defs/Avatar.def", AVATAR);
        index.refresh("res/scripts/entity_defs/Npc.def", NPC);
        index
    }

    fn labels(completions: &[Completion]) -> Vec<&str> {
        completions.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn a_field_context_only_sees_its_own_entitys_candidates() {
        let index = index_with_both();

        // Avatar's field context offers only Avatar's properties.
        let avatar = complete(&index, "Avatar", "");
        assert_eq!(labels(&avatar), vec!["name", "hp", "mood"]);

        // Npc's field context offers only Npc's — never a leaked Avatar
        // property, even though the index holds both files.
        let npc = complete(&index, "Npc", "");
        assert_eq!(labels(&npc), vec!["name", "level", "faction"]);
    }

    #[test]
    fn a_prefix_narrows_the_candidates() {
        let index = index_with_both();
        assert_eq!(labels(&complete(&index, "Avatar", "na")), vec!["name"]);
        // The match is exact: "HP" is not "hp".
        assert!(complete(&index, "Avatar", "HP").is_empty());
    }

    #[test]
    fn an_unknown_or_deleted_entity_completes_to_nothing() {
        let mut index = index_with_both();
        assert!(complete(&index, "Monster", "").is_empty());

        // A deletion drops the candidates with the entry.
        index.remove("res/scripts/entity_defs/Avatar.def");
        assert!(complete(&index, "Avatar", "").is_empty());
    }

    #[test]
    fn candidates_carry_the_declaration_line() {
        let index = index_with_both();
        let hp = &complete(&index, "Avatar", "hp")[0];
        assert_eq!((hp.label.as_str(), hp.line), ("hp", 5));
    }
}
