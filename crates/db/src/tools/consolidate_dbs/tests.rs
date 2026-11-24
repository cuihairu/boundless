//! BW-09-05-05 consolidate_dbs tests: recording doubles pin the reference's
//! target data (getopt quirks, verbatim messages, exit codes, reporter
//! strings) against the driver layer.

use super::*;

/// A valid uppercase-hex quoted digest.
const VALID_DIGEST: &str = "00112233445566778899AABBCCDDEEFF";
/// A different valid digest, for mismatches.
const OTHER_DIGEST: &str = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
/// The reference's long defs-mismatch message.
const DEFS_MISMATCH_MESSAGE: &str = "ConsolidateDBsApp::init: Our entity definitions do not match the ones used by the primary database\nDatabase consolidation should be run before making changes to entity definitions. Changing entity definitions potentially invalidates unconsolidated data.\nRun \"consolidate_dbs --clear\" to allow the server to run without doing data consolidation. Unconsolidated data will be lost.\n";
/// The parser's positional error, before the usage dump.
const MISSING_LIST_MESSAGE: &str =
    "Primary database specified, but no path to secondary database list was specified\n";

fn args(items: &[&str]) -> Vec<String> {
    std::iter::once("consolidate_dbs".to_string())
        .chain(items.iter().map(|item| item.to_string()))
        .collect()
}

fn test_digest() -> Digest {
    Digest::unquote(VALID_DIGEST).unwrap()
}

// ---- test doubles ----

struct ConfigDouble {
    is_bad: bool,
    info: ConnectionInfo,
    num_connections: u32,
    directory: String,
    directory_setting_path: String,
    resolve_ok: bool,
}

impl Default for ConfigDouble {
    fn default() -> Self {
        ConfigDouble {
            is_bad: false,
            info: ConnectionInfo {
                host: "primary".to_string(),
                port: 3306,
                username: "bw".to_string(),
                password: "pw".to_string(),
                database: "bigworld".to_string(),
            },
            num_connections: 2,
            directory: "/tmp/consolidated".to_string(),
            directory_setting_path: "/bigworld/secondaryDB/consolidation/directory".to_string(),
            resolve_ok: true,
        }
    }
}

impl ConsolidateConfig for ConfigDouble {
    fn bw_config_is_bad(&self) -> bool {
        self.is_bad
    }

    fn default_connection_info(&self) -> ConnectionInfo {
        self.info.clone()
    }

    fn num_connections(&self) -> u32 {
        self.num_connections
    }

    fn consolidation_directory(&self) -> String {
        self.directory.clone()
    }

    fn consolidation_directory_setting_path(&self) -> String {
        self.directory_setting_path.clone()
    }

    fn resolve_is_directory(&mut self, _path: &str) -> bool {
        self.resolve_ok
    }
}

struct ToolAppDouble {
    init_ok: bool,
    ip: u32,
    inits: Vec<(String, String, bool, bool, ConnectionInfo)>,
    signals: Vec<i32>,
}

impl Default for ToolAppDouble {
    fn default() -> Self {
        ToolAppDouble {
            init_ok: true,
            ip: 0x0A00_0009,
            inits: Vec::new(),
            signals: Vec::new(),
        }
    }
}

impl ToolAppFace for ToolAppDouble {
    fn init_tool(
        &mut self,
        app_name: &str,
        mechanism: &str,
        is_verbose: bool,
        should_lock: bool,
        info: &ConnectionInfo,
    ) -> bool {
        self.inits.push((
            app_name.to_string(),
            mechanism.to_string(),
            is_verbose,
            should_lock,
            info.clone(),
        ));
        self.init_ok
    }

    fn enable_signal_handler(&mut self, sig_num: i32) {
        self.signals.push(sig_num);
    }

    fn local_ip(&self) -> u32 {
        self.ip
    }
}

struct PrimaryDbDouble {
    secondary_rows: Result<Vec<(u32, String)>, String>,
    checksum: Result<Option<String>, String>,
    delete_result: Result<u32, String>,
    secondary_calls: usize,
    checksum_calls: usize,
    delete_calls: usize,
}

impl Default for PrimaryDbDouble {
    fn default() -> Self {
        PrimaryDbDouble {
            secondary_rows: Ok(Vec::new()),
            checksum: Ok(None),
            delete_result: Ok(0),
            secondary_calls: 0,
            checksum_calls: 0,
            delete_calls: 0,
        }
    }
}

impl PrimaryDatabase for PrimaryDbDouble {
    fn select_secondary_databases(&mut self) -> Result<Vec<(u32, String)>, String> {
        self.secondary_calls += 1;
        self.secondary_rows.clone()
    }

    fn select_entity_defs_checksum(&mut self) -> Result<Option<String>, String> {
        self.checksum_calls += 1;
        self.checksum.clone()
    }

    fn delete_secondary_databases(&mut self) -> Result<u32, String> {
        self.delete_calls += 1;
        self.delete_result.clone()
    }
}

struct EntityDefsDouble {
    digest: Digest,
}

impl Default for EntityDefsDouble {
    fn default() -> Self {
        EntityDefsDouble {
            digest: test_digest(),
        }
    }
}

impl EntityDefs for EntityDefsDouble {
    fn persistent_properties_digest(&self) -> Digest {
        self.digest
    }
}

struct DbAppDouble {
    init_ok: bool,
    init_calls: usize,
    statuses: Vec<String>,
}

impl Default for DbAppDouble {
    fn default() -> Self {
        DbAppDouble {
            init_ok: true,
            init_calls: 0,
            statuses: Vec::new(),
        }
    }
}

impl DbAppFace for DbAppDouble {
    fn init(&mut self) -> bool {
        self.init_calls += 1;
        self.init_ok
    }

    fn set_status(&mut self, status: &str) {
        self.statuses.push(status.to_string());
    }
}

struct TransferDouble {
    listen_ok: bool,
    bound_addr: String,
    request_ok: bool,
    wait_ok: bool,
    drive_reporter: bool,
    received: Vec<String>,
    listens: Vec<(u32, usize)>,
    requests: Vec<(u32, String)>,
    wait_calls: usize,
    cleanups: usize,
}

impl Default for TransferDouble {
    fn default() -> Self {
        TransferDouble {
            listen_ok: true,
            bound_addr: "10.0.0.9:3000".to_string(),
            request_ok: true,
            wait_ok: true,
            drive_reporter: false,
            received: Vec::new(),
            listens: Vec::new(),
            requests: Vec::new(),
            wait_calls: 0,
            cleanups: 0,
        }
    }
}

impl TransferLeg for TransferDouble {
    fn listen(&mut self, internal_ip: u32, count: usize) -> Option<String> {
        self.listens.push((internal_ip, count));
        if self.listen_ok {
            Some(self.bound_addr.clone())
        } else {
            None
        }
    }

    fn request_transfer(&mut self, host_ip: u32, location: &str) -> bool {
        self.requests.push((host_ip, location.to_string()));
        self.request_ok
    }

    fn wait_until_done(&mut self, progress: &mut FileTransferProgressReporter<'_, '_>) -> bool {
        self.wait_calls += 1;
        if self.drive_reporter {
            progress.on_start_transfer(100);
            progress.on_receive_data(50);
            progress.on_finish_transfer();
        }
        self.wait_ok
    }

    fn received_file_paths(&self) -> Vec<String> {
        self.received.clone()
    }

    fn clean_up_remote_files(&mut self, _errors: &DBConsolidatorErrors) {
        self.cleanups += 1;
    }
}

struct MergeDouble {
    queue_ok: bool,
    init_ok: bool,
    digest: Option<String>,
    consolidate_ok: bool,
    start_db_report: bool,
    abort_to_set: Option<bool>,
    queue_inits: Vec<ConnectionInfo>,
    inits: Vec<String>,
    digest_queries: usize,
    consolidations: Vec<(String, bool, bool)>,
}

impl Default for MergeDouble {
    fn default() -> Self {
        MergeDouble {
            queue_ok: true,
            init_ok: true,
            digest: Some(VALID_DIGEST.to_string()),
            consolidate_ok: true,
            start_db_report: false,
            abort_to_set: None,
            queue_inits: Vec::new(),
            inits: Vec::new(),
            digest_queries: 0,
            consolidations: Vec::new(),
        }
    }
}

impl MergeLeg for MergeDouble {
    fn init_queue(&mut self, info: &ConnectionInfo) -> bool {
        self.queue_inits.push(info.clone());
        self.queue_ok
    }

    fn init_secondary_db(&mut self, path: &str) -> bool {
        self.inits.push(path.to_string());
        self.init_ok
    }

    fn secondary_db_digest(&mut self) -> Option<String> {
        self.digest_queries += 1;
        self.digest.clone()
    }

    fn consolidate(
        &mut self,
        path: &str,
        progress: &mut ConsolidationProgressReporter<'_, '_>,
        ignore_sqlite_errors: bool,
        should_abort: &mut bool,
    ) -> bool {
        self.consolidations
            .push((path.to_string(), ignore_sqlite_errors, *should_abort));
        if self.start_db_report {
            progress.on_start_consolidate_db(path, 5);
        }
        if let Some(value) = self.abort_to_set {
            *should_abort = value;
        }
        self.consolidate_ok
    }
}

#[derive(Default)]
struct ListReaderDouble {
    content: Option<Vec<String>>,
}

impl ListFileReader for ListReaderDouble {
    fn read_lines(&mut self, _path: &str) -> Option<Vec<String>> {
        self.content.clone()
    }
}

struct ClockDouble {
    now: std::cell::Cell<u64>,
    sps: u64,
}

impl ClockFace for ClockDouble {
    fn timestamp(&self) -> u64 {
        self.now.get()
    }

    fn stamps_per_second(&self) -> u64 {
        self.sps
    }
}

/// A reporter sink readable while the reporter holds it (shared handle).
#[derive(Clone, Default)]
struct SinkReporter {
    statuses: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl SinkReporter {
    fn recorded(&self) -> Vec<String> {
        self.statuses.borrow().clone()
    }
}

impl StatusReporter for SinkReporter {
    fn on_status(&mut self, status: &str) {
        self.statuses.borrow_mut().push(status.to_string());
    }
}

/// The shared fixture: defaults configured for a happy run. The message log
/// stays outside (the faces bundle's borrows cover the whole rig).
struct Rig {
    config: ConfigDouble,
    tool: ToolAppDouble,
    primary: PrimaryDbDouble,
    defs: EntityDefsDouble,
    transfer: TransferDouble,
    merge: MergeDouble,
    reader: ListReaderDouble,
    clock: ClockDouble,
    db_app: Option<DbAppDouble>,
}

fn make_rig() -> Rig {
    Rig {
        config: ConfigDouble::default(),
        tool: ToolAppDouble::default(),
        primary: PrimaryDbDouble::default(),
        defs: EntityDefsDouble::default(),
        transfer: TransferDouble::default(),
        merge: MergeDouble::default(),
        reader: ListReaderDouble::default(),
        clock: ClockDouble {
            now: std::cell::Cell::new(1000),
            sps: 1000,
        },
        db_app: None,
    }
}

fn faces_of(rig: &mut Rig) -> Faces<'_> {
    let db_app = rig
        .db_app
        .as_mut()
        .map(|db_app| db_app as &mut dyn DbAppFace);
    Faces {
        config: &mut rig.config,
        tool_app: &mut rig.tool,
        primary_db: &mut rig.primary,
        entity_defs: &mut rig.defs,
        db_app,
        transfer: &mut rig.transfer,
        merge: &mut rig.merge,
        list_reader: &mut rig.reader,
        clock: &rig.clock,
    }
}

// ---- constants ----

#[test]
fn constants_pin_reference_strings() {
    assert_eq!(EXIT_SUCCESS, 0);
    assert_eq!(EXIT_FAILURE, 1);
    assert_eq!(SIGINT, 2);
    assert_eq!(SIGHUP, 1);
    assert_eq!(USAGE_MESSAGE, "\nUsage: consolidate_dbs [options]\n       consolidate_dbs [options] <primarydb_spec> <secondarydb_list> (when invoked from DBApp)\n\nOptions:\n --ignore-sqlite-errors    do not abort on sqlite read errors\n --clear                   clear unconsolidated secondary database info\n --list                    list unconsolidated secondary database info\n -v, --verbose             verbose output\n -h, --help                print usage\n");
    assert_eq!(
        SELECT_SECONDARY_DATABASES,
        "SELECT ip, location FROM bigworldSecondaryDatabases"
    );
    assert_eq!(
        SELECT_ENTITY_DEFS_CHECKSUM,
        "SELECT checksum FROM bigworldEntityDefsChecksum"
    );
    assert_eq!(
        DELETE_SECONDARY_DATABASES,
        "DELETE FROM bigworldSecondaryDatabases"
    );
}

#[test]
fn htonl_and_ip_as_string_shapes() {
    // The DB column stores a little-endian-written u32 (0x7F000001 for
    // 127.0.0.1); htonl swaps it back and Address prints the octets.
    assert_eq!(htonl(0x7F00_0001), 0x0100_007F);
    // ipAsString's internal ntohl cancels getSecondaryDBInfos' htonl: the
    // printed octets are those of the raw DB column value.
    assert_eq!(ip_as_string(htonl(0x7F00_0001)), "127.0.0.1");
    assert_eq!(ip_as_string(htonl(0x0A00_0002)), "10.0.0.2");
    assert_eq!(ip_as_string(0), "0.0.0.0");
}

// ---- strtoul_base0 ----

#[test]
fn strtoul_base0_reference_shapes() {
    // Empty segment: endptr == start == end → accepted as port 0.
    assert_eq!(strtoul_base0(""), (0, 0));
    // No digits: endptr parks at the very start → rejected unless empty.
    assert_eq!(strtoul_base0("-"), (0, 0));
    assert_eq!(strtoul_base0("abc"), (0, 0));
    // Negative wraps like strtoul's ULONG_MAX result.
    assert_eq!(strtoul_base0("-1"), (u64::MAX, 2));
    // Bases: 0x hex, leading-zero octal (the zero itself counts), decimal.
    assert_eq!(strtoul_base0("0x1F90"), (8080, 6));
    assert_eq!(strtoul_base0("010"), (8, 3));
    assert_eq!(strtoul_base0("12"), (12, 2));
    // Trailing junk leaves the endptr short of the end.
    assert_eq!(strtoul_base0("12x"), (12, 2));
    // A "0x" without hex digits converts only the leading zero.
    assert_eq!(strtoul_base0("0x"), (0, 1));
    // Leading whitespace is skipped and included in the end position.
    assert_eq!(strtoul_base0(" 12"), (12, 3));
    // Lone zero.
    assert_eq!(strtoul_base0("0"), (0, 1));
    // Positive sign.
    assert_eq!(strtoul_base0("+7"), (7, 2));
}

// ---- digest ----

#[test]
fn digest_quote_unquote_round_trip() {
    let digest = test_digest();
    assert_eq!(digest.quote(), VALID_DIGEST);
    assert_eq!(Digest::unquote(&digest.quote()), Some(digest));
    assert_eq!(digest.quote().len(), 32);
}

#[test]
fn digest_unquote_requires_32_characters() {
    assert_eq!(Digest::unquote(""), None);
    assert_eq!(Digest::unquote("00112233445566778899AABBCCDDEEFF0"), None);
    assert_eq!(Digest::unquote("00112233445566778899AABBCCDDEEF"), None);
}

#[test]
fn digest_unquote_nibble_math_is_unvalidated() {
    // Lowercase letters decode correctly by accident ('a'-55 = 0x2A whose
    // low nibble is 0xA).
    let lowercase = Digest::unquote("abcdef0123456789abcdef0123456789").unwrap();
    assert_eq!(lowercase.0[0], 0xAB);
    // Junk characters decode to junk bytes ('z' = 122 → 122-55 = 0x43;
    // 0x43<<4|0x43 truncates to 0x73, matching the reference's int→byte
    // truncation).
    let junk = Digest::unquote("zz000000000000000000000000000000").unwrap();
    assert_eq!(junk.0[0], 0x73);
    assert_eq!(Digest::cleared(), Digest([0u8; 16]));
}

// ---- CommandLineParser ----

#[test]
fn no_args_ok_and_seeds_config_info() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(&args(&[]), &mut rig.reader, &mut log));
    assert!(!parser.had_non_option_args());
    assert_eq!(parser.primary_database_info(), &rig.config.info);
    assert!(parser.secondary_databases().is_empty());
    assert!(log.is_empty());
}

#[test]
fn help_prints_usage_and_fails() {
    for flag in ["-h", "--help"] {
        let mut rig = make_rig();
        let mut log = Vec::new();
        let mut parser = CommandLineParser::new(&rig.config);
        assert!(!parser.init(&args(&[flag]), &mut rig.reader, &mut log));
        assert_eq!(log, vec![USAGE_MESSAGE.to_string()]);
    }
}

#[test]
fn long_flags_recorded() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(
        &args(&[
            "--verbose",
            "--clear",
            "--list",
            "--ignore-sqlite-errors",
            "--res",
            "/res/path"
        ]),
        &mut rig.reader,
        &mut log
    ));
    assert!(parser.is_verbose());
    assert!(parser.should_clear());
    assert!(parser.should_list());
    assert!(parser.should_ignore_sqlite_errors());
    assert_eq!(parser.res_path(), "/res/path");
    assert!(!parser.had_non_option_args());
}

#[test]
fn res_forms_all_reach_res_path() {
    for form in [
        vec!["-rX"],
        vec!["-r", "X"],
        vec!["--res=X"],
        vec!["--res", "X"],
    ] {
        let mut rig = make_rig();
        let mut log = Vec::new();
        let mut parser = CommandLineParser::new(&rig.config);
        assert!(parser.init(&args(&form), &mut rig.reader, &mut log));
        assert_eq!(parser.res_path(), "X");
    }
}

#[test]
fn missing_res_argument_reports_colon_without_newline() {
    for flag in ["--res", "-r"] {
        let mut rig = make_rig();
        let mut log = Vec::new();
        let mut parser = CommandLineParser::new(&rig.config);
        assert!(!parser.init(&args(&[flag]), &mut rig.reader, &mut log));
        assert_eq!(log, vec!["Missing argument for option -r".to_string()]);
    }
}

#[test]
fn unknown_short_option_reports_optopt() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(!parser.init(&args(&["-z"]), &mut rig.reader, &mut log));
    assert_eq!(log, vec!["Unknown option -z".to_string()]);
}

#[test]
fn unknown_long_option_carries_nul_optopt() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(!parser.init(&args(&["--bogus"]), &mut rig.reader, &mut log));
    // glibc reports '?' with optopt 0 for unknown long options, so the
    // reference's "-%c" carries a literal NUL.
    assert_eq!(log, vec!["Unknown option -\0".to_string()]);
}

#[test]
fn clear_list_ignore_have_no_short_forms() {
    // The short option string is ":r:vh" — -c/-l/-i are unknown shorts
    // even though the long options map to those characters.
    for (flag, expected) in [
        ("-c", "Unknown option -c"),
        ("-l", "Unknown option -l"),
        ("-i", "Unknown option -i"),
    ] {
        let mut rig = make_rig();
        let mut log = Vec::new();
        let mut parser = CommandLineParser::new(&rig.config);
        assert!(!parser.init(&args(&[flag]), &mut rig.reader, &mut log));
        assert_eq!(log, vec![expected.to_string()]);
    }
}

#[test]
fn long_option_with_inline_argument_rejected() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    // The reference additionally lets getopt print its own stderr
    // diagnostic; the port models the returned '?' with the mapped short.
    assert!(!parser.init(&args(&["--verbose=1"]), &mut rig.reader, &mut log));
    assert_eq!(log, vec!["Unknown option -v".to_string()]);
}

#[test]
fn short_cluster_vrX_reads_attached_argument() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(&args(&["-vrX"]), &mut rig.reader, &mut log));
    assert!(parser.is_verbose());
    assert_eq!(parser.res_path(), "X");
}

#[test]
fn single_positional_rejected_with_usage() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(!parser.init(&args(&["host;3306;u;p;d"]), &mut rig.reader, &mut log));
    assert_eq!(
        log,
        vec![MISSING_LIST_MESSAGE.to_string(), USAGE_MESSAGE.to_string(),]
    );
}

#[test]
fn three_positionals_warn_without_newline() {
    let mut rig = make_rig();
    rig.reader.content = Some(vec!["/a.db".to_string()]);
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(
        &args(&["host;3306;u;p;d", "/list", "extra"]),
        &mut rig.reader,
        &mut log
    ));
    assert!(parser.had_non_option_args());
    assert_eq!(
        log,
        vec!["Extraneous arguments after secondary database list path ignored".to_string()]
    );
    assert_eq!(parser.secondary_databases(), &["/a.db".to_string()]);
}

#[test]
fn clear_with_positionals_warns_and_uses_bw_xml() {
    let mut rig = make_rig();
    rig.reader.content = Some(vec!["/a.db".to_string()]);
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(
        &args(&["--clear", "host;3306;u;p;d", "/list"]),
        &mut rig.reader,
        &mut log
    ));
    assert!(parser.should_clear());
    assert!(parser.had_non_option_args());
    assert_eq!(
        log,
        vec![
            "consolidate_dbs: The --clear option does not take additional arguments\n".to_string()
        ]
    );
    // Not parsed — the bw.xml connection info and an empty list stand.
    assert_eq!(parser.primary_database_info(), &rig.config.info);
    assert!(parser.secondary_databases().is_empty());
}

#[test]
fn list_with_positionals_prefers_clear_in_message() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(
        &args(&["--list", "--clear", "host;3306;u;p;d", "/list"]),
        &mut rig.reader,
        &mut log
    ));
    assert_eq!(
        log,
        vec![
            "consolidate_dbs: The --clear option does not take additional arguments\n".to_string()
        ]
    );
}

#[test]
fn spec_five_segments_parsed() {
    let mut rig = make_rig();
    rig.reader.content = Some(vec!["/a.db".to_string()]);
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(
        &args(&["myhost;3306;user;pass;db", "/list"]),
        &mut rig.reader,
        &mut log
    ));
    assert_eq!(
        parser.primary_database_info(),
        &ConnectionInfo {
            host: "myhost".to_string(),
            port: 3306,
            username: "user".to_string(),
            password: "pass".to_string(),
            database: "db".to_string(),
        }
    );
    assert_eq!(parser.secondary_databases(), &["/a.db".to_string()]);
}

#[test]
fn spec_wrong_segment_count_rejected() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(!parser.init(
        &args(&["host;3306;user;pass", "/list"]),
        &mut rig.reader,
        &mut log
    ));
    assert_eq!(
        log,
        vec!["Primary database argument must be in the form <host>;<port>;<username>;<password>;<database>\n".to_string()]
    );
}

#[test]
fn empty_port_segment_is_port_zero() {
    let mut rig = make_rig();
    rig.reader.content = Some(Vec::new());
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(&args(&["h;;u;p;d", "/list"]), &mut rig.reader, &mut log));
    assert_eq!(parser.primary_database_info().port, 0);
}

#[test]
fn negative_port_wraps_out_of_range() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(!parser.init(&args(&["h;-1;u;p;d", "/list"]), &mut rig.reader, &mut log));
    assert_eq!(
        log,
        vec!["Primary database port is out-of-range: -1\n".to_string()]
    );
}

#[test]
fn hex_and_octal_ports_parse_base0() {
    for (text, expected) in [("0x1F90", 8080u16), ("010", 8u16), ("65535", 65535u16)] {
        let mut rig = make_rig();
        rig.reader.content = Some(Vec::new());
        let mut log = Vec::new();
        let mut parser = CommandLineParser::new(&rig.config);
        let spec = format!("h;{text};u;p;d");
        assert!(parser.init(&args(&[&spec, "/list"]), &mut rig.reader, &mut log));
        assert_eq!(parser.primary_database_info().port, expected);
    }
}

#[test]
fn port_trailing_junk_and_lone_dash_rejected() {
    for text in ["12x", "-", "abc"] {
        let mut rig = make_rig();
        let mut log = Vec::new();
        let mut parser = CommandLineParser::new(&rig.config);
        let spec = format!("h;{text};u;p;d");
        assert!(!parser.init(&args(&[&spec, "/list"]), &mut rig.reader, &mut log));
        assert_eq!(
            log,
            vec![format!("Could not parse database port :{text}\n")]
        );
    }
}

#[test]
fn port_65536_out_of_range() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(!parser.init(
        &args(&["h;65536;u;p;d", "/list"]),
        &mut rig.reader,
        &mut log
    ));
    assert_eq!(
        log,
        vec!["Primary database port is out-of-range: 65536\n".to_string()]
    );
}

#[test]
fn list_file_lines_loaded_and_empty_lines_skipped() {
    let mut rig = make_rig();
    rig.reader.content = Some(vec![
        "/a.db".to_string(),
        String::new(),
        "/b.db".to_string(),
    ]);
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(&args(&["h;1;u;p;d", "/list"]), &mut rig.reader, &mut log));
    assert_eq!(
        parser.secondary_databases(),
        &["/a.db".to_string(), "/b.db".to_string()]
    );
}

#[test]
fn list_file_unopenable_rejected() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(!parser.init(&args(&["h;1;u;p;d", "/nope"]), &mut rig.reader, &mut log));
    assert_eq!(
        log,
        vec!["Could not open secondary database list at \"/nope\"\n".to_string()]
    );
}

#[test]
fn option_after_positional_permutes() {
    let mut rig = make_rig();
    rig.reader.content = Some(vec!["/a.db".to_string()]);
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    assert!(parser.init(
        &args(&["h;1;u;p;d", "/list", "--verbose"]),
        &mut rig.reader,
        &mut log
    ));
    assert!(parser.is_verbose());
    assert_eq!(parser.primary_database_info().host, "h");
}

#[test]
fn double_dash_ends_option_parsing() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut parser = CommandLineParser::new(&rig.config);
    // "--clear" after "--" is a positional: a single positional is rejected.
    assert!(!parser.init(&args(&["--", "--clear"]), &mut rig.reader, &mut log));
    assert!(!parser.should_clear());
    assert_eq!(
        log,
        vec![MISSING_LIST_MESSAGE.to_string(), USAGE_MESSAGE.to_string(),]
    );
}

// ---- progress reporters ----

#[test]
fn consolidation_reporter_verbatim_and_throttle() {
    let mut sink = SinkReporter::default();
    let readback = sink.clone();
    let clock = ClockDouble {
        now: std::cell::Cell::new(1000),
        sps: 1000,
    };
    let mut reporter = ConsolidationProgressReporter::new(&mut sink, 5, &clock);

    reporter.on_start_consolidate_db("/a.db", 10);
    assert_eq!(
        readback.recorded(),
        vec!["Consolidating /a.db (0/10 entities) (1/5 databases)".to_string()]
    );

    // Rows inside the half-second window stay silent.
    reporter.on_consolidated_row();
    reporter.on_consolidated_row();
    assert_eq!(readback.recorded().len(), 1);

    // Past the interval the status fires again.
    clock.now.set(clock.now.get() + 600);
    reporter.on_consolidated_row();
    assert_eq!(
        readback.recorded().last().unwrap(),
        "Consolidating /a.db (3/10 entities) (1/5 databases)"
    );

    // A second DB restarts the counters and reports immediately.
    reporter.on_start_consolidate_db("/b.db", 0);
    assert_eq!(
        readback.recorded().last().unwrap(),
        "Consolidating /b.db (0/0 entities) (2/5 databases)"
    );
}

#[test]
fn transfer_reporter_waiting_then_progress() {
    let mut sink = SinkReporter::default();
    let readback = sink.clone();
    let clock = ClockDouble {
        now: std::cell::Cell::new(0),
        sps: 1000,
    };
    let mut reporter = FileTransferProgressReporter::new(&mut sink, 2, &clock);

    // The constructor reports the waiting status immediately (double space
    // after "for" is verbatim).
    assert_eq!(
        readback.recorded(),
        vec!["Data consolidation is waiting for  all secondary database file transfers to start (0/2 started)".to_string()]
    );

    // Inside the throttle window the first start stays silent.
    reporter.on_start_transfer(100);
    assert_eq!(readback.recorded().len(), 1);

    // Once all files have started the byte line takes over ("Transfering"
    // typo verbatim).
    clock.now.set(600);
    reporter.on_start_transfer(100);
    assert_eq!(
        readback.recorded().last().unwrap(),
        "Transfering secondary databases for consolidation (0/200 bytes) (0/2 files completed)"
    );

    clock.now.set(1200);
    reporter.on_receive_data(150);
    assert_eq!(
        readback.recorded().last().unwrap(),
        "Transfering secondary databases for consolidation (150/200 bytes) (0/2 files completed)"
    );

    reporter.on_finish_transfer();
    reporter.on_finish_transfer();
    clock.now.set(1800);
    reporter.on_receive_data(50);
    assert_eq!(
        readback.recorded().last().unwrap(),
        "Transfering secondary databases for consolidation (200/200 bytes) (2/2 files completed)"
    );
}

// ---- app init ----

#[test]
fn init_happy_pins_tool_app_and_trace() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.reports_to_db_app());
        assert!(!app.should_abort);
    }
    assert_eq!(
        rig.tool.inits,
        vec![(
            "ConsolidateDBs".to_string(),
            "consolidate_dbs".to_string(),
            true,
            false,
            info.clone()
        )]
    );
    assert_eq!(rig.tool.signals, vec![SIGINT, SIGHUP]);
    assert_eq!(
        log,
        vec!["ConsolidateDBsApp: Connected to primary database: host=primary:3306, username=bw, database=bigworld\n".to_string()]
    );
}

#[test]
fn init_tool_failure_is_silent_false() {
    let mut rig = make_rig();
    rig.tool.init_ok = false;
    let mut log = Vec::new();
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(!app.init(&mut faces, true, false, false, &info));
    }
    assert!(log.is_empty());
    assert_eq!(rig.tool.signals, Vec::<i32>::new());
}

#[test]
fn init_nonexistent_directory_error() {
    let mut rig = make_rig();
    rig.config.resolve_ok = false;
    let mut log = Vec::new();
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(!app.init(&mut faces, true, false, false, &info));
    }
    // The "Connected to primary database" trace precedes the error.
    assert_eq!(
        log[1],
        "ConsolidateDBsApp::init: Configuration setting </bigworld/secondaryDB/consolidation/directory> specifies a non-existent directory: /tmp/consolidated\n"
    );
}

#[test]
fn init_db_app_required_but_missing_or_failing_is_silent_false() {
    // No channel wired up.
    let mut rig = make_rig();
    let mut log = Vec::new();
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(!app.init(&mut faces, true, true, false, &info));
    }
    assert!(log.is_empty());

    // Channel present but DBApp::init fails.
    let mut rig2 = make_rig();
    rig2.db_app = Some(DbAppDouble {
        init_ok: false,
        ..DbAppDouble::default()
    });
    let mut log2 = Vec::new();
    let info = rig2.config.info.clone();
    {
        let mut faces = faces_of(&mut rig2);
        let mut app = ConsolidateDBsApp::new(&mut log2, true);
        assert!(!app.init(&mut faces, true, true, false, &info));
    }
    assert!(log2.is_empty());
}

// ---- transferAndConsolidate ----

#[test]
fn transfer_and_consolidate_empty_registrations() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.transfer_and_consolidate(&mut faces));
    }
    assert_eq!(
        log[1],
        "ConsolidateDBsApp::transferAndConsolidate: No secondary databases to consolidate\n"
    );
    assert_eq!(rig.transfer.listens, Vec::<(u32, usize)>::new());
}

#[test]
fn transfer_and_consolidate_happy_pins_everything() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(0x7F00_0001, "/data/sec.db".to_string())]);
    rig.primary.delete_result = Ok(3);
    rig.transfer.received = vec!["/tmp/sec.db".to_string()];
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(app.transfer_and_consolidate(&mut faces));
    }

    // Listener bound on the watcher nub's internal address for one DB.
    assert_eq!(rig.transfer.listens, vec![(0x0A00_0009, 1)]);
    // The registration's ip is htonl'd before the request.
    assert_eq!(
        rig.transfer.requests,
        vec![(0x0100_007F, "/data/sec.db".to_string())]
    );
    assert_eq!(rig.transfer.wait_calls, 1);
    assert_eq!(rig.transfer.cleanups, 1);
    // The merge queue got the primary connection info; the received file
    // was consolidated with ignore-sqlite-errors = false by default.
    assert_eq!(rig.merge.queue_inits, vec![info.clone()]);
    assert_eq!(rig.merge.inits, vec!["/tmp/sec.db".to_string()]);
    assert_eq!(
        rig.merge.consolidations,
        vec![("/tmp/sec.db".to_string(), false, false)]
    );
    assert_eq!(rig.primary.delete_calls, 1);
    assert_eq!(
        log,
        vec![
            "ConsolidateDBsApp: Connected to primary database: host=primary:3306, username=bw, database=bigworld\n".to_string(),
            "ConsolidateDBsApp::transferSecondaryDBs: Listening on: 10.0.0.9:3000\n".to_string(),
            "ConsolidateDBsApp::consolidateSecondaryDBs: Number of connections = 2.\n".to_string(),
            "ConsolidateDBsApp::consolidateSecondaryDB: Consolidating '/tmp/sec.db'\n".to_string(),
            "ConsolidateDBsApp::clearSecondaryDBEntries: Cleared 3 entries from primary:3306 (bigworld)\n".to_string(),
            "ConsolidateDBsApp::transferAndConsolidate: Completed successfully\n".to_string(),
        ]
    );
}

#[test]
fn transfer_and_consolidate_query_failure() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Err("boom".to_string());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.transfer_and_consolidate(&mut faces));
    }
    assert_eq!(
        log[1],
        "ConsolidateDBsApp::getSecondaryDBInfos: Failed to get secondary DB information from primary database: boom\n"
    );
    assert_eq!(rig.transfer.listens, Vec::<(u32, usize)>::new());
}

#[test]
fn transfer_and_consolidate_request_failure_sets_abort() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(1, "/a.db".to_string())]);
    rig.transfer.request_ok = false;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.transfer_and_consolidate(&mut faces));
        assert!(app.should_abort);
    }
    assert_eq!(
        log[2],
        "ConsolidateDBsApp::transferAndConsolidate: Failed to transfer secondaryDBs\n"
    );
    assert_eq!(rig.transfer.wait_calls, 0);
    assert_eq!(rig.transfer.cleanups, 0);
}

#[test]
fn transfer_and_consolidate_listen_failure_is_silent_up_to_error() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(1, "/a.db".to_string())]);
    rig.transfer.listen_ok = false;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.transfer_and_consolidate(&mut faces));
    }
    // getBoundAddr fails silently; only the app-level transfer error shows
    // (no "Listening on" line before it).
    assert_eq!(
        log[1],
        "ConsolidateDBsApp::transferAndConsolidate: Failed to transfer secondaryDBs\n"
    );
}

#[test]
fn transfer_and_consolidate_wait_failure_stops_before_consolidation() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(1, "/a.db".to_string())]);
    rig.transfer.wait_ok = false;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.transfer_and_consolidate(&mut faces));
    }
    assert_eq!(
        log[2],
        "ConsolidateDBsApp::transferAndConsolidate: Failed to transfer secondaryDBs\n"
    );
    assert_eq!(rig.merge.inits, Vec::<String>::new());
    assert_eq!(rig.transfer.cleanups, 0);
}

#[test]
fn transfer_and_consolidate_consolidate_failure_skips_cleanup() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(1, "/a.db".to_string())]);
    rig.transfer.received = vec!["/tmp/a.db".to_string()];
    rig.merge.consolidate_ok = false;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.transfer_and_consolidate(&mut faces));
        assert!(app.consolidation_errors.secondary_db_has_error("/tmp/a.db"));
        assert!(app.consolidation_errors.has_errors());
    }
    // trace, listening, connections, consolidating, the run-level WARNING,
    // then the app-level failure.
    assert_eq!(
        log[5],
        "ConsolidateDBsApp::transferAndConsolidate: Failed to consolidate secondaryDBs\n"
    );
    // The reference returns before cleanUpRemoteFiles and clear.
    assert_eq!(rig.transfer.cleanups, 0);
    assert_eq!(rig.primary.delete_calls, 0);
}

#[test]
fn transfer_reporter_reaches_db_app_channel() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(1, "/a.db".to_string())]);
    rig.db_app = Some(DbAppDouble::default());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, true, false, &info));
        assert!(app.reports_to_db_app());
        assert!(app.transfer_and_consolidate(&mut faces));
    }
    assert_eq!(rig.db_app.as_ref().unwrap().init_calls, 1);
    // The reporter's constructor status goes through the forwarding chain;
    // one registration → waiting for 1 of 1 files.
    assert_eq!(
        rig.db_app.as_ref().unwrap().statuses,
        vec!["Data consolidation is waiting for  all secondary database file transfers to start (0/1 started)".to_string()]
    );
}

// ---- consolidateSecondaryDBs ----

#[test]
fn consolidate_dbs_happy_two_files_pins_merges() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.start_db_report = true;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(
            app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string(), "/b.db".to_string()])
        );
    }
    assert_eq!(
        rig.merge.consolidations,
        vec![
            ("/a.db".to_string(), false, false),
            ("/b.db".to_string(), false, false)
        ]
    );
    // trace + connections + one Consolidating INFO per file
    assert_eq!(log.len(), 4);
}

#[test]
fn consolidate_dbs_progress_reaches_db_app() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.start_db_report = true;
    rig.db_app = Some(DbAppDouble::default());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, true, false, &info));
        assert!(
            app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string(), "/b.db".to_string()])
        );
    }
    assert_eq!(
        rig.db_app.as_ref().unwrap().statuses,
        vec![
            "Consolidating /a.db (0/5 entities) (1/2 databases)".to_string(),
            "Consolidating /b.db (0/5 entities) (2/2 databases)".to_string(),
        ]
    );
}

#[test]
fn consolidate_dbs_queue_init_failure() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.queue_ok = false;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string()]));
    }
    assert_eq!(
        log[2],
        "ConsolidateDBsApp::consolidateSecondaryDBs: Failed to initialize primaryDBQueue\n"
    );
    assert_eq!(rig.merge.inits, Vec::<String>::new());
}

#[test]
fn consolidate_dbs_digest_mismatch_fails_entity_check() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.digest = Some(OTHER_DIGEST.to_string());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string()]));
        assert!(!app.consolidation_errors.has_errors());
    }
    assert_eq!(
        log[2..4],
        [
            "ConsolidateDBsApp::consolidateSecondaryDB: Consolidating '/a.db'\n".to_string(),
            "ConsolidateDBsApp::consolidateSecondaryDB: /a.db failed entity digest check\n"
                .to_string(),
        ]
    );
    assert_eq!(
        log[4],
        "ConsolidateDBsApp::consolidateSecondaryDBs: Some entities were not consolidated. Data consolidation must be re-run after errors have been corrected.\n"
    );
}

#[test]
fn consolidate_dbs_invalid_digest_message() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.digest = Some("nothex".to_string());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string()]));
    }
    assert_eq!(
        log[3],
        "ConsolidateDBsApp::checkEntityDefsDigestMatch: Not a valid MD5 digest\n"
    );
    assert_eq!(
        log[4],
        "ConsolidateDBsApp::consolidateSecondaryDB: /a.db failed entity digest check\n"
    );
}

#[test]
fn consolidate_dbs_merge_failure_goes_to_ledger() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.consolidate_ok = false;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(
            !app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string(), "/b.db".to_string()])
        );
        assert!(app.consolidation_errors.secondary_db_has_error("/a.db"));
        // The loop stops at the first failure, so /b.db was never attempted.
        assert!(!app.consolidation_errors.secondary_db_has_error("/b.db"));
    }
    // The engine never set the abort flag → the long WARNING.
    assert_eq!(
        log[3],
        "ConsolidateDBsApp::consolidateSecondaryDBs: Some entities were not consolidated. Data consolidation must be re-run after errors have been corrected.\n"
    );
}

#[test]
fn consolidate_dbs_engine_set_abort_swaps_warning() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.consolidate_ok = false;
    rig.merge.abort_to_set = Some(true);
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string()]));
        assert!(app.should_abort);
    }
    assert_eq!(
        log[3],
        "ConsolidateDBsApp::consolidateSecondaryDBs: Data consolidation was aborted\n"
    );
}

#[test]
fn consolidate_dbs_init_and_digest_failures_are_silent() {
    // secondaryDB.init failure: no message at all.
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.merge.init_ok = false;
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string()]));
    }
    assert_eq!(log.len(), 3); // trace, connections, run-level WARNING

    // getChecksumDigest failure: silent after the Consolidating INFO, but
    // the run-level WARNING still lands.
    let mut rig2 = make_rig();
    let mut log2 = Vec::new();
    rig2.merge.digest = None;
    let info = rig2.config.info.clone();
    {
        let mut faces = faces_of(&mut rig2);
        let mut app = ConsolidateDBsApp::new(&mut log2, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.consolidate_secondary_dbs(&mut faces, &["/a.db".to_string()]));
    }
    assert_eq!(log2.len(), 4); // trace, connections, Consolidating, WARNING
    assert!(!log2[2].contains("failed entity digest check"));
}

// ---- defs match ----

#[test]
fn defs_match_ok_when_digests_agree() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.checksum = Ok(Some(VALID_DIGEST.to_string()));
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(app.check_primary_db_entity_defs_match(&mut faces));
    }
    assert_eq!(log.len(), 1);
}

#[test]
fn defs_mismatch_prints_long_message() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.checksum = Ok(Some(OTHER_DIGEST.to_string()));
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.check_primary_db_entity_defs_match(&mut faces));
    }
    assert_eq!(log[1], DEFS_MISMATCH_MESSAGE);
}

#[test]
fn checksum_table_empty_is_a_mismatch() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.check_primary_db_entity_defs_match(&mut faces));
    }
    assert_eq!(
        log[1],
        "ConsolidateDBsApp::checkEntityDefsMatch: Checksum table is empty\n"
    );
    assert_eq!(log[2], DEFS_MISMATCH_MESSAGE);
}

#[test]
fn checksum_query_error_is_a_mismatch() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.checksum = Err("boom".to_string());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.check_primary_db_entity_defs_match(&mut faces));
    }
    assert_eq!(
        log[1],
        "ConsolidateDBsApp::checkEntityDefsMatch: Failed to retrieve the primary database entity definition checksum: boom\n"
    );
    assert_eq!(log[2], DEFS_MISMATCH_MESSAGE);
}

// ---- clear / list / status / signals ----

#[test]
fn clear_secondary_db_entries_ok_and_out_param() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.delete_result = Ok(7);
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        let mut cleared = 0u32;
        assert!(app.clear_secondary_db_entries(&mut faces, Some(&mut cleared)));
        assert_eq!(cleared, 7);
    }
    assert_eq!(
        log[1],
        "ConsolidateDBsApp::clearSecondaryDBEntries: Cleared 7 entries from primary:3306 (bigworld)\n"
    );
}

#[test]
fn clear_secondary_db_entries_error_has_no_newline() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.delete_result = Err("boom".to_string());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.clear_secondary_db_entries(&mut faces, None));
    }
    // The reference's catch prints with no trailing newline.
    assert_eq!(log[1], "ConsolidateDBsApp::clearSecondaryDBEntries: boom");
}

#[test]
fn print_databases_rows_right_align_ips() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![
        (0x7F00_0001, "/data/sec.db".to_string()),
        (0x0A00_0002, "/other.db".to_string()),
    ]);
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(app.print_databases(&mut faces));
    }
    assert_eq!(
        log[1..],
        [
            "\n".to_string(),
            "Secondary databases to consolidate = 2\n".to_string(),
            "      127.0.0.1 /data/sec.db\n".to_string(),
            "       10.0.0.2 /other.db\n".to_string(),
        ]
    );
}

#[test]
fn print_databases_query_failure() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Err("boom".to_string());
    let info = rig.config.info.clone();
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        assert!(app.init(&mut faces, true, false, false, &info));
        assert!(!app.print_databases(&mut faces));
    }
    assert_eq!(log.len(), 2);
}

#[test]
fn on_signalled_aborts_on_interrupt_and_hangup() {
    let mut log = Vec::new();
    let mut app = ConsolidateDBsApp::new(&mut log, true);
    app.on_signalled(SIGINT);
    assert!(app.should_abort);
    app.on_signalled(SIGHUP);
    app.on_signalled(15);
    assert_eq!(
        log,
        vec![
            "ConsolidateDBsApp::onSignalled: aborting consolidation\n".to_string(),
            "ConsolidateDBsApp::onSignalled: aborting consolidation\n".to_string(),
        ]
    );
}

#[test]
fn on_status_forwards_only_with_db_app() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.db_app = Some(DbAppDouble::default());
    {
        let mut faces = faces_of(&mut rig);
        let mut app = ConsolidateDBsApp::new(&mut log, true);
        app.on_status(&mut faces, "doing things");
    }
    assert_eq!(
        rig.db_app.as_ref().unwrap().statuses,
        vec!["doing things".to_string()]
    );

    let mut rig2 = make_rig();
    let mut log2 = Vec::new();
    {
        let mut faces = faces_of(&mut rig2);
        let mut app = ConsolidateDBsApp::new(&mut log2, true);
        app.on_status(&mut faces, "doing things");
    }
    assert!(log2.is_empty());
}

// ---- main_face ----

#[test]
fn main_bad_config_returns_success_quirk() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.config.is_bad = true;
    let mut faces = faces_of(&mut rig);
    assert_eq!(main_face(&args(&[]), &mut faces, &mut log), EXIT_SUCCESS);
    assert!(log.is_empty());
}

#[test]
fn main_parse_failure_exit_one() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    let mut faces = faces_of(&mut rig);
    assert_eq!(
        main_face(&args(&["-z"]), &mut faces, &mut log),
        EXIT_FAILURE
    );
    assert_eq!(
        log,
        vec![
            "Unknown option -z".to_string(),
            "Failed to parse command line options.\n".to_string(),
        ]
    );
}

#[test]
fn main_init_failure_exit_one() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.tool.init_ok = false;
    let mut faces = faces_of(&mut rig);
    assert_eq!(main_face(&args(&[]), &mut faces, &mut log), EXIT_FAILURE);
    assert!(log.is_empty());
}

#[test]
fn main_clear_ok_zero_and_fail_one() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.delete_result = Ok(2);
    let mut faces = faces_of(&mut rig);
    assert_eq!(
        main_face(&args(&["--clear"]), &mut faces, &mut log),
        EXIT_SUCCESS
    );
    assert_eq!(
        log,
        vec![
            "ConsolidateDBsApp: Connected to primary database: host=primary:3306, username=bw, database=bigworld\n".to_string(),
            "ConsolidateDBsApp::clearSecondaryDBEntries: Cleared 2 entries from primary:3306 (bigworld)\n".to_string(),
        ]
    );

    let mut rig2 = make_rig();
    let mut log2 = Vec::new();
    rig2.primary.delete_result = Err("boom".to_string());
    let mut faces = faces_of(&mut rig2);
    assert_eq!(
        main_face(&args(&["--clear"]), &mut faces, &mut log2),
        EXIT_FAILURE
    );
    assert_eq!(log2[2], "Failed to clear secondary databases\n");
}

#[test]
fn main_list_ok_zero_and_fail_one() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(0x7F00_0001, "/data/sec.db".to_string())]);
    let mut faces = faces_of(&mut rig);
    assert_eq!(
        main_face(&args(&["--list"]), &mut faces, &mut log),
        EXIT_SUCCESS
    );
    assert_eq!(log.len(), 4);

    let mut rig2 = make_rig();
    let mut log2 = Vec::new();
    rig2.primary.secondary_rows = Err("boom".to_string());
    let mut faces = faces_of(&mut rig2);
    assert_eq!(
        main_face(&args(&["--list"]), &mut faces, &mut log2),
        EXIT_FAILURE
    );
    assert_eq!(log2[2], "Failed to list secondary databases\n");
}

#[test]
fn main_defs_mismatch_exit_one_before_any_transfer() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.checksum = Ok(Some(OTHER_DIGEST.to_string()));
    let mut faces = faces_of(&mut rig);
    assert_eq!(main_face(&args(&[]), &mut faces, &mut log), EXIT_FAILURE);
    assert_eq!(log[1], DEFS_MISMATCH_MESSAGE);
    // The transfer path never started.
    assert_eq!(rig.transfer.listens, Vec::<(u32, usize)>::new());
}

#[test]
fn main_positional_path_consolidates_the_list() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.reader.content = Some(vec!["/a.db".to_string(), "/b.db".to_string()]);
    rig.merge.start_db_report = true;
    rig.primary.checksum = Ok(Some(VALID_DIGEST.to_string()));
    // The file-list path implies reporting to DBApp, so init needs the
    // channel wired up.
    rig.db_app = Some(DbAppDouble::default());
    let mut faces = faces_of(&mut rig);
    assert_eq!(
        main_face(&args(&["h;1;u;p;d", "/list"]), &mut faces, &mut log),
        EXIT_SUCCESS
    );
    assert_eq!(
        rig.merge.consolidations,
        vec![
            ("/a.db".to_string(), false, false),
            ("/b.db".to_string(), false, false)
        ]
    );
    // The primary info came from the command line spec.
    assert_eq!(rig.merge.queue_inits[0].host, "h");
    assert_eq!(rig.merge.queue_inits[0].port, 1);
}

#[test]
fn main_positional_path_wires_db_app_and_reports() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.reader.content = Some(vec!["/a.db".to_string()]);
    rig.merge.start_db_report = true;
    rig.primary.checksum = Ok(Some(VALID_DIGEST.to_string()));
    rig.db_app = Some(DbAppDouble::default());
    let mut faces = faces_of(&mut rig);
    assert_eq!(
        main_face(&args(&["h;1;u;p;d", "/list"]), &mut faces, &mut log),
        EXIT_SUCCESS
    );
    assert_eq!(rig.db_app.as_ref().unwrap().init_calls, 1);
    assert_eq!(
        rig.db_app.as_ref().unwrap().statuses,
        vec!["Consolidating /a.db (0/5 entities) (1/1 databases)".to_string()]
    );
}

#[test]
fn main_positional_path_failure_exit_one() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.reader.content = Some(vec!["/a.db".to_string()]);
    rig.merge.consolidate_ok = false;
    rig.primary.checksum = Ok(Some(VALID_DIGEST.to_string()));
    rig.db_app = Some(DbAppDouble::default());
    let mut faces = faces_of(&mut rig);
    assert_eq!(
        main_face(&args(&["h;1;u;p;d", "/list"]), &mut faces, &mut log),
        EXIT_FAILURE
    );
    assert_eq!(
        log[log.len() - 1],
        "ConsolidateDBsApp::consolidateSecondaryDBs: Some entities were not consolidated. Data consolidation must be re-run after errors have been corrected.\n"
    );
}

#[test]
fn main_transfer_path_happy_exit_zero_and_fail_one() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.primary.secondary_rows = Ok(vec![(0x7F00_0001, "/data/sec.db".to_string())]);
    rig.primary.checksum = Ok(Some(VALID_DIGEST.to_string()));
    rig.transfer.received = vec!["/tmp/sec.db".to_string()];
    let mut faces = faces_of(&mut rig);
    assert_eq!(main_face(&args(&[]), &mut faces, &mut log), EXIT_SUCCESS);
    assert_eq!(rig.transfer.wait_calls, 1);

    let mut rig2 = make_rig();
    let mut log2 = Vec::new();
    rig2.primary.secondary_rows = Ok(vec![(1, "/a.db".to_string())]);
    rig2.primary.checksum = Ok(Some(VALID_DIGEST.to_string()));
    rig2.transfer.request_ok = false;
    let mut faces = faces_of(&mut rig2);
    assert_eq!(main_face(&args(&[]), &mut faces, &mut log2), EXIT_FAILURE);
    assert!(log2
        .last()
        .unwrap()
        .contains("Failed to transfer secondaryDBs"));
}

#[test]
fn main_ignore_sqlite_errors_reaches_merge_leg() {
    let mut rig = make_rig();
    let mut log = Vec::new();
    rig.reader.content = Some(vec!["/a.db".to_string()]);
    rig.primary.checksum = Ok(Some(VALID_DIGEST.to_string()));
    rig.db_app = Some(DbAppDouble::default());
    let mut faces = faces_of(&mut rig);
    assert_eq!(
        main_face(
            &args(&["h;1;u;p;d", "/list", "--ignore-sqlite-errors"]),
            &mut faces,
            &mut log
        ),
        EXIT_SUCCESS
    );
    // shouldStopOnError = false → ignoreSqliteErrors = true.
    assert_eq!(
        rig.merge.consolidations,
        vec![("/a.db".to_string(), true, false)]
    );
}
