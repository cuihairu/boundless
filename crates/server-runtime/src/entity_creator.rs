//! BW-07-01-01 dynamically creating a Base entity and allocating its id.
//!
//! `EntityCreator::createBaseLocally` / `createBase`
//! (`server/baseapp/entity_creator.cpp`) implement the script `createBase`:
//!
//! 1. the first argument names the entity type, which must be on the base and
//!    not a service;
//! 2. the remaining arguments are split into base and cell data by each
//!    property's `isBaseData`/`isCellData` flags. A property provided in the
//!    args moves to the cell data when it is cell data; a property not provided
//!    is filled with its initial value. `position`, `direction` and `spaceID`
//!    move to the cell data (unless the def marks them base data);
//! 3. `createBase` allocates an id from the `IDClient` pool (`id_client.cpp` —
//!    note that `MF_ID_RECYCLING` is off in this build, so `putUsedID` is a
//!    no-op), constructs the entity via `newEntityBase`, and `Base::init`
//!    (`base.cpp`) assigns the base properties, stores the cell data and calls
//!    the script `__init__` — the creation callback.
//!
//! The recorded callbacks are the acceptance: 类型、初始化参数和回调吻合. The
//! remote `createBase*` paths (which carry an explicit callback over the wire)
//! are a later item; this models the local script path.

use std::collections::HashMap;
use std::collections::VecDeque;

/// `EntityID`.
pub type EntityId = u32;

/// `DatabaseID`.
pub type DatabaseId = u64;

/// A property value (the Python object model is reduced to these).
#[derive(Clone, Debug, PartialEq)]
pub enum PropValue {
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(String),
    /// A boolean.
    Bool(bool),
    /// A `Vector3`.
    Vec3([f32; 3]),
}

/// One property of an entity type (`DataDescription`-lite).
#[derive(Clone, Debug, PartialEq)]
pub struct PropDesc {
    /// The property name.
    pub name: String,
    /// `isBaseData()`.
    pub base_data: bool,
    /// `isCellData()`.
    pub cell_data: bool,
    /// `isComponentised()`.
    pub componentised: bool,
    /// `pInitialValue()`.
    pub initial: PropValue,
}

impl PropDesc {
    /// Constructor.
    pub fn new(
        name: &str,
        base_data: bool,
        cell_data: bool,
        componentised: bool,
        initial: PropValue,
    ) -> Self {
        Self {
            name: name.to_string(),
            base_data,
            cell_data,
            componentised,
            initial,
        }
    }
}

/// An entity type (`EntityType`-lite).
#[derive(Clone, Debug, PartialEq)]
pub struct EntityType {
    /// The type id.
    pub id: u32,
    /// The type name.
    pub name: String,
    /// `canBeOnBase()`.
    pub can_be_on_base: bool,
    /// `isService()`.
    pub is_service: bool,
    /// `isProxy()`.
    pub is_proxy: bool,
    /// `canBeOnCell()`.
    pub can_be_on_cell: bool,
    /// The properties.
    pub properties: Vec<PropDesc>,
}

impl EntityType {
    /// Find a property by name.
    pub fn property(&self, name: &str) -> Option<&PropDesc> {
        self.properties.iter().find(|p| p.name == name)
    }
}

/// The `IDClient` id pool (`id_client.cpp`).
#[derive(Debug)]
pub struct IdPool {
    ready: VecDeque<EntityId>,
    low_size: usize,
    desired_size: usize,
    high_size: usize,
    critically_low_size: usize,
    pending_request: bool,
    in_emergency: bool,
}

impl IdPool {
    /// Constructor with the size thresholds (`criticallyLow < low < desired <
    /// high`).
    pub fn new(critically_low: usize, low: usize, desired: usize, high: usize) -> Self {
        Self {
            ready: VecDeque::new(),
            low_size: low,
            desired_size: desired,
            high_size: high,
            critically_low_size: critically_low,
            pending_request: false,
            in_emergency: true,
        }
    }

    /// `getID`: pop an id. When the pool is empty, an emergency refill is
    /// requested; if none arrives, `None` (the reference returns 0).
    pub fn get_id(&mut self) -> Option<EntityId> {
        if self.ready.is_empty() {
            self.perform_updates(true);
            if self.ready.is_empty() {
                return None;
            }
        }
        let id = self.ready.pop_front().unwrap();
        self.perform_updates(false);
        Some(id)
    }

    /// The number of ids ready to hand out.
    pub fn len(&self) -> usize {
        self.ready.len()
    }

    /// Whether no ids are ready.
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    /// Whether the pool has asked the upstream for more ids.
    pub fn pending_request(&self) -> bool {
        self.pending_request
    }

    /// `retrieveIDsFromStream`: the upstream's reply fills the pool.
    pub fn refill(&mut self, ids: &[EntityId]) {
        self.ready.extend(ids.iter().copied());
        self.pending_request = false;
    }

    /// `putUsedID`: a no-op while `MF_ID_RECYCLING` is off.
    pub fn put_used_id(&mut self, _id: EntityId) {}

    /// `returnIDs`: hand the ready ids back to the upstream.
    pub fn return_ids(&mut self) -> Vec<EntityId> {
        self.ready.drain(..).collect()
    }

    /// `performUpdates`: request more ids below the low watermark and grow the
    /// thresholds after an emergency.
    fn perform_updates(&mut self, is_emergency: bool) {
        let emergency = is_emergency || self.ready.len() < self.critically_low_size;
        if emergency && !self.in_emergency {
            // Grow the thresholds to avoid a repeat.
            self.high_size *= 2;
            self.desired_size *= 2;
            self.low_size *= 2;
            self.in_emergency = true;
        }
        if self.ready.len() < self.low_size && !self.pending_request {
            self.pending_request = true;
        }
    }
}

/// A callback recorded during creation.
#[derive(Clone, Debug, PartialEq)]
pub enum BaseCallback {
    /// The script `__init__` was called.
    Init {
        /// The entity id.
        entity_id: EntityId,
        /// The type name.
        type_name: String,
    },
    /// `onEntityInitialised` was called.
    Initialised {
        /// The entity id.
        entity_id: EntityId,
    },
}

/// A created Base entity.
#[derive(Clone, Debug, PartialEq)]
pub struct BaseEntity {
    /// The entity id.
    pub id: EntityId,
    /// The type id.
    pub type_id: u32,
    /// The type name.
    pub type_name: String,
    /// The database id (0 = not persisted).
    pub db_id: DatabaseId,
    /// Whether this is a proxy.
    pub is_proxy: bool,
    /// The assigned base properties.
    pub base_props: HashMap<String, PropValue>,
    /// The cell data (defaults plus the cell properties).
    pub cell_props: HashMap<String, PropValue>,
    /// The callbacks recorded in order.
    pub callbacks: Vec<BaseCallback>,
}

/// The failure of a `createBase` call.
#[derive(Clone, Debug, PartialEq)]
pub enum CreateError {
    /// The type name is unknown, not on the base or is a service.
    InvalidType { name: String },
    /// The id pool had no id to hand out.
    NoId,
}

/// `EntityCreator`: the type registry, the id pool and the created bases.
#[derive(Debug)]
pub struct EntityCreator {
    types: HashMap<String, EntityType>,
    id_pool: IdPool,
    bases: HashMap<EntityId, BaseEntity>,
}

/// The `createBase` script arguments: the type name and the init values.
pub type CreateArgs = HashMap<String, PropValue>;

impl EntityCreator {
    /// Constructor.
    pub fn new(id_pool: IdPool) -> Self {
        Self {
            types: HashMap::new(),
            id_pool,
            bases: HashMap::new(),
        }
    }

    /// Register an entity type.
    pub fn add_type(&mut self, ty: EntityType) {
        self.types.insert(ty.name.clone(), ty);
    }

    /// The id pool.
    pub fn id_pool(&self) -> &IdPool {
        &self.id_pool
    }

    /// The created bases.
    pub fn bases(&self) -> &HashMap<EntityId, BaseEntity> {
        &self.bases
    }

    /// `createBaseLocally`: consolidate the args and create the base.
    pub fn create_base_locally(
        &mut self,
        type_name: &str,
        args: CreateArgs,
    ) -> Result<BaseEntity, CreateError> {
        let ty = self
            .types
            .get(type_name)
            .ok_or_else(|| CreateError::InvalidType {
                name: type_name.to_string(),
            })?;
        if !ty.can_be_on_base || ty.is_service {
            return Err(CreateError::InvalidType {
                name: type_name.to_string(),
            });
        }

        let mut base_args = args;
        let mut cell_args = HashMap::new();

        // The default spatial data when the entity can be on a cell.
        if ty.can_be_on_cell {
            cell_args.insert("position".to_string(), PropValue::Vec3([0.0; 3]));
            cell_args.insert("direction".to_string(), PropValue::Vec3([0.0; 3]));
            cell_args.insert("spaceID".to_string(), PropValue::Int(0));
        }

        // Split each property by its flags: a cell-data property always goes to
        // the cell data (and leaves the base args), a base-only property stays
        // in the base args. A provided value wins, otherwise the initial value.
        for prop in &ty.properties {
            if !prop.base_data && !prop.cell_data {
                continue;
            }
            let value = base_args
                .remove(&prop.name)
                .unwrap_or_else(|| prop.initial.clone());
            if prop.cell_data {
                if ty.can_be_on_cell {
                    cell_args.insert(prop.name.clone(), value);
                }
            } else if prop.base_data {
                base_args.insert(prop.name.clone(), value);
            }
        }

        // Move position/direction/spaceID to the cell data.
        for attr in ["position", "direction", "spaceID"] {
            if let Some(value) = base_args.remove(attr) {
                if ty.can_be_on_cell {
                    cell_args.insert(attr.to_string(), value.clone());
                }
                // A base-data property named the same stays in the base args.
                let is_base_data = ty.property(attr).map(|p| p.base_data).unwrap_or(false);
                if is_base_data {
                    base_args.insert(attr.to_string(), value);
                }
            }
        }

        // The type snapshot: the base creation needs the id, name and proxy
        // flag, owned so the mutable call below can proceed.
        let (type_id, type_name, is_proxy) = (ty.id, ty.name.clone(), ty.is_proxy);
        self.create_base(type_id, &type_name, is_proxy, base_args, cell_args)
    }

    /// `createBase`: allocate an id, build the entity and initialise it.
    fn create_base(
        &mut self,
        type_id: u32,
        type_name: &str,
        is_proxy: bool,
        base_args: HashMap<String, PropValue>,
        cell_args: HashMap<String, PropValue>,
    ) -> Result<BaseEntity, CreateError> {
        let id = self.id_pool.get_id().ok_or(CreateError::NoId)?;

        let mut entity = BaseEntity {
            id,
            type_id,
            type_name: type_name.to_string(),
            db_id: 0,
            is_proxy,
            base_props: HashMap::new(),
            cell_props: HashMap::new(),
            callbacks: Vec::new(),
        };

        // `Base::init`: assign the base properties, store the cell data, then
        // call the script `__init__` and `onEntityInitialised`.
        for (name, value) in base_args {
            entity.base_props.insert(name, value);
        }
        entity.cell_props = cell_args;
        entity.callbacks.push(BaseCallback::Init {
            entity_id: id,
            type_name: type_name.to_string(),
        });
        entity
            .callbacks
            .push(BaseCallback::Initialised { entity_id: id });

        self.bases.insert(id, entity.clone());
        Ok(entity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account_type() -> EntityType {
        EntityType {
            id: 1,
            name: "Account".to_string(),
            can_be_on_base: true,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![
                PropDesc::new("id", true, false, false, PropValue::Int(0)),
                PropDesc::new("name", true, false, false, PropValue::Str(String::new())),
                PropDesc::new("pos", false, true, false, PropValue::Vec3([0.0; 3])),
                PropDesc::new("health", true, true, false, PropValue::Int(100)),
            ],
        }
    }

    #[test]
    fn create_base_locally_assigns_type_and_init_args() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[10, 11, 12]);
            pool
        });
        creator.add_type(account_type());

        let mut args = CreateArgs::new();
        args.insert("id".to_string(), PropValue::Int(1));
        args.insert("name".to_string(), PropValue::Str("alice".to_string()));
        args.insert("pos".to_string(), PropValue::Vec3([1.0, 2.0, 3.0]));
        args.insert("health".to_string(), PropValue::Int(200));

        let base = creator.create_base_locally("Account", args).unwrap();

        // The type and id match; the id came from the pool.
        assert_eq!(base.type_name, "Account");
        assert_eq!(base.type_id, 1);
        assert_eq!(base.id, 10);
        assert_eq!(base.db_id, 0);
        assert!(!base.is_proxy);

        // Base properties: the base-data props stay; the both-flag `health`
        // moved to the cell data.
        assert_eq!(base.base_props.get("id"), Some(&PropValue::Int(1)));
        assert_eq!(
            base.base_props.get("name"),
            Some(&PropValue::Str("alice".to_string()))
        );
        assert!(!base.base_props.contains_key("pos"));
        assert!(!base.base_props.contains_key("health"));

        // Cell data: the defaults, the cell `pos`, and the moved `health`.
        assert_eq!(
            base.cell_props.get("pos"),
            Some(&PropValue::Vec3([1.0, 2.0, 3.0]))
        );
        assert_eq!(base.cell_props.get("health"), Some(&PropValue::Int(200)));
        assert_eq!(base.cell_props.get("spaceID"), Some(&PropValue::Int(0)));

        // The creation callback: `__init__` then `onEntityInitialised`.
        assert_eq!(
            base.callbacks,
            vec![
                BaseCallback::Init {
                    entity_id: 10,
                    type_name: "Account".to_string(),
                },
                BaseCallback::Initialised { entity_id: 10 },
            ]
        );

        // The entity is registered.
        assert!(creator.bases().contains_key(&10));
    }

    #[test]
    fn an_unprovided_property_gets_its_initial_value() {
        let mut creator = EntityCreator::new({
            let mut pool = IdPool::new(5, 10, 50, 100);
            pool.refill(&[20]);
            pool
        });
        creator.add_type(account_type());

        let base = creator
            .create_base_locally("Account", CreateArgs::new())
            .unwrap();

        // The name defaults to "" and health to 100 (a both-flag prop moves to
        // the cell data).
        assert_eq!(
            base.base_props.get("name"),
            Some(&PropValue::Str(String::new()))
        );
        assert_eq!(base.cell_props.get("health"), Some(&PropValue::Int(100)));
    }

    #[test]
    fn a_service_or_non_base_type_is_rejected() {
        let mut creator = EntityCreator::new(IdPool::new(5, 10, 50, 100));
        creator.add_type(EntityType {
            id: 2,
            name: "Service".to_string(),
            can_be_on_base: true,
            is_service: true,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![],
        });
        creator.add_type(EntityType {
            id: 3,
            name: "CellOnly".to_string(),
            can_be_on_base: false,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![],
        });
        assert_eq!(
            creator.create_base_locally("Service", CreateArgs::new()),
            Err(CreateError::InvalidType {
                name: "Service".to_string()
            })
        );
        assert_eq!(
            creator.create_base_locally("CellOnly", CreateArgs::new()),
            Err(CreateError::InvalidType {
                name: "CellOnly".to_string()
            })
        );
    }

    #[test]
    fn an_empty_id_pool_fails_to_create() {
        let mut creator = EntityCreator::new(IdPool::new(5, 10, 50, 100));
        creator.add_type(account_type());
        assert_eq!(
            creator.create_base_locally("Account", CreateArgs::new()),
            Err(CreateError::NoId)
        );
        assert!(creator.bases().is_empty());
    }

    #[test]
    fn the_id_pool_requests_more_and_refills() {
        let mut pool = IdPool::new(2, 3, 10, 20);
        pool.refill(&[1, 2, 3]);
        assert_eq!(pool.get_id(), Some(1));
        assert_eq!(pool.get_id(), Some(2));
        // Below the low watermark, the pool asks for more.
        assert!(pool.pending_request());
        assert_eq!(pool.get_id(), Some(3));
        pool.refill(&[4, 5]);
        assert!(!pool.pending_request());
        assert_eq!(pool.get_id(), Some(4));
        // `putUsedID` is a no-op while recycling is off.
        pool.put_used_id(1);
        assert_eq!(pool.len(), 1);
    }
}
