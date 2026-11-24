//! BW-10-02-02 static scene models: the data face of the BWSM section
//! (`lib/compiled_space/static_scene_model_types.hpp` and the load half of
//! `static_scene_model.cpp`).
//!
//! The reference `StaticSceneModelHandler::load` reads the model array and
//! the shared string-index pool, then builds client-side SuperModel
//! instances — that rendering face stays out of the port. What the static
//! scene provider keeps is the parsed records plus the resource string
//! indices, so a loaded model's resources resolve through the string
//! table exactly like `loadSuperModel` does
//! (`static_scene_model.cpp:261-273`).

use crate::binary_format::{BinaryFormat, Magic, Stream};

/// `StaticSceneModelTypes::FORMAT_MAGIC` (`static_scene_model_types.hpp:16`).
pub const FORMAT_MAGIC: Magic = *b"BWSM";
/// `StaticSceneModelTypes::FORMAT_VERSION` (`static_scene_model_types.hpp:17`).
pub const FORMAT_VERSION: u32 = 1;

/// `StaticSceneModelTypes::Model` is 84 bytes on the wire (verified against
/// the reference build with a sizeof probe).
pub const MODEL_SIZE: usize = 84;
/// `StaticSceneModelTypes::IndexRange` is two `uint16`.
pub const INDEX_RANGE_SIZE: usize = 4;

pub const FLAG_CASTS_SHADOW: u32 = 1 << 0;
pub const FLAG_REFLECTION_VISIBLE: u32 = 1 << 1;
pub const FLAG_IS_SHELL: u32 = 1 << 2;

/// `StringTableTypes::INVALID_INDEX` re-used for the u16 range ends.
pub const INVALID_RANGE_END: u16 = u16::MAX;

/// `{ uint16 first_, last_ }` into the shared string-index pool
/// (`static_scene_model_types.hpp`; the writer initialises both to
/// `uint16(-1)` when a model has no such entries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexRange {
    pub first: u16,
    pub last: u16,
}

impl IndexRange {
    /// The unset range is `first_ == last_ == uint16(-1)` (the writer's
    /// `IndicesRangeBuilder` initial value).
    pub const EMPTY: Self = Self {
        first: INVALID_RANGE_END,
        last: INVALID_RANGE_END,
    };
}

/// One `StaticSceneModelTypes::Model` record: a row-major 4x4 transform,
/// the resource/dye index ranges into the section's string-index pool, the
/// animation name string index, multiplier, and the flag bits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Model {
    /// `Matrix::m` rows: 4 rows of 4 little-endian f32s.
    pub world_transform: [[f32; 4]; 4],
    pub resource_ids: IndexRange,
    pub animation_name: u32,
    pub animation_multiplier: f32,
    pub flags: u32,
    pub tint_dyes: IndexRange,
}

impl Model {
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != MODEL_SIZE {
            return None;
        }
        let mut world_transform = [[0f32; 4]; 4];
        for (row, chunk) in world_transform
            .iter_mut()
            .zip(bytes[..64].as_chunks::<16>().0)
        {
            for (slot, f) in row.iter_mut().zip(chunk.as_chunks::<4>().0) {
                *slot = f32::from_le_bytes(*f);
            }
        }
        // everything below is guarded by the exact-length check above
        let resource_ids = IndexRange {
            first: u16::from_le_bytes(bytes[64..66].try_into().expect("guarded")),
            last: u16::from_le_bytes(bytes[66..68].try_into().expect("guarded")),
        };
        let animation_name = u32::from_le_bytes(bytes[68..72].try_into().expect("guarded"));
        let animation_multiplier = f32::from_le_bytes(bytes[72..76].try_into().expect("guarded"));
        let flags = u32::from_le_bytes(bytes[76..80].try_into().expect("guarded"));
        let tint_dyes = IndexRange {
            first: u16::from_le_bytes(bytes[80..82].try_into().expect("guarded")),
            last: u16::from_le_bytes(bytes[82..84].try_into().expect("guarded")),
        };
        Some(Self {
            world_transform,
            resource_ids,
            animation_name,
            animation_multiplier,
            flags,
            tint_dyes,
        })
    }

    pub fn casts_shadow(&self) -> bool {
        self.flags & FLAG_CASTS_SHADOW != 0
    }

    pub fn reflection_visible(&self) -> bool {
        self.flags & FLAG_REFLECTION_VISIBLE != 0
    }

    pub fn is_shell(&self) -> bool {
        self.flags & FLAG_IS_SHELL != 0
    }

    /// `loadSuperModel` walks `resourceIDs_.first_ ..= resourceIDs_.last_`
    /// and skips the unset sentinel (`static_scene_model.cpp:261-270`).
    pub fn resource_indices(&self) -> impl Iterator<Item = u16> {
        let first = self.resource_ids.first;
        let last = self.resource_ids.last;
        (first..=last).filter(|i| *i != INVALID_RANGE_END)
    }
}

/// The parsed BWSM payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticSceneModelData {
    pub models: Vec<Model>,
    /// The section-wide string-index pool both index ranges refer to.
    pub resource_refs: Vec<u32>,
}

impl StaticSceneModelData {
    /// `StaticSceneModelHandler::load`
    /// (`static_scene_model.cpp:79-124`): locate the BWSM section, check
    /// the version, open it, then read the model array and the
    /// string-index pool. Errors carry the exact ASSET_MSG lines.
    pub fn load(format: &BinaryFormat) -> Result<Self, Vec<String>> {
        let section_idx = match format.find_section(FORMAT_MAGIC) {
            Some(idx) => idx,
            None => {
                return Err(vec![format!(
                    "'{}' does not contain static model.\n",
                    format.resource_id()
                )]);
            }
        };

        if format.section_version(section_idx) != Some(FORMAT_VERSION) {
            return Err(vec![format!(
                "'{}' has static model data with incompatible version.\n",
                format.resource_id()
            )]);
        }

        let mut stream = match format.open_section(section_idx) {
            Some(stream) => stream,
            None => {
                return Err(vec![
                    "Failed to map StaticModel section into memory.\n".to_string(),
                ]);
            }
        };

        let models = Self::read_models(&mut stream);
        let resource_refs = Self::read_resource_refs(&mut stream);
        match (models, resource_refs) {
            (Some(models), Some(resource_refs)) => Ok(Self {
                models,
                resource_refs,
            }),
            _ => Err(vec![
                "StaticModelHandler: failed to read data.\n".to_string(),
            ]),
        }
    }

    fn read_models(stream: &mut Stream) -> Option<Vec<Model>> {
        stream
            .read_external_array(MODEL_SIZE)?
            .as_chunks::<MODEL_SIZE>()
            .0
            .iter()
            .map(|bytes| Model::from_le_bytes(bytes))
            .collect()
    }

    fn read_resource_refs(stream: &mut Stream) -> Option<Vec<u32>> {
        stream
            .read_external_array(4)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| Some(u32::from_le_bytes(*bytes)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary_format::{FORMAT_MAGIC, FORMAT_VERSION};

    /// A minimal container with the given sections, laid out exactly like
    /// the reference writer (`binary_format_writer.cpp`).
    fn container(sections: &[(Magic, u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&FORMAT_MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        let table_end = 24 + sections.len() * 24;
        out.extend_from_slice(&(table_end as u32).to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(sections.len() as u32).to_le_bytes());
        let mut offset = table_end as u64;
        for (magic, version, payload) in sections {
            out.extend_from_slice(magic);
            out.extend_from_slice(&version.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            offset += payload.len() as u64;
        }
        for (_, _, payload) in sections {
            out.extend_from_slice(payload);
        }
        out
    }

    fn bwst_only() -> BinaryFormat {
        BinaryFormat::open("scene.bin", container(&[(*b"BWST", 1, Vec::new())])).unwrap()
    }
    use crate::string_table::INVALID_INDEX;

    #[allow(clippy::too_many_arguments)]
    fn model_bytes(
        transform: [[f32; 4]; 4],
        resource_first: u16,
        resource_last: u16,
        animation_name: u32,
        multiplier: f32,
        flags: u32,
        dye_first: u16,
        dye_last: u16,
    ) -> [u8; MODEL_SIZE] {
        let mut bytes = [0u8; MODEL_SIZE];
        for (row, chunk) in transform.iter().zip(bytes.as_chunks_mut::<16>().0) {
            for (f, out) in row.iter().zip(chunk.as_chunks_mut::<4>().0) {
                *out = f.to_le_bytes();
            }
        }
        bytes[64..66].copy_from_slice(&resource_first.to_le_bytes());
        bytes[66..68].copy_from_slice(&resource_last.to_le_bytes());
        bytes[68..72].copy_from_slice(&animation_name.to_le_bytes());
        bytes[72..76].copy_from_slice(&multiplier.to_le_bytes());
        bytes[76..80].copy_from_slice(&flags.to_le_bytes());
        bytes[80..82].copy_from_slice(&dye_first.to_le_bytes());
        bytes[82..84].copy_from_slice(&dye_last.to_le_bytes());
        bytes
    }

    #[test]
    fn model_parses_its_84_bytes() {
        let transform = [
            [10.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 10.0, 0.0],
            [10.0, 5.0, 10.0, 1.0],
        ];
        let model = Model::from_le_bytes(&model_bytes(
            transform,
            0,
            1,
            5,
            2.5,
            FLAG_CASTS_SHADOW | FLAG_IS_SHELL,
            2,
            5,
        ))
        .unwrap();
        assert_eq!(model.world_transform, transform);
        assert_eq!(model.resource_ids, IndexRange { first: 0, last: 1 });
        assert_eq!(model.animation_name, 5);
        assert_eq!(model.animation_multiplier, 2.5);
        assert!(model.casts_shadow());
        assert!(!model.reflection_visible());
        assert!(model.is_shell());
        assert_eq!(model.tint_dyes, IndexRange { first: 2, last: 5 });
        assert_eq!(model.resource_indices().collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn model_rejects_other_lengths() {
        assert!(Model::from_le_bytes(&[0u8; 83]).is_none());
        assert!(Model::from_le_bytes(&[0u8; 85]).is_none());
    }

    #[test]
    fn unset_ranges_are_the_u16_sentinel() {
        assert_eq!(IndexRange::EMPTY.first, INVALID_RANGE_END);
        assert_eq!(IndexRange::EMPTY.last, INVALID_RANGE_END);
        let model = Model::from_le_bytes(&model_bytes(
            [[1.0, 0.0, 0.0, 0.0]; 4],
            INVALID_RANGE_END,
            INVALID_RANGE_END,
            INVALID_INDEX,
            0.0,
            0,
            INVALID_RANGE_END,
            INVALID_RANGE_END,
        ))
        .unwrap();
        assert_eq!(model.resource_ids, IndexRange::EMPTY);
        assert_eq!(model.animation_name, INVALID_INDEX);
        assert!(model.resource_indices().next().is_none());
    }
    #[test]
    fn load_reports_a_missing_static_model_section() {
        let format = bwst_only();
        assert_eq!(
            StaticSceneModelData::load(&format),
            Err(vec![
                "'scene.bin' does not contain static model.\n".to_string()
            ])
        );
    }

    #[test]
    fn load_reports_an_incompatible_static_model_version() {
        let format =
            BinaryFormat::open("scene.bin", container(&[(*b"BWSM", 2, Vec::new())])).unwrap();
        assert_eq!(
            StaticSceneModelData::load(&format),
            Err(vec![
                "'scene.bin' has static model data with incompatible version.\n".to_string()
            ])
        );
    }

    #[test]
    fn load_reports_an_unmappable_static_model_section() {
        // A section whose declared length runs past the file cannot be
        // mapped; the reference dereferences a NULL buffer there, the port
        // reports the message that branch exists for.
        let mut bytes = container(&[(*b"BWSM", 1, vec![0u8; 4])]);
        // patch the section length (last 8 bytes of the section table entry)
        let len_at = 24 + 16;
        let start = len_at - 8;
        bytes[start..len_at].copy_from_slice(&9999u64.to_le_bytes());
        let format = BinaryFormat::open("scene.bin", bytes).unwrap();
        assert_eq!(
            StaticSceneModelData::load(&format),
            Err(vec![
                "Failed to map StaticModel section into memory.\n".to_string()
            ])
        );
    }

    #[test]
    fn load_reports_a_failed_static_model_data_read() {
        let format =
            BinaryFormat::open("scene.bin", container(&[(*b"BWSM", 1, Vec::new())])).unwrap();
        assert_eq!(
            StaticSceneModelData::load(&format),
            Err(vec![
                "StaticModelHandler: failed to read data.\n".to_string()
            ])
        );
    }
}
