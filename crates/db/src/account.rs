//! The account tables and the user-type binding contract.
//!
//! The reference keeps accounts in three of its own tables and reads them with
//! one join (`db_storage_mysql/tasks/get_entity_key_for_account_task.cpp:38-48`):
//!
//! ```sql
//! SELECT (i.isPasswordHashed AND m.password = MD5( CONCAT( ?, m.logOnName ) ))
//!        OR (NOT i.isPasswordHashed AND m.password = ?),
//!        t.bigworldID, m.entityID
//! FROM bigworldLogOnMapping m
//! LEFT JOIN bigworldEntityTypes t ON m.entityType = t.typeID
//! JOIN bigworldInfo i
//! WHERE m.logOnName = ?
//! ```
//!
//! `isPasswordHashed` is a server-wide switch read from the config
//! (`mysql_database.cpp:165`, default true), and `bigworldEntityTypes` is the
//! type-id to BigWorld-id mapping the mapping face reads from.

/// `bigworldLogOnMapping`: the account row itself.
pub const LOG_ON_MAPPING_TABLE: &str = "bigworldLogOnMapping";
/// `bigworldEntityTypes`: the entity type id to BigWorld id mapping.
pub const ENTITY_TYPES_TABLE: &str = "bigworldEntityTypes";
/// `bigworldInfo`: server-wide switches, among them `isPasswordHashed`.
pub const INFO_TABLE: &str = "bigworldInfo";
/// The account name column; also the salt the password hash is built from.
pub const LOG_ON_NAME_COLUMN: &str = "logOnName";
/// The stored password: an MD5 digest when hashing is on, the plain password
/// when it is off.
pub const PASSWORD_COLUMN: &str = "password";
/// The account's entity type id, joined to [`ENTITY_TYPES_TABLE`].
pub const ENTITY_TYPE_COLUMN: &str = "entityType";
/// The entity id the account owns.
pub const ENTITY_ID_COLUMN: &str = "entityID";
/// The BigWorld id the type maps to.
pub const BIGWORLD_ID_COLUMN: &str = "bigworldID";
/// The [`INFO_TABLE`] switch for stored password hashes.
pub const IS_PASSWORD_HASHED_COLUMN: &str = "isPasswordHashed";

/// The `LogOnStatus` outcomes the account lookup reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountLogOn {
    /// `LOGGED_ON`: the account exists and the password matched.
    LoggedOn,
    /// `LOGIN_REJECTED_NO_SUCH_USER`: no row carries the name
    /// (`getResult` found nothing to read).
    RejectedNoSuchUser,
    /// `LOGIN_REJECTED_INVALID_PASSWORD`: the row exists, the password did not
    /// match.
    RejectedInvalidPassword,
    /// `LOGIN_REJECTED_DB_GENERAL_FAILURE`: the query itself failed
    /// (`GetEntityKeyForAccountTask::onException`).
    RejectedDbGeneralFailure,
}

/// One `bigworldLogOnMapping` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    /// `logOnName`, and the salt the password digest is built from.
    pub log_on_name: String,
    /// `password`: an MD5 digest when hashing is on, the plain password when it
    /// is off.
    pub password: String,
    /// `entityType`, joined to [`ENTITY_TYPES_TABLE`].
    pub entity_type: i32,
    /// `entityID`.
    pub entity_id: i32,
}

/// Decide the login outcome for one account row.
///
/// `_is_password_hashed` is the server-wide `bigworldInfo` switch. The
/// comparison the reference makes is byte-exact on `m.password`, so the flag
/// never changes what this function compares: it decides how the **caller**
/// forms `presented` — the MD5 digest of `password || logOnName` when hashing
/// is on, the plain password when it is off — and it is carried on the
/// signature for symmetry with the reference query. The port stops at the string
/// comparison the reference makes, leaving the digest to the caller so this
/// module carries no hashing dependency.
///
/// With hashing on, a stored plain password can never match, and with hashing
/// off a stored digest cannot — the reference is equally strict.
pub fn check_password(
    row: &AccountRow,
    _is_password_hashed: bool,
    presented: &str,
) -> AccountLogOn {
    if presented == row.password {
        AccountLogOn::LoggedOn
    } else {
        AccountLogOn::RejectedInvalidPassword
    }
}

/// The outcome of asking to bind a user type to the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserTypeBinding {
    /// The user type bound to one of the property mappings.
    Bound,
    /// Nothing could bind it. The reference treats this as a parse error and
    /// stops the DBApp rather than altering tables, so a mapping is handed back
    /// with the error message instead of a panic.
    Unbound(String),
}

/// `PropertyMapping::create`'s user-type arm
/// (`property_mapping.cpp:246-258`): the user type gets to bind itself, and an
/// unbound result is reported as the reference reports it.
pub fn bind_user_type(property_name: &str, bind: impl FnOnce() -> bool) -> UserTypeBinding {
    if bind() {
        UserTypeBinding::Bound
    } else {
        UserTypeBinding::Unbound(format!(
            "Unable to bind USER_TYPE property '{property_name}' to database."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, password: &str) -> AccountRow {
        AccountRow {
            log_on_name: name.to_owned(),
            password: password.to_owned(),
            entity_type: 7,
            entity_id: 41,
        }
    }

    #[test]
    fn a_hashed_password_matches_only_the_digest() {
        let stored = row("alice", "900150983cd24fb0d6963f7d28e17f72");
        // Hashing is on: the presented value is the digest of password plus
        // name, so the stored digest is what it is compared against.
        assert_eq!(
            check_password(&stored, true, "900150983cd24fb0d6963f7d28e17f72"),
            AccountLogOn::LoggedOn
        );
        // ...and the plain password is not it.
        assert_eq!(
            check_password(&stored, true, "secret"),
            AccountLogOn::RejectedInvalidPassword
        );
    }

    #[test]
    fn a_plain_password_is_rejected_while_hashing_is_on() {
        // With hashing on the reference compares the stored digest against
        // MD5(password + name), so the plain password can never match.
        let stored = row("carol", "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            check_password(&stored, true, "secret"),
            AccountLogOn::RejectedInvalidPassword
        );
    }

    #[test]
    fn an_unhashed_password_matches_only_the_plain_text() {
        let stored = row("bob", "secret");
        assert_eq!(
            check_password(&stored, false, "secret"),
            AccountLogOn::LoggedOn
        );
        // The digest of the same password is not accepted with hashing off.
        assert_eq!(
            check_password(&stored, false, "900150983cd24fb0d6963f7d28e17f72"),
            AccountLogOn::RejectedInvalidPassword
        );
    }

    #[test]
    fn the_account_table_names_match_the_reference() {
        assert_eq!(LOG_ON_MAPPING_TABLE, "bigworldLogOnMapping");
        assert_eq!(ENTITY_TYPES_TABLE, "bigworldEntityTypes");
        assert_eq!(INFO_TABLE, "bigworldInfo");
        assert_eq!(LOG_ON_NAME_COLUMN, "logOnName");
        assert_eq!(PASSWORD_COLUMN, "password");
        assert_eq!(ENTITY_TYPE_COLUMN, "entityType");
        assert_eq!(ENTITY_ID_COLUMN, "entityID");
        assert_eq!(BIGWORLD_ID_COLUMN, "bigworldID");
        assert_eq!(IS_PASSWORD_HASHED_COLUMN, "isPasswordHashed");
    }

    #[test]
    fn a_user_type_that_binds_reports_bound() {
        assert_eq!(bind_user_type("health", || true), UserTypeBinding::Bound);
    }

    #[test]
    fn a_user_type_that_cannot_bind_reports_the_reference_error() {
        let binding = bind_user_type("route", || false);
        assert_eq!(
            binding,
            UserTypeBinding::Unbound(
                "Unable to bind USER_TYPE property 'route' to database.".to_owned()
            )
        );
    }
}
