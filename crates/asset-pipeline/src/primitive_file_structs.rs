//! The on-disk primitive structures (`moo/primitive_file_structs.hpp`) plus
//! the `HeaderMagic` constants: the headers the primitive processor reads
//! and writes around vertex/index payloads.
//!
//! Reference quirks preserved:
//! - the format name fields are fixed 64-byte NUL-padded char arrays;
//! - the processed magics are the little-endian FOURCCs `BPVT`
//!   (processed vertices) and `BPVS` (processed vertex stream);
//! - the layouts are packed exactly as declared (x86, no padding).

/// `Moo::VertexHeader`: the header in front of a raw vertex blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VertexHeader {
    /// `vertexFormat_[64]`.
    pub vertex_format: String,
    /// `nVertices_`.
    pub n_vertices: i32,
}

pub const VERTEX_FORMAT_FIELD_LEN: usize = 64;
pub const INDEX_FORMAT_FIELD_LEN: usize = 64;

impl VertexHeader {
    pub const SIZE: usize = VERTEX_FORMAT_FIELD_LEN + 4;

    /// Read the header off the front of `data`.
    pub fn read(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }
        let field = &data[..VERTEX_FORMAT_FIELD_LEN];
        let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
        Some(Self {
            vertex_format: String::from_utf8_lossy(&field[..end]).into_owned(),
            n_vertices: i32::from_le_bytes(
                data[VERTEX_FORMAT_FIELD_LEN..Self::SIZE]
                    .try_into()
                    .unwrap(),
            ),
        })
    }

    /// Write the header (NUL-padded to the field width, like `strcpy`
    /// into the fixed array plus the zero initialisation).
    pub fn write(&self, out: &mut Vec<u8>) {
        let mut field = [0u8; VERTEX_FORMAT_FIELD_LEN];
        let bytes = self.vertex_format.as_bytes();
        field[..bytes.len()].copy_from_slice(bytes);
        out.extend_from_slice(&field);
        out.extend_from_slice(&self.n_vertices.to_le_bytes());
    }
}

/// `Moo::HeaderMagic::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderMagic {
    /// `ProcessedVertices = 'B','P','V','T'`.
    ProcessedVertices,
    /// `ProcessedVertexStream = 'B','P','V','S'`.
    ProcessedVertexStream,
}

impl HeaderMagic {
    /// The `DECLARE_FOURCC` value.
    pub fn value(self) -> u32 {
        match self {
            Self::ProcessedVertices => fourcc(b'B', b'P', b'V', b'T'),
            Self::ProcessedVertexStream => fourcc(b'B', b'P', b'V', b'S'),
        }
    }
}

/// `DECLARE_FOURCC(ch0..3)`: little-endian byte assembly.
pub fn fourcc(ch0: u8, ch1: u8, ch2: u8, ch3: u8) -> u32 {
    (ch0 as u32) | ((ch1 as u32) << 8) | ((ch2 as u32) << 16) | ((ch3 as u32) << 24)
}

/// `Moo::ProcessedVertexHeader`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedVertexHeader {
    /// `magic_`.
    pub magic: HeaderMagic,
    /// `originalVertexFormat_[64]`.
    pub original_vertex_format: String,
    /// `vertexHeader_`.
    pub vertex_header: VertexHeader,
}

impl ProcessedVertexHeader {
    pub const SIZE: usize = 4 + VertexHeader::SIZE;

    /// Serialise the header (the `QuickFileWriter f << procVertHeader`
    /// flow: zero-initialised struct, fields `strcpy`-ed in).
    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.magic.value().to_le_bytes());
        let mut field = [0u8; VERTEX_FORMAT_FIELD_LEN];
        let bytes = self.original_vertex_format.as_bytes();
        field[..bytes.len()].copy_from_slice(bytes);
        out.extend_from_slice(&field);
        self.vertex_header.write(out);
    }
}

/// `Moo::IndexHeader`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexHeader {
    /// `indexFormat_[64]` ("list" for 16-bit, "list32" for 32-bit).
    pub index_format: String,
    /// `nIndices_`.
    pub n_indices: i32,
    /// `nTriangleGroups_`.
    pub n_triangle_groups: i32,
}

impl IndexHeader {
    pub const SIZE: usize = INDEX_FORMAT_FIELD_LEN + 4 + 4;

    /// Read the header off the front of `data`.
    pub fn read(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SIZE {
            return None;
        }
        let field = &data[..INDEX_FORMAT_FIELD_LEN];
        let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
        Some(Self {
            index_format: String::from_utf8_lossy(&field[..end]).into_owned(),
            n_indices: i32::from_le_bytes(
                data[INDEX_FORMAT_FIELD_LEN..INDEX_FORMAT_FIELD_LEN + 4]
                    .try_into()
                    .unwrap(),
            ),
            n_triangle_groups: i32::from_le_bytes(
                data[INDEX_FORMAT_FIELD_LEN + 4..Self::SIZE]
                    .try_into()
                    .unwrap(),
            ),
        })
    }
}

/// `Moo::PrimitiveGroup`: a range within the index buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PrimitiveGroup {
    /// `startIndex_`.
    pub start_index: i32,
    /// `nPrimitives_`.
    pub n_primitives: i32,
    /// `startVertex_`.
    pub start_vertex: i32,
    /// `nVertices_`.
    pub n_vertices: i32,
}

pub const PRIMITIVE_GROUP_SIZE: usize = 16;

/// Read `count` groups laid out back-to-back at `data`.
pub fn read_primitive_groups(data: &[u8], count: usize) -> Vec<PrimitiveGroup> {
    let mut groups = Vec::new();
    for index in 0..count {
        let base = index * PRIMITIVE_GROUP_SIZE;
        if base + PRIMITIVE_GROUP_SIZE > data.len() {
            break;
        }
        let words: [i32; 4] = std::array::from_fn(|word| {
            i32::from_le_bytes(
                data[base + word * 4..base + word * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        });
        groups.push(PrimitiveGroup {
            start_index: words[0],
            n_primitives: words[1],
            start_vertex: words[2],
            n_vertices: words[3],
        });
    }
    groups
}

/// Write groups back-to-back.
pub fn write_primitive_groups(groups: &[PrimitiveGroup], out: &mut Vec<u8>) {
    for group in groups {
        out.extend_from_slice(&group.start_index.to_le_bytes());
        out.extend_from_slice(&group.n_primitives.to_le_bytes());
        out.extend_from_slice(&group.start_vertex.to_le_bytes());
        out.extend_from_slice(&group.n_vertices.to_le_bytes());
    }
}
