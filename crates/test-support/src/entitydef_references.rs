//! BW-27-01-06: cross-file references to an entity definition.
//!
//! The language tool indexes the scripts too: each script belongs to an entity,
//! and a definition's usages are the places its scripts touch the entity or one
//! of its properties. `references` finds the field accesses (`self.hp`,
//! `target.hp`) and `entity_references` the bare name uses (`Avatar()`,
//! `Avatar.create`) — every hit carrying the path and position it occurred at,
//! so the editor can list and jump to them. A word that merely *contains* the
//! name (`self.maxhp`) is not a use.

use std::collections::BTreeMap;

/// One place a definition is used in a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The script the use appears in.
    pub path: String,
    /// The 0-based line the use starts on.
    pub line: usize,
    /// The 0-based column the use starts at.
    pub column: usize,
    /// The 0-based column just past the use.
    pub end_column: usize,
}

/// One indexed script: its path, the entity it belongs to, and its content.
#[derive(Debug, Clone)]
struct ScriptFile {
    path: String,
    entity: String,
    lines: Vec<String>,
}

/// The scripts indexed, keyed by path.
#[derive(Default)]
pub struct ScriptIndex {
    scripts: BTreeMap<String, ScriptFile>,
}

impl ScriptIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Index one script under the entity it belongs to, replacing whatever the
    /// path held. Returns a reference to the stored script.
    pub fn refresh(&mut self, path: &str, entity: &str, content: &str) {
        self.scripts.insert(
            path.to_string(),
            ScriptFile {
                path: path.to_string(),
                entity: entity.to_string(),
                lines: content.lines().map(str::to_string).collect(),
            },
        );
    }

    /// Drop a script after its deletion. Returns whether the path was indexed.
    pub fn remove(&mut self, path: &str) -> bool {
        self.scripts.remove(path).is_some()
    }

    /// How many scripts are indexed.
    pub fn count(&self) -> usize {
        self.scripts.len()
    }

    /// The field accesses of one property, across every script of the entity,
    /// in path then line order.
    pub fn references(&self, entity: &str, property: &str) -> Vec<Reference> {
        let mut found = Vec::new();
        for script in self.scripts.values().filter(|s| s.entity == entity) {
            for (line, text) in script.lines.iter().enumerate() {
                for (column, word) in field_words(text) {
                    if word == property {
                        found.push(Reference {
                            path: script.path.clone(),
                            line,
                            column,
                            end_column: column + property.len(),
                        });
                    }
                }
            }
        }
        found
    }

    /// The bare uses of an entity's name, across every script of the entity,
    /// in path then line order.
    pub fn entity_references(&self, entity: &str) -> Vec<Reference> {
        let mut found = Vec::new();
        for script in self.scripts.values().filter(|s| s.entity == entity) {
            for (line, text) in script.lines.iter().enumerate() {
                for column in word_columns(text, entity) {
                    found.push(Reference {
                        path: script.path.clone(),
                        line,
                        column,
                        end_column: column + entity.len(),
                    });
                }
            }
        }
        found
    }
}

/// The field accesses in one line: each word that follows a `.`, with the
/// column it starts at.
fn field_words(line: &str) -> Vec<(usize, &str)> {
    let bytes = line.as_bytes();
    let mut words = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                words.push((start, &line[start..end]));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    words
}

/// The columns a word stands alone at: not part of a longer identifier.
fn word_columns(line: &str, word: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let word = word.as_bytes();
    let mut columns = Vec::new();
    let mut i = 0;
    while i + word.len() <= bytes.len() {
        if &bytes[i..i + word.len()] == word {
            let before = i == 0 || !is_identifier(bytes[i - 1]);
            let after = i + word.len() == bytes.len() || !is_identifier(bytes[i + word.len()]);
            if before && after {
                columns.push(i);
            }
        }
        i += 1;
    }
    columns
}

fn is_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVATAR: &str = "#! /usr/bin/env python\ndef onEnterWorld(self):\n    self.hp = 100\n    self.hp += 1\n    Avatar()\n";

    const COMBAT: &str = "def hit(self, damage):\n    if self.hp > 0:\n        self.hp -= damage\n    Avatar.create()\n";

    const NPC: &str = "def onEnterWorld(self):\n    self.hp = 0\n";

    fn index_with() -> ScriptIndex {
        let mut scripts = ScriptIndex::new();
        scripts.refresh("res/scripts/entities/Avatar.py", "Avatar", AVATAR);
        scripts.refresh("res/scripts/entities/Avatar_combat.py", "Avatar", COMBAT);
        scripts.refresh("res/scripts/entities/Npc.py", "Npc", NPC);
        scripts
    }

    fn points(references: &[Reference]) -> Vec<(String, usize, usize)> {
        references
            .iter()
            .map(|r| (r.path.clone(), r.line, r.column))
            .collect()
    }

    #[test]
    fn a_property_usage_is_found_in_every_script_that_references_it() {
        let scripts = index_with();

        // Both of Avatar's scripts use hp, in path then line order.
        let hp = scripts.references("Avatar", "hp");
        assert_eq!(
            points(&hp),
            vec![
                ("res/scripts/entities/Avatar.py".to_string(), 2, 9),
                ("res/scripts/entities/Avatar.py".to_string(), 3, 9),
                ("res/scripts/entities/Avatar_combat.py".to_string(), 1, 12),
                ("res/scripts/entities/Avatar_combat.py".to_string(), 2, 13),
            ]
        );
        // Npc's use of its own hp is not a use of Avatar's.
        assert_eq!(scripts.references("Npc", "hp").len(), 1);
    }

    #[test]
    fn a_word_that_only_contains_the_property_is_not_a_use() {
        let mut scripts = ScriptIndex::new();
        scripts.refresh(
            "Avatar.py",
            "Avatar",
            "def f(self):\n    self.hp_max = 1\n    self.maxhp = 2\n    self.hp = 3\n",
        );
        let hp = scripts.references("Avatar", "hp");
        // hp_max and maxhp are different words; only the real hp is a use.
        assert_eq!(points(&hp), vec![("Avatar.py".to_string(), 3, 9)]);
    }

    #[test]
    fn an_entity_name_use_is_found_across_scripts() {
        let scripts = index_with();
        let uses = scripts.entity_references("Avatar");
        assert_eq!(
            points(&uses),
            vec![
                ("res/scripts/entities/Avatar.py".to_string(), 4, 4),
                ("res/scripts/entities/Avatar_combat.py".to_string(), 3, 4),
            ]
        );
    }

    #[test]
    fn scripts_follow_refresh_and_removal() {
        let mut scripts = ScriptIndex::new();
        scripts.refresh("Avatar.py", "Avatar", "def f(self):\n    self.hp = 0\n");
        assert_eq!(scripts.references("Avatar", "hp").len(), 1);

        // An edit adds a use: the index updates in place.
        scripts.refresh(
            "Avatar.py",
            "Avatar",
            "def f(self):\n    self.hp = 0\n    self.hp += 1\n",
        );
        assert_eq!(scripts.references("Avatar", "hp").len(), 2);

        // A deletion removes the uses with the script.
        assert!(scripts.remove("Avatar.py"));
        assert!(scripts.references("Avatar", "hp").is_empty());
        assert_eq!(scripts.count(), 0);
    }
}
