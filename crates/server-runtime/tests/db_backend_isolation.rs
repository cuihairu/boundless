//! BW-16-02-03: database drivers are isolated - every backend can be
//! enabled on its own.
//!
//! The acceptance: 可单独启用后端. Each storage backend switches on
//! without constructing or requiring any other. The enable surface is the
//! DBApp startup (`server/dbapp/dbapp.cpp`): the configured `<db><type>`
//! string selects the engine (`DatabaseEngineCreator::createInstance`),
//! a name no factory answers to fails the init, and the chosen engine is
//! the one the app reports running (`Database engine = %s`). Under that,
//! the [`db::EntityStorage`] contract is served by the in-memory backend
//! with no database process at all, while the PostgreSQL driver stands on
//! its own connection string and fails on its own when its server is
//! absent.

use db::{
    DatabaseConfig, DbError, EntityKey, EntityRecord, EntityStorage, MemoryStorage, StorageBackend,
    load_entity,
};
use db_postgres::PostgresStorage;
use server_runtime::dbapp::DbApp;

#[test]
fn every_engine_name_enables_its_own_backend() {
    // `DatabaseEngineCreator::createInstance`: each configured `<db><type>`
    // name creates that engine and nothing else.
    let expected = [
        ("xml", StorageBackend::Xml),
        ("mysql", StorageBackend::MySql),
        ("postgresql", StorageBackend::PostgreSql),
    ];
    for (name, backend) in expected {
        let app = DbApp::init_from_type_name(name).unwrap();
        assert_eq!(app.backend(), backend, "{name} enables its own engine");
        assert_eq!(app.engine_report(), format!("Database engine = {name}"));
    }

    // The config default is the reference's flat-file debug backend.
    assert_eq!(DatabaseConfig::default().backend, StorageBackend::Xml);
}

#[test]
fn an_unconfigured_backend_fails_without_affecting_the_others() {
    // A name no engine factory answers to: DBApp::init fails with the
    // configured string, and the selectable engines are untouched.
    let err = DbApp::init_from_type_name("db2").unwrap_err();
    assert_eq!(err, DbError::UnknownBackend("db2".to_string()));
    assert_eq!(err.to_string(), "Failed to create database of type: db2");
    assert_eq!(
        DbApp::init_from_type_name("postgresql").unwrap().backend(),
        StorageBackend::PostgreSql
    );
}

#[test]
fn the_memory_backend_serves_reads_without_a_database() {
    // The whole read path runs on the contract with zero database process:
    // seed, resolve by DBID, and honour the "row missing is not a failure"
    // outcome (`Ok(None)`).
    let mut store = MemoryStorage::new();
    assert!(store.is_empty());

    store.insert(EntityRecord {
        type_id: 3,
        db_id: 1001,
        values: Vec::new(),
    });
    assert_eq!(store.len(), 1);

    let found = load_entity(
        &store,
        &EntityKey {
            type_id: 3,
            db_id: 1001,
        },
    )
    .unwrap();
    assert_eq!(found.db_id, 1001);

    let key = EntityKey {
        type_id: 3,
        db_id: 1002,
    };
    assert_eq!(store.get_entity(&key), Ok(None));
    assert_eq!(load_entity(&store, &key), Err(DbError::NotFound));
}

#[test]
fn the_postgres_driver_stands_alone() {
    // The PostgreSQL driver enables and reports on its own connection
    // string; with no server listening it fails into the contract's error
    // type without touching any other backend.
    let err =
        PostgresStorage::connect("host=127.0.0.1 port=1 dbname=bigworld user=bigworld password=")
            .unwrap_err();
    assert!(matches!(err, DbError::Storage(_)), "own failure: {err}");
    // The memory backend keeps serving while the postgres one is down.
    let store = MemoryStorage::new();
    assert_eq!(
        load_entity(
            &store,
            &EntityKey {
                type_id: 1,
                db_id: 1
            }
        ),
        Err(DbError::NotFound)
    );
}
