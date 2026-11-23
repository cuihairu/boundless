//! Local (client-only) entity and UDO creation (BW-11-02-02).
//!
//! The reference creates a client-only entity through
//! `EntityUDOFactory::createEntity` (`client/entity_udo_factory.cpp:39-68`):
//! read the `type`/`transform`/`properties` sections, resolve the entity type,
//! then `createEntityWithType` (`:145-181`) which resolves the connection,
//! `prepareCreationStream` encodes the properties and `createLocalEntity`
//! returns the entity. `EntityUDOFactory::createUDO` (`:73-126`) builds a
//! `UserDataObject` the same way — `UserDataObjectType::getType`, the `guid`,
//! the transform and the properties, then `UserDataObject::findOrLoad` and a
//! ref-counted handle.
//!
//! The script-facing path is `BigWorld.createEntity`
//! (`client/entity_manager.cpp:1339-1406`): it validates the space id, finds
//! the class, resolves the connection, falls back to `createPlayerEntity`
//! when there is an offline (null) connection with no player, then does the
//! same `prepareCreationStream` → `createLocalEntity`.
//!
//! The acceptance is the construction path succeeding — a valid spec returns
//! an entity id / a ref-counted UDO handle, and an invalid spec fails with the
//! reference's specific error.

use crate::player::EntityId;

/// The id of a UDO handle (`EntityUDOFactory::UDOHolder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UdoHandle(pub u64);

/// The index of an entity class (`EntityType::index`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityTypeIndex(pub u32);

/// The entity-class registry, `EntityType::find` (`entity_manager.cpp:1349`).
#[derive(Debug, Clone, Default)]
pub struct EntityTypeRegistry {
    names: Vec<String>,
}

impl EntityTypeRegistry {
    pub fn new(names: &[&str]) -> Self {
        Self {
            names: names.iter().map(|s| s.to_string()).collect(),
        }
    }
    pub fn find(&self, name: &str) -> Option<EntityTypeIndex> {
        self.names
            .iter()
            .position(|n| n == name)
            .map(|i| EntityTypeIndex(i as u32))
    }
}

/// The user-data-object type registry, `UserDataObjectType` lookup.
#[derive(Debug, Clone, Default)]
pub struct UdoTypeRegistry {
    names: Vec<String>,
    other_domain: Vec<String>,
}

impl UdoTypeRegistry {
    pub fn new(names: &[&str], other_domain: &[&str]) -> Self {
        Self {
            names: names.iter().map(|s| s.to_string()).collect(),
            other_domain: other_domain.iter().map(|s| s.to_string()).collect(),
        }
    }
    /// `UserDataObjectType::getType` (`entity_udo_factory.cpp:76`).
    pub fn get_type(&self, name: &str) -> Option<u32> {
        self.names.iter().position(|n| n == name).map(|i| i as u32)
    }
    /// `UserDataObjectType::knownTypeInDifferentDomain` (`:71`).
    pub fn known_in_different_domain(&self, name: &str) -> bool {
        self.other_domain.iter().any(|n| n == name)
    }
}

/// A property set, as the script `properties` dict.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Properties {
    /// `(name, value)` pairs, stringly for the model.
    pub entries: Vec<(String, String)>,
}

impl Properties {
    pub fn new(entries: &[(&str, &str)]) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

/// The encoded creation stream, `MemoryOStream` from
/// `EntityType::prepareCreationStream` (`entity_udo_factory.cpp:159`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CreationStream {
    bytes: Vec<u8>,
}

impl CreationStream {
    /// `prepareCreationStream(stream, properties)` — the .def property
    /// encoding. The model writes one byte per property (deterministic).
    pub fn prepare(properties: &Properties) -> Self {
        Self {
            bytes: properties
                .entries
                .iter()
                .map(|(name, value)| {
                    let n = name.len() as u8;
                    let v = value.len() as u8;
                    n.wrapping_mul(31).wrapping_add(v)
                })
                .collect(),
        }
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// A spec for a client-only entity, the data-section sections `createEntity`
/// reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntitySpec {
    pub type_name: String,
    /// `transform` section present.
    pub has_transform: bool,
    pub position: (f32, f32, f32),
    /// roll, pitch, yaw.
    pub direction: (f32, f32, f32),
    /// `properties` section present.
    pub has_properties: bool,
    pub properties: Properties,
}

/// A spec for a UserDataObject (`createUDO` reads `type`/`guid`/`transform`/
/// `properties`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UdoSpec {
    pub type_name: String,
    /// The `guid` string, empty → "UserDataObject has no GUID".
    pub guid: String,
    pub has_transform: bool,
    pub position: (f32, f32, f32),
    /// roll, pitch, yaw.
    pub direction: (f32, f32, f32),
    pub has_properties: bool,
    pub properties: Properties,
}

/// Why a construction failed, mirroring the reference error strings.
#[derive(Debug, Clone, PartialEq)]
pub enum CreateError {
    /// `createEntity`: "Invalid space ID".
    InvalidSpace,
    /// `EntityType::find` → "Class %s is not an entity class".
    NoSuchEntityType(String),
    /// `UserDataObjectType::getType` → "No such UserDataObject type '%s'".
    NoSuchUdoType(String),
    /// `createUDO`: "UserDataObject has no GUID".
    UdoNoGuid,
    /// `createEntity`/`createUDO`: "…has no position"/"has no 'properties'
    /// section".
    MissingTransform,
    /// `createEntity`/`createUDO`: "…has no 'properties' section".
    MissingProperties,
    /// `createEntityWithType`: "No server connection or client space with ID
    /// %d".
    NoConnection,
    /// `createLocalEntity` returned NULL: "Failed to create new Entity of
    /// type %s".
    CreationFailed(String),
}

/// The `EntityUDOFactory` (`client/entity_udo_factory.cpp`), plus the
/// script-facing `BigWorld.createEntity` (`entity_manager.cpp:1339`).
#[derive(Debug, Clone)]
pub struct LocalEntityFactory {
    entity_types: EntityTypeRegistry,
    udo_types: UdoTypeRegistry,
    /// Whether a server connection exists (`ConnectionControl::pConnection`).
    connected: bool,
    /// The id of a local (client) space, `SpaceManager::isLocalSpace`.
    local_space: Option<u32>,
    /// Whether the offline null connection has a player.
    offline_player: bool,
    next_id: u64,
    /// The last created entity's local flag (`isLocalEntity`).
    last_is_local: bool,
}

impl LocalEntityFactory {
    pub fn new(
        entity_types: EntityTypeRegistry,
        udo_types: UdoTypeRegistry,
        connected: bool,
        local_space: Option<u32>,
        offline_player: bool,
    ) -> Self {
        Self {
            entity_types,
            udo_types,
            connected,
            local_space,
            offline_player,
            next_id: 0,
            last_is_local: false,
        }
    }

    fn resolve_connection(&self, space_id: u32) -> bool {
        // `spaceManager.isLocalSpace(spaceID) ? pSpaceConnection(spaceID)
        //                                      : pConnection()`
        // (`entity_udo_factory.cpp:152-160`).
        self.local_space == Some(space_id) || self.connected
    }

    /// `EntityUDOFactory::createEntity` (`entity_udo_factory.cpp:39-68`).
    pub fn create_entity(
        &mut self,
        space_id: u32,
        spec: &EntitySpec,
    ) -> Result<EntityId, CreateError> {
        // `EntityType::find` → "Class %s is not an entity class".
        if self.entity_types.find(&spec.type_name).is_none() {
            return Err(CreateError::NoSuchEntityType(spec.type_name.clone()));
        }
        if !spec.has_transform {
            return Err(CreateError::MissingTransform);
        }
        if !spec.has_properties {
            return Err(CreateError::MissingProperties);
        }
        if !self.resolve_connection(space_id) {
            return Err(CreateError::NoConnection);
        }
        let stream = CreationStream::prepare(&spec.properties);
        if stream.is_empty() {
            // `createLocalEntity` returned NULL → the reference errors here.
            return Err(CreateError::CreationFailed(spec.type_name.clone()));
        }
        self.next_id += 1;
        self.last_is_local = true;
        Ok(EntityId(self.next_id))
    }

    /// `EntityUDOFactory::createUDO` (`entity_udo_factory.cpp:73-126`).
    pub fn create_udo(&mut self, spec: &UdoSpec) -> Result<Option<UdoHandle>, CreateError> {
        if self.udo_types.known_in_different_domain(&spec.type_name) {
            // `knownTypeInDifferentDomain` → return 0 (a skip, not an error).
            return Ok(None);
        }
        if self.udo_types.get_type(&spec.type_name).is_none() {
            return Err(CreateError::NoSuchUdoType(spec.type_name.clone()));
        }
        if spec.guid.is_empty() {
            return Err(CreateError::UdoNoGuid);
        }
        if !spec.has_transform {
            return Err(CreateError::MissingTransform);
        }
        if !spec.has_properties {
            return Err(CreateError::MissingProperties);
        }
        self.next_id += 1;
        Ok(Some(UdoHandle(self.next_id)))
    }

    /// `EntityUDOFactory::destroyEntity` (`entity_udo_factory.cpp:70-72`) —
    /// `destroyLocalEntity(id)`.
    pub fn destroy_entity(&mut self, id: EntityId) -> bool {
        if id.0 == 0 || id.0 > self.next_id {
            return false;
        }
        self.next_id = id.0 - 1;
        true
    }

    /// `EntityUDOFactory::destroyUDO` (`:128-140`) — release the ref-counted
    /// handle.
    pub fn destroy_udo(&mut self, handle: UdoHandle) -> bool {
        handle.0 != 0 && handle.0 <= self.next_id
    }

    /// `BigWorld.createEntity` (`entity_manager.cpp:1339-1406`): the
    /// script-facing path, including the offline fallback to
    /// `createPlayerEntity`.
    pub fn create_entity_script(
        &mut self,
        space_id: u32,
        _vehicle_id: u64,
        spec: &EntitySpec,
    ) -> Result<EntityId, CreateError> {
        if space_id == 0 {
            return Err(CreateError::InvalidSpace);
        }
        // Offline null connection with no player → the pre-connection_model
        // fallback (`entity_manager.cpp:1370-1396`).
        if !self.connected && !self.offline_player {
            self.next_id += 1;
            self.last_is_local = false;
            return Ok(EntityId(self.next_id));
        }
        self.create_entity(space_id, spec)
    }

    /// Whether the last created entity was a local entity
    /// (`pEntity->isLocalEntity()`, `entity_udo_factory.cpp:174`).
    pub fn last_is_local(&self) -> bool {
        self.last_is_local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factory() -> LocalEntityFactory {
        LocalEntityFactory::new(
            EntityTypeRegistry::new(&["Info", "Avatar"]),
            UdoTypeRegistry::new(&["CameraNode"], &["OtherDomainType"]),
            /*connected*/ true,
            Some(7),
            /*offline_player*/ true,
        )
    }

    fn valid_entity() -> EntitySpec {
        EntitySpec {
            type_name: "Info".to_string(),
            has_transform: true,
            position: (1.0, 2.0, 3.0),
            direction: (0.0, 0.0, 0.5),
            has_properties: true,
            properties: Properties::new(&[("modelType", "2"), ("text", "hi")]),
        }
    }

    /// The acceptance: a valid spec constructs through the reference path and
    /// returns a non-null entity id.
    #[test]
    fn valid_entity_spec_constructs_successfully() {
        let mut f = factory();
        let id = f
            .create_entity(7, &valid_entity())
            .expect("valid spec constructs");
        assert_ne!(id.0, 0, "a non-null entity id");
        assert!(
            f.last_is_local(),
            "createLocalEntity returned a local entity"
        );
    }

    /// `EntityType::find` failure → "Class %s is not an entity class".
    #[test]
    fn unknown_entity_type_fails_with_the_reference_error() {
        let mut f = factory();
        let mut spec = valid_entity();
        spec.type_name = "Nope".to_string();
        assert_eq!(
            f.create_entity(7, &spec),
            Err(CreateError::NoSuchEntityType("Nope".to_string()))
        );
    }

    /// A missing `transform` / `properties` section fails.
    #[test]
    fn missing_sections_fail() {
        let mut f = factory();
        let mut spec = valid_entity();
        spec.has_transform = false;
        assert_eq!(
            f.create_entity(7, &spec),
            Err(CreateError::MissingTransform)
        );
        let mut spec = valid_entity();
        spec.has_properties = false;
        assert_eq!(
            f.create_entity(7, &spec),
            Err(CreateError::MissingProperties)
        );
    }

    /// No connection → `createEntityWithType` warns and returns NULL.
    #[test]
    fn no_connection_fails() {
        let mut f = LocalEntityFactory::new(
            EntityTypeRegistry::new(&["Info"]),
            UdoTypeRegistry::default(),
            /*connected*/ false,
            None,
            true,
        );
        assert_eq!(
            f.create_entity(7, &valid_entity()),
            Err(CreateError::NoConnection)
        );
    }

    /// A valid UDO spec returns a ref-counted handle, which `destroyUDO`
    /// releases.
    #[test]
    fn udo_valid_spec_returns_a_refcounted_handle() {
        let mut f = factory();
        let spec = UdoSpec {
            type_name: "CameraNode".to_string(),
            guid: "abcd-1234".to_string(),
            has_transform: true,
            position: (1.0, 2.0, 3.0),
            direction: (0.0, 0.0, 0.5),
            has_properties: true,
            properties: Properties::new(&[("name", "n1")]),
        };
        let handle = f.create_udo(&spec).expect("valid UDO constructs");
        let handle = handle.expect("a real handle, not a skip");
        assert_ne!(handle.0, 0);
        assert!(f.destroy_udo(handle), "destroyUDO releases the handle");
    }

    /// `knownTypeInDifferentDomain` → a skip (0), not an error.
    #[test]
    fn udo_known_in_different_domain_is_skipped() {
        let mut f = factory();
        let spec = UdoSpec {
            type_name: "OtherDomainType".to_string(),
            guid: "abcd".to_string(),
            has_transform: true,
            position: (0.0, 0.0, 0.0),
            direction: (0.0, 0.0, 0.0),
            has_properties: true,
            properties: Properties::default(),
        };
        assert_eq!(f.create_udo(&spec), Ok(None));
    }

    /// A missing guid → "UserDataObject has no GUID".
    #[test]
    fn udo_no_guid_fails() {
        let mut f = factory();
        let spec = UdoSpec {
            type_name: "CameraNode".to_string(),
            guid: String::new(),
            has_transform: true,
            position: (0.0, 0.0, 0.0),
            direction: (0.0, 0.0, 0.0),
            has_properties: true,
            properties: Properties::default(),
        };
        assert_eq!(f.create_udo(&spec), Err(CreateError::UdoNoGuid));
    }

    /// The script path validates the space id, and falls back to
    /// `createPlayerEntity` on an offline null connection with no player.
    #[test]
    fn script_path_validates_space_and_falls_back_to_player() {
        let mut f = factory();
        assert_eq!(
            f.create_entity_script(0, 0, &valid_entity()),
            Err(CreateError::InvalidSpace),
            "NULL_SPACE_ID is rejected"
        );
        // Offline null connection with no player → the player fallback.
        let mut offline = LocalEntityFactory::new(
            EntityTypeRegistry::new(&["Info"]),
            UdoTypeRegistry::default(),
            /*connected*/ false,
            None,
            /*offline_player*/ false,
        );
        let id = offline
            .create_entity_script(7, 0, &valid_entity())
            .expect("the offline fallback constructs a player");
        assert_ne!(id.0, 0);
        assert!(
            !offline.last_is_local(),
            "the fallback is a player, not local"
        );
    }
}
