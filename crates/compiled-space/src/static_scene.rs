//! BW-10-02-02 static scene provider: the load and query faces of
//! `lib/compiled_space/static_scene_provider.cpp` for the default `BWSS`
//! format.
//!
//! `StaticSceneProvider::doLoadFromSpace` (`static_scene_provider.cpp:136-230`)
//! reads the octree, the per-data-reference content spans, the packed
//! content object indices, per-object bounds and bound spheres, then the
//! type headers; each type header hands its object range to a handler.
//! The port keeps the data faces: the parsed scene, the model type, and
//! the `intersect` query (`static_scene_provider.cpp:343-380`) that
//! returns the object set of a fixed region. The client SceneObject /
//! SuperModel machinery stays out (rendering face).
//!
//! Object indexing follows the reference: `objects_` is one array indexed
//! by the accumulated per-type starts, so a model's scene index is its
//! index within its type plus the leading types' `numObjects_` totals.

use std::collections::BTreeSet;

use foundation::math::{BoundBox, Sphere, Vector3};

use crate::binary_format::{BinaryFormat, Magic, Stream};
use crate::loose_octree::StaticLooseOctree;
use crate::static_scene_model::{Model, StaticSceneModelData};
use crate::string_table::StringTable;

/// `StaticSceneTypes::FORMAT_MAGIC[DEFAULT]` (`static_scene_types.hpp:12`).
pub const FORMAT_MAGIC: Magic = *b"BWSS";
/// `StaticSceneTypes::FORMAT_VERSION` (`static_scene_types.hpp:29`).
pub const FORMAT_VERSION: u32 = 1;

/// `StaticSceneTypes::TypeHeader`: 8 bytes.
pub const TYPE_HEADER_SIZE: usize = 8;
/// `DataSpan` is `IndexSpan<uint16>`: two `uint16` (static_array.hpp:783-798).
pub const DATA_SPAN_SIZE: usize = 4;

/// `StaticSceneTypes::StaticObjectType::STATIC_MODEL`.
pub const STATIC_OBJECT_STATIC_MODEL: u32 = 0;

/// `{ RuntimeTypeID typeID_; uint32 numObjects_; }`
///
/// `RuntimeTypeID` is a `uint8` (`lib/scene/scene_type_system.hpp:16`), so
/// the record is one byte of id, three bytes of alignment padding and the
/// `uint32`. Like the asset list entry, the writer never initialises the
/// padding and the reader never consults it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeHeader {
    pub type_id: u32,
    pub num_objects: u32,
}

impl TypeHeader {
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != TYPE_HEADER_SIZE {
            return None;
        }
        Some(Self {
            // typeID_ is the single leading byte.
            type_id: bytes[0] as u32,
            num_objects: u32::from_le_bytes(
                bytes[4..8]
                    .try_into()
                    .expect("the length check guards the slice"),
            ),
        })
    }
}

/// `DataSpan` (`IndexSpan<uint16>`): an **inclusive** `[first, last]` range
/// into the packed content array; an empty node has `first == last + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataSpan {
    pub first: u16,
    pub last: u16,
}

impl DataSpan {
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != DATA_SPAN_SIZE {
            return None;
        }
        Some(Self {
            first: u16::from_le_bytes(
                bytes[0..2]
                    .try_into()
                    .expect("the length check guards the slice"),
            ),
            last: u16::from_le_bytes(
                bytes[2..4]
                    .try_into()
                    .expect("the length check guards the slice"),
            ),
        })
    }
}

/// The loaded static scene: spatial data plus the model type data.
///
/// Load messages that the reference only logs (unknown type ids, handler
/// load failures) are kept in `warnings` in load order; the reference
/// carries on in both cases.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticScene {
    pub octree: StaticLooseOctree,
    pub node_contents_spans: Vec<DataSpan>,
    pub contents: Vec<u32>,
    pub object_bounds: Vec<BoundBox>,
    pub object_spheres: Vec<Sphere>,
    pub type_headers: Vec<TypeHeader>,
    pub models: Vec<Model>,
    pub resource_refs: Vec<u32>,
    pub warnings: Vec<String>,
}

impl StaticScene {
    /// `StaticSceneProvider::doLoadFromSpace` for the model type
    /// (`static_scene_provider.cpp:136-230`). Fatal failures (missing
    /// section, incomplete spatial data, incomplete type headers) carry
    /// the reference message lines; per-type problems land in `warnings`
    /// because the reference logs them and carries on. The reference also
    /// takes the string table here, but only hands it to handler loads,
    /// which resolve resources lazily in the port
    /// (`model_resource_strings`).
    pub fn load(format: &BinaryFormat) -> Result<Self, Vec<String>> {
        let mut stream =
            match format.find_and_open_section(FORMAT_MAGIC, FORMAT_VERSION, "StaticSceneProvider")
            {
                Ok(stream) => stream,
                Err(message) => {
                    return Err(vec![
                        message,
                        format!("Failed to map '{}' into memory.\n", format.resource_id()),
                    ]);
                }
            };

        // Load the scene spatial data.
        let octree = StaticLooseOctree::read(&mut stream);
        let node_contents_spans = read_spans(&mut stream);
        let contents = read_u32_array(&mut stream);
        let object_bounds = read_bound_boxes(&mut stream);
        let object_spheres = read_spheres(&mut stream);
        let type_headers = read_type_headers(&mut stream);

        let (octree, node_contents_spans, contents, object_bounds, object_spheres, type_headers) =
            match (
                octree,
                node_contents_spans,
                contents,
                object_bounds,
                object_spheres,
                type_headers,
            ) {
                (
                    Some(octree),
                    Some(node_contents_spans),
                    Some(contents),
                    Some(object_bounds),
                    Some(object_spheres),
                    Some(type_headers),
                ) => (
                    octree,
                    node_contents_spans,
                    contents,
                    object_bounds,
                    object_spheres,
                    type_headers,
                ),
                _ => {
                    return Err(vec![format!(
                        "StaticSceneProvider in '{}' has incomplete spatial scene data.\n",
                        format.resource_id()
                    )]);
                }
            };

        let mut warnings = Vec::new();
        let mut models = Vec::new();
        let mut resource_refs = Vec::new();

        // Load each type.
        for header in &type_headers {
            if header.type_id != STATIC_OBJECT_STATIC_MODEL {
                warnings.push(format!(
                    "StaticScene binary contains unknown object typeID {}.\n",
                    header.type_id
                ));
                continue;
            }

            let model_data = StaticSceneModelData::load(format);
            match model_data {
                Ok(StaticSceneModelData {
                    models: loaded_models,
                    resource_refs: loaded_refs,
                }) => {
                    models = loaded_models;
                    resource_refs = loaded_refs;
                }
                Err(messages) => {
                    let mut failure = Vec::new();
                    failure.extend(messages);
                    failure.push(format!(
                        "StaticScene: failed to load objects with typeID {}.\n",
                        header.type_id
                    ));
                    warnings.extend(failure);
                }
            }
        }

        // The provider resolves nothing from the string table at load
        // time; model resources resolve on demand via
        // `model_resource_strings`.

        Ok(Self {
            octree,
            node_contents_spans,
            contents,
            object_bounds,
            object_spheres,
            type_headers,
            models,
            resource_refs,
            warnings,
        })
    }

    /// `StaticSceneProvider::intersect` (`static_scene_provider.cpp:343-380`)
    /// for a box-shaped region: octree traversal, span expansion, then the
    /// bound-sphere filter, returning the scene object indices as a set.
    /// The reference filters with `hull.intersects(center, radius)` over a
    /// convex hull of the query; for a box region that is exactly the
    /// sphere-versus-box overlap test used here.
    pub fn query_region(&self, region: &BoundBox) -> BTreeSet<usize> {
        let mut hits = BTreeSet::new();
        for data_reference in self.octree.intersect_aabb(region) {
            let span = &self.node_contents_spans[data_reference as usize];
            // The reference walks span.first() ..= span.last(), which the
            // u16 overflow would loop over; spans with first > last are
            // the writer's empty ranges.
            let mut index = span.first as usize;
            while index <= span.last as usize {
                let scene_object_index = self.contents[index] as usize;
                let sphere = &self.object_spheres[scene_object_index];
                if sphere_overlaps_box(sphere, region) {
                    hits.insert(scene_object_index);
                }
                index += 1;
            }
        }
        hits
    }

    /// The scene object index a model index maps to (accumulated type
    /// starts, `static_scene_provider.cpp:204-219`).
    pub fn model_scene_index(&self, model_index: usize) -> Option<usize> {
        let mut offset = 0usize;
        for header in &self.type_headers {
            if header.type_id == STATIC_OBJECT_STATIC_MODEL {
                return (model_index < header.num_objects as usize).then_some(offset + model_index);
            }
            offset += header.num_objects as usize;
        }
        None
    }

    /// `loadSuperModel`'s resource walk
    /// (`static_scene_model.cpp:261-273`): the model's index range into
    /// the string-index pool, resolved against the string table. Returns
    /// `None` when the model index is out of range.
    pub fn model_resource_strings<'s>(
        &self,
        model_index: usize,
        strings: &'s StringTable,
    ) -> Option<Vec<&'s str>> {
        let model = self.models.get(model_index)?;
        let mut names = Vec::new();
        for pool_index in model.resource_indices() {
            let string_index = self.resource_refs.get(pool_index as usize)?;
            names.push(strings.entry_str(*string_index as usize)?);
        }
        Some(names)
    }
}

/// Sphere-versus-box overlap: the distance from the sphere center to the
/// box is at most the radius (the box-region case of
/// `ConvexHull::intersects(center, radius)`).
fn sphere_overlaps_box(sphere: &Sphere, region: &BoundBox) -> bool {
    let dx = (region.min.x - sphere.center.x)
        .max(0.0)
        .max(sphere.center.x - region.max.x);
    let dy = (region.min.y - sphere.center.y)
        .max(0.0)
        .max(sphere.center.y - region.max.y);
    let dz = (region.min.z - sphere.center.z)
        .max(0.0)
        .max(sphere.center.z - region.max.z);
    dx * dx + dy * dy + dz * dz <= sphere.radius * sphere.radius
}

fn read_spans(stream: &mut Stream) -> Option<Vec<DataSpan>> {
    stream
        .read_external_array(DATA_SPAN_SIZE)?
        .as_chunks::<DATA_SPAN_SIZE>()
        .0
        .iter()
        .map(|bytes| DataSpan::from_le_bytes(bytes))
        .collect()
}

fn read_u32_array(stream: &mut Stream) -> Option<Vec<u32>> {
    stream
        .read_external_array(4)?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| Some(u32::from_le_bytes(*bytes)))
        .collect()
}

fn read_bound_boxes(stream: &mut Stream) -> Option<Vec<BoundBox>> {
    Some(
        stream
            .read_external_array(24)?
            .as_chunks::<24>()
            .0
            .iter()
            .map(|bytes| {
                BoundBox::new(
                    Vector3::new(
                        f32::from_le_bytes(bytes[0..4].try_into().expect("guarded chunk")),
                        f32::from_le_bytes(bytes[4..8].try_into().expect("guarded chunk")),
                        f32::from_le_bytes(bytes[8..12].try_into().expect("guarded chunk")),
                    ),
                    Vector3::new(
                        f32::from_le_bytes(bytes[12..16].try_into().expect("guarded chunk")),
                        f32::from_le_bytes(bytes[16..20].try_into().expect("guarded chunk")),
                        f32::from_le_bytes(bytes[20..24].try_into().expect("guarded chunk")),
                    ),
                )
            })
            .collect(),
    )
}

fn read_spheres(stream: &mut Stream) -> Option<Vec<Sphere>> {
    Some(
        stream
            .read_external_array(16)?
            .as_chunks::<16>()
            .0
            .iter()
            .map(|bytes| {
                Sphere::new(
                    Vector3::new(
                        f32::from_le_bytes(bytes[0..4].try_into().expect("guarded chunk")),
                        f32::from_le_bytes(bytes[4..8].try_into().expect("guarded chunk")),
                        f32::from_le_bytes(bytes[8..12].try_into().expect("guarded chunk")),
                    ),
                    f32::from_le_bytes(bytes[12..16].try_into().expect("guarded chunk")),
                )
            })
            .collect(),
    )
}

fn read_type_headers(stream: &mut Stream) -> Option<Vec<TypeHeader>> {
    stream
        .read_external_array(TYPE_HEADER_SIZE)?
        .as_chunks::<TYPE_HEADER_SIZE>()
        .0
        .iter()
        .map(|bytes| TypeHeader::from_le_bytes(bytes))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binary_format::{FORMAT_MAGIC, FORMAT_VERSION};
    use crate::loose_octree::CONFIGURATION_SIZE;

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

    /// An empty octree: typed configuration (the reference constructor
    /// values) followed by the five external arrays, all empty.
    fn empty_octree_blob() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(CONFIGURATION_SIZE as u32).to_le_bytes());
        let mut config = [0u8; CONFIGURATION_SIZE];
        config[16..20].copy_from_slice(&1.0f32.to_le_bytes());
        bytes.extend_from_slice(&config);
        for elem_size in [24u32, 12, 16, 4, 2] {
            bytes.extend_from_slice(&elem_size.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        bytes
    }

    fn external_array(elem_size: u32, count: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&elem_size.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes
    }

    /// `empty_octree_blob()` plus the four empty content arrays and one
    /// type header.
    fn bwss_payload(type_id: u8, num_objects: u32) -> Vec<u8> {
        let mut bytes = empty_octree_blob();
        bytes.extend(external_array(DATA_SPAN_SIZE as u32, 0));
        bytes.extend(external_array(4, 0));
        bytes.extend(external_array(24, 0));
        bytes.extend(external_array(16, 0));
        bytes.extend(external_array(TYPE_HEADER_SIZE as u32, 1));
        bytes.push(type_id);
        bytes.extend_from_slice(&[0x70, 0xbb, 0x96]);
        bytes.extend_from_slice(&num_objects.to_le_bytes());
        bytes
    }

    #[test]
    fn type_header_parses_its_8_bytes() {
        let header = TypeHeader::from_le_bytes(&[0, 0, 0, 0, 5, 0, 0, 0]).unwrap();
        assert_eq!(
            header,
            TypeHeader {
                type_id: STATIC_OBJECT_STATIC_MODEL,
                num_objects: 5
            }
        );
    }

    #[test]
    fn type_header_ignores_the_uninitialised_padding() {
        // The fixture record as the reference writer really lays it out:
        // the id byte, three bytes of never-initialised padding and the
        // object count.
        let header = TypeHeader::from_le_bytes(&[0, 0x70, 0xbb, 0x96, 5, 0, 0, 0]).unwrap();
        assert_eq!(
            header,
            TypeHeader {
                type_id: STATIC_OBJECT_STATIC_MODEL,
                num_objects: 5
            }
        );
    }

    #[test]
    fn type_header_rejects_other_lengths() {
        assert!(TypeHeader::from_le_bytes(&[0u8; 7]).is_none());
        assert!(TypeHeader::from_le_bytes(&[0u8; 9]).is_none());
    }

    #[test]
    fn data_span_parses_its_4_bytes() {
        let span = DataSpan::from_le_bytes(&[3, 0, 4, 0]).unwrap();
        assert_eq!(span, DataSpan { first: 3, last: 4 });
    }

    #[test]
    fn data_span_rejects_other_lengths() {
        assert!(DataSpan::from_le_bytes(&[0u8; 3]).is_none());
        assert!(DataSpan::from_le_bytes(&[0u8; 5]).is_none());
    }

    #[test]
    fn sphere_overlap_matches_distance_to_box() {
        let sphere = Sphere::new(Vector3::new(10.0, 5.0, 10.0), 2.0);
        // box contains the center
        assert!(sphere_overlaps_box(
            &sphere,
            &BoundBox::new(Vector3::new(9.0, 4.0, 9.0), Vector3::new(11.0, 6.0, 11.0))
        ));
        // box exactly touches the radius on x
        assert!(sphere_overlaps_box(
            &sphere,
            &BoundBox::new(Vector3::new(12.0, 5.0, 10.0), Vector3::new(13.0, 6.0, 11.0))
        ));
        // box one unit too far
        assert!(!sphere_overlaps_box(
            &sphere,
            &BoundBox::new(Vector3::new(13.0, 5.0, 10.0), Vector3::new(14.0, 6.0, 11.0))
        ));
    }

    #[test]
    fn query_region_expands_spans_and_filters_spheres() {
        let scene = StaticScene {
            octree: StaticLooseOctree {
                config: crate::loose_octree::Configuration {
                    max_depth: 1,
                    world_center: Vector3::new(4.0, 4.0, 4.0),
                    world_size: 8.0,
                    num_data_entries: 2,
                },
                bounds: vec![
                    BoundBox::new(Vector3::ZERO, Vector3::new(8.0, 8.0, 8.0)),
                    BoundBox::new(Vector3::ZERO, Vector3::new(4.0, 4.0, 4.0)),
                    BoundBox::new(Vector3::new(4.0, 4.0, 4.0), Vector3::new(8.0, 8.0, 8.0)),
                ],
                centers: vec![Vector3::new(4.0, 4.0, 4.0); 3],
                nodes: vec![
                    crate::loose_octree::Node {
                        child: [
                            crate::loose_octree::LEAF_REF_FLAG | 1,
                            crate::loose_octree::LEAF_REF_FLAG | 2,
                            crate::loose_octree::INVALID_NODE,
                            crate::loose_octree::INVALID_NODE,
                            crate::loose_octree::INVALID_NODE,
                            crate::loose_octree::INVALID_NODE,
                            crate::loose_octree::INVALID_NODE,
                            crate::loose_octree::INVALID_NODE,
                        ],
                    },
                    crate::loose_octree::Node {
                        child: [crate::loose_octree::INVALID_NODE; 8],
                    },
                    crate::loose_octree::Node {
                        child: [crate::loose_octree::INVALID_NODE; 8],
                    },
                ],
                data_references: vec![crate::loose_octree::INVALID_DATA_REFERENCE, 0, 1],
                parents: vec![crate::loose_octree::INVALID_NODE, 0, 0],
            },
            // data reference 0 holds objects 0 and 1; reference 1 holds
            // object 2, and reference 2 is an empty span (first > last).
            node_contents_spans: vec![
                DataSpan { first: 0, last: 1 },
                DataSpan { first: 2, last: 2 },
                DataSpan { first: 3, last: 2 },
            ],
            contents: vec![0, 1, 2],
            object_bounds: vec![
                BoundBox::new(Vector3::ZERO, Vector3::new(1.0, 1.0, 1.0)),
                BoundBox::new(Vector3::new(1.0, 1.0, 1.0), Vector3::new(2.0, 2.0, 2.0)),
                BoundBox::new(Vector3::new(4.0, 4.0, 4.0), Vector3::new(6.0, 6.0, 6.0)),
            ],
            object_spheres: vec![
                Sphere::new(Vector3::ZERO, 1.0),
                Sphere::new(Vector3::new(1.5, 1.5, 1.5), 0.9),
                Sphere::new(Vector3::new(5.0, 5.0, 5.0), 2.0),
            ],
            type_headers: vec![TypeHeader {
                type_id: STATIC_OBJECT_STATIC_MODEL,
                num_objects: 3,
            }],
            models: Vec::new(),
            resource_refs: Vec::new(),
            warnings: Vec::new(),
        };

        // Whole scene: both leaves, spans 0..1 and 2..2, all spheres in.
        let hits = scene.query_region(&BoundBox::new(Vector3::ZERO, Vector3::new(8.0, 8.0, 8.0)));
        assert_eq!(hits, [0, 1, 2].into_iter().collect());

        // Only the far leaf: sphere 2 filters in, span 0 is never reached.
        let hits = scene.query_region(&BoundBox::new(
            Vector3::new(4.5, 4.5, 4.5),
            Vector3::new(5.5, 5.5, 5.5),
        ));
        assert_eq!(hits, [2].into_iter().collect());

        // Near leaf but away from both object spheres: traversal hits,
        // the sphere filter rejects.
        let hits = scene.query_region(&BoundBox::new(
            Vector3::new(3.0, 3.0, 3.0),
            Vector3::new(3.9, 3.9, 3.9),
        ));
        assert!(hits.is_empty());
    }

    #[test]
    fn model_scene_index_accumulates_type_starts() {
        let scene = StaticScene {
            type_headers: vec![
                TypeHeader {
                    type_id: 9,
                    num_objects: 4,
                },
                TypeHeader {
                    type_id: STATIC_OBJECT_STATIC_MODEL,
                    num_objects: 2,
                },
            ],
            ..StaticScene::default()
        };
        assert_eq!(scene.model_scene_index(0), Some(4));
        assert_eq!(scene.model_scene_index(1), Some(5));
        assert_eq!(scene.model_scene_index(2), None);
    }

    #[test]
    fn model_scene_index_without_a_model_type_is_none() {
        let scene = StaticScene {
            type_headers: vec![TypeHeader {
                type_id: 9,
                num_objects: 4,
            }],
            ..StaticScene::default()
        };
        assert_eq!(scene.model_scene_index(0), None);
    }
    #[test]
    fn load_reports_a_missing_static_scene_section() {
        let format =
            BinaryFormat::open("scene.bin", container(&[(*b"BWST", 1, Vec::new())])).unwrap();
        assert_eq!(
            StaticScene::load(&format),
            Err(vec![
                "'scene.bin' does not contain StaticSceneProvider.\n".to_string(),
                "Failed to map 'scene.bin' into memory.\n".to_string(),
            ])
        );
    }

    #[test]
    fn load_reports_incomplete_spatial_scene_data() {
        let format =
            BinaryFormat::open("scene.bin", container(&[(*b"BWSS", 1, Vec::new())])).unwrap();
        assert_eq!(
            StaticScene::load(&format),
            Err(vec![
                "StaticSceneProvider in 'scene.bin' has incomplete spatial scene data.\n"
                    .to_string()
            ])
        );
    }

    #[test]
    fn load_warns_on_an_unknown_object_type() {
        let format =
            BinaryFormat::open("scene.bin", container(&[(*b"BWSS", 1, bwss_payload(9, 1))]))
                .unwrap();
        let scene = StaticScene::load(&format).unwrap();
        assert_eq!(
            scene.warnings,
            vec!["StaticScene binary contains unknown object typeID 9.\n".to_string()]
        );
        assert!(scene.models.is_empty());
        assert_eq!(scene.type_headers.len(), 1);
    }

    #[test]
    fn load_warns_when_the_model_handler_fails() {
        // typeID 0 routes to the model handler, but the container has no BWSM
        // section; the reference logs both messages and carries on.
        let format = BinaryFormat::open(
            "scene.bin",
            container(&[(
                *b"BWSS",
                1,
                bwss_payload(STATIC_OBJECT_STATIC_MODEL as u8, 1),
            )]),
        )
        .unwrap();
        let scene = StaticScene::load(&format).unwrap();
        assert_eq!(
            scene.warnings,
            vec![
                "'scene.bin' does not contain static model.\n".to_string(),
                "StaticScene: failed to load objects with typeID 0.\n".to_string(),
            ]
        );
        assert!(scene.models.is_empty());
    }

    #[test]
    fn model_resource_strings_fails_cleanly() {
        // An empty but valid BWST: no entries, no string data.
        let mut bwst = Vec::new();
        bwst.extend_from_slice(&8u32.to_le_bytes());
        bwst.extend_from_slice(&0u32.to_le_bytes());
        bwst.extend_from_slice(&0u32.to_le_bytes());
        let format = BinaryFormat::open("scene.bin", container(&[(*b"BWST", 1, bwst)])).unwrap();
        let strings = StringTable::read(&format).unwrap();

        let scene = StaticScene {
            models: vec![Model {
                world_transform: [[1.0; 4]; 4],
                resource_ids: crate::static_scene_model::IndexRange { first: 0, last: 1 },
                animation_name: 0,
                animation_multiplier: 0.0,
                flags: 0,
                tint_dyes: crate::static_scene_model::IndexRange::EMPTY,
            }],
            resource_refs: vec![0],
            ..StaticScene::default()
        };

        // out-of-range model index
        assert_eq!(scene.model_resource_strings(1, &strings), None);
        // pool index beyond the shared pool
        assert_eq!(scene.model_resource_strings(0, &strings), None);

        let scene = StaticScene {
            models: vec![Model {
                world_transform: [[1.0; 4]; 4],
                resource_ids: crate::static_scene_model::IndexRange { first: 9, last: 9 },
                animation_name: 0,
                animation_multiplier: 0.0,
                flags: 0,
                tint_dyes: crate::static_scene_model::IndexRange::EMPTY,
            }],
            resource_refs: vec![0],
            ..StaticScene::default()
        };
        // string index beyond the table
        assert_eq!(scene.model_resource_strings(0, &strings), None);
    }
}
