//! Entity table schemas: naming, column types and DDL
//! (`lib/db_storage_mysql/entity_type_mapping.cpp`, ported to PostgreSQL).
//!
//! The reference maps every entity type to one table and every persistent
//! property to one column: `EntityTypeMapping::createTable` builds
//! `tbl_<TypeName>` (`TABLE_NAME_PREFIX "tbl"`) with one column per
//! persistent property, named by the namer's `buildColumnName("sm",
//! propName)`. This module carries that mapping for PostgreSQL: the same
//! table and column names, quoted (PostgreSQL folds unquoted identifiers
//! to lower case, so the reference's mixed-case names need double quotes),
//! and one PostgreSQL column type per property kind. The fixed system
//! tables (`bigworldInfo`, …) join with the database-initialisation item
//! (BW-09-07).

use db::DbError;
use entitydef::value::PropertyValue;

/// The entity table name prefix (`TABLE_NAME_PREFIX`,
/// `entity_type_mapping.cpp`).
pub const TABLE_NAME_PREFIX: &str = "tbl";

/// The persistent-property column prefix (`buildColumnName("sm", name)`).
pub const COLUMN_NAME_PREFIX: &str = "sm";

/// The id column every entity table carries: the storage-assigned DBID.
pub const ID_COLUMN: &str = "id";

/// Quotes a PostgreSQL identifier: double quotes, inner quotes doubled
/// (the reference's mixed-case table names fold to lower case unquoted).
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `Namer::correctName`: a dot in a member name separates nested
/// components, so the stored name keeps both with a double underscore.
fn correct_name(name: &str) -> String {
    name.replace('.', "__")
}

/// The entity table name for `type_name`: `tbl_<TypeName>` — the prefix
/// and the name joined with the underscore `Namer::buildName` adds.
pub fn entity_table_name(type_name: &str) -> String {
    format!("{TABLE_NAME_PREFIX}_{}", correct_name(type_name))
}

/// The column name for a persistent property: `sm_<propName>`
/// (`Namer::buildColumnName("sm", name)`).
pub fn column_name(prop_name: &str) -> String {
    format!("{COLUMN_NAME_PREFIX}_{}", correct_name(prop_name))
}

/// The column kind a property value maps to. One kind per
/// [`PropertyValue`] shape: the integer kinds share PostgreSQL's `BIGINT`
/// because it carries both the reference's `INT64` and `UINT64` ranges,
/// and every nested shape (sequences, fixed dicts, mailboxes) shares one
/// `JSONB` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// `Int` — `BIGINT`.
    Int,
    /// `Uint` — `BIGINT` (the value must fit; PostgreSQL has no unsigned).
    Uint,
    /// `Float` — `DOUBLE PRECISION`.
    Float,
    /// `String` — `TEXT` (Unicode).
    String,
    /// `Blob` — `BYTEA`.
    Blob,
    /// `Vector` — `REAL[]` (one component per element).
    Vector,
    /// `Sequence`, `FixedDict` and `MailBox` — `JSONB` (the tagged codec
    /// in [`crate::json`]).
    Nested,
}

impl ColumnKind {
    /// The kind of a value's column.
    pub fn of_value(value: &PropertyValue) -> Self {
        match value {
            PropertyValue::Int(_) => ColumnKind::Int,
            PropertyValue::Uint(_) => ColumnKind::Uint,
            PropertyValue::Float(_) => ColumnKind::Float,
            PropertyValue::String(_) => ColumnKind::String,
            PropertyValue::Blob(_) => ColumnKind::Blob,
            PropertyValue::Vector(_) => ColumnKind::Vector,
            PropertyValue::Sequence(_)
            | PropertyValue::FixedDict(_)
            | PropertyValue::MailBox { .. } => ColumnKind::Nested,
        }
    }

    /// The PostgreSQL column type.
    pub fn sql_type(self) -> &'static str {
        match self {
            ColumnKind::Int | ColumnKind::Uint => "BIGINT",
            ColumnKind::Float => "DOUBLE PRECISION",
            ColumnKind::String => "TEXT",
            ColumnKind::Blob => "BYTEA",
            ColumnKind::Vector => "REAL[]",
            ColumnKind::Nested => "JSONB",
        }
    }
}

/// One entity type's table shape: the table's entity type name plus its
/// persistent property columns, in the storage visit order. The DBApp
/// builds this from the entity definition (the property names in
/// declaration order, exactly the order the record's values carry) the
/// way the reference's `EntityTypeMapping` builds its columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitySchema {
    /// The entity type name: the table is `tbl_<type_name>`.
    pub type_name: String,
    /// The persistent property columns: `(name, kind)` in visit order.
    pub columns: Vec<(String, ColumnKind)>,
}

impl EntitySchema {
    /// Builds the schema for a record: zips the property names with the
    /// record's values, deriving each column's kind from its value. The
    /// names and the values must list the same properties in the same
    /// order — the record's values are the definition's visit order.
    pub fn for_record(
        type_name: &str,
        prop_names: &[&str],
        values: &[PropertyValue],
    ) -> Result<Self, DbError> {
        if prop_names.len() != values.len() {
            return Err(DbError::Storage(format!(
                "record for {type_name} carries {} values but the definition lists {} properties",
                values.len(),
                prop_names.len()
            )));
        }
        Ok(Self {
            type_name: type_name.to_string(),
            columns: prop_names
                .iter()
                .zip(values)
                .map(|(name, value)| ((*name).to_string(), ColumnKind::of_value(value)))
                .collect(),
        })
    }

    /// The quoted table name: `"tbl_<TypeName>"`.
    pub fn quoted_table(&self) -> String {
        quote_ident(&entity_table_name(&self.type_name))
    }

    /// The quoted names of the persistent property columns, in visit
    /// order: one `sm_<prop>` per property.
    pub fn quoted_property_columns(&self) -> Vec<String> {
        self.columns
            .iter()
            .map(|(prop, _)| quote_ident(&column_name(prop)))
            .collect()
    }

    /// The quoted names of all columns: the id column first, then the
    /// property columns, in visit order.
    pub fn quoted_columns(&self) -> Vec<String> {
        let mut names = vec![quote_ident(ID_COLUMN)];
        names.extend(self.quoted_property_columns());
        names
    }

    /// The `CREATE TABLE` statement for this schema: one column per
    /// persistent property, every column `NOT NULL` (a record always
    /// carries one value per property — an unset fixed dict is the value
    /// `None`, stored as JSON `null`, not a missing column), and the DBID
    /// as the primary key.
    ///
    /// The DBID column is the table's identity column — PostgreSQL's
    /// spelling of the reference's `AUTO_INCREMENT` key column
    /// (`addAutoIncrement` in `column_type.cpp`) — so the table hands out
    /// a DatabaseID for every row that leaves the column out: nothing
    /// repeats and nothing restarts.
    pub fn create_table_ddl(&self) -> String {
        let mut columns = vec![format!(
            "{} BIGINT NOT NULL GENERATED ALWAYS AS IDENTITY",
            quote_ident(ID_COLUMN)
        )];
        columns.extend(self.columns.iter().map(|(prop, kind)| {
            format!(
                "{} {} NOT NULL",
                quote_ident(&column_name(prop)),
                kind.sql_type()
            )
        }));
        columns.push(format!("PRIMARY KEY ({})", quote_ident(ID_COLUMN)));
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n    {}\n)",
            self.quoted_table(),
            columns.join(",\n    ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_reference_namer() {
        // TABLE_NAME_PREFIX "tbl", buildColumnName("sm", name).
        assert_eq!(entity_table_name("Account"), "tbl_Account");
        assert_eq!(entity_table_name("Monster"), "tbl_Monster");
        assert_eq!(column_name("hitCount"), "sm_hitCount");
        assert_eq!(column_name("pos"), "sm_pos");
    }

    #[test]
    fn quoting_doubles_inner_quotes() {
        assert_eq!(quote_ident("tbl_Account"), "\"tbl_Account\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn column_types_map_one_kind_per_value_shape() {
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::Int(-5)).sql_type(),
            "BIGINT"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::Uint(u64::MAX)).sql_type(),
            "BIGINT"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::Float(1.5)).sql_type(),
            "DOUBLE PRECISION"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::String("x".into())).sql_type(),
            "TEXT"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::Blob(vec![0])).sql_type(),
            "BYTEA"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::Vector(vec![1.0])).sql_type(),
            "REAL[]"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::Sequence(vec![])).sql_type(),
            "JSONB"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::FixedDict(None)).sql_type(),
            "JSONB"
        );
        assert_eq!(
            ColumnKind::of_value(&PropertyValue::MailBox {
                id: 1,
                ip: 0,
                port: 0,
                salt: 0
            })
            .sql_type(),
            "JSONB"
        );
    }

    #[test]
    fn for_record_derives_kinds_from_the_values() {
        let values = vec![
            PropertyValue::String("cui".into()),
            PropertyValue::Int(-7),
            PropertyValue::Vector(vec![1.0, 2.0, 3.0]),
        ];
        let schema =
            EntitySchema::for_record("Account", &["name", "level", "pos"], &values).unwrap();
        assert_eq!(schema.type_name, "Account");
        assert_eq!(
            schema.columns,
            vec![
                ("name".to_string(), ColumnKind::String),
                ("level".to_string(), ColumnKind::Int),
                ("pos".to_string(), ColumnKind::Vector),
            ]
        );
        assert_eq!(schema.quoted_table(), "\"tbl_Account\"");
        assert_eq!(
            schema.quoted_columns(),
            vec!["\"id\"", "\"sm_name\"", "\"sm_level\"", "\"sm_pos\""]
        );
    }

    #[test]
    fn for_record_rejects_a_values_list_that_misses_properties() {
        let values = vec![PropertyValue::Int(1)];
        let err = EntitySchema::for_record("Account", &["a", "b"], &values).unwrap_err();
        assert!(err.to_string().contains("carries 1 values"));
    }

    #[test]
    fn create_table_ddl_lists_every_column_not_null_with_the_id_key() {
        let values = vec![
            PropertyValue::String("cui".into()),
            PropertyValue::Sequence(vec![PropertyValue::Int(2)]),
        ];
        let schema = EntitySchema::for_record("Monster", &["name", "loot"], &values).unwrap();
        let ddl = schema.create_table_ddl();
        assert!(ddl.starts_with("CREATE TABLE IF NOT EXISTS \"tbl_Monster\" ("));
        assert!(ddl.contains("\"id\" BIGINT NOT NULL"));
        assert!(ddl.contains("\"sm_name\" TEXT NOT NULL"));
        assert!(ddl.contains("\"sm_loot\" JSONB NOT NULL"));
        assert!(ddl.contains("PRIMARY KEY (\"id\")"));
        // PostgreSQL's spelling of the reference's `AUTO_INCREMENT` key
        // column: the table allocates the DBID for a row that leaves the
        // column out.
        assert!(ddl.contains("GENERATED ALWAYS AS IDENTITY"));
    }

    #[test]
    fn quoted_property_columns_lists_only_the_properties() {
        let values = vec![PropertyValue::Int(1), PropertyValue::String("a".into())];
        let schema = EntitySchema::for_record("Account", &["level", "name"], &values).unwrap();
        assert_eq!(
            schema.quoted_property_columns(),
            vec!["\"sm_level\"", "\"sm_name\""]
        );
        assert_eq!(
            schema.quoted_columns(),
            vec!["\"id\"", "\"sm_level\"", "\"sm_name\""]
        );
    }
}
