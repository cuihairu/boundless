//! The XML database backend (`lib/db_storage_xml/xml_database.{hpp,cpp}`).
//!
//! The file-backed database keeps every entity as one child section of the
//! database root, named after its entity type and carrying a `databaseID`
//! child; server bookkeeping lives under `<_BigWorldInfo>`. The acceptance
//! case is the reference's own: a file written by the original XML database
//! parses, indexes and reads back, and everything this module writes parses
//! again (`BWResource::openSection` in `startup`, `xml_database.cpp:100-179`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use entitydef::entity_description::EntityDescription;
use entitydef::entity_map::EntityDescriptionMap;
use entitydef::flags::{BASE_DATA, CELL_DATA, ONLY_PERSISTENT_DATA};
use foundation::ids::{
    DatabaseId, EntityId, EntityTypeId, FIRST_ENTITY_ID, FIRST_LOCAL_ENTITY_ID, PENDING_DATABASE_ID,
};
use foundation::math::Vector3;
use foundation::stream::{BinaryStream, StreamError};
use resources::datasection::DataSection;

use crate::error::DbError;
use crate::log_on::LogOnRecord;
use crate::record::EntityRecord;
use crate::storage::{EntityKey, EntityStorage};

/// `DATABASE_INFO_SECTION` (`xml_database.cpp:34`): the bookkeeping child of
/// the database root.
pub const DATABASE_INFO_SECTION: &str = "_BigWorldInfo";
/// `DATABASE_LOGONMAPPING_SECTION` (`xml_database.cpp:35`): the billing
/// system's log-on mapping, inside the info section.
pub const DATABASE_LOGONMAPPING_SECTION: &str = "LogOnMapping";
/// `DATABASE_AUTO_LOAD_ENTITIES_SECTION` (`xml_database.cpp:36`): the
/// `<databaseID>` list of auto-loading entities, inside the info section.
pub const DATABASE_AUTO_LOAD_ENTITIES_SECTION: &str = "AutoLoadedEntities";
/// `DATABASE_AUTO_LOAD_SPACE_DATA_SECTION` (`xml_database.cpp:37`): the
/// archived space data, inside the info section.
pub const DATABASE_AUTO_LOAD_SPACE_DATA_SECTION: &str = "AutoLoadedSpaceData";

/// The property domains an entity row carries
/// (`xml_database.cpp:502-504`).
pub const DATABASE_DATA_DOMAINS: u32 = BASE_DATA | CELL_DATA | ONLY_PERSISTENT_DATA;

/// The `<databaseID>` tag every entity section carries
/// (`xml_database.cpp:216`).
const DATABASE_ID_TAG: &str = "databaseID";

/// `XMLDatabase::numArchives_` default (`xml_database.cpp:65`).
pub const DEFAULT_NUM_ARCHIVES: i32 = 5;

/// `UPDATE_AUTO_LOAD` (`xml_database.hpp:73-81`): whether a put updates the
/// entity's auto-load membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoLoadUpdate {
    /// `UPDATE_AUTO_LOAD_RETAIN`: leave the membership alone.
    Retain,
    /// `UPDATE_AUTO_LOAD_TRUE`: add the entity.
    True,
    /// `UPDATE_AUTO_LOAD_FALSE`: remove the entity.
    False,
}

/// The cell payload an entity row carries behind its properties when
/// `canBeOnCell()` (`xml_database.cpp:506-510`): position, direction, space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CellPose {
    /// The cell position.
    pub position: Vector3,
    /// The `Direction3D`: roll, pitch, yaw (`basictypes.hpp:239-251`).
    pub direction: [f32; 3],
    /// The space the entity stands in.
    pub space_id: i32,
}

impl CellPose {
    /// Stream the payload the way `stream << position << direction <<
    /// spaceID` does: three floats each, then the space id.
    pub fn write_to(&self, stream: &mut BinaryStream) {
        for i in 0..3 {
            stream.write_f32(self.position.get(i));
        }
        for component in &self.direction {
            stream.write_f32(*component);
        }
        stream.write_i32(self.space_id);
    }

    /// Read a streamed payload.
    pub fn read_from(stream: &mut BinaryStream) -> Result<Self, StreamError> {
        let mut position = Vector3::ZERO;
        for i in 0..3 {
            position.set(i, stream.read_f32()?);
        }
        let mut direction = [0.0f32; 3];
        for slot in &mut direction {
            *slot = stream.read_f32()?;
        }
        let space_id = stream.read_i32()?;
        Ok(Self {
            position,
            direction,
            space_id,
        })
    }
}

/// The outcome of a `getEntity` (`xml_database.cpp:441-535`): the resolved
/// key plus the cell payload, with `found` standing in for the handler's
/// `isOK`.
#[derive(Debug, Clone, PartialEq)]
pub struct GetOutcome {
    /// The resolved database id (the by-name lookup fills it in).
    pub db_id: DatabaseId,
    /// The identifier value read back from the section.
    pub name: Option<String>,
    /// Whether the row was found and streamed.
    pub found: bool,
    /// The cell payload, when the type can be on a cell.
    pub pose: Option<CellPose>,
}

impl GetOutcome {
    fn missing(db_id: DatabaseId) -> Self {
        Self {
            db_id,
            name: None,
            found: false,
            pose: None,
        }
    }
}

/// The XML database (`XMLDatabase`).
pub struct XmlDatabase {
    /// `pDatabaseSection_`: the whole database as one section tree.
    section: DataSection,
    /// `pEntityDefs_`.
    defs: EntityDescriptionMap,
    /// `nameToIdMaps_`: per type, identifier value -> database id.
    name_to_id: BTreeMap<EntityTypeId, BTreeMap<String, DatabaseId>>,
    /// `idToData_`: database id -> section name. The reference maps to the
    /// section pointer; the section is re-found by `(name, id)` so the map
    /// stays valid across writes.
    id_to_data: BTreeMap<DatabaseId, String>,
    /// `autoLoadEntityMap_`: the auto-loading database ids.
    auto_load: BTreeSet<DatabaseId>,
    /// `maxID_`.
    max_id: DatabaseId,
    /// `spareIDs_`.
    spare_ids: Vec<EntityId>,
    /// `nextID_`.
    next_id: EntityId,
    /// `numArchives_`.
    num_archives: i32,
    /// The file this database saves to (`Constants::xmlDatabaseFile()`).
    file_path: Option<String>,
}

impl XmlDatabase {
    /// An empty database (`startup` on a missing file,
    /// `xml_database.cpp:100-105`).
    pub fn new(defs: EntityDescriptionMap) -> Self {
        let mut database = Self {
            section: DataSection::new("root"),
            defs,
            name_to_id: BTreeMap::new(),
            id_to_data: BTreeMap::new(),
            auto_load: BTreeSet::new(),
            max_id: 0,
            spare_ids: Vec::new(),
            next_id: FIRST_ENTITY_ID,
            num_archives: DEFAULT_NUM_ARCHIVES,
            file_path: None,
        };
        database.init();
        database
    }

    /// `startup` (`xml_database.cpp:100-179`): parse an existing database
    /// file and index it.
    pub fn from_xml(xml: &str, defs: EntityDescriptionMap) -> Result<Self, DbError> {
        let section = resources::datasection::parse("", xml.as_bytes())
            .map_err(|e| DbError::Storage(format!("parse database: {e}")))?;
        let mut database = Self {
            section,
            defs,
            name_to_id: BTreeMap::new(),
            id_to_data: BTreeMap::new(),
            auto_load: BTreeSet::new(),
            max_id: 0,
            spare_ids: Vec::new(),
            next_id: FIRST_ENTITY_ID,
            num_archives: DEFAULT_NUM_ARCHIVES,
            file_path: None,
        };
        database.init();
        Ok(database)
    }

    /// Point `save`/`archive` at a file (`Constants::xmlDatabaseFile()`).
    pub fn set_file_path(&mut self, path: impl Into<String>) {
        self.file_path = Some(path.into());
    }

    /// The database section rendered as XML (`timedSave`,
    /// `xml_database.cpp:383-398`).
    pub fn to_xml(&mut self) -> String {
        self.section.write_to_string()
    }

    /// `maxID_`.
    pub fn max_id(&self) -> DatabaseId {
        self.max_id
    }

    /// `numArchives_`.
    pub fn num_archives(&self) -> i32 {
        self.num_archives
    }

    /// The auto-loading database ids, in id order.
    pub fn auto_load_ids(&self) -> Vec<DatabaseId> {
        self.auto_load.iter().copied().collect()
    }

    // ------------------------------------------------------------------
    // Startup indexing (`initEntityMap`, xml_database.cpp:181-292)
    // ------------------------------------------------------------------

    /// `initEntityMap` + `initAutoLoad`: two passes, the first reads the
    /// stored database ids and `maxID_`, the second assigns ids to the
    /// entities that have none.
    fn init(&mut self) {
        let mut has_unassigned = false;
        let mut should_assign = false;
        for _ in 0..2 {
            for index in 0..self.section.count_children() {
                let Some(child) = self.section.open_child(index) else {
                    continue;
                };
                let type_name = child.section_name().to_string();
                let stored_id = child
                    .find_child(DATABASE_ID_TAG)
                    .map_or(0, |id| id.as_long(0));

                let Some(type_id) = self.defs.name_to_index(&type_name) else {
                    // Not our info section and strays are ignored with a
                    // warning in the first loop (`xml_database.cpp:191-206`).
                    continue;
                };

                let id = if stored_id == 0 {
                    if should_assign {
                        self.max_id += 1;
                        if let Some(child) = self.section.child_mut(index) {
                            child.add_child(DATABASE_ID_TAG).set_int64(self.max_id);
                        }
                        self.max_id
                    } else {
                        has_unassigned = true;
                        continue;
                    }
                } else {
                    if should_assign {
                        // Loaded in the first pass already
                        // (`xml_database.cpp:228-230`).
                        continue;
                    }
                    if stored_id > self.max_id {
                        self.max_id = stored_id;
                    }
                    stored_id
                };

                // A duplicate db id is ignored — bots' negative ids quietly,
                // the rest with a warning (`xml_database.cpp:239-255`).
                if self.id_to_data.contains_key(&id) {
                    continue;
                }
                self.id_to_data.insert(id, type_name.clone());

                let named = match (
                    self.defs.entity_description(type_id),
                    self.section.open_child(index),
                ) {
                    (Some(desc), Some(section)) => registered_name(desc, section),
                    _ => None,
                };
                if let Some(name) = named {
                    let map = self.name_to_id.entry(type_id).or_default();
                    // A second entity with the same name stays in the file
                    // but is not retrievable by name
                    // (`xml_database.cpp:271-287`).
                    if map.contains_key(&name) {
                        eprintln!(
                            "XMLDatabase::initEntityMap: Multiple entities of type '{type_name}' have the same name - second entity will not be retrievable by name"
                        );
                    } else {
                        map.insert(name, id);
                    }
                }
            }

            if has_unassigned && !should_assign {
                should_assign = true;
            } else {
                break;
            }
        }

        self.init_auto_load();
    }

    // ------------------------------------------------------------------
    // Lookups (`findEntityByName`, xml_database.cpp:846-855)
    // ------------------------------------------------------------------

    /// `findEntityByName`: 0 when nothing carries the name.
    pub fn find_entity_by_name(&self, type_id: EntityTypeId, name: &str) -> DatabaseId {
        self.name_to_id
            .get(&type_id)
            .and_then(|map| map.get(name))
            .copied()
            .unwrap_or(0)
    }

    /// `getDatabaseIDFromName` (`xml_database.cpp:541-556`): the id of the
    /// row of `type_id` whose identifier is `name`, `None` when unknown.
    pub fn get_database_id_from_name(
        &self,
        type_id: EntityTypeId,
        name: &str,
    ) -> Option<DatabaseId> {
        match self.find_entity_by_name(type_id, name) {
            0 => None,
            id => Some(id),
        }
    }

    // ------------------------------------------------------------------
    // getEntity (xml_database.cpp:441-535)
    // ------------------------------------------------------------------

    /// `getEntity`: stream the persistent properties of `db_id` (or of the
    /// row named `name` when `db_id` is 0) into `stream`, appending the cell
    /// payload for a cell type. A pending id has no meaning here — the
    /// reference never stores one.
    pub fn get_entity_stream(
        &self,
        type_id: EntityTypeId,
        db_id: DatabaseId,
        name: Option<&str>,
        stream: &mut BinaryStream,
    ) -> GetOutcome {
        let Some(desc) = self.defs.entity_description(type_id) else {
            return GetOutcome::missing(db_id);
        };

        let mut key_db_id = if db_id == PENDING_DATABASE_ID {
            0
        } else {
            db_id
        };
        let mut key_name = name.map(str::to_string);
        let lookup_by_name = key_db_id == 0;
        if lookup_by_name {
            key_db_id = self.find_entity_by_name(type_id, key_name.as_deref().unwrap_or(""));
        }
        let is_ok = key_db_id != 0;
        let mut pose = None;

        if is_ok {
            let index = match self.entity_child_index(desc.name.as_str(), key_db_id) {
                Some(index) => index,
                None => return GetOutcome::missing(key_db_id),
            };
            let section = self.section.open_child(index).expect("index from children");

            if section.section_name() != desc.name {
                // "Invalid entity type" (`xml_database.cpp:479-486`).
                return GetOutcome::missing(key_db_id);
            }

            if !lookup_by_name {
                key_name = registered_name(desc, section);
            }

            let mut failed = false;
            if !desc.visit(DATABASE_DATA_DOMAINS, &mut |property| {
                let value = match section.find_child(&property_section_name(property)) {
                    Some(child) => match property.data_type.value_from_section(child) {
                        Ok(value) => value,
                        Err(_) => return false,
                    },
                    None => match property.data_type.initial_value() {
                        Ok(value) => value,
                        Err(_) => return false,
                    },
                };
                if let Err(_) = property.data_type.add_persistent_to_stream(&value, stream) {
                    return false;
                }
                true
            }) {
                failed = true;
            }
            if failed {
                return GetOutcome::missing(key_db_id);
            }

            if desc.can_be_on_cell() {
                pose = Some(CellPose {
                    position: section.read_vector3("position", Vector3::ZERO),
                    direction: vector_to_direction(
                        section.read_vector3("direction", Vector3::ZERO),
                    ),
                    space_id: section
                        .find_child("spaceID")
                        .map_or(0, |space| space.as_int(0)),
                });
                if let Some(pose) = &pose {
                    pose.write_to(stream);
                }
            }
        }

        GetOutcome {
            db_id: key_db_id,
            name: key_name,
            found: is_ok,
            pose,
        }
    }

    // ------------------------------------------------------------------
    // putEntity (xml_database.cpp:561-752)
    // ------------------------------------------------------------------

    /// `putEntity` over the wire shape: `stream` carries the properties in
    /// the [`DATABASE_DATA_DOMAINS`] visit order, then the cell payload for a
    /// cell type, then the game time (`xml_database.cpp:616-621`, read and
    /// dropped — the XML database does not remember it).
    pub fn put_entity_stream(
        &mut self,
        type_id: EntityTypeId,
        db_id: DatabaseId,
        stream: &mut BinaryStream,
        auto_load: AutoLoadUpdate,
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        let Some(desc) = self.defs.entity_description(type_id) else {
            return Err(DbError::Storage(format!("unknown entity type {type_id}")));
        };

        let is_existing = db_id != 0
            && db_id != PENDING_DATABASE_ID
            && self.entity_child_index(desc.name.as_str(), db_id).is_some();

        if put_explicit_id && is_existing {
            // `insertExplicit` onto a taken row
            // (`put_entity_task.cpp:105-110`).
            return Err(DbError::Storage("entity id already in use".into()));
        }

        let mut target = db_id;
        if !is_existing {
            if db_id == 0 || db_id == PENDING_DATABASE_ID {
                // A create: the next id past `maxID_`
                // (`xml_database.cpp:601-603`).
                self.max_id += 1;
                target = self.max_id;
            } else if put_explicit_id {
                // `insertExplicit`: the row's own id, which must be free
                // (`put_entity_task.cpp:105-110`).
                if self.entity_child_index_by_id(db_id).is_some() {
                    return Err(DbError::Storage("entity id already in use".into()));
                }
                if db_id > self.max_id {
                    self.max_id = db_id;
                }
            } else {
                // An update that names no row (`xml_database.cpp:592-597`).
                return Err(DbError::NotFound);
            }
        }

        let old_index = if is_existing {
            Some(
                self.entity_child_index(desc.name.as_str(), target)
                    .expect("existence checked above"),
            )
        } else {
            None
        };
        let old_name = old_index.and_then(|index| {
            registered_name(
                desc,
                self.section.open_child(index).expect("index from children"),
            )
        });

        // The new section is built in place and dropped again on failure —
        // `newSection` + `delChild(pProps)` (`xml_database.cpp:605-703`).
        let new_index = self.section.count_children();
        self.section.add_child(desc.name.as_str());
        let mut failed = false;
        {
            let props = self.section.child_mut(new_index).expect("just appended");
            if !desc.visit(DATABASE_DATA_DOMAINS, &mut |property| {
                let value = match property.data_type.create_persistent_from_stream(stream) {
                    Ok(value) => value,
                    Err(_) => return false,
                };
                let child = props.add_child(&property_section_name(property));
                if let Err(_) = property.data_type.value_to_section(&value, child) {
                    return false;
                }
                true
            }) {
                failed = true;
            }
            if !failed && desc.can_be_on_cell() {
                match CellPose::read_from(stream) {
                    Ok(pose) => {
                        let props = self.section.child_mut(new_index).expect("just appended");
                        props.add_child("position").set_vector3(pose.position);
                        props.add_child("direction").set_vector3(Vector3::new(
                            pose.direction[0],
                            pose.direction[1],
                            pose.direction[2],
                        ));
                        props.add_child("spaceID").set_int(pose.space_id);
                    }
                    Err(_) => failed = true,
                }
            }
            if !failed {
                // The game time is MySQL-only; the XML database reads and
                // drops it (`xml_database.cpp:616-621`).
                let _ = stream.read_i64();
            }
        }

        if failed {
            self.section.remove_child_at(new_index);
            return Err(DbError::Storage("malformed entity stream".into()));
        }

        // The name check (`xml_database.cpp:631-660`).
        let new_name = self
            .section
            .open_child(new_index)
            .and_then(|props| registered_name(desc, props));
        if let Some(new_name) = &new_name {
            let taker = self.find_entity_by_name(type_id, new_name);
            if old_name.as_ref() != Some(new_name) && taker != 0 && taker != target {
                eprintln!(
                    "XMLDatabase::putEntity: '{}' entity named '{}' already exists",
                    desc.name, new_name
                );
                self.section.remove_child_at(new_index);
                return Err(DbError::Storage(format!(
                    "entity named '{new_name}' already exists"
                )));
            }
        }

        // Commit: stamp the id, drop the old section, index the new one
        // (`xml_database.cpp:662-672`).
        if let Some(props) = self.section.child_mut(new_index) {
            props.add_child(DATABASE_ID_TAG).set_int64(target);
        }
        if let Some(old_index) = old_index {
            self.section.remove_child_at(old_index);
        }
        self.id_to_data.insert(target, desc.name.clone());
        if let Some(new_name) = new_name {
            let map = self.name_to_id.entry(type_id).or_default();
            if let Some(old_name) = old_name {
                if old_name != new_name {
                    map.remove(&old_name);
                }
            }
            map.insert(new_name, target);
        }

        match auto_load {
            AutoLoadUpdate::Retain => {}
            AutoLoadUpdate::True => self.add_to_auto_load(target),
            AutoLoadUpdate::False => self.remove_from_auto_load(target),
        }

        Ok(target)
    }

    // ------------------------------------------------------------------
    // delEntity (xml_database.cpp:758-840)
    // ------------------------------------------------------------------

    /// `delEntity`: resolve a 0/pending id by `name`, then remove the row.
    pub fn del_entity(
        &mut self,
        type_id: EntityTypeId,
        db_id: DatabaseId,
        name: Option<&str>,
    ) -> Result<(), DbError> {
        let mut target = if db_id == PENDING_DATABASE_ID {
            0
        } else {
            db_id
        };
        if target == 0 {
            // "look up the id if we don't already know it"
            // (`xml_database.cpp:793-796`).
            target = self.find_entity_by_name(type_id, name.unwrap_or(""));
            if target == 0 {
                return Err(DbError::NotFound);
            }
        }
        let type_name = match self.id_to_data.get(&target) {
            Some(name) => name.clone(),
            None => return Err(DbError::NotFound),
        };
        if !self.delete_entity(&type_name, type_id, target) {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    /// `deleteEntity` (`xml_database.cpp:806-841`).
    fn delete_entity(&mut self, type_name: &str, type_id: EntityTypeId, id: DatabaseId) -> bool {
        let Some(index) = self.entity_child_index(type_name, id) else {
            return false;
        };
        let desc = match self.defs.entity_description(type_id) {
            Some(desc) => desc,
            None => return false,
        };
        let name = self
            .section
            .open_child(index)
            .and_then(|section| registered_name(desc, section));
        if let Some(name) = name {
            if let Some(map) = self.name_to_id.get_mut(&type_id) {
                map.remove(&name);
            }
        }
        self.id_to_data.remove(&id);
        self.section.remove_child_at(index);
        self.remove_from_auto_load(id);
        true
    }

    // ------------------------------------------------------------------
    // IDs (xml_database.cpp:889-928)
    // ------------------------------------------------------------------

    /// `putIDs`: hand spare ids back for reuse.
    pub fn put_ids(&mut self, ids: &[EntityId]) {
        self.spare_ids.extend_from_slice(ids);
    }

    /// `getIDs`: spare ids first, then freshly generated ones wrapping at
    /// [`FIRST_LOCAL_ENTITY_ID`].
    pub fn get_ids(&mut self, count: usize) -> Vec<EntityId> {
        let mut ids = Vec::with_capacity(count);
        while ids.len() < count {
            match self.spare_ids.pop() {
                Some(id) => ids.push(id),
                None => {
                    ids.push(self.next_id);
                    self.next_id += 1;
                    // "It should be safe to just wrap the ID around hoping
                    // nobody uses FIRST_ENTITY_ID after ~2.1 bn generated
                    // IDs" (`xml_database.cpp:922-926`).
                    if self.next_id >= FIRST_LOCAL_ENTITY_ID {
                        self.next_id = FIRST_ENTITY_ID;
                    }
                }
            }
        }
        ids
    }

    // ------------------------------------------------------------------
    // Space data (xml_database.cpp:935-1053)
    // ------------------------------------------------------------------

    /// `writeSpaceData`: archive the stream's spaces under the info
    /// section, replacing whatever was archived before.
    pub fn write_space_data(&mut self, stream: &mut BinaryStream) -> Result<(), DbError> {
        let (info_index, child_index) =
            self.find_or_create_info_child_index(DATABASE_AUTO_LOAD_SPACE_DATA_SECTION);
        let archive = self
            .section
            .child_mut(info_index)
            .and_then(|info| info.child_mut(child_index))
            .expect("info child just ensured");
        archive.clear_children();

        let num_spaces = stream.read_u32().map_err(stream_error)?;
        for _ in 0..num_spaces {
            let space_id = stream.read_i32().map_err(stream_error)?;
            let space = archive.add_child("space");
            space.add_child("id").set_uint(space_id as u32);

            let items = space.add_child("dataItems");
            let num_items = stream.read_u32().map_err(stream_error)?;
            for _ in 0..num_items {
                let space_key = stream.read_u64().map_err(stream_error)?;
                let data_key = stream.read_u16().map_err(stream_error)?;
                let data = stream.read_blob().map_err(stream_error)?;

                let item = items.add_child("item");
                item.add_child("spaceKey").set_uint64(space_key);
                item.add_child("dataKey").set_uint(data_key as u32);
                item.add_child("data")
                    .set_string(&resources::base64::encode(&data));
            }
        }
        Ok(())
    }

    /// `getSpacesData` (`xml_database.cpp:986-1053`): the spaces of the
    /// auto-loading entities, then each space's archived items (an empty
    /// item list when nothing was archived).
    pub fn get_spaces_data(&mut self, stream: &mut BinaryStream) -> Result<(), DbError> {
        let space_ids = self.auto_load_space_ids();
        stream.write_i32(space_ids.len() as i32);

        let (info_index, child_index) =
            self.find_or_create_info_child_index(DATABASE_AUTO_LOAD_SPACE_DATA_SECTION);
        let archive = self
            .section
            .child_mut(info_index)
            .and_then(|info| info.child_mut(child_index))
            .expect("info child just ensured");

        for space_id in &space_ids {
            stream.write_i32(*space_id);
            let items = archive
                .find_child("space")
                .filter(|space| {
                    space
                        .find_child("id")
                        .map_or(false, |id| id.as_int(0) == *space_id)
                })
                .and_then(|space| space.find_child("dataItems"));
            match items {
                Some(items) => {
                    stream.write_i32(items.count_children() as i32);
                    for index in 0..items.count_children() {
                        let item = items.open_child(index).expect("child count");
                        stream.write_u64(
                            item.find_child("spaceKey")
                                .map_or(0, |key| key.as_uint64(0)),
                        );
                        stream.write_u16(
                            item.find_child("dataKey").map_or(0, |key| key.as_uint(0)) as u16,
                        );
                        let data = item
                            .find_child("data")
                            .map_or(Vec::new(), |data| data.as_blob());
                        let _ = stream.write_blob(&data);
                    }
                }
                // Nothing was archived for this space (`xml_database.cpp:1046-1050`).
                None => {
                    stream.write_i32(0);
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Auto-load (xml_database.cpp:1062-1098, 1232-1298)
    // ------------------------------------------------------------------

    /// `initAutoLoad` (`xml_database.cpp:1232-1257`): read the
    /// `<databaseID>` children of the info section's auto-load list.
    fn init_auto_load(&mut self) {
        self.auto_load.clear();
        let (info_index, child_index) =
            self.find_or_create_info_child_index(DATABASE_AUTO_LOAD_ENTITIES_SECTION);
        let count = self
            .section
            .child_mut(info_index)
            .and_then(|info| info.child_mut(child_index))
            .expect("info child just ensured")
            .count_children();
        for index in 0..count {
            let id = self
                .section
                .child_mut(info_index)
                .and_then(|info| info.child_mut(child_index))
                .and_then(|list| list.open_child(index))
                .filter(|child| child.section_name() == DATABASE_ID_TAG)
                .map_or(0, |child| child.as_long(0));
            if id <= 0 {
                // Invalid entries are reported and skipped
                // (`xml_database.cpp:1246-1253`).
                if id != 0 {
                    eprintln!(
                        "XMLDatabase::initAutoLoadSet: read invalid auto-load entity database ID in element {index}"
                    );
                }
            } else {
                self.auto_load.insert(id);
            }
        }
    }

    /// `addToAutoLoad` (`xml_database.cpp:1263-1277`).
    pub fn add_to_auto_load(&mut self, database_id: DatabaseId) {
        if self.auto_load.contains(&database_id) {
            return;
        }
        let (info_index, child_index) =
            self.find_or_create_info_child_index(DATABASE_AUTO_LOAD_ENTITIES_SECTION);
        self.section
            .child_mut(info_index)
            .and_then(|info| info.child_mut(child_index))
            .expect("info child just ensured")
            .add_child(DATABASE_ID_TAG)
            .set_int64(database_id);
        self.auto_load.insert(database_id);
    }

    /// `removeFromAutoLoad` (`xml_database.cpp:1283-1298`).
    pub fn remove_from_auto_load(&mut self, database_id: DatabaseId) {
        if !self.auto_load.remove(&database_id) {
            return;
        }
        let (info_index, child_index) =
            self.find_or_create_info_child_index(DATABASE_AUTO_LOAD_ENTITIES_SECTION);
        let list = self
            .section
            .child_mut(info_index)
            .and_then(|info| info.child_mut(child_index))
            .expect("info child just ensured");
        for index in 0..list.count_children() {
            let matches = list
                .open_child(index)
                .filter(|child| child.section_name() == DATABASE_ID_TAG)
                .map_or(false, |child| child.as_long(0) == database_id);
            if matches {
                list.remove_child_at(index);
                return;
            }
        }
    }

    /// `autoLoadEntities`' resolution half (`xml_database.cpp:1062-1098`):
    /// every auto-loading id as `(type, db id)`, stale ids dropped from the
    /// set and unknown type names reported.
    pub fn auto_load_entities(&mut self) -> Vec<(EntityTypeId, DatabaseId)> {
        let mut loaded = Vec::with_capacity(self.auto_load.len());
        let mut stale = Vec::new();
        for &database_id in &self.auto_load {
            let index = self.entity_child_index_by_id(database_id);
            let type_name = index
                .and_then(|index| self.section.open_child(index))
                .map(|section| section.section_name().to_string());
            let type_id = type_name
                .as_ref()
                .and_then(|name| self.defs.name_to_index(name));
            match type_id {
                Some(type_id) => loaded.push((type_id, database_id)),
                None => {
                    if index.is_none() {
                        eprintln!(
                            "XMLDatabase::autoLoadEntities: removing database ID for non-existent entity from auto-loading entities set: {database_id}"
                        );
                    } else {
                        eprintln!(
                            "XMLDatabase::autoLoadEntities: could not find entity type for auto-loading entity with database ID = {database_id}"
                        );
                    }
                    stale.push(database_id);
                }
            }
        }
        for database_id in stale {
            self.remove_from_auto_load(database_id);
        }
        loaded
    }

    /// `getAutoLoadSpacesFromEntities` (`xml_database.cpp:1104-1127`): the
    /// distinct space ids of the auto-loading entities, in id order.
    pub fn auto_load_space_ids(&mut self) -> BTreeSet<i32> {
        let mut spaces = BTreeSet::new();
        for database_id in &self.auto_load {
            let space_id = self
                .entity_child_index_by_id(*database_id)
                .and_then(|index| self.section.open_child(index))
                .map_or(0, |section| {
                    section
                        .find_child("spaceID")
                        .map_or(0, |space| space.as_int(0))
                });
            if space_id != 0 {
                spaces.insert(space_id);
            }
        }
        spaces
    }

    // ------------------------------------------------------------------
    // Saving (xml_database.cpp:324-398)
    // ------------------------------------------------------------------

    /// `save` (`xml_database.cpp:362-374`): the current file becomes
    /// `.bak`, then the section is written out.
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.file_path.clone() else {
            return Ok(());
        };
        if Path::new(&path).exists() {
            std::fs::rename(&path, format!("{path}.bak"))?;
        }
        self.timed_save()
    }

    /// `archive` (`xml_database.cpp:324-354`): rotate `.bak`/`.1`/… up to
    /// [`XmlDatabase::num_archives`], the live file becoming `.1`, then
    /// write the section out.
    pub fn archive(&mut self) -> std::io::Result<()> {
        let Some(path) = self.file_path.clone() else {
            return Ok(());
        };
        for i in (1..=self.num_archives).rev() {
            let new_name = format!("{path}.{i}");
            let old_name = if i == 1 {
                path.clone()
            } else {
                format!("{path}.{}", i - 1)
            };
            if Path::new(&old_name).exists() {
                std::fs::rename(&old_name, &new_name)?;
            }
        }
        self.timed_save()
    }

    /// `timedSave` (`xml_database.cpp:383-398`), without the timing warn —
    /// the caller measures its own latency.
    fn timed_save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.file_path.clone() else {
            return Ok(());
        };
        std::fs::write(path, self.to_xml())
    }

    // ------------------------------------------------------------------
    // Section plumbing
    // ------------------------------------------------------------------

    /// The child of the database root holding the entity row `(type_name,
    /// db_id)`, as an index (`idToData_` lookup, re-found each time).
    fn entity_child_index(&self, type_name: &str, db_id: DatabaseId) -> Option<usize> {
        (0..self.section.count_children()).find(|&index| {
            self.section.open_child(index).is_some_and(|child| {
                child.section_name() == type_name
                    && child
                        .find_child(DATABASE_ID_TAG)
                        .is_some_and(|id| id.as_long(0) == db_id)
            })
        })
    }

    /// The row carrying `db_id`, whatever its type.
    fn entity_child_index_by_id(&self, db_id: DatabaseId) -> Option<usize> {
        (0..self.section.count_children()).find(|&index| {
            self.section
                .open_child(index)
                .and_then(|child| child.find_child(DATABASE_ID_TAG))
                .is_some_and(|id| id.as_long(0) == db_id)
        })
    }

    /// `findOrCreateInfoSection` (`xml_database.cpp:1208-1226`) as indexes:
    /// the `<_BigWorldInfo>` child of the root, then its child `name`, both
    /// created when missing.
    fn find_or_create_info_child_index(&mut self, name: &str) -> (usize, usize) {
        let info_index = match Self::child_index(&self.section, DATABASE_INFO_SECTION) {
            Some(index) => index,
            None => {
                self.section.add_child(DATABASE_INFO_SECTION);
                self.section.count_children() - 1
            }
        };
        let child_index = match Self::child_index(
            self.section.child_mut(info_index).expect("info index"),
            name,
        ) {
            Some(index) => index,
            None => {
                let info = self.section.child_mut(info_index).expect("info index");
                info.add_child(name);
                info.count_children() - 1
            }
        };
        (info_index, child_index)
    }

    /// The index of the first child named `tag`.
    fn child_index(section: &DataSection, tag: &str) -> Option<usize> {
        (0..section.count_children()).find(|&index| {
            section
                .open_child(index)
                .is_some_and(|c| c.section_name() == tag)
        })
    }
}

/// The identifier value of an entity section (`xml_database.cpp:268-271`
/// and `:489-495` read it by the property's plain name).
fn registered_name(desc: &EntityDescription, section: &DataSection) -> Option<String> {
    desc.identifier().map(|property| {
        section
            .find_child(property.name())
            .map_or_else(String::new, |child| child.as_string().to_string())
    })
}

/// The section tag a property lives under: the component-qualified
/// `fullName()` (`data_description.ipp:325-337`), which for the common
/// non-componentised property is the bare name.
fn property_section_name(property: &entitydef::data_description::DataDescription) -> String {
    if property.component_name.is_empty() {
        property.name().to_string()
    } else {
        format!("{}.{}", property.component_name, property.name())
    }
}

/// A `Direction3D` read out of a section (stored as the plain vector
/// `xml_database.cpp:508` reads).
fn vector_to_direction(vector: Vector3) -> [f32; 3] {
    [vector.x, vector.y, vector.z]
}

fn stream_error(error: StreamError) -> DbError {
    DbError::Storage(format!("stream: {error}"))
}

// ----------------------------------------------------------------------
// The storage contract
// ----------------------------------------------------------------------

/// The XML database behind the BW-09-01 storage contract. Online state has
/// no file representation — `set_log_on_record` is refused, `log_on_record`
/// always answers offline, matching a database whose `activeSet_` is never
/// populated.
impl EntityStorage for XmlDatabase {
    fn get_entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, DbError> {
        let mut stream = BinaryStream::new();
        let outcome = self.get_entity_stream(key.type_id, key.db_id, None, &mut stream);
        if !outcome.found {
            return Ok(None);
        }

        let desc = self
            .defs
            .entity_description(key.type_id)
            .ok_or(DbError::NotFound)?;
        stream.rewind();
        let mut values = Vec::new();
        let mut failed = false;
        if !desc.visit(DATABASE_DATA_DOMAINS, &mut |property| match property
            .data_type
            .create_persistent_from_stream(&mut stream)
        {
            Ok(value) => {
                values.push(value);
                true
            }
            Err(_) => {
                failed = true;
                false
            }
        }) {
            failed = true;
        }
        if failed {
            return Err(DbError::Storage("malformed entity stream".into()));
        }
        Ok(Some(EntityRecord {
            type_id: key.type_id,
            db_id: outcome.db_id,
            name: outcome.name,
            values,
        }))
    }

    fn put_entity(
        &mut self,
        record: &EntityRecord,
        put_explicit_id: bool,
    ) -> Result<DatabaseId, DbError> {
        let desc = self
            .defs
            .entity_description(record.type_id)
            .ok_or(DbError::NotFound)?;

        let mut stream = BinaryStream::new();
        let mut values = record.values.iter();
        let mut failed = false;
        if !desc.visit(DATABASE_DATA_DOMAINS, &mut |property| {
            let value = values
                .next()
                .cloned()
                .or_else(|| property.data_type.initial_value().ok());
            match value {
                Some(value) => property
                    .data_type
                    .add_persistent_to_stream(&value, &mut stream)
                    .is_ok(),
                // Neither a supplied value nor a type default: the record
                // cannot be written.
                None => false,
            }
        }) {
            failed = true;
        }
        if failed {
            return Err(DbError::Storage("malformed property values".into()));
        }
        // A record carries no cell payload; an update keeps the row's stored
        // pose instead of clobbering it.
        let mut pose = CellPose::default();
        if desc.can_be_on_cell() {
            if let Some(index) = self.entity_child_index(desc.name.as_str(), record.db_id) {
                if let Some(section) = self.section.open_child(index) {
                    pose = CellPose {
                        position: section.read_vector3("position", Vector3::ZERO),
                        direction: vector_to_direction(
                            section.read_vector3("direction", Vector3::ZERO),
                        ),
                        space_id: section
                            .find_child("spaceID")
                            .map_or(0, |space| space.as_int(0)),
                    };
                }
            }
            pose.write_to(&mut stream);
        }
        // The game time slot the reference's put stream carries.
        stream.write_i64(0);

        self.put_entity_stream(
            record.type_id,
            record.db_id,
            &mut stream,
            AutoLoadUpdate::Retain,
            put_explicit_id,
        )
    }

    fn del_entity(&mut self, key: &EntityKey) -> Result<(), DbError> {
        // The contract's delete is by id; a pending id is never a real key
        // (`del_entity_task.cpp:35`).
        if key.db_id == PENDING_DATABASE_ID || key.db_id == 0 {
            return Err(DbError::NotFound);
        }
        self.del_entity(key.type_id, key.db_id, None)
    }

    fn lookup_entity_by_name(
        &self,
        type_id: EntityTypeId,
        name: &str,
    ) -> Result<Option<EntityRecord>, DbError> {
        match self.get_database_id_from_name(type_id, name) {
            Some(db_id) => self.get_entity(&EntityKey { type_id, db_id }),
            None => Ok(None),
        }
    }

    fn lookup_entities(&self, type_id: EntityTypeId) -> Result<Vec<EntityRecord>, DbError> {
        let desc = self
            .defs
            .entity_description(type_id)
            .ok_or(DbError::NotFound)?;
        let mut records = Vec::new();
        for index in 0..self.section.count_children() {
            let Some(child) = self.section.open_child(index) else {
                continue;
            };
            if child.section_name() != desc.name {
                continue;
            }
            let db_id = child
                .find_child(DATABASE_ID_TAG)
                .map_or(0, |id| id.as_long(0));
            if db_id == 0 {
                continue;
            }
            if let Some(record) = self.get_entity(&EntityKey { type_id, db_id })? {
                records.push(record);
            }
        }
        records.sort_by_key(|record| record.db_id);
        Ok(records)
    }

    fn set_log_on_record(&mut self, _key: &EntityKey, _record: LogOnRecord) -> Result<(), DbError> {
        Err(DbError::Storage(
            "the xml database tracks no online state".into(),
        ))
    }

    fn remove_log_on_record(&mut self, _key: &EntityKey) -> Result<(), DbError> {
        Ok(())
    }

    fn log_on_record(&self, _key: &EntityKey) -> Result<Option<LogOnRecord>, DbError> {
        Ok(None)
    }
}

// ----------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use entitydef::INVALID_ENTITY_TYPE_ID;
    use entitydef::data_description::DataDescription;
    use entitydef::data_type::TypeContext;
    use entitydef::diagnostics::Diagnostics;
    use entitydef::entity_description::DefResolver;
    use entitydef::value::PropertyValue;
    use std::collections::HashMap;

    const RES: &str = "scripts/entities.xml";
    const DEFS: &str = "scripts/entity_defs";

    /// A persistent, base-data property block for an entity def: `<Flags>
    /// BASE </Flags>` makes it base data, `<Persistent>` makes it part of the
    /// database projection, `<Identifier>` makes it the name column.
    const PERSISTENT_FLAGS: &str = " <Flags> BASE </Flags> <Persistent> true </Persistent> ";

    /// A resolver serving an in-memory res tree.
    struct MemoryResolver {
        files: HashMap<String, Vec<u8>>,
    }

    impl MemoryResolver {
        fn new(files: &[(&str, &str)]) -> Self {
            MemoryResolver {
                files: files
                    .iter()
                    .map(|(path, xml)| (path.to_string(), xml.as_bytes().to_vec()))
                    .collect(),
            }
        }
    }

    impl DefResolver for MemoryResolver {
        fn open_section(&self, path: &str) -> Option<DataSection> {
            resources::datasection::parse("", self.files.get(path)?).ok()
        }

        fn python_script_exists(&self, path: &str) -> bool {
            self.files.contains_key(path) || self.files.contains_key(&format!("{path}/__init__.py"))
        }
    }

    /// A defs set with one base-only Avatar (identifier `name`, a persistent
    /// int) and one cell Avatar variant is impossible — one name, one type.
    /// Here: Avatar is base-only, Guard lives on a cell (the cell script
    /// exists) and carries an identifier too.
    fn defs() -> EntityDescriptionMap {
        let avatar_props = &format!(
            "<Properties> \
                 <name> <Type> STRING </Type>{PERSISTENT_FLAGS} <Identifier> true </Identifier> </name> \
                 <hp> <Type> INT32 </Type>{PERSISTENT_FLAGS} </hp> \
             </Properties>"
        );
        let guard_props = &format!(
            "<Properties> \
                 <name> <Type> STRING </Type>{PERSISTENT_FLAGS} <Identifier> true </Identifier> </name> \
             </Properties>"
        );
        let resolver = MemoryResolver::new(&[
            (
                RES,
                "<root> \
                     <ClientServerEntities> <Avatar/> <Guard/> </ClientServerEntities> \
                     <ServerOnlyEntities/> \
                 </root>",
            ),
            (
                &format!("{DEFS}/Avatar.def"),
                format!("<root> {avatar_props} </root>").as_str(),
            ),
            (
                &format!("{DEFS}/Guard.def"),
                format!("<root> {guard_props} </root>").as_str(),
            ),
            ("scripts/base/Avatar", "pass"),
            ("scripts/cell/Guard", "pass"),
        ]);

        let section = resolver.open_section(RES).expect("entities.xml");
        let mut map = EntityDescriptionMap::default();
        assert!(
            map.parse(
                Some(&section),
                &resolver,
                &TypeContext::default(),
                &mut Diagnostics::new(),
                &entitydef::entity_map::MessageRanges::default(),
            ),
            "defs parse failed"
        );
        map
    }

    /// An Avatar record stream: the persistent properties in visit order,
    /// then the game time (a base-only type carries no cell payload).
    fn avatar_stream(hp: i64, game_time: i64) -> BinaryStream {
        let mut stream = BinaryStream::new();
        let _ = stream.write_string(&format!("avatar-{hp}")); // name (STRING)
        stream.write_i32(hp as i32); // hp (INT32)
        stream.write_i64(game_time);
        stream
    }

    #[test]
    fn an_empty_database_round_trips_through_xml() {
        let mut database = XmlDatabase::new(defs());
        let xml = database.to_xml();
        let reopened = XmlDatabase::from_xml(&xml, defs()).expect("parse own xml");
        assert_eq!(reopened.max_id(), 0);
        assert!(reopened.auto_load_ids().is_empty());
    }

    #[test]
    fn init_indexes_stored_entities_and_assigns_the_missing_ids() {
        // One entity with a stored id, one without — the second pass
        // assigns past maxID_ (`xml_database.cpp:181-215`).
        let xml = "<root> \
                       <Avatar> <name> moe </name> <hp> 10 </hp> <databaseID> 5 </databaseID> </Avatar> \
                       <Avatar> <name> curly </name> <hp> 20 </hp> </Avatar> \
                   </root>";
        let mut database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        assert_eq!(database.max_id(), 6);

        let avatar = database
            .defs
            .name_to_index("Avatar")
            .expect("Avatar indexed");
        assert_eq!(database.get_database_id_from_name(avatar, "moe"), Some(5));
        assert_eq!(database.get_database_id_from_name(avatar, "curly"), Some(6));
        // An id with no type of its own is not a name lookup
        // (`xml_database.cpp:846-855`).
        assert_eq!(
            database.get_database_id_from_name(INVALID_ENTITY_TYPE_ID, "moe"),
            None
        );
        // The assigned id is stamped back into the section
        // (`xml_database.cpp:220-222`).
        assert!(database.to_xml().contains("<databaseID>\t6\t</databaseID>"));
    }

    #[test]
    fn a_stray_section_is_ignored_and_not_a_type() {
        let xml = "<root> \
                       <Avatar> <name> moe </name> <databaseID> 1 </databaseID> </Avatar> \
                       <_BigWorldInfo> <AutoLoadedEntities/> </_BigWorldInfo> \
                       <Monster> <name> stray </name> <databaseID> 2 </databaseID> </Monster> \
                   </root>";
        let database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        assert_eq!(database.get_database_id_from_name(avatar, "moe"), Some(1));
        assert_eq!(database.max_id(), 1, "the stray's id never feeds maxID_");
    }

    #[test]
    fn get_entity_streams_properties_and_the_cell_payload() {
        let xml = "<root> \
                       <Avatar> <name> moe </name> <hp> 10 </hp> <databaseID> 1 </databaseID> </Avatar> \
                       <Guard> <name> g </name> \
                           <position> 1.000000 2.000000 3.000000 </position> \
                           <direction> 0.000000 0.000000 1.500000 </direction> \
                           <spaceID> 7 </spaceID> \
                           <databaseID> 2 </databaseID> \
                       </Guard> \
                   </root>";
        let database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        let (avatar, guard) = (
            database.defs.name_to_index("Avatar").unwrap(),
            database.defs.name_to_index("Guard").unwrap(),
        );

        // By id: the identifier is read back (`xml_database.cpp:489-495`).
        let mut stream = BinaryStream::new();
        let outcome = database.get_entity_stream(avatar, 1, None, &mut stream);
        assert!(outcome.found);
        assert_eq!(outcome.db_id, 1);
        assert_eq!(outcome.name.as_deref(), Some("moe"));
        assert!(outcome.pose.is_none(), "Avatar is base-only");
        stream.rewind();
        assert_eq!(stream.read_string().unwrap(), "moe");
        assert_eq!(stream.read_i32().unwrap(), 10);

        // By name (`xml_database.cpp:454-457`).
        let mut stream = BinaryStream::new();
        let outcome = database.get_entity_stream(avatar, 0, Some("moe"), &mut stream);
        assert!(outcome.found);
        assert_eq!(outcome.db_id, 1);

        // A cell type appends position, direction, spaceID
        // (`xml_database.cpp:505-510`).
        let mut stream = BinaryStream::new();
        let outcome = database.get_entity_stream(guard, 2, None, &mut stream);
        assert_eq!(
            outcome.pose,
            Some(CellPose {
                position: Vector3::new(1.0, 2.0, 3.0),
                direction: [0.0, 0.0, 1.5],
                space_id: 7,
            })
        );
        stream.rewind();
        assert_eq!(stream.read_string().unwrap(), "g");
        let pose = CellPose::read_from(&mut stream).expect("cell payload");
        assert_eq!(pose.space_id, 7);
    }

    #[test]
    fn a_section_of_the_wrong_type_fails_to_load() {
        // The file says Avatar, the caller says Guard — the reference
        // refuses with "Invalid entity type" (`xml_database.cpp:479-486`).
        let xml =
            "<root> <Avatar> <name> moe </name> <databaseID> 1 </databaseID> </Avatar> </root>";
        let database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        let guard = database.defs.name_to_index("Guard").unwrap();
        let mut stream = BinaryStream::new();
        let outcome = database.get_entity_stream(guard, 1, None, &mut stream);
        assert!(!outcome.found);
        assert_eq!(outcome.name, None);
    }

    #[test]
    fn an_unknown_row_or_type_is_not_found() {
        let database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        let mut stream = BinaryStream::new();
        assert!(
            !database
                .get_entity_stream(avatar, 99, None, &mut stream)
                .found
        );
        assert!(
            !database
                .get_entity_stream(avatar, 0, Some("nobody"), &mut stream)
                .found
        );
    }

    #[test]
    fn put_entity_creates_updates_and_renames() {
        let mut database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();

        // Create (`xml_database.cpp:601-603`).
        let db_id = database
            .put_entity_stream(
                avatar,
                PENDING_DATABASE_ID,
                &mut avatar_stream(10, 0),
                AutoLoadUpdate::Retain,
                false,
            )
            .expect("create");
        assert_eq!(db_id, 1);
        assert_eq!(database.max_id(), 1);

        // Update in place: still one section (`xml_database.cpp:662-672`).
        let db_id = database
            .put_entity_stream(
                avatar,
                db_id,
                &mut avatar_stream(20, 0),
                AutoLoadUpdate::Retain,
                false,
            )
            .expect("update");
        assert_eq!(db_id, 1);
        let xml = database.to_xml();
        assert_eq!(xml.matches("<Avatar>").count(), 1, "{xml}");
        assert!(xml.contains("<hp>\t20\t</hp>"), "{xml}");

        // Rename: the old name is released (`xml_database.cpp:637-645`).
        let db_id = database
            .put_entity_stream(
                avatar,
                db_id,
                &mut avatar_stream(30, 0),
                AutoLoadUpdate::Retain,
                false,
            )
            .expect("rename");
        assert_eq!(db_id, 1);
        assert_eq!(
            database.get_database_id_from_name(avatar, "avatar-30"),
            Some(1)
        );
        assert_eq!(
            database.get_database_id_from_name(avatar, "avatar-20"),
            None
        );

        // An update naming an unknown row fails (`xml_database.cpp:592-597`).
        assert!(matches!(
            database.put_entity_stream(
                avatar,
                42,
                &mut avatar_stream(0, 0),
                AutoLoadUpdate::Retain,
                false
            ),
            Err(DbError::NotFound)
        ));
    }

    #[test]
    fn put_entity_refuses_a_taken_name_and_leaves_nothing_behind() {
        let mut database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        database
            .put_entity_stream(
                avatar,
                PENDING_DATABASE_ID,
                &mut avatar_stream(1, 0),
                AutoLoadUpdate::Retain,
                false,
            )
            .expect("first");

        // The same name again (`xml_database.cpp:645-655`).
        assert!(matches!(
            database.put_entity_stream(avatar, PENDING_DATABASE_ID, &mut avatar_stream(1, 0), AutoLoadUpdate::Retain, false),
            Err(DbError::Storage(msg)) if msg.contains("already exists")
        ));
        // The attempted section is deleted again — the file is untouched.
        let xml = database.to_xml();
        assert_eq!(xml.matches("<Avatar>").count(), 1, "{xml}");
        assert_eq!(
            database.max_id(),
            2,
            "the attempted id is spent (dbID = ++maxID_)"
        );
    }

    #[test]
    fn a_malformed_stream_fails_without_wrecking_the_file() {
        let mut database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        // The stream truncates inside the first property
        // (`xml_database.cpp:1683-1731`).
        let mut stream = BinaryStream::new();
        let _ = stream.write_string("moe");
        assert!(matches!(
            database.put_entity_stream(avatar, PENDING_DATABASE_ID, &mut stream, AutoLoadUpdate::Retain, false),
            Err(DbError::Storage(msg)) if msg.contains("malformed")
        ));
        assert_eq!(database.to_xml().matches("<Avatar>").count(), 0);
    }

    #[test]
    fn put_explicit_id_inserts_under_the_given_id() {
        let mut database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        let db_id = database
            .put_entity_stream(
                avatar,
                41,
                &mut avatar_stream(5, 0),
                AutoLoadUpdate::Retain,
                true,
            )
            .expect("explicit insert");
        assert_eq!(db_id, 41);
        assert_eq!(database.max_id(), 41, "maxID_ tracks the explicit id");
        // An explicit id onto a taken row is refused.
        assert!(matches!(
            database.put_entity_stream(avatar, 41, &mut avatar_stream(5, 0), AutoLoadUpdate::Retain, true),
            Err(DbError::Storage(msg)) if msg.contains("already in use")
        ));
    }

    #[test]
    fn del_entity_removes_by_id_and_by_name() {
        let mut database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        let first = database
            .put_entity_stream(
                avatar,
                PENDING_DATABASE_ID,
                &mut avatar_stream(1, 0),
                AutoLoadUpdate::True,
                false,
            )
            .expect("first");
        database
            .put_entity_stream(
                avatar,
                PENDING_DATABASE_ID,
                &mut avatar_stream(2, 0),
                AutoLoadUpdate::True,
                false,
            )
            .expect("second");

        database.del_entity(avatar, first, None).expect("by id");
        assert_eq!(database.to_xml().matches("<Avatar>").count(), 1);
        assert_eq!(database.get_database_id_from_name(avatar, "avatar-1"), None);

        database
            .del_entity(avatar, 0, Some("avatar-2"))
            .expect("by name");
        assert_eq!(database.to_xml().matches("<Avatar>").count(), 0);

        assert!(matches!(
            database.del_entity(avatar, 0, Some("nobody")),
            Err(DbError::NotFound)
        ));
        assert!(matches!(
            database.del_entity(avatar, 123, None),
            Err(DbError::NotFound)
        ));
    }

    #[test]
    fn spare_ids_are_reused_before_generated_ones() {
        let mut database = XmlDatabase::new(defs());
        database.put_ids(&[7, 3]);
        assert_eq!(database.get_ids(3), vec![3, 7, 1]);
        assert_eq!(database.get_ids(1), vec![2]);

        // The generated sequence wraps at the first local id
        // (`xml_database.cpp:922-926`).
        database.next_id = FIRST_LOCAL_ENTITY_ID - 1;
        assert_eq!(
            database.get_ids(2),
            vec![FIRST_LOCAL_ENTITY_ID - 1, FIRST_ENTITY_ID]
        );
    }

    #[test]
    fn space_data_round_trips_through_the_streams() {
        let mut original = BinaryStream::new();
        original.write_u32(1); // one space
        original.write_i32(9); // space id
        original.write_u32(2); // two items
        original.write_u64(0xA); // space key
        original.write_u16(1); // data key
        let _ = original.write_blob(&[1, 2, 3]);
        original.write_u64(0xB);
        original.write_u16(2);
        let _ = original.write_blob(&[]);

        let mut database = XmlDatabase::new(defs());
        let mut echo = BinaryStream::new();
        database.write_space_data(&mut original).expect("write");
        database.get_spaces_data(&mut echo).expect("get");

        // Nothing auto-loads, so the stream names zero spaces and carries no
        // items — the archive sits in the file waiting.
        echo.rewind();
        assert_eq!(echo.read_i32().unwrap(), 0);

        let xml = database.to_xml();
        assert!(xml.contains("<AutoLoadedSpaceData>"), "{xml}");
        assert!(xml.contains("<spaceKey>\t10\t</spaceKey>"), "{xml}");
        assert!(xml.contains("<data>\tAQID\t</data>"), "base64: {xml}");
    }

    #[test]
    fn archived_space_data_flows_for_auto_loading_entities() {
        let xml = "<root> \
                       <Guard> <name> g </name> <spaceID> 3 </spaceID> <databaseID> 1 </databaseID> </Guard> \
                       <_BigWorldInfo> <AutoLoadedEntities> <databaseID> 1 </databaseID> </AutoLoadedEntities> </_BigWorldInfo> \
                   </root>";
        let mut database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        assert_eq!(database.auto_load_space_ids(), BTreeSet::from([3]));

        let mut archive = BinaryStream::new();
        archive.write_u32(1);
        archive.write_i32(3);
        archive.write_u32(1);
        archive.write_u64(77);
        archive.write_u16(5);
        let _ = archive.write_blob(&[9]);
        database.write_space_data(&mut archive).expect("write");

        let mut stream = BinaryStream::new();
        database.get_spaces_data(&mut stream).expect("get");
        stream.rewind();
        assert_eq!(stream.read_i32().unwrap(), 1);
        assert_eq!(stream.read_i32().unwrap(), 3);
        assert_eq!(stream.read_i32().unwrap(), 1);
        assert_eq!(stream.read_u64().unwrap(), 77);
        assert_eq!(stream.read_u16().unwrap(), 5);
        assert_eq!(stream.read_blob().unwrap(), vec![9]);
    }

    #[test]
    fn auto_load_tracks_the_info_section() {
        let xml = "<root> \
                       <Avatar> <name> moe </name> <databaseID> 2 </databaseID> </Avatar> \
                       <_BigWorldInfo> <AutoLoadedEntities> <databaseID> 2 </databaseID> </AutoLoadedEntities> </_BigWorldInfo> \
                   </root>";
        let mut database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        assert_eq!(database.auto_load_ids(), vec![2]);

        // A repeat add is a no-op (`xml_database.cpp:1265-1267`).
        database.add_to_auto_load(2);
        database.add_to_auto_load(1);
        assert_eq!(database.auto_load_ids(), vec![1, 2]);
        assert_eq!(
            database.to_xml().matches("<databaseID>").count(),
            3,
            "one per entity section plus one per auto-load entry"
        );

        database.remove_from_auto_load(2);
        assert_eq!(database.auto_load_ids(), vec![1]);
        assert_eq!(database.to_xml().matches("<databaseID>").count(), 2);
        // Removing an absent id is a no-op.
        database.remove_from_auto_load(99);
        assert_eq!(database.auto_load_ids(), vec![1]);
    }

    #[test]
    fn auto_load_entities_resolves_types_and_drops_stale_rows() {
        let xml = "<root> \
                       <Avatar> <name> moe </name> <databaseID> 2 </databaseID> </Avatar> \
                       <Guard> <name> gone </name> <databaseID> 3 </databaseID> </Guard> \
                       <Monster> <name> stray </name> <databaseID> 4 </databaseID> </Monster> \
                       <_BigWorldInfo> \
                           <AutoLoadedEntities> \
                               <databaseID> 2 </databaseID> \
                               <databaseID> 3 </databaseID> \
                               <databaseID> 4 </databaseID> \
                               <databaseID> 404 </databaseID> \
                           </AutoLoadedEntities> \
                       </_BigWorldInfo> \
                   </root>";
        let mut database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        let guard = database.defs.name_to_index("Guard").unwrap();
        // Existing sections resolve to their type; the sectionless id 404 is
        // dropped and the unknown type name is reported and dropped
        // (`xml_database.cpp:1070-1092`).
        assert_eq!(database.auto_load_entities(), vec![(avatar, 2), (guard, 3)]);
        assert_eq!(database.auto_load_ids(), vec![2, 3]);
        assert_eq!(
            database.to_xml().matches("<databaseID>").count(),
            5,
            "three entity sections plus two auto-load entries"
        );
    }

    #[test]
    fn save_rotates_the_file_to_a_bak_and_archive_rotates_numbered() {
        let dir = std::env::temp_dir().join(format!("xmldb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("database.xml");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(dir.join("database.xml.bak"));
        let _ = std::fs::remove_file(dir.join("database.xml.1"));

        let mut database = XmlDatabase::new(defs());
        database.set_file_path(path.display().to_string());
        database.save().expect("save");
        assert!(path.exists());
        assert!(
            !dir.join("database.xml.bak").exists(),
            "nothing to move yet"
        );

        // A second save moves the live file to .bak first
        // (`xml_database.cpp:366-371`).
        let first = std::fs::read_to_string(&path).expect("read");
        database.save().expect("save again");
        assert_eq!(
            std::fs::read_to_string(dir.join("database.xml.bak")).expect("bak"),
            first
        );
        assert_eq!(std::fs::read_to_string(&path).expect("live"), first);

        // Archive: the live file becomes .1 (`xml_database.cpp:334-343`).
        database.archive().expect("archive");
        assert_eq!(
            std::fs::read_to_string(dir.join("database.xml.1")).expect("1"),
            first
        );
        assert!(path.exists(), "the archive rewrites the live file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_storage_contract_round_trips_a_record() {
        use crate::storage::load_entity;

        let mut database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();

        let mut record = EntityRecord::new(avatar);
        record.name = Some("moe".into());
        record.values = vec![PropertyValue::String("moe".into()), PropertyValue::Int(30)];
        let db_id = database.put_entity(&record, false).expect("create");
        assert_eq!(db_id, 1);

        let loaded = load_entity(
            &database,
            &EntityKey {
                type_id: avatar,
                db_id,
            },
        )
        .expect("load");
        assert_eq!(loaded.name.as_deref(), Some("moe"));
        assert_eq!(loaded.values, record.values);

        // By name (`idatabase.hpp:131-135`).
        let loaded = database
            .lookup_entity_by_name(avatar, "moe")
            .expect("lookup")
            .expect("found");
        assert_eq!(loaded.db_id, 1);

        // Every row of the type.
        let mut second = EntityRecord::new(avatar);
        second.name = Some("curly".into());
        second.values = vec![
            PropertyValue::String("curly".into()),
            PropertyValue::Int(30),
        ];
        let second_id = database.put_entity(&second, false).expect("create 2");
        let rows = database.lookup_entities(avatar).expect("lookup all");
        assert_eq!(
            rows.iter().map(|r| r.db_id).collect::<Vec<_>>(),
            vec![1, second_id]
        );

        // An unknown type or name is None, not an error.
        assert!(
            database
                .lookup_entity_by_name(avatar, "nobody")
                .expect("lookup")
                .is_none()
        );
        assert!(
            database
                .get_entity(&EntityKey {
                    type_id: avatar,
                    db_id: 99
                })
                .expect("get")
                .is_none()
        );

        // A cell entity keeps its pose across record puts (the record
        // carries none).
        let guard = database.defs.name_to_index("Guard").unwrap();
        let xml_in = "<root> \
                          <Guard> <name> g </name> \
                              <position> 8.000000 0.000000 0.000000 </position> \
                              <spaceID> 4 </spaceID> \
                              <databaseID> 9 </databaseID> \
                          </Guard> \
                      </root>";
        let mut cell_database = XmlDatabase::from_xml(xml_in, defs()).expect("parse");
        let mut cell_record = EntityRecord::new(guard);
        cell_record.db_id = 9;
        cell_record.name = Some("g".into());
        cell_record.values = vec![PropertyValue::String("g".into())];
        cell_database.put_entity(&cell_record, false).expect("put");
        let loaded = cell_database
            .get_entity(&EntityKey {
                type_id: guard,
                db_id: 9,
            })
            .expect("get")
            .expect("found");
        assert_eq!(loaded.values, cell_record.values);

        // Deletes and refusals.
        EntityStorage::del_entity(
            &mut database,
            &EntityKey {
                type_id: avatar,
                db_id,
            },
        )
        .expect("delete");
        assert!(
            database
                .get_entity(&EntityKey {
                    type_id: avatar,
                    db_id
                })
                .expect("get")
                .is_none()
        );
        assert!(matches!(
            EntityStorage::del_entity(
                &mut database,
                &EntityKey {
                    type_id: avatar,
                    db_id
                },
            ),
            Err(DbError::NotFound)
        ));
        assert!(matches!(
            EntityStorage::del_entity(
                &mut database,
                &EntityKey {
                    type_id: avatar,
                    db_id: PENDING_DATABASE_ID,
                },
            ),
            Err(DbError::NotFound)
        ));
        // The delete released the name, so the record creates anew past
        // `maxID_` (`xml_database.cpp:601-603`).
        assert_eq!(database.put_entity(&record, false).expect("recreate"), 3);
    }

    #[test]
    fn the_xml_database_tracks_no_online_state() {
        let mut database = XmlDatabase::new(defs());
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        let key = EntityKey {
            type_id: avatar,
            db_id: 1,
        };
        assert!(matches!(
            database.set_log_on_record(
                &key,
                LogOnRecord {
                    entity_id: 1,
                    ip: 0,
                    port: 0,
                    salt: 0
                }
            ),
            Err(DbError::Storage(msg)) if msg.contains("no online state")
        ));
        database
            .remove_log_on_record(&key)
            .expect("remove is a no-op");
        assert!(database.log_on_record(&key).expect("query").is_none());
    }

    #[test]
    fn the_cell_pose_streams_its_three_vectors() {
        let pose = CellPose {
            position: Vector3::new(1.0, 2.0, 3.0),
            direction: [4.0, 5.0, 6.0],
            space_id: 7,
        };
        let mut stream = BinaryStream::new();
        pose.write_to(&mut stream);
        stream.rewind();
        assert_eq!(CellPose::read_from(&mut stream).expect("read"), pose);
        // A truncated stream is an error, not zeros.
        let mut stream = BinaryStream::new();
        stream.write_f32(1.0);
        stream.rewind();
        assert!(CellPose::read_from(&mut stream).is_err());
    }

    #[test]
    fn a_duplicate_dbid_and_a_duplicate_name_are_ignored() {
        // The second entity of a pair is dropped from the maps
        // (`xml_database.cpp:239-255` and `:271-287`).
        let xml = "<root> \
                       <Avatar> <name> moe </name> <databaseID> 1 </databaseID> </Avatar> \
                       <Avatar> <name> twin </name> <databaseID> 1 </databaseID> </Avatar> \
                       <Avatar> <name> moe </name> <databaseID> 2 </databaseID> </Avatar> \
                   </root>";
        let database = XmlDatabase::from_xml(xml, defs()).expect("parse");
        let avatar = database.defs.name_to_index("Avatar").unwrap();
        assert_eq!(database.get_database_id_from_name(avatar, "moe"), Some(1));
        assert_eq!(database.get_database_id_from_name(avatar, "twin"), None);
        // Only the name duplicates for id 2, so it stays in idToData_
        // (`xml_database.cpp:271-287`: the second same-named entity is not
        // retrievable by name, but remains retrievable by id).
        assert_eq!(
            database
                .get_entity_stream(avatar, 2, None, &mut BinaryStream::new())
                .found,
            true
        );
    }

    /// Property-section naming: the component-qualified tag for a
    /// componentised property, the bare name otherwise.
    #[test]
    fn property_sections_are_named_after_the_component() {
        let mut property = DataDescription::default();
        property.member.name = "hp".into();
        property.component_name = String::new();
        assert_eq!(property_section_name(&property), "hp");
        property.component_name = "movement".into();
        assert_eq!(property_section_name(&property), "movement.hp");
    }
}
