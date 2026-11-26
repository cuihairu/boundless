//! BW-14-01-05 the DB write a logging-out base issues.
//!
//! `Base::destroy(deleteFromDB, writeToDB, logOffFromDB)` (`base.cpp:1488`)
//! decides the flags and `Base::writeToDB` (`base.cpp:1991`) normalises them
//! before any database call:
//!
//! * zero flags are nothing at all (`base.cpp:1997`);
//! * `WRITE_DELETE_FROM_DB` implies `WRITE_LOG_OFF` and excludes
//!   `WRITE_BASE_CELL_DATA` (`base.cpp:2005-2008`) — a deletion neither
//!   persists base nor cell data;
//! * a log-off for a base that was never written (`databaseID_ == 0`,
//!   `hasWrittenToDB`, `base.hpp:85`) is refused: "we don't need to tell the
//!   database to log off things that are not in the database"
//!   (`base.cpp:2011-2016`);
//! * a type that cannot be on a cell never writes cell data
//!   (`base.cpp:2018-2022`);
//! * the cell-data argument resolves to the supplied section, the stored
//!   `pCellData_` when the flags ask for cell data (a NULL stored section
//!   aborts), or `Py_None` otherwise (`base.cpp:2024-2049`);
//! * `WRITE_BASE_DATA` runs the script `onWriteToDB` before anything is
//!   written (`base.cpp:2051-2059`);
//! * a destroy never forces a backup (`base.cpp:2068` `shouldBackup = !
//!   inDestroy_`);
//! * a still-pending id (`PENDING_DATABASE_ID`) writes as 0, which routes to
//!   the alpha DBApp (`base.cpp:2126-2133`).
//!
//! The flags themselves are `WriteDBFlag` (`lib/server/writedb.hpp`), a
//! `uint16` mask. What the flags then do to the storage is the run's
//! business: `WRITE_BASE_DATA` persists the row (`putEntity`),
//! `WRITE_LOG_OFF` releases the checkout (`removeLogOnRecord`), and
//! `WRITE_DELETE_FROM_DB` deletes it (`delEntity`) — the same faces the
//! FantasyDemo logout-and-relogin run drives.

use foundation::ids::{DatabaseId, PENDING_DATABASE_ID};

/// `WriteDBFlag` (`lib/server/writedb.hpp`).
pub type WriteDbFlags = u16;

/// `WRITE_LOG_OFF`.
pub const WRITE_LOG_OFF: WriteDbFlags = 1;
/// `WRITE_EXPLICIT`.
pub const WRITE_EXPLICIT: WriteDbFlags = 2;
/// `WRITE_BASE_DATA`.
pub const WRITE_BASE_DATA: WriteDbFlags = 4;
/// `WRITE_CELL_DATA`.
pub const WRITE_CELL_DATA: WriteDbFlags = 8;
/// `WRITE_BASE_CELL_DATA`.
pub const WRITE_BASE_CELL_DATA: WriteDbFlags = WRITE_BASE_DATA | WRITE_CELL_DATA;
/// `WRITE_DELETE_FROM_DB`.
pub const WRITE_DELETE_FROM_DB: WriteDbFlags = 16;
/// `WRITE_AUTO_LOAD_YES`.
pub const WRITE_AUTO_LOAD_YES: WriteDbFlags = 32;
/// `WRITE_AUTO_LOAD_NO`.
pub const WRITE_AUTO_LOAD_NO: WriteDbFlags = 64;
/// `WRITE_AUTO_LOAD_MASK`.
pub const WRITE_AUTO_LOAD_MASK: WriteDbFlags = WRITE_AUTO_LOAD_YES | WRITE_AUTO_LOAD_NO;
/// `WRITE_TO_PRIMARY_DATABASE`.
pub const WRITE_TO_PRIMARY_DATABASE: WriteDbFlags = 128;
/// `WRITE_EXPLICIT_DBID`.
pub const WRITE_EXPLICIT_DBID: WriteDbFlags = 256;

/// The cell-data argument `Base::writeToDB` settles on
/// (`base.cpp:2024-2049`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellDataArg {
    /// The caller supplied `pCellData`.
    Supplied,
    /// The stored `pCellData_` is written (the flags asked for cell data).
    FromBase,
    /// `Py_None`: the write carries no cell payload.
    NoneValue,
}

/// The database write a `writeToDB` prologue ends up issuing, ready for the
/// storage calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteToDbPlan {
    /// The normalised flags.
    pub flags: WriteDbFlags,
    /// The id the write names: the base's own, or 0 while pending.
    pub db_id: DatabaseId,
    /// `WRITE_BASE_DATA`: the script `onWriteToDB` runs first.
    pub call_on_write_to_db: bool,
    /// `shouldBackup = !inDestroy_`.
    pub should_backup: bool,
    /// The resolved cell-data argument.
    pub cell_data: CellDataArg,
}

/// `Base::writeToDB`'s prologue: normalise `flags` into the plan the storage
/// calls implement, or `None` when the reference aborts before writing.
///
/// `database_id` is `Base::databaseID_`: 0 means "never written"
/// (`hasWrittenToDB`, `base.hpp:85`) and [`PENDING_DATABASE_ID`] means the
/// initial write is still in flight. `supplied_cell_data` is whether the
/// caller passed `pCellData`.
///
/// A `None` here is the reference's `return false`. The zero-flags case
/// additionally completes the reply as failed (`base.cpp:1998` calls
/// `onWriteToDBComplete(false)`) — the reply leg is the caller's, not the
/// plan's.
pub fn plan_write_to_db(
    flags: WriteDbFlags,
    database_id: DatabaseId,
    can_be_on_cell: bool,
    has_stored_cell_data: bool,
    supplied_cell_data: bool,
    in_destroy: bool,
) -> Option<WriteToDbPlan> {
    // Nothing to do (`base.cpp:1996-2001`).
    if flags == 0 {
        return None;
    }

    let mut flags = flags;
    // A deletion logs off and neither persists base nor cell data
    // (`base.cpp:2003-2008`).
    if flags & WRITE_DELETE_FROM_DB != 0 {
        flags |= WRITE_LOG_OFF;
        flags &= !WRITE_BASE_CELL_DATA;
    }

    // Never-written bases have nothing to log off (`base.cpp:2010-2016`).
    if flags & WRITE_LOG_OFF != 0 && database_id == 0 {
        return None;
    }

    // A base-only type carries no cell data (`base.cpp:2018-2022`).
    if !can_be_on_cell {
        flags &= !WRITE_CELL_DATA;
    }

    // The cell-data argument (`base.cpp:2024-2049`).
    let cell_data = if supplied_cell_data {
        CellDataArg::Supplied
    } else if flags & WRITE_CELL_DATA != 0 {
        if !has_stored_cell_data {
            // "pCellData_ for %u is NULL, aborting" (`base.cpp:2033-2041`).
            return None;
        }
        CellDataArg::FromBase
    } else {
        CellDataArg::NoneValue
    };

    // The id the write names (`base.cpp:2126-2133`): a pending id writes as
    // 0, which routes the operation to the alpha DBApp.
    let db_id = if database_id == PENDING_DATABASE_ID {
        0
    } else {
        database_id
    };

    Some(WriteToDbPlan {
        flags,
        db_id,
        call_on_write_to_db: flags & WRITE_BASE_DATA != 0,
        should_backup: !in_destroy,
        cell_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero flags are nothing at all (`base.cpp:1996`).
    #[test]
    fn zero_flags_plan_nothing() {
        assert_eq!(plan_write_to_db(0, 5, true, true, false, false), None);
    }

    /// The plain logout of a written cell base keeps every flag it was
    /// given.
    #[test]
    fn a_plain_logout_persists_and_logs_off() {
        let plan = plan_write_to_db(
            WRITE_LOG_OFF | WRITE_BASE_CELL_DATA,
            5,
            true,
            true,
            false,
            true,
        )
        .expect("the logout writes");
        assert_eq!(plan.flags, WRITE_LOG_OFF | WRITE_BASE_CELL_DATA);
        assert_eq!(plan.db_id, 5);
        assert!(plan.call_on_write_to_db);
        assert!(!plan.should_backup, "a destroy never forces a backup");
        assert_eq!(plan.cell_data, CellDataArg::FromBase);
    }

    /// A deletion logs off and carries no data flags.
    #[test]
    fn a_deletion_logs_off_and_drops_the_data() {
        let plan = plan_write_to_db(
            WRITE_DELETE_FROM_DB | WRITE_BASE_CELL_DATA,
            5,
            true,
            true,
            false,
            false,
        )
        .expect("the deletion writes");
        assert_eq!(plan.flags, WRITE_DELETE_FROM_DB | WRITE_LOG_OFF);
        assert_eq!(plan.db_id, 5);
        assert!(
            !plan.call_on_write_to_db,
            "the data flags are gone, so no onWriteToDB"
        );
        assert_eq!(plan.cell_data, CellDataArg::NoneValue);
    }

    /// Logging off a base that was never written is refused
    /// (`base.cpp:2010-2016`).
    #[test]
    fn a_never_written_base_refuses_the_log_off() {
        assert_eq!(
            plan_write_to_db(WRITE_LOG_OFF, 0, false, false, false, false),
            None
        );
    }

    /// A base-only type never writes cell data (`base.cpp:2018-2022`), and
    /// without the flag the cell-data argument is `Py_None`.
    #[test]
    fn a_base_only_type_drops_the_cell_data() {
        let plan = plan_write_to_db(
            WRITE_LOG_OFF | WRITE_BASE_CELL_DATA,
            5,
            false,
            true,
            false,
            false,
        )
        .expect("the write happens");
        assert_eq!(plan.flags, WRITE_LOG_OFF | WRITE_BASE_DATA);
        assert_eq!(plan.cell_data, CellDataArg::NoneValue);
    }

    /// Cell data with the flag but no stored section aborts
    /// (`base.cpp:2033-2041`).
    #[test]
    fn missing_stored_cell_data_aborts() {
        assert_eq!(
            plan_write_to_db(WRITE_CELL_DATA, 5, true, false, false, false),
            None
        );
    }

    /// A supplied cell-data argument wins over the stored one.
    #[test]
    fn a_supplied_cell_data_argument_wins() {
        let plan = plan_write_to_db(WRITE_BASE_CELL_DATA, 5, true, true, true, false)
            .expect("the write happens");
        assert_eq!(plan.cell_data, CellDataArg::Supplied);
    }

    /// A pending id writes as 0 (`base.cpp:2126-2133`) — and a pending base
    /// still counts as written for the log-off gate (`base.hpp:85`, -1 !=
    /// 0).
    #[test]
    fn a_pending_id_writes_as_zero() {
        let plan = plan_write_to_db(
            WRITE_LOG_OFF | WRITE_BASE_CELL_DATA,
            PENDING_DATABASE_ID,
            true,
            true,
            false,
            false,
        )
        .expect("the pending write happens");
        assert_eq!(plan.db_id, 0);
    }

    /// Not being in a destroy keeps the backup (`base.cpp:2068`).
    #[test]
    fn a_write_outside_a_destroy_backs_up() {
        let plan = plan_write_to_db(WRITE_BASE_CELL_DATA, 5, true, true, false, false)
            .expect("the write happens");
        assert!(plan.should_backup);
    }
}
