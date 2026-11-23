//! BW-06-04-04 the client's game-time sync and movement input.
//!
//! Two halves of one item:
//!
//! * `ServerTimeHandler` (`lib/connection/server_connection.cpp`) keeps the
//!   client's notion of the server's game time. `gameTime` seeds it from the
//!   `setGameTime` message; `tickSync` follows the packet-sequence transitions
//!   (ordered, unordered, and the wrap-around cases) and clamps the client to
//!   the server's pace by nudging the sequence start by at most
//!   `MAX_TIME_ADJUST`; `serverTime` maps a client time to the server's time,
//!   never going backwards.
//! * `MovementSender::addMove` builds the `avatarUpdate*` message the client
//!   sends for an entity it controls. It chooses implicit vs explicit (a
//!   vehicle or on-ground change forces explicit) and player vs ward, and
//!   serializes the args to the wire layout of `baseapp_ext_interface.hpp`:
//!   big-endian floats for `Position3D` (pre-`BW_HTONF`), big-endian u32 for
//!   the ids (pre-`BW_HTONL`), and a 3-byte bit-packed `YawPitchRoll`
//!   (8-bit signed yaw, half-angle pitch, roll).
//!
//! `VOLATILE_POSITIONS_ARE_ABSOLUTE` is 0 in this build, so the player moves
//! carry a `refNum` and store the position for later reference lookups.

use std::collections::{HashMap, HashSet};

/// `EntityID` (`network/basictypes.hpp`).
pub type EntityId = u32;

/// `SpaceID` (`space/forward_declarations.hpp`).
pub type SpaceId = u32;

/// `GameTime`.
pub type GameTime = u32;

/// `NULL_ENTITY_ID`.
pub const NULL_ENTITY_ID: EntityId = 0;

/// `NULL_SPACE_ID`.
pub const NULL_SPACE_ID: SpaceId = 0;

/// `AVATAR_UPDATE_EXPLICT_FLAG_ONGROUND`.
pub const AVATAR_UPDATE_EXPLICIT_FLAG_ON_GROUND: u8 = 0x1;

/// The initial `isOnGround_` ("unknown", `server_connection.cpp:51`).
pub const IS_ON_GROUND_UNKNOWN: u8 = 2;

const PI: f64 = std::f64::consts::PI;

/// `ServerTimeHandler`: the client's view of the server game time.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerTimeHandler {
    tick_byte: u8,
    has_received_game_time: bool,
    time_at_sequence_start: f64,
    game_time_at_sequence_start: u32,
    last_returned_server_time: f64,
    last_packet_was_ordered: bool,
    last_ordered_tick_byte: u8,
    update_frequency: f64,
}

impl ServerTimeHandler {
    /// Constructor. `update_frequency` is the server's `updateFrequency_`
    /// (default 10 Hz).
    pub fn new(update_frequency: f64) -> Self {
        Self {
            tick_byte: 0,
            has_received_game_time: false,
            time_at_sequence_start: 0.0,
            game_time_at_sequence_start: 0,
            last_returned_server_time: 0.0,
            last_packet_was_ordered: true,
            last_ordered_tick_byte: 0,
            update_frequency,
        }
    }

    /// `setGameTime`: seed the time from the server's absolute game time.
    pub fn game_time(&mut self, new_game_time: GameTime, current_time: f64) {
        self.has_received_game_time = true;
        self.tick_byte = (new_game_time & 0xff) as u8;
        self.game_time_at_sequence_start = new_game_time - self.tick_byte as u32;
        self.time_at_sequence_start = current_time - self.tick_byte as f64 / self.update_frequency;
    }

    /// `tickSync`: follow the packet-sequence transitions and keep the client
    /// from drifting.
    pub fn tick_sync(&mut self, new_seq_num: u8, current_time: f64, is_early: bool) {
        let seq_period = 256.0 / self.update_frequency;

        // The first packet is always the seed; there is nothing to do yet.
        if !self.has_received_game_time {
            return;
        }

        let seq_diff = new_seq_num as i32 - self.tick_byte as i32;
        let this_packet_is_ordered = !is_early;

        // Transition 1: an ordered packet, then an unordered one.
        if self.last_packet_was_ordered && !this_packet_is_ordered {
            self.last_ordered_tick_byte = self.tick_byte;
        }
        // Transition 3: an unordered packet, then an ordered one going
        // backward (wrapped around).
        else if !self.last_packet_was_ordered && this_packet_is_ordered {
            if seq_diff > 0 {
                self.time_at_sequence_start -= seq_period;
                self.game_time_at_sequence_start =
                    self.game_time_at_sequence_start.wrapping_sub(256);
            }
        }
        // Transition 4: two unordered packets.
        else if !self.last_packet_was_ordered && !this_packet_is_ordered {
            let mut unordered_tick_byte = self.tick_byte as u16;
            let mut unordered_input = new_seq_num as u16;
            if unordered_input < self.last_ordered_tick_byte as u16 {
                unordered_input += 256;
            }
            if unordered_tick_byte < self.last_ordered_tick_byte as u16 {
                unordered_tick_byte += 256;
            }
            if unordered_input >= 256 && unordered_tick_byte < 256 {
                self.time_at_sequence_start += seq_period;
                self.game_time_at_sequence_start =
                    self.game_time_at_sequence_start.wrapping_add(256);
            } else if unordered_input < 256 && unordered_tick_byte >= 256 {
                self.time_at_sequence_start -= seq_period;
                self.game_time_at_sequence_start =
                    self.game_time_at_sequence_start.wrapping_sub(256);
            }
        }

        // A backward sequence after an ordered packet means the tick wrapped
        // around going forward.
        if seq_diff < 0 && self.last_packet_was_ordered {
            self.time_at_sequence_start += seq_period;
            self.game_time_at_sequence_start = self.game_time_at_sequence_start.wrapping_add(256);
        }

        self.last_packet_was_ordered = this_packet_is_ordered;
        self.tick_byte = new_seq_num;

        if seq_diff == 0 {
            return;
        }

        // Keep the client from getting too far out of sync with the server.
        const MAX_TIME_ERROR: f64 = 0.05;
        const MAX_TIME_ADJUST: f64 = 0.005;

        let time_error = current_time - self.client_time_of_last_message();
        if time_error > MAX_TIME_ERROR {
            self.time_at_sequence_start += time_error.min(MAX_TIME_ADJUST);
        } else if -time_error > MAX_TIME_ERROR {
            self.time_at_sequence_start += time_error.max(-MAX_TIME_ADJUST);
        }
    }

    /// `serverTime`: map a client time to the server's time. Never goes
    /// backwards.
    pub fn server_time(&mut self, client_time: f64) -> f64 {
        if !self.has_received_game_time {
            return 0.0;
        }
        let srv_time = self.game_time_at_sequence_start as f64 / self.update_frequency
            + (client_time - self.time_at_sequence_start);
        if srv_time < self.last_returned_server_time {
            return self.last_returned_server_time;
        }
        self.last_returned_server_time = srv_time;
        srv_time
    }

    /// `clientTimeOfLastMessage`.
    pub fn client_time_of_last_message(&self) -> f64 {
        if !self.has_received_game_time {
            return 0.0;
        }
        self.time_at_sequence_start + self.tick_byte as f64 / self.update_frequency
    }

    /// `gameTimeOfLastMessage`.
    pub fn game_time_of_last_message(&self) -> GameTime {
        self.game_time_at_sequence_start
            .wrapping_add(self.tick_byte as u32)
    }

    /// Whether the time was seeded.
    pub fn has_received_game_time(&self) -> bool {
        self.has_received_game_time
    }
}

/// One `avatarUpdate*` message the client sends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AvatarUpdate {
    /// `avatarUpdateImplicit`: the player's position, with a reference number.
    PlayerImplicit {
        /// `pos` (network-order).
        pos: [f32; 3],
        /// `dir` (yaw, pitch, roll).
        dir: [f32; 3],
        /// `refNum`.
        ref_num: u8,
    },
    /// `avatarUpdateExplicit`: the player's position and vehicle.
    PlayerExplicit {
        /// `vehicleID`.
        vehicle_id: EntityId,
        /// `pos`.
        pos: [f32; 3],
        /// `dir`.
        dir: [f32; 3],
        /// `flags` (`AVATAR_UPDATE_EXPLICT_FLAG_ONGROUND`).
        flags: u8,
        /// `refNum`.
        ref_num: u8,
    },
    /// `avatarUpdateWardImplicit`: another controlled entity's position.
    WardImplicit {
        /// `ward`.
        ward: EntityId,
        /// `pos`.
        pos: [f32; 3],
        /// `dir`.
        dir: [f32; 3],
    },
    /// `avatarUpdateWardExplicit`: another controlled entity, with a vehicle.
    WardExplicit {
        /// `ward`.
        ward: EntityId,
        /// `spaceID`.
        space_id: SpaceId,
        /// `vehicleID`.
        vehicle_id: EntityId,
        /// `pos`.
        pos: [f32; 3],
        /// `dir`.
        dir: [f32; 3],
        /// `flags`.
        flags: u8,
    },
}

impl AvatarUpdate {
    /// The `refNum` when the message carries one.
    pub fn ref_num(&self) -> Option<u8> {
        match self {
            AvatarUpdate::PlayerImplicit { ref_num, .. }
            | AvatarUpdate::PlayerExplicit { ref_num, .. } => Some(*ref_num),
            _ => None,
        }
    }

    /// Serialize the args to the wire layout of `baseapp_ext_interface.hpp`
    /// (non-absolute positions).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match *self {
            AvatarUpdate::PlayerImplicit { pos, dir, ref_num } => {
                write_pos(&mut out, pos);
                out.extend_from_slice(&pack_dir(dir));
                out.push(ref_num);
            }
            AvatarUpdate::PlayerExplicit {
                vehicle_id,
                pos,
                dir,
                flags,
                ref_num,
            } => {
                write_u32(&mut out, vehicle_id);
                write_pos(&mut out, pos);
                out.extend_from_slice(&pack_dir(dir));
                out.push(flags);
                out.push(ref_num);
            }
            AvatarUpdate::WardImplicit { ward, pos, dir } => {
                write_u32(&mut out, ward);
                write_pos(&mut out, pos);
                out.extend_from_slice(&pack_dir(dir));
            }
            AvatarUpdate::WardExplicit {
                ward,
                space_id,
                vehicle_id,
                pos,
                dir,
                flags,
            } => {
                write_u32(&mut out, ward);
                write_u32(&mut out, space_id);
                write_u32(&mut out, vehicle_id);
                write_pos(&mut out, pos);
                out.extend_from_slice(&pack_dir(dir));
                out.push(flags);
            }
        }
        out
    }
}

/// Write a big-endian u32 (`BW_HTONL`).
fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Write a `Position3D` — three network-order floats.
fn write_pos(out: &mut Vec<u8>, pos: [f32; 3]) {
    for component in pos {
        out.extend_from_slice(&component.to_bits().to_be_bytes());
    }
}

/// Pack a `YawPitchRoll`: 8-bit signed yaw, half-angle pitch, roll.
fn pack_dir(dir: [f32; 3]) -> [u8; 3] {
    [
        angle_to_int8(dir[0]) as u8,
        half_angle_to_int8(dir[1]) as u8,
        angle_to_int8(dir[2]) as u8,
    ]
}

/// `angleToInt<8>`: `(int8)floorf(angle * 128 / PI + 0.5)`.
fn angle_to_int8(angle: f32) -> i8 {
    ((angle as f64 * 128.0 / PI + 0.5).floor() as i64) as i8
}

/// `halfAngleToInt<8>`: `(int8)clamp(-128, floorf(angle * 254 / PI + 0.5), 127)`.
fn half_angle_to_int8(angle: f32) -> i8 {
    let value = (angle as f64 * 254.0 / PI + 0.5).floor();
    value.clamp(-128.0, 127.0) as i8
}

/// The client's movement state and the `addMove` input.
#[derive(Debug)]
pub struct MovementSender {
    online: bool,
    self_entity_id: EntityId,
    space_id: SpaceId,
    is_on_ground: u8,
    sending_sequence_number: u8,
    sent_positions: Vec<[f32; 3]>,
    controlled: HashSet<EntityId>,
    vehicles: HashMap<EntityId, EntityId>,
}

impl MovementSender {
    /// Constructor: the connected (player) entity and its space.
    pub fn new(self_entity_id: EntityId, space_id: SpaceId) -> Self {
        Self {
            online: true,
            self_entity_id,
            space_id,
            is_on_ground: IS_ON_GROUND_UNKNOWN,
            sending_sequence_number: 0,
            sent_positions: vec![[0.0; 3]; 256],
            controlled: HashSet::new(),
            vehicles: HashMap::new(),
        }
    }

    /// Whether the client is online.
    pub fn is_online(&self) -> bool {
        self.online
    }

    /// Set the online state.
    pub fn set_online(&mut self, online: bool) {
        self.online = online;
    }

    /// The connection's space.
    pub fn space_id(&self) -> SpaceId {
        self.space_id
    }

    /// Set the connection's space.
    pub fn set_space(&mut self, space_id: SpaceId) {
        self.space_id = space_id;
    }

    /// Mark an entity as controlled locally, on the given vehicle.
    pub fn control(&mut self, id: EntityId, vehicle_id: EntityId) {
        self.controlled.insert(id);
        self.vehicles.insert(id, vehicle_id);
    }

    /// Stop controlling an entity.
    pub fn release(&mut self, id: EntityId) {
        self.controlled.remove(&id);
        self.vehicles.remove(&id);
    }

    /// Set an entity's vehicle (`setVehicle`).
    pub fn set_vehicle(&mut self, id: EntityId, vehicle_id: EntityId) {
        self.vehicles.insert(id, vehicle_id);
    }

    /// `getVehicleID`.
    pub fn vehicle(&self, id: EntityId) -> EntityId {
        self.vehicles.get(&id).copied().unwrap_or(0)
    }

    /// `sentPositions_[refNum]`.
    pub fn sent_position(&self, ref_num: u8) -> [f32; 3] {
        self.sent_positions[ref_num as usize]
    }

    /// `addMove`: build the movement message for a controlled entity. Returns
    /// `None` when the client is offline, the space is unknown or the entity
    /// is not controlled — the reference rejects those.
    ///
    /// The reference signature is
    /// `addMove(id, spaceID, vehicleID, coordPos, dir, onGround, globalPos)`;
    /// clippy counts `&mut self` as an eighth argument, so the lint is allowed
    /// rather than distorting the faithful parameter list.
    #[allow(clippy::too_many_arguments)]
    pub fn add_move(
        &mut self,
        id: EntityId,
        space_id: SpaceId,
        vehicle_id: EntityId,
        pos: [f32; 3],
        dir: [f32; 3],
        on_ground: bool,
        global_pos: [f32; 3],
    ) -> Option<AvatarUpdate> {
        if !self.online {
            return None;
        }

        let space_id = if space_id == NULL_SPACE_ID {
            self.space_id
        } else {
            space_id
        };
        let id = if id == NULL_ENTITY_ID {
            self.self_entity_id
        } else {
            id
        };

        if space_id != self.space_id {
            return None;
        }
        if !self.controlled.contains(&id) {
            return None;
        }

        let current_vehicle_id = self.vehicle(id);
        let mut should_send_explicit = vehicle_id != current_vehicle_id;

        if id == self.self_entity_id {
            let has_changed_on_ground = (u8::from(on_ground)) != self.is_on_ground;
            should_send_explicit |= has_changed_on_ground;
            self.is_on_ground = u8::from(on_ground);

            let ref_num = self.sending_sequence_number;
            self.sent_positions[ref_num as usize] = global_pos;
            self.sending_sequence_number = self.sending_sequence_number.wrapping_add(1);

            if !should_send_explicit {
                Some(AvatarUpdate::PlayerImplicit { pos, dir, ref_num })
            } else {
                Some(AvatarUpdate::PlayerExplicit {
                    vehicle_id,
                    pos,
                    dir,
                    flags: if on_ground {
                        AVATAR_UPDATE_EXPLICIT_FLAG_ON_GROUND
                    } else {
                        0
                    },
                    ref_num,
                })
            }
        } else if !should_send_explicit {
            Some(AvatarUpdate::WardImplicit { ward: id, pos, dir })
        } else {
            Some(AvatarUpdate::WardExplicit {
                ward: id,
                space_id: self.space_id,
                vehicle_id,
                pos,
                dir,
                flags: 0,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> MovementSender {
        let mut sender = MovementSender::new(7, 100);
        sender.control(7, 0);
        sender
    }

    #[test]
    fn game_time_seeds_the_time_base() {
        let mut handler = ServerTimeHandler::new(10.0);
        assert!(!handler.has_received_game_time());
        assert_eq!(handler.server_time(5.0), 0.0);

        handler.game_time(400, 40.0);
        assert!(handler.has_received_game_time());
        // tickByte = 400 & 0xff = 144; gameTimeAtSequenceStart = 256.
        assert_eq!(handler.game_time_of_last_message(), 400);
        // clientTimeOfLastMessage = 25.6 + 144/10 = 40.0.
        assert!((handler.client_time_of_last_message() - 40.0).abs() < 1e-9);
        // The server time at the same client time is equal (no drift yet).
        assert!((handler.server_time(40.0) - 40.0).abs() < 1e-9);
    }

    #[test]
    fn a_tick_sync_adjusts_the_client_to_the_server_pace() {
        let mut handler = ServerTimeHandler::new(10.0);
        handler.game_time(400, 40.0);
        // The next tick arrives 0.1s later but the client thinks it is 40.0:
        // the client is ahead, so it is nudged back by MAX_TIME_ADJUST.
        handler.tick_sync(145, 40.0, false);
        // clientTimeOfLastMessage after tick 145 = 25.6 + 14.5 = 40.1; the
        // -0.1 error is clamped to -0.005.
        assert!((handler.client_time_of_last_message() - 40.095).abs() < 1e-9);
    }

    #[test]
    fn a_late_tick_sync_does_not_nudge_past_the_adjusmt_limit() {
        let mut handler = ServerTimeHandler::new(10.0);
        handler.game_time(400, 40.0);
        // A tick on time: no adjustment.
        handler.tick_sync(145, 40.1, false);
        assert!((handler.client_time_of_last_message() - 40.1).abs() < 1e-9);
        // The server time never goes backwards.
        let t1 = handler.server_time(50.0);
        let t2 = handler.server_time(49.0);
        assert!(t2 >= t1 - 1e-9);
    }

    #[test]
    fn an_early_packet_is_unordered_and_does_not_reorder_the_time() {
        let mut handler = ServerTimeHandler::new(10.0);
        handler.game_time(400, 40.0);
        handler.tick_sync(146, 40.2, true);
        handler.tick_sync(145, 40.1, false);
        // The ordered 145 catches up without wrapping the time base: the game
        // time is 256 + 145 and the client time is unchanged.
        assert_eq!(handler.game_time_of_last_message(), 401);
        assert!((handler.client_time_of_last_message() - 40.1).abs() < 1e-9);
    }

    #[test]
    fn an_offline_client_sends_no_move() {
        let mut sender = player();
        sender.set_online(false);
        assert_eq!(
            sender.add_move(7, 100, 0, [1.0; 3], [0.0; 3], false, [1.0; 3]),
            None
        );
    }

    #[test]
    fn a_space_mismatch_sends_no_move() {
        let mut sender = player();
        assert_eq!(
            sender.add_move(7, 999, 0, [1.0; 3], [0.0; 3], false, [1.0; 3]),
            None
        );
    }

    #[test]
    fn an_uncontrolled_entity_sends_no_move() {
        let mut sender = player();
        assert_eq!(
            sender.add_move(9, 100, 0, [1.0; 3], [0.0; 3], false, [1.0; 3]),
            None
        );
    }

    #[test]
    fn the_first_move_is_explicit_because_on_ground_is_unknown() {
        let mut sender = player();
        let move_ = sender
            .add_move(0, 0, 0, [1.0, 2.0, 3.0], [0.0; 3], false, [1.0, 2.0, 3.0])
            .unwrap();
        // The player id defaults to the connected entity and the space to the
        // connection's space.
        assert_eq!(
            move_,
            AvatarUpdate::PlayerExplicit {
                vehicle_id: 0,
                pos: [1.0, 2.0, 3.0],
                dir: [0.0; 3],
                flags: 0,
                ref_num: 0,
            }
        );
        assert_eq!(move_.ref_num(), Some(0));
        assert_eq!(sender.sent_position(0), [1.0, 2.0, 3.0]);
    }

    #[test]
    fn a_steady_move_is_implicit() {
        let mut sender = player();
        sender.add_move(7, 100, 0, [1.0; 3], [0.0; 3], false, [1.0; 3]);
        let move_ = sender
            .add_move(7, 100, 0, [2.0; 3], [0.0; 3], false, [2.0; 3])
            .unwrap();
        assert_eq!(
            move_,
            AvatarUpdate::PlayerImplicit {
                pos: [2.0; 3],
                dir: [0.0; 3],
                ref_num: 1,
            }
        );
    }

    #[test]
    fn an_on_ground_change_forces_explicit() {
        let mut sender = player();
        sender.add_move(7, 100, 0, [1.0; 3], [0.0; 3], false, [1.0; 3]);
        let move_ = sender
            .add_move(7, 100, 0, [1.0; 3], [0.0; 3], true, [1.0; 3])
            .unwrap();
        assert_eq!(
            move_,
            AvatarUpdate::PlayerExplicit {
                vehicle_id: 0,
                pos: [1.0; 3],
                dir: [0.0; 3],
                flags: AVATAR_UPDATE_EXPLICIT_FLAG_ON_GROUND,
                ref_num: 1,
            }
        );
    }

    #[test]
    fn a_ward_move_is_implicit_and_explicit_with_a_vehicle() {
        let mut sender = player();
        sender.control(9, 0);
        let implicit = sender
            .add_move(9, 100, 0, [5.0; 3], [0.0; 3], false, [5.0; 3])
            .unwrap();
        assert_eq!(
            implicit,
            AvatarUpdate::WardImplicit {
                ward: 9,
                pos: [5.0; 3],
                dir: [0.0; 3],
            }
        );
        // The ward boards vehicle 5 while the connection still records no
        // vehicle, so the update is explicit.
        let explicit = sender
            .add_move(9, 100, 5, [6.0; 3], [0.0; 3], false, [6.0; 3])
            .unwrap();
        assert_eq!(
            explicit,
            AvatarUpdate::WardExplicit {
                ward: 9,
                space_id: 100,
                vehicle_id: 5,
                pos: [6.0; 3],
                dir: [0.0; 3],
                flags: 0,
            }
        );
        // `addMove` never records the vehicle itself — that waits for the
        // server's setVehicle — so the same move stays explicit.
        let again = sender
            .add_move(9, 100, 5, [7.0; 3], [0.0; 3], false, [7.0; 3])
            .unwrap();
        assert!(matches!(again, AvatarUpdate::WardExplicit { .. }));
        // A server setVehicle finally records it, and the move goes implicit.
        sender.set_vehicle(9, 5);
        assert_eq!(sender.vehicle(9), 5);
        let last = sender
            .add_move(9, 100, 5, [8.0; 3], [0.0; 3], false, [8.0; 3])
            .unwrap();
        assert_eq!(
            last,
            AvatarUpdate::WardImplicit {
                ward: 9,
                pos: [8.0; 3],
                dir: [0.0; 3],
            }
        );
    }

    #[test]
    fn the_wire_bytes_match_the_interface_layout() {
        // PlayerImplicit: pos (3 BE floats), dir (3 packed bytes), refNum.
        let implicit = AvatarUpdate::PlayerImplicit {
            pos: [1.0, 2.0, 3.0],
            dir: [0.0; 3],
            ref_num: 1,
        };
        let mut expected = Vec::new();
        for value in [1.0f32, 2.0, 3.0] {
            expected.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        expected.extend_from_slice(&[0, 0, 0, 1]);
        assert_eq!(implicit.serialize(), expected);
        assert_eq!(implicit.serialize().len(), 16);

        // PlayerExplicit: vehicle (BE u32), pos, dir, flags, refNum.
        let explicit = AvatarUpdate::PlayerExplicit {
            vehicle_id: 0,
            pos: [1.0, 2.0, 3.0],
            dir: [0.0; 3],
            flags: 1,
            ref_num: 2,
        };
        let mut expected = Vec::new();
        expected.extend_from_slice(&0u32.to_be_bytes());
        for value in [1.0f32, 2.0, 3.0] {
            expected.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        expected.extend_from_slice(&[0, 0, 0, 1, 2]);
        assert_eq!(explicit.serialize(), expected);
        assert_eq!(explicit.serialize().len(), 21);
    }

    #[test]
    fn a_nonzero_yaw_packs_into_signed_eight_bits() {
        // yaw = pi/2 → (pi/2 * 128 / pi) = 64.
        let dir = pack_dir([PI as f32 / 2.0, 0.0, 0.0]);
        assert_eq!(dir[0], 64);
        assert_eq!(dir[1], 0);
        assert_eq!(dir[2], 0);
    }
}
