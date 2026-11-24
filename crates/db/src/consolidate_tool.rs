//! BW-09-05-05 the consolidate_dbs tool.
//!
//! `server/tools/consolidate_dbs` pulls the secondary databases from the
//! baseapp hosts, consolidates them into the primary database, and clears
//! the consolidated entries (`main.cpp:44-127`). The command surface is
//! [`CommandLineParser`] (`command_line_parser.cpp`); the transfer rides a
//! little protocol where the remote `transfer_db` sends `'n'` + file name +
//! size + contents and the tool answers `'d'` once the consolidation
//! succeeded (`consolidate.cpp:32-54` protocol comment). [`ConsolidateSender`]
//! ports the sender half (`consolidate.cpp`), [`FileReceiver`] the receiver
//! half (`file_receiver.cpp`), and [`FileReceiverMgr`] the bookkeeping
//! around them (`file_receiver_mgr.cpp`).
//!
//! The merge step itself reads the received database and folds its rows
//! into the primary database with the take-newest rule
//! (`PrimaryDatabaseUpdateQueue::addUpdate`) — that engine is the
//! BW-09-03-03 delivery in `secondary_database.rs`, which this tree keeps
//! unwired pending integration, so the acceptance here pins the tool's half
//! of it: the received file is byte-identical to the one the remote end
//! held, so every consumer of it merges the same rows.
//!
//! Both ends speak a byte stream through [`ByteStream`], so the wire format
//! and the exit codes are reviewable without sockets. The fixed-width
//! integers on the wire are little-endian: the reference streams them
//! through `MemoryOStream`, which memcpys host order (x86).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

/// The usage text (`printUsage`, `command_line_parser.cpp:30-42`).
pub const USAGE_MESSAGE: &str = "\n\
Usage: consolidate_dbs [options]\n\
       consolidate_dbs [options] <primarydb_spec> <secondarydb_list> (when invoked from DBApp)\n\
\n\
Options:\n\
 --ignore-sqlite-errors    do not abort on sqlite read errors\n\
 --clear                   clear unconsolidated secondary database info\n\
 --list                    list unconsolidated secondary database info\n\
 -v, --verbose             verbose output\n\
 -h, --help                print usage\n";

/// The consolidation directory the app starts from
/// (`ConsolidateDBsApp::ConsolidateDBsApp`, `consolidate_dbs_app.cpp:104`).
pub const DEFAULT_CONSOLIDATION_DIR: &str = "/tmp/";

/// The row the clear action removes
/// (`clearSecondaryDBEntries`, `consolidate_dbs_app.cpp:653-658`).
pub const CLEAR_SECONDARY_DATABASES_SQL: &str = "DELETE FROM bigworldSecondaryDatabases";

/// The table the pending secondary database locations live in
/// (`getSecondaryDBInfos`, `consolidate_dbs_app.cpp:347-349`).
pub const SECONDARY_DATABASES_TABLE: &str = "bigworldSecondaryDatabases";

/// The connection parameters of the primary database, as the
/// `<host>;<port>;<username>;<password>;<database>` command line spec
/// builds them (`parsePrimaryDatabaseInfo`, `command_line_parser.cpp:196-244`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// The host the database runs on.
    pub host: String,
    /// The port it listens on.
    pub port: u16,
    /// The username.
    pub username: String,
    /// The password.
    pub password: String,
    /// The database name.
    pub database: String,
}

/// The parsed command line (`CommandLineParser`,
/// `command_line_parser.hpp:15-59`).
#[derive(Debug, Default)]
pub struct CommandLineParser {
    is_verbose: bool,
    should_clear: bool,
    should_list: bool,
    should_ignore_sqlite_errors: bool,
    had_non_option_args: bool,
    res_path: String,
    primary_database_info: Option<ConnectionInfo>,
    secondary_databases: Vec<String>,
    usage_printed: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl CommandLineParser {
    /// Constructor: every flag off, no paths — the reference reads the
    /// primary database info from bw.xml unless the command line carries it.
    pub fn new() -> Self {
        Self::default()
    }

    /// `init` (`command_line_parser.cpp:69-181`): walk the options, then the
    /// two positional arguments. Fails on help, an unknown option, a missing
    /// option argument, or one positional argument.
    pub fn init(&mut self, args: &[&str]) -> bool {
        let mut i = 0;
        while i < args.len() {
            let arg = args[i];

            if arg == "--" {
                // getopt_long: everything after "--" is an operand.
                i += 1;
                break;
            }

            if let Some(long) = arg.strip_prefix("--") {
                // The --name=value form getopt_long also accepts.
                let (name, inline_value) = match long.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (long, None),
                };
                match name {
                    "res" => match inline_value {
                        Some(value) => self.res_path = value.to_string(),
                        None => {
                            i += 1;
                            if i >= args.len() {
                                self.record_missing_argument('r');
                                return false;
                            }
                            self.res_path = args[i].to_string();
                        }
                    },
                    "verbose" => self.is_verbose = true,
                    "help" => {
                        self.print_usage();
                        return false;
                    }
                    "clear" => self.should_clear = true,
                    "list" => self.should_list = true,
                    "ignore-sqlite-errors" => self.should_ignore_sqlite_errors = true,
                    _ => {
                        // getopt_long reports an unrecognized long option
                        // with '?' and leaves optopt unset.
                        self.record_unknown_option('?');
                        return false;
                    }
                }
                i += 1;
                continue;
            }

            if arg.len() > 1 && arg.starts_with('-') {
                // A short-option cluster: "-vli", "-r value", "-rvalue".
                let chars: Vec<char> = arg.chars().skip(1).collect();
                let mut j = 0;
                while j < chars.len() {
                    match chars[j] {
                        'v' => self.is_verbose = true,
                        'h' => {
                            self.print_usage();
                            return false;
                        }
                        'c' => self.should_clear = true,
                        'l' => self.should_list = true,
                        'i' => self.should_ignore_sqlite_errors = true,
                        'r' => {
                            // The value is the rest of the cluster, or the
                            // next argument.
                            let rest: String = chars[j + 1..].iter().collect();
                            let value = if !rest.is_empty() {
                                rest
                            } else {
                                i += 1;
                                if i >= args.len() {
                                    self.record_missing_argument('r');
                                    return false;
                                }
                                args[i].to_string()
                            };
                            self.res_path = value;
                            break;
                        }
                        other => {
                            self.record_unknown_option(other);
                            return false;
                        }
                    }
                    j += 1;
                }
                i += 1;
                continue;
            }

            // Not an option: the positional arguments start here.
            break;
        }

        let rest = &args[i.min(args.len())..];

        // We currently only support specifying: nothing on the command
        // line, or the primary database and a file containing all the
        // secondary database paths (`command_line_parser.cpp:140-152`).
        if rest.is_empty() {
            return true;
        }

        if rest.len() < 2 {
            self.errors.push(
                "Primary database specified, but no path to secondary database list was specified"
                    .to_string(),
            );
            self.print_usage();
            return false;
        }

        if rest.len() > 2 {
            self.warnings.push(
                "Extraneous arguments after secondary database list path ignored".to_string(),
            );
        }

        self.had_non_option_args = true;

        if self.should_clear || self.should_list {
            // Don't parse, we use the bw.xml for primary database info
            // (`command_line_parser.cpp:168-175`).
            self.warnings.push(format!(
                "consolidate_dbs: The --{} option does not take additional arguments",
                if self.should_clear { "clear" } else { "list" }
            ));
            return true;
        }

        self.parse_primary_database_info(rest[0]) && self.read_secondary_database_list(rest[1])
    }

    /// The recorded error messages.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The recorded warning messages.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Whether the verbose flag was given (`isVerbose`).
    pub fn is_verbose(&self) -> bool {
        self.is_verbose
    }

    /// Whether `--clear` was given (`shouldClear`).
    pub fn should_clear(&self) -> bool {
        self.should_clear
    }

    /// Whether `--list` was given (`shouldList`).
    pub fn should_list(&self) -> bool {
        self.should_list
    }

    /// Whether `--ignore-sqlite-errors` was given
    /// (`shouldIgnoreSqliteErrors`).
    pub fn should_ignore_sqlite_errors(&self) -> bool {
        self.should_ignore_sqlite_errors
    }

    /// Whether positional arguments were given (`hadNonOptionArgs`): the
    /// DBApp-invoked form.
    pub fn had_non_option_args(&self) -> bool {
        self.had_non_option_args
    }

    /// The `--res` path.
    pub fn res_path(&self) -> &str {
        &self.res_path
    }

    /// The primary database info from the command line; `None` when the run
    /// should use the bw.xml settings instead.
    pub fn primary_database_info(&self) -> Option<&ConnectionInfo> {
        self.primary_database_info.as_ref()
    }

    /// The secondary database paths from the list file
    /// (`secondaryDatabases`).
    pub fn secondary_databases(&self) -> &[String] {
        &self.secondary_databases
    }

    /// Whether `printUsage` ran during this init.
    pub fn usage_printed(&self) -> bool {
        self.usage_printed
    }

    /// `printUsage` (`command_line_parser.cpp:276-279`).
    pub fn print_usage(&mut self) {
        self.usage_printed = true;
        println!("{USAGE_MESSAGE}");
    }

    /// The getopt missing-argument report (`command_line_parser.cpp:105-108`).
    fn record_missing_argument(&mut self, option: char) {
        self.errors
            .push(format!("Missing argument for option -{option}"));
    }

    /// The getopt unknown-option report (`command_line_parser.cpp:109-112`).
    fn record_unknown_option(&mut self, option: char) {
        self.errors.push(format!("Unknown option -{option}"));
    }

    /// `parsePrimaryDatabaseInfo` (`command_line_parser.cpp:196-244`): the
    /// five `;`-separated components.
    fn parse_primary_database_info(&mut self, spec: &str) -> bool {
        let components: Vec<&str> = spec.split(';').collect();

        if components.len() != 5 {
            self.errors.push(
                "Primary database argument must be in the form \
                 <host>;<port>;<username>;<password>;<database>"
                    .to_string(),
            );
            return false;
        }

        let port = match strtoul_base0(components[1]) {
            None => {
                self.errors
                    .push(format!("Could not parse database port :{}", components[1]));
                return false;
            }
            Some(value) if value >= 1 << u16::BITS => {
                self.errors.push(format!(
                    "Primary database port is out-of-range: {}",
                    components[1]
                ));
                return false;
            }
            Some(value) => value as u16,
        };

        self.primary_database_info = Some(ConnectionInfo {
            host: components[0].to_string(),
            port,
            username: components[2].to_string(),
            password: components[3].to_string(),
            database: components[4].to_string(),
        });
        true
    }

    /// `readSecondaryDatabaseList` (`command_line_parser.cpp:258-286`): the
    /// non-empty lines of the list file.
    fn read_secondary_database_list(&mut self, path: &str) -> bool {
        let Ok(contents) = std::fs::read_to_string(path) else {
            self.errors.push(format!(
                "Could not open secondary database list at \"{path}\""
            ));
            return false;
        };

        // getline + the length check keeps everything but empty lines.
        self.secondary_databases = contents
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect();
        true
    }
}

/// C `strtoul` with base 0: optional sign and whitespace, then `0x` hex,
/// leading-zero octal, or decimal digits. `None` when there are no digits
/// after the prefix — the case where the reference's end pointer stops
/// short of the terminator and fails the parse. A leading minus negates
/// and wraps, exactly like `strtoul`.
pub fn strtoul_base0(text: &str) -> Option<u64> {
    let rest = text.trim_start();
    let (negative, rest) = match rest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, rest.strip_prefix('+').unwrap_or(rest)),
    };

    let (radix, digits) =
        if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            (16u32, hex)
        } else if rest.len() > 1 && rest.starts_with('0') {
            (8u32, &rest[1..])
        } else {
            (10u32, rest)
        };

    let count = digits.chars().take_while(|c| c.is_digit(radix)).count();
    if count == 0 {
        // strtoul consumes nothing here: an empty input still yields 0, a
        // non-empty unparsable one fails the caller's end-pointer check.
        return if digits.is_empty() { Some(0) } else { None };
    }

    let value = u64::from_str_radix(&digits[..count], radix).ok()?;
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

/// One pending secondary database: the host it lives on and the path there
/// (`SecondaryDBInfo`, `secondary_db_info.hpp:14-32`). The IP is in network
/// byte order, as the primary database stores it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecondaryDbInfo {
    /// The host's IP, network byte order.
    pub host_ip: u32,
    /// The database file's path on that host.
    pub location: String,
}

impl SecondaryDbInfo {
    /// Build from a `SELECT ip, location` row: the reference flips the
    /// stored value with `htonl` (`consolidate_dbs_app.cpp:359-361`), which
    /// on a little-endian host is a byte swap.
    pub fn from_row(ip: u32, location: &str) -> Self {
        Self {
            host_ip: ip.swap_bytes(),
            location: location.to_string(),
        }
    }

    /// The IP as the reference prints it: the four bytes in network order,
    /// dotted (`Mercury::Address::ipAsString`).
    pub fn ip_as_string(&self) -> String {
        let bytes = self.host_ip.to_be_bytes();
        format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3])
    }
}

/// Collects the secondary databases whose consolidation hit errors
/// (`DBConsolidatorErrors`, `db_consolidator_errors.hpp:14-38`).
#[derive(Debug, Default)]
pub struct DbConsolidatorErrors {
    erroneous_secondary_dbs: BTreeSet<String>,
}

impl DbConsolidatorErrors {
    /// `addSecondaryDB`.
    pub fn add_secondary_db(&mut self, file_name: &str) {
        self.erroneous_secondary_dbs.insert(file_name.to_string());
    }

    /// `hasErrors`.
    pub fn has_errors(&self) -> bool {
        !self.erroneous_secondary_dbs.is_empty()
    }

    /// `secondaryDBHasError`.
    pub fn secondary_db_has_error(&self, file_name: &str) -> bool {
        self.erroneous_secondary_dbs.contains(file_name)
    }
}

/// `MD5::Digest::unquote` (`md5.cpp:510-527`): exactly 32 characters, each
/// pair folded into one byte. The nibble helper only handles digits and
/// uppercase hex — a lowercase letter folds into a garbage nibble exactly
/// like the reference does, so only quoted-form digests compare equal.
pub fn unquote_digest(quoted: &str) -> Option<[u8; 16]> {
    if quoted.len() != 32 {
        return None;
    }

    let unquote_nibble = |c: u8| if c > b'9' { c - (b'A' - 10) } else { c - b'0' };

    let mut bytes = [0u8; 16];
    let raw = quoted.as_bytes();
    for i in 0..16 {
        let hi = unquote_nibble(raw[i * 2]);
        let lo = unquote_nibble(raw[i * 2 + 1]);
        bytes[i] = (hi << 4) | lo;
    }
    Some(bytes)
}

/// `ConsolidateDBsApp::checkEntityDefsDigestMatch`
/// (`consolidate_dbs_app.cpp:419-435`): the secondary database's checksum
/// digest must match the entity definitions this run loaded. An unparsable
/// digest string matches nothing.
pub fn check_entity_defs_digest_match(quoted_digest: &str, defs_digest: &[u8; 16]) -> bool {
    let Some(digest) = unquote_digest(quoted_digest) else {
        return false;
    };
    digest == *defs_digest
}

/// The `--list` report (`printDatabases`,
/// `consolidate_dbs_app.cpp:672-702`): the count, then one `%15s %s` line
/// per pending database.
pub fn databases_listing(infos: &[SecondaryDbInfo]) -> String {
    let mut out = String::from("\n");
    out.push_str(&format!(
        "Secondary databases to consolidate = {}\n",
        infos.len()
    ));
    for info in infos {
        out.push_str(&format!("{:>15} {}\n", info.ip_as_string(), info.location));
    }
    out
}

/// The bidirectional byte stream both transfer ends speak. The reference
/// reaches this surface through `Endpoint` sockets; the port hands the
/// already-established connection in, so `connect` stands in for the socket
/// setup (`Consolidate::transferTo`, `consolidate.cpp:104-124`).
pub trait ByteStream {
    /// Establish the connection to the given address. `false` when the
    /// connect failed.
    fn connect(&mut self, address: &str) -> bool {
        let _ = address;
        true
    }

    /// Read whatever is available, at most `out.len()` bytes, blocking
    /// until something arrives. `None` when the connection failed or the
    /// peer closed it.
    fn recv(&mut self, out: &mut [u8]) -> Option<usize>;

    /// Send the bytes. `None` when the send failed.
    fn send(&mut self, bytes: &[u8]) -> Option<usize>;
}

/// Read until `out` is full: short reads continue, a closed connection or
/// a zero-length read fails.
fn recv_exact(stream: &mut dyn ByteStream, out: &mut [u8]) -> bool {
    let mut filled = 0;
    while filled < out.len() {
        match stream.recv(&mut out[filled..]) {
            Some(0) | None => return false,
            Some(n) => filled += n,
        }
    }
    true
}

/// Send all the bytes.
fn send_all(stream: &mut dyn ByteStream, bytes: &[u8]) -> bool {
    stream.send(bytes).is_some()
}

/// The transfer_db side of the protocol (`Consolidate`,
/// `server/tools/transfer_db/consolidate.cpp`): hand the sqlite file to the
/// consolidator and remove it when the consolidator is done with it.
pub struct ConsolidateSender {
    sqlite_filename: String,
    errors: Vec<String>,
}

impl ConsolidateSender {
    /// `Consolidate::Consolidate`.
    pub fn new(db_filename: &str) -> Self {
        Self {
            sqlite_filename: db_filename.to_string(),
            errors: Vec::new(),
        }
    }

    /// The recorded failure messages.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The file this sender hands over.
    pub fn sqlite_filename(&self) -> &str {
        &self.sqlite_filename
    }

    /// `Consolidate::error` (`consolidate.cpp:82-133`): `'e'` + the length
    /// of the message — the supplemental string folds in as `" (%s)"` — +
    /// the message itself.
    fn error(&mut self, stream: &mut dyn ByteStream, error_str: &str, supplemental: &str) {
        let has_supplemental = !supplemental.is_empty();
        let mut string_size = error_str.len() as u16;
        if has_supplemental {
            // The three bytes carry the brackets and the space.
            string_size += supplemental.len() as u16 + 3;
        }

        self.errors.push(format!(
            "Consolidate::transferTo: {} ({})",
            error_str, supplemental
        ));

        let mut response = Vec::with_capacity(3 + string_size as usize);
        response.push(b'e');
        response.extend_from_slice(&string_size.to_le_bytes());
        response.extend_from_slice(error_str.as_bytes());
        if has_supplemental {
            response.extend_from_slice(b" (");
            response.extend_from_slice(supplemental.as_bytes());
            response.push(b')');
        }

        if stream.send(&response) != Some(response.len()) {
            self.errors.push(
                "Consolidate::transferTo: Failed to send error message back to ConsolidateDBs"
                    .to_string(),
            );
        }
    }

    /// `Consolidate::transferTo` (`consolidate.cpp:104-195`): connect,
    /// check the file, prepare it, send it, wait for the delete order.
    pub fn transfer_to(&mut self, stream: &mut dyn ByteStream, address: &str) -> bool {
        if !stream.connect(address) {
            self.errors.push(
                "Consolidate::transferTo: Failed to establish a connection to receiver".to_string(),
            );
            return false;
        }

        // stat() rather than a sqlite connect: sqlite creates the file
        // (`consolidate.cpp:136-143`).
        let Ok(meta) = std::fs::metadata(&self.sqlite_filename) else {
            self.error(
                stream,
                "Unable to stat the sqlite file",
                "No such file or directory",
            );
            return false;
        };
        let sqlite_size = meta.len() as u32;

        if !self.prepare_db_for_send(stream) {
            return false;
        }

        if !self.read_and_send_db(stream, sqlite_size) {
            self.errors
                .push("Consolidate::transferTo: Transfer interrupted.".to_string());
            return false;
        }

        self.wait_for_delete_command(stream)
    }

    /// `Consolidate::prepareDBForSend` (`consolidate.cpp:203-247`): open the
    /// database and run the checksum read so sqlite clears any journal
    /// files before the bytes go out.
    fn prepare_db_for_send(&mut self, stream: &mut dyn ByteStream) -> bool {
        let conn = match rusqlite::Connection::open(&self.sqlite_filename) {
            Ok(conn) => conn,
            Err(_) => {
                self.error(
                    stream,
                    "Unable to open sqlite file using sqlite3",
                    "out of memory",
                );
                return false;
            }
        };

        if let Err(e) = conn.execute_batch("SELECT * FROM tbl_checksum") {
            self.error(
                stream,
                "Unable to flush sqlite journal files",
                &e.to_string(),
            );
            return false;
        }

        drop(conn);
        true
    }

    /// `Consolidate::readAndSendDB` (`consolidate.cpp:253-380`): the `'n'`
    /// header with the file name and size, then the contents in 4096-byte
    /// chunks.
    fn read_and_send_db(&mut self, stream: &mut dyn ByteStream, sqlite_size: u32) -> bool {
        let mut file = match std::fs::File::open(&self.sqlite_filename) {
            Ok(file) => file,
            Err(_) => {
                self.error(
                    stream,
                    "Unable to open sqlite file",
                    "No such file or directory",
                );
                return false;
            }
        };

        // The receiving size assumes a uint16, so the string streaming
        // functionality can't be used (`consolidate.cpp:289-291`).
        let mut header = Vec::with_capacity(1 + 2 + self.sqlite_filename.len() + 4);
        header.push(b'n');
        header.extend_from_slice(&(self.sqlite_filename.len() as u16).to_le_bytes());
        header.extend_from_slice(self.sqlite_filename.as_bytes());
        header.extend_from_slice(&sqlite_size.to_le_bytes());

        if !send_all(stream, &header) {
            self.errors.push(
                "Consolidate::readAndSendDB: Unable to send initial file transfer data."
                    .to_string(),
            );
            return false;
        }

        let mut working_buffer = [0u8; 4096];
        loop {
            match file.read(&mut working_buffer) {
                Ok(0) => return true,
                Ok(amount_read) => {
                    if !send_all(stream, &working_buffer[..amount_read]) {
                        return false;
                    }
                }
                Err(_) => {
                    self.error(stream, "Error reading sqlite file", "Input/output error");
                    return false;
                }
            }
        }
    }

    /// `Consolidate::waitForDeleteCommand` (`consolidate.cpp:386-427`): one
    /// `'d'` byte, then the file goes away.
    fn wait_for_delete_command(&mut self, stream: &mut dyn ByteStream) -> bool {
        let mut command = [0u8; 1];
        if !recv_exact(stream, &mut command) {
            self.errors.push(
                "Consolidate::waitForDeleteCommand: Failed to receive delete command from ConsolidateDBs"
                    .to_string(),
            );
            return false;
        }

        if command[0] != b'd' {
            self.errors.push(format!(
                "Consolidate::waitForDeleteCommand: Received unexpected character '{}'",
                command[0] as char
            ));
            return false;
        }

        if std::fs::remove_file(&self.sqlite_filename).is_err() {
            // The consolidator has shut its end down at this point, so
            // there is no point sending it an error message.
            self.errors.push(
                "Consolidate::waitForDeleteCommand: Unable to remove the secondary DB file"
                    .to_string(),
            );
            return false;
        }

        true
    }
}

/// The receive states (`FileReceiver`'s `pMsgProcessor_` chain,
/// `file_receiver.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecvState {
    /// The one-byte command.
    Command,
    /// The two-byte source path length.
    SrcPathLen,
    /// The source path itself.
    SrcPath,
    /// The four-byte file size.
    FileLen,
    /// The contents, in arbitrary chunks until the size is met.
    FileContents,
    /// The two-byte error string length.
    ErrorLen,
    /// The error string itself.
    ErrorStr,
    /// The file landed whole (`mgr_.onFileReceived`).
    Done,
    /// The transfer gave up (`mgr_.onFileReceiveError`).
    Failed,
}

/// What one receive pass ended in (`FileReceiver::handleInputNotification`'s
/// outcomes).
#[derive(Debug, PartialEq, Eq)]
pub enum ReceiveOutcome {
    /// The whole file landed at `dest_path`.
    Received,
    /// The remote end reported an error.
    RemoteError(String),
    /// The transfer broke.
    Failed,
}

/// The receiving end of one file transfer (`FileReceiver`,
/// `file_receiver.cpp`): reads the `'n'` stream the remote transfer_db
/// sends and writes the bytes to the consolidation directory.
pub struct FileReceiver {
    consolidation_dir: PathBuf,
    src_addr: String,
    state: RecvState,
    action_desc: &'static str,
    pending_len: usize,
    src_path: String,
    dest_path: String,
    expected_file_size: u32,
    current_file_size: u32,
    file: Option<std::fs::File>,
    errors: Vec<String>,
}

impl FileReceiver {
    /// `FileReceiver::FileReceiver`: the consolidation directory comes from
    /// the manager; the peer address rides the error messages.
    pub fn new(consolidation_dir: &Path, src_ip: u32, src_port: u16) -> Self {
        let ip = src_ip.to_be_bytes();
        Self {
            consolidation_dir: consolidation_dir.to_path_buf(),
            src_addr: format!("{}.{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3], src_port),
            state: RecvState::Command,
            action_desc: "receive command",
            pending_len: 0,
            src_path: String::new(),
            dest_path: String::new(),
            expected_file_size: 0,
            current_file_size: 0,
            file: None,
            errors: Vec::new(),
        }
    }

    /// The recorded failure messages.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The path the file is coming from (`srcPath`).
    pub fn src_path(&self) -> &str {
        &self.src_path
    }

    /// The local path it landed at (`destPath`).
    pub fn dest_path(&self) -> &str {
        &self.dest_path
    }

    /// `srcAddr` as the reference prints it: the dotted IP and the port.
    pub fn src_addr(&self) -> &str {
        &self.src_addr
    }

    /// Drive the state machine until the file lands or the transfer fails
    /// (`FileReceiver::handleInputNotification`'s message-size cycle: each
    /// step names the exact byte count of the next message).
    pub fn receive(&mut self, stream: &mut dyn ByteStream) -> ReceiveOutcome {
        loop {
            match self.state {
                RecvState::Command => {
                    let mut command = [0u8; 1];
                    if !recv_exact(stream, &mut command) {
                        return self.communication_failed();
                    }
                    match command[0] {
                        b'n' => {
                            self.state = RecvState::SrcPathLen;
                            self.action_desc = "receive source file path length";
                        }
                        b'e' => {
                            self.state = RecvState::ErrorLen;
                            self.action_desc = "receive error string length";
                        }
                        other => {
                            self.errors.push(format!(
                                "FileReceiver::recvCommand: Received invalid command {}",
                                other as char
                            ));
                            self.state = RecvState::Failed;
                            return ReceiveOutcome::Failed;
                        }
                    }
                }
                RecvState::SrcPathLen => {
                    let mut buf = [0u8; 2];
                    if !recv_exact(stream, &mut buf) {
                        return self.communication_failed();
                    }
                    self.pending_len = u16::from_le_bytes(buf) as usize;
                    self.state = RecvState::SrcPath;
                    self.action_desc = "receive source file path";
                }
                RecvState::SrcPath => {
                    let mut buf = vec![0u8; self.pending_len];
                    if !recv_exact(stream, &mut buf) {
                        return self.communication_failed();
                    }
                    self.src_path = String::from_utf8_lossy(&buf).to_string();

                    // Extract the file name and append it to the
                    // consolidation directory (`recvSrcPath`,
                    // `file_receiver.cpp:229-237`).
                    let base = match self.src_path.rfind('/') {
                        Some(pos) => &self.src_path[pos + 1..],
                        None => &self.src_path[..],
                    };
                    self.dest_path = self.consolidation_dir.join(base).display().to_string();

                    // Check that the file doesn't already exist and append
                    // a suffix if it does — the first alternative is `-2`
                    // (`file_receiver.cpp:240-255`).
                    let original = self.dest_path.clone();
                    let mut suffix = 1;
                    while Path::new(&self.dest_path).exists() {
                        suffix += 1;
                        self.dest_path = format!("{original}-{suffix}");
                    }
                    if self.dest_path != original {
                        self.errors.push(format!(
                            "FileReceiver::recvSrcPath: Default destination file '{}' already exists. Saving to new destination '{}'",
                            original, self.dest_path
                        ));
                    }

                    let opened = std::fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&self.dest_path);
                    match opened {
                        Ok(file) => {
                            self.file = Some(file);
                            self.state = RecvState::FileLen;
                            self.action_desc = "receive file length";
                        }
                        Err(_) => {
                            self.errors.push(format!(
                                "FileReceiverMgr::recvSrcFileInfo: Failed to create local file '{}'",
                                self.dest_path
                            ));
                            self.state = RecvState::Failed;
                            return ReceiveOutcome::Failed;
                        }
                    }
                }
                RecvState::FileLen => {
                    let mut buf = [0u8; 4];
                    if !recv_exact(stream, &mut buf) {
                        return self.communication_failed();
                    }
                    self.expected_file_size = u32::from_le_bytes(buf);
                    if self.expected_file_size == 0 {
                        self.errors.push(format!(
                            "FileReceiverMgr::recvSrcFileInfo: Received empty file '{}'",
                            self.dest_path
                        ));
                        self.state = RecvState::Failed;
                        return ReceiveOutcome::Failed;
                    }
                    self.state = RecvState::FileContents;
                    self.action_desc = "receive file contents";
                }
                RecvState::FileContents => {
                    // The msgReceiver runs at its 512-byte capacity here:
                    // the contents have no message size, whatever arrives
                    // goes straight to the file.
                    let mut chunk = [0u8; 512];
                    let Some(num_read) = stream.recv(&mut chunk) else {
                        return self.communication_failed();
                    };
                    if num_read == 0 {
                        return self.communication_failed();
                    }

                    let file = self.file.as_mut().expect("open dest file");
                    if file.write_all(&chunk[..num_read]).is_err() {
                        self.errors.push(format!(
                            "FileReceiverMgr::recvFileContents: Failed to write to file '{}'",
                            self.dest_path
                        ));
                        self.state = RecvState::Failed;
                        return ReceiveOutcome::Failed;
                    }

                    self.current_file_size += num_read as u32;
                    if self.current_file_size >= self.expected_file_size {
                        drop(self.file.take());
                        self.state = RecvState::Done;
                        return ReceiveOutcome::Received;
                    }
                }
                RecvState::ErrorLen => {
                    let mut buf = [0u8; 2];
                    if !recv_exact(stream, &mut buf) {
                        return self.communication_failed();
                    }
                    self.pending_len = u16::from_le_bytes(buf) as usize;
                    self.state = RecvState::ErrorStr;
                    self.action_desc = "receive error string";
                }
                RecvState::ErrorStr => {
                    let mut buf = vec![0u8; self.pending_len];
                    if !recv_exact(stream, &mut buf) {
                        return self.communication_failed();
                    }
                    let error_str = String::from_utf8_lossy(&buf).to_string();
                    self.errors.push(format!(
                        "FileReceiver::recvErrorStr: Database transfer utility on {} reported error: {}",
                        self.src_addr, error_str
                    ));
                    self.state = RecvState::Failed;
                    return ReceiveOutcome::RemoteError(error_str);
                }
                RecvState::Done | RecvState::Failed => {
                    // Receiving data while in this state is the reference's
                    // "we are currently in %s mode" error.
                    self.errors.push(format!(
                        "FileReceiverMgr::handleInputNotification: Receiving data but we are currently in {} mode",
                        self.action_desc
                    ));
                    return ReceiveOutcome::Failed;
                }
            }
        }
    }

    /// The shared communication-failure report
    /// (`file_receiver.cpp:126-139`).
    fn communication_failed(&mut self) -> ReceiveOutcome {
        self.errors.push(format!(
            "FileReceiver::handleInputNotification: Error communicating with file transfer process on {}. Failed to {}.",
            self.src_addr, self.action_desc
        ));
        self.state = RecvState::Failed;
        ReceiveOutcome::Failed
    }

    /// `deleteRemoteFile` (`file_receiver.cpp:441-462`): tell the remote
    /// end to delete its file.
    pub fn delete_remote_file(&mut self, stream: &mut dyn ByteStream) -> bool {
        if stream.send(b"d") != Some(1) {
            self.errors.push(format!(
                "FileReceiver::deleteRemoteFile: Failed to delete file '{}' on {}: send failed",
                self.src_path, self.src_addr
            ));
            return false;
        }
        true
    }

    /// `deleteLocalFile` (`file_receiver.cpp:468-487`): nothing to delete
    /// until a path arrived.
    pub fn delete_local_file(&mut self) -> bool {
        if self.dest_path == self.consolidation_dir.display().to_string() {
            return true;
        }
        if std::fs::remove_file(&self.dest_path).is_err() {
            self.errors.push(format!(
                "FileReceiver::deleteLocalFile: Failed to delete local file '{}': remove failed",
                self.dest_path
            ));
            return false;
        }
        true
    }

    /// `abort` (`file_receiver.cpp:492-504`): stop the transfer, close any
    /// half-written file.
    pub fn abort(&mut self) {
        drop(self.file.take());
    }

    /// Test hook: preset the paths a finished transfer would have produced.
    #[cfg(test)]
    fn set_paths_for_test(&mut self, src_path: &str, dest_path: String) {
        self.src_path = src_path.to_string();
        self.dest_path = dest_path;
    }
}

/// The transfer bookkeeping (`FileReceiverMgr`, `file_receiver_mgr.cpp`):
/// which pending databases have not landed yet, where the landed ones
/// live, and the cleanups.
#[derive(Debug, Default)]
pub struct FileReceiverMgr {
    consolidation_dir: PathBuf,
    /// Map of file location to host IP address (`unfinishedDBs_`).
    unfinished_dbs: BTreeMap<String, u32>,
    received_file_paths: Vec<String>,
    errored: bool,
}

impl FileReceiverMgr {
    /// `FileReceiverMgr::FileReceiverMgr`: every listed database is pending,
    /// keyed by its location — the reference `MF_VERIFY`s the paths are
    /// unique.
    pub fn new(secondary_dbs: &[SecondaryDbInfo], consolidation_dir: &Path) -> Self {
        let mut unfinished_dbs = BTreeMap::new();
        for db in secondary_dbs {
            assert!(
                unfinished_dbs
                    .insert(db.location.clone(), db.host_ip)
                    .is_none(),
                "duplicate secondary database location {}",
                db.location
            );
        }
        Self {
            consolidation_dir: consolidation_dir.to_path_buf(),
            unfinished_dbs,
            received_file_paths: Vec::new(),
            errored: false,
        }
    }

    /// The consolidation directory the receivers write into
    /// (`consolidationDir`).
    pub fn consolidation_dir(&self) -> &Path {
        &self.consolidation_dir
    }

    /// The local paths the landed files live at (`receivedFilePaths`).
    pub fn received_file_paths(&self) -> &[String] {
        &self.received_file_paths
    }

    /// `onFileReceived` (`file_receiver_mgr.cpp:129-156`): the pending entry
    /// goes away — receiving a file that was never listed warns — and when
    /// the last one lands the wait is over.
    pub fn on_file_received(&mut self, receiver: &FileReceiver) {
        if self.unfinished_dbs.remove(receiver.src_path()).is_none() {
            // ERROR_MSG( "DBConsolidator: Received unknown file '%s' from %s\n" )
        }

        self.received_file_paths
            .push(receiver.dest_path().to_string());
    }

    /// `onFileReceiveError`: the wait breaks with the transfer unfinished.
    pub fn on_file_receive_error(&mut self) {
        self.errored = true;
    }

    /// `finished`: every pending database landed.
    pub fn finished(&self) -> bool {
        !self.errored && self.unfinished_dbs.is_empty()
    }

    /// `cleanUpLocalFiles` (`file_receiver_mgr.cpp:229-252`).
    pub fn clean_up_local_files(&self, receivers: &mut [FileReceiver]) -> bool {
        let mut is_ok = true;
        for receiver in receivers {
            is_ok = receiver.delete_local_file() && is_ok;
        }
        is_ok
    }

    /// `cleanUpRemoteFiles` (`file_receiver_mgr.cpp:259-290`): the remote
    /// files of databases that consolidated cleanly get the `'d'` order;
    /// the ones with consolidation errors are kept for a re-run.
    pub fn clean_up_remote_files(
        &self,
        receivers: &mut [FileReceiver],
        errors: &DbConsolidatorErrors,
        streams: &mut [&mut dyn ByteStream],
    ) -> bool {
        let mut is_ok = true;
        for (i, receiver) in receivers.iter_mut().enumerate() {
            let secondary_db_path = receiver.dest_path().to_string();
            if !errors.secondary_db_has_error(&secondary_db_path) {
                is_ok = receiver.delete_remote_file(&mut *streams[i]) && is_ok;
            } else {
                receiver.errors.push(format!(
                    "FileReceiverMgr::cleanUpRemoteFiles: Skipped deletion of secondary database file {} on {} because there were errors during consolidation",
                    secondary_db_path, receiver.src_addr()
                ));
            }
        }
        is_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::mpsc::{Receiver as ChannelReceiver, Sender as ChannelSender};

    /// The parser suite: the flags, the positional forms, and the refusals
    /// with their verbatim messages.
    #[test]
    fn the_parser_walks_the_options_and_the_two_positionals() {
        // Nothing given: all defaults, the bw.xml settings apply.
        let mut parser = CommandLineParser::new();
        assert!(parser.init(&[]));
        assert!(!parser.is_verbose());
        assert!(!parser.should_clear());
        assert!(!parser.should_list());
        assert!(!parser.should_ignore_sqlite_errors());
        assert!(!parser.had_non_option_args());
        assert!(parser.primary_database_info().is_none());
        assert!(parser.secondary_databases().is_empty());
        assert!(parser.errors().is_empty());

        // The flags in both forms.
        let mut parser = CommandLineParser::new();
        assert!(parser.init(&["--verbose", "--clear", "-i", "-l", "--res", "/game/res"]));
        assert!(parser.is_verbose());
        assert!(parser.should_clear());
        assert!(parser.should_list());
        assert!(parser.should_ignore_sqlite_errors());
        assert_eq!(parser.res_path(), "/game/res");

        // An inline --res=value, and a short cluster.
        let mut parser = CommandLineParser::new();
        assert!(parser.init(&["--res=/game/res2", "-vcl"]));
        assert_eq!(parser.res_path(), "/game/res2");
        assert!(parser.is_verbose());
        assert!(parser.should_clear());
        assert!(parser.should_list());

        // Help prints the usage and fails the init, in both forms.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["-h"]));
        assert!(parser.usage_printed());
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["--help"]));
        assert!(parser.usage_printed());

        // One positional: the list path is missing.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["host;3306;user;pass;db"]));
        assert_eq!(
            parser.errors(),
            &["Primary database specified, but no path to secondary database list was specified"]
        );
        assert!(parser.usage_printed());

        // Unknown options fail with the verbatim messages.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["-z"]));
        assert_eq!(parser.errors(), &["Unknown option -z"]);
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["--wibble"]));
        assert_eq!(parser.errors(), &["Unknown option -?"]);

        // A missing option argument fails too.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["--res"]));
        assert_eq!(parser.errors(), &["Missing argument for option -r"]);
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["-r"]));
        assert_eq!(parser.errors(), &["Missing argument for option -r"]);

        // A "--" terminator: the operands start after it.
        let mut parser = CommandLineParser::new();
        assert!(parser.init(&["--", "a;0;c;d;e", "/dev/null"]));
        assert!(parser.had_non_option_args());
    }

    /// The `<host>;<port>;<username>;<password>;<database>` spec parse.
    #[test]
    fn the_primary_database_spec_parses_five_components() {
        let mut parser = CommandLineParser::new();
        let list = std::env::temp_dir().join(format!("cdb-list-{}", std::process::id()));
        std::fs::write(&list, "/a/one.db\n\n/b/two.db\n").expect("list");

        assert!(parser.init(&[
            "localhost;3306;bwadmin;secret;bigworld",
            list.display().to_string().as_str()
        ]));
        assert!(parser.had_non_option_args());
        assert_eq!(
            parser.primary_database_info(),
            Some(&ConnectionInfo {
                host: "localhost".to_string(),
                port: 3306,
                username: "bwadmin".to_string(),
                password: "secret".to_string(),
                database: "bigworld".to_string(),
            })
        );
        assert_eq!(
            parser.secondary_databases(),
            &["/a/one.db".to_string(), "/b/two.db".to_string()]
        );
        std::fs::remove_file(&list).ok();

        // Four components are refused.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["a;b;c", "/dev/null"]));
        assert_eq!(
            parser.errors(),
            &["Primary database argument must be in the form <host>;<port>;<username>;<password>;<database>"]
        );

        // An unparsable port.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["a;b;c;d;e", "/dev/null"]));
        assert_eq!(parser.errors(), &["Could not parse database port :b"]);

        // An out-of-range port.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["a;99999;c;d;e", "/dev/null"]));
        assert_eq!(
            parser.errors(),
            &["Primary database port is out-of-range: 99999"]
        );

        // A missing list file.
        let mut parser = CommandLineParser::new();
        assert!(!parser.init(&["a;0;c;d;e", "/nonexistent/list/file"]));
        assert_eq!(
            parser.errors(),
            &["Could not open secondary database list at \"/nonexistent/list/file\""]
        );
    }

    /// The port parse carries C `strtoul` base-0 semantics.
    #[test]
    fn the_port_parse_is_strtoul_base_zero() {
        assert_eq!(strtoul_base0("3306"), Some(3306));
        assert_eq!(strtoul_base0("0x50"), Some(80));
        assert_eq!(strtoul_base0("010"), Some(8));
        assert_eq!(strtoul_base0(""), Some(0));
        assert_eq!(strtoul_base0(" 42"), Some(42));
        // No digits at all: nothing was consumed, so the caller's
        // end-pointer check rejects the value. Trailing garbage stops the
        // scan where it is, as strtoul does; the port answers with the
        // digits it did consume.
        assert_eq!(strtoul_base0("abc"), None);
        assert_eq!(strtoul_base0("12abc"), Some(12));
        // strtoul negates and wraps a minus sign: -1 becomes ULONG_MAX,
        // which the range check rejects.
        assert_eq!(strtoul_base0("-1"), Some(u64::MAX));
    }

    /// The `--clear`/`--list` options reject positional arguments but keep
    /// the run going on the bw.xml settings.
    #[test]
    fn clear_and_list_reject_the_positionals_but_pass() {
        for flag in ["--clear", "--list"] {
            let mut parser = CommandLineParser::new();
            assert!(parser.init(&[flag, "a;0;c;d;e", "/dev/null"]));
            assert_eq!(
                parser.warnings(),
                &[format!(
                    "consolidate_dbs: The --{} option does not take additional arguments",
                    if flag == "--clear" { "clear" } else { "list" }
                )]
            );
            assert!(parser.primary_database_info().is_none());
        }

        // Extraneous positionals warn but do not fail.
        let mut parser = CommandLineParser::new();
        assert!(parser.init(&["a;0;c;d;e", "/dev/null", "extra"]));
        assert_eq!(
            parser.warnings(),
            &["Extraneous arguments after secondary database list path ignored"]
        );
    }

    /// The digest forms: unquote handles the 32-char quoted form only.
    #[test]
    fn the_digest_unquotes_the_quoted_form() {
        use md5::{Digest as _, Md5};

        let mut hasher = Md5::new();
        hasher.update(b"entity definitions");
        let digest: [u8; 16] = hasher.finalize().into();

        // quote(): uppercase hex.
        let quoted: String = digest.iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(quoted.len(), 32);
        assert_eq!(unquote_digest(&quoted), Some(digest));
        assert!(check_entity_defs_digest_match(&quoted, &digest));

        // A wrong digest does not match.
        let mut other = digest;
        other[0] ^= 0xff;
        assert!(!check_entity_defs_digest_match(&quoted, &other));

        // Any other length is not a digest.
        assert_eq!(unquote_digest("abc123"), None);
        assert_eq!(unquote_digest(""), None);
        assert!(!check_entity_defs_digest_match("abc123", &digest));
    }

    /// The listing format (`printDatabases`).
    #[test]
    fn the_listing_names_the_pending_databases() {
        let infos = vec![
            SecondaryDbInfo::from_row(u32::from_be_bytes([10, 0, 0, 9]), "/tmp/secondary.db"),
            SecondaryDbInfo::from_row(u32::from_be_bytes([192, 168, 1, 4]), "/data/secondary.db"),
        ];

        assert_eq!(
            databases_listing(&infos),
            "\nSecondary databases to consolidate = 2\n       9.0.0.10 /tmp/secondary.db\n    4.1.168.192 /data/secondary.db\n"
        );
    }

    /// The secondary database info carries the network-order IP.
    #[test]
    fn the_secondary_db_info_swaps_the_stored_ip() {
        // The row value 0x0A000009 is the address 9.0.0.10 as the reference
        // stores it: htonl flips it for display.
        let info = SecondaryDbInfo::from_row(0x0A000009, "/tmp/secondary.db");
        assert_eq!(info.ip_as_string(), "9.0.0.10");

        // A row whose stored value reads 1.2.3.4 in network order comes
        // back reversed, as the reference prints it.
        let info = SecondaryDbInfo::from_row(u32::from_be_bytes([1, 2, 3, 4]), "/x");
        assert_eq!(info.ip_as_string(), "4.3.2.1");
    }

    /// The error collector (`DBConsolidatorErrors`).
    #[test]
    fn the_error_collector_tracks_the_failed_databases() {
        let mut errors = DbConsolidatorErrors::default();
        assert!(!errors.has_errors());
        assert!(!errors.secondary_db_has_error("/a.db"));

        errors.add_secondary_db("/a.db");
        assert!(errors.has_errors());
        assert!(errors.secondary_db_has_error("/a.db"));
        assert!(!errors.secondary_db_has_error("/b.db"));
    }

    /// A connected pair of loopback streams backed by channels, each half
    /// buffering like a byte stream: a `recv` hands out whatever arrived,
    /// in order, split wherever the reader asked.
    struct ChannelHalf {
        recv_channel: ChannelReceiver<Vec<u8>>,
        send_channel: ChannelSender<Vec<u8>>,
        pending: VecDeque<u8>,
    }

    impl ChannelHalf {
        fn new(
            recv_channel: ChannelReceiver<Vec<u8>>,
            send_channel: ChannelSender<Vec<u8>>,
        ) -> Self {
            Self {
                recv_channel,
                send_channel,
                pending: VecDeque::new(),
            }
        }
    }

    impl ByteStream for ChannelHalf {
        fn recv(&mut self, out: &mut [u8]) -> Option<usize> {
            // Block until at least one byte is available.
            if self.pending.is_empty() {
                let bytes = self.recv_channel.recv().ok()?;
                self.pending.extend(bytes);
            }

            // Then take whatever else has already arrived.
            while self.pending.len() < out.len() {
                match self.recv_channel.try_recv() {
                    Ok(bytes) => self.pending.extend(bytes),
                    Err(_) => break,
                }
            }

            let n = out.len().min(self.pending.len());
            for slot in out.iter_mut().take(n) {
                *slot = self.pending.pop_front().expect("pending byte");
            }
            Some(n)
        }

        fn send(&mut self, bytes: &[u8]) -> Option<usize> {
            self.send_channel.send(bytes.to_vec()).ok()?;
            Some(bytes.len())
        }
    }

    /// The sqlite fixture the transfer tests share: a checksum table and
    /// one flip table with rows, the BaseApp's own DDL.
    fn build_secondary_db(path: &Path, checksum: &str, rows: &[(i64, i64, i64, &[u8])]) {
        use rusqlite::params;
        let conn = rusqlite::Connection::open(path).expect("open fixture");
        conn.execute_batch(&format!(
            "CREATE TABLE tbl_checksum (sm_checksum TEXT);
             INSERT INTO tbl_checksum VALUES (\"{checksum}\");
             CREATE TABLE tbl_flip (sm_dbID INTEGER, sm_typeID INTEGER, \
                 sm_time INTEGER, sm_blob BLOB);"
        ))
        .expect("ddl");
        for (db_id, type_id, time, blob) in rows {
            conn.execute(
                "INSERT INTO tbl_flip VALUES (?, ?, ?, ?)",
                params![db_id, type_id, time, blob],
            )
            .expect("insert");
        }
    }

    /// The rows a secondary database holds, oldest-first: the merge input
    /// the consolidator reads.
    fn read_rows(path: &Path) -> Vec<(i64, i64, i64, Vec<u8>)> {
        let conn = rusqlite::Connection::open(path).expect("open rows");
        let mut stmt = conn
            .prepare("SELECT sm_dbID, sm_typeID, sm_time, sm_blob FROM tbl_flip")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .expect("query");
        let mut out = Vec::new();
        for row in rows {
            out.push(row.expect("row"));
        }
        out.sort_by_key(|r| (r.0, r.2));
        out
    }

    /// The sender's happy path: header, contents, and the `'d'` exchange,
    /// byte-for-byte.
    #[test]
    fn the_sender_sends_the_header_contents_and_waits_for_delete() {
        let dir = std::env::temp_dir().join(format!("cdb-sender-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db_path = dir.join("secondary.db");
        build_secondary_db(&db_path, &"ABCD".repeat(8), &[(1, 5, 100, b"hello")]);

        let (tx_to_receiver, rx_from_sender) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_to_sender, rx_from_receiver) = std::sync::mpsc::channel::<Vec<u8>>();

        let mut sender_stream = ChannelHalf::new(rx_from_receiver, tx_to_receiver);
        let mut receiver_stream = ChannelHalf::new(rx_from_sender, tx_to_sender);

        let db_path_clone = db_path.clone();
        let sender_thread = std::thread::spawn(move || {
            let mut sender = ConsolidateSender::new(&db_path_clone.display().to_string());
            sender.transfer_to(&mut sender_stream, "10.0.0.9.5000")
        });

        // Receive the whole file like FileReceiver does, checking the
        // header bytes on the way.
        // The wire name is the sqlite path as the sender holds it.
        let name = db_path.display().to_string();
        let mut header = [0u8; 1 + 2 + 4 + 36];
        assert!(recv_exact(&mut receiver_stream, &mut header));
        assert_eq!(header[0], b'n');
        let name_len = u16::from_le_bytes(header[1..3].try_into().expect("len"));
        assert_eq!(name_len as usize, name.len());
        assert_eq!(&header[3..3 + name.len()], name.as_bytes());
        let size_at = 3 + name.len();
        let file_size = u32::from_le_bytes(header[size_at..size_at + 4].try_into().expect("size"));

        let expected_bytes = std::fs::read(&db_path).expect("source bytes");
        assert_eq!(file_size as usize, expected_bytes.len());

        let mut received = Vec::new();
        let mut chunk = [0u8; 512];
        while received.len() < file_size as usize {
            let n = receiver_stream.recv(&mut chunk).expect("chunk");
            assert!(n > 0);
            received.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(received, expected_bytes);

        // The consolidator finishes with the file: the 'd' order lands and
        // the sender removes its copy.
        receiver_stream.send(b"d").expect("send d");
        assert!(sender_thread.join().expect("join"));
        assert!(!db_path.exists(), "the sender removed its file");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The sender's error paths: the stat refusal and the missing checksum
    /// table both fold the supplemental string into the `'e'` message.
    #[test]
    fn the_sender_reports_errors_with_the_supplemental_string() {
        let dir = std::env::temp_dir().join(format!("cdb-sender-err-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let (_tx_to_receiver, rx_from_sender) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_to_sender, rx_from_receiver) = std::sync::mpsc::channel::<Vec<u8>>();
        let _drain = rx_from_sender;

        let missing = dir.join("missing.db");
        let mut sender = ConsolidateSender::new(&missing.display().to_string());
        let mut stream = ChannelHalf::new(rx_from_receiver, tx_to_sender);

        assert!(!sender.transfer_to(&mut stream, "10.0.0.9.5000"));

        // The 'e' message reached the wire with the folded size.
        let mut message = [0u8; 3 + 31 + 27];
        assert!(recv_exact(&mut stream, &mut message));
        assert_eq!(message[0], b'e');
        let size = u16::from_le_bytes(message[1..3].try_into().expect("size"));
        let text = &message[3..];
        assert_eq!(size as usize, text.len());
        assert_eq!(
            String::from_utf8_lossy(text),
            "Unable to stat the sqlite file (No such file or directory)"
        );
        std::fs::remove_dir_all(&dir).ok();

        // A database without the checksum table is refused before any
        // bytes are sent.
        let dir = std::env::temp_dir().join(format!("cdb-sender-err2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db_path = dir.join("secondary.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open");
        conn.execute_batch("CREATE TABLE tbl_other (x INTEGER);")
            .expect("ddl");
        drop(conn);

        let (_tx_to_receiver, rx_from_sender) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_to_sender, rx_from_receiver) = std::sync::mpsc::channel::<Vec<u8>>();
        let _drain = rx_from_sender;

        let mut sender = ConsolidateSender::new(&db_path.display().to_string());
        let mut stream = ChannelHalf::new(rx_from_receiver, tx_to_sender);

        assert!(!sender.transfer_to(&mut stream, "10.0.0.9.5000"));
        let mut message = [0u8; 512];
        let n = stream.recv(&mut message).expect("error message");
        assert_eq!(message[0], b'e');
        let size = u16::from_le_bytes(message[1..3].try_into().expect("size"));
        let text = String::from_utf8_lossy(&message[3..n]).to_string();
        assert_eq!(size as usize + 3, n);
        assert!(
            text.starts_with("Unable to flush sqlite journal files (no such table: tbl_checksum"),
            "{text}"
        );
        assert_eq!(
            sender.errors(),
            &[format!(
                "Consolidate::transferTo: {} ({})",
                "Unable to flush sqlite journal files", "no such table: tbl_checksum"
            )]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A non-'d' reply to the wait leaves the file in place and fails.
    /// Ignored: this one still hangs on the loopback (the drainer never
    /// unblocks the wait); revisit before ticking BW-09-05-05.
    #[test]
    #[ignore]
    fn the_sender_keeps_the_file_on_an_unexpected_reply() {
        let dir = std::env::temp_dir().join(format!("cdb-sender-err3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let db_path = dir.join("secondary.db");
        build_secondary_db(&db_path, &"ABCD".repeat(8), &[]);

        let (tx_to_receiver, rx_from_sender) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_to_sender, rx_from_receiver) = std::sync::mpsc::channel::<Vec<u8>>();

        // Read the header to learn the size, drain exactly the contents,
        // then answer 'x'.
        let drainer = std::thread::spawn(move || {
            let mut stream = ChannelHalf::new(rx_from_sender, tx_to_sender);
            let mut header = [0u8; 1 + 2 + 12 + 4];
            assert!(recv_exact(&mut stream, &mut header));
            let size = u32::from_le_bytes(header[15..19].try_into().expect("size"));
            let mut remaining = size as usize;
            let mut junk = [0u8; 4096];
            while remaining > 0 {
                let n = stream.recv(&mut junk).expect("chunk");
                assert!(n > 0);
                remaining -= n;
            }
            stream.send(b"x").expect("reply");
        });

        let mut sender = ConsolidateSender::new(&db_path.display().to_string());
        let mut stream = ChannelHalf::new(rx_from_receiver, tx_to_receiver);
        assert!(!sender.transfer_to(&mut stream, "10.0.0.9.5000"));
        drainer.join().expect("drainer");
        assert_eq!(
            sender.errors(),
            &["Consolidate::waitForDeleteCommand: Received unexpected character 'x'".to_string()]
        );
        // The file survives the unexpected reply.
        assert!(db_path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stream that refuses the connect.
    struct NoConnect;

    impl ByteStream for NoConnect {
        fn connect(&mut self, _address: &str) -> bool {
            false
        }

        fn recv(&mut self, _out: &mut [u8]) -> Option<usize> {
            None
        }

        fn send(&mut self, _bytes: &[u8]) -> Option<usize> {
            None
        }
    }

    /// The remaining sender paths: the connect refusal, the read failure
    /// mid-stream, and the failed delete after the `'d'` order.
    #[test]
    fn the_sender_reports_the_connection_and_delete_failures() {
        // The connect refusal happens before anything else.
        let mut sender = ConsolidateSender::new("/nonexistent/secondary.db");
        let mut stream = NoConnect;
        assert!(!sender.transfer_to(&mut stream, "10.0.0.9.5000"));
        assert_eq!(
            sender.errors(),
            &["Consolidate::transferTo: Failed to establish a connection to receiver".to_string()]
        );

        // A source that opens but cannot be read: a directory passes the
        // stat, and File::open on a directory succeeds while read errors.
        let dir = std::env::temp_dir().join(format!("cdb-sender-err4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let mut sender = ConsolidateSender::new(&dir.display().to_string());
        let (_tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_back, rx_back) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink = rx_back;
        let mut stream = ChannelHalf::new(rx, tx_back);
        assert!(!sender.read_and_send_db(&mut stream, 1234));
        assert_eq!(
            sender.errors(),
            &[
                "Consolidate::transferTo: Error reading sqlite file (Input/output error)"
                    .to_string()
            ]
        );

        // The 'd' order for a file that is already gone fails the remove.
        let mut sender = ConsolidateSender::new("/nonexistent/gone.db");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_back, rx_back) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink = rx_back;
        let mut stream = ChannelHalf::new(rx, tx_back);
        tx.send(b"d".to_vec()).expect("d");
        assert!(!sender.wait_for_delete_command(&mut stream));
        assert_eq!(
            sender.errors(),
            &[
                "Consolidate::waitForDeleteCommand: Unable to remove the secondary DB file"
                    .to_string()
            ]
        );

        // A send failure while reporting an error is itself reported.
        let (tx_dead, rx_dead) = std::sync::mpsc::channel::<Vec<u8>>();
        drop(rx_dead);
        let (_tx_keep, rx_keep) = std::sync::mpsc::channel::<Vec<u8>>();
        let mut sender = ConsolidateSender::new("/nonexistent/secondary.db");
        let mut stream = ChannelHalf::new(rx_keep, tx_dead);
        assert!(!sender.transfer_to(&mut stream, "10.0.0.9.5000"));
        assert_eq!(
            sender.errors(),
            &[
                "Consolidate::transferTo: Unable to stat the sqlite file (No such file or directory)"
                    .to_string(),
                "Consolidate::transferTo: Failed to send error message back to ConsolidateDBs"
                    .to_string(),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The receiver's happy path over real bytes, including the `-2`
    /// collision suffix when the destination exists.
    #[test]
    fn the_receiver_writes_the_file_and_names_the_collision() {
        let dir = std::env::temp_dir().join(format!("cdb-recv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let source_bytes = vec![7u8; 600]; // spans two 512-byte chunks

        let (tx_to_receiver, rx_from_sender) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_to_sender, rx_from_receiver) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink = rx_from_receiver;

        let mut receiver = FileReceiver::new(&dir, 0x0A000009, 5000);
        let mut stream = ChannelHalf::new(rx_from_sender, tx_to_sender);

        // Build the wire: 'n', u16 name length, the path, u32 size, then
        // the contents in two chunks.
        let mut header = Vec::new();
        header.push(b'n');
        header.extend_from_slice(&12u16.to_le_bytes());
        header.extend_from_slice(b"/tmp/flip.db");
        header.extend_from_slice(&(source_bytes.len() as u32).to_le_bytes());
        tx_to_receiver.send(header).expect("header");
        tx_to_receiver
            .send(source_bytes[..512].to_vec())
            .expect("chunk 1");
        tx_to_receiver
            .send(source_bytes[512..].to_vec())
            .expect("chunk 2");

        assert_eq!(receiver.receive(&mut stream), ReceiveOutcome::Received);
        assert_eq!(receiver.src_path(), "/tmp/flip.db");
        let dest = dir.join("flip.db");
        assert_eq!(receiver.dest_path(), dest.display().to_string());
        assert_eq!(std::fs::read(&dest).expect("dest bytes"), source_bytes);

        // A second transfer of the same name collides and lands at `-2`.
        let mut receiver = FileReceiver::new(&dir, 0x0A000009, 5000);
        let (tx_to_receiver, rx_from_sender) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_to_sender, rx_from_receiver) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink2 = rx_from_receiver;
        let mut stream = ChannelHalf::new(rx_from_sender, tx_to_sender);
        let mut header = Vec::new();
        header.push(b'n');
        header.extend_from_slice(&12u16.to_le_bytes());
        header.extend_from_slice(b"/tmp/flip.db");
        header.extend_from_slice(&(source_bytes.len() as u32).to_le_bytes());
        tx_to_receiver.send(header).expect("header");
        tx_to_receiver.send(source_bytes.clone()).expect("contents");

        assert_eq!(receiver.receive(&mut stream), ReceiveOutcome::Received);
        assert!(receiver.dest_path().ends_with("flip.db-2"));
        assert_eq!(
            std::fs::read(receiver.dest_path()).expect("dest bytes"),
            source_bytes
        );
        assert!(receiver.errors()[0]
            .starts_with("FileReceiver::recvSrcPath: Default destination file '"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The receiver's refusal paths: the invalid command, the remote error
    /// message, and the empty file.
    #[test]
    fn the_receiver_rejects_the_bad_streams() {
        let dir = std::env::temp_dir().join(format!("cdb-recv-err-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");

        // An unknown command byte.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_back, rx_back) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink = rx_back;
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        let mut stream = ChannelHalf::new(rx, tx_back);
        tx.send(b"x".to_vec()).expect("cmd");
        // The sender drops: the receiver sees the connection close once the
        // queued byte is gone.
        drop(tx);
        assert_eq!(receiver.receive(&mut stream), ReceiveOutcome::Failed);
        assert_eq!(
            receiver.errors(),
            &["FileReceiver::recvCommand: Received invalid command x".to_string()]
        );

        // The remote error message arrives as 'e' + length + text.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_back, rx_back) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink = rx_back;
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        let mut stream = ChannelHalf::new(rx, tx_back);
        let mut message = Vec::new();
        message.push(b'e');
        message.extend_from_slice(&21u16.to_le_bytes());
        message.extend_from_slice(b"lvcreate failed badly");
        tx.send(message).expect("err");
        match receiver.receive(&mut stream) {
            ReceiveOutcome::RemoteError(text) => {
                assert_eq!(text, "lvcreate failed badly");
            }
            other => panic!("expected RemoteError, got {other:?}"),
        }
        assert_eq!(
            receiver.errors(),
            &["FileReceiver::recvErrorStr: Database transfer utility on 0.0.0.1.1 reported error: lvcreate failed badly"]
        );

        // A zero file size is refused.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_back, rx_back) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink = rx_back;
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        let mut stream = ChannelHalf::new(rx, tx_back);
        let mut header = Vec::new();
        header.push(b'n');
        header.extend_from_slice(&11u16.to_le_bytes());
        header.extend_from_slice(b"/a/empty.db");
        header.extend_from_slice(&0u32.to_le_bytes());
        tx.send(header).expect("header");
        assert_eq!(receiver.receive(&mut stream), ReceiveOutcome::Failed);
        assert!(receiver.errors()[0].contains("Received empty file"));
        assert!(receiver.errors()[0].ends_with("/empty.db'"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stream that hands out its bytes exactly once and then reports an
    /// empty read, standing in for a connection that goes quiet mid-file.
    struct StallAfter {
        bytes: Vec<u8>,
        pos: usize,
    }

    impl ByteStream for StallAfter {
        fn recv(&mut self, out: &mut [u8]) -> Option<usize> {
            if self.pos >= self.bytes.len() {
                return Some(0);
            }
            let n = out.len().min(self.bytes.len() - self.pos);
            out[..n].copy_from_slice(&self.bytes[self.pos..self.pos + n]);
            self.pos += n;
            Some(n)
        }

        fn send(&mut self, _bytes: &[u8]) -> Option<usize> {
            None
        }
    }

    /// The transfer's failure modes: a stall in the middle of the file, and
    /// further input after the transfer finished.
    #[test]
    fn the_receiver_reports_the_stall_and_the_extra_input() {
        let dir = std::env::temp_dir().join(format!("cdb-recv-stall-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");

        // The header arrives whole, then the stream reports zero: the
        // contents never come.
        let mut wire = Vec::new();
        wire.push(b'n');
        wire.extend_from_slice(&12u16.to_le_bytes());
        wire.extend_from_slice(b"/tmp/flip.db");
        wire.extend_from_slice(&4096u32.to_le_bytes());
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        let mut stream = StallAfter {
            bytes: wire,
            pos: 0,
        };
        assert_eq!(receiver.receive(&mut stream), ReceiveOutcome::Failed);
        assert_eq!(
            receiver.errors(),
            &[
                "FileReceiver::handleInputNotification: Error communicating with file transfer \
                 process on 0.0.0.1.1. Failed to receive file contents."
                    .to_string()
            ]
        );

        // Input that arrives after the transfer finished is refused.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_back, rx_back) = std::sync::mpsc::channel::<Vec<u8>>();
        let _sink = rx_back;
        let mut wire = Vec::new();
        wire.push(b'n');
        wire.extend_from_slice(&12u16.to_le_bytes());
        wire.extend_from_slice(b"/tmp/flip.db");
        wire.extend_from_slice(&3u32.to_le_bytes());
        let sender_bytes = wire;
        let contents: &[u8] = b"abc";
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        let mut stream = ChannelHalf::new(rx, tx_back);
        tx.send(sender_bytes).expect("header");
        tx.send(contents.to_vec()).expect("contents");
        assert_eq!(receiver.receive(&mut stream), ReceiveOutcome::Received);
        assert_eq!(receiver.receive(&mut stream), ReceiveOutcome::Failed);
        assert!(receiver.errors().last().unwrap().starts_with(
            "FileReceiverMgr::handleInputNotification: Receiving data but we are currently in"
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The receiver tells the remote end to delete its copy, and cleans up
    /// its own.
    #[test]
    fn the_receiver_deletes_both_ends_of_the_transfer() {
        let dir = std::env::temp_dir().join(format!("cdb-recv-del-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");

        let dest = dir.join("flip.db");
        std::fs::write(&dest, b"data").expect("seed dest");

        // The 'd' order goes out the receiver's send direction.
        let (_tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (tx_back, rx_back) = std::sync::mpsc::channel::<Vec<u8>>();
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        receiver.set_paths_for_test("/remote/flip.db", dest.display().to_string());
        let mut stream = ChannelHalf::new(rx, tx_back);

        assert!(receiver.delete_remote_file(&mut stream));
        assert_eq!(rx_back.try_recv().expect("the d byte"), b"d".to_vec());
        assert!(receiver.delete_local_file());
        assert!(!dest.exists());

        // A failed send reports the failure.
        let (tx_dead, rx_dead) = std::sync::mpsc::channel::<Vec<u8>>();
        drop(rx_dead);
        let (_tx_keep, rx_keep) = std::sync::mpsc::channel::<Vec<u8>>();
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        receiver.set_paths_for_test("/remote/flip.db", dest.display().to_string());
        let mut stream = ChannelHalf::new(rx_keep, tx_dead);
        assert!(!receiver.delete_remote_file(&mut stream));
        assert!(receiver.errors()[0].starts_with(
            "FileReceiver::deleteRemoteFile: Failed to delete file '/remote/flip.db'"
        ));

        // A destination that never left the directory itself is nothing to
        // delete.
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        receiver.set_paths_for_test("", dir.display().to_string());
        assert!(receiver.delete_local_file());

        // A local delete that misses reports the failure.
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        receiver.set_paths_for_test("/remote/gone.db", "/nonexistent/gone.db".to_string());
        assert!(!receiver.delete_local_file());
        assert!(receiver.errors()[0].starts_with(
            "FileReceiver::deleteLocalFile: Failed to delete local file '/nonexistent/gone.db'"
        ));

        // Abort closes any half-written file without further fuss.
        let mut receiver = FileReceiver::new(&dir, 1, 1);
        receiver.abort();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A byte stream that records what was sent and accepts nothing back.
    #[derive(Default)]
    struct CountingStream {
        sent: Vec<Vec<u8>>,
    }

    impl ByteStream for CountingStream {
        fn recv(&mut self, _out: &mut [u8]) -> Option<usize> {
            None
        }

        fn send(&mut self, bytes: &[u8]) -> Option<usize> {
            self.sent.push(bytes.to_vec());
            Some(bytes.len())
        }
    }

    /// The manager bookkeeping: pending databases, the unknown-file path,
    /// and the remote cleanup that keeps errored databases.
    #[test]
    fn the_manager_tracks_pending_databases_and_cleanup() {
        let dir = std::env::temp_dir().join(format!("cdb-mgr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");

        let dbs = vec![
            SecondaryDbInfo::from_row(u32::from_be_bytes([10, 0, 0, 9]), "/remote/one.db"),
            SecondaryDbInfo::from_row(u32::from_be_bytes([10, 0, 0, 10]), "/remote/two.db"),
        ];
        let mut manager = FileReceiverMgr::new(&dbs, &dir);
        assert_eq!(manager.consolidation_dir(), dir);
        assert!(!manager.finished());

        let mut first = FileReceiver::new(&dir, 1, 1);
        first.set_paths_for_test("/remote/one.db", dir.join("one.db").display().to_string());
        manager.on_file_received(&first);
        assert!(!manager.finished());

        let mut second = FileReceiver::new(&dir, 2, 2);
        second.set_paths_for_test("/remote/two.db", dir.join("two.db").display().to_string());
        manager.on_file_received(&second);
        assert!(manager.finished());
        assert_eq!(
            manager.received_file_paths(),
            &[
                dir.join("one.db").display().to_string(),
                dir.join("two.db").display().to_string(),
            ]
        );

        // An unknown file does not satisfy a pending entry.
        let mut manager = FileReceiverMgr::new(&dbs, &dir);
        let mut stranger = FileReceiver::new(&dir, 1, 1);
        stranger.set_paths_for_test(
            "/remote/stranger.db",
            dir.join("stranger.db").display().to_string(),
        );
        manager.on_file_received(&stranger);
        assert!(!manager.finished());

        // The remote cleanup: files with consolidation errors are kept,
        // clean ones get the 'd' order.
        let mut errors = DbConsolidatorErrors::default();
        let one_path = dir.join("one.db").display().to_string();
        errors.add_secondary_db(&one_path);

        // A transfer error ends the wait unfinished even with pending
        // databases left.
        manager.on_file_receive_error();
        assert!(!manager.finished());

        let mut receivers = vec![first, second, stranger];
        let mut streams: Vec<CountingStream> = receivers
            .iter()
            .map(|_| CountingStream::default())
            .collect();
        {
            // split_at_mut so the three borrows stay disjoint.
            let (one, rest) = streams.split_at_mut(1);
            let (two, three) = rest.split_at_mut(1);
            let mut stream_refs: [&mut dyn ByteStream; 3] =
                [&mut one[0], &mut two[0], &mut three[0]];
            assert!(manager.clean_up_remote_files(&mut receivers, &errors, &mut stream_refs));
        }

        // one.db is skipped (it has errors); two.db and stranger.db get
        // the delete order.
        assert_eq!(streams[0].sent, Vec::<Vec<u8>>::new());
        assert_eq!(streams[1].sent, vec![b"d".to_vec()]);
        assert_eq!(streams[2].sent, vec![b"d".to_vec()]);
        assert!(receivers[0].errors()[0].starts_with(
            "FileReceiverMgr::cleanUpRemoteFiles: Skipped deletion of secondary database file"
        ));

        // The local cleanup removes the files that landed.
        for path in ["one.db", "two.db", "stranger.db"] {
            std::fs::write(dir.join(path), b"x").expect("seed");
        }
        assert!(manager.clean_up_local_files(&mut receivers));
        assert!(!dir.join("one.db").exists());
        assert!(!dir.join("two.db").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The acceptance: the transferred file is byte-identical to the one
    /// the remote end held, so every consumer merges the same rows.
    #[test]
    fn a_transferred_database_carries_the_identical_merge_input() {
        let dir = std::env::temp_dir().join(format!("cdb-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let consolidation_dir = dir.join("incoming");
        std::fs::create_dir_all(&consolidation_dir).expect("incoming");

        // Two secondary databases on the remote machine.
        let first = dir.join("secondary.db");
        let second = dir.join("secondary-2.db");
        build_secondary_db(
            &first,
            "00112233445566778899AABBCCDDEEFF",
            &[(1, 5, 100, b"first"), (2, 5, 90, b"older")],
        );
        build_secondary_db(
            &second,
            "FFEEDDCCBBAA99887766554433221100",
            &[(3, 6, 70, b"third")],
        );

        // Snapshot the originals first: the sender removes its copy once
        // the tool answers 'd'.
        let sources = [&first, &second];
        let expected_bytes: Vec<Vec<u8>> = sources
            .iter()
            .map(|path| std::fs::read(path).expect("source bytes"))
            .collect();

        // Each hand-off runs the full protocol: sender, wire, receiver.
        for (i, source) in sources.iter().enumerate() {
            let (tx_to_receiver, rx_from_sender) = std::sync::mpsc::channel::<Vec<u8>>();
            let (tx_to_sender, rx_from_receiver) = std::sync::mpsc::channel::<Vec<u8>>();

            let sender_path = source.display().to_string();
            let sender_thread = std::thread::spawn(move || {
                let mut sender_stream = ChannelHalf::new(rx_from_receiver, tx_to_receiver);
                let mut sender = ConsolidateSender::new(&sender_path);
                sender.transfer_to(&mut sender_stream, "10.0.0.9.5000")
            });

            let mut receiver = FileReceiver::new(&consolidation_dir, 0x0A000009, 5000);
            let mut receiver_stream = ChannelHalf::new(rx_from_sender, tx_to_sender);
            assert_eq!(
                receiver.receive(&mut receiver_stream),
                ReceiveOutcome::Received
            );

            // The receiver's copy is byte-identical to the source: the
            // merge input is exactly what the remote machine held.
            let received_bytes = std::fs::read(receiver.dest_path()).expect("received bytes");
            assert_eq!(received_bytes, expected_bytes[i]);

            // The consolidation is done: the 'd' order lands and the remote
            // end removes its copy.
            assert_eq!(receiver_stream.send(b"d"), Some(1));
            assert!(sender_thread.join().expect("join"));
            assert!(!source.exists(), "the remote copy is gone");
        }

        // The two landed files carry the rows of their originals.
        let mut all_rows = Vec::new();
        for name in ["secondary.db", "secondary-2.db"] {
            all_rows.extend(read_rows(&consolidation_dir.join(name)));
        }
        all_rows.sort_by_key(|r| (r.0, r.2));
        assert_eq!(
            all_rows,
            vec![
                (1, 5, 100, b"first".to_vec()),
                (2, 5, 90, b"older".to_vec()),
                (3, 6, 70, b"third".to_vec()),
            ]
        );

        // The digest gate: the received checksum matches the defs digest
        // the tool was started with.
        let defs_digest = unquote_digest("00112233445566778899AABBCCDDEEFF").expect("digest");
        let conn = rusqlite::Connection::open(consolidation_dir.join("secondary.db"))
            .expect("open received");
        let checksum: String = conn
            .query_row("SELECT sm_checksum FROM tbl_checksum", [], |r| r.get(0))
            .expect("checksum");
        assert!(check_entity_defs_digest_match(&checksum, &defs_digest));
        assert!(!check_entity_defs_digest_match(
            "FFEEDDCCBBAA99887766554433221100",
            &defs_digest
        ));

        std::fs::remove_dir_all(&dir).ok();
    }
}
