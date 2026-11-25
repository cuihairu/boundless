//! Primitive processor port (`tools/asset_pipeline/converters/
//! primitive_processor/primitive_processor.cpp`): converts a `.primitives`
//! container into `<source>.processed`, transforming its vertex sections
//! to the target device's vertex formats.
//!
//! Reference behaviour preserved:
//! - the `target` parameter is a ';'-split device list (default `D3D9`);
//!   more than one target fails both `createDependencies` and `convert`
//!   ("Only a single target device is currently supported");
//! - sections are matched by their *lowercased* name suffix: the known
//!   vertex streams (`uv2` → stream 10, `colour` → stream 11) convert as
//!   header-less streams (BPVS output), a plain `vertices` section
//!   converts through its `VertexHeader` format (BPVT output), and
//!   `mvertices` plus everything else copies through verbatim;
//! - vertices conversion keeps the original format name in
//!   `originalVertexFormat_` and falls back to the source format when the
//!   source declares no target for the device;
//! - dependency hunting visits *only* `[m]vertices` sections and registers
//!   both the source and target format XMLs; `huntVertexDependencies`'s
//!   return value is ignored by its caller (a missing source format fails
//!   the conversion later instead);
//! - the output filename is `<source>.processed`, resolved through the
//!   compiler's output path.
//!
//! Port deviations (structural, behaviour preserved): the reference opens
//! the primitives file through `BWResource::openSection` (a BinSection) —
//! the port's `open_section` does not yet build BinSections, so the raw
//! bytes are read off the file system and parsed with
//! [`BinSection::introspect`]; the global `BWResource`/`VertexFormatCache`
//! singletons are owned handles held by the processor.

use std::rc::Rc;

use crate::compiler::Compiler;
use crate::converter::Converter;
use crate::dependency_list::DependencyList;
use crate::primitive_file_structs::{
    HeaderMagic, ProcessedVertexHeader, VERTEX_FORMAT_FIELD_LEN, VertexHeader,
};
use crate::vertex_format::{
    StorageType, VertexFormat, VertexFormatCache, can_convert, convert_buffer_stream,
};
use resources::binary::{BinSection, BinaryBlock};
use resources::bwresource::BwResource;

/// The known vertex stream sections (`Moo::VertexStreamDesc::getKnownStreams`):
/// `(lowercased file-section suffix, stream index, default format name)`
/// in the reference's registration order.
const KNOWN_STREAMS: [(&str, u32, &str); 2] = [("uv2", 10, "uv2"), ("colour", 11, "colour")];

/// The section-suffix matchers: morph vertices and plain vertices.
const MORPH_VERTICES_ENDING: &str = "mvertices";
/// `verticesEnding = morphVerticesEnding.to_string().c_str() + 1`.
const VERTICES_ENDING: &str = "vertices";

/// The maximum stored format-name length (the 64-byte field minus its NUL).
const MAX_FORMAT_LENGTH: usize = VERTEX_FORMAT_FIELD_LEN - 1;

/// A plugin object that converts `.primitives` files.
pub struct PrimitiveProcessor {
    /// The converter params (owned for the converter's lifetime).
    #[allow(dead_code)]
    params: String,
    /// `targets_`.
    targets: Vec<String>,
    /// The resource handle the file IO runs through (the reference uses the
    /// `BWResource` singleton).
    resource: Rc<BwResource>,
}

/// `sectionNameEndsWith`: the name is lowercased before matching.
fn section_name_ends_with(name: &str, ending: &str) -> bool {
    name.ends_with(ending)
}

/// `copySectionToOutput`: the child bytes, verbatim, under its own name.
fn copy_section_to_output(section: &BinSection, output: &mut BinSection) {
    output
        .new_section(section.section_name())
        .set_binary(section.as_binary().clone());
}

/// The advisory `validateVerticesSingleStream`: re-run every destination
/// element's conversion on the real bytes and report whether any produced
/// an invalid value. The reference *ignores* this result at both call
/// sites (the warning is logged, the conversion proceeds), so the port
/// calls it the same way.
fn validate_vertices_single_stream(
    dst_format: &VertexFormat,
    src_format: &VertexFormat,
    src_vertices: &[u8],
    num_verts: u32,
    stream_index: u32,
) -> bool {
    let src_stride = src_format.stream_stride(stream_index) as usize;
    let dst_stride = dst_format.stream_stride(stream_index) as usize;
    if src_stride == 0 || dst_stride == 0 {
        return true;
    }
    let verts = (src_vertices.len() / src_stride).min(num_verts as usize);
    for vertex in 0..verts {
        for (element_stream, element) in dst_format.elements() {
            if element_stream != stream_index || element.storage == StorageType::Unknown {
                continue;
            }
            let Some((src_stream, src_offset, src_storage)) =
                src_format.find_element(element.semantic, element.semantic_index)
            else {
                continue;
            };
            if src_stream != stream_index {
                continue;
            }
            let base = vertex * src_stride + src_offset;
            if base + src_storage.size() > src_vertices.len() {
                continue;
            }
            let mut dst_element = vec![0u8; element.storage.size()];
            if !crate::vertex_format::convert_value(
                element.semantic,
                &mut dst_element,
                element.storage,
                &src_vertices[base..base + src_storage.size()],
                src_storage,
            ) {
                return false;
            }
        }
    }
    true
}

/// `convertVertices`: convert one `vertices` section through its
/// `VertexHeader` (BPVT output).
#[allow(clippy::too_many_arguments)]
fn convert_vertices(
    _filename: &str,
    section: &BinSection,
    target_device: &str,
    resource: &BwResource,
    output: &mut BinSection,
) -> bool {
    let vert_binary = section.as_binary();
    let Some(vh) = VertexHeader::read(vert_binary.data()) else {
        // The reference reinterprets the buffer unchecked; the port guards
        // the read (a too-short section is an invalid format name case).
        return false;
    };
    let src_vertices = &vert_binary.data()[VertexHeader::SIZE..];

    if vh.vertex_format.len() >= MAX_FORMAT_LENGTH {
        // strnlen(pVH->vertexFormat_, maxFormatLength + 1) >= maxFormatLength.
        return false;
    }

    let mut cache = VertexFormatCache::new(resource);
    let Some(src_format) = cache.get(&vh.vertex_format) else {
        return false;
    };

    // Attempt to fetch the target vertex format; none declared means no
    // conversion is required, but save with a consistent format.
    let dst_format_name = match src_format.get_target_format_name(target_device) {
        Some(name) => name.to_string(),
        None => vh.vertex_format.clone(),
    };
    let Some(dst_format) = cache.get(&dst_format_name) else {
        return false;
    };

    if dst_format_name.len() > MAX_FORMAT_LENGTH {
        return false;
    }

    // Now check the length of the source stream.
    let stream_index = 0;
    let src_vert_buffer_length = src_vertices.len();
    let src_stream_length = vh.n_vertices as i64 * src_format.stream_stride(stream_index) as i64;
    if src_vert_buffer_length as i64 != src_stream_length {
        return false;
    }

    // Validate source data to make sure it is valid for the conversion
    // (advisory — the reference ignores the result).
    let _ = validate_vertices_single_stream(
        &dst_format,
        &src_format,
        src_vertices,
        vh.n_vertices as u32,
        stream_index,
    );

    // Now setup a conversion between the two streams.
    let dst_stream_length =
        vh.n_vertices as usize * dst_format.stream_stride(stream_index) as usize;
    let mut dst_buffer = vec![0u8; dst_stream_length];
    if !convert_buffer_stream(
        &src_format,
        src_vertices,
        stream_index,
        &dst_format,
        &mut dst_buffer,
        stream_index,
    ) {
        return false;
    }

    // Now output the data to disk again.
    let header = ProcessedVertexHeader {
        magic: HeaderMagic::ProcessedVertices,
        original_vertex_format: vh.vertex_format.clone(),
        vertex_header: VertexHeader {
            vertex_format: dst_format_name,
            n_vertices: vh.n_vertices,
        },
    };
    let mut blob = Vec::new();
    header.write(&mut blob);
    blob.extend_from_slice(&dst_buffer);
    output
        .new_section(section.section_name())
        .set_binary(BinaryBlock::new(blob));
    true
}

/// `convertVertexStream`: convert one known stream section (BPVS output).
#[allow(clippy::too_many_arguments)]
fn convert_vertex_stream(
    _filename: &str,
    section: &BinSection,
    stream_index: u32,
    default_format_name: &str,
    target_device: &str,
    resource: &BwResource,
    output: &mut BinSection,
) -> bool {
    let stream_binary = section.as_binary();
    let src_buffer = stream_binary.data();

    let mut cache = VertexFormatCache::new(resource);
    // Set up a vertex format conversion from the stream's default format.
    let Some(src_format) = cache.get(default_format_name) else {
        return false;
    };
    let Some(dst_format) = cache.get_target(&src_format, target_device, true) else {
        return false;
    };

    // The reference's ConversionContext validity check: every destination
    // element must have a conversion (empty vertex counts — the sizes
    // arrays just span every stream index).
    let dst_sizes = vec![0usize; dst_format.stream_count() as usize];
    let src_sizes = vec![0usize; src_format.stream_count() as usize];
    if !can_convert(&dst_format, &dst_sizes, &src_format, &src_sizes) {
        return false;
    }

    // Check length of source buffer. No header for vertex streams; the
    // vertex count comes from the buffer length (numVerts == 0 here).
    let src_vertex_size = src_format.stream_stride(stream_index) as usize;
    if src_vertex_size == 0 {
        // MF_ASSERT(srcVertexSize).
        return false;
    }
    let num_verts = (src_buffer.len() / src_vertex_size) as u32;

    // Create destination buffer.
    let dst_vertex_size = dst_format.stream_stride(stream_index) as usize;
    let dst_buffer_size = dst_vertex_size * num_verts as usize;
    if dst_buffer_size == 0 {
        return false;
    }

    // Validate source data (advisory — the reference ignores the result).
    let _ = validate_vertices_single_stream(
        &dst_format,
        &src_format,
        src_buffer,
        num_verts,
        stream_index,
    );

    // Perform the conversion.
    let mut dst_buffer = vec![0u8; dst_buffer_size];
    if !convert_buffer_stream(
        &src_format,
        src_buffer,
        stream_index,
        &dst_format,
        &mut dst_buffer,
        stream_index,
    ) {
        return false;
    }

    // Prepare header and output processed data.
    let header = ProcessedVertexHeader {
        magic: HeaderMagic::ProcessedVertexStream,
        original_vertex_format: src_format.name().to_string(),
        vertex_header: VertexHeader {
            vertex_format: dst_format.name().to_string(),
            n_vertices: num_verts as i32,
        },
    };
    let mut blob = Vec::new();
    header.write(&mut blob);
    blob.extend_from_slice(&dst_buffer);
    output
        .new_section(section.section_name())
        .set_binary(BinaryBlock::new(blob));
    true
}

/// `convertPrimitives`: walk every section of the primitives container and
/// convert or copy it into the output container, then save.
fn convert_primitives(
    filename: &str,
    output_file: &str,
    target_device: &str,
    resource: &BwResource,
) -> bool {
    let Some(raw) = resource.file_system().read_file(filename) else {
        return false;
    };
    let mut prim_file = BinSection::new(filename, BinaryBlock::new(raw));

    // Create output file.
    let mut output = BinSection::new(output_file, BinaryBlock::default());

    // Iterate over each section in the primitives file looking for
    // convertible sections by matching endings.
    let children: Vec<BinSection> = prim_file.children().to_vec();
    for section in &children {
        let name = section.section_name().to_ascii_lowercase();

        let mut found_stream = false;
        // Try matching known vertex stream sections.
        for (ending, stream_index, default_format) in KNOWN_STREAMS {
            if section_name_ends_with(&name, ending) {
                // Found a vertex stream, perform conversion.
                found_stream = true;
                if !convert_vertex_stream(
                    filename,
                    section,
                    stream_index,
                    default_format,
                    target_device,
                    resource,
                    &mut output,
                ) {
                    return false;
                }
                break;
            }
        }
        if found_stream {
            // Already converted a vertex stream.
            continue;
        }

        // Simply copy any remaining cases that don't match [m]vertices
        // ending. Morph vertices are not supported currently — shares the
        // same suffix, so check and skip explicitly.
        if !section_name_ends_with(&name, VERTICES_ENDING)
            || section_name_ends_with(&name, MORPH_VERTICES_ENDING)
        {
            copy_section_to_output(section, &mut output);
        } else {
            // This must be a vertices section, execute conversion.
            if !convert_vertices(filename, section, target_device, resource, &mut output) {
                return false;
            }
        }
    }

    let blob = output.collect_binary();
    resource.file_system().write_file(output_file, blob.data())
}

/// `huntVertexDependencies`: register the source and target format XMLs of
/// one vertices section. The reference's caller ignores this function's
/// return value, so failures here do not fail `createDependencies` — the
/// dependency list just comes up short.
fn hunt_vertex_dependencies(
    _filename: &str,
    section: &BinSection,
    target_device: &str,
    resource: &BwResource,
    dependencies: &mut Vec<String>,
) -> bool {
    let vert_binary = section.as_binary();
    let Some(vh) = VertexHeader::read(vert_binary.data()) else {
        return false;
    };
    if vh.vertex_format.len() >= MAX_FORMAT_LENGTH {
        return false;
    }

    // Attempt to fetch the relevant vertex format.
    let src_format_filename = VertexFormatCache::get_resource_path(&vh.vertex_format);
    dependencies.push(src_format_filename);

    let mut cache = VertexFormatCache::new(resource);
    let Some(src_format) = cache.get(&vh.vertex_format) else {
        return false;
    };

    // Attempt to fetch the target vertex format.
    let dst_format_name = match src_format.get_target_format_name(target_device) {
        Some(name) => name.to_string(),
        None => vh.vertex_format.clone(),
    };
    let dst_format_filename = VertexFormatCache::get_resource_path(&dst_format_name);
    dependencies.push(dst_format_filename);
    true
}

/// `huntPrimitiveDependencies`: walk the container for `vertices` sections
/// (morph vertices excluded) and hunt their format dependencies.
fn hunt_primitive_dependencies(
    filename: &str,
    target_device: &str,
    resource: &BwResource,
    dependencies: &mut Vec<String>,
) -> bool {
    let Some(raw) = resource.file_system().read_file(filename) else {
        return false;
    };
    let mut prim_file = BinSection::new(filename, BinaryBlock::new(raw));

    let children: Vec<BinSection> = prim_file.children().to_vec();
    for section in &children {
        let name = section.section_name().to_ascii_lowercase();
        if !section_name_ends_with(&name, VERTICES_ENDING) {
            // Did not match the ending.
            continue;
        }
        if section_name_ends_with(&name, MORPH_VERTICES_ENDING) {
            // Morph vertices not supported currently!
            continue;
        }

        // Found a vertices section, so execute conversion.
        let _ = hunt_vertex_dependencies(filename, section, target_device, resource, dependencies);
    }

    true
}

impl PrimitiveProcessor {
    /// Construct a converter with parameters (the `target` value,
    /// ';'-split, default `D3D9`).
    pub fn new(params: &str, resource: Rc<BwResource>) -> Self {
        // CommandLine cmd( params ); cmd.getParam( "target" ).
        let target_value = params
            .split_whitespace()
            .find_map(|token| token.strip_prefix("target="))
            .unwrap_or_default();
        let mut targets: Vec<String> = target_value
            .split(';')
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .collect();
        if targets.is_empty() {
            targets.push("D3D9".to_string());
        }
        Self {
            params: params.to_string(),
            targets,
            resource,
        }
    }
}

impl Converter for PrimitiveProcessor {
    fn create_dependencies(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        dependencies: &mut DependencyList<&dyn Compiler>,
    ) -> bool {
        if self.targets.len() > 1 {
            // Different output files would need to be configured if there
            // are multiple targets. Currently there isn't a well defined
            // strategy to deal with such a situation.
            compiler.set_error();
            return false;
        }

        let mut dependency_filenames: Vec<String> = Vec::new();
        let mut success = true;
        for target_name in &self.targets {
            success &= hunt_primitive_dependencies(
                source_file,
                target_name,
                &self.resource,
                &mut dependency_filenames,
            );
        }

        for dependency in &mut dependency_filenames {
            compiler.resolve_relative_path(dependency);
            dependencies.add_secondary_source_file_dependency(dependency.clone(), true);
        }

        success
    }

    fn convert(
        &self,
        source_file: &str,
        compiler: &dyn Compiler,
        _intermediate_files: &mut Vec<String>,
        output_files: &mut Vec<String>,
    ) -> bool {
        if self.targets.len() > 1 {
            compiler.set_error();
            return false;
        }

        let mut success = true;
        for target_name in &self.targets {
            let mut output_filename = format!("{source_file}.processed");
            compiler.resolve_output_path(&mut output_filename);
            self.resource.ensure_path_exists(&output_filename);

            success &=
                convert_primitives(source_file, &output_filename, target_name, &self.resource);

            output_files.push(output_filename);
        }

        success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependency::Dependency;
    use crate::primitive_file_structs::{
        IndexHeader, PrimitiveGroup, read_primitive_groups, write_primitive_groups,
    };
    use std::cell::RefCell;

    // --- fixture helpers -------------------------------------------------

    fn temp_root(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!("primproc-{tag}-{}", std::process::id()));
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

    /// Write one `shaders/formats/<name>.xml`.
    fn write_format(root: &str, name: &str, elements: &str, targets: &str) {
        std::fs::create_dir_all(format!("{root}/shaders/formats")).unwrap();
        std::fs::write(
            format!("{root}/shaders/formats/{name}.xml"),
            format!("<root>\n<elements>\n{elements}</elements>\n{targets}</root>\n"),
        )
        .unwrap();
    }

    /// The source format: position + normal + uv, all float.
    const XYZNUV_ELEMENTS: &str = concat!(
        "<POSITION><type>FLOAT3</type></POSITION>\n",
        "<NORMAL><type>FLOAT3</type></NORMAL>\n",
        "<TEXCOORD><type>FLOAT2</type><semanticIndex>0</semanticIndex></TEXCOORD>\n"
    );

    /// The packed target format: position float, normal 8-8-8, uv ubytes.
    const XYZNUVPC_ELEMENTS: &str = concat!(
        "<POSITION><type>FLOAT3</type></POSITION>\n",
        "<NORMAL><type>UBYTE4_NORMAL_8_8_8</type></NORMAL>\n",
        "<TEXCOORD><type>UBYTE2</type><semanticIndex>0</semanticIndex></TEXCOORD>\n"
    );

    fn write_formats(root: &str) {
        write_format(
            root,
            "xyznuv",
            XYZNUV_ELEMENTS,
            "<targets>\n<D3D9><name>D3D9</name><format>xyznuvpc</format></D3D9>\n</targets>\n",
        );
        write_format(root, "xyznuvpc", XYZNUVPC_ELEMENTS, "");
        // Known-stream formats declare their real stream indices (the
        // reference's uv2.xml/colour.xml put the element on streams
        // 10/11, which is what makes streamStride(10/11) answer).
        write_format(
            root,
            "uv2",
            "<TEXCOORD><type>FLOAT2</type><semanticIndex>0</semanticIndex><stream>10</stream></TEXCOORD>\n",
            "<targets>\n<D3D9><name>D3D9</name><format>uv2packed</format></D3D9>\n</targets>\n",
        );
        write_format(
            root,
            "uv2packed",
            "<TEXCOORD><type>SHORT2</type><semanticIndex>0</semanticIndex><stream>10</stream></TEXCOORD>\n",
            "",
        );
        write_format(
            root,
            "colour",
            "<COLOR><type>COLOR</type><stream>11</stream></COLOR>\n",
            "",
        );
    }

    /// Build a `.primitives` container: two vertices of xyznuv, an index
    /// list with one group, a uv2 stream, a colour stream, a passthrough
    /// blob and a morph-vertices blob.
    fn write_primitives_fixture(root: &str, name: &str) -> Vec<u8> {
        // Two xyznuv vertices: pos (1,2,3)/(4,5,6), normal (0,1,0)/(0,0,1),
        // uv (0.5,0.25)/(0.75,1.0).
        let mut vertices = Vec::new();
        VertexHeader {
            vertex_format: "xyznuv".to_string(),
            n_vertices: 2,
        }
        .write(&mut vertices);
        let vertices_data = [
            (
                1.0f32, 2.0f32, 3.0f32, 0.0f32, 1.0f32, 0.0f32, 0.5f32, 0.25f32,
            ),
            (
                4.0f32, 5.0f32, 6.0f32, 0.0f32, 0.0f32, 1.0f32, 0.75f32, 1.0f32,
            ),
        ];
        for &(x, y, z, nx, ny, nz, u, v) in &vertices_data {
            vertices.extend_from_slice(&x.to_le_bytes());
            vertices.extend_from_slice(&y.to_le_bytes());
            vertices.extend_from_slice(&z.to_le_bytes());
            vertices.extend_from_slice(&nx.to_le_bytes());
            vertices.extend_from_slice(&ny.to_le_bytes());
            vertices.extend_from_slice(&nz.to_le_bytes());
            vertices.extend_from_slice(&u.to_le_bytes());
            vertices.extend_from_slice(&v.to_le_bytes());
        }

        // Index list: 6 indices, one triangle group.
        let mut list = Vec::new();
        let header = IndexHeader {
            index_format: "list".to_string(),
            n_indices: 6,
            n_triangle_groups: 1,
        };
        list.extend_from_slice(header.index_format.as_bytes());
        list.resize(64, 0);
        list.extend_from_slice(&header.n_indices.to_le_bytes());
        list.extend_from_slice(&header.n_triangle_groups.to_le_bytes());
        for index in [0u16, 1, 2, 2, 1, 3] {
            list.extend_from_slice(&index.to_le_bytes());
        }
        write_primitive_groups(
            &[PrimitiveGroup {
                start_index: 0,
                n_primitives: 2,
                start_vertex: 0,
                n_vertices: 2,
            }],
            &mut list,
        );

        // uv2 stream: two FLOAT2s.
        let mut uv2 = Vec::new();
        for &(u, v) in &[(0.5f32, 0.25f32), (0.25f32, 0.5f32)] {
            uv2.extend_from_slice(&u.to_le_bytes());
            uv2.extend_from_slice(&v.to_le_bytes());
        }

        // colour stream: two COLORs.
        let colour = vec![10u8, 20, 30, 255, 40, 50, 60, 128];

        let passthrough = b"raw-blob".to_vec();
        let mvertices = b"morph-blob".to_vec();

        // Assemble the container through the BinSection writer.
        let mut container = BinSection::new(name, BinaryBlock::default());
        for (section_name, blob) in [
            ("vertices", &vertices),
            ("list", &list),
            ("uv2", &uv2),
            ("colour", &colour),
            ("positions", &passthrough),
            ("mvertices", &mvertices),
        ] {
            container
                .new_section(section_name)
                .set_binary(BinaryBlock::new(blob.clone()));
        }
        let bytes = container.collect_binary().data().to_vec();
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(format!("{root}/{name}"), &bytes).unwrap();
        bytes
    }

    /// The expected converted `vertices` payload: BPVT header + two packed
    /// xyznuvpc vertices.
    fn expected_vertices_blob() -> Vec<u8> {
        let mut blob = Vec::new();
        let header = ProcessedVertexHeader {
            magic: HeaderMagic::ProcessedVertices,
            original_vertex_format: "xyznuv".to_string(),
            vertex_header: VertexHeader {
                vertex_format: "xyznuvpc".to_string(),
                n_vertices: 2,
            },
        };
        header.write(&mut blob);
        // Vertex 0: position copied, normal (0,1,0) packed, uv packed.
        blob.extend_from_slice(&1.0f32.to_le_bytes());
        blob.extend_from_slice(&2.0f32.to_le_bytes());
        blob.extend_from_slice(&3.0f32.to_le_bytes());
        // 8-8-8 packing of (0,1,0): 127 + 127*component per axis.
        blob.extend_from_slice(&[127, 254, 127, 0]);
        // (u8)(0.5*255) = 127, (u8)(0.25*255) = 63.
        blob.extend_from_slice(&[127, 63]);
        // Vertex 1.
        blob.extend_from_slice(&4.0f32.to_le_bytes());
        blob.extend_from_slice(&5.0f32.to_le_bytes());
        blob.extend_from_slice(&6.0f32.to_le_bytes());
        // 8-8-8 packing of (0,0,1).
        blob.extend_from_slice(&[127, 127, 254, 0]);
        // (u8)(0.75*255) = 191, (u8)(1.0*255) = 255.
        blob.extend_from_slice(&[191, 255]);
        blob
    }

    /// The expected converted `uv2` payload: BPVS header + two SHORT2s.
    fn expected_uv2_blob() -> Vec<u8> {
        let mut blob = Vec::new();
        let header = ProcessedVertexHeader {
            magic: HeaderMagic::ProcessedVertexStream,
            original_vertex_format: "uv2".to_string(),
            vertex_header: VertexHeader {
                vertex_format: "uv2packed".to_string(),
                n_vertices: 2,
            },
        };
        header.write(&mut blob);
        // (i16)(f*2047+0.5): 0.5->1024, 0.25->512, 0.25->512, 0.5->1024.
        for packed in [1024i16, 512, 512, 1024] {
            blob.extend_from_slice(&packed.to_le_bytes());
        }
        blob
    }

    /// The expected `colour` payload: BPVS header with the source format
    /// kept (no D3D9 target) + the bytes reinterpreted as-is.
    fn expected_colour_blob(source: &[u8]) -> Vec<u8> {
        let mut blob = Vec::new();
        let header = ProcessedVertexHeader {
            magic: HeaderMagic::ProcessedVertexStream,
            original_vertex_format: "colour".to_string(),
            vertex_header: VertexHeader {
                vertex_format: "colour".to_string(),
                n_vertices: (source.len() / 4) as i32,
            },
        };
        header.write(&mut blob);
        blob.extend_from_slice(source);
        blob
    }

    // --- compiler mock ----------------------------------------------------

    /// A no-op compiler: paths pass through unchanged, error flags record.
    struct StubCompiler {
        error: RefCell<bool>,
    }

    impl StubCompiler {
        fn new() -> Self {
            Self {
                error: RefCell::new(false),
            }
        }
    }

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
        fn set_error(&self) {
            *self.error.borrow_mut() = true;
        }
        fn set_warning(&self) {}
        fn reset_error_flags(&self) {
            *self.error.borrow_mut() = false;
        }
        fn has_error(&self) -> bool {
            *self.error.borrow()
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

    /// The full round trip: a `.primitives` container converts into a
    /// `.processed` container whose sections parse back to the expected
    /// payloads.
    #[test]
    fn primitives_convert_round_trips_every_section() {
        let (root, resource) = make_resource("roundtrip");
        write_formats(&root);
        write_primitives_fixture(&root, "unit.primitives");

        let processor = PrimitiveProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(processor.convert(
            "unit.primitives",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
        assert_eq!(outputs, vec!["unit.primitives.processed".to_string()]);

        // Parse the output container back.
        let raw = std::fs::read(format!("{root}/unit.primitives.processed")).unwrap();
        let mut out = BinSection::new("processed", BinaryBlock::new(raw));
        assert_eq!(out.count_children(), 6);

        // vertices: BPVT with the packed payload.
        let vertices = out.read_binary("vertices").unwrap();
        assert_eq!(vertices.data(), expected_vertices_blob().as_slice());

        // uv2: BPVS with SHORT2 payload.
        let uv2 = out.read_binary("uv2").unwrap();
        assert_eq!(uv2.data(), expected_uv2_blob().as_slice());

        // colour: BPVS identity (no D3D9 target registered).
        let colour = out.read_binary("colour").unwrap();
        assert_eq!(
            colour.data(),
            expected_colour_blob(&[10, 20, 30, 255, 40, 50, 60, 128]).as_slice()
        );

        // list / positions / mvertices: verbatim copies.
        let mut input = BinSection::new(
            "input",
            BinaryBlock::new(write_primitives_fixture(&root, "unit-check.primitives")),
        );
        assert_eq!(
            out.read_binary("list").unwrap().data(),
            input.read_binary("list").unwrap().data()
        );
        assert_eq!(
            out.read_binary("positions").unwrap().data(),
            input.read_binary("positions").unwrap().data()
        );
        assert_eq!(
            out.read_binary("mvertices").unwrap().data(),
            input.read_binary("mvertices").unwrap().data()
        );

        // The converted vertices payload parses back through the file
        // structs: BPVT magic, original + target format names, count.
        let processed = out.read_binary("vertices").unwrap();
        let mut expected_prefix = Vec::new();
        let header = ProcessedVertexHeader {
            magic: HeaderMagic::ProcessedVertices,
            original_vertex_format: "xyznuv".to_string(),
            vertex_header: VertexHeader {
                vertex_format: "xyznuvpc".to_string(),
                n_vertices: 2,
            },
        };
        header.write(&mut expected_prefix);
        assert!(processed.data().starts_with(&expected_prefix));
        let vh = VertexHeader::read(&processed.data()[4 + 64..]).unwrap();
        assert_eq!(vh.vertex_format, "xyznuvpc");
        assert_eq!(vh.n_vertices, 2);
    }

    /// The converted index list parses back through `IndexHeader` and the
    /// primitive groups round-trip.
    #[test]
    fn index_list_round_trips_through_file_structs() {
        let mut bytes = Vec::new();
        let header = IndexHeader {
            index_format: "list32".to_string(),
            n_indices: 3,
            n_triangle_groups: 1,
        };
        bytes.extend_from_slice(header.index_format.as_bytes());
        bytes.resize(64, 0);
        bytes.extend_from_slice(&header.n_indices.to_le_bytes());
        bytes.extend_from_slice(&header.n_triangle_groups.to_le_bytes());
        for index in [7u32, 8, 9] {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        write_primitive_groups(
            &[PrimitiveGroup {
                start_index: 0,
                n_primitives: 1,
                start_vertex: 2,
                n_vertices: 3,
            }],
            &mut bytes,
        );

        let parsed = IndexHeader::read(&bytes).unwrap();
        assert_eq!(parsed.index_format, "list32");
        assert_eq!(parsed.n_indices, 3);
        assert_eq!(parsed.n_triangle_groups, 1);

        let groups = read_primitive_groups(&bytes[IndexHeader::SIZE + 12..], 1);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].start_vertex, 2);
        assert_eq!(groups[0].n_vertices, 3);
    }

    /// `create_dependencies` registers the source and target format XMLs of
    /// every `vertices` section (known streams and passthrough sections
    /// contribute none).
    #[test]
    fn create_dependencies_hunts_vertex_format_files() {
        let (root, resource) = make_resource("deps");
        write_formats(&root);
        write_primitives_fixture(&root, "unit.primitives");

        let processor = PrimitiveProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let compiler_dyn: &dyn Compiler = &compiler;
        let mut dependencies = DependencyList::new(compiler_dyn);
        assert!(processor.create_dependencies("unit.primitives", &compiler, &mut dependencies));

        let names: Vec<&str> = dependencies
            .secondary_inputs()
            .iter()
            .filter_map(|(dependency, _)| match dependency {
                Dependency::SourceFile(dep) => Some(dep.file_name()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            ["shaders/formats/xyznuv.xml", "shaders/formats/xyznuvpc.xml"]
        );
    }

    /// More than one target fails both entry points with the error flag
    /// raised.
    #[test]
    fn multiple_targets_fail_both_entry_points() {
        let (root, resource) = make_resource("multitarget");
        write_formats(&root);
        write_primitives_fixture(&root, "unit.primitives");

        let processor = PrimitiveProcessor::new("target=D3D9;XENON", Rc::clone(&resource));
        let compiler = StubCompiler::new();

        let compiler_dyn: &dyn Compiler = &compiler;
        let mut dependencies = DependencyList::new(compiler_dyn);
        assert!(!processor.create_dependencies("unit.primitives", &compiler, &mut dependencies));
        assert!(compiler.has_error());

        compiler.reset_error_flags();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "unit.primitives",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
        assert!(compiler.has_error());
        assert!(outputs.is_empty());
    }

    /// A missing primitives file fails cleanly.
    #[test]
    fn a_missing_primitives_file_fails() {
        let (_root, resource) = make_resource("missing");
        let processor = PrimitiveProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "nope.primitives",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
    }

    /// A vertices section whose format XML does not exist fails the
    /// conversion.
    #[test]
    fn an_unknown_vertex_format_fails_the_conversion() {
        let (root, resource) = make_resource("unknownformat");
        write_formats(&root);
        let mut vertices = Vec::new();
        VertexHeader {
            vertex_format: "nope".to_string(),
            n_vertices: 1,
        }
        .write(&mut vertices);
        vertices.extend_from_slice(&0.0f32.to_le_bytes());
        let mut container = BinSection::new("bad.primitives", BinaryBlock::default());
        container
            .new_section("vertices")
            .set_binary(BinaryBlock::new(vertices));
        std::fs::write(
            format!("{root}/bad.primitives"),
            container.collect_binary().data(),
        )
        .unwrap();

        let processor = PrimitiveProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "bad.primitives",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
    }

    /// A vertices section whose byte length disagrees with the header
    /// count and source stride fails.
    #[test]
    fn a_truncated_vertices_section_fails() {
        let (root, resource) = make_resource("truncated");
        write_formats(&root);
        let mut vertices = Vec::new();
        VertexHeader {
            vertex_format: "xyznuv".to_string(),
            n_vertices: 2,
        }
        .write(&mut vertices);
        // Only one vertex worth of bytes.
        vertices.extend_from_slice(&[0u8; 32]);
        let mut container = BinSection::new("short.primitives", BinaryBlock::default());
        container
            .new_section("vertices")
            .set_binary(BinaryBlock::new(vertices));
        std::fs::write(
            format!("{root}/short.primitives"),
            container.collect_binary().data(),
        )
        .unwrap();

        let processor = PrimitiveProcessor::new("", Rc::clone(&resource));
        let compiler = StubCompiler::new();
        let mut outputs = Vec::new();
        let mut intermediates = Vec::new();
        assert!(!processor.convert(
            "short.primitives",
            &compiler,
            &mut intermediates,
            &mut outputs
        ));
    }

    /// The `target` param value splits on ';' and defaults to D3D9.
    #[test]
    fn target_params_split_and_default() {
        let (_root, resource) = make_resource("params");
        let processor = PrimitiveProcessor::new("target=a;b", Rc::clone(&resource));
        assert_eq!(processor.targets.len(), 2);

        let processor = PrimitiveProcessor::new("", Rc::clone(&resource));
        assert_eq!(processor.targets, ["D3D9"]);
    }
}
