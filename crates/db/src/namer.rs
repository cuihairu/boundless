//! Table and column naming for the MySQL backend.
//!
//! The reference's `Namer` (`db_storage_mysql/namer.cpp`) builds schema names
//! for a nested entity property layout: table names are fully qualified while
//! column names are relative to the table they live in. A nested class or
//! fixed dict gets its own table, so each time the mapping descends into a new
//! table the names it has already walked are recorded as a "table level" that
//! column building restarts from.

/// `TABLE_NAME_PREFIX` (`db_storage_mysql/constants.hpp`).
pub const TABLE_NAME_PREFIX: &str = "tbl";
/// `DEFAULT_SEQUENCE_COLUMN_NAME`: the column a sequence of scalars lands in.
pub const DEFAULT_SEQUENCE_COLUMN_NAME: &str = "value";
/// `DEFAULT_SEQUENCE_TABLE_NAME`: the table a sequence of scalars lands in.
pub const DEFAULT_SEQUENCE_TABLE_NAME: &str = "values";
/// `ID_COLUMN_NAME`: the primary key column of every entity table.
pub const ID_COLUMN_NAME: &str = "id";
/// `PARENTID_COLUMN_NAME`: the parent's key in a sequence child's table.
pub const PARENTID_COLUMN_NAME: &str = "parentID";
/// `GAME_TIME_COLUMN_NAME`: the record's write time.
pub const GAME_TIME_COLUMN_NAME: &str = "gameTime";
/// `TIMESTAMP_COLUMN_NAME`: the record's write timestamp.
pub const TIMESTAMP_COLUMN_NAME: &str = "timestamp";

/// `PARENTID_INDEX_NAME` (`table_meta_data.hpp:18`): the fixed index name
/// under which a child table's [`PARENTID_COLUMN_NAME`] column is indexed.
pub const PARENTID_INDEX_NAME: &str = "parentIDIndex";

/// `MemberDescription::COMPONENT_NAME_SEPARATOR`
/// (`entitydef/member_description.hpp:26`): the separator inside a component
/// name, which has no place in a SQL identifier and is doubled into an
/// underscore pair.
pub const COMPONENT_NAME_SEPARATOR: char = '.';

/// `ColumnIndexType` (`column_type.hpp:15-22`): how a column is indexed.
/// The mapping side asks for the first three; the schema-observation side
/// ([`crate::sync_db`]) also sees the index kinds that only exist in a
/// live database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnIndexType {
    /// `INDEX_TYPE_NONE`: the column carries no index.
    None,
    /// `INDEX_TYPE_PRIMARY`: the table's primary key, created with the
    /// table itself and neither addable nor droppable afterwards.
    Primary,
    /// `INDEX_TYPE_PARENT_ID`: the child-table link to its parent row,
    /// indexed under [`PARENTID_INDEX_NAME`].
    ParentId,
    /// `INDEX_TYPE_UNIQUE`: duplicate values are refused by the server.
    Unique,
    /// `INDEX_TYPE_NON_UNIQUE`: the column is searchable but not unique.
    NonUnique,
    /// `INDEX_TYPE_EXTERNAL`: an index somebody else created. Detected
    /// but never created or dropped.
    External,
}

/// `DatabaseIndexingType` (`entitydef/data_description.hpp:49-53`): what the
/// entity definition asks of a property's column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Indexing {
    /// `DATABASE_INDEXING_NONE`.
    #[default]
    None,
    /// `DATABASE_INDEXING_UNIQUE`: exactly one row may carry a value.
    Unique,
    /// `DATABASE_INDEXING_NON_UNIQUE`: rows may repeat the value.
    NonUnique,
}

/// `PropertyMapping::getColumnIndexType`: the index the MySQL mapping asks for,
/// which defaults to "none" for anything that is not one of the two asked-for
/// kinds.
pub fn column_index_type(indexing: Indexing) -> ColumnIndexType {
    match indexing {
        Indexing::Unique => ColumnIndexType::Unique,
        Indexing::NonUnique => ColumnIndexType::NonUnique,
        Indexing::None => ColumnIndexType::None,
    }
}

/// Builds table and column names for one entity type's mapping.
#[derive(Debug, Clone)]
pub struct Namer {
    prefix: String,
    /// The names walked so far, entity name first.
    names: Vec<String>,
    /// The indices in `names` that open a new table, so column names stay
    /// relative to their own table.
    table_levels: Vec<usize>,
}

impl Namer {
    /// `Namer(entityName, tableNamePrefix)`.
    pub fn new(entity_name: &str, table_name_prefix: &str) -> Self {
        Self {
            prefix: table_name_prefix.to_owned(),
            names: vec![entity_name.to_owned()],
            table_levels: vec![1],
        }
    }

    /// The default `Namer(entityName, TABLE_NAME_PREFIX)`.
    pub fn for_entity(entity_name: &str) -> Self {
        Self::new(entity_name, TABLE_NAME_PREFIX)
    }

    /// Descend one level. `is_table` marks a nested class or fixed dict, whose
    /// properties live in a new table; an empty property name falls back to the
    /// default sequence table or column name.
    pub fn child(&self, prop_name: &str, is_table: bool) -> Self {
        let mut namer = Self {
            prefix: self.prefix.clone(),
            names: self.names.clone(),
            table_levels: self.table_levels.clone(),
        };
        let name = if prop_name.is_empty() {
            if is_table {
                DEFAULT_SEQUENCE_TABLE_NAME.to_owned()
            } else {
                DEFAULT_SEQUENCE_COLUMN_NAME.to_owned()
            }
        } else {
            Self::correct_name(prop_name)
        };
        namer.names.push(name);
        if is_table {
            namer.table_levels.push(namer.names.len());
        }
        namer
    }

    /// `buildColumnName`: relative to the innermost table.
    pub fn build_column_name(&self, prefix: &str, prop_name: &str) -> String {
        let suffix = if prop_name.is_empty() {
            DEFAULT_SEQUENCE_COLUMN_NAME.to_owned()
        } else {
            Self::correct_name(prop_name)
        };
        self.build_name(prefix, &suffix, *self.table_levels.last().unwrap_or(&0))
    }

    /// The entity's own table: the prefix plus the entity name, with no sequence
    /// suffix (the reference reaches for this through `buildName(prefix, "", 0)`).
    pub fn entity_table_name(&self) -> String {
        self.build_name(&self.prefix, "", 0)
    }

    /// `buildTableName`: fully qualified from the entity name.
    pub fn build_table_name(&self, prop_name: &str) -> String {
        let suffix = if prop_name.is_empty() {
            DEFAULT_SEQUENCE_TABLE_NAME.to_owned()
        } else {
            Self::correct_name(prop_name)
        };
        self.build_name(&self.prefix, &suffix, 0)
    }

    fn build_name(&self, prefix: &str, suffix: &str, start_idx: usize) -> String {
        let mut name = prefix.to_owned();
        for part in self.names.iter().skip(start_idx) {
            name.push('_');
            name.push_str(part);
        }
        if !suffix.is_empty() {
            name.push('_');
            name.push_str(suffix);
        }
        name
    }

    /// `correctName`: a component separator has no place in an identifier.
    fn correct_name(name: &str) -> String {
        let mut out = String::with_capacity(name.len() + 2);
        for ch in name.chars() {
            if ch == COMPONENT_NAME_SEPARATOR {
                out.push_str("__");
            } else {
                out.push(ch);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_separators_become_double_underscores() {
        let namer = Namer::for_entity("player");
        let child = namer.child("inventory.item", true);
        assert_eq!(
            child.build_table_name(""),
            "tbl_player_inventory__item_values"
        );
        assert_eq!(child.build_column_name("sm", "stack"), "sm_stack");
    }

    #[test]
    fn an_entity_table_is_prefixed_and_a_nested_one_is_relative() {
        let namer = Namer::for_entity("item");
        assert_eq!(namer.entity_table_name(), "tbl_item");
        let nested = namer.child("parts", true);
        // The nested class starts a new table, so its columns are relative to
        // that table rather than repeating the entity name.
        assert_eq!(nested.build_table_name(""), "tbl_item_parts_values");
        assert_eq!(nested.build_column_name("sm", "mass"), "sm_mass");
        // A property of the entity itself stays on the entity's table.
        assert_eq!(namer.build_column_name("sm", "mass"), "sm_mass");
    }

    #[test]
    fn the_sequence_falls_back_to_the_default_names() {
        let namer = Namer::for_entity("unit");
        // A sequence opens the default "values" table, and an empty property
        // name resolves to that default table name as its suffix.
        let seq = namer.child("", true);
        assert_eq!(seq.build_table_name(""), "tbl_unit_values_values");
        // ...whose scalar column carries the default column name.
        assert_eq!(seq.build_column_name("sm", ""), "sm_value");
        // A nested property of that table stays relative to the sequence table.
        assert_eq!(seq.build_column_name("sm", "state"), "sm_state");
        // A property-less, non-table child instead appends the default column
        // name to the path, the way the reference pushes it into `names_`.
        assert_eq!(
            namer.child("", false).build_column_name("sm", ""),
            "sm_value_value"
        );
    }

    #[test]
    fn column_names_restart_at_each_table_level() {
        let namer = Namer::for_entity("shop");
        let inventory = namer.child("inventory", true);
        let slot = inventory.child("slot", true);
        // Two tables deep: the column names only span the innermost one.
        assert_eq!(slot.build_column_name("sm", "count"), "sm_count");
        // ...while the table name still carries the whole path.
        assert_eq!(
            inventory.build_table_name("slot"),
            "tbl_shop_inventory_slot"
        );
        // A plain property adds no table level, so it keeps its own name in
        // the path — the reference appends it to `names_` either way.
        let mass = inventory.child("mass", false);
        assert_eq!(mass.build_column_name("sm", "unit"), "sm_mass_unit");
    }

    #[test]
    fn a_prefixed_column_keeps_its_prefix() {
        let namer = Namer::for_entity("unit");
        let nested = namer.child("parts", true);
        assert_eq!(nested.build_column_name(ID_COLUMN_NAME, "mass"), "id_mass");
    }

    #[test]
    fn the_table_name_prefix_is_configurable() {
        let namer = Namer::new("unit", "legacy");
        assert_eq!(namer.entity_table_name(), "legacy_unit");
        assert_eq!(namer.build_table_name("props"), "legacy_unit_props");
    }

    #[test]
    fn the_index_type_follows_the_definition() {
        assert_eq!(column_index_type(Indexing::Unique), ColumnIndexType::Unique);
        assert_eq!(
            column_index_type(Indexing::NonUnique),
            ColumnIndexType::NonUnique
        );
        assert_eq!(column_index_type(Indexing::None), ColumnIndexType::None);
        assert_eq!(Indexing::default(), Indexing::None);
    }

    #[test]
    fn the_reserved_names_match_the_reference() {
        assert_eq!(TABLE_NAME_PREFIX, "tbl");
        assert_eq!(DEFAULT_SEQUENCE_COLUMN_NAME, "value");
        assert_eq!(DEFAULT_SEQUENCE_TABLE_NAME, "values");
        assert_eq!(ID_COLUMN_NAME, "id");
        assert_eq!(PARENTID_COLUMN_NAME, "parentID");
        assert_eq!(GAME_TIME_COLUMN_NAME, "gameTime");
        assert_eq!(TIMESTAMP_COLUMN_NAME, "timestamp");
        assert_eq!(COMPONENT_NAME_SEPARATOR, '.');
    }
}
