//! BW-01-01-01 entity and space identifier widths and sentinel values.
//!
//! Wire widths and sentinels are taken from the reference implementation's
//! `lib/network/basictypes.hpp` and `lib/space/forward_declarations.hpp`,
//! which are the authoritative definitions shared by every role.

/// A game timestamp: incremented by one every game tick and synchronised
/// between all clients and servers (`basictypes.hpp`).
pub type GameTime = u32;

/// Identifies an entity. Constant across every instance of the entity on
/// baseapp, cellapp and client, but not persistent across server runs.
///
/// IDs at or above [`FIRST_LOCAL_ENTITY_ID`] are reserved for client-only
/// entities; IDs below it are server-generated.
pub type EntityId = i32;

/// No entity is referenced.
pub const NULL_ENTITY_ID: EntityId = 0;

/// The first server-generated entity id.
pub const FIRST_ENTITY_ID: EntityId = 1;

/// First local (client-only) entity id. Leaves roughly 2.1 billion unique
/// server ids and 16 million client-only ids.
pub const FIRST_LOCAL_ENTITY_ID: EntityId = 0x7F00_0000;

/// Identifies a space.
pub type SpaceId = i32;

/// No space is referenced.
pub const NULL_SPACE_ID: SpaceId = 0;

/// Identifies the class of an entity.
pub type EntityTypeId = u16;

/// No entity type.
pub const INVALID_ENTITY_TYPE_ID: EntityTypeId = u16::MAX;

/// Identifies an entity's position in the database.
pub type DatabaseId = i64;

/// A database id that has not yet been assigned.
pub const PENDING_DATABASE_ID: DatabaseId = -1;

/// Identifies an instance of a server application.
pub type ServerAppInstanceId = i32;

/// Identifies a cell application.
pub type CellAppId = ServerAppInstanceId;

/// Identifies a base application.
pub type BaseAppId = ServerAppInstanceId;

/// Identifies a login application.
pub type LoginAppId = ServerAppInstanceId;

/// Identifies a database application.
pub type DbAppId = ServerAppInstanceId;

/// Authentication key used between the client app and the base app.
pub type SessionKey = u32;

/// Identifies a grid square. The high 16 bits are the signed X position and
/// the low 16 bits the signed Y position.
pub type GridId = u32;

/// Sequence number of an entity event.
pub type EventNumber = i32;

/// The first event number of an entity.
pub const INITIAL_EVENT_NUMBER: EventNumber = 1;

/// Sequence number used for volatile updates.
pub type VolatileNumber = u16;

/// Data level-of-detail level.
pub type DetailLevel = u8;

/// Maximum number of data LOD levels.
pub const MAX_DATA_LOD_LEVELS: u8 = 6;

/// Identifies a space type.
pub type SpaceTypeId = u32;

/// No entity is referenced.
pub const fn is_null_entity_id(id: EntityId) -> bool {
    id == NULL_ENTITY_ID
}

/// Whether the id belongs to the client-only range.
pub const fn is_local_entity_id(id: EntityId) -> bool {
    id >= FIRST_LOCAL_ENTITY_ID
}

/// Whether the id has not been assigned by the database yet.
pub const fn is_pending_database_id(id: DatabaseId) -> bool {
    id == PENDING_DATABASE_ID
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_widths_match_reference() {
        assert_eq!(size_of::<EntityId>(), 4);
        assert_eq!(size_of::<SpaceId>(), 4);
        assert_eq!(size_of::<DatabaseId>(), 8);
        assert_eq!(size_of::<GameTime>(), 4);
        assert_eq!(size_of::<SessionKey>(), 4);
        assert_eq!(size_of::<EntityTypeId>(), 2);
        assert_eq!(size_of::<VolatileNumber>(), 2);
        assert_eq!(size_of::<DetailLevel>(), 1);
    }

    #[test]
    fn entity_id_sentinels() {
        assert!(is_null_entity_id(NULL_ENTITY_ID));
        assert!(!is_null_entity_id(FIRST_ENTITY_ID));
        assert!(is_local_entity_id(FIRST_LOCAL_ENTITY_ID));
        assert!(!is_local_entity_id(FIRST_ENTITY_ID));
        assert!(is_pending_database_id(PENDING_DATABASE_ID));
        assert!(!is_pending_database_id(42));
    }
}
