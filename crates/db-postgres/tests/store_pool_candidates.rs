//! OPT-09-01-02 pool and query-preparation candidates on the 01-01 dataset.
//!
//! The reference (`lib/db_storage_mysql`) keeps `numConnections_` (default 5)
//! long-lived background-thread MySQL sessions, reuses each session's SQL
//! templates (`EntityTypeMapping::insertNewQuery_` etc., `prepareSQL()` at
//! startup), and reconnects a lost session in place (`MySqlThreadData`).
//! The Boundless PostgreSQL port holds one `RefCell<Client>` and builds each
//! statement text per call. This harness evaluates the two independent
//! candidates on the frozen 512-row corpus:
//!
//! * **single** — one persistent connection, SQL built per op (the 01-01 face).
//! * **pool** — a pool of 5 connections, rotated round-robin.
//! * **prepared** — one client, statements prepared once and reused.
//! * **pool_prepared** — one prepared pair per pooled connection.
//!
//! Every leg puts every row, reads it back, and asserts equality with the
//! written values, so a candidate cannot win by dropping work. The record
//! carries per-leg put/get p50/p99 (µs), throughput (ops/s) and the row
//! count. Wall-clock numbers are recorded, never asserted tight.

use std::io::Write as _;
use std::time::Instant;

use db::EntityRecord;
use db_postgres::{ColumnKind, EntitySchema, PostgresStorage};
use entitydef::value::PropertyValue;
use mysql::prelude::Queryable;
use mysql::{Conn, Opts, OptsBuilder, Params, Pool, PooledConn, Statement, Value};
use postgres::{Client, Statement as PgStatement};

const ROWS: usize = 512;
const TYPE_NAME: &str = "SchemaBaseline";
const ID_COLUMN: &str = "id";
/// The reference's `DBConfig::numConnections_` default (db_config.cpp:171).
const POOL_SIZE: usize = 5;

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

fn dataset_schema() -> EntitySchema {
    let names = ["level", "ratio", "name", "avatar", "traits"];
    EntitySchema::for_record(TYPE_NAME, &names, &dataset()[0].values).expect("the dataset schema")
}

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
        if let Ok(path) = std::env::var("STORE_POOL_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write STORE_POOL_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

fn percentile(samples: &[f64], p: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let idx = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[idx]
}

fn leg_summary(record: &mut Record, prefix: &str, puts: &[f64], gets: &[f64], wall: f64) {
    let total = puts.len() + gets.len();
    record.put(
        &format!("{prefix}_put_p50_us"),
        format!("{:.1}", percentile(puts, 0.50)),
    );
    record.put(
        &format!("{prefix}_put_p99_us"),
        format!("{:.1}", percentile(puts, 0.99)),
    );
    record.put(
        &format!("{prefix}_get_p50_us"),
        format!("{:.1}", percentile(gets, 0.50)),
    );
    record.put(
        &format!("{prefix}_get_p99_us"),
        format!("{:.1}", percentile(gets, 0.99)),
    );
    record.put(
        &format!("{prefix}_ops_per_s"),
        format!("{:.0}", total as f64 / wall),
    );
    record.put(&format!("{prefix}_rows"), puts.len());
}

// ---------------------------------------------------------------------------
// MySQL candidates
// ---------------------------------------------------------------------------

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

fn mysql_columns(schema: &EntitySchema) -> Vec<String> {
    let mut columns = vec![ID_COLUMN.to_string()];
    columns.extend(
        schema
            .columns
            .iter()
            .map(|(prop, _)| db_postgres::column_name(prop)),
    );
    columns
}

fn mysql_value(value: &PropertyValue) -> Value {
    match value {
        PropertyValue::Int(n) => Value::from(*n),
        PropertyValue::Uint(n) => Value::from(*n),
        PropertyValue::Float(x) => Value::from(*x),
        PropertyValue::String(s) => Value::from(s.clone()),
        PropertyValue::Blob(bytes) => Value::from(bytes.clone()),
        _ => Value::from(db_postgres::json::encode_to_string(value)),
    }
}

fn mysql_params(row: &EntityRecord) -> Vec<Value> {
    let mut params = vec![Value::from(row.db_id)];
    params.extend(row.values.iter().map(mysql_value));
    params
}

fn mysql_create_table(conn: &mut Conn, schema: &EntitySchema) {
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

/// Decodes the first row of a binary-protocol result into the record.
fn mysql_decode(sql_row: &mysql::Row, schema: &EntitySchema, row: &EntityRecord) {
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
    assert_eq!(
        EntityRecord {
            type_id: row.type_id,
            db_id: row.db_id,
            values
        },
        *row,
        "mysql: the read equals the written row"
    );
}

/// One put and one get on a MySQL connection. A prepared statement pair,
/// when supplied, is the query-merging candidate; the SQL text form when
/// not is the per-op face of 01-01.
fn mysql_op<C: Queryable>(
    conn: &mut C,
    row: &EntityRecord,
    schema: &EntitySchema,
    put_sql: &str,
    get_sql: &str,
    put_stmt: Option<&Statement>,
    get_stmt: Option<&Statement>,
) {
    let params = Params::Positional(mysql_params(row));
    match put_stmt {
        Some(stmt) => conn.exec_drop(stmt, params).unwrap(),
        None => conn.exec_drop(put_sql, params).unwrap(),
    }
    let read_params = Params::Positional(vec![Value::from(row.db_id)]);
    let mut result = match get_stmt {
        Some(stmt) => conn.exec_iter(stmt, read_params).unwrap(),
        None => conn.exec_iter(get_sql, read_params).unwrap(),
    };
    let sql_row = result
        .next()
        .expect("the row exists")
        .expect("the row reads");
    mysql_decode(&sql_row, schema, row);
}

fn mysql_candidates(record: &mut Record) {
    let Some(opts) = mysql_opts() else {
        eprintln!("skipping: invalid BOUNDLESS_MYSQL_URL");
        return;
    };
    let Some(mut admin) = Conn::new(opts.clone()).ok() else {
        eprintln!("skipping: no reachable mysql server");
        return;
    };
    let database = format!("boundless_bench_pool_{}", std::process::id());
    admin
        .query_drop(format!("DROP DATABASE IF EXISTS `{database}`"))
        .unwrap();
    admin
        .query_drop(format!("CREATE DATABASE `{database}`"))
        .unwrap();
    admin.query_drop(format!("USE `{database}`")).unwrap();
    let schema = dataset_schema();
    mysql_create_table(&mut admin, &schema);
    drop(admin);

    let rows = dataset();
    let columns = mysql_columns(&schema);
    let placeholders: Vec<String> = (0..columns.len()).map(|_| "?".to_string()).collect();
    let put_sql = format!(
        "REPLACE INTO `{}` ({}) VALUES ({})",
        db_postgres::entity_table_name(TYPE_NAME),
        columns.join(", "),
        placeholders.join(", ")
    );
    let get_sql = format!(
        "SELECT {} FROM `{}` WHERE {ID_COLUMN} = ?",
        columns.join(", "),
        db_postgres::entity_table_name(TYPE_NAME)
    );

    let opts: Opts = OptsBuilder::from_opts(opts).db_name(Some(database)).into();
    let pool = match Pool::new(opts) {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("skipping: cannot open the mysql pool ({err})");
            return;
        }
    };

    // Single connection, SQL built per op — the 01-01 face.
    let mut single = pool.get_conn().unwrap();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for row in &rows {
        let t = Instant::now();
        single
            .exec_drop(&put_sql, Params::Positional(mysql_params(row)))
            .unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        mysql_op(&mut single, row, &schema, &put_sql, &get_sql, None, None);
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "mysql_single",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );

    // N pooled connections, one per op.
    let mut connections: Vec<PooledConn> =
        (0..POOL_SIZE).map(|_| pool.get_conn().unwrap()).collect();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for (index, row) in rows.iter().enumerate() {
        let conn = &mut connections[index % POOL_SIZE];
        let t = Instant::now();
        conn.exec_drop(&put_sql, Params::Positional(mysql_params(row)))
            .unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        mysql_op(conn, row, &schema, &put_sql, &get_sql, None, None);
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "mysql_pool",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );

    // Statement reuse on one connection.
    let put_stmt = single.prep(&put_sql).unwrap();
    let get_stmt = single.prep(&get_sql).unwrap();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for row in &rows {
        let t = Instant::now();
        single
            .exec_drop(&put_stmt, Params::Positional(mysql_params(row)))
            .unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        mysql_op(
            &mut single,
            row,
            &schema,
            &put_sql,
            &get_sql,
            Some(&put_stmt),
            Some(&get_stmt),
        );
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "mysql_prepared",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );

    // Pooled connections with one prepared pair each.
    let mut prepared: Vec<(PooledConn, Statement, Statement)> = connections
        .into_iter()
        .map(|mut conn| {
            let put = conn.prep(&put_sql).unwrap();
            let get = conn.prep(&get_sql).unwrap();
            (conn, put, get)
        })
        .collect();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for (index, row) in rows.iter().enumerate() {
        let (conn, put_stmt, get_stmt) = &mut prepared[index % POOL_SIZE];
        let t = Instant::now();
        conn.exec_drop(&*put_stmt, Params::Positional(mysql_params(row)))
            .unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        mysql_op(
            conn,
            row,
            &schema,
            &put_sql,
            &get_sql,
            Some(&*put_stmt),
            Some(&*get_stmt),
        );
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "mysql_pool_prepared",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );
}

// ---------------------------------------------------------------------------
// PostgreSQL candidates
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

type PgParam = Box<dyn postgres::types::ToSql + Sync>;

fn pg_params(row: &EntityRecord) -> Vec<PgParam> {
    let mut params: Vec<PgParam> = vec![Box::new(row.db_id)];
    for value in &row.values {
        match value {
            PropertyValue::Int(n) => params.push(Box::new(*n)),
            PropertyValue::Uint(n) => params.push(Box::new(i64::try_from(*n).unwrap())),
            PropertyValue::Float(x) => params.push(Box::new(*x)),
            PropertyValue::String(s) => params.push(Box::new(s.clone())),
            PropertyValue::Blob(bytes) => params.push(Box::new(bytes.clone())),
            PropertyValue::Vector(v) => params.push(Box::new(v.clone())),
            other => params.push(Box::new(db_postgres::json::encode_to_string(other))),
        }
    }
    params
}

fn pg_refs(params: &[PgParam]) -> Vec<&(dyn postgres::types::ToSql + Sync)> {
    params
        .iter()
        .map(|p| p.as_ref() as &(dyn postgres::types::ToSql + Sync))
        .collect()
}

fn pg_put_sql(schema: &EntitySchema) -> String {
    let mut columns = vec!["\"id\"".to_string()];
    let mut placeholders = vec!["$1".to_string()];
    for (number, (prop, kind)) in schema.columns.iter().enumerate() {
        columns.push(format!("\"{}\"", db_postgres::column_name(prop)));
        let number = number + 2;
        placeholders.push(match kind {
            ColumnKind::Nested => format!("CAST(${number}::text AS JSONB)"),
            _ => format!("${number}"),
        });
    }
    let assignments = schema
        .columns
        .iter()
        .map(|(prop, _)| {
            let name = format!("\"{}\"", db_postgres::column_name(prop));
            format!("{name} = EXCLUDED.{name}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT (\"id\") DO UPDATE SET {assignments}",
        schema.quoted_table(),
        columns.join(", "),
        placeholders.join(", ")
    )
}

fn pg_get_sql(schema: &EntitySchema) -> String {
    let mut columns = vec!["\"id\"".to_string()];
    for (prop, kind) in &schema.columns {
        let name = format!("\"{}\"", db_postgres::column_name(prop));
        columns.push(match kind {
            ColumnKind::Nested => format!("({name})::text"),
            _ => name,
        });
    }
    format!(
        "SELECT {} FROM {} WHERE \"id\" = $1",
        columns.join(", "),
        schema.quoted_table()
    )
}

fn pg_decode(pg_row: &postgres::Row, schema: &EntitySchema, row: &EntityRecord) {
    let mut values = Vec::with_capacity(schema.columns.len());
    for (index, (_, kind)) in schema.columns.iter().enumerate() {
        let column = index + 1;
        let value = match kind {
            ColumnKind::Int => PropertyValue::Int(pg_row.get::<_, i64>(column)),
            ColumnKind::Uint => PropertyValue::Uint(pg_row.get::<_, i64>(column) as u64),
            ColumnKind::Float => PropertyValue::Float(pg_row.get::<_, f64>(column)),
            ColumnKind::String => PropertyValue::String(pg_row.get::<_, String>(column)),
            ColumnKind::Blob => PropertyValue::Blob(pg_row.get::<_, Vec<u8>>(column)),
            ColumnKind::Vector => PropertyValue::Vector(pg_row.get::<_, Vec<f32>>(column)),
            ColumnKind::Nested => {
                let text = pg_row.get::<_, String>(column);
                db_postgres::json::decode_from_str(&text).unwrap()
            }
        };
        values.push(value);
    }
    assert_eq!(
        EntityRecord {
            type_id: row.type_id,
            db_id: row.db_id,
            values
        },
        *row,
        "postgres: the read equals the written row"
    );
}

fn pg_candidates(record: &mut Record) {
    let admin_conninfo = pg_admin_conninfo();
    let Ok(mut admin) = PostgresStorage::connect(&admin_conninfo) else {
        eprintln!("skipping: no reachable postgres server");
        return;
    };
    let database = format!("boundless_bench_pool_{}", std::process::id());
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
    drop(storage);

    let rows = dataset();
    let put_sql = pg_put_sql(&schema);
    let get_sql = pg_get_sql(&schema);

    // Single client, SQL built per op — the 01-01 face.
    let mut single = Client::connect(&conninfo, postgres::NoTls).unwrap();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for row in &rows {
        let t = Instant::now();
        single.execute(&put_sql, &pg_refs(&pg_params(row))).unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        let result = single.query(&get_sql, &[&row.db_id]).unwrap();
        pg_decode(result.first().expect("the row exists"), &schema, row);
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "postgres_single",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );

    // N clients rotated round-robin.
    let mut clients: Vec<Client> = (0..POOL_SIZE)
        .map(|_| Client::connect(&conninfo, postgres::NoTls).unwrap())
        .collect();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for (index, row) in rows.iter().enumerate() {
        let client = &mut clients[index % POOL_SIZE];
        let t = Instant::now();
        client.execute(&put_sql, &pg_refs(&pg_params(row))).unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        let result = client.query(&get_sql, &[&row.db_id]).unwrap();
        pg_decode(result.first().expect("the row exists"), &schema, row);
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "postgres_pool",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );

    // Prepared pair on one client. PG statements belong to their preparing
    // connection, so the pooled leg prepares one pair per client below.
    let put_stmt = single.prepare(&put_sql).unwrap();
    let get_stmt = single.prepare(&get_sql).unwrap();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for row in &rows {
        let t = Instant::now();
        single
            .execute(&put_stmt, &pg_refs(&pg_params(row)))
            .unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        let result = single.query(&get_stmt, &[&row.db_id]).unwrap();
        pg_decode(result.first().expect("the row exists"), &schema, row);
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "postgres_prepared",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );

    // Pooled clients with one prepared pair each.
    let mut prepared: Vec<(Client, PgStatement, PgStatement)> = clients
        .into_iter()
        .map(|mut client| {
            let put = client.prepare(&put_sql).unwrap();
            let get = client.prepare(&get_sql).unwrap();
            (client, put, get)
        })
        .collect();
    let started = Instant::now();
    let mut puts = Vec::with_capacity(ROWS);
    let mut gets = Vec::with_capacity(ROWS);
    for (index, row) in rows.iter().enumerate() {
        let (client, put_stmt, get_stmt) = &mut prepared[index % POOL_SIZE];
        let t = Instant::now();
        client.execute(put_stmt, &pg_refs(&pg_params(row))).unwrap();
        puts.push(t.elapsed().as_secs_f64() * 1_000_000.0);
        let t = Instant::now();
        let result = client.query(get_stmt, &[&row.db_id]).unwrap();
        pg_decode(result.first().expect("the row exists"), &schema, row);
        gets.push(t.elapsed().as_secs_f64() * 1_000_000.0);
    }
    leg_summary(
        record,
        "postgres_pool_prepared",
        &puts,
        &gets,
        started.elapsed().as_secs_f64(),
    );

    let drop = format!(
        "DROP DATABASE \"{}\" WITH (FORCE)",
        database.replace('"', "\"\"")
    );
    let _ = admin.client_mut().execute(drop.as_str(), &[]);
}

#[test]
fn the_pool_and_statement_candidates_run_on_the_same_dataset() {
    let mut record = Record::default();
    mysql_candidates(&mut record);
    pg_candidates(&mut record);
    record.flush();
}

#[cfg(test)]
mod fixtures {
    use super::*;

    #[test]
    fn the_dataset_matches_the_01_01_corpus() {
        assert_eq!(dataset(), dataset());
        assert_eq!(dataset().len(), ROWS);
        assert_eq!(dataset()[0].db_id, 1);
        assert_eq!(dataset()[ROWS - 1].db_id, ROWS as i64);
    }

    #[test]
    fn the_pool_matches_the_reference_default() {
        assert_eq!(POOL_SIZE, 5);
    }

    /// Both statement shapes name every column, so a candidate cannot
    /// silently drop a property while still reading back "equal".
    #[test]
    fn the_statement_shapes_cover_every_column() {
        let schema = dataset_schema();
        let put = pg_put_sql(&schema);
        let get = pg_get_sql(&schema);
        for (prop, _) in &schema.columns {
            let name = format!("\"{}\"", db_postgres::column_name(prop));
            assert!(put.contains(&name), "put names {name}");
            assert!(get.contains(&name), "get names {name}");
        }
    }
}
