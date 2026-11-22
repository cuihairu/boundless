//! BW-07-02-01: construct and stream the cellData creation payload.
//!
//! The reference `Base::sendCreateCellEntity` (`baseapp/base.cpp:3369`)
//! frames a standalone `Mercury::UDPBundle` as the
//! `CellAppInterface::createEntityNearEntity` request and streams the
//! creation data in the order recorded at
//! `cellapp/cellapp_interface.hpp:186-214`:
//!
//! ```text
//! ChannelVersion           channel version           (request framing)
//! bool                     isRestore                 (request framing)
//! StreamHelper::AddEntityData   (lib/server/stream_helper.hpp:57)
//!   EntityID               id
//!   EntityTypeID           typeID
//!   Vector3                position
//!   uint8                  flags  isOnGround 0x1 | hasTemplate 0x2
//!   Direction3D            direction
//!   EventNumber            lastEventNumber
//!   Mercury::Address       baseAddr (the owning BaseApp)
//! T: BW::string            templateID             when hasTemplate is set
//! S: CELL_DATA blob        the cellData dictionary, property-by-property in
//!                          definition order (`PyCellData::addToStream` ->
//!                          `EntityDescription::addDictionaryToStream`)
//! RealEntity blob          StreamHelper::addRealEntity (0-size today)
//! char                     'W' with witness data, '-' without
//! ```
//!
//! The nearby entity id that names `createEntityNearEntity` is part of the
//! request framing, not this payload. This module is the payload codec over
//! `foundation::stream::BinaryStream`, mirroring how server-runtime streams
//! every other cross-role message; the Mercury channel transport itself is
//! the `mercury` crate. Byte-level fidelity to the reference protocol is a
//! separate compatibility concern (the compat matrix), tracked apart from
//! the payload-consistency guarantee this module provides.

use crate::entity_creator::{EntityId, EntityType, PropValue};
use foundation::stream::{BinaryStream, StreamError};
use std::collections::HashMap;

/// `Mercury::Address`: the owning BaseApp address carried in `AddEntityData`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseAddr {
    /// The address IP in host byte order.
    pub ip: u32,
    /// The address port.
    pub port: u16,
}

impl BaseAddr {
    /// `Mercury::Address::NONE`.
    pub const NONE: BaseAddr = BaseAddr { ip: 0, port: 0 };

    /// Write the packed address: the IP then the port.
    pub fn write_to(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.ip);
        stream.write_u16(self.port);
    }

    /// Read a packed address.
    pub fn read_from(stream: &mut BinaryStream) -> Result<Self, CellDataError> {
        Ok(BaseAddr {
            ip: stream.read_u32()?,
            port: stream.read_u16()?,
        })
    }
}

/// `StreamHelper::AddEntityData::Flags`.
pub mod header_flags {
    /// `isOnGround`.
    pub const IS_ON_GROUND: u8 = 0x1;
    /// `hasTemplate`: the T path instead of the S blob.
    pub const HAS_TEMPLATE: u8 = 0x2;
}

/// The trailing `createEntity` witness char values.
pub mod witness {
    /// A witness is created; its backup data follows.
    pub const YES: u8 = b'W';
    /// No witness; nothing follows the char.
    pub const NO: u8 = b'-';
}

/// The `StreamHelper::AddEntityData` structure (packed, reference field
/// order).
#[derive(Clone, Debug, PartialEq)]
pub struct CellEntityHeader {
    /// The entity id.
    pub id: EntityId,
    /// The entity type id.
    pub type_id: u32,
    /// The creation position.
    pub position: [f32; 3],
    /// The `isOnGround` flag.
    pub is_on_ground: bool,
    /// The `hasTemplate` flag: template data (T) instead of the S blob.
    pub has_template: bool,
    /// The creation direction (yaw, pitch, roll).
    pub direction: [f32; 3],
    /// `EventNumber`: the last event in the history (1 for a fresh entity).
    pub last_event_number: u32,
    /// The owning BaseApp address.
    pub base_addr: BaseAddr,
}

impl CellEntityHeader {
    /// The packed flags byte.
    pub fn flags(&self) -> u8 {
        let mut flags = 0;
        if self.is_on_ground {
            flags |= header_flags::IS_ON_GROUND;
        }
        if self.has_template {
            flags |= header_flags::HAS_TEMPLATE;
        }
        flags
    }

    /// `stream << data` for `AddEntityData`: the packed field order.
    pub fn write_to(&self, stream: &mut BinaryStream) {
        stream.write_u32(self.id);
        stream.write_u32(self.type_id);
        for component in &self.position {
            stream.write_f32(*component);
        }
        stream.write_u8(self.flags());
        for component in &self.direction {
            stream.write_f32(*component);
        }
        stream.write_u32(self.last_event_number);
        self.base_addr.write_to(stream);
    }

    /// `stream >> data` for `AddEntityData`.
    pub fn read_from(stream: &mut BinaryStream) -> Result<Self, CellDataError> {
        let id = stream.read_u32()?;
        let type_id = stream.read_u32()?;
        let mut position = [0.0; 3];
        for component in &mut position {
            *component = stream.read_f32()?;
        }
        let flags = stream.read_u8()?;
        let mut direction = [0.0; 3];
        for component in &mut direction {
            *component = stream.read_f32()?;
        }
        let last_event_number = stream.read_u32()?;
        let base_addr = BaseAddr::read_from(stream)?;
        Ok(CellEntityHeader {
            id,
            type_id,
            position,
            is_on_ground: flags & header_flags::IS_ON_GROUND != 0,
            has_template: flags & header_flags::HAS_TEMPLATE != 0,
            direction,
            last_event_number,
            base_addr,
        })
    }
}

/// A payload decode failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellDataError {
    /// The stream ran past its end, or a string was not valid UTF-8.
    Truncated,
    /// The trailing witness char was neither `W` nor `-`.
    BadWitness(u8),
    /// A witness is requested (`W`) and witness backup data is not modelled
    /// yet (a later item).
    WitnessNotSupported,
    /// The message is a restore (isRestore set) and the restore blobs are a
    /// separate item.
    RestoreNotSupported,
}

impl From<StreamError> for CellDataError {
    fn from(_: StreamError) -> Self {
        CellDataError::Truncated
    }
}

/// The builtin spatial properties carried by the header and the creation
/// context, never streamed in the CELL_DATA blob (they are not user
/// `DataDescription`s).
pub const SPATIAL_BUILTINS: [&str; 3] = ["position", "direction", "spaceID"];

/// Whether `name` is a builtin spatial property.
pub fn is_builtin_spatial(name: &str) -> bool {
    SPATIAL_BUILTINS.contains(&name)
}

/// Encode one property value in the model's payload encoding (little-endian
/// scalars, a packed-int string, a one-byte bool and three little-endian
/// floats).
pub fn write_prop_value(stream: &mut BinaryStream, value: &PropValue) -> Result<(), CellDataError> {
    match value {
        PropValue::Int(v) => {
            stream.write_i64(*v);
        }
        PropValue::Float(v) => {
            stream.write_f64(*v);
        }
        PropValue::Str(s) => {
            stream.write_string(s)?;
        }
        PropValue::Bool(v) => {
            stream.write_bool(*v);
        }
        PropValue::Vec3(v) => {
            for component in v {
                stream.write_f32(*component);
            }
        }
    }
    Ok(())
}

/// Decode one property value of the schema type of `witness`.
pub fn read_prop_value(
    stream: &mut BinaryStream,
    witness: &PropValue,
) -> Result<PropValue, CellDataError> {
    Ok(match witness {
        PropValue::Int(_) => PropValue::Int(stream.read_i64()?),
        PropValue::Float(_) => PropValue::Float(stream.read_f64()?),
        PropValue::Str(_) => PropValue::Str(stream.read_string()?),
        PropValue::Bool(_) => PropValue::Bool(stream.read_bool()?),
        PropValue::Vec3(_) => {
            let mut value = [0.0; 3];
            for component in &mut value {
                *component = stream.read_f32()?;
            }
            PropValue::Vec3(value)
        }
    })
}

/// `EntityDescription::addDictionaryToStream( ..., CELL_DATA )`: stream
/// every user cell-data property in definition order, taking the value from
/// `cell_data` or falling back to the property's initial value.
pub fn write_cell_data(
    stream: &mut BinaryStream,
    ty: &EntityType,
    cell_data: &HashMap<String, PropValue>,
) -> Result<(), CellDataError> {
    for prop in &ty.properties {
        if !prop.cell_data || is_builtin_spatial(&prop.name) {
            continue;
        }
        let value = cell_data.get(&prop.name).unwrap_or(&prop.initial);
        write_prop_value(stream, value)?;
    }
    Ok(())
}

/// Read the CELL_DATA blob back, decoding each property by its schema type.
pub fn read_cell_data(
    stream: &mut BinaryStream,
    ty: &EntityType,
) -> Result<HashMap<String, PropValue>, CellDataError> {
    let mut data = HashMap::new();
    for prop in &ty.properties {
        if !prop.cell_data || is_builtin_spatial(&prop.name) {
            continue;
        }
        let value = read_prop_value(stream, &prop.initial)?;
        data.insert(prop.name.clone(), value);
    }
    Ok(data)
}

/// The decoded `createEntity` payload.
#[derive(Clone, Debug, PartialEq)]
pub struct CreateEntityInit {
    /// The `AddEntityData` header.
    pub header: CellEntityHeader,
    /// The `CELL_DATA` dictionary (empty on the template path).
    pub cell_data: HashMap<String, PropValue>,
    /// `Some` on the template path, `None` when the S blob was streamed.
    pub template_id: Option<String>,
}

/// `Base::addCellCreationData` (`baseapp/base.cpp:3619`): stream the header,
/// then the template id (T) or the CELL_DATA blob (S), then the no-witness
/// char. `template_id` selects the T path; the caller keeps
/// `header.has_template` consistent with it.
pub fn write_create_entity_payload(
    stream: &mut BinaryStream,
    header: &CellEntityHeader,
    ty: &EntityType,
    cell_data: &HashMap<String, PropValue>,
    template_id: Option<&str>,
) -> Result<(), CellDataError> {
    header.write_to(stream);
    match template_id {
        Some(id) => {
            stream.write_string(id)?;
        }
        None => write_cell_data(stream, ty, cell_data)?,
    }
    stream.write_u8(witness::NO);
    Ok(())
}

/// Decode a `createEntity` payload (the message body after the request
/// framing).
pub fn read_create_entity_payload(
    stream: &mut BinaryStream,
    ty: &EntityType,
) -> Result<CreateEntityInit, CellDataError> {
    let header = CellEntityHeader::read_from(stream)?;
    let (template_id, cell_data) = if header.has_template {
        (Some(stream.read_string()?), HashMap::new())
    } else {
        (None, read_cell_data(stream, ty)?)
    };
    match stream.read_u8()? {
        witness::NO => {}
        witness::YES => return Err(CellDataError::WitnessNotSupported),
        other => return Err(CellDataError::BadWitness(other)),
    }
    Ok(CreateEntityInit {
        header,
        cell_data,
        template_id,
    })
}

/// The decoded `createEntityNearEntity` request (the message body after the
/// interface header).
#[derive(Clone, Debug, PartialEq)]
pub struct CreateEntityNearRequest {
    /// The anchor entity id: the new entity is created near it.
    pub nearby_id: EntityId,
    /// The channel version of the sender.
    pub channel_version: u32,
    /// The `createEntity` payload (header + cellData blob + witness).
    pub init: CreateEntityInit,
}

/// `Base::sendCreateCellEntity` (`baseapp/base.cpp:3369`): the
/// `createEntityNearEntity` message body — the anchor id, the channel
/// version, the `isRestore` flag (always false on the create path) and then
/// `Base::addCellCreationData`. The interface header (message id, flags,
/// body length) is the Mercury framing and is not part of this body.
pub fn write_create_entity_near_request(
    stream: &mut BinaryStream,
    nearby_id: EntityId,
    channel_version: u32,
    header: &CellEntityHeader,
    ty: &EntityType,
    cell_data: &HashMap<String, PropValue>,
    template_id: Option<&str>,
) -> Result<(), CellDataError> {
    stream.write_u32(nearby_id);
    stream.write_u32(channel_version);
    stream.write_bool(false); // isRestore: the create path never restores.
    write_create_entity_payload(stream, header, ty, cell_data, template_id)
}

/// Decode a `createEntityNearEntity` request body.
pub fn read_create_entity_near_request(
    stream: &mut BinaryStream,
    ty: &EntityType,
) -> Result<CreateEntityNearRequest, CellDataError> {
    let nearby_id = stream.read_u32()?;
    let channel_version = stream.read_u32()?;
    if stream.read_bool()? {
        // The restore path carries the VolatileInfo and backup blobs the
        // create path never does; it is a separate item.
        return Err(CellDataError::RestoreNotSupported);
    }
    let init = read_create_entity_payload(stream, ty)?;
    Ok(CreateEntityNearRequest {
        nearby_id,
        channel_version,
        init,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_creator::{CreateArgs, EntityCreator, IdPool, PropDesc};

    /// A cell-capable entity type with one of each cell property kind and a
    /// base-only property, mirroring the `entity_creator` tests.
    fn player_type() -> EntityType {
        EntityType {
            id: 7,
            name: "Player".to_string(),
            can_be_on_base: true,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![
                PropDesc::new(
                    "name",
                    true,
                    false,
                    false,
                    true,
                    PropValue::Str(String::new()),
                ),
                PropDesc::new("pos", false, true, false, true, PropValue::Vec3([0.0; 3])),
                PropDesc::new("health", true, true, false, true, PropValue::Int(100)),
                PropDesc::new("speed", false, true, false, true, PropValue::Float(0.0)),
                PropDesc::new("alive", false, true, false, true, PropValue::Bool(true)),
            ],
        }
    }

    fn creator() -> EntityCreator {
        let mut pool = IdPool::new(5, 10, 50, 100);
        pool.refill(&[42, 43]);
        let mut creator = EntityCreator::new(pool);
        creator.add_type(player_type());
        creator
    }

    /// `createBase` then a `createEntity` send/read round trip: the target
    /// Cell decodes exactly the cellData the Base built.
    #[test]
    fn a_created_base_sends_a_consistent_cell_data_payload() {
        let mut creator = creator();
        let mut args = CreateArgs::new();
        args.insert("name".to_string(), PropValue::Str("bob".to_string()));
        args.insert("position".to_string(), PropValue::Vec3([10.0, 20.0, 30.0]));
        args.insert("pos".to_string(), PropValue::Vec3([1.5, 2.5, 3.5]));
        args.insert("health".to_string(), PropValue::Int(250));
        let base = creator.create_base_locally("Player", args).unwrap();

        // The sender's header: the builtin spatial defaults, the allocated
        // id and the owning BaseApp address.
        let header = CellEntityHeader {
            id: base.id,
            type_id: base.type_id,
            position: [10.0, 20.0, 30.0],
            is_on_ground: true,
            has_template: false,
            direction: [0.0, 0.0, 0.0],
            last_event_number: 1,
            base_addr: BaseAddr {
                ip: 0x0a000001,
                port: 20010,
            },
        };

        let ty = player_type();
        let mut stream = BinaryStream::new();
        write_create_entity_payload(&mut stream, &header, &ty, &base.cell_props, None).unwrap();

        // The Cell reads the payload back off the wire.
        stream.rewind();
        let init = read_create_entity_payload(&mut stream, &ty).unwrap();

        // The header arrives intact, including the spatial builtins.
        assert_eq!(init.header, header);
        // Every user cell-data property arrives with the value the Base
        // built (definition order, initial values for the unset ones).
        for prop in &ty.properties {
            if prop.cell_data && !is_builtin_spatial(&prop.name) {
                assert_eq!(
                    init.cell_data.get(&prop.name),
                    base.cell_props.get(&prop.name)
                );
            }
        }
        assert_eq!(
            init.cell_data.get("pos"),
            Some(&PropValue::Vec3([1.5, 2.5, 3.5]))
        );
        assert_eq!(init.cell_data.get("health"), Some(&PropValue::Int(250)));
        assert_eq!(init.cell_data.get("speed"), Some(&PropValue::Float(0.0)));
        assert_eq!(init.cell_data.get("alive"), Some(&PropValue::Bool(true)));
        assert!(init.template_id.is_none());
    }

    /// The T path streams the template id instead of the CELL_DATA blob.
    #[test]
    fn a_template_path_streams_the_id_not_the_blob() {
        let ty = player_type();
        let header = CellEntityHeader {
            id: 9,
            type_id: 7,
            position: [0.0; 3],
            is_on_ground: false,
            has_template: true,
            direction: [0.0; 3],
            last_event_number: 1,
            base_addr: BaseAddr::NONE,
        };
        let mut stream = BinaryStream::new();
        write_create_entity_payload(
            &mut stream,
            &header,
            &ty,
            &HashMap::new(),
            Some("TemplateA"),
        )
        .unwrap();

        stream.rewind();
        let init = read_create_entity_payload(&mut stream, &ty).unwrap();
        assert_eq!(init.header, header);
        assert_eq!(init.template_id.as_deref(), Some("TemplateA"));
        assert!(init.cell_data.is_empty());
    }

    /// A `W` witness char is recognised but not supported yet.
    #[test]
    fn a_witness_request_is_not_supported() {
        let ty = player_type();
        let header = CellEntityHeader {
            id: 1,
            type_id: 7,
            position: [0.0; 3],
            is_on_ground: true,
            has_template: true,
            direction: [0.0; 3],
            last_event_number: 1,
            base_addr: BaseAddr::NONE,
        };
        let mut stream = BinaryStream::new();
        write_create_entity_payload(&mut stream, &header, &ty, &HashMap::new(), Some("T")).unwrap();
        let mut bytes = stream.as_bytes().to_vec();
        let last = bytes.len() - 1;
        bytes[last] = witness::YES;

        let mut bad = BinaryStream::from_slice(&bytes);
        assert_eq!(
            read_create_entity_payload(&mut bad, &ty).unwrap_err(),
            CellDataError::WitnessNotSupported
        );
    }

    /// A trailing char that is neither `W` nor `-` is a decode error.
    #[test]
    fn an_unknown_witness_char_is_rejected() {
        let ty = player_type();
        let header = CellEntityHeader {
            id: 1,
            type_id: 7,
            position: [0.0; 3],
            is_on_ground: true,
            has_template: true,
            direction: [0.0; 3],
            last_event_number: 1,
            base_addr: BaseAddr::NONE,
        };
        let mut stream = BinaryStream::new();
        write_create_entity_payload(&mut stream, &header, &ty, &HashMap::new(), Some("T")).unwrap();
        let mut bytes = stream.as_bytes().to_vec();
        let last = bytes.len() - 1;
        bytes[last] = b'X';

        let mut bad = BinaryStream::from_slice(&bytes);
        assert_eq!(
            read_create_entity_payload(&mut bad, &ty).unwrap_err(),
            CellDataError::BadWitness(b'X')
        );
    }

    /// Reading a payload from an empty stream fails without panic.
    #[test]
    fn a_truncated_payload_is_a_decode_error() {
        let ty = player_type();
        let mut empty = BinaryStream::new();
        assert_eq!(
            read_create_entity_payload(&mut empty, &ty).unwrap_err(),
            CellDataError::Truncated
        );
    }

    /// The full `createEntityNearEntity` request (framing + payload) round
    /// trips: the Cell decodes the anchor, the channel version and the same
    /// cellData the Base built.
    #[test]
    fn a_create_entity_near_request_round_trips() {
        let mut creator = creator();
        let mut args = CreateArgs::new();
        args.insert("name".to_string(), PropValue::Str("carol".to_string()));
        args.insert("pos".to_string(), PropValue::Vec3([4.0, 5.0, 6.0]));
        let base = creator.create_base_locally("Player", args).unwrap();

        let header = CellEntityHeader {
            id: base.id,
            type_id: base.type_id,
            position: [0.0, 0.0, 0.0],
            is_on_ground: false,
            has_template: false,
            direction: [0.0, 0.0, 0.0],
            last_event_number: 1,
            base_addr: BaseAddr {
                ip: 0x7f000001,
                port: 20020,
            },
        };

        let ty = player_type();
        let mut stream = BinaryStream::new();
        write_create_entity_near_request(&mut stream, 42, 5, &header, &ty, &base.cell_props, None)
            .unwrap();

        stream.rewind();
        let request = read_create_entity_near_request(&mut stream, &ty).unwrap();

        assert_eq!(request.nearby_id, 42);
        assert_eq!(request.channel_version, 5);
        assert_eq!(request.init.header, header);
        assert_eq!(
            request.init.cell_data.get("pos"),
            Some(&PropValue::Vec3([4.0, 5.0, 6.0]))
        );
        assert_eq!(
            request.init.cell_data.get("health"),
            Some(&PropValue::Int(100))
        );
    }

    /// A restore request (isRestore set) is not the create path and is a
    /// decode error until the restore blobs are modelled.
    #[test]
    fn a_restore_request_is_not_the_create_path() {
        let ty = player_type();
        let header = CellEntityHeader {
            id: 1,
            type_id: 7,
            position: [0.0; 3],
            is_on_ground: true,
            has_template: false,
            direction: [0.0; 3],
            last_event_number: 1,
            base_addr: BaseAddr::NONE,
        };
        let mut stream = BinaryStream::new();
        stream.write_u32(1);
        stream.write_u32(1);
        stream.write_bool(true); // isRestore
        header.write_to(&mut stream);

        assert_eq!(
            read_create_entity_near_request(&mut stream, &ty).unwrap_err(),
            CellDataError::RestoreNotSupported
        );
    }
}
