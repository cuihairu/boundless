//! Statement-log pins for the sync_db port. The double records every
//! statement in order; the reviewable artifact is that log.

use super::*;

/// A `SyncDatabase` double: canned metadata, recorded statements.
struct DoubleSyncDatabase {
    statements: Vec<String>,
    commits: usize,
    engine: EngineSupportState,
    tables_ok: bool,
    bigworld_tables: Vec<String>,
    entity_tables: Vec<String>,
    described: BTreeMap<String, DescribedTable>,
    version: Option<u32>,
    is_password_hashed: Option<bool>,
    secondary_dbs: u32,
    fail_execute_on: Vec<String>,
    upgrades: Vec<u32>,
    fail_upgrade: Option<String>,
}

impl Default for DoubleSyncDatabase {
    fn default() -> Self {
        Self {
            statements: Vec::new(),
            commits: 0,
            engine: EngineSupportState::Default,
            tables_ok: true,
            bigworld_tables: Vec::new(),
            entity_tables: Vec::new(),
            described: BTreeMap::new(),
            version: Some(DBAPP_CURRENT_VERSION),
            is_password_hashed: Some(true),
            secondary_dbs: 0,
            fail_execute_on: Vec::new(),
            upgrades: Vec::new(),
            fail_upgrade: None,
        }
    }
}

/// A fresh database: no version row, no password flag yet.
fn fresh() -> DoubleSyncDatabase {
    DoubleSyncDatabase {
        version: None,
        is_password_hashed: None,
        ..Default::default()
    }
}

impl SyncDatabase for DoubleSyncDatabase {
    fn execute(&mut self, statement: &str) -> Result<(), String> {
        // A fresh database gains its info row through the INSERT.
        if statement.starts_with("INSERT INTO bigworldInfo") {
            self.version = Some(DBAPP_CURRENT_VERSION);
            self.is_password_hashed = Some(false);
        }
        for needle in &self.fail_execute_on {
            if statement.contains(needle) {
                return Err(format!("execute failed: {statement}"));
            }
        }
        self.statements.push(statement.to_string());
        Ok(())
    }

    fn commit(&mut self) {
        self.commits += 1;
    }

    fn engine_support_state(&mut self) -> EngineSupportState {
        self.engine
    }

    fn check_table_engines(&mut self) -> bool {
        self.tables_ok
    }

    fn table_names(&mut self, pattern: &str) -> Vec<String> {
        if pattern == "bigworld%" {
            self.bigworld_tables.clone()
        } else {
            self.entity_tables.clone()
        }
    }

    fn describe_table(&mut self, table: &str) -> DescribedTable {
        self.described.get(table).cloned().unwrap_or_default()
    }

    fn query_version(&mut self) -> Option<u32> {
        self.version
    }

    fn query_is_password_hashed(&mut self) -> Option<bool> {
        self.is_password_hashed
    }

    fn count_secondary_databases(&mut self) -> u32 {
        self.secondary_dbs
    }

    fn upgrade_database(
        &mut self,
        from_version: u32,
        _log: &mut Vec<String>,
    ) -> Result<(), String> {
        self.upgrades.push(from_version);
        match &self.fail_upgrade {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

fn field(name: &str, field_type: FieldType, flags: u32, length: u32, def: Option<&str>) -> Field {
    Field {
        name: name.to_string(),
        field_type: Some(field_type),
        flags,
        length,
        charsetnr: 63,
        charset_width: 1,
        def: def.map(str::to_string),
    }
}

fn col(field_type: FieldType, length: u32) -> ColumnType {
    ColumnType {
        field_type,
        is_unsigned_or_binary: false,
        length,
        default_value: String::new(),
        on_update_cmd: String::new(),
        is_auto_increment: false,
    }
}

fn unsigned(field_type: FieldType) -> ColumnType {
    ColumnType {
        is_unsigned_or_binary: true,
        ..col(field_type, 0)
    }
}

fn info(column_type: ColumnType, index_type: ColumnIndexType) -> ColumnInfo {
    ColumnInfo {
        column_type,
        index_type,
    }
}

fn default_log() -> Vec<String> {
    Vec::new()
}

// -----------------------------------------------------------------------------
// Section: column type rendering
// -----------------------------------------------------------------------------

#[test]
fn renders_integer_family_with_unsigned() {
    assert_eq!(
        unsigned(FieldType::Tiny).get_as_string(ColumnIndexType::None),
        "TINYINT UNSIGNED NOT NULL"
    );
    assert_eq!(
        unsigned(FieldType::Short).get_as_string(ColumnIndexType::None),
        "SMALLINT UNSIGNED NOT NULL"
    );
    assert_eq!(
        unsigned(FieldType::Int24).get_as_string(ColumnIndexType::None),
        "MEDIUMINT UNSIGNED NOT NULL"
    );
    assert_eq!(
        unsigned(FieldType::Long).get_as_string(ColumnIndexType::None),
        "INT UNSIGNED NOT NULL"
    );
    assert_eq!(
        unsigned(FieldType::LongLong).get_as_string(ColumnIndexType::None),
        "BIGINT UNSIGNED NOT NULL"
    );
    assert_eq!(
        unsigned(FieldType::NewDecimal).get_as_string(ColumnIndexType::None),
        "DECIMAL UNSIGNED NOT NULL"
    );
    assert_eq!(
        col(FieldType::Long, 0).get_as_string(ColumnIndexType::None),
        "INT NOT NULL"
    );
}

#[test]
fn renders_float_double_and_temporal_without_unsigned() {
    assert_eq!(
        unsigned(FieldType::Float).get_as_string(ColumnIndexType::None),
        "FLOAT NOT NULL"
    );
    assert_eq!(
        unsigned(FieldType::Double).get_as_string(ColumnIndexType::None),
        "DOUBLE NOT NULL"
    );
    assert_eq!(
        col(FieldType::Timestamp, 0).get_as_string(ColumnIndexType::None),
        "TIMESTAMP NOT NULL"
    );
    assert_eq!(
        col(FieldType::Time, 0).get_as_string(ColumnIndexType::None),
        "TIME NOT NULL"
    );
    assert_eq!(
        col(FieldType::NewDate, 0).get_as_string(ColumnIndexType::None),
        "DATE NOT NULL"
    );
}

#[test]
fn renders_length_carrying_types() {
    assert_eq!(
        col(FieldType::Year, 4).get_as_string(ColumnIndexType::None),
        "YEAR(4) NOT NULL"
    );
    assert_eq!(
        col(FieldType::VarChar, 64).get_as_string(ColumnIndexType::None),
        "VARCHAR(64) NOT NULL"
    );
    assert_eq!(
        col(FieldType::Bit, 1).get_as_string(ColumnIndexType::None),
        "BIT(1) NOT NULL"
    );
    assert_eq!(
        col(FieldType::VarString, 16).get_as_string(ColumnIndexType::None),
        "VARCHAR(16) NOT NULL"
    );
    assert_eq!(
        col(FieldType::String, 32).get_as_string(ColumnIndexType::None),
        "CHAR(32) NOT NULL"
    );
}

#[test]
fn renders_blob_family_as_text_or_blob() {
    assert_eq!(
        col(FieldType::TinyBlob, 0).get_as_string(ColumnIndexType::None),
        "TINYTEXT NOT NULL"
    );
    assert_eq!(
        col(FieldType::MediumBlob, 0).get_as_string(ColumnIndexType::None),
        "MEDIUMTEXT NOT NULL"
    );
    assert_eq!(
        col(FieldType::LongBlob, 0).get_as_string(ColumnIndexType::None),
        "LONGTEXT NOT NULL"
    );
    assert_eq!(
        col(FieldType::Blob, 0).get_as_string(ColumnIndexType::None),
        "TEXT NOT NULL"
    );

    let binary = ColumnType {
        is_unsigned_or_binary: true,
        ..col(FieldType::Blob, 0)
    };
    assert_eq!(binary.get_as_string(ColumnIndexType::None), "BLOB NOT NULL");
    let tiny_binary = ColumnType {
        is_unsigned_or_binary: true,
        ..col(FieldType::TinyBlob, 0)
    };
    assert_eq!(
        tiny_binary.get_as_string(ColumnIndexType::None),
        "TINYBLOB NOT NULL"
    );
}

#[test]
fn renders_default_value_quoted_and_escaped_for_strings() {
    let text = ColumnType {
        default_value: "it's".to_string(),
        ..col(FieldType::String, 16)
    };
    assert_eq!(
        text.get_as_string(ColumnIndexType::None),
        "CHAR(16) DEFAULT 'it\\'s' NOT NULL"
    );

    let number = ColumnType {
        default_value: "42".to_string(),
        ..col(FieldType::Long, 0)
    };
    assert_eq!(
        number.get_as_string(ColumnIndexType::None),
        "INT DEFAULT 42 NOT NULL"
    );

    // Empty default renders no clause at all.
    assert_eq!(
        col(FieldType::Long, 0).get_as_string(ColumnIndexType::None),
        "INT NOT NULL"
    );
}

#[test]
fn renders_auto_increment_and_primary_key_in_reference_order() {
    assert_eq!(
        id_column_type().get_as_string(ColumnIndexType::Primary),
        "BIGINT AUTO_INCREMENT PRIMARY KEY NOT NULL"
    );
    assert_eq!(
        parentid_column_type().get_as_string(ColumnIndexType::ParentId),
        "BIGINT NOT NULL"
    );
}

#[test]
fn blob_columns_render_no_default_even_when_set() {
    let blob = ColumnType {
        default_value: "x".to_string(),
        ..col(FieldType::Blob, 0)
    };
    assert!(!blob.is_default_value_supported());
    assert_eq!(blob.get_as_string(ColumnIndexType::None), "TEXT NOT NULL");
}

#[test]
fn renders_on_update_and_unsupported_type_panics() {
    let timestamp = ColumnType {
        on_update_cmd: "CURRENT_TIMESTAMP".to_string(),
        ..col(FieldType::Timestamp, 0)
    };
    assert_eq!(
        timestamp.get_as_string(ColumnIndexType::None),
        "TIMESTAMP ON UPDATE CURRENT_TIMESTAMP NOT NULL"
    );
    let result =
        std::panic::catch_unwind(|| col(FieldType::Null, 0).get_as_string(ColumnIndexType::None));
    assert!(result.is_err());
}

// -----------------------------------------------------------------------------
// Section: equality quirks
// -----------------------------------------------------------------------------

#[test]
fn equality_ignores_length_for_numerics_but_not_sign() {
    assert_ne!(unsigned(FieldType::Long), col(FieldType::Long, 0));
    assert_eq!(col(FieldType::Long, 4), col(FieldType::Long, 11));
    // Length is ignored but auto-increment is not.
    assert_ne!(id_column_type(), col(FieldType::LongLong, 0));
    assert_eq!(id_column_type(), id_column_type());
}

#[test]
fn equality_of_varchar_compares_length_only() {
    let a = col(FieldType::VarChar, 64);
    let b = ColumnType {
        is_unsigned_or_binary: true,
        ..col(FieldType::VarChar, 64)
    };
    assert_eq!(a, b); // binary flag ignored for VARCHAR
    assert_ne!(a, col(FieldType::VarChar, 65));
}

#[test]
fn equality_of_strings_compares_length_and_binary() {
    let char32 = col(FieldType::String, 32);
    let binary32 = ColumnType {
        is_unsigned_or_binary: true,
        ..col(FieldType::String, 32)
    };
    assert_ne!(char32, binary32);
    assert_eq!(char32, col(FieldType::String, 32));
}

#[test]
fn equality_of_blobs_compares_binary_only() {
    let text = col(FieldType::Blob, 0);
    let other_text = ColumnType {
        ..col(FieldType::Blob, 0)
    };
    assert_eq!(text, other_text);
    let blob = ColumnType {
        is_unsigned_or_binary: true,
        ..col(FieldType::Blob, 0)
    };
    assert_ne!(text, blob);
    // Length is irrelevant for blobs.
    assert_eq!(
        text,
        ColumnType {
            length: 999,
            ..col(FieldType::Blob, 0)
        }
    );
}

#[test]
fn numerical_zero_and_empty_defaults_are_equal() {
    let empty = ColumnType {
        default_value: String::new(),
        ..col(FieldType::Long, 0)
    };
    let zero = ColumnType {
        default_value: "0".to_string(),
        ..col(FieldType::Long, 0)
    };
    let one = ColumnType {
        default_value: "1".to_string(),
        ..col(FieldType::Long, 0)
    };
    assert_eq!(empty, zero);
    assert_eq!(zero, empty);
    assert_ne!(empty, one);
}

#[test]
fn string_default_aliases_the_null_string() {
    let empty = ColumnType {
        default_value: String::new(),
        ..col(FieldType::String, 4)
    };
    let spaces = ColumnType {
        default_value: "    ".to_string(),
        ..col(FieldType::String, 4)
    };
    assert_eq!(empty, spaces);
    assert_ne!(
        spaces,
        ColumnType {
            default_value: "  x ".to_string(),
            ..col(FieldType::String, 4)
        }
    );
    // Length only matters through the null-string width.
    assert_ne!(
        empty,
        ColumnType {
            default_value: "        ".to_string(),
            ..col(FieldType::String, 8)
        }
    );
    // Binary strings pad with NULs, not spaces.
    let binary = ColumnType {
        is_unsigned_or_binary: true,
        ..empty.clone()
    };
    let nuls = ColumnType {
        default_value: "\0\0\0\0".to_string(),
        ..col(FieldType::String, 4)
    };
    assert_eq!(
        binary,
        ColumnType {
            is_unsigned_or_binary: true,
            ..nuls
        }
    );
}

#[test]
fn timestamp_defaults_always_equal_but_on_update_is_ignored() {
    let plain = col(FieldType::Timestamp, 0);
    let stamped = ColumnType {
        default_value: "2020-01-01 00:00:00".to_string(),
        on_update_cmd: "CURRENT_TIMESTAMP".to_string(),
        ..col(FieldType::Timestamp, 0)
    };
    assert_eq!(plain, stamped); // default text and onUpdateCmd not compared
}

#[test]
fn blobs_skip_default_comparison_and_floats_too() {
    let blob_empty = col(FieldType::Blob, 0);
    let blob_set = ColumnType {
        default_value: "x".to_string(),
        ..col(FieldType::Blob, 0)
    };
    assert_eq!(blob_empty, blob_set);
    let float_empty = col(FieldType::Float, 0);
    let float_set = ColumnType {
        default_value: "3.14".to_string(),
        ..col(FieldType::Float, 0)
    };
    assert_ne!(float_empty, float_set); // floats are not simple numericals
}

// -----------------------------------------------------------------------------
// Section: field derivation and helpers
// -----------------------------------------------------------------------------

#[test]
fn blob_fields_remap_to_sized_family_by_charset_width() {
    let mut blob = field("data", FieldType::Blob, 0, 100, None);
    blob.charsetnr = 63;
    let derived = ColumnType::from_field(&blob);
    assert_eq!(derived.field_type, FieldType::TinyBlob);

    blob.length = 300;
    assert_eq!(ColumnType::from_field(&blob).field_type, FieldType::Blob);

    blob.length = 70000;
    assert_eq!(
        ColumnType::from_field(&blob).field_type,
        FieldType::MediumBlob
    );

    blob.length = 1 << 24;
    assert_eq!(
        ColumnType::from_field(&blob).field_type,
        FieldType::LongBlob
    );
}

#[test]
fn field_derivation_carries_flags_and_defaults() {
    let auto = field(
        "id",
        FieldType::LongLong,
        AUTO_INCREMENT_FLAG | PRI_KEY_FLAG,
        8,
        None,
    );
    let derived = ColumnType::from_field(&auto);
    assert!(derived.is_auto_increment);
    assert!(!derive_is_unsigned_or_binary(&auto));

    let mut uint = field("n", FieldType::Long, UNSIGNED_FLAG, 4, None);
    assert!(derive_is_unsigned_or_binary(&uint));
    uint.field_type = Some(FieldType::VarChar);
    assert!(!derive_is_unsigned_or_binary(&uint)); // VARCHAR: not applicable
    uint.field_type = Some(FieldType::String);
    uint.charsetnr = 33;
    assert!(!derive_is_unsigned_or_binary(&uint)); // charset 33 is not binary
}

#[test]
fn derived_default_value_and_unknown_charset_width() {
    let named = field("name", FieldType::VarString, 0, 40, Some("bob"));
    let derived = ColumnType::from_field(&named);
    assert_eq!(derived.default_value, "bob");
    assert_eq!(derived.length, 40); // binary charset -> width 1

    let mut utf8_field = named.clone();
    utf8_field.charset_width = 3;
    let utf8 = ColumnType::from_field(&utf8_field);
    assert_eq!(utf8.length, 13); // 40 / 3

    let mut widths = BTreeMap::new();
    widths.insert(33u16, 3u32);
    assert_eq!(charset_width(33, &widths), 3);
    assert_eq!(charset_width(999, &widths), 1);
}

#[test]
fn blob_ladder_boundaries() {
    assert_eq!(string_col_type(255), FieldType::TinyBlob);
    assert_eq!(string_col_type(256), FieldType::Blob);
    assert_eq!(string_col_type(65535), FieldType::Blob);
    assert_eq!(string_col_type(65536), FieldType::MediumBlob);
    assert_eq!(string_col_type(1 << 24), FieldType::LongBlob);
    assert_eq!(string_col_type(2048), FieldType::Blob);
    assert_eq!(string_col_type_str(255), "TINYBLOB");
    assert_eq!(string_col_type_str(2048), "BLOB");
    assert_eq!(string_col_type_str(65536), "MEDIUMBLOB");
    assert_eq!(string_col_type_str(1 << 24), "LONGBLOB");
}

#[test]
fn escape_mysql_string_covers_the_reference_charset() {
    assert_eq!(escape_mysql_string("plain"), "plain");
    assert_eq!(escape_mysql_string("a\0b"), "a\\0b");
    assert_eq!(escape_mysql_string("a\nb\rc"), "a\\nb\\rc");
    assert_eq!(escape_mysql_string("a\\b"), "a\\\\b");
    assert_eq!(escape_mysql_string("a'b\"c"), "a\\'b\\\"c");
    assert_eq!(escape_mysql_string("a\u{1a}b"), "a\\Zb");
    assert_eq!(escape_mysql_string(""), "");
}

#[test]
fn index_names_strip_up_to_the_first_underscore() {
    assert_eq!(generate_index_name("name"), "nameIndex");
    assert_eq!(generate_index_name("tbl_name"), "nameIndex");
    assert_eq!(generate_index_name("a_b_c"), "b_cIndex");
}

// -----------------------------------------------------------------------------
// Section: index derivation and equality matrix
// -----------------------------------------------------------------------------

fn indices_from(rows: &[(&str, &str, &str)]) -> TableIndices {
    let rows: Vec<IndexRow> = rows
        .iter()
        .map(|(non_unique, key, col)| IndexRow {
            non_unique: (*non_unique).to_string(),
            key_name: (*key).to_string(),
            column_name: (*col).to_string(),
        })
        .collect();
    TableIndices::from_rows(&rows)
}

#[test]
fn derive_index_type_pins_all_seven_branches() {
    let mut log = default_log();
    let empty = indices_from(&[]);

    // Primary key.
    let pk = field("id", FieldType::LongLong, PRI_KEY_FLAG, 8, None);
    assert_eq!(
        derive_index_type(&pk, &empty, &mut log),
        ColumnIndexType::Primary
    );

    // Unique with the generated name.
    let unique = field("name", FieldType::VarString, UNIQUE_KEY_FLAG, 64, None);
    let owned = indices_from(&[("0", "nameIndex", "name")]);
    assert_eq!(
        derive_index_type(&unique, &owned, &mut log),
        ColumnIndexType::Unique
    );

    // Unique with a foreign name -> External plus the warning.
    let foreign = indices_from(&[("0", "someoneElses", "name")]);
    assert_eq!(
        derive_index_type(&unique, &foreign, &mut log),
        ColumnIndexType::External
    );
    assert!(log[0].contains("Found unknown unique index someoneElses for column name"));

    // Multiple key matching parentIDIndex -> ParentId.
    let parent = field("parentID", FieldType::LongLong, MULTIPLE_KEY_FLAG, 8, None);
    let parent_index = indices_from(&[("1", "parentIDIndex", "parentID")]);
    assert_eq!(
        derive_index_type(&parent, &parent_index, &mut log),
        ColumnIndexType::ParentId
    );

    // Multiple key matching the generated name -> NonUnique.
    let plain = field("name", FieldType::VarString, MULTIPLE_KEY_FLAG, 64, None);
    let generated = indices_from(&[("1", "nameIndex", "name")]);
    assert_eq!(
        derive_index_type(&plain, &generated, &mut log),
        ColumnIndexType::NonUnique
    );

    // Multiple key with a foreign name -> External plus the warning.
    let foreign_multi = indices_from(&[("1", "mystery", "name")]);
    assert_eq!(
        derive_index_type(&plain, &foreign_multi, &mut log),
        ColumnIndexType::External
    );
    assert!(log[1].contains("Found unknown multiple key index mystery for column name"));

    // No flags -> None.
    let bare = field("n", FieldType::Long, 0, 4, None);
    assert_eq!(
        derive_index_type(&bare, &empty, &mut log),
        ColumnIndexType::None
    );
}

#[test]
#[should_panic]
fn unique_flag_without_index_row_panics_like_the_mf_assert() {
    let mut log = default_log();
    let empty = indices_from(&[]);
    let unique = field("name", FieldType::VarString, UNIQUE_KEY_FLAG, 64, None);
    derive_index_type(&unique, &empty, &mut log);
}

#[test]
fn is_index_equal_matrix() {
    let mk = |t| info(col(FieldType::Long, 0), t);
    // External counts as BW-generated unique/non-unique.
    assert!(mk(ColumnIndexType::External).is_index_equal(&mk(ColumnIndexType::Unique)));
    assert!(mk(ColumnIndexType::External).is_index_equal(&mk(ColumnIndexType::NonUnique)));
    assert!(mk(ColumnIndexType::Unique).is_index_equal(&mk(ColumnIndexType::External)));
    assert!(!mk(ColumnIndexType::External).is_index_equal(&mk(ColumnIndexType::None)));
    assert!(!mk(ColumnIndexType::None).is_index_equal(&mk(ColumnIndexType::External)));
    // Two externals are still a mismatch: the first branch short-circuits on
    // self == External before the plain equality falls through.
    assert!(!mk(ColumnIndexType::External).is_index_equal(&mk(ColumnIndexType::External)));
    assert!(!mk(ColumnIndexType::Unique).is_index_equal(&mk(ColumnIndexType::NonUnique)));
    assert!(mk(ColumnIndexType::None).is_index_equal(&mk(ColumnIndexType::None)));
    assert!(!mk(ColumnIndexType::ParentId).is_index_equal(&mk(ColumnIndexType::NonUnique)));
}

// -----------------------------------------------------------------------------
// Section: classify and the inspector
// -----------------------------------------------------------------------------

#[test]
fn classify_columns_buckets_the_diff_and_leaves_residues() {
    let mut old = BTreeMap::new();
    old.insert(
        "kept".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::None),
    );
    old.insert(
        "obsolete".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::None),
    );
    old.insert(
        "retyped".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::None),
    );
    old.insert(
        "reindexed".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::None),
    );
    let mut new = BTreeMap::new();
    new.insert(
        "kept".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::None),
    );
    new.insert(
        "retyped".to_string(),
        info(col(FieldType::LongLong, 0), ColumnIndexType::None),
    );
    new.insert(
        "reindexed".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::Unique),
    );
    new.insert(
        "added".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::None),
    );

    let diff = classify_columns(&mut old, &mut new);
    assert_eq!(diff.updated.len(), 1);
    assert!(diff.updated.contains_key("retyped"));
    assert_eq!(diff.indexed.len(), 1);
    assert_eq!(
        diff.indexed["reindexed"].old_index_type,
        ColumnIndexType::None
    );
    assert_eq!(
        diff.indexed["reindexed"].column_info.index_type,
        ColumnIndexType::Unique
    );
    // Residues: obsolete stays in old, added stays in new.
    assert_eq!(old.len(), 1);
    assert!(old.contains_key("obsolete"));
    assert_eq!(new.len(), 1);
    assert!(new.contains_key("added"));
}

#[test]
fn classify_identical_maps_produce_no_diff() {
    let mut old = BTreeMap::new();
    old.insert(
        "a".to_string(),
        info(id_column_type(), ColumnIndexType::Primary),
    );
    let mut new = old.clone();
    let diff = classify_columns(&mut old, &mut new);
    assert!(diff.updated.is_empty() && diff.indexed.is_empty());
    assert!(old.is_empty() && new.is_empty());
}

#[test]
fn visit_table_creates_when_missing_and_rejects_duplicates() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut scan = Inspector::new();
    let mut handler = TableValidator::new();

    let desired = DesiredTable::new("tbl_Account")
        .with_column("id", info(id_column_type(), ColumnIndexType::Primary))
        .with_column(
            "name",
            info(col(FieldType::VarString, 64), ColumnIndexType::Unique),
        );
    scan.visit_table(
        &mut db,
        &mut log,
        &mut handler,
        "tbl_Account",
        &desired.columns,
    )
    .unwrap();
    assert!(!scan.is_synced); // validator reports a required table
    assert!(log.iter().any(|l| l == "\tRequire table tbl_Account\n"));
    assert!(scan.visited.contains("tbl_Account"));

    // Same table again -> the runtime error text.
    let err = scan
        .visit_table(
            &mut db,
            &mut log,
            &mut handler,
            "tbl_Account",
            &desired.columns,
        )
        .unwrap_err();
    assert_eq!(err, "table tbl_Account requested twice");
}

#[test]
fn visit_table_identical_existing_table_only_converts_charset() {
    let mut db = DoubleSyncDatabase::default();
    let described = DescribedTable {
        fields: vec![
            field(
                "id",
                FieldType::LongLong,
                PRI_KEY_FLAG | AUTO_INCREMENT_FLAG,
                8,
                None,
            ),
            field("name", FieldType::VarString, UNIQUE_KEY_FLAG, 64, None),
        ],
        indices: vec![
            IndexRow {
                non_unique: "0".to_string(),
                key_name: "idKey".to_string(),
                column_name: "id".to_string(),
            },
            IndexRow {
                non_unique: "0".to_string(),
                key_name: "nameIndex".to_string(),
                column_name: "name".to_string(),
            },
        ],
    };
    db.described.insert("tbl_Account".to_string(), described);

    let desired = DesiredTable::new("tbl_Account")
        .with_column("id", info(id_column_type(), ColumnIndexType::Primary))
        .with_column(
            "name",
            info(
                ColumnType::from_field(&field(
                    "name",
                    FieldType::VarString,
                    UNIQUE_KEY_FLAG,
                    64,
                    None,
                )),
                ColumnIndexType::Unique,
            ),
        );

    let initialiser = TableInitialiser::new(true, "utf8".to_string(), "utf8_bin".to_string());
    let mut scan = Inspector::new();
    let mut log = default_log();
    let mut handler = initialiser;
    scan.visit_table(
        &mut db,
        &mut log,
        &mut handler,
        "tbl_Account",
        &desired.columns,
    )
    .unwrap();
    assert!(scan.is_synced);
    // Exactly one statement: the character set conversion.
    assert_eq!(
        db.statements,
        vec![
            "ALTER TABLE tbl_Account CONVERT TO CHARACTER SET 'utf8' COLLATE 'utf8_bin'"
                .to_string()
        ]
    );
}

#[test]
fn visit_table_reports_and_applies_the_update_buckets() {
    // Live table has an obsolete column and a mis-typed, mis-indexed one.
    let mut db = DoubleSyncDatabase::default();
    let described = DescribedTable {
        fields: vec![
            field("gone", FieldType::Long, 0, 4, None),
            field("num", FieldType::Long, 0, 4, None),
        ],
        ..Default::default()
    };
    db.described.insert("tbl_Account".to_string(), described);

    let mut validator_log = default_log();
    let mut scan = Inspector::new();
    let mut validator = TableValidator::new();
    let desired = DesiredTable::new("tbl_Account")
        .with_column(
            "num",
            info(col(FieldType::LongLong, 0), ColumnIndexType::Unique),
        )
        .with_column(
            "fresh",
            info(col(FieldType::Long, 0), ColumnIndexType::None),
        );
    scan.visit_table(
        &mut db,
        &mut validator_log,
        &mut validator,
        "tbl_Account",
        &desired.columns,
    )
    .unwrap();
    assert!(!scan.is_synced);
    let expected = [
        "\tNeed to add column fresh into table tbl_Account\n",
        "\tNeed to delete column gone from table tbl_Account\n",
        "\tNeed to update column num in table tbl_Account to BIGINT NOT NULL\n",
        "\tNeed to add index to column num in table tbl_Account\n",
    ];
    for line in expected {
        assert!(validator_log.iter().any(|l| l == line), "missing {line}");
    }

    // An index removal is reported with the "delete" word: the live table
    // carries a generated non-unique index the definitions no longer want.
    let live_index = DescribedTable {
        fields: vec![field("num", FieldType::Long, MULTIPLE_KEY_FLAG, 4, None)],
        indices: vec![IndexRow {
            non_unique: "1".to_string(),
            key_name: "numIndex".to_string(),
            column_name: "num".to_string(),
        }],
    };
    let mut db2 = DoubleSyncDatabase::default();
    db2.described.insert("tbl_Account".to_string(), live_index);
    let mut log2 = default_log();
    let mut scan2 = Inspector::new();
    let mut validator2 = TableValidator::new();
    let desired2 = DesiredTable::new("tbl_Account")
        .with_column("num", info(col(FieldType::Long, 4), ColumnIndexType::None));
    scan2
        .visit_table(
            &mut db2,
            &mut log2,
            &mut validator2,
            "tbl_Account",
            &desired2.columns,
        )
        .unwrap();
    assert!(
        log2.iter()
            .any(|l| l == "\tNeed to delete index to column num in table tbl_Account\n")
    );
}

#[test]
fn delete_unvisited_tables_reports_only_the_difference() {
    let mut db = DoubleSyncDatabase {
        entity_tables: vec![
            "tbl_Account".to_string(),
            "tbl_Ghost".to_string(),
            "tbl_Stale".to_string(),
        ],
        ..Default::default()
    };
    let mut log = default_log();
    let mut scan = Inspector::new();
    scan.visited.insert("tbl_Account".to_string());
    scan.visited.insert("tbl_Ghost".to_string());

    let mut validator = TableValidator::new();
    assert_eq!(
        scan.delete_unvisited_tables(&mut db, &mut log, &mut validator),
        Ok(false)
    );
    assert!(!scan.is_synced); // the failed delete clears the flag
    assert!(log.iter().any(|l| l == "Need to remove table tbl_Stale\n"));

    // Nothing obsolete -> handler untouched, flag stays.
    let mut clean = Inspector::new();
    clean.visited = db.entity_tables.iter().cloned().collect();
    let mut clean_log = default_log();
    let mut initialiser = TableInitialiser::new(true, String::new(), String::new());
    assert_eq!(
        clean.delete_unvisited_tables(&mut db, &mut clean_log, &mut initialiser),
        Ok(true)
    );
    assert!(clean.is_synced);
    assert!(db.statements.is_empty());
}

// -----------------------------------------------------------------------------
// Section: AlterTableHelper
// -----------------------------------------------------------------------------

#[test]
fn alter_table_helper_joins_clauses_and_skips_empty_execution() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut helper = AlterTableHelper::new("tbl_Account");
    assert_eq!(helper.num_altered_items(), 0);
    assert_eq!(helper.execute(&mut db, &mut log), Ok(0));
    assert!(db.statements.is_empty()); // nothing collected, nothing executed

    helper.add_column("a", "INT NOT NULL");
    helper.drop_column("b");
    helper.modify_column("c", "BIGINT NOT NULL");
    assert_eq!(helper.num_altered_items(), 3);
    assert_eq!(helper.execute(&mut db, &mut log), Ok(3));
    assert_eq!(db.statements, vec![
        "ALTER TABLE tbl_Account ADD COLUMN a INT NOT NULL, DROP COLUMN b, MODIFY COLUMN c BIGINT NOT NULL".to_string()
    ]);
    assert!(
        log.iter()
            .any(|l| l == "\tAdding 1 columns into table tbl_Account\n")
    );
    assert!(
        log.iter()
            .any(|l| l == "\tDeleting 1 columns from table tbl_Account\n")
    );
    assert!(
        log.iter()
            .any(|l| l == "\tUpdating 1 columns from table tbl_Account\n")
    );
    assert!(
        log.iter()
            .any(|l| l.starts_with("SQL: ALTER TABLE tbl_Account"))
    );
}

// -----------------------------------------------------------------------------
// Section: TableInitialiser
// -----------------------------------------------------------------------------

#[test]
fn new_table_witholding_reports_and_does_nothing() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(false, String::new(), String::new());
    let desired = DesiredTable::new("tbl_Account");
    assert!(
        handler
            .on_need_new_table(&mut db, &mut log, "tbl_Account", &desired.columns)
            .unwrap()
    );
    assert!(db.statements.is_empty());
    assert!(
        log.iter()
            .any(|l| l == "\tWitholding table creation: tbl_Account\n")
    );
}

#[test]
fn new_table_with_id_column_creates_and_skips_the_alter() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(true, String::new(), String::new());
    let desired = DesiredTable::new("tbl_Account")
        .with_column("id", info(id_column_type(), ColumnIndexType::Primary));
    assert!(
        handler
            .on_need_new_table(&mut db, &mut log, "tbl_Account", &desired.columns)
            .unwrap()
    );
    assert_eq!(db.statements, vec![
        "CREATE TABLE IF NOT EXISTS tbl_Account (id BIGINT AUTO_INCREMENT, PRIMARY KEY idKey (id)) ENGINE=InnoDB".to_string()
    ]);
    // The id column was erased (already created by the CREATE), so no ALTER
    // ran and no initialisation pass happened.
    assert!(log.iter().any(|l| l == "\tCreating table tbl_Account\n"));
    assert!(log.iter().all(|l| !l.contains("Adding")));
}

#[test]
fn new_table_without_id_column_drops_the_placeholder() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(true, String::new(), String::new());
    let desired = DesiredTable::new("tbl_Account").with_column(
        "name",
        info(col(FieldType::VarString, 64), ColumnIndexType::None),
    );
    handler
        .on_need_new_table(&mut db, &mut log, "tbl_Account", &desired.columns)
        .unwrap();
    assert_eq!(db.statements, vec![
        "CREATE TABLE IF NOT EXISTS tbl_Account (id BIGINT AUTO_INCREMENT, PRIMARY KEY idKey (id)) ENGINE=InnoDB".to_string(),
        "ALTER TABLE tbl_Account ADD COLUMN name VARCHAR(64) NOT NULL, DROP COLUMN id".to_string(),
    ]);
}

#[test]
fn new_table_with_blob_column_backfills_the_default() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(true, String::new(), String::new());
    let blob = ColumnType {
        default_value: "{}".to_string(),
        ..col(FieldType::Blob, 0)
    };
    let desired = DesiredTable::new("tbl_Account")
        .with_column("id", info(id_column_type(), ColumnIndexType::Primary))
        .with_column("data", info(blob, ColumnIndexType::None));
    handler
        .on_need_new_table(&mut db, &mut log, "tbl_Account", &desired.columns)
        .unwrap();
    // ALTER ran, so the initialisation pass backfills the blob default.
    assert!(
        db.statements
            .iter()
            .any(|s| s == "UPDATE tbl_Account SET data='{}'")
    );
}

#[test]
fn existing_table_character_set_conversion_and_empty_charset_noop() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(true, "utf8".to_string(), String::new());
    assert!(
        handler
            .on_existing_table(&mut db, &mut log, "tbl_Account")
            .unwrap()
    );
    assert_eq!(
        db.statements,
        vec!["ALTER TABLE tbl_Account CONVERT TO CHARACTER SET 'utf8'".to_string()]
    );

    let mut none = TableInitialiser::new(true, String::new(), "utf8_bin".to_string());
    assert!(
        none.on_existing_table(&mut db, &mut log, "tbl_Account")
            .unwrap()
    );
    assert_eq!(db.statements.len(), 1); // unchanged
}

#[test]
fn update_table_emits_drop_index_before_the_alter_and_indexes_after() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(true, String::new(), String::new());

    let mut obsolete = BTreeMap::new();
    obsolete.insert(
        "gone".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::Unique),
    );
    let mut new = BTreeMap::new();
    new.insert(
        "fresh".to_string(),
        info(col(FieldType::Long, 0), ColumnIndexType::NonUnique),
    );
    let mut updated = BTreeMap::new();
    updated.insert(
        "num".to_string(),
        info(unsigned(FieldType::Long), ColumnIndexType::None),
    );
    let mut indexed = BTreeMap::new();
    indexed.insert(
        "num".to_string(),
        IndexedColumnInfo {
            column_info: info(unsigned(FieldType::Long), ColumnIndexType::None),
            old_index_type: ColumnIndexType::NonUnique,
        },
    );

    let update = TableUpdate {
        table: "tbl_Account",
        obsolete: &obsolete,
        new: &new,
        updated: &updated,
        indexed: &indexed,
    };
    assert!(
        handler
            .on_need_update_table(&mut db, &mut log, &update)
            .unwrap()
    );

    // The DROP INDEX of an obsolete column runs before the combined ALTER;
    // the indexed bucket then removes the (None, old NonUnique) pair.
    assert_eq!(db.statements, vec![
        "ALTER TABLE tbl_Account DROP INDEX goneIndex".to_string(),
        "ALTER TABLE tbl_Account ADD COLUMN fresh INT NOT NULL, DROP COLUMN gone, MODIFY COLUMN num INT UNSIGNED NOT NULL".to_string(),
        "ALTER TABLE tbl_Account DROP INDEX numIndex".to_string(),
    ]);
    assert!(
        log.iter()
            .any(|l| l == "\tPreparing to delete index for table tbl_Account column num\n")
    );
}

#[test]
fn update_table_index_removal_uses_the_old_index_kind() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(true, String::new(), String::new());
    let mut indexed = BTreeMap::new();
    indexed.insert(
        "num".to_string(),
        IndexedColumnInfo {
            column_info: info(col(FieldType::Long, 0), ColumnIndexType::None),
            old_index_type: ColumnIndexType::ParentId,
        },
    );
    let update = TableUpdate {
        table: "tbl_Account",
        obsolete: &BTreeMap::new(),
        new: &BTreeMap::new(),
        updated: &BTreeMap::new(),
        indexed: &indexed,
    };
    handler
        .on_need_update_table(&mut db, &mut log, &update)
        .unwrap();
    assert_eq!(
        db.statements,
        vec!["ALTER TABLE tbl_Account DROP INDEX parentIDIndex".to_string()]
    );
    assert!(
        log.iter()
            .any(|l| l == "\tPreparing to delete index for table tbl_Account column num\n")
    );
}

#[test]
fn create_index_covers_each_index_kind() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let handler = TableInitialiser::new(true, String::new(), String::new());

    handler
        .create_index(
            &mut db,
            &mut log,
            "tbl_A",
            "x",
            &info(col(FieldType::Long, 0), ColumnIndexType::None),
        )
        .unwrap();
    handler
        .create_index(
            &mut db,
            &mut log,
            "tbl_A",
            "id",
            &info(id_column_type(), ColumnIndexType::Primary),
        )
        .unwrap();
    handler
        .create_index(
            &mut db,
            &mut log,
            "tbl_A",
            "parentID",
            &info(col(FieldType::LongLong, 0), ColumnIndexType::ParentId),
        )
        .unwrap();
    assert_eq!(
        db.statements,
        vec!["CREATE INDEX parentIDIndex ON tbl_A (parentID)".to_string()]
    );

    // Unique success.
    handler
        .create_index(
            &mut db,
            &mut log,
            "tbl_A",
            "name",
            &info(col(FieldType::VarChar, 64), ColumnIndexType::Unique),
        )
        .unwrap();
    assert_eq!(
        db.statements.last().unwrap(),
        "CREATE UNIQUE INDEX nameIndex ON tbl_A (name(64))"
    );
}

#[test]
fn create_index_failure_messages_are_verbatim_and_rethrown() {
    let mut unique_db = DoubleSyncDatabase {
        fail_execute_on: vec!["CREATE".to_string()],
        ..Default::default()
    };
    let mut log = default_log();
    let handler = TableInitialiser::new(true, String::new(), String::new());
    let err = handler
        .create_index(
            &mut unique_db,
            &mut log,
            "tbl_A",
            "name",
            &info(col(FieldType::VarChar, 64), ColumnIndexType::Unique),
        )
        .unwrap_err();
    assert_eq!(
        err,
        "execute failed: CREATE UNIQUE INDEX nameIndex ON tbl_A (name(64))"
    );
    assert!(log.iter().any(|l| l == "TableInitialiser::createIndex: Failed to create unique index on column 'tbl_A.name'. Please ensure that all values in the column are unique before attempting to create a name index.\n"));

    let mut non_unique_db = DoubleSyncDatabase {
        fail_execute_on: vec!["CREATE".to_string()],
        ..Default::default()
    };
    let mut log2 = default_log();
    handler
        .create_index(
            &mut non_unique_db,
            &mut log2,
            "tbl_A",
            "num",
            &info(col(FieldType::Long, 0), ColumnIndexType::NonUnique),
        )
        .unwrap_err();
    assert!(log2.iter().any(|l| l.starts_with(
        "TableInitialiser::createIndex: Failed to create non-unique index on column 'tbl_A.num: execute failed:"
    )));
    assert!(log2.iter().any(|l| l.ends_with(")'.\n")));
}

#[test]
#[should_panic(expected = "createEntityTableIndex: Unknown index type 5")]
fn create_index_on_external_panics_verbatim() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let handler = TableInitialiser::new(true, String::new(), String::new());
    handler
        .create_index(
            &mut db,
            &mut log,
            "tbl_A",
            "x",
            &info(col(FieldType::Long, 0), ColumnIndexType::External),
        )
        .unwrap();
}

#[test]
fn remove_index_swallows_failures_and_skips_non_deletable_kinds() {
    let mut db = DoubleSyncDatabase {
        fail_execute_on: vec!["ALTER TABLE".to_string()],
        ..Default::default()
    };
    let mut log = default_log();
    let handler = TableInitialiser::new(true, String::new(), String::new());
    handler.remove_index(&mut db, &mut log, "tbl_A", "x", ColumnIndexType::NonUnique);
    assert!(
        log.iter()
            .any(|l| l.starts_with("TableInitialiser::removeIndex: "))
    );

    let mut clean = DoubleSyncDatabase::default();
    let mut clean_log = default_log();
    for kind in [
        ColumnIndexType::None,
        ColumnIndexType::Primary,
        ColumnIndexType::External,
    ] {
        handler.remove_index(&mut clean, &mut clean_log, "tbl_A", "x", kind);
    }
    assert!(clean.statements.is_empty());
}

#[test]
fn create_index_statement_caps_non_varchar_index_length_with_warning() {
    let handler = TableInitialiser::new(true, String::new(), String::new());
    let mut log = default_log();

    // A 400-char TEXT column caps at 255.
    let sql = handler.generate_create_index_statement(
        &mut log,
        "tbl_A",
        "bio",
        &info(col(FieldType::Blob, 400), ColumnIndexType::NonUnique),
    );
    assert_eq!(sql, "CREATE INDEX bioIndex ON tbl_A (bio(255))");
    assert_eq!(
        log[0],
        "Column bio on table tbl_A can only be indexed up to the first 255 characters (column is of length 400).\n"
    );

    // VAR_STRING keeps its full length and emits no warning.
    let mut log2 = default_log();
    let varchar_sql = handler.generate_create_index_statement(
        &mut log2,
        "tbl_A",
        "name",
        &info(col(FieldType::VarString, 400), ColumnIndexType::NonUnique),
    );
    assert_eq!(varchar_sql, "CREATE INDEX nameIndex ON tbl_A (name(400))");
    assert!(log2.is_empty());

    // Numerical columns get no length suffix at all.
    let int_sql = handler.generate_create_index_statement(
        &mut log2,
        "tbl_A",
        "num",
        &info(col(FieldType::Long, 0), ColumnIndexType::NonUnique),
    );
    assert_eq!(int_sql, "CREATE INDEX numIndex ON tbl_A (num)");
}

#[test]
fn delete_tables_drops_each_with_the_tab_prefixed_log() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let mut handler = TableInitialiser::new(true, String::new(), String::new());
    let mut names = BTreeSet::new();
    names.insert("tbl_Stale".to_string());
    names.insert("tbl_Older".to_string());
    assert!(
        handler
            .on_need_delete_tables(&mut db, &mut log, &names)
            .unwrap()
    );
    assert_eq!(
        db.statements,
        vec![
            "DROP TABLE tbl_Older".to_string(),
            "DROP TABLE tbl_Stale".to_string()
        ]
    );
    assert!(log.iter().any(|l| l == "\tDeleting table tbl_Older\n"));
    assert!(log.iter().any(|l| l == "\tDeleting table tbl_Stale\n"));
}

// -----------------------------------------------------------------------------
// Section: validator
// -----------------------------------------------------------------------------

#[test]
fn validator_delete_report_has_no_tab_and_fails_the_pass() {
    let mut db = DoubleSyncDatabase {
        entity_tables: vec!["tbl_Stale".to_string()],
        ..Default::default()
    };
    let mut log = default_log();
    let result = validate_entity_tables(&mut db, &mut log, &[]);
    assert_eq!(result, Ok(false));
    assert!(log.iter().any(|l| l == "Need to remove table tbl_Stale\n"));
}

#[test]
fn validator_duplicate_table_surfaces_the_runtime_error() {
    let mut db = DoubleSyncDatabase::default();
    let mut log = default_log();
    let desired = DesiredTable::new("tbl_Account");
    let tables = vec![desired.clone(), desired];
    let err = validate_entity_tables(&mut db, &mut log, &tables).unwrap_err();
    assert_eq!(err, "table tbl_Account requested twice");
}

// -----------------------------------------------------------------------------
// Section: special tables and version ladder
// -----------------------------------------------------------------------------

#[test]
fn special_tables_default_size_statements_are_verbatim() {
    let statements = special_bigworld_table_statements(2048);
    assert_eq!(statements.len(), 12);
    assert_eq!(
        statements[0],
        "CREATE TABLE IF NOT EXISTS bigworldInfo (version INT UNSIGNED NOT NULL, snapshotTime TIMESTAMP NULL, isPasswordHashed BOOL NOT NULL DEFAULT FALSE) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[1],
        "CREATE TABLE IF NOT EXISTS bigworldEntityTypes (typeID INT NOT NULL AUTO_INCREMENT, bigworldID INT, name CHAR(255) CHARACTER SET latin1 NOT NULL UNIQUE,  PRIMARY KEY(typeID), KEY(bigworldID)) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[2],
        "CREATE TABLE IF NOT EXISTS bigworldLogOns (databaseID BIGINT NOT NULL, typeID INT NOT NULL, objectID INT, ip INT UNSIGNED, port SMALLINT UNSIGNED, salt SMALLINT UNSIGNED, shouldAutoLoad BOOL NOT NULL DEFAULT FALSE, PRIMARY KEY(typeID, databaseID)) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[3],
        "CREATE TABLE IF NOT EXISTS bigworldLogOnMapping (logOnName VARBINARY(255) NOT NULL, password VARBINARY(255) NOT NULL, entityType INT, entityID BIGINT, PRIMARY KEY(logOnName)) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[4],
        "CREATE TABLE IF NOT EXISTS bigworldNewID (id INT NOT NULL) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[5],
        "CREATE TABLE IF NOT EXISTS bigworldUsedIDs (id INT NOT NULL) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[6],
        "CREATE TABLE IF NOT EXISTS bigworldGameTime (time INT NOT NULL) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[7],
        "CREATE TABLE IF NOT EXISTS bigworldSpaces (id INT NOT NULL UNIQUE) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[8],
        "CREATE TABLE IF NOT EXISTS bigworldSpaceData (id INT NOT NULL, INDEX (id), spaceEntryID BIGINT NOT NULL, entryKey SMALLINT UNSIGNED NOT NULL, data BLOB NOT NULL ) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[9],
        "ALTER TABLE bigworldSpaceData MODIFY data BLOB"
    );
    assert_eq!(
        statements[10],
        "CREATE TABLE IF NOT EXISTS bigworldSecondaryDatabases (ip INT UNSIGNED NOT NULL, port SMALLINT UNSIGNED NOT NULL, location BLOB NOT NULL, INDEX addr (ip, port)) ENGINE=InnoDB"
    );
    assert_eq!(
        statements[11],
        "CREATE TABLE IF NOT EXISTS bigworldEntityDefsChecksum (checksum CHAR(255) CHARACTER SET latin1) ENGINE=InnoDB"
    );
}

#[test]
fn special_tables_blob_family_tracks_max_space_data_size() {
    let tiny = special_bigworld_table_statements(255);
    assert!(tiny[8].contains("data TINYBLOB NOT NULL )"));
    assert_eq!(
        tiny[9],
        "ALTER TABLE bigworldSpaceData MODIFY data TINYBLOB"
    );
    let medium = special_bigworld_table_statements(65536);
    assert!(medium[8].contains("data MEDIUMBLOB NOT NULL )"));
    let long = special_bigworld_table_statements(1 << 24);
    assert!(long[8].contains("data LONGBLOB NOT NULL )"));
}

#[test]
fn version_ladder_constants_match_the_reference() {
    assert_eq!(DBAPP_VERSION_1_7, 1);
    assert_eq!(DBAPP_VERSION_1_8, 2);
    assert_eq!(DBAPP_VERSION_1_9_SNAPSHOT, 3);
    assert_eq!(DBAPP_VERSION_1_9_NON_NULL, 4);
    assert_eq!(DBAPP_VERSION_SECONDARYDB, 5);
    assert_eq!(DBAPP_VERSION_BINARY_STRINGS, 6);
    assert_eq!(DBAPP_VERSION_LOGON_MAPPING, 7);
    assert_eq!(DBAPP_VERSION_SHOULD_AUTO_LOAD, 8);
    assert_eq!(DBAPP_VERSION_LOGON_MAPPING_2, 9);
    assert_eq!(DBAPP_CURRENT_VERSION, 9);
    assert_eq!(DBAPP_OLDEST_SUPPORTED_VERSION, 1);
}

#[test]
fn sync_config_defaults_match_the_reference() {
    let config = SyncConfig::default();
    assert_eq!(config.character_set, "utf8");
    assert_eq!(config.collation, "utf8_bin");
    assert!(config.is_password_hashed);
    assert_eq!(config.max_space_data_size, 2048);
}

// -----------------------------------------------------------------------------
// Section: the version and password helpers
// -----------------------------------------------------------------------------

#[test]
fn read_or_record_version_inserts_on_a_fresh_database() {
    let mut db = fresh();
    let mut log = default_log();
    assert_eq!(read_or_record_version(&mut db, &mut log), Ok(9));
    assert_eq!(
        db.statements,
        vec!["INSERT INTO bigworldInfo (version) VALUES (9)".to_string()]
    );
    // The double now behaves like a database holding that row.
    assert_eq!(read_or_record_version(&mut db, &mut log), Ok(9));
    assert_eq!(db.statements.len(), 1);

    let mut existing = DoubleSyncDatabase {
        version: Some(5),
        ..Default::default()
    };
    assert_eq!(read_or_record_version(&mut existing, &mut log), Ok(5));
    assert!(existing.statements.is_empty());
}

#[test]
fn read_is_password_hashed_reports_the_row_or_the_assert() {
    let mut db = DoubleSyncDatabase {
        is_password_hashed: Some(false),
        ..Default::default()
    };
    assert_eq!(read_is_password_hashed(&mut db), Ok(false));
    db.is_password_hashed = None;
    assert!(read_is_password_hashed(&mut db).is_err());
}

// -----------------------------------------------------------------------------
// Section: the doSynchronise gate order
// -----------------------------------------------------------------------------

/// The twelve special table names, as the double's `bigworld%` answer.
fn special_table_names() -> Vec<String> {
    [
        "bigworldInfo",
        "bigworldEntityTypes",
        "bigworldLogOns",
        "bigworldLogOnMapping",
        "bigworldNewID",
        "bigworldUsedIDs",
        "bigworldGameTime",
        "bigworldSpaces",
        "bigworldSpaceData",
        "bigworldSecondaryDatabases",
        "bigworldEntityDefsChecksum",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn account_tables() -> Vec<DesiredTable> {
    vec![
        DesiredTable::new("tbl_Account")
            .with_column("id", info(id_column_type(), ColumnIndexType::Primary))
            .with_column(
                "name",
                info(col(FieldType::VarChar, 64), ColumnIndexType::Unique),
            ),
    ]
}

#[test]
fn fresh_database_happy_path_statement_log_is_pinned() {
    let mut db = fresh();
    db.bigworld_tables = special_table_names();
    db.entity_tables = vec!["tbl_Account".to_string()];
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), false);

    assert!(sync.run(&mut db, &account_tables(), &mut log));

    // Gate 1: twelve special tables.
    for statement in special_bigworld_table_statements(2048) {
        assert!(
            db.statements.contains(&statement),
            "missing special statement: {statement}"
        );
    }
    // The info row insert and the password-hash transition (fresh DB).
    assert!(
        db.statements
            .iter()
            .any(|s| s == "INSERT INTO bigworldInfo (version) VALUES (9)")
    );
    assert!(
        db.statements
            .iter()
            .any(|s| s == "UPDATE bigworldInfo SET isPasswordHashed = TRUE")
    );
    assert!(
        db.statements.iter().any(|s| s
            == "UPDATE bigworldLogOnMapping SET password = MD5( CONCAT( password, logOnName ) )")
    );
    assert_eq!(db.commits, 1);
    // The character set ALTER with both placeholders.
    assert!(
        db.statements
            .iter()
            .any(|s| s == "ALTER DATABASE CHARACTER SET ? COLLATE ?")
    );
    // Entity table: CREATE, ALTER with the name column, unique index.
    assert!(db.statements.iter().any(|s| s == "CREATE TABLE IF NOT EXISTS tbl_Account (id BIGINT AUTO_INCREMENT, PRIMARY KEY idKey (id)) ENGINE=InnoDB"));
    assert!(
        db.statements
            .iter()
            .any(|s| s.starts_with("ALTER TABLE tbl_Account ADD COLUMN name VARCHAR(64) NOT NULL"))
    );
    assert!(
        db.statements
            .iter()
            .any(|s| s == "CREATE UNIQUE INDEX nameIndex ON tbl_Account (name(64))")
    );
    // No obsolete tables -> no DROP TABLE.
    assert!(db.statements.iter().all(|s| !s.starts_with("DROP TABLE")));
}

#[test]
fn engine_not_supported_refuses_before_any_statement() {
    let mut db = DoubleSyncDatabase {
        engine: EngineSupportState::NotSupported,
        ..Default::default()
    };
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), false);
    assert!(!sync.run(&mut db, &[], &mut log));
    assert!(db.statements.is_empty());
    assert_eq!(
        log[log.len() - 1],
        "MySqlSynchronise::doSynchronise: InnoDB is not a supported MySQL storage engine. "
    );
}

#[test]
fn engine_supported_logs_the_notice_and_proceeds() {
    let mut db = DoubleSyncDatabase {
        engine: EngineSupportState::Supported,
        bigworld_tables: special_table_names(),
        ..Default::default()
    };
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), false);
    assert!(sync.run(&mut db, &[], &mut log));
    assert!(log.iter().any(|l| l == "MySqlSynchronise::doSynchronise: InnoDB is not the default MySQL storage engine. Add 'default-storage-engine=InnoDB' to /etc/my.cnf and restart the MySQL server\n"));
}

#[test]
fn table_engine_check_failure_stops_after_special_tables() {
    let mut db = DoubleSyncDatabase {
        tables_ok: false,
        ..Default::default()
    };
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), false);
    assert!(!sync.run(&mut db, &[], &mut log));
    assert_eq!(db.statements.len(), 12); // the specials, then the refusal
    assert_eq!(
        log[log.len() - 1],
        "MySqlSynchronise::doSynchronise: One or more tables are not using the InnoDB engine type\n"
    );
}

#[test]
fn unconsolidated_secondary_databases_block_the_sync() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        secondary_dbs: 3,
        ..Default::default()
    };
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), false);
    assert!(!sync.run(&mut db, &[], &mut log));
    assert_eq!(
        log[log.len() - 1],
        "Cannot sync database while there are unconsolidated secondary databases.\nPlease revert any changes to entity definitions and run the data consolidation tool.\nAlternatively, run \"consolidate_dbs --clear\" to clear secondary database information, unconsolidated data will be lost.\n"
    );

    // The table is absent -> the count is never taken, sync proceeds.
    let mut missing = DoubleSyncDatabase {
        bigworld_tables: special_table_names()
            .into_iter()
            .filter(|t| *t != "bigworldSecondaryDatabases")
            .collect(),
        entity_tables: vec!["tbl_Account".to_string()],
        ..Default::default()
    };
    let mut log2 = default_log();
    assert!(Synchronise::new(SyncConfig::default(), false).run(
        &mut missing,
        &account_tables(),
        &mut log2
    ));
}

#[test]
fn newer_database_version_refuses_the_upgrade() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        version: Some(10),
        ..Default::default()
    };
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), false);
    assert!(!sync.run(&mut db, &[], &mut log));
    assert_eq!(
        log[log.len() - 1],
        "Unable to upgrade the database to version 9, a newer DB version (10) is already in use.\n"
    );
    assert!(db.upgrades.is_empty());
}

#[test]
fn older_version_runs_the_upgrade_hook_before_proceeding() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        entity_tables: vec!["tbl_Account".to_string()],
        version: Some(5),
        is_password_hashed: Some(true),
        ..Default::default()
    };
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), false);
    assert!(sync.run(&mut db, &account_tables(), &mut log));
    assert_eq!(db.upgrades, vec![5]);
    // Password flag agrees -> no hash UPDATEs, but the sync continued.
    assert!(
        db.statements
            .iter()
            .all(|s| !s.starts_with("UPDATE bigworldInfo"))
    );
}

#[test]
fn upgrade_hook_failure_propagates_as_an_exception() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        version: Some(5),
        is_password_hashed: Some(true),
        fail_upgrade: Some("upgrade ladder broke".to_string()),
        ..Default::default()
    };
    let mut log = default_log();
    assert!(!Synchronise::new(SyncConfig::default(), false).run(&mut db, &[], &mut log));
    assert_eq!(log[log.len() - 1], "upgrade ladder broke\n");
}

#[test]
fn password_hash_transitions_cover_all_three_shapes() {
    // Removing hashing is refused with the verbatim message.
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        is_password_hashed: Some(true),
        ..Default::default()
    };
    let mut log = default_log();
    let config = SyncConfig {
        is_password_hashed: false,
        ..SyncConfig::default()
    };
    assert!(!Synchronise::new(config, false).run(&mut db, &[], &mut log));
    assert_eq!(
        log[log.len() - 1],
        "MySqlSynchronise::updatePasswordHash: Cannot remove hashing. Manually update bigworldInfo.isPasswordHashed and bigworldLogOnMapping.password fields.\n"
    );

    // Both hashed -> nothing happens.
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        is_password_hashed: Some(true),
        ..Default::default()
    };
    let mut log = default_log();
    assert!(Synchronise::new(SyncConfig::default(), false).run(&mut db, &[], &mut log));
    assert_eq!(db.commits, 0);
    assert!(
        db.statements
            .iter()
            .all(|s| !s.starts_with("UPDATE bigworld"))
    );

    // Fresh DB is unhashed -> hashed: the two UPDATEs and the commit.
    let mut db = fresh();
    db.bigworld_tables = special_table_names();
    let mut log = default_log();
    assert!(Synchronise::new(SyncConfig::default(), false).run(&mut db, &[], &mut log));
    assert_eq!(db.commits, 1);
    assert!(
        db.statements
            .iter()
            .any(|s| s == "UPDATE bigworldInfo SET isPasswordHashed = TRUE")
    );
}

#[test]
fn dry_run_without_the_info_table_reads_nothing_and_skips_entities() {
    let mut db = fresh();
    db.bigworld_tables = vec!["bigworldLogOns".to_string()];
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), true);
    assert!(sync.run(&mut db, &account_tables(), &mut log));
    // Twelve specials + the ALTER DATABASE + the two password UPDATEs —
    // enforcement lives in the connection (the BW-09-05-02 wrapper).
    assert_eq!(db.statements.len(), 15);
    assert_eq!(db.commits, 1);
    assert!(db.statements.iter().all(|s| !s.starts_with("INSERT INTO")));
    assert!(
        db.statements
            .iter()
            .all(|s| !s.starts_with("CREATE TABLE IF NOT EXISTS tbl_"))
    );
    assert_eq!(db.upgrades.len(), 0);
}

#[test]
fn dry_run_with_the_info_table_reads_version_and_reaches_entity_ddl() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        is_password_hashed: Some(true),
        ..Default::default()
    };
    let mut log = default_log();
    let sync = Synchronise::new(SyncConfig::default(), true);
    assert!(sync.run(&mut db, &account_tables(), &mut log));
    // bigworldEntityTypes exists, so the driver runs the entity pass; the
    // dry-run connection is what withholds the DDL statements.
    assert!(
        db.statements
            .iter()
            .any(|s| s.starts_with("CREATE TABLE IF NOT EXISTS tbl_Account"))
    );
    assert_eq!(db.upgrades.len(), 0);
    assert!(db.statements.iter().all(|s| !s.starts_with("INSERT INTO")));
}

#[test]
fn custom_character_set_and_collation_are_reported_and_used() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        ..Default::default()
    };
    let config = SyncConfig {
        character_set: "utf8mb4".to_string(),
        collation: "utf8mb4_bin".to_string(),
        ..SyncConfig::default()
    };
    let mut log = default_log();
    assert!(Synchronise::new(config, false).run(&mut db, &[], &mut log));
    assert!(log.iter().any(|l| l == "Character set is 'utf8mb4'\n"));
    assert!(log.iter().any(|l| l == "Collation is 'utf8mb4_bin'\n"));
    assert!(
        db.statements
            .iter()
            .any(|s| s == "ALTER DATABASE CHARACTER SET ? COLLATE ?")
    );
}

#[test]
fn empty_character_set_leaves_the_setting_alone() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        ..Default::default()
    };
    let config = SyncConfig {
        character_set: String::new(),
        ..SyncConfig::default()
    };
    let mut log = default_log();
    assert!(Synchronise::new(config, false).run(&mut db, &[], &mut log));
    assert!(
        log.iter()
            .any(|l| l == "Empty character set. Leaving at existing setting\n")
    );
    assert!(
        db.statements
            .iter()
            .all(|s| !s.starts_with("ALTER DATABASE"))
    );
}

#[test]
fn collation_only_setting_uses_the_single_placeholder_statement() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        ..Default::default()
    };
    let config = SyncConfig {
        collation: String::new(),
        ..SyncConfig::default()
    };
    let mut log = default_log();
    assert!(Synchronise::new(config, false).run(&mut db, &[], &mut log));
    assert!(
        db.statements
            .iter()
            .any(|s| s == "ALTER DATABASE CHARACTER SET ?")
    );
    assert!(log.iter().any(|l| l == "Collation is ''\n"));
}

#[test]
fn charset_execute_failure_reports_the_verbatim_pair() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        fail_execute_on: vec!["ALTER DATABASE".to_string()],
        ..Default::default()
    };
    let mut log = default_log();
    assert!(!Synchronise::new(SyncConfig::default(), false).run(&mut db, &[], &mut log));
    assert_eq!(
        log[log.len() - 1],
        "Invalid characterSet or collation setting in 'bw.xml'\nexecute failed: ALTER DATABASE CHARACTER SET ? COLLATE ?\n"
    );
}

#[test]
fn execute_failure_during_special_tables_surfaces_through_run() {
    let mut db = DoubleSyncDatabase {
        fail_execute_on: vec!["bigworldLogOns".to_string()],
        ..Default::default()
    };
    let mut log = default_log();
    assert!(!Synchronise::new(SyncConfig::default(), false).run(&mut db, &[], &mut log));
    assert_eq!(
        log[log.len() - 1],
        "execute failed: CREATE TABLE IF NOT EXISTS bigworldLogOns (databaseID BIGINT NOT NULL, typeID INT NOT NULL, objectID INT, ip INT UNSIGNED, port SMALLINT UNSIGNED, salt SMALLINT UNSIGNED, shouldAutoLoad BOOL NOT NULL DEFAULT FALSE, PRIMARY KEY(typeID, databaseID)) ENGINE=InnoDB\n"
    );
}

#[test]
fn duplicate_entity_table_in_the_sync_path_reports_and_fails() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        entity_tables: vec!["tbl_Account".to_string()],
        ..Default::default()
    };
    let duplicate = vec![account_tables()[0].clone(), account_tables()[0].clone()];
    let mut log = default_log();
    assert!(!Synchronise::new(SyncConfig::default(), false).run(&mut db, &duplicate, &mut log));
    assert!(log.iter().any(|l| l
        == "MySqlSynchronise::synchroniseEntityDefinitions: table tbl_Account requested twice"));
}

#[test]
fn obsolete_entity_tables_are_dropped_by_the_sync_path() {
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        entity_tables: vec!["tbl_Account".to_string(), "tbl_Old".to_string()],
        ..Default::default()
    };
    let mut log = default_log();
    assert!(Synchronise::new(SyncConfig::default(), false).run(
        &mut db,
        &account_tables(),
        &mut log
    ));
    assert!(db.statements.iter().any(|s| s == "DROP TABLE tbl_Old"));
    assert!(log.iter().any(|l| l == "\tDeleting table tbl_Old\n"));
}

#[test]
fn sync_path_delete_failure_reports_through_the_same_catch() {
    // The obsolete-table sweep runs inside the same try block as the visits,
    // so a failing DROP unwinds into the synchroniseEntityDefinitions catch.
    let mut db = DoubleSyncDatabase {
        bigworld_tables: special_table_names(),
        entity_tables: vec!["tbl_Account".to_string(), "tbl_Old".to_string()],
        fail_execute_on: vec!["DROP TABLE".to_string()],
        ..Default::default()
    };
    let mut log = default_log();
    assert!(!Synchronise::new(SyncConfig::default(), false).run(
        &mut db,
        &account_tables(),
        &mut log
    ));
    assert!(log.iter().any(|l| l
        == "MySqlSynchronise::synchroniseEntityDefinitions: execute failed: DROP TABLE tbl_Old"));
    assert!(!db.statements.iter().any(|s| s == "DROP TABLE tbl_Account"));
}

// -----------------------------------------------------------------------------
// Section: the main() face
// -----------------------------------------------------------------------------

#[test]
fn connection_overrides_follow_the_option_presence_rule() {
    let config = ConnectionInfo {
        host: "dbhost".to_string(),
        port: 3306,
        database: "bigworld".to_string(),
        username: "bw".to_string(),
        password: "secret".to_string(),
        secure_auth: true,
    };
    // No options -> config untouched.
    let plain = CommandLine::default();
    assert_eq!(resolve_connection(&plain, &config), config);

    // Every option overrides its config field.
    let all = CommandLine {
        verbose: true,
        run_from_dbapp: true,
        dry_run: true,
        host: Some("other".to_string()),
        port: Some(3307),
        database: Some("bw2".to_string()),
        username: Some("root".to_string()),
        password: Some("pw".to_string()),
        secure_auth: Some(false),
    };
    let resolved = resolve_connection(&all, &config);
    assert_eq!(resolved.host, "other");
    assert_eq!(resolved.port, 3307);
    assert_eq!(resolved.database, "bw2");
    assert_eq!(resolved.username, "root");
    assert_eq!(resolved.password, "pw");
    assert!(!resolved.secure_auth);

    // secure-auth absent keeps the config value.
    let partial = CommandLine {
        secure_auth: None,
        ..CommandLine::default()
    };
    assert!(resolve_connection(&partial, &config).secure_auth);
    let present = CommandLine {
        secure_auth: Some(true),
        ..CommandLine::default()
    };
    assert!(resolve_connection(&present, &config).secure_auth);
}

#[test]
fn main_face_messages_and_exit_codes_are_verbatim() {
    assert_eq!(INITIALISATION_FAILED_MESSAGE, "Initialisation failed\n");
    assert_eq!(SYNC_TO_DATABASE_FAILED_MESSAGE, "Sync to database failed\n");
    assert_eq!(
        SYNC_TO_DATABASE_SUCCESSFUL_MESSAGE,
        "Sync to database successful\n"
    );
    assert_eq!(exit_code(true), 0);
    assert_eq!(exit_code(false), 1);
}

#[test]
fn failure_messages_render_the_reference_error_texts() {
    assert_eq!(
        SyncFailure::Exception("boom".to_string()).error_message(),
        "boom\n"
    );
    assert_eq!(
        SyncFailure::Charset("bad collation".to_string()).error_message(),
        "Invalid characterSet or collation setting in 'bw.xml'\nbad collation\n"
    );
    assert_eq!(
        SyncFailure::NewerVersion {
            current: 9,
            found: 10
        }
        .error_message(),
        "Unable to upgrade the database to version 9, a newer DB version (10) is already in use.\n"
    );
    assert_eq!(
        SyncFailure::EngineNotSupported.error_message(),
        "MySqlSynchronise::doSynchronise: InnoDB is not a supported MySQL storage engine. "
    );
    assert_eq!(
        SyncFailure::TableEngines.error_message(),
        "MySqlSynchronise::doSynchronise: One or more tables are not using the InnoDB engine type\n"
    );
}

#[test]
fn table_columns_maps_fields_through_the_index_rows() {
    let mut log = default_log();
    let described = DescribedTable {
        fields: vec![
            field("id", FieldType::LongLong, PRI_KEY_FLAG, 8, None),
            field("parentID", FieldType::LongLong, MULTIPLE_KEY_FLAG, 8, None),
            field("gone", FieldType::Long, 0, 4, None),
        ],
        indices: vec![IndexRow {
            non_unique: "1".to_string(),
            key_name: "parentIDIndex".to_string(),
            column_name: "parentID".to_string(),
        }],
    };
    let columns = table_columns(&described, &mut log);
    assert_eq!(columns.len(), 3);
    assert_eq!(columns["id"].index_type, ColumnIndexType::Primary);
    assert_eq!(columns["parentID"].index_type, ColumnIndexType::ParentId);
    assert_eq!(columns["gone"].index_type, ColumnIndexType::None);
    assert!(log.is_empty());

    // Missing table -> empty map (the "no existing table" signal).
    let missing = table_columns(&DescribedTable::default(), &mut log);
    assert!(missing.is_empty());
}
