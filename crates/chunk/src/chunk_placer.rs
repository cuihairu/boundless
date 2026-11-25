//! BW-13-03-04 object placement and transform: the WorldEditor faces of
//! `tools/worldeditor/editor/chunk_item_placer.cpp` (creation, the placement
//! coordinate maths), `editor/item_properties.cpp` (`ChunkItemMatrix`, the
//! move/commit cycle with its undo records) and the selection machinery of
//! `world/world_manager.cpp` (`updateSelection`, the deferred replacement).
//!
//! The port is headless: instead of a live scene graph the world keeps
//! [`EditorItem`] records with their observable editor callbacks counted
//! (`edSelected`/`edPostCreate`/`edSave`...), so the acceptance can compare
//! coordinates, property changes and selection events. The reference
//! speed-tree/VLO clamps of `ChunkItemMatrix::commitState` need the
//! supermodel face and are documented divergences; the `Snap::toGround`
//! collision ray is injected as a closure.

use std::collections::BTreeSet;

use foundation::math::{BoundBox, Matrix, Vector3, clamp};
use resources::datasection::DataSection;

use crate::snaps;
use crate::space::ChunkSpace;

/// `ITEM_CAMERA_OFFSET` (`chunk_item_placer.cpp:40`): the camera-relative
/// placement distance is scaled by this and the item's bounding box.
pub const ITEM_CAMERA_OFFSET: f32 = 2.0;

/// `PLACER_FEET_TO_METRES` (`chunk_item_placer.cpp:42`).
pub const PLACER_FEET_TO_METRES: f32 = 0.3048;

/// `PLACECTRL_NORMAL_STR` (`placement_presets.cpp:10`): the preset selector
/// value that means "no preset", i.e. rotation 0 and scale 1.
pub const PLACECTRL_NORMAL_STR: &str = "normalPlacement";

/// `unitRand` (`mathdef.hpp:118-122`): the injected `bw_random` over the
/// full u32 range.
pub fn unit_rand(rng: &mut dyn FnMut() -> u32) -> f32 {
    rng() as f32 / u32::MAX as f32
}

/// `randInRange` (`mathdef.hpp:127-130`).
pub fn rand_in_range(rng: &mut dyn FnMut() -> u32, min: f32, max: f32) -> f32 {
    min + unit_rand(rng) * (max - min)
}

/// `(bw_random() % int(max - min + 1)) + min` degrees
/// (`chunk_item_placer.cpp:1390-1393`). A non-positive span keeps the
/// minimum — the reference would take `% 0`.
fn random_degrees(rng: &mut dyn FnMut() -> u32, min: f32, max: f32) -> f32 {
    let span = (max - min + 1.0) as i32;
    if span > 0 {
        (rng() % span as u32) as f32 + min
    } else {
        min
    }
}

/// `VeryLargeObject::generateUID` (`chunk_vlo.cpp:732-738`): a `UniqueID`
/// string, lowercased. The reference seeds from the OS GUID source; the port
/// draws four words from the injected generator (`%08X.%08X.%08X.%08X`, the
/// `UniqueID` layout the asset pipeline already pins).
pub fn generate_uid(rng: &mut dyn FnMut() -> u32) -> String {
    format!("{:08X}.{:08X}.{:08X}.{:08X}", rng(), rng(), rng(), rng()).to_lowercase()
}

// -----------------------------------------------------------------------------
// PlacementPresets (misc/placement_presets.cpp)
// -----------------------------------------------------------------------------

/// One axis triple of a preset: min/max degrees (rotation) or factors (scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresetRange {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

/// `PlacementPresets`: the current preset selector plus the stock list read
/// from the placement-presets section.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementPresets {
    pub current_preset: String,
    pub presets: Vec<PlacementPreset>,
}

/// One preset row: a name plus rotation/scale ranges and flags.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementPreset {
    pub name: String,
    pub rotation: PresetRange,
    pub scale: PresetRange,
    /// `uniformScale` (`placement_presets.cpp:190`).
    pub uniform_scale: bool,
    /// `stockPreset` (`placement_presets.cpp:312`).
    pub stock_preset: bool,
}

impl PlacementPresets {
    /// The stock state: no preset selected (`PlacementPresets::PlacementPresets`,
    /// `placement_presets.cpp:25`).
    pub fn normal() -> Self {
        Self {
            current_preset: PLACECTRL_NORMAL_STR.to_string(),
            presets: Vec::new(),
        }
    }

    /// `defaultPresetCurrent` (`placement_presets.cpp:301-304`).
    pub fn default_preset_current(&self) -> bool {
        self.current_preset == PLACECTRL_NORMAL_STR
    }

    /// `getPresetData` (`placement_presets.cpp:194-208`): the current preset's
    /// row, found by its `name` child.
    pub fn current(&self) -> Option<&PlacementPreset> {
        self.presets.iter().find(|p| p.name == self.current_preset)
    }

    /// `getCurrentPresetData(ROTATION, axis, value)` (`placement_presets.cpp:161-177`).
    pub fn rotation(&self, axis: usize, max: bool) -> f32 {
        if self.default_preset_current() {
            return 0.0;
        }
        match self.current() {
            Some(p) => {
                if max {
                    p.rotation.max[axis]
                } else {
                    p.rotation.min[axis]
                }
            }
            None => 0.0,
        }
    }

    /// `getCurrentPresetData(SCALE, axis, value)`.
    pub fn scale(&self, axis: usize, max: bool) -> f32 {
        if self.default_preset_current() {
            return 1.0;
        }
        match self.current() {
            Some(p) => {
                if max {
                    p.scale.max[axis]
                } else {
                    p.scale.min[axis]
                }
            }
            None => 0.0,
        }
    }

    /// `isCurrentDataUniform(SCALE)` (`placement_presets.cpp:179-191`): the
    /// default preset and a missing preset are both uniform.
    pub fn scale_uniform(&self) -> bool {
        if self.default_preset_current() {
            return true;
        }
        match self.current() {
            Some(p) => p.uniform_scale,
            None => true,
        }
    }

    /// The presets section face: every child with a `name` child becomes a
    /// preset row reading `minRotX`..`maxRotZ`, `minScaX`..`maxScaZ`,
    /// `uniformScale` and `stockPreset` (`placement_presets.cpp:194-208` and
    /// the `getCurrentPresetData` section names).
    pub fn from_section(current_preset: &str, section: &DataSection) -> Self {
        let mut presets = Vec::new();
        for i in 0..section.count_children() {
            let Some(child) = section.open_child(i) else {
                continue;
            };
            let name = child.read_string("name", "");
            if name.is_empty() {
                continue;
            }
            let rotation = PresetRange {
                min: [
                    child.read_float("minRotX", 0.0),
                    child.read_float("minRotY", 0.0),
                    child.read_float("minRotZ", 0.0),
                ],
                max: [
                    child.read_float("maxRotX", 0.0),
                    child.read_float("maxRotY", 0.0),
                    child.read_float("maxRotZ", 0.0),
                ],
            };
            let scale = PresetRange {
                min: [
                    child.read_float("minScaX", 1.0),
                    child.read_float("minScaY", 1.0),
                    child.read_float("minScaZ", 1.0),
                ],
                max: [
                    child.read_float("maxScaX", 1.0),
                    child.read_float("maxScaY", 1.0),
                    child.read_float("maxScaZ", 1.0),
                ],
            };
            presets.push(PlacementPreset {
                name,
                rotation,
                scale,
                uniform_scale: child.read_bool("uniformScale", false),
                stock_preset: child.read_bool("stockPreset", false),
            });
        }
        Self {
            current_preset: current_preset.to_string(),
            presets,
        }
    }
}

// -----------------------------------------------------------------------------
// The editor item (ChunkItemPtr with its editor virtuals)
// -----------------------------------------------------------------------------

/// The live editor item a placement or transform works on: the chunk-item
/// section fields plus the editor virtuals the placement path calls
/// (`edTransform`/`edBounds`/`edMovementDeltaSnaps`/`edDescription`...).
/// The callback counters make the acceptance's event comparison observable.
#[derive(Debug, Clone)]
pub struct EditorItem {
    pub tag: String,
    pub resource: Option<String>,
    pub label: String,
    /// The chunk-local pose (`transform_`).
    pub transform: Matrix,
    /// `edBounds` (local).
    pub bounds: BoundBox,
    /// The owning chunk identifier (`pChunk_`); `None` is the deleted state.
    pub chunk: Option<String>,
    /// `edDescription` — the undo barrier and error texts carry it.
    pub description: String,
    /// `edMovementDeltaSnaps` (`editor_chunk_item.hpp:340` defaults to zero).
    pub movement_delta_snaps: Vector3,
    /// `edAngleSnaps` (`editor_chunk_item.hpp:344` defaults to zero).
    pub angle_snaps: f32,
    /// `isShellModel` — the too-small placement path deletes the chunk.
    pub is_shell_model: bool,
    /// The drop-point flavour: `EditorChunkModel::edTransform` drops by the
    /// transformed bounding-box centre (`editor_chunk_model.cpp:1021`), the
    /// entity/light family by `applyToOrigin` (`editor_chunk_entity.cpp:828`).
    pub drop_at_bounds_centre: bool,
    /// The factory's `onePerChunk` claim.
    pub one_per_chunk: bool,
    /// `edCanDelete` (defaults to true).
    pub can_delete: bool,
    // Observable editor callbacks:
    pub is_selected: bool,
    pub post_creates: u32,
    pub pre_deletes: u32,
    pub post_modifies: u32,
    pub chunk_binds: u32,
    pub saves: u32,
    pub selected_calls: u32,
    pub deselected_calls: u32,
}

impl EditorItem {
    /// `Chunk::loadItem`'s factory view of a section: build the item from the
    /// section tag/label/resource. The chunk-level section names have no
    /// factory and are ignored (`chunk.cpp` `Chunk::loadItem` falls through
    /// to `SucceededWithoutItem` for unknown names).
    pub fn from_section(section: &DataSection) -> Option<Self> {
        let tag = section.section_name().to_string();
        if matches!(tag.as_str(), "transform" | "boundingBox" | "shell") {
            return None;
        }
        let resource = child_string(section, "resource");
        Some(Self {
            label: section.as_string().to_string(),
            resource,
            tag: tag.clone(),
            transform: Matrix::identity(),
            bounds: BoundBox::new(Vector3::ZERO, Vector3::new(1.0, 1.0, 1.0)),
            chunk: None,
            description: format!(
                "{} {}",
                tag,
                child_string(section, "resource").unwrap_or_default()
            ),
            movement_delta_snaps: Vector3::ZERO,
            angle_snaps: 0.0,
            is_shell_model: false,
            drop_at_bounds_centre: true,
            one_per_chunk: false,
            can_delete: true,
            is_selected: false,
            post_creates: 0,
            pre_deletes: 0,
            post_modifies: 0,
            chunk_binds: 0,
            saves: 0,
            selected_calls: 0,
            deselected_calls: 0,
        })
    }
}

fn child_string(section: &DataSection, tag: &str) -> Option<String> {
    section.find_child(tag).map(|c| c.as_string().to_string())
}

// -----------------------------------------------------------------------------
// The item factory (Chunk::loadItem + ChunkItemFactory::Result)
// -----------------------------------------------------------------------------

/// `ChunkItemFactory::Result` (`chunk_item.hpp:272-330`).
#[derive(Debug, Clone)]
pub struct FactoryResult {
    /// The `operator bool` conversion.
    pub succeeded: bool,
    /// The created (or pre-existing) item; `None` is a NULL item even on
    /// success — `createChunkItem` answers "Couldn't create Chunk Item".
    pub item: Option<usize>,
    pub one_per_chunk: bool,
    pub error: String,
}

impl FactoryResult {
    pub fn ok(item: usize) -> Self {
        Self {
            succeeded: true,
            item: Some(item),
            one_per_chunk: false,
            error: String::new(),
        }
    }

    /// `ChunkItemFactory::SucceededWithoutItem` — unknown section names are
    /// ignored (`chunk.cpp` `Chunk::loadItem`).
    pub fn succeeded_without_item() -> Self {
        Self {
            succeeded: true,
            item: None,
            one_per_chunk: false,
            error: String::new(),
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            succeeded: false,
            item: None,
            one_per_chunk: false,
            error: error.into(),
        }
    }
}

/// `Chunk::loadItem` through the factory shim.
pub trait ItemFactory {
    fn load_item(
        &mut self,
        world: &mut EditorWorld,
        chunk: &str,
        section: &DataSection,
    ) -> FactoryResult;
}

/// The default factory: build an [`EditorItem`] from the section.
pub struct SectionItemFactory;

impl ItemFactory for SectionItemFactory {
    fn load_item(
        &mut self,
        world: &mut EditorWorld,
        chunk: &str,
        section: &DataSection,
    ) -> FactoryResult {
        match EditorItem::from_section(section) {
            Some(mut item) => {
                item.chunk = Some(chunk.to_string());
                let index = world.items.len();
                world.items.push(item);
                FactoryResult::ok(index)
            }
            None => FactoryResult::succeeded_without_item(),
        }
    }
}

// -----------------------------------------------------------------------------
// EditorWorld (the WorldManager/ChunkSpace state the faces share)
// -----------------------------------------------------------------------------

/// The headless editor world: the chunk space, the live items, the selection
/// state and the trace lists the acceptance compares. (`Clone` is gone: the
/// undo machinery owns non-cloneable operation boxes.)
#[derive(Debug)]
pub struct EditorWorld {
    pub space: ChunkSpace,
    pub items: Vec<EditorItem>,
    /// `selectedItems_` (`world_manager.cpp`), in selection order.
    pub selected: Vec<usize>,
    /// `WorldManager::movementSnaps`.
    pub movement_snaps: Vector3,
    /// `WorldManager::snapsEnabled`.
    pub snaps_enabled: bool,
    /// `WorldManager::terrainSnapsEnabled`.
    pub terrain_snaps_enabled: bool,
    /// The chunk locks: a locked chunk is not writeable
    /// (`EditorChunkCache::edIsWriteable`).
    pub locked_chunks: BTreeSet<String>,
    /// `itemsToAdd_` of the deferred selection replacement.
    pub pending_add: Vec<usize>,
    /// `itemsToRemove_` of the deferred selection replacement.
    pub pending_remove: Vec<usize>,
    /// `addUndoBarrier_` (`world_manager.hpp:347`).
    pub add_undo_barrier_flag: bool,
    /// The `WorldEditorDirector.bd` script calls, recorded verbatim.
    pub script_calls: Vec<String>,
    /// The `edDeselected`/`edSelected` callbacks in call order
    /// (`(item, selected?)`); the acceptance's selection-event trace.
    pub selection_events: Vec<(usize, bool)>,
    pub commentary: Vec<String>,
    pub errors: Vec<String>,
    /// `WorldManager::changedChunk`'s set.
    pub changed_chunks: BTreeSet<String>,
    /// The `UndoRedo` engine (`tools/common/undoredo.cpp`, BW-13-03-05).
    pub undoredo: crate::undoredo::UndoRedo,
    /// `UndoRedo::barrier` names in order (every call, skipped ones too).
    pub undo_barriers: Vec<String>,
}

impl EditorWorld {
    pub fn new(space: ChunkSpace) -> Self {
        Self {
            space,
            items: Vec::new(),
            selected: Vec::new(),
            movement_snaps: Vector3::ZERO,
            snaps_enabled: false,
            terrain_snaps_enabled: false,
            locked_chunks: BTreeSet::new(),
            pending_add: Vec::new(),
            pending_remove: Vec::new(),
            add_undo_barrier_flag: false,
            script_calls: Vec::new(),
            selection_events: Vec::new(),
            commentary: Vec::new(),
            errors: Vec::new(),
            changed_chunks: BTreeSet::new(),
            undoredo: crate::undoredo::UndoRedo::new(),
            undo_barriers: Vec::new(),
        }
    }

    /// `EditorChunkCache::edIsWriteable`: a chunk is writeable unless locked.
    pub fn ed_is_writeable(&self, chunk: &str) -> bool {
        !self.locked_chunks.contains(chunk)
    }

    /// `WorldManager::addError`.
    pub fn add_error(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }

    /// `WorldManager::addCommentaryMsg`.
    pub fn add_commentary(&mut self, msg: impl Into<String>) {
        self.commentary.push(msg.into());
    }

    /// `WorldManager::changedChunk`.
    pub fn changed_chunk(&mut self, chunk: &str) {
        self.changed_chunks.insert(chunk.to_string());
    }

    /// `WorldManager::collectOccupiedChunks` (`world_manager.cpp`): a null or
    /// outside resident samples the bounding box on the grid (plus the max
    /// edges the step loops never reach); an inside resident takes the bound
    /// portal neighbours whose bounds intersect plus itself.
    pub fn collect_occupied_chunks(
        &self,
        world_bounds: &BoundBox,
        resident: Option<&str>,
    ) -> Vec<String> {
        let grid = self.space.grid_size();
        let resident_chunk = resident.and_then(|id| self.space.chunk(id));
        let mut out: BTreeSet<String> = BTreeSet::new();
        let outside_face = match resident_chunk {
            None => true,
            Some(chunk) => chunk.is_outside,
        };
        if outside_face {
            let bb = world_bounds;
            let mut x = bb.min.x;
            while x < bb.max.x {
                let mut z = bb.min.z;
                while z < bb.max.z {
                    if let Some(id) = self.find_outside_chunk(x, z) {
                        out.insert(id);
                    }
                    z += grid;
                }
                if let Some(id) = self.find_outside_chunk(x, bb.max.z) {
                    out.insert(id);
                }
                x += grid;
            }
            let mut z = bb.min.z;
            while z < bb.max.z {
                if let Some(id) = self.find_outside_chunk(bb.max.x, z) {
                    out.insert(id);
                }
                z += grid;
            }
            if let Some(id) = self.find_outside_chunk(bb.max.x, bb.max.z) {
                out.insert(id);
            }
        } else {
            let chunk = resident_chunk.unwrap();
            let resident_id = resident.unwrap();
            for &ji in &chunk.joints {
                let Some(boundary) = chunk.boundaries.get(ji) else {
                    continue;
                };
                for portal in &boundary.bound_portals {
                    if let crate::boundary::PortalTarget::Chunk(name) = &portal.target
                        && let Some(other) = self.space.chunk(name)
                        && other.bounding_box.intersects(world_bounds)
                    {
                        out.insert(name.clone());
                    }
                }
            }
            out.insert(resident_id.to_string());
        }
        out.into_iter().collect()
    }

    /// `EditorChunk::findOutsideChunk`: the outside chunk owning the grid
    /// cell, if loaded.
    fn find_outside_chunk(&self, x: f32, z: f32) -> Option<String> {
        let grid_x = self.space.point_to_grid(x);
        let grid_z = self.space.point_to_grid(z);
        self.space
            .find_chunk_by_grid(grid_x, grid_z)
            .map(|c| c.identifier.clone())
    }

    /// `EditorChunkItem::edWorldBounds` (`editor_chunk_item.cpp`): local
    /// bounds through the item transform then the chunk transform; an item
    /// without size answers a point box, an item without chunk stays
    /// inside-out.
    pub fn item_world_bounds(&self, item: usize) -> BoundBox {
        let live = &self.items[item];
        let Some(chunk_id) = &live.chunk else {
            return BoundBox::new(Vector3::new(1.0, 1.0, 1.0), Vector3::new(-1.0, -1.0, -1.0));
        };
        let mut bb = live.bounds;
        if bb.inside_out() {
            bb = BoundBox::new(Vector3::ZERO, Vector3::ZERO);
        }
        bb.transform_by(&live.transform);
        if let Some(chunk) = self.space.chunk(chunk_id) {
            bb.transform_by(&chunk.transform);
        }
        bb
    }

    /// `Chunk::loadItem` through the factory shim.
    pub fn load_item(
        &mut self,
        factory: &mut dyn ItemFactory,
        chunk: &str,
        section: &DataSection,
    ) -> FactoryResult {
        factory.load_item(self, chunk, section)
    }

    // ---- selection (world/world_manager.cpp) ----

    /// `WorldManager::setSelection` (`world_manager.cpp`): the
    /// `WorldEditorDirector.bd.setSelection` script call. The reference
    /// defers the actual diff to the director script; the port applies the
    /// same diff synchronously so the events stay comparable.
    pub fn set_selection(&mut self, items: &[usize], update_selection: bool) {
        debug_assert!(self.pending_add.is_empty() && self.pending_remove.is_empty());
        self.script_calls.push(format!(
            "bd.setSelection({} items, update={})",
            items.len(),
            update_selection
        ));
        self.update_selection(items);
    }

    /// `WorldManager::updateSelection` (`world_manager.cpp:5440-...`): the
    /// diff — drop items not in the new list or already deselected, add the
    /// rest; all `edDeselected` callbacks fire before all `edSelected` ones
    /// (which see the full new selection). The trailing script event is dead
    /// in the reference (the size it guards cannot have changed), so the
    /// port records none.
    pub fn update_selection(&mut self, items: &[usize]) {
        debug_assert!(self.pending_add.is_empty() && self.pending_remove.is_empty());
        let mut removed = Vec::new();
        let mut added = Vec::new();
        let mut i = 0;
        while i < self.selected.len() {
            let item = self.selected[i];
            if !items.contains(&item) || !self.items[item].is_selected {
                self.selected.remove(i);
                removed.push(item);
            } else {
                i += 1;
            }
        }
        for &item in items {
            if !self.selected.contains(&item) {
                self.selected.push(item);
                added.push(item);
            }
        }
        let items_selected = self.selected.len();
        for &item in &removed {
            self.items[item].is_selected = false;
            self.items[item].deselected_calls += 1;
            self.selection_events.push((item, false));
        }
        for &item in &added {
            self.items[item].is_selected = true;
            self.items[item].selected_calls += 1;
            self.selection_events.push((item, true));
        }
        // `if (itemsSelected == selectedItems_.size()) return;` — captured
        // after the diff, nothing between can change it; the script event
        // behind this branch never fires.
        let _ = items_selected;
    }

    /// `ChunkItemPlacer::removeFromSelected` (`chunk_item_placer.cpp`): copy
    /// the selection, erase one, `setSelection(copy, false)`.
    pub fn remove_from_selected(&mut self, item: usize) {
        let mut selection = self.selected.clone();
        if let Some(pos) = selection.iter().position(|&i| i == item) {
            selection.remove(pos);
        }
        self.set_selection(&selection, false);
    }

    /// `WorldManager::hasPendingSelections`.
    pub fn has_pending_selections(&self) -> bool {
        !self.pending_add.is_empty() || !self.pending_remove.is_empty()
    }

    /// `WorldManager::deferReplaceSelection` (`world_manager.cpp:4493-4533`):
    /// a remove already pending as an add cancels out (and vice versa),
    /// otherwise it queues.
    pub fn defer_replace_selection(&mut self, remove: usize, add: usize) {
        debug_assert!(self.selected.contains(&remove) || self.pending_add.contains(&remove));
        debug_assert!(!self.pending_remove.contains(&remove));
        debug_assert!(!self.selected.contains(&add) || self.pending_remove.contains(&add));
        debug_assert!(!self.pending_add.contains(&add));
        match self.pending_add.iter().position(|&i| i == remove) {
            Some(pos) => {
                self.pending_add.remove(pos);
            }
            None => self.pending_remove.push(remove),
        }
        match self.pending_remove.iter().position(|&i| i == add) {
            Some(pos) => {
                self.pending_remove.remove(pos);
            }
            None => self.pending_add.push(add),
        }
    }

    /// `WorldManager::flushSelectionReplacement` (`world_manager.cpp:4535-...`):
    /// final list = selected − pending removals + pending additions, then a
    /// `setSelection` (with the `Move` barrier when
    /// `needsDeferSelectionAddUndoBarrier` asked for one).
    pub fn flush_selection_replacement(&mut self) {
        if self.pending_remove.is_empty() && self.pending_add.is_empty() {
            return;
        }
        let mut items: Vec<usize> = self
            .selected
            .iter()
            .copied()
            .filter(|i| !self.pending_remove.contains(i))
            .collect();
        items.extend(self.pending_add.iter().copied());
        self.pending_remove.clear();
        self.pending_add.clear();
        self.set_selection(&items, true);
        if self.add_undo_barrier_flag
            && let Some(&first) = items.first()
        {
            let description = self.items[first].description.clone();
            self.barrier(format!("GIZMO/UNDO/MOVE_PROPERTY {}", description), false);
        }
    }

    /// `WorldManager::replaceSelection`: defer + flush immediately (the
    /// `ScopedDeferSelectionReplacement` counter is zero outside a guard).
    pub fn replace_selection(&mut self, remove: usize, add: usize) {
        self.defer_replace_selection(remove, add);
        self.flush_selection_replacement();
    }

    /// `WorldManager::needsDeferSelectionAddUndoBarrier`
    /// (`world_manager.hpp:347`): stash the flag for the flush.
    pub fn needs_defer_selection_add_undo_barrier(&mut self, add_undo_barrier: bool) {
        self.add_undo_barrier_flag = add_undo_barrier;
    }

    // ---- the transform face (EditorChunkModel/EditorChunkEntity::edTransform) ----

    /// `EditorChunkItem::edDropChunk` (`editor_chunk_item.cpp:187-210`) plus
    /// the permanent branch of `EditorChunkModel::edTransform`
    /// (`editor_chunk_model.cpp:1010-1051`): find the owning chunk from the
    /// drop point, refuse when it is not loaded or either chunk is locked,
    /// and store the pose re-expressed into the new chunk's space.
    pub fn item_ed_transform(&mut self, item: usize, m: &Matrix, transient: bool) -> bool {
        let Some(old_chunk_id) = self.items[item].chunk.clone() else {
            // `ERROR_MSG( "%s has not been added to a chunk!\n", ... )`
            let description = self.items[item].description.clone();
            self.add_error(format!("{} has not been added to a chunk!", description));
            return false;
        };
        let drop = if self.items[item].drop_at_bounds_centre {
            let bb = self.items[item].bounds;
            m.apply_point(&((bb.min + bb.max) * 0.5))
        } else {
            m.apply_to_origin()
        };
        let old_transform = self
            .space
            .chunk(&old_chunk_id)
            .map(|c| c.transform)
            .unwrap_or_else(Matrix::identity);
        let world_drop = old_transform.apply_point(&drop);
        let Some(new_chunk_id) = self
            .space
            .find_chunk_from_point_exact(&world_drop)
            .map(|c| c.identifier.clone())
        else {
            let description = self.items[item].description.clone();
            self.add_error(format!(
                "Cannot move {} to ({},{},{}) because it is not in any loaded chunk!",
                description, world_drop.x, world_drop.y, world_drop.z
            ));
            return false;
        };
        let new_transform = self.space.chunk(&new_chunk_id).unwrap().transform;

        // for transient transforms there are no more checks, update the
        // transform (and stay in the old chunk) and return
        if transient {
            self.items[item].transform = *m;
            return true;
        }

        // make sure the chunks aren't readonly
        if !self.ed_is_writeable(&old_chunk_id) || !self.ed_is_writeable(&new_chunk_id) {
            return false;
        }

        let mut chunk_inverse = new_transform;
        chunk_inverse.invert();
        let mut t = Matrix::identity();
        t.multiply(m, &old_transform);
        t.post_multiply(&chunk_inverse);
        self.items[item].transform = t;
        if new_chunk_id != old_chunk_id {
            // `edMove`: the item changes chunk membership
            self.items[item].chunk = Some(new_chunk_id);
        }
        true
    }

    // ---- ChunkItemPlacer::createChunkItem ----

    /// `ChunkItemPlacer::createChunkItem` (`chunk_item_placer.cpp:1277-1547`):
    /// create an item from a section, place it at the locator pose (snapped,
    /// optionally ground-dropped and camera-offset), hand it to the selection
    /// and record the creation undo. The too-large model branch converts to
    /// a proxy VLO ref section and retries, exactly like the reference.
    pub fn create_chunk_item(
        &mut self,
        factory: &mut dyn ItemFactory,
        section: &DataSection,
        pose: &Matrix,
        options: &mut PlacementOptions,
    ) -> CreateOutcome {
        let snaps = self.movement_snaps;
        let zero = Vector3::ZERO;

        // Don't place when in terrain or object snap mode
        if !options.position_valid && options.item_snap_mode != 0 {
            return CreateOutcome::Error(
                "WorldEditor.createChunkItem() invalid locking mode for camera relative placing, must be Free"
                    .to_string(),
            );
        }

        let mut pose = *pose;
        let mut wpos = pose.apply_to_origin();
        if self.snaps_enabled {
            snaps::vector3(&mut wpos, &snaps, &zero);
        }
        if self.terrain_snaps_enabled
            && let Some(to_ground) = options.to_ground
        {
            wpos = to_ground(wpos);
        }
        let Some(chunk_id) = self
            .space
            .find_chunk_from_point_exact(&wpos)
            .map(|c| c.identifier.clone())
        else {
            return CreateOutcome::Error(format!(
                "WorldEditor.createChunkItem() cannot find chunk at point ({},{},{})",
                wpos.x, wpos.y, wpos.z
            ));
        };
        if !self.ed_is_writeable(&chunk_id) {
            self.add_commentary(
                "WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CANNOT_ADD_ASSET_ON_NON_LOCKED_CHUNK",
            );
            return CreateOutcome::NotWriteable;
        }

        // ok, simply create the chunk item from the data section then
        let mut result = self.load_item(factory, &chunk_id, section);
        if result.succeeded
            && result.one_per_chunk
            && let Some(first) = result.item
        {
            // ERROR_MSG + an existence undo for the replaced item, then retry
            let error = result.error.clone();
            self.add_error(error);
            self.undo_add(Box::new(crate::undoredo::ExistenceOperation {
                item: first,
                old_chunk: Some(chunk_id.clone()),
            }));
            self.remove_from_selected(first);
            result = self.load_item(factory, &chunk_id, section);
        }
        if !result.succeeded {
            self.add_error("WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CANNOT_ADD_ASSET");
            return CreateOutcome::Error(
                "WorldEditor.createChunkItem() error creating item from given section".to_string(),
            );
        }
        let Some(item) = result.item else {
            self.add_error("WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CANNOT_ADD_ASSET");
            return CreateOutcome::Error(
                "WorldEditor.createChunkItem() Couldn't create Chunk Item".to_string(),
            );
        };

        // call the main thread load
        self.items[item].chunk_binds += 1;
        self.changed_chunk(&chunk_id);

        let bb = self.items[item].bounds;
        let dim = bb.max - bb.min;
        if !options.position_valid {
            let dir = options.camera_direction * ITEM_CAMERA_OFFSET;
            let trans = Vector3::new(dir.x * dim.x, dir.y * dim.y, dir.z * dim.z);
            pose.post_translate_by(&trans);
        }

        // now move it to the matrix of the locator, if desired
        if options.use_loc_pos != 0 {
            let chunk_transform = self.space.chunk(&chunk_id).unwrap().transform;
            let mut chunk_inverse = chunk_transform;
            chunk_inverse.invert();
            let mut local_pose = Matrix::identity();
            local_pose.multiply(&pose, &chunk_inverse);
            let mut new_pose;
            if options.use_loc_pos == 2 {
                new_pose = local_pose;
            } else {
                new_pose = self.items[item].transform;
                let t = local_pose.apply_to_origin();
                new_pose.translation(&t);
            }

            if self.snaps_enabled {
                let mut t = new_pose.apply_to_origin();
                snaps::vector3(&mut t, &snaps, &zero);
                new_pose.translation(&t);
            }
            if self.terrain_snaps_enabled
                && let Some(to_ground) = options.to_ground
            {
                let mut world_pos = chunk_transform.apply_point(&new_pose.apply_to_origin());
                world_pos = to_ground(world_pos);
                let mut inverse = chunk_transform;
                inverse.invert();
                let t = inverse.apply_point(&world_pos);
                new_pose.translation(&t);
            }

            let section_name = section.section_name().to_string();

            // check for random rotation placement
            if (section_name == "model" || section_name == "speedtree")
                && !options.presets.default_preset_current()
            {
                let mut work = Matrix::identity();
                let rx = random_degrees(
                    options.rng,
                    options.presets.rotation(0, false),
                    options.presets.rotation(0, true),
                );
                let ry = random_degrees(
                    options.rng,
                    options.presets.rotation(1, false),
                    options.presets.rotation(1, true),
                );
                let rz = random_degrees(
                    options.rng,
                    options.presets.rotation(2, false),
                    options.presets.rotation(2, true),
                );
                work.set_rotate_ypr(
                    snaps::deg_to_rad(rx),
                    snaps::deg_to_rad(ry),
                    snaps::deg_to_rad(rz),
                );
                new_pose.pre_multiply(&work);
            }

            // scale speedtree placement to convert feet into metres
            if section_name == "speedtree" {
                let mut work = Matrix::identity();
                work.set_scale(
                    PLACER_FEET_TO_METRES,
                    PLACER_FEET_TO_METRES,
                    PLACER_FEET_TO_METRES,
                );
                new_pose.pre_multiply(&work);
            }

            // check for random scale placement
            if section_name == "model" || section_name == "speedtree" {
                let mut largest_edge = dim.x.max(dim.z);
                if largest_edge < 0.01 {
                    largest_edge = 0.01;
                }
                let largest_scale = 99.99 / largest_edge;
                let scale_x = rand_in_range(
                    options.rng,
                    largest_scale.min(options.presets.scale(0, false)),
                    largest_scale.min(options.presets.scale(0, true)),
                );
                let mut scale_y = rand_in_range(
                    options.rng,
                    largest_scale.min(options.presets.scale(1, false)),
                    largest_scale.min(options.presets.scale(1, true)),
                );
                let mut scale_z = rand_in_range(
                    options.rng,
                    largest_scale.min(options.presets.scale(2, false)),
                    largest_scale.min(options.presets.scale(2, true)),
                );
                if largest_edge > 99.99 {
                    // tell it it's going to be deleted, then delete it now
                    self.items[item].pre_deletes += 1;
                    self.items[item].chunk = None;
                    self.changed_chunk(&chunk_id);
                    // convert to a proxy VLO ref and start over
                    let uid = generate_uid(options.rng);
                    let vlo_section = proxy_vlo_ref_section(section, &uid);
                    return self.create_chunk_item(factory, &vlo_section, &pose, options);
                }
                if options.presets.scale_uniform() {
                    scale_y = scale_x;
                    scale_z = scale_x;
                }
                let mut work = Matrix::identity();
                work.set_scale(scale_x, scale_y, scale_z);
                new_pose.pre_multiply(&work);
            }

            self.items[item].transform = new_pose;
            if !self.item_ed_transform(item, &new_pose, false) {
                // couldn't move it there, so throw it away then
                self.items[item].chunk = None;
                return CreateOutcome::Error(
                    "WorldEditor.createChunkItem() could not move item to desired transform"
                        .to_string(),
                );
            }
        }

        // tell the item it was just created
        self.items[item].post_creates += 1;

        let world_bb = self.item_world_bounds(item);
        let resident = self.items[item].chunk.clone();
        for chunk in self.collect_occupied_chunks(&world_bb, resident.as_deref()) {
            self.changed_chunk(&chunk);
        }

        let min_edge = dim.x.min(dim.y).min(dim.z);
        if min_edge < 0.001 {
            self.add_error(
                "WorldEditor.createChunkItem() the item is too small in at least one axis, please check its bounding box",
            );
            self.add_error("WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/ITEM_TOO_SMALL");
            if self.items[item].is_shell_model {
                // ChunkPlacer::deleteChunk( pChunk )
                self.space.remove_chunk(&chunk_id);
            } else {
                // see if it wants to be deleted
                if self.items[item].can_delete {
                    // tell it it's going to be deleted
                    self.items[item].pre_deletes += 1;
                    // delete it now
                    self.items[item].chunk = None;
                    self.changed_chunk(&chunk_id);
                }
            }
            return CreateOutcome::Error(
                "WorldEditor.createChunkItem() the item is too small in at least one axis, please check its bounding box"
                    .to_string(),
            );
        }

        // ok, everyone's happy then. so add an undo which deletes it
        self.undo_add(Box::new(crate::undoredo::ExistenceOperation {
            item,
            old_chunk: None,
        }));

        self.set_selection(&[item], true);
        let description = self.items[item].description.clone();
        self.barrier(
            format!(
                "WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CREATE_ASSET:{}",
                description
            ),
            false,
        );

        // and that's it then. return a group that contains it
        CreateOutcome::Placed { item }
    }
}

// -----------------------------------------------------------------------------
// PlacementOptions and the outcome
// -----------------------------------------------------------------------------

/// The per-call placement inputs (`py_createChunkItem`'s locator/options and
/// the WorldManager snap flags not already on the world).
pub struct PlacementOptions<'r> {
    /// `useLocPos`: 0 keeps the item pose, 1 takes the locator translation,
    /// 2 takes the full locator pose in chunk space.
    pub use_loc_pos: i32,
    /// `ToolLocator::positionValid` — false means camera-relative placement.
    pub position_valid: bool,
    /// `ToolLocator::direction` (used when the position is not valid).
    pub camera_direction: Vector3,
    /// `Options::getOptionInt("snaps/itemSnapMode", 0)`.
    pub item_snap_mode: i32,
    /// `PlacementPresets::instance()`.
    pub presets: PlacementPresets,
    /// `Snap::toGround` — the collision ray is injected; it runs when
    /// `terrain_snaps_enabled`.
    pub to_ground: Option<&'r dyn Fn(Vector3) -> Vector3>,
    /// `bw_random`.
    pub rng: &'r mut dyn FnMut() -> u32,
}

/// The `createChunkItem` outcomes: a placed group, the `Py_RETURN_NONE`
/// not-writeable face, or the `PyErr_SetString` failure with its exact text.
#[derive(Debug, Clone, PartialEq)]
pub enum CreateOutcome {
    Placed { item: usize },
    NotWriteable,
    Error(String),
}

/// `EditorChunkModelVLORef::createDataSection`
/// (`editor_chunk_model_vlo_ref.cpp`): a `proxyvlo` section declaring the
/// large-object type and uid, wrapping a `proxy/model` copy of the original.
fn proxy_vlo_ref_section(model: &DataSection, uid: &str) -> DataSection {
    let mut xml = DataSection::new("proxyvlo");
    xml.write_string("type", "proxy");
    xml.write_string("uid", uid);
    let model_copy = {
        let proxy = xml.add_child("proxy");
        proxy.add_child("model")
    };
    model_copy.copy_from(model);
    xml
}

// -----------------------------------------------------------------------------
// ChunkItemMatrix (editor/item_properties.cpp)
// -----------------------------------------------------------------------------

/// `ChunkItemMatrix` (`item_properties.cpp:177-464`): the matrix proxy the
/// transform gizmos drive on a selected item. Record/commit is two-phase:
/// the drag transients run through [`Self::set_matrix`], the release runs
/// [`Self::commit_state`].
pub struct ChunkItemMatrix {
    pub item: usize,
    orig_chunk: Option<String>,
    orig_pose: Matrix,
    movement_snaps: Vector3,
    have_recorded: bool,
}

impl ChunkItemMatrix {
    /// The ctor (`item_properties.cpp:177-190`): the item's own snap vector
    /// is captured once (`edMovementDeltaSnaps`); `lastGoodPose_` starts
    /// zeroed (the speed-tree clamp it feeds is a documented divergence).
    pub fn new(item: usize, world: &EditorWorld) -> Self {
        Self {
            item,
            orig_chunk: world.items[item].chunk.clone(),
            orig_pose: world.items[item].transform,
            movement_snaps: world.items[item].movement_delta_snaps,
            have_recorded: false,
        }
    }

    /// `getMatrix` (`item_properties.cpp:199-208`): the chunk-local pose,
    /// post-multiplied by the chunk transform when `world_space`.
    pub fn get_matrix(&self, world: &EditorWorld, world_space: bool) -> Matrix {
        let mut m = world.items[self.item].transform;
        if world_space
            && let Some(chunk_id) = world.items[self.item].chunk.clone()
            && let Some(chunk) = world.space.chunk(&chunk_id)
        {
            m.post_multiply(&chunk.transform);
        }
        m
    }

    /// `setMatrix` (`item_properties.cpp:238-289`): a moved translation is
    /// snapped — the item snaps when it has some, else the world snaps when
    /// enabled — then the transform is always applied transiently.
    pub fn set_matrix(&mut self, world: &mut EditorWorld, m: &Matrix) -> bool {
        let mut new_transform = *m;
        let current = self.get_matrix(world, false);
        if !snaps::almost_equal(&current.apply_to_origin(), &new_transform.apply_to_origin()) {
            let mut t = new_transform.apply_to_origin();
            let mut s = self.movement_snaps;
            if s == Vector3::ZERO && world.snaps_enabled {
                s = world.movement_snaps;
            }
            snaps::vector3(&mut t, &s, &Vector3::ZERO);
            new_transform.translation(&t);
        }
        world.item_ed_transform(self.item, &new_transform, true)
    }

    /// `recordState` (`item_properties.cpp:291-299`).
    pub fn record_state(&mut self, world: &EditorWorld) {
        self.orig_chunk = world.items[self.item].chunk.clone();
        self.orig_pose = world.items[self.item].transform;
        self.have_recorded = true;
    }

    /// `commitState` (`item_properties.cpp:301-457`): set back to the
    /// recorded pose, notify the occupied chunks, re-apply the destination
    /// permanently (reverting when the move is refused), notify again, push
    /// the undo op, `edPostModify`, and raise the `Move` barrier unless a
    /// selection replacement is pending. The reference's speed-tree clamp
    /// and VLO conversion need the supermodel face; the port records the
    /// plain move.
    pub fn commit_state(
        &mut self,
        world: &mut EditorWorld,
        revert_to_record: bool,
        add_undo_barrier: bool,
    ) -> bool {
        if !self.have_recorded {
            self.record_state(world);
        }
        let dest_pose = world.items[self.item].transform;

        // set it back so it starts from the old spot
        world.item_ed_transform(self.item, &self.orig_pose, true);
        if revert_to_record {
            return true;
        }

        // notify to update chunks before move
        let old_bb = world.item_world_bounds(self.item);
        let resident = world.items[self.item].chunk.clone();
        for chunk in world.collect_occupied_chunks(&old_bb, resident.as_deref()) {
            world.changed_chunk(&chunk);
        }

        // attempt to set the matrix permanently
        let ok_to_commit = world.item_ed_transform(self.item, &dest_pose, false);
        if !ok_to_commit {
            // set it back if that failed
            world.item_ed_transform(self.item, &self.orig_pose, true);
        }

        // notify to update chunks after move
        let new_bb = world.item_world_bounds(self.item);
        let resident = world.items[self.item].chunk.clone();
        for chunk in world.collect_occupied_chunks(&new_bb, resident.as_deref()) {
            world.changed_chunk(&chunk);
        }

        // add the undo operation for it
        world.undo_add(Box::new(crate::undoredo::MatrixOperation {
            item: self.item,
            old_chunk: self.orig_chunk.clone(),
            old_pose: self.orig_pose,
            replaced: None,
        }));
        world.items[self.item].post_modifies += 1;

        world.needs_defer_selection_add_undo_barrier(add_undo_barrier);
        if add_undo_barrier && !world.has_pending_selections() {
            // set the barrier with a meaningful name
            let description = world.items[self.item].description.clone();
            world.barrier(format!("GIZMO/UNDO/MOVE_PROPERTY {}", description), false);
        }
        ok_to_commit
    }

    /// `hasChanged` (`item_properties.cpp:459-464`): chunk or pose moved
    /// (the reference compares with the default `almostEqual`).
    pub fn has_changed(&self, world: &EditorWorld) -> bool {
        self.orig_chunk != world.items[self.item].chunk
            || !self
                .orig_pose
                .almost(&world.items[self.item].transform, 0.0004)
    }
}

// -----------------------------------------------------------------------------
// AccessorDataProxy (editor/item_properties.hpp:77-136)
// -----------------------------------------------------------------------------

/// `AccessorDataProxy`: an undoable get/set pair on one item. `setPermanent`
/// flags the chunk changed, calls `edPostModify` and re-saves the item's own
/// section — the property face the acceptance compares. The C++ template
/// collapses to one float property with injected accessors.
/// The injected property accessors (`AccessorDataProxy`'s C++ templates).
pub type PropertyGetter = dyn Fn(&EditorWorld, usize) -> f32;
/// The injected property setter; `false` refuses the value.
pub type PropertySetter = dyn Fn(&mut EditorWorld, usize, f32) -> bool;

pub struct AccessorDataProxy {
    pub item: usize,
    pub prop_desc: String,
    get: Box<PropertyGetter>,
    set: Box<PropertySetter>,
}

impl AccessorDataProxy {
    pub fn new(
        item: usize,
        prop_desc: &str,
        get: Box<PropertyGetter>,
        set: Box<PropertySetter>,
    ) -> Self {
        Self {
            item,
            prop_desc: prop_desc.to_string(),
            get,
            set,
        }
    }

    /// `get` (`item_properties.hpp:93-99`).
    pub fn get(&self, world: &EditorWorld) -> f32 {
        (self.get)(world, self.item)
    }

    /// `setTransient` (`item_properties.hpp:100-105`): no bookkeeping.
    pub fn set_transient(&self, world: &mut EditorWorld, v: f32) {
        (self.set)(world, self.item, v);
    }

    /// `setPermanent` (`item_properties.hpp:107-122`): a failing setter is
    /// refused; otherwise changedChunk + `edPostModify` + `edSave(pOwnSect)`.
    pub fn set_permanent(&self, world: &mut EditorWorld, v: f32) -> bool {
        if !(self.set)(world, self.item, v) {
            return false;
        }
        if let Some(chunk) = world.items[self.item].chunk.clone() {
            world.changed_chunk(&chunk);
        }
        world.items[self.item].post_modifies += 1;
        world.items[self.item].saves += 1;
        true
    }

    /// `opName` (`item_properties.hpp:124-129`).
    pub fn op_name(&self, world: &EditorWorld) -> String {
        format!(
            "Set {} {}",
            world.items[self.item].description, self.prop_desc
        )
    }
}

/// `ChunkItemPositionProperty::length` (`item_properties.cpp:466-477`): the
/// gizmo length from the bounding-box diagonal, clamped to 10..200; an
/// inside-out box answers zero.
pub fn chunk_item_position_property_length(world: &EditorWorld, item: usize) -> f32 {
    let bb = world.items[item].bounds;
    if bb.inside_out() {
        return 0.0;
    }
    let length = (bb.max - bb.min).length() * 10.0;
    clamp(10.0, length, 200.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::{ChunkBoundary, Portal, PortalTarget};
    use crate::chunk::Chunk;
    use crate::mapping::{GeometryMapping, GridBounds};
    use crate::undoredo::{ExistenceOperation, MatrixOperation};
    use foundation::math::PlaneEq;

    /// The most recently closed undo barrier (`undo_list` back minus the
    /// always-open seed/next entry).
    fn closed_barrier(world: &EditorWorld) -> &crate::undoredo::Barrier {
        &world.undoredo.undo_list[world.undoredo.undo_list.len() - 2]
    }

    fn chunk_portal(target: &str) -> Portal {
        Portal {
            internal: false,
            permissive: true,
            target: PortalTarget::Chunk(target.to_string()),
            label: String::new(),
            points: Vec::new(),
            u_axis: Vector3::new(1.0, 0.0, 0.0),
            v_axis: Vector3::new(0.0, 0.0, 1.0),
            origin: Vector3::ZERO,
            lcentre: Vector3::ZERO,
            centre: Vector3::ZERO,
            plane: PlaneEq::default(),
        }
    }

    /// A 2×2 grid of 100-unit outside chunks, all loaded.
    fn grid_world() -> EditorWorld {
        let mapping = GeometryMapping::new(
            "space/",
            100.0,
            GridBounds {
                min_l_grid_x: 0,
                min_l_grid_y: 0,
                max_l_grid_x: 1,
                max_l_grid_y: 1,
                min_grid_x: 0,
                min_grid_y: 0,
                max_grid_x: 1,
                max_grid_y: 1,
            },
            false,
        );
        let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
        for x in 0..2 {
            for z in 0..2 {
                let id = mapping.outside_chunk_identifier(x, z, false);
                let mut chunk = Chunk::new(&id, &mapping, &Matrix::identity());
                chunk.is_loaded = true;
                space.add_chunk(chunk);
            }
        }
        // The editor space a placer runs against is ready: every chunk is
        // bound and focused (the reference's ChunkManager did that as the
        // camera moved), so point lookups resolve through the focus grid.
        for x in 0..2 {
            for z in 0..2 {
                let id = mapping.outside_chunk_identifier(x, z, false);
                assert!(space.bind(&id), "bind {id}");
            }
        }
        space.focus();
        EditorWorld::new(space)
    }

    fn model_section(resource: &str) -> DataSection {
        let mut section = DataSection::new("model");
        section.write_string("resource", resource);
        section
    }

    fn options(use_loc_pos: i32, rng: &mut dyn FnMut() -> u32) -> PlacementOptions<'_> {
        PlacementOptions {
            use_loc_pos,
            position_valid: true,
            camera_direction: Vector3::ZERO,
            item_snap_mode: 0,
            presets: PlacementPresets::normal(),
            to_ground: None,
            rng,
        }
    }

    fn item_transform(world: &EditorWorld, item: usize) -> Matrix {
        world.items[item].transform
    }

    #[test]
    fn find_chunk_from_point_exact_walks_the_grid() {
        let world = grid_world();
        let id = world
            .space
            .find_chunk_from_point_exact(&Vector3::new(137.0, 0.0, 84.0))
            .map(|c| c.identifier.clone());
        assert_eq!(id.as_deref(), Some("00010000o"));
        // the +y fudge keeps points sitting exactly on the chunk face working
        let id = world
            .space
            .find_chunk_from_point_exact(&Vector3::new(100.0, 0.0, 0.0))
            .map(|c| c.identifier.clone());
        assert_eq!(id.as_deref(), Some("00010000o"));
        // outside every mapping bound
        assert!(
            world
                .space
                .find_chunk_from_point_exact(&Vector3::new(500.0, 0.0, 500.0))
                .is_none()
        );
        // a not-completed chunk is not a valid target (`completed()`):
        // smudging drops the flag until the next focus pass
        let mut unloaded = grid_world();
        assert!(unloaded.space.smudge_chunk("00000000o"));
        assert!(
            unloaded
                .space
                .find_chunk_from_point_exact(&Vector3::new(1.0, 0.0, 1.0))
                .is_none()
        );
    }

    #[test]
    fn an_inside_chunk_displaces_the_outside_column() {
        let mut world = grid_world();
        // an inside chunk at the world origin with a small box: the column's
        // outside chunk keeps the point when no inside box contains it
        let mapping = world.space.mappings[0].clone();
        let mut inner = Chunk::new("00000000i", &mapping, &Matrix::identity());
        // the box planes below are world-space, so the shell keeps the
        // identity transform (the focus pass transforms boundary planes by
        // it when building the hull border)
        inner.bounding_box = BoundBox::new(
            Vector3::new(40.0, -5.0, 40.0),
            Vector3::new(60.0, 15.0, 60.0),
        );
        inner.is_loaded = true;
        // a loaded shell carries its inward-facing hull border (`bounds`
        // index the boundaries the hull tree is built from)
        inner.boundaries = [
            (Vector3::new(1.0, 0.0, 0.0), 40.0),
            (Vector3::new(-1.0, 0.0, 0.0), -60.0),
            (Vector3::new(0.0, 1.0, 0.0), -5.0),
            (Vector3::new(0.0, -1.0, 0.0), -15.0),
            (Vector3::new(0.0, 0.0, 1.0), 40.0),
            (Vector3::new(0.0, 0.0, -1.0), -60.0),
        ]
        .iter()
        .map(|&(normal, d)| crate::boundary::ChunkBoundary {
            plane: PlaneEq::new(normal, d),
            bound_portals: Vec::new(),
            unbound_portals: Vec::new(),
        })
        .collect();
        inner.bounds = vec![0, 1, 2, 3, 4, 5];
        world.space.add_chunk(inner);
        // the ready-space contract: bind and focus the new chunk so the
        // column at the point holds both chunks
        assert!(world.space.bind("00000000i"));
        world.space.focus();
        let id = world
            .space
            .find_chunk_from_point_exact(&Vector3::new(50.0, 5.0, 50.0))
            .map(|c| c.identifier.clone());
        assert_eq!(id.as_deref(), Some("00000000i"));
        let id = world
            .space
            .find_chunk_from_point_exact(&Vector3::new(10.0, 5.0, 10.0))
            .map(|c| c.identifier.clone());
        assert_eq!(id.as_deref(), Some("00000000o"));
    }

    #[test]
    fn placement_lands_on_the_snapped_world_cell() {
        let mut world = grid_world();
        world.snaps_enabled = true;
        world.movement_snaps = Vector3::new(10.0, 0.0, 10.0);
        let mut pose = Matrix::identity();
        pose.set_translate(137.0, 0.0, 84.0);
        let section = model_section("abc.model");
        let outcome = {
            let mut rng = || 1u32;
            let mut opts = options(1, &mut rng);
            world.create_chunk_item(&mut SectionItemFactory, &section, &pose, &mut opts)
        };
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        assert_eq!(world.items[item].chunk.as_deref(), Some("00010000o"));
        // world (137,0,84) snaps to (140,0,80); the chunk-local pose carries
        // the chunk origin subtracted
        let t = item_transform(&world, item).apply_to_origin();
        assert_eq!(t, Vector3::new(40.0, 0.0, 80.0));
        // and the world coordinates through the chunk transform match
        let chunk = world.space.chunk("00010000o").unwrap();
        let world_pos = chunk.transform.apply_point(&t);
        assert_eq!(world_pos, Vector3::new(140.0, 0.0, 80.0));
        // the creation trace: bind, changed chunk, post-create, undo, selection
        assert_eq!(world.items[item].chunk_binds, 1);
        assert_eq!(world.items[item].post_creates, 1);
        assert!(world.changed_chunks.contains("00010000o"));
        // the creation trace ends in one closed barrier holding one
        // existence undo (plus the open seed/new entry)
        assert_eq!(world.undoredo.undo_list.len(), 2);
        let op = closed_barrier(&world).ops[0]
            .as_any()
            .downcast_ref::<ExistenceOperation>()
            .unwrap();
        assert_eq!(op.item, item);
        assert_eq!(op.old_chunk, None);
        assert_eq!(world.selected, vec![item]);
    }

    #[test]
    fn use_loc_pos_two_takes_the_full_local_pose() {
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        pose.pre_rotate_y(snaps::deg_to_rad(90.0));
        let section = model_section("abc.model");
        let outcome = {
            let mut rng = || 0u32;
            let mut opts = options(2, &mut rng);
            world.create_chunk_item(&mut SectionItemFactory, &section, &pose, &mut opts)
        };
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        let mut expected = Matrix::identity();
        let mut chunk_inverse = world.space.chunk("00000000o").unwrap().transform;
        chunk_inverse.invert();
        expected.multiply(&pose, &chunk_inverse);
        let got = item_transform(&world, item);
        assert!(got.almost(&expected, 1e-6), "full local pose expected");
    }

    #[test]
    fn placement_errors_carry_the_reference_texts() {
        let mut rng = || 0u32;
        // out of every mapping
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(500.0, 0.0, 500.0);
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() cannot find chunk at point (500,0,500)".to_string()
            )
        );

        // camera-relative placement with a locking mode is refused
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(1.0, 0.0, 1.0);
        let mut opts = options(1, &mut rng);
        opts.position_valid = false;
        opts.item_snap_mode = 2;
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() invalid locking mode for camera relative placing, must be Free"
                    .to_string()
            )
        );

        // a locked chunk answers Py_RETURN_NONE plus the commentary key
        let mut world = grid_world();
        world.locked_chunks.insert("00000000o".to_string());
        let mut pose = Matrix::identity();
        pose.set_translate(1.0, 0.0, 1.0);
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        assert_eq!(outcome, CreateOutcome::NotWriteable);
        assert_eq!(
            world.commentary,
            vec![
                "WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CANNOT_ADD_ASSET_ON_NON_LOCKED_CHUNK"
            ]
        );

        // an unknown section name is ignored by the factory and surfaces as
        // "Couldn't create Chunk Item"
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(1.0, 0.0, 1.0);
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &DataSection::new("shell"),
            &pose,
            &mut opts,
        );
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() Couldn't create Chunk Item".to_string()
            )
        );
        assert_eq!(
            world.errors,
            vec!["WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CANNOT_ADD_ASSET"]
        );
    }

    /// A factory that refuses to create, for the failed-load face.
    struct FailingFactory;
    impl ItemFactory for FailingFactory {
        fn load_item(
            &mut self,
            _world: &mut EditorWorld,
            _chunk: &str,
            _section: &DataSection,
        ) -> FactoryResult {
            FactoryResult::failed("bad section")
        }
    }

    #[test]
    fn a_failed_factory_answers_the_section_error() {
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(1.0, 0.0, 1.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut FailingFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() error creating item from given section".to_string()
            )
        );
    }

    /// The ambient-light factory face: one-per-chunk items hand back the
    /// existing item first (`editor_chunk_light.cpp:1846-1856`).
    struct ReplacingFactory {
        existing: Option<usize>,
    }
    impl ItemFactory for ReplacingFactory {
        fn load_item(
            &mut self,
            world: &mut EditorWorld,
            chunk: &str,
            section: &DataSection,
        ) -> FactoryResult {
            if let Some(existing) = self.existing.take() {
                // the reference factory removes the old one from its chunk
                world.items[existing].chunk = None;
                return FactoryResult {
                    succeeded: true,
                    item: Some(existing),
                    one_per_chunk: true,
                    error: "The old ambient light was removed".to_string(),
                };
            }
            SectionItemFactory.load_item(world, chunk, section)
        }
    }

    #[test]
    fn one_per_chunk_replacement_records_the_replaced_item() {
        let mut world = grid_world();
        // the pre-existing ambient light lives in the chunk already
        let existing_section = model_section("old.model");
        let existing = world
            .load_item(&mut SectionItemFactory, "00000000o", &existing_section)
            .item
            .unwrap();
        world.selected.push(existing);
        world.items[existing].is_selected = true;

        let mut pose = Matrix::identity();
        pose.set_translate(1.0, 0.0, 1.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let mut factory = ReplacingFactory {
            existing: Some(existing),
        };
        let outcome =
            world.create_chunk_item(&mut factory, &model_section("new.model"), &pose, &mut opts);
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        assert_ne!(item, existing);
        // the replacement pushed an existence undo for the replaced item and
        // pulled it out of the selection first
        let op = closed_barrier(&world).ops[0]
            .as_any()
            .downcast_ref::<ExistenceOperation>()
            .unwrap();
        assert_eq!(op.item, existing);
        assert_eq!(op.old_chunk, Some("00000000o".to_string()));
        assert_eq!(world.errors[0], "The old ambient light was removed");
        assert!(
            world
                .script_calls
                .contains(&"bd.setSelection(0 items, update=false)".to_string())
        );
        assert_eq!(world.selected, vec![item]);
    }

    #[test]
    fn one_per_chunk_without_an_item_skips_the_replacement() {
        // the reference dereferences result.item() unconditionally; the port
        // guards and simply proceeds to the "Couldn't create" error
        struct NullReplacingFactory;
        impl ItemFactory for NullReplacingFactory {
            fn load_item(
                &mut self,
                _world: &mut EditorWorld,
                _chunk: &str,
                _section: &DataSection,
            ) -> FactoryResult {
                FactoryResult {
                    succeeded: true,
                    item: None,
                    one_per_chunk: true,
                    error: "gone".to_string(),
                }
            }
        }
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(1.0, 0.0, 1.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut NullReplacingFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() Couldn't create Chunk Item".to_string()
            )
        );
        assert!(!world.undoredo.can_undo());
    }

    #[test]
    fn camera_relative_placement_offsets_by_the_item_box() {
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let section = model_section("abc.model");
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        opts.position_valid = false;
        opts.camera_direction = Vector3::new(1.0, 1.0, 0.0);
        // the item's default box is unit: dir * ITEM_CAMERA_OFFSET scaled by
        // the box extents lands (2, 2, 0) further out
        let outcome = world.create_chunk_item(&mut SectionItemFactory, &section, &pose, &mut opts);
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        let t = item_transform(&world, item).apply_to_origin();
        assert_eq!(t, Vector3::new(32.0, 2.0, 30.0));
    }

    #[test]
    fn terrain_snap_drops_the_placement_onto_the_ground() {
        let mut world = grid_world();
        world.terrain_snaps_enabled = true;
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 55.0, 30.0);
        let section = model_section("abc.model");
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        opts.to_ground = Some(&|v: Vector3| Vector3::new(v.x, 12.5, v.z));
        let outcome = world.create_chunk_item(&mut SectionItemFactory, &section, &pose, &mut opts);
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        let t = item_transform(&world, item).apply_to_origin();
        assert_eq!(t, Vector3::new(30.0, 12.5, 30.0));
    }

    #[test]
    fn random_presets_rotate_and_scale_the_pose() {
        let mut section = DataSection::new("presets");
        let preset = section.add_child("row");
        preset.write_string("name", "scatter");
        preset.write_float("minRotX", 0.0);
        preset.write_float("maxRotX", 90.0);
        preset.write_float("minRotY", 0.0);
        preset.write_float("maxRotY", 90.0);
        preset.write_float("minRotZ", 90.0);
        preset.write_float("maxRotZ", 0.0);
        preset.write_float("minScaX", 2.0);
        preset.write_float("maxScaX", 2.0);
        preset.write_float("minScaY", 3.0);
        preset.write_float("maxScaY", 3.0);
        preset.write_float("minScaZ", 4.0);
        preset.write_float("maxScaZ", 4.0);
        preset.write_bool("uniformScale", false);
        let presets = PlacementPresets::from_section("scatter", &section);
        assert!(!presets.default_preset_current());
        assert!(!presets.scale_uniform());
        assert_eq!(presets.rotation(1, true), 90.0);
        assert_eq!(presets.scale(2, false), 4.0);
        // a row without a `name` child is skipped like the reference's
        // getPresetData walk
        let junk = section.add_child("row");
        junk.write_float("minRotX", 5.0);
        let preset = section.add_child("row");
        preset.write_string("name", "backwards");
        // a reversed span (max < min) keeps the minimum when placed
        preset.write_float("minRotX", 90.0);
        preset.write_float("maxRotX", 0.0);
        preset.write_bool("uniformScale", true);
        let backwards = PlacementPresets::from_section("backwards", &section);
        // from_section reads every named row; the selector picks one
        assert_eq!(backwards.presets.len(), 2);
        assert_eq!(backwards.current().unwrap().name, "backwards");
        assert_eq!(backwards.rotation(0, false), 90.0);
        assert_eq!(backwards.rotation(0, true), 0.0);
        // rows missing the scale fields answer the 1.0 defaults
        assert_eq!(backwards.scale(1, true), 1.0);
        assert!(backwards.scale_uniform());

        // the defaults answer 0 rotation and 1 scale with uniform collapsing
        let normal = PlacementPresets::normal();
        assert_eq!(normal.rotation(0, false), 0.0);
        assert_eq!(normal.scale(0, true), 1.0);
        assert!(normal.scale_uniform());
        // a non-default selector without matching rows answers the no-preset
        // values like the reference's NULL preset section
        let empty = PlacementPresets {
            current_preset: "other".to_string(),
            presets: Vec::new(),
        };
        assert_eq!(empty.rotation(0, false), 0.0);
        assert_eq!(empty.scale(0, true), 0.0);
        assert!(empty.scale_uniform());

        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 1u32;
        let mut opts = options(1, &mut rng);
        opts.presets = presets;
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        let got = item_transform(&world, item);
        // the per-axis scales land on each basis row (rng()=1 answers 1°
        // rotations, which barely perturb the row lengths)
        let row_length =
            |row: usize| Vector3::new(got.m[row][0], got.m[row][1], got.m[row][2]).length();
        assert!((row_length(0) - 2.0).abs() < 0.1, "x scaled by 2");
        assert!((row_length(1) - 3.0).abs() < 0.1, "y scaled by 3");
        assert!((row_length(2) - 4.0).abs() < 0.1, "z scaled by 4");
        // ...and the 1° rotation mixed the axes (uniform collapse off)
        assert!(got.m[0][2].abs() > 1e-4, "row 0 is rotated");
        assert!((got.apply_to_origin() - Vector3::new(30.0, 0.0, 30.0)).length() < 1e-4);
    }

    #[test]
    fn default_presets_scale_by_one_and_keep_the_pose() {
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 7u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        // the default preset answers scale 1 (uniform): basis stays identity
        let got = item_transform(&world, item);
        assert!(got.almost(&pose, 1e-6));
    }

    #[test]
    fn speedtree_scales_feet_into_metres() {
        let mut world = grid_world();
        let mut section = DataSection::new("speedtree");
        section.write_string("resource", "tree.speedtree");
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(&mut SectionItemFactory, &section, &pose, &mut opts);
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        let got = item_transform(&world, item);
        // the pre-multiplied feet-to-metres conversion shows on the basis
        assert!((got.m[0][0] - PLACER_FEET_TO_METRES).abs() < 1e-6);
    }

    /// A factory that reads `<bounds>` children into the item box.
    struct BoundsFactory;
    impl ItemFactory for BoundsFactory {
        fn load_item(
            &mut self,
            world: &mut EditorWorld,
            chunk: &str,
            section: &DataSection,
        ) -> FactoryResult {
            let Some(mut item) = EditorItem::from_section(section) else {
                return FactoryResult::succeeded_without_item();
            };
            if let Some(bb) = section.find_child("bounds") {
                item.bounds = BoundBox::new(
                    bb.find_child("min")
                        .map(|m| m.as_vector3(Vector3::ZERO))
                        .unwrap(),
                    bb.find_child("max")
                        .map(|m| m.as_vector3(Vector3::ZERO))
                        .unwrap(),
                );
            }
            item.is_shell_model = section.find_child("shellItem").is_some();
            item.can_delete = section.read_bool("canDelete", true);
            item.chunk = Some(chunk.to_string());
            let index = world.items.len();
            world.items.push(item);
            FactoryResult::ok(index)
        }
    }

    #[test]
    fn an_oversized_model_converts_to_a_proxy_vlo_and_retries() {
        let mut world = grid_world();
        let mut section = DataSection::new("model");
        section.write_string("resource", "huge.model");
        let bounds = section.add_child("bounds");
        bounds.write_vector3("min", Vector3::ZERO);
        bounds.write_vector3("max", Vector3::new(200.0, 10.0, 100.0));
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 3u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(&mut BoundsFactory, &section, &pose, &mut opts);
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        // the original item was deleted; the replacement is the proxy
        assert_eq!(world.items[0].chunk, None);
        assert_eq!(world.items[0].pre_deletes, 1);
        assert_eq!(world.items[item].tag, "proxyvlo");
        // the converted section: type/uid on the root, the model under proxy
        let uid = &world.items[item].description;
        assert!(uid.starts_with("proxyvlo"), "description {}", uid);
        assert_eq!(world.selected.len(), 1);
    }

    #[test]
    fn a_too_small_item_is_deleted_with_the_reference_error() {
        let mut world = grid_world();
        let mut section = DataSection::new("model");
        section.write_string("resource", "tiny.model");
        let bounds = section.add_child("bounds");
        bounds.write_vector3("min", Vector3::new(0.0, 0.0, 0.0));
        bounds.write_vector3("max", Vector3::new(0.0005, 0.0005, 0.0005));
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(&mut BoundsFactory, &section, &pose, &mut opts);
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() the item is too small in at least one axis, please check its bounding box"
                    .to_string()
            )
        );
        assert_eq!(world.items[0].chunk, None);
        assert_eq!(world.items[0].pre_deletes, 1);
        assert_eq!(
            world.errors[0],
            "WorldEditor.createChunkItem() the item is too small in at least one axis, please check its bounding box"
        );
        assert_eq!(
            world.errors[1],
            "WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/ITEM_TOO_SMALL"
        );
        assert!(world.changed_chunks.contains("00000000o"));
    }

    #[test]
    fn a_too_small_shell_model_takes_its_chunk_down() {
        let mut world = grid_world();
        let mut section = DataSection::new("model");
        section.write_string("resource", "shell.model");
        section.write_bool("shellItem", true);
        let bounds = section.add_child("bounds");
        bounds.write_vector3("min", Vector3::ZERO);
        bounds.write_vector3("max", Vector3::new(0.0005, 2.0, 2.0));
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(&mut BoundsFactory, &section, &pose, &mut opts);
        assert!(matches!(outcome, CreateOutcome::Error(_)));
        assert!(world.items[0].is_shell_model);
        assert!(world.space.chunk("00000000o").is_none(), "chunk deleted");
    }

    #[test]
    fn a_refused_permanent_transform_throws_the_item_away() {
        // an item whose owning chunk is locked (factory-side membership)
        // fails the permanent transform and the creation throws it away
        struct LockedChunkFactory;
        impl ItemFactory for LockedChunkFactory {
            fn load_item(
                &mut self,
                world: &mut EditorWorld,
                _chunk: &str,
                section: &DataSection,
            ) -> FactoryResult {
                let mut item = EditorItem::from_section(section).unwrap();
                item.chunk = Some("00010000o".to_string());
                let index = world.items.len();
                world.items.push(item);
                FactoryResult::ok(index)
            }
        }
        let mut world = grid_world();
        world.locked_chunks.insert("00010000o".to_string());
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut LockedChunkFactory,
            &model_section("a.model"),
            &pose,
            &mut opts,
        );
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() could not move item to desired transform"
                    .to_string()
            )
        );
        assert_eq!(world.items[0].chunk, None, "thrown away");
    }

    #[test]
    fn uid_generation_is_lowercase_uniqueid() {
        let mut rng = || 0xABCD_EF12u32;
        assert_eq!(
            generate_uid(&mut rng),
            "abcdef12.abcdef12.abcdef12.abcdef12"
        );
    }

    // ---- the transform face ----

    #[test]
    fn transient_transform_keeps_the_chunk_and_permanent_moves_it() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();

        // transient: the pose is stored as-is and the chunk stays
        let mut m = Matrix::identity();
        m.set_translate(30.0, 0.0, 30.0);
        assert!(world.item_ed_transform(item, &m, true));
        assert_eq!(
            world.items[item].transform.apply_to_origin(),
            Vector3::new(30.0, 0.0, 30.0)
        );
        assert_eq!(world.items[item].chunk.as_deref(), Some("00000000o"));

        // permanent into the neighbouring chunk re-expresses the pose
        let mut far = Matrix::identity();
        far.set_translate(130.0, 0.0, 30.0);
        assert!(world.item_ed_transform(item, &far, false));
        assert_eq!(world.items[item].chunk.as_deref(), Some("00010000o"));
        // local pose is world minus the new chunk origin
        assert_eq!(
            world.items[item].transform.apply_to_origin(),
            Vector3::new(30.0, 0.0, 30.0)
        );
        // ...and the world position through the new chunk transform matches
        let chunk = world.space.chunk("00010000o").unwrap();
        assert_eq!(
            chunk
                .transform
                .apply_point(&world.items[item].transform.apply_to_origin()),
            Vector3::new(130.0, 0.0, 30.0)
        );
    }

    #[test]
    fn drop_outside_every_loaded_chunk_refuses_with_the_reference_error() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        // the origin flavour keeps the drop point on the round numbers
        world.items[item].drop_at_bounds_centre = false;
        let mut m = Matrix::identity();
        m.set_translate(500.0, 0.0, 500.0);
        assert!(!world.item_ed_transform(item, &m, false));
        let description = world.items[item].description.clone();
        let error = format!(
            "Cannot move {} to (500,0,500) because it is not in any loaded chunk!",
            description
        );
        assert_eq!(world.errors, vec![error]);
    }

    #[test]
    fn an_item_without_a_chunk_refuses_the_transform() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.items[item].chunk = None;
        assert!(!world.item_ed_transform(item, &Matrix::identity(), true));
        assert_eq!(
            world.errors,
            vec![format!(
                "{} has not been added to a chunk!",
                world.items[item].description
            )]
        );
    }

    #[test]
    fn a_locked_target_chunk_refuses_the_permanent_move() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.locked_chunks.insert("00010000o".to_string());
        let mut m = Matrix::identity();
        m.set_translate(130.0, 0.0, 30.0);
        assert!(!world.item_ed_transform(item, &m, false));
        assert_eq!(world.items[item].chunk.as_deref(), Some("00000000o"));
        // transient ignores the locks (only the permanent branch checks them)
        assert!(world.item_ed_transform(item, &m, true));
    }

    #[test]
    fn the_entity_flavour_drops_by_the_origin() {
        let mut world = grid_world();
        let section = model_section("npc.entity");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.items[item].drop_at_bounds_centre = false;
        world.items[item].bounds =
            BoundBox::new(Vector3::new(0.0, 0.0, 0.0), Vector3::new(60.0, 1.0, 60.0));
        // the asymmetric box puts the bounds centre at +(30, 0, 30): with the
        // drop at (95, 0, 95) the origin flavour lands in chunk (0,0) while
        // the bounds-centre flavour would cross into chunk (1,1)
        let mut m = Matrix::identity();
        m.set_translate(95.0, 0.0, 95.0);
        assert!(world.item_ed_transform(item, &m, false));
        assert_eq!(world.items[item].chunk.as_deref(), Some("00000000o"));
    }

    // ---- ChunkItemMatrix ----

    fn matrix_proxy(item: usize, world: &EditorWorld) -> ChunkItemMatrix {
        ChunkItemMatrix::new(item, world)
    }

    #[test]
    fn set_matrix_snaps_the_moved_translation_by_item_snaps() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut start = Matrix::identity();
        start.set_translate(30.0, 0.0, 30.0);
        world.items[item].transform = start;
        world.items[item].movement_delta_snaps = Vector3::new(5.0, 0.0, 5.0);
        let mut proxy = matrix_proxy(item, &world);

        // the identity translation equals the current one: no snap, no move
        let same = Matrix::identity();
        assert!(proxy.set_matrix(&mut world, &same));

        let mut moved = Matrix::identity();
        moved.set_translate(32.3, 1.0, 38.0);
        assert!(proxy.set_matrix(&mut world, &moved));
        let t = world.items[item].transform.apply_to_origin();
        assert_eq!(t, Vector3::new(30.0, 1.0, 40.0), "xz snapped to the 5 grid");

        // item snaps of zero fall back to the world snaps when enabled
        world.items[item].movement_delta_snaps = Vector3::ZERO;
        world.snaps_enabled = true;
        world.movement_snaps = Vector3::new(100.0, 0.0, 100.0);
        let mut proxy = matrix_proxy(item, &world);
        let mut moved = Matrix::identity();
        moved.set_translate(92.0, 1.0, 98.0);
        assert!(proxy.set_matrix(&mut world, &moved));
        let t = world.items[item].transform.apply_to_origin();
        assert_eq!(t, Vector3::new(100.0, 1.0, 100.0));
    }

    #[test]
    fn get_matrix_multiplies_the_chunk_transform_in_world_space() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut start = Matrix::identity();
        start.set_translate(10.0, 0.0, 10.0);
        let mut world_items = world;
        world_items.items[item].transform = start;
        let proxy = matrix_proxy(item, &world_items);
        let local = proxy.get_matrix(&world_items, false);
        assert_eq!(local.apply_to_origin(), Vector3::new(10.0, 0.0, 10.0));
        let world_m = proxy.get_matrix(&world_items, true);
        assert_eq!(world_m.apply_to_origin(), Vector3::new(10.0, 0.0, 10.0));
        // in a moved chunk the world form carries the chunk translation
        world_items
            .space
            .chunk_mut("00000000o")
            .unwrap()
            .transform
            .set_translate(100.0, 0.0, 0.0);
        let proxy = matrix_proxy(item, &world_items);
        assert_eq!(
            proxy.get_matrix(&world_items, true).apply_to_origin(),
            Vector3::new(110.0, 0.0, 10.0)
        );
        // a chunk record missing from the space answers the plain local pose
        world_items.items[item].chunk = Some("ghost-chunk".to_string());
        assert_eq!(
            matrix_proxy(item, &world_items)
                .get_matrix(&world_items, true)
                .apply_to_origin(),
            Vector3::new(10.0, 0.0, 10.0)
        );
    }

    #[test]
    fn commit_revert_restores_the_recorded_pose_without_undo() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut start = Matrix::identity();
        start.set_translate(10.0, 0.0, 10.0);
        world.items[item].transform = start;
        let mut proxy = matrix_proxy(item, &world);
        proxy.record_state(&world);
        let mut moved = Matrix::identity();
        moved.set_translate(20.0, 0.0, 20.0);
        proxy.set_matrix(&mut world, &moved);
        assert!(proxy.has_changed(&world));

        // a revert commit puts the recorded pose back and pushes nothing
        assert!(proxy.commit_state(&mut world, true, true));
        assert_eq!(
            world.items[item].transform.apply_to_origin(),
            Vector3::new(10.0, 0.0, 10.0)
        );
        assert!(!world.undoredo.can_undo());
        assert!(!proxy.has_changed(&world));
    }

    #[test]
    fn commit_moves_between_chunks_and_records_the_matrix_undo() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut start = Matrix::identity();
        start.set_translate(10.0, 0.0, 10.0);
        world.items[item].transform = start;
        let mut proxy = matrix_proxy(item, &world);
        proxy.record_state(&world);

        let mut moved = Matrix::identity();
        moved.set_translate(120.0, 0.0, 10.0);
        proxy.set_matrix(&mut world, &moved);
        assert!(proxy.commit_state(&mut world, false, true));

        assert_eq!(world.items[item].chunk.as_deref(), Some("00010000o"));
        assert_eq!(world.items[item].post_modifies, 1);
        assert_eq!(closed_barrier(&world).ops.len(), 1);
        let op = closed_barrier(&world).ops[0]
            .as_any()
            .downcast_ref::<MatrixOperation>()
            .unwrap();
        assert_eq!(op.item, item);
        assert_eq!(op.old_chunk, Some("00000000o".to_string()));
        assert_eq!(op.old_pose, start);
        assert_eq!(op.replaced, None);
        assert_eq!(
            world.undo_barriers,
            vec![format!(
                "GIZMO/UNDO/MOVE_PROPERTY {}",
                world.items[item].description
            )]
        );
        // both faces of the move reported their chunks
        assert!(world.changed_chunks.contains("00000000o"));
        assert!(world.changed_chunks.contains("00010000o"));
    }

    #[test]
    fn a_refused_commit_restores_the_pose_but_still_pushes_the_undo() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut start = Matrix::identity();
        start.set_translate(10.0, 0.0, 10.0);
        world.items[item].transform = start;
        let mut proxy = matrix_proxy(item, &world);
        proxy.record_state(&world);
        // lock the destination so the permanent re-apply fails
        world.locked_chunks.insert("00010000o".to_string());

        let mut moved = Matrix::identity();
        moved.set_translate(120.0, 0.0, 10.0);
        proxy.set_matrix(&mut world, &moved);
        assert!(!proxy.commit_state(&mut world, false, true));
        // the item sits back on the recorded pose, in the original chunk
        assert_eq!(world.items[item].chunk.as_deref(), Some("00000000o"));
        assert_eq!(
            world.items[item].transform.apply_to_origin(),
            Vector3::new(10.0, 0.0, 10.0)
        );
        // the undo op is pushed regardless (the reference unconditionally adds)
        assert_eq!(closed_barrier(&world).ops.len(), 1);
        assert_eq!(world.undo_barriers.len(), 1);
    }

    #[test]
    fn a_pending_selection_replacement_defers_the_move_barrier() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let a = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let b = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.selected = vec![a];
        world.items[a].is_selected = true;
        // queue a replacement: pending lists are non-empty
        world.defer_replace_selection(a, b);
        assert!(world.has_pending_selections());

        let mut proxy = matrix_proxy(a, &world);
        proxy.record_state(&world);
        let mut moved = Matrix::identity();
        moved.set_translate(20.0, 0.0, 20.0);
        proxy.set_matrix(&mut world, &moved);
        assert!(proxy.commit_state(&mut world, false, true));
        // the barrier is withheld while the replacement is pending...
        assert!(world.undo_barriers.is_empty());
        // ...and `needsDeferSelectionAddUndoBarrier` remembered the flag
        assert!(world.add_undo_barrier_flag);
        // the flush raises it with the replacement's first item description
        world.flush_selection_replacement();
        assert_eq!(
            world.undo_barriers,
            vec![format!(
                "GIZMO/UNDO/MOVE_PROPERTY {}",
                world.items[b].description
            )]
        );
        assert_eq!(world.selected, vec![b]);
    }

    // ---- undo and redo (tools/common/undoredo.cpp) ----

    /// The acceptance's object snapshot: chunk membership plus the pose.
    fn pose_snapshot(world: &EditorWorld, item: usize) -> (Option<String>, Matrix) {
        (world.items[item].chunk.clone(), world.items[item].transform)
    }

    #[test]
    fn move_undo_redo_round_trip_restores_both_snapshots() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut start = Matrix::identity();
        start.set_translate(10.0, 0.0, 10.0);
        world.items[item].transform = start;
        let before = pose_snapshot(&world, item);

        let mut proxy = matrix_proxy(item, &world);
        proxy.record_state(&world);
        let mut moved = Matrix::identity();
        moved.set_translate(120.0, 0.0, 10.0);
        proxy.set_matrix(&mut world, &moved);
        assert!(proxy.commit_state(&mut world, false, true));
        let after_move = pose_snapshot(&world, item);
        assert_eq!(after_move.0.as_deref(), Some("00010000o"));
        assert!(world.can_undo());
        assert!(!world.can_redo());

        world.undo();
        // the pre-move snapshot matches again
        assert_eq!(pose_snapshot(&world, item), before);
        assert!(!world.can_undo());
        assert!(world.can_redo());
        assert!(world.redo_info(0).starts_with("GIZMO/UNDO/MOVE_PROPERTY"));
        // the restore notified the chunk it went back to
        assert!(world.changed_chunks.contains("00000000o"));

        world.redo();
        // and the moved snapshot matches the original move exactly
        assert_eq!(pose_snapshot(&world, item), after_move);
        assert!(world.can_undo());
        assert!(!world.can_redo());
        assert!(world.undo_info(0).starts_with("GIZMO/UNDO/MOVE_PROPERTY"));
    }

    #[test]
    fn creation_undo_redo_round_trip_restores_the_snapshot() {
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("abc.model"),
            &pose,
            &mut opts,
        );
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        let created = pose_snapshot(&world, item);
        assert_eq!(world.items[item].post_creates, 1);
        assert_eq!(world.selection_events, vec![(item, true)]);

        world.undo();
        // the item is gone: unbound, deleted, deselected
        assert_eq!(world.items[item].chunk, None);
        assert_eq!(world.items[item].pre_deletes, 1);
        assert_eq!(world.items[item].post_creates, 1);
        assert_eq!(world.selected, Vec::<usize>::new());
        // the deletion notified the chunk it lived in
        assert!(world.changed_chunks.contains("00000000o"));

        world.redo();
        // the created snapshot matches again, with the re-create callbacks
        assert_eq!(pose_snapshot(&world, item), created);
        assert_eq!(world.items[item].post_creates, 2);
        assert_eq!(world.selected, vec![item]);
        assert_eq!(
            world.selection_events,
            vec![(item, true), (item, false), (item, true)]
        );
    }

    #[test]
    fn an_undeleteable_creation_cannot_be_undone() {
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("abc.model"),
            &pose,
            &mut opts,
        );
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        world.items[item].can_delete = false;

        world.undo();
        // the removal branch bailed out on `edCanDelete`
        assert!(world.items[item].chunk.is_some());
        assert_eq!(world.items[item].pre_deletes, 0);
        assert_eq!(world.selected, vec![item]);
        // nothing landed on the redo list
        assert!(world.undoredo.redo_list[0].ops.is_empty());
    }

    #[test]
    fn replacement_undo_restores_the_replaced_item() {
        let mut world = grid_world();
        let existing = world
            .load_item(
                &mut SectionItemFactory,
                "00000000o",
                &model_section("old.model"),
            )
            .item
            .unwrap();
        world.selected.push(existing);
        world.items[existing].is_selected = true;

        let mut pose = Matrix::identity();
        pose.set_translate(1.0, 0.0, 1.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let mut factory = ReplacingFactory {
            existing: Some(existing),
        };
        let outcome =
            world.create_chunk_item(&mut factory, &model_section("new.model"), &pose, &mut opts);
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        // the factory removed the old one from its chunk
        assert_eq!(world.items[existing].chunk, None);

        world.undo();
        // the walk ran the barrier backwards: the created item is removed,
        // the replaced one is back in its chunk and re-selected
        assert_eq!(world.items[item].chunk, None);
        assert_eq!(
            pose_snapshot(&world, existing).0.as_deref(),
            Some("00000000o")
        );
        assert_eq!(world.items[existing].post_creates, 1);
        assert_eq!(world.selected, vec![existing]);
        assert_eq!(
            world.selection_events,
            vec![
                (existing, false),
                (item, true),
                (item, false),
                (existing, true)
            ]
        );
    }

    #[test]
    fn matrix_undo_on_an_unbound_item_only_records_the_inverse() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        // the vlo-reference face: the item has no resident chunk
        world.items[item].chunk = None;

        world.undo_add(Box::new(MatrixOperation {
            item,
            old_chunk: Some("00000000o".to_string()),
            old_pose: Matrix::identity(),
            replaced: None,
        }));
        world.barrier("MOVE", false);
        world.undo();

        // the safety check skipped every chunk touch
        assert_eq!(world.items[item].chunk, None);
        assert_eq!(world.items[item].transform, Matrix::identity());
        assert_eq!(world.changed_chunks.len(), 0);
        // but the inverse landed on the redo entry
        assert_eq!(world.undoredo.redo_list[0].ops.len(), 1);
    }

    #[test]
    fn matrix_undo_with_a_replaced_item_throws_the_mover_away() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let mover = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let other = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.items[mover].chunk = Some("00010000o".to_string());
        let mut moved_pose = Matrix::identity();
        moved_pose.set_translate(5.0, 0.0, 5.0);
        world.items[mover].transform = moved_pose;

        // the reference constructor accepts a replaced item; the port pins
        // its undo face directly
        world.undo_add(Box::new(MatrixOperation {
            item: mover,
            old_chunk: Some("00000000o".to_string()),
            old_pose: Matrix::identity(),
            replaced: Some(other),
        }));
        world.barrier("REPLACE", false);
        world.undo();

        // the mover went back to the old chunk and pose, then the replaced
        // branch threw it away
        assert_eq!(world.items[mover].pre_deletes, 1);
        assert_eq!(world.items[mover].chunk, None);
        assert_eq!(world.items[mover].transform, Matrix::identity());
        // the replaced item is restored
        assert_eq!(world.items[other].chunk.as_deref(), Some("00000000o"));
        assert_eq!(world.items[other].transform, Matrix::identity());
    }

    #[test]
    fn undo_info_names_the_editor_barriers_in_order() {
        let mut world = grid_world();
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 0u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(
            &mut SectionItemFactory,
            &model_section("abc.model"),
            &pose,
            &mut opts,
        );
        let item = match outcome {
            CreateOutcome::Placed { item } => item,
            other => panic!("expected placement, got {:?}", other),
        };
        let mut proxy = matrix_proxy(item, &world);
        proxy.record_state(&world);
        let mut moved = Matrix::identity();
        moved.set_translate(20.0, 0.0, 20.0);
        proxy.set_matrix(&mut world, &moved);
        proxy.commit_state(&mut world, false, true);

        assert!(world.undo_info(0).starts_with("GIZMO/UNDO/MOVE_PROPERTY"));
        assert!(
            world
                .undo_info(1)
                .starts_with("WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CREATE_ASSET")
        );
        assert_eq!(world.undo_info(2), "");

        world.undo();
        assert!(world.redo_info(0).starts_with("GIZMO/UNDO/MOVE_PROPERTY"));
        assert_eq!(world.redo_info(1), "");
        assert!(
            world
                .undo_info(0)
                .starts_with("WORLDEDITOR/WORLDEDITOR/CHUNK/CHUNK_ITEM_PLACER/CREATE_ASSET")
        );
    }

    // ---- selection (world_manager.cpp) ----

    #[test]
    fn selection_events_fire_deselects_before_selects() {
        let mut world = grid_world();
        let section = model_section("a.model");
        let a = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let b = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let c = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.set_selection(&[a, b], true);
        assert_eq!(world.selected, vec![a, b]);
        assert_eq!(world.items[a].selected_calls, 1);
        assert_eq!(
            world.script_calls,
            vec!["bd.setSelection(2 items, update=true)".to_string()]
        );

        // swap b out for c: b deselects before c selects, in that order
        world.set_selection(&[a, c], true);
        assert_eq!(world.selected, vec![a, c]);
        assert_eq!(
            world.selection_events,
            vec![(a, true), (b, true), (b, false), (c, true)]
        );
        assert!(!world.items[b].is_selected);
    }

    #[test]
    fn an_externally_deselected_item_drops_out_of_the_next_update() {
        let mut world = grid_world();
        let section = model_section("a.model");
        let a = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let b = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.set_selection(&[a, b], true);
        // the item deselects itself (edIsSelected false): the next update
        // drops it from the selection... and the add pass re-adds it, because
        // the reference filters the removal by edIsSelected but the add pass
        // only checks membership. It ends up at the tail with a fresh
        // edSelected, and the deselect fires before the re-select.
        world.items[a].is_selected = false;
        world.update_selection(&[a, b]);
        assert_eq!(world.selected, vec![b, a]);
        assert_eq!(
            world.selection_events,
            vec![(a, true), (b, true), (a, false), (a, true)]
        );
    }

    #[test]
    fn remove_from_selected_round_trips_through_set_selection_false() {
        let mut world = grid_world();
        let section = model_section("a.model");
        let a = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let b = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let c = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.set_selection(&[a, b], true);
        world.remove_from_selected(a);
        assert_eq!(world.selected, vec![b]);
        assert_eq!(
            world.script_calls.last(),
            Some(&"bd.setSelection(1 items, update=false)".to_string())
        );
        // dropping a stranger leaves the selection as it was
        world.remove_from_selected(c);
        assert_eq!(world.selected, vec![b]);
    }

    #[test]
    fn replace_selection_cancels_out_through_the_pending_lists() {
        let mut world = grid_world();
        let section = model_section("a.model");
        let a = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let b = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let c = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.set_selection(&[a, b], true);

        // a deferred swap that round-trips cancels out entirely
        // (remove a / add c, then remove c / add a — c was queued as an add)
        world.defer_replace_selection(a, c);
        world.defer_replace_selection(c, a);
        assert!(world.pending_remove.is_empty() && world.pending_add.is_empty());
        // replacing a with c queues remove a / add c
        world.defer_replace_selection(a, c);
        assert_eq!(world.pending_remove, vec![a]);
        assert_eq!(world.pending_add, vec![c]);
        world.flush_selection_replacement();
        assert_eq!(world.selected, vec![b, c]);
        assert!(world.script_calls.last().unwrap().contains("update=true"));

        // an empty flush is a no-op
        world.flush_selection_replacement();
        assert_eq!(world.selected, vec![b, c]);
    }

    #[test]
    fn replace_selection_moves_the_selection_in_one_step() {
        let mut world = grid_world();
        let section = model_section("a.model");
        let a = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let b = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        world.set_selection(&[a], true);
        world.replace_selection(a, b);
        assert_eq!(world.selected, vec![b]);
        assert!(!world.has_pending_selections());
    }

    // ---- the property face ----

    fn angle_property(item: usize) -> AccessorDataProxy {
        AccessorDataProxy::new(
            item,
            "Angle",
            Box::new(|world, item| world.items[item].angle_snaps),
            Box::new(|world, item, v| {
                // angles are only legal in [0, 360)
                if !(0.0..360.0).contains(&v) {
                    return false;
                }
                world.items[item].angle_snaps = v;
                true
            }),
        )
    }

    #[test]
    fn property_set_permanent_carries_the_edit_bookkeeping() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let property = angle_property(item);

        assert_eq!(property.get(&world), 0.0);
        // transient: no chunk bookkeeping, no save
        property.set_transient(&mut world, 45.0);
        assert_eq!(world.items[item].angle_snaps, 45.0);
        assert_eq!(world.items[item].saves, 0);
        assert!(world.changed_chunks.is_empty());

        // permanent: changed chunk + edPostModify + edSave
        assert!(property.set_permanent(&mut world, 90.0));
        assert_eq!(property.get(&world), 90.0);
        assert!(world.changed_chunks.contains("00000000o"));
        assert_eq!(world.items[item].post_modifies, 1);
        assert_eq!(world.items[item].saves, 1);
        assert_eq!(
            property.op_name(&world),
            format!("Set {} Angle", world.items[item].description)
        );

        // a refused value answers false and touches nothing
        assert!(!property.set_permanent(&mut world, 400.0));
        assert_eq!(property.get(&world), 90.0);
        assert_eq!(world.items[item].saves, 1);

        // an item without a chunk records no changed chunk
        world.items[item].chunk = None;
        assert!(property.set_permanent(&mut world, 120.0));
        assert_eq!(property.get(&world), 120.0);
        assert_eq!(world.items[item].saves, 2);
    }

    #[test]
    fn position_property_length_clamps_the_box_diagonal() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        // a point box has no diagonal: the length clamps up to 10
        world.items[item].bounds = BoundBox::new(Vector3::ZERO, Vector3::ZERO);
        assert_eq!(chunk_item_position_property_length(&world, item), 10.0);
        // a long box clamps down to 200
        world.items[item].bounds = BoundBox::new(Vector3::ZERO, Vector3::new(30.0, 0.0, 0.0));
        assert_eq!(chunk_item_position_property_length(&world, item), 200.0);
        // a medium box scales by ten
        world.items[item].bounds = BoundBox::new(Vector3::ZERO, Vector3::new(12.0, 0.0, 0.0));
        assert_eq!(chunk_item_position_property_length(&world, item), 120.0);
        // an inside-out box answers zero
        world.items[item].bounds =
            BoundBox::new(Vector3::new(1.0, 1.0, 1.0), Vector3::new(-1.0, -1.0, -1.0));
        assert_eq!(chunk_item_position_property_length(&world, item), 0.0);
    }

    // ---- the world helpers ----

    #[test]
    fn occupied_chunks_sample_the_grid_and_the_bound_portals() {
        let mut world = grid_world();
        // a degenerate outside box answers exactly its own cell
        let bb = BoundBox::new(Vector3::new(10.0, 0.0, 10.0), Vector3::new(10.0, 0.0, 10.0));
        assert_eq!(
            world.collect_occupied_chunks(&bb, Some("00000000o")),
            vec!["00000000o".to_string()]
        );
        // a box straddling the grid line collects both cells (the max edges
        // are sampled explicitly)
        let bb = BoundBox::new(
            Vector3::new(90.0, 0.0, 10.0),
            Vector3::new(110.0, 0.0, 20.0),
        );
        assert_eq!(
            world.collect_occupied_chunks(&bb, Some("00000000o")),
            vec!["00000000o".to_string(), "00010000o".to_string()]
        );
        // a null resident samples the same way
        assert_eq!(
            world.collect_occupied_chunks(&bb, None),
            vec!["00000000o".to_string(), "00010000o".to_string()]
        );
        // an inside resident takes itself plus intersecting bound portals
        let mapping = world.space.mappings[0].clone();
        let mut inner = Chunk::new("00000000i", &mapping, &Matrix::identity());
        inner.transform.set_translate(50.0, 0.0, 50.0);
        inner.bounding_box = BoundBox::new(
            Vector3::new(30.0, -5.0, 30.0),
            Vector3::new(70.0, 15.0, 70.0),
        );
        inner.is_loaded = true;
        let inner_bb = inner.bounding_box;
        world.space.add_chunk(inner);
        assert_eq!(
            world.collect_occupied_chunks(&inner_bb, Some("00000000i")),
            vec!["00000000i".to_string()]
        );
        // a resident without a loaded record samples like a null one
        assert_eq!(
            world.collect_occupied_chunks(&bb, Some("missing-chunk")),
            vec!["00000000o".to_string(), "00010000o".to_string()]
        );
    }

    #[test]
    fn world_bounds_wrap_the_chunk_transform() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut m = Matrix::identity();
        m.set_translate(120.0, 0.0, 20.0);
        world.item_ed_transform(item, &m, false);
        let bb = world.item_world_bounds(item);
        // the unit box (0..1) at local (20, 0, 20) in chunk (1,0) whose
        // origin is (100, 0, 0): (120..121, 0..1, 20..21) — chunk transform
        // included
        assert_eq!(bb.min.x, 120.0);
        assert_eq!(bb.max.x, 121.0);
        assert_eq!(bb.min.z, 20.0);
        assert_eq!(bb.max.z, 21.0);

        // an inside-out local box collapses to a point box at the pose
        // (the chunk transform included)
        world.items[item].bounds =
            BoundBox::new(Vector3::new(1.0, 1.0, 1.0), Vector3::new(-1.0, -1.0, -1.0));
        let bb = world.item_world_bounds(item);
        let pose = world.items[item].transform.apply_to_origin();
        let point = world
            .space
            .chunk("00010000o")
            .unwrap()
            .transform
            .apply_point(&pose);
        assert_eq!(bb.min, point);
        assert_eq!(bb.max, point);

        // a chunk record missing from the space skips the chunk transform
        world.items[item].bounds = BoundBox::new(Vector3::ZERO, Vector3::ZERO);
        world.items[item].chunk = Some("ghost-chunk".to_string());
        let bb = world.item_world_bounds(item);
        assert_eq!(bb.min, world.items[item].transform.apply_to_origin());

        // a deleted item answers the inside-out sentinel
        world.items[item].chunk = None;
        assert!(world.item_world_bounds(item).inside_out());
    }

    #[test]
    fn generate_uid_is_lowercase_uniqueid() {
        let mut rng = || 1u32;
        let uid = generate_uid(&mut rng);
        assert_eq!(uid, "00000001.00000001.00000001.00000001");
    }

    #[test]
    fn occupied_chunks_step_off_the_loaded_grid_without_looping_forever() {
        let world = grid_world();
        let mapping = world.space.mappings[0].clone();
        // a box whose sample steps run past the loaded grid: the off-grid
        // cells answer nothing and the four loaded cells come back sorted
        let bb = BoundBox::new(
            Vector3::new(90.0, 0.0, 90.0),
            Vector3::new(210.0, 0.0, 250.0),
        );
        let mut expected: Vec<String> = (0..2)
            .flat_map(|x| (0..2).map(move |z| (x, z)))
            .map(|(x, z)| mapping.outside_chunk_identifier(x, z, false))
            .collect();
        expected.sort();
        assert_eq!(
            world.collect_occupied_chunks(&bb, Some("00000000o")),
            expected
        );
    }

    #[test]
    fn an_inside_resident_collects_its_bound_portal_neighbours() {
        let mut world = grid_world();
        let mapping = world.space.mappings[0].clone();
        let mut inner = Chunk::new("00000000i", &mapping, &Matrix::identity());
        inner.transform.set_translate(50.0, 0.0, 50.0);
        inner.bounding_box = BoundBox::new(
            Vector3::new(30.0, -5.0, 30.0),
            Vector3::new(70.0, 15.0, 70.0),
        );
        inner.is_loaded = true;
        // joint 1 points past the boundary list — the walk skips it
        inner.joints = vec![0, 1];
        inner.boundaries = vec![ChunkBoundary {
            plane: PlaneEq::default(),
            bound_portals: vec![chunk_portal("00010000o")],
            unbound_portals: Vec::new(),
        }];
        world.space.add_chunk(inner);
        // the box reaches into chunk (1,0) through the bound portal
        let bb = BoundBox::new(
            Vector3::new(90.0, 0.0, 40.0),
            Vector3::new(110.0, 10.0, 60.0),
        );
        assert_eq!(
            world.collect_occupied_chunks(&bb, Some("00000000i")),
            vec!["00000000i".to_string(), "00010000o".to_string()]
        );
    }

    #[test]
    fn an_undeletable_too_small_item_stays_put() {
        let mut world = grid_world();
        let mut section = DataSection::new("model");
        section.write_string("resource", "tiny.model");
        section.write_bool("canDelete", false);
        let bounds = section.add_child("bounds");
        bounds.write_vector3("min", Vector3::ZERO);
        bounds.write_vector3("max", Vector3::new(0.0005, 0.0005, 0.0005));
        let mut pose = Matrix::identity();
        pose.set_translate(30.0, 0.0, 30.0);
        let mut rng = || 1u32;
        let mut opts = options(1, &mut rng);
        let outcome = world.create_chunk_item(&mut BoundsFactory, &section, &pose, &mut opts);
        assert_eq!(
            outcome,
            CreateOutcome::Error(
                "WorldEditor.createChunkItem() the item is too small in at least one axis, please check its bounding box"
                    .to_string()
            )
        );
        // the item survives when it refuses deletion
        assert_eq!(world.items[0].chunk.as_deref(), Some("00000000o"));
        assert_eq!(world.items[0].pre_deletes, 0);
    }

    #[test]
    fn commit_without_a_record_records_on_the_spot() {
        let mut world = grid_world();
        let section = model_section("abc.model");
        let item = world
            .load_item(&mut SectionItemFactory, "00000000o", &section)
            .item
            .unwrap();
        let mut start = Matrix::identity();
        start.set_translate(10.0, 0.0, 10.0);
        world.items[item].transform = start;
        // no explicit record_state: the commit records the live pose first
        let mut proxy = matrix_proxy(item, &world);
        assert!(proxy.commit_state(&mut world, true, false));
        assert_eq!(
            world.items[item].transform.apply_to_origin(),
            Vector3::new(10.0, 0.0, 10.0)
        );
        assert!(!proxy.has_changed(&world));
    }
}
