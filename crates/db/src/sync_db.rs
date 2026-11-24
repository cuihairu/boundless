//! The `sync_db` tool (`server/tools/sync_db`) and the schema-difference
//! machinery it drives (`lib/db_storage_mysql/table_inspector.cpp` +
//! `mysql_table_initialiser.cpp`).
//!
//! The reference opens a MySQL connection, walks every table the entity
//! definitions require, and repairs the difference with DDL. The port
//! carries the same three faces the tool is made of, without a driver —
//! the schema is handed in and the statements are handed back:
//!
//! 1. [`special_table_statements`] — `MySqlSynchronise::
//!    createSpecialBigWorldTables` (`mysql_synchronise.cpp:227-310`), the
//!    literal `CREATE TABLE IF NOT EXISTS` set for the `bigworld*`
//!    bookkeeping tables, byte-exact.
//! 2. [`inspect`] + [`plan_statements`] — `TableInspector::onVisitTable`
//!    (`table_inspector.cpp:51-97`), `classifyColumns`
//!    (`:124-166`), `deleteUnvisitedTables` (`:80-106`), and the
//!    `TableInitialiser` statements they drive (`mysql_table_initialiser.cpp:
//!    357-660`). This is the acceptance: over the original schema the DDL
//!    difference is reviewable — every `ADD`/`DROP`/`MODIFY COLUMN`,
//!    every index change, and every dropped table is a string the
//!    reference would have executed, in the order it would have executed
//!    them.
//! 3. [`plan_sync`] — `MySqlSynchronise::doSynchronise`
//!    (`mysql_synchronise.cpp:120-216`) as a decision: the engine,
//!    secondary-database and version gates, the character-set alter, the
//!    version-upgrade hook, and the password-hashing update, each refusal
//!    carrying the reference's message.
//!
//! The column type vocabulary (`column_type.hpp`) is ported so the
//! statements render exactly as `ColumnType::getAsString` does
//! (`column_type.cpp:17-194`), including the reference's equality rule
//! (`:272-357`) that decides when a column is "out of date".

use std::collections::BTreeMap;

use crate::namer::{ColumnIndexType, ID_COLUMN_NAME, PARENTID_INDEX_NAME};

/// `MYSQL_ENGINE_TYPE` (`wrapper.hpp:32`).
pub const MYSQL_ENGINE_TYPE: &str = "InnoDB";

/// `DBAPP_CURRENT_VERSION` (`versions.hpp:16-26`): the schema version this
/// code writes, `DBAPP_VERSION_LOGON_MAPPING_2`.
pub const DBAPP_CURRENT_VERSION: u32 = 9;

/// `generateIndexName` (`table_meta_data.cpp:22-29`): everything after the
/// first underscore, then `Index`.
pub fn generate_index_name(column: &str) -> String {
    let after_underscore = match column.find('_') {
        Some(pos) => &column[pos + 1..],
        None => column,
    };
    format!("{after_underscore}Index")
}

/// `MySqlEscapedString`: the string-literal escaping a quoted default
/// value rides through. The backslash escapes are MySQL's
/// `mysql_real_escape_string` set — the ones a server-side string literal
/// cannot carry raw.
fn escape_sql_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            '"' => escaped.push_str("\\\""),
            '\0' => escaped.push_str("\\0"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\u{1a}' => escaped.push_str("\\Z"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// The `enum_field_types` the reference's column types take
/// (`column_type.cpp:95-183` switches over exactly these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// `MYSQL_TYPE_TINY`.
    Tiny,
    /// `MYSQL_TYPE_SHORT`.
    Short,
    /// `MYSQL_TYPE_INT24`.
    Int24,
    /// `MYSQL_TYPE_LONG`.
    Long,
    /// `MYSQL_TYPE_LONGLONG`.
    LongLong,
    /// `MYSQL_TYPE_DECIMAL`.
    Decimal,
    /// `MYSQL_TYPE_NEWDECIMAL`.
    NewDecimal,
    /// `MYSQL_TYPE_FLOAT`.
    Float,
    /// `MYSQL_TYPE_DOUBLE`.
    Double,
    /// `MYSQL_TYPE_TIMESTAMP`.
    Timestamp,
    /// `MYSQL_TYPE_TIME`.
    Time,
    /// `MYSQL_TYPE_NEWDATE`.
    NewDate,
    /// `MYSQL_TYPE_YEAR`.
    Year,
    /// `MYSQL_TYPE_VARCHAR`.
    VarChar,
    /// `MYSQL_TYPE_TINY_BLOB`.
    TinyBlob,
    /// `MYSQL_TYPE_MEDIUM_BLOB`.
    MediumBlob,
    /// `MYSQL_TYPE_LONG_BLOB`.
    LongBlob,
    /// `MYSQL_TYPE_BLOB`.
    Blob,
    /// `MYSQL_TYPE_BIT`.
    Bit,
    /// `MYSQL_TYPE_VAR_STRING`.
    VarString,
    /// `MYSQL_TYPE_STRING`.
    String,
}

/// `ColumnType` (`column_type.hpp:29-100`): what a column is, as the
/// reference tracks it.
#[derive(Debug, Clone, Default)]
pub struct ColumnType {
    /// `fieldType`.
    pub field_type: Option<FieldType>,
    /// `isUnsignedOrBinary` — dual use: `UNSIGNED` on the integer kinds,
    /// `BINARY` on the text kinds.
    pub is_unsigned_or_binary: bool,
    /// `length`: characters on the text kinds, bytes otherwise.
    pub length: u32,
    /// `defaultValue` (raw; quoting and escaping happen at render time).
    pub default_value: String,
    /// `onUpdateCmd`.
    pub on_update_cmd: String,
    /// `isAutoIncrement`.
    pub is_auto_increment: bool,
}

impl ColumnType {
    /// The bare type, everything else defaulted.
    pub fn simple(field_type: FieldType) -> Self {
        Self {
            field_type: Some(field_type),
            ..Self::default()
        }
    }

    /// `isDefaultValueSupported` (`column_type.cpp:216-230`): the BLOB
    /// family takes no `DEFAULT` clause.
    pub fn is_default_value_supported(&self) -> bool {
        !matches!(
            self.field_type,
            Some(FieldType::TinyBlob)
                | Some(FieldType::MediumBlob)
                | Some(FieldType::LongBlob)
                | Some(FieldType::Blob)
        )
    }

    /// `isStringType` (`column_type.cpp:233-250`).
    pub fn is_string_type(&self) -> bool {
        matches!(
            self.field_type,
            Some(FieldType::TinyBlob)
                | Some(FieldType::MediumBlob)
                | Some(FieldType::LongBlob)
                | Some(FieldType::Blob)
                | Some(FieldType::VarString)
                | Some(FieldType::String)
                | Some(FieldType::VarChar)
        )
    }

    /// `isSimpleNumericalType` (`column_type.cpp:253-270`).
    pub fn is_simple_numerical_type(&self) -> bool {
        matches!(
            self.field_type,
            Some(FieldType::Decimal)
                | Some(FieldType::NewDecimal)
                | Some(FieldType::Float)
                | Some(FieldType::Double)
                | Some(FieldType::Tiny)
                | Some(FieldType::Short)
                | Some(FieldType::Int24)
                | Some(FieldType::Long)
                | Some(FieldType::LongLong)
        )
    }

    /// `ColumnType::operator==` (`column_type.cpp:272-357`): the field
    /// type must match, the optional extras per family, the auto-increment
    /// flag, and the default value with its per-family equivalences.
    fn equals(&self, other: &Self) -> bool {
        if self.field_type != other.field_type {
            return false;
        }
        if !self.equals_optional_stuff(other) {
            return false;
        }
        if self.is_auto_increment != other.is_auto_increment {
            return false;
        }
        self.equals_default_value(other)
    }

    /// `equalOptionalStuff` (`column_type.cpp:274-323`): what else counts
    /// per family — nothing for the float/time kinds, sign and
    /// auto-increment for the integer kinds, binary flag for the BLOB
    /// kinds, length plus binary flag for `CHAR`/`VARBINARY`, length for
    /// `YEAR`/`VARCHAR`.
    fn equals_optional_stuff(&self, other: &Self) -> bool {
        match self.field_type {
            Some(
                FieldType::Decimal
                | FieldType::NewDecimal
                | FieldType::Float
                | FieldType::Double
                | FieldType::Timestamp
                | FieldType::Time
                | FieldType::NewDate,
            )
            // The reference's default arm asserts on unknown types and
            // returns true; an absent type is that same dead case.
            | None => true,
            Some(
                FieldType::Tiny
                | FieldType::Short
                | FieldType::Int24
                | FieldType::Long
                | FieldType::LongLong,
            ) => {
                self.is_unsigned_or_binary == other.is_unsigned_or_binary
                    && self.is_auto_increment == other.is_auto_increment
            }
            Some(
                FieldType::TinyBlob
                | FieldType::MediumBlob
                | FieldType::LongBlob
                | FieldType::Blob,
            ) => self.is_unsigned_or_binary == other.is_unsigned_or_binary,
            Some(FieldType::VarString | FieldType::String) => {
                self.length == other.length
                    && self.is_unsigned_or_binary == other.is_unsigned_or_binary
            }
            Some(FieldType::Bit | FieldType::VarChar | FieldType::Year) => {
                self.length == other.length
            }
        }
    }

    /// `equalDefaultValue` (`column_type.cpp:325-357`): auto-increment and
    /// no-`DEFAULT` types ignore it; numerics treat empty as `"0"`;
    /// `CHAR`/`BINARY` treats empty as the all-padding string; `TIMESTAMP`
    /// ignores it; everything else must match exactly.
    fn equals_default_value(&self, other: &Self) -> bool {
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
        if self.field_type == Some(FieldType::String) {
            let pad = if self.is_unsigned_or_binary {
                '\0'
            } else {
                ' '
            };
            let null_string: String = std::iter::repeat_n(pad, self.length as usize).collect();
            return (self.default_value.is_empty() && other.default_value == null_string)
                || (self.default_value == null_string && other.default_value.is_empty());
        }
        self.field_type == Some(FieldType::Timestamp)
    }

    /// `ColumnType::getAsString` (`column_type.cpp:17-194`): the SQL the
    /// column is created or modified with — the type keyword, the
    /// family's qualifiers, the default, `ON UPDATE`, the primary-key
    /// marker, and the trailing `NOT NULL`.
    pub fn render(&self, index_type: ColumnIndexType) -> String {
        let mut out = String::new();
        let unsigned_or_binary = self.is_unsigned_or_binary;
        match self.field_type {
            Some(FieldType::Tiny) => {
                out.push_str("TINYINT");
                if unsigned_or_binary {
                    out.push_str(" UNSIGNED");
                }
            }
            Some(FieldType::Short) => {
                out.push_str("SMALLINT");
                if unsigned_or_binary {
                    out.push_str(" UNSIGNED");
                }
            }
            Some(FieldType::Int24) => {
                out.push_str("MEDIUMINT");
                if unsigned_or_binary {
                    out.push_str(" UNSIGNED");
                }
            }
            Some(FieldType::Long) => {
                out.push_str("INT");
                if unsigned_or_binary {
                    out.push_str(" UNSIGNED");
                }
            }
            Some(FieldType::LongLong) => {
                out.push_str("BIGINT");
                if unsigned_or_binary {
                    out.push_str(" UNSIGNED");
                }
            }
            Some(FieldType::Decimal) | Some(FieldType::NewDecimal) => {
                out.push_str("DECIMAL");
                if unsigned_or_binary {
                    out.push_str(" UNSIGNED");
                }
            }
            Some(FieldType::Float) => out.push_str("FLOAT"),
            Some(FieldType::Double) => out.push_str("DOUBLE"),
            Some(FieldType::Timestamp) => out.push_str("TIMESTAMP"),
            Some(FieldType::Time) => out.push_str("TIME"),
            Some(FieldType::NewDate) => out.push_str("DATE"),
            Some(FieldType::Year) => out.push_str(&format!("YEAR({})", self.length)),
            Some(FieldType::VarChar) => out.push_str(&format!("VARCHAR({})", self.length)),
            Some(FieldType::TinyBlob) => out.push_str(if unsigned_or_binary {
                "TINYBLOB"
            } else {
                "TINYTEXT"
            }),
            Some(FieldType::MediumBlob) => out.push_str(if unsigned_or_binary {
                "MEDIUMBLOB"
            } else {
                "MEDIUMTEXT"
            }),
            Some(FieldType::LongBlob) => out.push_str(if unsigned_or_binary {
                "LONGBLOB"
            } else {
                "LONGTEXT"
            }),
            Some(FieldType::Blob) => out.push_str(if unsigned_or_binary { "BLOB" } else { "TEXT" }),
            Some(FieldType::Bit) => out.push_str(&format!("BIT({})", self.length)),
            Some(FieldType::VarString) => {
                out.push_str(if unsigned_or_binary {
                    "VARBINARY"
                } else {
                    "VARCHAR"
                });
                out.push_str(&format!("({})", self.length));
            }
            Some(FieldType::String) => {
                out.push_str(if unsigned_or_binary { "BINARY" } else { "CHAR" });
                out.push_str(&format!("({})", self.length));
            }
            // `getAsString`'s default arm is `MF_ASSERT( false )` — the
            // port renders nothing rather than panicking mid-report.
            None => {}
        }

        if self.is_auto_increment {
            out.push_str(" AUTO_INCREMENT");
        } else if self.is_default_value_supported() && !self.default_value.is_empty() {
            out.push_str(" DEFAULT ");
            if self.is_string_type() {
                out.push('\'');
                out.push_str(&escape_sql_string(&self.default_value));
                out.push('\'');
            } else {
                out.push_str(&self.default_value);
            }
        }

        if !self.on_update_cmd.is_empty() {
            out.push_str(" ON UPDATE ");
            out.push_str(&self.on_update_cmd);
        }

        if index_type == ColumnIndexType::Primary {
            out.push_str(" PRIMARY KEY");
        }

        out.push_str(" NOT NULL");
        out
    }
}

/// The derived `PartialEq` the reference's `operator==` provides — field
/// types must both be known, then the per-family rule.
impl PartialEq for ColumnType {
    fn eq(&self, other: &Self) -> bool {
        self.equals(other)
    }
}

impl Eq for ColumnType {}

/// `TableMetaData::ColumnInfo`: a column's type and its index kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// `columnType`.
    pub column_type: ColumnType,
    /// `indexType`.
    pub index_type: ColumnIndexType,
}

impl ColumnInfo {
    /// One column as the definitions require it or the database has it.
    pub fn new(column_type: ColumnType, index_type: ColumnIndexType) -> Self {
        Self {
            column_type,
            index_type,
        }
    }

    /// `isIndexEqual` (`table_meta_data.cpp:179-198`): an externally
    /// created index counts as either BigWorld-generated kind — presume a
    /// production database — everything else is plain equality.
    pub fn is_index_equal(&self, other: &Self) -> bool {
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

/// `TableMetaData::IndexedColumnInfo`: a column whose index needs
/// changing, carrying the kind the database has now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedColumnInfo {
    /// The column as the definitions require it.
    pub new: ColumnInfo,
    /// `oldIndexType`.
    pub old_index_type: ColumnIndexType,
}

/// What `classifyColumns` sorted a table's columns into
/// (`table_inspector.cpp:124-166`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnDiff {
    /// Only in the definitions — `ADD COLUMN`.
    pub add: BTreeMap<String, ColumnInfo>,
    /// Only in the database — `DROP COLUMN`.
    pub drop: BTreeMap<String, ColumnInfo>,
    /// In both, type out of date — `MODIFY COLUMN`.
    pub modify: BTreeMap<String, ColumnInfo>,
    /// In both, index out of date.
    pub reindex: BTreeMap<String, IndexedColumnInfo>,
}

impl ColumnDiff {
    /// Whether anything at all needs changing.
    pub fn is_empty(&self) -> bool {
        self.add.is_empty()
            && self.drop.is_empty()
            && self.modify.is_empty()
            && self.reindex.is_empty()
    }

    /// `TableInspector::classifyColumns` (`table_inspector.cpp:124-166`):
    /// walk the observed columns; a column the definitions also ask for is
    /// compared on type and index — any difference lands in `modify` or
    /// `reindex` — and erased from both maps; the survivors are the pure
    /// drops (observed only) and the pure adds (definitions only).
    pub fn classify(
        observed: &BTreeMap<String, ColumnInfo>,
        required: &BTreeMap<String, ColumnInfo>,
    ) -> Self {
        let mut diff = Self::default();
        for (name, old_col) in observed {
            let Some(new_col) = required.get(name) else {
                diff.drop.insert(name.clone(), old_col.clone());
                continue;
            };
            if new_col.column_type != old_col.column_type {
                diff.modify.insert(name.clone(), new_col.clone());
            }
            if !new_col.is_index_equal(old_col) {
                diff.reindex.insert(
                    name.clone(),
                    IndexedColumnInfo {
                        new: new_col.clone(),
                        old_index_type: old_col.index_type,
                    },
                );
            }
        }
        for (name, new_col) in required {
            if !observed.contains_key(name) {
                diff.add.insert(name.clone(), new_col.clone());
            }
        }
        diff
    }
}

/// `SyncError`: why the inspection could not even produce a report —
/// `onVisitTable`'s "table %s requested twice" throw
/// (`table_inspector.cpp:62-68`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// The definitions ask for the same table twice.
    DuplicateTable(String),
}

/// What one table needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableAction {
    /// `onNeedNewTable`: absent from the database. `needs_id_column` is
    /// the reference's `deleteIDCol` — the table is created with an `id`
    /// column even when the definitions want none, and the column is
    /// dropped right after (`mysql_table_initialiser.cpp:379-395`).
    Create {
        /// The columns the definitions require.
        columns: BTreeMap<String, ColumnInfo>,
        /// The required set carries no `id`, so the bootstrap column is
        /// dropped.
        needs_id_column: bool,
    },
    /// `onNeedUpdateTable`: the table exists with differences.
    Update(ColumnDiff),
    /// Present and identical on type and index.
    Unchanged,
}

/// One table's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePlan {
    /// The table name.
    pub table: String,
    /// What the table needs.
    pub action: TableAction,
}

/// The `TableInspector` walk over one set of definitions
/// (`table_inspector.cpp:51-106`): per-table plans plus the obsolete
/// tables (`deleteUnvisitedTables`), and `isSynced` — the validator's
/// verdict that nothing is left to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InspectionReport {
    /// One plan per required table, in definition order.
    pub tables: Vec<TablePlan>,
    /// Tables the database has that the definitions no longer mention —
    /// the set difference, in name order.
    pub obsolete_tables: Vec<String>,
    /// `isSynced`: no creations, no updates, no deletions pending.
    pub is_synced: bool,
}

/// Walk the required tables against the observed schema
/// (`onVisitTable` + `deleteUnvisitedTables`).
///
/// `required` is an ordered list of `(table, columns)` because the
/// reference visits providers in order and throws on a repeated name.
pub fn inspect(
    required: &[(String, BTreeMap<String, ColumnInfo>)],
    observed: &BTreeMap<String, BTreeMap<String, ColumnInfo>>,
) -> Result<InspectionReport, SyncError> {
    let mut report = InspectionReport {
        // `isSynced_( true )` — the verdict starts clean and is cleared
        // per table by any pending difference.
        is_synced: true,
        ..InspectionReport::default()
    };
    let mut visited: Vec<&str> = Vec::new();
    for (table, columns) in required {
        if visited.iter().any(|seen| seen == table) {
            return Err(SyncError::DuplicateTable(table.clone()));
        }
        visited.push(table);
        let action = match observed.get(table) {
            None => {
                let needs_id_column = !columns.contains_key(ID_COLUMN_NAME);
                report.is_synced = false;
                TableAction::Create {
                    columns: columns.clone(),
                    needs_id_column,
                }
            }
            Some(observed_columns) => {
                let diff = ColumnDiff::classify(observed_columns, columns);
                if diff.is_empty() {
                    TableAction::Unchanged
                } else {
                    report.is_synced = false;
                    TableAction::Update(diff)
                }
            }
        };
        report.tables.push(TablePlan {
            table: table.clone(),
            action,
        });
    }

    for table in observed.keys() {
        if !visited.iter().any(|seen| seen == table) {
            report.is_synced = false;
            report.obsolete_tables.push(table.clone());
        }
    }

    Ok(report)
}

/// `ALTER TABLE` assembly — `AlterTableHelper`
/// (`mysql_table_initialiser.cpp:57-162`): one statement, items joined
/// with commas, issued only when non-empty.
#[derive(Debug, Default)]
pub struct AlterTableBuilder {
    table: String,
    items: Vec<String>,
}

impl AlterTableBuilder {
    /// Start a statement for `table`.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            items: Vec::new(),
        }
    }

    /// `addColumn`.
    pub fn add_column(&mut self, column: &str, column_type: &str) -> &mut Self {
        self.items
            .push(format!(" ADD COLUMN {column} {column_type}"));
        self
    }

    /// `dropColumn`.
    pub fn drop_column(&mut self, column: &str) -> &mut Self {
        self.items.push(format!(" DROP COLUMN {column}"));
        self
    }

    /// `modifyColumn`.
    pub fn modify_column(&mut self, column: &str, column_type: &str) -> &mut Self {
        self.items
            .push(format!(" MODIFY COLUMN {column} {column_type}"));
        self
    }

    /// `execute`'s statement — `None` when nothing was queued, exactly
    /// the reference's `numAlteredItems()` guard.
    pub fn build(self) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let mut statement = format!("ALTER TABLE {}", self.table);
        for item in self.items {
            statement.push(',');
            statement.push_str(&item);
        }
        Some(statement)
    }
}

/// `generateCreateIndexStatement` (`mysql_table_initialiser.cpp:177-242`):
/// `CREATE [UNIQUE ]INDEX <name> ON <table> (<column>[ (length)])`. The
/// parenthesised length rides only on the string and BLOB kinds, and on
/// everything but `VARBINARY` it clamps at 255 — MySQL's index limit for
/// TEXT/BLOB (the reference warns "can only be indexed up to the first
/// %d characters").
pub fn create_index_statement(table: &str, column: &str, info: &ColumnInfo) -> Option<String> {
    let index_name = generate_index_name(column);
    let mut out = String::from("CREATE ");
    if info.index_type == ColumnIndexType::Unique {
        out.push_str("UNIQUE ");
    }
    out.push_str(&format!("INDEX {index_name} ON {table} ({column}"));

    let mut index_length = info.column_type.length;
    if info.column_type.field_type != Some(FieldType::VarString)
        && index_length > MAX_TEXT_BLOB_INDEX_LENGTH
    {
        index_length = MAX_TEXT_BLOB_INDEX_LENGTH;
    }

    if let Some(
        FieldType::VarChar
        | FieldType::TinyBlob
        | FieldType::MediumBlob
        | FieldType::LongBlob
        | FieldType::Blob
        | FieldType::VarString
        | FieldType::String,
    ) = info.column_type.field_type
    {
        out.push_str(&format!("({index_length})"));
    }

    out.push(')');
    Some(out)
}

/// MySQL's TEXT/BLOB index length limit
/// (`mysql_table_initialiser.cpp:206-207`).
pub const MAX_TEXT_BLOB_INDEX_LENGTH: u32 = 255;

/// `createIndex` (`mysql_table_initialiser.cpp:244-311`): the index
/// statement a column needs, if any — none and primary ride with the
/// table creation, external indexes are left alone.
pub fn create_index_for(table: &str, column: &str, info: &ColumnInfo) -> Option<String> {
    match info.index_type {
        ColumnIndexType::None | ColumnIndexType::Primary | ColumnIndexType::External => None,
        ColumnIndexType::Unique | ColumnIndexType::NonUnique => {
            create_index_statement(table, column, info)
        }
        ColumnIndexType::ParentId => Some(format!(
            "CREATE INDEX {PARENTID_INDEX_NAME} ON {table} ({column})"
        )),
    }
}

/// `removeIndex` (`mysql_table_initialiser.cpp:313-356`): the `DROP
/// INDEX` a column's old index needs, if any.
pub fn remove_index_statement(
    table: &str,
    column: &str,
    old_index_type: ColumnIndexType,
) -> Option<String> {
    match old_index_type {
        ColumnIndexType::None
        | ColumnIndexType::Primary
        | ColumnIndexType::External
        | ColumnIndexType::ParentId => None,
        ColumnIndexType::Unique | ColumnIndexType::NonUnique => {
            let index_name = generate_index_name(column);
            Some(format!("ALTER TABLE {table} DROP INDEX {index_name}"))
        }
    }
}

/// `onExistingTable`'s character-set statement
/// (`mysql_table_initialiser.cpp:409-432`): `None` when no character set
/// is configured (leave as-is).
pub fn convert_charset_statement(
    table: &str,
    character_set: &str,
    collation: &str,
) -> Option<String> {
    if character_set.is_empty() {
        return None;
    }
    let mut out = format!("ALTER TABLE {table} CONVERT TO CHARACTER SET {character_set}");
    if !collation.is_empty() {
        out.push_str(&format!(" COLLATE {collation}"));
    }
    Some(out)
}

/// `onNeedNewTable`'s bootstrap `CREATE TABLE`
/// (`mysql_table_initialiser.cpp:367-371`): a fresh table with just the
/// id column, because MySQL refuses to create an empty one.
pub fn create_entity_table_statement(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (id BIGINT AUTO_INCREMENT, \
         PRIMARY KEY idKey (id)) ENGINE={MYSQL_ENGINE_TYPE}"
    )
}

/// `initialiseColumn`'s default-value backfill
/// (`mysql_table_initialiser.cpp:500-517`): the `UPDATE` that applies
/// the default the column type cannot carry in its definition.
fn default_backfill_statement(table: &str, column: &str, info: &ColumnInfo) -> Option<String> {
    if info.column_type.is_default_value_supported() || info.column_type.default_value.is_empty() {
        return None;
    }
    Some(format!(
        "UPDATE {table} SET {column}='{}'",
        escape_sql_string(&info.column_type.default_value)
    ))
}

/// The statements one table plan needs
/// (`onNeedNewTable` / `onNeedUpdateTable` / `deleteUnvisitedTables`).
///
/// `allow_new` is `TableInitialiser::allowNew_`: false withholds
/// creations and column additions (the reference reports "Witholding
/// table creation" and "addColumns" is skipped), which is the validator
/// face of the tool.
pub fn plan_table_statements(plan: &TablePlan, allow_new: bool) -> Vec<String> {
    let table = plan.table.as_str();
    let mut statements = Vec::new();
    match &plan.action {
        TableAction::Create {
            columns,
            needs_id_column,
        } => {
            if !allow_new {
                return statements;
            }
            statements.push(create_entity_table_statement(table));
            let mut alter = AlterTableBuilder::new(table);
            let mut added: Vec<(&str, &ColumnInfo)> = Vec::new();
            for (name, info) in columns {
                if name == ID_COLUMN_NAME {
                    continue;
                }
                alter.add_column(name, &info.column_type.render(info.index_type));
                added.push((name, info));
            }
            if *needs_id_column {
                alter.drop_column(ID_COLUMN_NAME);
            }
            if let Some(statement) = alter.build() {
                statements.push(statement);
            }
            for (name, info) in added {
                if let Some(statement) = create_index_for(table, name, info) {
                    statements.push(statement);
                }
                if let Some(statement) = default_backfill_statement(table, name, info) {
                    statements.push(statement);
                }
            }
        }
        TableAction::Update(diff) => {
            for (name, info) in &diff.drop {
                if let Some(statement) = remove_index_statement(table, name, info.index_type) {
                    statements.push(statement);
                }
            }
            let mut alter = AlterTableBuilder::new(table);
            if allow_new {
                for (name, info) in &diff.add {
                    alter.add_column(name, &info.column_type.render(info.index_type));
                }
            }
            for name in diff.drop.keys() {
                alter.drop_column(name);
            }
            for (name, info) in &diff.modify {
                alter.modify_column(name, &info.column_type.render(info.index_type));
            }
            if let Some(statement) = alter.build() {
                statements.push(statement);
            }
            for (name, indexed) in &diff.reindex {
                if indexed.new.index_type != ColumnIndexType::None {
                    if let Some(statement) = create_index_for(table, name, &indexed.new) {
                        statements.push(statement);
                    }
                } else if let Some(statement) =
                    remove_index_statement(table, name, indexed.old_index_type)
                {
                    statements.push(statement);
                }
            }
        }
        TableAction::Unchanged => {}
    }
    statements
}

/// The whole report's statements in execution order: each required table,
/// then `deleteUnvisitedTables`' `DROP TABLE`s
/// (`property_mappings_per_type.cpp:213` runs the deletion after the
/// visit).
pub fn plan_statements(report: &InspectionReport, allow_new: bool) -> Vec<String> {
    let mut statements = Vec::new();
    for plan in &report.tables {
        statements.extend(plan_table_statements(plan, allow_new));
    }
    for table in &report.obsolete_tables {
        statements.push(format!("DROP TABLE {table}"));
    }
    statements
}

/// `MySqlTypeTraits<BW::string>::colTypeStr` (`type_traits.hpp:46-93`):
/// which BLOB kind carries a string of up to `max_bytes`.
pub fn string_blob_type_name(max_bytes: u32) -> &'static str {
    if max_bytes < 1 << 8 {
        "TINYBLOB"
    } else if max_bytes < 1 << 16 {
        "BLOB"
    } else if max_bytes < 1 << 24 {
        "MEDIUMBLOB"
    } else {
        "LONGBLOB"
    }
}

/// `createSpecialBigWorldTables` (`mysql_synchronise.cpp:227-310`): the
/// bookkeeping tables, byte-exact as the reference executes them. The
/// variable bits are the account field widths
/// (`BWMySQLMaxLogOnNameLen`/`BWMySQLMaxLogOnPasswordLen`, both 255) and
/// the space-data blob, sized by the `maxSpaceDataSize` setting
/// (default 2048 → `BLOB`).
pub fn special_table_statements(max_space_data_size: u32) -> Vec<String> {
    let blob = string_blob_type_name(max_space_data_size);
    vec![
        "CREATE TABLE IF NOT EXISTS bigworldInfo (version INT UNSIGNED NOT NULL, \
         snapshotTime TIMESTAMP NULL, isPasswordHashed BOOL NOT NULL DEFAULT FALSE) \
         ENGINE=InnoDB"
            .to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldEntityTypes (typeID INT NOT NULL \
         AUTO_INCREMENT, bigworldID INT, name CHAR(255) CHARACTER SET latin1 NOT NULL \
         UNIQUE,  PRIMARY KEY(typeID), KEY(bigworldID)) ENGINE=InnoDB"
            .to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldLogOns (databaseID BIGINT NOT NULL, typeID \
         INT NOT NULL, objectID INT, ip INT UNSIGNED, port SMALLINT UNSIGNED, salt \
         SMALLINT UNSIGNED, shouldAutoLoad BOOL NOT NULL DEFAULT FALSE, PRIMARY \
         KEY(typeID, databaseID)) ENGINE=InnoDB"
            .to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldLogOnMapping (logOnName VARBINARY(255) \
         NOT NULL, password VARBINARY(255) NOT NULL, entityType INT, entityID BIGINT, \
         PRIMARY KEY(logOnName)) ENGINE=InnoDB"
            .to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldNewID (id INT NOT NULL) ENGINE=InnoDB".to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldUsedIDs (id INT NOT NULL) ENGINE=InnoDB".to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldGameTime (time INT NOT NULL) ENGINE=InnoDB".to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldSpaces (id INT NOT NULL UNIQUE) \
         ENGINE=InnoDB"
            .to_owned(),
        format!(
            "CREATE TABLE IF NOT EXISTS bigworldSpaceData (id INT NOT NULL, INDEX (id), \
             spaceEntryID BIGINT NOT NULL, entryKey SMALLINT UNSIGNED NOT NULL, data \
             {blob} NOT NULL ) ENGINE=InnoDB"
        ),
        format!("ALTER TABLE bigworldSpaceData MODIFY data {blob}"),
        "CREATE TABLE IF NOT EXISTS bigworldSecondaryDatabases (ip INT UNSIGNED NOT \
         NULL, port SMALLINT UNSIGNED NOT NULL, location BLOB NOT NULL, INDEX addr \
         (ip, port)) ENGINE=InnoDB"
            .to_owned(),
        "CREATE TABLE IF NOT EXISTS bigworldEntityDefsChecksum (checksum CHAR(255) \
         CHARACTER SET latin1) ENGINE=InnoDB"
            .to_owned(),
    ]
}

/// `MySql::EngineSupportState` (`wrapper.cpp:559-602`): what `SHOW
/// ENGINES` reports about [`MYSQL_ENGINE_TYPE`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EngineSupport {
    /// `NOT_SUPPORTED`: the server cannot use the engine at all.
    #[default]
    NotSupported,
    /// `SUPPORTED`: usable, but not the server default (the reference
    /// prints an advisory notice pointing at `/etc/my.cnf`).
    Supported,
    /// `DEFAULT`: the server's default engine.
    Default,
}

/// What `doSynchronise` refuses and why. Each carries the reference's
/// message through [`SyncRefusal::message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncRefusal {
    /// `checkEngineSupportState`: the server cannot use
    /// [`MYSQL_ENGINE_TYPE`].
    EngineNotSupported,
    /// `checkTableEngines`: existing tables are not InnoDB.
    TableEnginesMismatch,
    /// `numSecondaryDBs() > 0`: dropping columns or tables would eat
    /// unconsolidated data.
    UnconsolidatedSecondaryDatabases,
    /// `version > DBAPP_CURRENT_VERSION`: the database was written by a
    /// newer build.
    NewerDatabaseVersion {
        /// The version found in `bigworldInfo`.
        found: u32,
    },
    /// `updatePasswordHash`: hashing cannot be turned back off.
    CannotRemovePasswordHashing,
}

impl SyncRefusal {
    /// The reference's error text.
    pub fn message(&self) -> String {
        match self {
            Self::EngineNotSupported => format!(
                "MySqlSynchronise::doSynchronise: {MYSQL_ENGINE_TYPE} is not a \
                 supported MySQL storage engine."
            ),
            Self::TableEnginesMismatch => format!(
                "MySqlSynchronise::doSynchronise: One or more tables are not \
                 using the {MYSQL_ENGINE_TYPE} engine type"
            ),
            Self::UnconsolidatedSecondaryDatabases => String::from(
                "Cannot sync database while there are unconsolidated secondary \
                 databases.\nPlease revert any changes to entity definitions and \
                 run the data consolidation tool.\nAlternatively, run \
                 \"consolidate_dbs --clear\" to clear secondary database \
                 information, unconsolidated data will be lost.",
            ),
            Self::NewerDatabaseVersion { found } => format!(
                "Unable to upgrade the database to version \
                 {DBAPP_CURRENT_VERSION}, a newer DB version ({found}) is \
                 already in use."
            ),
            Self::CannotRemovePasswordHashing => String::from(
                "MySqlSynchronise::updatePasswordHash: Cannot remove hashing. \
                 Manually update bigworldInfo.isPasswordHashed and \
                 bigworldLogOnMapping.password fields.",
            ),
        }
    }
}

/// One action `doSynchronise` would take, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStep {
    /// `createSpecialBigWorldTables` — run
    /// [`special_table_statements`] for the statements.
    CreateSpecialTables,
    /// `alterDBCharSet`.
    AlterDatabaseCharset {
        /// The `ALTER DATABASE` statement.
        statement: String,
    },
    /// `MySqlUpgradeDatabase::run` — the per-version recipe is the
    /// upgrade tool's own surface; the plan carries the version it would
    /// run from.
    UpgradeDatabase {
        /// The database's current version.
        from_version: u32,
    },
    /// `updatePasswordHash`'s upgrade transaction.
    EnablePasswordHashing,
    /// `synchroniseEntityDefinitions` — the [`inspect`]-driven DDL above.
    SyncEntityDefinitions,
}

/// The state `doSynchronise` reads before deciding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncPlanRequest {
    /// `SHOW ENGINES`' verdict on [`MYSQL_ENGINE_TYPE`]
    /// (`mysql_synchronise.cpp:91-110`).
    pub engine_support: EngineSupport,
    /// `DBConfig::get().mysql.unicodeString.characterSet()` — empty
    /// leaves the setting alone.
    pub character_set: String,
    /// The matching collation.
    pub collation: String,
    /// `billingSystem/isPasswordHashed` (default true).
    pub should_hash_passwords: bool,
    /// `--dry-run`.
    pub dry_run: bool,
    /// `bigworldInfo` exists (it is only read when it does, or when not
    /// dry-running).
    pub info_table_exists: bool,
    /// `bigworldEntityTypes` exists.
    pub entity_types_table_exists: bool,
    /// `bigworldSecondaryDatabases` exists and `numSecondaryDBs` read
    /// from it; 0 when the table is absent.
    pub secondary_db_count: u32,
    /// The version read from `bigworldInfo`, when it was read.
    pub db_version: Option<u32>,
    /// `getIsPasswordHashed` from `bigworldInfo`, when it was read.
    pub was_password_hashed: Option<bool>,
}

/// `MySqlSynchronise::doSynchronise` (`mysql_synchronise.cpp:120-216`)
/// as a decision: the ordered steps, or the gate that refused.
pub fn plan_sync(request: &SyncPlanRequest) -> Result<Vec<SyncStep>, SyncRefusal> {
    if request.engine_support == EngineSupport::NotSupported {
        return Err(SyncRefusal::EngineNotSupported);
    }
    let mut steps = vec![SyncStep::CreateSpecialTables];

    if request.secondary_db_count > 0 {
        return Err(SyncRefusal::UnconsolidatedSecondaryDatabases);
    }

    let version = if request.dry_run && !request.info_table_exists {
        None
    } else {
        request.db_version
    };
    if let Some(found) = version
        && found > DBAPP_CURRENT_VERSION
    {
        return Err(SyncRefusal::NewerDatabaseVersion { found });
    }

    if !request.character_set.is_empty() {
        let mut statement = format!("ALTER DATABASE CHARACTER SET {}", request.character_set);
        if !request.collation.is_empty() {
            statement.push_str(&format!(" COLLATE {}", request.collation));
        }
        steps.push(SyncStep::AlterDatabaseCharset { statement });
    }

    if let Some(from_version) = version
        && from_version != DBAPP_CURRENT_VERSION
    {
        steps.push(SyncStep::UpgradeDatabase { from_version });
    }

    let was_hashed = request.was_password_hashed.unwrap_or(false);
    if was_hashed && !request.should_hash_passwords {
        return Err(SyncRefusal::CannotRemovePasswordHashing);
    }
    if !was_hashed && request.should_hash_passwords {
        steps.push(SyncStep::EnablePasswordHashing);
    }

    if request.dry_run && !request.entity_types_table_exists {
        return Ok(steps);
    }
    steps.push(SyncStep::SyncEntityDefinitions);
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(field_type: FieldType) -> ColumnInfo {
        ColumnInfo::new(ColumnType::simple(field_type), ColumnIndexType::None)
    }

    fn indexed(field_type: FieldType, index_type: ColumnIndexType) -> ColumnInfo {
        ColumnInfo::new(ColumnType::simple(field_type), index_type)
    }

    #[test]
    fn index_names_follow_the_reference_rule() {
        assert_eq!(generate_index_name("smartID"), "smartIDIndex");
        assert_eq!(generate_index_name("sm_art"), "artIndex");
        assert_eq!(generate_index_name("_lead"), "leadIndex");
        assert_eq!(PARENTID_INDEX_NAME, "parentIDIndex");
    }

    #[test]
    fn integer_types_render_with_their_sign() {
        assert_eq!(
            plain(FieldType::Tiny)
                .column_type
                .render(ColumnIndexType::None),
            "TINYINT NOT NULL"
        );
        let unsigned = ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::Long)
        };
        assert_eq!(
            unsigned.render(ColumnIndexType::None),
            "INT UNSIGNED NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::Short).render(ColumnIndexType::None),
            "SMALLINT NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::Int24).render(ColumnIndexType::None),
            "MEDIUMINT NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::LongLong).render(ColumnIndexType::None),
            "BIGINT NOT NULL"
        );
        let decimal = ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::NewDecimal)
        };
        assert_eq!(
            decimal.render(ColumnIndexType::None),
            "DECIMAL UNSIGNED NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::Decimal).render(ColumnIndexType::None),
            "DECIMAL NOT NULL"
        );
    }

    #[test]
    fn float_and_time_types_ignore_the_sign_flag() {
        let unsigned_double = ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::Double)
        };
        assert_eq!(
            unsigned_double.render(ColumnIndexType::None),
            "DOUBLE NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::Float).render(ColumnIndexType::None),
            "FLOAT NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::Timestamp).render(ColumnIndexType::None),
            "TIMESTAMP NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::Time).render(ColumnIndexType::None),
            "TIME NOT NULL"
        );
        assert_eq!(
            ColumnType::simple(FieldType::NewDate).render(ColumnIndexType::None),
            "DATE NOT NULL"
        );
    }

    #[test]
    fn sized_types_render_their_length_and_text_family() {
        assert_eq!(
            ColumnType {
                length: 10,
                ..ColumnType::simple(FieldType::Year)
            }
            .render(ColumnIndexType::None),
            "YEAR(10) NOT NULL"
        );
        assert_eq!(
            ColumnType {
                length: 32,
                ..ColumnType::simple(FieldType::VarChar)
            }
            .render(ColumnIndexType::None),
            "VARCHAR(32) NOT NULL"
        );
        assert_eq!(
            ColumnType {
                length: 8,
                ..ColumnType::simple(FieldType::Bit)
            }
            .render(ColumnIndexType::None),
            "BIT(8) NOT NULL"
        );
        assert_eq!(
            plain(FieldType::TinyBlob)
                .column_type
                .render(ColumnIndexType::None),
            "TINYTEXT NOT NULL"
        );
        assert_eq!(
            plain(FieldType::Blob)
                .column_type
                .render(ColumnIndexType::None),
            "TEXT NOT NULL"
        );
        assert_eq!(
            plain(FieldType::MediumBlob)
                .column_type
                .render(ColumnIndexType::None),
            "MEDIUMTEXT NOT NULL"
        );
        assert_eq!(
            plain(FieldType::LongBlob)
                .column_type
                .render(ColumnIndexType::None),
            "LONGTEXT NOT NULL"
        );
        let binary_blob = ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::Blob)
        };
        assert_eq!(binary_blob.render(ColumnIndexType::None), "BLOB NOT NULL");
    }

    #[test]
    fn char_and_binary_render_the_prefix() {
        let char_col = ColumnType {
            length: 20,
            ..ColumnType::simple(FieldType::String)
        };
        assert_eq!(char_col.render(ColumnIndexType::None), "CHAR(20) NOT NULL");
        let binary_col = ColumnType {
            length: 20,
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::String)
        };
        assert_eq!(
            binary_col.render(ColumnIndexType::None),
            "BINARY(20) NOT NULL"
        );
        let varchar_col = ColumnType {
            length: 20,
            ..ColumnType::simple(FieldType::VarString)
        };
        assert_eq!(
            varchar_col.render(ColumnIndexType::None),
            "VARCHAR(20) NOT NULL"
        );
        let varbinary_col = ColumnType {
            length: 20,
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::VarString)
        };
        assert_eq!(
            varbinary_col.render(ColumnIndexType::None),
            "VARBINARY(20) NOT NULL"
        );
    }

    #[test]
    fn defaults_quote_on_strings_and_render_raw_elsewhere() {
        let mut text = ColumnType {
            length: 10,
            ..ColumnType::simple(FieldType::VarString)
        };
        text.default_value = "it's".to_owned();
        assert_eq!(
            text.render(ColumnIndexType::None),
            "VARCHAR(10) DEFAULT 'it\\'s' NOT NULL"
        );
        let mut number = ColumnType::simple(FieldType::Long);
        number.default_value = "0".to_owned();
        assert_eq!(
            number.render(ColumnIndexType::None),
            "INT DEFAULT 0 NOT NULL"
        );
        // A BLOB carries no DEFAULT clause even with a value set.
        let mut blob = ColumnType::simple(FieldType::Blob);
        blob.default_value = "x".to_owned();
        assert_eq!(blob.render(ColumnIndexType::None), "TEXT NOT NULL");
    }

    #[test]
    fn auto_increment_on_update_and_primary_key_render_in_order() {
        let mut key = ColumnType::simple(FieldType::LongLong);
        key.is_auto_increment = true;
        assert_eq!(
            key.render(ColumnIndexType::Primary),
            "BIGINT AUTO_INCREMENT PRIMARY KEY NOT NULL"
        );
        let mut stamp = ColumnType::simple(FieldType::Timestamp);
        stamp.on_update_cmd = "CURRENT_TIMESTAMP".to_owned();
        stamp.default_value = "CURRENT_TIMESTAMP".to_owned();
        assert_eq!(
            stamp.render(ColumnIndexType::None),
            "TIMESTAMP DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP NOT NULL"
        );
    }

    #[test]
    fn equality_follows_the_reference_families() {
        // Floats ignore the sign flag entirely.
        let float = ColumnType::simple(FieldType::Float);
        let unsigned_float = ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::Float)
        };
        assert_eq!(float, unsigned_float);
        // Integers compare sign and auto-increment.
        let int_a = ColumnType::simple(FieldType::Long);
        let int_b = ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::Long)
        };
        assert_ne!(int_a, int_b);
        let auto = ColumnType {
            is_auto_increment: true,
            ..ColumnType::simple(FieldType::Long)
        };
        assert_ne!(int_a, auto);
        // BLOBs compare only the binary flag; the length is not theirs.
        let blob_a = ColumnType {
            length: 4,
            ..ColumnType::simple(FieldType::Blob)
        };
        let blob_b = ColumnType {
            length: 9,
            ..ColumnType::simple(FieldType::Blob)
        };
        assert_eq!(blob_a, blob_b);
        let binary_blob = ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::Blob)
        };
        assert_ne!(blob_a, binary_blob);
        // CHAR/VARBINARY compare length and binary flag.
        let char_a = ColumnType {
            length: 5,
            ..ColumnType::simple(FieldType::VarString)
        };
        let char_longer = ColumnType {
            length: 6,
            ..ColumnType::simple(FieldType::VarString)
        };
        assert_ne!(char_a, char_longer);
        // YEAR/VARCHAR compare only length.
        let year_a = ColumnType {
            length: 4,
            ..ColumnType::simple(FieldType::Year)
        };
        let year_binary = ColumnType {
            length: 4,
            is_unsigned_or_binary: true,
            ..ColumnType::simple(FieldType::Year)
        };
        assert_eq!(year_a, year_binary);
        // Different field types never compare equal.
        assert_ne!(int_a, char_a);
    }

    #[test]
    fn default_equality_carries_the_family_equivalences() {
        // Numerics treat empty as "0".
        let zero = ColumnType {
            default_value: "0".to_owned(),
            ..ColumnType::simple(FieldType::Long)
        };
        assert_eq!(ColumnType::simple(FieldType::Long), zero);
        // CHAR/BINARY treat empty as the padding string.
        let padded = ColumnType {
            length: 3,
            default_value: "   ".to_owned(),
            ..ColumnType::simple(FieldType::String)
        };
        assert_eq!(
            ColumnType {
                length: 3,
                ..ColumnType::simple(FieldType::String)
            },
            padded
        );
        let nul_padded = ColumnType {
            length: 3,
            is_unsigned_or_binary: true,
            default_value: "\0\0\0".to_owned(),
            ..ColumnType::simple(FieldType::String)
        };
        assert_eq!(
            ColumnType {
                length: 3,
                is_unsigned_or_binary: true,
                ..ColumnType::simple(FieldType::String)
            },
            nul_padded
        );
        // Auto-increment and BLOB types ignore defaults; TIMESTAMP too.
        let mut auto = ColumnType::simple(FieldType::Long);
        auto.is_auto_increment = true;
        let mut auto_other = ColumnType::simple(FieldType::Long);
        auto_other.is_auto_increment = true;
        auto_other.default_value = "7".to_owned();
        assert_eq!(auto, auto_other);
        let mut blob = ColumnType::simple(FieldType::Blob);
        blob.default_value = "x".to_owned();
        let mut blob_other = ColumnType::simple(FieldType::Blob);
        blob_other.default_value = "y".to_owned();
        assert_eq!(blob, blob_other);
        let mut stamp = ColumnType::simple(FieldType::Timestamp);
        stamp.default_value = "a".to_owned();
        let mut stamp_other = ColumnType::simple(FieldType::Timestamp);
        stamp_other.default_value = "b".to_owned();
        assert_eq!(stamp, stamp_other);
        // Anything else must match exactly.
        let mut time_a = ColumnType::simple(FieldType::Time);
        time_a.default_value = "1".to_owned();
        let mut time_b = ColumnType::simple(FieldType::Time);
        time_b.default_value = "2".to_owned();
        assert_ne!(time_a, time_b);
    }

    #[test]
    fn the_external_index_counts_as_either_generated_kind() {
        let external = indexed(FieldType::Long, ColumnIndexType::External);
        assert!(external.is_index_equal(&indexed(FieldType::Long, ColumnIndexType::Unique)));
        assert!(external.is_index_equal(&indexed(FieldType::Long, ColumnIndexType::NonUnique)));
        assert!(!external.is_index_equal(&indexed(FieldType::Long, ColumnIndexType::None)));
        let generated = indexed(FieldType::Long, ColumnIndexType::Unique);
        assert!(generated.is_index_equal(&external));
        assert!(generated.is_index_equal(&indexed(FieldType::Long, ColumnIndexType::Unique)));
        assert!(!generated.is_index_equal(&indexed(FieldType::Long, ColumnIndexType::NonUnique)));
    }

    #[test]
    fn classification_sorts_the_four_kinds_of_difference() {
        // Observed: gone_from_defs (drop), stale_type (modify),
        // stale_index (reindex), both_stale, fine. Required adds: newcomer.
        let mut observed = BTreeMap::new();
        observed.insert("gone_from_defs".to_owned(), plain(FieldType::Long));
        observed.insert("stale_type".to_owned(), plain(FieldType::Long));
        observed.insert(
            "stale_index".to_owned(),
            indexed(FieldType::VarString, ColumnIndexType::None),
        );
        observed.insert(
            "both_stale".to_owned(),
            indexed(FieldType::Tiny, ColumnIndexType::None),
        );
        observed.insert("fine".to_owned(), plain(FieldType::Double));

        let mut required = BTreeMap::new();
        required.insert("stale_type".to_owned(), plain(FieldType::LongLong));
        required.insert(
            "stale_index".to_owned(),
            indexed(FieldType::VarString, ColumnIndexType::Unique),
        );
        required.insert(
            "both_stale".to_owned(),
            indexed(FieldType::Long, ColumnIndexType::NonUnique),
        );
        required.insert("fine".to_owned(), plain(FieldType::Double));
        required.insert("newcomer".to_owned(), plain(FieldType::Long));

        let diff = ColumnDiff::classify(&observed, &required);
        assert_eq!(diff.drop.keys().collect::<Vec<_>>(), vec!["gone_from_defs"]);
        assert_eq!(diff.add.keys().collect::<Vec<_>>(), vec!["newcomer"]);
        assert_eq!(
            diff.modify.keys().collect::<Vec<_>>(),
            vec!["both_stale", "stale_type"]
        );
        assert_eq!(
            diff.modify["stale_type"].column_type.field_type,
            Some(FieldType::LongLong)
        );
        assert_eq!(
            diff.reindex.keys().collect::<Vec<_>>(),
            vec!["both_stale", "stale_index"]
        );
        assert_eq!(
            diff.reindex["stale_index"].old_index_type,
            ColumnIndexType::None
        );
        assert_eq!(
            diff.reindex["stale_index"].new.index_type,
            ColumnIndexType::Unique
        );
        assert!(!diff.is_empty());
        assert!(ColumnDiff::classify(&observed, &observed.clone()).is_empty());
    }

    #[test]
    fn an_index_only_change_does_not_modify_the_type() {
        let mut observed = BTreeMap::new();
        observed.insert("c".to_owned(), plain(FieldType::Long));
        let mut required = BTreeMap::new();
        required.insert(
            "c".to_owned(),
            indexed(FieldType::Long, ColumnIndexType::Unique),
        );
        let diff = ColumnDiff::classify(&observed, &required);
        assert!(diff.modify.is_empty());
        assert_eq!(diff.reindex.len(), 1);
    }

    #[test]
    fn inspection_sorts_new_changed_matching_and_obsolete_tables() {
        let mut avatar = BTreeMap::new();
        avatar.insert("entityID".to_owned(), plain(FieldType::LongLong));
        let mut mailbox = BTreeMap::new();
        mailbox.insert("sm_name".to_owned(), plain(FieldType::VarString));

        let mut observed = BTreeMap::new();
        // Avatar exists but lost its column type — needs an update.
        let mut observed_avatar = BTreeMap::new();
        observed_avatar.insert("entityID".to_owned(), plain(FieldType::Long));
        observed.insert("tbl_avatar".to_owned(), observed_avatar);
        // Mailbox is exactly as required.
        observed.insert("tbl_mailbox".to_owned(), mailbox.clone());
        // And the database holds a table the definitions dropped.
        observed.insert("tbl_ghost".to_owned(), BTreeMap::new());

        let required = vec![
            ("tbl_avatar".to_owned(), avatar.clone()),
            ("tbl_mailbox".to_owned(), mailbox.clone()),
            ("tbl_newcomer".to_owned(), avatar.clone()),
        ];

        let report = inspect(&required, &observed).expect("no duplicate tables");
        assert_eq!(report.tables.len(), 3);
        assert_eq!(report.tables[0].table, "tbl_avatar");
        assert!(matches!(report.tables[0].action, TableAction::Update(_)));
        assert_eq!(report.tables[1].table, "tbl_mailbox");
        assert_eq!(report.tables[1].action, TableAction::Unchanged);
        assert_eq!(report.tables[2].table, "tbl_newcomer");
        assert!(matches!(
            report.tables[2].action,
            TableAction::Create { .. }
        ));
        assert_eq!(report.obsolete_tables, vec!["tbl_ghost".to_owned()]);
        assert!(!report.is_synced);
    }

    #[test]
    fn a_create_plan_carries_the_bootstrap_id_decision() {
        let mut with_id = BTreeMap::new();
        with_id.insert(
            ID_COLUMN_NAME.to_owned(),
            ColumnInfo::new(
                ColumnType {
                    is_auto_increment: true,
                    ..ColumnType::simple(FieldType::LongLong)
                },
                ColumnIndexType::Primary,
            ),
        );
        let without_id = BTreeMap::new();
        let mut observed = BTreeMap::new();
        let required = vec![
            ("tbl_with".to_owned(), with_id.clone()),
            ("tbl_without".to_owned(), without_id.clone()),
        ];
        let report = inspect(&required, &observed).expect("distinct tables");
        assert_eq!(
            report.tables[0].action,
            TableAction::Create {
                columns: with_id,
                needs_id_column: false,
            }
        );
        assert_eq!(
            report.tables[1].action,
            TableAction::Create {
                columns: without_id,
                needs_id_column: true,
            }
        );
        assert!(!report.is_synced);
        // A fully matching database leaves the report synced.
        observed.insert("tbl_with".to_owned(), required[0].1.clone());
        observed.insert("tbl_without".to_owned(), required[1].1.clone());
        let report = inspect(&required, &observed).expect("distinct tables");
        assert!(report.is_synced);
        assert!(report.obsolete_tables.is_empty());
    }

    #[test]
    fn a_repeated_table_is_refused_like_the_reference_throws() {
        let columns = BTreeMap::new();
        let required = vec![
            ("tbl_avatar".to_owned(), columns.clone()),
            ("tbl_avatar".to_owned(), columns),
        ];
        let observed = BTreeMap::new();
        assert_eq!(
            inspect(&required, &observed),
            Err(SyncError::DuplicateTable("tbl_avatar".to_owned()))
        );
    }

    #[test]
    fn the_alter_builder_joins_with_commas_and_skips_empty() {
        let mut alter = AlterTableBuilder::new("tbl_avatar");
        alter
            .add_column("hp", "INT UNSIGNED")
            .drop_column("old_hp")
            .modify_column("mp", "BIGINT NOT NULL");
        assert_eq!(
            alter.build(),
            Some(String::from(
                "ALTER TABLE tbl_avatar, ADD COLUMN hp INT UNSIGNED, DROP COLUMN \
                 old_hp, MODIFY COLUMN mp BIGINT NOT NULL"
            ))
        );
        assert_eq!(AlterTableBuilder::new("tbl_x").build(), None);
    }

    #[test]
    fn index_statements_follow_the_reference_shapes() {
        let column = indexed(FieldType::Long, ColumnIndexType::Unique);
        assert_eq!(
            create_index_statement("tbl_avatar", "sm_name", &column),
            Some(String::from(
                "CREATE UNIQUE INDEX nameIndex ON tbl_avatar (sm_name)"
            ))
        );
        let plain_column = indexed(FieldType::Long, ColumnIndexType::NonUnique);
        assert_eq!(
            create_index_statement("tbl_avatar", "sm_id", &plain_column),
            Some(String::from("CREATE INDEX idIndex ON tbl_avatar (sm_id)"))
        );
        // Integer columns carry no parenthesised length.
        assert_eq!(
            create_index_for("tbl_avatar", "sm_id", &plain_column),
            Some(String::from("CREATE INDEX idIndex ON tbl_avatar (sm_id)"))
        );
    }

    #[test]
    fn blob_and_text_indexes_carry_a_clamped_length() {
        let long_text = ColumnInfo::new(
            ColumnType {
                length: 1024,
                ..ColumnType::simple(FieldType::VarString)
            },
            ColumnIndexType::NonUnique,
        );
        // VARBINARY is exempt from the clamp.
        assert_eq!(
            create_index_statement("tbl_avatar", "sm_data", &long_text),
            Some(String::from(
                "CREATE INDEX dataIndex ON tbl_avatar (sm_data(1024))"
            ))
        );
        let blob_column = ColumnInfo::new(
            ColumnType {
                length: 1024,
                is_unsigned_or_binary: true,
                ..ColumnType::simple(FieldType::Blob)
            },
            ColumnIndexType::NonUnique,
        );
        assert_eq!(
            create_index_statement("tbl_avatar", "sm_data", &blob_column),
            Some(String::from(
                "CREATE INDEX dataIndex ON tbl_avatar (sm_data(255))"
            ))
        );
        let short_char = ColumnInfo::new(
            ColumnType {
                length: 30,
                is_unsigned_or_binary: true,
                ..ColumnType::simple(FieldType::String)
            },
            ColumnIndexType::Unique,
        );
        assert_eq!(
            create_index_statement("tbl_avatar", "sm_name", &short_char),
            Some(String::from(
                "CREATE UNIQUE INDEX nameIndex ON tbl_avatar (sm_name(30))"
            ))
        );
    }

    #[test]
    fn index_kinds_the_database_owns_are_never_dropped_or_created() {
        assert_eq!(
            create_index_for(
                "tbl_child",
                "parentID",
                &indexed(FieldType::Long, ColumnIndexType::ParentId)
            ),
            Some(String::from(
                "CREATE INDEX parentIDIndex ON tbl_child (parentID)"
            ))
        );
        assert_eq!(
            create_index_for(
                "tbl_child",
                "parentID",
                &indexed(FieldType::Long, ColumnIndexType::External)
            ),
            None
        );
        assert_eq!(
            create_index_for(
                "tbl_child",
                "id",
                &indexed(FieldType::Long, ColumnIndexType::Primary)
            ),
            None
        );
        assert_eq!(
            remove_index_statement("tbl_child", "parentID", ColumnIndexType::ParentId),
            None,
            "ParentId is dropped only via its reindex arm"
        );
    }

    #[test]
    fn only_generated_indexes_have_a_drop_statement() {
        assert_eq!(
            remove_index_statement("tbl_avatar", "sm_name", ColumnIndexType::Unique),
            Some(String::from("ALTER TABLE tbl_avatar DROP INDEX nameIndex"))
        );
        assert_eq!(
            remove_index_statement("tbl_avatar", "sm_name", ColumnIndexType::NonUnique),
            Some(String::from("ALTER TABLE tbl_avatar DROP INDEX nameIndex"))
        );
        assert_eq!(
            remove_index_statement("tbl_avatar", "sm_name", ColumnIndexType::None),
            None
        );
        assert_eq!(
            remove_index_statement("tbl_avatar", "sm_name", ColumnIndexType::External),
            None
        );
        assert_eq!(
            remove_index_statement("tbl_avatar", "sm_name", ColumnIndexType::Primary),
            None
        );
    }

    #[test]
    fn the_charset_convert_takes_collation_only_when_set() {
        assert_eq!(
            convert_charset_statement("tbl_avatar", "", "utf8_bin"),
            None,
            "no character set leaves the table alone"
        );
        assert_eq!(
            convert_charset_statement("tbl_avatar", "utf8", ""),
            Some(String::from(
                "ALTER TABLE tbl_avatar CONVERT TO CHARACTER SET utf8"
            ))
        );
        assert_eq!(
            convert_charset_statement("tbl_avatar", "utf8", "utf8_bin"),
            Some(String::from(
                "ALTER TABLE tbl_avatar CONVERT TO CHARACTER SET utf8 COLLATE utf8_bin"
            ))
        );
    }

    #[test]
    fn a_create_plan_issues_the_bootstrap_and_the_backfill() {
        // Definitions with the id column: no drop-back.
        let mut columns = BTreeMap::new();
        columns.insert(
            ID_COLUMN_NAME.to_owned(),
            ColumnInfo::new(
                ColumnType {
                    is_auto_increment: true,
                    ..ColumnType::simple(FieldType::LongLong)
                },
                ColumnIndexType::Primary,
            ),
        );
        let blob_column = ColumnInfo::new(
            ColumnType {
                is_unsigned_or_binary: true,
                default_value: "preset".to_owned(),
                ..ColumnType::simple(FieldType::Blob)
            },
            ColumnIndexType::None,
        );
        columns.insert("sm_data".to_owned(), blob_column);
        let indexed_column = ColumnInfo::new(
            ColumnType {
                length: 64,
                ..ColumnType::simple(FieldType::VarString)
            },
            ColumnIndexType::Unique,
        );
        columns.insert("sm_name".to_owned(), indexed_column);

        let plan = TablePlan {
            table: "tbl_avatar".to_owned(),
            action: TableAction::Create {
                columns,
                needs_id_column: false,
            },
        };
        let statements = plan_table_statements(&plan, true);
        assert_eq!(
            statements,
            vec![
                String::from(
                    "CREATE TABLE IF NOT EXISTS tbl_avatar (id BIGINT AUTO_INCREMENT, \
                     PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                ),
                String::from(
                    "ALTER TABLE tbl_avatar, ADD COLUMN sm_data BLOB NOT NULL, ADD \
                     COLUMN sm_name VARCHAR(64) NOT NULL"
                ),
                String::from("UPDATE tbl_avatar SET sm_data='preset'"),
                String::from("CREATE UNIQUE INDEX nameIndex ON tbl_avatar (sm_name(64))"),
            ]
        );
        // Withholding: the validator face emits nothing.
        assert!(plan_table_statements(&plan, false).is_empty());
    }

    #[test]
    fn a_create_plan_without_id_drops_the_bootstrap_column() {
        let mut columns = BTreeMap::new();
        columns.insert("sm_id".to_owned(), plain(FieldType::Long));
        let plan = TablePlan {
            table: "tbl_new".to_owned(),
            action: TableAction::Create {
                columns,
                needs_id_column: true,
            },
        };
        assert_eq!(
            plan_table_statements(&plan, true),
            vec![
                String::from(
                    "CREATE TABLE IF NOT EXISTS tbl_new (id BIGINT AUTO_INCREMENT, \
                     PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                ),
                String::from("ALTER TABLE tbl_new, ADD COLUMN sm_id INT NOT NULL, DROP COLUMN id"),
            ]
        );
    }

    #[test]
    fn an_update_plan_orders_drop_indexes_alter_then_reindex() {
        let mut add = BTreeMap::new();
        add.insert("new_col".to_owned(), plain(FieldType::Long));
        let mut drop = BTreeMap::new();
        drop.insert(
            "old_indexed".to_owned(),
            indexed(FieldType::VarString, ColumnIndexType::Unique),
        );
        drop.insert("old_plain".to_owned(), plain(FieldType::Long));
        let mut modify = BTreeMap::new();
        modify.insert(
            "changed".to_owned(),
            ColumnInfo::new(
                ColumnType {
                    length: 3,
                    is_unsigned_or_binary: true,
                    ..ColumnType::simple(FieldType::VarString)
                },
                ColumnIndexType::None,
            ),
        );
        let mut reindex = BTreeMap::new();
        reindex.insert(
            "reindexed".to_owned(),
            IndexedColumnInfo {
                new: indexed(FieldType::Long, ColumnIndexType::NonUnique),
                old_index_type: ColumnIndexType::None,
            },
        );
        reindex.insert(
            "unindexed".to_owned(),
            IndexedColumnInfo {
                new: plain(FieldType::Long),
                old_index_type: ColumnIndexType::NonUnique,
            },
        );
        let plan = TablePlan {
            table: "tbl_avatar".to_owned(),
            action: TableAction::Update(ColumnDiff {
                add,
                drop,
                modify,
                reindex,
            }),
        };
        let statements = plan_table_statements(&plan, true);
        assert_eq!(
            statements,
            vec![
                // The dropped column's index goes first, then the ALTER.
                String::from("ALTER TABLE tbl_avatar DROP INDEX indexedIndex"),
                String::from(
                    "ALTER TABLE tbl_avatar, ADD COLUMN new_col INT NOT NULL, DROP \
                     COLUMN old_indexed, DROP COLUMN old_plain, MODIFY COLUMN changed \
                     VARBINARY(3) NOT NULL"
                ),
                String::from("CREATE INDEX reindexedIndex ON tbl_avatar (reindexed)"),
                String::from("ALTER TABLE tbl_avatar DROP INDEX unindexedIndex"),
            ]
        );
        // Withholding: the adds vanish, the drops and modifies stay.
        let withheld = plan_table_statements(&plan, false);
        assert_eq!(
            withheld[1],
            String::from(
                "ALTER TABLE tbl_avatar, DROP COLUMN old_indexed, DROP COLUMN \
                 old_plain, MODIFY COLUMN changed VARBINARY(3) NOT NULL"
            )
        );
    }

    #[test]
    fn the_whole_plan_ends_with_the_obsolete_drops() {
        let columns = BTreeMap::new();
        let mut observed = BTreeMap::new();
        observed.insert("tbl_ghost".to_owned(), BTreeMap::new());
        let required = vec![("tbl_new".to_owned(), columns.clone())];
        let report = inspect(&required, &observed).expect("distinct tables");
        let statements = plan_statements(&report, true);
        assert_eq!(
            statements,
            vec![
                String::from(
                    "CREATE TABLE IF NOT EXISTS tbl_new (id BIGINT AUTO_INCREMENT, \
                     PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                ),
                String::from("ALTER TABLE tbl_new, DROP COLUMN id"),
                String::from("DROP TABLE tbl_ghost"),
            ]
        );
        assert!(plan_statements(&InspectionReport::default(), true).is_empty());
    }

    #[test]
    fn the_blob_kind_grows_with_the_size() {
        assert_eq!(string_blob_type_name(0), "TINYBLOB");
        assert_eq!(string_blob_type_name(255), "TINYBLOB");
        assert_eq!(string_blob_type_name(256), "BLOB");
        assert_eq!(string_blob_type_name(65_535), "BLOB");
        assert_eq!(string_blob_type_name(65_536), "MEDIUMBLOB");
        assert_eq!(string_blob_type_name(1 << 24), "LONGBLOB");
    }

    #[test]
    fn the_special_tables_match_the_reference_text() {
        let statements = special_table_statements(2048);
        assert_eq!(statements.len(), 12);
        assert_eq!(
            statements[0],
            "CREATE TABLE IF NOT EXISTS bigworldInfo (version INT UNSIGNED NOT \
             NULL, snapshotTime TIMESTAMP NULL, isPasswordHashed BOOL NOT NULL \
             DEFAULT FALSE) ENGINE=InnoDB"
        );
        assert_eq!(
            statements[1],
            "CREATE TABLE IF NOT EXISTS bigworldEntityTypes (typeID INT NOT NULL \
             AUTO_INCREMENT, bigworldID INT, name CHAR(255) CHARACTER SET latin1 \
             NOT NULL UNIQUE,  PRIMARY KEY(typeID), KEY(bigworldID)) ENGINE=InnoDB"
        );
        assert_eq!(
            statements[2],
            "CREATE TABLE IF NOT EXISTS bigworldLogOns (databaseID BIGINT NOT \
             NULL, typeID INT NOT NULL, objectID INT, ip INT UNSIGNED, port \
             SMALLINT UNSIGNED, salt SMALLINT UNSIGNED, shouldAutoLoad BOOL NOT \
             NULL DEFAULT FALSE, PRIMARY KEY(typeID, databaseID)) ENGINE=InnoDB"
        );
        assert_eq!(
            statements[3],
            "CREATE TABLE IF NOT EXISTS bigworldLogOnMapping (logOnName \
             VARBINARY(255) NOT NULL, password VARBINARY(255) NOT NULL, \
             entityType INT, entityID BIGINT, PRIMARY KEY(logOnName)) \
             ENGINE=InnoDB"
        );
        assert_eq!(
            statements[4],
            "CREATE TABLE IF NOT EXISTS bigworldNewID (id INT NOT NULL) ENGINE=InnoDB"
        );
        assert_eq!(
            statements[5],
            "CREATE TABLE IF NOT EXISTS bigworldUsedIDs (id INT NOT NULL) \
             ENGINE=InnoDB"
        );
        assert_eq!(
            statements[6],
            "CREATE TABLE IF NOT EXISTS bigworldGameTime (time INT NOT NULL) \
             ENGINE=InnoDB"
        );
        assert_eq!(
            statements[7],
            "CREATE TABLE IF NOT EXISTS bigworldSpaces (id INT NOT NULL UNIQUE) \
             ENGINE=InnoDB"
        );
        assert_eq!(
            statements[8],
            "CREATE TABLE IF NOT EXISTS bigworldSpaceData (id INT NOT NULL, INDEX \
             (id), spaceEntryID BIGINT NOT NULL, entryKey SMALLINT UNSIGNED NOT \
             NULL, data BLOB NOT NULL ) ENGINE=InnoDB"
        );
        assert_eq!(
            statements[9],
            "ALTER TABLE bigworldSpaceData MODIFY data BLOB"
        );
        assert_eq!(
            statements[10],
            "CREATE TABLE IF NOT EXISTS bigworldSecondaryDatabases (ip INT \
             UNSIGNED NOT NULL, port SMALLINT UNSIGNED NOT NULL, location BLOB NOT \
             NULL, INDEX addr (ip, port)) ENGINE=InnoDB"
        );
        // A tiny setting sizes the space-data blob down; a huge one sizes up.
        assert!(special_table_statements(100)[8].contains("data TINYBLOB NOT NULL"));
        assert!(special_table_statements(1 << 24)[8].contains("data LONGBLOB NOT NULL"));
        assert!(special_table_statements(1 << 24)[9].ends_with("MODIFY data LONGBLOB"));
    }

    #[test]
    fn the_refusals_carry_the_reference_messages() {
        assert_eq!(
            SyncRefusal::EngineNotSupported.message(),
            "MySqlSynchronise::doSynchronise: InnoDB is not a supported MySQL \
             storage engine."
        );
        assert_eq!(
            SyncRefusal::TableEnginesMismatch.message(),
            "MySqlSynchronise::doSynchronise: One or more tables are not using \
             the InnoDB engine type"
        );
        assert!(
            SyncRefusal::UnconsolidatedSecondaryDatabases
                .message()
                .contains("consolidate_dbs --clear")
        );
        assert_eq!(
            SyncRefusal::NewerDatabaseVersion { found: 10 }.message(),
            "Unable to upgrade the database to version 9, a newer DB version \
             (10) is already in use."
        );
        assert!(
            SyncRefusal::CannotRemovePasswordHashing
                .message()
                .contains("Cannot remove hashing")
        );
        assert_eq!(DBAPP_CURRENT_VERSION, 9);
        assert_eq!(MYSQL_ENGINE_TYPE, "InnoDB");
    }

    #[test]
    fn an_unsupported_engine_refuses_before_anything_else() {
        // The default request state is NOT_SUPPORTED: the reference's
        // SHOW ENGINES read is what decides, and the tool refuses before
        // creating even the special tables.
        let request = SyncPlanRequest {
            db_version: Some(DBAPP_CURRENT_VERSION + 1),
            ..SyncPlanRequest::default()
        };
        assert_eq!(plan_sync(&request), Err(SyncRefusal::EngineNotSupported));
        // DEFAULT and SUPPORTED both pass the gate.
        let supported = SyncPlanRequest {
            engine_support: EngineSupport::Supported,
            ..SyncPlanRequest::default()
        };
        assert!(plan_sync(&supported).is_ok());
        let default = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            ..SyncPlanRequest::default()
        };
        assert!(plan_sync(&default).is_ok());
    }

    #[test]
    fn a_fresh_database_plans_creation_then_the_definitions() {
        let request = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            character_set: "utf8".to_owned(),
            collation: "utf8_bin".to_owned(),
            should_hash_passwords: true,
            was_password_hashed: Some(false),
            db_version: Some(DBAPP_CURRENT_VERSION),
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&request),
            Ok(vec![
                SyncStep::CreateSpecialTables,
                SyncStep::AlterDatabaseCharset {
                    statement: String::from("ALTER DATABASE CHARACTER SET utf8 COLLATE utf8_bin")
                },
                SyncStep::EnablePasswordHashing,
                SyncStep::SyncEntityDefinitions,
            ])
        );
    }

    #[test]
    fn an_old_version_plans_the_upgrade_and_a_matching_one_skips_it() {
        let request = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            db_version: Some(5),
            was_password_hashed: Some(true),
            should_hash_passwords: true,
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&request),
            Ok(vec![
                SyncStep::CreateSpecialTables,
                SyncStep::UpgradeDatabase { from_version: 5 },
                SyncStep::SyncEntityDefinitions,
            ])
        );
        let current = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            db_version: Some(DBAPP_CURRENT_VERSION),
            was_password_hashed: Some(true),
            should_hash_passwords: true,
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&current),
            Ok(vec![
                SyncStep::CreateSpecialTables,
                SyncStep::SyncEntityDefinitions,
            ])
        );
    }

    #[test]
    fn the_gates_refuse_in_the_reference_order() {
        // A newer database version is read before the character set is
        // altered.
        let newer = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            db_version: Some(DBAPP_CURRENT_VERSION + 1),
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&newer),
            Err(SyncRefusal::NewerDatabaseVersion {
                found: DBAPP_CURRENT_VERSION + 1
            })
        );
        // Unconsolidated secondary databases refuse before the version is
        // even read.
        let unconsolidated = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            secondary_db_count: 2,
            db_version: Some(DBAPP_CURRENT_VERSION + 1),
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&unconsolidated),
            Err(SyncRefusal::UnconsolidatedSecondaryDatabases)
        );
        // Hashing cannot be removed.
        let unhash = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            was_password_hashed: Some(true),
            should_hash_passwords: false,
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&unhash),
            Err(SyncRefusal::CannotRemovePasswordHashing)
        );
    }

    #[test]
    fn a_dry_run_without_tables_stops_before_the_definitions() {
        // No bigworldInfo in a dry run: the version is never read, so the
        // plan goes straight to the entity-type existence check.
        let request = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            dry_run: true,
            info_table_exists: false,
            entity_types_table_exists: false,
            should_hash_passwords: true,
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&request),
            Ok(vec![
                SyncStep::CreateSpecialTables,
                // was_password_hashed unread defaults to false, so the
                // hashing upgrade still plans.
                SyncStep::EnablePasswordHashing,
            ])
        );
        // With the entity types present the definitions are walked too.
        let established = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            dry_run: true,
            info_table_exists: false,
            entity_types_table_exists: true,
            should_hash_passwords: true,
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&established),
            Ok(vec![
                SyncStep::CreateSpecialTables,
                SyncStep::EnablePasswordHashing,
                SyncStep::SyncEntityDefinitions,
            ])
        );
        // An empty character set alters nothing.
        let no_charset = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            entity_types_table_exists: true,
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&no_charset),
            Ok(vec![
                SyncStep::CreateSpecialTables,
                SyncStep::SyncEntityDefinitions,
            ])
        );
    }

    #[test]
    fn the_charset_alter_takes_the_collation_only_when_set() {
        let charset_only = SyncPlanRequest {
            engine_support: EngineSupport::Default,
            character_set: "utf8".to_owned(),
            ..SyncPlanRequest::default()
        };
        assert_eq!(
            plan_sync(&charset_only),
            Ok(vec![
                SyncStep::CreateSpecialTables,
                SyncStep::AlterDatabaseCharset {
                    statement: String::from("ALTER DATABASE CHARACTER SET utf8")
                },
                SyncStep::SyncEntityDefinitions,
            ])
        );
    }

    #[test]
    fn string_defaults_escape_the_whole_reference_set() {
        let mut text = ColumnType {
            length: 30,
            ..ColumnType::simple(FieldType::VarString)
        };
        text.default_value = "a\\b'c\"d\0e\nf\rg\u{1a}h".to_owned();
        assert_eq!(
            text.render(ColumnIndexType::None),
            "VARCHAR(30) DEFAULT 'a\\\\b\\'c\\\"d\\0e\\nf\\rg\\Zh' NOT NULL"
        );
    }

    #[test]
    fn auto_increment_is_a_top_level_equality_term() {
        // The float family ignores the flag inside the optional stuff, so
        // the difference only shows at the operator== level.
        let auto = ColumnType {
            is_auto_increment: true,
            ..ColumnType::simple(FieldType::Float)
        };
        assert_ne!(auto, ColumnType::simple(FieldType::Float));
    }

    #[test]
    fn the_default_equivalences_hold_in_both_directions() {
        let zero = ColumnType {
            default_value: "0".to_owned(),
            ..ColumnType::simple(FieldType::Long)
        };
        // self "0", other empty — the right half of the numeric rule.
        assert_eq!(zero, ColumnType::simple(FieldType::Long));
        // self padded, other empty — the right half of the CHAR rule.
        let padded = ColumnType {
            length: 3,
            default_value: "   ".to_owned(),
            ..ColumnType::simple(FieldType::String)
        };
        let bare = ColumnType {
            length: 3,
            ..ColumnType::simple(FieldType::String)
        };
        assert_eq!(padded, bare);
    }

    #[test]
    fn the_remaining_sign_and_text_family_renders() {
        let unsigned = |field_type| ColumnType {
            is_unsigned_or_binary: true,
            ..ColumnType::simple(field_type)
        };
        assert_eq!(
            unsigned(FieldType::Tiny).render(ColumnIndexType::None),
            "TINYINT UNSIGNED NOT NULL"
        );
        assert_eq!(
            unsigned(FieldType::Short).render(ColumnIndexType::None),
            "SMALLINT UNSIGNED NOT NULL"
        );
        assert_eq!(
            unsigned(FieldType::Int24).render(ColumnIndexType::None),
            "MEDIUMINT UNSIGNED NOT NULL"
        );
        assert_eq!(
            unsigned(FieldType::LongLong).render(ColumnIndexType::None),
            "BIGINT UNSIGNED NOT NULL"
        );
        assert_eq!(
            unsigned(FieldType::TinyBlob).render(ColumnIndexType::None),
            "TINYBLOB NOT NULL"
        );
        assert_eq!(
            unsigned(FieldType::MediumBlob).render(ColumnIndexType::None),
            "MEDIUMBLOB NOT NULL"
        );
        assert_eq!(
            unsigned(FieldType::LongBlob).render(ColumnIndexType::None),
            "LONGBLOB NOT NULL"
        );
        // The unknown-type arm renders just the trailing qualifier — the
        // reference asserts here instead.
        assert_eq!(
            ColumnType::default().render(ColumnIndexType::None),
            " NOT NULL"
        );
    }

    #[test]
    fn a_columnless_create_plan_issues_only_the_bootstrap() {
        // The reference creates the id-only table and has no ALTER left
        // when the definitions want no columns and the id stays.
        let plan = TablePlan {
            table: "tbl_empty".to_owned(),
            action: TableAction::Create {
                columns: BTreeMap::new(),
                needs_id_column: false,
            },
        };
        assert_eq!(
            plan_table_statements(&plan, true),
            vec![String::from(
                "CREATE TABLE IF NOT EXISTS tbl_empty (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
            )]
        );
    }

    #[test]
    fn a_reindex_only_update_plan_skips_the_empty_alter() {
        let mut reindex = BTreeMap::new();
        // New index is the primary key: carried by the table itself, so
        // no statement; the old kind was a plain index to drop, but the
        // primary is undroppable — again no statement.
        reindex.insert(
            "promoted".to_owned(),
            IndexedColumnInfo {
                new: indexed(FieldType::Long, ColumnIndexType::Primary),
                old_index_type: ColumnIndexType::NonUnique,
            },
        );
        reindex.insert(
            "demoted".to_owned(),
            IndexedColumnInfo {
                new: plain(FieldType::Long),
                old_index_type: ColumnIndexType::Primary,
            },
        );
        let plan = TablePlan {
            table: "tbl_avatar".to_owned(),
            action: TableAction::Update(ColumnDiff {
                reindex,
                ..ColumnDiff::default()
            }),
        };
        assert!(plan_table_statements(&plan, true).is_empty());
    }

    #[test]
    fn an_unchanged_table_contributes_no_statements() {
        let mut columns = BTreeMap::new();
        columns.insert("sm_id".to_owned(), plain(FieldType::Long));
        let mut observed = BTreeMap::new();
        observed.insert("tbl_fine".to_owned(), columns.clone());
        let required = vec![("tbl_fine".to_owned(), columns)];
        let report = inspect(&required, &observed).expect("distinct tables");
        assert_eq!(report.tables[0].action, TableAction::Unchanged);
        assert!(plan_statements(&report, true).is_empty());
    }
}
