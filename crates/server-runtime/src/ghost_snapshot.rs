//! BW-08-04-02: copying properties and position into a ghost.
//!
//! Ported from `Entity::writeGhostDataToStream` /
//! `writeGhostDataToStreamInternal` (`entity.cpp:1829-1886`) and the
//! receiving `Entity::readGhostDataFromStream(Internal)`
//! (`entity.cpp:1751-1820`): the full mutable state a ghost needs to match
//! its real - position, direction, ground flag, event number, volatile
//! priorities, the real and base addresses, the clientServer property
//! change stamps, every ghosted property, the ghost controllers and the
//! tail counters - framed by the `GProperties` / `GController` / `GTail`
//! tokens the reference `TOKEN_CHECK`s on read.
//!
//! The reference frames this blob with the entity id and type id ahead of
//! the data (`entity.cpp:1831-1834`, read by `Space::createGhost`), so
//! [`GhostSnapshot`] carries both. Properties are streamed through the
//! entitydef layer there; without it, [`GhostPropertyValue`] carries a
//! one-byte tag alongside the payload. The reference's trailing
//! `MF_ASSERT( data.remainingLength() == 0 )` becomes a checked error, as
//! do the token checks.

use foundation::math::Vector3;
use foundation::stream::BinaryStream;

/// `TOKEN_ADD/_CHECK( "GProperties" )` (`entity.cpp:1857` / `1786`).
pub const TOKEN_GHOST_PROPERTIES: &[u8] = b"GProperties";
/// `TOKEN_ADD/CHECK( "GController" )` (`entity.cpp:1880` / `1810`).
pub const TOKEN_GHOST_CONTROLLER: &[u8] = b"GController";
/// `TOKEN_ADD/CHECK( "GTail" )` (`entity.cpp:1884` / `1815`).
pub const TOKEN_GHOST_TAIL: &[u8] = b"GTail";

/// `VolatileInfo` (`lib/entitydef/volatile_info.hpp:13-50`): what changes
/// often enough to send every update, as four priorities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolatileInfo {
    /// `positionPriority_`.
    pub position_priority: f32,
    /// `yawPriority_`.
    pub yaw_priority: f32,
    /// `pitchPriority_`.
    pub pitch_priority: f32,
    /// `rollPriority_`.
    pub roll_priority: f32,
}

impl Default for VolatileInfo {
    /// The constructor defaults: every priority at -1 (never volatile).
    fn default() -> Self {
        VolatileInfo {
            position_priority: -1.0,
            yaw_priority: -1.0,
            pitch_priority: -1.0,
            roll_priority: -1.0,
        }
    }
}

impl VolatileInfo {
    /// `shouldSendPosition`.
    pub fn should_send_position(&self) -> bool {
        self.position_priority > 0.0
    }

    /// `hasVolatile` for any axis.
    pub fn has_volatile(&self) -> bool {
        self.should_send_position()
            || self.yaw_priority > 0.0
            || self.pitch_priority > 0.0
            || self.roll_priority > 0.0
    }

    /// `isLessVolatileThan`: true when every priority here is at most the
    /// other's.
    pub fn is_less_volatile_than(&self, other: &VolatileInfo) -> bool {
        self.position_priority <= other.position_priority
            && self.yaw_priority <= other.yaw_priority
            && self.pitch_priority <= other.pitch_priority
            && self.roll_priority <= other.roll_priority
    }
}

/// `Direction3` (`lib/math/direction3.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Direction3 {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
}

/// One ghosted property. The entitydef layer knows each slot's type, so
/// the reference streams bare payloads; this port tags each value.
#[derive(Debug, Clone, PartialEq)]
pub enum GhostPropertyValue {
    Bool(bool),
    Int32(i32),
    Float(f32),
    Str(String),
    Vector(Vector3),
}

/// A `GhostController` as `writeGhostControllersToStream` frames it: the
/// controller type id and its data blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostController {
    pub controller_type: u8,
    pub data: Vec<u8>,
}

/// The mutable state copied from a real into its ghost
/// (`writeGhostDataToStreamInternal`, `entity.cpp:1846-1886`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GhostSnapshot {
    /// Streamed ahead of the blob and read by `Space::createGhost`.
    pub entity_id: u32,
    pub entity_type_id: u16,
    /// `numTimesRealOffloaded_`.
    pub num_times_real_offloaded: u32,
    /// `localPosition_`.
    pub local_position: Vector3,
    /// `isOnGround_`.
    pub is_on_ground: bool,
    /// `lastEventNumber_`.
    pub last_event_number: i32,
    /// `volatileInfo_`.
    pub volatile_info: VolatileInfo,
    /// The real's address (`CellApp::instance().interface().address()`).
    pub real_addr: u64,
    /// `baseAddr_`.
    pub base_addr: u64,
    /// `localDirection_`.
    pub local_direction: Direction3,
    /// `propertyEventStamps_`: one stamp per clientServer property.
    pub property_event_stamps: Vec<u32>,
    /// The ghosted properties, in type order.
    pub properties: Vec<GhostPropertyValue>,
    /// The ghost controllers.
    pub controllers: Vec<GhostController>,
    /// `periodsWithoutWitness_`.
    pub periods_without_witness: u8,
    /// `aoiUpdateSchemeID_`.
    pub aoi_update_scheme_id: u8,
}

/// The derived state the ghost seeds from the stream
/// (`readGhostDataFromStreamInternal`, `entity.cpp:1768-1770`): the global
/// position starts as the local one and the event history floor is pinned
/// to the real's last event number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GhostSeed {
    pub global_position: Vector3,
    pub last_trimmed_event_number: i32,
}

/// What can go wrong reading a ghost blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhostStreamError {
    /// A `TOKEN_CHECK` failed.
    BadToken,
    /// The stream ran short or holds trailing bytes.
    BadLength,
    /// The stream's own error flag was set.
    Stream,
}

fn write_token(stream: &mut BinaryStream, token: &[u8]) {
    stream.write_bytes(token);
}

fn read_token(stream: &mut BinaryStream, token: &[u8]) -> Result<(), GhostStreamError> {
    let read = stream
        .read_blob_raw(token.len())
        .map_err(|_| GhostStreamError::BadLength)?;
    if read.as_slice() == token {
        Ok(())
    } else {
        Err(GhostStreamError::BadToken)
    }
}

impl GhostPropertyValue {
    fn write_to(&self, stream: &mut BinaryStream) {
        match self {
            GhostPropertyValue::Bool(v) => {
                stream.write_u8(0).write_bool(*v);
            }
            GhostPropertyValue::Int32(v) => {
                stream.write_u8(1).write_i32(*v);
            }
            GhostPropertyValue::Float(v) => {
                stream.write_u8(2).write_f32(*v);
            }
            GhostPropertyValue::Str(v) => {
                let _ = stream.write_u8(3).write_string(v);
            }
            GhostPropertyValue::Vector(v) => {
                stream
                    .write_u8(4)
                    .write_f32(v.x)
                    .write_f32(v.y)
                    .write_f32(v.z);
            }
        }
    }

    fn read_from(stream: &mut BinaryStream) -> Result<Self, GhostStreamError> {
        let tag = stream.read_u8().map_err(|_| GhostStreamError::Stream)?;
        match tag {
            0 => Ok(GhostPropertyValue::Bool(
                stream.read_bool().map_err(|_| GhostStreamError::Stream)?,
            )),
            1 => Ok(GhostPropertyValue::Int32(
                stream.read_i32().map_err(|_| GhostStreamError::Stream)?,
            )),
            2 => Ok(GhostPropertyValue::Float(
                stream.read_f32().map_err(|_| GhostStreamError::Stream)?,
            )),
            3 => Ok(GhostPropertyValue::Str(
                stream.read_string().map_err(|_| GhostStreamError::Stream)?,
            )),
            4 => Ok(GhostPropertyValue::Vector(Vector3::new(
                stream.read_f32().map_err(|_| GhostStreamError::Stream)?,
                stream.read_f32().map_err(|_| GhostStreamError::Stream)?,
                stream.read_f32().map_err(|_| GhostStreamError::Stream)?,
            ))),
            _ => Err(GhostStreamError::BadToken),
        }
    }
}

impl GhostSnapshot {
    /// `Entity::writeGhostDataToStream` (`entity.cpp:1829-1839`): the id
    /// and type id ahead of the internal blob. The compression wrapper is
    /// not modelled.
    pub fn write_to(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.entity_id);
        stream.write_u16(self.entity_type_id);
        self.write_internal_to(stream);
    }

    /// `writeGhostDataToStreamInternal` (`entity.cpp:1846-1886`).
    fn write_internal_to(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.num_times_real_offloaded);
        stream.write_f32(self.local_position.x);
        stream.write_f32(self.local_position.y);
        stream.write_f32(self.local_position.z);
        stream.write_bool(self.is_on_ground);
        stream.write_i32(self.last_event_number);
        stream.write_f32(self.volatile_info.position_priority);
        stream.write_f32(self.volatile_info.yaw_priority);
        stream.write_f32(self.volatile_info.pitch_priority);
        stream.write_f32(self.volatile_info.roll_priority);

        stream.write_u64(self.real_addr);
        stream.write_u64(self.base_addr);
        stream.write_f32(self.local_direction.yaw);
        stream.write_f32(self.local_direction.pitch);
        stream.write_f32(self.local_direction.roll);

        // `PropertyEventStamps::addToStream`: the stamps, bare.
        stream.write_count(self.property_event_stamps.len() as u32);
        for stamp in &self.property_event_stamps {
            stream.write_u32(*stamp);
        }

        write_token(stream, TOKEN_GHOST_PROPERTIES);
        stream.write_count(self.properties.len() as u32);
        for value in &self.properties {
            value.write_to(stream);
        }

        write_token(stream, TOKEN_GHOST_CONTROLLER);
        stream.write_count(self.controllers.len() as u32);
        for controller in &self.controllers {
            stream.write_u8(controller.controller_type);
            let _ = stream.write_blob(&controller.data);
        }

        write_token(stream, TOKEN_GHOST_TAIL);
        stream.write_u8(self.periods_without_witness);
        stream.write_u8(self.aoi_update_scheme_id);
    }

    /// `Entity::readGhostDataFromStream` (`entity.cpp:1751-1820`): returns
    /// the snapshot plus the [`GhostSeed`] the ghost derives from it. Any
    /// token mismatch, truncation or trailing byte is an error rather than
    /// the reference's `CRITICAL_MSG` / `MF_ASSERT`.
    pub fn read_from(
        stream: &mut BinaryStream,
    ) -> Result<(GhostSnapshot, GhostSeed), GhostStreamError> {
        let entity_id = stream.read_u32().map_err(|_| GhostStreamError::Stream)?;
        let entity_type_id = stream.read_u16().map_err(|_| GhostStreamError::Stream)?;
        let snapshot = read_internal(stream)?;
        let seed = GhostSeed {
            global_position: snapshot.local_position,
            last_trimmed_event_number: snapshot.last_event_number,
        };
        Ok((
            GhostSnapshot {
                entity_id,
                entity_type_id,
                ..snapshot
            },
            seed,
        ))
    }
}

/// `readGhostDataFromStreamInternal` (`entity.cpp:1762-1820`).
fn read_internal(stream: &mut BinaryStream) -> Result<GhostSnapshot, GhostStreamError> {
    let mut snapshot = GhostSnapshot {
        entity_id: 0,
        entity_type_id: 0,
        num_times_real_offloaded: stream.read_u32().map_err(map_len)?,
        local_position: Vector3::new(
            stream.read_f32().map_err(map_len)?,
            stream.read_f32().map_err(map_len)?,
            stream.read_f32().map_err(map_len)?,
        ),
        is_on_ground: stream.read_bool().map_err(map_len)?,
        last_event_number: stream.read_i32().map_err(map_len)?,
        volatile_info: VolatileInfo {
            position_priority: stream.read_f32().map_err(map_len)?,
            yaw_priority: stream.read_f32().map_err(map_len)?,
            pitch_priority: stream.read_f32().map_err(map_len)?,
            roll_priority: stream.read_f32().map_err(map_len)?,
        },
        real_addr: stream.read_u64().map_err(map_len)?,
        base_addr: stream.read_u64().map_err(map_len)?,
        local_direction: Direction3 {
            yaw: stream.read_f32().map_err(map_len)?,
            pitch: stream.read_f32().map_err(map_len)?,
            roll: stream.read_f32().map_err(map_len)?,
        },
        property_event_stamps: Vec::new(),
        properties: Vec::new(),
        controllers: Vec::new(),
        periods_without_witness: 0,
        aoi_update_scheme_id: 0,
    };

    // `PropertyEventStamps::removeFromStream`.
    let stamp_count = stream.read_count().map_err(map_len)?;
    for _ in 0..stamp_count {
        snapshot
            .property_event_stamps
            .push(stream.read_u32().map_err(map_len)?);
    }

    read_token(stream, TOKEN_GHOST_PROPERTIES)?;

    let prop_count = stream.read_count().map_err(map_len)?;
    for _ in 0..prop_count {
        snapshot
            .properties
            .push(GhostPropertyValue::read_from(stream)?);
    }

    read_token(stream, TOKEN_GHOST_CONTROLLER)?;

    let controller_count = stream.read_count().map_err(map_len)?;
    for _ in 0..controller_count {
        let controller_type = stream.read_u8().map_err(map_len)?;
        let data = stream.read_blob().map_err(map_len)?;
        snapshot.controllers.push(GhostController {
            controller_type,
            data,
        });
    }

    read_token(stream, TOKEN_GHOST_TAIL)?;
    snapshot.periods_without_witness = stream.read_u8().map_err(map_len)?;
    snapshot.aoi_update_scheme_id = stream.read_u8().map_err(map_len)?;

    // `MF_ASSERT( data.remainingLength() == 0 )`.
    if !stream.at_end() {
        return Err(GhostStreamError::BadLength);
    }
    Ok(snapshot)
}

fn map_len(_: foundation::stream::StreamError) -> GhostStreamError {
    GhostStreamError::BadLength
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> GhostSnapshot {
        GhostSnapshot {
            entity_id: 4242,
            entity_type_id: 17,
            num_times_real_offloaded: 3,
            local_position: Vector3::new(12.5, 0.0, -7.25),
            is_on_ground: true,
            last_event_number: 91,
            volatile_info: VolatileInfo {
                position_priority: 0.5,
                yaw_priority: -1.0,
                pitch_priority: 2.0,
                roll_priority: -1.0,
            },
            real_addr: 0x0001_0a00_144e,
            base_addr: 0x0001_0a00_144f,
            local_direction: Direction3 {
                yaw: 1.25,
                pitch: 0.5,
                roll: -0.75,
            },
            property_event_stamps: vec![7, 0, 42],
            properties: vec![
                GhostPropertyValue::Bool(true),
                GhostPropertyValue::Int32(-123),
                GhostPropertyValue::Float(3.5),
                GhostPropertyValue::Str("ghosted".to_string()),
                GhostPropertyValue::Vector(Vector3::new(1.0, 2.0, 3.0)),
            ],
            controllers: vec![
                GhostController {
                    controller_type: 4,
                    data: vec![1, 2, 3],
                },
                GhostController {
                    controller_type: 9,
                    data: Vec::new(),
                },
            ],
            periods_without_witness: 6,
            aoi_update_scheme_id: 2,
        }
    }

    #[test]
    fn a_ghost_reads_back_what_the_real_wrote() {
        let real = sample_snapshot();
        let mut stream = BinaryStream::new();
        real.write_to(&mut stream);

        let (ghost, seed) = GhostSnapshot::read_from(&mut stream).unwrap();
        assert_eq!(ghost, real, "the ghost matches its real field for field");
        assert_eq!(
            seed.global_position, real.local_position,
            "globalPosition_ = localPosition_ on read"
        );
        assert_eq!(
            seed.last_trimmed_event_number, real.last_event_number,
            "the event history floor is pinned to the real's last event"
        );
    }

    #[test]
    fn an_empty_snapshot_round_trips() {
        let real = GhostSnapshot::default();
        let mut stream = BinaryStream::new();
        real.write_to(&mut stream);
        let (ghost, _) = GhostSnapshot::read_from(&mut stream).unwrap();
        assert_eq!(ghost, real);
    }

    #[test]
    fn corrupted_tokens_are_rejected() {
        let mut stream = BinaryStream::new();
        sample_snapshot().write_to(&mut stream);
        let bytes = stream.as_bytes().to_vec();

        // Find each token and flip its first byte.
        for token in [
            TOKEN_GHOST_PROPERTIES,
            TOKEN_GHOST_CONTROLLER,
            TOKEN_GHOST_TAIL,
        ] {
            let mut broken = bytes.clone();
            let pos = find_subslice(&broken, token).expect("token present");
            broken[pos] = b'X';
            let mut reader = BinaryStream::from_slice(&broken);
            assert_eq!(
                GhostSnapshot::read_from(&mut reader),
                Err(GhostStreamError::BadToken),
                "a broken token must fail the read"
            );
        }
    }

    #[test]
    fn a_cut_at_a_token_is_a_length_error() {
        let mut stream = BinaryStream::new();
        sample_snapshot().write_to(&mut stream);
        let full = stream.as_bytes().to_vec();
        let tail = find_subslice(&full, TOKEN_GHOST_TAIL).expect("tail token present");
        let mut reader = BinaryStream::from_slice(&full[..tail]);
        assert_eq!(
            GhostSnapshot::read_from(&mut reader),
            Err(GhostStreamError::BadLength)
        );
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut stream = BinaryStream::new();
        sample_snapshot().write_to(&mut stream);
        let mut padded = stream.as_bytes().to_vec();
        padded.push(0);
        let mut reader = BinaryStream::from_slice(&padded);
        assert_eq!(
            GhostSnapshot::read_from(&mut reader),
            Err(GhostStreamError::BadLength)
        );
    }

    #[test]
    fn a_truncated_stream_is_rejected() {
        let mut stream = BinaryStream::new();
        sample_snapshot().write_to(&mut stream);
        let full = stream.as_bytes().to_vec();
        for cut in [1, 5, 20, full.len() - 1] {
            let mut reader = BinaryStream::from_slice(&full[..cut]);
            assert!(
                GhostSnapshot::read_from(&mut reader).is_err(),
                "cut at {cut} must fail"
            );
        }
    }

    #[test]
    fn an_unknown_property_tag_is_rejected() {
        let mut stream = BinaryStream::new();
        let mut real = sample_snapshot();
        real.properties = vec![GhostPropertyValue::Int32(1)];
        real.write_to(&mut stream);
        let mut bytes = stream.as_bytes().to_vec();
        // The property region starts after the stamps; find it via the
        // token and the count that follows it.
        let pos = find_subslice(&bytes, TOKEN_GHOST_PROPERTIES).unwrap();
        bytes[pos + TOKEN_GHOST_PROPERTIES.len() + 1] = 0xff;
        let mut reader = BinaryStream::from_slice(&bytes);
        assert_eq!(
            GhostSnapshot::read_from(&mut reader),
            Err(GhostStreamError::BadToken)
        );
    }

    #[test]
    fn volatile_info_helpers() {
        let none = VolatileInfo::default();
        assert!(!none.should_send_position());
        assert!(!none.has_volatile());

        let some = VolatileInfo {
            position_priority: 0.1,
            ..VolatileInfo::default()
        };
        assert!(some.should_send_position());
        assert!(some.has_volatile());
        assert!(none.is_less_volatile_than(&some));
        assert!(!some.is_less_volatile_than(&none));
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
