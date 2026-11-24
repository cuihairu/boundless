//! Visual processor port (`tools/asset_pipeline/converters/
//! visual_processor/visual_processor.cpp`): copies a `.visual` file to
//! `<source>.visual.processed` and injects the precalculated group origins
//! into each `primitiveGroup` section.
//!
//! Reference behaviour preserved:
//! - the visual is opened through the `VisualLoader` face: `<base>.visual`
//!   then `<base>.static.visual` (the processor disables the processed
//!   lookup); the primitives resource id is
//!   `readString("primitivesName", base) + ".primitives"`;
//! - dependencies are one secondary *output* file dependency on the
//!   primitives' `.processed` container (not critical);
//! - node identifiers are collected recursively from the `node` hierarchy
//!   (`identifier` children); with no hierarchy the name `root` is the
//!   only known node; every renderSet `node` reference must be known;
//! - every renderSet needs geometry; each geometry's `vertices` and
//!   `primitive` names resolve against the primitives resource id when
//!   they contain no '/';
//! - vertices with no vertices and primitives with no indices both fail;
//! - group origins are the bounding-box centres over each primitive
//!   group's vertex range, read as three floats at stride intervals from
//!   the start of the vertex data (the reference locks the vertex buffer
//!   at offset 0 with the target format's stream-0 stride);
//! - each `primitiveGroup` section's integer value picks the group whose
//!   origin is written into its `groupOrigin` child.
//!
//! Port deviations (structural, behaviour preserved): the vertices are
//! parsed straight out of the primitives container with
//! [`BinSection::introspect`] instead of going through the D3D vertex
//! buffer path, and the `VerticesManager`/`PrimitiveManager` singletons
//! become plain loads per geometry (no caching); the blend-index node
//! validation (`Vertices::validateFile`) is a later task; `origin()`'s
//! dev assert on an out-of-range group index becomes a skip.

use std::collections::BTreeSet;
use std::rc::Rc;

use foundation::math::Vector3;

use crate::compiler::Compiler;
use crate::converter::Converter;
use crate::dependency_list::DependencyList;
use crate::primitive_file_structs::{
    HeaderMagic, IndexHeader, PrimitiveGroup, VertexHeader, read_primitive_groups,
};
use crate::vertex_format::VertexFormatCache;
use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::BwResource;
use resources::datasection::DataSection;

/// A plugin object that converts `.visual` files.
pub struct VisualProcessor {
    /// The converter params (owned for the converter's lifetime).
    #[allow(dead_code)]
    params: String,
    /// The resource handle the file IO runs through (the reference uses the
    /// `BWResource` singleton).
    resource: Rc<BwResource>,
}

/// The source file's base name (up to the last '.').
fn base_name_of(source_file: &str) -> &str {
    match source_file.rfind('.') {
        Some(pos) => &source_file[..pos],
        None => source_file,
    }
}

/// The `VisualLoader` face the processor needs: open the visual XML
/// (`useProcessedData = false`, so no `.visual.processed` try; the
/// `.static.visual` fallback comes second).
fn load_visual(resource: &BwResource, base_name: &str) -> Option<DataSection> {
    for candidate in [
        format!("{base_name}.visual"),
        format!("{base_name}.static.visual"),
    ] {
        if let Some(section) = resource.open_section(&candidate, false) {
            return Some((*section).clone());
        }
    }
    None
}

/// `VisualLoader::getPrimitivesResID`.
fn primitives_res_id(root: &DataSection, base_name: &str) -> String {
    format!(
        "{}.primitives",
        root.read_string("primitivesName", base_name)
    )
}

/// The anonymous `loadNodeNames` walk: the section's `identifier` plus
/// every nested `node` section's identifiers.
fn load_node_names(section: &DataSection, node_names: &mut BTreeSet<String>) {
    node_names.insert(section.read_string("identifier", ""));

    for index in 0..section.count_children() {
        if let Some(child) = section.open_child(index)
            && child.section_name() == "node"
        {
            load_node_names(child, node_names);
        }
    }
}

/// The child indices of `parent` whose sections are named `tag` (the
/// reference's `openSections(tag, sections)` walk).
fn child_indices(parent: &DataSection, tag: &str) -> Vec<usize> {
    (0..parent.count_children())
        .filter(|index| {
            parent
                .open_child(*index)
                .is_some_and(|child| child.section_name() == tag)
        })
        .collect()
}

/// Split a runtime resource id like `boxes/box.primitives/vertices` into
/// the container (`boxes/box.primitives`) and the section (`vertices`).
fn split_primitive_resource_id(name: &str) -> Option<(&str, &str)> {
    let offset = name.find(".primitives/")?;
    let container_end = offset + ".primitives".len();
    Some((&name[..container_end], &name[container_end + 1..]))
}

/// Read the primitives container and hand back the named section's bytes
/// (the `PrimitiveFile::get` + `readBinary` face).
fn read_primitive_section(resource: &BwResource, resource_id: &str) -> Option<BinaryBlock> {
    let (container, section_name) = split_primitive_resource_id(resource_id)?;
    let raw = resource.file_system().read_file(container)?;
    let mut prim_file = BinSection::new(container, BinaryBlock::new(raw));
    prim_file.read_binary(section_name)
}

/// The loaded vertices the group-origin calculation needs: the vertex
/// positions read off the start of the vertex data at stride intervals
/// (`Vertices::vertexPositionsVB` over the locked vertex buffer). The
/// section is either a processed `BPVT` payload (magic, original format
/// name, inner `VertexHeader`) or a plain `VertexHeader` payload.
fn load_vertices(
    resource: &BwResource,
    vertices_name: &str,
    cache: &mut VertexFormatCache,
) -> Option<Vec<Vector3>> {
    let blob = read_primitive_section(resource, vertices_name)?;
    let data = blob.data();

    let (n_vertices, format_name, vertex_data) = if data.len() >= 4
        && u32::from_le_bytes(data[0..4].try_into().ok()?) == HeaderMagic::ProcessedVertices.value()
    {
        let header = VertexHeader::read(&data[4 + 64..])?;
        (header.n_vertices, header.vertex_format, &data[136..])
    } else {
        let header = VertexHeader::read(data)?;
        (
            header.n_vertices,
            header.vertex_format,
            &data[VertexHeader::SIZE..],
        )
    };

    // vertexStride_ = the format's stream-0 stride; the positions sit at
    // offset 0.
    let format = cache.get(&format_name)?;
    let stride = format.stream_stride(0) as usize;
    if stride == 0 {
        return None;
    }

    let mut positions = Vec::with_capacity(n_vertices.max(0) as usize);
    for vertex in 0..n_vertices.max(0) as usize {
        let base = vertex * stride;
        let slice = vertex_data.get(base..base + 12)?;
        let read =
            |word: usize| f32::from_le_bytes(slice[word * 4..word * 4 + 4].try_into().unwrap());
        positions.push(Vector3::new(read(0), read(1), read(2)));
    }
    Some(positions)
}

/// The loaded primitive: the index count plus the triangle groups (the
/// `Primitive::load` face over the `list`/`list32` section).
fn load_primitive(
    resource: &BwResource,
    primitive_name: &str,
) -> Option<(i32, Vec<PrimitiveGroup>)> {
    let blob = read_primitive_section(resource, primitive_name)?;
    let header = IndexHeader::read(blob.data())?;
    let entry_size = match header.index_format.as_str() {
        "list" => 2usize,
        "list32" => 4usize,
        _ => return None,
    };
    let indices_len = header.n_indices.max(0) as usize * entry_size;
    let groups_data = blob.data().get(IndexHeader::SIZE + indices_len..)?;
    let groups = read_primitive_groups(groups_data, header.n_triangle_groups.max(0) as usize);
    Some((header.n_indices, groups))
}

/// `Primitive::calcGroupOrigins`: the bounding-box centre over each
/// group's vertex range; groups with no vertices in range get no origin
/// (the reference's inside-out box).
fn calc_group_origins(groups: &[PrimitiveGroup], positions: &[Vector3]) -> Vec<Option<Vector3>> {
    groups
        .iter()
        .map(|group| {
            let start = group.start_vertex.max(0) as usize;
            let end = (start + group.n_vertices.max(0) as usize).min(positions.len());
            if start >= end {
                return None;
            }
            let mut min = [positions[start].x, positions[start].y, positions[start].z];
            let mut max = min;
            for position in &positions[start + 1..end] {
                let words = [position.x, position.y, position.z];
                for axis in 0..3 {
                    if words[axis] < min[axis] {
                        min[axis] = words[axis];
                    }
                    if words[axis] > max[axis] {
                        max[axis] = words[axis];
                    }
                }
            }
            Some(Vector3::new(
                (min[0] + max[0]) * 0.5,
                (min[1] + max[1]) * 0.5,
                (min[2] + max[2]) * 0.5,
            ))
        })
        .collect()
}

impl VisualProcessor {
    /// Construct a converter with parameters.
    pub fn new(params: &str, resource: Rc<BwResource>) -> Self {
        Self {
            params: params.to_string(),
            resource,
        }
    }

    /// Walk the visual tree, validate it against the primitives data and
    /// collect the tree positions (`renderSet`/`geometry`/`primitiveGroup`
    /// child indices) that need a `groupOrigin`, with the origin value.
    fn prepare(
        &self,
        root: &DataSection,
        base_name: &str,
    ) -> Option<Vec<(usize, usize, usize, Vector3)>> {
        let primitives_file_name = format!("{}/", primitives_res_id(root, base_name));
        let mut cache = VertexFormatCache::new(&self.resource);

        // Load the hierarchy.
        let mut node_names: BTreeSet<String> = BTreeSet::new();
        match root.find_child("node") {
            Some(node_section) => load_node_names(node_section, &mut node_names),
            None => {
                node_names.insert("root".to_string());
            }
        }

        // Open the rendersets.
        let render_sets = child_indices(root, "renderSet");
        if render_sets.is_empty() {
            return None;
        }

        let mut origins: Vec<(usize, usize, usize, Vector3)> = Vec::new();
        for render_set_index in render_sets {
            let render_set = root.open_child(render_set_index)?;

            // Read the node identifiers of all nodes that affect this
            // renderset; every one must exist in the hierarchy.
            let nodes: Vec<String> = child_indices(render_set, "node")
                .into_iter()
                .map(|index| {
                    render_set
                        .open_child(index)
                        .unwrap()
                        .as_string()
                        .to_string()
                })
                .collect();
            for node in &nodes {
                if !node_names.contains(node) {
                    return None;
                }
            }

            // Open the geometry sections.
            let geometries = child_indices(render_set, "geometry");
            if geometries.is_empty() {
                return None;
            }

            for geometry_index in geometries {
                let geometry = render_set.open_child(geometry_index)?;

                // Get a reference to the vertices used by this geometry.
                let vertices_name = geometry.read_string("vertices", "");
                let vertices_name = if vertices_name.contains('/') {
                    vertices_name
                } else {
                    format!("{primitives_file_name}{vertices_name}")
                };
                let positions = load_vertices(&self.resource, &vertices_name, &mut cache)?;
                if positions.is_empty() {
                    return None;
                }

                // Get a reference to the indices used by this geometry.
                let primitive_name = geometry.read_string("primitive", "");
                let primitive_name = if primitive_name.contains('/') {
                    primitive_name
                } else {
                    format!("{primitives_file_name}{primitive_name}")
                };
                let (n_indices, groups) = load_primitive(&self.resource, &primitive_name)?;
                if n_indices == 0 {
                    return None;
                }

                // Precalculate the group origins and pair them with the
                // primitiveGroup sections' tree positions.
                let group_origins = calc_group_origins(&groups, &positions);
                for section_index in child_indices(geometry, "primitiveGroup") {
                    let group_index = geometry.open_child(section_index).unwrap().as_int(0);
                    let Some(Some(origin)) =
                        group_origins.get(group_index.max(0) as usize).copied()
                    else {
                        continue;
                    };
                    origins.push((render_set_index, geometry_index, section_index, origin));
                }
            }
        }
        Some(origins)
    }
}

impl Converter for VisualProcessor {
    fn create_dependencies(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        let base_name = base_name_of(source_file);
        let Some(root) = load_visual(&self.resource, base_name) else {
            return false;
        };

        // Load our primitives.
        let primitives_resource = primitives_res_id(&root, base_name);
        let mut primitives_processed_resource = format!("{primitives_resource}.processed");

        compiler.resolve_relative_path(&mut primitives_processed_resource);
        dependencies.add_secondary_output_file_dependency(&primitives_processed_resource, false);

        true
    }

    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        let base_name = base_name_of(source_file);
        let Some(orig) = load_visual(&self.resource, base_name) else {
            return false;
        };

        let mut output_file = format!("{base_name}.visual.processed");
        compiler.resolve_output_path(&mut output_file);

        // Walk and validate against the primitives data.
        let Some(origins) = self.prepare(&orig, base_name) else {
            return false;
        };

        // Inject the primitive origins: root->copy(orig) then one
        // groupOrigin child per primitiveGroup section.
        let mut root = orig.clone();
        for (render_set_index, geometry_index, section_index, origin) in origins {
            let Some(group) = root
                .child_mut(render_set_index)
                .and_then(|render_set| render_set.child_mut(geometry_index))
                .and_then(|geometry| geometry.child_mut(section_index))
            else {
                return false;
            };
            group.add_child("groupOrigin").set_vector3(origin);
        }

        // Now output the .visual file.
        let xml = root.write_to_string();
        if !self
            .resource
            .file_system()
            .write_file(&output_file, xml.as_bytes())
        {
            return false;
        }
        output_files.push(output_file);

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fixture helpers -------------------------------------------------

    fn temp_root(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("visproc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn make_resource(tag: &str) -> (String, Rc<BwResource>) {
        let root = temp_root(tag);
        let mut resource = BwResource::init_paths(&[root.as_str()]);
        resource.enable_editor_default_root();
        (root.clone(), Rc::new(resource))
    }

    /// A processed-vertices section: BPVT header naming xyznuvpc (three
    /// floats at stride 12) + the positions.
    fn processed_vertices(positions: &[[f32; 3]]) -> Vec<u8> {
        let mut blob = Vec::new();
        crate::primitive_file_structs::ProcessedVertexHeader {
            magic: HeaderMagic::ProcessedVertices,
            original_vertex_format: "xyznuv".to_string(),
            vertex_header: VertexHeader {
                vertex_format: "xyznuvpc".to_string(),
                n_vertices: positions.len() as i32,
            },
        }
        .write(&mut blob);
        for position in positions {
            for word in position {
                blob.extend_from_slice(&word.to_le_bytes());
            }
        }
        blob
    }

    /// A `list` index section over one triangle group.
    fn list_section(n_vertices: i32) -> Vec<u8> {
        let mut blob = Vec::new();
        let header = IndexHeader {
            index_format: "list".to_string(),
            n_indices: 3,
            n_triangle_groups: 1,
        };
        blob.extend_from_slice(header.index_format.as_bytes());
        blob.resize(64, 0);
        blob.extend_from_slice(&header.n_indices.to_le_bytes());
        blob.extend_from_slice(&header.n_triangle_groups.to_le_bytes());
        for index in [0u16, 1, 2] {
            blob.extend_from_slice(&index.to_le_bytes());
        }
        crate::primitive_file_structs::write_primitive_groups(
            &[crate::primitive_file_structs::PrimitiveGroup {
                start_index: 0,
                n_primitives: 1,
                start_vertex: 0,
                n_vertices,
            }],
            &mut blob,
        );
        blob
    }

    /// Write the fixture resource tree: format XMLs, a primitives
    /// container with processed vertices + list, and a visual XML.
    fn write_fixture(root: &str) {
        std::fs::create_dir_all(format!("{root}/shaders/formats")).unwrap();
        std::fs::create_dir_all(format!("{root}/models")).unwrap();
        std::fs::write(
            format!("{root}/shaders/formats/xyznuv.xml"),
            "<root><elements><POSITION><type>FLOAT3</type></POSITION></elements></root>",
        )
        .unwrap();
        std::fs::write(
            format!("{root}/shaders/formats/xyznuvpc.xml"),
            "<root><elements><POSITION><type>FLOAT3</type></POSITION></elements></root>",
        )
        .unwrap();

        let mut container = BinSection::new("box.primitives", BinaryBlock::default());
        container
            .new_section("vertices")
            .set_binary(BinaryBlock::new(processed_vertices(&[
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
            ])));
        container
            .new_section("list")
            .set_binary(BinaryBlock::new(list_section(3)));
        std::fs::write(
            format!("{root}/models/box.primitives"),
            container.collect_binary().data(),
        )
        .unwrap();

        std::fs::write(
            format!("{root}/models/box.visual"),
            concat!(
                "<root>\n",
                "  <primitivesName>models/box</primitivesName>\n",
                "  <node>\n",
                "    <identifier>RootNode</identifier>\n",
                "    <node><identifier>ChildNode</identifier></node>\n",
                "  </node>\n",
                "  <renderSet>\n",
                "    <node>RootNode</node>\n",
                "    <geometry>\n",
                "      <vertices>vertices</vertices>\n",
                "      <primitive>list</primitive>\n",
                "      <primitiveGroup>0</primitiveGroup>\n",
                "    </geometry>\n",
                "  </renderSet>\n",
                "</root>\n",
            ),
        )
        .unwrap();
    }

    // --- compiler mock ----------------------------------------------------

    struct StubCompiler;

    impl StubCompiler {
        fn new() -> Self {
            Self
        }
    }

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

    // --- tests -------------------------------------------------------------

    /// The round trip: the visual copies out to `.visual.processed` with
    /// the group origin injected into its primitiveGroup section.
    #[test]
    fn visual_convert_round_trips_with_group_origin() {
        let (root, resource) = make_resource("roundtrip");
        write_fixture(&root);

        let processor = VisualProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(processor.convert(
            "models/box.visual",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
        assert_eq!(outputs, vec!["models/box.visual.processed".to_string()]);

        // Read the output back and check the injected origin: bbox over
        // (0,0,0),(1,0,0),(0,1,0) has centre (0.5, 0.5, 0).
        let raw = std::fs::read(format!("{root}/models/box.visual.processed")).unwrap();
        let parsed = resources::datasection::parse("box.visual.processed", &raw).unwrap();
        let render_set = parsed.find_child("renderSet").unwrap();
        let geometry = render_set.find_child("geometry").unwrap();
        let group = geometry.find_child("primitiveGroup").unwrap();
        assert_eq!(group.as_int(-1), 0);
        let origin = group.read_vector3("groupOrigin", Vector3::new(f32::MAX, f32::MAX, f32::MAX));
        assert_eq!(origin, Vector3::new(0.5, 0.5, 0.0));

        // The rest of the visual came through unchanged.
        assert_eq!(parsed.read_string("primitivesName", ""), "models/box");
        let node = parsed.find_child("node").unwrap();
        assert_eq!(node.read_string("identifier", ""), "RootNode");
        assert_eq!(
            node.find_child("node")
                .unwrap()
                .read_string("identifier", ""),
            "ChildNode"
        );
        assert_eq!(geometry.read_string("vertices", ""), "vertices");
    }

    /// `create_dependencies` registers the processed primitives container
    /// as a non-critical secondary output dependency.
    #[test]
    fn create_dependencies_registers_processed_primitives() {
        let (root, resource) = make_resource("deps");
        write_fixture(&root);

        let processor = VisualProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let compiler_dyn: &dyn Compiler = &compiler;
        let mut dependencies = DependencyList::new(compiler_dyn);
        assert!(processor.create_dependencies("models/box.visual", &compiler, &mut dependencies));

        use crate::dependency::Dependency;
        let entries: Vec<(&str, bool)> = dependencies
            .secondary_inputs()
            .iter()
            .filter_map(|(dependency, _)| match dependency {
                Dependency::OutputFile(dep) => Some((dep.file_name(), dependency.is_critical())),
                _ => None,
            })
            .collect();
        assert_eq!(entries, [("models/box.primitives.processed", false)]);
    }

    /// A renderSet referencing an unknown node fails the conversion.
    #[test]
    fn an_unknown_node_reference_fails() {
        let (root, resource) = make_resource("badnode");
        write_fixture(&root);
        std::fs::write(
            format!("{root}/models/box.visual"),
            concat!(
                "<root>\n",
                "  <primitivesName>models/box</primitivesName>\n",
                "  <node><identifier>RootNode</identifier></node>\n",
                "  <renderSet>\n",
                "    <node>NotANode</node>\n",
                "    <geometry><vertices>vertices</vertices><primitive>list</primitive>",
                "<primitiveGroup>0</primitiveGroup></geometry>\n",
                "  </renderSet>\n",
                "</root>\n",
            ),
        )
        .unwrap();

        let processor = VisualProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "models/box.visual",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
    }

    /// A visual without rendersets, or a renderSet without geometry,
    /// fails.
    #[test]
    fn missing_renderset_or_geometry_fails() {
        let (root, resource) = make_resource("norenderset");
        write_fixture(&root);
        std::fs::write(
            format!("{root}/models/box.visual"),
            "<root><primitivesName>models/box</primitivesName></root>",
        )
        .unwrap();
        let processor = VisualProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "models/box.visual",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));

        let (root, resource) = make_resource("nogeometry");
        write_fixture(&root);
        std::fs::write(
            format!("{root}/models/box.visual"),
            concat!(
                "<root><primitivesName>models/box</primitivesName>\n",
                "<renderSet><node>root</node></renderSet></root>\n"
            ),
        )
        .unwrap();
        let processor = VisualProcessor::new("", Rc::clone(&resource));
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "models/box.visual",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
    }

    /// A missing visual fails cleanly.
    #[test]
    fn a_missing_visual_fails() {
        let (_root, resource) = make_resource("missing");
        let processor = VisualProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "models/nope.visual",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
    }

    /// Vertices sections in the plain (unprocessed) header form also load.
    #[test]
    fn plain_header_vertices_also_load() {
        let (root, resource) = make_resource("plain");
        write_fixture(&root);

        // Replace the vertices section with a plain VertexHeader payload
        // (the xyznuv format also has FLOAT3 positions at stride 12).
        let mut container = BinSection::new("box.primitives", BinaryBlock::default());
        let mut blob = Vec::new();
        VertexHeader {
            vertex_format: "xyznuv".to_string(),
            n_vertices: 2,
        }
        .write(&mut blob);
        for position in [[0.0f32, 0.0, 2.0], [4.0, 0.0, 2.0]] {
            for word in position {
                blob.extend_from_slice(&word.to_le_bytes());
            }
        }
        container
            .new_section("vertices")
            .set_binary(BinaryBlock::new(blob));
        container
            .new_section("list")
            .set_binary(BinaryBlock::new(list_section(2)));
        std::fs::write(
            format!("{root}/models/box.primitives"),
            container.collect_binary().data(),
        )
        .unwrap();

        let processor = VisualProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(processor.convert(
            "models/box.visual",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
        let raw = std::fs::read(format!("{root}/models/box.visual.processed")).unwrap();
        let parsed = resources::datasection::parse("box.visual.processed", &raw).unwrap();
        let origin = parsed
            .find_child("renderSet")
            .unwrap()
            .find_child("geometry")
            .unwrap()
            .find_child("primitiveGroup")
            .unwrap()
            .read_vector3("groupOrigin", Vector3::ZERO);
        assert_eq!(origin, Vector3::new(2.0, 0.0, 2.0));
    }
}
