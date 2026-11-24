//! `TexFormatConverter` port
//! (`tools/asset_pipeline/converters/texformat_converter/`): normalises a
//! `.texformat` file — read it through [`TextureDetailLevel::init`], write
//! it back through [`TextureDetailLevel::write`], save the result in place.
//!
//! `TEXFORMAT_CONVERTER_VERSION` is a build-system define the tree never
//! pins; the port resolves it to 1, so the version reads `2.7v1`.

use std::rc::Rc;

use resources::bwresource::BwResource;

use crate::compiler::Compiler;
use crate::converter::Converter;
use crate::dependency_list::DependencyList;
use crate::texture_detail_level::TextureDetailLevel;

/// `TexFormatConverter::getTypeName`.
pub const TYPE_NAME: &str = "TexFormatConverter";

/// `TexFormatConverter::getTypeId` — `Hash64::compute( getTypeName() )`.
pub fn type_id() -> u64 {
    crate::hash64::compute_str(TYPE_NAME)
}

/// `TexFormatConverter::getVersion` (`"2.7v" TO_STRING(TEXFORMAT_CONVERTER_VERSION)`).
pub const VERSION: &str = "2.7v1";

/// `TexFormatConverter`.
pub struct TexFormatConverter {
    resource: Rc<BwResource>,
}

impl TexFormatConverter {
    /// The reference is constructed with its `params` string (unused).
    pub fn new(resource: Rc<BwResource>) -> Self {
        Self { resource }
    }
}

impl Converter for TexFormatConverter {
    /// `createDependencies` adds nothing.
    fn create_dependencies(
        &self,
        _source_file: &str,
        _compiler: &dyn Compiler,
        _dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        true
    }

    /// `convert`: normalise the section in place and save it to the
    /// resolved output.
    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        // Resolve the path we wish to save this texture format file to.
        let mut outputfile = source_file.to_string();
        compiler.resolve_output_path(&mut outputfile);
        self.resource.ensure_absolute_path_exists(&outputfile);

        // Read in the source texture format file.
        let Some(mut section) = self
            .resource
            .open_section(source_file, false)
            .map(|section| (*section).clone())
        else {
            eprintln!("Could not open source file.");
            return false;
        };

        let mut detail_level = TextureDetailLevel::default();
        detail_level.init(&section);
        detail_level.write(&mut section);

        // Save out the texture format file. The reference keeps the same
        // section object it normalised; the port's write bypasses the
        // section cache, so drop the stale entry.
        let xml = section.write_to_string();
        if !self
            .resource
            .file_system()
            .write_file(&outputfile, xml.as_bytes())
        {
            eprintln!("Could not save output file.");
            return false;
        }
        self.resource.purge(&outputfile, false);

        output_files.push(outputfile);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependency::Dependency;
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
        let dir = std::env::temp_dir().join(format!("texfmt-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// The round trip: a partial `.texformat` comes out fully normalised —
    /// every field present, the version stamped, stray children dropped —
    /// and re-initing from the file reproduces the same detail level.
    #[test]
    fn texformat_convert_normalises_in_place() {
        let root = temp_root("roundtrip");
        write_file(
            &root.join("bark.texformat"),
            "<texformat>\n\t<junk>1</junk>\n\t<format>DXT5</format>\n\t<formatCompressed>DXT1</formatCompressed>\n\t<maxExtent>128</maxExtent>\n\t<prefix>env</prefix>\n</texformat>\n",
        );
        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        let resource = Rc::new(resource);

        let converter = TexFormatConverter::new(Rc::clone(&resource));
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(converter.convert("bark.texformat", &compiler, &mut Vec::new(), &mut outputs));
        assert_eq!(outputs, vec!["bark.texformat".to_string()]);

        let xml = resource.file_system().read_file("bark.texformat").unwrap();
        let xml = String::from_utf8(xml).unwrap();
        assert!(xml.contains("version=\"1\""));
        assert!(!xml.contains("junk"));

        // The file re-inits to the exact detail level the normaliser built.
        let mut section = resource
            .open_section("bark.texformat", false)
            .map(|section| (*section).clone())
            .unwrap();
        let mut level = TextureDetailLevel::default();
        level.init(&section);
        let mut expected = TextureDetailLevel::default();
        expected.init(&resources::datasection::parse(
            "texformat",
            "<texformat><format>DXT5</format><formatCompressed>DXT1</formatCompressed><maxExtent>128</maxExtent><prefix>env</prefix></texformat>".as_bytes(),
        ).unwrap());
        assert_eq!(level, expected);
        assert_eq!(
            resources::datasection::parse("bark.texformat", xml.as_bytes())
                .unwrap()
                .write_to_string(),
            section.write_to_string()
        );
    }

    /// A missing source fails and pushes nothing; the dependency face adds
    /// nothing but succeeds.
    #[test]
    fn missing_source_fails_and_dependencies_add_nothing() {
        let root = temp_root("missing");
        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        let resource = Rc::new(resource);

        let converter = TexFormatConverter::new(resource);
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(!converter.convert("gone.texformat", &compiler, &mut Vec::new(), &mut outputs));
        assert!(outputs.is_empty());

        let compiler_dyn: &dyn Compiler = &compiler;
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(converter.create_dependencies("gone.texformat", &compiler, &mut dependencies));
        assert!(dependencies.secondary_inputs().is_empty());
    }
}
