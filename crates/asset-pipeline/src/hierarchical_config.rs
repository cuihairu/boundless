//! `HierarchicalConfig` port (`resmgr/hierarchical_config.cpp`): the
//! path-hierarchical rule set that maps an asset path to its conversion rule.
//!
//! Reference quirks preserved:
//! - rules come from `<rule>` sections in a config file (`asset_rules.xml`)
//!   found in each directory; each rule has a `pattern` (an RE2 full match on
//!   the remaining path) and optional `name`/`import` metadata;
//! - `get( path )` resolves a *merged* rule: it walks the directory tree
//!   matching the path, and for every matching rule copies its children into
//!   the result, with deeper rules overriding shallower ones (`openSection`
//!   + `copy` semantics);
//! - `name`/`pattern`/`import` children are metadata and are never copied
//!   into the merged rule;
//! - an `import` rule copies the named rules registered earlier (in the same
//!   directory walk) before the local rule's own children (`_namedRules`),
//!   so the imported content is the *merged* content of the named rule;
//! - a `<rule>` with no `pattern` is dropped; a rule with neither a pattern
//!   nor a name warns.
//!
//! The reference builds the tree from disk through `populateChildren`; here
//! the tree is [`HierarchicalConfig::load`]ed the same way (walking resource
//! paths for `filename`), or built directly from an in-memory root section
//! ([`HierarchicalConfig::load_section`], the reference's `load(DataSectionPtr)`
//! variant) so tests can drive it without a filesystem.

use std::collections::HashMap;

use resources::bwresource::BwResource;
use resources::datasection::DataSection;

/// Metadata child names (`HierarchicalConfig_Locals`), never merged.
const TAG_NAME: &str = "name";
const TAG_PATTERN: &str = "pattern";
const TAG_IMPORT: &str = "import";
const TAG_DIRECTORY: &str = "directory";
const TAG_PATH: &str = "path";
const TAG_RULE: &str = "rule";

/// One compiled `<rule>`: its full-match pattern and its merged content.
struct RuleEntry {
    pattern: regex::Regex,
    content: DataSection,
}

/// A node of the rule hierarchy: rules at this level, plus sub-directories
/// keyed by their `path` attribute.
struct ConfigNode {
    directory: String,
    rules: Vec<RuleEntry>,
    children: Vec<ConfigNode>,
}

/// A path-hierarchical rule set (see the module docs).
pub struct HierarchicalConfig {
    root: ConfigNode,
}

impl Default for HierarchicalConfig {
    fn default() -> Self {
        Self {
            root: ConfigNode {
                directory: String::new(),
                rules: Vec::new(),
                children: Vec::new(),
            },
        }
    }
}

impl HierarchicalConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// `load( filename, rootDirectory )`: build the tree by walking resource
    /// paths for `filename`. An empty `rootDirectory` constructs the tree
    /// from all resource paths (deepest first, the reference's reverse path
    /// loop); a non-empty one walks that single directory.
    pub fn load(&mut self, filename: &str, root_directory: &str, resource: &BwResource) {
        let fresh = HashMap::new();
        if !root_directory.is_empty() {
            populate_children(&mut self.root, filename, root_directory, resource, fresh);
            return;
        }

        let num_paths = resource.get_path_num();
        for i in (0..num_paths).rev() {
            let path = resource.get_path(i).to_string();
            // A fresh named-rule map per resource path (the reference passes a
            // fresh `StringHashMap< DataSectionPtr >()` for each).
            let named_rules = HashMap::new();
            populate_children(&mut self.root, filename, &path, resource, named_rules);
        }
    }

    /// `load( pRootSection )`: build the tree from an already-constructed
    /// root section of `<rule>`/`<directory>` children. This mirrors the
    /// reference's in-memory variant, except that it also resolves `import`s
    /// exactly like the disk walk (the reference's raw passthrough does not,
    /// but no pipeline code loads a pre-built tree this way).
    pub fn load_section(&mut self, root: &DataSection) {
        self.root = ConfigNode {
            directory: String::new(),
            rules: Vec::new(),
            children: Vec::new(),
        };
        populate_node_from_section(&mut self.root, root);
    }

    /// `get( path )`: the merged rule for `path` (empty section when no rule
    /// matches).
    pub fn get(&self, path: &str) -> DataSection {
        let mut config = DataSection::new("");
        populate_configuration(&mut config, &self.root, path);
        config
    }
}

/// `populateChildren`: scan `directory` for `filename`, add its `<rule>`
/// sections, then recurse into sub-directories.
fn populate_children(
    node: &mut ConfigNode,
    filename: &str,
    directory: &str,
    resource: &BwResource,
    mut named_rules: HashMap<String, DataSection>,
) {
    let Some(entries) = resource.file_system().read_directory(directory) else {
        return;
    };

    // Process config files in this directory (rules before directories).
    for entry in &entries {
        if entry != filename {
            continue;
        }
        let path = format!("{directory}/{entry}");
        if resource.get_file_type(&path, None) != resources::zip::FileType::File {
            continue;
        }
        let Some(file_section) = resource.open_section(&path, false) else {
            continue;
        };
        let file_section = &*file_section;

        for i in 0..file_section.count_children() {
            let Some(child) = file_section.open_child(i) else {
                continue;
            };
            if child.section_name() != TAG_RULE {
                continue;
            }

            // Build the rule's content: imports first, then the local rule.
            let mut content = DataSection::new(TAG_RULE);
            if let Some(import_section) = child.find_child(TAG_IMPORT) {
                for name in import_section
                    .as_string()
                    .split(',')
                    .filter(|t| !t.is_empty())
                {
                    match named_rules.get(name) {
                        Some(imported) => populate_rule(&mut content, imported),
                        None => eprintln!("Could not import rule {name}."),
                    }
                }
            }
            populate_rule(&mut content, child);

            let pattern_section = child.find_child(TAG_PATTERN);
            let pattern_str = pattern_section.map(|s| s.as_string()).unwrap_or("");
            let name_section = child.find_child(TAG_NAME);
            if let Some(name) = name_section {
                // Register the named rule (its merged content) for imports.
                named_rules.insert(name.as_string().to_string(), content.clone());
            }

            if pattern_section.is_none() {
                // A rule with no pattern is dropped from the tree.
                if name_section.is_none() {
                    eprintln!("Rule found in {path} with no pattern or name.");
                }
                continue;
            }

            let Ok(compiled) = compile_full_match(pattern_str) else {
                continue;
            };
            node.rules.push(RuleEntry {
                pattern: compiled,
                content,
            });
        }
    }

    // Recurse into sub-directories (each gets a copy of the named rules so a
    // sibling's additions are invisible, matching the reference's copy).
    for entry in &entries {
        let path = format!("{directory}/{entry}");
        if resource.get_file_type(&path, None) != resources::zip::FileType::Directory {
            continue;
        }
        let mut sub = ConfigNode {
            directory: entry.clone(),
            rules: Vec::new(),
            children: Vec::new(),
        };
        populate_children(&mut sub, filename, &path, resource, named_rules.clone());
        // A directory with no rules anywhere inside is pruned (the reference
        // deletes a directory section that only holds its path attribute).
        if !sub.rules.is_empty() || !sub.children.is_empty() {
            node.children.push(sub);
        }
    }
}

/// Build a node's rules/children from an in-memory `<rule>`/`<directory>`
/// tree (the reference's `load(DataSectionPtr)` path).
fn populate_node_from_section(node: &mut ConfigNode, section: &DataSection) {
    let mut named_rules: HashMap<String, DataSection> = HashMap::new();
    for i in 0..section.count_children() {
        let Some(child) = section.open_child(i) else {
            continue;
        };
        match child.section_name() {
            TAG_RULE => {
                let mut content = DataSection::new(TAG_RULE);
                if let Some(import_section) = child.find_child(TAG_IMPORT) {
                    for name in import_section
                        .as_string()
                        .split(',')
                        .filter(|t| !t.is_empty())
                    {
                        if let Some(imported) = named_rules.get(name) {
                            populate_rule(&mut content, imported);
                        }
                    }
                }
                populate_rule(&mut content, child);

                let pattern_section = child.find_child(TAG_PATTERN);
                let pattern_str = pattern_section.map(|s| s.as_string()).unwrap_or("");
                if let Some(name) = child.find_child(TAG_NAME) {
                    named_rules.insert(name.as_string().to_string(), content.clone());
                }
                if pattern_section.is_none() {
                    continue;
                }
                if let Ok(compiled) = compile_full_match(pattern_str) {
                    node.rules.push(RuleEntry {
                        pattern: compiled,
                        content,
                    });
                }
            }
            TAG_DIRECTORY => {
                let Some(path_attr) = child.find_child(TAG_PATH) else {
                    continue;
                };
                let mut sub = ConfigNode {
                    directory: path_attr.as_string().to_string(),
                    rules: Vec::new(),
                    children: Vec::new(),
                };
                populate_node_from_section(&mut sub, child);
                node.children.push(sub);
            }
            _ => {}
        }
    }
}

/// `RE2::FullMatch`: compile a pattern anchored at both ends.
fn compile_full_match(pattern: &str) -> Result<regex::Regex, regex::Error> {
    regex::Regex::new(&format!("^(?:{pattern})$"))
}

/// `populateConfiguration`: merge the rules matching `path` into `config`,
/// descending into sub-directories that prefix `path`.
fn populate_configuration(config: &mut DataSection, node: &ConfigNode, path: &str) {
    for rule in &node.rules {
        if rule.pattern.is_match(path) {
            populate_rule(config, &rule.content);
        }
    }
    for child in &node.children {
        let directory_path = format!("{}/", child.directory);
        if let Some(rest) = path.strip_prefix(&directory_path) {
            populate_configuration(config, child, rest);
        }
    }
}

/// `populateRule`: copy each non-metadata child of `overrides` into `rule`,
/// overriding any existing child of the same name.
fn populate_rule(rule: &mut DataSection, overrides: &DataSection) {
    for i in 0..overrides.count_children() {
        let Some(child) = overrides.open_child(i) else {
            continue;
        };
        let name = child.section_name();
        if name == TAG_NAME || name == TAG_PATTERN || name == TAG_IMPORT {
            continue;
        }
        // `openSection( name, true )`: reuse an existing child or append one.
        let index = child_index(rule, name);
        let node = match index {
            Some(index) => rule.child_mut(index).expect("child index"),
            None => rule.add_child(name),
        };
        copy_section(node, child);
    }
}

/// The index of the child named `name`, if any.
fn child_index(section: &DataSection, name: &str) -> Option<usize> {
    (0..section.count_children())
        .find(|&i| section.open_child(i).map(|c| c.section_name()) == Some(name))
}

/// `DataSection::copy( pSection, true )`: blow away `dst`'s children, take
/// `src`'s value and attribute flag, then deep-copy its children.
fn copy_section(dst: &mut DataSection, src: &DataSection) {
    dst.del_children();
    dst.set_string(src.as_string());
    dst.set_attribute(src.is_attribute());
    for i in 0..src.count_children() {
        let Some(child) = src.open_child(i) else {
            continue;
        };
        let node = dst.add_child(child.section_name());
        copy_section(node, child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `models` directory with one `.model` rule.
    fn models_root() -> DataSection {
        let mut root = DataSection::new("root");
        let dir = root.add_child(TAG_DIRECTORY);
        dir.add_child(TAG_PATH).set_string("models");
        let child = dir.add_child(TAG_RULE);
        child.add_child(TAG_PATTERN).set_string(".*\\.model");
        child.add_child("converter").set_string("ModelConverter");
        child.add_child("converterParams").set_string("-x");
        root
    }

    /// `get` returns the merged content of the rules matching a path.
    #[test]
    fn get_merges_matching_rules() {
        let mut config = HierarchicalConfig::new();
        config.load_section(&models_root());

        let resolved = config.get("models/hero.model");
        assert_eq!(
            resolved.find_child("converter").unwrap().as_string(),
            "ModelConverter"
        );
        assert_eq!(
            resolved.find_child("converterParams").unwrap().as_string(),
            "-x"
        );
    }

    /// A non-matching path yields an empty section.
    #[test]
    fn get_no_match_is_empty() {
        let mut config = HierarchicalConfig::new();
        config.load_section(&DataSection::new("root"));
        let resolved = config.get("models/hero.model");
        assert_eq!(resolved.count_children(), 0);
    }

    /// A deeper rule overrides a shallower one with the same child name.
    #[test]
    fn deeper_rule_overrides() {
        let mut root = DataSection::new("root");
        let top = root.add_child(TAG_RULE);
        top.add_child(TAG_PATTERN).set_string(".*\\.model");
        top.add_child("converter").set_string("ConverterA");

        let dir = root.add_child(TAG_DIRECTORY);
        dir.add_child(TAG_PATH).set_string("models");
        let child = dir.add_child(TAG_RULE);
        child.add_child(TAG_PATTERN).set_string(".*");
        child.add_child("converter").set_string("ConverterB");

        let mut config = HierarchicalConfig::new();
        config.load_section(&root);
        let resolved = config.get("models/hero.model");
        assert_eq!(
            resolved.find_child("converter").unwrap().as_string(),
            "ConverterB"
        );
    }

    /// `name`/`pattern`/`import` metadata is never merged into the result.
    #[test]
    fn metadata_is_not_merged() {
        let mut root = DataSection::new("root");
        let child = root.add_child(TAG_RULE);
        child.add_child(TAG_PATTERN).set_string(".*\\.model");
        child.add_child(TAG_NAME).set_string("myRule");
        child.add_child("converter").set_string("ModelConverter");

        let mut config = HierarchicalConfig::new();
        config.load_section(&root);
        let resolved = config.get("hero.model");
        assert!(resolved.find_child(TAG_NAME).is_none());
        assert!(resolved.find_child(TAG_PATTERN).is_none());
        assert!(resolved.find_child(TAG_IMPORT).is_none());
        assert_eq!(
            resolved.find_child("converter").unwrap().as_string(),
            "ModelConverter"
        );
    }

    /// A rule without a pattern is dropped during the build.
    #[test]
    fn rule_without_pattern_is_dropped() {
        let mut root = DataSection::new("root");
        let child = root.add_child(TAG_RULE);
        child.add_child("converter").set_string("NoPattern");

        let mut config = HierarchicalConfig::new();
        config.load_section(&root);
        assert_eq!(config.get("anything").count_children(), 0);
    }

    /// `import` copies a named rule's content before the local rule's.
    #[test]
    fn import_merges_named_rule_first() {
        let mut root = DataSection::new("root");
        let base = root.add_child(TAG_RULE);
        base.add_child(TAG_PATTERN).set_string(".*\\.base");
        base.add_child(TAG_NAME).set_string("baseRule");
        base.add_child("converter").set_string("BaseConverter");
        base.add_child("quality").set_string("low");

        let derived = root.add_child(TAG_RULE);
        derived.add_child(TAG_PATTERN).set_string(".*\\.model");
        derived.add_child(TAG_IMPORT).set_string("baseRule");
        derived.add_child("converter").set_string("ModelConverter");

        let mut config = HierarchicalConfig::new();
        config.load_section(&root);

        // The derived rule's merged content is the imported base content
        // overridden by the local converter.
        let resolved = config.get("hero.model");
        assert_eq!(
            resolved.find_child("converter").unwrap().as_string(),
            "ModelConverter"
        );
        assert_eq!(resolved.find_child("quality").unwrap().as_string(), "low");
        // The base rule itself still resolves independently.
        let base = config.get("x.base");
        assert_eq!(
            base.find_child("converter").unwrap().as_string(),
            "BaseConverter"
        );
    }

    /// The merged `converter` child is reused (overridden) in place, so an
    /// import that lacks a key keeps the earlier one rather than duplicating.
    #[test]
    fn override_reuses_existing_child() {
        let mut root = DataSection::new("root");
        let top = root.add_child(TAG_RULE);
        top.add_child(TAG_PATTERN).set_string(".*\\.model");
        top.add_child("converter").set_string("A");

        let dir = root.add_child(TAG_DIRECTORY);
        dir.add_child(TAG_PATH).set_string("models");
        let child = dir.add_child(TAG_RULE);
        child.add_child(TAG_PATTERN).set_string(".*");
        child.add_child("converter").set_string("B");

        let mut config = HierarchicalConfig::new();
        config.load_section(&root);
        let resolved = config.get("models/hero.model");
        // Exactly one `converter` child survives the override.
        let mut count = 0;
        for i in 0..resolved.count_children() {
            if resolved.open_child(i).unwrap().section_name() == "converter" {
                count += 1;
            }
        }
        assert_eq!(count, 1);
        assert_eq!(resolved.find_child("converter").unwrap().as_string(), "B");
    }
}
