//! BW-13-03-05 undo and redo: `tools/common/undoredo.cpp` plus the two
//! concrete operations the editor pushes — `ChunkItemExistenceOperation`
//! (`editor/chunk_item_placer.hpp:12-39`, undo in `chunk_item_placer.cpp`)
//! and `ChunkItemMatrixOperation` (`editor/item_properties.cpp:80-164`).
//!
//! The reference keeps one process-wide `UndoRedo` singleton whose lists
//! hold `Barrier`s; the barrier at the back of the undo list is always open
//! (empty until the next `barrier` call closes it). Operations are added
//! through `add`, which routes to the redo list while an undo walk is in
//! flight — the port keeps that routing on the [`UndoRedo`] state owned by
//! [`EditorWorld`], so an operation's `undo` can call `world.undo_add`
//! exactly like the reference calls `UndoRedo::instance().add`.

use std::any::Any;

use foundation::math::Matrix;

use crate::chunk_placer::EditorWorld;

// -----------------------------------------------------------------------------
// UndoRedo::Operation (`undoredo.hpp`)
// -----------------------------------------------------------------------------

/// One undo step. The machinery never inspects the payload — it only ever
/// calls [`UndoOperation::undo`], which replays the state change and (like
/// the reference) adds the inverse operation back through the world.
pub trait UndoOperation: std::fmt::Debug {
    /// `kind_`: the typeid name used for replace-dedup. An empty kind is the
    /// reference's kind 0 (`ChunkItemExistenceOperation`) — those operations
    /// never compare equal.
    fn kind(&self) -> &'static str;

    fn as_any(&self) -> &dyn Any;

    /// `iseq`: "would this operation replace the other?". The default is
    /// never-replace.
    fn is_eq(&self, _oth: &dyn UndoOperation) -> bool {
        false
    }

    /// Apply the stored state change (its inverse, seen from the action that
    /// recorded it) and add the reverse operation to the machinery.
    fn undo(&self, world: &mut EditorWorld);
}

/// `UndoRedo::Operation::operator==` (`undoredo.hpp`): kinds must match and
/// be non-empty, then the operation's `iseq` decides.
pub fn operations_equal(op: &dyn UndoOperation, oth: &dyn UndoOperation) -> bool {
    let kind = op.kind();
    if oth.kind() != kind || kind.is_empty() {
        return false;
    }
    op.is_eq(oth)
}

// -----------------------------------------------------------------------------
// UndoRedo
// -----------------------------------------------------------------------------

/// `UndoRedo::Barrier`: a named set of operations closed by one user action.
#[derive(Debug, Default)]
pub struct Barrier {
    pub what: String,
    pub ops: Vec<Box<dyn UndoOperation>>,
}

/// The undo/redo engine (`undoredo.cpp`). The back of `undo_list` is the
/// open barrier collecting the additions of the action in flight; `canUndo`
/// needs more than that one seed barrier.
#[derive(Debug)]
pub struct UndoRedo {
    pub undo_list: Vec<Barrier>,
    pub redo_list: Vec<Barrier>,
    pub undoing: bool,
    /// The `WARNING_MSG` texts of [`UndoRedo::barrier`], verbatim.
    pub warnings: Vec<String>,
}

impl Default for UndoRedo {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoRedo {
    /// The constructor seeds one empty barrier.
    pub fn new() -> Self {
        Self {
            undo_list: vec![Barrier::default()],
            redo_list: Vec::new(),
            undoing: false,
            warnings: Vec::new(),
        }
    }

    /// `add`: during an undo walk additions land on the redo list.
    pub fn add(&mut self, op: Box<dyn UndoOperation>) {
        if self.undoing {
            self.add_redo(op);
        } else {
            self.add_undo(op);
        }
    }

    /// `addUndo`: an operation that replaces an earlier one of the same kind
    /// is dropped, keeping the original "since that sets it to the correct
    /// data".
    pub fn add_undo(&mut self, op: Box<dyn UndoOperation>) {
        let ops = &mut self.undo_list.last_mut().unwrap().ops;
        for existing in ops.iter() {
            if operations_equal(op.as_ref(), existing.as_ref()) {
                return;
            }
        }
        ops.push(op);
    }

    /// `addRedo`: no dedup — "we assume that these will closely follow the
    /// original undo list order".
    pub fn add_redo(&mut self, op: Box<dyn UndoOperation>) {
        self.redo_list.last_mut().unwrap().ops.push(op);
    }

    /// `canUndo`.
    pub fn can_undo(&self) -> bool {
        self.undo_list.len() > 1
    }

    /// `canRedo`.
    pub fn can_redo(&self) -> bool {
        !self.redo_list.is_empty()
    }

    /// `barrier`: close the current set of operations and move on, dropping
    /// any redos first. An empty set warns unless `skip_if_no_change`.
    pub fn barrier(&mut self, what: impl Into<String>, skip_if_no_change: bool) {
        self.barrier_inner(what.into(), skip_if_no_change)
    }

    fn barrier_inner(&mut self, what: String, skip_if_no_change: bool) {
        // get rid of any redos ... we're moving forward again
        self.clear_redos();

        if self.undo_list.last().unwrap().ops.is_empty() {
            if skip_if_no_change {
                return;
            }
            // the reference's format string carries a doubled apostrophe
            // after the name; kept verbatim
            self.warnings.push(format!(
                "UndoRedo::barrier: Barrier closed for '{}'' but no intermediate operations added!\n",
                what
            ));
        }

        self.barrier_internal(what);
    }

    /// `barrierInternal`: name the closed barrier and open the next one.
    fn barrier_internal(&mut self, what: String) {
        let back = self.undo_list.last_mut().unwrap();
        back.what = what;
        self.undo_list.push(Barrier::default());
    }

    /// `clearRedos`.
    pub fn clear_redos(&mut self) {
        self.redo_list.clear();
    }

    /// `undoInfo`: the barrier name `level` entries back; empty when there
    /// is no such level.
    pub fn undo_info(&self, level: u32) -> String {
        let level = level as usize + 1;
        if level >= self.undo_list.len() {
            return String::new();
        }
        self.undo_list[self.undo_list.len() - 1 - level]
            .what
            .clone()
    }

    /// `redoInfo`: the barrier name `level` entries down the redo list;
    /// empty when there is no such level.
    pub fn redo_info(&self, level: u32) -> String {
        if level as usize >= self.redo_list.len() {
            return String::new();
        }
        self.redo_list[self.redo_list.len() - 1 - level as usize]
            .what
            .clone()
    }

    /// `clear`.
    pub fn clear(&mut self) {
        self.undo_list.clear();
        self.redo_list.clear();
        self.undo_list.push(Barrier::default());
    }
}

// -----------------------------------------------------------------------------
// The undo/redo walks on the world
// -----------------------------------------------------------------------------

impl EditorWorld {
    /// `UndoRedo::add`.
    pub fn undo_add(&mut self, op: Box<dyn UndoOperation>) {
        self.undoredo.add(op);
    }

    /// `UndoRedo::barrier`, with the port's barrier-name trace kept in
    /// `undo_barriers` (every call lands there, skipped ones included).
    pub fn barrier(&mut self, what: impl Into<String>, skip_if_no_change: bool) {
        let what = what.into();
        self.undo_barriers.push(what.clone());
        self.undoredo.barrier(what, skip_if_no_change);
    }

    /// `UndoRedo::undo` (`undoredo.cpp:97-130`): apply the newest closed
    /// barrier's operations in reverse; each operation adds its inverse to
    /// the fresh redo entry.
    pub fn undo(&mut self) {
        // make sure not in some weird state, and the barrier is closed
        debug_assert!(!self.undoredo.undoing);
        debug_assert!(self.undoredo.undo_list.last().unwrap().ops.is_empty());

        // get out now if there's nothing to undo
        if !self.undoredo.can_undo() {
            return;
        }

        self.undoredo.undoing = true;

        // remove the empty last element of the undo list
        self.undoredo.undo_list.pop();

        // set up an entry on the redo list
        let what = self.undoredo.undo_list.last().unwrap().what.clone();
        self.undoredo.redo_list.push(Barrier {
            what,
            ops: Vec::new(),
        });

        // now go through and apply each of the undo operations in turn
        let back = self.undoredo.undo_list.len() - 1;
        let ops = std::mem::take(&mut self.undoredo.undo_list[back].ops);
        for op in ops.iter().rev() {
            op.undo(self);
        }

        // then clear the last undo list element
        self.undoredo.undo_list[back] = Barrier::default();

        self.undoredo.undoing = false;
    }

    /// `UndoRedo::redo` (`undoredo.cpp:138-162`): apply the newest redo
    /// entry's operations in reverse — their `add`s rebuild the original
    /// undo barrier — then close it again.
    pub fn redo(&mut self) {
        debug_assert!(!self.undoredo.undoing);
        debug_assert!(self.undoredo.undo_list.last().unwrap().ops.is_empty());

        // get out now if there's nothing to redo
        if !self.undoredo.can_redo() {
            return;
        }

        let ops = std::mem::take(&mut self.undoredo.redo_list.last_mut().unwrap().ops);
        for op in ops.iter().rev() {
            op.undo(self);
        }

        // set the barrier on the reconstructed undo list
        let what = self.undoredo.redo_list.last().unwrap().what.clone();
        self.undoredo.barrier_internal(what);

        // and forget about this redo element
        self.undoredo.redo_list.pop();
    }

    /// `canUndo`.
    pub fn can_undo(&self) -> bool {
        self.undoredo.can_undo()
    }

    /// `canRedo`.
    pub fn can_redo(&self) -> bool {
        self.undoredo.can_redo()
    }

    /// `undoInfo`.
    pub fn undo_info(&self, level: u32) -> String {
        self.undoredo.undo_info(level)
    }

    /// `redoInfo`.
    pub fn redo_info(&self, level: u32) -> String {
        self.undoredo.redo_info(level)
    }

    /// `UndoRedo::clear`.
    pub fn clear_undo(&mut self) {
        self.undoredo.clear();
    }
}

// -----------------------------------------------------------------------------
// ChunkItemExistenceOperation (editor/chunk_item_placer.hpp:12-39)
// -----------------------------------------------------------------------------

/// The add/remove record. Creation pushes `old_chunk = None` (undo deletes
/// the item), the one-per-chunk replacement pushes the replaced item's
/// chunk. The reference marks this operation kind 0: its operations never
/// replace each other in a barrier.
#[derive(Debug, Clone, PartialEq)]
pub struct ExistenceOperation {
    pub item: usize,
    pub old_chunk: Option<String>,
}

impl UndoOperation for ExistenceOperation {
    fn kind(&self) -> &'static str {
        // kind 0: "these operations never replace each other"
        ""
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn undo(&self, world: &mut EditorWorld) {
        let item = self.item;

        // If we're removing the item, check that it's ok with that
        let removing = self.old_chunk.is_none();
        if removing && !world.items[item].can_delete {
            return;
        }

        // a copy of the current selection
        let mut selection = world.selected.clone();

        // Since the current chunk will change, save it to use it later as
        // the old or previous chunk for the item.
        let undo_old_chunk = world.items[item].chunk.clone();

        match self.old_chunk.as_deref() {
            Some(old_chunk) => {
                // re-add: `pOldChunk_->addStaticItem` + `edPostCreate`
                world.items[item].chunk = Some(old_chunk.to_string());
                world.items[item].post_creates += 1;

                let world_bb = world.item_world_bounds(item);
                for chunk in world.collect_occupied_chunks(&world_bb, Some(old_chunk)) {
                    world.changed_chunk(&chunk);
                }

                // the fence branch keeps the fence out of the selection; the
                // port has no fence face, so the item always re-enters
                selection.push(item);
            }
            None => {
                // notify to update chunks before deletion
                let resident = world.items[item].chunk.clone();
                let world_bb = world.item_world_bounds(item);
                for chunk in world.collect_occupied_chunks(&world_bb, resident.as_deref()) {
                    world.changed_chunk(&chunk);
                }

                world.items[item].pre_deletes += 1;

                // delStaticItem — "a vlo reference could have been tossed
                // out", the resident may already be gone
                world.items[item].chunk = None;

                if let Some(pos) = selection.iter().position(|&i| i == item) {
                    selection.remove(pos);
                }
            }
        }

        // Add the current state of this item to the undo/redo list.
        world.undo_add(Box::new(ExistenceOperation {
            item,
            old_chunk: undo_old_chunk,
        }));
        world.set_selection(&selection, false);
    }
}

// -----------------------------------------------------------------------------
// ChunkItemMatrixOperation (editor/item_properties.cpp:80-164)
// -----------------------------------------------------------------------------

/// The move record: which item, the chunk it came from and its pose there.
/// `replaced` (`pReplacedItem_`) is always `None` from `commitState`; the
/// undo role swap keeps the field for parity with the reference signature.
#[derive(Debug, Clone, PartialEq)]
pub struct MatrixOperation {
    pub item: usize,
    pub old_chunk: Option<String>,
    pub old_pose: Matrix,
    pub replaced: Option<usize>,
}

impl UndoOperation for MatrixOperation {
    fn kind(&self) -> &'static str {
        // `typeid(ChunkItemMatrixOperation).name()` — a stable non-empty tag
        "ChunkItemMatrixOperation"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn is_eq(&self, oth: &dyn UndoOperation) -> bool {
        match oth.as_any().downcast_ref::<MatrixOperation>() {
            Some(o) => self.item == o.item,
            None => false,
        }
    }

    fn undo(&self, world: &mut EditorWorld) {
        let item = self.item;

        // first add the current state of this block to the undo/redo list
        world.undo_add(Box::new(MatrixOperation {
            item: self.replaced.unwrap_or(item),
            old_chunk: world.items[item].chunk.clone(),
            old_pose: world.items[item].transform,
            replaced: self.replaced.map(|_| item),
        }));

        // `pChunk` safety check for vlo references
        let p_chunk = match world.items[item].chunk.clone() {
            Some(p_chunk) => p_chunk,
            None => return,
        };

        // notify to update the chunks before move
        let old_bb = world.item_world_bounds(item);
        for chunk in world.collect_occupied_chunks(&old_bb, Some(&p_chunk)) {
            world.changed_chunk(&chunk);
        }

        // fix up the chunk if necessary (`delStaticItem` + `addStaticItem`)
        if world.items[item].chunk.as_deref() != self.old_chunk.as_deref() {
            world.items[item].chunk = self.old_chunk.clone();
        }

        // now change the matrix back (the transient `edTransform`)
        world.items[item].transform = self.old_pose;

        // notify to update the chunks after move — the resident is the stale
        // pre-move `pChunk`, exactly like the reference
        let new_bb = world.item_world_bounds(item);
        for chunk in world.collect_occupied_chunks(&new_bb, Some(&p_chunk)) {
            world.changed_chunk(&chunk);
        }

        if let Some(replaced) = self.replaced {
            // put the replaced item back and throw the mover away
            world.items[replaced].chunk = self.old_chunk.clone();
            world.items[replaced].transform = self.old_pose;
            world.items[item].pre_deletes += 1;
            world.items[item].chunk = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::ChunkSpace;

    /// A probe operation that logs its `tag` into `world.commentary` when
    /// undone and (optionally) reports itself replaceable per item.
    #[derive(Debug)]
    struct ProbeOp {
        kind: &'static str,
        item: usize,
        tag: &'static str,
        replace_same_item: bool,
    }

    impl ProbeOp {
        fn probe(item: usize, tag: &'static str) -> Box<dyn UndoOperation> {
            Box::new(ProbeOp {
                kind: "Probe",
                item,
                tag,
                replace_same_item: false,
            })
        }

        fn replacing(item: usize, tag: &'static str) -> Box<dyn UndoOperation> {
            Box::new(ProbeOp {
                kind: "Probe",
                item,
                tag,
                replace_same_item: true,
            })
        }
    }

    impl UndoOperation for ProbeOp {
        fn kind(&self) -> &'static str {
            self.kind
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn is_eq(&self, oth: &dyn UndoOperation) -> bool {
            match oth.as_any().downcast_ref::<ProbeOp>() {
                Some(o) => self.replace_same_item && self.item == o.item,
                None => false,
            }
        }

        fn undo(&self, world: &mut EditorWorld) {
            world.commentary.push(self.tag.to_string());
            // like the real operations: the inverse lands on the machinery
            world.undo_add(Box::new(ProbeOp {
                kind: self.kind,
                item: self.item,
                tag: self.tag,
                replace_same_item: self.replace_same_item,
            }));
        }
    }

    fn probe_world() -> EditorWorld {
        EditorWorld::new(ChunkSpace::new(0, Vec::new()))
    }

    #[test]
    fn the_machinery_seeds_one_open_barrier() {
        let machine = UndoRedo::default();
        assert!(!machine.can_undo());
        assert!(!machine.can_redo());
        assert_eq!(machine.undo_list.len(), 1);
        assert!(machine.undo_list[0].what.is_empty());
        // `new` builds the same seed
        assert_eq!(UndoRedo::new().undo_list.len(), machine.undo_list.len());
    }

    /// An operation that leaves `is_eq` at its default: never replaces.
    #[derive(Debug)]
    struct DefaultOp {
        tag: &'static str,
    }

    impl UndoOperation for DefaultOp {
        fn kind(&self) -> &'static str {
            "DefaultOp"
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn undo(&self, world: &mut EditorWorld) {
            world.commentary.push(self.tag.to_string());
        }
    }

    #[test]
    fn the_default_iseq_never_replaces() {
        assert!(DefaultOp { tag: "a" }.as_any().is::<DefaultOp>());
        assert!(!operations_equal(
            &DefaultOp { tag: "a" },
            &DefaultOp { tag: "b" }
        ));
        let mut machine = UndoRedo::new();
        machine.add(Box::new(DefaultOp { tag: "a" }));
        machine.add(Box::new(DefaultOp { tag: "b" }));
        assert_eq!(machine.undo_list[0].ops.len(), 2);
        // and a full round trip walks them like any operation
        let mut world = probe_world();
        world.undo_add(Box::new(DefaultOp { tag: "a" }));
        world.barrier("DEF", false);
        world.undo();
        assert_eq!(world.commentary, vec!["a"]);
    }

    #[test]
    fn replaceable_operations_dedup_keeping_the_original() {
        let mut machine = UndoRedo::new();
        machine.add(ProbeOp::replacing(3, "first"));
        machine.add(ProbeOp::replacing(3, "second"));
        machine.add(ProbeOp::replacing(4, "other item"));
        // same item replaces, the original is kept; a different item compounds
        let ops = &machine.undo_list[0].ops;
        assert_eq!(ops.len(), 2);
        let kept = ops[0].as_any().downcast_ref::<ProbeOp>().unwrap();
        assert_eq!(kept.tag, "first");
    }

    #[test]
    fn non_replaceable_operations_compound() {
        let mut machine = UndoRedo::new();
        machine.add(ProbeOp::probe(3, "a"));
        machine.add(ProbeOp::probe(3, "b"));
        assert_eq!(machine.undo_list[0].ops.len(), 2);
    }

    #[test]
    fn operations_equal_needs_matching_non_empty_kinds() {
        let existence = ExistenceOperation {
            item: 1,
            old_chunk: None,
        };
        let matrix = MatrixOperation {
            item: 1,
            old_chunk: None,
            old_pose: Matrix::identity(),
            replaced: None,
        };
        let matrix_too = MatrixOperation {
            item: 2,
            old_chunk: None,
            old_pose: Matrix::identity(),
            replaced: None,
        };
        // kind 0 (existence) never compares, not even with itself
        assert!(!operations_equal(&existence, &existence));
        assert!(!operations_equal(&existence, &matrix));
        // matrix compares per item
        assert!(!operations_equal(&matrix, &matrix_too));
        assert!(operations_equal(&matrix, &matrix));
        // through the dedup face too: both matrices compound (different
        // items) and both existences compound (they never compare)
        let mut machine = UndoRedo::new();
        machine.add(Box::new(matrix.clone()));
        machine.add(Box::new(matrix_too));
        machine.add(Box::new(existence.clone()));
        machine.add(Box::new(existence));
        assert_eq!(machine.undo_list[0].ops.len(), 4);
    }

    #[test]
    fn an_empty_barrier_warns_with_the_doubled_apostrophe() {
        let mut world = probe_world();
        world.barrier("FLIP", false);
        assert_eq!(
            world.undoredo.warnings,
            vec![
                "UndoRedo::barrier: Barrier closed for 'FLIP'' but no intermediate operations added!\n"
            ]
        );
        // the barrier still closed
        assert!(world.can_undo());
        assert_eq!(world.undo_info(0), "FLIP");
    }

    #[test]
    fn skip_if_no_change_closes_nothing() {
        let mut world = probe_world();
        world.barrier("SKIP", true);
        assert!(world.undoredo.warnings.is_empty());
        assert!(!world.can_undo());
        assert_eq!(world.undo_info(0), "");
        // the trace still recorded the call
        assert_eq!(world.undo_barriers, vec!["SKIP"]);
    }

    #[test]
    fn undo_walks_operations_in_reverse_and_rebuilds_redo() {
        let mut world = probe_world();
        world.undo_add(ProbeOp::probe(0, "one"));
        world.undo_add(ProbeOp::probe(1, "two"));
        world.barrier("PAIR", false);
        assert!(world.can_undo());

        world.undo();
        assert!(!world.can_undo());
        assert!(world.can_redo());
        // the walk ran the operations back to front
        assert_eq!(world.commentary, vec!["two", "one"]);
        // the redo entry carries the undone barrier's name and inverses
        assert_eq!(world.redo_info(0), "PAIR");
        assert_eq!(world.undoredo.redo_list[0].ops.len(), 2);

        world.redo();
        assert_eq!(world.commentary, vec!["two", "one", "one", "two"]);
        assert!(world.can_undo());
        assert!(!world.can_redo());
        assert_eq!(world.undo_info(0), "PAIR");
    }

    #[test]
    fn undo_with_nothing_to_undo_is_a_no_op() {
        let mut world = probe_world();
        world.undo();
        world.redo();
        assert!(!world.can_undo());
        assert!(!world.can_redo());
        assert!(world.commentary.is_empty());
    }

    #[test]
    fn info_levels_index_backwards_and_end_empty() {
        let mut world = probe_world();
        world.undo_add(ProbeOp::probe(0, "a"));
        world.barrier("FIRST", false);
        world.undo_add(ProbeOp::probe(0, "b"));
        world.barrier("SECOND", false);

        assert_eq!(world.undo_info(0), "SECOND");
        assert_eq!(world.undo_info(1), "FIRST");
        assert_eq!(world.undo_info(2), "");
        assert_eq!(world.redo_info(0), "");

        world.undo();
        assert_eq!(world.redo_info(0), "SECOND");
        assert_eq!(world.redo_info(1), "");
        assert_eq!(world.undo_info(0), "FIRST");
    }

    #[test]
    fn a_new_edit_drops_the_redo_branch() {
        let mut world = probe_world();
        world.undo_add(ProbeOp::probe(0, "a"));
        world.barrier("OLD", false);
        world.undo();
        assert!(world.can_redo());
        // moving forward again forgets the redos
        world.undo_add(ProbeOp::probe(0, "b"));
        world.barrier("NEW", false);
        assert!(!world.can_redo());
    }

    #[test]
    fn clear_resets_both_lists() {
        let mut world = probe_world();
        world.undo_add(ProbeOp::probe(0, "a"));
        world.barrier("OLD", false);
        world.undo();
        world.clear_undo();
        assert!(!world.can_undo());
        assert!(!world.can_redo());
        assert_eq!(world.undoredo.undo_list.len(), 1);
    }

    #[test]
    fn barrier_redo_rebuilds_the_barrier_name() {
        let mut world = probe_world();
        world.undo_add(ProbeOp::probe(0, "a"));
        world.barrier("STEP", false);
        world.undo();
        world.redo();
        assert_eq!(world.undo_info(0), "STEP");
    }
}
