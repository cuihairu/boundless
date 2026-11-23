//! One stored entity: its identity and its persistent property values.
//!
//! The reference keeps an entity's persistent properties per its definition
//! and moves them over the storage block in a fixed order
//! (`EntityDescription::visit` with `BASE_DATA | CELL_DATA |
//! ONLY_PERSISTENT_DATA`: base-not-client, base-client, cell-client,
//! cell-not-client, in declaration order). A [`EntityRecord`] is the
//! storage-facing shape of that block: the entity's type and database id
//! plus its persistent values, in that visit order.

use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId, PENDING_DATABASE_ID};

/// A stored entity: `(type, database id, identifier name, values)`.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityRecord {
    /// The entity's type, `EntityDescription::index()`.
    pub type_id: EntityTypeId,
    /// The storage-assigned id, or [`PENDING_DATABASE_ID`] while unassigned.
    pub db_id: DatabaseId,
    /// The identifier property, the `name` column of the row
    /// (`EntityTypeMapping::hasIdentifier()`); `None` for a type without
    /// an `<Identifier>`.
    pub name: Option<String>,
    /// Persistent property values, in the storage visit order.
    pub values: Vec<PropertyValue>,
}

impl EntityRecord {
    /// A record for a newly created entity: pending id, unnamed, no values
    /// yet.
    pub fn new(type_id: EntityTypeId) -> Self {
        Self {
            type_id,
            db_id: PENDING_DATABASE_ID,
            name: None,
            values: Vec::new(),
        }
    }
}
