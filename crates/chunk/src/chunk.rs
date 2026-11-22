//! BW-10-01-01 a chunk: its identifier, grid coordinates, transform,
//! bounding box and the object set it owns.
//!
//! The loader path is a port of `lib/chunk/chunk.cpp` `Chunk::Chunk`,
//! `Chunk::load` and `Chunk::loadInclude`. The chunk file is an XML
//! DataSection whose root is the file name (`xxxxxxxxo.chunk` /
//! `xxxxxxxxi.chunk`); outside chunks derive their transform and bounds from
//! the grid coordinates, inside chunks read a `<transform>` and `<boundingBox>`
//! from the section.

use foundation::math::{BoundBox, Matrix, Vector3};
use resources::datasection::DataSection;

use crate::boundary::{ChunkBoundary, form_boundaries};
use crate::identifier::{grid_from_chunk_name, is_outside_chunk};
use crate::mapping::GeometryMapping;

/// Reference `MIN_CHUNK_HEIGHT` / `MAX_CHUNK_HEIGHT` (`base_chunk_space.hpp`).
pub const MIN_CHUNK_HEIGHT: f32 = -10000.0;
pub const MAX_CHUNK_HEIGHT: f32 = 10000.0;

/// A scene object owned by a chunk: one `<model>`, `<entity>`, `<light>`,
/// `<sound>`, `<include>`, `<terrain>` etc. The tag is the section name, the
/// label is the section's own string value, and the resource/position are the
/// optional named children.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkItem {
    pub tag: String,
    pub label: String,
    pub resource: Option<String>,
    pub position: Option<Vector3>,
    pub transform: Matrix,
}

/// `readMooMatrix` (`chunk.cpp:350-357`) + `XMLSection::asMatrix34`
/// (`xml_section.cpp:915-929`): the `<tag>` section's four `<row0>`..`<row3>`
/// vectors, with the homogeneous column fixed.
pub fn read_matrix34(section: &DataSection, tag: &str) -> Matrix {
    let Some(ts) = section.find_child(tag) else {
        return Matrix::identity();
    };
    let mut m = Matrix::identity();
    for (row, name) in ["row0", "row1", "row2", "row3"].iter().enumerate() {
        let v = ts.read_vector3(name, Vector3::ZERO);
        m.m[row] = [v.x, v.y, v.z, m.m[row][3]];
    }
    m.m[0][3] = 0.0;
    m.m[1][3] = 0.0;
    m.m[2][3] = 0.0;
    m.m[3][3] = 1.0;
    m
}

fn parse_bounding_box(section: &DataSection) -> Option<BoundBox> {
    let bb = section.find_child("boundingBox")?;
    let min = bb.find_child("min")?.as_vector3(Vector3::ZERO);
    let max = bb.find_child("max")?.as_vector3(Vector3::ZERO);
    Some(BoundBox::new(min, max))
}

/// A chunk in a space. `boundaries` is filled by `form_boundaries`
/// (BW-10-01-02); this leaf leaves it empty.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub identifier: String,
    pub is_outside: bool,
    /// Grid coordinates; for an inside chunk these are the directory cell the
    /// identifier names, not the chunk's own position (which is the transform).
    pub x: i16,
    pub z: i16,
    pub transform: Matrix,
    pub unmapped_transform: Matrix,
    pub bounding_box: BoundBox,
    pub items: Vec<ChunkItem>,
    /// The owning mapping (kept so `load` can form the synthetic boundaries).
    pub mapping: GeometryMapping,
    /// `bounds_` (`chunk.cpp:856`): indices into `boundaries` of the planes
    /// that contain the chunk (formed by `form_boundaries`, BW-10-01-02).
    pub bounds: Vec<usize>,
    /// `joints_`: indices into `boundaries` of the passable faces.
    pub joints: Vec<usize>,
    pub boundaries: Vec<ChunkBoundary>,
    /// `loaded_` (`chunk.cpp:397,497`): the chunk section was read.
    pub is_loaded: bool,
    /// `isBound_` (`chunk.cpp:1069`): the chunk's portals were bound.
    pub is_bound: bool,
}

impl Chunk {
    /// `Chunk::Chunk` (`chunk.cpp:157-247`): the transform and bounds for an
    /// outside chunk come from the grid coordinates, scaled by the grid size.
    /// `mapper` is the mapping's transform (identity for the common case).
    pub fn new(identifier: &str, mapping: &GeometryMapping, mapper: &Matrix) -> Self {
        let is_outside = is_outside_chunk(identifier);
        let (x, z) = if is_outside {
            grid_from_chunk_name(identifier).unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        let mut transform = Matrix::identity();
        let mut bounding_box;
        if is_outside {
            let xf = f32::from(x) * mapping.grid_size;
            let zf = f32::from(z) * mapping.grid_size;
            bounding_box = BoundBox::new(
                Vector3::new(xf, MIN_CHUNK_HEIGHT, zf),
                Vector3::new(
                    xf + mapping.grid_size,
                    MAX_CHUNK_HEIGHT,
                    zf + mapping.grid_size,
                ),
            );
            transform.set_translate(xf, 0.0, zf);
            transform.post_multiply(mapper);
            bounding_box.transform_by(mapper);
        } else {
            bounding_box = BoundBox::new(Vector3::ZERO, Vector3::new(1.0, 1.0, 1.0));
            transform.post_multiply(mapper);
        }
        Self {
            identifier: identifier.to_string(),
            is_outside,
            x,
            z,
            transform,
            unmapped_transform: transform,
            bounding_box,
            items: Vec::new(),
            mapping: mapping.clone(),
            bounds: Vec::new(),
            joints: Vec::new(),
            boundaries: Vec::new(),
            is_loaded: false,
            is_bound: false,
        }
    }

    /// `Chunk::load` (`chunk.cpp:370-499`): fill in the transform, bounding
    /// box, boundaries and object set from the section. Outside chunks keep
    /// their grid-derived transform and get their six boundaries synthesised
    /// from the grid; inside chunks read `<transform>` and `<boundingBox>`
    /// and take their boundaries from the shell visual, which
    /// `resource_resolver` loads (the reference opens it via `BWResource`,
    /// `chunk.cpp:814-839`). Like the reference the result is
    /// `good && !boundingBox_.insideOut()`: an inside chunk without a shell
    /// visual that yields at least four boundaries fails to load.
    pub fn load(
        &mut self,
        section: &DataSection,
        mapper: &Matrix,
        resource_resolver: &dyn Fn(&str) -> Option<DataSection>,
    ) -> bool {
        if !self.is_outside {
            let t = read_matrix34(section, "transform");
            self.unmapped_transform = t;
            self.transform = t;
            self.transform.post_multiply(mapper);
            if let Some(bb) = parse_bounding_box(section) {
                self.bounding_box = bb;
                self.bounding_box.transform_by(&self.transform);
            }
        }
        self.items = load_include(section, self.is_outside, resource_resolver);
        let (boundaries, joints, bounds, good) =
            form_boundaries(&self.identifier, section, &self.mapping, resource_resolver);
        self.boundaries = boundaries;
        self.joints = joints;
        self.bounds = bounds;
        // `loaded_ = true` even when `good` is false (`chunk.cpp:497`).
        self.is_loaded = true;
        good && !self.bounding_box.inside_out()
    }
}

/// `Chunk::loadInclude` (`chunk.cpp:555-640`): iterate the section's children
/// recording every scene object. `<shell>` and (for a shell-less inside
/// chunk) the first `<model>` are the shell, not items; `<transform>` and
/// `<boundingBox>` are chunk-level, not items. `include_resolver` loads an
/// include's resource section so nested includes expand inline like the
/// reference.
pub fn load_include(
    section: &DataSection,
    is_outside: bool,
    include_resolver: &dyn Fn(&str) -> Option<DataSection>,
) -> Vec<ChunkItem> {
    let mut items = Vec::new();
    let need_shell = !is_outside && section.find_child("shell").is_none();
    let mut got_first_model = false;
    for i in 0..section.count_children() {
        let Some(child) = section.open_child(i) else {
            continue;
        };
        if child.is_attribute() {
            continue;
        }
        let tag = child.section_name();
        if tag == "shell" || tag == "transform" || tag == "boundingBox" {
            continue;
        }
        if need_shell && tag == "model" && !got_first_model {
            got_first_model = true;
            continue;
        }
        let label = child.as_string().to_string();
        let resource = child
            .find_child("resource")
            .map(|r| r.as_string().to_string());
        let position = child
            .find_child("position")
            .map(|p| p.as_vector3(Vector3::ZERO));
        let transform = read_matrix34(child, "transform");
        if tag == "include"
            && let Some(res) = &resource
            && let Some(include_section) = include_resolver(res)
        {
            let mut nested = load_include(&include_section, is_outside, include_resolver);
            items.append(&mut nested);
            continue;
        }
        items.push(ChunkItem {
            tag: tag.to_string(),
            label,
            resource,
            position,
            transform,
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{GeometryMapping, GridBounds};
    use resources::datasection;

    fn identity_mapping() -> GeometryMapping {
        let bounds = GridBounds {
            min_l_grid_x: 0,
            min_l_grid_y: 0,
            max_l_grid_x: 1,
            max_l_grid_y: 1,
            min_grid_x: 0,
            min_grid_y: 0,
            max_grid_x: 1,
            max_grid_y: 1,
        };
        GeometryMapping::new("space/", 100.0, bounds, false)
    }

    #[test]
    fn outside_chunk_bounds_come_from_the_grid() {
        let mapping = identity_mapping();
        let chunk = Chunk::new("00010000o", &mapping, &Matrix::identity());
        assert!(chunk.is_outside);
        assert_eq!((chunk.x, chunk.z), (1, 0));
        let bb = &chunk.bounding_box;
        assert_eq!(bb.min, Vector3::new(100.0, MIN_CHUNK_HEIGHT, 0.0));
        assert_eq!(bb.max, Vector3::new(200.0, MAX_CHUNK_HEIGHT, 100.0));
    }

    #[test]
    fn inside_chunk_reads_transform_and_bbox() {
        let mapping = identity_mapping();
        let xml = r#"
            <00000000i.chunk>
                <transform>
                    <row0>1 0 0</row0>
                    <row1>0 1 0</row1>
                    <row2>0 0 1</row2>
                    <row3>5 10 15</row3>
                </transform>
                <boundingBox>
                    <min>0 0 0</min>
                    <max>20 30 40</max>
                </boundingBox>
                <model>
                    <resource>shell.model</resource>
                </model>
            </00000000i.chunk>"#;
        let section = datasection::parse("", xml.as_bytes()).unwrap();
        let mut chunk = Chunk::new("00000000i", &mapping, &Matrix::identity());
        assert!(!chunk.is_outside);
        // No shell visual resolves, so the chunk fails to load exactly like
        // the reference (fewer than four boundaries) — but the transform and
        // bounding box are still read before that failure.
        assert!(!chunk.load(&section, &Matrix::identity(), &|_| None));
        assert_eq!(chunk.bounding_box.min, Vector3::new(5.0, 10.0, 15.0));
        assert_eq!(chunk.bounding_box.max, Vector3::new(25.0, 40.0, 55.0));
    }

    #[test]
    fn object_set_lists_scene_objects() {
        let mapping = identity_mapping();
        let xml = r#"
            <00000000o.chunk>
                <model>
                    <resource>tree.model</resource>
                    <transform>
                        <row3>1 2 3</row3>
                    </transform>
                </model>
                <entity>
                    <id>7</id>
                    <type>Bush</type>
                    <position>1 0 2</position>
                </entity>
                <light>
                    <type>Omni</type>
                    <colour>1 1 1</colour>
                </light>
                <sound>
                    <resource>amb.sound</resource>
                    <position>3 0 4</position>
                </sound>
            </00000000o.chunk>"#;
        let section = datasection::parse("", xml.as_bytes()).unwrap();
        let mut chunk = Chunk::new("00000000o", &mapping, &Matrix::identity());
        chunk.load(&section, &Matrix::identity(), &|_| None);
        let tags: Vec<&str> = chunk.items.iter().map(|i| i.tag.as_str()).collect();
        assert_eq!(tags, vec!["model", "entity", "light", "sound"]);
        assert_eq!(chunk.items[0].resource.as_deref(), Some("tree.model"));
        assert_eq!(chunk.items[1].position, Some(Vector3::new(1.0, 0.0, 2.0)));
        assert_eq!(chunk.items[3].position, Some(Vector3::new(3.0, 0.0, 4.0)));
    }

    #[test]
    fn shell_is_not_an_item_and_include_expands() {
        let mapping = identity_mapping();
        // The inside chunk has a <shell>: it is skipped. The include is
        // expanded via the resolver.
        let xml = r#"
            <00000000i.chunk>
                <shell><resource>shell.model</resource></shell>
                <include>
                    <resource>door.i.chunk</resource>
                    <transform><row3>0 0 0</row3></transform>
                </include>
            </00000000i.chunk>"#;
        let section = datasection::parse("", xml.as_bytes()).unwrap();
        let mut chunk = Chunk::new("00000000i", &mapping, &Matrix::identity());
        let include_xml = r#"
            <door.i.chunk>
                <shell><resource>door_shell.model</resource></shell>
                <model><resource>door.model</resource></model>
            </door.i.chunk>"#;
        let resolver = |res: &str| {
            if res == "door.i.chunk" {
                Some(datasection::parse("", include_xml.as_bytes()).unwrap())
            } else {
                None
            }
        };
        chunk.load(&section, &Matrix::identity(), &|_| None);
        // Without a resolver the include stays as an unexpanded item.
        assert_eq!(chunk.items.len(), 1);
        assert_eq!(chunk.items[0].tag, "include");

        // With the resolver the include's own shell is skipped and its model
        // lands in the object set.
        let section = datasection::parse("", xml.as_bytes()).unwrap();
        let items = load_include(&section, false, &resolver);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tag, "model");
        assert_eq!(items[0].resource.as_deref(), Some("door.model"));
    }
}
