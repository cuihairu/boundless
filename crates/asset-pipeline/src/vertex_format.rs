//! Vertex format definitions and conversions (`moo/vertex_element.hpp`,
//! `moo/vertex_format.{hpp,cpp}`, `moo/vertex_format_conversions.*` and
//! `moo/vertex_format_cache.{cpp,ipp}`): the machinery the primitive and
//! visual processors use to convert vertex streams between formats.
//!
//! Reference behaviour preserved:
//! - formats declare streams of elements (semantic + storage + index +
//!   offset); an element without an explicit `offset` lands at the stream's
//!   current stride, one without a `semanticIndex` takes the first free
//!   index, and the stride grows to cover each element (`addElement`);
//! - a format XML (`shaders/formats/<name>.xml`, the cache's resource
//!   path) holds an `elements` block and an optional `targets` block
//!   mapping target keys (device names) to other format names;
//! - the conversion matrix is deliberately sparse — only the pairs the
//!   reference enables exist (float↔ubyte normalised, float↔short packed,
//!   like-type component assignment with zero fill, colour reinterpret,
//!   the special-case blend-index layouts) plus the semantic specials
//!   (position w-expansion, normal packing schemes, blend-index
//!   broadcast); everything else fails the conversion;
//! - storage sizes with no runtime conversion path (the GPU packed types
//!   `HALF2/HALF4/UBYTE4N/SHORT2N/SHORT4N/USHORT2N/USHORT4N/UDEC3/DEC3N`)
//!   only convert to themselves (the reference's dispatch macro list
//!   omits them).
//!
//! Port deviations (structural, behaviour preserved): the C++ float→ubyte
//! and float→short casts are C truncating conversions — reproduced with
//! `as i32 as u8` / `as i32 as i16` rather than Rust's saturating `as`,
//! so out-of-range values wrap exactly like the reference builds; the
//! reference's static cache singleton is an owned [`VertexFormatCache`]
//! handed the resource it loads format XMLs through.

use std::collections::HashMap;

use resources::bwresource::BwResource;
use resources::datasection::DataSection;

/// `VertexElement::SemanticType::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SemanticType {
    Unknown,
    Position,
    BlendWeight,
    BlendIndices,
    Normal,
    Psize,
    TexCoord,
    Tangent,
    Binormal,
    TessFactor,
    PositionT,
    Color,
    Fog,
    Depth,
    Sample,
}

/// The semantic string table (`s_semanticData`, order preserved).
const SEMANTIC_NAMES: [(&str, SemanticType); 14] = [
    ("POSITION", SemanticType::Position),
    ("BLENDWEIGHT", SemanticType::BlendWeight),
    ("BLENDINDICES", SemanticType::BlendIndices),
    ("NORMAL", SemanticType::Normal),
    ("PSIZE", SemanticType::Psize),
    ("TEXCOORD", SemanticType::TexCoord),
    ("TANGENT", SemanticType::Tangent),
    ("BINORMAL", SemanticType::Binormal),
    ("TESSFACTOR", SemanticType::TessFactor),
    ("POSITIONT", SemanticType::PositionT),
    ("COLOR", SemanticType::Color),
    ("FOG", SemanticType::Fog),
    ("DEPTH", SemanticType::Depth),
    ("SAMPLE", SemanticType::Sample),
];

impl SemanticType {
    /// `SemanticFromString`: case-insensitive; unknown yields `None` (the
    /// reference logs and returns UNKNOWN).
    pub fn from_string(name: &str) -> Option<Self> {
        SEMANTIC_NAMES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, semantic)| *semantic)
    }
}

/// `VertexElement::StorageType::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageType {
    Unknown,
    Float1,
    Float2,
    Float3,
    Float4,
    Half2,
    Half4,
    Color,
    Ubyte1,
    Ubyte2,
    Ubyte3,
    Ubyte4,
    /// In a NORMALs context this is the (11, 11, 10) packing.
    Ubyte4Normal888,
    Short1,
    Short2,
    Short3,
    Short4,
    Ubyte4N,
    Short2N,
    Short4N,
    Ushort2N,
    Ushort4N,
    Udec3,
    Dec3N,
    /// Special case: three blend indices, `Ubyte3` on extraction.
    ScDiv3Iii,
    ScReverseIii,
    ScReversePaddedIii,
    ScReversePaddedWw,
    ScReversePaddedWw_,
}

/// The storage string table (`s_storageData`, order preserved; HALF2/HALF4
/// answer to their `FLOAT16_x` spellings).
const STORAGE_NAMES: [(&str, StorageType); 28] = [
    ("FLOAT1", StorageType::Float1),
    ("FLOAT2", StorageType::Float2),
    ("FLOAT3", StorageType::Float3),
    ("FLOAT4", StorageType::Float4),
    ("COLOR", StorageType::Color),
    ("UBYTE1", StorageType::Ubyte1),
    ("UBYTE2", StorageType::Ubyte2),
    ("UBYTE3", StorageType::Ubyte3),
    ("UBYTE4", StorageType::Ubyte4),
    ("UBYTE4_NORMAL_8_8_8", StorageType::Ubyte4Normal888),
    ("SHORT1", StorageType::Short1),
    ("SHORT2", StorageType::Short2),
    ("SHORT3", StorageType::Short3),
    ("SHORT4", StorageType::Short4),
    ("UBYTE4N", StorageType::Ubyte4N),
    ("SHORT2N", StorageType::Short2N),
    ("SHORT4N", StorageType::Short4N),
    ("USHORT2N", StorageType::Ushort2N),
    ("USHORT4N", StorageType::Ushort4N),
    ("UDEC3", StorageType::Udec3),
    ("DEC3N", StorageType::Dec3N),
    ("FLOAT16_2", StorageType::Half2),
    ("FLOAT16_4", StorageType::Half4),
    ("SC_DIV3_III", StorageType::ScDiv3Iii),
    ("SC_REVERSE_III", StorageType::ScReverseIii),
    ("SC_REVERSE_PADDED_III_", StorageType::ScReversePaddedIii),
    ("SC_REVERSE_PADDED__WW_", StorageType::ScReversePaddedWw),
    ("SC_REVERSE_PADDED_WW_", StorageType::ScReversePaddedWw_),
];

impl StorageType {
    /// `StorageFromString`: case-insensitive; unknown yields `None`.
    pub fn from_string(name: &str) -> Option<Self> {
        STORAGE_NAMES
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, storage)| *storage)
    }

    /// `VertexElement::typeSize`: the byte size (0 for unknown).
    pub fn size(self) -> usize {
        match self {
            Self::Float1 => 4,
            Self::Float2 => 8,
            Self::Float3 => 12,
            Self::Float4 => 16,
            Self::Color => 4,
            Self::Ubyte1 => 1,
            Self::Ubyte2 => 2,
            Self::Ubyte3 => 3,
            Self::Ubyte4 => 4,
            Self::Ubyte4Normal888 => 4,
            Self::Short1 => 2,
            Self::Short2 => 4,
            Self::Short3 => 6,
            Self::Short4 => 8,
            Self::Ubyte4N | Self::Short2N | Self::Ushort2N => 4,
            Self::Short4N | Self::Ushort4N | Self::Half2 => 8,
            Self::Half4 => 8,
            Self::Udec3 | Self::Dec3N => 4,
            Self::ScDiv3Iii | Self::ScReverseIii => 3,
            Self::ScReversePaddedIii | Self::ScReversePaddedWw => 4,
            Self::ScReversePaddedWw_ => 3,
            Self::Unknown => 0,
        }
    }

    /// `VertexElement::componentCount` (0 for unknown).
    pub fn components(self) -> usize {
        match self {
            Self::Float1 | Self::Ubyte1 | Self::Short1 => 1,
            Self::Float2
            | Self::Ubyte2
            | Self::Short2
            | Self::Short2N
            | Self::Ushort2N
            | Self::Half2
            | Self::ScReversePaddedWw
            | Self::ScReversePaddedWw_ => 2,
            Self::Float3
            | Self::Ubyte3
            | Self::Short3
            | Self::Udec3
            | Self::Dec3N
            | Self::Ubyte4Normal888
            | Self::ScDiv3Iii
            | Self::ScReverseIii
            | Self::ScReversePaddedIii => 3,
            Self::Float4
            | Self::Ubyte4
            | Self::Short4
            | Self::Color
            | Self::Ubyte4N
            | Self::Short4N
            | Self::Ushort4N
            | Self::Half4 => 4,
            Self::Unknown => 0,
        }
    }

    /// The component byte size for the component-based conversions: floats
    /// are 4 bytes, ubytes 1, shorts 2, colour components are ubytes.
    fn component_size(self) -> usize {
        match self {
            Self::Float1 | Self::Float2 | Self::Float3 | Self::Float4 => 4,
            Self::Ubyte1
            | Self::Ubyte2
            | Self::Ubyte3
            | Self::Ubyte4
            | Self::Color
            | Self::Ubyte4Normal888 => 1,
            Self::Short1 | Self::Short2 | Self::Short3 | Self::Short4 => 2,
            _ => 0,
        }
    }
}

/// An element of a stream (`VertexFormat::ElementDefinition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementDefinition {
    pub semantic: SemanticType,
    pub semantic_index: u32,
    pub storage: StorageType,
    pub offset: usize,
}

/// A stream of elements (`VertexFormat::StreamDefinition`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamDefinition {
    pub elements: Vec<ElementDefinition>,
    pub stride: u32,
}

/// The `DEFAULT` marker of the reference (`0xFFFFFFFF`); the port uses
/// `Option` at the call sites.
pub const DEFAULT_INDEX: u32 = u32::MAX;

/// A vertex format definition (`VertexFormat`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VertexFormat {
    name: String,
    streams: Vec<StreamDefinition>,
    /// `formatTargets_`: target key → format name.
    targets: Vec<(String, String)>,
}

impl VertexFormat {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }

    /// `VertexFormat::name`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `addStream`.
    pub fn add_stream(&mut self) -> u32 {
        self.streams.push(StreamDefinition::default());
        (self.streams.len() - 1) as u32
    }

    /// `streamCount`.
    pub fn stream_count(&self) -> u32 {
        self.streams.len() as u32
    }

    /// `streamStride` (0 beyond the last stream).
    pub fn stream_stride(&self, stream_index: u32) -> u32 {
        self.streams
            .get(stream_index as usize)
            .map_or(0, |stream| stream.stride)
    }

    /// `addElement`: an omitted offset lands at the current stride, an
    /// omitted semantic index takes the first free index, and the stride
    /// grows to cover the element.
    pub fn add_element(
        &mut self,
        stream_index: u32,
        semantic: SemanticType,
        storage: StorageType,
        semantic_index: Option<u32>,
        offset: Option<usize>,
    ) {
        let semantic_index = semantic_index.unwrap_or_else(|| {
            let mut index = 0;
            while self.contains_element(semantic, index) {
                index += 1;
            }
            index
        });
        let stream = &mut self.streams[stream_index as usize];
        let offset = offset.unwrap_or(stream.stride as usize);
        let stride = (offset + storage.size()).max(stream.stride as usize);
        stream.stride = stride as u32;
        stream.elements.push(ElementDefinition {
            semantic,
            semantic_index,
            storage,
            offset,
        });
    }

    /// `findElement` by semantic across all streams.
    pub fn find_element(
        &self,
        semantic: SemanticType,
        semantic_index: u32,
    ) -> Option<(u32, usize, StorageType)> {
        for (stream_index, stream) in self.streams.iter().enumerate() {
            for element in &stream.elements {
                if element.semantic == semantic && element.semantic_index == semantic_index {
                    return Some((stream_index as u32, element.offset, element.storage));
                }
            }
        }
        None
    }

    /// `containsElement`.
    pub fn contains_element(&self, semantic: SemanticType, semantic_index: u32) -> bool {
        self.find_element(semantic, semantic_index).is_some()
    }

    /// `countElements` over all streams.
    pub fn count_elements(&self) -> usize {
        self.streams
            .iter()
            .map(|stream| stream.elements.len())
            .sum()
    }

    /// The flat element list in declaration order (the driver's
    /// `findElement(index)` walk), as `(stream, element)` pairs.
    pub fn elements(&self) -> impl Iterator<Item = (u32, &ElementDefinition)> {
        self.streams
            .iter()
            .enumerate()
            .flat_map(|(stream_index, stream)| {
                stream
                    .elements
                    .iter()
                    .map(move |element| (stream_index as u32, element))
            })
    }

    /// `addFormatTarget`.
    pub fn add_format_target(&mut self, target_name: &str, format: &str) {
        self.targets
            .push((target_name.to_string(), format.to_string()));
    }

    /// `getTargetFormatName`: the format name registered for `target_key`.
    pub fn get_target_format_name(&self, target_key: &str) -> Option<&str> {
        self.targets
            .iter()
            .find(|(name, _)| name == target_key)
            .map(|(_, format)| format.as_str())
    }

    /// `VertexFormat::load`: parse a format XML (see the module docs). A
    /// format is valid once at least one element was added (`ret`).
    pub fn load(format: &mut Self, section: &DataSection, name: &str) -> bool {
        format.name = name.to_string();
        let mut ret = false;

        fn children(section: &DataSection) -> Vec<&DataSection> {
            (0..section.count_children())
                .filter_map(|index| section.open_child(index))
                .collect()
        }
        for child in children(section) {
            match child.section_name() {
                "elements" => {
                    for element_section in children(child) {
                        let Some(semantic) =
                            SemanticType::from_string(element_section.section_name())
                        else {
                            continue;
                        };
                        let storage_name = element_section.read_string("type", "FLOAT3");
                        let Some(storage) = StorageType::from_string(&storage_name) else {
                            continue;
                        };
                        let read_int = |tag: &str| -> Option<i32> {
                            element_section.find_child(tag).map(|value| value.as_int(0))
                        };
                        let semantic_index =
                            read_int("semanticIndex").and_then(|v| u32::try_from(v).ok());
                        let stream = read_int("stream")
                            .and_then(|v| u32::try_from(v).ok())
                            .unwrap_or(if format.stream_count() > 0 {
                                format.stream_count() - 1
                            } else {
                                0
                            });
                        let offset = read_int("offset").and_then(|v| usize::try_from(v).ok());

                        while stream >= format.stream_count() {
                            format.add_stream();
                        }
                        format.add_element(stream, semantic, storage, semantic_index, offset);
                        ret = true;
                    }
                }
                "targets" => {
                    for target_section in children(child) {
                        let format_target = target_section.read_string("format", "");
                        if !format_target.is_empty() {
                            let target_name = target_section.read_string("name", "");
                            format.add_format_target(&target_name, &format_target);
                        }
                    }
                }
                _ => {}
            }
        }
        ret
    }
}

/// The format cache (`VertexFormatCache`): formats by name, loaded on
/// demand from `shaders/formats/<name>.xml` through the resource.
pub struct VertexFormatCache<'a> {
    formats: HashMap<String, std::rc::Rc<VertexFormat>>,
    resource: &'a BwResource,
}

impl<'a> VertexFormatCache<'a> {
    pub fn new(resource: &'a BwResource) -> Self {
        Self {
            formats: HashMap::new(),
            resource,
        }
    }

    /// `VertexFormatCacheInternal::getResourcePath`.
    pub fn get_resource_path(format_name: &str) -> String {
        format!("shaders/formats/{format_name}.xml")
    }

    /// `get`: the cached format, or a load attempt from the resource path.
    pub fn get(&mut self, format_name: &str) -> Option<std::rc::Rc<VertexFormat>> {
        if let Some(format) = self.formats.get(format_name) {
            return Some(std::rc::Rc::clone(format));
        }
        let path = Self::get_resource_path(format_name);
        let mut format = VertexFormat::default();
        let loaded = self
            .resource
            .open_section(&path, false)
            .map(|section| VertexFormat::load(&mut format, &section, format_name))
            .unwrap_or(false);
        if !loaded {
            return None;
        }
        let format = std::rc::Rc::new(format);
        self.formats
            .insert(format_name.to_string(), std::rc::Rc::clone(&format));
        Some(format)
    }

    /// `add`: insert without overwriting an existing name.
    pub fn add(
        &mut self,
        format_name: &str,
        format: VertexFormat,
    ) -> Option<std::rc::Rc<VertexFormat>> {
        if self.formats.contains_key(format_name) {
            return None;
        }
        let format = std::rc::Rc::new(format);
        self.formats
            .insert(format_name.to_string(), std::rc::Rc::clone(&format));
        Some(format)
    }

    /// `getTarget`: the format for the source's target key, falling back
    /// to the source format itself.
    pub fn get_target(
        &mut self,
        source: &VertexFormat,
        target_key: &str,
        fallback_to_source: bool,
    ) -> Option<std::rc::Rc<VertexFormat>> {
        if let Some(target_name) = source.get_target_format_name(target_key) {
            if let Some(target) = self.get(target_name) {
                return Some(target);
            }
        } else if fallback_to_source {
            // The source hands back a handle to itself; cache it under its
            // own name so the identity is shared.
            if !self.formats.contains_key(source.name()) {
                self.formats
                    .insert(source.name().to_string(), std::rc::Rc::new(source.clone()));
            }
            return self.formats.get(source.name()).cloned();
        }
        None
    }
}

// -----------------------------------------------------------------------------
// Section: conversions (`VertexConversions`)
// -----------------------------------------------------------------------------

/// `packFloatToInt16`: truncating fixed-point pack (`src * 2047 + 0.5`).
fn pack_float_to_int16(src: f32) -> i16 {
    ((src * 2047.0 + 0.5) as i32) as i16
}

/// `unpackInt16ToFloat`.
fn unpack_int16_to_float(src: i16) -> f32 {
    src as f32 / 2047.0
}

/// `packNormal`: 11-11-10 packing of a normalised, clamped float3.
// The reference's expression keeps the `<< 0` low-word term for symmetry
// with the packed words above.
#[allow(clippy::identity_op)]
fn pack_normal(x: f32, y: f32, z: f32) -> u32 {
    let len = (x * x + y * y + z * z).sqrt();
    let (mut x, mut y, mut z) = (x, y, z);
    if len != 0.0 {
        x /= len;
        y /= len;
        z /= len;
    }
    let x = x.clamp(-1.0, 1.0);
    let y = y.clamp(-1.0, 1.0);
    let z = z.clamp(-1.0, 1.0);
    ((((z * 511.0) as u32) & 0x3ff) << 22)
        | ((((y * 1023.0) as u32) & 0x7ff) << 11)
        | ((((x * 1023.0) as u32) & 0x7ff) << 0)
}

/// `unpackNormal`: 11-11-10 back to float3.
fn unpack_normal(packed: u32) -> (f32, f32, f32) {
    let z = ((packed as i32) >> 22) as f32;
    let y = (packed << 10 >> 21) as i32 as f32;
    let x = (packed << 21 >> 21) as i32 as f32;
    (x / 1023.0, y / 1023.0, z / 511.0)
}

/// `packFloat3toUByte4Normal888`: 8-8-8 packing of a normalised float3.
// The reference's expression keeps the `<< 0` low-word term for symmetry
// with the packed words above.
#[allow(clippy::identity_op)]
fn pack_float3_to_ubyte4_normal_888(x: f32, y: f32, z: f32) -> u32 {
    let len = (x * x + y * y + z * z).sqrt();
    let (mut x, mut y, mut z) = (x, y, z);
    if len != 0.0 {
        x /= len;
        y /= len;
        z /= len;
    }
    let x = x.clamp(-1.0, 1.0);
    let y = y.clamp(-1.0, 1.0);
    let z = z.clamp(-1.0, 1.0);
    (((127 + (z * 127.0) as u32) & 0xFF) << 16)
        | (((127 + (y * 127.0) as u32) & 0xFF) << 8)
        | (((127 + (x * 127.0) as u32) & 0xFF) << 0)
}

/// `unpackUByte4Normal888toFloat3`.
fn unpack_ubyte4_normal_888_to_float3(packed: u32) -> (f32, f32, f32) {
    let z = ((packed >> 16) & 0xFF) as i32;
    let y = ((packed >> 8) & 0xFF) as i32;
    let x = (packed & 0xFF) as i32;
    (
        (x - 127) as f32 / 127.0,
        (y - 127) as f32 / 127.0,
        (z - 127) as f32 / 127.0,
    )
}

/// The C++ truncating `static_cast<uint8>(float)`: convert through int,
/// which wraps out-of-range values instead of saturating.
fn c_cast_u8(value: f32) -> u8 {
    (value as i32) as u8
}

/// `copyComponents` semantics: copy `min(dst, src)` component values (raw
/// bytes for same-type assignment), then zero-fill the remaining
/// destination components.
fn copy_components(
    dst: &mut [u8],
    src: &[u8],
    dst_count: usize,
    src_count: usize,
    src_component_size: usize,
    dst_component_size: usize,
    convert: impl Fn(&[u8], &[u8]) -> Vec<u8>,
) {
    let count = dst_count.min(src_count);
    for i in 0..count {
        let value = convert(
            &src[i * src_component_size..(i + 1) * src_component_size],
            &dst[i * dst_component_size..(i + 1) * dst_component_size],
        );
        dst[i * dst_component_size..(i + 1) * dst_component_size].copy_from_slice(&value);
    }
    for i in count..dst_count {
        dst[i * dst_component_size..(i + 1) * dst_component_size].fill(0);
    }
}

fn read_f32(bytes: &[u8]) -> f32 {
    f32::from_le_bytes(bytes.try_into().unwrap())
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().unwrap())
}

fn f32_bytes(value: f32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// `VertexConversions::convertValue`: convert one element value between
/// storage types under a semantic. `dst` and `src` are the full element
/// bytes; both sides must be sized to their storage type.
pub fn convert_value(
    semantic: SemanticType,
    dst: &mut [u8],
    dst_type: StorageType,
    src: &[u8],
    src_type: StorageType,
) -> bool {
    if dst_type == StorageType::Unknown || src_type == StorageType::Unknown {
        return false;
    }

    // The semantic specials (see the reference's fetchConvertFunction
    // specialisations).
    let normal_like = matches!(
        semantic,
        SemanticType::Normal | SemanticType::Tangent | SemanticType::Binormal
    );
    if semantic == SemanticType::Position
        && dst_type == StorageType::Float4
        && src_type == StorageType::Float3
    {
        // positionAddWConversion: standard f3->f4 then w = 1.
        if !default_convert(dst, dst_type, src, src_type) {
            return false;
        }
        dst[12..16].copy_from_slice(&1.0f32.to_le_bytes());
        return true;
    }
    if normal_like {
        if dst_type == StorageType::Float3
            && matches!(src_type, StorageType::Ubyte4 | StorageType::Ubyte4N)
        {
            let (x, y, z) = unpack_normal(read_u32(src));
            dst[0..4].copy_from_slice(&x.to_le_bytes());
            dst[4..8].copy_from_slice(&y.to_le_bytes());
            dst[8..12].copy_from_slice(&z.to_le_bytes());
            return true;
        }
        if matches!(dst_type, StorageType::Ubyte4 | StorageType::Ubyte4N)
            && src_type == StorageType::Float3
        {
            let packed = pack_normal(
                read_f32(&src[0..4]),
                read_f32(&src[4..8]),
                read_f32(&src[8..12]),
            );
            dst[0..4].copy_from_slice(&packed.to_le_bytes());
            return true;
        }
        if dst_type == StorageType::Float3 && src_type == StorageType::Ubyte4Normal888 {
            let (x, y, z) = unpack_ubyte4_normal_888_to_float3(read_u32(src));
            dst[0..4].copy_from_slice(&x.to_le_bytes());
            dst[4..8].copy_from_slice(&y.to_le_bytes());
            dst[8..12].copy_from_slice(&z.to_le_bytes());
            return true;
        }
        if dst_type == StorageType::Ubyte4Normal888 && src_type == StorageType::Float3 {
            let packed = pack_float3_to_ubyte4_normal_888(
                read_f32(&src[0..4]),
                read_f32(&src[4..8]),
                read_f32(&src[8..12]),
            );
            dst[0..4].copy_from_slice(&packed.to_le_bytes());
            return true;
        }
        if dst_type == StorageType::Ubyte4Normal888
            && matches!(src_type, StorageType::Ubyte4 | StorageType::Ubyte4N)
        {
            // 11-11-10 -> 8-8-8 through float3.
            let (x, y, z) = unpack_normal(read_u32(src));
            let packed = pack_float3_to_ubyte4_normal_888(x, y, z);
            dst[0..4].copy_from_slice(&packed.to_le_bytes());
            return true;
        }
        if matches!(dst_type, StorageType::Ubyte4 | StorageType::Ubyte4N)
            && src_type == StorageType::Ubyte4Normal888
        {
            let (x, y, z) = unpack_ubyte4_normal_888_to_float3(read_u32(src));
            let packed = pack_normal(x, y, z);
            dst[0..4].copy_from_slice(&packed.to_le_bytes());
            return true;
        }
    }
    if semantic == SemanticType::BlendIndices
        && dst_type == StorageType::Ubyte3
        && src_type == StorageType::Float1
    {
        // copyBlendIndicesUbyte3FromFloat1: broadcast the one index.
        let index = c_cast_u8(read_f32(src));
        dst[0] = index;
        dst[1] = index;
        dst[2] = index;
        return true;
    }

    default_convert(dst, dst_type, src, src_type)
}

/// `fetchDefaultConvertFunction` + the enabled matrix: same type reinterprets,
/// the component conversions run for the enabled pairs, and everything
/// else fails.
fn default_convert(
    dst: &mut [u8],
    dst_type: StorageType,
    src: &[u8],
    src_type: StorageType,
) -> bool {
    if dst_type == src_type {
        // reinterpretConvert with the destination size.
        let size = dst_type.size().min(dst.len());
        dst[..size].copy_from_slice(&src[..size]);
        return true;
    }

    use StorageType::*;
    let pair = (dst_type, src_type);
    match pair {
        // Ubyte[n] <-> Float[m]: normalised components.
        (Float1 | Float2 | Float3 | Float4, Float1 | Float2 | Float3 | Float4)
        | (Ubyte1 | Ubyte2 | Ubyte3 | Ubyte4, Ubyte1 | Ubyte2 | Ubyte3 | Ubyte4)
        | (Short1 | Short2 | Short3 | Short4, Short1 | Short2 | Short3 | Short4) => {
            // AssignmentComponentConverter: component-wise copy, zero fill.
            copy_components(
                dst,
                src,
                dst_type.components(),
                src_type.components(),
                src_type.component_size(),
                dst_type.component_size(),
                |src_component, _| src_component.to_vec(),
            );
            true
        }
        (Float1 | Float2 | Float3 | Float4, Ubyte1 | Ubyte2 | Ubyte3 | Ubyte4) => {
            copy_components(
                dst,
                src,
                dst_type.components(),
                src_type.components(),
                1,
                4,
                |src_component, _| f32_bytes(src_component[0] as f32 / 255.0),
            );
            true
        }
        (Ubyte1 | Ubyte2 | Ubyte3 | Ubyte4, Float1 | Float2 | Float3 | Float4) => {
            copy_components(
                dst,
                src,
                dst_type.components(),
                src_type.components(),
                4,
                1,
                |src_component, _| vec![c_cast_u8(read_f32(src_component) * 255.0)],
            );
            true
        }
        (Float1 | Float2 | Float3 | Float4, Short1 | Short2 | Short3 | Short4) => {
            copy_components(
                dst,
                src,
                dst_type.components(),
                src_type.components(),
                2,
                4,
                |src_component, _| {
                    f32_bytes(unpack_int16_to_float(i16::from_le_bytes(
                        src_component.try_into().unwrap(),
                    )))
                },
            );
            true
        }
        (Short1 | Short2 | Short3 | Short4, Float1 | Float2 | Float3 | Float4) => {
            copy_components(
                dst,
                src,
                dst_type.components(),
                src_type.components(),
                4,
                2,
                |src_component, _| {
                    pack_float_to_int16(read_f32(src_component))
                        .to_le_bytes()
                        .to_vec()
                },
            );
            true
        }
        // Ubyte4 <-> Color: reinterpret (min size).
        (Ubyte4, Color) | (Color, Ubyte4) => {
            let size = dst_type.size().min(src_type.size());
            dst[..size].copy_from_slice(&src[..size]);
            true
        }
        // Ubyte[2|3] <-> Color: component assignment.
        (Ubyte2 | Ubyte3, Color) | (Color, Ubyte2 | Ubyte3) => {
            copy_components(
                dst,
                src,
                dst_type.components(),
                src_type.components(),
                1,
                1,
                |src_component, _| src_component.to_vec(),
            );
            true
        }
        // The special-case layouts: reversed blend-index/weight bytes.
        (ScReverseIii, Ubyte3) | (ScReversePaddedIii, Ubyte3) => {
            // operator Ubyte3(): z=i1, y=i2, x=i3.
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            true
        }
        (Ubyte3, ScReverseIii) | (Ubyte3, ScReversePaddedIii) => {
            // ctor from Ubyte3: i1=z, i2=y, i3=x (pad 0).
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            if dst_type == ScReversePaddedIii {
                dst[3] = 0;
            }
            true
        }
        (ScReverseIii, Float1) | (ScReversePaddedIii, Float1) => {
            let index = c_cast_u8(read_f32(src));
            dst[0] = index;
            dst[1] = index;
            dst[2] = index;
            if dst_type == ScReversePaddedIii {
                dst[3] = 0;
            }
            true
        }
        (ScReversePaddedWw, Ubyte2) | (ScReversePaddedWw_, Ubyte2) => {
            // ctor from Ubyte2: w1=y, w2=x (pads 0).
            dst[0] = src[1];
            dst[1] = src[0];
            true
        }
        (Ubyte2, ScReversePaddedWw) | (Ubyte2, ScReversePaddedWw_) => {
            dst[0] = src[1];
            dst[1] = src[0];
            true
        }
        (Ubyte3, ScDiv3Iii) => {
            // operator Ubyte3(): z=i1, y=i2, x=i3.
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            true
        }
        _ => false,
    }
}

/// `VertexFormat::canConvert`: every destination element that has a source
/// counterpart must have a conversion, and the vertex counts must match.
pub fn can_convert(
    dst_format: &VertexFormat,
    dst_sizes: &[usize],
    src_format: &VertexFormat,
    src_sizes: &[usize],
) -> bool {
    for (dst_stream, element) in dst_format.elements() {
        let Some((src_stream, _, src_storage)) =
            src_format.find_element(element.semantic, element.semantic_index)
        else {
            // No counterpart: filled with defaults at convert time.
            continue;
        };
        // A conversion must exist (probed on scratch buffers, like the
        // reference fetching the function pointer).
        let mut probe_dst = vec![0u8; element.storage.size()];
        let probe_src = vec![0u8; src_storage.size()];
        if !convert_value(
            element.semantic,
            &mut probe_dst,
            element.storage,
            &probe_src,
            src_storage,
        ) {
            return false;
        }
        let dst_stride = dst_format.stream_stride(dst_stream) as usize;
        let src_stride = src_format.stream_stride(src_stream) as usize;
        if dst_stride == 0 || src_stride == 0 {
            return false;
        }
        let Some(&dst_size) = dst_sizes.get(dst_stream as usize) else {
            return false;
        };
        let Some(&src_size) = src_sizes.get(src_stream as usize) else {
            return false;
        };
        if dst_size / dst_stride != src_size / src_stride {
            return false;
        }
    }
    true
}

/// `VertexFormat::convertBuffer`: convert a single stream of vertices
/// from `src_format` (stream 0) to `dst_format` (stream 0).
pub fn convert_buffer(
    src_format: &VertexFormat,
    src: &[u8],
    dst_format: &VertexFormat,
    dst: &mut [u8],
) -> bool {
    convert_buffer_stream(src_format, src, 0, dst_format, dst, 0)
}

/// `VertexFormat::convertBufferStream`: convert the vertices of one stream
/// between formats. Destination elements without a source counterpart are
/// zero-filled (the reference's `fillElements` default); a destination
/// element with no conversion fails the whole call.
pub fn convert_buffer_stream(
    src_format: &VertexFormat,
    src: &[u8],
    src_stream_index: u32,
    dst_format: &VertexFormat,
    dst: &mut [u8],
    dst_stream_index: u32,
) -> bool {
    let src_stride = src_format.stream_stride(src_stream_index) as usize;
    let dst_stride = dst_format.stream_stride(dst_stream_index) as usize;
    if src_stride == 0 || dst_stride == 0 {
        return false;
    }
    let src_count = src.len() / src_stride;
    let dst_count = dst.len() / dst_stride;
    if src_count == 0 || src_count != dst_count {
        return false;
    }

    let Some(dst_stream) = dst_format.streams.get(dst_stream_index as usize) else {
        return false;
    };
    for element in &dst_stream.elements {
        let Some((src_stream, src_offset, src_storage)) =
            src_format.find_element(element.semantic, element.semantic_index)
        else {
            // New element: zero-fill it across every vertex.
            let size = element.storage.size();
            for vertex in 0..dst_count {
                let base = vertex * dst_stride + element.offset;
                dst[base..base + size].fill(0);
            }
            continue;
        };
        if src_stream != src_stream_index {
            // The source element lives on another stream; without that
            // stream's data the conversion fails (the reference's
            // canConvert sees an empty source buffer).
            return false;
        }

        for vertex in 0..src_count {
            let src_base = vertex * src_stride + src_offset;
            let dst_base = vertex * dst_stride + element.offset;
            let mut dst_element = vec![0u8; element.storage.size()];
            let ok = convert_value(
                element.semantic,
                &mut dst_element,
                element.storage,
                &src[src_base..src_base + src_storage.size()],
                src_storage,
            );
            if !ok {
                return false;
            }
            let size = element.storage.size();
            dst[dst_base..dst_base + size].copy_from_slice(&dst_element);
        }
    }
    true
}
