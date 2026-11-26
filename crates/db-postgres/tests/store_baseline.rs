//! OPT-09-01-01 the same-dataset baseline of MySQL and PostgreSQL.
//!
//! The reference ships MySQL; Boundless runs PostgreSQL. Before any
//! pooling, batching or consolidation work (OPT-09-01-02..04) both
//! backends must answer the same dataset with the same rows, so this
//! harness drives one deterministic corpus through two storage faces:
//!
//! * **MySQL** — the reference's backend over its own `tbl_<TypeName>`
//!   layout: the `id BIGINT AUTO_INCREMENT` key column and one `sm_*`
//!   column per persistent property, the `REPLACE INTO` upsert of
//!   `putEntity` (`mysql_database.cpp:451-473`).
//! * **PostgreSQL** — the Boundless backend ported to
//!   `INSERT ... ON CONFLICT ("id") DO UPDATE` (`schema`, `store`).
//!
//! Both legs store the same `ROWS` records with the same DBIDs and
//! property values, generated from a fixed seed. They assert, per row,
//! that the record read back equals the one written, and record the
//! per-operation put/get latencies (p50/p90/p99/max/min) per backend
//! plus the row count. The record is written to `STORE_BASELINE_OUT`
//! in the network/load TSV format (stdout when no path is set).
//!
//! Structural assertions run on every execution. Wall-clock numbers are
//! **recorded, never asserted tight** — machine-dependent, as with the
//! entitydef and network baselines.
//!
//! The MySQL connection comes from `BOUNDLESS_MYSQL_URL` or the CI
//! storage service's `CI_STORE_MYSQL_*` settings or a local unix socket;
//! the PostgreSQL leg reads `BOUNDLESS_PG_URL`, the CI service's
//! `CI_STORE_PG_*` settings or a local unix socket. When a server is not
//! reachable the leg skips with a notice, the same gate the other PG
//! cases use. Every leg creates and drops its own private database.

use std::io::Write as _;
use std::time::Instant;

use db::{EntityKey, EntityRecord, EntityStorage};
use db_postgres::{ColumnKind, EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, Params, Value};

/// Rows per dataset. Enough to spread a latency distribution over the
/// per-op samples without a multi-second run.
const ROWS: usize = 512;
/// The entity type both legs store. One column per persistent property,
/// the shapes the storage contract carries.
const TYPE_NAME: &str = "SchemaBaseline";
/// The id column every entity table carries.
const ID_COLUMN: &str = "id";

/// The p50/p90/p99/max/min summary of a sample set, in the recorded unit.
fn summarize(samples: &[f64]) -> [f64; 5] {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pick = |p: f64| {
        let idx = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
        sorted[idx]
    };
    [
        pick(0.50),
        pick(0.90),
        pick(0.99),
        sorted[sorted.len() - 1],
        sorted[0],
    ]
}

/// Collected `metric<TAB>value` rows. Printed to stdout on flush and
/// written to `$STORE_BASELINE_OUT` when set (the network/load fixture
/// format).
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: &str, value: impl std::fmt::Display) {
        self.0.push((metric.to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("STORE_BASELINE_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write STORE_BASELINE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// Records one leg's summary under `prefix_{p50,p90,p99,max,min}`.
fn put_summary(record: &mut Record, prefix: &str, summary: [f64; 5]) {
    let names = ["p50", "p90", "p99", "max", "min"];
    for (name, value) in names.iter().zip(summary) {
        record.put(&format!("{prefix}_{name}"), format!("{value:.1}"));
    }
}

// ---------------------------------------------------------------------------
// The dataset
// ---------------------------------------------------------------------------

/// The deterministic record corpus. Same seed, same rows on every run;
/// the MySQL and PostgreSQL legs store exactly these values.
fn dataset() -> Vec<EntityRecord> {
    (0..ROWS)
        .map(|index| {
            let seed = index as u64;
            EntityRecord {
                type_id: 1,
                db_id: index as i64 + 1,
                values: vec![
                    PropertyValue::Int((seed as i64 % 251) - 125),
                    PropertyValue::Float(f64::from_bits(0x3FF0_0000_0000_0000 + seed)),
                    PropertyValue::String(format!(
                        "avatar-{seed} \u{1F984} \u{4F60}\u{597D} \"q\"\n\t\u{7}"
                    )),
                    PropertyValue::Blob(vec![
                        0x00,
                        0xff,
                        0xfe,
                        0x0a,
                        0x0d,
                        0x1a,
                        0x80,
                        (seed & 0xff) as u8,
                        0x00,
                    ]),
                    PropertyValue::Sequence(vec![
                        PropertyValue::Int((seed % 97) as i64),
                        PropertyValue::String("mixed".into()),
                        PropertyValue::FixedDict(Some(vec![
                            ("a".to_string(), PropertyValue::Float(1.5)),
                            ("b".to_string(), PropertyValue::Uint(seed)),
                        ])),
                    ]),
                ],
            }
        })
        .collect()
}

/// The schema for the dataset, built once so both legs share the exact
/// column kinds.
fn dataset_schema() -> EntitySchema {
    let names = ["level", "ratio", "name", "avatar", "traits"];
    let values = dataset()[0].values.clone();
    EntitySchema::for_record(TYPE_NAME, &names, &values).expect("the dataset schema")
}

// ---------------------------------------------------------------------------
// The MySQL face
// ---------------------------------------------------------------------------

/// The MySQL connection: a service container, `BOUNDLESS_MYSQL_URL`, or
/// the local server's socket. `None` means no server is reachable.
fn mysql_opts() -> Option<Opts> {
    if let Ok(url) = std::env::var("BOUNDLESS_MYSQL_URL") {
        return Opts::from_url(&url).ok();
    }
    if let (Ok(host), Ok(port)) = (
        std::env::var("CI_STORE_MYSQL_HOST"),
        std::env::var("CI_STORE_MYSQL_PORT"),
    ) {
        let mut builder = OptsBuilder::new()
            .ip_or_hostname(Some(host))
            .tcp_port(port.parse().ok()?)
            .user(Some(
                std::env::var("CI_STORE_MYSQL_USER").unwrap_or_else(|_| "root".to_string()),
            ));
        if let Ok(password) = std::env::var("CI_STORE_MYSQL_PASSWORD") {
            builder = builder.pass(Some(password));
        }
        return Some(builder.into());
    }
    Some(
        OptsBuilder::new()
            .socket(Some("/var/run/mysqld/mysqld.sock".to_string()))
            .user(Some("root"))
            .into(),
    )
}

/// Creates the MySQL table the reference lays out: the `id` key column
/// and one `sm_*` column per property.
fn create_mysql_table(conn: &mut Conn, schema: &EntitySchema) {
    let mut columns = vec![format!("{ID_COLUMN} BIGINT NOT NULL AUTO_INCREMENT")];
    for (prop, kind) in &schema.columns {
        let name = db_postgres::column_name(prop);
        let ty = match kind {
            ColumnKind::Int | ColumnKind::Uint => "BIGINT NOT NULL",
            ColumnKind::Float => "DOUBLE NOT NULL",
            ColumnKind::String => "TEXT NOT NULL",
            ColumnKind::Blob => "LONGBLOB NOT NULL",
            ColumnKind::Vector => "MEDIUMBLOB NOT NULL",
            ColumnKind::Nested => "LONGTEXT NOT NULL",
        };
        columns.push(format!("`{name}` {ty}"));
    }
    columns.push("PRIMARY KEY (id)".to_string());
    conn.query_drop(format!(
        "CREATE TABLE IF NOT EXISTS `{}` ({}) ENGINE=InnoDB",
        db_postgres::entity_table_name(TYPE_NAME),
        columns.join(", ")
    ))
    .unwrap();
}

/// The SQL value for one property. Nested values travel as the tagged
/// JSON text of the PostgreSQL codec, so the two legs carry the same
/// representation, and the same codec decodes it on the way back.
fn mysql_value(value: &PropertyValue) -> Value {
    match value {
        PropertyValue::Int(n) => Value::from(*n),
        PropertyValue::Uint(n) => Value::from(*n),
        PropertyValue::Float(x) => Value::from(*x),
        PropertyValue::String(s) => Value::from(s.clone()),
        PropertyValue::Blob(bytes) => Value::from(bytes.clone()),
        PropertyValue::Vector(_) => Value::from(db_postgres::json::encode_to_string(value)),
        PropertyValue::Sequence(_)
        | PropertyValue::FixedDict(_)
        | PropertyValue::MailBox { .. } => Value::from(db_postgres::json::encode_to_string(value)),
    }
}

/// Writes one row with the reference's `REPLACE INTO` upsert.
fn put_mysql(conn: &mut Conn, schema: &EntitySchema, row: &EntityRecord) {
    let mut columns = vec![ID_COLUMN.to_string()];
    let mut placeholders = vec!["?".to_string()];
    let mut params = vec![Value::from(row.db_id)];
    for ((prop, _), value) in schema.columns.iter().zip(&row.values) {
        columns.push(db_postgres::column_name(prop));
        placeholders.push("?".to_string());
        params.push(mysql_value(value));
    }
    conn.exec_drop(
        format!(
            "REPLACE INTO `{}` ({}) VALUES ({})",
            db_postgres::entity_table_name(TYPE_NAME),
            columns.join(", "),
            placeholders.join(", ")
        ),
        Params::Positional(params),
    )
    .unwrap();
}

/// Reads one row back and decodes it into the record's values.
fn get_mysql(conn: &mut Conn, schema: &EntitySchema, row: &EntityRecord) -> EntityRecord {
    let mut columns = vec![ID_COLUMN.to_string()];
    for (prop, _) in &schema.columns {
        columns.push(format!("`{}`", db_postgres::column_name(prop)));
    }
    let sql = format!(
        "SELECT {} FROM `{}` WHERE {ID_COLUMN} = ?",
        columns.join(", "),
        db_postgres::entity_table_name(TYPE_NAME)
    );
    let stmt = conn.prep(sql).unwrap();
    let mut result = conn
        .exec_iter(&stmt, Params::Positional(vec![Value::from(row.db_id)]))
        .unwrap();
    let sql_row = result
        .next()
        .expect("the row exists")
        .expect("the row reads");
    let mut values = Vec::with_capacity(schema.columns.len());
    for (index, (_, kind)) in schema.columns.iter().enumerate() {
        let value = match kind {
            ColumnKind::Int => PropertyValue::Int(sql_row.get::<i64, _>(index + 1).unwrap()),
            ColumnKind::Uint => {
                PropertyValue::Uint(sql_row.get::<i64, _>(index + 1).unwrap() as u64)
            }
            ColumnKind::Float => PropertyValue::Float(sql_row.get::<f64, _>(index + 1).unwrap()),
            ColumnKind::String => {
                PropertyValue::String(sql_row.get::<String, _>(index + 1).unwrap())
            }
            ColumnKind::Blob => PropertyValue::Blob(sql_row.get::<Vec<u8>, _>(index + 1).unwrap()),
            ColumnKind::Vector | ColumnKind::Nested => {
                let text = sql_row.get::<String, _>(index + 1).unwrap();
                db_postgres::json::decode_from_str(&text).unwrap()
            }
        };
        values.push(value);
    }
    EntityRecord {
        type_id: row.type_id,
        db_id: row.db_id,
        values,
    }
}

/// The MySQL leg: one private database, the reference table layout, the
/// whole dataset stored and read back, the per-op latencies recorded.
fn mysql_leg(record: &mut Record) {
    let Some(opts) = mysql_opts() else {
        eprintln!("skipping: invalid BOUNDLESS_MYSQL_URL");
        return;
    };
    let Some(mut conn) = Conn::new(opts).ok() else {
        eprintln!(
            "skipping: no reachable mysql server (BOUNDLESS_MYSQL_URL / CI_STORE_MYSQL_* / local socket)"
        );
        return;
    };
    let database = format!("boundless_bench_mysql_{}", std::process::id());
    conn.query_drop(format!("DROP DATABASE IF EXISTS `{database}`"))
        .unwrap();
    conn.query_drop(format!("CREATE DATABASE `{database}`"))
        .unwrap();
    conn.query_drop(format!("USE `{database}`")).unwrap();

    let schema = dataset_schema();
    create_mysql_table(&mut conn, &schema);
    let rows = dataset();

    let mut put_latencies = Vec::with_capacity(rows.len());
    for row in &rows {
        let started = Instant::now();
        put_mysql(&mut conn, &schema, row);
        put_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let mut get_latencies = Vec::with_capacity(rows.len());
    for row in &rows {
        let started = Instant::now();
        let read = get_mysql(&mut conn, &schema, row);
        get_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);
        assert_eq!(read, *row, "mysql: the read equals the written row");
    }

    record.put("mysql_rows", rows.len());
    put_summary(record, "mysql_put_us", summarize(&put_latencies));
    put_summary(record, "mysql_get_us", summarize(&get_latencies));
}

// ---------------------------------------------------------------------------
// The PostgreSQL leg
// ---------------------------------------------------------------------------

fn pg_admin_conninfo() -> String {
    if let Ok(url) = std::env::var("BOUNDLESS_PG_URL") {
        return url;
    }
    if let (Ok(host), Ok(port)) = (
        std::env::var("CI_STORE_PG_HOST"),
        std::env::var("CI_STORE_PG_PORT"),
    ) {
        let password = std::env::var("CI_STORE_PG_PASSWORD").unwrap_or_default();
        return format!(
            "host={host} port={port} dbname=postgres user=boundless password={password}"
        );
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "postgres".to_string());
    format!("host=/var/run/postgresql dbname=postgres user={user}")
}

/// The PostgreSQL leg: the same dataset through the Boundless backend,
/// the same assertions, the same record.
fn postgres_leg(record: &mut Record) {
    let admin_conninfo = pg_admin_conninfo();
    let Ok(mut admin) = PostgresStorage::connect(&admin_conninfo) else {
        eprintln!("skipping: no reachable postgres server (BOUNDLESS_PG_URL / local socket)");
        return;
    };
    let database = format!("boundless_bench_pg_{}", std::process::id());
    let create = format!("CREATE DATABASE \"{}\"", database.replace('"', "\"\""));
    if let Err(err) = admin.client_mut().execute(create.as_str(), &[]) {
        eprintln!("skipping: cannot create the test database ({err})");
        return;
    }
    let conninfo = format!("{admin_conninfo} dbname={database}");
    let mut storage = PostgresStorage::connect(&conninfo).unwrap();

    let schema = dataset_schema();
    storage.create_entity_table(&schema).unwrap();
    storage.register_entity_type(1, &schema);
    let rows = dataset();
    let type_id = rows[0].type_id;

    let mut put_latencies = Vec::with_capacity(rows.len());
    for row in &rows {
        let key = EntityKey {
            type_id,
            db_id: row.db_id,
        };
        let started = Instant::now();
        EntityStorage::put_entity(&mut storage, &key, &row.values, true).unwrap();
        put_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let mut get_latencies = Vec::with_capacity(rows.len());
    for row in &rows {
        let key = EntityKey {
            type_id,
            db_id: row.db_id,
        };
        let started = Instant::now();
        let read = EntityStorage::get_entity(&storage, &key)
            .unwrap()
            .expect("the row exists");
        get_latencies.push(started.elapsed().as_secs_f64() * 1_000_000.0);
        assert_eq!(read, *row, "postgres: the read equals the written row");
    }

    record.put("postgres_rows", rows.len());
    put_summary(record, "postgres_put_us", summarize(&put_latencies));
    put_summary(record, "postgres_get_us", summarize(&get_latencies));

    let drop = format!(
        "DROP DATABASE \"{}\" WITH (FORCE)",
        database.replace('"', "\"\"")
    );
    let _ = admin.client_mut().execute(drop.as_str(), &[]);
}

/// Both legs run in fixed order over the same dataset.
#[test]
fn the_same_dataset_runs_on_mysql_and_postgres() {
    let mut record = Record::default();
    mysql_leg(&mut record);
    postgres_leg(&mut record);
    record.flush();
}

#[cfg(test)]
mod fixtures {
    use super::*;

    /// The corpus is fixed: the same dataset on every run, so the
    /// baselines stay comparable across machines and across the OPT-09
    /// optimizations (01-02..04).
    #[test]
    fn the_dataset_is_deterministic() {
        assert_eq!(dataset(), dataset(), "the dataset is identical run to run");
        assert_eq!(ROWS, dataset().len());
    }

    /// The schema pins one column per property, in the shared visit
    /// order, with the kinds the values carry.
    #[test]
    fn the_schema_matches_the_dataset() {
        let schema = dataset_schema();
        assert_eq!(schema.type_name, TYPE_NAME);
        let expected = [
            ("level", ColumnKind::Int),
            ("ratio", ColumnKind::Float),
            ("name", ColumnKind::String),
            ("avatar", ColumnKind::Blob),
            ("traits", ColumnKind::Nested),
        ];
        let actual: Vec<(&str, ColumnKind)> = schema
            .columns
            .iter()
            .map(|(p, k)| (p.as_str(), *k))
            .collect();
        assert_eq!(actual, expected, "the columns follow the dataset order");
    }

    /// The MySQL table the reference lays out: the id key column and one
    /// `sm_*` column per property, the MySQL types the backend maps to.
    #[test]
    fn the_mysql_table_ddl_matches_the_reference_layout() {
        let schema = dataset_schema();
        let Some(mut conn) = mysql_opts().and_then(|opts| Conn::new(opts).ok()) else {
            eprintln!("skipping: no reachable mysql server for the DDL shape");
            return;
        };
        let database = format!("boundless_bench_ddl_{}", std::process::id());
        conn.query_drop(format!("DROP DATABASE IF EXISTS `{database}`"))
            .unwrap();
        conn.query_drop(format!("CREATE DATABASE `{database}`"))
            .unwrap();
        conn.query_drop(format!("USE `{database}`")).unwrap();
        create_mysql_table(&mut conn, &schema);
        let create: Vec<(String, String)> = conn
            .query_map(
                format!(
                    "SHOW CREATE TABLE `{}`",
                    db_postgres::entity_table_name(TYPE_NAME)
                ),
                |(name, ddl)| (name, ddl),
            )
            .unwrap();
        let (_, create) = create.into_iter().next().expect("the table was created");
        // The key column, the auto increment and the primary key.
        assert!(
            create.contains("`id` bigint(20) NOT NULL AUTO_INCREMENT"),
            "the key column"
        );
        assert!(create.contains("PRIMARY KEY (`id`)"), "the primary key");
        // One column per property, with the MySQL type each kind maps to.
        for (prop, kind) in &schema.columns {
            let column = db_postgres::column_name(prop);
            // MariaDB normalises BIGINT to bigint(20) in SHOW CREATE.
            let ty = match kind {
                ColumnKind::Int | ColumnKind::Uint => "bigint(20)",
                ColumnKind::Float => "double",
                ColumnKind::String => "text",
                ColumnKind::Blob => "longblob",
                ColumnKind::Vector => "mediumblob",
                ColumnKind::Nested => "longtext",
            };
            assert!(
                create.contains(&format!("`{column}` {ty} NOT NULL")),
                "column {column} carries its MySQL type"
            );
        }
        conn.query_drop(format!("DROP DATABASE `{database}`"))
            .unwrap();
    }

    /// The nested codec round trips through the JSON text the two legs
    /// carry, so the tagged representation survives both backends.
    #[test]
    fn the_nested_column_round_trips_through_the_json_codec() {
        for value in &dataset()[0].values {
            let text = db_postgres::json::encode_to_string(value);
            let read = db_postgres::json::decode_from_str(&text).unwrap();
            assert_eq!(&read, value, "the nested value survives the codec");
        }
    }

    /// The MySQL value binding carries each shape into its column: the
    /// scalar shapes as native values, the nested shapes as the tagged
    /// JSON text.
    #[test]
    fn the_mysql_values_match_their_kinds() {
        let schema = dataset_schema();
        for row in dataset() {
            for ((_, kind), value) in schema.columns.iter().zip(&row.values) {
                let mysql = mysql_value(value);
                match kind {
                    ColumnKind::Int => assert!(matches!(mysql, Value::Int(_))),
                    ColumnKind::Uint => assert!(matches!(mysql, Value::UInt(_))),
                    ColumnKind::Float => assert!(matches!(mysql, Value::Double(_))),
                    ColumnKind::String => assert!(matches!(mysql, Value::Bytes(_))),
                    ColumnKind::Blob => assert!(matches!(mysql, Value::Bytes(_))),
                    ColumnKind::Vector | ColumnKind::Nested => {
                        assert!(matches!(mysql, Value::Bytes(_)))
                    }
                }
            }
        }
    }
}
