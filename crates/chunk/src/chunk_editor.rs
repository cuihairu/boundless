//! BW-13-03-01 the editor save path: writing an edited chunk back to its
//! `.chunk` file and the pending-save book that drives the batch.
//!
//! A port of `tools/worldeditor/world/editor_chunk_cache.cpp`
//! (`EditorChunkCache::edSave`, `updateDataSectionWithTransform`),
//! `tools/common/editor_chunk_cache_base.cpp` and `lib/chunk/unsaved_chunks.cpp`
//! (`UnsavedChunks` with its `IChunkSaver`/`IChunkLoader` seams). The open
//! half of the map face is the existing [`crate::ChunkSpace`] load path.
//!
//! Seams behind the port, each noted at the affected face:
//! - `edChunkSave`/`edChunkSaveCData` are the live item caches saving their
//!   own edits into the section tree *before* this face runs — the caller
//!   mutates the same [`DataSection`] instead.
//! - The CVS `addFile` bookkeeping has no analogue here; the `add` fact is
//!   returned as the `created` result.
//! - `EditorChunkCacheBase::edSave`'s per-cache `saveChunk` loop and its
//!   second identical `save(resourceID)` are runtime-cache faces.
//! - The stale `.cdata/terrain` cleanup block is marked
//!   `TODO: This code seems to be outdated` in the reference and belongs to
//!   terrain editing (BW-13-03-03).
//! - `UnsavedChunks::mark` pins chunk+overlapper removability, and
//!   `saveChunks`' wait-while-loading / unbind-when-removable steps run on
//!   live `Chunk*` objects the port does not keep; the bookkeeping face
//!   (sets, order, failure propagation) is what carries over.

use std::collections::BTreeSet;
use std::fmt;

use foundation::math::{Vector2, Vector3};
use resources::datasection::DataSection;
use resources::file_system::FileType;
use resources::multi_file_system::MultiFileSystem;

use crate::boundary::{Portal, PortalTarget};
use crate::chunk::Chunk;

/// The `LocaliseUTF8` keys the three save guards report. The english `.res`
/// bodies are compiled resources not present in the reference tree, so the
/// key plus the chunk identifier is the error identity (each call passes
/// exactly `chunk_.identifier()` as the argument).
pub const SAVE_CHUNK_WITHOUT_DATASECTION: &str =
    "WORLDEDITOR/WORLDEDITOR/CHUNK/EDITOR_CHUNK/SAVE_CHUNK_WITHOUT_DATASECTION";
pub const SAVE_CHUNK_WITHOUT_LOCK: &str =
    "WORLDEDITOR/WORLDEDITOR/CHUNK/EDITOR_CHUNK/SAVE_CHUNK_WITHOUT_LOCK";
pub const SAVE_CHUNK_READONLY: &str =
    "WORLDEDITOR/WORLDEDITOR/CHUNK/EDITOR_CHUNK/SAVE_CHUNK_READONLY";

/// One failed save guard: the localisation key and the chunk it fired for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSaveError {
    pub key: &'static str,
    pub identifier: String,
}

impl fmt::Display for ChunkSaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.identifier, self.key)
    }
}

/// The save-side state of `EditorChunkCache` (`present_`, `deleted_` and
/// the `edIsLocked`/`edReadOnly` queries the WorldEditor drives).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorChunkCache {
    /// `present_`: the chunk file existed when last seen.
    pub present: bool,
    /// `deleted_`: erase the file on the next save.
    pub deleted: bool,
    /// `edIsLocked()`: the chunk is locked for editing.
    pub locked: bool,
    /// `edReadOnly()`: the chunk's file is read-only.
    pub read_only: bool,
}

impl EditorChunkCache {
    /// `Chunk::resourceID` (`chunk.cpp:3566-3569`): the mapping's path plus
    /// the identifier plus `.chunk`.
    pub fn resource_id(mapping_path: &str, identifier: &str) -> String {
        format!("{mapping_path}{identifier}.chunk")
    }

    /// `Chunk::binFileName` (`chunk.cpp:3517-3519`): the mapping's path
    /// plus the identifier plus `.cdata`.
    pub fn bin_file_id(mapping_path: &str, identifier: &str) -> String {
        format!("{mapping_path}{identifier}.cdata")
    }

    /// `EditorChunkCache::edSave` (`editor_chunk_cache.cpp:872-980`): the
    /// three guards in the reference order, then the deletion/creation
    /// bookkeeping, the `processed`/`boundary` strips (boundaries are
    /// rewritten from the portals on the next load), the transform and
    /// bounding box refresh, and the XML write through the file system.
    ///
    /// `section` is `None` exactly when `pChunkSection_` is null. `Ok`
    /// carries the reference's `add` flag — true when the save created the
    /// file (the point where the reference runs CVS `addFile`, twice, around
    /// the write); the erase paths return `Ok(false)`.
    pub fn ed_save(
        &self,
        chunk: &Chunk,
        section: Option<&mut DataSection>,
        file_system: &MultiFileSystem,
    ) -> Result<bool, ChunkSaveError> {
        let Some(section) = section else {
            return Err(ChunkSaveError {
                key: SAVE_CHUNK_WITHOUT_DATASECTION,
                identifier: chunk.identifier.clone(),
            });
        };
        if !self.locked {
            return Err(ChunkSaveError {
                key: SAVE_CHUNK_WITHOUT_LOCK,
                identifier: chunk.identifier.clone(),
            });
        }
        if self.read_only {
            return Err(ChunkSaveError {
                key: SAVE_CHUNK_READONLY,
                identifier: chunk.identifier.clone(),
            });
        }

        // figure out what resource this chunk lives in
        let resource_id = Self::resource_id(&chunk.mapping.path, &chunk.identifier);

        // see if we're deleting it
        if self.deleted {
            if self.present {
                // delete the resource, plus the .cdata container when one
                // made it to disk
                file_system.erase_file_or_directory(&resource_id);
                let bin_resource_id = Self::bin_file_id(&chunk.mapping.path, &chunk.identifier);
                if file_system.get_file_type(&bin_resource_id, None) == FileType::File {
                    file_system.erase_file_or_directory(&bin_resource_id);
                }
            }
            // deleted in the same session it was created: nothing on disk
            return Ok(false);
        }

        // give the items a chance to save any changes: the caller has
        // already applied the item edits to `section`.

        // first rewrite the boundary information, then update the bounding
        // box and transform
        section.delete_sections("processed");
        section.delete_sections("boundary");
        update_data_section_with_transform(section, chunk);

        // now save out the datasection to the file
        //  (with any changes made by items to themselves)
        let add = file_system.get_file_type(&resource_id, None) != FileType::File;
        let xml = section.write_to_string();
        file_system.write_file(&resource_id, xml.as_bytes());
        Ok(add)
    }
}

/// `EditorChunkCache::updateDataSectionWithTransform`
/// (`editor_chunk_cache.cpp:1168-1182`): drop the stored transform and
/// bounding box, and write the chunk's current ones back — but only for an
/// inside chunk, whose transform is not implied by its grid coordinates.
pub fn update_data_section_with_transform(section: &mut DataSection, chunk: &Chunk) {
    section.del_child("transform");
    section.del_child("boundingBox");
    if !chunk.is_outside {
        section.write_matrix34("transform", &chunk.transform);
        let bounding_box = section
            .open_or_make_section("boundingBox")
            .expect("just made");
        bounding_box.write_vector3("min", chunk.bounding_box.min);
        bounding_box.write_vector3("max", chunk.bounding_box.max);
    }
}

/// `UnsavedChunks::IChunkSaver` (`unsaved_chunks.hpp:19-25`). The reference
/// passes the live `Chunk*` (`we_chunk_saver.cpp`'s `WEChunkSaver` is the
/// WorldEditor implementation, delegating to `ed_save`); the port names the
/// chunk and the [`EditorChunkCache::ed_save`] face above is the body.
pub trait ChunkSaver {
    fn save(&mut self, identifier: &str) -> bool;
    /// Only the not-deleted assert/wait block consults this in the
    /// reference; the save itself runs either way.
    fn is_deleted(&self, identifier: &str) -> bool;
}

/// `UnsavedChunks::IChunkLoader` (`unsaved_chunks.hpp:27-32`). `find`
/// returns `Chunk*` in the reference, consulted only for
/// `chunk == NULL || chunk->isBound()`; the port answers that directly.
pub trait ChunkLoader {
    fn load(&mut self, identifier: &str);
    fn find_is_unbound(&self, identifier: &str) -> bool;
}

/// `UnsavedChunks` (`unsaved_chunks.cpp`): the identifiers waiting to be
/// written and the ones the last batch got to. A `BTreeSet` carries the
/// reference's `BW::set<BW::string>` ordering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnsavedChunks {
    chunks: BTreeSet<String>,
    saved_chunks: BTreeSet<String>,
}

impl UnsavedChunks {
    pub fn new() -> Self {
        Self::default()
    }

    /// `UnsavedChunks::add` — the reference asserts the chunk is loaded or
    /// loading (`unsaved_chunks.cpp:100-104`); the port only books the
    /// identifier.
    pub fn add(&mut self, identifier: impl Into<String>) {
        self.chunks.insert(identifier.into());
    }

    /// `UnsavedChunks::clear`.
    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    /// `UnsavedChunks::empty`.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// The pending set (`chunks_`).
    pub fn pending(&self) -> &BTreeSet<String> {
        &self.chunks
    }

    /// The already-saved set (`savedChunks_`).
    pub fn saved_chunks(&self) -> &BTreeSet<String> {
        &self.saved_chunks
    }

    /// `UnsavedChunks::save` (`unsaved_chunks.cpp:100-152`): first drop the
    /// extra chunks that are not loadable-and-unbound, save everything
    /// pending, then load each kept extra chunk and save again — loading
    /// can book more chunks, so the pending set is drained once per extra.
    /// `progress` is the save dialog and is a no-op here; a `None` loader
    /// makes the extras list inert where the reference would dereference it.
    pub fn save(
        &mut self,
        saver: &mut dyn ChunkSaver,
        loader: Option<&mut dyn ChunkLoader>,
        extra_chunks: Vec<String>,
    ) -> bool {
        let mut result = true;
        let mut extra_chunks = extra_chunks;
        if let Some(loader) = loader {
            extra_chunks.retain(|id| loader.find_is_unbound(id));
            result &= self.save_chunks(saver);
            for id in &extra_chunks {
                loader.load(id);
                result &= self.save_chunks(saver);
            }
        } else {
            result &= self.save_chunks(saver);
        }
        result
    }

    /// `UnsavedChunks::saveChunks` (`unsaved_chunks.cpp:64-96`): save every
    /// pending chunk (the `isDeleted` check only gates the loaded assert in
    /// the reference), record each one as saved even when its save failed,
    /// then clear the pending set.
    fn save_chunks(&mut self, saver: &mut dyn ChunkSaver) -> bool {
        let mut result = true;
        for id in std::mem::take(&mut self.chunks) {
            result &= saver.save(&id);
            self.saved_chunks.insert(id);
        }
        result
    }

    /// `UnsavedChunks::filter`: remove everything the other container
    /// already saved.
    pub fn filter(&mut self, other: &UnsavedChunks) {
        self.chunks.retain(|id| !other.saved_chunks.contains(id));
    }

    /// `UnsavedChunks::filterWithoutSaving`: drop these identifiers from
    /// the pending set without saving them.
    pub fn filter_without_saving(&mut self, identifiers: &[String]) {
        for id in identifiers {
            self.chunks.remove(id);
        }
    }
}

// ---------------------------------------------------------------------------
// BW-13-03-02: portal editing
// ---------------------------------------------------------------------------

/// One editable portal record — the field surface `EditorChunkPortal`
/// exposes to the property panel and scripts (`editor_chunk_portal.hpp:46-60`:
/// `label`, `internal`, `permissive`, `chunk`, `points`, `uAxis`; the
/// draw-only `triFlags` and the read-only geometry getters stay out).
/// Points keep the reference's plane-space 2D u/v storage.
#[derive(Debug, Clone, PartialEq)]
pub struct PortalEdit {
    /// The portal section's own string value (`setLabel` assigns
    /// `pPortal_->label`; `Portal::from_section` reads it back as the value).
    pub label: String,
    pub internal: bool,
    pub permissive: bool,
    /// The `<chunk>` target: a named chunk, one of the special spellings
    /// (`heaven`/`earth`/`invasive`/`extern`) or empty for none.
    pub chunk: String,
    pub points: Vec<Vector2>,
    pub u_axis: Vector3,
}

/// The child coordinates of the index'th `<portal>` — every `<boundary>`'s
/// `<portal>` children in document order, the enumeration
/// [`PortalEdit::read_all`] and [`apply_portal_edit`] share.
fn portal_coordinates(visual: &DataSection, index: usize) -> Option<(usize, usize)> {
    let mut seen = 0;
    for b in 0..visual.count_children() {
        let Some(boundary) = visual.open_child(b) else {
            continue;
        };
        if boundary.section_name() != "boundary" {
            continue;
        }
        for p in 0..boundary.count_children() {
            let Some(portal) = boundary.open_child(p) else {
                continue;
            };
            if portal.section_name() == "portal" {
                if seen == index {
                    return Some((b, p));
                }
                seen += 1;
            }
        }
    }
    None
}

impl PortalEdit {
    /// Read every portal of a shell visual into editable records. The
    /// boundary copies inside a chunk section are transient — `Chunk::load`
    /// copies them out of the visual (`chunk.cpp:831-839`) and
    /// `EditorChunkCache::edSave` deletes them before writing (`edSave`'s
    /// `deleteSections("boundary")`) — so the visual is the only durable
    /// home a portal edit has.
    pub fn read_all(visual: &DataSection) -> Vec<PortalEdit> {
        let mut out = Vec::new();
        for b in 0..visual.count_children() {
            let Some(boundary) = visual.open_child(b) else {
                continue;
            };
            if boundary.section_name() != "boundary" {
                continue;
            }
            for p in 0..boundary.count_children() {
                let Some(portal) = boundary.open_child(p) else {
                    continue;
                };
                if portal.section_name() != "portal" {
                    continue;
                }
                out.push(Self::from_section(portal));
            }
        }
        out
    }

    /// The parse the record's fields mirror (`Portal::from_section`,
    /// `chunk_boundary.cpp:358-475`).
    fn from_section(portal: &DataSection) -> PortalEdit {
        let mut points = Vec::new();
        for p in 0..portal.count_children() {
            if let Some(child) = portal.open_child(p)
                && child.section_name() == "point"
            {
                let v = child.as_vector3(Vector3::ZERO);
                points.push(Vector2::new(v.x, v.y));
            }
        }
        PortalEdit {
            label: portal.as_string().to_string(),
            internal: portal
                .find_child("internal")
                .is_some_and(|s| s.as_bool(false)),
            permissive: !portal
                .find_child("permissive")
                .is_some_and(|s| !s.as_bool(true)),
            chunk: portal.read_string("chunk", ""),
            points,
            u_axis: portal.read_vector3("uAxis", Vector3::ZERO),
        }
    }
}

/// Apply an edit to the index'th portal of a shell visual — the write side
/// of `EditorChunkPortal`'s setters: `setLabel` reassigns the label, the
/// flag/target/geometry setters land in the portal section the chunk later
/// re-reads on load. The `<point>` children are rewritten in place. Returns
/// false when there is no portal at `index`.
pub fn apply_portal_edit(visual: &mut DataSection, index: usize, edit: &PortalEdit) -> bool {
    let Some((boundary_at, portal_at)) = portal_coordinates(visual, index) else {
        return false;
    };
    let Some(boundary) = visual.child_mut(boundary_at) else {
        return false;
    };
    let Some(portal) = boundary.child_mut(portal_at) else {
        return false;
    };
    portal.set_string(&edit.label);
    portal.write_bool("internal", edit.internal);
    portal.write_bool("permissive", edit.permissive);
    portal.write_string("chunk", &edit.chunk);
    portal.write_vector3("uAxis", edit.u_axis);
    while portal.del_child("point") {}
    for point in &edit.points {
        let child = portal.add_child("point");
        child.set_vector3(Vector3::new(point.x, point.y, 0.0));
    }
    true
}

/// `EditorChunkPortal::getX/getY/getZ`/`centre`
/// (`editor_chunk_portal.cpp:1041-1100`): the world-space centre is the
/// local centre through the owning chunk's transform.
pub fn portal_world_centre(chunk: &Chunk, portal: &Portal) -> Vector3 {
    chunk.transform.apply_point(&portal.lcentre)
}

/// `EditorChunkPortal::getExtern`/`getInvasive`
/// (`editor_chunk_portal.cpp:968-995`): the string spelling the property
/// panel shows.
pub fn portal_extern(portal: &Portal) -> &'static str {
    if matches!(portal.target, PortalTarget::Extern) {
        "True"
    } else {
        "False"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identifier::is_outside_chunk;
    use crate::mapping::{GeometryMapping, GridBounds};
    use foundation::math::{Matrix, Vector3};
    use resources::datasection;
    use resources::file_system::NativeFileSystem;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chunkedit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture(tag: &str) -> (std::path::PathBuf, MultiFileSystem) {
        let root = temp_root(tag);
        // The space directory exists by the time chunks are edited in it.
        std::fs::create_dir_all(root.join("space")).unwrap();
        let mut file_system = MultiFileSystem::new();
        file_system.add_base_file_system(
            Box::new(NativeFileSystem::new(root.to_str().expect("utf8 root"))),
            -1,
        );
        (root, file_system)
    }

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

    const OUTSIDE_CHUNK_XML: &str = r#"
        <00010000o.chunk>
            <model>
                <resource>tree.model</resource>
                <transform>
                    <row3>12 0 34</row3>
                </transform>
            </model>
            <processed><vertexFormat>legacy</vertexFormat></processed>
            <boundary><portal>migrated</portal></boundary>
        </00010000o.chunk>"#;

    const INSIDE_CHUNK_XML: &str = r#"
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
            <processed><vertexFormat>legacy</vertexFormat></processed>
            <boundary><portal>migrated</portal></boundary>
            <boundary><portal>second</portal></boundary>
        </00000000i.chunk>"#;

    fn write_xml(file_system: &MultiFileSystem, path: &str, xml: &str) {
        assert!(file_system.write_file(path, xml.as_bytes()));
    }

    /// How many direct children carry `tag`.
    fn children_tagged(section: &DataSection, tag: &str) -> usize {
        (0..section.count_children())
            .filter(|i| {
                section
                    .open_child(*i)
                    .is_some_and(|child| child.section_name() == tag)
            })
            .count()
    }

    fn open_chunk(
        file_system: &MultiFileSystem,
        mapping: &GeometryMapping,
        identifier: &str,
    ) -> (Chunk, DataSection) {
        let resource_id = EditorChunkCache::resource_id(&mapping.path, identifier);
        let bytes = file_system
            .read_file(&resource_id)
            .expect("chunk file read");
        let section = datasection::parse(&resource_id, &bytes).expect("chunk parse");
        let mut chunk = Chunk::new(identifier, mapping, &Matrix::identity());
        chunk.load(&section, &Matrix::identity(), &|_| None);
        (chunk, section)
    }

    fn editing_cache() -> EditorChunkCache {
        EditorChunkCache {
            present: true,
            deleted: false,
            locked: true,
            read_only: false,
        }
    }

    #[test]
    fn guards_fire_in_reference_order() {
        let mapping = identity_mapping();
        let chunk = Chunk::new("00010000o", &mapping, &Matrix::identity());
        let (_root, file_system) = fixture("guards");
        // No section beats the lock check.
        let err = editing_cache()
            .ed_save(&chunk, None, &file_system)
            .unwrap_err();
        assert_eq!(err.key, SAVE_CHUNK_WITHOUT_DATASECTION);
        assert_eq!(err.identifier, "00010000o");
        // Unlocked with a section is the lock guard.
        let mut section = datasection::parse("chunk", OUTSIDE_CHUNK_XML.as_bytes()).unwrap();
        let err = EditorChunkCache {
            locked: false,
            ..Default::default()
        }
        .ed_save(&chunk, Some(&mut section), &file_system)
        .unwrap_err();
        assert_eq!(err.key, SAVE_CHUNK_WITHOUT_LOCK);
        // Read-only beats nothing else: locked but read-only is refused.
        let err = EditorChunkCache {
            locked: true,
            read_only: true,
            ..Default::default()
        }
        .ed_save(&chunk, Some(&mut section), &file_system)
        .unwrap_err();
        assert_eq!(err.key, SAVE_CHUNK_READONLY);
    }

    #[test]
    fn old_map_edit_survives_save_and_reloads() {
        let (root, file_system) = fixture("roundtrip");
        let mapping = identity_mapping();
        write_xml(&file_system, "space/00010000o.chunk", OUTSIDE_CHUNK_XML);
        let (chunk, mut section) = open_chunk(&file_system, &mapping, "00010000o");
        assert!(chunk.is_outside);
        assert!(chunk.is_loaded);

        // The edit: another tree placed by hand (the item caches' `edChunkSave`
        // face is the caller's mutation of the same section tree); a new
        // child appends like the reference's `newSection` does.
        let model = section.add_child("model");
        model.write_string("resource", "house.model");
        model.write_vector3("position", Vector3::new(50.0, 0.0, 25.0));

        let created = editing_cache()
            .ed_save(&chunk, Some(&mut section), &file_system)
            .expect("save ok");
        assert!(!created, "the chunk file already existed");

        // Reload: the reference-faithful loader takes the written bytes.
        let bytes = file_system
            .read_file("space/00010000o.chunk")
            .expect("file rewritten");
        let text = String::from_utf8(bytes.clone()).expect("xml is utf8");
        let reloaded_section = datasection::parse("space/00010000o.chunk", &bytes).expect("parse");
        let mut reloaded = Chunk::new("00010000o", &mapping, &Matrix::identity());
        assert!(reloaded.load(&reloaded_section, &Matrix::identity(), &|_| None));

        // The edit is in the reloaded object set, the legacy strips are
        // gone, and the transform is not stored for an outside chunk.
        assert_eq!(reloaded.items.len(), 2);
        assert_eq!(reloaded.items[1].resource.as_deref(), Some("house.model"));
        assert_eq!(
            reloaded.items[1].position,
            Some(Vector3::new(50.0, 0.0, 25.0))
        );
        assert!(reloaded_section.find_child("processed").is_none());
        assert!(reloaded_section.find_child("boundary").is_none());
        assert!(reloaded_section.find_child("transform").is_none());
        assert!(reloaded_section.find_child("boundingBox").is_none());
        // The written XML keeps the reference writer's shape: `save`
        // reassigns the root tag to the file name
        // (`splitSaveAsFileName`), and a digit-leading name fails
        // `isValidXMLTag` so `writeToStream` emits `<root>` — the same name
        // `createFromBinary("root", …)` gives it on the way in.
        assert!(text.starts_with("<root>"));
        assert!(text.contains("\t\t<resource>\thouse.model\t</resource>"));
        assert!(root.join("space").exists());
    }

    #[test]
    fn inside_chunk_transform_and_bbox_are_rewritten_from_the_chunk() {
        let (_root, file_system) = fixture("inside");
        let mapping = identity_mapping();
        write_xml(&file_system, "space/00000000i.chunk", INSIDE_CHUNK_XML);
        let (mut chunk, mut section) = open_chunk(&file_system, &mapping, "00000000i");
        assert!(!chunk.is_outside);

        // The chunk moved: the new transform goes out, the stored box follows.
        chunk.transform.m[3][0] = 7.0;
        chunk.transform.m[3][1] = 8.0;
        chunk.transform.m[3][2] = 9.0;
        chunk.bounding_box.min = Vector3::new(1.0, 2.0, 3.0);
        chunk.bounding_box.max = Vector3::new(11.0, 12.0, 13.0);
        assert!(
            editing_cache()
                .ed_save(&chunk, Some(&mut section), &file_system)
                .is_ok()
        );

        let bytes = file_system.read_file("space/00000000i.chunk").unwrap();
        let back = datasection::parse("space/00000000i.chunk", &bytes).unwrap();
        // Exactly one transform, from the chunk, and exactly one box.
        assert_eq!(children_tagged(&back, "transform"), 1);
        let ts = back.open_section("transform").unwrap();
        assert_eq!(
            ts.read_vector3("row3", Vector3::ZERO),
            Vector3::new(7.0, 8.0, 9.0)
        );
        let bb = back.open_section("boundingBox").unwrap();
        assert_eq!(
            bb.read_vector3("min", Vector3::ZERO),
            Vector3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(
            bb.read_vector3("max", Vector3::ZERO),
            Vector3::new(11.0, 12.0, 13.0)
        );
        assert_eq!(children_tagged(&back, "boundary"), 0);
        assert_eq!(children_tagged(&back, "processed"), 0);
    }

    #[test]
    fn delete_mark_saves_erase_chunk_and_cdata() {
        let (_root, file_system) = fixture("erase");
        let mapping = identity_mapping();
        write_xml(&file_system, "space/00010000o.chunk", OUTSIDE_CHUNK_XML);
        write_xml(&file_system, "space/00010000o.cdata", "<cdata/>");
        let chunk = Chunk::new("00010000o", &mapping, &Matrix::identity());
        // The guards run before the deletion branch, so the section is
        // loaded even when the chunk is on its way out.
        let mut section = datasection::parse("chunk", OUTSIDE_CHUNK_XML.as_bytes()).unwrap();
        let cache = EditorChunkCache {
            present: true,
            deleted: true,
            locked: true,
            read_only: false,
        };
        assert_eq!(
            cache.ed_save(&chunk, Some(&mut section), &file_system),
            Ok(false)
        );
        assert_eq!(
            file_system.get_file_type("space/00010000o.chunk", None),
            FileType::NotFound
        );
        assert_eq!(
            file_system.get_file_type("space/00010000o.cdata", None),
            FileType::NotFound
        );
        // Deleted in the same session it was created: a miss on disk is fine.
        let ghost = Chunk::new("00020000o", &mapping, &Matrix::identity());
        let cache = EditorChunkCache {
            present: false,
            deleted: true,
            locked: true,
            read_only: false,
        };
        assert_eq!(
            cache.ed_save(&ghost, Some(&mut section), &file_system),
            Ok(false)
        );
        assert_eq!(
            file_system.get_file_type("space/00020000o.chunk", None),
            FileType::NotFound
        );
    }

    #[test]
    fn creating_a_chunk_reports_add_once() {
        let (_root, file_system) = fixture("created");
        let mapping = identity_mapping();
        let chunk = Chunk::new("00030000o", &mapping, &Matrix::identity());
        let mut section =
            datasection::parse("chunk", b"<00030000o.chunk><model/></00030000o.chunk>").unwrap();
        let cache = editing_cache();
        let created = cache
            .ed_save(&chunk, Some(&mut section), &file_system)
            .expect("save ok");
        assert!(created, "the file did not exist before this save");
        assert_eq!(
            file_system.get_file_type("space/00030000o.chunk", None),
            FileType::File
        );
        let created = cache
            .ed_save(&chunk, Some(&mut section), &file_system)
            .expect("save ok");
        assert!(!created);
    }

    #[test]
    fn unsaved_chunks_save_drains_and_records() {
        struct CountingSaver {
            failing: Option<&'static str>,
            calls: Vec<String>,
        }
        impl ChunkSaver for CountingSaver {
            fn save(&mut self, identifier: &str) -> bool {
                self.calls.push(identifier.to_string());
                Some(identifier) != self.failing
            }
            fn is_deleted(&self, identifier: &str) -> bool {
                identifier == "zzzo"
            }
        }
        struct NoLoader;
        impl ChunkLoader for NoLoader {
            fn load(&mut self, _identifier: &str) {}
            fn find_is_unbound(&self, _identifier: &str) -> bool {
                false
            }
        }

        let mut pending = UnsavedChunks::new();
        assert!(pending.is_empty());
        pending.add("aaa");
        pending.add("bbb");
        assert!(!pending.is_empty());
        assert_eq!(
            pending
                .pending()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["aaa", "bbb"]
        );

        let mut saver = CountingSaver {
            failing: Some("bbb"),
            calls: Vec::new(),
        };
        // The `isDeleted` face: only the reference's assert consults it,
        // the port exposes it untouched.
        assert!(saver.is_deleted("zzzo"));
        assert!(!saver.is_deleted("aaa"));
        let mut no_loader = NoLoader;
        assert!(!no_loader.find_is_unbound("aaa"));
        assert!(!pending.save(&mut saver, Some(&mut no_loader), Vec::new()));
        // Both attempted, both recorded, pending drained despite the miss.
        assert_eq!(saver.calls, vec!["aaa", "bbb"]);
        assert_eq!(
            pending
                .saved_chunks()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["aaa", "bbb"]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn unsaved_chunks_extra_chunks_load_then_resave() {
        struct RecordingLoader {
            loadable: &'static [&'static str],
            loaded: Vec<String>,
        }
        impl ChunkLoader for RecordingLoader {
            fn load(&mut self, identifier: &str) {
                self.loaded.push(identifier.to_string());
            }
            fn find_is_unbound(&self, identifier: &str) -> bool {
                self.loadable.contains(&identifier)
            }
        }
        struct OkSaver;
        impl ChunkSaver for OkSaver {
            fn save(&mut self, _identifier: &str) -> bool {
                true
            }
            fn is_deleted(&self, _identifier: &str) -> bool {
                false
            }
        }

        let mut pending = UnsavedChunks::new();
        pending.add("aaao");
        // Bound or missing extras are dropped before any load.
        let mut loader = RecordingLoader {
            loadable: &["bbbo"],
            loaded: Vec::new(),
        };
        let mut saver = OkSaver;
        assert!(!saver.is_deleted("bbbo"));
        assert!(loader.find_is_unbound("bbbo"));
        assert!(!loader.find_is_unbound("ccco"));
        assert!(pending.save(
            &mut saver,
            Some(&mut loader),
            vec!["bbbo".to_string(), "ccco".to_string()]
        ));
        assert_eq!(loader.loaded, vec!["bbbo"]);
        // `ccco` never saved, `aaao` did.
        assert_eq!(
            pending
                .saved_chunks()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["aaao"]
        );
        // No loader: the extras list is inert rather than fatal.
        let mut pending = UnsavedChunks::new();
        pending.add("aaao");
        let mut saver = OkSaver;
        assert!(pending.save(&mut saver, None, vec!["zzzo".to_string()]));
    }

    #[test]
    fn unsaved_chunks_filter_faces() {
        struct OkSaver;
        impl ChunkSaver for OkSaver {
            fn save(&mut self, _identifier: &str) -> bool {
                true
            }
            fn is_deleted(&self, _identifier: &str) -> bool {
                false
            }
        }
        let mut saved_batch = UnsavedChunks::new();
        saved_batch.add("aaao");
        saved_batch.add("bbbo");
        let mut saver = OkSaver;
        assert!(!saver.is_deleted("aaao"));
        saved_batch.save(&mut saver, None::<&mut dyn ChunkLoader>, Vec::new());

        let mut pending = UnsavedChunks::new();
        pending.add("aaao");
        pending.add("ccco");
        pending.filter(&saved_batch);
        assert_eq!(
            pending
                .pending()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["ccco"]
        );
        pending.filter_without_saving(&["ccco".to_string()]);
        assert!(pending.is_empty());
        pending.clear();
        assert!(pending.is_empty());
    }

    #[test]
    fn resource_and_bin_ids_follow_the_mapping_path() {
        assert_eq!(
            EditorChunkCache::resource_id("space/", "00010000o"),
            "space/00010000o.chunk"
        );
        assert_eq!(
            EditorChunkCache::bin_file_id("space/", "00010000o"),
            "space/00010000o.cdata"
        );
    }

    #[test]
    fn save_error_displays_identifier_then_key() {
        let err = ChunkSaveError {
            key: SAVE_CHUNK_READONLY,
            identifier: "00010000o".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "00010000o: WORLDEDITOR/WORLDEDITOR/CHUNK/EDITOR_CHUNK/SAVE_CHUNK_READONLY"
        );
    }

    #[test]
    fn is_outside_chunk_gate_agrees_with_the_identifier() {
        // The save path branches on outside-ness; pin the predicate used.
        assert!(is_outside_chunk("00010000o"));
        assert!(!is_outside_chunk("00010000i"));
    }

    // ---- BW-13-03-02: chunk and portal editing ----

    /// A shell visual for an inside chunk: four wall boundaries, the +x
    /// wall carrying one portal to `portal_chunk`. The label is the
    /// portal's own string value, exactly how `Portal::from_section` and
    /// `setLabel` treat it.
    fn visual_xml(portal_chunk: &str, label: &str) -> String {
        format!(
            r#"
            <shell_a.visual>
                <boundary><normal>-1 0 0</normal><d>0</d></boundary>
                <boundary><normal>1 0 0</normal><d>10</d>
                    <portal>
                        {label}
                        <chunk>{portal_chunk}</chunk>
                        <uAxis>0 1 0</uAxis>
                        <point>0 0 0</point>
                        <point>10 0 0</point>
                        <point>10 10 0</point>
                        <point>0 10 0</point>
                    </portal>
                </boundary>
                <boundary><normal>0 -1 0</normal><d>0</d></boundary>
                <boundary><normal>0 1 0</normal><d>10</d></boundary>
            </shell_a.visual>"#
        )
    }

    fn chunk_xml() -> &'static str {
        r#"
            <00000000i.chunk>
                <transform>
                    <row0>1 0 0</row0>
                    <row1>0 1 0</row1>
                    <row2>0 0 1</row2>
                    <row3>5 10 15</row3>
                </transform>
                <shell><resource>shell_a.model</resource></shell>
            </00000000i.chunk>"#
    }

    fn resolver(visual: &DataSection) -> impl Fn(&str) -> Option<DataSection> + '_ {
        move |name: &str| {
            if name == "shell_a.visual" {
                Some(visual.clone())
            } else {
                None
            }
        }
    }

    #[test]
    fn portal_edit_round_trips_through_the_visual() {
        let mut visual =
            datasection::parse("shell_a.visual", visual_xml("00000001i", "door").as_bytes())
                .unwrap();
        // Only the shell visual resolves; anything else is a miss.
        assert!(resolver(&visual)("nope.visual").is_none());
        let edits = PortalEdit::read_all(&visual);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].label, "door");
        assert!(!edits[0].internal);
        assert!(edits[0].permissive);
        assert_eq!(edits[0].chunk, "00000001i");
        assert_eq!(edits[0].u_axis, Vector3::new(0.0, 1.0, 0.0));
        assert_eq!(edits[0].points.len(), 4);

        // The edit: relabel, forbid passage, shrink the opening.
        let edit = PortalEdit {
            label: "doorway".to_string(),
            internal: false,
            permissive: false,
            chunk: "00000002i".to_string(),
            points: vec![
                Vector2::new(2.0, 0.0),
                Vector2::new(8.0, 0.0),
                Vector2::new(8.0, 6.0),
                Vector2::new(2.0, 6.0),
            ],
            u_axis: edits[0].u_axis,
        };
        assert!(apply_portal_edit(&mut visual, 0, &edit));
        let edits = PortalEdit::read_all(&visual);
        assert_eq!(edits[0].label, "doorway");
        assert!(!edits[0].permissive);
        assert_eq!(edits[0].chunk, "00000002i");
        assert_eq!(edits[0].points.len(), 4);
        assert_eq!(edits[0].points[0], Vector2::new(2.0, 0.0));

        // The re-derived portal on load picks up the target and the moved
        // centre (the u/v average through the plane basis).
        let boundaries = crate::boundary::create_boundaries_from_shell(
            &datasection::parse("chunk", chunk_xml().as_bytes()).unwrap(),
            &resolver(&visual),
        );
        let moved = boundaries[1].unbound_portals[0].clone();
        assert_eq!(moved.target, PortalTarget::Chunk("00000002i".into()));
        assert_eq!(moved.lcentre, Vector3::new(10.0, 5.0, 3.0));

        // Out-of-range edits are refused.
        let mut scratch = visual.clone();
        assert!(!apply_portal_edit(&mut scratch, 1, &edit));
    }

    #[test]
    fn internal_flag_edit_moves_a_boundary_out_of_bounds() {
        let visual =
            datasection::parse("shell_a.visual", visual_xml("00000001i", "door").as_bytes())
                .unwrap();
        let resolve = resolver(&visual);

        // Baseline adjacency: four boundaries, the portal wall is a joint
        // and a bound (non-internal portal).
        let section = datasection::parse("chunk", chunk_xml().as_bytes()).unwrap();
        let (boundaries, joints, bounds, good) =
            crate::boundary::form_boundaries("00000000i", &section, &identity_mapping(), &resolve);
        assert!(good);
        assert_eq!(boundaries.len(), 4);
        assert_eq!(joints, vec![1]);
        assert_eq!(bounds, vec![0, 1, 2, 3]);

        // The edit: mark the portal wall internal — still a joint, no
        // longer a bound (chunk.cpp's convexity rule: only non-internal
        // first portals make a boundary a bound).
        let mut edited_visual = visual.clone();
        let mut edit = PortalEdit::read_all(&edited_visual).remove(0);
        edit.internal = true;
        assert!(apply_portal_edit(&mut edited_visual, 0, &edit));
        let resolve = resolver(&edited_visual);
        let (_, joints, bounds, _) =
            crate::boundary::form_boundaries("00000000i", &section, &identity_mapping(), &resolve);
        assert_eq!(joints, vec![1]);
        assert_eq!(
            bounds,
            vec![0, 2, 3],
            "the internal wall left the bounds set"
        );
    }

    #[test]
    fn retarget_edit_moves_the_adjacency_to_the_new_neighbour() {
        let visual =
            datasection::parse("shell_a.visual", visual_xml("00000001i", "door").as_bytes())
                .unwrap();
        let resolve = resolver(&visual);
        let section = datasection::parse("chunk", chunk_xml().as_bytes()).unwrap();
        let (boundaries, _, _, _) =
            crate::boundary::form_boundaries("00000000i", &section, &identity_mapping(), &resolve);
        assert_eq!(
            boundaries[1].unbound_portals[0].target,
            PortalTarget::Chunk("00000001i".into())
        );

        // Retarget to the chunk across the wall.
        let mut edited_visual = visual.clone();
        let mut edit = PortalEdit::read_all(&edited_visual).remove(0);
        edit.chunk = "00000009i".to_string();
        assert!(apply_portal_edit(&mut edited_visual, 0, &edit));
        {
            // Scoped: the resolver's opaque type may hold drop glue, so the
            // borrow would otherwise outlive the next mutation below.
            let resolve = resolver(&edited_visual);
            let (boundaries, joints, _, _) = crate::boundary::form_boundaries(
                "00000000i",
                &section,
                &identity_mapping(),
                &resolve,
            );
            assert_eq!(
                joints,
                vec![1],
                "the wall keeps connecting through the portal"
            );
            assert_eq!(
                boundaries[1].unbound_portals[0].target,
                PortalTarget::Chunk("00000009i".into())
            );
        }

        // Emptying the target disconnects it: the portal stays a joint
        // candidate but carries no neighbour.
        let mut edit = PortalEdit::read_all(&edited_visual).remove(0);
        edit.chunk = String::new();
        assert!(apply_portal_edit(&mut edited_visual, 0, &edit));
        let resolve = resolver(&edited_visual);
        let (boundaries, _, _, _) =
            crate::boundary::form_boundaries("00000000i", &section, &identity_mapping(), &resolve);
        assert_eq!(
            boundaries[1].unbound_portals[0].target,
            PortalTarget::Nothing
        );
    }

    #[test]
    fn saving_a_chunk_writes_no_boundary_sections() {
        // `EditorChunkPortal::edSave` is a no-op ("portals are saved with
        // the chunk") and edSave deletes the boundary copies outright:
        // they are transient runtime copies of the shell visual's
        // boundaries (chunk.cpp:831-839).
        let (_root, file_system) = fixture("portalsave");
        let mapping = identity_mapping();
        let xml = r#"
            <00000000i.chunk>
                <transform><row3>0 0 0</row3></transform>
                <shell><resource>shell_a.model</resource></shell>
                <boundary><normal>1 0 0</normal><d>5</d>
                    <portal><chunk>00000001i</chunk><uAxis>0 1 0</uAxis></portal>
                </boundary>
            </00000000i.chunk>"#;
        write_xml(&file_system, "space/00000000i.chunk", xml);
        let (chunk, mut section) = open_chunk(&file_system, &mapping, "00000000i");
        assert!(
            editing_cache()
                .ed_save(&chunk, Some(&mut section), &file_system)
                .is_ok()
        );
        let bytes = file_system.read_file("space/00000000i.chunk").unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("<boundary>"));
        assert!(text.contains("<shell>"), "the rest of the chunk survives");
    }

    #[test]
    fn portal_world_centre_and_extern_face() {
        let visual =
            datasection::parse("shell_a.visual", visual_xml("00000001i", "door").as_bytes())
                .unwrap();
        let resolve = resolver(&visual);
        let section = datasection::parse("chunk", chunk_xml().as_bytes()).unwrap();
        let mut chunk = Chunk::new("00000000i", &identity_mapping(), &Matrix::identity());
        chunk.load(&section, &Matrix::identity(), &resolve);
        let (boundaries, _, _, _) =
            crate::boundary::form_boundaries("00000000i", &section, &identity_mapping(), &resolve);
        let portal = &boundaries[1].unbound_portals[0];

        // lcentre (10, 5, 5) through the chunk transform (5, 10, 15).
        assert_eq!(
            portal_world_centre(&chunk, portal),
            Vector3::new(15.0, 15.0, 20.0)
        );
        // The property-panel spelling of the extern face.
        assert_eq!(portal_extern(portal), "False");
        let mut ext = portal.clone();
        ext.target = PortalTarget::Extern;
        assert_eq!(portal_extern(&ext), "True");
    }
}
