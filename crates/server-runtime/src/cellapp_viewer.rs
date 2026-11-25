//! The Cell Viewer queries: the three ASCII commands the viewer
//! protocol speaks, answered off the live Space/Cell state.
//!
//! Reference faces:
//! - `server/cellapp/cell_viewer_connection.cpp` — `handleInputNotification`
//!   (:56): the command dispatch, `GET_ENTITY_TYPE_NAMES`' registry walk,
//!   `GET_ENTITIES`' real/ghost listings, `GET_GRID`'s bounds and grid
//!   size, and the unknown command's drain-and-empty reply;
//!   `receiveData` (:313) with its wrong-number-of-bytes WARNING;
//!   `sendReply`/`sendReply_brief` (:338/:354) — the length-prefixed
//!   and raw reply framings the port's byte streams carry verbatim.
//! - `server/cellapp/cell_viewer_server.cpp` — the TCP listener the
//!   connections hang off (the transport face; the queries are the
//!   port's deliverable).
//! - `EntityType::getTypes()` — the names registry
//!   ([`CellApp::entity_type_names`]) the type-names query walks.

use crate::cellapp_space::{CellApp, Entity};
use std::cell::RefCell;
use std::rc::Rc;

/// `CellViewerExport::GET_ENTITY_TYPE_NAMES` = `'n'`.
pub const GET_ENTITY_TYPE_NAMES: u8 = b'n';
/// `CellViewerExport::GET_ENTITIES` = `'b'`.
pub const GET_ENTITIES: u8 = b'b';
/// `CellViewerExport::GET_GRID` = `'e'`.
pub const GET_GRID: u8 = b'e';
/// `CellViewerExport::EntitySelect::REAL` = `'0'`.
pub const VIEWER_REAL: u8 = b'0';
/// `CellViewerExport::EntitySelect::GHOST` = `'1'`.
pub const VIEWER_GHOST: u8 = b'1';

// ---------------------------------------------------------------------------
// Section: the command dispatch
// ---------------------------------------------------------------------------

impl CellApp {
    /// `CellViewerConnection::handleInputNotification` (:56): one
    /// command's worth of input bytes answers one reply byte stream —
    /// `sendReply_brief` appends raw, `sendReply` appends the `u32`
    /// length first, exactly as the socket would have carried them.
    /// [`None`] is the peer's close (the reference deletes the
    /// connection).
    pub fn handle_viewer_input(&mut self, input: &[u8]) -> Option<Vec<u8>> {
        let Some(&command) = input.first() else {
            // bytesRead == 0: the peer closed; the connection deletes
            // itself.
            return None;
        };
        let mut reply = Vec::new();
        let mut cursor = 1;
        match command {
            GET_ENTITY_TYPE_NAMES => {
                // The registry's names, the length sent before each
                // string.
                let num_types = self.entity_type_names.len() as u32;
                reply.extend_from_slice(&num_types.to_le_bytes());
                for name in self.entity_type_names.values() {
                    reply.extend_from_slice(&(name.len() as u16).to_le_bytes());
                    reply.extend_from_slice(name.as_bytes());
                }
            }
            GET_ENTITIES => {
                let which = self.viewer_receive_u8(input, &mut cursor);
                let space_id = self.viewer_receive_i32(input, &mut cursor);
                match self.spaces.get(&(space_id as u32)) {
                    None => {
                        // WARNING "space not found."
                        self.viewer_notes.push("space not found.".to_string());
                        self.reply_minus_one(&mut reply);
                    }
                    Some(space) => match space.cell() {
                        None => {
                            // WARNING "cell not found."
                            self.viewer_notes.push("cell not found.".to_string());
                            self.reply_minus_one(&mut reply);
                        }
                        Some(_) => {
                            // The count and the records read the same
                            // list: the reference's `realEntities()`
                            // and the space's entities agree in
                            // membership, and the ghost arm walks the
                            // space's list twice itself. Anything but
                            // `'0'` takes the ghost arm.
                            let want_real = which == VIEWER_REAL;
                            let entities = space.space_entities().to_vec();
                            let picked: Vec<Rc<RefCell<Entity>>> = entities
                                .into_iter()
                                .filter(|entity| entity.borrow().is_real == want_real)
                                .collect();
                            reply.extend_from_slice(&(picked.len() as u32).to_le_bytes());
                            for entity in picked {
                                reply.extend_from_slice(&viewer_entity_record(&entity));
                            }
                        }
                    },
                }
            }
            GET_GRID => {
                let space_id = self.viewer_receive_u32(input, &mut cursor);
                match self.spaces.get(&space_id) {
                    None => {
                        // ERROR "space == NULL"
                        self.viewer_notes.push("space == NULL".to_string());
                        reply.extend_from_slice(&0u32.to_le_bytes());
                    }
                    Some(space) => match space.cell() {
                        None => {
                            // ERROR "pCell == NULL"
                            self.viewer_notes.push("pCell == NULL".to_string());
                            reply.extend_from_slice(&0u32.to_le_bytes());
                        }
                        Some(_) => {
                            let reading = space
                                .physical()
                                .map(|physical| physical.borrow().sub_bounds());
                            let ((min, max), grid_size) =
                                reading.unwrap_or((([0.0; 3], [0.0; 3]), None));
                            reply.extend_from_slice(&1u32.to_le_bytes());
                            for value in min.iter().copied().chain(max.iter().copied()) {
                                reply.extend_from_slice(&value.to_le_bytes());
                            }
                            let grid_size = match grid_size {
                                Some(size) => size,
                                None => {
                                    let width = max[0] - min[0];
                                    let depth = max[2] - min[2];
                                    if width != depth {
                                        // WARNING "physical space is
                                        // neither a grid nor a square."
                                        self.viewer_notes.push(
                                            "physical space is neither a grid nor a square."
                                                .to_string(),
                                        );
                                    }
                                    width.min(depth)
                                }
                            };
                            reply.extend_from_slice(&grid_size.to_le_bytes());
                            // The loaded-grid-positions block is gone in
                            // the reference too (`#if 0`): a zero count.
                            reply.extend_from_slice(&0u32.to_le_bytes());
                        }
                    },
                }
            }
            _ => {
                // Unknown command: the stream is drained (the port's
                // input is already the whole read) and a bare empty
                // frame answers.
                reply.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        Some(reply)
    }

    /// `sendReply( &errorCode, sizeof( errorCode ) )` with `-1`.
    fn reply_minus_one(&self, reply: &mut Vec<u8>) {
        reply.extend_from_slice(&4u32.to_le_bytes());
        reply.extend_from_slice(&(-1i32).to_le_bytes());
    }

    /// `receiveData( A & )`'s one-byte read: a short input warns and
    /// lands on zero, the WARNING face of the non-blocking socket read.
    fn viewer_receive_u8(&mut self, input: &[u8], cursor: &mut usize) -> u8 {
        if *cursor < input.len() {
            let value = input[*cursor];
            *cursor += 1;
            value
        } else {
            self.viewer_notes
                .push("Received wrong number of bytes.".to_string());
            0
        }
    }

    /// `receiveData( A & )`'s four-byte read.
    fn viewer_receive_u32(&mut self, input: &[u8], cursor: &mut usize) -> u32 {
        if *cursor + 4 <= input.len() {
            let value =
                u32::from_le_bytes(input[*cursor..*cursor + 4].try_into().expect("4 bytes"));
            *cursor += 4;
            value
        } else {
            self.viewer_notes
                .push("Received wrong number of bytes.".to_string());
            0
        }
    }

    /// `receiveData( A & )`'s signed four-byte read (`SpaceID`).
    fn viewer_receive_i32(&mut self, input: &[u8], cursor: &mut usize) -> i32 {
        self.viewer_receive_u32(input, cursor) as i32
    }
}

/// One entity record of the listings: `x`, `z`, the u16 type id and the
/// id — `entityTypeID()` is a `u16` in the reference, so the truncation
/// is the type system's own.
fn viewer_entity_record(entity: &Rc<RefCell<Entity>>) -> Vec<u8> {
    let borrowed = entity.borrow();
    let mut record = Vec::with_capacity(14);
    record.extend_from_slice(&borrowed.position[0].to_le_bytes());
    record.extend_from_slice(&borrowed.position[2].to_le_bytes());
    record.extend_from_slice(&(borrowed.type_id as u16).to_le_bytes());
    record.extend_from_slice(&borrowed.id.to_le_bytes());
    record
}

// ---------------------------------------------------------------------------
// Section: the tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellapp_real_entity::EntityScript;
    use crate::cellapp_space::{Cell, CellInfo, CellInfoNode, Space, SpaceWorldFactory};
    use crate::cellappmgr::SpaceId;
    use std::collections::BTreeMap;

    const SID: SpaceId = 3;
    const TYPE: u32 = 7;

    /// A cell on the space, so the entity and grid queries take the
    /// found-cell path.
    fn a_cell() -> Rc<RefCell<Cell>> {
        Rc::new(RefCell::new(Cell {
            info: CellInfo {
                space_id: SID,
                app_addr: 42,
                node: CellInfoNode::first(),
                rect: (0.0, 0.0, 0.0, 0.0),
                is_delete_pending: false,
            },
            space_gone_count: 0,
            reuse_count: 0,
            should_offload: true,
            is_removed: false,
        }))
    }

    /// The creation stream: id + type, position, the on-ground flag,
    /// direction, last event number, base address, witness byte.
    fn create_stream(id: u32, pos: [f32; 3]) -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(&id.to_le_bytes());
        stream.extend_from_slice(&TYPE.to_le_bytes());
        for value in pos {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.push(crate::cellapp_real_entity::ADD_ENTITY_DATA_IS_ON_GROUND);
        for value in [0.0_f32; 3] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        stream.extend_from_slice(&42u32.to_le_bytes());
        stream.extend_from_slice(&7u32.to_le_bytes());
        stream.extend_from_slice(&6502u16.to_le_bytes());
        stream.push(b'-');
        stream
    }

    /// A started space with a cell and two reals at known positions.
    fn fixture() -> CellApp {
        let mut app = CellApp::default();
        let mut spaces = BTreeMap::new();
        let mut space = Space::new(SID);
        space.set_cell(a_cell());
        spaces.insert(SID, space);
        app.entity_scripts
            .insert(TYPE, EntityScript::with_init(vec![]));
        app.create_real_entity(&mut spaces, SID, &create_stream(1, [1.5, 2.0, 3.5]));
        app.create_real_entity(&mut spaces, SID, &create_stream(2, [3000.0, 0.0, 7.0]));
        app.spaces = spaces;
        app
    }

    fn entities_request(which: u8, space_id: i32) -> Vec<u8> {
        let mut request = vec![GET_ENTITIES, which];
        request.extend_from_slice(&space_id.to_le_bytes());
        request
    }

    fn grid_request(space_id: u32) -> Vec<u8> {
        let mut request = vec![GET_GRID];
        request.extend_from_slice(&space_id.to_le_bytes());
        request
    }

    /// One entity record: `x`, `z`, `etype` (u16), `id`.
    fn record(pos: [f32; 3], id: u32) -> Vec<u8> {
        let mut record = Vec::new();
        record.extend_from_slice(&pos[0].to_le_bytes());
        record.extend_from_slice(&pos[2].to_le_bytes());
        record.extend_from_slice(&(TYPE as u16).to_le_bytes());
        record.extend_from_slice(&id.to_le_bytes());
        record
    }

    #[test]
    fn the_type_names_reply_lists_the_registry() {
        let mut app = fixture();
        app.entity_type_names.insert(1, "avatar".to_string());
        app.entity_type_names.insert(2, "npc".to_string());
        let reply = app
            .handle_viewer_input(&[GET_ENTITY_TYPE_NAMES])
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(&6u16.to_le_bytes());
        expected.extend_from_slice(b"avatar");
        expected.extend_from_slice(&3u16.to_le_bytes());
        expected.extend_from_slice(b"npc");
        assert_eq!(reply, expected);
    }

    #[test]
    fn the_real_entities_reply_matches_the_live_space() {
        let mut app = fixture();
        let reply = app
            .handle_viewer_input(&entities_request(VIEWER_REAL, SID as i32))
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(&record([1.5, 2.0, 3.5], 1));
        expected.extend_from_slice(&record([3000.0, 0.0, 7.0], 2));
        assert_eq!(reply, expected);
        assert!(app.viewer_notes.is_empty());
    }

    #[test]
    fn the_ghost_entities_reply_lists_the_not_real_ones() {
        let mut app = fixture();
        // Entity 2 turned ghost: the real listing drops it, the ghost
        // listing carries it alone.
        app.entity_population[&2].borrow_mut().is_real = false;
        let real = app
            .handle_viewer_input(&entities_request(VIEWER_REAL, SID as i32))
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&record([1.5, 2.0, 3.5], 1));
        assert_eq!(real, expected);
        let ghost = app
            .handle_viewer_input(&entities_request(VIEWER_GHOST, SID as i32))
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&record([3000.0, 0.0, 7.0], 2));
        assert_eq!(ghost, expected);
    }

    #[test]
    fn a_missing_space_or_cell_answers_minus_one() {
        let mut app = fixture();
        let reply = app
            .handle_viewer_input(&entities_request(VIEWER_REAL, 99))
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(reply, expected);
        assert_eq!(app.viewer_notes, vec!["space not found.".to_string()]);
        // A space without a cell takes the same answer with the cell's
        // WARNING.
        let mut app = fixture();
        app.spaces.insert(9, Space::new(9));
        let reply = app
            .handle_viewer_input(&entities_request(VIEWER_REAL, 9))
            .expect("a reply");
        assert_eq!(reply, expected);
        assert_eq!(app.viewer_notes, vec!["cell not found.".to_string()]);
    }

    #[test]
    fn the_grid_reply_carries_bounds_and_grid_size() {
        let mut app = fixture();
        {
            let physical = app.spaces[&SID].physical().expect("the backend");
            let mut borrowed = physical.borrow_mut();
            borrowed.set_sub_bounds([0.0, -10.0, 5.0], [200.0, 10.0, 205.0]);
            borrowed.set_grid_size(50.0);
        }
        let reply = app
            .handle_viewer_input(&grid_request(SID))
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u32.to_le_bytes());
        for value in [0.0_f32, -10.0, 5.0, 200.0, 10.0, 205.0] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&50.0_f32.to_le_bytes());
        expected.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(reply, expected);
        assert!(app.viewer_notes.is_empty());
    }

    #[test]
    fn the_grid_falls_back_to_the_bounding_square() {
        // The delegate backend has no chunk grid: the grid size is the
        // smaller of the bounds' width and depth, and a non-square
        // bounds warns.
        let mut app = CellApp::default();
        let mut spaces = BTreeMap::new();
        let mut space =
            Space::new_delegate(SID, Rc::new(RefCell::new(SpaceWorldFactory::default())));
        space.set_cell(a_cell());
        spaces.insert(SID, space);
        app.spaces = spaces;
        {
            let physical = app.spaces[&SID].physical().expect("the backend");
            let mut borrowed = physical.borrow_mut();
            borrowed.set_sub_bounds([0.0; 3], [200.0, 0.0, 100.0]);
            // No grid slot on the delegate backend — setting one is a no-op.
            borrowed.set_grid_size(50.0);
        }
        let reply = app
            .handle_viewer_input(&grid_request(SID))
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u32.to_le_bytes());
        for value in [0.0_f32, 0.0, 0.0, 200.0, 0.0, 100.0] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&100.0_f32.to_le_bytes());
        expected.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(reply, expected);
        assert_eq!(
            app.viewer_notes,
            vec!["physical space is neither a grid nor a square.".to_string()]
        );
        // A square bounds takes the silent branch — no warning.
        app.spaces[&SID]
            .physical()
            .expect("the backend")
            .borrow_mut()
            .set_sub_bounds([0.0; 3], [100.0, 0.0, 100.0]);
        app.viewer_notes.clear();
        let reply = app
            .handle_viewer_input(&grid_request(SID))
            .expect("a reply");
        assert_eq!(&reply[28..32], &100.0_f32.to_le_bytes());
        assert!(app.viewer_notes.is_empty());
    }

    #[test]
    fn a_missing_space_or_cell_answers_zero_on_the_grid_query() {
        let mut app = fixture();
        let reply = app.handle_viewer_input(&grid_request(99)).expect("a reply");
        assert_eq!(reply, 0u32.to_le_bytes().to_vec());
        assert_eq!(app.viewer_notes, vec!["space == NULL".to_string()]);
        let mut app = fixture();
        app.spaces.insert(9, Space::new(9));
        let reply = app.handle_viewer_input(&grid_request(9)).expect("a reply");
        assert_eq!(reply, 0u32.to_le_bytes().to_vec());
        assert_eq!(app.viewer_notes, vec!["pCell == NULL".to_string()]);
    }

    #[test]
    fn an_unknown_command_drains_and_answers_an_empty_frame() {
        let mut app = fixture();
        let reply = app.handle_viewer_input(&[b'z', 1, 2, 3]).expect("a reply");
        assert_eq!(reply, 0u32.to_le_bytes().to_vec());
        assert!(app.viewer_notes.is_empty());
    }

    #[test]
    fn an_empty_input_closes_the_connection() {
        let mut app = fixture();
        assert!(app.handle_viewer_input(b"").is_none());
    }

    #[test]
    fn a_short_entities_request_warns_and_reads_zeroes() {
        let mut app = fixture();
        // No space id bytes follow: `receiveData` warns and the read
        // lands on zero — the space lookup then fails.
        let reply = app
            .handle_viewer_input(&[GET_ENTITIES, VIEWER_REAL])
            .expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(reply, expected);
        assert_eq!(
            app.viewer_notes,
            vec![
                "Received wrong number of bytes.".to_string(),
                "space not found.".to_string(),
            ]
        );
    }

    #[test]
    fn a_bare_entities_request_warns_on_both_reads() {
        let mut app = fixture();
        // Neither the selector nor the space id follow: both reads warn
        // and land on zero — the ghost selector and the space lookup on
        // zero both fail.
        let reply = app.handle_viewer_input(&[GET_ENTITIES]).expect("a reply");
        let mut expected = Vec::new();
        expected.extend_from_slice(&4u32.to_le_bytes());
        expected.extend_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(reply, expected);
        assert_eq!(
            app.viewer_notes,
            vec![
                "Received wrong number of bytes.".to_string(),
                "Received wrong number of bytes.".to_string(),
                "space not found.".to_string(),
            ]
        );
    }
}
