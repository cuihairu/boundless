//! `BSPConverter` port
//! (`tools/asset_pipeline/converters/bsp_converter/`): upgrades BSP
//! collision trees to the current on-disk format.
//!
//! The converter's two branches mirror the reference exactly:
//! - a `.primitives` container has its `bsp2` child re-saved inside the
//!   container (load → `saveBSPInMemory` → `writeBinary` → container save);
//! - a standalone `.bsp2` file is loaded and its current-format bytes
//!   written back out (`saveBSPInFile`);
//! - any other extension prints the reference's "Unsupported file
//!   extension" error but **still reports the output and success** (the
//!   reference falls through to the push).
//!
//! [`BspConversionRule`] is the discovery face: a BSP is out of date when
//! its encoded version differs from the current one, and an out-of-date
//! BSP's source file is itself.

use std::rc::Rc;

use physics::bsp::{BSP_FILE_CUR_VERSION, BspTree};

use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::BwResource;

use crate::compiler::Compiler;
use crate::conversion_rule::ConversionRule;
use crate::conversion_task::ConversionTask;
use crate::converter::Converter;
use crate::dependency_list::DependencyList;
use crate::hash64;

/// `BSPConverter::getTypeName`.
pub const TYPE_NAME: &str = "BSPConverter";
/// `BSPConverter::s_TypeId`.
pub fn type_id() -> u64 {
    hash64::compute_str(TYPE_NAME)
}
/// `BSPConverter::getVersion`.
pub const VERSION: &str = "2.7.1";

/// `BSPConverter` (`bsp_converter.cpp`).
pub struct BspConverter {
    resource: Rc<BwResource>,
}

impl BspConverter {
    pub fn new(resource: Rc<BwResource>) -> Self {
        Self { resource }
    }

    /// `convertBSPInPrimitives`: swap the container's `bsp2` child for its
    /// current-format bytes and save the whole container.
    fn convert_bsp_in_primitives(&self, sourcefile: &str, outputfile: &str) -> bool {
        let Some(raw) = self.resource.file_system().read_file(sourcefile) else {
            eprintln!("Unable to open primitives {sourcefile}");
            return false;
        };
        let mut container = BinSection::new("primitives", BinaryBlock::new(raw));
        let Some(bsp) = container.read_binary("bsp2") else {
            eprintln!("Primitives does not contain bsp section {sourcefile}");
            return false;
        };
        let Ok(tree) = BspTree::load(bsp.data()) else {
            eprintln!("Could not load bsp from primitives {sourcefile}");
            return false;
        };
        let saved = BinaryBlock::new(tree.save());

        // `writeBinary( "bsp2", bp )`: the child keeps its position; the
        // fresh child append covers a container without one.
        let mut rebuilt = BinSection::new(container.section_name(), BinaryBlock::default());
        let mut replaced = false;
        for child in container.children() {
            if child.section_name() == "bsp2" {
                rebuilt
                    .new_section("bsp2")
                    .set_binary(BinaryBlock::new(saved.data().to_vec()));
                replaced = true;
            } else {
                rebuilt
                    .new_section(child.section_name())
                    .set_binary(child.as_binary().clone());
            }
        }
        if !replaced {
            rebuilt.new_section("bsp2").set_binary(saved);
        }

        if !self
            .resource
            .file_system()
            .write_file(outputfile, &rebuilt.save_bytes())
        {
            eprintln!("Failed to save primitives {outputfile}");
            return false;
        }
        true
    }

    /// `convertBSPFile`: the standalone block straight back out.
    fn convert_bsp_file(&self, sourcefile: &str, outputfile: &str) -> bool {
        let Some(raw) = self.resource.file_system().read_file(sourcefile) else {
            eprintln!("Could not open file {sourcefile}");
            return false;
        };
        let Ok(tree) = BspTree::load(&raw) else {
            eprintln!("Could not load bsp from {sourcefile}");
            return false;
        };
        if !self
            .resource
            .file_system()
            .write_file(outputfile, &tree.save())
        {
            eprintln!("BSPTreeTool::saveBSPInFile failed to save a bsp file {outputfile}");
            return false;
        }
        true
    }
}

impl Converter for BspConverter {
    fn create_dependencies(
        &self,
        _source_file: &str,
        _compiler: &dyn Compiler,
        _dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        true
    }

    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        let mut outputfile = source_file.to_string();
        compiler.resolve_output_path(&mut outputfile);
        self.resource.ensure_absolute_path_exists(&outputfile);

        let ext = source_file.rsplit('.').next().unwrap_or_default();
        if ext == "primitives" {
            if !self.convert_bsp_in_primitives(source_file, &outputfile) {
                return false;
            }
        } else if ext == "bsp2" {
            if !self.convert_bsp_file(source_file, &outputfile) {
                return false;
            }
        } else {
            // The reference prints this and still pushes the output below.
            eprintln!("Unsupported file extension {ext}");
        }

        output_files.push(outputfile);
        true
    }
}

/// `BSPConversionRule` (`bsp_conversion_rule.cpp`): discovers out-of-date
/// BSP trees and maps them to a [`BspConverter`] task.
pub struct BspConversionRule {
    resource: Rc<BwResource>,
}

impl BspConversionRule {
    pub fn new(resource: Rc<BwResource>) -> Self {
        Self { resource }
    }

    /// `isOutOfDateBSP`: true when the file carries a BSP whose encoded
    /// version is not the current one. Files without a BSP answer false.
    fn is_out_of_date_bsp(&self, resource_id: &str) -> bool {
        let ext = resource_id.rsplit('.').next().unwrap_or_default();
        let block = if ext == "primitives" {
            let Some(raw) = self.resource.file_system().read_file(resource_id) else {
                return false;
            };
            let mut container = BinSection::new("primitives", BinaryBlock::new(raw));
            container.read_binary("bsp2")
        } else if ext == "bsp2" {
            self.resource
                .file_system()
                .read_file(resource_id)
                .map(BinaryBlock::new)
        } else {
            return false;
        };
        let Some(block) = block else {
            return false;
        };
        physics::bsp::bsp_version(block.data()) != BSP_FILE_CUR_VERSION
    }
}

impl ConversionRule for BspConversionRule {
    fn create_root_task(&self, file: &str, task: &mut ConversionTask) -> bool {
        if !self.is_out_of_date_bsp(file) {
            return false;
        }
        task.converter_id = type_id();
        task.converter_version = VERSION.to_string();
        task.converter_params = String::new();
        true
    }

    /// `getSourceFile`: an out-of-date BSP is its own source.
    fn get_source_file(&self, file: &str) -> Option<String> {
        if self.is_out_of_date_bsp(file) {
            Some(file.to_string())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundation::math::Vector3;
    use physics::bsp::{BSP_FILE_LEGACY_TOKEN, bsp_version};
    use physics::world_triangle::WorldTriangle;
    use resources::binary::{BinSection, BinaryBlock};
    use std::fs;
    use std::path::PathBuf;

    struct StubCompiler;

    impl Compiler for StubCompiler {
        fn get_resource_paths(&self) -> &[String] {
            &[]
        }
        fn get_hash(&self, _dependency: &crate::dependency::Dependency) -> u64 {
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
            _dependency: &crate::dependency::Dependency,
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
        let dir = std::env::temp_dir().join(format!("bspconv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A one-node legacy BSP over two triangles: the 1.x layout — token,
    /// counts, triangle rows, then per-node `flags u8, PlaneEq 16,
    /// numRefTris u16, u16 indices` with no children.
    fn legacy_bsp() -> Vec<u8> {
        let triangles = [
            WorldTriangle::new(
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                0,
            ),
            WorldTriangle::new(
                Vector3::new(2.0, 0.0, 0.0),
                Vector3::new(3.0, 0.0, 0.0),
                Vector3::new(2.0, 1.0, 0.0),
                0,
            ),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(&BSP_FILE_LEGACY_TOKEN.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes()); // maxTriangles, skipped
        for tri in &triangles {
            out.extend_from_slice(&tri.to_row());
        }
        // The single leaf node: no children, both triangles referenced.
        out.push(0); // flags
        out.extend_from_slice(&[0u8; 16]); // planeEq
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out
    }

    /// A `.primitives` container with the given `bsp2` blob plus one other
    /// child, to prove the child survives the swap in place.
    fn primitives_container(bsp2: &[u8]) -> Vec<u8> {
        let mut container = BinSection::new("primitives", BinaryBlock::default());
        container
            .new_section("vertices")
            .set_binary(BinaryBlock::new(vec![1, 2, 3, 4, 5, 6, 7, 8]));
        container
            .new_section("bsp2")
            .set_binary(BinaryBlock::new(bsp2.to_vec()));
        container.save_bytes()
    }

    fn make_resource(tag: &str) -> (PathBuf, Rc<BwResource>) {
        let root = temp_root(tag);
        fs::create_dir_all(root.join("models")).unwrap();
        fs::write(
            root.join("models/box.primitives"),
            primitives_container(&legacy_bsp()),
        )
        .unwrap();
        fs::write(root.join("models/wall.bsp2"), legacy_bsp()).unwrap();

        let mut resource = BwResource::init_paths(&[root.to_str().unwrap()]);
        resource.enable_editor_default_root();
        (root, Rc::new(resource))
    }

    /// The primitives round trip: the legacy `bsp2` child becomes a
    /// current-format blob, the sibling child survives in place, and the
    /// re-saved container reads back through `BspTree::load`.
    #[test]
    fn primitives_bsp_upgrades_in_place() {
        let (_root, resource) = make_resource("primitives");
        let converter = BspConverter::new(Rc::clone(&resource));
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(converter.convert(
            "models/box.primitives",
            &compiler,
            &mut Vec::new(),
            &mut outputs
        ));
        assert_eq!(outputs, vec!["models/box.primitives".to_string()]);

        // The container: vertices first (unchanged), bsp2 swapped to the
        // current token.
        let raw = resource
            .file_system()
            .read_file("models/box.primitives")
            .unwrap();
        let mut container = BinSection::new("primitives", BinaryBlock::new(raw));
        assert_eq!(container.children()[0].section_name(), "vertices");
        assert_eq!(
            container.children()[0].as_binary().data(),
            &[1u8, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(container.children()[1].section_name(), "bsp2");
        let bsp2 = container.read_binary("bsp2").unwrap();
        assert_eq!(bsp_version(bsp2.data()), BSP_FILE_CUR_VERSION);

        // The upgraded tree reads back with the legacy content intact.
        let tree = BspTree::load(bsp2.data()).unwrap();
        assert_eq!(tree.num_triangles(), 2);
        assert_eq!(tree.num_nodes(), 1);
        assert_eq!(tree.triangles()[1].v0(), Vector3::new(2.0, 0.0, 0.0));

        // And the current-format bytes round-trip through save.
        assert_eq!(tree.save(), bsp2.data());
    }

    /// The standalone round trip: a legacy `.bsp2` becomes a current-format
    /// file that loads back, and a second conversion is a byte-identical
    /// no-op.
    #[test]
    fn standalone_bsp_upgrades_and_is_idempotent() {
        let (_root, resource) = make_resource("standalone");
        let converter = BspConverter::new(Rc::clone(&resource));
        let compiler = StubCompiler;
        let mut outputs = Vec::new();
        assert!(converter.convert("models/wall.bsp2", &compiler, &mut Vec::new(), &mut outputs));
        assert_eq!(outputs, vec!["models/wall.bsp2".to_string()]);

        let raw = resource
            .file_system()
            .read_file("models/wall.bsp2")
            .unwrap();
        assert_eq!(bsp_version(&raw), BSP_FILE_CUR_VERSION);
        let tree = BspTree::load(&raw).unwrap();
        assert_eq!(tree.num_triangles(), 2);
        assert_eq!(tree.num_nodes(), 1);

        // Converting the current-format file again changes nothing.
        let mut outputs = Vec::new();
        assert!(converter.convert("models/wall.bsp2", &compiler, &mut Vec::new(), &mut outputs));
        assert_eq!(
            resource
                .file_system()
                .read_file("models/wall.bsp2")
                .unwrap(),
            raw
        );
    }

    /// An unsupported extension reports the reference's fall-through
    /// success; a missing primitives fails.
    #[test]
    fn unsupported_extension_falls_through_and_missing_fails() {
        let (root, resource) = make_resource("quirks");
        fs::write(root.join("models/weird.bsp"), legacy_bsp()).unwrap();
        let converter = BspConverter::new(Rc::clone(&resource));
        let compiler = StubCompiler;

        // The reference prints the error but still pushes and returns true.
        let mut outputs = Vec::new();
        assert!(converter.convert("models/weird.bsp", &compiler, &mut Vec::new(), &mut outputs));
        assert_eq!(outputs, vec!["models/weird.bsp".to_string()]);

        let mut outputs = Vec::new();
        assert!(!converter.convert(
            "models/ghost.primitives",
            &compiler,
            &mut Vec::new(),
            &mut outputs
        ));
        assert!(outputs.is_empty());
    }

    /// The conversion rule: a legacy BSP is out of date and is its own
    /// source; a current one is not.
    #[test]
    fn conversion_rule_flags_only_out_of_date_bsps() {
        let (_root, resource) = make_resource("rule");
        let rule = BspConversionRule::new(Rc::clone(&resource));

        let mut task = ConversionTask::new("models/box.primitives");
        assert!(rule.create_task("models/box.primitives", &mut task));
        assert_eq!(task.converter_id, type_id());
        assert_eq!(task.converter_version, VERSION);
        assert_eq!(
            rule.get_source_file("models/box.primitives").as_deref(),
            Some("models/box.primitives")
        );

        // Upgrade it, then the rule stops matching.
        let converter = BspConverter::new(resource);
        converter.convert(
            "models/box.primitives",
            &StubCompiler,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        let mut task = ConversionTask::new("models/box.primitives");
        assert!(!rule.create_task("models/box.primitives", &mut task));
        assert_eq!(rule.get_source_file("models/box.primitives"), None);

        // Files without a BSP never match.
        assert!(!rule.create_task(
            "models/nothing.tga",
            &mut ConversionTask::new("models/nothing.tga")
        ));
    }
}
