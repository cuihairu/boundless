//! BW-08-06-03: the Cell Viewer queries.
//!
//! The Cell Viewer is a socket client a developer connects to a CellApp to
//! watch it: it asks for the entity type names, for the real or ghost
//! entities of a space with their positions and types, and for the space's
//! grid. Every reply is computed from the live state at the moment of the
//! query.
//!
//! The acceptance, "与实时 Space/Cell 状态匹配", is that each reply equals
//! the state at query time: entities added, moved, removed or handed off,
//! and a cell lost, all show up in the very next query.
//!
//! Reference:
//! - `cell_viewer_connection.hpp:17-32` — the protocol: printable ASCII
//!   commands `n` (type names), `b` (entities) and `e` (grid), with the
//!   entity select `0` = real and `1` = ghost, each followed by a SpaceID.
//! - `cell_viewer_connection.cpp:57-84` — `GET_ENTITY_TYPE_NAMES`: a u32
//!   count, then per type a u16 Pascal string.
//! - `cell_viewer_connection.cpp:86-187` — `GET_ENTITIES`: an unknown space
//!   or a space without a cell replies the i32 error code -1; otherwise a
//!   u32 count and one record per entity — f32 x, f32 z, u16 type, u32 id —
//!   reals from the cell's `realEntities()` (`:122-152`), ghosts from the
//!   space's entities where `!isReal()` (`:154-183`).
//! - `cell_viewer_connection.cpp:189-298` — `GET_GRID`: an unknown space or
//!   cell replies a u32 0; otherwise a u32 1, the physical space's bounding
//!   box as six f32s, the grid size as an f32, and a u32 0 where the loaded
//!   grid positions used to be (`:280-284`, the block is `#if 0`).
//! - `cell_viewer_connection.cpp:303-313` — an unknown command drains the
//!   stream and replies a zero-length `sendReply(NULL, 0)`.
//! - `cell_viewer_server.cpp` — the listening server itself, which owns one
//!   connection per client.

use std::collections::BTreeMap;

use crate::range_list::EntityId;

/// `GET_ENTITY_TYPE_NAMES` (`cell_viewer_connection.hpp:24`).
pub const GET_ENTITY_TYPE_NAMES: u8 = b'n';
/// `GET_ENTITIES` (`cell_viewer_connection.hpp:25`).
pub const GET_ENTITIES: u8 = b'b';
/// `GET_GRID` (`cell_viewer_connection.hpp:26`).
pub const GET_GRID: u8 = b'e';

/// The `EntitySelect` for the cell's reals (`cell_viewer_connection.hpp:31`).
pub const REAL: u8 = b'0';
/// The `EntitySelect` for the space's ghosts
/// (`cell_viewer_connection.hpp:32`).
pub const GHOST: u8 = b'1';

/// The reply to a query against a space that is not there
/// (`cell_viewer_connection.cpp:152-156`, `:171-175`).
pub const SPACE_NOT_FOUND: i32 = -1;

/// One entity as the viewer reports it: the record of
/// `cell_viewer_connection.cpp:144-150` plus the real/ghost flag the two
/// selects partition on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerEntity {
    /// The entity's id.
    pub id: EntityId,
    /// The entity type's id (`entity.entityTypeID()`).
    pub type_id: u16,
    /// The x position (`entity.position()[0]`).
    pub x: f64,
    /// The z position (`entity.position()[2]`).
    pub z: f64,
    /// Whether this cell holds the real (`isReal()`).
    pub is_real: bool,
}

/// The physical space's bounds and grid (`cell_viewer_connection.cpp:225-256`):
/// the sub-bounds box and the chunk grid size, taken from the chunk space
/// or the minimum of the box's width and depth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridBounds {
    /// The box's min corner.
    pub min: [f64; 3],
    /// The box's max corner.
    pub max: [f64; 3],
    /// The chunk grid size.
    pub grid_size: f64,
}

/// The live state of one space as the viewer sees it.
#[derive(Debug, Clone, Default)]
pub struct ViewerSpace {
    /// `pCell != NULL`: a space whose cell is gone answers queries with the
    /// error codes instead of records.
    pub has_cell: bool,
    /// The physical space's bounds, once known.
    pub bounds: Option<GridBounds>,
    /// The space's entities by id, reals and ghosts together.
    entities: BTreeMap<EntityId, ViewerEntity>,
}

impl ViewerSpace {
    /// The entities in id order.
    pub fn entities(&self) -> impl Iterator<Item = &ViewerEntity> {
        self.entities.values()
    }

    /// Add or replace an entity.
    pub fn set_entity(&mut self, entity: ViewerEntity) {
        self.entities.insert(entity.id, entity);
    }

    /// Remove an entity; whether it was there.
    pub fn remove_entity(&mut self, id: EntityId) -> bool {
        self.entities.remove(&id).is_some()
    }
}

/// The CellApp's live state the queries run against, one CellViewer server's
/// worth (`CellViewerServer`, `cell_viewer_server.hpp:20`).
#[derive(Debug, Clone, Default)]
pub struct CellViewer {
    /// The entity type names in type-id order.
    type_names: Vec<String>,
    /// The spaces by id.
    spaces: BTreeMap<u32, ViewerSpace>,
}

impl CellViewer {
    /// A viewer over the given entity type names
    /// (`EntityType::getTypes()`, `cell_viewer_connection.cpp:61`).
    pub fn new<S: AsRef<str>>(type_names: &[S]) -> Self {
        CellViewer {
            type_names: type_names
                .iter()
                .map(|name| name.as_ref().to_string())
                .collect(),
            spaces: BTreeMap::new(),
        }
    }

    /// The type names.
    pub fn type_names(&self) -> &[String] {
        &self.type_names
    }

    /// Create the space if absent and return it for live update.
    pub fn space_mut(&mut self, space_id: u32) -> &mut ViewerSpace {
        self.spaces.entry(space_id).or_default()
    }

    /// Drop a space; whether it was there.
    pub fn remove_space(&mut self, space_id: u32) -> bool {
        self.spaces.remove(&space_id).is_some()
    }

    /// The space, if present.
    pub fn space(&self, space_id: u32) -> Option<&ViewerSpace> {
        self.spaces.get(&space_id)
    }

    /// `handleInputNotification` (`cell_viewer_connection.cpp:44`): answer
    /// one request from the live state. All integers are big-endian, as on
    /// the wire.
    pub fn handle(&self, request: &[u8]) -> Vec<u8> {
        let Some(&command) = request.first() else {
            return Vec::new();
        };
        match command {
            GET_ENTITY_TYPE_NAMES => self.reply_type_names(),
            GET_ENTITIES => {
                let Some((select, space_id)) = read_select_and_space(request) else {
                    return encode_u32(SPACE_NOT_FOUND as u32);
                };
                self.reply_entities(select, space_id)
            }
            GET_GRID => {
                let Some(space_id) = read_space(request) else {
                    return encode_u32(0);
                };
                self.reply_grid(space_id)
            }
            _ => {
                // The unknown command is drained and answered with a
                // zero-length reply (`cell_viewer_connection.cpp:303-313`).
                encode_u32(0)
            }
        }
    }

    /// `GET_ENTITY_TYPE_NAMES` (`cell_viewer_connection.cpp:57-84`): a u32
    /// count, then per name a u16 length and the bytes.
    fn reply_type_names(&self) -> Vec<u8> {
        let mut reply = encode_u32(self.type_names.len() as u32);
        for name in &self.type_names {
            encode_u16(&mut reply, name.len() as u16);
            reply.extend_from_slice(name.as_bytes());
        }
        reply
    }

    /// `GET_ENTITIES` (`cell_viewer_connection.cpp:86-187`): the error code
    /// for a missing space or cell, otherwise the u32 count and one record
    /// per entity — f32 x, f32 z, u16 type, u32 id — in id order, reals for
    /// `REAL` and ghosts for `GHOST`.
    fn reply_entities(&self, select: u8, space_id: u32) -> Vec<u8> {
        let Some(space) = self.spaces.get(&space_id) else {
            return encode_u32(SPACE_NOT_FOUND as u32);
        };
        if !space.has_cell {
            return encode_u32(SPACE_NOT_FOUND as u32);
        }

        let records: Vec<&ViewerEntity> = space
            .entities()
            .filter(|entity| entity.is_real == (select == REAL))
            .collect();
        let mut reply = encode_u32(records.len() as u32);
        for entity in records {
            encode_f32(&mut reply, entity.x);
            encode_f32(&mut reply, entity.z);
            encode_u16(&mut reply, entity.type_id);
            reply.extend_from_slice(&entity.id.to_be_bytes());
        }
        reply
    }

    /// `GET_GRID` (`cell_viewer_connection.cpp:189-298`): a u32 0 for a
    /// missing space or cell, otherwise a u32 1, the six f32s of the
    /// bounding box, the f32 grid size and the u32 0 of the retired
    /// grid-positions block.
    fn reply_grid(&self, space_id: u32) -> Vec<u8> {
        let Some(space) = self.spaces.get(&space_id) else {
            return encode_u32(0);
        };
        if !space.has_cell {
            return encode_u32(0);
        }

        let mut reply = encode_u32(1);
        if let Some(bounds) = space.bounds {
            for value in bounds.min {
                encode_f32(&mut reply, value);
            }
            for value in bounds.max {
                encode_f32(&mut reply, value);
            }
            encode_f32(&mut reply, bounds.grid_size);
        }
        reply.extend_from_slice(&0u32.to_be_bytes());
        reply
    }
}

/// The select byte and the u32 space id of an entities query
/// (`receiveData(whichChar)` + `receiveData(spaceID)`,
/// `cell_viewer_connection.cpp:93-95`) — the select sits between the
/// command and the id — or `None` when truncated.
fn read_select_and_space(request: &[u8]) -> Option<(u8, u32)> {
    let select = *request.get(1)?;
    let bytes = request.get(2..6)?;
    let space_id = u32::from_be_bytes(bytes.try_into().expect("four bytes"));
    Some((select, space_id))
}

/// The u32 space id following the command (`receiveData(spaceID)`).
fn read_space(request: &[u8]) -> Option<u32> {
    let bytes = request.get(1..5)?;
    Some(u32::from_be_bytes(bytes.try_into().expect("four bytes")))
}

fn encode_u16(reply: &mut Vec<u8>, value: u16) {
    reply.extend_from_slice(&value.to_be_bytes());
}

fn encode_u32(value: u32) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

fn encode_f32(reply: &mut Vec<u8>, value: f64) {
    reply.extend_from_slice(&(value as f32).to_bits().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: EntityId, type_id: u16, x: f64, z: f64, is_real: bool) -> ViewerEntity {
        ViewerEntity {
            id,
            type_id,
            x,
            z,
            is_real,
        }
    }

    /// A cursor over a reply, reading the wire types.
    struct Reply<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl<'a> Reply<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Reply { bytes, at: 0 }
        }

        fn u16(&mut self) -> u16 {
            let value = u16::from_be_bytes(
                self.bytes[self.at..self.at + 2]
                    .try_into()
                    .expect("two bytes"),
            );
            self.at += 2;
            value
        }

        fn u32(&mut self) -> u32 {
            let value = u32::from_be_bytes(
                self.bytes[self.at..self.at + 4]
                    .try_into()
                    .expect("four bytes"),
            );
            self.at += 4;
            value
        }

        fn f32(&mut self) -> f32 {
            f32::from_bits(self.u32())
        }

        fn string(&mut self, len: usize) -> String {
            let value = String::from_utf8(self.bytes[self.at..self.at + len].to_vec())
                .expect("utf-8 names");
            self.at += len;
            value
        }

        fn record(&mut self) -> (f32, f32, u16, u32) {
            (self.f32(), self.f32(), self.u16(), self.u32())
        }

        fn exhausted(&self) -> bool {
            self.at == self.bytes.len()
        }
    }

    /// The acceptance: every reply matches the live space and cell state at
    /// query time.
    #[test]
    fn the_replies_match_the_live_state() {
        let mut viewer = CellViewer::new(&["Avatar", "NPC"]);
        let space = viewer.space_mut(7);
        space.has_cell = true;
        space.bounds = Some(GridBounds {
            min: [0.0, 0.0, 0.0],
            max: [200.0, 0.0, 200.0],
            grid_size: 100.0,
        });
        space.set_entity(entity(1, 0, 10.0, 20.0, true));
        space.set_entity(entity(2, 0, 30.0, 40.0, true));
        space.set_entity(entity(4, 1, 50.0, 60.0, false));

        // The reals of the cell, in id order.
        let bytes = viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 7]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 2);
        assert_eq!(reply.record(), (10.0, 20.0, 0, 1));
        assert_eq!(reply.record(), (30.0, 40.0, 0, 2));
        assert!(reply.exhausted());

        // The ghosts of the space.
        let bytes = viewer.handle(&[GET_ENTITIES, GHOST, 0, 0, 0, 7]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 1);
        assert_eq!(reply.record(), (50.0, 60.0, 1, 4));
        assert!(reply.exhausted());

        // The grid: header 1, the box, the size, the retired block's zero.
        let bytes = viewer.handle(&[GET_GRID, 0, 0, 0, 7]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 1);
        assert_eq!(reply.f32(), 0.0);
        assert_eq!(reply.f32(), 0.0);
        assert_eq!(reply.f32(), 0.0);
        assert_eq!(reply.f32(), 200.0);
        assert_eq!(reply.f32(), 0.0);
        assert_eq!(reply.f32(), 200.0);
        assert_eq!(reply.f32(), 100.0);
        assert_eq!(reply.u32(), 0);
        assert!(reply.exhausted());

        // A move shows up in the very next query.
        viewer
            .space_mut(7)
            .set_entity(entity(1, 0, 70.0, 80.0, true));
        let bytes = viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 7]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 2);
        assert_eq!(reply.record(), (70.0, 80.0, 0, 1));

        // A removal does too, and the entity count follows.
        assert!(viewer.space_mut(7).remove_entity(2));
        let bytes = viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 7]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 1);
        assert_eq!(reply.record(), (70.0, 80.0, 0, 1));
        assert!(reply.exhausted());

        // A handoff flips the selects the entity answers on.
        viewer
            .space_mut(7)
            .set_entity(entity(1, 0, 70.0, 80.0, false));
        assert_eq!(
            viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 7]),
            encode_u32(0),
            "no reals left on the cell"
        );
        let bytes = viewer.handle(&[GET_ENTITIES, GHOST, 0, 0, 0, 7]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 2);

        // A lost cell answers both queries with the error code and the zero.
        viewer.space_mut(7).has_cell = false;
        assert_eq!(
            viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 7]),
            [0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(
            viewer.handle(&[GET_GRID, 0, 0, 0, 7]),
            [0, 0, 0, 0],
            "the grid reply is the u32 zero"
        );
    }

    /// `GET_ENTITY_TYPE_NAMES` (`cell_viewer_connection.cpp:57-84`).
    #[test]
    fn the_type_names_reply_is_a_count_and_pascal_strings() {
        let viewer = CellViewer::new(&["Avatar", "NPC"]);
        assert_eq!(viewer.type_names(), ["Avatar", "NPC"]);

        let bytes = viewer.handle(&[GET_ENTITY_TYPE_NAMES]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 2);
        let len = reply.u16() as usize;
        assert_eq!(reply.string(len), "Avatar");
        let len = reply.u16() as usize;
        assert_eq!(reply.string(len), "NPC");
        assert!(reply.exhausted());

        // A space with no entities answers an empty count, not an error.
        let mut viewer = CellViewer::new(&["Avatar"]);
        viewer.space_mut(1).has_cell = true;
        assert_eq!(
            viewer.handle(&[GET_ENTITIES, GHOST, 0, 0, 0, 1]),
            encode_u32(0)
        );
    }

    /// A query against a space that is not there gets the error codes
    /// (`cell_viewer_connection.cpp:152-156`, `:196-203`).
    #[test]
    fn an_unknown_space_answers_the_error_codes() {
        let viewer = CellViewer::new(&["Avatar"]);

        assert_eq!(
            viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 99]),
            [0xFF, 0xFF, 0xFF, 0xFF],
            "the entities reply is the i32 -1"
        );
        assert_eq!(viewer.handle(&[GET_GRID, 0, 0, 0, 99]), [0, 0, 0, 0]);
    }

    /// An unknown command is drained and answered with the zero-length
    /// reply (`cell_viewer_connection.cpp:303-313`).
    #[test]
    fn an_unknown_command_is_answered_with_an_empty_reply() {
        let viewer = CellViewer::new(&["Avatar"]);
        assert_eq!(viewer.handle(&[b'x', 1, 2, 3]), encode_u32(0));
    }

    /// A request truncated before its space id is answered as failing
    /// (`receiveData` warns, `cell_viewer_connection.cpp:315-330`).
    #[test]
    fn a_truncated_request_is_answered_as_failing() {
        let mut viewer = CellViewer::new(&["Avatar"]);
        viewer.space_mut(1).has_cell = true;

        assert_eq!(
            viewer.handle(&[GET_ENTITIES, REAL, 0]),
            [0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(viewer.handle(&[GET_GRID, 0]), [0, 0, 0, 0]);
    }

    /// The f64 positions go out as the f32s of the wire record
    /// (`cell_viewer_connection.cpp:144-150`).
    #[test]
    fn the_positions_are_reported_as_f32() {
        let mut viewer = CellViewer::new(&["Avatar"]);
        let space = viewer.space_mut(1);
        space.has_cell = true;
        space.set_entity(entity(5, 0, 16777216.5, 0.25, true));

        let bytes = viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 1]);
        let mut reply = Reply::new(&bytes);
        assert_eq!(reply.u32(), 1);
        assert_eq!(reply.record(), (16777216.0, 0.25, 0, 5));
    }

    /// Spaces come and go on the CellApp, and the queries follow
    /// (`cellApp_.findSpace`, `cell_viewer_connection.cpp:100`).
    #[test]
    fn spaces_come_and_go() {
        let mut viewer = CellViewer::new(&["Avatar"]);
        assert!(viewer.space(1).is_none(), "the space is not there yet");

        viewer.space_mut(1).has_cell = true;
        assert!(viewer.space(1).is_some());

        // A space whose bounds are not known yet still answers the grid
        // header with the retired block's zero.
        assert_eq!(
            viewer.handle(&[GET_GRID, 0, 0, 0, 1]),
            [0, 0, 0, 1, 0, 0, 0, 0]
        );

        assert!(viewer.remove_space(1));
        assert!(!viewer.remove_space(1), "the second drop finds nothing");
        assert_eq!(
            viewer.handle(&[GET_ENTITIES, REAL, 0, 0, 0, 1]),
            [0xFF, 0xFF, 0xFF, 0xFF]
        );

        // An empty request on the socket answers nothing at all.
        assert_eq!(viewer.handle(&[]), Vec::<u8>::new());
    }
}
