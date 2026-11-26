//! BW-10-05-04 remainder: the navmesh cache save face —
//! `tools/common/editor_chunk_navmesh_cache_base.cpp`. The per-girth
//! ring ([`crate::navmesh_processor::process_in_background`]) hands
//! its blob to `calcDone` (`:341-364`); the cache then stores it as
//! the chunk's `.cdata/worldNavmesh` section with the `navmeshDirty`
//! flag navgen also writes (`saveCData`, `:109-134`; navgen's
//! `outputDirtyFlag` is designed to match, `chunk_waypoint_generator.cpp`
//! `outputDirtyFlag`).

/// `ChunkSpace::NAVMESH_GENERATOR_*`
/// (`base_chunk_space.hpp`): the space's navmesh generator setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavmeshGenerator {
    /// `NAVMESH_GENERATOR_NONE` — no navmesh is generated or saved.
    None,
    /// `NAVMESH_GENERATOR_NAVGEN` — the flood/BSP generator.
    Navgen,
    /// `NAVMESH_GENERATOR_RECAST` — the Recast generator.
    Recast,
}

/// Which processor `recalc` created (`:292-312`). The reference
/// compares pointers in `calcDone`; the port compares the variant —
/// only one processor can be current at a time either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Processor {
    /// `NavmeshProcessor` (the flood/BSP face).
    Navmesh,
    /// `RecastProcessor`.
    Recast,
}

/// `EditorChunkNavmeshCacheBase`'s state machine over one chunk: the
/// stored bytes, the dirty flag, and the current background
/// processor.
#[derive(Default)]
pub struct NavmeshCache {
    /// `navmesh_` — the stored (or freshly calculated) record blob.
    navmesh: Vec<u8>,
    /// `dirty_` — whether the navmesh needs recalculation.
    dirty: bool,
    /// `currentProcessor_` — the processor `recalc` created.
    current_processor: Option<Processor>,
}

impl NavmeshCache {
    /// `load` (`:44-90`): read the stored dirty flag (a bool written
    /// as binary, `len == sizeof(bool)`), mark dirty when a clean
    /// chunk has no stored navmesh at all (`:64-67`), and keep the
    /// stored bytes.
    pub fn load(&mut self, world_navmesh: Option<&[u8]>, navmesh_dirty: Option<&[u8]>) {
        if let Some(flag) = navmesh_dirty.filter(|flag| flag.len() == 1) {
            self.dirty = flag[0] != 0;
        }
        if !self.dirty && world_navmesh.is_none() {
            self.dirty = true;
        }
        if let Some(bytes) = world_navmesh {
            self.navmesh = bytes.to_vec();
        }
    }

    /// `dirty()` (`:266-277`): a space without a generator never
    /// reports dirty.
    pub fn dirty(&self, generator: NavmeshGenerator) -> bool {
        if generator == NavmeshGenerator::None {
            return false;
        }
        self.dirty
    }

    /// `recalc` (`:279-330`): clear the blob, mark dirty, and create
    /// the processor for the space's generator.
    pub fn recalc(&mut self, generator: NavmeshGenerator) -> bool {
        self.navmesh.clear();
        self.dirty = true;

        self.current_processor = match generator {
            NavmeshGenerator::Navgen => Some(Processor::Navmesh),
            NavmeshGenerator::Recast => Some(Processor::Recast),
            // the reference's ERROR_MSG arm handles generator values
            // outside the enum (`:313-320`); the port's enum cannot
            // hold an invalid value, and NONE reports dirty() == false
            // before recalc is reached
            NavmeshGenerator::None => None,
        };
        self.current_processor.is_some()
    }

    /// `calcDone` (`:341-364`): a successful background task stores
    /// the blob and clears the dirty flag, and the chunk joins the
    /// unsaved list (the returned `true`); a failure keeps the chunk
    /// dirty and drops the result. The identity check mirrors the
    /// reference's `currentProcessor_ == processor`.
    pub fn calc_done(&mut self, processor: Processor, success: bool, navmesh: &[u8]) -> bool {
        let mut saved = false;
        if self.current_processor == Some(processor) {
            if success {
                self.navmesh = navmesh.to_vec();
                self.dirty = false;
                saved = true; // unsavedList.unsavedChunks_.add(&chunk_)
            }
            self.current_processor = None;
        }
        saved
    }

    /// `saveCData` (`:109-134`): delete both sections, then — unless
    /// the space has no generator — write the `navmeshDirty` bool
    /// (matching navgen's `outputDirtyFlag`) and the stored blob when
    /// non-empty. `None` yields nothing to write.
    pub fn save_cdata(&self, generator: NavmeshGenerator) -> Option<(bool, Option<&[u8]>)> {
        if generator == NavmeshGenerator::None {
            return None;
        }
        Some((
            self.dirty,
            if self.navmesh.is_empty() {
                None
            } else {
                Some(self.navmesh.as_slice())
            },
        ))
    }

    /// `saveChunk` (`:93-106`): the chunk XML's `worldNavmesh`
    /// resource reference, or nothing without a generator.
    pub fn save_chunk(
        &self,
        generator: NavmeshGenerator,
        chunk_identifier: &str,
    ) -> Option<String> {
        if generator == NavmeshGenerator::None {
            return None;
        }
        Some(format!("{chunk_identifier}.cdata/worldNavmesh"))
    }

    /// The stored blob (`navmesh_`), for tests and callers that read
    /// the section back.
    pub fn navmesh(&self) -> &[u8] {
        &self.navmesh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_without_stored_navmesh_marks_dirty() {
        let mut cache = NavmeshCache::default();
        cache.load(None, None);
        assert!(cache.dirty(NavmeshGenerator::Navgen));
        assert!(cache.navmesh().is_empty());
    }

    #[test]
    fn load_keeps_stored_bytes_when_clean() {
        let mut cache = NavmeshCache::default();
        cache.load(Some(&[1, 2, 3]), Some(&[0]));
        assert!(!cache.dirty(NavmeshGenerator::Navgen));
        assert_eq!(cache.navmesh(), &[1, 2, 3]);
    }

    #[test]
    fn load_of_a_dirty_flag_keeps_it_dirty() {
        let mut cache = NavmeshCache::default();
        cache.load(Some(&[1, 2, 3]), Some(&[1]));
        assert!(cache.dirty(NavmeshGenerator::Navgen));
        assert_eq!(cache.navmesh(), &[1, 2, 3]);
    }

    #[test]
    fn a_space_without_a_generator_never_reports_dirty() {
        let mut cache = NavmeshCache::default();
        cache.load(None, None);
        assert!(!cache.dirty(NavmeshGenerator::None));
    }

    #[test]
    fn recalc_clears_and_marks_dirty_then_calculated_blob_stores() {
        let mut cache = NavmeshCache::default();
        cache.load(Some(&[9]), Some(&[0]));
        assert!(cache.recalc(NavmeshGenerator::Navgen));
        assert!(cache.dirty(NavmeshGenerator::Navgen));
        assert!(cache.navmesh().is_empty(), "recalc cleared the blob");

        assert!(cache.calc_done(Processor::Navmesh, true, &[4, 5, 6]));
        assert!(!cache.dirty(NavmeshGenerator::Navgen));
        assert_eq!(cache.navmesh(), &[4, 5, 6]);
    }

    #[test]
    fn failed_calculation_keeps_the_chunk_dirty_and_drops_the_blob() {
        let mut cache = NavmeshCache::default();
        cache.recalc(NavmeshGenerator::Navgen);
        assert!(!cache.calc_done(Processor::Navmesh, false, &[7]));
        assert!(cache.dirty(NavmeshGenerator::Navgen));
        assert!(cache.navmesh().is_empty());
    }

    #[test]
    fn a_result_from_a_stale_processor_is_ignored() {
        let mut cache = NavmeshCache::default();
        cache.recalc(NavmeshGenerator::Navgen);
        // RecastProcessor reporting for a Navgen chunk: not the
        // current processor, nothing stored
        assert!(!cache.calc_done(Processor::Recast, true, &[7]));
        assert!(cache.dirty(NavmeshGenerator::Navgen));
        assert!(cache.navmesh().is_empty());
    }

    #[test]
    fn recast_spaces_create_the_recast_processor() {
        let mut cache = NavmeshCache::default();
        assert!(cache.recalc(NavmeshGenerator::Recast));
        assert!(cache.calc_done(Processor::Recast, true, &[1]));
        assert!(!cache.calc_done(Processor::Navmesh, true, &[2]), "only one");
    }

    #[test]
    fn save_cdata_writes_the_flag_and_the_mesh() {
        let mut cache = NavmeshCache::default();
        cache.recalc(NavmeshGenerator::Navgen);
        cache.calc_done(Processor::Navmesh, true, &[1, 2, 3]);

        // flag false (just calculated), mesh present
        assert_eq!(
            cache.save_cdata(NavmeshGenerator::Navgen),
            Some((false, Some(&[1, 2, 3][..])))
        );
        // a space without a generator writes nothing
        assert_eq!(cache.save_cdata(NavmeshGenerator::None), None);
        // an empty blob writes only the flag
        let empty = NavmeshCache::default();
        assert_eq!(
            empty.save_cdata(NavmeshGenerator::Navgen),
            Some((false, None))
        );
    }

    #[test]
    fn save_chunk_names_the_cdata_resource() {
        let cache = NavmeshCache::default();
        assert_eq!(
            cache.save_chunk(NavmeshGenerator::Navgen, "chunkA"),
            Some("chunkA.cdata/worldNavmesh".to_string())
        );
        assert_eq!(cache.save_chunk(NavmeshGenerator::None, "chunkA"), None);
    }
}
