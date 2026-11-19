//! BW-04 entity type registry groundwork: the wire type id space shared by
//! every role (`basictypes.hpp`).
//!
//! Entity type ids are assigned by the entity definition loader in
//! load-order; only the sentinel is fixed by the wire format.

use foundation::ids::EntityTypeId;

/// Sentinel meaning "no type" on the wire (`INVALID_ENTITY_TYPE_ID`).
pub const INVALID_ENTITY_TYPE_ID: EntityTypeId = 0xFFFF;

/// First assignable type id (the reference starts counting at 0).
pub const FIRST_ENTITY_TYPE_ID: EntityTypeId = 0;

/// A single entity definition: identifier string plus its assigned wire id.
/// Full property/method tables arrive with BW-04-02.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDescription {
    /// `<entity>` name, e.g. `Avatar`.
    pub name: String,
    /// Wire type id assigned in load order.
    pub type_id: EntityTypeId,
}

/// Load-order registry mapping names to wire type ids.
#[derive(Debug, Default, Clone)]
pub struct EntityTypeRegistry {
    by_name: Vec<EntityDescription>,
}

impl EntityTypeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Assigns the next free id (0, 1, 2, ...) in registration order.
    pub fn register(&mut self, name: &str) -> EntityTypeId {
        if let Some(existing) = self.lookup(name) {
            return existing;
        }
        let type_id = self
            .by_name
            .last()
            .map_or(FIRST_ENTITY_TYPE_ID, |last| last.type_id + 1);
        self.by_name.push(EntityDescription {
            name: name.to_string(),
            type_id,
        });
        type_id
    }

    pub fn lookup(&self, name: &str) -> Option<EntityTypeId> {
        self.by_name
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.type_id)
    }

    pub fn name_of(&self, type_id: EntityTypeId) -> Option<&str> {
        self.by_name
            .iter()
            .find(|d| d.type_id == type_id)
            .map(|d| d.name.as_str())
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_is_the_wire_escape() {
        assert_eq!(INVALID_ENTITY_TYPE_ID, 0xFFFF);
    }

    #[test]
    fn ids_are_assigned_in_load_order() {
        let mut reg = EntityTypeRegistry::new();
        assert_eq!(reg.register("Avatar"), 0);
        assert_eq!(reg.register("Guard"), 1);
        assert_eq!(reg.register("Chair"), 2);
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn re_registration_is_idempotent() {
        let mut reg = EntityTypeRegistry::new();
        let first = reg.register("Avatar");
        assert_eq!(reg.register("Avatar"), first, "no duplicate ids");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn fresh_registry_is_empty() {
        let mut reg = EntityTypeRegistry::new();
        assert!(reg.is_empty());
        reg.register("Avatar");
        assert!(!reg.is_empty());
    }

    #[test]
    fn lookup_roundtrips_and_misses_cleanly() {
        let mut reg = EntityTypeRegistry::new();
        let id = reg.register("Guard");
        assert_eq!(reg.lookup("Guard"), Some(id));
        assert_eq!(reg.name_of(id), Some("Guard"));
        assert_eq!(reg.lookup("Missing"), None);
        assert_eq!(reg.name_of(INVALID_ENTITY_TYPE_ID), None);
    }
}
