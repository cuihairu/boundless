//! The BW-09-05-05 consolidate_dbs tool (`server/tools/consolidate_dbs/`).
//!
//! The reference binary is the consolidator driver the operator (or DBApp)
//! runs: it reads the unconsolidated secondary database registrations from
//! the primary's `bigworldSecondaryDatabases` table, has them transferred in
//! (via the `transfer_db` machinery), merges each into the primary through
//! the sqlite/MySQL consolidation engine, and clears the registrations. This
//! port carries the driver layer — `main`, `CommandLineParser` (the getopt
//! cluster with its NUL unknown-long-option quirk and the five-segment
//! `strtoul`-base-0 primary spec) and `ConsolidateDBsApp` (the orchestration,
//! verbatim messages and exit codes); the deep legs stay behind seams:
//! `ToolAppFace`/`PrimaryDatabase` (the MySQL world), `TransferLeg`
//! (listener + file receivers), `MergeLeg` (`SecondaryDatabase` +
//! `PrimaryDatabaseUpdateQueue`, the merge core whose row-level result math
//! lives in `lib/db_storage`), `ListFileReader`, `ConsolidateConfig`, the
//! `DbAppFace` status channel and the reporters' clock.
//!
//! Honest modelling notes:
//! - glibc `getopt_long` is emulated for exact-match long options, short
//!   clustering, `--` termination and GNU permutation; the unambiguous-prefix
//!   abbreviation and getopt's own stderr diagnostics (which bypass the
//!   logger in the reference too) stay unported;
//! - unknown long options surface as `'?'` with `optopt = 0`, so the
//!   reference's `"Unknown option -%c"` line carries a literal NUL — kept;
//! - `abort()`'s `dispatcher().breakProcessing()` is transport-level; the
//!   port keeps the `shouldAbort` flag flip that the messages observe;
//! - `START_MSG`/`ShutdownLogSuppressor` in `main` are console-facing
//!   startup/shutdown cosmetics outside the log face.

use std::collections::BTreeSet;

/// `EXIT_SUCCESS` / `EXIT_FAILURE` (`main.cpp` returns these per branch).
pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_FAILURE: i32 = 1;

/// The `puts(USAGE_MESSAGE)` text, byte for byte.
pub const USAGE_MESSAGE: &str = "\nUsage: consolidate_dbs [options]\n       consolidate_dbs [options] <primarydb_spec> <secondarydb_list> (when invoked from DBApp)\n\nOptions:\n --ignore-sqlite-errors    do not abort on sqlite read errors\n --clear                   clear unconsolidated secondary database info\n --list                    list unconsolidated secondary database info\n -v, --verbose             verbose output\n -h, --help                print usage\n";

/// `getSecondaryDBInfos`' query (`consolidate_dbs_app.cpp:303`).
pub const SELECT_SECONDARY_DATABASES: &str = "SELECT ip, location FROM bigworldSecondaryDatabases";
/// `checkPrimaryDBEntityDefsMatchInternal`'s query (`:461`).
pub const SELECT_ENTITY_DEFS_CHECKSUM: &str = "SELECT checksum FROM bigworldEntityDefsChecksum";
/// `clearSecondaryDBEntries`' statement (`:510`).
pub const DELETE_SECONDARY_DATABASES: &str = "DELETE FROM bigworldSecondaryDatabases";

/// SIGINT / SIGHUP, the two signals `init` enables handlers for.
pub const SIGINT: i32 = 2;
pub const SIGHUP: i32 = 1;

/// `DBConfig::ConnectionInfo`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// `host`.
    pub host: String,
    /// `port`.
    pub port: u16,
    /// `username`.
    pub username: String,
    /// `password`.
    pub password: String,
    /// `database`.
    pub database: String,
}

/// `SecondaryDBInfo`: the registration row, with `hostIP` as `htonl` left it
/// (the column stores a little-endian-written u32; the reference byte-swaps
/// it back before `Address` formats it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryDBInfo {
    /// `hostIP`.
    pub host_ip: u32,
    /// `location`.
    pub location: String,
}

/// `htonl` on a little-endian host: the reference's byte swap.
pub fn htonl(value: u32) -> u32 {
    value.swap_bytes()
}

/// `Mercury::Address::ipAsString` (`basictypes.cpp:119-131`): the reference
/// `ntohl`s the stored value first, then reads four big-endian octets — so
/// the printed string shows the octets of the raw DB column value (the
/// `htonl` in `getSecondaryDBInfos` and the `ntohl` here cancel).
pub fn ip_as_string(ip: u32) -> String {
    let hip = ip.swap_bytes();
    format!(
        "{}.{}.{}.{}",
        hip >> 24 & 0xff,
        hip >> 16 & 0xff,
        hip >> 8 & 0xff,
        hip & 0xff
    )
}

/// `MD5::Digest` — the opaque entity-defs checksum. `unquote` is the
/// reference's verbatim math: a 32-character check, then nibble decoding
/// with no hex validation (`c > '9' ? c - ('A'-10) : c - '0'`), so junk
/// characters decode to junk bytes rather than failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digest([u8; 16]);

impl Digest {
    /// The all-zero digest.
    pub fn cleared() -> Self {
        Digest([0; 16])
    }

    /// `MD5::Digest::quote` — uppercase hex via the reference's
    /// `"0123456789ABCDEF"` table (`md5.cpp:484-497`).
    pub fn quote(&self) -> String {
        const HEX_TABLE: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = String::with_capacity(32);
        for byte in self.0 {
            out.push(HEX_TABLE[(byte >> 4) as usize] as char);
            out.push(HEX_TABLE[(byte & 0x0f) as usize] as char);
        }
        out
    }

    /// `MD5::Digest::unquote`: `Some` only for 32 characters; the nibble
    /// decode keeps the reference's unvalidated arithmetic.
    pub fn unquote(quoted: &str) -> Option<Self> {
        let bytes = quoted.as_bytes();
        if bytes.len() != 32 {
            return None;
        }
        let mut digest = [0u8; 16];
        for (i, slot) in digest.iter_mut().enumerate() {
            let high = unquote_nibble(bytes[i * 2]);
            let low = unquote_nibble(bytes[i * 2 + 1]);
            *slot = high << 4 | low;
        }
        Some(Digest(digest))
    }
}

fn unquote_nibble(c: u8) -> u8 {
    if c > b'9' {
        c.wrapping_sub(b'A' - 10)
    } else {
        c.wrapping_sub(b'0')
    }
}

/// C `strtoul` with base 0: leading whitespace skipped, optional sign (a
/// negative wraps like the reference's `-1` → `ULONG_MAX`), `0x` hex /
/// leading-zero octal / decimal. Returns the value and the endptr index —
/// `0` when no digit was converted (only an empty string then passes the
/// caller's endptr check, yielding port 0), else the index just past the
/// digits.
pub fn strtoul_base0(text: &str) -> (u64, usize) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let negative = match bytes.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };

    // Base detection: "0x" followed by a hex digit → 16; a leading '0' → 8
    // (the zero itself is the first digit); otherwise decimal.
    let (base, digits_start, mut digit_count) = if i + 2 < bytes.len()
        && bytes[i] == b'0'
        && matches!(bytes[i + 1], b'x' | b'X')
        && bytes[i + 2].is_ascii_hexdigit()
    {
        (16u64, i + 2, 0usize)
    } else if i < bytes.len() && bytes[i] == b'0' {
        // The leading zero is itself the first octal digit.
        (8u64, i, 0usize)
    } else {
        (10u64, i, 0usize)
    };

    let mut value: u64 = 0;
    for &b in &bytes[digits_start..] {
        let digit = match (b as char).to_digit(base as u32) {
            Some(d) => d as u64,
            None => break,
        };
        value = value.wrapping_mul(base).wrapping_add(digit);
        digit_count += 1;
    }

    let endptr = if digit_count == 0 {
        0
    } else {
        digits_start + digit_count
    };
    let value = if negative {
        value.wrapping_neg()
    } else {
        value
    };
    (value, endptr)
}

/// The `std::ifstream` face for the secondary database list file.
pub trait ListFileReader {
    /// Open + getline loop; `None` when the file will not open. A trailing
    /// newline yields no extra entry (the empty final getline is skipped).
    fn read_lines(&mut self, path: &str) -> Option<Vec<String>>;
}

/// The `DBConfig`/`BWResource` face.
pub trait ConsolidateConfig {
    /// `BWConfig::isBad()` after `BWResource::init`/`BWConfig::init`.
    fn bw_config_is_bad(&self) -> bool;

    /// `DBConfig::connectionInfo()` — the parser's starting primary info.
    fn default_connection_info(&self) -> ConnectionInfo;

    /// `DBConfig::get().mysql.numConnections()`.
    fn num_connections(&self) -> u32;

    /// `DBConfig::get().secondaryDB.consolidation.directory()`.
    fn consolidation_directory(&self) -> String;

    /// The same setting's XML path (`…directory.path()` in the error).
    fn consolidation_directory_setting_path(&self) -> String;

    /// `BWResource::resolveToAbsolutePath` → `IFileSystem::FT_DIRECTORY`.
    fn resolve_is_directory(&mut self, path: &str) -> bool;
}

/// The `DatabaseToolApp` world: logger/watcher/MySQL connection setup.
pub trait ToolAppFace {
    /// `DatabaseToolApp::init( appName, mechanism, isVerbose, shouldLock,
    /// connectionInfo )`.
    fn init_tool(
        &mut self,
        app_name: &str,
        mechanism: &str,
        is_verbose: bool,
        should_lock: bool,
        info: &ConnectionInfo,
    ) -> bool;

    /// `enableSignalHandler( SIGINT / SIGHUP )`.
    fn enable_signal_handler(&mut self, sig_num: i32);

    /// `watcherNub().udpSocket().getLocalAddress().ip`.
    fn local_ip(&self) -> u32;
}

/// The primary database's MySQL face.
pub trait PrimaryDatabase {
    /// `SELECT ip, location FROM bigworldSecondaryDatabases`; `Err` is the
    /// caught exception text.
    fn select_secondary_databases(&mut self) -> Result<Vec<(u32, String)>, String>;

    /// `SELECT checksum FROM bigworldEntityDefsChecksum`; `Ok(None)` is the
    /// empty result set, `Err` the exception text.
    fn select_entity_defs_checksum(&mut self) -> Result<Option<String>, String>;

    /// `DELETE FROM bigworldSecondaryDatabases` inside a transaction;
    /// `Ok` carries `affectedRows()`, `Err` the exception text.
    fn delete_secondary_databases(&mut self) -> Result<u32, String>;
}

/// The loaded entity definitions' digest face.
pub trait EntityDefs {
    /// `entityDefs().getPersistentPropertiesDigest()`.
    fn persistent_properties_digest(&self) -> Digest;
}

/// The `DBApp` status channel (present only when reporting was requested).
pub trait DbAppFace {
    /// `DBApp::init` — finding the DBApp host.
    fn init(&mut self) -> bool;

    /// `DBApp::setStatus`.
    fn set_status(&mut self, status: &str);
}

/// `DBAppStatusReporter`.
pub trait StatusReporter {
    /// `onStatus`.
    fn on_status(&mut self, status: &str);
}

/// The reporters' clock (`timestamp()` / `stampsPerSecondD()`).
pub trait ClockFace {
    /// `timestamp()`.
    fn timestamp(&self) -> u64;

    /// `stampsPerSecondD()`.
    fn stamps_per_second(&self) -> u64;
}

/// The transfer world: `TcpListener` + `FileReceiverMgr` +
/// `TransferDBProcess` + the dispatcher wait.
pub trait TransferLeg {
    /// `connectionsListener.init( 0, internalIP, count )` +
    /// `getBoundAddr` — `Some("ip:port")` or `None` on failure.
    fn listen(&mut self, internal_ip: u32, count: usize) -> Option<String>;

    /// `transferDB.transfer( hostIP, location )` for one secondary.
    fn request_transfer(&mut self, host_ip: u32, location: &str) -> bool;

    /// `dispatcher().processUntilBreak()` + `fileReceiverMgr.finished()`.
    /// The reporter is the one the manager drives.
    fn wait_until_done(&mut self, progress: &mut FileTransferProgressReporter<'_, '_>) -> bool;

    /// `fileReceiverMgr.receivedFilePaths()`.
    fn received_file_paths(&self) -> Vec<String>;

    /// `fileReceiverMgr.cleanUpRemoteFiles( errors )`.
    fn clean_up_remote_files(&mut self, errors: &DBConsolidatorErrors);
}

/// The merge world: `SecondaryDatabase` + `PrimaryDatabaseUpdateQueue`.
pub trait MergeLeg {
    /// `primaryDBQueue.init( connectionInfo, entityDefs )`.
    fn init_queue(&mut self, info: &ConnectionInfo) -> bool;

    /// `secondaryDB.init( filePath )`.
    fn init_secondary_db(&mut self, path: &str) -> bool;

    /// `secondaryDB.getChecksumDigest` — `None` on failure.
    fn secondary_db_digest(&mut self) -> Option<String>;

    /// `secondaryDB.consolidate( queue, reporter, ignoreSqliteErrors,
    /// shouldAbort )` — the abort flag is passed by reference and may be
    /// set by the engine's sqlite loop.
    fn consolidate(
        &mut self,
        path: &str,
        progress: &mut ConsolidationProgressReporter<'_, '_>,
        ignore_sqlite_errors: bool,
        should_abort: &mut bool,
    ) -> bool;
}

/// The seams `main_face` and the app drive.
pub struct Faces<'a> {
    /// `DBConfig`/`BWResource`.
    pub config: &'a mut dyn ConsolidateConfig,
    /// `DatabaseToolApp` world.
    pub tool_app: &'a mut dyn ToolAppFace,
    /// Primary database queries.
    pub primary_db: &'a mut dyn PrimaryDatabase,
    /// Loaded entity definitions.
    pub entity_defs: &'a mut dyn EntityDefs,
    /// The `DBApp` status channel, when reporting was requested.
    pub db_app: Option<&'a mut dyn DbAppFace>,
    /// The file-transfer world.
    pub transfer: &'a mut dyn TransferLeg,
    /// The merge engine.
    pub merge: &'a mut dyn MergeLeg,
    /// The secondary list file reader.
    pub list_reader: &'a mut dyn ListFileReader,
    /// The reporters' clock (shared — only read).
    pub clock: &'a dyn ClockFace,
}

/// `DBConsolidatorErrors` — the consolidation error ledger. (`HACK_BACKTRACE`
/// fires on insert in the reference; the set itself is the observable.)
#[derive(Default)]
pub struct DBConsolidatorErrors {
    erroneous_secondary_dbs: BTreeSet<String>,
}

impl DBConsolidatorErrors {
    /// `addSecondaryDB`.
    pub fn add_secondary_db(&mut self, secondary_db_file_name: &str) {
        self.erroneous_secondary_dbs
            .insert(secondary_db_file_name.to_string());
    }

    /// `hasErrors`.
    pub fn has_errors(&self) -> bool {
        !self.erroneous_secondary_dbs.is_empty()
    }

    /// `secondaryDBHasError`.
    pub fn secondary_db_has_error(&self, secondary_db_file_name: &str) -> bool {
        self.erroneous_secondary_dbs
            .contains(secondary_db_file_name)
    }
}

/// `SluggishProgressReporter`'s gate (`sluggish_progress_reporter.hpp:29`):
/// `reportProgress` fires `reportProgressNow` only once the elapsed stamps
/// pass the interval.
fn sluggish_gate(last_report_time: &mut u64, report_interval: u64, clock: &dyn ClockFace) -> bool {
    let now = clock.timestamp();
    if now.wrapping_sub(*last_report_time) > report_interval {
        *last_report_time = now;
        return true;
    }
    false
}

/// `ConsolidationProgressReporter`
/// (`consolidation_progress_reporter.{hpp,cpp}` over
/// `SluggishProgressReporter`, half a second of stamps per report).
pub struct ConsolidationProgressReporter<'r, 'c> {
    reporter: &'r mut dyn StatusReporter,
    clock: &'c dyn ClockFace,
    report_interval: u64,
    last_report_time: u64,
    num_dbs: i32,
    done_dbs: i32,
    cur_db_name: String,
    num_entities_in_cur_db: i32,
    done_entities_in_cur_db: i32,
}

impl<'r, 'c> ConsolidationProgressReporter<'r, 'c> {
    /// Half a second of stamps is the default report interval. The reporter
    /// and clock lifetimes stay separate: the reporter is often a local
    /// forwarder, while the clock reaches the call through `Faces`.
    pub fn new(
        reporter: &'r mut dyn StatusReporter,
        num_dbs: i32,
        clock: &'c dyn ClockFace,
    ) -> Self {
        ConsolidationProgressReporter {
            report_interval: (0.5 * clock.stamps_per_second() as f64) as u64,
            reporter,
            clock,
            last_report_time: clock.timestamp(),
            num_dbs,
            done_dbs: 0,
            cur_db_name: String::new(),
            num_entities_in_cur_db: 0,
            done_entities_in_cur_db: 0,
        }
    }

    /// `reportProgressNow` — the verbatim status string.
    pub fn report_progress_now(&mut self) {
        // Generate string
        let status = format!(
            "Consolidating {} ({}/{} entities) ({}/{} databases)",
            self.cur_db_name,
            self.done_entities_in_cur_db,
            self.num_entities_in_cur_db,
            self.done_dbs,
            self.num_dbs
        );
        self.reporter.on_status(&status);
    }

    /// `reportProgress` — the sluggish gate.
    pub fn report_progress(&mut self) {
        if sluggish_gate(&mut self.last_report_time, self.report_interval, self.clock) {
            self.report_progress_now();
        }
    }

    /// `onStartConsolidateDB`.
    pub fn on_start_consolidate_db(&mut self, db_name: &str, num_entities: i32) {
        self.done_dbs += 1;
        self.cur_db_name = db_name.to_string();
        self.num_entities_in_cur_db = num_entities;
        self.done_entities_in_cur_db = 0;

        self.report_progress_now();
    }

    /// `onConsolidatedRow`.
    pub fn on_consolidated_row(&mut self) {
        self.done_entities_in_cur_db += 1;
        self.report_progress(); // SluggishProgressReporter method
    }
}

/// `FileTransferProgressReporter`
/// (`file_transfer_progress_reporter.{hpp,cpp}` over
/// `SluggishProgressReporter`; the constructor reports immediately).
pub struct FileTransferProgressReporter<'r, 'c> {
    reporter: &'r mut dyn StatusReporter,
    clock: &'c dyn ClockFace,
    report_interval: u64,
    last_report_time: u64,
    num_files: i32,
    started_files: i32,
    done_files: i32,
    num_bytes: i32,
    done_bytes: i32,
}

impl<'r, 'c> FileTransferProgressReporter<'r, 'c> {
    pub fn new(
        reporter: &'r mut dyn StatusReporter,
        num_files: i32,
        clock: &'c dyn ClockFace,
    ) -> Self {
        let mut out = FileTransferProgressReporter {
            report_interval: (0.5 * clock.stamps_per_second() as f64) as u64,
            reporter,
            clock,
            last_report_time: clock.timestamp(),
            num_files,
            started_files: 0,
            done_files: 0,
            num_bytes: 0,
            done_bytes: 0,
        };
        out.report_progress_now();
        out
    }

    /// `reportProgressNow` — the verbatim strings, including the reference's
    /// "Transfering" typo and the double space after "waiting for".
    pub fn report_progress_now(&mut self) {
        let status = if self.started_files < self.num_files {
            format!(
                "Data consolidation is waiting for  all secondary database file transfers to start ({}/{} started)",
                self.started_files, self.num_files
            )
        } else {
            format!(
                "Transfering secondary databases for consolidation ({}/{} bytes) ({}/{} files completed)",
                self.done_bytes, self.num_bytes, self.done_files, self.num_files
            )
        };
        self.reporter.on_status(&status);
    }

    /// `reportProgress` — the sluggish gate.
    pub fn report_progress(&mut self) {
        if sluggish_gate(&mut self.last_report_time, self.report_interval, self.clock) {
            self.report_progress_now();
        }
    }

    /// `onStartTransfer`.
    pub fn on_start_transfer(&mut self, num_bytes_in_file: i32) {
        self.started_files += 1;
        self.num_bytes += num_bytes_in_file;

        self.report_progress();
    }

    /// `onReceiveData`.
    pub fn on_receive_data(&mut self, num_bytes: i32) {
        self.done_bytes += num_bytes;
        self.report_progress();
    }

    /// `onFinishTransfer` — counts only; no status report.
    pub fn on_finish_transfer(&mut self) {
        self.done_files += 1;
    }
}

/// `CommandLineParser` (`command_line_parser.{hpp,cpp}`).
pub struct CommandLineParser {
    is_verbose: bool,
    should_clear: bool,
    should_list: bool,
    should_ignore_sqlite_errors: bool,
    had_non_option_args: bool,
    res_path: String,
    primary_database_info: ConnectionInfo,
    secondary_databases: Vec<String>,
}

impl CommandLineParser {
    /// The constructor seeds the primary info from `DBConfig::connectionInfo`.
    pub fn new(config: &dyn ConsolidateConfig) -> Self {
        CommandLineParser {
            is_verbose: false,
            should_clear: false,
            should_list: false,
            should_ignore_sqlite_errors: false,
            had_non_option_args: false,
            res_path: String::new(),
            primary_database_info: config.default_connection_info(),
            secondary_databases: Vec::new(),
        }
    }

    /// `isVerbose`.
    pub fn is_verbose(&self) -> bool {
        self.is_verbose
    }

    /// `shouldClear`.
    pub fn should_clear(&self) -> bool {
        self.should_clear
    }

    /// `shouldList`.
    pub fn should_list(&self) -> bool {
        self.should_list
    }

    /// `shouldIgnoreSqliteErrors`.
    pub fn should_ignore_sqlite_errors(&self) -> bool {
        self.should_ignore_sqlite_errors
    }

    /// `hadNonOptionArgs`.
    pub fn had_non_option_args(&self) -> bool {
        self.had_non_option_args
    }

    /// `resPath`.
    pub fn res_path(&self) -> &str {
        &self.res_path
    }

    /// `primaryDatabaseInfo`.
    pub fn primary_database_info(&self) -> &ConnectionInfo {
        &self.primary_database_info
    }

    /// `secondaryDatabases`.
    pub fn secondary_databases(&self) -> &[String] {
        &self.secondary_databases
    }

    /// `printUsage` — `puts(USAGE_MESSAGE)`.
    pub fn print_usage(&self, log: &mut Vec<String>) {
        log.push(USAGE_MESSAGE.to_string());
    }

    /// `init` (`command_line_parser.cpp:60-172`): the getopt_long cluster,
    /// then the positional rules.
    pub fn init(
        &mut self,
        args: &[String],
        list_reader: &mut dyn ListFileReader,
        log: &mut Vec<String>,
    ) -> bool {
        let mut ok = true;

        // The long option table, in declaration order.
        const LONG_OPTIONS: [(&str, char, bool); 6] = [
            ("res", 'r', true),
            ("verbose", 'v', false),
            ("help", 'h', false),
            ("clear", 'c', false),
            ("list", 'l', false),
            ("ignore-sqlite-errors", 'i', false),
        ];

        let mut non_options: Vec<String> = Vec::new();
        let mut arg_index = 1usize; // argv[0] is skipped
        let mut option_parsing_ended = false;

        while ok {
            // The next token getopt_long would return; None is -1.
            let arg = match args.get(arg_index) {
                Some(arg) => arg.clone(),
                None => break,
            };
            arg_index += 1;

            // Non-options are permuted to the tail (GNU getopt); "--" ends
            // option parsing; a lone "-" is a non-option.
            if option_parsing_ended || !arg.starts_with('-') || arg == "-" {
                non_options.push(arg);
                continue;
            }
            if arg == "--" {
                option_parsing_ended = true;
                continue;
            }

            if let Some(body) = arg.strip_prefix("--") {
                // Long options match exactly; the glibc unambiguous-prefix
                // abbreviation is unported.
                let (name, inline) = match body.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (body, None),
                };
                match LONG_OPTIONS.iter().find(|(long, _, _)| *long == name) {
                    Some(&(long, short, takes_arg)) => {
                        if takes_arg {
                            let value = match inline {
                                Some(value) => value,
                                None => match args.get(arg_index) {
                                    Some(next) => {
                                        arg_index += 1;
                                        next.clone()
                                    }
                                    None => {
                                        // The leading ':' in ":r:vh" makes
                                        // getopt report ':' with optopt set.
                                        log.push(format!("Missing argument for option -{short}"));
                                        ok = false;
                                        break;
                                    }
                                },
                            };
                            self.res_path = value;
                        } else if inline.is_some() {
                            // getopt prints its own stderr diagnostic here
                            // and reports '?' with optopt at the short.
                            log.push(format!("Unknown option -{short}"));
                            ok = false;
                            break;
                        } else {
                            match long {
                                "verbose" => self.is_verbose = true,
                                "help" => {
                                    self.print_usage(log);
                                    ok = false;
                                }
                                "clear" => self.should_clear = true,
                                "list" => self.should_list = true,
                                "ignore-sqlite-errors" => self.should_ignore_sqlite_errors = true,
                                _ => unreachable!(),
                            }
                        }
                    }
                    None => {
                        // Unknown long option: getopt reports '?' with
                        // optopt 0 — the reference's "-%c" carries a NUL.
                        log.push("Unknown option -\0".to_string());
                        ok = false;
                        break;
                    }
                }
                continue;
            }

            // Short cluster: each char is an option until 'r' swallows the
            // rest of the cluster (or the next argument).
            let cluster = arg[1..].to_string();
            for (position, c) in cluster.chars().enumerate() {
                if c == 'r' {
                    let rest: String = cluster.chars().skip(position + 1).collect();
                    let value = if !rest.is_empty() {
                        rest
                    } else {
                        match args.get(arg_index) {
                            Some(next) => {
                                arg_index += 1;
                                next.clone()
                            }
                            None => {
                                log.push("Missing argument for option -r".to_string());
                                ok = false;
                                break;
                            }
                        }
                    };
                    self.res_path = value;
                    break; // 'r' ends the cluster
                }
                match c {
                    'v' => self.is_verbose = true,
                    'h' => {
                        self.print_usage(log);
                        ok = false;
                    }
                    _ => {
                        log.push(format!("Unknown option -{c}"));
                        ok = false;
                    }
                }
                if !ok {
                    break;
                }
            }
        }

        if !ok {
            return false;
        }

        // We currently only support specifying:
        // 1) Nothing on the command-line
        // OR
        // 2) The primary and a file containing all secondary database paths.
        // We can't make the paths arguments as there can arbitrarily many and
        // we may exceed command line limit.

        if non_options.is_empty() {
            // No args, we're done!
            return true;
        }

        if non_options.len() < 2 {
            log.push("Primary database specified, but no path to secondary database list was specified\n".to_string());
            self.print_usage(log);
            return false;
        }

        if non_options.len() > 2 {
            // The reference's WARNING carries no trailing newline.
            log.push("Extraneous arguments after secondary database list path ignored".to_string());
        }

        self.had_non_option_args = true;

        let primary_spec = non_options[0].clone();
        let secondary_database_list_path = non_options[1].clone();

        if self.should_clear || self.should_list {
            log.push(format!(
                "consolidate_dbs: The --{} option does not take additional arguments\n",
                if self.should_clear { "clear" } else { "list" }
            ));
            // Don't parse, we use the bw.xml for primary database info.
            return true;
        }

        self.parse_primary_database_info(&primary_spec, log)
            && self.read_secondary_database_list(&secondary_database_list_path, list_reader, log)
    }

    /// `parsePrimaryDatabaseInfo` (`command_line_parser.cpp:174-220`): five
    /// `;`-separated segments, the port parsed with base-0 `strtoul` and the
    /// reference's endptr semantics (an empty segment parses as port 0).
    fn parse_primary_database_info(
        &mut self,
        command_line_spec: &str,
        log: &mut Vec<String>,
    ) -> bool {
        let components: Vec<&str> = command_line_spec.split(';').collect();

        if components.len() != 5 {
            log.push("Primary database argument must be in the form <host>;<port>;<username>;<password>;<database>\n".to_string());
            return false;
        }

        self.primary_database_info.host = components[0].to_string();

        let (port, endptr) = strtoul_base0(components[1]);

        if endptr != components[1].len() {
            log.push(format!(
                "Could not parse database port :{}\n",
                components[1]
            ));
            return false;
        } else if port >= 1u64 << 16 {
            log.push(format!(
                "Primary database port is out-of-range: {}\n",
                components[1]
            ));
            return false;
        }

        self.primary_database_info.port = port as u16;

        self.primary_database_info.username = components[2].to_string();
        self.primary_database_info.password = components[3].to_string();
        self.primary_database_info.database = components[4].to_string();

        true
    }

    /// `readSecondaryDatabaseList` (`command_line_parser.cpp:228-252`).
    fn read_secondary_database_list(
        &mut self,
        path: &str,
        list_reader: &mut dyn ListFileReader,
        log: &mut Vec<String>,
    ) -> bool {
        let lines = match list_reader.read_lines(path) {
            Some(lines) => lines,
            None => {
                log.push(format!(
                    "Could not open secondary database list at \"{path}\"\n"
                ));
                return false;
            }
        };

        for secondary_database in lines {
            if !secondary_database.is_empty() {
                self.secondary_databases.push(secondary_database);
            }
        }

        true
    }
}

/// `ConsolidateDBsApp` (`consolidate_dbs_app.{hpp,cpp}`).
pub struct ConsolidateDBsApp<'a> {
    /// The message log (the port's logger sink).
    pub log: &'a mut Vec<String>,
    internal_ip: u32,
    db_app_checked: bool,
    connection_info: ConnectionInfo,
    consolidation_dir: String,
    /// `DBConsolidatorErrors`.
    pub consolidation_errors: DBConsolidatorErrors,
    should_stop_on_error: bool,
    /// Flag for aborting our wait loop.
    pub should_abort: bool,
}

impl<'a> ConsolidateDBsApp<'a> {
    /// The constructor (`consolidationDir_` starts at `"/tmp/"`).
    pub fn new(log: &'a mut Vec<String>, should_stop_on_error: bool) -> Self {
        ConsolidateDBsApp {
            log,
            internal_ip: 0,
            db_app_checked: false,
            connection_info: ConnectionInfo::default(),
            consolidation_dir: "/tmp/".to_string(),
            consolidation_errors: DBConsolidatorErrors::default(),
            should_stop_on_error,
            should_abort: false,
        }
    }

    /// `init` (`consolidate_dbs_app.cpp:84-137`).
    pub fn init(
        &mut self,
        faces: &mut Faces,
        is_verbose: bool,
        should_report_to_db_app: bool,
        should_lock: bool,
        primary_db_connection_info: &ConnectionInfo,
    ) -> bool {
        if !faces.tool_app.init_tool(
            "ConsolidateDBs",
            "consolidate_dbs",
            is_verbose,
            should_lock,
            primary_db_connection_info,
        ) {
            return false;
        }

        faces.tool_app.enable_signal_handler(SIGINT);
        faces.tool_app.enable_signal_handler(SIGHUP);

        self.internal_ip = faces.tool_app.local_ip();

        if should_report_to_db_app {
            // No DBApp found!
            let db_app_ready = match faces.db_app.as_mut() {
                Some(db_app) => db_app.init(),
                None => false,
            };
            if !db_app_ready {
                return false;
            }
        }
        self.db_app_checked = should_report_to_db_app;

        self.log.push(format!(
            "ConsolidateDBsApp: Connected to primary database: host={}:{}, username={}, database={}\n",
            primary_db_connection_info.host,
            primary_db_connection_info.port,
            primary_db_connection_info.username,
            primary_db_connection_info.database
        ));

        self.connection_info = primary_db_connection_info.clone();

        self.consolidation_dir = faces.config.consolidation_directory();

        if !faces.config.resolve_is_directory(&self.consolidation_dir) {
            self.log.push(format!(
                "ConsolidateDBsApp::init: Configuration setting <{}> specifies a non-existent directory: {}\n",
                faces.config.consolidation_directory_setting_path(),
                self.consolidation_dir
            ));
            return false;
        }

        true
    }

    /// Whether a DBApp status channel was wired up by `init`.
    pub fn reports_to_db_app(&self) -> bool {
        self.db_app_checked
    }

    /// `transferAndConsolidate` (`consolidate_dbs_app.cpp:141-190`).
    pub fn transfer_and_consolidate(&mut self, faces: &mut Faces) -> bool {
        // Get the secondary DB info from primary database.
        let secondary_dbs = match self.get_secondary_db_infos(faces) {
            Some(infos) => infos,
            None => return false,
        };

        if secondary_dbs.is_empty() {
            self.log
                .push("ConsolidateDBsApp::transferAndConsolidate: No secondary databases to consolidate\n".to_string());
            return false;
        }

        // The transfer reporter is wired to the DBApp channel for its
        // lifetime; the forwarder owns the channel for that stretch.
        let mut status_forwarder = AppStatusForwarder {
            db_app: std::mem::take(&mut faces.db_app),
        };
        let transferred = {
            let mut progress_reporter = FileTransferProgressReporter::new(
                &mut status_forwarder,
                secondary_dbs.len() as i32,
                faces.clock,
            );
            self.transfer_secondary_dbs(faces, &secondary_dbs, &mut progress_reporter)
        };

        // The reporter's scope ended; hand the channel back to the bundle.
        faces.db_app = status_forwarder.release();

        if !transferred {
            self.log.push(
                "ConsolidateDBsApp::transferAndConsolidate: Failed to transfer secondaryDBs\n"
                    .to_string(),
            );
            return false;
        }

        // Consolidate databases
        let db_file_paths = faces.transfer.received_file_paths();

        let consolidated = self.consolidate_secondary_dbs(faces, &db_file_paths);

        if !consolidated {
            self.log.push(
                "ConsolidateDBsApp::transferAndConsolidate: Failed to consolidate secondaryDBs\n"
                    .to_string(),
            );
            return false;
        }

        faces
            .transfer
            .clean_up_remote_files(&self.consolidation_errors);

        self.clear_secondary_db_entries(faces, None);

        self.log.push(
            "ConsolidateDBsApp::transferAndConsolidate: Completed successfully\n".to_string(),
        );

        true
    }

    /// `transferSecondaryDBs` (`consolidate_dbs_app.cpp:196-242`).
    fn transfer_secondary_dbs(
        &mut self,
        faces: &mut Faces,
        secondary_dbs: &[SecondaryDBInfo],
        progress_reporter: &mut FileTransferProgressReporter,
    ) -> bool {
        let internal_ip = self.internal_ip;

        // Start listening for incoming connections; a failed getBoundAddr
        // fails silently.
        let bound_addr = match faces.transfer.listen(internal_ip, secondary_dbs.len()) {
            Some(addr) => addr,
            None => return false,
        };

        self.log.push(format!(
            "ConsolidateDBsApp::transferSecondaryDBs: Listening on: {bound_addr}\n"
        ));

        // Start remote file transfer service
        for secondary_db_info in secondary_dbs {
            if !faces
                .transfer
                .request_transfer(secondary_db_info.host_ip, &secondary_db_info.location)
            {
                self.should_abort = true;
                return false;
            }
        }

        // Wait for file transfer to complete
        faces.transfer.wait_until_done(progress_reporter)
    }

    /// `consolidateSecondaryDBs` (`consolidate_dbs_app.cpp:249-291`).
    pub fn consolidate_secondary_dbs(&mut self, faces: &mut Faces, file_paths: &[String]) -> bool {
        let num_connections = faces.config.num_connections();

        self.log.push(format!(
            "ConsolidateDBsApp::consolidateSecondaryDBs: Number of connections = {num_connections}.\n"
        ));

        let connection_info = self.connection_info.clone();
        if !faces.merge.init_queue(&connection_info) {
            self.log.push(
                "ConsolidateDBsApp::consolidateSecondaryDBs: Failed to initialize primaryDBQueue\n"
                    .to_string(),
            );
            return false;
        }

        // One progress reporter for the whole run, wired to the DBApp
        // channel for its lifetime (in the reference the transfer path has
        // no DBApp anyway, since reporting implies the file-list path).
        let mut status_forwarder = AppStatusForwarder {
            db_app: std::mem::take(&mut faces.db_app),
        };
        let mut failed = false;
        {
            let mut progress_reporter = ConsolidationProgressReporter::new(
                &mut status_forwarder,
                file_paths.len() as i32,
                faces.clock,
            );
            for file_path in file_paths {
                if !self.consolidate_secondary_db(faces, file_path, &mut progress_reporter) {
                    failed = true;
                    break;
                }
            }
        }

        // The reporter's scope ended; hand the channel back to the bundle.
        faces.db_app = status_forwarder.release();

        if failed {
            if self.should_abort {
                self.log.push(
                    "ConsolidateDBsApp::consolidateSecondaryDBs: Data consolidation was aborted\n"
                        .to_string(),
                );
            } else {
                self.log
                    .push("ConsolidateDBsApp::consolidateSecondaryDBs: Some entities were not consolidated. Data consolidation must be re-run after errors have been corrected.\n".to_string());
            }
            return false;
        }

        true
    }

    /// `getSecondaryDBInfos` (`consolidate_dbs_app.cpp:297-326`).
    fn get_secondary_db_infos(&mut self, faces: &mut Faces) -> Option<Vec<SecondaryDBInfo>> {
        match faces.primary_db.select_secondary_databases() {
            Ok(rows) => Some(
                rows.into_iter()
                    .map(|(ip, location)| SecondaryDBInfo {
                        host_ip: htonl(ip),
                        location,
                    })
                    .collect(),
            ),
            Err(e) => {
                self.log.push(format!(
                    "ConsolidateDBsApp::getSecondaryDBInfos: Failed to get secondary DB information from primary database: {e}\n"
                ));
                None
            }
        }
    }

    /// `consolidateSecondaryDB` (`consolidate_dbs_app.cpp:333-371`).
    fn consolidate_secondary_db(
        &mut self,
        faces: &mut Faces,
        file_path: &str,
        progress_reporter: &mut ConsolidationProgressReporter,
    ) -> bool {
        if !faces.merge.init_secondary_db(file_path) {
            return false;
        }

        self.log.push(format!(
            "ConsolidateDBsApp::consolidateSecondaryDB: Consolidating '{file_path}'\n"
        ));

        let secondary_db_digest = match faces.merge.secondary_db_digest() {
            Some(digest) => digest,
            None => return false,
        };

        if !self.check_entity_defs_digest_match(faces, &secondary_db_digest) {
            self.log.push(format!(
                "ConsolidateDBsApp::consolidateSecondaryDB: {file_path} failed entity digest check\n"
            ));
            return false;
        }

        let should_stop_on_error = self.should_stop_on_error;
        // The engine receives the abort flag by reference and may set it.
        let mut should_abort = self.should_abort;
        let merged = faces.merge.consolidate(
            file_path,
            progress_reporter,
            !should_stop_on_error,
            &mut should_abort,
        );
        self.should_abort = should_abort;

        if !merged {
            self.consolidation_errors.add_secondary_db(file_path);
            return false;
        }

        true
    }

    /// `checkEntityDefsDigestMatch` (`consolidate_dbs_app.cpp:378-390`).
    fn check_entity_defs_digest_match(&mut self, faces: &mut Faces, quoted_digest: &str) -> bool {
        let digest = match Digest::unquote(quoted_digest) {
            Some(digest) => digest,
            None => {
                self.log.push(
                    "ConsolidateDBsApp::checkEntityDefsDigestMatch: Not a valid MD5 digest\n"
                        .to_string(),
                );
                return false;
            }
        };

        faces.entity_defs.persistent_properties_digest() == digest
    }

    /// `onStatus` (`consolidate_dbs_app.cpp:396-402`).
    pub fn on_status(&mut self, faces: &mut Faces, status: &str) {
        if let Some(db_app) = faces.db_app.as_mut() {
            db_app.set_status(status);
        }
    }

    /// `onSignalled` (`consolidate_dbs_app.cpp:408-424`).
    pub fn on_signalled(&mut self, sig_num: i32) {
        match sig_num {
            SIGINT | SIGHUP => {
                self.log
                    .push("ConsolidateDBsApp::onSignalled: aborting consolidation\n".to_string());
                self.abort();
            }
            _ => {}
        }
    }

    /// `checkPrimaryDBEntityDefsMatch` (`consolidate_dbs_app.cpp:431-448`).
    pub fn check_primary_db_entity_defs_match(&mut self, faces: &mut Faces) -> bool {
        if !self.check_primary_db_entity_defs_match_internal(faces) {
            self.log
                .push("ConsolidateDBsApp::init: Our entity definitions do not match the ones used by the primary database\nDatabase consolidation should be run before making changes to entity definitions. Changing entity definitions potentially invalidates unconsolidated data.\nRun \"consolidate_dbs --clear\" to allow the server to run without doing data consolidation. Unconsolidated data will be lost.\n".to_string());
            return false;
        }

        true
    }

    /// `checkPrimaryDBEntityDefsMatchInternal` (`:455-487`). The failure
    /// messages keep the reference's `checkEntityDefsMatch` (no `Internal`)
    /// spelling.
    fn check_primary_db_entity_defs_match_internal(&mut self, faces: &mut Faces) -> bool {
        match faces.primary_db.select_entity_defs_checksum() {
            Ok(Some(check_sum)) => self.check_entity_defs_digest_match(faces, &check_sum),
            Ok(None) => {
                self.log.push(
                    "ConsolidateDBsApp::checkEntityDefsMatch: Checksum table is empty\n"
                        .to_string(),
                );
                false
            }
            Err(e) => {
                self.log.push(format!(
                    "ConsolidateDBsApp::checkEntityDefsMatch: Failed to retrieve the primary database entity definition checksum: {e}\n"
                ));
                false
            }
        }
    }

    /// `abort` (`consolidate_dbs_app.cpp:492-497`). The dispatcher break is
    /// transport-level; the flag is what the port's messages observe.
    pub fn abort(&mut self) {
        self.should_abort = true;
    }

    /// `clearSecondaryDBEntries` (`consolidate_dbs_app.cpp:504-536`).
    pub fn clear_secondary_db_entries(
        &mut self,
        faces: &mut Faces,
        p_num_entries_cleared: Option<&mut u32>,
    ) -> bool {
        match faces.primary_db.delete_secondary_databases() {
            Ok(num_entries_cleared) => {
                if let Some(slot) = p_num_entries_cleared {
                    *slot = num_entries_cleared;
                }
                self.log.push(format!(
                    "ConsolidateDBsApp::clearSecondaryDBEntries: Cleared {} entries from {}:{} ({})\n",
                    num_entries_cleared,
                    self.connection_info.host,
                    self.connection_info.port,
                    self.connection_info.database
                ));
                true
            }
            // The reference's catch prints with no trailing newline.
            Err(e) => {
                self.log
                    .push(format!("ConsolidateDBsApp::clearSecondaryDBEntries: {e}"));
                false
            }
        }
    }

    /// `printDatabases` (`consolidate_dbs_app.cpp:542-568`).
    pub fn print_databases(&mut self, faces: &mut Faces) -> bool {
        let infos = match self.get_secondary_db_infos(faces) {
            Some(infos) => infos,
            None => return false,
        };

        self.log.push("\n".to_string());
        self.log.push(format!(
            "Secondary databases to consolidate = {}\n",
            infos.len()
        ));

        for info in &infos {
            // %15s right-aligns the dotted quad.
            self.log.push(format!(
                "{:>15} {}\n",
                ip_as_string(info.host_ip),
                info.location
            ));
        }

        true
    }
}

/// Forwards status strings into the `DBApp` channel when one was wired up
/// (the reference passes `*this` — the app is its own reporter).
struct AppStatusForwarder<'a> {
    db_app: Option<&'a mut dyn DbAppFace>,
}

impl StatusReporter for AppStatusForwarder<'_> {
    fn on_status(&mut self, status: &str) {
        if let Some(db_app) = self.db_app.as_mut() {
            db_app.set_status(status);
        }
    }
}

impl<'a> AppStatusForwarder<'a> {
    /// Hands the channel back to the bundle (`&'a mut` is invariant, so the
    /// forwarder must own the reference outright — a reborrow could not be
    /// stored into the `Option<&'a mut dyn>` field).
    fn release(self) -> Option<&'a mut dyn DbAppFace> {
        self.db_app
    }
}

/// `main` (`main.cpp:47-125`).
pub fn main_face(args: &[String], faces: &mut Faces, log: &mut Vec<String>) -> i32 {
    // DebugFilter filterThreshold(WARNING) / Allocator / BWResource /
    // BWConfig startup live behind the config face.

    if faces.config.bw_config_is_bad() {
        return EXIT_SUCCESS;
    }

    let mut options = CommandLineParser::new(faces.config);
    if !options.init(args, faces.list_reader, log) {
        log.push("Failed to parse command line options.\n".to_string());
        return EXIT_FAILURE;
    }

    let should_report_to_db_app = options.had_non_option_args();

    // TODO: Scalable DB, now only acquire lock when do --clear action,
    // after full DBApp sharding implementation, consolidate_dbs should
    // acquire lock by default again.
    let should_lock = options.should_clear();

    // ShutdownLogSuppressor / START_MSG are console-facing cosmetics.

    let mut app = ConsolidateDBsApp::new(log, !options.should_ignore_sqlite_errors());

    if !app.init(
        faces,
        options.is_verbose(),
        should_report_to_db_app,
        should_lock,
        options.primary_database_info(),
    ) {
        return EXIT_FAILURE;
    }

    if options.should_clear() {
        if app.clear_secondary_db_entries(faces, None) {
            return EXIT_SUCCESS;
        } else {
            app.log
                .push("Failed to clear secondary databases\n".to_string());
            return EXIT_FAILURE;
        }
    }

    if options.should_list() {
        if app.print_databases(faces) {
            return EXIT_SUCCESS;
        } else {
            app.log
                .push("Failed to list secondary databases\n".to_string());
            return EXIT_FAILURE;
        }
    }

    if !app.check_primary_db_entity_defs_match(faces) {
        return EXIT_FAILURE;
    }

    if options.had_non_option_args() {
        // We were run with the secondary databases already transferred by
        // the snapshot_helper and are specified in a file list, the path to
        // which is specified on the command line.
        let secondary_databases = options.secondary_databases().to_vec();
        if app.consolidate_secondary_dbs(faces, &secondary_databases) {
            EXIT_SUCCESS
        } else {
            EXIT_FAILURE
        }
    } else {
        // We were run with no specific primary database or secondary
        // databases specified, the consolidator must transfer those that
        // are registered in the primary database, before consolidating.
        if app.transfer_and_consolidate(faces) {
            EXIT_SUCCESS
        } else {
            EXIT_FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
