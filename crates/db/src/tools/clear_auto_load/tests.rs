//! BW-09-05-06 acceptance: isolated doubles drive the same seams `main()`
//! uses; every operation and exit code is pinned against the reference
//! (main.cpp:33-88, clear_auto_load.cpp:41-190).

use super::*;

/// `init_tool` call records: "<app>|<script>|<verbose>|<lock>".
struct ToolAppDouble {
    result: bool,
    calls: Vec<String>,
}

impl Default for ToolAppDouble {
    fn default() -> Self {
        Self {
            result: true,
            calls: Vec::new(),
        }
    }
}

impl ToolAppFace for ToolAppDouble {
    fn init_tool(
        &mut self,
        app_name: &str,
        script_name: &str,
        is_verbose: bool,
        should_lock: bool,
    ) -> bool {
        self.calls.push(format!(
            "{}|{}|{}|{}",
            app_name, script_name, is_verbose, should_lock
        ));
        self.result
    }
}

struct MappingsDouble {
    init_result: bool,
    init_calls: usize,
    /// Popped front per delete_with_id; empty means Ok(true).
    delete_results: Vec<Result<bool, String>>,
    delete_calls: Vec<(u16, i64)>,
}

impl Default for MappingsDouble {
    fn default() -> Self {
        Self {
            init_result: true,
            init_calls: 0,
            delete_results: Vec::new(),
            delete_calls: Vec::new(),
        }
    }
}

impl EntityTypeMappingsFace for MappingsDouble {
    fn init_mappings(&mut self) -> bool {
        self.init_calls += 1;
        self.init_result
    }

    fn delete_with_id(&mut self, type_id: u16, database_id: i64) -> Result<bool, String> {
        self.delete_calls.push((type_id, database_id));
        match self.delete_results.first() {
            Some(result) => result.clone(),
            None => Ok(true),
        }
    }
}

#[derive(Default)]
struct EntityDefsDouble {
    names: Vec<(u16, String)>,
}

impl EntityDefsFace for EntityDefsDouble {
    fn entity_name(&self, type_id: u16) -> String {
        self.names
            .iter()
            .find(|(id, _)| *id == type_id)
            .map(|(_, name)| name.clone())
            .unwrap_or_default()
    }
}

/// Records the operation sequence; SQL strings listed in `fail_execute_on`
/// come back Err("boom").
struct DbDouble {
    ops: Vec<String>,
    fail_execute_on: Vec<String>,
    show_tables: Result<Vec<String>, String>,
    select: Result<Vec<(u16, i64)>, String>,
    commit_result: Result<(), String>,
}

impl Default for DbDouble {
    fn default() -> Self {
        Self {
            ops: Vec::new(),
            fail_execute_on: Vec::new(),
            show_tables: Ok(vec![
                "bigworldLogOns".to_string(),
                "bigworldSpaceData".to_string(),
                "bigworldSpaces".to_string(),
                "bigworldGameTime".to_string(),
            ]),
            select: Ok(Vec::new()),
            commit_result: Ok(()),
        }
    }
}

impl ClearAutoLoadDb for DbDouble {
    fn begin(&mut self) {
        self.ops.push("begin".to_string());
    }

    fn execute(&mut self, sql: &str) -> Result<(), String> {
        self.ops.push(format!("execute:{}", sql));
        if self.fail_execute_on.iter().any(|item| item == sql) {
            Err("boom".to_string())
        } else {
            Ok(())
        }
    }

    fn show_tables(&mut self) -> Result<Vec<String>, String> {
        self.ops.push("execute:SHOW TABLES".to_string());
        self.show_tables.clone()
    }

    fn select_auto_load_entities(&mut self) -> Result<Vec<(u16, i64)>, String> {
        self.ops.push("execute:SELECT_AUTO_LOAD".to_string());
        self.select.clone()
    }

    fn query(&mut self, sql: &str) {
        self.ops.push(format!("query:{}", sql));
    }

    fn commit(&mut self) -> Result<(), String> {
        self.ops.push("commit".to_string());
        self.commit_result.clone()
    }
}

struct Rig {
    tool_app: ToolAppDouble,
    db: DbDouble,
    mappings: MappingsDouble,
    entity_defs: EntityDefsDouble,
    log: Vec<String>,
}

fn make_rig() -> Rig {
    Rig {
        tool_app: ToolAppDouble::default(),
        db: DbDouble::default(),
        mappings: MappingsDouble::default(),
        entity_defs: EntityDefsDouble::default(),
        log: Vec::new(),
    }
}

fn faces_of(rig: &mut Rig) -> Faces<'_> {
    Faces {
        tool_app: &mut rig.tool_app,
        db: &mut rig.db,
        mappings: &mut rig.mappings,
        entity_defs: &rig.entity_defs,
        log: &mut rig.log,
    }
}

fn args(items: &[&str]) -> Vec<String> {
    std::iter::once("clear_auto_load".to_string())
        .chain(items.iter().map(|item| item.to_string()))
        .collect()
}

/// The happy operation sequence, in reference order: checkTablesExist
/// (SHOW TABLES) then run()'s transaction.
fn happy_ops() -> Vec<String> {
    vec![
        "execute:SHOW TABLES".to_string(),
        "begin".to_string(),
        "execute:DELETE FROM bigworldSpaces".to_string(),
        "execute:DELETE FROM bigworldSpaceData".to_string(),
        "execute:UPDATE bigworldGameTime SET time = 0".to_string(),
        "execute:SELECT_AUTO_LOAD".to_string(),
        "query:DELETE FROM bigworldLogOns".to_string(),
        "commit".to_string(),
    ]
}

#[test]
fn help_short_exits_success_with_usage() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["-h"]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(rig.log, vec![USAGE_MESSAGE.to_string()]);
    assert!(rig.tool_app.calls.is_empty());
    assert!(rig.db.ops.is_empty());
}

#[test]
fn help_long_exits_success_with_usage() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["--help"]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(rig.log, vec![USAGE_MESSAGE.to_string()]);
    assert!(rig.tool_app.calls.is_empty());
}

#[test]
fn verbose_short_reaches_tool_app_true() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["-v"]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        rig.tool_app.calls,
        vec!["ClearAutoLoad|clear_auto_load|true|true".to_string()]
    );
    assert_eq!(rig.db.ops, happy_ops());
    assert_eq!(rig.log, vec!["Cleared auto-load data\n".to_string()]);
}

#[test]
fn verbose_long_reaches_tool_app_true() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["--verbose"]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        rig.tool_app.calls,
        vec!["ClearAutoLoad|clear_auto_load|true|true".to_string()]
    );
}

#[test]
fn not_verbose_reaches_tool_app_false() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        rig.tool_app.calls,
        vec!["ClearAutoLoad|clear_auto_load|false|true".to_string()]
    );
}

#[test]
fn res_with_value_is_swallowed() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["--res", "/game/res"]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(rig.log, vec!["Cleared auto-load data\n".to_string()]);
}

#[test]
fn res_short_with_value_is_swallowed() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["-r", "/game/res"]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(rig.log, vec!["Cleared auto-load data\n".to_string()]);
}

#[test]
fn trailing_res_is_unrecognised() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["--res"]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(rig.log, vec!["Unrecognised option: --res\n".to_string()]);
    assert!(rig.tool_app.calls.is_empty());
}

#[test]
fn trailing_res_short_is_unrecognised() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["-r"]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(rig.log, vec!["Unrecognised option: -r\n".to_string()]);
}

#[test]
fn unknown_long_option_exits_failure() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["--bogus"]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(rig.log, vec!["Unrecognised option: --bogus\n".to_string()]);
    assert!(rig.tool_app.calls.is_empty());
}

#[test]
fn unknown_short_option_exits_failure() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&["-x"]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(rig.log, vec!["Unrecognised option: -x\n".to_string()]);
}

#[test]
fn unknown_option_after_swallowed_res_still_exits_failure() {
    let mut rig = make_rig();
    let code = main_face(
        &mut faces_of(&mut rig),
        &args(&["--res", "/game/res", "--bogus"]),
    );
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(rig.log, vec!["Unrecognised option: --bogus\n".to_string()]);
}

#[test]
fn tool_app_failure_prints_failed_to_initialise() {
    let mut rig = make_rig();
    rig.tool_app.result = false;
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(rig.log, vec!["Failed to initialise\n".to_string()]);
    assert_eq!(rig.mappings.init_calls, 0);
}

#[test]
fn mappings_failure_message_has_no_trailing_newline() {
    let mut rig = make_rig();
    rig.mappings.init_result = false;
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_FAILURE);
    // The app's message (no newline) then main()'s own ERROR_MSG.
    assert_eq!(
        rig.log,
        vec![
            "Failed to initialise Entity Type Mappings".to_string(),
            "Failed to initialise\n".to_string()
        ]
    );
    // checkTablesExist never ran.
    assert!(rig.db.ops.is_empty());
}

#[test]
fn missing_table_exits_failure_before_run() {
    let mut rig = make_rig();
    rig.db.show_tables = Ok(vec![
        "bigworldSpaces".to_string(),
        "bigworldGameTime".to_string(),
    ]);
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(
        rig.log,
        vec![
            "Missing a required table in the database, please run sync_db.\n".to_string(),
            "Failed to initialise\n".to_string()
        ]
    );
    assert_eq!(rig.db.ops, vec!["execute:SHOW TABLES".to_string()]);
}

#[test]
fn duplicate_table_rows_mask_a_missing_table() {
    // Counting quirk: each matching row decrements, so two bigworldSpaces
    // rows plus the other two requisites drive `left` to zero even though
    // bigworldLogOns never appeared.
    let mut rig = make_rig();
    rig.db.show_tables = Ok(vec![
        "bigworldSpaces".to_string(),
        "bigworldSpaces".to_string(),
        "bigworldSpaceData".to_string(),
        "bigworldGameTime".to_string(),
    ]);
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(rig.log, vec!["Cleared auto-load data\n".to_string()]);
}

#[test]
fn extra_tables_are_ignored() {
    let mut rig = make_rig();
    let mut tables = vec![
        "bigworldLogOns".to_string(),
        "bigworldSpaceData".to_string(),
        "bigworldSpaces".to_string(),
        "bigworldGameTime".to_string(),
        "bigworldNewTable".to_string(),
    ];
    tables.reverse();
    rig.db.show_tables = Ok(tables);
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
}

#[test]
fn show_tables_throw_reports_enumeration_failure() {
    let mut rig = make_rig();
    rig.db.show_tables = Err("boom".to_string());
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(
        rig.log,
        vec![
            "Could not enumerate tables in database: boom\n".to_string(),
            "Failed to initialise\n".to_string()
        ]
    );
}

#[test]
fn run_happy_pins_operation_order_and_log() {
    let mut rig = make_rig();
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(rig.db.ops, happy_ops());
    assert_eq!(rig.log, vec!["Cleared auto-load data\n".to_string()]);
}

#[test]
fn first_statement_throw_aborts_transaction() {
    let mut rig = make_rig();
    rig.db.fail_execute_on = vec!["DELETE FROM bigworldSpaces".to_string()];
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(
        rig.log,
        vec![
            "Failed to clear auto-load data: boom\n".to_string(),
            "Failed to clear auto-load data\n".to_string()
        ]
    );
    assert_eq!(
        rig.db.ops,
        vec![
            "execute:SHOW TABLES".to_string(),
            "begin".to_string(),
            "execute:DELETE FROM bigworldSpaces".to_string()
        ]
    );
}

#[test]
fn third_statement_throw_skips_delete_and_commit() {
    let mut rig = make_rig();
    rig.db.fail_execute_on = vec!["UPDATE bigworldGameTime SET time = 0".to_string()];
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(
        rig.log,
        vec![
            "Failed to clear auto-load data: boom\n".to_string(),
            "Failed to clear auto-load data\n".to_string()
        ]
    );
    assert_eq!(
        rig.db.ops,
        vec![
            "execute:SHOW TABLES".to_string(),
            "begin".to_string(),
            "execute:DELETE FROM bigworldSpaces".to_string(),
            "execute:DELETE FROM bigworldSpaceData".to_string(),
            "execute:UPDATE bigworldGameTime SET time = 0".to_string()
        ]
    );
}

#[test]
fn commit_throw_still_ran_delete_auto_load_entities() {
    let mut rig = make_rig();
    rig.db.commit_result = Err("boom".to_string());
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_FAILURE);
    assert_eq!(
        rig.log,
        vec![
            "Failed to clear auto-load data: boom\n".to_string(),
            "Failed to clear auto-load data\n".to_string()
        ]
    );
    let last = rig.db.ops.last().unwrap().clone();
    assert_eq!(last, "commit");
    assert!(rig
        .db
        .ops
        .contains(&"query:DELETE FROM bigworldLogOns".to_string()));
}

#[test]
fn delete_loop_deletes_each_row_in_order() {
    let mut rig = make_rig();
    rig.db.select = Ok(vec![(3, 42), (7, 99)]);
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(rig.mappings.delete_calls, vec![(3, 42), (7, 99)]);
    assert_eq!(rig.log, vec!["Cleared auto-load data\n".to_string()]);
}

#[test]
fn delete_false_warns_with_entity_name_and_dbid() {
    let mut rig = make_rig();
    rig.db.select = Ok(vec![(3, 42)]);
    rig.entity_defs.names = vec![(3, "Npc".to_string())];
    rig.mappings.delete_results = vec![Ok(false)];
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        rig.log,
        vec![
            "ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity Npc with DBID=42\n"
                .to_string(),
            "Cleared auto-load data\n".to_string()
        ]
    );
    // The loop continues and the log-ons query still runs.
    assert!(rig
        .db
        .ops
        .contains(&"query:DELETE FROM bigworldLogOns".to_string()));
}

#[test]
fn dbid_is_signed_decimal_in_warning() {
    // DatabaseID is int64 and FMT_DBID is "lld" — negative DBIDs print with
    // a minus sign.
    let mut rig = make_rig();
    rig.db.select = Ok(vec![(5, -5)]);
    rig.entity_defs.names = vec![(5, "Avatar".to_string())];
    rig.mappings.delete_results = vec![Ok(false)];
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        rig.log[0],
        "ClearAutoLoad::deleteAutoLoadEntities: Could not delete entity Avatar with DBID=-5\n"
            .to_string()
    );
}

#[test]
fn delete_throw_swallowed_and_transaction_still_commits() {
    let mut rig = make_rig();
    rig.db.select = Ok(vec![(3, 42)]);
    rig.mappings.delete_results = vec![Err("boom".to_string())];
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    // Verbatim "Enitites" typo, then the success message — commit ran.
    assert_eq!(
        rig.log,
        vec![
            "ClearAutoLoad::deleteAutoLoadEnitites: An exception occurred while removing auto-loading entities from the database: boom\n".to_string(),
            "Cleared auto-load data\n".to_string()
        ]
    );
    // Loop aborted: the trailing query was skipped but commit ran.
    assert!(!rig
        .db
        .ops
        .contains(&"query:DELETE FROM bigworldLogOns".to_string()));
    assert_eq!(rig.db.ops.last(), Some(&"commit".to_string()));
}

#[test]
fn select_throw_swallowed_no_deletes_no_query() {
    let mut rig = make_rig();
    rig.db.select = Err("boom".to_string());
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert_eq!(
        rig.log[0],
        "ClearAutoLoad::deleteAutoLoadEnitites: An exception occurred while removing auto-loading entities from the database: boom\n".to_string()
    );
    assert!(rig.mappings.delete_calls.is_empty());
    assert!(!rig
        .db
        .ops
        .contains(&"query:DELETE FROM bigworldLogOns".to_string()));
    assert_eq!(rig.log[1], "Cleared auto-load data\n".to_string());
}

#[test]
fn empty_result_set_still_queries_log_ons() {
    let mut rig = make_rig();
    rig.db.select = Ok(vec![]);
    let code = main_face(&mut faces_of(&mut rig), &args(&[]));
    assert_eq!(code, EXIT_SUCCESS);
    assert!(rig.mappings.delete_calls.is_empty());
    assert_eq!(rig.db.ops, happy_ops());
}
