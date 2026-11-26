//! The `clear_auto_load` tool face — `server/tools/clear_auto_load`
//! (BW-09-05-06).
//!
//! The tool wipes the volatile runtime state a DBApp leaves behind so the
//! next `sync_db`/start is clean: spaces, space data and game times go,
//! and every entity whose `bigworldLogOns` row says `shouldAutoLoad` is
//! deleted outright. The reference drives those statements over its MySQL
//! connection inside one transaction (`clear_auto_load.cpp:72-96`); this
//! port keeps the statement sequence and its failure semantics verbatim
//! behind the [`ClearAutoLoadDatabase`] trait, since the port has no
//! MySQL backend — the caller supplies the legs (a scripted double in
//! tests, or an adapter over a real [`crate::storage::EntityStorage`]).
//!
//! Failure semantics worth noting, all reference-verbatim:
//! - `deleteAutoLoadEntities` catches its own exceptions
//!   (`clear_auto_load.cpp:180-186`), so a failed logon SELECT or a
//!   failed final `DELETE FROM bigworldLogOns` does **not** fail the run —
//!   the transaction still commits.
//! - The earlier three statements share the outer try: the first failure
//!   aborts the run (and the remaining statements never execute).
//! - A per-entity delete that returns false only WARNINGs and moves on.

use std::collections::HashMap;

use foundation::ids::{DatabaseId, EntityTypeId};

use crate::secondary::StreamTypeDef;

/// The `EXIT_SUCCESS`/`EXIT_FAILURE` pair `main` returns.
pub const SUCCESS: i32 = 0;
/// The `EXIT_FAILURE` half of the pair.
pub const FAILURE: i32 = 1;

/// The usage text `main` `puts`es for `--help` (`main.cpp:15-19`).
pub const USAGE_MESSAGE: &str = "\
Usage: clear_auto_load [options]

Options:
 --verbose | -v      Display verbose program output to the console.
 --help | -h         Program usage.
";

/// The four tables `checkTablesExist` insists on
/// (`clear_auto_load.cpp:102-123`).
const REQUIRED_TABLES: [&str; 4] = [
    "bigworldLogOns",
    "bigworldSpaceData",
    "bigworldSpaces",
    "bigworldGameTime",
];

// -----------------------------------------------------------------------------
// Section: the command line (`main.cpp:31-61`)
// -----------------------------------------------------------------------------

/// What the option scan decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Keep going; the flag records whether `--verbose` was seen.
    Run(bool),
    /// `--help` printed the usage: the process exits successfully.
    Help,
    /// An unrecognised argument: the process exits with failure.
    Unrecognised(String),
}

impl ParseOutcome {
    /// Whether the scan wants `main` to keep going.
    pub fn is_run(&self) -> bool {
        matches!(self, ParseOutcome::Run(_))
    }
}

/// The `main` option scan (`main.cpp:37-61`). `args` excludes `argv[0]`.
///
/// `--res`/`-r` swallow the following argument only when one follows —
/// as the last argument they fall through to the rejection, exactly as
/// the reference's `i < (argc - 1)` guard does. `--help`/`-h` and the
/// rejection both stop the scan immediately.
pub fn parse_args(args: &[String]) -> ParseOutcome {
    let mut is_verbose = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if (arg == "--res" || arg == "-r") && index < args.len() - 1 {
            index += 2;
            continue;
        }
        match arg {
            "--verbose" | "-v" => is_verbose = true,
            "--help" | "-h" => return ParseOutcome::Help,
            _ => return ParseOutcome::Unrecognised(arg.to_owned()),
        }
        index += 1;
    }
    ParseOutcome::Run(is_verbose)
}

// -----------------------------------------------------------------------------
// Section: the database legs (`clear_auto_load.cpp`)
// -----------------------------------------------------------------------------

/// The MySQL legs `ClearAutoLoad` drives, as a trait — the port has no
/// MySQL backend, so the caller supplies them.
pub trait ClearAutoLoadDatabase {
    /// `SHOW TABLES` (`checkTablesExist`).
    fn table_names(&mut self) -> Result<Vec<String>, String>;

    /// The auto-load join (`deleteAutoLoadEntities`): `SELECT
    /// et.bigworldID, lo.databaseID FROM bigworldLogOns lo,
    /// bigworldEntityTypes et WHERE lo.typeID = et.typeID AND
    /// lo.shouldAutoLoad` — (entity type id, database id) pairs.
    fn auto_load_logons(&mut self) -> Result<Vec<(EntityTypeId, DatabaseId)>, String>;

    /// `EntityTypeMapping::deleteWithID`: delete the entity row, `false`
    /// when it could not be deleted.
    fn delete_with_id(&mut self, type_id: EntityTypeId, db_id: DatabaseId) -> bool;

    /// `DELETE FROM bigworldSpaces`.
    fn clear_spaces(&mut self) -> Result<(), String>;

    /// `DELETE FROM bigworldSpaceData`.
    fn clear_space_data(&mut self) -> Result<(), String>;

    /// `UPDATE bigworldGameTime SET time = 0`.
    fn reset_game_times(&mut self) -> Result<(), String>;

    /// `DELETE FROM bigworldLogOns`.
    fn clear_logons(&mut self) -> Result<(), String>;
}

/// `ClearAutoLoad::checkTablesExist` (`clear_auto_load.cpp:102-140`):
/// all four required tables must appear in `SHOW TABLES`.
pub fn check_tables_exist(db: &mut dyn ClearAutoLoadDatabase, out: &mut Vec<String>) -> bool {
    let names = match db.table_names() {
        Ok(names) => names,
        Err(error) => {
            out.push(format!("Could not enumerate tables in database: {error}"));
            return false;
        }
    };
    let mut left = REQUIRED_TABLES.len();
    for name in &names {
        if REQUIRED_TABLES.contains(&name.as_str()) {
            left = left.saturating_sub(1);
        }
    }
    if left > 0 {
        out.push("Missing a required table in the database, please run sync_db.".to_owned());
    }
    left == 0
}

/// `ClearAutoLoad::deleteAutoLoadEntities` (`clear_auto_load.cpp:146-187`):
/// delete every auto-loaded entity, then clear the logon rows. The whole
/// body sits in one try — a failed join or a failed logon clear lands in
/// its ERROR_MSG (with the reference's `deleteAutoLoadEnitites` typo,
/// verbatim) and does not propagate.
pub fn delete_auto_load_entities(
    db: &mut dyn ClearAutoLoadDatabase,
    defs: &HashMap<EntityTypeId, StreamTypeDef>,
    out: &mut Vec<String>,
) {
    const CAUGHT: &str = "ClearAutoLoad::deleteAutoLoadEnitites: An exception occurred while \
         removing auto-loading entities from the database";
    let entity_logons = match db.auto_load_logons() {
        Ok(entity_logons) => entity_logons,
        Err(error) => {
            out.push(format!("{CAUGHT}: {error}"));
            return;
        }
    };
    for (type_id, db_id) in entity_logons {
        if !db.delete_with_id(type_id, db_id) {
            // The reference reads the name off the entity description; a
            // type missing from `defs` cannot happen against a healthy
            // database (the join reads bigworldEntityTypes) and the
            // reference would misbehave there — recorded deviation.
            let name = defs
                .get(&type_id)
                .map(|def| def.name.as_str())
                .unwrap_or("<unknown>");
            out.push(format!(
                "ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity \
                 {name} with DBID={db_id}"
            ));
        }
    }
    if let Err(error) = db.clear_logons() {
        out.push(format!("{CAUGHT}: {error}"));
    }
}

/// `ClearAutoLoad::run` (`clear_auto_load.cpp:72-96`): inside one MySQL
/// transaction — spaces, space data, game times, then the auto-load
/// entity sweep, then commit with the INFO line. A failure in the first
/// three statements rolls back and fails the run; a failure inside the
/// sweep does not.
pub fn clear(
    db: &mut dyn ClearAutoLoadDatabase,
    defs: &HashMap<EntityTypeId, StreamTypeDef>,
    out: &mut Vec<String>,
) -> bool {
    let cleared = db
        .clear_spaces()
        .and_then(|_| db.clear_space_data())
        .and_then(|_| db.reset_game_times());
    if let Err(error) = cleared {
        out.push(format!("Failed to clear auto-load data: {error}"));
        return false;
    }
    delete_auto_load_entities(db, defs, out);
    out.push("Cleared auto-load data".to_owned());
    true
}

// -----------------------------------------------------------------------------
// Section: the tool flow (`main.cpp`)
// -----------------------------------------------------------------------------

/// `main` (`main.cpp:31-86`) with the BWResource/BWConfig preamble
/// absorbed: parse the options, gate on the tables, clear. Returns the
/// process exit code; log lines land in `out`.
pub fn run(
    args: &[String],
    db: &mut dyn ClearAutoLoadDatabase,
    defs: &HashMap<EntityTypeId, StreamTypeDef>,
    out: &mut Vec<String>,
) -> i32 {
    match parse_args(args) {
        ParseOutcome::Help => {
            out.push(USAGE_MESSAGE.to_owned());
            SUCCESS
        }
        ParseOutcome::Unrecognised(arg) => {
            out.push(format!("Unrecognised option: {arg}"));
            FAILURE
        }
        ParseOutcome::Run(_is_verbose) => {
            // DatabaseToolApp::init plus the mappings init — the verbose
            // flag only turns console logging on, which the port's log
            // list always is.
            if !check_tables_exist(db, out) {
                out.push("Failed to initialise".to_owned());
                return FAILURE;
            }
            if !clear(db, defs, out) {
                out.push("Failed to clear auto-load data".to_owned());
                return FAILURE;
            }
            SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use entitydef::value::PropertyValue;

    use crate::auto_load::UpdateAutoLoad;
    use crate::secondary::{StreamKind, StreamPropDef};
    use crate::storage::{EntityKey, EntityStorage, MemoryStorage, load_entity};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// An Avatar-shaped def on type 3.
    fn defs() -> HashMap<EntityTypeId, StreamTypeDef> {
        HashMap::from([(
            3,
            StreamTypeDef {
                name: "Avatar".to_owned(),
                type_id: 3,
                properties: vec![StreamPropDef {
                    name: "mass".to_owned(),
                    kind: StreamKind::Uint32,
                }],
            },
        )])
    }

    // -----------------------------------------------------------------
    // the command line
    // -----------------------------------------------------------------

    #[test]
    fn plain_and_verbose_scans_keep_going() {
        assert_eq!(parse_args(&[]), ParseOutcome::Run(false));
        assert_eq!(parse_args(&args(&[])), ParseOutcome::Run(false));
        assert_eq!(parse_args(&args(&["--verbose"])), ParseOutcome::Run(true));
        assert_eq!(parse_args(&args(&["-v"])), ParseOutcome::Run(true));
        assert!(parse_args(&args(&["--verbose"])).is_run());
    }

    #[test]
    fn help_prints_the_usage_and_exits_successfully() {
        for flag in ["--help", "-h"] {
            assert_eq!(parse_args(&args(&[flag])), ParseOutcome::Help);
            let mut out = Vec::new();
            let code = run(
                &args(&[flag]),
                &mut ScriptedDb::default(),
                &defs(),
                &mut out,
            );
            assert_eq!(code, SUCCESS);
            assert_eq!(out, vec![USAGE_MESSAGE.to_owned()]);
        }
    }

    #[test]
    fn an_unrecognised_option_exits_with_failure() {
        for stray in ["-x", "--bogus", "stray"] {
            let mut out = Vec::new();
            let code = run(
                &args(&[stray]),
                &mut ScriptedDb::default(),
                &defs(),
                &mut out,
            );
            assert_eq!(code, FAILURE, "{stray}");
            assert_eq!(out, vec![format!("Unrecognised option: {stray}")]);
        }
    }

    #[test]
    fn a_trailing_res_argument_is_rejected() {
        // `--res` with no following argument fails the reference's
        // `i < (argc - 1)` guard and lands in the rejection.
        let mut out = Vec::new();
        let code = run(
            &args(&["--res"]),
            &mut ScriptedDb::default(),
            &defs(),
            &mut out,
        );
        assert_eq!(code, FAILURE);
        assert_eq!(out, vec!["Unrecognised option: --res"]);

        let mut out = Vec::new();
        let code = run(
            &args(&["-r", "--verbose"]),
            &mut ScriptedDb::default(),
            &defs(),
            &mut out,
        );
        // `-r` swallows `--verbose` as its value; nothing rejects.
        assert_eq!(code, SUCCESS);
        assert_eq!(out, vec!["Cleared auto-load data".to_owned()]);
    }

    // -----------------------------------------------------------------
    // the tables gate
    // -----------------------------------------------------------------

    #[test]
    fn all_four_tables_pass_the_gate() {
        let mut db = ScriptedDb::default();
        let mut out = Vec::new();
        assert!(check_tables_exist(&mut db, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn a_missing_table_fails_the_gate() {
        let mut db = ScriptedDb {
            tables: Ok(vec![
                "bigworldLogOns".to_owned(),
                "bigworldSpaceData".to_owned(),
                "bigworldSpaces".to_owned(),
            ]),
            ..ScriptedDb::default()
        };
        let mut out = Vec::new();
        assert!(!check_tables_exist(&mut db, &mut out));
        assert_eq!(
            out,
            vec!["Missing a required table in the database, please run sync_db.".to_owned()]
        );

        // The main flow turns the gate failure into the init failure.
        let mut out = Vec::new();
        let code = run(&args(&[]), &mut db, &defs(), &mut out);
        assert_eq!(code, FAILURE);
        assert_eq!(out.last().map(String::as_str), Some("Failed to initialise"));
    }

    #[test]
    fn an_unenumerable_database_fails_the_gate() {
        let mut db = ScriptedDb {
            tables: Err("connection lost".to_owned()),
            ..ScriptedDb::default()
        };
        let mut out = Vec::new();
        assert!(!check_tables_exist(&mut db, &mut out));
        assert_eq!(
            out,
            vec!["Could not enumerate tables in database: connection lost".to_owned()]
        );
    }

    // -----------------------------------------------------------------
    // the clear run
    // -----------------------------------------------------------------

    #[test]
    fn a_clean_run_reports_the_cleared_line() {
        let mut db = ScriptedDb::default();
        let mut out = Vec::new();
        assert!(clear(&mut db, &defs(), &mut out));
        assert_eq!(out, vec!["Cleared auto-load data".to_owned()]);
        // Statement order: spaces, space data, game times, then the
        // auto-load sweep.
        assert_eq!(
            db.calls,
            [
                "spaces",
                "space_data",
                "game_times",
                "logons_join",
                "logons_clear"
            ]
        );
    }

    #[test]
    fn the_first_failing_statement_aborts_the_rest() {
        for failing in ["spaces", "space_data", "game_times"] {
            let mut db = ScriptedDb::default();
            match failing {
                "spaces" => db.spaces = Err("spaces exploded".to_owned()),
                "space_data" => db.space_data = Err("space data exploded".to_owned()),
                _ => db.game_times = Err("game times exploded".to_owned()),
            }
            let mut out = Vec::new();
            assert!(!clear(&mut db, &defs(), &mut out), "{failing}");
            let detail = match failing {
                "spaces" => "spaces exploded",
                "space_data" => "space data exploded",
                _ => "game times exploded",
            };
            assert_eq!(
                out,
                vec![format!("Failed to clear auto-load data: {detail}")]
            );
            // The sweep never ran behind the failed statement.
            assert!(!db.calls.contains(&"logons_join"), "{failing}");
        }
    }

    #[test]
    fn a_failed_delete_only_warns_and_the_run_still_succeeds() {
        let mut db = ScriptedDb {
            logons_join: Ok(vec![(3, 7), (3, 8)]),
            ..ScriptedDb::default()
        };
        db.delete_ok = false;
        let mut out = Vec::new();
        assert!(clear(&mut db, &defs(), &mut out));
        assert_eq!(
            out,
            vec![
                "ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity Avatar with DBID=7"
                    .to_owned(),
                "ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity Avatar with DBID=8"
                    .to_owned(),
                "Cleared auto-load data".to_owned(),
            ]
        );
        // The final logon clear still happened.
        assert_eq!(db.calls.last(), Some(&"logons_clear"));
    }

    #[test]
    fn a_failed_join_or_logon_clear_is_caught_inside_the_sweep() {
        // The reference's own try/catch around deleteAutoLoadEntities —
        // the typo'd message, verbatim — keeps the run alive.
        let caught = "ClearAutoLoad::deleteAutoLoadEnitites: An exception occurred while removing auto-loading entities from the database".to_owned();

        let mut db = ScriptedDb {
            logons_join: Err("no such table: bigworldLogOns".to_owned()),
            ..ScriptedDb::default()
        };
        let mut out = Vec::new();
        assert!(clear(&mut db, &defs(), &mut out));
        assert_eq!(
            out,
            vec![
                format!("{caught}: no such table: bigworldLogOns"),
                "Cleared auto-load data".to_owned(),
            ]
        );

        let mut db = ScriptedDb {
            logons_clear: Err("lock wait timeout".to_owned()),
            ..ScriptedDb::default()
        };
        let mut out = Vec::new();
        assert!(clear(&mut db, &defs(), &mut out));
        assert_eq!(
            out,
            vec![
                format!("{caught}: lock wait timeout"),
                "Cleared auto-load data".to_owned(),
            ]
        );
    }

    #[test]
    fn an_unknown_type_deletes_with_a_placeholder_name() {
        // Unreachable against a healthy database (the join reads
        // bigworldEntityTypes); the reference would misbehave — recorded.
        let mut db = ScriptedDb {
            logons_join: Ok(vec![(9, 4)]),
            ..ScriptedDb::default()
        };
        db.delete_ok = false;
        let mut out = Vec::new();
        assert!(clear(&mut db, &defs(), &mut out));
        assert_eq!(
            out[0],
            "ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity <unknown> with DBID=4"
        );
    }

    #[test]
    fn the_tool_flow_end_to_end_is_success_or_failure() {
        let mut db = ScriptedDb::default();
        let mut out = Vec::new();
        let code = run(&args(&[]), &mut db, &defs(), &mut out);
        assert_eq!(code, SUCCESS);
        assert_eq!(out, vec!["Cleared auto-load data".to_owned()]);

        db.game_times = Err("game times exploded".to_owned());
        let mut out = Vec::new();
        let code = run(&args(&["--verbose"]), &mut db, &defs(), &mut out);
        assert_eq!(code, FAILURE);
        assert_eq!(
            out,
            vec![
                "Failed to clear auto-load data: game times exploded".to_owned(),
                "Failed to clear auto-load data".to_owned(),
            ]
        );
    }

    // -----------------------------------------------------------------
    // a real-storage sample
    // -----------------------------------------------------------------

    /// An adapter over a real [`MemoryStorage`]: the entity deletes go
    /// through the actual backend, the rest of the legs are the memory
    /// model's fixed answers.
    struct SampleStorage<'a> {
        storage: &'a mut MemoryStorage,
        logons: Vec<(EntityTypeId, DatabaseId)>,
    }

    impl ClearAutoLoadDatabase for SampleStorage<'_> {
        fn table_names(&mut self) -> Result<Vec<String>, String> {
            Ok(REQUIRED_TABLES
                .iter()
                .map(|name| (*name).to_owned())
                .collect())
        }
        fn auto_load_logons(&mut self) -> Result<Vec<(EntityTypeId, DatabaseId)>, String> {
            Ok(self.logons.clone())
        }
        fn delete_with_id(&mut self, type_id: EntityTypeId, db_id: DatabaseId) -> bool {
            self.storage
                .del_entity(&EntityKey { type_id, db_id })
                .is_ok()
        }
        fn clear_spaces(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn clear_space_data(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn reset_game_times(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn clear_logons(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn the_tool_deletes_only_the_auto_load_entities_of_a_real_sample() {
        let mut storage = MemoryStorage::new();
        storage
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: 1,
                },
                &[PropertyValue::Uint(1)],
                true,
                UpdateAutoLoad::True,
            )
            .expect("the auto-load entity seeds");
        storage
            .put_entity(
                &EntityKey {
                    type_id: 3,
                    db_id: 2,
                },
                &[PropertyValue::Uint(2)],
                true,
                UpdateAutoLoad::Retain,
            )
            .expect("the plain entity seeds");

        let mut db = SampleStorage {
            storage: &mut storage,
            logons: vec![(3, 1)],
        };
        let mut out = Vec::new();
        let code = run(&args(&[]), &mut db, &defs(), &mut out);
        assert_eq!(code, SUCCESS);
        assert_eq!(out, vec!["Cleared auto-load data".to_owned()]);
        // The auto-load entity is gone; the plain one survives.
        assert!(
            load_entity(
                &storage,
                &EntityKey {
                    type_id: 3,
                    db_id: 1
                }
            )
            .is_err()
        );
        assert!(
            load_entity(
                &storage,
                &EntityKey {
                    type_id: 3,
                    db_id: 2
                }
            )
            .is_ok()
        );
    }

    // -----------------------------------------------------------------
    // the scripted database
    // -----------------------------------------------------------------

    /// A scripted [`ClearAutoLoadDatabase`] with call recording.
    struct ScriptedDb {
        tables: Result<Vec<String>, String>,
        logons_join: Result<Vec<(EntityTypeId, DatabaseId)>, String>,
        delete_ok: bool,
        spaces: Result<(), String>,
        space_data: Result<(), String>,
        game_times: Result<(), String>,
        logons_clear: Result<(), String>,
        calls: Vec<&'static str>,
    }

    impl Default for ScriptedDb {
        fn default() -> Self {
            ScriptedDb {
                tables: Ok(REQUIRED_TABLES
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect()),
                logons_join: Ok(Vec::new()),
                delete_ok: true,
                spaces: Ok(()),
                space_data: Ok(()),
                game_times: Ok(()),
                logons_clear: Ok(()),
                calls: Vec::new(),
            }
        }
    }

    impl ClearAutoLoadDatabase for ScriptedDb {
        fn table_names(&mut self) -> Result<Vec<String>, String> {
            self.calls.push("tables");
            self.tables.clone()
        }
        fn auto_load_logons(&mut self) -> Result<Vec<(EntityTypeId, DatabaseId)>, String> {
            self.calls.push("logons_join");
            self.logons_join.clone()
        }
        fn delete_with_id(&mut self, type_id: EntityTypeId, db_id: DatabaseId) -> bool {
            self.calls.push("delete");
            let _ = (type_id, db_id);
            self.delete_ok
        }
        fn clear_spaces(&mut self) -> Result<(), String> {
            self.calls.push("spaces");
            self.spaces.clone()
        }
        fn clear_space_data(&mut self) -> Result<(), String> {
            self.calls.push("space_data");
            self.space_data.clone()
        }
        fn reset_game_times(&mut self) -> Result<(), String> {
            self.calls.push("game_times");
            self.game_times.clone()
        }
        fn clear_logons(&mut self) -> Result<(), String> {
            self.calls.push("logons_clear");
            self.logons_clear.clone()
        }
    }
}
