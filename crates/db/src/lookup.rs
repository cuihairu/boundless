//! The lookup operations: by identifier name and by type + property
//! queries.
//!
//! Reference faces: `IDatabase::getDatabaseIDFromName` with
//! `XMLDatabase::findEntityByName` (the per-type name maps,
//! `xml_database.cpp:846-857`), and `IDatabase::lookUpEntities`
//! (`LookUpEntitiesCriteria`, `lib/server/look_up_entities.*`). The XML
//! backend does not implement `lookUpEntities`; the MySQL backend runs it
//! as SQL — string matches on simple properties, an optional ORDER BY,
//! and LIMIT/OFFSET (`entity_type_mapping.cpp:378-470`). The in-memory
//! backend runs the same matching over the stored records.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use entitydef::value::PropertyValue;
use foundation::ids::{DatabaseId, EntityTypeId};

use crate::error::DbError;
use crate::record::EntityRecord;
use crate::storage::{EntityKey, EntityStorage, load_entity};

/// The storage face of one entity type's definition — the knowledge
/// `EntityDefs`/`EntityTypeMapping` carry in the reference: the persistent
/// property names in [`EntityRecord::values`] order, and which of them is
/// the identifier. The DBApp role builds these from
/// `EntityDescription::visit(BASE_DATA | CELL_DATA | ONLY_PERSISTENT_DATA)`
/// and `EntityDataFlags::is_identifier` when it lands.
#[derive(Debug, Clone)]
pub struct EntityDefIndex {
    /// The persistent property names, in `EntityRecord::values` order.
    pub properties: Vec<String>,
    /// The identifier property's value index
    /// (`EntityDefs::getIdentifierProperty`), if the type has one.
    pub identifier: Option<usize>,
}

impl EntityDefIndex {
    /// The value index of a property name.
    fn index_of(&self, name: &str) -> Option<usize> {
        self.properties.iter().position(|p| p == name)
    }
}

/// Per-type def knowledge (the `EntityDefs` face the lookups read).
#[derive(Debug, Default, Clone)]
pub struct LookupDefs {
    by_type: BTreeMap<EntityTypeId, EntityDefIndex>,
}

impl LookupDefs {
    /// No types known.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one type's storage face.
    pub fn insert(&mut self, type_id: EntityTypeId, index: EntityDefIndex) {
        self.by_type.insert(type_id, index);
    }

    /// The def face of `type_id`. An unknown type is the reference's
    /// NULL `EntityTypeMapping` — the operation fails
    /// (`mysql_database.cpp:459-466`).
    fn get(&self, type_id: EntityTypeId) -> Result<&EntityDefIndex, DbError> {
        self.by_type
            .get(&type_id)
            .ok_or_else(|| DbError::Storage(format!("entity type {type_id} is invalid")))
    }
}

/// `LookUpEntitiesFilter` (`lib/server/look_up_entities.hpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookUpEntitiesFilter {
    /// All entities.
    All,
    /// Only online entities — the `bigworldLogOnRecords` join.
    Online,
    /// Only offline entities — its negation.
    Offline,
}

/// `LookUpEntitiesCriteria` (`lib/server/look_up_entities.hpp`). The
/// defaults are the reference constructor's: filter ALL, ascending, and
/// `limit`/`offset` -1 (no clause).
#[derive(Debug, Clone)]
pub struct LookUpEntitiesCriteria {
    /// `propertyQueries_`: `(property name, search value)` string matches,
    /// all of which must hold (the SQL AND).
    pub property_queries: Vec<(String, String)>,
    /// `filter_`.
    pub filter: LookUpEntitiesFilter,
    /// `sortProperty_` — empty means no ORDER BY.
    pub sort_property: String,
    /// `isAscending_`.
    pub is_ascending: bool,
    /// `limit_` — negative means no LIMIT clause.
    pub limit: i32,
    /// `offset_` — non-positive means no OFFSET clause.
    pub offset: i32,
}

impl Default for LookUpEntitiesCriteria {
    fn default() -> Self {
        LookUpEntitiesCriteria {
            property_queries: Vec::new(),
            filter: LookUpEntitiesFilter::All,
            sort_property: String::new(),
            is_ascending: true,
            limit: -1,
            offset: -1,
        }
    }
}

impl LookUpEntitiesCriteria {
    /// `hasSortProperty`.
    pub fn has_sort_property(&self) -> bool {
        !self.sort_property.is_empty()
    }

    /// `addPropertyQuery`.
    pub fn add_property_query(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.property_queries.push((name.into(), value.into()));
    }
}

/// `IDatabase::getDatabaseIDFromName`: the identifier name → database id
/// (`findEntityByName` over the name maps). A name with no row — or a
/// type without an identifier property, which never gets name-map
/// entries — is [`DbError::NotFound`], the reference's `isOK=false` on
/// dbID 0.
pub fn get_database_id_from_name(
    storage: &impl EntityStorage,
    defs: &LookupDefs,
    type_id: EntityTypeId,
    name: &str,
) -> Result<DatabaseId, DbError> {
    let def = defs.get(type_id)?;
    let ident = def.identifier.ok_or(DbError::NotFound)?;
    for record in storage.all_entities()? {
        if record.type_id != type_id {
            continue;
        }
        match record.values.get(ident) {
            Some(PropertyValue::String(found)) if found == name => {
                return Ok(record.db_id);
            }
            _ => {}
        }
    }
    Err(DbError::NotFound)
}

/// The name leg returning the record: resolve the name, then read the
/// row (`getEntity` after `getDatabaseIDFromName`).
pub fn lookup_entity_by_name(
    storage: &impl EntityStorage,
    defs: &LookupDefs,
    type_id: EntityTypeId,
    name: &str,
) -> Result<EntityRecord, DbError> {
    let db_id = get_database_id_from_name(storage, defs, type_id, name)?;
    load_entity(storage, &EntityKey { type_id, db_id })
}

/// `IDatabase::lookUpEntities`: the records of `type_id` matching
/// `criteria`. The matching runs over simple (scalar) properties — the
/// reference rejects table-backed properties for a string match up front
/// (`entity_type_mapping.cpp:389-401`).
pub fn lookup_entities(
    storage: &impl EntityStorage,
    defs: &LookupDefs,
    type_id: EntityTypeId,
    criteria: &LookUpEntitiesCriteria,
) -> Result<Vec<EntityRecord>, DbError> {
    let def = defs.get(type_id)?;

    match criteria.filter {
        LookUpEntitiesFilter::All => {}
        // The reference runs these as a join over `bigworldLogOnRecords`;
        // they need the online-state face (BW-09-01-05).
        LookUpEntitiesFilter::Online | LookUpEntitiesFilter::Offline => {
            return Err(DbError::Storage(
                "the online and offline filters need the online-state face (BW-09-01-05)".into(),
            ));
        }
    }

    // Resolve the query legs against the def up front, as the reference
    // validates the property mappings before building the query.
    let mut queries = Vec::with_capacity(criteria.property_queries.len());
    for (name, want) in &criteria.property_queries {
        let index = def.index_of(name).ok_or_else(|| {
            DbError::Storage(format!(
                "property {name} is not of appropriate type for string match"
            ))
        })?;
        queries.push((index, want.as_str()));
    }
    let sort_index = if criteria.has_sort_property() {
        Some(def.index_of(&criteria.sort_property).ok_or_else(|| {
            DbError::Storage(format!(
                "property {} is not of appropriate type for sorting",
                criteria.sort_property
            ))
        })?)
    } else {
        None
    };

    let mut matched: Vec<EntityRecord> = storage
        .all_entities()?
        .into_iter()
        .filter(|record| record.type_id == type_id)
        .filter(|record| {
            queries
                .iter()
                .all(|(index, want)| match_string(record.values.get(*index), want))
        })
        .collect();

    if let Some(index) = sort_index {
        matched.sort_by(|a, b| {
            compare_values(a.values.get(index), b.values.get(index))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if !criteria.is_ascending {
            matched.reverse();
        }
    }

    if criteria.offset > 0 {
        let offset = criteria.offset as usize;
        matched = if offset < matched.len() {
            matched.split_off(offset)
        } else {
            Vec::new()
        };
    }
    if criteria.limit >= 0 {
        matched.truncate(criteria.limit as usize);
    }
    Ok(matched)
}

/// The SQL string-match rule: a stored scalar renders to its string form
/// and compares. Table-backed shapes (the reference's `hasTable`
/// properties) never string-match, and a missing value matches nothing.
fn match_string(value: Option<&PropertyValue>, want: &str) -> bool {
    match value {
        Some(PropertyValue::String(s)) => s == want,
        Some(PropertyValue::Int(i)) => i.to_string() == want,
        Some(PropertyValue::Uint(u)) => u.to_string() == want,
        Some(PropertyValue::Float(f)) => f.to_string() == want,
        Some(PropertyValue::Blob(b)) => String::from_utf8_lossy(b) == want,
        Some(_) => false,
        None => false,
    }
}

/// The ORDER BY comparison for a scalar column; shapes that cannot be
/// ordered (or absent values) compare equal and keep the record order.
fn compare_values(a: Option<&PropertyValue>, b: Option<&PropertyValue>) -> Option<Ordering> {
    match (a, b) {
        (Some(PropertyValue::Int(x)), Some(PropertyValue::Int(y))) => Some(x.cmp(y)),
        (Some(PropertyValue::Uint(x)), Some(PropertyValue::Uint(y))) => Some(x.cmp(y)),
        (Some(PropertyValue::Float(x)), Some(PropertyValue::Float(y))) => x.partial_cmp(y),
        (Some(PropertyValue::String(x)), Some(PropertyValue::String(y))) => Some(x.cmp(y)),
        (Some(PropertyValue::Blob(x)), Some(PropertyValue::Blob(y))) => Some(x.cmp(y)),
        _ => None,
    }
}

// -----------------------------------------------------------------------------
// Section: tests
// -----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    /// Type 3: `name` (identifier), `level`, `title`. Type 4: `model`
    /// only, no identifier.
    fn defs() -> LookupDefs {
        let mut defs = LookupDefs::new();
        defs.insert(
            3,
            EntityDefIndex {
                properties: vec!["name".into(), "level".into(), "title".into()],
                identifier: Some(0),
            },
        );
        defs.insert(
            4,
            EntityDefIndex {
                properties: vec!["model".into()],
                identifier: None,
            },
        );
        defs
    }

    fn entity(db_id: DatabaseId, name: &str, level: i64, title: &str) -> EntityRecord {
        EntityRecord {
            type_id: 3,
            db_id,
            values: vec![
                PropertyValue::String(name.into()),
                PropertyValue::Int(level),
                PropertyValue::String(title.into()),
            ],
        }
    }

    fn store() -> MemoryStorage {
        let mut store = MemoryStorage::new();
        store.insert(entity(1, "alice", 30, "mage"));
        store.insert(entity(2, "bob", 20, "warrior"));
        store.insert(entity(3, "carol", 40, "mage"));
        store.insert(EntityRecord {
            type_id: 4,
            db_id: 4,
            values: vec![PropertyValue::String("goblin".into())],
        });
        store
    }

    #[test]
    fn a_name_resolves_to_its_record() {
        let store = store();
        let reader = store.connection();
        let defs = defs();

        let found = lookup_entity_by_name(&reader, &defs, 3, "alice").expect("alice resolves");
        assert_eq!(found, entity(1, "alice", 30, "mage"));

        let db_id = get_database_id_from_name(&store, &defs, 3, "carol").expect("carol");
        assert_eq!(db_id, 3);
    }

    #[test]
    fn an_unknown_name_is_not_found() {
        let store = store();
        let defs = defs();

        let err = lookup_entity_by_name(&store, &defs, 3, "dave").expect_err("no dave");
        assert_eq!(err, DbError::NotFound);
        let err =
            get_database_id_from_name(&store, &defs, 3, "dave").expect_err("no dave by id either");
        assert_eq!(err, DbError::NotFound);
    }

    #[test]
    fn a_type_without_an_identifier_has_no_name_leg() {
        let store = store();
        let defs = defs();

        let err = get_database_id_from_name(&store, &defs, 4, "goblin")
            .expect_err("no identifier, no name map");
        assert_eq!(err, DbError::NotFound);
    }

    #[test]
    fn an_unknown_type_is_an_error() {
        let store = store();
        let defs = defs();

        let err = get_database_id_from_name(&store, &defs, 99, "anyone").expect_err("unknown type");
        assert!(matches!(err, DbError::Storage(msg) if msg.contains("invalid")));

        let err = lookup_entities(&store, &defs, 99, &LookUpEntitiesCriteria::default())
            .expect_err("unknown type on the type leg too");
        assert!(matches!(err, DbError::Storage(msg) if msg.contains("invalid")));
    }

    #[test]
    fn by_type_without_queries_returns_every_row_of_that_type() {
        let store = store();
        let defs = defs();

        let found = lookup_entities(&store, &defs, 3, &LookUpEntitiesCriteria::default())
            .expect("type scan");
        assert_eq!(
            found,
            vec![
                entity(1, "alice", 30, "mage"),
                entity(2, "bob", 20, "warrior"),
                entity(3, "carol", 40, "mage")
            ]
        );
    }

    #[test]
    fn property_queries_narrow_the_match() {
        let store = store();
        let defs = defs();

        let mut criteria = LookUpEntitiesCriteria::default();
        criteria.add_property_query("title", "mage");
        let found = lookup_entities(&store, &defs, 3, &criteria).expect("title match");
        assert_eq!(
            found,
            vec![
                entity(1, "alice", 30, "mage"),
                entity(3, "carol", 40, "mage")
            ]
        );

        criteria.add_property_query("level", "30");
        let found = lookup_entities(&store, &defs, 3, &criteria).expect("title and level");
        assert_eq!(found, vec![entity(1, "alice", 30, "mage")]);

        criteria.add_property_query("name", "carol");
        let found = lookup_entities(&store, &defs, 3, &criteria)
            .expect("unsatisfiable conjunction is empty, not an error");
        assert!(found.is_empty());

        // A scalar column matches its string rendering (the SQL cast).
        let mut numeric = LookUpEntitiesCriteria::default();
        numeric.add_property_query("level", "20");
        let found = lookup_entities(&store, &defs, 3, &numeric).expect("rendered int");
        assert_eq!(found, vec![entity(2, "bob", 20, "warrior")]);
    }

    #[test]
    fn sort_offset_and_limit_apply() {
        let store = store();
        let defs = defs();

        let criteria = LookUpEntitiesCriteria {
            sort_property: "level".into(),
            is_ascending: false,
            offset: 1,
            limit: 1,
            ..Default::default()
        };
        let found = lookup_entities(&store, &defs, 3, &criteria).expect("paged slice");
        // Descending by level is carol(40), alice(30), bob(20); skip one,
        // take one.
        assert_eq!(found, vec![entity(1, "alice", 30, "mage")]);

        let criteria = LookUpEntitiesCriteria {
            sort_property: "level".into(),
            ..Default::default()
        };
        let found = lookup_entities(&store, &defs, 3, &criteria).expect("ascending");
        assert_eq!(
            found,
            vec![
                entity(2, "bob", 20, "warrior"),
                entity(1, "alice", 30, "mage"),
                entity(3, "carol", 40, "mage")
            ]
        );
    }

    #[test]
    fn an_unknown_query_or_sort_property_is_an_error() {
        let store = store();
        let defs = defs();

        let mut criteria = LookUpEntitiesCriteria::default();
        criteria.add_property_query("nope", "x");
        let err = lookup_entities(&store, &defs, 3, &criteria).expect_err("unknown query property");
        assert!(
            matches!(err, DbError::Storage(ref msg) if msg.contains("not of appropriate type for string match"))
        );

        let criteria = LookUpEntitiesCriteria {
            sort_property: "nope".into(),
            ..Default::default()
        };
        let err = lookup_entities(&store, &defs, 3, &criteria).expect_err("unknown sort property");
        assert!(
            matches!(err, DbError::Storage(ref msg) if msg.contains("not of appropriate type for sorting"))
        );
    }

    #[test]
    fn the_online_and_offline_filters_wait_for_the_online_state_face() {
        let store = store();
        let defs = defs();

        for filter in [LookUpEntitiesFilter::Online, LookUpEntitiesFilter::Offline] {
            let criteria = LookUpEntitiesCriteria {
                filter,
                ..Default::default()
            };
            let err =
                lookup_entities(&store, &defs, 3, &criteria).expect_err("no online state yet");
            assert!(matches!(err, DbError::Storage(ref msg) if msg.contains("online-state face")));
        }
    }

    #[test]
    fn scalar_columns_render_and_order_across_the_scalar_kinds() {
        let mut store = MemoryStorage::new();
        let mut defs = defs();
        // "missing" is known to the def but absent from the stored rows —
        // the None column legs.
        defs.insert(
            5,
            EntityDefIndex {
                properties: vec![
                    "name".into(),
                    "count".into(),
                    "weight".into(),
                    "tag".into(),
                    "missing".into(),
                ],
                identifier: Some(0),
            },
        );
        for (db_id, name, count, weight, tag) in [
            (10i64, "b", 2u64, 2.5f64, &b"zz"[..]),
            (11, "a", 10, 1.5, &b"aa"[..]),
        ] {
            store.insert(EntityRecord {
                type_id: 5,
                db_id,
                values: vec![
                    PropertyValue::String(name.into()),
                    PropertyValue::Uint(count),
                    PropertyValue::Float(weight),
                    PropertyValue::Blob(tag.to_vec()),
                ],
            });
        }
        // A short row: the def knows five properties, this row has one.
        store.insert(EntityRecord {
            type_id: 5,
            db_id: 12,
            values: vec![PropertyValue::String("c".into())],
        });

        let query = |name: &str, want: &str| {
            let mut criteria = LookUpEntitiesCriteria::default();
            criteria.add_property_query(name, want);
            lookup_entities(&store, &defs, 5, &criteria)
                .expect("scalar match")
                .into_iter()
                .map(|r| r.db_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(query("count", "10"), vec![11]); // Uint renders
        assert_eq!(query("weight", "1.5"), vec![11]); // Float renders
        assert_eq!(query("tag", "aa"), vec![11]); // Blob renders
        assert!(query("missing", "x").is_empty()); // absent column matches nothing

        let sort_by = |name: &str| {
            let criteria = LookUpEntitiesCriteria {
                sort_property: name.into(),
                ..Default::default()
            };
            lookup_entities(&store, &defs, 5, &criteria)
                .expect("scalar sort")
                .into_iter()
                .map(|r| r.db_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(sort_by("name"), vec![11, 10, 12]); // String order
        // The rows the column cannot order (the short row's absent
        // values tie) stay at the end by stability.
        assert_eq!(sort_by("count"), vec![10, 11, 12]); // Uint order
        assert_eq!(sort_by("weight"), vec![11, 10, 12]); // Float order
        assert_eq!(sort_by("tag"), vec![11, 10, 12]); // Blob order
        // The absent column cannot order anything: the rows keep their
        // (type, db) order.
        assert_eq!(sort_by("missing"), vec![10, 11, 12]);

        // An OFFSET past the end of the results is an empty page.
        let criteria = LookUpEntitiesCriteria {
            offset: 99,
            ..Default::default()
        };
        let found = lookup_entities(&store, &defs, 5, &criteria).expect("past the end");
        assert!(found.is_empty());
    }

    #[test]
    fn table_backed_values_never_string_match() {
        let mut store = MemoryStorage::new();
        let arrayed = EntityRecord {
            type_id: 4,
            db_id: 9,
            values: vec![PropertyValue::Sequence(vec![PropertyValue::Int(1)])],
        };
        store.insert(arrayed.clone());
        let mut defs = defs();
        defs.insert(
            4,
            EntityDefIndex {
                properties: vec!["model".into()],
                identifier: None,
            },
        );

        let mut criteria = LookUpEntitiesCriteria::default();
        criteria.add_property_query("model", "1");
        let found = lookup_entities(&store, &defs, 4, &criteria)
            .expect("a table-backed property is skipped, not matched");
        assert!(found.is_empty());
    }
}
