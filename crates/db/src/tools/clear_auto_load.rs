//! Port of `server/tools/clear_auto_load/` (main.cpp:33-88 +
//! clear_auto_load.cpp:41-190): the offline tool that empties a primary
//! database of its auto-load entities and world state.
//!
//! Verbatim quirks kept:
//! - `--res`/`-r` swallow the next argument only when one exists
//!   (`i < argc - 1`); a trailing one falls through the whole else-if chain
//!   and exits 1 with "Unrecognised option: --res\n" (printf to stdout in
//!   the reference — logged here, sink deviation).
//! - `--help`/`-h` prints `USAGE_MESSAGE` (puts to stdout) and returns
//!   `EXIT_SUCCESS` immediately.
//! - "Failed to initialise Entity Type Mappings" has no trailing newline.
//! - checkTablesExist counts matching rows down from 4, so duplicate rows
//!   of one table mask a missing one (counting quirk, pinned by tests).
//! - run(): the three statements and the commit share the outer try
//!   ("Failed to clear auto-load data: %s\n"); deleteAutoLoadEntities
//!   carries its own catch with the verbatim "deleteAutoLoadEnitites" typo
//!   and swallows the exception — the transaction still commits and run()
//!   still succeeds.
//! - The trailing "DELETE FROM bigworldLogOns" goes through MySql::query
//!   (the non-throwing variant), so its failure reaches neither catch.

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;

pub const USAGE_MESSAGE: &str = "Usage: clear_auto_load [options]\n\nOptions:\n --verbose | -v      Display verbose program output to the console.\n --help | -h         Program usage.\n";

/// `NUM_REQUISITE_TABLES` (clear_auto_load.cpp:104).
const NUM_REQUISITE_TABLES: i32 = 4;

/// `DatabaseToolApp::init` (database_tool_app.cpp): the
/// logger/script/entity-defs/connect bootstrap. A false here is silent —
/// the reference logs only inside its logger leg, which stays unmodelled.
pub trait ToolAppFace {
    fn init_tool(
        &mut self,
        app_name: &str,
        script_name: &str,
        is_verbose: bool,
        should_lock: bool,
    ) -> bool;
}

/// `EntityTypeMappings` (entity_type_mappings.hpp:32): `operator[]` is an
/// unchecked vector index, so the port folds the lookup into the delete
/// call; `deleteWithID` is entity_type_mapping.cpp:501 — Ok(false) is the
/// WARNING path, Err the exception text for the inner catch.
pub trait EntityTypeMappingsFace {
    /// EntityTypeMappings::init(entityDefs, connection).
    fn init_mappings(&mut self) -> bool;
    fn delete_with_id(&mut self, type_id: u16, database_id: i64) -> Result<bool, String>;
}

/// `entityDefs.getEntityDescription(typeID).name()` — a bogus typeID asserts
/// in the reference (outside the inner catch); the port keeps the lookup
/// infallible.
pub trait EntityDefsFace {
    fn entity_name(&self, type_id: u16) -> String;
}

/// The `MySql` connection as ClearAutoLoad sees it. Every Err is the
/// exception text caught at the matching try.
pub trait ClearAutoLoadDb {
    /// MySqlTransaction construction.
    fn begin(&mut self);
    /// connection.execute(sql) — no result set.
    fn execute(&mut self, sql: &str) -> Result<(), String>;
    /// connection.execute(sql, &resultSet) with a one-field getResult loop.
    fn show_tables(&mut self) -> Result<Vec<String>, String>;
    /// connection.execute(sql, &resultSet) with a two-field getResult loop.
    fn select_auto_load_entities(&mut self) -> Result<Vec<(u16, i64)>, String>;
    /// connection.query(sql) — the non-throwing variant.
    fn query(&mut self, sql: &str);
    /// MySqlTransaction::commit().
    fn commit(&mut self) -> Result<(), String>;
}

/// Per-call faces for the tool.
pub struct Faces<'a> {
    pub tool_app: &'a mut dyn ToolAppFace,
    pub db: &'a mut dyn ClearAutoLoadDb,
    pub mappings: &'a mut dyn EntityTypeMappingsFace,
    pub entity_defs: &'a dyn EntityDefsFace,
    /// The debug-message sink (ERROR_MSG/INFO_MSG/WARNING_MSG text verbatim,
    /// newlines included).
    pub log: &'a mut Vec<String>,
}

/// ClearAutoLoad (clear_auto_load.cpp:41).
pub struct ClearAutoLoad;

impl Default for ClearAutoLoad {
    fn default() -> Self {
        Self::new()
    }
}

impl ClearAutoLoad {
    pub fn new() -> Self {
        Self
    }

    /// ClearAutoLoad::init (clear_auto_load.cpp:47-61).
    pub fn init(&mut self, faces: &mut Faces, is_verbose: bool) -> bool {
        if !faces.tool_app.init_tool(
            "ClearAutoLoad",
            "clear_auto_load",
            is_verbose,
            /*shouldLock=*/ true,
        ) {
            return false;
        }

        if !faces.mappings.init_mappings() {
            // Verbatim: no trailing newline.
            faces
                .log
                .push("Failed to initialise Entity Type Mappings".to_string());
            return false;
        }

        self.check_tables_exist(faces)
    }

    /// ClearAutoLoad::run (clear_auto_load.cpp:75-95): one outer try around
    /// the transaction; deleteAutoLoadEntities swallows its own exceptions,
    /// so nothing escapes it into this catch.
    pub fn run(&mut self, faces: &mut Faces) -> bool {
        let outcome = (|| -> Result<(), String> {
            faces.db.begin();
            faces.db.execute("DELETE FROM bigworldSpaces")?;
            faces.db.execute("DELETE FROM bigworldSpaceData")?;
            faces.db.execute("UPDATE bigworldGameTime SET time = 0")?;
            self.delete_auto_load_entities(faces);
            faces.db.commit()?;
            Ok(())
        })();

        match outcome {
            Ok(()) => {
                faces.log.push("Cleared auto-load data\n".to_string());
                true
            }
            Err(e) => {
                faces
                    .log
                    .push(format!("Failed to clear auto-load data: {}\n", e));
                false
            }
        }
    }

    /// ClearAutoLoad::checkTablesExist (clear_auto_load.cpp:99-141): counts
    /// matching rows down from NUM_REQUISITE_TABLES.
    fn check_tables_exist(&mut self, faces: &mut Faces) -> bool {
        let tables = match faces.db.show_tables() {
            Ok(tables) => tables,
            Err(e) => {
                faces
                    .log
                    .push(format!("Could not enumerate tables in database: {}\n", e));
                return false;
            }
        };

        let mut left = NUM_REQUISITE_TABLES;
        for table in &tables {
            if table == "bigworldLogOns"
                || table == "bigworldSpaceData"
                || table == "bigworldSpaces"
                || table == "bigworldGameTime"
            {
                left -= 1;
            }
        }

        if left > 0 {
            faces.log.push(
                "Missing a required table in the database, please run sync_db.\n".to_string(),
            );
            return false;
        }

        true
    }

    /// ClearAutoLoad::deleteAutoLoadEntities (clear_auto_load.cpp:146-185):
    /// self-contained try. A throw aborts the row loop and skips the
    /// trailing "DELETE FROM bigworldLogOns" query, but is swallowed with
    /// the verbatim "Enitites" typo — the caller still commits.
    fn delete_auto_load_entities(&mut self, faces: &mut Faces) {
        let outcome = (|| -> Result<(), String> {
            let rows = faces.db.select_auto_load_entities()?;

            for (entity_type_id, database_id) in rows {
                // Reference order: mapping lookup, entity description, then
                // the delete; the name lookup is infallible here.
                let entity_name = faces.entity_defs.entity_name(entity_type_id);
                let deleted = faces.mappings.delete_with_id(entity_type_id, database_id)?;

                if !deleted {
                    faces.log.push(format!(
                        "ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity {} with DBID={}\n",
                        entity_name, database_id
                    ));
                }
            }

            faces.db.query("DELETE FROM bigworldLogOns");
            Ok(())
        })();

        if let Err(e) = outcome {
            faces.log.push(format!(
                "ClearAutoLoad::deleteAutoLoadEnitites: An exception occurred while removing auto-loading entities from the database: {}\n",
                e
            ));
        }
    }
}

/// `main` (main.cpp:33-88). `args` includes argv[0]; parsing starts at 1.
pub fn main_face(faces: &mut Faces, args: &[String]) -> i32 {
    let mut is_verbose = false;

    let mut i = 1usize;
    while i < args.len() {
        let arg = args[i].as_str();

        // A --res/-r is skipped together with its value, but only when a
        // value exists; a trailing one falls through to the else branch.
        if (arg == "--res" || arg == "-r") && i < args.len() - 1 {
            i += 1;
        } else if arg == "--verbose" || arg == "-v" {
            is_verbose = true;
        } else if arg == "--help" || arg == "-h" {
            // puts(USAGE_MESSAGE) — stdout sink in the reference.
            faces.log.push(USAGE_MESSAGE.to_string());
            return EXIT_SUCCESS;
        } else {
            // printf to stdout in the reference.
            faces.log.push(format!("Unrecognised option: {}\n", arg));
            return EXIT_FAILURE;
        }

        i += 1;
    }

    // DebugFilter::shouldWriteToConsole / BWResource / BWConfig inits are
    // environmental and not modelled.

    let mut app = ClearAutoLoad::new();

    if !app.init(faces, is_verbose) {
        faces.log.push("Failed to initialise\n".to_string());
        return EXIT_FAILURE;
    }

    // START_MSG is console decoration, not in the log face.

    if !app.run(faces) {
        faces
            .log
            .push("Failed to clear auto-load data\n".to_string());
        return EXIT_FAILURE;
    }

    EXIT_SUCCESS
}

#[cfg(test)]
mod tests;
