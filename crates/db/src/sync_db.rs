//! BW-09-05-01 the sync_db tool's schema synchronisation core.
//!
//! `server/tools/sync_db` drives the DB's schema to match the entity
//! definitions. The diff engine is the `TableInspector`
//! (`lib/db_storage_mysql/table_inspector.{hpp,cpp}`): it walks the required
//! tables, reads each table's existing columns, and classifies the
//! difference — `classifyColumns` (`table_inspector.cpp:107-163`) splits the
//! common columns into type-changed and index-changed, and what remains in
//! the old map needs deleting while what remains in the new map needs
//! adding. Two visitors consume the classification:
//!
//! * `TableValidator` (`table_inspector.cpp:172-235`) only reports — the
//!   reviewable difference listing the acceptance asks for, one message per
//!   needed change ("Need to add column %s into table %s", …);
//! * `TableInitialiser` (`server/tools/sync_db/mysql_table_initialiser.cpp`)
//!   applies — `CREATE TABLE IF NOT EXISTS %s (id BIGINT AUTO_INCREMENT,
//!   PRIMARY KEY idKey (id)) ENGINE=InnoDB` for missing tables
//!   (`mysql_table_initialiser.cpp:372-383`), one batched `ALTER TABLE` per
//!   changed table through `AlterTableHelper`
//!   (`mysql_table_initialiser.cpp:50-160`), `CREATE INDEX`/`DROP INDEX` for
//!   changed indexes (`:177-349`), `DROP TABLE` for unvisited tables.
//!
//! The inspector also flags tables that exist in the database but were never
//! visited (`deleteUnvisitedTables`, `table_inspector.cpp:79-105`) and a
//! duplicate table request is an error
//! (`table_inspector.cpp:35-41`).
//!
//! The MySQL connection is modelled as a [`StatementLog`]: the tool records
//! the statements it would execute — the dry-run wrapper
//! (`server/tools/sync_db/mysql_dry_run_connection.cpp`) does exactly this on
//! the real tool.

use std::collections::{BTreeMap, BTreeSet};

/// `ColumnIndexType` (`db_storage_mysql/column_type.hpp:15-22`).
///
/// The full MySQL-side enumeration: this engine reads indexes the database
/// reports, so it needs `Primary`, `ParentId` and `External` too.
/// [`crate::namer::ColumnIndexType`] is the mapping-facing subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColumnIndexType {
    /// `INDEX_TYPE_NONE`.
    None = 0,
    /// `INDEX_TYPE_PRIMARY`.
    Primary = 1,
    /// `INDEX_TYPE_PARENT_ID`.
    ParentId = 2,
    /// `INDEX_TYPE_UNIQUE`.
    Unique = 3,
    /// `INDEX_TYPE_NON_UNIQUE`.
    NonUnique = 4,
    /// `INDEX_TYPE_EXTERNAL`.
    External = 5,
}

/// The column's storage family, as far as the index statements care
/// (`MYSQL_TYPE_*` in `mysql_table_initialiser.cpp:194-217`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// `MYSQL_TYPE_VAR_STRING` — indexed without a length cap.
    VarString,
    /// `MYSQL_TYPE_VARCHAR` and the blob/text family.
    VarChar,
    /// `MYSQL_TYPE_TINY_BLOB`.
    TinyBlob,
    /// `MYSQL_TYPE_MEDIUM_BLOB`.
    MediumBlob,
    /// `MYSQL_TYPE_LONG_BLOB`.
    LongBlob,
    /// `MYSQL_TYPE_BLOB`.
    Blob,
    /// `MYSQL_TYPE_STRING`.
    Char,
    /// Any other field type — indexed without a length suffix.
    Other,
}

impl FieldType {
    /// Whether the index statement carries a `(length)` suffix
    /// (`mysql_table_initialiser.cpp:205-216`).
    fn indexes_with_length(self) -> bool {
        matches!(
            self,
            FieldType::VarString
                | FieldType::VarChar
                | FieldType::TinyBlob
                | FieldType::MediumBlob
                | FieldType::LongBlob
                | FieldType::Blob
                | FieldType::Char
        )
    }
}

/// One column's comparable DDL facts
/// (`TableMetaData::ColumnInfo`, `db_storage_mysql/table_meta_data.hpp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnInfo {
    /// The column's DDL fragment — `ColumnType::getAsString( connection,
    /// indexType )`, e.g. `VARCHAR(255)` or `BIGINT`.
    pub column_type: String,
    /// The storage family, for the index-length rules.
    pub field_type: FieldType,
    /// The column's declared length, for the index-length rules.
    pub length: u32,
    /// The index the column requires (or the DB reports).
    pub index_type: ColumnIndexType,
    /// Whether the type takes an inline `DEFAULT`; blob types do not, and
    /// their default is back-filled with an `UPDATE`
    /// (`mysql_table_initialiser.cpp:502-515`).
    pub default_supported: bool,
    /// The value the back-fill `UPDATE` writes.
    pub default_value: String,
}

/// An index difference: the required column plus the index the database
/// actually has (`TableMetaData::IndexedColumnInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedColumnInfo {
    /// The required column's DDL fragment.
    pub column_type: String,
    /// The required column's storage family.
    pub field_type: FieldType,
    /// The required column's declared length.
    pub length: u32,
    /// The index required now.
    pub index_type: ColumnIndexType,
    /// The index the database has.
    pub old_index_type: ColumnIndexType,
}

/// The required columns of a table, by column name — `BW::map` order, so
/// statement and report order follow byte order.
pub type Columns = BTreeMap<String, ColumnInfo>;

/// `generateIndexName` (`db_storage_mysql/table_meta_data.cpp:22-27`): the
/// part of the column name after the first underscore, plus `Index` — so
/// `fm_hp` indexes as `hpIndex`.
pub fn generate_index_name(column_name: &str) -> String {
    let tail = match column_name.find('_') {
        Some(pos) => &column_name[pos + 1..],
        None => column_name,
    };
    format!("{tail}Index")
}

/// `TableMetaData::ColumnInfo::isIndexEqual`
/// (`table_meta_data.cpp:179-197`): an externally created index counts as
/// the BigWorld generated index it stands in for.
pub fn is_index_equal(old: &ColumnInfo, new: &ColumnInfo) -> bool {
    use ColumnIndexType::*;
    if old.index_type == External {
        return matches!(new.index_type, Unique | NonUnique);
    }
    if new.index_type == External {
        return matches!(old.index_type, Unique | NonUnique);
    }
    old.index_type == new.index_type
}

/// What the database has — `TableMetaData::getTableColumns` and
/// `getEntityTables` stand-ins (`db_storage_mysql/table_meta_data.cpp`).
pub trait SchemaSource {
    /// `getTableColumns`: the columns of `table`, empty when the table does
    /// not exist.
    fn table_columns(&self, table: &str) -> Columns;

    /// `getEntityTables`: the entity tables in the database.
    fn entity_tables(&self) -> BTreeSet<String>;
}

/// The index difference column — the `IndexedColumns` map value.
pub type IndexedColumns = BTreeMap<String, IndexedColumnInfo>;

/// `TableInspector::classifyColumns`
/// (`table_inspector.cpp:107-163`): columns present in both maps are removed
/// from both — a type change lands in `updated`, an index change in
/// `indexed` — so afterwards `old` holds the obsolete columns and `new` the
/// columns to add.
pub fn classify_columns(
    old: &mut Columns,
    new: &mut Columns,
) -> (BTreeMap<String, ColumnInfo>, IndexedColumns) {
    let mut updated = BTreeMap::new();
    let mut indexed = IndexedColumns::new();

    let old_names: Vec<String> = old.keys().cloned().collect();
    for name in old_names {
        let Some(new_col) = new.get(&name) else {
            continue;
        };
        let old_col = &old[&name];

        if new_col.column_type != old_col.column_type {
            updated.insert(name.clone(), new_col.clone());
        }

        if !is_index_equal(old_col, new_col) {
            indexed.insert(
                name.clone(),
                IndexedColumnInfo {
                    column_type: new_col.column_type.clone(),
                    field_type: new_col.field_type,
                    length: new_col.length,
                    index_type: new_col.index_type,
                    old_index_type: old_col.index_type,
                },
            );
        }

        old.remove(&name);
        new.remove(&name);
    }

    (updated, indexed)
}

/// The inspector's consumers (`TableInspector`'s virtuals,
/// `db_storage_mysql/table_inspector.hpp:20-77`). Every callback answers
/// whether it succeeded — the inspector folds each into `isSynced`
/// (`table_inspector.cpp:49-65`).
pub trait SyncVisitor {
    /// `onNeedNewTable`.
    fn on_need_new_table(&mut self, table: &str, columns: &Columns) -> bool;

    /// `onExistingTable`: the base default reports success
    /// (`table_inspector.hpp:73-75`).
    fn on_existing_table(&mut self, _table: &str) -> bool {
        true
    }

    /// `onNeedUpdateTable`. The obsolete and to-add sets are the remainders
    /// `classifyColumns` left in the old and new maps.
    fn on_need_update_table(
        &mut self,
        table: &str,
        obsolete: &Columns,
        new: &Columns,
        updated: &Columns,
        indexed: &IndexedColumns,
    ) -> bool;

    /// `onNeedDeleteTables`.
    fn on_need_delete_tables(&mut self, tables: &BTreeSet<String>) -> bool;
}

/// The diff engine (`TableInspector`,
/// `lib/db_storage_mysql/table_inspector.cpp:22-105`).
pub struct TableInspector {
    visited: BTreeSet<String>,
    is_synced: bool,
}

impl TableInspector {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `TableInspector::onVisitTable`
    /// (`table_inspector.cpp:22-72`): compare one required table against the
    /// database. A table requested twice is an error.
    pub fn visit_table(
        &mut self,
        visitor: &mut dyn SyncVisitor,
        schema: &dyn SchemaSource,
        table: &str,
        required: &Columns,
    ) -> Result<(), String> {
        if !self.visited.insert(table.to_string()) {
            return Err(format!("table {table} requested twice"));
        }

        let old = schema.table_columns(table);

        if old.is_empty() {
            // No existing table: the visitor creates (or refuses) it.
            self.is_synced &= visitor.on_need_new_table(table, required);
        } else {
            self.is_synced &= visitor.on_existing_table(table);

            let mut obsolete = old;
            let mut new = required.clone();
            let (updated, indexed) = classify_columns(&mut obsolete, &mut new);

            if !obsolete.is_empty() || !new.is_empty() || !updated.is_empty() || !indexed.is_empty()
            {
                self.is_synced &=
                    visitor.on_need_update_table(table, &obsolete, &new, &updated, &indexed);
            }
        }

        Ok(())
    }

    /// `TableInspector::deleteUnvisitedTables`
    /// (`table_inspector.cpp:79-105`): the entity tables the walk never
    /// visited are handed to the visitor for removal.
    pub fn delete_unvisited_tables(
        &mut self,
        visitor: &mut dyn SyncVisitor,
        schema: &dyn SchemaSource,
    ) -> bool {
        let existing = schema.entity_tables();
        let obsolete: BTreeSet<String> = existing
            .iter()
            .filter(|t| !self.visited.contains(*t))
            .cloned()
            .collect();

        if obsolete.is_empty() {
            return true;
        }

        let deleted = visitor.on_need_delete_tables(&obsolete);
        if !deleted {
            self.is_synced = false;
        }
        deleted
    }

    /// Whether the walk found the database in sync.
    pub fn is_synced(&self) -> bool {
        self.is_synced
    }
}

impl Default for TableInspector {
    fn default() -> Self {
        Self {
            visited: BTreeSet::new(),
            is_synced: true,
        }
    }
}

/// The statements the tool would execute — `MySql::execute` with the dry-run
/// wrapper's recording (`server/tools/sync_db/mysql_dry_run_connection.cpp`)
/// and the INFO messages beside them.
#[derive(Debug, Default)]
pub struct StatementLog {
    /// The SQL statements, in execution order.
    pub statements: Vec<String>,
    /// The INFO lines, in order.
    pub infos: Vec<String>,
}

impl StatementLog {
    fn execute(&mut self, statement: impl Into<String>) {
        self.statements.push(statement.into());
    }

    fn info(&mut self, line: impl Into<String>) {
        self.infos.push(line.into());
    }
}

/// The difference report (`TableValidator`,
/// `table_inspector.cpp:172-235`): one message per needed change, no
/// statement executed, every callback reports failure — the walk ends
/// not-synced.
#[derive(Debug, Default)]
pub struct TableValidator {
    log: StatementLog,
}

impl TableValidator {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// The collected report.
    pub fn report(&self) -> &[String] {
        &self.log.infos
    }
}

impl SyncVisitor for TableValidator {
    fn on_need_new_table(&mut self, table: &str, _columns: &Columns) -> bool {
        self.log.info(format!("\tRequire table {table}"));
        // We didn't create the table.
        false
    }

    fn on_need_update_table(
        &mut self,
        table: &str,
        obsolete: &Columns,
        new: &Columns,
        updated: &Columns,
        indexed: &IndexedColumns,
    ) -> bool {
        for name in new.keys() {
            self.log
                .info(format!("\tNeed to add column {name} into table {table}"));
        }

        for name in obsolete.keys() {
            self.log
                .info(format!("\tNeed to delete column {name} from table {table}"));
        }

        for (name, info) in updated {
            self.log.info(format!(
                "\tNeed to update column {name} in table {table} to {}",
                info.column_type
            ));
        }

        for (name, info) in indexed {
            self.log.info(format!(
                "\tNeed to {} index to column {name} in table {table}",
                if info.index_type == ColumnIndexType::None {
                    "delete"
                } else {
                    "add"
                }
            ));
        }
        // We didn't update the table.
        false
    }

    fn on_need_delete_tables(&mut self, tables: &BTreeSet<String>) -> bool {
        for table in tables {
            self.log.info(format!("Need to remove table {table}"));
        }
        // We didn't delete the tables.
        false
    }
}

/// `AlterTableHelper` (`mysql_table_initialiser.cpp:50-160`): one batched
/// `ALTER TABLE` per changed table.
struct AlterTableHelper {
    table_name: String,
    sql: String,
    additions: u32,
    drops: u32,
    modifications: u32,
}

impl AlterTableHelper {
    fn new(table_name: &str) -> Self {
        Self {
            table_name: table_name.to_string(),
            sql: format!("ALTER TABLE {table_name}"),
            additions: 0,
            drops: 0,
            modifications: 0,
        }
    }

    fn num_altered_items(&self) -> u32 {
        self.additions + self.drops + self.modifications
    }

    fn add_column(&mut self, column_name: &str, column_type: &str) {
        if self.num_altered_items() > 0 {
            self.sql.push(',');
        }
        self.sql
            .push_str(&format!(" ADD COLUMN {column_name} {column_type}"));
        self.additions += 1;
    }

    fn drop_column(&mut self, column_name: &str) {
        if self.num_altered_items() > 0 {
            self.sql.push(',');
        }
        self.sql.push_str(&format!(" DROP COLUMN {column_name}"));
        self.drops += 1;
    }

    fn modify_column(&mut self, column_name: &str, column_type: &str) {
        if self.num_altered_items() > 0 {
            self.sql.push(',');
        }
        self.sql
            .push_str(&format!(" MODIFY COLUMN {column_name} {column_type}"));
        self.modifications += 1;
    }

    fn execute(&mut self, log: &mut StatementLog, is_verbose: bool) -> u32 {
        if self.num_altered_items() > 0 {
            if is_verbose {
                log.info(format!(
                    "\tAdding {} columns into table {}",
                    self.additions,
                    self.table_name()
                ));
                log.info(format!(
                    "\tDeleting {} columns from table {}",
                    self.drops,
                    self.table_name()
                ));
                log.info(format!(
                    "\tUpdating {} columns from table {}",
                    self.modifications,
                    self.table_name()
                ));
                log.info(format!("SQL: {}", self.sql));
            }
            let sql = std::mem::take(&mut self.sql);
            log.execute(sql);
        }
        self.num_altered_items()
    }

    /// The table this helper alters.
    fn table_name(&self) -> &str {
        &self.table_name
    }
}

/// The `id` column the reference creates every new table with
/// (`ID_COLUMN_NAME`, `db_storage_mysql/namer.hpp`).
const ID_COLUMN_NAME: &str = "id";

/// The `parentID` index's fixed name (`PARENTID_INDEX_NAME`,
/// `db_storage_mysql/table_meta_data.hpp:18`).
const PARENTID_INDEX_NAME: &str = "parentIDIndex";

/// The storage engine every table is created with (`MYSQL_ENGINE_TYPE`,
/// `db_storage_mysql/wrapper.hpp:32`).
const MYSQL_ENGINE_TYPE: &str = "InnoDB";

/// The applier (`TableInitialiser`,
/// `server/tools/sync_db/mysql_table_initialiser.cpp`).
pub struct TableInitialiser {
    allow_new: bool,
    character_set: String,
    collation: String,
    log: StatementLog,
}

impl TableInitialiser {
    /// Constructor.
    pub fn new(allow_new: bool, character_set: &str, collation: &str) -> Self {
        Self {
            allow_new,
            character_set: character_set.to_string(),
            collation: collation.to_string(),
            log: StatementLog::default(),
        }
    }

    /// The statements the applier issued.
    pub fn statements(&self) -> &[String] {
        &self.log.statements
    }

    /// The INFO lines the applier emitted.
    pub fn infos(&self) -> &[String] {
        &self.log.infos
    }

    /// `generateCreateIndexStatement`
    /// (`mysql_table_initialiser.cpp:177-222`): `CREATE [UNIQUE ]INDEX
    /// <name> ON <table> (<col>[(<len>)])`, the length suffix only for the
    /// string and blob families, capped at 255 characters for everything but
    /// `VAR_STRING`.
    fn create_index_statement(
        &mut self,
        table: &str,
        column: &str,
        info: &IndexedColumnInfo,
    ) -> String {
        let index_name = generate_index_name(column);
        let mut sql = String::from("CREATE ");

        if info.index_type == ColumnIndexType::Unique {
            sql.push_str("UNIQUE ");
        }

        sql.push_str(&format!("INDEX {index_name} ON {table} ({column}"));

        if info.field_type.indexes_with_length() {
            const MAX_TEXT_BLOB_INDEX_LENGTH: u32 = 255;
            let mut index_length = info.length;
            // Only VAR_STRING escapes the cap (`mysql_table_initialiser.cpp:
            // 206-216`).
            if info.field_type != FieldType::VarString && index_length > MAX_TEXT_BLOB_INDEX_LENGTH
            {
                self.log.info(format!(
                    "Column {column} on table {table} can only be indexed up to the first \
                     {} characters (column is of length {}).",
                    MAX_TEXT_BLOB_INDEX_LENGTH, info.length
                ));
                index_length = MAX_TEXT_BLOB_INDEX_LENGTH;
            }
            sql.push_str(&format!("({index_length})"));
        }

        sql.push(')');
        sql
    }

    /// `createIndex` (`mysql_table_initialiser.cpp:244-303`).
    fn create_index(&mut self, table: &str, column: &str, info: &IndexedColumnInfo) {
        match info.index_type {
            ColumnIndexType::None => {}
            // Created with the table; it cannot be added afterwards.
            ColumnIndexType::Primary => {}
            ColumnIndexType::Unique | ColumnIndexType::NonUnique => {
                let sql = self.create_index_statement(table, column, info);
                self.log.execute(sql);
            }
            // Externally created indexes are left alone.
            ColumnIndexType::External => {}
            ColumnIndexType::ParentId => {
                self.log.execute(format!(
                    "CREATE INDEX {PARENTID_INDEX_NAME} ON {table} ({column})"
                ));
            }
        }
    }

    /// `removeIndex` (`mysql_table_initialiser.cpp:311-350`).
    fn remove_index(&mut self, table: &str, column: &str, index_type: ColumnIndexType) {
        match index_type {
            ColumnIndexType::None => {}
            // Can't delete the primary index.
            ColumnIndexType::Primary => {}
            // Externally created indexes are left alone.
            ColumnIndexType::External => {}
            ColumnIndexType::Unique | ColumnIndexType::NonUnique => {
                let index_name = generate_index_name(column);
                self.log
                    .execute(format!("ALTER TABLE {table} DROP INDEX {index_name}"));
            }
            ColumnIndexType::ParentId => {
                self.log.execute(format!(
                    "ALTER TABLE {table} DROP INDEX {PARENTID_INDEX_NAME}"
                ));
            }
        }
    }

    /// `addColumns` (`mysql_table_initialiser.cpp:516-547`).
    fn add_columns(&mut self, _table: &str, columns: &Columns, helper: &mut AlterTableHelper) {
        for (name, info) in columns {
            helper.add_column(name, &info.column_type);
        }
    }

    /// `dropColumns` (`mysql_table_initialiser.cpp:549-596`): the index is
    /// removed before the column.
    fn drop_columns(&mut self, table: &str, columns: &Columns, helper: &mut AlterTableHelper) {
        for (name, info) in columns {
            self.remove_index(table, name, info.index_type);
            helper.drop_column(name);
        }
    }

    /// `updateColumns` (`mysql_table_initialiser.cpp:598-629`).
    fn update_columns(&mut self, _table: &str, columns: &Columns, helper: &mut AlterTableHelper) {
        for (name, info) in columns {
            helper.modify_column(name, &info.column_type);
        }
    }

    /// `indexColumns` (`mysql_table_initialiser.cpp:631-662`).
    fn index_columns(&mut self, table: &str, columns: &IndexedColumns) {
        for (name, info) in columns {
            if info.index_type != ColumnIndexType::None {
                self.create_index(table, name, info);
            } else {
                self.remove_index(table, name, info.old_index_type);
            }
        }
    }

    /// `initialiseColumn` (`mysql_table_initialiser.cpp:482-516`): the new
    /// column's index, then the default-value back-fill for types without
    /// inline `DEFAULT` support.
    fn initialise_column(&mut self, table: &str, name: &str, info: &ColumnInfo) {
        let indexed = IndexedColumnInfo {
            column_type: info.column_type.clone(),
            field_type: info.field_type,
            length: info.length,
            index_type: info.index_type,
            old_index_type: info.index_type,
        };
        self.create_index(table, name, &indexed);

        if !info.default_supported {
            self.log.execute(format!(
                "UPDATE {table} SET {name}='{}'",
                info.default_value
            ));
        }
    }
}

impl SyncVisitor for TableInitialiser {
    fn on_need_new_table(&mut self, table: &str, columns: &Columns) -> bool {
        if !self.allow_new {
            self.log
                .info(format!("\tWitholding table creation: {table}"));
            return true;
        }

        self.log.info(format!("\tCreating table {table}"));
        self.log.execute(format!(
            "CREATE TABLE IF NOT EXISTS {table} (id BIGINT AUTO_INCREMENT, \
             PRIMARY KEY idKey (id)) ENGINE={MYSQL_ENGINE_TYPE}"
        ));

        // A table cannot exist without columns, so the id column is created
        // with it and dropped when the required set has none
        // (`mysql_table_initialiser.cpp:372-411`).
        let delete_id_col = !columns.contains_key(ID_COLUMN_NAME);
        let mut new_columns = columns.clone();
        new_columns.remove(ID_COLUMN_NAME);

        let mut helper = AlterTableHelper::new(table);
        self.add_columns(table, &new_columns, &mut helper);
        if delete_id_col {
            helper.drop_column(ID_COLUMN_NAME);
        }
        helper.execute(&mut self.log, false);

        for (name, info) in &new_columns {
            self.initialise_column(table, name, info);
        }

        true
    }

    fn on_existing_table(&mut self, table: &str) -> bool {
        if self.character_set.is_empty() {
            // If not set, leave as-is.
            return true;
        }

        let mut sql = format!(
            "ALTER TABLE {table} CONVERT TO CHARACTER SET {}",
            self.character_set
        );
        if !self.collation.is_empty() {
            sql.push_str(&format!(" COLLATE {}", self.collation));
        }
        self.log.execute(sql);
        true
    }

    fn on_need_update_table(
        &mut self,
        table: &str,
        obsolete: &Columns,
        new: &Columns,
        updated: &Columns,
        indexed: &IndexedColumns,
    ) -> bool {
        let mut helper = AlterTableHelper::new(table);

        if self.allow_new {
            self.add_columns(table, new, &mut helper);
        }
        self.drop_columns(table, obsolete, &mut helper);
        self.update_columns(table, updated, &mut helper);
        helper.execute(&mut self.log, true);

        self.index_columns(table, indexed);
        true
    }

    fn on_need_delete_tables(&mut self, tables: &BTreeSet<String>) -> bool {
        for table in tables {
            self.log.info(format!("\tDeleting table {table}"));
            self.log.execute(format!("DROP TABLE {table}"));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain integer column.
    fn int_column() -> ColumnInfo {
        ColumnInfo {
            column_type: "INT".to_string(),
            field_type: FieldType::Other,
            length: 4,
            index_type: ColumnIndexType::None,
            default_supported: true,
            default_value: String::new(),
        }
    }

    /// A big integer column.
    fn bigint_column() -> ColumnInfo {
        ColumnInfo {
            column_type: "BIGINT".to_string(),
            field_type: FieldType::Other,
            length: 8,
            index_type: ColumnIndexType::None,
            default_supported: true,
            default_value: String::new(),
        }
    }

    /// An indexed, unique, length-capped string column.
    fn name_column(index: ColumnIndexType) -> ColumnInfo {
        ColumnInfo {
            column_type: "VARCHAR(255)".to_string(),
            field_type: FieldType::VarChar,
            length: 255,
            index_type: index,
            default_supported: true,
            default_value: String::new(),
        }
    }

    /// A blob column whose default needs back-filling.
    fn blob_column() -> ColumnInfo {
        ColumnInfo {
            column_type: "BLOB".to_string(),
            field_type: FieldType::Blob,
            length: 65535,
            index_type: ColumnIndexType::None,
            default_supported: false,
            default_value: "0".to_string(),
        }
    }

    /// The `id` column every entity table requires.
    fn id_column() -> ColumnInfo {
        ColumnInfo {
            column_type: "BIGINT".to_string(),
            field_type: FieldType::Other,
            length: 8,
            index_type: ColumnIndexType::Primary,
            default_supported: true,
            default_value: String::new(),
        }
    }

    /// The `parentID` column.
    fn parent_id_column() -> ColumnInfo {
        ColumnInfo {
            column_type: "BIGINT".to_string(),
            field_type: FieldType::Other,
            length: 8,
            index_type: ColumnIndexType::ParentId,
            default_supported: true,
            default_value: String::new(),
        }
    }

    /// An in-memory database for the walk.
    #[derive(Default)]
    struct MemorySchema {
        tables: BTreeMap<String, Columns>,
    }

    impl MemorySchema {
        fn with_table(mut self, name: &str, columns: Columns) -> Self {
            self.tables.insert(name.to_string(), columns);
            self
        }
    }

    impl SchemaSource for MemorySchema {
        fn table_columns(&self, table: &str) -> Columns {
            self.tables.get(table).cloned().unwrap_or_default()
        }

        fn entity_tables(&self) -> BTreeSet<String> {
            self.tables.keys().cloned().collect()
        }
    }

    /// `generateIndexName` (`table_meta_data.cpp:22-27`).
    #[test]
    fn the_index_name_strips_the_prefix() {
        assert_eq!(generate_index_name("fm_hp"), "hpIndex");
        assert_eq!(generate_index_name("parentID"), "parentIDIndex");
    }

    /// `isIndexEqual` (`table_meta_data.cpp:179-197`).
    #[test]
    fn an_external_index_stands_in_for_a_generated_one() {
        let external = name_column(ColumnIndexType::External);
        assert!(is_index_equal(
            &external,
            &name_column(ColumnIndexType::Unique)
        ));
        assert!(is_index_equal(
            &name_column(ColumnIndexType::NonUnique),
            &external
        ));
        assert!(!is_index_equal(
            &external,
            &name_column(ColumnIndexType::None)
        ));
        // Equal requirements compare equal.
        assert!(is_index_equal(
            &name_column(ColumnIndexType::Unique),
            &name_column(ColumnIndexType::Unique)
        ));
        assert!(!is_index_equal(
            &name_column(ColumnIndexType::None),
            &name_column(ColumnIndexType::Unique)
        ));
    }

    /// `classifyColumns` (`table_inspector.cpp:107-163`): the remainders and
    /// the two difference maps.
    #[test]
    fn classification_splits_common_and_differing_columns() {
        let mut old = Columns::new();
        old.insert("id".to_string(), id_column());
        old.insert("sm_level".to_string(), int_column());
        old.insert("gone".to_string(), int_column());
        old.insert("name".to_string(), name_column(ColumnIndexType::None));

        let mut new = Columns::new();
        new.insert("id".to_string(), id_column());
        new.insert("sm_level".to_string(), bigint_column());
        new.insert("fresh".to_string(), int_column());
        new.insert("name".to_string(), name_column(ColumnIndexType::Unique));

        let (updated, indexed) = classify_columns(&mut old, &mut new);

        // The type change is classified, the index change is classified.
        assert_eq!(updated.len(), 1);
        assert_eq!(updated["sm_level"].column_type, "BIGINT");
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed["name"].index_type, ColumnIndexType::Unique);
        assert_eq!(indexed["name"].old_index_type, ColumnIndexType::None);

        // What remains is obsolete (old) and to-add (new).
        assert_eq!(old.len(), 1);
        assert!(old.contains_key("gone"));
        assert_eq!(new.len(), 1);
        assert!(new.contains_key("fresh"));
    }

    /// `TableValidator`'s report: one message per needed change
    /// (`table_inspector.cpp:172-235`).
    #[test]
    fn the_validator_reports_the_reviewable_difference() {
        let schema = MemorySchema::default()
            .with_table(
                "tbl_Account",
                Columns::from_iter([
                    ("id".to_string(), id_column()),
                    ("sm_level".to_string(), int_column()),
                    ("gone".to_string(), int_column()),
                    ("name".to_string(), name_column(ColumnIndexType::Unique)),
                ]),
            )
            .with_table("tbl_Obsolete", Columns::new());

        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("sm_level".to_string(), bigint_column());
        required.insert("name".to_string(), name_column(ColumnIndexType::None));
        required.insert("fresh".to_string(), int_column());

        let mut validator = TableValidator::new();
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut validator, &schema, "tbl_Account", &required)
            .expect("visited once");
        inspector.delete_unvisited_tables(&mut validator, &schema);

        assert_eq!(
            validator.report(),
            &[
                // adds first, in map order
                "\tNeed to add column fresh into table tbl_Account".to_string(),
                // then deletes, in map order
                "\tNeed to delete column gone from table tbl_Account".to_string(),
                // then updates
                "\tNeed to update column sm_level in table tbl_Account to BIGINT".to_string(),
                // then index changes: the unique index is required to go
                "\tNeed to delete index to column name in table tbl_Account".to_string(),
                // the unvisited table
                "Need to remove table tbl_Obsolete".to_string(),
            ]
        );
        // The validator never applies anything, so the walk is not synced.
        assert!(!inspector.is_synced());
    }

    /// A synced database produces no report and stays synced.
    #[test]
    fn a_synced_schema_reports_nothing() {
        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("name".to_string(), name_column(ColumnIndexType::Unique));

        let schema = MemorySchema::default().with_table("tbl_Account", required.clone());

        let mut validator = TableValidator::new();
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut validator, &schema, "tbl_Account", &required)
            .expect("visited once");
        inspector.delete_unvisited_tables(&mut validator, &schema);

        assert!(validator.report().is_empty());
        assert!(inspector.is_synced());
    }

    /// A duplicate table request is an error
    /// (`table_inspector.cpp:35-41`).
    #[test]
    fn a_table_requested_twice_is_an_error() {
        let schema = MemorySchema::default();
        let required = Columns::new();
        let mut inspector = TableInspector::new();
        let mut validator = TableValidator::new();

        inspector
            .visit_table(&mut validator, &schema, "tbl_Account", &required)
            .expect("first visit");

        let err = inspector
            .visit_table(&mut validator, &schema, "tbl_Account", &required)
            .expect_err("duplicate is refused");
        assert_eq!(err, "table tbl_Account requested twice");
    }

    /// `TableInitialiser`'s new-table sequence
    /// (`mysql_table_initialiser.cpp:372-411`).
    #[test]
    fn the_initialiser_creates_a_missing_table() {
        let schema = MemorySchema::default();
        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("hp".to_string(), int_column());
        required.insert("name".to_string(), name_column(ColumnIndexType::Unique));

        let mut initialiser = TableInitialiser::new(true, "", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Account", &required)
            .expect("visited once");

        assert_eq!(
            initialiser.statements(),
            &[
                "CREATE TABLE IF NOT EXISTS tbl_Account (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                    .to_string(),
                // One batched ALTER adds the property columns; the id column
                // was created with the table and is kept.
                "ALTER TABLE tbl_Account ADD COLUMN hp INT, ADD COLUMN name VARCHAR(255)"
                    .to_string(),
                // The unique index follows; `hp` carries no index, so it
                // gets no statement (`createIndex`, INDEX_TYPE_NONE).
                "CREATE UNIQUE INDEX nameIndex ON tbl_Account (name(255))".to_string(),
            ]
        );
        assert_eq!(initialiser.infos(), &["\tCreating table tbl_Account"]);
        assert!(inspector.is_synced());
    }

    /// A required table without an id column has the placeholder dropped
    /// (`mysql_table_initialiser.cpp:390-403`).
    #[test]
    fn the_placeholder_id_column_is_dropped_when_not_required() {
        let schema = MemorySchema::default();
        let mut required = Columns::new();
        required.insert("sm_count".to_string(), int_column());

        let mut initialiser = TableInitialiser::new(true, "", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Counter", &required)
            .expect("visited once");

        assert_eq!(
            initialiser.statements(),
            &[
                "CREATE TABLE IF NOT EXISTS tbl_Counter (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                    .to_string(),
                "ALTER TABLE tbl_Counter ADD COLUMN sm_count INT, DROP COLUMN id".to_string(),
            ]
        );
    }

    /// `allowNew = false` withholds the creation (`:364-370`).
    #[test]
    fn the_initialiser_withholds_a_new_table() {
        let schema = MemorySchema::default();
        let required = Columns::new();

        let mut initialiser = TableInitialiser::new(false, "", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Account", &required)
            .expect("visited once");

        assert!(initialiser.statements().is_empty());
        assert_eq!(
            initialiser.infos(),
            &["\tWitholding table creation: tbl_Account"]
        );
        assert!(inspector.is_synced(), "withholding still counts as done");
    }

    /// The update path: one batched ALTER with add, drop and modify clauses,
    /// then the index changes (`mysql_table_initialiser.cpp:437-455`).
    #[test]
    fn the_initialiser_alters_a_changed_table() {
        let schema = MemorySchema::default().with_table(
            "tbl_Account",
            Columns::from_iter([
                ("id".to_string(), id_column()),
                ("sm_level".to_string(), int_column()),
                ("gone".to_string(), int_column()),
                ("name".to_string(), name_column(ColumnIndexType::Unique)),
            ]),
        );

        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("sm_level".to_string(), bigint_column());
        required.insert("name".to_string(), name_column(ColumnIndexType::None));
        required.insert("fresh".to_string(), int_column());

        let mut initialiser = TableInitialiser::new(true, "", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Account", &required)
            .expect("visited once");

        assert_eq!(
            initialiser.statements(),
            &[
                // One ALTER carries add, drop and modify together.
                "ALTER TABLE tbl_Account ADD COLUMN fresh INT, DROP COLUMN gone, \
                 MODIFY COLUMN sm_level BIGINT"
                    .to_string(),
                // The index change: the unique index goes.
                "ALTER TABLE tbl_Account DROP INDEX nameIndex".to_string(),
            ]
        );
        assert_eq!(
            initialiser.infos(),
            &[
                "\tAdding 1 columns into table tbl_Account".to_string(),
                "\tDeleting 1 columns from table tbl_Account".to_string(),
                "\tUpdating 1 columns from table tbl_Account".to_string(),
                "SQL: ALTER TABLE tbl_Account ADD COLUMN fresh INT, DROP COLUMN gone, \
                 MODIFY COLUMN sm_level BIGINT"
                    .to_string(),
            ]
        );
        assert!(inspector.is_synced());
    }

    /// The unvisited tables are dropped (`mysql_table_initialiser.cpp:
    /// 526-539`).
    #[test]
    fn the_initialiser_drops_unvisited_tables() {
        let schema = MemorySchema::default()
            .with_table("tbl_Obsolete", Columns::new())
            .with_table("tbl_AlsoGone", Columns::new());

        let mut initialiser = TableInitialiser::new(true, "", "");
        let mut inspector = TableInspector::new();
        inspector.delete_unvisited_tables(&mut initialiser, &schema);

        assert_eq!(
            initialiser.statements(),
            &[
                "DROP TABLE tbl_AlsoGone".to_string(),
                "DROP TABLE tbl_Obsolete".to_string()
            ]
        );
        assert_eq!(
            initialiser.infos(),
            &[
                "\tDeleting table tbl_AlsoGone".to_string(),
                "\tDeleting table tbl_Obsolete".to_string()
            ]
        );
        assert!(inspector.is_synced());
    }

    /// A configured character set converts every existing table
    /// (`mysql_table_initialiser.cpp:414-436`).
    #[test]
    fn the_initialiser_converts_the_character_set() {
        let schema = MemorySchema::default().with_table(
            "tbl_Account",
            Columns::from_iter([("id".to_string(), id_column())]),
        );

        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());

        let mut initialiser = TableInitialiser::new(true, "utf8", "utf8_bin");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Account", &required)
            .expect("visited once");

        assert_eq!(
            initialiser.statements(),
            &[
                "ALTER TABLE tbl_Account CONVERT TO CHARACTER SET utf8 COLLATE utf8_bin"
                    .to_string()
            ]
        );
    }

    /// The index-length cap and the default back-fill
    /// (`mysql_table_initialiser.cpp:196-217,502-515`).
    #[test]
    fn long_text_indexes_capped_and_blob_defaults_backfilled() {
        let schema = MemorySchema::default();
        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("data".to_string(), blob_column());
        required.insert(
            "memo".to_string(),
            ColumnInfo {
                column_type: "VARCHAR(1024)".to_string(),
                field_type: FieldType::VarChar,
                length: 1024,
                index_type: ColumnIndexType::NonUnique,
                default_supported: true,
                default_value: String::new(),
            },
        );

        let mut initialiser = TableInitialiser::new(true, "", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Account", &required)
            .expect("visited once");

        assert_eq!(
            initialiser.statements(),
            &[
                "CREATE TABLE IF NOT EXISTS tbl_Account (id BIGINT AUTO_INCREMENT, \
                 PRIMARY KEY idKey (id)) ENGINE=InnoDB"
                    .to_string(),
                "ALTER TABLE tbl_Account ADD COLUMN data BLOB, ADD COLUMN memo VARCHAR(1024)"
                    .to_string(),
                // The blob's default is back-filled.
                "UPDATE tbl_Account SET data='0'".to_string(),
                // The memo index is capped at 255 characters.
                "CREATE INDEX memoIndex ON tbl_Account (memo(255))".to_string(),
            ]
        );
        assert_eq!(
            initialiser.infos(),
            &[
                "\tCreating table tbl_Account".to_string(),
                "Column memo on table tbl_Account can only be indexed up to the first \
                 255 characters (column is of length 1024)."
                    .to_string(),
            ]
        );
    }

    /// The parentID index has its fixed name and statements
    /// (`mysql_table_initialiser.cpp:326-348`, `table_meta_data.hpp:18`).
    #[test]
    fn the_parent_id_index_is_managed_under_its_fixed_name() {
        let schema = MemorySchema::default();
        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("parentID".to_string(), parent_id_column());

        let mut initialiser = TableInitialiser::new(true, "", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_CellEntity", &required)
            .expect("visited once");

        assert!(
            initialiser
                .statements()
                .iter()
                .any(|s| { s == "CREATE INDEX parentIDIndex ON tbl_CellEntity (parentID)" }),
            "the create path raises the parentID index under its fixed name"
        );

        // Removing it uses the fixed name.
        let mut initialiser = TableInitialiser::new(true, "", "");
        initialiser.remove_index("tbl_CellEntity", "parentID", ColumnIndexType::ParentId);
        assert_eq!(
            initialiser.statements(),
            &["ALTER TABLE tbl_CellEntity DROP INDEX parentIDIndex".to_string()]
        );
    }

    /// A VAR_STRING column is indexed without a length cap
    /// (`mysql_table_initialiser.cpp:194-200`).
    #[test]
    fn a_var_string_index_carries_no_cap() {
        let mut initialiser = TableInitialiser::new(true, "", "");
        let sql = initialiser.create_index_statement(
            "tbl_Account",
            "sm_name",
            &IndexedColumnInfo {
                column_type: "VARCHAR(1024)".to_string(),
                field_type: FieldType::VarString,
                length: 1024,
                index_type: ColumnIndexType::NonUnique,
                old_index_type: ColumnIndexType::None,
            },
        );
        assert_eq!(
            sql,
            // VAR_STRING carries the full declared length, no 255 cap; the
            // index name is the part after the underscore
            // (`generateIndexName`, `table_meta_data.cpp:22-27`).
            "CREATE INDEX nameIndex ON tbl_Account (sm_name(1024))"
        );
        assert!(initialiser.infos().is_empty());
    }

    /// `AlterTableHelper` batches its clauses behind single commas and stays
    /// silent with nothing to do (`mysql_table_initialiser.cpp:50-160`).
    #[test]
    fn the_helper_batches_one_alter_per_table() {
        // The first clause carries no comma; the second one does.
        let mut helper = AlterTableHelper::new("tbl_Account");
        helper.add_column("fresh", "INT");
        helper.add_column("extra", "BIGINT");
        assert_eq!(helper.num_altered_items(), 2);

        // A helper whose first clause is a drop or a modify starts clean too.
        let mut drop_first = AlterTableHelper::new("tbl_Account");
        drop_first.drop_column("gone");
        drop_first.modify_column("sm_level", "BIGINT");
        assert_eq!(drop_first.num_altered_items(), 2);

        // A modify as the only clause carries no comma.
        let mut modify_only = AlterTableHelper::new("tbl_Account");
        modify_only.modify_column("sm_level", "BIGINT");
        let mut log = StatementLog::default();
        modify_only.execute(&mut log, false);
        assert_eq!(
            log.statements,
            &["ALTER TABLE tbl_Account MODIFY COLUMN sm_level BIGINT".to_string()]
        );

        // Nothing to do: no statement, and the count comes back zero.
        let mut empty = AlterTableHelper::new("tbl_Account");
        let mut log = StatementLog::default();
        assert_eq!(empty.execute(&mut log, true), 0);
        assert!(log.statements.is_empty());
        assert!(log.infos.is_empty());

        // Verbose execution reports each clause family and the SQL.
        let mut log = StatementLog::default();
        assert_eq!(helper.execute(&mut log, true), 2);
        assert_eq!(
            log.statements,
            &["ALTER TABLE tbl_Account ADD COLUMN fresh INT, ADD COLUMN extra BIGINT".to_string()]
        );
        assert_eq!(
            log.infos,
            &[
                "\tAdding 2 columns into table tbl_Account".to_string(),
                "\tDeleting 0 columns from table tbl_Account".to_string(),
                "\tUpdating 0 columns from table tbl_Account".to_string(),
                "SQL: ALTER TABLE tbl_Account ADD COLUMN fresh INT, ADD COLUMN extra BIGINT"
                    .to_string(),
            ]
        );

        // Quiet execution issues the statement without the report.
        let mut quiet = AlterTableHelper::new("tbl_Account");
        quiet.add_column("fresh", "INT");
        let mut log = StatementLog::default();
        assert_eq!(quiet.execute(&mut log, false), 1);
        assert_eq!(log.statements.len(), 1);
        assert!(log.infos.is_empty());
    }

    /// A column that gains an index on an existing table: the validator
    /// reports the addition and the initialiser raises the index
    /// (`table_inspector.cpp:220-229`,
    /// `mysql_table_initialiser.cpp:631-662`).
    #[test]
    fn a_column_that_gains_an_index_is_indexed() {
        let schema = MemorySchema::default().with_table(
            "tbl_Account",
            Columns::from_iter([
                ("id".to_string(), id_column()),
                ("name".to_string(), name_column(ColumnIndexType::None)),
            ]),
        );

        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("name".to_string(), name_column(ColumnIndexType::Unique));

        let mut validator = TableValidator::new();
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut validator, &schema, "tbl_Account", &required)
            .expect("visited once");
        assert_eq!(
            validator.report(),
            &["\tNeed to add index to column name in table tbl_Account".to_string()]
        );
        assert!(!inspector.is_synced());

        let mut initialiser = TableInitialiser::new(true, "", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Account", &required)
            .expect("visited once");
        assert_eq!(
            initialiser.statements(),
            &["CREATE UNIQUE INDEX nameIndex ON tbl_Account (name(255))".to_string()]
        );
        assert!(inspector.is_synced());
    }

    /// The index kinds the tool neither creates nor deletes: the primary
    /// index lives with the table, external indexes are not ours to touch,
    /// and an untyped column carries no length suffix at all
    /// (`mysql_table_initialiser.cpp:246-252,309-315`).
    #[test]
    fn primary_and_external_indexes_are_left_alone() {
        let mut initialiser = TableInitialiser::new(true, "", "");
        let other = IndexedColumnInfo {
            column_type: "INT".to_string(),
            field_type: FieldType::Other,
            length: 4,
            index_type: ColumnIndexType::NonUnique,
            old_index_type: ColumnIndexType::None,
        };

        // An untyped column is indexed bare, no length suffix; the index
        // name is the part of the column name after the underscore.
        assert_eq!(
            initialiser.create_index_statement("tbl_Account", "sm_count", &other),
            "CREATE INDEX countIndex ON tbl_Account (sm_count)"
        );

        for kind in [ColumnIndexType::Primary, ColumnIndexType::External] {
            let mut info = other.clone();
            info.index_type = kind;

            let mut create = TableInitialiser::new(true, "", "");
            create.create_index("tbl_Account", "sm_count", &info);
            assert!(create.statements().is_empty(), "{kind:?} is not created");

            let mut remove = TableInitialiser::new(true, "", "");
            remove.remove_index("tbl_Account", "sm_count", kind);
            assert!(remove.statements().is_empty(), "{kind:?} is not deleted");
        }
    }

    /// A character set without a collation converts without a `COLLATE`
    /// clause (`mysql_table_initialiser.cpp:414-436`), and with new tables
    /// withheld an existing table is still altered
    /// (`mysql_table_initialiser.cpp:437-455`).
    #[test]
    fn the_charset_alter_omits_an_unset_collation() {
        let schema = MemorySchema::default().with_table(
            "tbl_Account",
            Columns::from_iter([
                ("id".to_string(), id_column()),
                ("sm_level".to_string(), int_column()),
                ("gone".to_string(), int_column()),
            ]),
        );

        let mut required = Columns::new();
        required.insert("id".to_string(), id_column());
        required.insert("sm_level".to_string(), bigint_column());

        let mut initialiser = TableInitialiser::new(false, "utf8", "");
        let mut inspector = TableInspector::new();
        inspector
            .visit_table(&mut initialiser, &schema, "tbl_Account", &required)
            .expect("visited once");

        assert_eq!(
            initialiser.statements(),
            &[
                "ALTER TABLE tbl_Account CONVERT TO CHARACTER SET utf8".to_string(),
                // New columns are withheld with the tables; drops and
                // modifies still apply.
                "ALTER TABLE tbl_Account DROP COLUMN gone, MODIFY COLUMN sm_level BIGINT"
                    .to_string(),
            ]
        );
        assert!(inspector.is_synced());
    }
}
