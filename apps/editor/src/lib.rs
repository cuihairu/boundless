//! BW-15-02-03's Linux editor leg: edit the fixed map and verify the
//! edit survives a reload.
//!
//! The reference editor is the Windows WorldEditor binary
//! (`tools/worldeditor`); the BW-10/BW-13 port carries its save path as
//! a library — [`chunk::chunk_editor`] is `EditorChunkCache::edSave`
//! (`editor_chunk_cache.cpp:872-980`) and the chunk load/binding is the
//! same shipped sequence the display leg uses. This module binds them
//! into the acceptance the ledger row states — a *fixed map* edited —
//! as one session over a *copy* of the reference fixture space (the
//! fixture itself is never written):
//!
//! 1. load the space through `load_and_bind`;
//! 2. edit the first `<model>` of the outside chunk `00000000o` in its
//!    chunk section — move the transform to `new_position`;
//! 3. save through `EditorChunkCache::ed_save` (locked, present, not
//!    read-only, not deleted), which strips the `processed`/`boundary`
//!    strips and refreshes the transform/bounding box (an outside
//!    chunk's transform stays grid-implied, so it is not rewritten);
//! 4. reload the file from disk and report what changed.
//!
//! Everything is deterministic, so a second session over a second copy
//! must produce byte-identical output — the rerun evidence.

use std::fs;
use std::path::{Path, PathBuf};

use chunk::{ChunkSpace, EditorChunkCache, GeometryMapping};
use foundation::math::Vector3;
use resources::datasection::{self, DataSection};
use resources::file_system::NativeFileSystem;
use resources::multi_file_system::MultiFileSystem;

/// The outside chunk the session edits.
pub const EDITED_CHUNK: &str = "00000000o";

/// What one edit session observed.
#[derive(Debug, Clone, PartialEq)]
pub struct EditReport {
    /// The space copy the session ran against.
    pub space_root: PathBuf,
    /// The edited chunk's identifier.
    pub chunk: String,
    /// The first model's transform before the edit (section space).
    pub old_position: Vector3,
    /// The transform written.
    pub new_position: Vector3,
    /// `ed_save`'s return: true when the save *created* the file. The
    /// fixture's chunk file exists, so this is false.
    pub saved_add: bool,
    /// The chunk file's FNV-1a digest before the edit.
    pub input_sha: u64,
    /// The chunk file's FNV-1a digest after the save.
    pub output_sha: u64,
    /// The reloaded first model's transform equals `new_position`.
    pub new_position_reloaded: bool,
    /// The reloaded file has no `processed` strip (edSave deletes it;
    /// boundaries are rewritten from the portals on the next load).
    pub processed_stripped: bool,
    /// The reloaded file has no `boundary` section.
    pub boundary_stripped: bool,
    /// The reloaded file still carries the chunk's other items (the
    /// edit replaced a transform, not the chunk).
    pub model_count: usize,
    /// The reloaded model count is unchanged from the loaded one.
    pub model_count_preserved: bool,
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        h ^= u64::from(*byte);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Copy a fixture space tree (`res/space_00010001`-shaped) to `dst`,
/// creating `dst` first. The session edits only its copy.
pub fn copy_space(src: &Path, dst: &Path) -> Result<(), String> {
    fn copy_rec(dir: &Path, into: &Path) -> Result<(), String> {
        fs::create_dir_all(into).map_err(|e| format!("mkdir {}: {e}", into.display()))?;
        for entry in fs::read_dir(dir).map_err(|e| format!("readdir {}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| format!("dirent: {e}"))?;
            let from = entry.path();
            let to = into.join(entry.file_name());
            if from.is_dir() {
                copy_rec(&from, &to)?;
            } else {
                fs::copy(&from, &to).map_err(|e| format!("copy {}: {e}", from.display()))?;
            }
        }
        Ok(())
    }
    copy_rec(src, dst)
}

/// Load the space under `space_root` the way the display leg does —
/// parse `settings.xml`, gather the candidates, run `load_and_bind`.
fn load_space(space_root: &Path) -> Result<ChunkSpace, String> {
    let dir_name = space_root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("space root has no directory name: {}", space_root.display()))?;
    let raw = fs::read(space_root.join("settings.xml"))
        .map_err(|e| format!("settings.xml unreadable: {e}"))?;
    let settings = datasection::parse("", &raw).map_err(|e| format!("settings.xml: {e}"))?;
    let mapping = GeometryMapping::from_settings(format!("{dir_name}/"), &settings);
    let candidates: Vec<String> = mapping.gather_chunks(space_root).into_iter().collect();
    let resolver = |name: &str| -> Option<DataSection> {
        let raw = fs::read(space_root.join(name)).ok()?;
        datasection::parse("", &raw).ok()
    };
    let mut space = ChunkSpace::new(1, vec![mapping]);
    let bound = space.load_and_bind(&candidates, &resolver);
    if bound.is_empty() {
        return Err(format!("no chunk bound from {}", space_root.display()));
    }
    Ok(space)
}

/// The first `<model>` child index in the section, if any.
fn first_model(section: &DataSection) -> Option<usize> {
    (0..section.count_children()).find(|i| {
        section
            .open_child(*i)
            .is_some_and(|child| child.section_name() == "model")
    })
}

/// How many direct children carry the tag.
fn children_tagged(section: &DataSection, tag: &str) -> usize {
    (0..section.count_children())
        .filter(|i| {
            section
                .open_child(*i)
                .is_some_and(|child| child.section_name() == tag)
        })
        .count()
}

/// Run one edit session against `space_root` (a *copy* of the fixture —
/// the function writes the edited chunk file back in place). The
/// referenced file system is rooted at the space's parent, matching the
/// mapping path the space loaded with.
pub fn run_edit_session(space_root: &Path, new_position: Vector3) -> Result<EditReport, String> {
    let chunk_path = space_root.join(format!("{EDITED_CHUNK}.chunk"));

    let input_raw = fs::read(&chunk_path).map_err(|e| format!("chunk file unreadable: {e}"))?;
    let input_sha = fnv(&input_raw);
    let mut section = datasection::parse("", &input_raw).map_err(|e| format!("chunk file: {e}"))?;

    // The loaded chunk supplies what edSave reads off it: the
    // identifier, the mapping path, the grid-implied transform and the
    // item list (whose first model transform is the edit's before-side).
    let space = load_space(space_root)?;
    let chunk = space
        .chunk(EDITED_CHUNK)
        .ok_or_else(|| format!("{EDITED_CHUNK} not bound"))?;
    let first = first_model(&section).ok_or_else(|| "no model in the chunk".to_string())?;
    let old_position = section
        .open_child(first)
        .and_then(|model| model.find_child("transform"))
        .map(|t| t.read_vector3("row3", Vector3::ZERO))
        .ok_or_else(|| "the first model has no transform".to_string())?;
    if old_position == new_position {
        return Err("the new position equals the old one; nothing to edit".to_string());
    }
    let loaded_model_count = children_tagged(&section, "model");

    // The edit: move the first model's transform in the section.
    {
        let model = section
            .child_mut(first)
            .ok_or_else(|| "model child vanished".to_string())?;
        let transform_index = (0..model.count_children())
            .find(|i| {
                model
                    .open_child(*i)
                    .is_some_and(|child| child.section_name() == "transform")
            })
            .ok_or_else(|| "the first model has no transform section".to_string())?;
        let transform = model.child_mut(transform_index).expect("transform child");
        assert!(transform.write_vector3("row3", new_position), "row3 writes");
    }

    // The save: locked, present, writable, not deleted — the WorldEditor
    // marks the chunk lockable before it saves.
    let cache = EditorChunkCache {
        present: true,
        deleted: false,
        locked: true,
        read_only: false,
    };
    let parent = space_root
        .parent()
        .ok_or_else(|| format!("{} has no parent", space_root.display()))?;
    let mut file_system = MultiFileSystem::new();
    file_system.add_base_file_system(
        Box::new(NativeFileSystem::new(parent.to_str().ok_or_else(|| {
            format!("parent is not utf-8: {}", parent.display())
        })?)),
        -1,
    );
    let saved_add = cache
        .ed_save(chunk, Some(&mut section), &file_system)
        .map_err(|e| format!("ed_save: {e}"))?;

    // The reload: read the file back off disk and observe the edit.
    let output_raw = fs::read(&chunk_path).map_err(|e| format!("reload failed: {e}"))?;
    let output_sha = fnv(&output_raw);
    let reloaded = datasection::parse("", &output_raw).map_err(|e| format!("reload parse: {e}"))?;
    let first = first_model(&reloaded).ok_or_else(|| "reload lost the model".to_string())?;
    let moved = reloaded
        .open_child(first)
        .and_then(|model| model.find_child("transform"))
        .map(|t| t.read_vector3("row3", Vector3::ZERO))
        .ok_or_else(|| "reload lost the transform".to_string())?;
    let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
    let new_position_reloaded = close(moved.x, new_position.x)
        && close(moved.y, new_position.y)
        && close(moved.z, new_position.z);
    let model_count = children_tagged(&reloaded, "model");

    Ok(EditReport {
        space_root: space_root.to_path_buf(),
        chunk: EDITED_CHUNK.to_string(),
        old_position,
        new_position,
        saved_add,
        input_sha,
        output_sha,
        new_position_reloaded,
        processed_stripped: children_tagged(&reloaded, "processed") == 0,
        boundary_stripped: children_tagged(&reloaded, "boundary") == 0,
        model_count,
        model_count_preserved: model_count == loaded_model_count,
    })
}
