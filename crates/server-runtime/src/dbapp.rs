//! The DBApp role: storage startup and the engine report
//! (`server/dbapp/dbapp.cpp`).
//!
//! At startup the reference DBApp reads the configured database type and
//! creates the engine for it (`DatabaseEngineCreator::createInstance`), then
//! reports the engine it is running on
//! (`DBENGINE_INFO_MSG("Database engine = %s\n", databaseType.c_str())`,
//! `dbapp.cpp:427-440`). A `<db><type>` no engine factory answers to fails
//! the init with "Failed to create database of type: %s". This module
//! carries that startup surface; the entity operations land with the
//! BW-09-01 lane.

use db::DbError;
use db::config::{DatabaseConfig, StorageBackend};

/// The DBApp process state: the storage configuration it started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbApp {
    config: DatabaseConfig,
}

impl DbApp {
    /// `DBApp::init` — starts the app on the given storage configuration.
    pub fn init(config: DatabaseConfig) -> Self {
        Self { config }
    }

    /// The XML bootstrap path: builds the config from the raw `<db><type>`
    /// string. Unknown names fail the init the way the reference does.
    pub fn init_from_type_name(name: &str) -> Result<Self, DbError> {
        Ok(Self::init(DatabaseConfig::from_type_name(name)?))
    }

    /// The storage configuration this app started with.
    pub fn config(&self) -> &DatabaseConfig {
        &self.config
    }

    /// The engine selected at startup.
    pub fn backend(&self) -> StorageBackend {
        self.config.backend
    }

    /// The `Database engine = %s` startup report
    /// (`dbapp.cpp:427`).
    pub fn engine_report(&self) -> String {
        self.config.backend.describe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_reports_the_configured_backend() {
        // Acceptance (BW-09-06-01): the DBApp reports which storage backend
        // it is using at startup.
        let app = DbApp::init_from_type_name("postgresql").unwrap();
        assert_eq!(app.backend(), StorageBackend::PostgreSql);
        assert_eq!(app.engine_report(), "Database engine = postgresql");
        assert_eq!(
            app.config().postgres.database,
            db::config::PostgresConnection::default().database
        );
    }

    #[test]
    fn default_config_reports_xml() {
        let app = DbApp::init(DatabaseConfig::default());
        assert_eq!(app.engine_report(), "Database engine = xml");
    }

    #[test]
    fn unknown_type_fails_the_startup() {
        let err = DbApp::init_from_type_name("db2").unwrap_err();
        assert_eq!(err, DbError::UnknownBackend("db2".to_string()));
        assert_eq!(err.to_string(), "Failed to create database of type: db2");
    }
}
