//! The `consolidate_dbs` tool face — `server/tools/consolidate_dbs`
//! (BW-09-05-05).
//!
//! Three faces of the reference tool:
//!
//! 1. **The command line** (`command_line_parser.{hpp,cpp}`): the
//!    `--res/-r`, `--verbose/-v`, `--help/-h`, `--clear/-c`, `--list/-l`,
//!    and `--ignore-sqlite-errors/-i` options, the
//!    `<primarydb_spec> <secondarydb_list>` positional pair DBApp invokes
//!    the tool with, and the primary spec's
//!    `<host>;<port>;<username>;<password>;<database>` shape.
//! 2. **The primary-database ledger** (`consolidate_dbs_app.cpp`): the
//!    `--clear` and `--list` actions over `bigworldSecondaryDatabases`,
//!    and the `bigworldEntityDefsChecksum` gate that refuses to run when
//!    the loaded definitions no longer match the primary's.
//! 3. **The file-list consolidation loop** (`consolidateSecondaryDBs`):
//!    per file — open, checksum, digest gate, merge through the
//!    BW-09-03-03 engine — with the error ledger
//!    (`db_consolidator_errors.hpp`) and the two failure warnings.
//!
//! Out of scope, as recorded: the `transferAndConsolidate` leg that pulls
//! the registered secondaries over the network (a Mercury TCP listener
//! plus `TransferDB` child processes) — [`ConsolidationSession::
//! transfer_and_consolidate`] stays with the caller, and the concrete
//! [`StorageConsolidator`] refuses it. That is the only shape the
//! reference supports without a DBApp driving it.

use std::collections::{BTreeSet, HashMap};

use entitydef::digest::Md5;
use foundation::ids::EntityTypeId;

use crate::consolidate::consolidate_database;
use crate::secondary::{SecondaryDatabase, StreamTypeDef};
use crate::storage::EntityStorage;

/// The `EXIT_SUCCESS`/`EXIT_FAILURE` pair `main` returns.
pub const SUCCESS: i32 = 0;
/// The `EXIT_FAILURE` half of the pair.
pub const FAILURE: i32 = 1;

/// The usage text `printUsage` `puts`es to stdout
/// (`command_line_parser.cpp:31-41`).
pub const USAGE_MESSAGE: &str = "\
\nUsage: consolidate_dbs [options]
       consolidate_dbs [options] <primarydb_spec> <secondarydb_list> (when invoked from DBApp)

Options:
 --ignore-sqlite-errors    do not abort on sqlite read errors
 --clear                   clear unconsolidated secondary database info
 --list                    list unconsolidated secondary database info
 -v, --verbose             verbose output
 -h, --help                print usage
";

// -----------------------------------------------------------------------------
// Section: the command line (`command_line_parser.{hpp,cpp}`)
// -----------------------------------------------------------------------------

/// `DBConfig::ConnectionInfo` — the primary database coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionInfo {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: String,
}

/// Everything `CommandLineParser::init` gathers. The defaults mirror the
/// constructor: all flags false, the primary database left at the
/// caller-supplied `DBConfig::connectionInfo()` default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandLineOptions {
    pub is_verbose: bool,
    pub should_clear: bool,
    pub should_list: bool,
    pub should_ignore_sqlite_errors: bool,
    pub had_non_option_args: bool,
    pub res_path: String,
    pub primary_database_info: ConnectionInfo,
    pub secondary_databases: Vec<String>,
}

/// `CommandLineParser::init` (`command_line_parser.cpp:57-166`): parse the
/// option list, then the optional `<primarydb_spec> <secondarydb_list>`
/// positional pair. `args` excludes `argv[0]`. Messages land in `out` the
/// way the reference's ERROR_MSG/WARNING_MSG do; `None` is `init`
/// returning false (which `main` turns into the parse-failure exit).
///
/// The reference parses with GNU `getopt_long`, which also accepts
/// unambiguous long-option prefixes (`--ver`); the port takes the exact
/// names only — recorded as a deviation. Everything else `getopt_long`
/// does is reproduced: bundled shorts (`-vh`), attached option values
/// (`-rX`, `--res=X`), the `--` terminator, and options appearing after
/// positional arguments.
pub fn parse_command_line(
    args: &[String],
    default_primary: ConnectionInfo,
    out: &mut Vec<String>,
) -> Option<CommandLineOptions> {
    let mut options = CommandLineOptions {
        primary_database_info: default_primary,
        ..CommandLineOptions::default()
    };
    let mut positionals: Vec<&str> = Vec::new();

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if arg == "--" {
            // getopt: everything after `--` is positional.
            while index < args.len() {
                positionals.push(&args[index]);
                index += 1;
            }
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline_value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            match name {
                "res" => {
                    let value = match inline_value {
                        Some(value) => value.to_owned(),
                        None => match args.get(index) {
                            Some(value) => {
                                index += 1;
                                value.clone()
                            }
                            None => {
                                out.push("Missing argument for option -r".to_owned());
                                return None;
                            }
                        },
                    };
                    options.res_path = value;
                }
                "verbose" => options.is_verbose = true,
                "help" => {
                    out.push(USAGE_MESSAGE.to_owned());
                    return None;
                }
                "clear" => options.should_clear = true,
                "list" => options.should_list = true,
                "ignore-sqlite-errors" => options.should_ignore_sqlite_errors = true,
                // An unrecognized long option leaves `optopt` at zero, so
                // the reference prints `Unknown option -` followed by the
                // NUL byte its `%c` renders.
                _ => {
                    out.push(format!("Unknown option -{}", '\0'));
                    return None;
                }
            }
        } else if arg.len() > 1 && arg.starts_with('-') {
            // A short-option cluster: `-vh` bundles, `-rX` attaches a
            // value.
            let cluster = &arg[1..];
            let mut chars = cluster.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    'r' => {
                        let rest: String = chars.by_ref().collect();
                        let value = if rest.is_empty() {
                            match args.get(index) {
                                Some(value) => {
                                    index += 1;
                                    value.clone()
                                }
                                None => {
                                    out.push("Missing argument for option -r".to_owned());
                                    return None;
                                }
                            }
                        } else {
                            rest
                        };
                        options.res_path = value;
                        break;
                    }
                    'v' => options.is_verbose = true,
                    'h' => {
                        out.push(USAGE_MESSAGE.to_owned());
                        return None;
                    }
                    'c' => options.should_clear = true,
                    'l' => options.should_list = true,
                    'i' => options.should_ignore_sqlite_errors = true,
                    _ => {
                        out.push(format!("Unknown option -{ch}"));
                        return None;
                    }
                }
            }
        } else {
            positionals.push(arg);
        }
    }

    // The positional pair: nothing, or the primary spec plus the file
    // that lists the secondary database paths.
    if positionals.is_empty() {
        return Some(options);
    }
    if positionals.len() < 2 {
        out.push(
            "Primary database specified, but no path to secondary database \
             list was specified"
                .to_owned(),
        );
        out.push(USAGE_MESSAGE.to_owned());
        return None;
    }
    if positionals.len() > 2 {
        out.push("Extraneous arguments after secondary database list path ignored".to_owned());
    }
    options.had_non_option_args = true;

    if options.should_clear || options.should_list {
        // The reference keeps the bw.xml primary info and skips parsing
        // the spec entirely.
        out.push(format!(
            "consolidate_dbs: The --{} option does not take additional arguments",
            if options.should_clear {
                "clear"
            } else {
                "list"
            }
        ));
        return Some(options);
    }

    if !parse_primary_database_info(positionals[0], &mut options.primary_database_info, out) {
        return None;
    }
    if !read_secondary_database_list(positionals[1], &mut options.secondary_databases, out) {
        return None;
    }
    Some(options)
}

/// `parsePrimaryDatabaseInfo` (`command_line_parser.cpp:196-256`): split
/// `<host>;<port>;<username>;<password>;<database>` on `;` — five
/// components exactly — with the port through `strtoul` base zero.
fn parse_primary_database_info(
    spec: &str,
    info: &mut ConnectionInfo,
    out: &mut Vec<String>,
) -> bool {
    let components: Vec<&str> = spec.split(';').collect();
    if components.len() != 5 {
        out.push(
            "Primary database argument must be in the form \
             <host>;<port>;<username>;<password>;<database>"
                .to_owned(),
        );
        return false;
    }

    info.host = components[0].to_owned();
    let (port, consumed) = strtoul_base_0(components[1]);
    if consumed != components[1].len() {
        out.push(format!("Could not parse database port :{}", components[1]));
        return false;
    }
    if port >= 1u64 << 16 {
        out.push(format!(
            "Primary database port is out-of-range: {}",
            components[1]
        ));
        return false;
    }
    info.port = port as u16;
    info.username = components[2].to_owned();
    info.password = components[3].to_owned();
    info.database = components[4].to_owned();
    true
}

/// C `strtoul` with base zero: leading whitespace, an optional sign, then
/// `0x` hex, a leading-zero octal, or decimal. Returns the value (a
/// negative parse wraps like the unsigned conversion does) and how many
/// leading characters were consumed — zero when no conversion happened.
fn strtoul_base_0(text: &str) -> (u64, usize) {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let mut negative = false;
    if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
        negative = bytes[index] == b'-';
        index += 1;
    }
    let mut radix: u64 = 10;
    if index < bytes.len() && bytes[index] == b'0' {
        radix = 8;
        index += 1;
        if index < bytes.len()
            && (bytes[index] == b'x' || bytes[index] == b'X')
            && index + 1 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
        {
            radix = 16;
            index += 1;
        }
    }
    let digits_start = index;
    let mut magnitude: u64 = 0;
    while index < bytes.len() {
        let Some(digit) = (bytes[index] as char).to_digit(radix as u32) else {
            break;
        };
        magnitude = magnitude.saturating_mul(radix).saturating_add(digit as u64);
        index += 1;
    }
    if index == digits_start {
        // No conversion performed (a sign with no digits, bare whitespace,
        // or an empty string): strtoul points endptr at the start.
        return (0, 0);
    }
    let value = if negative {
        magnitude.wrapping_neg()
    } else {
        magnitude
    };
    (value, index)
}

/// `readSecondaryDatabaseList` (`command_line_parser.cpp:262-293`): one
/// path per line, empty lines skipped.
fn read_secondary_database_list(
    path: &str,
    databases: &mut Vec<String>,
    out: &mut Vec<String>,
) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        out.push(format!(
            "Could not open secondary database list at \"{path}\""
        ));
        return false;
    };
    for line in content.lines() {
        if !line.is_empty() {
            databases.push(line.to_owned());
        }
    }
    true
}

// -----------------------------------------------------------------------------
// Section: the primary-database ledger (`consolidate_dbs_app.cpp`)
// -----------------------------------------------------------------------------

/// `SecondaryDBInfo` (`secondary_db_info.hpp`): where a secondary
/// database lives. `host_ip` is the value `getSecondaryDBInfos` passes
/// through `htonl` — the wire-order form the reference's
/// `Mercury::Address` prints from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecondaryDbInfo {
    pub host_ip: u32,
    pub location: String,
}

/// The primary-database legs `ConsolidateDBsApp` drives over its MySQL
/// connection, as a trait — the port has no MySQL backend, so the caller
/// supplies the ledger (BW-09-02's contracts are the eventual tenant).
pub trait PrimaryDatabase {
    /// `getSecondaryDBInfos`: `SELECT ip, location FROM
    /// bigworldSecondaryDatabases`, each ip through `htonl`.
    fn secondary_db_infos(&mut self) -> Result<Vec<SecondaryDbInfo>, String>;

    /// `checkPrimaryDBEntityDefsMatchInternal`: `SELECT checksum FROM
    /// bigworldEntityDefsChecksum` — `None` is the empty table.
    fn entity_defs_checksum(&mut self) -> Result<Option<String>, String>;

    /// `clearSecondaryDBEntries`: `DELETE FROM
    /// bigworldSecondaryDatabases`, returning the affected row count.
    fn clear_secondary_db_entries(&mut self) -> Result<u32, String>;
}

/// `DBConsolidatorErrors` (`db_consolidator_errors.hpp`): the secondary
/// databases that failed consolidation, collected for the clean-up pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DbConsolidatorErrors {
    erroneous: BTreeSet<String>,
}

impl DbConsolidatorErrors {
    /// `addSecondaryDB`.
    pub fn add_secondary_db(&mut self, file_name: &str) {
        self.erroneous.insert(file_name.to_owned());
    }

    /// `hasErrors`.
    pub fn has_errors(&self) -> bool {
        !self.erroneous.is_empty()
    }

    /// `secondaryDBHasError`.
    pub fn secondary_db_has_error(&self, file_name: &str) -> bool {
        self.erroneous.contains(file_name)
    }
}

// -----------------------------------------------------------------------------
// Section: the run flow (`main.cpp` + `consolidateSecondaryDBs`)
// -----------------------------------------------------------------------------

/// The consolidation half of `ConsolidateDBsApp`, split from the
/// primary-database ledger so the tool flow can drive either a scripted
/// session or the concrete [`StorageConsolidator`].
pub trait ConsolidationSession {
    /// `EntityDefs::getPersistentPropertiesDigest`, quoted — the value the
    /// primary's `bigworldEntityDefsChecksum` row must match.
    fn expected_defs_digest(&self) -> String;

    /// `consolidateSecondaryDBs` over the file list DBApp handed over.
    fn consolidate_files(&mut self, file_paths: &[String], out: &mut Vec<String>) -> bool;

    /// `transferAndConsolidate` — the network leg. The reference pulls
    /// the registered secondaries over a Mercury TCP listener with
    /// `TransferDB` children; that leg has no port, so the concrete
    /// [`StorageConsolidator`] refuses it and a test double answers for
    /// it here.
    fn transfer_and_consolidate(&mut self, out: &mut Vec<String>) -> bool;
}

/// `ConsolidateDBsApp::checkEntityDefsDigestMatch`
/// (`consolidate_dbs_app.cpp:398-413`): the quoted checksum must unquote
/// — the reference's only validity check is the length — and equal the
/// loaded definitions' digest, compared as raw bytes.
pub fn defs_digest_matches(quoted_digest: &str, expected_quoted: &str) -> bool {
    match Md5::unquote(quoted_digest) {
        Some(digest) => Md5::unquote(expected_quoted) == Some(digest),
        None => false,
    }
}

/// `main` (`main.cpp:47-127`) with the BWResource/BWConfig preamble
/// already absorbed into the caller's default connection info: parse,
/// dispatch `--clear`/`--list`, gate on the entity-defs checksum, then
/// either consolidate the file list DBApp supplied or hand the run to the
/// network leg. Returns the process exit code; log lines land in `out`.
pub fn run(
    args: &[String],
    default_primary: ConnectionInfo,
    primary: &mut dyn PrimaryDatabase,
    session: &mut dyn ConsolidationSession,
    out: &mut Vec<String>,
) -> i32 {
    let Some(options) = parse_command_line(args, default_primary, out) else {
        out.push("Failed to parse command line options.".to_owned());
        return FAILURE;
    };

    if options.should_clear {
        return match primary.clear_secondary_db_entries() {
            Ok(count) => {
                out.push(format!(
                    "ConsolidateDBsApp::clearSecondaryDBEntries: Cleared {} \
                     entries from {}:{} ({})",
                    count,
                    options.primary_database_info.host,
                    options.primary_database_info.port,
                    options.primary_database_info.database
                ));
                SUCCESS
            }
            Err(_) => {
                out.push("Failed to clear secondary databases".to_owned());
                FAILURE
            }
        };
    }

    if options.should_list {
        return match primary.secondary_db_infos() {
            Ok(infos) => {
                out.push(String::new());
                out.push(format!(
                    "Secondary databases to consolidate = {}",
                    infos.len()
                ));
                for info in &infos {
                    let address = mercury::endpoint::Address::new(info.host_ip, 0);
                    out.push(format!("{:>15} {}", address.ip_as_string(), info.location));
                }
                SUCCESS
            }
            Err(_) => {
                out.push("Failed to list secondary databases".to_owned());
                FAILURE
            }
        };
    }

    if !check_primary_db_entity_defs_match(primary, &session.expected_defs_digest(), out) {
        return FAILURE;
    }

    if options.had_non_option_args {
        bool_code(session.consolidate_files(&options.secondary_databases, out))
    } else {
        bool_code(session.transfer_and_consolidate(out))
    }
}

/// `EXIT_SUCCESS`/`EXIT_FAILURE` from a bool.
fn bool_code(ok: bool) -> i32 {
    if ok { SUCCESS } else { FAILURE }
}

/// `checkPrimaryDBEntityDefsMatch` (`consolidate_dbs_app.cpp:427-462`):
/// read the primary's checksum row and compare digests; the long
/// "do not match" error is the wrapper's, printed for every failure the
/// internal check reports.
fn check_primary_db_entity_defs_match(
    primary: &mut dyn PrimaryDatabase,
    expected_quoted: &str,
    out: &mut Vec<String>,
) -> bool {
    let ok = match primary.entity_defs_checksum() {
        Err(error) => {
            out.push(format!(
                "ConsolidateDBsApp::checkEntityDefsMatch: Failed to retrieve \
                 the primary database entity definition checksum: {error}"
            ));
            false
        }
        Ok(None) => {
            out.push("ConsolidateDBsApp::checkEntityDefsMatch: Checksum table is empty".to_owned());
            false
        }
        Ok(Some(checksum)) => {
            if Md5::unquote(&checksum).is_none() {
                out.push(
                    "ConsolidateDBsApp::checkEntityDefsDigestMatch: Not a valid MD5 digest"
                        .to_owned(),
                );
            }
            defs_digest_matches(&checksum, expected_quoted)
        }
    };
    if !ok {
        out.push(
            "ConsolidateDBsApp::init: Our entity definitions do not match the \
             ones used by the primary database\nDatabase consolidation should \
             be run before making changes to entity definitions. Changing \
             entity definitions potentially invalidates unconsolidated data.\n\
             Run \"consolidate_dbs --clear\" to allow the server to run \
             without doing data consolidation. Unconsolidated data will be \
             lost."
                .to_owned(),
        );
    }
    ok
}

// -----------------------------------------------------------------------------
// Section: the concrete consolidator (`consolidateSecondaryDB`)
// -----------------------------------------------------------------------------

/// `ConsolidateDBsApp`'s merge half against a real [`EntityStorage`]:
/// `consolidateSecondaryDBs`/`consolidateSecondaryDB`
/// (`consolidate_dbs_app.cpp:242-367`) around the BW-09-03-03 engine,
/// with the error ledger and the abort flag.
pub struct StorageConsolidator<'a> {
    storage: &'a mut dyn EntityStorage,
    defs: &'a HashMap<EntityTypeId, StreamTypeDef>,
    expected_digest: String,
    errors: DbConsolidatorErrors,
    abort_requested: bool,
}

impl<'a> StorageConsolidator<'a> {
    pub fn new(
        storage: &'a mut dyn EntityStorage,
        defs: &'a HashMap<EntityTypeId, StreamTypeDef>,
        expected_digest: &str,
    ) -> Self {
        StorageConsolidator {
            storage,
            defs,
            expected_digest: expected_digest.to_owned(),
            errors: DbConsolidatorErrors::default(),
            abort_requested: false,
        }
    }

    /// `ConsolidateDBsApp::abort`: the SIGINT/SIGHUP handler's body.
    pub fn abort(&mut self) {
        self.abort_requested = true;
    }

    /// `shouldAbort_`: set by [`Self::abort`], it changes only the
    /// failure warning `consolidate_files` prints.
    pub fn aborted(&self) -> bool {
        self.abort_requested
    }

    /// The error ledger, for the caller's clean-up pass.
    pub fn errors(&self) -> &DbConsolidatorErrors {
        &self.errors
    }
}

impl ConsolidationSession for StorageConsolidator<'_> {
    fn expected_defs_digest(&self) -> String {
        self.expected_digest.clone()
    }

    /// `consolidateSecondaryDBs` (`consolidate_dbs_app.cpp:242-282`): one
    /// file at a time; the first failure warns and stops the run.
    fn consolidate_files(&mut self, file_paths: &[String], out: &mut Vec<String>) -> bool {
        for file_path in file_paths {
            if !self.consolidate_secondary_db(file_path, out) {
                if self.abort_requested {
                    out.push(
                        "ConsolidateDBsApp::consolidateSecondaryDBs: Data \
                         consolidation was aborted"
                            .to_owned(),
                    );
                } else {
                    out.push(
                        "ConsolidateDBsApp::consolidateSecondaryDBs: Some entities \
                         were not consolidated. Data consolidation must be re-run \
                         after errors have been corrected."
                            .to_owned(),
                    );
                }
                return false;
            }
        }
        true
    }

    /// `transferAndConsolidate` has no port: the network leg (a Mercury
    /// TCP listener fed by `TransferDB` children) stays with the caller.
    /// Recorded deviation — a file-less run refuses here.
    fn transfer_and_consolidate(&mut self, out: &mut Vec<String>) -> bool {
        out.push(
            "ConsolidateDBsApp::transferAndConsolidate: the file-transfer leg \
             has no port; run with <primarydb_spec> <secondarydb_list>"
                .to_owned(),
        );
        false
    }
}

impl StorageConsolidator<'_> {
    /// `consolidateSecondaryDB` (`consolidate_dbs_app.cpp:316-367`): open
    /// the file, gate its checksum digest, merge it, and ledger the
    /// failures. `init`/`getChecksumDigest` report through their own
    /// ERROR_MSGs, which the reader face carries in its error text.
    fn consolidate_secondary_db(&mut self, file_path: &str, out: &mut Vec<String>) -> bool {
        let database = match SecondaryDatabase::open(file_path) {
            Ok(database) => database,
            Err(error) => {
                out.push(error.to_string());
                return false;
            }
        };

        out.push(format!(
            "ConsolidateDBsApp::consolidateSecondaryDB: Consolidating '{file_path}'"
        ));

        let digest = match database.checksum_digest() {
            Ok(digest) => digest,
            Err(error) => {
                out.push(error.to_string());
                return false;
            }
        };
        if Md5::unquote(&digest).is_none() {
            out.push(
                "ConsolidateDBsApp::checkEntityDefsDigestMatch: Not a valid MD5 digest".to_owned(),
            );
        }
        if !defs_digest_matches(&digest, &self.expected_digest) {
            out.push(format!(
                "ConsolidateDBsApp::consolidateSecondaryDB: {file_path} failed \
                 entity digest check"
            ));
            return false;
        }

        let report =
            consolidate_database(self.storage, &database, self.defs, &self.expected_digest);
        if report.has_error() {
            self.errors.add_secondary_db(file_path);
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use entitydef::value::PropertyValue;

    use crate::record::EntityRecord;
    use crate::secondary::{FLIP_TABLE, StreamKind, StreamPropDef, values_to_blob};
    use crate::storage::{EntityKey, MemoryStorage, load_entity};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn primary_info() -> ConnectionInfo {
        ConnectionInfo {
            host: "dbhost".to_owned(),
            port: 3306,
            username: "bw".to_owned(),
            password: "pw".to_owned(),
            database: "bigworld".to_owned(),
        }
    }

    // -----------------------------------------------------------------
    // the command line
    // -----------------------------------------------------------------

    #[test]
    fn no_args_yields_the_bw_xml_defaults() {
        let mut out = Vec::new();
        let options = parse_command_line(&[], primary_info(), &mut out).unwrap();
        assert!(out.is_empty());
        assert!(!options.is_verbose);
        assert!(!options.should_clear);
        assert!(!options.should_list);
        assert!(!options.should_ignore_sqlite_errors);
        assert!(!options.had_non_option_args);
        assert_eq!(options.res_path, "");
        assert_eq!(options.primary_database_info, primary_info());
        assert!(options.secondary_databases.is_empty());
    }

    #[test]
    fn every_flag_has_a_long_and_a_short_form() {
        let mut out = Vec::new();
        let options = parse_command_line(
            &args(&[
                "--ignore-sqlite-errors",
                "--clear",
                "--list",
                "--verbose",
                "--res",
                "/game/res",
            ]),
            ConnectionInfo::default(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(options.is_verbose);
        assert!(options.should_clear);
        assert!(options.should_list);
        assert!(options.should_ignore_sqlite_errors);
        assert_eq!(options.res_path, "/game/res");

        let mut out = Vec::new();
        let options = parse_command_line(
            &args(&["-i", "-c", "-l", "-v", "-r", "/other/res"]),
            ConnectionInfo::default(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(options.is_verbose);
        assert!(options.should_clear);
        assert!(options.should_list);
        assert!(options.should_ignore_sqlite_errors);
        assert_eq!(options.res_path, "/other/res");
    }

    #[test]
    fn bundled_shorts_and_attached_values_parse_like_getopt() {
        let mut out = Vec::new();
        let options = parse_command_line(
            &args(&["-vi", "--res=/bundled"]),
            ConnectionInfo::default(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(options.is_verbose);
        assert!(options.should_ignore_sqlite_errors);
        assert!(!options.should_clear);
        assert_eq!(options.res_path, "/bundled");

        let mut out = Vec::new();
        let options =
            parse_command_line(&args(&["-vr/bundled"]), ConnectionInfo::default(), &mut out)
                .unwrap();
        assert!(options.is_verbose);
        assert_eq!(options.res_path, "/bundled");
    }

    #[test]
    fn help_prints_usage_and_fails_init() {
        let mut out = Vec::new();
        assert!(parse_command_line(&args(&["-h"]), ConnectionInfo::default(), &mut out).is_none());
        assert_eq!(out, vec![USAGE_MESSAGE.to_owned()]);

        let mut out = Vec::new();
        assert!(
            parse_command_line(&args(&["--help"]), ConnectionInfo::default(), &mut out).is_none()
        );
        assert_eq!(out, vec![USAGE_MESSAGE.to_owned()]);
    }

    #[test]
    fn a_starved_or_unknown_option_fails_init() {
        let mut out = Vec::new();
        assert!(parse_command_line(&args(&["-r"]), ConnectionInfo::default(), &mut out).is_none());
        assert_eq!(out, vec!["Missing argument for option -r".to_owned()]);

        let mut out = Vec::new();
        assert!(
            parse_command_line(&args(&["--res"]), ConnectionInfo::default(), &mut out).is_none()
        );
        assert_eq!(
            out.last().map(String::as_str),
            Some("Missing argument for option -r")
        );

        let mut out = Vec::new();
        assert!(parse_command_line(&args(&["-x"]), ConnectionInfo::default(), &mut out).is_none());
        assert_eq!(out, vec!["Unknown option -x".to_owned()]);

        let mut out = Vec::new();
        assert!(
            parse_command_line(&args(&["--bogus"]), ConnectionInfo::default(), &mut out).is_none()
        );
        // An unknown long option leaves `optopt` at zero, so the `%c`
        // renders a NUL byte.
        assert_eq!(out, vec![format!("Unknown option -{}", '\0')]);
    }

    #[test]
    fn one_positional_argument_is_a_usage_error() {
        let mut out = Vec::new();
        assert!(
            parse_command_line(
                &args(&["spec;1;u;p;db"]),
                ConnectionInfo::default(),
                &mut out
            )
            .is_none()
        );
        assert_eq!(
            out[0],
            "Primary database specified, but no path to secondary database list was specified"
        );
        assert_eq!(out[1], USAGE_MESSAGE);
    }

    #[test]
    fn extraneous_positionals_are_ignored_with_a_warning() {
        let list = temp_list("extra", "second.sqlite\n");

        let mut out = Vec::new();
        let options = parse_command_line(
            &args(&["h;1;u;p;d", &list, "junk", "more-junk"]),
            ConnectionInfo::default(),
            &mut out,
        )
        .unwrap();
        assert_eq!(
            out,
            vec!["Extraneous arguments after secondary database list path ignored".to_owned()]
        );
        assert!(options.had_non_option_args);
        assert_eq!(options.secondary_databases, ["second.sqlite"]);
    }

    #[test]
    fn clear_and_list_take_no_positional_pair() {
        for flag in ["--clear", "--list"] {
            let mut out = Vec::new();
            let options = parse_command_line(
                &args(&[flag, "h;1;u;p;d", "list-file"]),
                primary_info(),
                &mut out,
            )
            .unwrap();
            assert_eq!(
                out,
                vec![format!(
                    "consolidate_dbs: The --{} option does not take additional arguments",
                    if flag == "--clear" { "clear" } else { "list" }
                )]
            );
            // The spec is skipped: the bw.xml primary info stands and no
            // secondary list is read.
            assert_eq!(options.primary_database_info, primary_info());
            assert!(options.secondary_databases.is_empty());
            assert!(options.had_non_option_args);
        }
    }

    #[test]
    fn the_primary_spec_is_five_semicolon_parts_with_a_strtoul_port() {
        let list = temp_list("spec", "");
        let cases = [
            ("db;3306;u;p;bigworld", 3306u16),
            ("db;0x1f;u;p;bigworld", 31),
            ("db;010;u;p;bigworld", 8),
            ("db;;u;p;bigworld", 0),
        ];
        for (spec, port) in cases {
            let mut out = Vec::new();
            let options =
                parse_command_line(&args(&[spec, &list]), ConnectionInfo::default(), &mut out)
                    .unwrap();
            assert!(out.is_empty(), "{spec}: {out:?}");
            assert_eq!(options.primary_database_info.port, port, "{spec}");
            assert_eq!(options.primary_database_info.host, "db");
            assert_eq!(options.primary_database_info.username, "u");
            assert_eq!(options.primary_database_info.password, "p");
            assert_eq!(options.primary_database_info.database, "bigworld");
            assert!(options.secondary_databases.is_empty());
        }
    }

    #[test]
    fn a_malformed_primary_spec_is_refused() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["h;u;p;d", "list"],
                "Primary database argument must be in the form \
                 <host>;<port>;<username>;<password>;<database>",
            ),
            (
                &["h;1;u;p;d;e;f", "list"],
                "Primary database argument must be in the form \
                 <host>;<port>;<username>;<password>;<database>",
            ),
            (
                &["h;abc;u;p;d", "list"],
                "Could not parse database port :abc",
            ),
            (
                &["h;3306x;u;p;d", "list"],
                "Could not parse database port :3306x",
            ),
            (
                &["h; 15 ;u;p;d", "list"],
                "Could not parse database port : 15 ",
            ),
            (
                &["h;-1;u;p;d", "list"],
                "Primary database port is out-of-range: -1",
            ),
            (
                &["h;99999;u;p;d", "list"],
                "Primary database port is out-of-range: 99999",
            ),
        ];
        for (argv, message) in cases {
            let mut out = Vec::new();
            assert!(
                parse_command_line(&args(argv), ConnectionInfo::default(), &mut out).is_none(),
                "{argv:?}"
            );
            assert_eq!(out[0], *message, "{argv:?}");
        }
    }

    #[test]
    fn the_secondary_list_is_one_path_per_line() {
        let path = temp_list("lines", "one.sqlite\n\ntwo.sqlite\n\n");

        let mut out = Vec::new();
        let options = parse_command_line(
            &args(&["h;1;u;p;d", &path]),
            ConnectionInfo::default(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(options.had_non_option_args);
        assert_eq!(options.secondary_databases, ["one.sqlite", "two.sqlite"]);

        let mut out = Vec::new();
        assert!(
            parse_command_line(
                &args(&["h;1;u;p;d", "/no/such/dir/list.txt"]),
                ConnectionInfo::default(),
                &mut out
            )
            .is_none()
        );
        assert_eq!(
            out,
            vec!["Could not open secondary database list at \"/no/such/dir/list.txt\"".to_owned()]
        );
    }

    #[test]
    fn options_after_positionals_are_still_options() {
        let list = temp_list("perm", "a.sqlite\n");

        let mut out = Vec::new();
        let options = parse_command_line(
            &args(&["h;1;u;p;d", &list, "-v"]),
            ConnectionInfo::default(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(options.is_verbose);
        assert_eq!(options.secondary_databases, ["a.sqlite"]);
    }

    #[test]
    fn the_double_dash_terminator_ends_the_options() {
        let list = temp_list("terminator", "term.sqlite\n");

        let mut out = Vec::new();
        let options = parse_command_line(
            &args(&["--", "h;1;u;p;d", &list]),
            ConnectionInfo::default(),
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(options.had_non_option_args);
        assert_eq!(options.secondary_databases, ["term.sqlite"]);
    }

    /// One temporary list file per call: the tests run in parallel.
    fn temp_list(tag: &str, content: &str) -> String {
        static LIST: AtomicUsize = AtomicUsize::new(0);
        let index = LIST.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("cdbl-{}-{tag}-{}", std::process::id(), index));
        std::fs::write(&path, content).unwrap();
        path.to_str().unwrap().to_owned()
    }

    // -----------------------------------------------------------------
    // the ledger and the tool flow
    // -----------------------------------------------------------------

    /// `DIGEST-0`..: a 32-character quoted-looking digest, distinct per
    /// seed, that both sides of the gate unquote identically.
    fn digest(seed: u8) -> String {
        format!("{}{:02X}", &"0123456789ABCDEF".repeat(2)[..30], seed)
    }

    /// A scripted primary database.
    struct Ledger {
        infos: Result<Vec<SecondaryDbInfo>, String>,
        checksum: Result<Option<String>, String>,
        cleared: Result<u32, String>,
    }

    impl Ledger {
        fn with_checksum(checksum: Result<Option<String>, String>) -> Self {
            Ledger {
                infos: Ok(Vec::new()),
                checksum,
                cleared: Ok(3),
            }
        }
    }

    impl PrimaryDatabase for Ledger {
        fn secondary_db_infos(&mut self) -> Result<Vec<SecondaryDbInfo>, String> {
            self.infos.clone()
        }
        fn entity_defs_checksum(&mut self) -> Result<Option<String>, String> {
            self.checksum.clone()
        }
        fn clear_secondary_db_entries(&mut self) -> Result<u32, String> {
            self.cleared.clone()
        }
    }

    /// A scripted consolidation session.
    struct ScriptedSession {
        expected: String,
        files_result: bool,
        transfer_result: bool,
        received_files: Vec<String>,
    }

    impl ScriptedSession {
        fn new(expected: String) -> Self {
            ScriptedSession {
                expected,
                files_result: true,
                transfer_result: true,
                received_files: Vec::new(),
            }
        }
    }

    impl ConsolidationSession for ScriptedSession {
        fn expected_defs_digest(&self) -> String {
            self.expected.clone()
        }
        fn consolidate_files(&mut self, file_paths: &[String], _out: &mut Vec<String>) -> bool {
            self.received_files = file_paths.to_vec();
            self.files_result
        }
        fn transfer_and_consolidate(&mut self, _out: &mut Vec<String>) -> bool {
            self.transfer_result
        }
    }

    #[test]
    fn clear_reports_the_cleared_count_against_the_primary() {
        let mut ledger = Ledger::with_checksum(Ok(Some(digest(1))));
        let mut session = ScriptedSession::new(digest(1));
        let mut out = Vec::new();
        let code = run(
            &args(&["--clear"]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, SUCCESS);
        assert_eq!(
            out,
            vec!["ConsolidateDBsApp::clearSecondaryDBEntries: Cleared 3 entries from dbhost:3306 (bigworld)".to_owned()]
        );

        ledger.cleared = Err("no such table".to_owned());
        let mut out = Vec::new();
        let code = run(
            &args(&["--clear"]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, FAILURE);
        assert_eq!(out, vec!["Failed to clear secondary databases".to_owned()]);
    }

    #[test]
    fn list_prints_the_registered_databases() {
        let mut ledger = Ledger::with_checksum(Ok(Some(digest(1))));
        ledger.infos = Ok(vec![
            SecondaryDbInfo {
                // `getSecondaryDBInfos` passes the row's ip through
                // `htonl`; `Address::ip_as_string` swaps it back.
                host_ip: u32::from_be_bytes([10, 0, 0, 1]).swap_bytes(),
                location: "/tmp/a.sqlite".to_owned(),
            },
            SecondaryDbInfo {
                host_ip: u32::from_be_bytes([192, 168, 1, 254]).swap_bytes(),
                location: "/tmp/b.sqlite".to_owned(),
            },
        ]);
        let mut session = ScriptedSession::new(digest(1));
        let mut out = Vec::new();
        let code = run(
            &args(&["--list"]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, SUCCESS);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], "");
        assert_eq!(out[1], "Secondary databases to consolidate = 2");
        assert_eq!(out[2], "       10.0.0.1 /tmp/a.sqlite");
        assert_eq!(out[3], "  192.168.1.254 /tmp/b.sqlite");

        ledger.infos = Err("connection lost".to_owned());
        let mut out = Vec::new();
        let code = run(
            &args(&["--list"]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, FAILURE);
        assert_eq!(out, vec!["Failed to list secondary databases".to_owned()]);
    }

    #[test]
    fn the_defs_gate_refuses_every_flavour_of_mismatch() {
        // (the checksum row the primary returns, the digest we loaded,
        // the expected log) — every failure ends with the long init
        // error, and neither leg runs.
        let long_error = "ConsolidateDBsApp::init: Our entity definitions do not match the ones used by the primary database\nDatabase consolidation should be run before making changes to entity definitions. Changing entity definitions potentially invalidates unconsolidated data.\nRun \"consolidate_dbs --clear\" to allow the server to run without doing data consolidation. Unconsolidated data will be lost.".to_owned();
        let cases = [
            (
                Err("boom".to_owned()),
                digest(1),
                vec![
                    "ConsolidateDBsApp::checkEntityDefsMatch: Failed to retrieve the primary database entity definition checksum: boom".to_owned(),
                    long_error.clone(),
                ],
            ),
            (
                Ok(None),
                digest(1),
                vec![
                    "ConsolidateDBsApp::checkEntityDefsMatch: Checksum table is empty".to_owned(),
                    long_error.clone(),
                ],
            ),
            (
                Ok(Some("not-a-digest".to_owned())),
                digest(1),
                vec![
                    "ConsolidateDBsApp::checkEntityDefsDigestMatch: Not a valid MD5 digest"
                        .to_owned(),
                    long_error.clone(),
                ],
            ),
            (Ok(Some(digest(1))), digest(2), vec![long_error.clone()]),
        ];
        for (checksum, expected, expected_out) in cases {
            let mut ledger = Ledger::with_checksum(checksum);
            let mut session = ScriptedSession::new(expected);
            let mut out = Vec::new();
            let code = run(
                &args(&[]),
                primary_info(),
                &mut ledger,
                &mut session,
                &mut out,
            );
            assert_eq!(code, FAILURE, "{expected_out:?}");
            assert_eq!(out, expected_out, "{expected_out:?}");
            assert!(session.received_files.is_empty(), "{expected_out:?}");
        }
    }

    #[test]
    fn a_matching_gate_consolidates_the_file_list() {
        let list = temp_list("files", "dbs.txt\n");
        let mut ledger = Ledger::with_checksum(Ok(Some(digest(1))));
        let mut session = ScriptedSession::new(digest(1));
        session.files_result = false;
        let mut out = Vec::new();
        let code = run(
            &args(&["h;1;u;p;d", &list]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, FAILURE);
        assert_eq!(session.received_files, ["dbs.txt"]);

        session.files_result = true;
        let mut out = Vec::new();
        let code = run(
            &args(&["h;1;u;p;d", &list]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, SUCCESS);
    }

    #[test]
    fn a_gate_pass_without_positionals_goes_to_the_transfer_leg() {
        let mut ledger = Ledger::with_checksum(Ok(Some(digest(1))));
        let mut session = ScriptedSession::new(digest(1));
        session.transfer_result = false;
        let mut out = Vec::new();
        let code = run(
            &args(&[]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, FAILURE);
        assert!(session.received_files.is_empty());

        session.transfer_result = true;
        let mut out = Vec::new();
        let code = run(
            &args(&[]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, SUCCESS);
    }

    #[test]
    fn a_failed_parse_is_the_parse_failure_exit() {
        let mut ledger = Ledger::with_checksum(Ok(Some(digest(1))));
        let mut session = ScriptedSession::new(digest(1));
        let mut out = Vec::new();
        let code = run(
            &args(&["-h"]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, FAILURE);
        assert_eq!(
            out.last().map(String::as_str),
            Some("Failed to parse command line options.")
        );

        // No ledger action ran behind the failed parse.
        ledger.cleared = Err("should not be reached".to_owned());
        let mut out = Vec::new();
        let code = run(
            &args(&["-h"]),
            primary_info(),
            &mut ledger,
            &mut session,
            &mut out,
        );
        assert_eq!(code, FAILURE);
        assert_eq!(
            out.last().map(String::as_str),
            Some("Failed to parse command line options.")
        );
    }

    // -----------------------------------------------------------------
    // the concrete consolidator
    // -----------------------------------------------------------------

    /// One temporary file per call: the tests run in parallel threads.
    static TEMP_FILE: AtomicUsize = AtomicUsize::new(0);

    fn unique_path() -> String {
        let index = TEMP_FILE.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir()
            .join(format!("db-cdbs-{}-{}", std::process::id(), index))
            .to_str()
            .unwrap()
            .to_owned()
    }

    /// An Avatar-shaped stream def on type 3: one `Uint32` property.
    fn avatar_def() -> StreamTypeDef {
        StreamTypeDef {
            name: "Avatar".to_owned(),
            type_id: 3,
            properties: vec![StreamPropDef {
                name: "mass".to_owned(),
                kind: StreamKind::Uint32,
            }],
        }
    }

    fn defs() -> HashMap<EntityTypeId, StreamTypeDef> {
        HashMap::from([(3, avatar_def())])
    }

    /// A real SQLite secondary database file: the flip/flop schema plus a
    /// checksum row, with `rows` of (dbID, time, values) in the flip
    /// table.
    fn fixture_file(rows: &[(i64, i64, Vec<PropertyValue>)], checksum: &str) -> String {
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).expect("the fixture file opens");
        connection
            .execute("CREATE TABLE tbl_checksum (sm_checksum TEXT)", [])
            .unwrap();
        connection
            .execute(
                &format!("INSERT INTO tbl_checksum VALUES (\"{checksum}\")"),
                [],
            )
            .unwrap();
        for table in [FLIP_TABLE, crate::secondary::FLOP_TABLE] {
            connection
                .execute(
                    &format!(
                        "CREATE TABLE {table} (sm_dbID INTEGER, sm_typeID INTEGER, \
                         sm_time INTEGER, sm_blob BLOB)"
                    ),
                    [],
                )
                .unwrap();
        }
        for (db_id, time, values) in rows {
            let blob = values_to_blob(values, &avatar_def());
            connection
                .execute(
                    &format!("INSERT INTO {FLIP_TABLE} VALUES (?, ?, ?, ?)"),
                    rusqlite::params![db_id, 3, time, blob],
                )
                .unwrap();
        }
        drop(connection);
        path
    }

    /// A primary with one Avatar (db 1) carrying `Uint32(5)`.
    fn seeded_primary() -> MemoryStorage {
        let mut store = MemoryStorage::new();
        store.insert(EntityRecord {
            type_id: 3,
            db_id: 1,
            values: vec![PropertyValue::Uint(5)],
        });
        store
    }

    #[test]
    fn a_clean_file_consolidates_and_ledgers_nothing() {
        let path = fixture_file(&[(1, 200, vec![PropertyValue::Uint(10)])], &digest(1));

        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let mut out = Vec::new();
        assert!(consolidator.consolidate_files(std::slice::from_ref(&path), &mut out));
        assert_eq!(
            out,
            vec![format!(
                "ConsolidateDBsApp::consolidateSecondaryDB: Consolidating '{path}'"
            )]
        );
        assert!(!consolidator.errors().has_errors());
        // The secondary's newer row (time 200) won over the seed (5).
        let record = load_entity(
            &storage,
            &EntityKey {
                type_id: 3,
                db_id: 1,
            },
        )
        .expect("the entity loads");
        assert_eq!(record.values, vec![PropertyValue::Uint(10)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_digest_mismatch_refuses_the_file_without_ledgering() {
        let path = fixture_file(&[(1, 200, vec![PropertyValue::Uint(10)])], &digest(2));

        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let mut out = Vec::new();
        assert!(!consolidator.consolidate_files(std::slice::from_ref(&path), &mut out));
        assert_eq!(
            out[1],
            format!("ConsolidateDBsApp::consolidateSecondaryDB: {path} failed entity digest check")
        );
        // The digest gate is not an engine failure, so the ledger stays
        // clean — the reference only ledgers consolidate() failures.
        assert!(!consolidator.errors().has_errors());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_merge_failure_ledgers_the_file_and_warns() {
        // A corrupt blob fails the row decode, the engine reports an
        // error, and the file lands in the ledger.
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute("CREATE TABLE tbl_checksum (sm_checksum TEXT)", [])
            .unwrap();
        connection
            .execute(
                &format!("INSERT INTO tbl_checksum VALUES (\"{}\")", digest(1)),
                [],
            )
            .unwrap();
        connection
            .execute(
                &format!(
                    "CREATE TABLE {FLIP_TABLE} (sm_dbID INTEGER, sm_typeID INTEGER, \
                     sm_time INTEGER, sm_blob BLOB)"
                ),
                [],
            )
            .unwrap();
        connection
            .execute(
                &format!("INSERT INTO {FLIP_TABLE} VALUES (?, ?, ?, ?)"),
                rusqlite::params![1, 3, 200, vec![0xFFu8; 5]],
            )
            .unwrap();
        drop(connection);

        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let mut out = Vec::new();
        assert!(!consolidator.consolidate_files(std::slice::from_ref(&path), &mut out));
        assert!(consolidator.errors().secondary_db_has_error(&path));
        assert_eq!(
            out.last().map(String::as_str),
            Some(
                "ConsolidateDBsApp::consolidateSecondaryDBs: Some entities were \
                 not consolidated. Data consolidation must be re-run after \
                 errors have been corrected."
            )
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_abort_changes_the_failure_warning() {
        let path = fixture_file(&[(1, 200, vec![PropertyValue::Uint(10)])], &digest(2));

        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        consolidator.abort();
        assert!(consolidator.aborted());
        let mut out = Vec::new();
        assert!(!consolidator.consolidate_files(std::slice::from_ref(&path), &mut out));
        assert_eq!(
            out.last().map(String::as_str),
            Some(
                "ConsolidateDBsApp::consolidateSecondaryDBs: Data consolidation \
                 was aborted"
            )
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unopenable_file_fails_with_the_reader_message() {
        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let missing = "/no/such/dir/x.sqlite".to_owned();
        let mut out = Vec::new();
        assert!(!consolidator.consolidate_files(std::slice::from_ref(&missing), &mut out));
        assert!(out[0].contains("could not open"), "got: {out:?}");
        assert!(!consolidator.errors().has_errors());
    }

    #[test]
    fn the_transfer_leg_is_recorded_out_of_scope() {
        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let mut out = Vec::new();
        assert!(!consolidator.transfer_and_consolidate(&mut out));
        assert!(
            out[0].contains("the file-transfer leg has no port"),
            "got: {out:?}"
        );
    }

    #[test]
    fn a_full_run_consolidates_a_real_file_into_the_storage() {
        // The whole `main` flow driven end to end with the concrete
        // consolidator: parse, gate, merge, exit code.
        let path = fixture_file(&[(1, 200, vec![PropertyValue::Uint(10)])], &digest(1));
        let list = temp_list("run", &format!("{path}\n"));

        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let mut ledger = Ledger::with_checksum(Ok(Some(digest(1))));
        let mut out = Vec::new();
        let code = run(
            &args(&["h;1;u;p;d", &list]),
            primary_info(),
            &mut ledger,
            &mut consolidator,
            &mut out,
        );
        assert_eq!(code, SUCCESS);
        assert_eq!(
            out,
            vec![format!(
                "ConsolidateDBsApp::consolidateSecondaryDB: Consolidating '{path}'"
            )]
        );
        let record = load_entity(
            &storage,
            &EntityKey {
                type_id: 3,
                db_id: 1,
            },
        )
        .expect("the entity loads");
        assert_eq!(record.values, vec![PropertyValue::Uint(10)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_checksum_table_fails_the_file() {
        // flip only, no tbl_checksum: the file opens but the digest read
        // fails inside consolidateSecondaryDB.
        let path = unique_path();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                &format!(
                    "CREATE TABLE {FLIP_TABLE} (sm_dbID INTEGER, sm_typeID INTEGER, \
                     sm_time INTEGER, sm_blob BLOB)"
                ),
                [],
            )
            .unwrap();
        drop(connection);

        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let mut out = Vec::new();
        assert!(!consolidator.consolidate_files(std::slice::from_ref(&path), &mut out));
        assert_eq!(
            out[0],
            format!("ConsolidateDBsApp::consolidateSecondaryDB: Consolidating '{path}'")
        );
        assert!(
            out[1].contains("failed to read the checksum table"),
            "got: {out:?}"
        );
        assert!(!consolidator.errors().has_errors());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_ill_formed_digest_fails_the_file() {
        // A checksum row that does not unquote: the gate announces the
        // bad digest and then refuses the file.
        let path = fixture_file(&[(1, 200, vec![PropertyValue::Uint(10)])], "junk");

        let defs = defs();
        let mut storage = seeded_primary();
        let mut consolidator = StorageConsolidator::new(&mut storage, &defs, &digest(1));
        let mut out = Vec::new();
        assert!(!consolidator.consolidate_files(std::slice::from_ref(&path), &mut out));
        assert_eq!(
            out[1],
            "ConsolidateDBsApp::checkEntityDefsDigestMatch: Not a valid MD5 digest"
        );
        assert_eq!(
            out[2],
            format!("ConsolidateDBsApp::consolidateSecondaryDB: {path} failed entity digest check")
        );
        assert!(!consolidator.errors().has_errors());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_ledger_answers_its_three_questions() {
        let mut errors = DbConsolidatorErrors::default();
        assert!(!errors.has_errors());
        assert!(!errors.secondary_db_has_error("a.sqlite"));
        errors.add_secondary_db("a.sqlite");
        assert!(errors.has_errors());
        assert!(errors.secondary_db_has_error("a.sqlite"));
        assert!(!errors.secondary_db_has_error("b.sqlite"));
    }

    #[test]
    fn the_digest_gate_binds_the_quoted_forms() {
        // Same digest, both quoted: the compare is on raw bytes, the
        // validity check is the length.
        let quoted = digest(1);
        assert!(defs_digest_matches(&quoted, &quoted));
        assert!(!defs_digest_matches("short", &quoted));
        assert!(!defs_digest_matches(&quoted, "short"));
        assert!(!defs_digest_matches("not-a-digest", &quoted));
    }
}
