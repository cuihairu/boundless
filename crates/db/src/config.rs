//! The storage configuration: which database engine the DBApp uses.
//!
//! The reference reads a `<dbMgr><db><type>` string option
//! (`lib/db/db_config.cpp`, default `"xml"`) and hands it to
//! `DatabaseEngineCreator::createInstance`, which returns NULL for a name no
//! engine factory answers to — `DBApp::init` then fails with "Failed to
//! create database of type: %s". This module carries that selection: the
//! engine names, the default, and the PostgreSQL connection settings that
//! the `postgresql` engine consumes (`<db/postgresql>` child section:
//! host / port / database / username / password).

use crate::error::DbError;

/// The database engines this build can create (`DatabaseEngineCreator`).
///
/// `xml` is the flat-file debug backend and the config default; `mysql` is
/// the reference production backend; `postgresql` is this port's production
/// backend (BW-09-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StorageBackend {
    /// `xml` — the reference default backend.
    #[default]
    Xml,
    /// `mysql` — the reference production backend.
    MySql,
    /// `postgresql` — this port's production backend (BW-09-06).
    PostgreSql,
}

impl StorageBackend {
    /// The `<db><type>` name this backend answers to.
    pub fn config_name(self) -> &'static str {
        match self {
            StorageBackend::Xml => "xml",
            StorageBackend::MySql => "mysql",
            StorageBackend::PostgreSql => "postgresql",
        }
    }

    /// Selects the engine from the `<db><type>` string
    /// (`DatabaseEngineCreator::createInstance`). Unknown names yield
    /// [`DbError::UnknownBackend`] — the DBApp init failure.
    pub fn from_config_name(name: &str) -> Result<Self, DbError> {
        match name {
            "xml" => Ok(StorageBackend::Xml),
            "mysql" => Ok(StorageBackend::MySql),
            "postgresql" => Ok(StorageBackend::PostgreSql),
            other => Err(DbError::UnknownBackend(other.to_string())),
        }
    }

    /// The DBApp startup report line
    /// (`DBENGINE_INFO_MSG("Database engine = %s\n", databaseType.c_str())`,
    /// `server/dbapp/dbapp.cpp`).
    pub fn describe(self) -> String {
        format!("Database engine = {}", self.config_name())
    }
}

/// The `<db/postgresql>` child section: how to reach the PostgreSQL server.
///
/// The reference keeps the MySQL settings under `<db/mysql>` the same way;
/// the fields carry the usual libpq connection vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresConnection {
    /// Server host (`<host>`).
    pub host: String,
    /// Server port (`<port>`, PostgreSQL default 5432).
    pub port: u16,
    /// Database name (`<database>`).
    pub database: String,
    /// Role name (`<username>`).
    pub username: String,
    /// Password (`<password>`; empty means no password supplied).
    pub password: String,
}

impl Default for PostgresConnection {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 5432,
            database: "bigworld".to_string(),
            username: "bigworld".to_string(),
            password: String::new(),
        }
    }
}

impl PostgresConnection {
    /// The libpq connection keyword/value string for this connection.
    pub fn to_conninfo(&self) -> String {
        // Values are the operator's own settings, so plain interpolation
        // into `key=value` pairs is enough; spaces inside values still
        // work because libpq accepts them unquoted.
        format!(
            "host={} port={} dbname={} user={} password={}",
            self.host, self.port, self.database, self.username, self.password
        )
    }
}

/// The DBApp storage configuration: the engine plus that engine's settings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatabaseConfig {
    /// The selected engine (`<db><type>`, default [`StorageBackend::Xml`]).
    pub backend: StorageBackend,
    /// The `<db/postgresql>` child section, read when the engine is
    /// [`StorageBackend::PostgreSql`].
    pub postgres: PostgresConnection,
}

impl DatabaseConfig {
    /// Builds the config from the `<db><type>` name, with defaults for the
    /// engine's settings. Unknown names fail the way `DBApp::init` does.
    pub fn from_type_name(name: &str) -> Result<Self, DbError> {
        Ok(Self {
            backend: StorageBackend::from_config_name(name)?,
            postgres: PostgresConnection::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_the_default_backend() {
        // db_config.cpp: the `type` option defaults to "xml".
        let config = DatabaseConfig::default();
        assert_eq!(config.backend, StorageBackend::Xml);
        assert_eq!(config.backend.config_name(), "xml");
    }

    #[test]
    fn engine_names_round_trip() {
        for name in ["xml", "mysql", "postgresql"] {
            let backend = StorageBackend::from_config_name(name).unwrap();
            assert_eq!(backend.config_name(), name);
        }
    }

    #[test]
    fn unknown_backend_fails_the_dbapp_way() {
        // DatabaseEngineCreator::createInstance returns NULL for a name no
        // factory answers to; DBApp::init reports the configured string.
        let err = StorageBackend::from_config_name("oracle").unwrap_err();
        assert_eq!(err, DbError::UnknownBackend("oracle".to_string()));
        assert_eq!(err.to_string(), "Failed to create database of type: oracle");
    }

    #[test]
    fn describe_reports_the_engine() {
        // dbapp.cpp: DBENGINE_INFO_MSG("Database engine = %s\n", ...).
        assert_eq!(StorageBackend::Xml.describe(), "Database engine = xml");
        assert_eq!(
            StorageBackend::PostgreSql.describe(),
            "Database engine = postgresql"
        );
    }

    #[test]
    fn postgres_connection_defaults_and_conninfo() {
        let mut connection = PostgresConnection::default();
        assert_eq!(connection.host, "localhost");
        assert_eq!(connection.port, 5432);
        assert_eq!(
            connection.to_conninfo(),
            "host=localhost port=5432 dbname=bigworld user=bigworld password="
        );

        connection.host = "db.internal".to_string();
        connection.port = 5433;
        connection.database = "bw_shard_2".to_string();
        connection.username = "bw".to_string();
        connection.password = "s3cret".to_string();
        assert_eq!(
            connection.to_conninfo(),
            "host=db.internal port=5433 dbname=bw_shard_2 user=bw password=s3cret"
        );
    }

    #[test]
    fn from_type_name_selects_postgresql() {
        let config = DatabaseConfig::from_type_name("postgresql").unwrap();
        assert_eq!(config.backend, StorageBackend::PostgreSql);
        assert_eq!(config.postgres, PostgresConnection::default());

        assert!(DatabaseConfig::from_type_name("sqlite").is_err());
    }
}
