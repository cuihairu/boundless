//! The Cell Viewer query protocol: what a viewer socket gets when it asks
//! this cellapp about its live spaces, cells and entities.
//!
//! Ported from `cellapp/cell_viewer_connection.cpp` (the whole request
//! switch) and `cellapp/cell_viewer_connection.hpp` (the `CellViewerExport`
//! command constants). The server half (`cell_viewer_server.cpp`) is the
//! accept/dispatch loop around these connections.
//!
//! Wire notes: every request starts with one printable-ASCII command byte.
//! Replies are raw concatenations — the length-prefixed `sendReply` form
//! is only used for the unknown-command answer (`uint32 0`).

use std::collections::BTreeMap;

/// `CellViewerExport::RequestTypes` — printable ASCII for easy debugging.
pub mod request {
    /// `GET_ENTITY_TYPE_NAMES`.
    pub const GET_ENTITY_TYPE_NAMES: u8 = b'n';
    /// `GET_ENTITIES`.
    pub const GET_ENTITIES: u8 = b'b';
    /// `GET_GRID`.
    pub const GET_GRID: u8 = b'e';
}

/// `CellViewerExport::EntitySelect`.
pub mod select {
    /// `REAL`.
    pub const REAL: u8 = b'0';
    /// `GHOST`.
    pub const GHOST: u8 = b'1';
}

/// One entity row the viewer can see: id, type id and the full position
/// (only x and z travel the wire).
#[derive(Clone, Debug, PartialEq)]
pub struct ViewerEntityRow {
    pub id: u32,
    pub entity_type_id: u16,
    pub position: (f32, f32, f32),
}

/// `Cell::realEntities()` — the reals this cell serves, in list order.
#[derive(Clone, Debug, Default)]
pub struct ViewerCell {
    pub reals: Vec<ViewerEntityRow>,
}

/// `space->spaceEntities()` — every entity this process serves in the
/// space, reals and ghosts, with the `isReal` flag the ghost query
/// filters on.
#[derive(Clone, Debug)]
pub struct ViewerEntitySlot {
    pub is_real: bool,
    pub row: ViewerEntityRow,
}

/// The physical space's bounds (`pPhysicalSpace()->subBounds()`) and,
/// when the backend is a chunk grid, its `gridSize()`.
#[derive(Clone, Debug)]
pub struct ViewerPhysical {
    pub min_bounds: (f32, f32, f32),
    pub max_bounds: (f32, f32, f32),
    /// `space->pChunkSpace()` — `None` falls back to
    /// `min(width, depth)` with the non-square warning.
    pub grid_size: Option<f32>,
}

/// The viewer's view of one space.
#[derive(Clone, Debug)]
pub struct ViewerSpace {
    /// `space->pCell()`.
    pub cell: Option<ViewerCell>,
    /// `space->spaceEntities()`.
    pub entities: Vec<ViewerEntitySlot>,
    pub physical: ViewerPhysical,
}

/// `CellApp` stand-in: the spaces and the entity type registry the
/// queries read.
#[derive(Clone, Debug, Default)]
pub struct ViewerApp {
    pub spaces: BTreeMap<u32, ViewerSpace>,
    /// `EntityType::getTypes()` names, in registry order.
    pub entity_type_names: Vec<String>,
}

impl ViewerApp {
    /// `CellApp::findSpace`.
    pub fn find_space(&self, space_id: u32) -> Option<&ViewerSpace> {
        self.spaces.get(&space_id)
    }
}

/// One viewer socket's accumulated inbox and its warnings.
#[derive(Default)]
pub struct CellViewerConnection {
    inbox: Vec<u8>,
    /// WARNING_MSG records the reference would log.
    pub warnings: Vec<String>,
}

impl CellViewerConnection {
    /// Bytes off the socket land here before the dispatcher runs.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.inbox.extend_from_slice(bytes);
    }

    /// `CellViewerConnection::handleInputNotification` — process one
    /// buffered command; `None` when the inbox is empty (nothing to do).
    /// A read error or EOF is the caller's concern (`delete this` in the
    /// reference); the leftover buffer is the next notification.
    pub fn handle_input(&mut self, app: &ViewerApp) -> Option<Vec<u8>> {
        let cmd = *self.inbox.first()?;
        self.inbox.remove(0);
        let mut reply = Vec::new();
        match cmd {
            request::GET_ENTITY_TYPE_NAMES => {
                let num_types = app.entity_type_names.len() as u32;
                reply.extend_from_slice(&num_types.to_le_bytes());
                for name in &app.entity_type_names {
                    let name_size = name.len() as u16;
                    reply.extend_from_slice(&name_size.to_le_bytes());
                    reply.extend_from_slice(name.as_bytes());
                }
            }
            request::GET_ENTITIES => {
                let which = self.take_byte();
                let space_id = self.take_u32();
                match app
                    .find_space(space_id)
                    .filter(|space| space.cell.is_some())
                {
                    Some(space) => {
                        let rows: Vec<ViewerEntityRow> = if which == select::REAL {
                            // `pCell->realEntities()`.
                            space
                                .cell
                                .as_ref()
                                .map(|cell| cell.reals.clone())
                                .unwrap_or_default()
                        } else {
                            // `spaceEntities()` filtered to `!isReal()`.
                            space
                                .entities
                                .iter()
                                .filter(|slot| !slot.is_real)
                                .map(|slot| slot.row.clone())
                                .collect()
                        };
                        let nrec = rows.len() as u32;
                        reply.extend_from_slice(&nrec.to_le_bytes());
                        for row in rows {
                            reply.extend_from_slice(&row.position.0.to_le_bytes());
                            reply.extend_from_slice(&row.position.2.to_le_bytes());
                            reply.extend_from_slice(&row.entity_type_id.to_le_bytes());
                            reply.extend_from_slice(&row.id.to_le_bytes());
                        }
                    }
                    None => {
                        let error_code: i32 = -1;
                        reply.extend_from_slice(&error_code.to_le_bytes());
                    }
                }
            }
            request::GET_GRID => {
                let space_id = self.take_u32();
                match app
                    .find_space(space_id)
                    .filter(|space| space.cell.is_some())
                {
                    Some(space) => {
                        let ret: u32 = 1;
                        reply.extend_from_slice(&ret.to_le_bytes());
                        for v in [
                            space.physical.min_bounds.0,
                            space.physical.min_bounds.1,
                            space.physical.min_bounds.2,
                            space.physical.max_bounds.0,
                            space.physical.max_bounds.1,
                            space.physical.max_bounds.2,
                        ] {
                            reply.extend_from_slice(&v.to_le_bytes());
                        }
                        let width = space.physical.max_bounds.0 - space.physical.min_bounds.0;
                        let depth = space.physical.max_bounds.2 - space.physical.min_bounds.2;
                        let grid_size = match space.physical.grid_size {
                            Some(g) => g,
                            None => {
                                if width > depth || depth > width {
                                    self.warnings.push(
                                        "physical space is neither a grid nor a square".to_string(),
                                    );
                                }
                                width.min(depth)
                            }
                        };
                        reply.extend_from_slice(&grid_size.to_le_bytes());
                        let num_to_send: u32 = 0;
                        reply.extend_from_slice(&num_to_send.to_le_bytes());
                    }
                    None => {
                        let ret: u32 = 0;
                        reply.extend_from_slice(&ret.to_le_bytes());
                    }
                }
            }
            _ => {
                // The reference drains the socket, then answers with an
                // empty length-prefixed reply.
                self.inbox.clear();
                let len: u32 = 0;
                reply.extend_from_slice(&len.to_le_bytes());
            }
        }
        Some(reply)
    }

    fn take_byte(&mut self) -> u8 {
        if self.inbox.is_empty() {
            return 0;
        }
        self.inbox.remove(0)
    }

    fn take_u32(&mut self) -> u32 {
        let mut buf = [0u8; 4];
        for slot in buf.iter_mut() {
            *slot = self.take_byte();
        }
        u32::from_le_bytes(buf)
    }
}

/// `CellViewerServer` — the accept/dispatch shell: one connection per
/// file descriptor, deleted when its read fails or hits EOF.
#[derive(Default)]
pub struct CellViewerServer {
    pub connections: BTreeMap<u32, CellViewerConnection>,
    /// INFO_MSG records for the connections the reference deletes.
    pub deleted: Vec<u32>,
}

impl CellViewerServer {
    /// `CellViewerServer::handleInputNotification` — route a read to the
    /// connection. An empty slice is EOF: the connection goes away with
    /// no reply. A failed read (the reference's `-1`) closes it too; the
    /// harness only sees EOF.
    pub fn handle_input_notification(
        &mut self,
        fd: u32,
        app: &ViewerApp,
        bytes: &[u8],
    ) -> Option<Vec<u8>> {
        if bytes.is_empty() {
            self.connections.remove(&fd);
            self.deleted.push(fd);
            return None;
        }
        let connection = self.connections.entry(fd).or_default();
        connection.feed(bytes);
        connection.handle_input(app)
    }
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u32, etype: u16, x: f32, z: f32) -> ViewerEntityRow {
        ViewerEntityRow {
            id,
            entity_type_id: etype,
            position: (x, 0.0, z),
        }
    }

    /// The standing fixture: space 9 with a cell holding reals 10 and 11,
    /// plus ghosts 21 and 22 (and a real) in the space's own list.
    fn app() -> ViewerApp {
        let mut app = ViewerApp {
            entity_type_names: vec!["NPC".to_string(), "Avatar".to_string()],
            ..ViewerApp::default()
        };
        let space = ViewerSpace {
            cell: Some(ViewerCell {
                reals: vec![row(10, 1, 1.5, 2.5), row(11, 2, -3.0, 7.0)],
            }),
            entities: vec![
                ViewerEntitySlot {
                    is_real: true,
                    row: row(10, 1, 1.5, 2.5),
                },
                ViewerEntitySlot {
                    is_real: false,
                    row: row(21, 1, 5.0, 5.0),
                },
                ViewerEntitySlot {
                    is_real: false,
                    row: row(22, 2, 6.0, 6.0),
                },
            ],
            physical: ViewerPhysical {
                min_bounds: (-100.0, 0.0, -200.0),
                max_bounds: (100.0, 0.0, 200.0),
                grid_size: Some(100.0),
            },
        };
        app.spaces.insert(9, space);
        app
    }

    #[test]
    fn type_names_go_out_count_first_then_pascal_strings() {
        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_ENTITY_TYPE_NAMES]);
        let reply = connection.handle_input(&app()).unwrap();
        let mut want = 2u32.to_le_bytes().to_vec();
        want.extend_from_slice(&3u16.to_le_bytes());
        want.extend_from_slice(b"NPC");
        want.extend_from_slice(&6u16.to_le_bytes());
        want.extend_from_slice(b"Avatar");
        assert_eq!(reply, want);
    }

    #[test]
    fn the_real_query_lists_the_cell_reals_x_z_type_id() {
        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_ENTITIES, select::REAL, 9, 0, 0, 0]);
        let reply = connection.handle_input(&app()).unwrap();
        let mut want = 2u32.to_le_bytes().to_vec();
        for r in [row(10, 1, 1.5, 2.5), row(11, 2, -3.0, 7.0)] {
            want.extend_from_slice(&r.position.0.to_le_bytes());
            want.extend_from_slice(&r.position.2.to_le_bytes());
            want.extend_from_slice(&r.entity_type_id.to_le_bytes());
            want.extend_from_slice(&r.id.to_le_bytes());
        }
        assert_eq!(reply, want);
    }

    #[test]
    fn the_ghost_query_walks_the_space_list_past_the_reals() {
        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_ENTITIES, select::GHOST, 9, 0, 0, 0]);
        let reply = connection.handle_input(&app()).unwrap();
        let mut want = 2u32.to_le_bytes().to_vec();
        for r in [row(21, 1, 5.0, 5.0), row(22, 2, 6.0, 6.0)] {
            want.extend_from_slice(&r.position.0.to_le_bytes());
            want.extend_from_slice(&r.position.2.to_le_bytes());
            want.extend_from_slice(&r.entity_type_id.to_le_bytes());
            want.extend_from_slice(&r.id.to_le_bytes());
        }
        assert_eq!(reply, want);
    }

    #[test]
    fn a_missing_space_or_cell_answers_minus_one() {
        let mut connection = CellViewerConnection::default();
        // No such space.
        connection.feed(&[request::GET_ENTITIES, select::REAL, 7, 0, 0, 0]);
        assert_eq!(
            connection.handle_input(&app()).unwrap(),
            (-1i32).to_le_bytes().to_vec()
        );

        // The space exists but carries no cell here.
        let mut bare = app();
        bare.spaces.get_mut(&9).unwrap().cell = None;
        connection.feed(&[request::GET_ENTITIES, select::GHOST, 9, 0, 0, 0]);
        assert_eq!(
            connection.handle_input(&bare).unwrap(),
            (-1i32).to_le_bytes().to_vec()
        );
    }

    #[test]
    fn the_grid_query_reports_bounds_and_grid_size() {
        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_GRID, 9, 0, 0, 0]);
        let reply = connection.handle_input(&app()).unwrap();
        let mut want = 1u32.to_le_bytes().to_vec();
        for v in [-100.0f32, 0.0, -200.0, 100.0, 0.0, 200.0, 100.0] {
            want.extend_from_slice(&v.to_le_bytes());
        }
        want.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(reply, want);
    }

    #[test]
    fn a_chunkless_grid_falls_back_to_the_shorter_side() {
        let mut square = app();
        {
            let physical = &mut square.spaces.get_mut(&9).unwrap().physical;
            physical.grid_size = None;
            // Zero the depth floor: 200 wide by 200 deep, a true square.
            physical.min_bounds.2 = 0.0;
        }
        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_GRID, 9, 0, 0, 0]);
        let reply = connection.handle_input(&square).unwrap();
        let size = f32::from_le_bytes(reply[28..32].try_into().unwrap());
        assert_eq!(size, 200.0);
        assert!(connection.warnings.is_empty());

        // A non-square space takes the minimum and warns (width 200,
        // depth 400).
        let mut lopsided = app();
        {
            let physical = &mut lopsided.spaces.get_mut(&9).unwrap().physical;
            physical.grid_size = None;
        }
        connection.feed(&[request::GET_GRID, 9, 0, 0, 0]);
        let reply = connection.handle_input(&lopsided).unwrap();
        let size = f32::from_le_bytes(reply[28..32].try_into().unwrap());
        assert_eq!(size, 200.0);
        assert_eq!(connection.warnings.len(), 1);
    }

    #[test]
    fn a_missing_space_or_cell_answers_zero_for_the_grid() {
        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_GRID, 7, 0, 0, 0]);
        assert_eq!(
            connection.handle_input(&app()).unwrap(),
            0u32.to_le_bytes().to_vec()
        );

        let mut bare = app();
        bare.spaces.get_mut(&9).unwrap().cell = None;
        connection.feed(&[request::GET_GRID, 9, 0, 0, 0]);
        assert_eq!(
            connection.handle_input(&bare).unwrap(),
            0u32.to_le_bytes().to_vec()
        );
    }

    #[test]
    fn an_unknown_command_empties_the_socket_and_answers_nothing() {
        let mut connection = CellViewerConnection::default();
        connection.feed(&[b'z', 1, 2, 3]);
        assert_eq!(
            connection.handle_input(&app()).unwrap(),
            0u32.to_le_bytes().to_vec()
        );
        // The drain took the trailing bytes with it.
        connection.feed(&[request::GET_ENTITY_TYPE_NAMES]);
        let reply = connection.handle_input(&app()).unwrap();
        assert_eq!(reply[..4], 2u32.to_le_bytes());
    }

    #[test]
    fn pipelined_commands_answer_in_order() {
        let mut connection = CellViewerConnection::default();
        connection.feed(&[
            request::GET_ENTITIES,
            select::REAL,
            9,
            0,
            0,
            0,
            request::GET_ENTITY_TYPE_NAMES,
        ]);
        let first = connection.handle_input(&app()).unwrap();
        let second = connection.handle_input(&app()).unwrap();
        assert_eq!(first[..4], 2u32.to_le_bytes());
        assert_eq!(second[..4], 2u32.to_le_bytes());
    }

    #[test]
    fn an_empty_inbox_has_nothing_to_say() {
        let mut connection = CellViewerConnection::default();
        assert!(connection.handle_input(&app()).is_none());
    }

    #[test]
    fn a_truncated_request_zero_fills_the_rest() {
        // The command byte arrives without its payload; the reads fall
        // back to zeros (which = 0, space 0) and the space lookup fails.
        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_ENTITIES]);
        assert_eq!(
            connection.handle_input(&app()).unwrap(),
            (-1i32).to_le_bytes().to_vec()
        );
    }

    #[test]
    fn the_server_tracks_connections_and_deletes_them_at_eof() {
        let mut server = CellViewerServer::default();
        let reply = server
            .handle_input_notification(4, &app(), &[request::GET_ENTITY_TYPE_NAMES])
            .unwrap();
        assert_eq!(reply[..4], 2u32.to_le_bytes());
        assert!(server.connections.contains_key(&4));

        // EOF deletes the connection without a reply.
        assert!(server.handle_input_notification(4, &app(), &[]).is_none());
        assert!(!server.connections.contains_key(&4));
        assert_eq!(server.deleted, vec![4]);
    }

    #[test]
    fn the_live_space_state_drives_the_rows() {
        // The acceptance face: the queries read exactly what the live
        // Space/Cell carry. The fixture's space list is the source of
        // truth for ghosts; the cell's realEntities for reals — so a real
        // appearing only in the space list is invisible to both queries,
        // and a ghost promoted to real moves between the answers.
        let mut live = app();
        {
            let space = live.spaces.get_mut(&9).unwrap();
            // Ghost 22 becomes a real on this cell.
            space.entities[2].is_real = true;
            space
                .cell
                .as_mut()
                .unwrap()
                .reals
                .push(row(22, 2, 6.0, 6.0));
        }

        let mut connection = CellViewerConnection::default();
        connection.feed(&[request::GET_ENTITIES, select::REAL, 9, 0, 0, 0]);
        let reply = connection.handle_input(&live).unwrap();
        let nrec = u32::from_le_bytes(reply[..4].try_into().unwrap());
        assert_eq!(nrec, 3);
        let last_id = u32::from_le_bytes(reply[reply.len() - 4..].try_into().unwrap());
        assert_eq!(last_id, 22);

        connection.feed(&[request::GET_ENTITIES, select::GHOST, 9, 0, 0, 0]);
        let reply = connection.handle_input(&live).unwrap();
        let nrec = u32::from_le_bytes(reply[..4].try_into().unwrap());
        assert_eq!(nrec, 1);
        let id = u32::from_le_bytes(reply[reply.len() - 4..].try_into().unwrap());
        assert_eq!(id, 21);
    }
}
