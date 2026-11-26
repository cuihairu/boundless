//! `HierarchicalConfigConverter` port
//! (`tools/asset_pipeline/converters/hierarchical_config_converter/`):
//! bakes a path-hierarchical rule set into one merged config file.
//!
//! The converter is parameterised (`--file`/`-f`, `--directory`/`-d`,
//! `--output`/`-o`); its dependency face registers the config directory as a
//! non-regex recursive critical directory dependency on the config file
//! pattern, and its convert face re-walks the resource paths and saves the
//! populated rule tree (the reference's `pRootSection_`, here
//! [`HierarchicalConfig::root_section`]) as the output XML.

use std::rc::Rc;

use resources::bwresource::BwResource;

use crate::compiler::Compiler;
use crate::converter::Converter;
use crate::dependency_list::DependencyList;
use crate::hierarchical_config::HierarchicalConfig;

/// `HierarchicalConfigConverter` (`hierarchical_config_converter.cpp`).
pub struct HierarchicalConfigConverter {
    resource: Rc<BwResource>,
    config_file: String,
    config_directory: String,
    output_file: String,
}

impl HierarchicalConfigConverter {
    /// The converter is constructed with its `params` string, which carries
    /// the flag/value pairs the reference tokenises on spaces.
    pub fn new(params: &str, resource: Rc<BwResource>) -> Self {
        let tokens: Vec<&str> = params.split_whitespace().collect();
        let mut config_file = String::new();
        let mut config_directory = String::new();
        let mut output_file = String::new();

        // The reference only acts on a flag when a value follows it
        // (`i + 1 < parameters.size()`), consuming the value with `++i`.
        let mut i = 0;
        while i + 1 < tokens.len() {
            match tokens[i] {
                "--file" | "-f" => {
                    config_file = tokens[i + 1].to_string();
                    i += 1;
                }
                "--directory" | "-d" => {
                    config_directory = tokens[i + 1].to_string();
                    i += 1;
                }
                "--output" | "-o" => {
                    output_file = tokens[i + 1].to_string();
                    i += 1;
                }
                _ => {}
            }
            i += 1;
        }

        Self {
            resource,
            config_file,
            config_directory,
            output_file,
        }
    }

    /// The two guards the reference runs before both faces.
    fn check_params(&self) -> bool {
        if self.config_file.is_empty() {
            eprintln!("No config file specified.");
            return false;
        }
        if self.output_file.is_empty() {
            eprintln!("No output file specified.");
            return false;
        }
        true
    }
}

impl Converter for HierarchicalConfigConverter {
    /// `createDependencies`: the config directory as one secondary
    /// directory dependency — the config file as a plain (non-regex)
    /// pattern, recursive and critical.
    fn create_dependencies(
        &self,
        _source_file: &str,
        _compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        if !self.check_params() {
            return false;
        }
        dependencies.add_secondary_directory_dependency(
            self.config_directory.clone(),
            self.config_file.clone(),
            false,
            true,
            true,
        );
        true
    }

    /// `convert`: re-walk the resource paths for the config file, save the
    /// populated rule tree as the output XML.
    fn convert(
        &self,
        _source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        if !self.check_params() {
            return false;
        }

        // BWResource::instance().purgeAll()
        self.resource.purge_all();

        let mut outputfile = self.output_file.clone();
        compiler.resolve_output_path(&mut outputfile);
        self.resource.ensure_absolute_path_exists(&outputfile);

        let mut config = HierarchicalConfig::new();
        config.load(&self.config_file, "", &self.resource);
        // The reference checks the compiler's error flag right after the
        // walk, before it even looks at the tree.
        if compiler.has_error() {
            return false;
        }

        let xml = config.root_section().write_to_string();
        if !self
            .resource
            .file_system()
            .write_file(&outputfile, xml.as_bytes())
        {
            eprintln!("Could not save output file: {outputfile}");
            return false;
        }
        output_files.push(outputfile);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependency::Dependency;
    use crate::hierarchical_config::HierarchicalConfig;
    use resources::datasection;
    use std::fs;
    use std::path::PathBuf;

    struct StubCompiler;

    impl Compiler for StubCompiler {
        fn get_resource_paths(&self) -> &[String] {
            &[]
        }
        fn get_hash(&self, _dependency: &Dependency) -> u64 {
            0
        }
        fn get_file_hash(&self, _file_name: &str, _force_update: bool) -> u64 {
            0
        }
        fn get_directory_hash(
            &self,
            _directory: &str,
            _pattern: &str,
            _regex: bool,
            _recursive: bool,
        ) -> u64 {
            0
        }
        fn check_file_hash_up_to_date(&self, _file_name: &str) -> bool {
            true
        }
        fn ensure_up_to_date(
            &self,
            _dependency: &Dependency,
        ) -> (bool, Option<crate::conversion_task::TaskHandle>) {
            (true, None)
        }
        fn get_source_file(&self, _file: &str) -> Option<String> {
            None
        }
        fn queue_task(&self, _task: crate::conversion_task::TaskHandle) {}
        fn get_next_task(&self) -> Option<crate::conversion_task::TaskHandle> {
            None
        }
        fn has_tasks(&self) -> bool {
            false
        }
        fn set_error(&self) {}
        fn set_warning(&self) {}
        fn reset_error_flags(&self) {}
        fn has_error(&self) -> bool {
            false
        }
        fn has_warning(&self) -> bool {
            false
        }
        fn resolve_relative_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_source_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_intermediate_path(&self, _path: &mut String) -> bool {
            true
        }
        fn resolve_output_path(&self, _path: &mut String) -> bool {
            true
        }
        fn cache_read(&self, _file_name: &str, _hash: u64) -> bool {
            false
        }
        fn cache_write(&self, _file_name: &str, _hash: u64) -> bool {
            false
        }
        fn should_iterate_file(&self, _file: &str) -> bool {
            true
        }
        fn should_iterate_directory(&self, _directory: &str) -> bool {
            true
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hierconv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A config hierarchy: the top level owns two rules (one named for
    /// imports), a sub-directory imports the named one and overrides it.
    fn make_config(tag: &str) -> (PathBuf, Rc<BwResource>) {
        let root = temp_root(tag);
        write_file(
            &root.join("rules.xml"),
            "<rules>\n\t<rule>\n\t\t<pattern>.*\\.tga</pattern>\n\t\t<colour>red</colour>\n\t</rule>\n\t<rule>\n\t\t<pattern>.*\\.dds</pattern>\n\t\t<name>ddsrule</name>\n\t\t<colour>blue</colour>\n\t</rule>\n</rules>\n",
        );
        write_file(
            &root.join("subdir/rules.xml"),
            "<rules>\n\t<rule>\n\t\t<pattern>.*\\.png</pattern>\n\t\t<import>ddsrule</import>\n\t\t<colour>green</colour>\n\t</rule>\n</rules>\n",
        );

        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    /// The round trip: convert bakes the rule tree into the output XML, and
    /// the output parses back into a config that answers queries exactly
    /// like a fresh walk of the same hierarchy.
    #[test]
    fn config_convert_round_trips_through_the_baked_tree() {
        let (root, resource) = make_config("roundtrip");
        let converter = HierarchicalConfigConverter::new(
            "--file rules.xml --directory . --output baked.xml",
            Rc::clone(&resource),
        );
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(converter.convert("rules.xml", &compiler, &mut Vec::new(), &mut outputs));
        assert_eq!(outputs, vec!["baked.xml".to_string()]);

        // The baked tree: two rules then the sub-directory, whose rule
        // carries the imported colour overridden to green.
        let raw = resource.file_system().read_file("baked.xml").unwrap();
        let baked = datasection::parse("root", &raw).unwrap();
        assert_eq!(baked.count_children(), 3);
        assert_eq!(baked.open_child(0).unwrap().section_name(), "rule");
        assert_eq!(baked.open_child(2).unwrap().section_name(), "directory");
        let dir = baked.open_child(2).unwrap();
        let sub_rule = dir.find_child("rule").unwrap();
        assert_eq!(sub_rule.find_child("colour").unwrap().as_string(), "green");

        // The baked tree re-loads through the section loader and answers
        // path queries like a fresh walk.
        let mut reloaded = HierarchicalConfig::new();
        reloaded.load_section(&baked);
        assert_eq!(
            reloaded
                .get("tex.tga")
                .find_child("colour")
                .unwrap()
                .as_string(),
            "red"
        );
        assert_eq!(
            reloaded
                .get("a.dds")
                .find_child("colour")
                .unwrap()
                .as_string(),
            "blue"
        );
        assert_eq!(
            reloaded
                .get("subdir/img.png")
                .find_child("colour")
                .unwrap()
                .as_string(),
            "green"
        );

        // The queries match a direct walk of the same hierarchy.
        let mut direct = HierarchicalConfig::new();
        direct.load("rules.xml", "", &resource);
        for path in ["tex.tga", "a.dds", "subdir/img.png"] {
            assert_eq!(
                reloaded.get(path).write_to_string(),
                direct.get(path).write_to_string(),
                "baked and direct disagree on {path}"
            );
        }
        let _ = root;
    }

    /// Missing flags fail both faces without registering anything.
    #[test]
    fn missing_params_fail_both_faces() {
        let (_root, resource) = make_config("params");
        let compiler = StubCompiler;
        let compiler_dyn: &dyn Compiler = &compiler;

        let converter = HierarchicalConfigConverter::new("", resource.clone());
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(!converter.create_dependencies("rules.xml", &compiler, &mut dependencies));
        assert!(dependencies.secondary_inputs().is_empty());
        assert!(!converter.convert("rules.xml", &compiler, &mut Vec::new(), &mut Vec::new()));

        // A trailing flag with no value is ignored by the paired parse.
        let converter = HierarchicalConfigConverter::new("--file rules.xml --output", resource);
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(!converter.create_dependencies("rules.xml", &compiler, &mut dependencies));
    }

    /// `createDependencies` registers the config directory with the config
    /// file as a plain, recursive, critical pattern.
    #[test]
    fn create_dependencies_registers_the_config_directory() {
        let (_root, resource) = make_config("deps");
        let converter =
            HierarchicalConfigConverter::new("-f rules.xml -d . -o baked.xml", resource);
        let compiler = StubCompiler;
        let compiler_dyn: &dyn Compiler = &compiler;
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(converter.create_dependencies("rules.xml", &compiler, &mut dependencies));

        let inputs = dependencies.secondary_inputs();
        assert_eq!(inputs.len(), 1);
        let (dependency, _) = &inputs[0];
        assert!(dependency.is_critical());
        let Dependency::Directory(dir) = dependency else {
            panic!("expected a directory dependency");
        };
        assert_eq!(dir.directory(), ".");
        assert_eq!(dir.pattern(), "rules.xml");
        assert!(!dir.is_regex());
        assert!(dir.is_recursive());
    }
}
