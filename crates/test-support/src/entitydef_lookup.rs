//! BW-27-01-07: resolving a definition across renames and multiple roots.
//!
//! A rename leaves the old path behind for a while, and two project roots can
//! each declare an `Avatar.def`. Looking a file up by entity name alone would
//! pick one same-stem file and mix its results into the other's, so resolution
//! is file-first: the open path wins, an entity with a lone candidate resolves
//! to it, and an entity with several candidates and no open path resolves to
//! nothing rather than guessing.

use crate::entitydef_index::EntityDefIndex;

/// The file an entity reference resolves to. The `open` path is the file the
/// editor is looking at: when it declares the entity it always wins, so a
/// same-stem file in another root never bleeds in. Without a hint, only a
/// lone candidate resolves.
pub fn resolve(index: &EntityDefIndex, entity: &str, open: &str) -> Option<String> {
    let candidates = index.file_of_all(entity);
    match candidates.len() {
        0 => None,
        1 => Some(candidates[0].to_string()),
        _ => candidates
            .iter()
            .copied()
            .find(|path| *path == open)
            .map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entitydef_completion::complete_in_file;
    use crate::entitydef_definition::definition_of_file;

    const AVATAR_HP: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tUINT32\t</Type>\n\t\t</hp>\n\t</Properties>\n</root>\n";

    const AVATAR_MOOD: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<mood>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</mood>\n\t</Properties>\n</root>\n";

    fn two_roots() -> EntityDefIndex {
        let mut index = EntityDefIndex::new();
        index.refresh("rootA/Avatar.def", AVATAR_HP);
        index.refresh("rootB/Avatar.def", AVATAR_MOOD);
        index
    }

    fn labels(completions: &[crate::entitydef_completion::Completion]) -> Vec<&str> {
        completions.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn a_renamed_file_resolves_its_new_stem() {
        let mut index = EntityDefIndex::new();
        index.refresh("rootA/Avatar.def", AVATAR_HP);

        // Avatar.def is renamed to Player.def: the old path is removed.
        index.refresh("rootA/Player.def", AVATAR_HP);
        index.remove("rootA/Avatar.def");

        assert_eq!(
            resolve(&index, "Player", "").as_deref(),
            Some("rootA/Player.def")
        );
        // The old name is gone with the old path.
        assert!(resolve(&index, "Avatar", "").is_none());
    }

    #[test]
    fn same_stem_files_in_two_roots_resolve_by_the_open_path() {
        let index = two_roots();

        // Each root's own file wins when it is the one open.
        assert_eq!(
            resolve(&index, "Avatar", "rootA/Avatar.def").as_deref(),
            Some("rootA/Avatar.def")
        );
        assert_eq!(
            resolve(&index, "Avatar", "rootB/Avatar.def").as_deref(),
            Some("rootB/Avatar.def")
        );
        // With no open path, several candidates do not guess.
        assert!(resolve(&index, "Avatar", "").is_none());
    }

    #[test]
    fn completion_and_jumps_use_only_the_open_files_own_declarations() {
        let index = two_roots();

        // Completion in rootA sees only rootA's properties; rootB's only its
        // own — the two same-stem files never mix.
        assert_eq!(
            labels(&complete_in_file(&index, "rootA/Avatar.def", "")),
            vec!["name", "hp"]
        );
        assert_eq!(
            labels(&complete_in_file(&index, "rootB/Avatar.def", "")),
            vec!["name", "mood"]
        );

        // A jump follows the same rule: rootA has hp, rootB does not.
        assert!(definition_of_file(&index, "rootA/Avatar.def", Some("hp")).is_some());
        assert!(definition_of_file(&index, "rootB/Avatar.def", Some("hp")).is_none());
    }

    #[test]
    fn an_entity_name_lookup_never_guesses_between_same_stems() {
        let index = two_roots();

        // By name alone the entity is ambiguous: completion and jumps decline
        // rather than pick one file and mix it into the other.
        assert!(crate::entitydef_completion::complete(&index, "Avatar", "").is_empty());
        assert!(crate::entitydef_definition::definition_of(&index, "Avatar", Some("hp")).is_none());
    }

    #[test]
    fn a_lone_candidate_resolves_without_an_open_path() {
        let mut index = EntityDefIndex::new();
        index.refresh("rootA/Avatar.def", AVATAR_HP);
        assert_eq!(
            resolve(&index, "Avatar", "").as_deref(),
            Some("rootA/Avatar.def")
        );
        // The by-name completion is unchanged when the entity is unique.
        assert_eq!(
            labels(&crate::entitydef_completion::complete(&index, "Avatar", "")),
            vec!["name", "hp"]
        );
    }
}
