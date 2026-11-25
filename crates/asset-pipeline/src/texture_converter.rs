//! `Moo::TextureConverter` port
//! (`tools/asset_pipeline/converters/texture_converter/`): converts source
//! textures into the DDS pair the runtime loads.
//!
//! - dependencies: the source's `.texformat` file (non-critical secondary
//!   source) and the texture_detail_levels rule-set file (critical secondary
//!   output — whoever regenerates it invalidates every texture);
//! - convert: the priority veto (`textureExts`, walked from index 1 — the
//!   reference loop never checks `.dds`), a fresh detail-level manager init
//!   per conversion, then the tool face: the uncompressed `.dds`, plus a
//!   `.c.dds` when the detail level is compressed. The pixel work sits
//!   behind the [`TextureTool`] seam (see [`crate::convert_texture_tool`]).

use std::cell::RefCell;
use std::rc::Rc;

use resources::bwresource::BwResource;

use crate::compiler::Compiler;
use crate::convert_texture_tool::{TextureTool, TgaDdsTool};
use crate::converter::Converter;
use crate::dependency_list::DependencyList;
use crate::texture_detail_level::{
    TEXTURE_EXTS, TextureDetailLevelManager, remove_texture_extension,
};

/// `TextureConverter::getTypeName`.
pub const TYPE_NAME: &str = "TextureConverter";

/// `TextureConverter::getTypeId` — `Hash64::compute( getTypeName() )`.
pub fn type_id() -> u64 {
    crate::hash64::compute_str(TYPE_NAME)
}

/// `TextureConverter::getVersion`.
pub const VERSION: &str = "2.8.2";

/// `Moo::TextureConverter`.
pub struct TextureConverter {
    resource: Rc<BwResource>,
    tool: Rc<dyn TextureTool>,
    /// The reference mutates this member from the `&`-methods; the port
    /// carries it interior-mutable.
    texture_detail_level_manager: RefCell<TextureDetailLevelManager>,
}

impl TextureConverter {
    /// The reference is constructed with its `params` string (unused) and
    /// shares one static tool; the port carries both as fields.
    pub fn new(resource: Rc<BwResource>, tool: Rc<dyn TextureTool>) -> Self {
        Self {
            resource,
            tool,
            texture_detail_level_manager: RefCell::new(TextureDetailLevelManager::default()),
        }
    }

    /// The plugin's `TextureConverter::s_pTool = new Moo::ConvertTextureTool()`:
    /// the default face over the resource tree.
    pub fn with_default_tool(resource: Rc<BwResource>) -> Self {
        Self::new(Rc::clone(&resource), Rc::new(TgaDdsTool::new(resource)))
    }
}

impl Converter for TextureConverter {
    /// `createDependencies`: the `.texformat` file and the rule-set file.
    fn create_dependencies(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        let mut texformat_file = resources::bwresource::change_extension(source_file, ".texformat");
        compiler.resolve_relative_path(&mut texformat_file);
        dependencies.add_secondary_source_file_dependency(texformat_file, false);
        dependencies.add_secondary_output_file_dependency(
            self.texture_detail_level_manager
                .borrow()
                .get_texture_detail_levels_name(),
            true,
        );
        true
    }

    /// `convert`: veto, detail level, then the `.dds` pair.
    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        let ext = resources::bwresource::get_extension(source_file);
        // MF_ASSERT( !ext.equals_lower( "dds" ) ).
        assert!(
            !ext.eq_ignore_ascii_case("dds"),
            "TextureConverter cannot convert a dds source: {source_file}"
        );

        let mut relative_sourcefile = source_file.to_string();
        assert!(
            compiler.resolve_relative_path(&mut relative_sourcefile),
            "MF_VERIFY resolveRelativePath failed on {source_file}"
        );
        let base_name = remove_texture_extension(&relative_sourcefile);

        // The priority veto. The reference loop starts at index 1, so
        // `.dds` — the head of the list — is never checked here.
        for texture_ext in &TEXTURE_EXTS[1..] {
            if ext.eq_ignore_ascii_case(texture_ext) {
                break;
            }
            let filename = format!("{base_name}.{texture_ext}");
            if self.resource.file_exists(&filename) {
                eprintln!(
                    "Could not convert texture {source_file}. A higher priority texture was found {filename}."
                );
                return false;
            }
        }

        let texture_details_name = self
            .texture_detail_level_manager
            .borrow()
            .get_texture_detail_levels_name()
            .to_string();
        self.texture_detail_level_manager
            .borrow_mut()
            .init(&texture_details_name, &self.resource);

        let dissolved_name = self.resource.dissolve_filename(source_file);
        let lod = self
            .texture_detail_level_manager
            .borrow_mut()
            .detail_level(&dissolved_name, &self.resource);

        // The uncompressed pass.
        let mut outputfile = resources::bwresource::change_extension(source_file, ".dds");
        compiler.resolve_output_path(&mut outputfile);
        self.resource.ensure_absolute_path_exists(&outputfile);
        if !self.tool.convert(source_file, &outputfile, &lod, false) {
            eprintln!("Could not convert texture {source_file}");
            self.texture_detail_level_manager.borrow_mut().fini();
            return false;
        }
        output_files.push(outputfile);

        // The compressed pass.
        if lod.is_compressed() {
            let mut outputfile = resources::bwresource::change_extension(source_file, ".c.dds");
            compiler.resolve_output_path(&mut outputfile);
            if !self.tool.convert(source_file, &outputfile, &lod, true) {
                eprintln!("Could not convert compressed texture {source_file}");
                self.texture_detail_level_manager.borrow_mut().fini();
                return false;
            }
            output_files.push(outputfile);
        }

        self.texture_detail_level_manager.borrow_mut().fini();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert_texture_tool::{decode_dds, decode_tga};
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
        let dir = std::env::temp_dir().join(format!("texconv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &std::path::Path, contents: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A 1x2 32bpp uncompressed truecolor TGA.
    fn make_tga() -> Vec<u8> {
        let mut raw = vec![0u8, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        raw.extend_from_slice(&1u16.to_le_bytes());
        raw.extend_from_slice(&2u16.to_le_bytes());
        raw.push(32);
        raw.push(0);
        raw.extend_from_slice(&[10, 11, 12, 0x40]); // bottom row
        raw.extend_from_slice(&[20, 21, 22, 0x40]); // top row
        raw
    }

    fn make_resource(tag: &str) -> (PathBuf, Rc<BwResource>) {
        let root = temp_root(tag);
        write_file(&root.join("textures/ground.tga"), &make_tga());
        write_file(
            &root.join("system/textureDetailLevels"),
            "<root>\n\t<rule>\n\t\t<pattern>.*\\.tga</pattern>\n\t\t<format>DXT5</format>\n\t\t<formatCompressed>DXT1</formatCompressed>\n\t</rule>\n</root>\n".as_bytes(),
        );
        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    /// The id/version face matches the reference constants.
    #[test]
    fn converter_identity_matches_the_reference() {
        assert_eq!(TYPE_NAME, "TextureConverter");
        assert_eq!(VERSION, "2.8.2");
        assert_eq!(type_id(), crate::hash64::compute_str("TextureConverter"));
    }

    /// `createDependencies`: the `.texformat` source (non-critical) and the
    /// rule-set output (critical, under the auto-config name).
    #[test]
    fn create_dependencies_registers_texformat_and_rule_set() {
        let (_root, resource) = make_resource("deps");
        let converter = TextureConverter::with_default_tool(resource);
        let compiler = StubCompiler;
        let compiler_dyn: &dyn Compiler = &compiler;
        let mut dependencies: DependencyList<&dyn Compiler> = DependencyList::new(compiler_dyn);
        assert!(converter.create_dependencies("textures/ground.tga", &compiler, &mut dependencies));

        let inputs = dependencies.secondary_inputs();
        assert_eq!(inputs.len(), 2);
        assert!(matches!(&inputs[0].0,
            Dependency::SourceFile(dep) if dep.file_name() == "textures/ground.texformat"));
        assert!(!inputs[0].0.is_critical());
        assert!(matches!(&inputs[1].0,
            Dependency::OutputFile(dep) if dep.file_name() == "system/textureDetailLevels"));
        assert!(inputs[1].0.is_critical());
    }

    /// The round trip: `.tga` in, `.dds` + `.c.dds` out under the
    /// compressed rule, pixels preserved through both.
    #[test]
    fn convert_round_trips_a_tga_through_the_tool() {
        let (root, resource) = make_resource("roundtrip");
        let converter = TextureConverter::with_default_tool(Rc::clone(&resource));
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(converter.convert(
            "textures/ground.tga",
            &compiler,
            &mut Vec::new(),
            &mut outputs
        ));
        assert_eq!(
            outputs,
            vec![
                "textures/ground.dds".to_string(),
                "textures/ground.c.dds".to_string(),
            ]
        );

        let source = decode_tga(&fs::read(root.join("textures/ground.tga")).unwrap()).unwrap();
        for name in ["textures/ground.dds", "textures/ground.c.dds"] {
            let image = decode_dds(&resource.file_system().read_file(name).unwrap()).unwrap();
            assert_eq!(image.width, 1);
            assert_eq!(image.height, 2);
            assert_eq!(image.rgba, source.rgba, "{name} pixels");
        }
    }

    /// An uncompressed detail level yields only the `.dds`; a higher
    /// priority sibling vetoes the conversion; a dds source asserts.
    #[test]
    fn veto_and_uncompressed_and_dds_source_paths() {
        let (root, resource) = make_resource("veto");

        // An uncompressed rule for pngs.
        write_file(
            &root.join("system/textureDetailLevels"),
            "<root>\n\t<rule>\n\t\t<pattern>.*\\.png</pattern>\n\t\t<format>A8R8G8B8</format>\n\t</rule>\n</root>\n".as_bytes(),
        );
        write_file(&root.join("textures/poster.png"), &make_tga());
        let converter = TextureConverter::with_default_tool(Rc::clone(&resource));
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(converter.convert(
            "textures/poster.png",
            &compiler,
            &mut Vec::new(),
            &mut outputs
        ));
        assert_eq!(outputs, vec!["textures/poster.dds".to_string()]);

        // A higher priority sibling (bmp beats tga in the veto list) kills
        // the conversion.
        write_file(&root.join("textures/leaf.bmp"), &make_tga());
        write_file(&root.join("textures/leaf.tga"), &make_tga());
        let mut outputs = Vec::new();
        assert!(!converter.convert(
            "textures/leaf.tga",
            &compiler,
            &mut Vec::new(),
            &mut outputs
        ));
        assert!(outputs.is_empty());
        assert!(!resource.file_exists("textures/leaf.dds"));

        // The reference asserts on a dds source (MF_ASSERT).
        write_file(&root.join("textures/round.dds"), &make_tga());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            converter.convert(
                "textures/round.dds",
                &compiler,
                &mut Vec::new(),
                &mut Vec::new(),
            )
        }));
        assert!(result.is_err());
    }
}
