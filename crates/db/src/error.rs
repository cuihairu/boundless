//! Storage and entity-operation errors.
//!
//! The reference `DBStorage::getEntity`/`putEntity` return a status, not an
//! exception. This enum mirrors that contract: a missing entity is a value
//! ([`DbError::NotFound`]) rather than a panicking `unwrap`, a transient
//! backend condition is [`DbError::Retry`], and everything else is a
//! [`DbError::Storage`] message. The unique-key outcome
//! ([`DbError::DuplicateAccount`]) is its own variant because the account
//! tables carry a unique name constraint — inserting a name that already
//! exists is a contract outcome, not a storage fault. The unknown-type
//! outcome ([`DbError::UnknownBackend`]) carries the configured database
//! type name, mirroring `DBApp::init`'s "Failed to create database of type"
//! error for a `<db><type>` no engine answers to.

use std::fmt;

/// A failure from the storage contract or an entity operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbError {
    /// The entity or key does not exist in storage. This is the "missing
    /// DBID" / "missing name" outcome, not a failure.
    NotFound,
    /// A transient condition (a locked row, a backend that is momentarily
    /// unreachable): the caller may retry. Mirrors the reference `RETRY`
    /// status.
    Retry,
    /// A unique-key violation on the account tables: the log-on name (or
    /// email) already belongs to another account row.
    DuplicateAccount,
    /// The configured `<db><type>` name has no database engine. The payload
    /// is the configured type string (`DBApp::init`,
    /// `server/dbapp/dbapp.cpp`).
    UnknownBackend(String),
    /// A backend or contract failure with a human-readable message.
    Storage(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::NotFound => write!(f, "entity not found"),
            DbError::Retry => write!(f, "storage busy, retry"),
            DbError::DuplicateAccount => write!(f, "account already exists"),
            DbError::UnknownBackend(name) => {
                write!(f, "Failed to create database of type: {name}")
            }
            DbError::Storage(msg) => write!(f, "storage error: {msg}"),
        }
    }
}

impl std::error::Error for DbError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_state_the_contract_outcome() {
        assert_eq!(DbError::NotFound.to_string(), "entity not found");
        assert_eq!(DbError::Retry.to_string(), "storage busy, retry");
        assert_eq!(
            DbError::DuplicateAccount.to_string(),
            "account already exists"
        );
        assert_eq!(
            DbError::UnknownBackend("oracle".to_string()).to_string(),
            "Failed to create database of type: oracle"
        );
        assert_eq!(
            DbError::Storage("lock wait".to_string()).to_string(),
            "storage error: lock wait"
        );
    }
}
