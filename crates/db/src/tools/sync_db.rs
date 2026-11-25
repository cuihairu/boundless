//! The `sync_db` tool: synchronise a MySQL database with the entity
//! definitions (`server/tools/sync_db/` + the `db_storage_mysql` schema
//! plumbing in the reference).
//!
//! The driver is [`Synchronise::do_synchronise`], a line-for-line port of the
//! `MySqlSynchronise::doSynchronise` gate order (mysql_synchronise.cpp:88-217):
//! storage-engine support, the twelve special BigWorld tables, per-table
//! engine check, the unconsolidated-secondary-databases guard, the version
//! ladder read (with INSERT-on-fresh-database), the character-set ALTER, the
//! upgrade hook, the password-hash transition, and finally the entity table
//! surgery. The surgery itself is [`Inspector`] + [`TableHandler`]
//! (table_inspector.cpp:26-104) with two handlers: [`TableInitialiser`]
//! (mysql_table_initialiser.cpp:354-529) and the report-only
//! [`TableValidator`] (table_inspector.cpp:168-243).
//!
//! The MySQL wire level stays behind the [`SyncDatabase`] seam: the live
//! wrapper is another item's face, and the statement log a test double
//! records is the reviewable artifact the acceptance clause asks for. The
//! desired-table list is likewise an input — building it from property
//! mappings (`mappings/property_mappings_per_type`) belongs to the
//! property-mapping items, as does the content of the upgrade ladder
//! ([`SyncDatabase::upgrade_database`]).

use std::collections::{BTreeMap, BTreeSet};

/// The reference's storage-engine string (constants.hpp `MYSQL_ENGINE_TYPE`).
pub const MYSQL_ENGINE_TYPE: &str = "InnoDB";
/// Entity table prefix (constants.hpp `TABLE_NAME_PREFIX`).
pub const TABLE_NAME_PREFIX: &str = "tbl";
/// The implicit per-entity-table id column (constants.hpp `ID_COLUMN_NAME`).
pub const ID_COLUMN_NAME: &str = "id";
/// The parent-id column name (constants.hpp `PARENTID_COLUMN_NAME`).
pub const PARENTID_COLUMN_NAME: &str = "parentID";
/// The parent-id index name (table_meta_data.hpp `PARENTID_INDEX_NAME`).
pub const PARENTID_INDEX_NAME: &str = "parentIDIndex";
/// The index-suffix rule shared with the def-side namer ("Index").
const INDEX_SUFFIX: &str = "Index";
/// constants.hpp `BWMySQLMaxLogOnNameLen`.
pub const BW_MYSQL_MAX_LOG_ON_NAME_LEN: u32 = 255;
/// constants.hpp `BWMySQLMaxLogOnPasswordLen`.
pub const BW_MYSQL_MAX_LOG_ON_PASSWORD_LEN: u32 = 255;
/// constants.hpp `MAX_SECONDARY_DB_LOCATION_LENGTH`; must fit a BLOB.
pub const MAX_SECONDARY_DB_LOCATION_LENGTH: u64 = 4096;
/// mysql_synchronise.cpp:171 defaults (config override resolution).
pub const DEFAULT_CHARACTER_SET: &str = "utf8";
/// mysql_synchronise.cpp:172.
pub const DEFAULT_COLLATION: &str = "utf8_bin";

// Version ladder (versions.hpp).
pub const DBAPP_VERSION_1_7: u32 = 1;
pub const DBAPP_VERSION_1_8: u32 = 2;
pub const DBAPP_VERSION_1_9_SNAPSHOT: u32 = 3;
pub const DBAPP_VERSION_1_9_NON_NULL: u32 = 4;
pub const DBAPP_VERSION_SECONDARYDB: u32 = 5;
pub const DBAPP_VERSION_BINARY_STRINGS: u32 = 6;
pub const DBAPP_VERSION_LOGON_MAPPING: u32 = 7;
pub const DBAPP_VERSION_SHOULD_AUTO_LOAD: u32 = 8;
pub const DBAPP_VERSION_LOGON_MAPPING_2: u32 = 9;
pub const DBAPP_CURRENT_VERSION: u32 = DBAPP_VERSION_LOGON_MAPPING_2;
pub const DBAPP_OLDEST_SUPPORTED_VERSION: u32 = 1;

// MYSQL_FIELD flag bits (mysql.h) used by the schema plumbing.
pub const PRI_KEY_FLAG: u32 = 2;
pub const UNIQUE_KEY_FLAG: u32 = 16;
pub const UNSIGNED_FLAG: u32 = 32;
pub const MULTIPLE_KEY_FLAG: u32 = 256;
pub const AUTO_INCREMENT_FLAG: u32 = 512;

/// The mysql-side index-kind vocabulary (column_type.hpp `ColumnIndexType`).
///
/// Distinct from the def-side [`crate::namer::ColumnIndexType`]: that one
/// says what a property *wants*, this one says what the live table *has*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ColumnIndexType {
    None = 0,
    Primary = 1,
    ParentId = 2,
    Unique = 3,
    NonUnique = 4,
    External = 5,
}

/// The `enum_field_types` values the reference switches on (mysql.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum FieldType {
    Decimal = 0,
    Tiny = 1,
    Short = 2,
    Long = 3,
    Float = 4,
    Double = 5,
    Null = 6,
    Timestamp = 7,
    LongLong = 8,
    Int24 = 9,
    Time = 11,
    Year = 13,
    NewDate = 14,
    VarChar = 15,
    Bit = 16,
    NewDecimal = 246,
    TinyBlob = 249,
    MediumBlob = 250,
    LongBlob = 251,
    Blob = 252,
    VarString = 253,
    String = 254,
}

/// One `MYSQL_FIELD` as the seam reports it.
#[derive(Clone, Debug, Default)]
pub struct Field {
    pub name: String,
    pub field_type: Option<FieldType>,
    pub flags: u32,
    /// In bytes for binary types, in characters otherwise (mysql.h `length`).
    pub length: u32,
    pub charsetnr: u16,
    /// The live wrapper's `MySql::charsetWidth(charsetnr)` answer (1 when
    /// unknown); `length` is divided by it on derivation.
    pub charset_width: u32,
    pub def: Option<String>,
}

/// One row of `SHOW INDEX FROM t` — the reference reads columns 1/2/4
/// (table_meta_data.cpp:140-146).
#[derive(Clone, Debug)]
pub struct IndexRow {
    pub non_unique: String,
    pub key_name: String,
    pub column_name: String,
}

/// What [`SyncDatabase::describe_table`] returns: the field list (empty when
/// the table is missing) plus the index rows.
#[derive(Clone, Debug, Default)]
pub struct DescribedTable {
    pub fields: Vec<Field>,
    pub indices: Vec<IndexRow>,
}

/// The character width of a charset id; unknown ids fall back to one byte per
/// character (wrapper.cpp:260-272).
pub fn charset_width(charsetnr: u16, collation_lengths: &BTreeMap<u16, u32>) -> u32 {
    collation_lengths.get(&charsetnr).copied().unwrap_or(1)
}

/// `MySqlTypeTraits<BW::string>::colType` (type_traits.hpp:46-64): the blob
/// family chosen by maximum width in bytes.
pub fn string_col_type(max_col_width_bytes: u32) -> FieldType {
    if max_col_width_bytes < 1 << 8 {
        FieldType::TinyBlob
    } else if max_col_width_bytes < 1 << 16 {
        FieldType::Blob
    } else if max_col_width_bytes < 1 << 24 {
        FieldType::MediumBlob
    } else {
        FieldType::LongBlob
    }
}

/// The `colTypeStr` names (type_traits.cpp:9-12).
pub fn string_col_type_str(max_col_width_bytes: u32) -> &'static str {
    match string_col_type(max_col_width_bytes) {
        FieldType::TinyBlob => "TINYBLOB",
        FieldType::Blob => "BLOB",
        FieldType::MediumBlob => "MEDIUMBLOB",
        _ => "LONGBLOB",
    }
}

/// `generateIndexName` (table_meta_data.cpp:22-30): text after the first
/// underscore, else the whole name, then "Index".
pub fn generate_index_name(col_name: &str) -> String {
    match col_name.find('_') {
        Some(pos) => format!("{}{}", &col_name[pos + 1..], INDEX_SUFFIX),
        None => format!("{}{}", col_name, INDEX_SUFFIX),
    }
}

/// `mysql_real_escape_string`'s character set: NUL, newline, carriage
/// return, backslash, quote, double quote and Ctrl-Z (helper_types.cpp:11-21
/// delegates to it).
pub fn escape_mysql_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\u{1a}' => out.push_str("\\Z"),
            _ => out.push(c),
        }
    }
    out
}

/// The description of one column (column_type.hpp:29-96).
#[derive(Clone, Debug)]
pub struct ColumnType {
    pub field_type: FieldType,
    /// Unsigned for numerics, binary charset for string/blob types.
    pub is_unsigned_or_binary: bool,
    /// Characters for text types, bytes otherwise.
    pub length: u32,
    pub default_value: String,
    pub on_update_cmd: String,
    pub is_auto_increment: bool,
}

impl ColumnType {
    /// The default constructor shape (MYSQL_TYPE_NULL, no flags).
    pub fn new() -> Self {
        Self {
            field_type: FieldType::Null,
            is_unsigned_or_binary: false,
            length: 0,
            default_value: String::new(),
            on_update_cmd: String::new(),
            is_auto_increment: false,
        }
    }

    /// ColumnType from a MYSQL_FIELD (column_type.hpp:59-71): length divided
    /// by charset width, BLOB remapped to the sized family by that length.
    pub fn from_field(field: &Field) -> Self {
        let mut field_type = field.field_type.unwrap_or(FieldType::Null);
        let width = field.charset_width.max(1);
        let length = field.length / width;
        if field_type == FieldType::Blob {
            field_type = string_col_type(length);
        }
        Self {
            field_type,
            is_unsigned_or_binary: derive_is_unsigned_or_binary(field),
            length,
            default_value: field.def.clone().unwrap_or_default(),
            on_update_cmd: String::new(),
            is_auto_increment: field.flags & AUTO_INCREMENT_FLAG != 0,
        }
    }

    /// Whether the type takes a DEFAULT clause (column_type.cpp:215-227) —
    /// the blob family does not.
    pub fn is_default_value_supported(&self) -> bool {
        !matches!(
            self.field_type,
            FieldType::TinyBlob | FieldType::MediumBlob | FieldType::LongBlob | FieldType::Blob
        )
    }

    /// String-like types (column_type.cpp:232-247).
    pub fn is_string_type(&self) -> bool {
        matches!(
            self.field_type,
            FieldType::TinyBlob
                | FieldType::MediumBlob
                | FieldType::LongBlob
                | FieldType::Blob
                | FieldType::VarString
                | FieldType::String
                | FieldType::VarChar
        )
    }

    /// Numerical types with a "0" default alias (column_type.cpp:249-266).
    pub fn is_simple_numerical_type(&self) -> bool {
        matches!(
            self.field_type,
            FieldType::Decimal
                | FieldType::NewDecimal
                | FieldType::Float
                | FieldType::Double
                | FieldType::Tiny
                | FieldType::Short
                | FieldType::Int24
                | FieldType::Long
                | FieldType::LongLong
        )
    }

    /// `getAsString` (column_type.cpp:17-191): the DDL fragment for CREATE
    /// TABLE / ALTER TABLE, ending in NOT NULL.
    pub fn get_as_string(&self, index_type: ColumnIndexType) -> String {
        let mut s = String::new();
        match self.field_type {
            FieldType::Tiny => numerical(&mut s, "TINYINT", self.is_unsigned_or_binary),
            FieldType::Short => numerical(&mut s, "SMALLINT", self.is_unsigned_or_binary),
            FieldType::Int24 => numerical(&mut s, "MEDIUMINT", self.is_unsigned_or_binary),
            FieldType::Long => numerical(&mut s, "INT", self.is_unsigned_or_binary),
            FieldType::LongLong => numerical(&mut s, "BIGINT", self.is_unsigned_or_binary),
            FieldType::Decimal | FieldType::NewDecimal => {
                numerical(&mut s, "DECIMAL", self.is_unsigned_or_binary)
            }
            FieldType::Float => s += "FLOAT",
            FieldType::Double => s += "DOUBLE",
            FieldType::Timestamp => s += "TIMESTAMP",
            FieldType::Time => s += "TIME",
            FieldType::NewDate => s += "DATE",
            FieldType::Year => s += &format!("YEAR({})", self.length),
            FieldType::VarChar => s += &format!("VARCHAR({})", self.length),
            FieldType::TinyBlob => s += &multi_len_type("TINY", self.is_unsigned_or_binary),
            FieldType::MediumBlob => s += &multi_len_type("MEDIUM", self.is_unsigned_or_binary),
            FieldType::LongBlob => s += &multi_len_type("LONG", self.is_unsigned_or_binary),
            FieldType::Blob => s += &multi_len_type("", self.is_unsigned_or_binary),
            FieldType::Bit => s += &format!("BIT({})", self.length),
            FieldType::VarString => {
                s += &format!(
                    "VAR{}({})",
                    char_type(self.is_unsigned_or_binary),
                    self.length
                )
            }
            FieldType::String => {
                s += &format!("{}({})", char_type(self.is_unsigned_or_binary), self.length)
            }
            // MF_ASSERT(false) in the reference; unreachable for live types.
            _ => panic!("ColumnType::getAsString: unsupported field type"),
        }

        if self.is_auto_increment {
            s += " AUTO_INCREMENT";
        } else if self.is_default_value_supported() && !self.default_value.is_empty() {
            s += " DEFAULT ";
            if self.is_string_type() {
                s += &format!("'{}'", escape_mysql_string(&self.default_value));
            } else {
                s += &self.default_value;
            }
        }

        if !self.on_update_cmd.is_empty() {
            s += " ON UPDATE ";
            s += &self.on_update_cmd;
        }

        if index_type == ColumnIndexType::Primary {
            s += " PRIMARY KEY";
        }

        s += " NOT NULL";
        s
    }
}

impl Default for ColumnType {
    fn default() -> Self {
        Self::new()
    }
}

fn numerical(s: &mut String, name: &str, is_unsigned: bool) {
    *s += name;
    if is_unsigned {
        *s += " UNSIGNED";
    }
}

fn char_type(is_binary: bool) -> &'static str {
    if is_binary { "BINARY" } else { "CHAR" }
}

fn multi_len_type(prefix: &'static str, is_binary: bool) -> String {
    format!("{prefix}{}", if is_binary { "BLOB" } else { "TEXT" })
}

/// `deriveIsUnsignedOrBinary` (column_type.cpp:366-396): UNSIGNED_FLAG for
/// numerics, charset 63 ("binary") for string/blob types.
pub fn derive_is_unsigned_or_binary(field: &Field) -> bool {
    match field.field_type {
        Some(FieldType::Tiny)
        | Some(FieldType::Short)
        | Some(FieldType::Int24)
        | Some(FieldType::Long)
        | Some(FieldType::LongLong) => field.flags & UNSIGNED_FLAG != 0,
        Some(FieldType::TinyBlob)
        | Some(FieldType::MediumBlob)
        | Some(FieldType::LongBlob)
        | Some(FieldType::Blob)
        | Some(FieldType::VarString)
        | Some(FieldType::String) => field.charsetnr == 63,
        _ => false,
    }
}

/// The equality contract (column_type.cpp:272-359): not all members are
/// relevant. Integer lengths, onUpdateCmd and (for several families) the
/// default text are deliberately not compared.
impl PartialEq for ColumnType {
    fn eq(&self, other: &Self) -> bool {
        if self.field_type != other.field_type {
            return false;
        }
        let optional_stuff_equal = match self.field_type {
            FieldType::Decimal
            | FieldType::NewDecimal
            | FieldType::Float
            | FieldType::Double
            | FieldType::Timestamp
            | FieldType::Time
            | FieldType::NewDate => true,
            FieldType::Tiny
            | FieldType::Short
            | FieldType::Int24
            | FieldType::Long
            | FieldType::LongLong => {
                self.is_unsigned_or_binary == other.is_unsigned_or_binary
                    && self.is_auto_increment == other.is_auto_increment
            }
            FieldType::TinyBlob | FieldType::MediumBlob | FieldType::LongBlob | FieldType::Blob => {
                self.is_unsigned_or_binary == other.is_unsigned_or_binary
            }
            FieldType::VarString | FieldType::String => {
                self.length == other.length
                    && self.is_unsigned_or_binary == other.is_unsigned_or_binary
            }
            FieldType::Year | FieldType::VarChar => self.length == other.length,
            _ => true,
        };
        if !optional_stuff_equal || self.is_auto_increment != other.is_auto_increment {
            return false;
        }
        self.default_values_equal(other)
    }
}

impl ColumnType {
    /// `equalDefaultValue` (column_type.cpp:316-352): auto-increment and
    /// blob columns skip the check; numerical "" vs "0" alias; STRING "" vs
    /// the null-string of the column's length; TIMESTAMP always equal.
    fn default_values_equal(&self, other: &Self) -> bool {
        if self.is_auto_increment || !self.is_default_value_supported() {
            return true;
        }
        if self.default_value == other.default_value {
            return true;
        }
        if self.is_simple_numerical_type() {
            return (self.default_value.is_empty() && other.default_value == "0")
                || (self.default_value == "0" && other.default_value.is_empty());
        }
        if self.field_type == FieldType::String {
            let fill = if self.is_unsigned_or_binary {
                '\0'
            } else {
                ' '
            };
            let null_string: String = std::iter::repeat_n(fill, self.length as usize).collect();
            return (self.default_value.is_empty() && other.default_value == null_string)
                || (self.default_value == null_string && other.default_value.is_empty());
        }
        self.field_type == FieldType::Timestamp
    }
}

/// constants.hpp free-standing column type helpers: the parentID column and
/// the auto-increment id column (column_type.hpp:144-146).
pub fn parentid_column_type() -> ColumnType {
    ColumnType {
        field_type: FieldType::LongLong,
        ..ColumnType::new()
    }
}

/// column_type.hpp:145-146 — auto-increment column.
pub fn id_column_type() -> ColumnType {
    ColumnType {
        field_type: FieldType::LongLong,
        is_auto_increment: true,
        ..ColumnType::new()
    }
}

/// One live index under a column (table_meta_data.cpp:41-86); the
/// uniqueness flag is carried by the column flags, the name is what the
/// derivation reads.
#[derive(Clone, Debug)]
struct TableIndex {
    column_name: String,
}

/// `TableIndices` (table_meta_data.cpp:95-153): column name -> index.
#[derive(Clone, Debug, Default)]
pub struct TableIndices {
    column_to_index: BTreeMap<String, TableIndex>,
}

impl TableIndices {
    /// Built from the SHOW INDEX rows; `non_unique == "0"` is unique.
    pub fn from_rows(rows: &[IndexRow]) -> Self {
        let mut column_to_index = BTreeMap::new();
        for row in rows {
            column_to_index.insert(
                row.column_name.clone(),
                TableIndex {
                    column_name: row.key_name.clone(),
                },
            );
        }
        Self { column_to_index }
    }

    pub fn index_name(&self, col_name: &str) -> Option<&str> {
        self.column_to_index
            .get(col_name)
            .map(|index| index.column_name.as_str())
    }
}

/// One column as read from the live table (table_meta_data.hpp:25-39).
#[derive(Clone, Debug)]
pub struct ColumnInfo {
    pub column_type: ColumnType,
    pub index_type: ColumnIndexType,
}

impl ColumnInfo {
    /// From a MYSQL_FIELD plus the table's index rows.
    pub fn from_field(field: &Field, indices: &TableIndices, log: &mut Vec<String>) -> Self {
        Self {
            column_type: ColumnType::from_field(field),
            index_type: derive_index_type(field, indices, log),
        }
    }

    /// `isIndexEqual` (table_meta_data.cpp:179-197): externally created
    /// indexes count as BW-generated ones.
    pub fn is_index_equal(&self, other: &ColumnInfo) -> bool {
        if self.index_type == ColumnIndexType::External {
            return matches!(
                other.index_type,
                ColumnIndexType::Unique | ColumnIndexType::NonUnique
            );
        }
        if other.index_type == ColumnIndexType::External {
            return matches!(
                self.index_type,
                ColumnIndexType::Unique | ColumnIndexType::NonUnique
            );
        }
        self.index_type == other.index_type
    }
}

/// `deriveIndexType` (table_meta_data.cpp:204-253).
pub fn derive_index_type(
    field: &Field,
    indices: &TableIndices,
    log: &mut Vec<String>,
) -> ColumnIndexType {
    if field.flags & PRI_KEY_FLAG != 0 {
        return ColumnIndexType::Primary;
    }
    if field.flags & UNIQUE_KEY_FLAG != 0 {
        // MF_ASSERT in the reference: a flagged column must have an index.
        let index_name = indices
            .index_name(&field.name)
            .unwrap_or_else(|| panic!("deriveIndexType: no index for column {}", field.name));
        if index_name == generate_index_name(&field.name) {
            return ColumnIndexType::Unique;
        }
        log.push(format!(
            "TableMetaData::ColumnInfo::deriveIndexType: Found unknown unique index {} for column {}\n",
            index_name, field.name
        ));
        return ColumnIndexType::External;
    }
    if field.flags & MULTIPLE_KEY_FLAG != 0 {
        let index_name = indices
            .index_name(&field.name)
            .unwrap_or_else(|| panic!("deriveIndexType: no index for column {}", field.name));
        if index_name == PARENTID_INDEX_NAME {
            return ColumnIndexType::ParentId;
        }
        if index_name == generate_index_name(&field.name) {
            return ColumnIndexType::NonUnique;
        }
        log.push(format!(
            "TableMetaData::ColumnInfo::deriveIndexType: Found unknown multiple key index {} for column {}\n",
            index_name, field.name
        ));
        return ColumnIndexType::External;
    }
    ColumnIndexType::None
}

/// `IndexedColumnInfo` (table_meta_data.hpp:44-52): the desired column plus
/// the index kind it used to have.
#[derive(Clone, Debug)]
pub struct IndexedColumnInfo {
    pub column_info: ColumnInfo,
    pub old_index_type: ColumnIndexType,
}

/// The classify output (table_inspector.cpp:122-165). `obsolete` and `new`
/// are the residues of the input maps after matched columns were removed.
#[derive(Clone, Debug, Default)]
pub struct TableDiff {
    pub obsolete: BTreeMap<String, ColumnInfo>,
    pub new: BTreeMap<String, ColumnInfo>,
    pub updated: BTreeMap<String, ColumnInfo>,
    pub indexed: BTreeMap<String, IndexedColumnInfo>,
}

/// `classifyColumns`: matched columns leave both maps; type mismatches go to
/// `updated`, index mismatches to `indexed` (with the old index kind).
pub fn classify_columns(
    old: &mut BTreeMap<String, ColumnInfo>,
    new: &mut BTreeMap<String, ColumnInfo>,
) -> TableDiff {
    let mut diff = TableDiff::default();
    let old_keys: Vec<String> = old.keys().cloned().collect();
    for key in old_keys {
        let Some(new_col) = new.get(&key) else {
            continue;
        };
        let old_col = &old[&key];
        if new_col.column_type != old_col.column_type {
            diff.updated.insert(key.clone(), new_col.clone());
        }
        if !new_col.is_index_equal(old_col) {
            diff.indexed.insert(
                key.clone(),
                IndexedColumnInfo {
                    column_info: new_col.clone(),
                    old_index_type: old_col.index_type,
                },
            );
        }
        old.remove(&key);
        new.remove(&key);
    }
    diff
}

/// `getTableColumns` (table_meta_data.cpp:286-300): the live column map;
/// empty when the table does not exist.
pub fn table_columns(
    described: &DescribedTable,
    log: &mut Vec<String>,
) -> BTreeMap<String, ColumnInfo> {
    let mut columns = BTreeMap::new();
    if described.fields.is_empty() {
        return columns;
    }
    let indices = TableIndices::from_rows(&described.indices);
    for field in &described.fields {
        columns.insert(
            field.name.clone(),
            ColumnInfo::from_field(field, &indices, log),
        );
    }
    columns
}

/// The per-table callbacks (table_inspector.hpp:66-123). `db` travels with
/// the call because the reference handlers hold the connection.
pub trait TableHandler {
    /// Existing table that still matches the definitions. `Err` carries a
    /// DatabaseException text the way the reference lets it unwind.
    fn on_existing_table(
        &mut self,
        _db: &mut dyn SyncDatabase,
        _log: &mut Vec<String>,
        _table: &str,
    ) -> Result<bool, String> {
        Ok(true)
    }

    /// A table the definitions want but the database lacks.
    fn on_need_new_table(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table: &str,
        columns: &BTreeMap<String, ColumnInfo>,
    ) -> Result<bool, String>;

    /// An existing table whose columns need surgery.
    fn on_need_update_table(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        update: &TableUpdate,
    ) -> Result<bool, String>;

    /// Entity tables left in the database but not in the definitions.
    fn on_need_delete_tables(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        tables: &BTreeSet<String>,
    ) -> Result<bool, String>;
}

/// The four diff buckets handed to [`TableHandler::on_need_update_table`].
pub struct TableUpdate<'a> {
    pub table: &'a str,
    pub obsolete: &'a BTreeMap<String, ColumnInfo>,
    pub new: &'a BTreeMap<String, ColumnInfo>,
    pub updated: &'a BTreeMap<String, ColumnInfo>,
    pub indexed: &'a BTreeMap<String, IndexedColumnInfo>,
}

/// `TableInspector` (table_inspector.cpp:26-104): the visiting driver,
/// tracking the visited names and the synchronised flag.
#[derive(Default)]
pub struct Inspector {
    pub visited: BTreeSet<String>,
    pub is_synced: bool,
}

impl Inspector {
    pub fn new() -> Self {
        Self {
            visited: BTreeSet::new(),
            is_synced: true,
        }
    }

    /// `onVisitTable`: duplicate names throw "table %s requested twice".
    pub fn visit_table(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        handler: &mut dyn TableHandler,
        table: &str,
        desired: &BTreeMap<String, ColumnInfo>,
    ) -> Result<(), String> {
        if !self.visited.insert(table.to_string()) {
            return Err(format!("table {table} requested twice"));
        }

        let described = db.describe_table(table);
        if described.fields.is_empty() {
            self.is_synced &= handler.on_need_new_table(db, log, table, desired)?;
        } else {
            self.is_synced &= handler.on_existing_table(db, log, table)?;

            let mut old = table_columns(&described, log);
            let mut new = desired.clone();
            let diff = classify_columns(&mut old, &mut new);
            if !old.is_empty()
                || !new.is_empty()
                || !diff.updated.is_empty()
                || !diff.indexed.is_empty()
            {
                self.is_synced &= handler.on_need_update_table(
                    db,
                    log,
                    &TableUpdate {
                        table,
                        obsolete: &old,
                        new: &new,
                        updated: &diff.updated,
                        indexed: &diff.indexed,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// `deleteUnvisitedTables` (table_inspector.cpp:79-104): the entity
    /// tables (`tbl_%`) not visited this run. Returns whether all obsolete
    /// tables were handled.
    pub fn delete_unvisited_tables(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        handler: &mut dyn TableHandler,
    ) -> Result<bool, String> {
        let existing: BTreeSet<String> = db
            .table_names(&format!("{TABLE_NAME_PREFIX}_%"))
            .into_iter()
            .collect();
        let obsolete: BTreeSet<String> = existing.difference(&self.visited).cloned().collect();
        let mut is_deleted = true;
        if !obsolete.is_empty() {
            is_deleted = handler.on_need_delete_tables(db, log, &obsolete)?;
            if !is_deleted {
                self.is_synced = false;
            }
        }
        Ok(is_deleted)
    }
}

/// One entity table the definitions want, with its desired columns.
#[derive(Clone, Debug, Default)]
pub struct DesiredTable {
    pub name: String,
    pub columns: BTreeMap<String, ColumnInfo>,
}

impl DesiredTable {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            columns: BTreeMap::new(),
        }
    }

    pub fn with_column(mut self, name: &str, info: ColumnInfo) -> Self {
        self.columns.insert(name.to_string(), info);
        self
    }
}

/// `AlterTableHelper` (mysql_table_initialiser.cpp:22-153): collect the
/// column additions, drops and modifications into one ALTER TABLE.
pub struct AlterTableHelper {
    table_name: String,
    num_additions: u32,
    num_drops: u32,
    num_modifications: u32,
    sql: String,
}

impl AlterTableHelper {
    pub fn new(table_name: &str) -> Self {
        Self {
            table_name: table_name.to_string(),
            num_additions: 0,
            num_drops: 0,
            num_modifications: 0,
            sql: format!("ALTER TABLE {table_name}"),
        }
    }

    pub fn num_altered_items(&self) -> u32 {
        self.num_additions + self.num_drops + self.num_modifications
    }

    pub fn add_column(&mut self, column_name: &str, column_type: &str) {
        if self.num_altered_items() > 0 {
            self.sql.push(',');
        }
        self.sql
            .push_str(&format!(" ADD COLUMN {column_name} {column_type}"));
        self.num_additions += 1;
    }

    pub fn drop_column(&mut self, column_name: &str) {
        if self.num_altered_items() > 0 {
            self.sql.push(',');
        }
        self.sql.push_str(&format!(" DROP COLUMN {column_name}"));
        self.num_drops += 1;
    }

    pub fn modify_column(&mut self, column_name: &str, column_type: &str) {
        if self.num_altered_items() > 0 {
            self.sql.push(',');
        }
        self.sql
            .push_str(&format!(" MODIFY COLUMN {column_name} {column_type}"));
        self.num_modifications += 1;
    }

    /// Executes the ALTER when anything was collected; returns the count.
    pub fn execute(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
    ) -> Result<u32, String> {
        if self.num_altered_items() > 0 {
            log.push(format!(
                "\tAdding {} columns into table {}\n",
                self.num_additions, self.table_name
            ));
            log.push(format!(
                "\tDeleting {} columns from table {}\n",
                self.num_drops, self.table_name
            ));
            log.push(format!(
                "\tUpdating {} columns from table {}\n",
                self.num_modifications, self.table_name
            ));
            log.push(format!("SQL: {}\n", self.sql));
            db.execute(&self.sql)?;
        }
        Ok(self.num_altered_items())
    }
}

/// `TableInitialiser` (mysql_table_initialiser.cpp:163-662): performs the
/// table surgery the inspector asks for.
pub struct TableInitialiser {
    pub allow_new: bool,
    pub character_set: String,
    pub collation: String,
}

impl TableInitialiser {
    pub fn new(allow_new: bool, character_set: String, collation: String) -> Self {
        Self {
            allow_new,
            character_set,
            collation,
        }
    }

    /// `generateCreateIndexStatement` (mysql_table_initialiser.cpp:177-236).
    /// Indexes on non-VAR_STRING columns cap at the first 255 characters,
    /// with the verbatim warning.
    pub fn generate_create_index_statement(
        &self,
        log: &mut Vec<String>,
        table_name: &str,
        col_name: &str,
        col_info: &ColumnInfo,
    ) -> String {
        let index_name = generate_index_name(col_name);
        let mut s = String::from("CREATE ");
        if col_info.index_type == ColumnIndexType::Unique {
            s += "UNIQUE ";
        }
        s += &format!("INDEX {index_name} ON {table_name} ({col_name}");

        let mut index_length = col_info.column_type.length;
        let field_type = col_info.column_type.field_type;
        if field_type != FieldType::VarString && index_length > 255 {
            log.push(format!(
                "Column {} on table {} can only be indexed up to the first 255 characters (column is of length {}).\n",
                col_name, table_name, col_info.column_type.length
            ));
            index_length = 255;
        }

        match field_type {
            FieldType::VarChar
            | FieldType::TinyBlob
            | FieldType::MediumBlob
            | FieldType::LongBlob
            | FieldType::Blob
            | FieldType::VarString
            | FieldType::String => s += &format!("({index_length})"),
            _ => {}
        }

        s += ")";
        s
    }

    /// `createIndex` (mysql_table_initialiser.cpp:244-300).
    pub fn create_index(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table_name: &str,
        col_name: &str,
        col_info: &ColumnInfo,
    ) -> Result<(), String> {
        match col_info.index_type {
            ColumnIndexType::None => {}
            ColumnIndexType::Primary => {
                // Created with the table; cannot be added afterwards.
            }
            ColumnIndexType::Unique | ColumnIndexType::NonUnique => {
                let sql = self.generate_create_index_statement(log, table_name, col_name, col_info);
                if let Err(db_exception) = db.execute(&sql) {
                    if col_info.index_type != ColumnIndexType::NonUnique {
                        log.push(format!(
                            "TableInitialiser::createIndex: Failed to create unique index on column '{}.{}'. Please ensure that all values in the column are unique before attempting to create a name index.\n",
                            table_name, col_name
                        ));
                    } else {
                        log.push(format!(
                            "TableInitialiser::createIndex: Failed to create non-unique index on column '{}.{}: {}'.\n",
                            table_name, col_name, db_exception
                        ));
                    }
                    return Err(db_exception);
                }
            }
            ColumnIndexType::ParentId => {
                db.execute(&format!(
                    "CREATE INDEX {PARENTID_INDEX_NAME} ON {table_name} ({col_name})"
                ))?;
            }
            // Can only be detected not created.
            _ => panic!("createEntityTableIndex: Unknown index type 5\n"),
        }
        Ok(())
    }

    /// `removeIndex` (mysql_table_initialiser.cpp:311-351): failures are
    /// logged and swallowed.
    pub fn remove_index(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table_name: &str,
        col_name: &str,
        index_type: ColumnIndexType,
    ) {
        let statement = match index_type {
            ColumnIndexType::None | ColumnIndexType::Primary | ColumnIndexType::External => None,
            ColumnIndexType::Unique | ColumnIndexType::NonUnique => Some(format!(
                "ALTER TABLE {table_name} DROP INDEX {}",
                generate_index_name(col_name)
            )),
            ColumnIndexType::ParentId => Some(format!(
                "ALTER TABLE {table_name} DROP INDEX {PARENTID_INDEX_NAME}"
            )),
        };
        if let Some(statement) = statement
            && let Err(e) = db.execute(&statement)
        {
            log.push(format!("TableInitialiser::removeIndex: {e}\n"));
        }
    }

    /// `onNeedNewTable` (mysql_table_initialiser.cpp:357-403).
    fn on_need_new_table_impl(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table_name: &str,
        columns: &BTreeMap<String, ColumnInfo>,
    ) -> Result<bool, String> {
        if !self.allow_new {
            log.push(format!("\tWitholding table creation: {table_name}\n"));
            return Ok(true);
        }

        log.push(format!("\tCreating table {table_name}\n"));
        db.execute(&format!(
            "CREATE TABLE IF NOT EXISTS {table_name} (id BIGINT AUTO_INCREMENT, PRIMARY KEY idKey (id)) ENGINE={MYSQL_ENGINE_TYPE}"
        ))?;

        // A table cannot be created with no columns, so the id column is
        // created up front and removed later if the definitions lack it.
        let mut new_columns = columns.clone();
        let delete_id_col = new_columns.remove(ID_COLUMN_NAME).is_none();

        let mut helper = AlterTableHelper::new(table_name);
        self.add_columns(&mut helper, &new_columns);

        if delete_id_col {
            helper.drop_column(ID_COLUMN_NAME);
        }

        if helper.execute(db, log)? > 0 {
            self.initialise_columns(db, log, table_name, &new_columns, true)?;
        }

        Ok(true)
    }

    /// `onExistingTable` (mysql_table_initialiser.cpp:409-430): convert the
    /// character set when configured.
    fn on_existing_table_impl(
        &self,
        db: &mut dyn SyncDatabase,
        _log: &mut Vec<String>,
        table_name: &str,
    ) -> Result<bool, String> {
        if self.character_set.is_empty() {
            return Ok(true);
        }
        let mut s = format!(
            "ALTER TABLE {table_name} CONVERT TO CHARACTER SET '{}'",
            escape_mysql_string(&self.character_set)
        );
        if !self.collation.is_empty() {
            s += &format!(" COLLATE '{}'", escape_mysql_string(&self.collation));
        }
        db.execute(&s)?;
        Ok(true)
    }

    /// `onNeedUpdateTable` (mysql_table_initialiser.cpp:436-455).
    fn on_need_update_table_impl(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        update: &TableUpdate,
    ) -> Result<bool, String> {
        let table_name = update.table;
        let mut helper = AlterTableHelper::new(table_name);

        if self.allow_new {
            for (name, info) in update.new {
                log.push(format!(
                    "\tPreparing to update table {table_name}, adding column {name}\n"
                ));
                helper.add_column(name, &info.column_type.get_as_string(info.index_type));
            }
        }
        for (name, info) in update.obsolete {
            log.push(format!(
                "\tPreparing to update table {table_name}, deleting column {name}\n"
            ));
            self.remove_index(db, log, table_name, name, info.index_type);
            helper.drop_column(name);
        }
        for (name, info) in update.updated {
            let column_type_str = info.column_type.get_as_string(info.index_type);
            log.push(format!(
                "\tPreparing to update table {table_name}, modifying type of column {name} to {column_type_str}\n"
            ));
            helper.modify_column(name, &column_type_str);
        }
        helper.execute(db, log)?;

        self.index_columns(db, log, table_name, update.indexed)?;

        Ok(true)
    }

    /// `initialiseColumns` (mysql_table_initialiser.cpp:466-477).
    pub fn initialise_columns(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table_name: &str,
        columns: &BTreeMap<String, ColumnInfo>,
        should_apply_default_value: bool,
    ) -> Result<(), String> {
        for (name, info) in columns {
            self.initialise_column(db, log, table_name, name, info, should_apply_default_value)?;
        }
        Ok(())
    }

    /// `initialiseColumn` (mysql_table_initialiser.cpp:492-512): create the
    /// index, then backfill the default for types MySQL cannot DEFAULT.
    pub fn initialise_column(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table_name: &str,
        column_name: &str,
        column_info: &ColumnInfo,
        should_apply_default_value: bool,
    ) -> Result<(), String> {
        self.create_index(db, log, table_name, column_name, column_info)?;

        if should_apply_default_value && !column_info.column_type.is_default_value_supported() {
            let statement = format!(
                "UPDATE {table_name} SET {column_name}='{}'",
                escape_mysql_string(&column_info.column_type.default_value)
            );
            db.execute(&statement)?;
        }
        Ok(())
    }

    /// `onNeedDeleteTables` (mysql_table_initialiser.cpp:518-529).
    fn on_need_delete_tables_impl(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table_names: &BTreeSet<String>,
    ) -> Result<bool, String> {
        for name in table_names {
            log.push(format!("\tDeleting table {name}\n"));
            db.execute(&format!("DROP TABLE {name}"))?;
        }
        Ok(true)
    }

    /// `addColumns` (mysql_table_initialiser.cpp:540-561).
    fn add_columns(&self, helper: &mut AlterTableHelper, columns: &BTreeMap<String, ColumnInfo>) {
        for (name, info) in columns {
            helper.add_column(name, &info.column_type.get_as_string(info.index_type));
        }
    }

    /// `indexColumns` (mysql_table_initialiser.cpp:638-662).
    fn index_columns(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table_name: &str,
        columns: &BTreeMap<String, IndexedColumnInfo>,
    ) -> Result<(), String> {
        for (name, info) in columns {
            let is_create = info.column_info.index_type != ColumnIndexType::None;
            log.push(format!(
                "\tPreparing to {} index for table {table_name} column {name}\n",
                if is_create { "add" } else { "delete" }
            ));
            if is_create {
                self.create_index(db, log, table_name, name, &info.column_info)?;
            } else {
                self.remove_index(db, log, table_name, name, info.old_index_type);
            }
        }
        Ok(())
    }
}

impl TableHandler for TableInitialiser {
    fn on_need_new_table(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table: &str,
        columns: &BTreeMap<String, ColumnInfo>,
    ) -> Result<bool, String> {
        self.on_need_new_table_impl(db, log, table, columns)
    }

    fn on_existing_table(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table: &str,
    ) -> Result<bool, String> {
        self.on_existing_table_impl(db, log, table)
    }

    fn on_need_update_table(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        update: &TableUpdate,
    ) -> Result<bool, String> {
        self.on_need_update_table_impl(db, log, update)
    }

    fn on_need_delete_tables(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        tables: &BTreeSet<String>,
    ) -> Result<bool, String> {
        self.on_need_delete_tables_impl(db, log, tables)
    }
}

/// `TableValidator` (table_inspector.cpp:168-243): report the differences
/// without touching the database.
#[derive(Default)]
pub struct TableValidator;

impl TableValidator {
    pub fn new() -> Self {
        Self
    }
}

impl TableHandler for TableValidator {
    fn on_need_new_table(
        &mut self,
        _db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        table: &str,
        _columns: &BTreeMap<String, ColumnInfo>,
    ) -> Result<bool, String> {
        log.push(format!("\tRequire table {table}\n"));
        Ok(false)
    }

    fn on_need_update_table(
        &mut self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        update: &TableUpdate,
    ) -> Result<bool, String> {
        let table = update.table;
        for name in update.new.keys() {
            log.push(format!("\tNeed to add column {name} into table {table}\n"));
        }
        for name in update.obsolete.keys() {
            log.push(format!(
                "\tNeed to delete column {name} from table {table}\n"
            ));
        }
        for (name, info) in update.updated {
            log.push(format!(
                "\tNeed to update column {name} in table {table} to {}\n",
                info.column_type.get_as_string(info.index_type)
            ));
        }
        for (name, info) in update.indexed {
            log.push(format!(
                "\tNeed to {} index to column {name} in table {table}\n",
                if info.column_info.index_type == ColumnIndexType::None {
                    "delete"
                } else {
                    "add"
                }
            ));
        }
        let _ = db;
        Ok(false)
    }

    fn on_need_delete_tables(
        &mut self,
        _db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        tables: &BTreeSet<String>,
    ) -> Result<bool, String> {
        for name in tables {
            log.push(format!("Need to remove table {name}\n"));
        }
        Ok(false)
    }
}

/// `isEntityTablesInSync`-shaped driver: report-only pass over the desired
/// tables. Duplicates surface as `Err("table %s requested twice")`.
pub fn validate_entity_tables(
    db: &mut dyn SyncDatabase,
    log: &mut Vec<String>,
    tables: &[DesiredTable],
) -> Result<bool, String> {
    let mut validator = TableValidator::new();
    let mut scan = Inspector::new();
    for table in tables {
        scan.visit_table(db, log, &mut validator, &table.name, &table.columns)?;
    }
    scan.delete_unvisited_tables(db, log, &mut validator)?;
    Ok(scan.is_synced)
}

/// The twelve special BigWorld statements (mysql_synchronise.cpp:266-349),
/// in creation order. `max_space_data_size` selects the blob family for
/// bigworldSpaceData and its MODIFY backstop.
pub fn special_bigworld_table_statements(max_space_data_size: u32) -> Vec<String> {
    const { assert!(MAX_SECONDARY_DB_LOCATION_LENGTH < 1 << 16) };
    let blob_type_name = string_col_type_str(max_space_data_size);
    vec![
        format!("CREATE TABLE IF NOT EXISTS bigworldInfo (version INT UNSIGNED NOT NULL, snapshotTime TIMESTAMP NULL, isPasswordHashed BOOL NOT NULL DEFAULT FALSE) ENGINE={MYSQL_ENGINE_TYPE}"),
        "CREATE TABLE IF NOT EXISTS bigworldEntityTypes (typeID INT NOT NULL AUTO_INCREMENT, bigworldID INT, name CHAR(255) CHARACTER SET latin1 NOT NULL UNIQUE,  PRIMARY KEY(typeID), KEY(bigworldID)) ENGINE=InnoDB".to_string(),
        "CREATE TABLE IF NOT EXISTS bigworldLogOns (databaseID BIGINT NOT NULL, typeID INT NOT NULL, objectID INT, ip INT UNSIGNED, port SMALLINT UNSIGNED, salt SMALLINT UNSIGNED, shouldAutoLoad BOOL NOT NULL DEFAULT FALSE, PRIMARY KEY(typeID, databaseID)) ENGINE=InnoDB".to_string(),
        format!("CREATE TABLE IF NOT EXISTS bigworldLogOnMapping (logOnName VARBINARY({BW_MYSQL_MAX_LOG_ON_NAME_LEN}) NOT NULL, password VARBINARY({BW_MYSQL_MAX_LOG_ON_PASSWORD_LEN}) NOT NULL, entityType INT, entityID BIGINT, PRIMARY KEY(logOnName)) ENGINE={MYSQL_ENGINE_TYPE}"),
        format!("CREATE TABLE IF NOT EXISTS bigworldNewID (id INT NOT NULL) ENGINE={MYSQL_ENGINE_TYPE}"),
        format!("CREATE TABLE IF NOT EXISTS bigworldUsedIDs (id INT NOT NULL) ENGINE={MYSQL_ENGINE_TYPE}"),
        format!("CREATE TABLE IF NOT EXISTS bigworldGameTime (time INT NOT NULL) ENGINE={MYSQL_ENGINE_TYPE}"),
        format!("CREATE TABLE IF NOT EXISTS bigworldSpaces (id INT NOT NULL UNIQUE) ENGINE={MYSQL_ENGINE_TYPE}"),
        format!("CREATE TABLE IF NOT EXISTS bigworldSpaceData (id INT NOT NULL, INDEX (id), spaceEntryID BIGINT NOT NULL, entryKey SMALLINT UNSIGNED NOT NULL, data {blob_type_name} NOT NULL ) ENGINE={MYSQL_ENGINE_TYPE}"),
        format!("ALTER TABLE bigworldSpaceData MODIFY data {blob_type_name}"),
        format!("CREATE TABLE IF NOT EXISTS bigworldSecondaryDatabases (ip INT UNSIGNED NOT NULL, port SMALLINT UNSIGNED NOT NULL, location BLOB NOT NULL, INDEX addr (ip, port)) ENGINE={MYSQL_ENGINE_TYPE}"),
        format!("CREATE TABLE IF NOT EXISTS bigworldEntityDefsChecksum (checksum CHAR(255) CHARACTER SET latin1) ENGINE={MYSQL_ENGINE_TYPE}"),
    ]
}

/// The `MySql::EngineSupportState` vocabulary (wrapper.hpp:44-49).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EngineSupportState {
    #[default]
    Default,
    Supported,
    NotSupported,
}

/// The settings `doSynchronise` consumes (bw.xml mysql/unicodeString +
/// billingSystem/isPasswordHashed + mysql/maxSpaceDataSize).
#[derive(Clone, Debug)]
pub struct SyncConfig {
    pub character_set: String,
    pub collation: String,
    pub is_password_hashed: bool,
    pub max_space_data_size: u32,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            character_set: DEFAULT_CHARACTER_SET.to_string(),
            collation: DEFAULT_COLLATION.to_string(),
            is_password_hashed: true,
            max_space_data_size: 2048,
        }
    }
}

/// The failure vocabulary of one synchronisation run; `error_message()`
/// reproduces the reference ERROR_MSG texts verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncFailure {
    EngineNotSupported,
    TableEngines,
    UnconsolidatedSecondaryDatabases,
    NewerVersion { current: u32, found: u32 },
    Charset(String),
    RemovePasswordHashing,
    Exception(String),
}

impl SyncFailure {
    pub fn error_message(&self) -> String {
        match self {
            Self::EngineNotSupported => format!(
                "MySqlSynchronise::doSynchronise: {MYSQL_ENGINE_TYPE} is not a supported MySQL storage engine. "
            ),
            Self::TableEngines => format!(
                "MySqlSynchronise::doSynchronise: One or more tables are not using the {MYSQL_ENGINE_TYPE} engine type\n"
            ),
            Self::UnconsolidatedSecondaryDatabases => {
                "Cannot sync database while there are unconsolidated secondary databases.\nPlease revert any changes to entity definitions and run the data consolidation tool.\nAlternatively, run \"consolidate_dbs --clear\" to clear secondary database information, unconsolidated data will be lost.\n".to_string()
            }
            Self::NewerVersion { current, found } => format!(
                "Unable to upgrade the database to version {current}, a newer DB version ({found}) is already in use.\n"
            ),
            Self::Charset(e) => format!(
                "Invalid characterSet or collation setting in 'bw.xml'\n{e}\n"
            ),
            Self::RemovePasswordHashing => {
                "MySqlSynchronise::updatePasswordHash: Cannot remove hashing. Manually update bigworldInfo.isPasswordHashed and bigworldLogOnMapping.password fields.\n".to_string()
            }
            Self::Exception(e) => format!("{e}\n"),
        }
    }
}

/// The MySQL wire seam the synchronisation drives. Statement text (with the
/// reference's `?` placeholders kept) is the reviewable artifact.
pub trait SyncDatabase {
    /// One statement; `Err` is the DatabaseException text.
    fn execute(&mut self, statement: &str) -> Result<(), String>;

    /// COMMIT the open transaction.
    fn commit(&mut self);

    /// `checkEngineSupportState`.
    fn engine_support_state(&mut self) -> EngineSupportState;

    /// `checkTableEngines`: alter wrong-engine tables, false on failure.
    fn check_table_engines(&mut self) -> bool;

    /// `getTableNames` for an SQL LIKE pattern.
    fn table_names(&mut self, pattern: &str) -> Vec<String>;

    /// `MySqlTableMetadata` + `SHOW INDEX` fields (empty when missing).
    fn describe_table(&mut self, table: &str) -> DescribedTable;

    /// `getBigWorldDBVersion`'s SELECT; `None` when there is no row.
    fn query_version(&mut self) -> Option<u32>;

    /// `getIsPasswordHashed`'s SELECT; `None` when there is no row.
    fn query_is_password_hashed(&mut self) -> Option<bool>;

    /// `numSecondaryDBs`.
    fn count_secondary_databases(&mut self) -> u32;

    /// `MySqlUpgradeDatabase::run`; the ladder content is another face.
    fn upgrade_database(&mut self, from_version: u32, log: &mut Vec<String>) -> Result<(), String> {
        let _ = (from_version, log);
        Ok(())
    }
}

/// `getBigWorldDBVersion` (table_inspector.cpp:298-316): a missing row means
/// a freshly created database — record the current version and use it.
pub fn read_or_record_version(
    db: &mut dyn SyncDatabase,
    _log: &mut Vec<String>,
) -> Result<u32, SyncFailure> {
    match db.query_version() {
        Some(version) => Ok(version),
        None => {
            db.execute(&format!(
                "INSERT INTO bigworldInfo (version) VALUES ({DBAPP_CURRENT_VERSION})"
            ))
            .map_err(SyncFailure::Exception)?;
            Ok(DBAPP_CURRENT_VERSION)
        }
    }
}

/// `getIsPasswordHashed` (table_inspector.cpp:322-336); the MF_ASSERT on the
/// row count becomes the `None` case.
pub fn read_is_password_hashed(db: &mut dyn SyncDatabase) -> Result<bool, SyncFailure> {
    db.query_is_password_hashed()
        .ok_or_else(|| SyncFailure::Exception("assert failed: resultSet.numRows() > 0".to_string()))
}

/// `MySqlSynchronise`'s driver. `tables` is the desired schema derived from
/// the entity definitions (property-mapping face).
pub struct Synchronise {
    pub config: SyncConfig,
    pub is_dry_run: bool,
}

impl Synchronise {
    pub fn new(config: SyncConfig, is_dry_run: bool) -> Self {
        Self { config, is_dry_run }
    }

    /// `run` (mysql_synchronise.cpp:51-65): wrap the exception handling.
    pub fn run(
        &self,
        db: &mut dyn SyncDatabase,
        tables: &[DesiredTable],
        log: &mut Vec<String>,
    ) -> bool {
        match self.do_synchronise(db, tables, log) {
            Ok(is_ok) => is_ok,
            Err(failure) => {
                log.push(failure.error_message());
                false
            }
        }
    }

    /// `doSynchronise` (mysql_synchronise.cpp:88-217), gate for gate.
    pub fn do_synchronise(
        &self,
        db: &mut dyn SyncDatabase,
        tables: &[DesiredTable],
        log: &mut Vec<String>,
    ) -> Result<bool, SyncFailure> {
        match db.engine_support_state() {
            EngineSupportState::NotSupported => return Err(SyncFailure::EngineNotSupported),
            EngineSupportState::Supported => {
                log.push(format!(
                    "MySqlSynchronise::doSynchronise: {MYSQL_ENGINE_TYPE} is not the default MySQL storage engine. Add 'default-storage-engine={MYSQL_ENGINE_TYPE}' to /etc/my.cnf and restart the MySQL server\n"
                ));
            }
            EngineSupportState::Default => {}
        }

        for statement in special_bigworld_table_statements(self.config.max_space_data_size) {
            db.execute(&statement).map_err(SyncFailure::Exception)?;
        }

        let special_tables = db.table_names("bigworld%");

        if !db.check_table_engines() {
            return Err(SyncFailure::TableEngines);
        }

        let is_secondary_databases_exists = special_tables
            .iter()
            .any(|t| t == "bigworldSecondaryDatabases");
        let num_secondary_dbs = if is_secondary_databases_exists {
            db.count_secondary_databases()
        } else {
            0
        };
        if num_secondary_dbs > 0 {
            return Err(SyncFailure::UnconsolidatedSecondaryDatabases);
        }

        let mut version = DBAPP_CURRENT_VERSION;
        let mut was_password_hashed = false;

        let is_info_tbl_exists = special_tables.iter().any(|t| t == "bigworldInfo");
        if !self.is_dry_run || is_info_tbl_exists {
            version = read_or_record_version(db, log)?;
            was_password_hashed = read_is_password_hashed(db)?;
        }

        if version > DBAPP_CURRENT_VERSION {
            return Err(SyncFailure::NewerVersion {
                current: DBAPP_CURRENT_VERSION,
                found: version,
            });
        }

        let character_set = self.config.character_set.clone();
        let collation = self.config.collation.clone();

        if character_set != DEFAULT_CHARACTER_SET {
            log.push(format!("Character set is '{character_set}'\n"));
        }
        if collation != DEFAULT_COLLATION {
            log.push(format!("Collation is '{collation}'\n"));
        }

        self.alter_db_char_set(db, log, &character_set, &collation)?;

        if version != DBAPP_CURRENT_VERSION {
            db.upgrade_database(version, log)
                .map_err(SyncFailure::Exception)?;
        }

        if !self.update_password_hash(db, was_password_hashed)? {
            return Err(SyncFailure::RemovePasswordHashing);
        }

        let is_entity_types_tbl_exists = special_tables.iter().any(|t| t == "bigworldEntityTypes");
        if self.is_dry_run && !is_entity_types_tbl_exists {
            return Ok(true);
        }

        Ok(synchronise_entity_definitions(
            db,
            log,
            tables,
            true,
            &character_set,
            &collation,
        ))
    }

    /// `alterDBCharSet` (mysql_synchronise.cpp:227-259).
    fn alter_db_char_set(
        &self,
        db: &mut dyn SyncDatabase,
        log: &mut Vec<String>,
        character_set: &str,
        collation: &str,
    ) -> Result<(), SyncFailure> {
        if character_set.is_empty() {
            log.push("Empty character set. Leaving at existing setting\n".to_string());
            return Ok(());
        }
        let result = if collation.is_empty() {
            db.execute("ALTER DATABASE CHARACTER SET ?")
        } else {
            db.execute("ALTER DATABASE CHARACTER SET ? COLLATE ?")
        };
        if let Err(e) = result {
            return Err(SyncFailure::Charset(e));
        }
        Ok(())
    }

    /// `updatePasswordHash` (mysql_synchronise.cpp:352-381).
    fn update_password_hash(
        &self,
        db: &mut dyn SyncDatabase,
        was_hashed: bool,
    ) -> Result<bool, SyncFailure> {
        let should_hash = self.config.is_password_hashed;

        if was_hashed && !should_hash {
            return Ok(false);
        }

        if !was_hashed && should_hash {
            db.execute("UPDATE bigworldInfo SET isPasswordHashed = TRUE")
                .map_err(SyncFailure::Exception)?;
            db.execute(
                "UPDATE bigworldLogOnMapping SET password = MD5( CONCAT( password, logOnName ) )",
            )
            .map_err(SyncFailure::Exception)?;
            db.commit();
        }

        Ok(true)
    }
}

/// `synchroniseEntityDefinitions` (mysql_synchronise.cpp:387-416): the
/// initialiser pass over the desired tables, then drop the unvisited ones.
pub fn synchronise_entity_definitions(
    db: &mut dyn SyncDatabase,
    log: &mut Vec<String>,
    tables: &[DesiredTable],
    allow_new: bool,
    character_set: &str,
    collation: &str,
) -> bool {
    let mut initialiser =
        TableInitialiser::new(allow_new, character_set.to_string(), collation.to_string());
    let mut scan = Inspector::new();

    for table in tables {
        if let Err(e) = scan.visit_table(db, log, &mut initialiser, &table.name, &table.columns) {
            log.push(format!(
                "MySqlSynchronise::synchroniseEntityDefinitions: {e}"
            ));
            return false;
        }
    }

    if let Err(e) = scan.delete_unvisited_tables(db, log, &mut initialiser) {
        log.push(format!(
            "MySqlSynchronise::synchroniseEntityDefinitions: {e}"
        ));
        return false;
    }
    scan.is_synced
}

// -----------------------------------------------------------------------------
// Section: the main() face (sync_db main.cpp)
// -----------------------------------------------------------------------------

pub const INITIALISATION_FAILED_MESSAGE: &str = "Initialisation failed\n";
pub const SYNC_TO_DATABASE_FAILED_MESSAGE: &str = "Sync to database failed\n";
pub const SYNC_TO_DATABASE_SUCCESSFUL_MESSAGE: &str = "Sync to database successful\n";

/// `DBConfig::ConnectionInfo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    pub secure_auth: bool,
}

/// The ArgParser options sync_db takes (main.cpp:29-39). `None` = option
/// absent; `secure_auth` present without a value parses as `Some(true)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandLine {
    pub verbose: bool,
    pub run_from_dbapp: bool,
    pub dry_run: bool,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub secure_auth: Option<bool>,
}

/// main.cpp:55-68: config values with per-option overrides; `secure-auth`
/// only overrides when the option is present.
pub fn resolve_connection(command_line: &CommandLine, config: &ConnectionInfo) -> ConnectionInfo {
    let mut connection = config.clone();
    if let Some(host) = &command_line.host {
        connection.host = host.clone();
    }
    if let Some(port) = command_line.port {
        connection.port = port;
    }
    if let Some(database) = &command_line.database {
        connection.database = database.clone();
    }
    if let Some(username) = &command_line.username {
        connection.username = username.clone();
    }
    if let Some(password) = &command_line.password {
        connection.password = password.clone();
    }
    if let Some(secure_auth) = command_line.secure_auth {
        connection.secure_auth = secure_auth;
    }
    connection
}

/// EXIT_SUCCESS / EXIT_FAILURE.
pub fn exit_code(successful: bool) -> i32 {
    if successful { 0 } else { 1 }
}

#[cfg(test)]
mod tests;
