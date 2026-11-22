//! `GenericConversionRule` port (`compiler/generic_conversion_rule.cpp`): the
//! default rule that maps an asset path to a converter via `asset_rules.xml`.
//!
//! Reference quirks preserved:
//! - `load` walks the resource paths for the rules file (the reference's
//!   `load( rules, "" )`) into a [`HierarchicalConfig`];
//! - `createTask` resolves the source to a relative path, looks up the merged
//!   rule, checks the `root`/`noConversion` gates, then matches the rule's
//!   `converter` name against the registered converters (a rule naming an
//!   unknown converter yields no task, not an error);
//! - `getSourceFile` applies the rule's `sourcePattern` (an RE2 *replace*,
//!   not a match) against each `|`-separated `sourceFormat` and returns the
//!   first source that exists on disk (falling back to the last match so a
//!   destination with a missing source still gets a task).

use std::cell::RefCell;
use std::rc::Rc;

use resources::bwresource::{BwResource, path_is_relative};

use crate::conversion_rule::ConversionRule;
use crate::conversion_task::ConversionTask;
use crate::converter_map::ConverterMap;
use crate::hierarchical_config::HierarchicalConfig;

/// `GenericConversionRule( ConverterMap & converters )`: the rule backed by
/// `asset_rules.xml`.
pub struct GenericConversionRule {
    rules: HierarchicalConfig,
    converters: Rc<RefCell<ConverterMap>>,
    resource: Rc<BwResource>,
}

impl GenericConversionRule {
    pub fn new(converters: Rc<RefCell<ConverterMap>>, resource: Rc<BwResource>) -> Self {
        Self {
            rules: HierarchicalConfig::new(),
            converters,
            resource,
        }
    }

    /// `load( rules )`: load the rules file from the resource paths.
    pub fn load(&mut self, rules: &str) {
        self.rules.load(rules, "", &self.resource);
    }

    /// `createTask( sourceFile, task, root )`.
    fn create_task_inner(&self, source_file: &str, task: &mut ConversionTask, root: bool) -> bool {
        // Resolve the source to a path relative to its resource root.
        let mut relative_path = source_file.to_string();
        if !self.resource.resolve_to_relative_path(&mut relative_path) {
            return false;
        }

        let rule = self.rules.get(&relative_path);

        if root {
            let root_section = rule.find_child("root");
            if !root_section.map(|s| s.as_bool(false)).unwrap_or(false) {
                // Not a root rule.
                return false;
            }
        }

        if let Some(no_conversion) = rule.find_child("noConversion")
            && no_conversion.as_bool(false)
        {
            return false;
        }

        let Some(converter_section) = rule.find_child("converter") else {
            return false;
        };
        let converter = converter_section.as_string();
        let converter_params = rule
            .find_child("converterParams")
            .map(|s| s.as_string().to_string())
            .unwrap_or_default();

        for info in self.converters.borrow().values() {
            if info.name == converter {
                task.converter_id = info.type_id;
                task.converter_version = info.version.clone();
                task.converter_params = converter_params;
                return true;
            }
        }

        false
    }
}

impl ConversionRule for GenericConversionRule {
    /// `createRootTask`: delegate to `createTask` with `root = true`.
    fn create_root_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
        self.create_task_inner(source_file, task, true)
    }

    /// `createTask`: delegate to `createTask` with `root = false`.
    fn create_task(&self, source_file: &str, task: &mut ConversionTask) -> bool {
        self.create_task_inner(source_file, task, false)
    }

    /// `getSourceFile`: map a destination file back to its source.
    fn get_source_file(&self, file: &str) -> Option<String> {
        let rule = self.rules.get(file);
        let source_pattern = rule.find_child("sourcePattern")?;
        let source_format = rule.find_child("sourceFormat")?;
        let compiled = regex::Regex::new(source_pattern.as_string()).ok()?;

        // `sourcefile` keeps the last match (the reference returns it when no
        // source exists on disk, so a destination with a missing source still
        // gets a task).
        let mut last_source: Option<String> = None;
        for fmt in source_format.as_string().split('|') {
            // RE2::Replace reports no match when the pattern does not occur.
            if !compiled.is_match(file) {
                continue;
            }
            let filename: String = compiled
                .replacen(file, 1, &translate_re2_replacement(fmt))
                .into_owned();

            let mut source_filename = filename.clone();
            if !self.resource.search_paths_for_file(&mut source_filename) {
                source_filename = filename;
            }
            if path_is_relative(&source_filename) {
                continue;
            }
            last_source = Some(source_filename.clone());
            if self.resource.file_exists(&source_filename) {
                return Some(source_filename);
            }
        }
        last_source
    }
}

/// Translate an RE2 replacement string (`\0`, `\1`, ... group references)
/// into the Rust `regex` crate's (`$0`, `$1`, ...) syntax.
fn translate_re2_replacement(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(d) = chars.next()
        {
            if d.is_ascii_digit() {
                out.push('$');
                out.push(d);
                continue;
            }
            out.push('\\');
            out.push(d);
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion_task::{ConversionStatus, UNKNOWN_ID};
    use crate::converter_info::ConverterInfo;
    use crate::converter_map::ConverterMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "bw_generic_rule_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn make_resource(tag: &str) -> (String, Rc<BwResource>) {
        let root = temp_root(tag);
        let mut resource = BwResource::init_paths(&[root.as_str()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    /// Write an `asset_rules.xml` into a fresh temp root and build a rule over
    /// it. The resource root is returned so tests can build absolute source
    /// paths (the rule only matches paths it can resolve to a relative one).
    fn rule_with(rules_xml: &str, converters: &[ConverterInfo]) -> (String, GenericConversionRule) {
        let (root, resource) = make_resource("rules");
        std::fs::write(format!("{root}/asset_rules.xml"), rules_xml).unwrap();
        let map = Rc::new(RefCell::new(ConverterMap::new()));
        for info in converters {
            map.borrow_mut().insert(info.type_id, info.clone());
        }
        let mut rule = GenericConversionRule::new(map, resource);
        rule.load("asset_rules.xml");
        (root, rule)
    }

    /// A registered converter type whose instance is never built by the rule
    /// (the registry only needs the metadata to match against).
    struct NoopConverter;

    impl crate::converter::Converter for NoopConverter {
        fn create_dependencies(
            &self,
            _source_file: &str,
            _compiler: &dyn crate::compiler::Compiler,
            _dependencies: &mut crate::dependency_list::DependencyList<
                &dyn crate::compiler::Compiler,
            >,
        ) -> bool {
            true
        }
        fn convert(
            &self,
            _source_file: &str,
            _compiler: &dyn crate::compiler::Compiler,
            _intermediate_files: &mut Vec<String>,
            _output_files: &mut Vec<String>,
        ) -> bool {
            true
        }
    }

    fn converter(name: &str, type_id: u64) -> ConverterInfo {
        ConverterInfo::new(
            name,
            type_id,
            "1.0",
            crate::converter_info::ConverterFlags::DEFAULT_FLAGS,
            |_| Box::new(NoopConverter) as Box<dyn crate::converter::Converter>,
        )
    }

    #[test]
    fn root_task_matches_converter() {
        let xml = r#"<root>
            <rule>
                <pattern>.*\.model</pattern>
                <root>true</root>
                <converter>ModelConverter</converter>
                <converterParams>-x</converterParams>
            </rule>
        </root>"#;
        let (root, rule) = rule_with(xml, &[converter("ModelConverter", 7)]);
        let source = format!("{root}/hero.model");

        let mut task = ConversionTask::new(&source);
        let ok = rule.create_root_task(&source, &mut task);
        assert!(ok);
        assert_eq!(task.converter_id, 7);
        assert_eq!(task.converter_version, "1.0");
        assert_eq!(task.converter_params, "-x");
        assert_eq!(task.status, ConversionStatus::New);
    }

    #[test]
    fn non_root_rule_needs_root_flag() {
        let xml = r#"<root>
            <rule>
                <pattern>.*\.model</pattern>
                <converter>ModelConverter</converter>
            </rule>
        </root>"#;
        let (root, rule) = rule_with(xml, &[converter("ModelConverter", 7)]);
        let source = format!("{root}/hero.model");

        // Root lookup requires the `root` flag.
        let mut root_task = ConversionTask::new(&source);
        assert!(!rule.create_root_task(&source, &mut root_task));
        // Non-root lookup succeeds.
        let mut task = ConversionTask::new(&source);
        assert!(rule.create_task(&source, &mut task));
        assert_eq!(task.converter_id, 7);
    }

    #[test]
    fn unknown_converter_yields_no_task() {
        let xml = r#"<root>
            <rule>
                <pattern>.*\.model</pattern>
                <root>true</root>
                <converter>MissingConverter</converter>
            </rule>
        </root>"#;
        let (root, rule) = rule_with(xml, &[converter("ModelConverter", 7)]);
        let source = format!("{root}/hero.model");

        let mut task = ConversionTask::new(&source);
        assert!(!rule.create_root_task(&source, &mut task));
        assert_eq!(task.converter_id, UNKNOWN_ID);
    }

    #[test]
    fn no_conversion_rule_is_skipped() {
        let xml = r#"<root>
            <rule>
                <pattern>.*\.model</pattern>
                <root>true</root>
                <noConversion>true</noConversion>
                <converter>ModelConverter</converter>
            </rule>
        </root>"#;
        let (root, rule) = rule_with(xml, &[converter("ModelConverter", 7)]);
        let source = format!("{root}/hero.model");

        let mut task = ConversionTask::new(&source);
        assert!(!rule.create_root_task(&source, &mut task));
    }

    /// A rule matching nothing yields no task.
    #[test]
    fn unmatched_rule_yields_no_task() {
        let xml = r#"<root>
            <rule>
                <pattern>.*\.model</pattern>
                <root>true</root>
                <converter>ModelConverter</converter>
            </rule>
        </root>"#;
        let (root, rule) = rule_with(xml, &[converter("ModelConverter", 7)]);
        let source = format!("{root}/hero.txt");

        let mut task = ConversionTask::new(&source);
        assert!(!rule.create_root_task(&source, &mut task));
    }

    /// The path is resolved relative to the resource root before lookup.
    #[test]
    fn lookup_is_on_relative_path() {
        let (root, resource) = make_resource("rel");
        std::fs::write(
            format!("{root}/asset_rules.xml"),
            r#"<root>
            <rule>
                <pattern>.*\.model</pattern>
                <root>true</root>
                <converter>ModelConverter</converter>
            </rule>
        </root>"#,
        )
        .unwrap();
        let map = Rc::new(RefCell::new(ConverterMap::new()));
        map.borrow_mut().insert(7, converter("ModelConverter", 7));
        let mut rule = GenericConversionRule::new(map, resource);
        // The rule must be loaded with the resource; use the resource paths.
        rule.load("asset_rules.xml");

        // An absolute source path resolves to a relative one before lookup.
        let absolute = format!("{root}/hero.model");
        let mut task = ConversionTask::new(&absolute);
        assert!(rule.create_root_task(&absolute, &mut task));
        assert_eq!(task.converter_id, 7);
    }

    #[test]
    fn get_source_file_maps_destination_to_source() {
        let (root, resource) = make_resource("srcmap");
        // `sourceFormat` is an RE2 replacement string, so group refs are `\N`.
        std::fs::write(
            format!("{root}/asset_rules.xml"),
            r#"<root>
            <rule>
                <pattern>.*\.compiled</pattern>
                <root>true</root>
                <converter>ModelConverter</converter>
                <sourcePattern>(.*)\.compiled</sourcePattern>
                <sourceFormat>\1.model</sourceFormat>
            </rule>
        </root>"#,
        )
        .unwrap();
        std::fs::write(format!("{root}/hero.model"), b"x").unwrap();
        let map = Rc::new(RefCell::new(ConverterMap::new()));
        let mut rule = GenericConversionRule::new(map, resource);
        rule.load("asset_rules.xml");

        let source = rule.get_source_file("hero.compiled");
        assert_eq!(source, Some(format!("{root}/hero.model")));
    }

    /// The reference returns the *last* format that matched when none of the
    /// mapped sources exist on disk, so a destination with a missing source
    /// still gets a task. This only fires when the mapped name is already
    /// absolute (a relative name the resolver cannot find is skipped by
    /// `pathIsRelative`, see below).
    #[test]
    fn get_source_file_missing_absolute_source_returns_last_match() {
        let (root, resource) = make_resource("srcmiss");
        std::fs::write(
            format!("{root}/asset_rules.xml"),
            r#"<root>
            <rule>
                <pattern>.*\.compiled</pattern>
                <root>true</root>
                <converter>ModelConverter</converter>
                <sourcePattern>(.*)\.compiled</sourcePattern>
                <sourceFormat>\1.model</sourceFormat>
            </rule>
        </root>"#,
        )
        .unwrap();
        let map = Rc::new(RefCell::new(ConverterMap::new()));
        let mut rule = GenericConversionRule::new(map, resource);
        rule.load("asset_rules.xml");

        // An absolute destination maps to an absolute (missing) source.
        let destination = format!("{root}/ghost.compiled");
        let source = rule.get_source_file(&destination);
        assert_eq!(source, Some(format!("{root}/ghost.model")));
    }

    /// A mapped source that stays relative (the resolver found nothing) is
    /// skipped by the reference's `pathIsRelative` guard, so no source is
    /// reported for it.
    #[test]
    fn get_source_file_unresolvable_source_is_skipped() {
        let (root, resource) = make_resource("srcskip");
        std::fs::write(
            format!("{root}/asset_rules.xml"),
            r#"<root>
            <rule>
                <pattern>.*\.compiled</pattern>
                <root>true</root>
                <converter>ModelConverter</converter>
                <sourcePattern>(.*)\.compiled</sourcePattern>
                <sourceFormat>\1.model</sourceFormat>
            </rule>
        </root>"#,
        )
        .unwrap();
        let map = Rc::new(RefCell::new(ConverterMap::new()));
        let mut rule = GenericConversionRule::new(map, resource);
        rule.load("asset_rules.xml");

        assert_eq!(rule.get_source_file("ghost.compiled"), None);
    }

    /// `translate_re2_replacement` maps `\N` group refs to `$N` and leaves
    /// other backslash escapes (and pre-translated `$N`) untouched.
    #[test]
    fn re2_replacement_translation() {
        assert_eq!(translate_re2_replacement("\\0\\1"), "$0$1");
        assert_eq!(translate_re2_replacement("\\1.model"), "$1.model");
        assert_eq!(translate_re2_replacement("plain"), "plain");
        assert_eq!(translate_re2_replacement("$1.model"), "$1.model");
        assert_eq!(translate_re2_replacement("$1\\.model"), "$1\\.model");
        assert_eq!(translate_re2_replacement("a\\nb"), "a\\nb");
    }
}
