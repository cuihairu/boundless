//! Storage and entity-operation errors.
//!
//! The reference `DBStorage::getEntity`/`putEntity` return a status, not an
//! exception. This enum mirrors that contract: a missing entity is a value
//! ([`DbError::NotFound`]) rather than a panicking `unwrap`, a transient
//! backend condition is [`DbError::Retry`], and everything else is a
//! [`DbError::Storage`] message.

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
    /// A backend or contract failure with a human-readable message.
    Storage(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::NotFound => write!(f, "entity not found"),
            DbError::Retry => write!(f, "storage busy, retry"),
            DbError::Storage(msg) => write!(f, "storage error: {msg}"),
        }
    }
}

impl std::error::Error for DbError {}
