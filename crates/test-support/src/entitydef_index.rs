//! BW-27-01-01: the incremental EntityDef index behind the language tools.
//!
//! An editor sends one file at a time: opened, edited, deleted. The index
//! keeps one entry per `.def` file — its entity name (the file stem), the
//! properties the file declares with the lines they span, and a fingerprint of
//! the content so an unchanged re-send can be skipped. A refresh replaces one
//! file's entry in place; a deletion drops it — the index never requires a
//! full rescan to stay current.

/// One indexed property declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedProperty {
    /// The property's name, from its opening tag.
    pub name: String,
    /// The `Flags` the declaration carries, when it declares them.
    pub flags: String,
    /// The 0-based line the declaration opens on.
    pub line: usize,
    /// The 0-based line just past the declaration's close.
    pub end_line: usize,
}

/// One indexed `.def` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    /// The file's path as the editor named it.
    pub path: String,
    /// The entity the file declares: its stem.
    pub entity: String,
    /// The properties the file declares, in declaration order.
    pub properties: Vec<IndexedProperty>,
    /// A fingerprint of the indexed content, so an unchanged re-send is
    /// detectable without re-reading.
    pub fingerprint: u64,
}

/// The `.def` files indexed, keyed by path.
#[derive(Default)]
pub struct EntityDefIndex {
    files: std::collections::BTreeMap<String, IndexedFile>,
}

impl EntityDefIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-index one file's content, replacing whatever the path held. The
    /// entity name is the file's stem.
    pub fn refresh(&mut self, path: &str, content: &str) -> &IndexedFile {
        let entity = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .strip_suffix(".def")
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(path))
            .to_string();
        let scanned = scan(content);
        self.files.insert(
            path.to_string(),
            IndexedFile {
                path: path.to_string(),
                entity,
                properties: scanned.properties,
                fingerprint: scanned.fingerprint,
            },
        );
        self.files.get(path).expect("entry just inserted")
    }

    /// Drop a file's entry after its deletion. Returns whether the path was
    /// indexed.
    pub fn remove(&mut self, path: &str) -> bool {
        self.files.remove(path).is_some()
    }

    /// How many files are indexed.
    pub fn count(&self) -> usize {
        self.files.len()
    }

    /// The indexed entity names, in path order.
    pub fn entities(&self) -> Vec<&str> {
        self.files
            .values()
            .map(|file| file.entity.as_str())
            .collect()
    }

    /// The path that declares an entity, by name.
    pub fn file_of(&self, entity: &str) -> Option<&str> {
        self.files
            .values()
            .find(|file| file.entity == entity)
            .map(|file| file.path.as_str())
    }

    /// One indexed file, by path.
    pub fn file(&self, path: &str) -> Option<&IndexedFile> {
        self.files.get(path)
    }
}

/// One scan's outcome: the declared properties, the content's fingerprint, and
/// the places a diagnostic can point.
#[derive(Default, Debug, Clone)]
pub(crate) struct Scan {
    /// The properties declared in the root `<Properties>` block.
    pub(crate) properties: Vec<IndexedProperty>,
    /// A fingerprint of the scanned content.
    pub(crate) fingerprint: u64,
    /// The property left open at the content's end, when one was: its name
    /// and the line it opened on.
    pub(crate) unclosed_property: Option<(String, usize)>,
    /// The line the root `<Properties>` opened on, when it was never closed.
    pub(crate) unclosed_properties: Option<usize>,
    /// The properties that never declare a `<Type>`, each as name and line.
    pub(crate) missing_type: Vec<(String, usize)>,
}

/// Scan one file's content: the properties declared in its root
/// `<Properties>` block, with their line spans, and the content's fingerprint.
pub(crate) fn scan(content: &str) -> Scan {
    let mut properties = Vec::new();
    let mut depth: usize = 0;
    let mut root_open: Option<usize> = None;
    let mut open: Option<(String, usize)> = None;
    let mut flags = String::new();
    let mut has_type = false;
    let mut missing_type: Vec<(String, usize)> = Vec::new();

    for (number, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "<Properties>" {
            if depth == 0 {
                root_open = Some(number);
            }
            depth += 1;
        } else if trimmed == "</Properties>" {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                root_open = None;
            }
        } else if trimmed.starts_with("</") {
            // Only the open property's own close ends it: a nested block's
            // closer (a FIXED_DICT field's `</x>`) is not this declaration.
            let closes_open = open
                .as_ref()
                .is_some_and(|(name, _)| trimmed == format!("</{name}>"));
            if closes_open && let Some((name, line)) = open.take() {
                if !has_type {
                    missing_type.push((name.clone(), line));
                }
                has_type = false;
                properties.push(IndexedProperty {
                    name,
                    flags: std::mem::take(&mut flags),
                    line,
                    end_line: number + 1,
                });
            }
        } else if depth == 1
            && open.is_none()
            && trimmed.starts_with('<')
            && trimmed.ends_with('>')
            && !trimmed.contains("</")
            && !trimmed.contains("<Properties>")
        {
            let name = trimmed[1..trimmed.len() - 1].to_string();
            open = Some((name, number));
            has_type = false;
        } else if depth == 1 && open.is_some() && trimmed.contains("<Type>") {
            has_type = true;
        } else if depth == 1
            && open.is_some()
            && let Some(from) = trimmed.find("<Flags>").map(|from| from + 7)
            && let Some(to) = trimmed[from..].find("</Flags>")
        {
            flags = trimmed[from..from + to].trim().to_string();
        }
    }

    Scan {
        properties,
        fingerprint: fingerprint(content.as_bytes()),
        unclosed_property: open,
        unclosed_properties: root_open,
        missing_type,
    }
}

/// A dependency-free FNV-1a fingerprint of the content.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    const AVATAR: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t\t<Flags>\tOTHER_CLIENTS\t</Flags>\n\t\t\t<Default>\tnobody\t</Default>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tHP\t</Type>\n\t\t\t<Flags>\tOWN_CLIENT\t</Flags>\n\t\t</hp>\n\t</Properties>\n</root>\n";

    const AVATAR_EDITED: &str = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t\t<Flags>\tBASE\t</Flags>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tHP\t</Type>\n\t\t\t<Flags>\tOWN_CLIENT\t</Flags>\n\t\t</hp>\n\t\t<mood>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t\t<Flags>\tALL_CLIENTS\t</Flags>\n\t\t</mood>\n\t</Properties>\n</root>\n";

    #[test]
    fn editing_a_file_refreshes_its_entries() {
        let mut index = EntityDefIndex::new();
        let first = index.refresh("res/scripts/entity_defs/Avatar.def", AVATAR);
        assert_eq!(first.entity, "Avatar");
        let names: Vec<&str> = first.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["name", "hp"]);
        assert_eq!(first.properties[0].flags, "OTHER_CLIENTS");

        // The editor sends the edited content: the entry is replaced in place
        // — the new property appears, the changed flag reads, the spans move.
        let edited = index.refresh("res/scripts/entity_defs/Avatar.def", AVATAR_EDITED);
        let names: Vec<&str> = edited.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["name", "hp", "mood"]);
        assert_eq!(edited.properties[0].flags, "BASE");
        assert_eq!(edited.properties[2].name, "mood");
        assert_eq!(index.count(), 1);
        assert_eq!(index.entities(), vec!["Avatar"]);
    }

    #[test]
    fn deleting_a_file_drops_its_entries() {
        let mut index = EntityDefIndex::new();
        index.refresh("res/scripts/entity_defs/Avatar.def", AVATAR);
        index.refresh("res/scripts/entity_defs/Npc.def", AVATAR);

        // The editor closes and deletes Avatar.def: its entry drops, the other
        // file's stays.
        assert!(index.remove("res/scripts/entity_defs/Avatar.def"));
        assert_eq!(index.entities(), vec!["Npc"]);
        assert_eq!(index.count(), 1);
        assert!(index.file("res/scripts/entity_defs/Avatar.def").is_none());
        // A second deletion of the same path is a no-op.
        assert!(!index.remove("res/scripts/entity_defs/Avatar.def"));
    }

    #[test]
    fn entity_names_come_from_the_file_stems() {
        let mut index = EntityDefIndex::new();
        index.refresh("res/scripts/entity_defs/Npc.def", AVATAR);
        index.refresh("res/scripts/entity_defs/Avatar.def", AVATAR);

        // The stem is the entity, and the index reads in path order.
        assert_eq!(index.entities(), vec!["Avatar", "Npc"]);
        assert_eq!(
            index.file_of("Avatar"),
            Some("res/scripts/entity_defs/Avatar.def")
        );
        assert!(index.file_of("Monster").is_none());
    }

    #[test]
    fn spans_and_fingerprints_track_the_content() {
        let mut index = EntityDefIndex::new();
        let file = index.refresh("Avatar.def", AVATAR);
        let fingerprint = file.fingerprint;

        // A declaration spans its open line through its close.
        let name = &file.properties[0];
        assert_eq!(name.line, 2);
        assert_eq!(name.end_line, 7);
        let hp = &file.properties[1];
        assert_eq!(hp.line, 7);
        assert_eq!(hp.end_line, 11);

        // Re-sending the same content keeps the fingerprint, so an unchanged
        // re-send is skippable; an edit changes it.
        let again = index.refresh("Avatar.def", AVATAR);
        assert_eq!(again.fingerprint, fingerprint);
        let edited = index.refresh("Avatar.def", AVATAR_EDITED);
        assert_ne!(edited.fingerprint, fingerprint);
    }

    #[test]
    fn a_nested_properties_block_is_type_structure_not_a_declaration() {
        // A FIXED_DICT's <Properties> sits inside <Type>: its children are
        // fields of the type, not properties of the entity.
        let content = "<root>\n\t<Properties>\n\t\t<pos>\n\t\t\t<Type>\tFIXED_DICT\n\t\t\t\t<Properties>\n\t\t\t\t\t<x>\n\t\t\t\t\t\t<Type>\tFLOAT32\t</Type>\n\t\t\t\t\t</x>\n\t\t\t\t</Properties>\n\t\t\t</Type>\n\t\t\t<Flags>\tCELL_PUBLIC_AND_OWN\t</Flags>\n\t\t</pos>\n\t</Properties>\n</root>\n";
        let mut index = EntityDefIndex::new();
        let file = index.refresh("Avatar.def", content);
        let names: Vec<&str> = file.properties.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["pos"]);
        assert_eq!(file.properties[0].flags, "CELL_PUBLIC_AND_OWN");
    }
}
