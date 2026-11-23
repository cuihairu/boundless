//! The entity-table schema: `tbl_<Entity>` tables, their columns and
//! indexes, exactly as the old MySQL schema defines them (BW-09-02-01).
//!
//! The reference builds each entity type's mapping from its def
//! (`PropertyMappingsPerType::initPropertyMappings`,
//! `property_mappings_per_type.cpp:139-174`): a [`Namer`] rooted at the
//! entity name (`namer.cpp:37-43`), one column per persistent property,
//! and the fixed columns every entity table carries — `id`
//! (`entity_mapping.cpp:62-66`), and for cell entities the
//! `position`/`direction`/`spaceID` columns plus `gameTime` and
//! `timestamp` (`property_mappings_per_type.cpp:148-173`). A non-blobbed
//! array becomes its own sub-table keyed by `parentID`
//! (`sequence_mapping.cpp:33-42`, `:452-460`).
//!
//! The DDL statements are the ones `TableInitialiser` runs on a fresh
//! database: the `CREATE TABLE` with the temporary auto-increment `id`
//! (`mysql_table_initialiser.cpp:369-372`), one `ALTER TABLE` carrying
//! every `ADD COLUMN` (and the `DROP COLUMN id` when the mapping supplies
//! no id of its own, `mysql_table_initialiser.cpp:375-400`), then the
//! `CREATE [UNIQUE] INDEX` per indexed column
//! (`mysql_table_initialiser.cpp:177-296`). Column definitions render
//! through `ColumnType::getAsString` (`column_type.cpp:130-183`); index
//! names are the column name with its `sm_`-style prefix stripped plus
//! `Index` (`table_meta_data.cpp:22-29`).

use entitydef::data_description::DataDescription;
use entitydef::data_type::{DataType, Kind, TypeContext};
use entitydef::entity_description::EntityDescription;
use entitydef::flags::DatabaseIndexing;
use resources::datasection::DataSection;
use std::collections::HashMap;

/// `TABLE_NAME_PREFIX` (`constants.hpp:38`).
pub const TABLE_NAME_PREFIX: &str = "tbl";
/// `ID_COLUMN_NAME` (`constants.hpp:40`).
pub const ID_COLUMN_NAME: &str = "id";
/// `PARENTID_COLUMN_NAME` (`constants.hpp:41`).
pub const PARENTID_COLUMN_NAME: &str = "parentID";
/// `GAME_TIME_COLUMN_NAME` (`constants.hpp:42`).
pub const GAME_TIME_COLUMN_NAME: &str = "gameTime";
/// `TIMESTAMP_COLUMN_NAME` (`constants.hpp:43`).
pub const TIMESTAMP_COLUMN_NAME: &str = "timestamp";
/// The engine every table is created with (`MYSQL_ENGINE_TYPE`,
/// `wrapper.hpp:32`).
const ENGINE: &str = "InnoDB";
/// `PARENTID_INDEX_NAME` (`table_meta_data.hpp:18`).
const PARENTID_INDEX_NAME: &str = "parentIDIndex";
/// MySQL's indexing length limit for TEXT and BLOB columns
/// (`MAX_TEXT_BLOB_INDEX_LENGTH`, `mysql_table_initialiser.cpp:202`).
const MAX_TEXT_BLOB_INDEX_LENGTH: u32 = 255;

/// `ColumnIndexType` (`column_type.hpp:15-23`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexType {
    /// `INDEX_TYPE_NONE`.
    #[default]
    None,
    /// `INDEX_TYPE_PRIMARY` — the `id` column, created with the table.
    Primary,
    /// `INDEX_TYPE_PARENT_ID` — a sub-table's `parentID` link.
    ParentId,
    /// `INDEX_TYPE_UNIQUE` — a `<Identifier>` property.
    Unique,
    /// `INDEX_TYPE_NON_UNIQUE` — an indexed non-identifier property.
    NonUnique,
}

impl IndexType {
    /// `PropertyMapping::getColumnIndexType`
    /// (`property_mapping.cpp:107-119`): the def's `DatabaseIndexing`
    /// becomes a UNIQUE or NON_UNIQUE column index.
    pub fn from_database_indexing(indexing: DatabaseIndexing) -> Self {
        match indexing {
            DatabaseIndexing::Unique => IndexType::Unique,
            DatabaseIndexing::NonUnique => IndexType::NonUnique,
            DatabaseIndexing::None => IndexType::None,
        }
    }
}

/// The `enum_field_types` a mapping can produce (`column_type.cpp:131-166`).
/// Introspection-only types (DECIMAL, YEAR, ...) are not carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MySqlType {
    /// `MYSQL_TYPE_TINY` — 1-byte integers and the `fm_` none-flags.
    Tiny,
    /// `MYSQL_TYPE_SHORT` — 2-byte integers.
    Short,
    /// `MYSQL_TYPE_LONG` — 4-byte integers (`gameTime`, `spaceID`).
    Long,
    /// `MYSQL_TYPE_LONGLONG` — 8-byte integers (`id`, `parentID`).
    LongLong,
    /// `MYSQL_TYPE_FLOAT`.
    Float,
    /// `MYSQL_TYPE_DOUBLE`.
    Double,
    /// `MYSQL_TYPE_TIMESTAMP` — the `timestamp` column.
    Timestamp,
    /// `MYSQL_TYPE_VAR_STRING` — strings shorter than 256 characters.
    VarString,
    /// `MYSQL_TYPE_STRING` — fixed-length char/binary (`UniqueID`).
    Str,
    /// `MYSQL_TYPE_TINY_BLOB` — string-like values under 256 bytes.
    TinyBlob,
    /// `MYSQL_TYPE_BLOB` — string-like values under 64K.
    Blob,
    /// `MYSQL_TYPE_MEDIUM_BLOB` — string-like values under 16M.
    MediumBlob,
    /// `MYSQL_TYPE_LONG_BLOB` — anything larger.
    LongBlob,
}

impl MySqlType {
    /// Whether the type renders as a variable-length string (`VARCHAR`,
    /// `CHAR`/`BINARY`) — `ColumnType::isStringType` includes the blob
    /// family too (`column_type.cpp:232-248`), which this does not.
    fn is_char_type(self) -> bool {
        matches!(self, MySqlType::VarString | MySqlType::Str)
    }

    /// Whether the type is one of the blob family, the types that accept no
    /// default value (`column_type.cpp:212-229`) and render as
    /// `TEXT`/`BLOB` by the binary flag (`column_type.cpp:79-84`).
    fn is_blob_family(self) -> bool {
        matches!(
            self,
            MySqlType::TinyBlob | MySqlType::Blob | MySqlType::MediumBlob | MySqlType::LongBlob
        )
    }

    /// Whether an index on this type carries a length suffix
    /// (`mysql_table_initialiser.cpp:213-221`).
    fn is_indexable_with_length(self) -> bool {
        self.is_char_type() || self.is_blob_family()
    }
}

/// The type of a column (`ColumnType`, `column_type.hpp:29-96`).
///
/// `is_unsigned_or_binary` is the reference's dual-use field: unsigned for
/// integer types, binary for string-like ones (`column_type.hpp:33`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnType {
    /// The MySQL field type.
    pub field_type: MySqlType,
    /// Unsigned for integers, binary for string-likes.
    pub is_unsigned_or_binary: bool,
    /// Characters for text types, bytes otherwise (`column_type.hpp:34`).
    pub length: u32,
    /// The column's default, rendered raw for numerics and quoted for
    /// strings (`column_type.cpp:53-69`).
    pub default_value: String,
    /// An `ON UPDATE` clause (the `timestamp` column's).
    pub on_update_cmd: String,
    /// Whether the column is `AUTO_INCREMENT` (the `id` column's).
    pub is_auto_increment: bool,
}

impl ColumnType {
    /// A numeric column type (`isUnsignedOrBinary` from the C++ width).
    fn numeric(field_type: MySqlType, is_unsigned: bool) -> Self {
        ColumnType {
            field_type,
            is_unsigned_or_binary: is_unsigned,
            ..Default::default()
        }
    }

    /// A string-like column type (`isBinary` from the C++ dual-use flag).
    fn string_like(field_type: MySqlType, is_binary: bool, length: u32) -> Self {
        ColumnType {
            field_type,
            is_unsigned_or_binary: is_binary,
            length,
            ..Default::default()
        }
    }

    /// `ColumnType::getAsString` (`column_type.cpp:130-183`): the type
    /// keyword, then `AUTO_INCREMENT` or `DEFAULT`, then `ON UPDATE`, then
    /// `PRIMARY KEY` (it must be part of the column spec for a created
    /// table, `column_type.cpp:169-175`), then `NOT NULL`.
    pub fn get_as_string(&self, index_type: IndexType) -> String {
        let mut sql = String::new();
        let unsigned = if self.is_unsigned_or_binary {
            " UNSIGNED"
        } else {
            ""
        };
        match self.field_type {
            MySqlType::Tiny => sql += &format!("TINYINT{unsigned}"),
            MySqlType::Short => sql += &format!("SMALLINT{unsigned}"),
            MySqlType::Long => sql += &format!("INT{unsigned}"),
            MySqlType::LongLong => sql += &format!("BIGINT{unsigned}"),
            MySqlType::Float => sql += "FLOAT",
            MySqlType::Double => sql += "DOUBLE",
            MySqlType::Timestamp => sql += "TIMESTAMP",
            MySqlType::VarString => {
                let keyword = if self.is_unsigned_or_binary {
                    "VARBINARY"
                } else {
                    "VARCHAR"
                };
                sql += &format!("{keyword}({})", self.length);
            }
            MySqlType::Str => {
                let keyword = if self.is_unsigned_or_binary {
                    "BINARY"
                } else {
                    "CHAR"
                };
                sql += &format!("{keyword}({})", self.length);
            }
            MySqlType::TinyBlob => {
                sql += if self.is_unsigned_or_binary {
                    "TINYBLOB"
                } else {
                    "TINYTEXT"
                }
            }
            MySqlType::Blob => {
                sql += if self.is_unsigned_or_binary {
                    "BLOB"
                } else {
                    "TEXT"
                }
            }
            MySqlType::MediumBlob => {
                sql += if self.is_unsigned_or_binary {
                    "MEDIUMBLOB"
                } else {
                    "MEDIUMTEXT"
                }
            }
            MySqlType::LongBlob => {
                sql += if self.is_unsigned_or_binary {
                    "LONGBLOB"
                } else {
                    "LONGTEXT"
                }
            }
        }

        if self.is_auto_increment {
            sql += " AUTO_INCREMENT";
        } else if !self.default_value.is_empty() && self.is_default_value_supported() {
            // addDefaultValue only fires for a non-empty default
            // (`column_type.cpp:53-69`).
            sql += " DEFAULT ";
            if self.is_string_type() {
                sql += &format!("'{}'", escape_sql_string(&self.default_value));
            } else {
                sql += &self.default_value;
            }
        }

        if !self.on_update_cmd.is_empty() {
            sql += " ON UPDATE ";
            sql += &self.on_update_cmd;
        }

        if index_type == IndexType::Primary {
            sql += " PRIMARY KEY";
        }

        sql += " NOT NULL";
        sql
    }

    /// `ColumnType::isDefaultValueSupported` (`column_type.cpp:206-229`):
    /// the blob family takes no default clause.
    fn is_default_value_supported(&self) -> bool {
        !self.field_type.is_blob_family()
    }

    /// `ColumnType::isStringType` (`column_type.cpp:232-248`): defaults on
    /// these types render quoted and escaped.
    fn is_string_type(&self) -> bool {
        self.field_type.is_blob_family() || self.field_type.is_char_type()
    }
}

impl Default for ColumnType {
    /// The reference's default constructor (`column_type.hpp:43-47`) builds
    /// `MYSQL_TYPE_NULL`; with no mapping producing it, plain `LongLong`
    /// (the `id`/`parentID` shape without its flags) stands in so every
    /// field stays explicit.
    fn default() -> Self {
        ColumnType {
            field_type: MySqlType::LongLong,
            is_unsigned_or_binary: false,
            length: 0,
            default_value: String::new(),
            on_update_cmd: String::new(),
            is_auto_increment: false,
        }
    }
}

/// Escape a string literal the way `MySqlEscapedString` does for the
/// characters MySQL treats specially.
fn escape_sql_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\0' => out += "\\0",
            '\n' => out += "\\n",
            '\r' => out += "\\r",
            '\x08' => out += "\\b",
            '\t' => out += "\\t",
            '\x1a' => out += "\\Z",
            '\\' => out += "\\\\",
            '\'' => out += "\\'",
            '"' => out += "\\\"",
            _ => out.push(ch),
        }
    }
    out
}

/// The description of one column (`ColumnDescription`,
/// `column_type.hpp:102-121`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDescription {
    /// The column name.
    pub name: String,
    /// The column type.
    pub col_type: ColumnType,
    /// The index to create on the column.
    pub index_type: IndexType,
}

impl ColumnDescription {
    /// A column with no default and no index.
    fn new(name: &str, col_type: ColumnType, index_type: IndexType) -> Self {
        ColumnDescription {
            name: name.to_string(),
            col_type,
            index_type,
        }
    }
}

/// One table and its columns — a `TableProvider`'s visited column set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDescription {
    /// The table name (`tbl_<Entity>` or a sub-table `tbl_<Entity>_<prop>`).
    pub name: String,
    /// The columns, in mapping order.
    pub columns: Vec<ColumnDescription>,
}

impl TableDescription {
    /// The statement sequence `TableInitialiser` runs for a table that does
    /// not exist yet (`mysql_table_initialiser.cpp:357-405`): the `CREATE
    /// TABLE` with its temporary `id`, one `ALTER TABLE` adding every
    /// column the mapping supplies (dropping the temporary `id` when it
    /// supplies none), then one `CREATE INDEX` per indexed column.
    ///
    /// Columns are visited by column name, the order of the reference's
    /// `NameToColInfoMap` (`table_meta_data.hpp:42`).
    pub fn create_statements(&self) -> Vec<String> {
        let mut statements = vec![format!(
            "CREATE TABLE IF NOT EXISTS {} (id BIGINT AUTO_INCREMENT, \
             PRIMARY KEY idKey (id)) ENGINE={ENGINE}",
            self.name
        )];

        let has_id_column = self.columns.iter().any(|c| c.name == ID_COLUMN_NAME);
        let mut ordered: Vec<&ColumnDescription> = self.columns.iter().collect();
        ordered.sort_by(|a, b| a.name.cmp(&b.name));

        let mut alter = format!("ALTER TABLE {}", self.name);
        let mut altered_items = 0;
        for column in ordered
            .iter()
            .filter(|c| !(has_id_column && c.name == ID_COLUMN_NAME))
        {
            // Each request after the first is comma-separated
            // (`mysql_table_initialiser.cpp:75-84`).
            if altered_items > 0 {
                alter += ",";
            }
            altered_items += 1;
            alter += &format!(
                " ADD COLUMN {} {}",
                column.name,
                column.col_type.get_as_string(column.index_type)
            );
        }
        if !has_id_column {
            // The table was created with a placeholder id no mapping
            // supplied (`mysql_table_initialiser.cpp:375-382`).
            if altered_items > 0 {
                alter += ",";
            }
            alter += " DROP COLUMN id";
        }
        statements.push(alter);

        for column in &ordered {
            if let Some(sql) = create_index_statement(&self.name, column) {
                statements.push(sql);
            }
        }
        statements
    }
}

/// `generateIndexName` (`table_meta_data.cpp:22-29`): everything up to and
/// including the first underscore stripped, then `Index`.
fn generate_index_name(column_name: &str) -> String {
    match column_name.find('_') {
        Some(pos) => format!("{}Index", &column_name[pos + 1..]),
        None => format!("{column_name}Index"),
    }
}

/// `TableInitialiser::createIndex` (`mysql_table_initialiser.cpp:247-296`)
/// and its `generateCreateIndexStatement` (`:177-219`).
fn create_index_statement(table_name: &str, column: &ColumnDescription) -> Option<String> {
    let column_name = &column.name;
    match column.index_type {
        IndexType::None | IndexType::Primary => None,
        IndexType::ParentId => Some(format!(
            "CREATE INDEX {PARENTID_INDEX_NAME} ON {table_name} ({column_name})"
        )),
        IndexType::Unique | IndexType::NonUnique => {
            let keyword = if column.index_type == IndexType::Unique {
                "CREATE UNIQUE INDEX"
            } else {
                "CREATE INDEX"
            };
            // All indexes on properties that are not VARCHAR fields are
            // indexed on a maximum of the first 255 characters
            // (`mysql_table_initialiser.cpp:195-211`).
            let mut index_length = column.col_type.length;
            if column.col_type.field_type != MySqlType::VarString
                && index_length > MAX_TEXT_BLOB_INDEX_LENGTH
            {
                index_length = MAX_TEXT_BLOB_INDEX_LENGTH;
            }
            let length_suffix = if column.col_type.field_type.is_indexable_with_length() {
                format!("({index_length})")
            } else {
                String::new()
            };
            Some(format!(
                "{keyword} {} ON {table_name} ({column_name}{length_suffix})",
                generate_index_name(column_name)
            ))
        }
    }
}

/// Column names and table names (`Namer`, `namer.hpp:18-45`).
///
/// Table names are fully qualified (`tbl_<Entity>_<prop>`); column names
/// are relative to the current table, so a root-level property on
/// `tbl_Avatar` is `sm_<prop>` with no entity name in it (`namer.cpp:75-96`).
#[derive(Debug, Clone)]
pub struct Namer {
    /// `tableNamePrefix_`.
    table_name_prefix: String,
    /// `names_`: the entity name plus each nested property name.
    names: Vec<String>,
    /// `tableLevels_`: indices into `names` where each sub-table starts.
    table_levels: Vec<usize>,
}

/// `Namer::correctName` (`namer.cpp:19-32`): the component-name separator
/// (a dot) becomes a double underscore.
fn correct_name(name: &str) -> String {
    name.replace('.', "__")
}

impl Namer {
    /// The root namer of an entity mapping
    /// (`Namer( entityName, TABLE_NAME_PREFIX )`).
    pub fn new(entity_name: &str) -> Self {
        Namer {
            table_name_prefix: TABLE_NAME_PREFIX.to_string(),
            names: vec![entity_name.to_string()],
            table_levels: vec![1],
        }
    }

    /// The second constructor (`namer.cpp:49-69`): descend into a property;
    /// an empty property name (a sequence's element) adds the
    /// `value`/`values` placeholder level instead. Sub-tables record their
    /// name level so columns inside them stay relative.
    fn child(&self, prop_name: &str, is_table: bool) -> Namer {
        let mut names = self.names.clone();
        let mut table_levels = self.table_levels.clone();
        names.push(if prop_name.is_empty() {
            if is_table {
                "values".to_string()
            } else {
                "value".to_string()
            }
        } else {
            correct_name(prop_name)
        });
        if is_table {
            table_levels.push(names.len());
        }
        Namer {
            table_name_prefix: self.table_name_prefix.clone(),
            names,
            table_levels,
        }
    }

    /// `Namer::buildColumnName` (`namer.cpp:75-83`): `prefix`, then the
    /// property path after the current table level, then the property name.
    pub fn build_column_name(&self, prefix: &str, prop_name: &str) -> String {
        let suffix = if prop_name.is_empty() {
            "value".to_string()
        } else {
            correct_name(prop_name)
        };
        self.build_name(
            prefix,
            &suffix,
            *self.table_levels.last().expect("root level"),
        )
    }

    /// `Namer::buildTableName` (`namer.cpp:89-96`): the prefix, entity name,
    /// and property path — always fully qualified.
    pub fn build_table_name(&self, prop_name: &str) -> String {
        let suffix = if prop_name.is_empty() {
            "values".to_string()
        } else {
            correct_name(prop_name)
        };
        self.build_name(&self.table_name_prefix, &suffix, 0)
    }

    /// `Namer::buildName` (`namer.cpp:102-121`).
    fn build_name(&self, prefix: &str, suffix: &str, start: usize) -> String {
        let mut name = prefix.to_string();
        for part in &self.names[start.min(self.names.len())..] {
            name += "_";
            name += part;
        }
        if !suffix.is_empty() {
            name += "_";
            name += suffix;
        }
        name
    }
}

/// What one property mapping contributes: columns on the current table, a
/// sub-table of its own, or a composite's mix of both
/// (`CompositePropertyMapping`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mapped {
    Columns(Vec<ColumnDescription>),
    SubTable(TableDescription),
    /// `UserTypeMapping`'s bound children: columns and sub-tables at once.
    Composite(Vec<ColumnDescription>, Vec<TableDescription>),
}

/// The schema of one entity type: its table, any array sub-tables, and the
/// names of persistent properties no mapping could take
/// (`PropertyMapping::create`'s "cannot be persistent" error,
/// `property_mapping.cpp:340-349`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitySchema {
    /// The entity's own table, `tbl_<EntityName>`.
    pub table: TableDescription,
    /// Sub-tables for non-blobbed array properties, in def order.
    pub sub_tables: Vec<TableDescription>,
    /// Persistent properties the reference refuses to persist, by full
    /// name: mailbox and user types, indexed compound types.
    pub unmapped: Vec<String>,
}

/// One `bind()` call of a user type's binding
/// (`PyUserTypeBinder::bind`, `py_user_type_binder.cpp:44-77`): an
/// attribute bound to a BigWorld type name, with the bind's database
/// length (the reference default is 255).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundAttribute {
    /// The attribute name, the bind's `propName`.
    pub name: String,
    /// The type name, the bind's `typeName` — plain (`INT32`) or compound
    /// (`ARRAY <of> INT32 </of>`), the text of the constructed
    /// `<Type>` section (`py_user_type_binder.cpp:56-60`).
    pub type_name: String,
    /// The bind's `databaseLength`.
    pub database_length: i32,
}

/// The binding one user type registers through `bindSectionToDB`
/// (`user_type_mapping.cpp:106-145`): the `bind()` calls the script
/// makes, in order. `beginTable`/`endTable` groups are not carried — a
/// user type binding maps attributes as columns on the owner's table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserTypeBinding {
    /// The bound attributes, in bind order.
    pub attributes: Vec<BoundAttribute>,
}

/// The bindings of all user types, keyed by the `<implementedBy>`
/// `module.instance` pair (`Kind::UserType`).
pub type UserTypeBindings = HashMap<(String, String), UserTypeBinding>;

/// The name a property maps under — `DataDescription::fullName`
/// (`data_description.ipp:325-336`): `componentName.name` for a
/// componentised property, the plain name otherwise.
fn mapping_name(prop: &DataDescription) -> String {
    if prop.component_name.is_empty() {
        prop.name().to_string()
    } else {
        format!("{}.{}", prop.component_name, prop.name())
    }
}

/// Build the schema of one entity type — the Rust counterpart of
/// `PropertyMappingsPerType::initPropertyMappings`
/// (`property_mappings_per_type.cpp:139-174`) plus `EntityMapping`'s fixed
/// columns. User types stay unmapped; see
/// [`entity_schema_with_user_types`].
pub fn entity_schema(entity: &EntityDescription) -> EntitySchema {
    entity_schema_with_user_types(entity, &HashMap::new())
}

/// [`entity_schema`] with a user type binding registry: a `USER_TYPE`
/// property whose `<implementedBy>` module/instance has a binding maps
/// like the reference's `UserTypeMapping` — a composite of the bound
/// attributes (`user_type_mapping.cpp:106-145` calls the script's
/// `bindSectionToDB`, whose `bind()` calls declare the children,
/// `py_user_type_binder.cpp:44-77`). Without a binding it stays
/// unmapped, the reference's failed script import.
pub fn entity_schema_with_user_types(
    entity: &EntityDescription,
    bindings: &UserTypeBindings,
) -> EntitySchema {
    let namer = Namer::new(&entity.name);
    let table_name = format!("{TABLE_NAME_PREFIX}_{}", entity.name);

    let mut columns = vec![id_column()];
    let mut sub_tables = Vec::new();
    let mut unmapped = Vec::new();

    // The visit is BASE|CELL data with ONLY_PERSISTENT_DATA — every
    // persistent property (base and cell are the only distributions).
    for prop in entity
        .properties()
        .iter()
        .filter(|p| p.data_flags.is_persistent())
    {
        let name = mapping_name(prop);
        // getDefaultSection: the property's default section, else the
        // type's own (`property_mappings_per_type.cpp:31-41`). A const
        // type's property-level default lives in the initial value, not
        // here — the reference reads the same two slots.
        let default = prop
            .default_section
            .as_ref()
            .or_else(|| prop.data_type.default_section());
        match map_property(
            &namer,
            &name,
            &prop.data_type,
            prop.database_length,
            prop.database_indexing,
            default,
            bindings,
        ) {
            Some(Mapped::Columns(cols)) => columns.extend(cols),
            Some(Mapped::SubTable(table)) => sub_tables.push(table),
            Some(Mapped::Composite(cols, tables)) => {
                columns.extend(cols);
                sub_tables.extend(tables);
            }
            None => unmapped.push(name),
        }
    }

    if entity.can_be_on_cell() {
        // Cell entities carry the position, direction, spaceID, gameTime and
        // timestamp columns (`property_mappings_per_type.cpp:148-173`).
        columns.extend(vector_columns(&namer, "position", &zero_defaults(0.0, 3)));
        columns.extend(vector_columns(&namer, "direction", &zero_defaults(0.0, 3)));
        let mut space_id = ColumnType::numeric(MySqlType::Long, false);
        space_id.default_value = "0".to_string();
        columns.push(ColumnDescription::new(
            &namer.build_column_name("sm", "spaceID"),
            space_id,
            IndexType::None,
        ));
    }

    let mut game_time = ColumnType::numeric(MySqlType::Long, true);
    game_time.default_value = "0".to_string();
    columns.push(ColumnDescription::new(
        &namer.build_column_name("sm", GAME_TIME_COLUMN_NAME),
        game_time,
        IndexType::None,
    ));

    let mut timestamp = ColumnType {
        field_type: MySqlType::Timestamp,
        ..Default::default()
    };
    timestamp.default_value = "CURRENT_TIMESTAMP".to_string();
    timestamp.on_update_cmd = "CURRENT_TIMESTAMP".to_string();
    columns.push(ColumnDescription::new(
        &namer.build_column_name("sm", TIMESTAMP_COLUMN_NAME),
        timestamp,
        IndexType::None,
    ));

    EntitySchema {
        table: TableDescription {
            name: table_name,
            columns,
        },
        sub_tables,
        unmapped,
    }
}

/// The `id` column (`EntityMapping::visitIDColumnWith`,
/// `entity_mapping.cpp:62-66`; `ID_COLUMN_TYPE` is auto-increment,
/// `column_type.hpp:145-146`).
fn id_column() -> ColumnDescription {
    ColumnDescription {
        name: ID_COLUMN_NAME.to_string(),
        col_type: ColumnType {
            field_type: MySqlType::LongLong,
            is_auto_increment: true,
            ..Default::default()
        },
        index_type: IndexType::Primary,
    }
}

/// A sub-table's `parentID` column (`SequenceMapping::visitColumnsWith`,
/// `sequence_mapping.cpp:452-460`; `PARENTID_COLUMN_TYPE`,
/// `column_type.hpp:144`).
fn parent_id_column() -> ColumnDescription {
    ColumnDescription::new(
        PARENTID_COLUMN_NAME,
        ColumnType::numeric(MySqlType::LongLong, false),
        IndexType::ParentId,
    )
}

/// `vm_%i_<path>` float columns, one per vector component
/// (`VectorMapping::visitParentColumns`, `vector_mapping.hpp:62-82`).
fn vector_columns(namer: &Namer, prop_name: &str, defaults: &[String]) -> Vec<ColumnDescription> {
    let template = namer.build_column_name("vm_%i", prop_name);
    (0..3)
        .map(|i| {
            ColumnDescription::new(
                &template.replace("%i", &i.to_string()),
                ColumnType::numeric(MySqlType::Float, false).with_default(defaults[i].clone()),
                IndexType::None,
            )
        })
        .collect()
}

fn zero_defaults(value: f32, count: usize) -> Vec<String> {
    vec![format!("{value}"); count]
}

impl ColumnType {
    /// Set the default value, builder-style.
    fn with_default(mut self, value: String) -> Self {
        self.default_value = value;
        self
    }
}

/// The `fm_<path>` none-flag of an `AllowNone` class or fixed dict
/// (`ClassMapping::visitParentColumns`, `class_mapping.cpp:97-111`).
fn none_flag_column(namer: &Namer, prop_name: &str) -> ColumnDescription {
    let mut col_type = ColumnType::numeric(MySqlType::Tiny, true);
    col_type.default_value = "1".to_string();
    ColumnDescription::new(
        &namer.build_column_name("fm", prop_name),
        col_type,
        IndexType::None,
    )
}

/// The blob-family type for a string-like value of `char_length` bytes
/// (`MySqlTypeTraits<BW::string>::colType`, `type_traits.hpp:47-66`).
fn blob_ladder(char_length: u32) -> MySqlType {
    if char_length < 1 << 8 {
        MySqlType::TinyBlob
    } else if char_length < 1 << 16 {
        MySqlType::Blob
    } else if char_length < 1 << 24 {
        MySqlType::MediumBlob
    } else {
        MySqlType::LongBlob
    }
}

/// A string-like leaf column: `StringLikeMapping::visitParentColumns`
/// (`string_like_mapping.cpp:79-90`), with `StringMapping`'s and
/// `UnicodeStringMapping`'s short-string special case
/// (`string_mapping.cpp:28-38`, `unicode_string_mapping.cpp:93-101`) —
/// only those two types take the VAR_STRING path; blobs, python and
/// blobbed sequences always use the blob ladder.
fn string_like_column(
    namer: &Namer,
    prop_name: &str,
    is_binary: bool,
    char_length: u32,
    index_type: IndexType,
    default: Option<String>,
    short_strings_are_var: bool,
) -> ColumnDescription {
    let field_type = if short_strings_are_var && char_length < 256 {
        MySqlType::VarString
    } else {
        blob_ladder(char_length)
    };
    let mut col_type = ColumnType::string_like(field_type, is_binary, char_length);
    // StringMapping truncates its default to the character length
    // (`string_mapping.cpp:20-26`).
    if let Some(value) = default {
        let mut bytes = value.into_bytes();
        bytes.truncate(char_length as usize);
        col_type.default_value = String::from_utf8_lossy(&bytes).into_owned();
    }
    ColumnDescription::new(
        &namer.build_column_name("sm", prop_name),
        col_type,
        index_type,
    )
}

/// Map one property to its columns or sub-table — the Rust counterpart of
/// `PropertyMapping::create` (`property_mapping.cpp:129-349`). `None` is
/// the "cannot be persistent" outcome.
fn map_property(
    namer: &Namer,
    prop_name: &str,
    type_: &DataType,
    db_len: i32,
    indexing: DatabaseIndexing,
    default: Option<&DataSection>,
    bindings: &UserTypeBindings,
) -> Option<Mapped> {
    let index_type = IndexType::from_database_indexing(indexing);
    let is_indexed = indexing != DatabaseIndexing::None;

    match type_.kind() {
        // Compound types only map without an index; an indexed one falls
        // through the reference's dispatch to the "cannot be persistent"
        // error (`property_mapping.cpp:144,179,190,197,264-288`).
        Kind::Sequence {
            is_tuple: _,
            size: _,
            db_len: seq_db_len,
            element,
        } if !is_indexed => {
            if *seq_db_len > 0 {
                // persistAsBlob: one binary blob column holding the whole
                // sequence (`BlobbedSequenceMapping`,
                // `blobbed_sequence_mapping.cpp:18-32`).
                let column = string_like_column(
                    namer,
                    prop_name,
                    /*is_binary=*/ true,
                    *seq_db_len as u32,
                    IndexType::None,
                    None,
                    /*short_strings_are_var=*/ false,
                );
                Some(Mapped::Columns(vec![column]))
            } else {
                // A sub-table keyed by parentID; the child's columns are
                // relative to it (`property_mapping.cpp:173-197`,
                // `sequence_mapping.cpp:33-42`).
                let child_namer = namer.child(prop_name, true);
                let child = map_property(
                    &child_namer,
                    "",
                    element,
                    db_len,
                    DatabaseIndexing::None,
                    element.default_section(),
                    bindings,
                )?;
                let mut columns = vec![parent_id_column()];
                let mut nested_tables = Vec::new();
                // A plain sub-table element (an array of arrays) has no
                // column set — the reference's dispatch falls out.
                match child {
                    Mapped::Columns(child_columns) => columns.extend(child_columns),
                    // A composite element (a bound user type) contributes
                    // its columns relative to the sub-table — the binder's
                    // unnamed context pushes no level
                    // (`py_user_type_binder.cpp:17-21`) — and carries its
                    // own sub-tables after this one.
                    Mapped::Composite(child_columns, child_tables) => {
                        columns.extend(child_columns);
                        nested_tables = child_tables;
                    }
                    Mapped::SubTable(_) => return None,
                }
                let element_table = TableDescription {
                    name: namer.build_table_name(prop_name),
                    columns,
                };
                if nested_tables.is_empty() {
                    Some(Mapped::SubTable(element_table))
                } else {
                    Some(Mapped::Composite(
                        Vec::new(),
                        std::iter::once(element_table)
                            .chain(nested_tables)
                            .collect(),
                    ))
                }
            }
        }
        Kind::FixedDict {
            allow_none, fields, ..
        } if !is_indexed => map_class(namer, prop_name, *allow_none, fields, default, bindings),
        Kind::Class { allow_none, fields } if !is_indexed => {
            map_class(namer, prop_name, *allow_none, fields, default, bindings)
        }
        // A USER_TYPE maps only through a registered binding — the
        // reference's script `bindSectionToDB`
        // (`user_type_mapping.cpp:106-145`); a failed import (no binding)
        // or an indexed user type is the "cannot be persistent" error
        // (`property_mapping.cpp:264-288`).
        Kind::UserType { module, instance } if !is_indexed => bindings
            .get(&(module.clone(), instance.clone()))
            .map(|binding| map_user_type(namer, prop_name, binding, default, bindings)),
        Kind::UserType { .. } | Kind::MailBox => None,
        Kind::Int { bytes, signed } => {
            let field_type = match (*bytes, usize::from(!signed)) {
                (1, _) => MySqlType::Tiny,
                (2, _) => MySqlType::Short,
                (4, _) => MySqlType::Long,
                _ => MySqlType::LongLong,
            };
            let mut col_type = ColumnType::numeric(field_type, !signed);
            col_type.default_value = default.map(read_default_int).unwrap_or_else(|| "0".into());
            Some(Mapped::Columns(vec![ColumnDescription::new(
                &namer.build_column_name("sm", prop_name),
                col_type,
                index_type,
            )]))
        }
        Kind::Float32 | Kind::Float64 => {
            let field_type = match type_.kind() {
                Kind::Float64 => MySqlType::Double,
                _ => MySqlType::Float,
            };
            let mut col_type = ColumnType::numeric(field_type, false);
            col_type.default_value = default
                .map(|section| format!("{}", section.as_double(0.0)))
                .unwrap_or_else(|| "0".into());
            Some(Mapped::Columns(vec![ColumnDescription::new(
                &namer.build_column_name("sm", prop_name),
                col_type,
                index_type,
            )]))
        }
        Kind::Vector { elements } if !is_indexed => {
            let defaults = default
                .map(|section| vector_defaults(section, *elements))
                .unwrap_or_else(|| zero_defaults(0.0, 3));
            let template = namer.build_column_name("vm_%i", prop_name);
            let columns = (0..*elements)
                .map(|i| {
                    let mut col_type = ColumnType::numeric(MySqlType::Float, false);
                    col_type.default_value = defaults
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| "0".into());
                    ColumnDescription::new(
                        &template.replace("%i", &i.to_string()),
                        col_type,
                        IndexType::None,
                    )
                })
                .collect();
            Some(Mapped::Columns(columns))
        }
        Kind::String => Some(Mapped::Columns(vec![string_like_column(
            namer,
            prop_name,
            // StringMapping keeps raw bytes: short strings are VARBINARY,
            // long ones BLOB (`string_mapping.hpp:22`,
            // `string_like_mapping.cpp:79-90`).
            /*is_binary=*/
            true,
            db_len as u32,
            index_type,
            default.map(|section| section.as_string().to_string()),
            /*short_strings_are_var=*/ true,
        )])),
        Kind::UnicodeString => Some(Mapped::Columns(vec![string_like_column(
            namer,
            prop_name,
            /*is_binary=*/ false,
            db_len as u32,
            index_type,
            default.map(|section| section.as_string().to_string()),
            /*short_strings_are_var=*/ true,
        )])),
        Kind::Blob => Some(Mapped::Columns(vec![string_like_column(
            namer,
            prop_name,
            /*is_binary=*/ true,
            db_len as u32,
            index_type,
            // The blob family takes no default clause; the section is not
            // read (`string_mapping.cpp:20-26` is the only truncation
            // path and `column_type.cpp:206-229` rejects the clause).
            None,
            /*short_strings_are_var=*/ false,
        )])),
        Kind::Python if !is_indexed => {
            // PythonMapping is a binary string-like of the database length
            // (`python_mapping.cpp:20-24`, `python_mapping.hpp:20`).
            Some(Mapped::Columns(vec![string_like_column(
                namer,
                prop_name,
                /*is_binary=*/ true,
                db_len as u32,
                IndexType::None,
                None,
                /*short_strings_are_var=*/ false,
            )]))
        }
        Kind::UdoRef if !is_indexed => {
            // UDORefMapping extends UniqueIDMapping: a 16-byte binary
            // column (`udo_ref_mapping.hpp:12-16`,
            // `unique_id_mapping.cpp:91-102`). The reference renders the
            // zeroed 16-byte default into the DDL; we omit it.
            Some(Mapped::Columns(vec![ColumnDescription::new(
                &namer.build_column_name("sm", prop_name),
                ColumnType::string_like(MySqlType::Str, true, 16),
                IndexType::None,
            )]))
        }
        _ => None,
    }
}

/// `createClassTypeMapping` (`property_mapping.cpp:68-98`): an optional
/// `fm_` none-flag, then each persistent field's columns; the property path
/// gains a level unless we are directly inside a sequence (empty name).
/// A field no mapping takes is skipped, the way `PropertyMapping::create`'s
/// null result is skipped by the collector (`property_mapping.cpp:90-96`).
fn map_class(
    namer: &Namer,
    prop_name: &str,
    allow_none: bool,
    fields: &[entitydef::data_type::Field],
    default: Option<&DataSection>,
    bindings: &UserTypeBindings,
) -> Option<Mapped> {
    let mut columns = Vec::new();
    if allow_none {
        columns.push(none_flag_column(namer, prop_name));
    }

    let child_namer = namer.child(prop_name, false);
    let field_namer = if prop_name.is_empty() {
        namer
    } else {
        &child_namer
    };

    for field in fields.iter().filter(|f| f.is_persistent) {
        // getChildDefaultSection: the parent default's field section, else
        // the child type's own (`property_mapping.cpp:46-61`).
        let field_default = default
            .and_then(|section| section.find_child(&field.name))
            .or(field.type_.default_section());
        if let Some(Mapped::Columns(field_columns)) = map_property(
            field_namer,
            &field.name,
            &field.type_,
            field.db_len,
            DatabaseIndexing::None,
            field_default,
            bindings,
        ) {
            columns.extend(field_columns);
        }
    }
    Some(Mapped::Columns(columns))
}

/// `UserTypeMapping` — a bound user type maps as the composite of its
/// `bind()` calls (`py_user_type_binder.cpp:16-31` pushes a non-table
/// naming level for the property, then every `bind` creates the child
/// mapping on that namer, `:44-77`): no `fm_` flag, one level for the
/// property, then each attribute's columns on it. Attributes the dispatch
/// cannot take are skipped the way class fields are.
fn map_user_type(
    namer: &Namer,
    prop_name: &str,
    binding: &UserTypeBinding,
    default: Option<&DataSection>,
    bindings: &UserTypeBindings,
) -> Mapped {
    let child_namer = namer.child(prop_name, false);
    let attr_namer = if prop_name.is_empty() {
        namer
    } else {
        &child_namer
    };

    let ctx = TypeContext::default();
    let mut columns = Vec::new();
    let mut sub_tables = Vec::new();
    for attr in &binding.attributes {
        let Some(type_) = build_bound_type(&ctx, &attr.type_name) else {
            continue;
        };
        // The bind's default: the parent default's attribute section
        // (`py_user_type_binder.cpp:50-55`).
        let attr_default = default.and_then(|section| section.find_child(&attr.name));
        match map_property(
            attr_namer,
            &attr.name,
            &type_,
            attr.database_length,
            DatabaseIndexing::None,
            attr_default,
            bindings,
        ) {
            Some(Mapped::Columns(attr_columns)) => columns.extend(attr_columns),
            Some(Mapped::SubTable(table)) => sub_tables.push(table),
            Some(Mapped::Composite(attr_columns, attr_tables)) => {
                columns.extend(attr_columns);
                sub_tables.extend(attr_tables);
            }
            None => {}
        }
    }
    Mapped::Composite(columns, sub_tables)
}

/// Build a bound attribute's type from its name — the binder's
/// `<Type>typeName</Type>` section through `DataType::buildDataType`
/// (`py_user_type_binder.cpp:56-66`).
fn build_bound_type(ctx: &TypeContext, type_name: &str) -> Option<DataType> {
    let xml = format!("<Type>{type_name}</Type>");
    let section = resources::datasection::parse("", xml.as_bytes()).ok()?;
    let mut diag = entitydef::diagnostics::Diagnostics::new();
    entitydef::data_type::build_type(ctx, Some(&section), &mut diag)
}

/// `StringConv::toStr` for a numeric default: integers decimal, floats the
/// short form (`string_conv.hpp:119-159`).
fn read_default_int(section: &DataSection) -> String {
    format!("{}", section.as_long(0))
}

/// The per-component defaults of a vector default section, as
/// `VectorMapping` renders them (`vector_mapping.hpp:18-27`).
fn vector_defaults(section: &DataSection, elements: i32) -> Vec<String> {
    let components: Vec<f32> = match elements {
        2 => {
            let v = section.as_vector2(Default::default());
            vec![v.x, v.y]
        }
        4 => {
            let v = section.as_vector4(Default::default());
            vec![v.x, v.y, v.z, v.w]
        }
        _ => {
            let v = section.as_vector3(Default::default());
            vec![v.x, v.y, v.z]
        }
    };
    components.iter().map(|v| format!("{v}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use entitydef::entity_description::{DefResolver, HasScriptDecider};
    use std::collections::HashMap;

    /// A resolver serving an in-memory res tree, the way entitydef's own
    /// tests do.
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

    /// Parse one entity def from an in-memory tree.
    fn parse_entity(files: &[(&str, &str)]) -> EntityDescription {
        let resolver = MemoryResolver::new(files);
        let mut desc = EntityDescription::new();
        let mut diag = entitydef::diagnostics::Diagnostics::new();
        assert!(
            desc.parse(
                "Avatar",
                "",
                Some(&HasScriptDecider::new(&resolver)),
                &resolver,
                &entitydef::data_type::TypeContext::default(),
                &mut diag
            ),
            "{:?}",
            diag.messages()
        );
        desc
    }

    /// The statements of one table, as a flat list for assertions.
    fn statements(table: &TableDescription) -> Vec<String> {
        table.create_statements()
    }

    /// INT8/UINT8 defaults render as numbers, not characters
    /// (`string_conv.hpp:127-134`).
    #[test]
    fn int_defaults_render_as_numbers() {
        assert_eq!(read_default_int_value(42), "42");
    }

    /// Helper standing in for the int8 stream special case; the formatting
    /// itself is `format!("{}", i64)`.
    fn read_default_int_value(value: i64) -> String {
        format!("{value}")
    }

    /// `generateIndexName` strips through the first underscore
    /// (`table_meta_data.cpp:22-29`).
    #[test]
    fn the_index_name_strips_the_prefix() {
        assert_eq!(generate_index_name("sm_name"), "nameIndex");
        assert_eq!(generate_index_name("sm_bag__items"), "bag__itemsIndex");
        assert_eq!(generate_index_name("gameTime"), "gameTimeIndex");
    }

    /// `Namer` puts the entity name into table names but not into root
    /// column names (`namer.cpp:75-96`), and corrects dotted names
    /// (`namer.cpp:19-32`).
    #[test]
    fn the_namer_qualsifies_tables_and_keeps_columns_relative() {
        let namer = Namer::new("Avatar");
        assert_eq!(namer.build_table_name("inventory"), "tbl_Avatar_inventory");
        assert_eq!(namer.build_column_name("sm", "hp"), "sm_hp");
        assert_eq!(namer.build_column_name("sm", "iface.hp"), "sm_iface__hp");

        // A nested class adds a level to the column path.
        let child = namer.child("stats", false);
        assert_eq!(child.build_column_name("sm", "hp"), "sm_stats_hp");

        // A sequence sub-table's columns are relative to the sub-table.
        let table = namer.child("inventory", true);
        assert_eq!(table.build_table_name(""), "tbl_Avatar_inventory_values");
        assert_eq!(table.build_column_name("sm", ""), "sm_value");
        assert_eq!(table.build_column_name("sm", "count"), "sm_count");
    }

    /// `ColumnType::getAsString` renders each mapping-produced type the way
    /// `column_type.cpp:130-183` does.
    #[test]
    fn the_column_types_render_as_the_reference_ddl() {
        let renders = |col_type: ColumnType, index: IndexType| col_type.get_as_string(index);

        // The id column: BIGINT AUTO_INCREMENT PRIMARY KEY NOT NULL.
        let id = ColumnType {
            field_type: MySqlType::LongLong,
            is_auto_increment: true,
            ..Default::default()
        };
        assert_eq!(
            renders(id, IndexType::Primary),
            "BIGINT AUTO_INCREMENT PRIMARY KEY NOT NULL"
        );

        // parentID: plain BIGINT.
        assert_eq!(
            renders(
                ColumnType::numeric(MySqlType::LongLong, false),
                IndexType::ParentId
            ),
            "BIGINT NOT NULL"
        );

        // Integer widths and unsignedness.
        assert_eq!(
            renders(ColumnType::numeric(MySqlType::Tiny, false), IndexType::None),
            "TINYINT NOT NULL"
        );
        assert_eq!(
            renders(ColumnType::numeric(MySqlType::Tiny, true), IndexType::None),
            "TINYINT UNSIGNED NOT NULL"
        );
        assert_eq!(
            renders(ColumnType::numeric(MySqlType::Short, true), IndexType::None),
            "SMALLINT UNSIGNED NOT NULL"
        );
        assert_eq!(
            renders(ColumnType::numeric(MySqlType::Long, false), IndexType::None),
            "INT NOT NULL"
        );
        assert_eq!(
            renders(ColumnType::numeric(MySqlType::Long, true), IndexType::None),
            "INT UNSIGNED NOT NULL"
        );

        // Numeric defaults render raw.
        let mut flag = ColumnType::numeric(MySqlType::Tiny, true);
        flag.default_value = "1".into();
        assert_eq!(
            renders(flag, IndexType::None),
            "TINYINT UNSIGNED DEFAULT 1 NOT NULL"
        );

        // Floats and doubles.
        assert_eq!(
            renders(
                ColumnType::numeric(MySqlType::Float, false),
                IndexType::None
            ),
            "FLOAT NOT NULL"
        );
        assert_eq!(
            renders(
                ColumnType::numeric(MySqlType::Double, false),
                IndexType::None
            ),
            "DOUBLE NOT NULL"
        );

        // Short STRING: VARBINARY (raw bytes, `string_mapping.hpp:22`).
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::VarString, true, 255),
                IndexType::None
            ),
            "VARBINARY(255) NOT NULL"
        );

        // Short UNICODE_STRING: VARCHAR.
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::VarString, false, 128),
                IndexType::None
            ),
            "VARCHAR(128) NOT NULL"
        );

        // Fixed BINARY(16) for unique ids.
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::Str, true, 16),
                IndexType::None
            ),
            "BINARY(16) NOT NULL"
        );

        // The blob family, binary for strings and blobs, text for unicode.
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::TinyBlob, true, 100),
                IndexType::None
            ),
            "TINYBLOB NOT NULL"
        );
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::Blob, true, 1000),
                IndexType::None
            ),
            "BLOB NOT NULL"
        );
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::MediumBlob, true, 70000),
                IndexType::None
            ),
            "MEDIUMBLOB NOT NULL"
        );
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::LongBlob, true, 1 << 24),
                IndexType::None
            ),
            "LONGBLOB NOT NULL"
        );
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::Blob, false, 1000),
                IndexType::None
            ),
            "TEXT NOT NULL"
        );

        // The timestamp column carries DEFAULT and ON UPDATE.
        let mut timestamp = ColumnType {
            field_type: MySqlType::Timestamp,
            ..Default::default()
        };
        timestamp.default_value = "CURRENT_TIMESTAMP".into();
        timestamp.on_update_cmd = "CURRENT_TIMESTAMP".into();
        assert_eq!(
            renders(timestamp, IndexType::None),
            "TIMESTAMP DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP NOT NULL"
        );

        // String defaults are quoted and escaped; blob columns take none.
        let mut named = ColumnType::string_like(MySqlType::VarString, true, 32);
        named.default_value = "no name".into();
        assert_eq!(
            renders(named, IndexType::None),
            "VARBINARY(32) DEFAULT 'no name' NOT NULL"
        );
        let mut quoted = ColumnType::string_like(MySqlType::VarString, false, 32);
        quoted.default_value = "it's \"big\"\\wide\n".into();
        assert_eq!(
            renders(quoted, IndexType::None),
            "VARCHAR(32) DEFAULT 'it\\'s \\\"big\\\"\\\\wide\\n' NOT NULL"
        );
        assert_eq!(
            renders(
                ColumnType::string_like(MySqlType::Blob, true, 1000),
                IndexType::None
            ),
            "BLOB NOT NULL",
            "the blob family takes no default clause"
        );
    }

    /// The fresh-database statement sequence of a table with and without a
    /// mapping-supplied id (`mysql_table_initialiser.cpp:357-405`).
    #[test]
    fn a_table_emits_the_fresh_database_statement_sequence() {
        // An entity table: the mapping supplies the id, so the ALTER adds
        // the rest and keeps the created id. Columns are visited by name,
        // the order of the reference's map (`table_meta_data.hpp:42`).
        let table = TableDescription {
            name: "tbl_Avatar".to_string(),
            columns: vec![
                id_column(),
                ColumnDescription::new(
                    "sm_z",
                    ColumnType::numeric(MySqlType::Long, false),
                    IndexType::None,
                ),
                ColumnDescription::new(
                    "sm_name",
                    ColumnType::string_like(MySqlType::VarString, true, 255),
                    IndexType::Unique,
                ),
                ColumnDescription::new(
                    "sm_a",
                    ColumnType::numeric(MySqlType::Long, false),
                    IndexType::None,
                ),
            ],
        };
        assert_eq!(
            statements(&table),
            vec![
                "CREATE TABLE IF NOT EXISTS tbl_Avatar (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                    .to_string(),
                "ALTER TABLE tbl_Avatar ADD COLUMN sm_a INT NOT NULL, \
                 ADD COLUMN sm_name VARBINARY(255) NOT NULL, \
                 ADD COLUMN sm_z INT NOT NULL"
                    .to_string(),
                "CREATE UNIQUE INDEX nameIndex ON tbl_Avatar (sm_name(255))".to_string(),
            ]
        );

        // A sub-table: no mapping id, so the temporary one is dropped; the
        // parentID gets its fixed-name index.
        let sub_table = TableDescription {
            name: "tbl_Avatar_inventory".to_string(),
            columns: vec![
                parent_id_column(),
                ColumnDescription::new(
                    "sm_value",
                    ColumnType::string_like(MySqlType::Blob, true, 1000),
                    IndexType::None,
                ),
            ],
        };
        assert_eq!(
            statements(&sub_table),
            vec![
                "CREATE TABLE IF NOT EXISTS tbl_Avatar_inventory (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                    .to_string(),
                "ALTER TABLE tbl_Avatar_inventory ADD COLUMN parentID BIGINT NOT NULL, \
                 ADD COLUMN sm_value BLOB NOT NULL, \
                 DROP COLUMN id"
                    .to_string(),
                "CREATE INDEX parentIDIndex ON tbl_Avatar_inventory (parentID)".to_string(),
            ]
        );
    }

    /// Index lengths: VARCHAR keeps its full length, everything string-like
    /// clamps to the first 255 characters
    /// (`mysql_table_initialiser.cpp:195-211`); numeric types carry no
    /// length at all.
    #[test]
    fn index_lengths_clamp_like_the_reference() {
        let indexed = |field_type: MySqlType, binary: bool, length: u32| {
            let table = TableDescription {
                name: "tbl_X".to_string(),
                columns: vec![ColumnDescription::new(
                    "sm_x",
                    ColumnType::string_like(field_type, binary, length),
                    IndexType::NonUnique,
                )],
            };
            statements(&table).pop().expect("one index statement")
        };

        // VARCHAR keeps its full length.
        assert_eq!(
            indexed(MySqlType::VarString, false, 255),
            "CREATE INDEX xIndex ON tbl_X (sm_x(255))"
        );
        // A long BLOB clamps to 255.
        assert_eq!(
            indexed(MySqlType::Blob, true, 65535),
            "CREATE INDEX xIndex ON tbl_X (sm_x(255))"
        );
        // TINYBLOB under the limit keeps its length.
        assert_eq!(
            indexed(MySqlType::TinyBlob, true, 100),
            "CREATE INDEX xIndex ON tbl_X (sm_x(100))"
        );
        // Numerics carry no length suffix.
        let numeric_table = TableDescription {
            name: "tbl_X".to_string(),
            columns: vec![ColumnDescription::new(
                "sm_x",
                ColumnType::numeric(MySqlType::LongLong, false),
                IndexType::Unique,
            )],
        };
        assert_eq!(
            statements(&numeric_table)
                .pop()
                .expect("one index statement"),
            "CREATE UNIQUE INDEX xIndex ON tbl_X (sm_x)"
        );
    }

    /// The acceptance: 与旧 MySQL schema 对照 — an entity def maps to the
    /// table set, columns, and indexes the old MySQL schema produces for
    /// it, statement by statement.
    #[test]
    fn an_entity_def_maps_to_the_old_mysql_schema() {
        let desc = parse_entity(&[
            (
                "scripts/entity_defs/Avatar.def",
                "<root> <Properties> \
                     <name> <Type> STRING </Type> <Flags> BASE </Flags> \
                         <Persistent> true </Persistent> <Identifier> true </Identifier> \
                         <DatabaseLength> 255 </DatabaseLength> \
                         <Default> no name </Default> </name> \
                     <hp> <Type> INT32 </Type> <Flags> BASE </Flags> \
                         <Persistent> true </Persistent> </hp> \
                     <home> <Type> VECTOR3 </Type> <Flags> BASE </Flags> \
                         <Persistent> true </Persistent> </home> \
                     <icon> <Type> BLOB </Type> <Flags> BASE </Flags> \
                         <Persistent> true </Persistent> \
                         <DatabaseLength> 1024 </DatabaseLength> </icon> \
                     <inventory> <Type> ARRAY <size> 0 </size> \
                             <of> FIXED_DICT <Properties> \
                                 <item> <Type> STRING </Type> \
                                     <DatabaseLength> 32 </DatabaseLength> </item> \
                                 <count> <Type> INT8 </Type> </count> \
                             </Properties> </of> </Type> \
                         <Flags> BASE </Flags> <Persistent> true </Persistent> \
                     </inventory> \
                 </Properties> </root>",
            ),
            // The cell script makes can_be_on_cell() true, which is what
            // adds the position/direction/spaceID columns
            // (`property_mappings_per_type.cpp:148-173`).
            ("scripts/cell/Avatar", ""),
        ]);

        let schema = entity_schema(&desc);
        assert!(schema.unmapped.is_empty(), "{:?}", schema.unmapped);

        // The main table: def properties in order, then the cell columns
        // and the fixed gameTime/timestamp tail.
        let cols: Vec<&ColumnDescription> = schema.table.columns.iter().by_ref().collect();
        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "id",
                "sm_name",
                "sm_hp",
                "vm_0_home",
                "vm_1_home",
                "vm_2_home",
                "sm_icon",
                "vm_0_position",
                "vm_1_position",
                "vm_2_position",
                "vm_0_direction",
                "vm_1_direction",
                "vm_2_direction",
                "sm_spaceID",
                "sm_gameTime",
                "sm_timestamp"
            ]
        );

        let stmts = statements(&schema.table);
        assert_eq!(
            stmts[0],
            "CREATE TABLE IF NOT EXISTS tbl_Avatar (id BIGINT AUTO_INCREMENT, \
             PRIMARY KEY idKey (id)) ENGINE=InnoDB"
        );
        assert_eq!(
            stmts[1],
            "ALTER TABLE tbl_Avatar \
             ADD COLUMN sm_gameTime INT UNSIGNED DEFAULT 0 NOT NULL, \
             ADD COLUMN sm_hp INT DEFAULT 0 NOT NULL, \
             ADD COLUMN sm_icon BLOB NOT NULL, \
             ADD COLUMN sm_name VARBINARY(255) NOT NULL, \
             ADD COLUMN sm_spaceID INT DEFAULT 0 NOT NULL, \
             ADD COLUMN sm_timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP \
                 ON UPDATE CURRENT_TIMESTAMP NOT NULL, \
             ADD COLUMN vm_0_direction FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_0_home FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_0_position FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_1_direction FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_1_home FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_1_position FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_2_direction FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_2_home FLOAT DEFAULT 0 NOT NULL, \
             ADD COLUMN vm_2_position FLOAT DEFAULT 0 NOT NULL"
        );
        assert_eq!(
            stmts[2],
            "CREATE UNIQUE INDEX nameIndex ON tbl_Avatar (sm_name(255))"
        );
        assert_eq!(stmts.len(), 3, "no other indexes: {:?}", stmts);

        // The array becomes a sub-table keyed by parentID with the dict
        // fields as relative columns.
        assert_eq!(schema.sub_tables.len(), 1);
        let inventory = &schema.sub_tables[0];
        assert_eq!(inventory.name, "tbl_Avatar_inventory");
        let sub_names: Vec<&str> = inventory.columns.iter().map(|c| c.name.as_str()).collect();
        // Def-visit order in the column list; the DDL below is what
        // iterates alphabetically.
        assert_eq!(sub_names, vec!["parentID", "sm_item", "sm_count"]);
        assert_eq!(
            statements(inventory),
            vec![
                "CREATE TABLE IF NOT EXISTS tbl_Avatar_inventory (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                    .to_string(),
                "ALTER TABLE tbl_Avatar_inventory ADD COLUMN parentID BIGINT NOT NULL, \
                 ADD COLUMN sm_count TINYINT DEFAULT 0 NOT NULL, \
                 ADD COLUMN sm_item VARBINARY(32) NOT NULL, \
                 DROP COLUMN id"
                    .to_string(),
                "CREATE INDEX parentIDIndex ON tbl_Avatar_inventory (parentID)".to_string(),
            ]
        );
    }

    /// An entity that cannot be on a cell carries no position/direction/
    /// spaceID columns (`property_mappings_per_type.cpp:147`).
    #[test]
    fn a_base_only_entity_carries_no_cell_columns() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Volatile> </Volatile> <Properties> \
                 <hp> <Type> INT32 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </hp> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        let names: Vec<&str> = schema
            .table
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["id", "sm_hp", "sm_gameTime", "sm_timestamp"],
            "no position, direction, or spaceID"
        );
    }

    /// persistAsBlob arrays become one binary blob column of the blob
    /// length (`BlobbedSequenceMapping`,
    /// `blobbed_sequence_mapping.cpp:18-32`).
    #[test]
    fn a_blobbed_sequence_maps_to_a_single_blob_column() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <bag> <Type> ARRAY <size> 0 </size> <persistAsBlob> true </persistAsBlob> \
                         <of> FIXED_DICT <Properties> \
                             <x> <Type> INT32 </Type> </x> \
                         </Properties> </of> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> </bag> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        assert!(schema.sub_tables.is_empty());
        assert!(schema.unmapped.is_empty(), "{:?}", schema.unmapped);
        let bag = schema
            .table
            .columns
            .iter()
            .find(|c| c.name == "sm_bag")
            .expect("the blobbed sequence column");
        assert_eq!(
            bag.col_type.get_as_string(IndexType::None),
            "MEDIUMBLOB NOT NULL"
        );
    }

    /// Indexed compound types and unbindable types are the reference's
    /// "cannot be persistent" set (`property_mapping.cpp:144,179,190,197,
    /// 264-288`): mailbox, user type, and every indexed ARRAY/VECTOR/
    /// FIXED_DICT/CLASS. Plain `AllowNone` classes carry the `fm_` flag
    /// column (`class_mapping.cpp:15-22`).
    #[test]
    fn unmappable_properties_are_reported_not_mapped() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <box> <Type> MAILBOX </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </box> \
                 <ut> <Type> USER_TYPE <implementedBy> no.such </implementedBy> \
                     </Type> <Flags> BASE </Flags> <Persistent> true </Persistent> </ut> \
                 <array> <Type> ARRAY <of> INT32 </of> </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> <Identifier> true </Identifier> \
                 </array> \
                 <vec> <Type> VECTOR3 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> <Indexed> true </Indexed> </vec> \
                 <fdict> <Type> FIXED_DICT <Properties> \
                         <x> <Type> INT32 </Type> </x> </Properties> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> \
                     <Identifier> true </Identifier> </fdict> \
                 <klass> <Type> CLASS <AllowNone> true </AllowNone> <Properties> \
                         <sub> <Type> INT32 </Type> </sub> </Properties> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> \
                     <Indexed> true </Indexed> </klass> \
                 <noneklass> <Type> CLASS <AllowNone> true </AllowNone> <Properties> \
                         <sub> <Type> INT32 </Type> </sub> </Properties> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> \
                 </noneklass> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);

        // MAILBOX never maps; USER_TYPE needs a script binding this schema
        // does not have; the indexed ARRAY/VECTOR3/FIXED_DICT/CLASS fall
        // out of the reference's dispatch.
        assert_eq!(
            schema.unmapped,
            vec!["box", "ut", "array", "vec", "fdict", "klass"]
        );

        // The non-indexed AllowNone CLASS maps with its fm_ flag and its
        // field column.
        let names: Vec<&str> = schema
            .table
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert!(names.contains(&"fm_noneklass"), "{names:?}");
        assert!(names.contains(&"sm_noneklass_sub"), "{names:?}");
    }

    /// A VECTOR default section feeds the per-component defaults
    /// (`vector_mapping.hpp:18-27`).
    #[test]
    fn a_vector_default_feeds_the_component_defaults() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <home> <Type> VECTOR3 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <Default> 1.5 2.5 3.5 </Default> </home> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        let home: Vec<String> = schema
            .table
            .columns
            .iter()
            .filter(|c| c.name.starts_with("vm_") && c.name.ends_with("_home"))
            .map(|c| c.col_type.get_as_string(IndexType::None))
            .collect();
        assert_eq!(
            home,
            vec![
                "FLOAT DEFAULT 1.5 NOT NULL",
                "FLOAT DEFAULT 2.5 NOT NULL",
                "FLOAT DEFAULT 3.5 NOT NULL"
            ]
        );
    }

    /// Long string defaults truncate to the column length
    /// (`string_mapping.cpp:20-26`), and a long unicode string column is
    /// TEXT while a long string column is BLOB.
    #[test]
    fn long_defaults_truncate_and_long_strings_use_the_blob_ladder() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <bin> <Type> STRING \
                         <Default> 0123456789012345678901234567890123456789 </Default> \
                     </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <DatabaseLength> 1000 </DatabaseLength> </bin> \
                 <uni> <Type> UNICODE_STRING </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <DatabaseLength> 1000 </DatabaseLength> </uni> \
                 <tiny> <Type> BLOB </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <DatabaseLength> 100 </DatabaseLength> </tiny> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        let find = |name: &str| {
            schema
                .table
                .columns
                .iter()
                .find(|c| c.name == name)
                .expect(name)
                .col_type
                .get_as_string(IndexType::None)
        };
        // The 40-character default truncates to nothing? No: the column is
        // 1000 long, the default fits — but renders as BLOB, so the default
        // clause is dropped entirely.
        assert_eq!(find("sm_bin"), "BLOB NOT NULL");
        assert_eq!(find("sm_uni"), "TEXT NOT NULL");
        assert_eq!(find("sm_tiny"), "TINYBLOB NOT NULL");
    }

    /// A default longer than the column truncates to the column length
    /// (`string_mapping.cpp:20-26`).
    #[test]
    fn an_over_long_default_truncates_to_the_column_length() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <nick> <Type> UNICODE_STRING \
                         <Default> 0123456789ABCDEF </Default> </Type> \
                     <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <DatabaseLength> 8 </DatabaseLength> </nick> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        let nick = schema
            .table
            .columns
            .iter()
            .find(|c| c.name == "sm_nick")
            .expect("the nick column");
        assert_eq!(nick.col_type.length, 8);
        assert_eq!(nick.col_type.default_value, "01234567");
    }

    /// Non-persistent properties contribute nothing — the visit is
    /// ONLY_PERSISTENT_DATA (`property_mappings_per_type.cpp:147`).
    #[test]
    fn non_persistent_properties_contribute_nothing() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <hp> <Type> INT32 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </hp> \
                 <mood> <Type> STRING </Type> <Flags> BASE </Flags> </mood> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        let names: Vec<&str> = schema
            .table
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["id", "sm_hp", "sm_gameTime", "sm_timestamp"]);
        assert!(schema.unmapped.is_empty());
    }

    /// The remaining text-side keywords of the char/blob family — strings
    /// are CHAR/BINARY, and the blob ladder has a TEXT flavour for unicode
    /// strings (`type_traits.hpp:47-66`, `column_type.cpp:130-183`).
    #[test]
    fn the_text_side_keywords_render_too() {
        let renders = |field_type: MySqlType, binary: bool, length: u32| {
            ColumnType::string_like(field_type, binary, length).get_as_string(IndexType::None)
        };
        assert_eq!(renders(MySqlType::Str, false, 16), "CHAR(16) NOT NULL");
        assert_eq!(
            renders(MySqlType::TinyBlob, false, 100),
            "TINYTEXT NOT NULL"
        );
        assert_eq!(
            renders(MySqlType::MediumBlob, false, 70000),
            "MEDIUMTEXT NOT NULL"
        );
        assert_eq!(
            renders(MySqlType::LongBlob, false, 1 << 24),
            "LONGTEXT NOT NULL"
        );
    }

    /// `MySqlEscapedString` escapes every character MySQL treats specially
    /// (`mysqldataset_helper` escaping, exercised through the default
    /// clause).
    #[test]
    fn the_escape_table_covers_every_special_character() {
        let mut named = ColumnType::string_like(MySqlType::VarString, false, 64);
        named.default_value = "\0\r\x08\t\x1a".into();
        assert_eq!(
            named.get_as_string(IndexType::None),
            "VARCHAR(64) DEFAULT '\\0\\r\\b\\t\\Z' NOT NULL"
        );
    }

    /// A table that supplies no column at all still drops the placeholder
    /// id — with no `ADD COLUMN` before it, no comma
    /// (`mysql_table_initialiser.cpp:75-84,375-382`).
    #[test]
    fn a_columnless_table_drops_the_placeholder_id_alone() {
        let table = TableDescription {
            name: "tbl_X".to_string(),
            columns: vec![],
        };
        assert_eq!(
            statements(&table),
            vec![
                "CREATE TABLE IF NOT EXISTS tbl_X (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                    .to_string(),
                "ALTER TABLE tbl_X DROP COLUMN id".to_string(),
            ]
        );
    }

    /// An empty property name pushes the `value`/`values` placeholder
    /// level, and an empty suffix adds nothing (`namer.cpp:49-69,102-121`).
    #[test]
    fn the_element_placeholder_and_the_empty_suffix_behave_like_the_reference() {
        let namer = Namer::new("Avatar");
        assert_eq!(namer.child("", false).names, vec!["Avatar", "value"]);
        assert_eq!(namer.child("", true).names, vec!["Avatar", "values"]);
        assert_eq!(namer.build_name("sm", "", 1), "sm");
    }

    /// `DataDescription::fullName` for a componentised property is
    /// `componentName.name` (`data_description.ipp:325-336`) — the name the
    /// mapping is created under.
    #[test]
    fn a_componentised_property_maps_under_its_component_name() {
        let mut prop = entitydef::data_description::DataDescription::default();
        prop.member.name = "hp".into();
        prop.component_name = "movement".into();
        assert_eq!(mapping_name(&prop), "movement.hp");

        let mut plain = entitydef::data_description::DataDescription::default();
        plain.member.name = "hp".into();
        assert_eq!(mapping_name(&plain), "hp");
    }

    /// The leaf kinds the other cases do not touch: every integer width,
    /// both float widths, python, entity-user-data references, the long-blob
    /// rung, nested arrays, and class defaults — each against the DDL the
    /// reference renders for it.
    #[test]
    fn the_remaining_leaf_kinds_map_like_the_reference() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <i16> <Type> INT16 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </i16> \
                 <u64> <Type> UINT64 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </u64> \
                 <f32> <Type> FLOAT32 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </f32> \
                 <f64> <Type> FLOAT64 <Default> 2.5 </Default> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> </f64> \
                 <py> <Type> PYTHON </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </py> \
                 <uref> <Type> UDO_REF </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </uref> \
                 <longstr> <Type> STRING </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <DatabaseLength> 16777216 </DatabaseLength> </longstr> \
                 <num> <Type> INT32 <Default> 42 </Default> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> </num> \
                 <two> <Type> VECTOR2 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <Default> 1.5 2.5 </Default> </two> \
                 <four> <Type> VECTOR4 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> \
                     <Default> 1 2 3 4 </Default> </four> \
                 <plain4> <Type> VECTOR4 </Type> <Flags> BASE </Flags> \
                     <Persistent> true </Persistent> </plain4> \
                 <klass> <Type> CLASS <AllowNone> true </AllowNone> <Properties> \
                         <sub> <Type> INT32 </Type> </sub> \
                         <box> <Type> MAILBOX </Type> </box> </Properties> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> \
                     <Default> <sub> 7 </sub> </Default> </klass> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        assert!(schema.unmapped.is_empty(), "{:?}", schema.unmapped);

        let find = |name: &str| {
            schema
                .table
                .columns
                .iter()
                .find(|c| c.name == name)
                .expect(name)
                .col_type
                .get_as_string(IndexType::None)
        };
        // Integer widths: SMALLINT for 16-bit, BIGINT for 64-bit
        // (`num_mapping.hpp:15-45`).
        assert_eq!(find("sm_i16"), "SMALLINT DEFAULT 0 NOT NULL");
        assert_eq!(find("sm_u64"), "BIGINT UNSIGNED DEFAULT 0 NOT NULL");
        // Float widths: FLOAT32 and FLOAT64 columns
        // (`num_mapping.hpp`, `string_conv.hpp:117-159`).
        assert_eq!(find("sm_f32"), "FLOAT DEFAULT 0 NOT NULL");
        assert_eq!(find("sm_f64"), "DOUBLE DEFAULT 2.5 NOT NULL");
        // Python: a binary blob of the database length
        // (`python_mapping.cpp:20-24`); the default length is 64K-1, the
        // BLOB rung of the ladder.
        assert_eq!(find("sm_py"), "BLOB NOT NULL");
        // A UDO reference is a 16-byte binary column
        // (`unique_id_mapping.cpp:91-102`).
        assert_eq!(find("sm_uref"), "BINARY(16) NOT NULL");
        // 16MB strings reach the long rung of the ladder
        // (`type_traits.hpp:47-66`).
        assert_eq!(find("sm_longstr"), "LONGBLOB NOT NULL");
        // A type-level default on a const type is what the DDL reads
        // (`property_mappings_per_type.cpp:31-41`).
        assert_eq!(find("sm_num"), "INT DEFAULT 42 NOT NULL");

        // VECTOR2/VECTOR4 take one column per component
        // (`vector_mapping.hpp:62-82`); a missing trailing default falls
        // back to zero.
        let mut vector: Vec<String> = schema
            .table
            .columns
            .iter()
            .filter(|c| c.name.ends_with("_two"))
            .map(|c| c.col_type.get_as_string(IndexType::None))
            .collect();
        assert_eq!(
            vector,
            vec!["FLOAT DEFAULT 1.5 NOT NULL", "FLOAT DEFAULT 2.5 NOT NULL"]
        );
        vector = schema
            .table
            .columns
            .iter()
            .filter(|c| c.name.ends_with("_four"))
            .map(|c| c.col_type.get_as_string(IndexType::None))
            .collect();
        assert_eq!(
            vector,
            vec![
                "FLOAT DEFAULT 1 NOT NULL",
                "FLOAT DEFAULT 2 NOT NULL",
                "FLOAT DEFAULT 3 NOT NULL",
                "FLOAT DEFAULT 4 NOT NULL"
            ]
        );
        vector = schema
            .table
            .columns
            .iter()
            .filter(|c| c.name.ends_with("_plain4"))
            .map(|c| c.col_type.get_as_string(IndexType::None))
            .collect();
        assert_eq!(
            vector,
            vec![
                "FLOAT DEFAULT 0 NOT NULL",
                "FLOAT DEFAULT 0 NOT NULL",
                "FLOAT DEFAULT 0 NOT NULL",
                "FLOAT DEFAULT 0 NOT NULL"
            ]
        );

        // The class maps its flag and its int field with the parent
        // default's field section (`property_mapping.cpp:46-61`); the
        // mailbox field no mapping takes is skipped, not reported
        // (`property_mapping.cpp:90-96`).
        assert_eq!(find("fm_klass"), "TINYINT UNSIGNED DEFAULT 1 NOT NULL");
        assert_eq!(find("sm_klass_sub"), "INT DEFAULT 7 NOT NULL");
        assert!(schema.table.columns.iter().all(|c| !c.name.contains("box")));
    }

    /// A sequence whose element is itself a sequence has no leaf mapping —
    /// the reference's dispatch runs out and the property "cannot be
    /// persistent" (`property_mapping.cpp:173-197,340-349`).
    #[test]
    fn a_sequence_of_sequences_is_not_persistent() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <grid> <Type> ARRAY <of> ARRAY <of> INT32 </of> </of> </Type> \
                     <Flags> BASE </Flags> <Persistent> true </Persistent> </grid> \
             </Properties> </root>",
        )]);
        let schema = entity_schema(&desc);
        assert_eq!(schema.unmapped, vec!["grid"]);
    }

    /// The acceptance, 原版数据库行可读写: a bound user type maps as the
    /// composite of its `bind()` calls — the attributes become columns on
    /// the owner's table under the property's naming level, the way
    /// `UserTypeMapping` produces them
    /// (`user_type_mapping.cpp:106-145`, `py_user_type_binder.cpp:16-77`).
    #[test]
    fn a_bound_user_type_maps_as_the_composite_of_its_binds() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <stats> <Type> USER_TYPE <implementedBy> game.stats </implementedBy> \
                     </Type> <Flags> BASE </Flags> <Persistent> true </Persistent> \
                 </stats> \
             </Properties> </root>",
        )]);

        let mut bindings: UserTypeBindings = HashMap::new();
        bindings.insert(
            ("game".to_string(), "stats".to_string()),
            UserTypeBinding {
                attributes: vec![
                    BoundAttribute {
                        name: "hp".to_string(),
                        type_name: "INT32".to_string(),
                        database_length: 255,
                    },
                    BoundAttribute {
                        name: "title".to_string(),
                        type_name: "STRING".to_string(),
                        database_length: 32,
                    },
                    // A bound array attribute gets its own sub-table, the
                    // way a beginTable group does.
                    BoundAttribute {
                        name: "tags".to_string(),
                        type_name: "ARRAY <of> INT32 </of>".to_string(),
                        database_length: 255,
                    },
                    // A bound nested user type contributes its own binds'
                    // columns under this one.
                    BoundAttribute {
                        name: "inner".to_string(),
                        type_name: "USER_TYPE <implementedBy> game.inner </implementedBy>"
                            .to_string(),
                        database_length: 255,
                    },
                    // A mailbox attribute no mapping takes is skipped, the
                    // way class fields are.
                    BoundAttribute {
                        name: "box".to_string(),
                        type_name: "MAILBOX".to_string(),
                        database_length: 255,
                    },
                    // An unknown type name is the reference's invalid bind;
                    // the attribute contributes nothing.
                    BoundAttribute {
                        name: "junk".to_string(),
                        type_name: "NO_SUCH_TYPE".to_string(),
                        database_length: 255,
                    },
                ],
            },
        );
        bindings.insert(
            ("game".to_string(), "inner".to_string()),
            UserTypeBinding {
                attributes: vec![BoundAttribute {
                    name: "level".to_string(),
                    type_name: "INT8".to_string(),
                    database_length: 255,
                }],
            },
        );

        let schema = entity_schema_with_user_types(&desc, &bindings);
        assert!(schema.unmapped.is_empty(), "{:?}", schema.unmapped);

        let names: Vec<&str> = schema
            .table
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        // sm_stats_<attr>: the binder's naming level for the property, then
        // each attribute; no fm_ flag, the composite has none. The nested
        // user type's own bind lands under it; the mailbox and the unknown
        // type contribute nothing.
        assert_eq!(
            names,
            vec![
                "id",
                "sm_stats_hp",
                "sm_stats_title",
                "sm_stats_inner_level",
                "sm_gameTime",
                "sm_timestamp"
            ]
        );
        let find = |name: &str| {
            schema
                .table
                .columns
                .iter()
                .find(|c| c.name == name)
                .expect(name)
                .col_type
                .get_as_string(IndexType::None)
        };
        assert_eq!(find("sm_stats_hp"), "INT DEFAULT 0 NOT NULL");
        assert_eq!(find("sm_stats_title"), "VARBINARY(32) NOT NULL");
        // The bound array's sub-table, off the property's naming level.
        assert_eq!(schema.sub_tables.len(), 1);
        let tags = &schema.sub_tables[0];
        assert_eq!(tags.name, "tbl_Avatar_stats_tags");
        let tag_names: Vec<&str> = tags.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(tag_names, vec!["parentID", "sm_value"]);
    }

    /// An unbound user type stays unmapped — the reference's failed script
    /// import (`user_type_mapping.cpp:112-125`) — and an indexed one is
    /// the "cannot be persistent" error (`property_mapping.cpp:264-288`).
    #[test]
    fn an_unbound_or_indexed_user_type_is_unmapped() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <plain> <Type> USER_TYPE <implementedBy> no.such </implementedBy> \
                     </Type> <Flags> BASE </Flags> <Persistent> true </Persistent> \
                 </plain> \
                 <fixed> <Type> USER_TYPE <implementedBy> game.stats </implementedBy> \
                     </Type> <Flags> BASE </Flags> <Persistent> true </Persistent> \
                     <Identifier> true </Identifier> </fixed> \
             </Properties> </root>",
        )]);

        let mut bindings: UserTypeBindings = HashMap::new();
        bindings.insert(
            ("game".to_string(), "stats".to_string()),
            UserTypeBinding::default(),
        );

        let schema = entity_schema_with_user_types(&desc, &bindings);
        assert_eq!(schema.unmapped, vec!["plain", "fixed"]);
    }

    /// A bound user type inside a sequence element maps relative to the
    /// sub-table — the binder's unnamed context pushes no level
    /// (`py_user_type_binder.cpp:17-21`).
    #[test]
    fn a_bound_user_type_element_maps_relative_to_the_sub_table() {
        let desc = parse_entity(&[(
            "scripts/entity_defs/Avatar.def",
            "<root> <Properties> \
                 <items> <Type> ARRAY <size> 0 </size> \
                         <of> USER_TYPE <implementedBy> game.item </implementedBy> </of> \
                     </Type> <Flags> BASE </Flags> <Persistent> true </Persistent> \
                 </items> \
             </Properties> </root>",
        )]);

        let mut bindings: UserTypeBindings = HashMap::new();
        bindings.insert(
            ("game".to_string(), "item".to_string()),
            UserTypeBinding {
                attributes: vec![
                    BoundAttribute {
                        name: "count".to_string(),
                        type_name: "INT8".to_string(),
                        database_length: 255,
                    },
                    // An array attribute inside the element carries its own
                    // sub-table after the element's.
                    BoundAttribute {
                        name: "mods".to_string(),
                        type_name: "ARRAY <of> INT32 </of>".to_string(),
                        database_length: 255,
                    },
                ],
            },
        );

        let schema = entity_schema_with_user_types(&desc, &bindings);
        assert!(schema.unmapped.is_empty(), "{:?}", schema.unmapped);
        assert_eq!(schema.sub_tables.len(), 2);
        let items = &schema.sub_tables[0];
        assert_eq!(items.name, "tbl_Avatar_items");
        let names: Vec<&str> = items.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["parentID", "sm_count"]);
        let mods = &schema.sub_tables[1];
        assert_eq!(mods.name, "tbl_Avatar_items_mods");
        let mod_names: Vec<&str> = mods.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(mod_names, vec!["parentID", "sm_value"]);
    }
}
