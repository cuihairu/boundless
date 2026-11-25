//! The `transfer_db` tool (`server/tools/transfer_db/`): the sender half of
//! the secondary-database consolidation pipeline.
//!
//! Three commands (`main.cpp:20-24`):
//! - `consolidate <sqlite_file> <ip:port>` streams a secondary SQLite
//!   database over TCP to a waiting ConsolidateDBs process
//!   (`consolidate.cpp` — the `'n'`/`'d'` protocol).
//! - `snapshotprimary <dest IP> <dest path> <limit kbps>` drives
//!   [`crate::snapshot_helper`] to image the MySQL volume and rsync it back
//!   (`snapshot.cpp` `transferPrimary`).
//! - `snapshotsecondary <sqlite_file> <dest IP> <dest path> <limit kbps>`
//!   backs a SQLite database up with the sqlite backup API and rsyncs the
//!   copy back (`snapshot.cpp` `transferSecondary`).
//!
//! The child processes (snapshot_helper, rsync) run behind [`ChildRuns`]
//! and the TCP link behind [`ConsolidateLink`] — the production wiring is
//! the caller's binary, the same seam as everywhere else in this crate.
//! Only the ERROR-level diagnostics are collected in the output sink; the
//! INFO/NOTICE chatter goes to the logger and is not part of the contract.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::snapshot_helper::{FAILURE, SUCCESS, parse_snapshot_path};

/// `RSYNC` (`snapshot.cpp:27`).
pub const RSYNC: &str = "/usr/bin/rsync";

/// `APP_NAME` (`transfer_db.cpp:33`).
pub const APP_NAME: &str = "TransferDB";

// ===========================================================================
// main.cpp — the argument dispatch and the exit codes
// ===========================================================================

/// The three command faces `main` dispatches to. The booleans are what the
/// exit codes are made of: true → [`SUCCESS`], false → [`FAILURE`].
pub trait TransferCommands {
    /// `consolidate <sqlite_file> <receiving IP>:<receiving port>`.
    fn consolidate(&mut self, secondary_db: &str, send_to_addr: &str) -> bool;
    /// `snapshotprimary <dest IP> <dest path> <limit kbps>`.
    fn snapshot_primary(&mut self, dest_ip: &str, dest_path: &str, limit_kbps: &str) -> bool;
    /// `snapshotsecondary <sqlite_file> <dest IP> <dest path> <limit kbps>`.
    fn snapshot_secondary(
        &mut self,
        secondary_db: &str,
        dest_ip: &str,
        dest_path: &str,
        limit_kbps: &str,
    ) -> bool;
}

/// `BIGWORLD_MAIN` (`main.cpp:27-124`): walk the arguments, dispatch the
/// first command found, return its exit code. `--res`/`-r` swallow the
/// argument after them — unless they are the last argument, in which case
/// they fall through to command matching like any other word. An argument
/// list that names no command at all exits successfully.
pub fn run(args: &[String], commands: &mut dyn TransferCommands) -> i32 {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if (arg == "--res" || arg == "-r") && i < args.len() - 1 {
            i += 2;
            continue;
        }
        if arg == "consolidate" {
            if args.len() - (i + 1) < 2 {
                return FAILURE;
            }
            return code(commands.consolidate(&args[i + 1], &args[i + 2]));
        } else if arg == "snapshotprimary" {
            if args.len() - (i + 1) < 3 {
                return FAILURE;
            }
            return code(commands.snapshot_primary(&args[i + 1], &args[i + 2], &args[i + 3]));
        } else if arg == "snapshotsecondary" {
            if args.len() - (i + 1) < 4 {
                return FAILURE;
            }
            return code(commands.snapshot_secondary(
                &args[i + 1],
                &args[i + 2],
                &args[i + 3],
                &args[i + 4],
            ));
        }
        i += 1;
    }
    SUCCESS
}

fn code(success: bool) -> i32 {
    if success { SUCCESS } else { FAILURE }
}

// ===========================================================================
// TransferDB::consolidate — the <ip:port> parse
// ===========================================================================

/// Why `<ip:port>` refused to parse (`transfer_db.cpp:126-155`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendToAddressError {
    /// No `:` in the string at all.
    NoColon(String),
    /// The address half is not a numbers-and-dots address.
    BadAddress(String),
}

impl SendToAddressError {
    /// The `ERROR_MSG` text `TransferDB::consolidate` prints for this
    /// failure.
    pub fn message(&self) -> String {
        match self {
            Self::NoColon(address) => format!(
                "TransferDB::consolidate: Address provided appears invalid \
                 '{address}'. Expecting <ip:port>"
            ),
            Self::BadAddress(address) => format!(
                "TransferDB::consolidate: Unable to convert string address \
                 '{address}'."
            ),
        }
    }
}

/// `Endpoint::convertAddress` (`endpoint.cpp:485-499`) restricted to the
/// numbers-and-dots forms of `inet_aton`: one to four decimal parts, where
/// the classic 1–3 part forms fold the tail parts into a wider field.
/// The hostname lookup leg stays with the caller's resolver — the library
/// port does not open sockets for names.
pub fn convert_address(spec: &str) -> Option<u32> {
    let parts: Vec<&str> = spec.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let parse = |part: &str| -> Option<u32> {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u32>().ok()
    };
    match parts.len() {
        4 => {
            let [a, b, c, d] = [
                parse(parts[0])?,
                parse(parts[1])?,
                parse(parts[2])?,
                parse(parts[3])?,
            ];
            if a > 255 || b > 255 || c > 255 || d > 255 {
                return None;
            }
            Some((a << 24) | (b << 16) | (c << 8) | d)
        }
        // inet_aton folds the last part of the short forms into the
        // remaining bits: a.b.c puts c in 16 bits, a.b puts b in 24 bits,
        // and a lone number is the address itself.
        3 => {
            let (a, b) = (parse(parts[0])?, parse(parts[1])?);
            let c = parse(parts[2])?;
            if a > 255 || b > 255 || c > 0xFFFF {
                return None;
            }
            Some((a << 24) | (b << 16) | c)
        }
        2 => {
            let (a, b) = (parse(parts[0])?, parse(parts[1])?);
            if a > 255 || b > 0xFFFFFF {
                return None;
            }
            Some((a << 24) | b)
        }
        // A lone number is the address itself; u32 cannot overflow here.
        1 => {
            let a = parse(parts[0])?;
            Some(a)
        }
        _ => None,
    }
}

/// `TransferDB::consolidate` (`transfer_db.cpp:126-155`): split at the last
/// `:`, convert the address half, and store the port the way the reference
/// does — `ntohs(atoi(portStr))`, i.e. C `atoi` reads a leading int and the
/// `ntohs` puts the bytes into network order. Overflowing or non-numeric
/// ports truncate to their low 16 bits or 0 exactly as the C cast does.
pub fn parse_send_to_address(send_to: &str) -> Result<(u32, u16), SendToAddressError> {
    let Some(token_pos) = send_to.rfind(':') else {
        return Err(SendToAddressError::NoColon(send_to.to_owned()));
    };
    let ip_str = &send_to[..token_pos];
    let port_str = &send_to[token_pos + 1..];
    let Some(ip) = convert_address(ip_str) else {
        return Err(SendToAddressError::BadAddress(ip_str.to_owned()));
    };
    let port = (atoi_prefix(port_str) as u16).swap_bytes();
    Ok((ip, port))
}

/// C `atoi`: skip whitespace, an optional sign, then read digits; anything
/// else (including an empty tail) is 0.
pub fn atoi_prefix(text: &str) -> i32 {
    let text = text.trim_start();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut value: i64 = 0;
    for ch in digits.chars() {
        let Some(digit) = ch.to_digit(10) else { break };
        value = value * 10 + i64::from(digit);
        if value > i32::MAX as i64 {
            value = i32::MAX as i64;
        }
    }
    if negative {
        -(value as i32)
    } else {
        value as i32
    }
}

// ===========================================================================
// consolidate.cpp — the wire protocol sender
// ===========================================================================
//
// TransferDB protocol (`consolidate.cpp:15-38`):
//
//   1 byte  - message type: 'e' error, 'n' sqlite file transfer, and the
//             'd' delete command that travels the other way.
//   'e'     - u16 payload size, then that many bytes of message
//             (`<error> (<supplemental>)` when there is a supplement).
//   'n'     - u16 filename size, filename, u32 file size, file contents.
//   'd'     - no further data; the sender unlinks the file on receipt.
//
// The u16/u32 integers stream in host order (`MemoryOStream`'s `<<` is a
// raw value copy) — little-endian on the machines this ran on; the
// fixtures pin that.

/// The TCP link `Consolidate::transferTo` runs over. A production link is
/// one [`TcpLink`]; tests drive a buffered fake.
pub trait ConsolidateLink {
    /// `Endpoint::connect`; `Err` is the `strerror` text.
    fn connect(&mut self, ip: u32, port: u16) -> Result<(), String>;
    /// `Endpoint::send`: how many bytes the link accepted (short writes are
    /// the sender's problem to drain), `Err` when the send failed.
    fn send(&mut self, bytes: &[u8]) -> Result<usize, String>;
    /// `Endpoint::recv` into `buf`: how many bytes arrived, `Err` on
    /// failure (a clean end-of-stream is 0 bytes).
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String>;
}

/// A TCP [`ConsolidateLink`], the reference's `Endpoint` face.
pub struct TcpLink {
    stream: Option<std::net::TcpStream>,
}

impl TcpLink {
    /// An unconnected link.
    pub fn new() -> Self {
        Self { stream: None }
    }
}

impl Default for TcpLink {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsolidateLink for TcpLink {
    fn connect(&mut self, ip: u32, port: u16) -> Result<(), String> {
        let address = std::net::SocketAddrV4::new(std::net::Ipv4Addr::from(ip), u16::from_be(port));
        let stream = std::net::TcpStream::connect(address).map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        Ok(())
    }

    fn send(&mut self, bytes: &[u8]) -> Result<usize, String> {
        match self.stream.as_mut() {
            Some(stream) => stream.write(bytes).map_err(|e| e.to_string()),
            None => Err("not connected".to_owned()),
        }
    }

    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self.stream.as_mut() {
            Some(stream) => stream.read(buf).map_err(|e| e.to_string()),
            None => Err("not connected".to_owned()),
        }
    }
}

/// The `'e'` message payload (`Consolidate::error`): the size field covers
/// the error string plus — when there is a supplement — the three extra
/// bytes that wrap it in parentheses on the receiving end.
fn error_payload(error: &str, supplemental: &str) -> Vec<u8> {
    let has_supplemental = !supplemental.is_empty();
    let mut string_size = error.len() as u16;
    if has_supplemental {
        string_size = string_size.wrapping_add(supplemental.len() as u16 + 3);
    }
    let mut payload = Vec::with_capacity(3 + string_size as usize);
    payload.push(b'e');
    payload.extend_from_slice(&string_size.to_le_bytes());
    payload.extend_from_slice(error.as_bytes());
    if has_supplemental {
        payload.extend_from_slice(b" (");
        payload.extend_from_slice(supplemental.as_bytes());
        payload.push(b')');
    }
    payload
}

/// Send an `'e'` message; a short send is logged but not fatal (the
/// reference only logs `Failed to send error message` and moves on).
fn send_error(
    link: &mut dyn ConsolidateLink,
    error: &str,
    supplemental: &str,
    out: &mut Vec<String>,
) {
    let payload = error_payload(error, supplemental);
    if link.send(&payload) != Ok(payload.len()) {
        out.push(
            "Consolidate::transferTo: Failed to send error message back to \
             ConsolidateDBs"
                .to_owned(),
        );
    }
}

/// `Consolidate::transferTo` (`consolidate.cpp:157-219`): connect, stat the
/// file, flush its journals, stream it, and wait for the delete command
/// that lets us unlink it. The reference constructs `Consolidate` with the
/// filename and keeps the size from `stat`; the port carries both as
/// arguments.
pub fn transfer_to(
    secondary_db: &str,
    ip: u32,
    port: u16,
    link: &mut dyn ConsolidateLink,
    out: &mut Vec<String>,
) -> bool {
    if let Err(error) = link.connect(ip, port) {
        out.push(format!(
            "Consolidate::transferTo: Failed to establish a connection to \
             receiver: {error}"
        ));
        return false;
    }

    let sqlite_size = match std::fs::metadata(secondary_db) {
        Ok(stat) => stat.len(),
        Err(error) => {
            send_error(
                link,
                "Unable to stat the sqlite file",
                &error.to_string(),
                out,
            );
            return false;
        }
    };

    if !prepare_db_for_send(secondary_db, link, out) {
        return false;
    }
    if !read_and_send_db(secondary_db, sqlite_size, link, out) {
        out.push("Consolidate::transferTo: Transfer interrupted.".to_owned());
        return false;
    }
    wait_for_delete_command(secondary_db, link, out)
}

/// `Consolidate::prepareDBForSend` (`consolidate.cpp:221-257`): open the
/// database and read the checksum table — opening and closing replay/erase
/// any journal files so the bytes we ship are a clean database.
fn prepare_db_for_send(path: &str, link: &mut dyn ConsolidateLink, out: &mut Vec<String>) -> bool {
    let connection = match rusqlite::Connection::open(path) {
        Ok(connection) => connection,
        Err(error) => {
            send_error(
                link,
                "Unable to open sqlite file using sqlite3",
                &error.to_string(),
                out,
            );
            return false;
        }
    };
    match connection.prepare("SELECT * FROM tbl_checksum") {
        Ok(_) => true,
        Err(error) => {
            send_error(
                link,
                "Unable to flush sqlite journal files",
                &error.to_string(),
                out,
            );
            false
        }
    }
}

/// `Consolidate::readAndSendDB` (`consolidate.cpp:259-347`): open the file,
/// announce it (name first, so a multi-file receiver knows what landed),
/// then pump the contents through 4096-byte reads. The header send only
/// fails on a hard error — a short header send sails through, as in the
/// reference.
fn read_and_send_db(
    path: &str,
    sqlite_size: u64,
    link: &mut dyn ConsolidateLink,
    out: &mut Vec<String>,
) -> bool {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            send_error(link, "Unable to open sqlite file", &error.to_string(), out);
            return false;
        }
    };
    pump_db_contents(&mut file, path, sqlite_size, link, out)
}

/// The announce header plus the 4096-byte content pump, generic over the
/// reader so the tests can drive the read legs the reference's `fread`
/// could hit (an EINTR retry, a hard read failure).
fn pump_db_contents<R: std::io::Read>(
    reader: &mut R,
    path: &str,
    sqlite_size: u64,
    link: &mut dyn ConsolidateLink,
    out: &mut Vec<String>,
) -> bool {
    let filename = path.as_bytes();
    let mut header = Vec::with_capacity(7 + filename.len());
    header.push(b'n');
    header.extend_from_slice(&(filename.len() as u16).to_le_bytes());
    header.extend_from_slice(filename);
    header.extend_from_slice(&(sqlite_size as u32).to_le_bytes());
    if link.send(&header).is_err() {
        out.push(
            "Consolidate::readAndSendDB: Unable to send initial file \
             transfer data."
                .to_owned(),
        );
        return false;
    }

    let mut buffer = [0u8; 4096];
    let mut fatal_error = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(amount_read) => {
                let mut sent = 0;
                while sent < amount_read && !fatal_error {
                    match link.send(&buffer[sent..amount_read]) {
                        // A zero-byte send would spin the reference's loop
                        // forever; the port treats it as fatal instead.
                        Ok(0) | Err(_) => fatal_error = true,
                        Ok(last_send) => sent += last_send,
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                send_error(link, "Error reading sqlite file", &error.to_string(), out);
                fatal_error = true;
            }
        }
        if fatal_error {
            break;
        }
    }
    !fatal_error
}

/// `Consolidate::waitForDeleteCommand` (`consolidate.cpp:349-383`): the
/// receiver answers `'d'` once it has written the file; then — and only
/// then — the sender unlinks its copy. An end-of-stream read lands here as
/// a zero byte, the same "unexpected character" the reference sees.
fn wait_for_delete_command(
    path: &str,
    link: &mut dyn ConsolidateLink,
    out: &mut Vec<String>,
) -> bool {
    let mut command = [0u8; 1];
    if link.recv(&mut command).is_err() {
        out.push(
            "Consolidate::waitForDeleteCommand: Failed to receive delete \
             command from ConsolidateDBs."
                .to_owned(),
        );
        return false;
    }
    if command[0] != b'd' {
        out.push(format!(
            "Consolidate::waitForDeleteCommand: Received unexpected \
             character '{}'",
            command[0] as char
        ));
        return false;
    }
    if let Err(error) = std::fs::remove_file(path) {
        out.push(format!(
            "Consolidate::waitForDeleteCommand: Unable to remove the \
             secondary DB file: {error}"
        ));
        return false;
    }
    true
}

/// The `TransferDB::consolidate` entry face (`transfer_db.cpp:115-163`):
/// parse `<ip:port>`, then hand the file to [`transfer_to`].
pub fn consolidate(
    secondary_db: &str,
    send_to_addr: &str,
    link: &mut dyn ConsolidateLink,
    out: &mut Vec<String>,
) -> bool {
    let (ip, port) = match parse_send_to_address(send_to_addr) {
        Ok(address) => address,
        Err(error) => {
            out.push(error.message());
            return false;
        }
    };
    transfer_to(secondary_db, ip, port, link, out)
}

// ===========================================================================
// snapshot.cpp — the rsync face over snapshot_helper
// ===========================================================================

/// The destination triple `Snapshot::init` carries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SnapshotPlan {
    /// Where the snapshot is wanted.
    pub destination_ip: String,
    /// Where on that host it should land.
    pub destination_path: String,
    /// `--bwlimit` for the rsync, in kbps; `0` (or any non-number) sends
    /// unthrottled.
    pub limit_kbps: String,
}

impl SnapshotPlan {
    /// `Snapshot::destination` (`snapshot.cpp:363-370`): the merged form
    /// rsync consumes.
    pub fn destination(&self) -> String {
        format!("{}:{}", self.destination_ip, self.destination_path)
    }
}

/// The child-process face of `snapshot.cpp` (`ChildProcess`): snapshot_helper
/// runs return their stdout when the process exited with
/// `SnapshotHelper::SUCCESS`, and rsync runs return its stderr when it
/// failed. Spawning failures and non-zero exits are the same failure, as
/// `WIFEXITED && WEXITSTATUS == SUCCESS` makes them in the reference.
pub trait ChildRuns {
    /// `snapshot_helper args...` — an empty argument list is the setuid
    /// probe `isSnapshotHelperSetUidRoot` runs. `None` is a spawn failure
    /// or a non-zero exit; both are the same failure to the caller.
    fn run_snapshot_helper(&mut self, args: &[String]) -> Option<String>;
    /// `rsync args...`.
    fn run_rsync(&mut self, args: &[String]) -> Result<(), String>;
}

/// `basename` (`libgen.h`): the last path component, with trailing slashes
/// ignored.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_owned()
}

/// `mkdtemp("/tmp/transferdb-XXXXXX")`: the port stamps the process id and
/// a counter into the name instead of six random characters — the same
/// one-directory-per-call guarantee without pulling a random source in.
fn make_temp_dir() -> std::io::Result<String> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "/tmp/transferdb-{:x}{:03}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    std::fs::create_dir(&name)?;
    Ok(name)
}

/// `Snapshot::transferPrimary` (`snapshot.cpp:472-534`): probe the helper,
/// take the snapshot, rsync it and its completion file back, then release
/// the snapshot. The snapshot is released even when the transfer failed —
/// but only the pair of outcomes together makes the run succeed.
pub fn transfer_primary(
    plan: &SnapshotPlan,
    db_user: &str,
    db_pass: &str,
    runs: &mut dyn ChildRuns,
    out: &mut Vec<String>,
) -> bool {
    if runs.run_snapshot_helper(&[]).is_none() {
        out.push(
            "Snapshot::transferPrimary: snapshot_helper binary appears to \
             be incorrectly installed."
                .to_owned(),
        );
        return false;
    }

    let Some(path_to_snapshot) = take_primary_snapshot(db_user, db_pass, runs, out) else {
        out.push("Snapshot::transferPrimary: Failed to take primary snapshot.".to_owned());
        return false;
    };

    let was_transferred = transfer_snapshot_via_rsync(plan, &path_to_snapshot, runs, out);
    if !was_transferred {
        out.push(
            "Snapshot::transferPrimary: Failed to transfer successful \
             snapshot back to the requesting host."
                .to_owned(),
        );
    }

    let was_snapshot_removed = remove_primary_snapshot(runs, out);
    if !was_snapshot_removed {
        out.push(
            "Snapshot::transferPrimary: Failed to release snapshot of \
             primary database."
                .to_owned(),
        );
    }
    was_transferred && was_snapshot_removed
}

/// `Snapshot::takePrimarySnapshot` (`snapshot.cpp:231-307`): refuse empty
/// credentials before touching the helper, run
/// `snapshot_helper acquire-snapshot <user> <pass>`, and take the path the
/// helper printed from its stdout.
fn take_primary_snapshot(
    db_user: &str,
    db_pass: &str,
    runs: &mut dyn ChildRuns,
    out: &mut Vec<String>,
) -> Option<String> {
    if db_user.is_empty() || db_pass.is_empty() {
        out.push("Snapshot::takePrimarySnapshot: dbapp username / password empty.".to_owned());
        return None;
    }

    let snapshot_output = match runs.run_snapshot_helper(&[
        "acquire-snapshot".to_owned(),
        db_user.to_owned(),
        db_pass.to_owned(),
    ]) {
        Some(output) => output,
        None => {
            out.push(
                "Snapshot::takePrimarySnapshot: snapshot_helper failed to \
                 acquire a snapshot."
                    .to_owned(),
            );
            return None;
        }
    };
    match parse_snapshot_path(&snapshot_output) {
        Some(path) => Some(path),
        None => {
            out.push(
                "Snapshot::takePrimarySnapshot: Unable to find snapshot path \
                 from snapshot_helper output."
                    .to_owned(),
            );
            out.push(snapshot_output);
            None
        }
    }
}

/// `Snapshot::removePrimarySnapshot` (`snapshot.cpp:314-350`).
fn remove_primary_snapshot(runs: &mut dyn ChildRuns, out: &mut Vec<String>) -> bool {
    match runs.run_snapshot_helper(&["release-snapshot".to_owned()]) {
        Some(_) => true,
        None => {
            out.push("Snapshot::removePrimarySnapshot: Failed to release snapshot".to_owned());
            false
        }
    }
}

/// `Snapshot::transferSnapshotViaRsync` (`snapshot.cpp:460-466`): the
/// snapshot first, then the `.complete` marker that tells the far end the
/// stream is finished.
fn transfer_snapshot_via_rsync(
    plan: &SnapshotPlan,
    src_path: &str,
    runs: &mut dyn ChildRuns,
    out: &mut Vec<String>,
) -> bool {
    send_snapshot(plan, src_path, runs, out) && send_completion_file(src_path, plan, runs, out)
}

/// The `Snapshot::sendSnapshot` argument list (`snapshot.cpp:381-397`):
/// archive without specials/devices, the source path, the
/// `<ip>:<path>` destination, and `--bwlimit=<k>` only when the limit
/// parses to a non-zero `atoi`.
fn rsync_args(plan: &SnapshotPlan, src_path: &str) -> Vec<String> {
    let mut args = vec![
        "--archive".to_owned(),
        "--no-specials".to_owned(),
        "--no-devices".to_owned(),
        src_path.to_owned(),
        plan.destination(),
    ];
    if atoi_prefix(&plan.limit_kbps) != 0 {
        args.push(format!("--bwlimit={}", plan.limit_kbps));
    }
    args
}

/// `Snapshot::sendSnapshot` (`snapshot.cpp:373-420`).
fn send_snapshot(
    plan: &SnapshotPlan,
    src_path: &str,
    runs: &mut dyn ChildRuns,
    out: &mut Vec<String>,
) -> bool {
    match runs.run_rsync(&rsync_args(plan, src_path)) {
        Ok(()) => true,
        Err(stderr) => {
            out.push("Snapshot::sendSnapshot: rsync failed to transfer snapshot.".to_owned());
            out.push(format!("rsync stderr:\n{stderr}"));
            false
        }
    }
}

/// `Snapshot::sendCompletionFile` (`snapshot.cpp:426-484`): touch
/// `/tmp/<basename>.complete`, rsync it to the destination, unlink it
/// whatever the rsync did, and answer with the rsync outcome — the rsync
/// failure is deliberately not an early exit so the marker is always
/// cleaned up, while a failed unlink does fail the run, as in the
/// reference.
fn send_completion_file(
    src_path: &str,
    plan: &SnapshotPlan,
    runs: &mut dyn ChildRuns,
    out: &mut Vec<String>,
) -> bool {
    let src_base_path = format!("/tmp/{}.complete", basename(src_path));
    if let Err(error) = std::fs::File::create(&src_base_path) {
        out.push(format!(
            "Snapshot::sendCompletionFile: Unable to create completion file \
             '{src_base_path}': {error}"
        ));
        return false;
    }

    let rsync_ok = match runs.run_rsync(&[src_base_path.clone(), plan.destination()]) {
        Ok(()) => true,
        Err(stderr) => {
            out.push(
                "Snapshot::sendCompletionFile: rsync failed to transfer \
                 snapshot. stderr follows."
                    .to_owned(),
            );
            out.push(stderr);
            false
        }
    };

    if let Err(error) = std::fs::remove_file(&src_base_path) {
        out.push(format!(
            "Snapshot::sendCompletionFile: Unable to unlink completion file \
             '{src_base_path}': {error}"
        ));
        return false;
    }
    rsync_ok
}

/// `Snapshot::transferSecondary` (`snapshot.cpp:592-680`): copy the SQLite
/// database with the sqlite backup API into a fresh temp directory, rsync
/// the copy back with its completion file, then clean up — the temp-file
/// and directory removals log but never fail the run.
pub fn transfer_secondary(
    plan: &SnapshotPlan,
    sqlite_path: &str,
    runs: &mut dyn ChildRuns,
    out: &mut Vec<String>,
) -> bool {
    let sqlite_filename = basename(sqlite_path);
    let temp_dirname = match make_temp_dir() {
        Ok(dir) => dir,
        Err(error) => {
            out.push(format!(
                "Snapshot::transferSecondary: Failed to create temporary \
                 directory: {error}"
            ));
            return false;
        }
    };
    let snapshot_path = format!("{temp_dirname}/{sqlite_filename}");

    let was_copied = backup_sqlite_database(sqlite_path, &snapshot_path, out);
    if !was_copied {
        out.push(format!(
            "Snapshot::transferSecondary: Failed to backup the sqlite db \
             '{sqlite_path}'."
        ));
    }

    let mut was_transferred = false;
    if was_copied {
        was_transferred = transfer_snapshot_via_rsync(plan, &snapshot_path, runs, out);
        if !was_transferred {
            out.push(
                "Snapshot::transferSecondary: Failed to transfer successful \
                 secondary snapshot back to the requesting host."
                    .to_owned(),
            );
        }
    }

    if let Err(error) = std::fs::remove_file(&snapshot_path) {
        out.push(format!(
            "Snapshot::transferSecondary: Unable to remove secondary db \
             snapshot '{snapshot_path}': {error}"
        ));
    }
    if let Err(error) = std::fs::remove_dir(&temp_dirname) {
        out.push(format!(
            "Snapshot::transferSecondary: Unable to remove temporary \
             directory '{temp_dirname}': {error}"
        ));
    }

    was_copied && was_transferred
}

/// `Snapshot::backupSqliteDatabase` (`snapshot.cpp:536-590`): open both
/// ends and run the sqlite backup API — the same "main" to "main" copy the
/// reference drives by hand (`sqlite3_backup_init`/`step(-1)`/`finish`).
fn backup_sqlite_database(src_path: &str, dest_path: &str, out: &mut Vec<String>) -> bool {
    let src_db = match rusqlite::Connection::open(src_path) {
        Ok(db) => db,
        Err(_) => {
            out.push("Snapshot::backupSqliteDatabase: Source database inaccessible".to_owned());
            return false;
        }
    };
    let mut dest_db = match rusqlite::Connection::open(dest_path) {
        Ok(db) => db,
        Err(_) => {
            out.push(
                "Snapshot::backupSqliteDatabase: Destination database \
                 inaccessible"
                    .to_owned(),
            );
            return false;
        }
    };
    let backup = match rusqlite::backup::Backup::new(&src_db, &mut dest_db) {
        Ok(backup) => backup,
        Err(_) => {
            out.push(
                "Snapshot::backupSqliteDatabase: Destination database \
                 inaccessible"
                    .to_owned(),
            );
            return false;
        }
    };
    // `sqlite3_backup_step(pBackup, -1)`: the whole database in one step.
    matches!(backup.step(-1), Ok(rusqlite::backup::StepResult::Done))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    // -----------------------------------------------------------------
    // fixtures
    // -----------------------------------------------------------------

    /// The command ledger `run` dispatches against.
    #[derive(Default)]
    struct Commands {
        calls: Vec<String>,
        consolidate_ok: bool,
        snapshot_primary_ok: bool,
        snapshot_secondary_ok: bool,
    }

    impl TransferCommands for Commands {
        fn consolidate(&mut self, secondary_db: &str, send_to_addr: &str) -> bool {
            self.calls
                .push(format!("consolidate:{secondary_db}:{send_to_addr}"));
            self.consolidate_ok
        }
        fn snapshot_primary(&mut self, dest_ip: &str, dest_path: &str, limit_kbps: &str) -> bool {
            self.calls.push(format!(
                "snapshotprimary:{dest_ip}:{dest_path}:{limit_kbps}"
            ));
            self.snapshot_primary_ok
        }
        fn snapshot_secondary(
            &mut self,
            secondary_db: &str,
            dest_ip: &str,
            dest_path: &str,
            limit_kbps: &str,
        ) -> bool {
            self.calls.push(format!(
                "snapshotsecondary:{secondary_db}:{dest_ip}:{dest_path}:{limit_kbps}"
            ));
            self.snapshot_secondary_ok
        }
    }

    /// The bytes the "receiver" has read, with a scripted delete reply and
    /// optional short/failing sends.
    struct FakeLink {
        received: Vec<u8>,
        partial_send: Option<usize>,
        fail_sends_from: Option<usize>,
        delete_reply: Result<u8, String>,
        /// When set, the `recv` side deletes the sender's file first — the
        /// receiver crashed mid-write and the sender's copy is gone.
        delete_target_before_reply: Option<String>,
    }

    impl FakeLink {
        fn new() -> Self {
            Self {
                received: Vec::new(),
                partial_send: None,
                fail_sends_from: None,
                delete_reply: Ok(b'd'),
                delete_target_before_reply: None,
            }
        }
    }

    impl ConsolidateLink for FakeLink {
        fn connect(&mut self, _ip: u32, _port: u16) -> Result<(), String> {
            Ok(())
        }
        fn send(&mut self, bytes: &[u8]) -> Result<usize, String> {
            if let Some(from) = self.fail_sends_from
                && self.received.len() >= from
            {
                return Err("connection reset".to_owned());
            }
            let accepted = match self.partial_send {
                Some(limit) => bytes.len().min(limit),
                None => bytes.len(),
            };
            self.received.extend_from_slice(&bytes[..accepted]);
            Ok(accepted)
        }
        fn recv(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            let reply = self.delete_reply.clone()?;
            if let Some(target) = self.delete_target_before_reply.take() {
                let _ = std::fs::remove_file(target);
            }
            if !buf.is_empty() {
                buf[0] = reply;
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    /// A reader with scripted `read` outcomes, so the pump's EINTR retry
    /// and hard-read-error legs can be driven the way the reference's
    /// `fread` could hit them.
    struct ScriptedReader {
        steps: std::collections::VecDeque<std::io::Result<usize>>,
    }

    impl std::io::Read for ScriptedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.steps.pop_front() {
                Some(Ok(amount)) => {
                    buf[..amount].fill(0xAB);
                    Ok(amount)
                }
                Some(Err(error)) => Err(error),
                None => Ok(0),
            }
        }
    }

    /// A real SQLite secondary database on disk.
    fn write_sqlite(path: &str, with_checksum: bool) {
        let connection = Connection::open(path).expect("create sqlite");
        connection
            .execute_batch(
                "CREATE TABLE tbl_flip (sm_dbID INTEGER, sm_typeID INTEGER, \
                 sm_time INTEGER, sm_blob BLOB);
                 CREATE TABLE tbl_flop (sm_dbID INTEGER, sm_typeID INTEGER, \
                 sm_time INTEGER, sm_blob BLOB);",
            )
            .unwrap();
        if with_checksum {
            connection
                .execute("CREATE TABLE tbl_checksum (sm_data TEXT)", [])
                .unwrap();
            connection
                .execute(
                    "INSERT INTO tbl_checksum VALUES \
                     ('aabbccddeeff00112233445566778899')",
                    [],
                )
                .unwrap();
        }
        for i in 0..3 {
            connection
                .execute(
                    "INSERT INTO tbl_flip VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![i, 7, 1_000 + i, format!("payload-{i}").into_bytes()],
                )
                .unwrap();
        }
    }

    // -----------------------------------------------------------------
    // run: the exit codes
    // -----------------------------------------------------------------

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn each_command_dispatches_and_returns_its_exit_code() {
        let mut ok = Commands {
            consolidate_ok: true,
            snapshot_primary_ok: true,
            snapshot_secondary_ok: true,
            ..Commands::default()
        };
        assert_eq!(
            run(
                &args(&["consolidate", "a.sqlite", "1.2.3.4:27015"]),
                &mut ok
            ),
            SUCCESS
        );
        assert_eq!(
            ok.calls,
            vec!["consolidate:a.sqlite:1.2.3.4:27015".to_owned()]
        );
        assert_eq!(
            run(
                &args(&["snapshotprimary", "10.0.0.1", "/backup", "512"]),
                &mut ok
            ),
            SUCCESS
        );
        assert_eq!(
            run(
                &args(&["snapshotsecondary", "a.sqlite", "10.0.0.1", "/backup", "0"]),
                &mut ok
            ),
            SUCCESS
        );

        let mut bad = Commands::default();
        assert_eq!(
            run(
                &args(&["consolidate", "a.sqlite", "1.2.3.4:27015"]),
                &mut bad
            ),
            FAILURE
        );
        assert_eq!(
            run(
                &args(&["snapshotprimary", "10.0.0.1", "/backup", "512"]),
                &mut bad
            ),
            FAILURE
        );
        assert_eq!(
            run(
                &args(&["snapshotsecondary", "a.sqlite", "10.0.0.1", "/backup", "0"]),
                &mut bad
            ),
            FAILURE
        );
    }

    #[test]
    fn a_command_with_too_few_arguments_is_refused() {
        let mut commands = Commands::default();
        assert_eq!(
            run(&args(&["consolidate", "a.sqlite"]), &mut commands),
            FAILURE
        );
        assert_eq!(
            run(
                &args(&["snapshotprimary", "10.0.0.1", "/backup"]),
                &mut commands
            ),
            FAILURE
        );
        assert_eq!(
            run(
                &args(&["snapshotsecondary", "a.sqlite", "10.0.0.1", "/backup"]),
                &mut commands
            ),
            FAILURE
        );
        assert!(commands.calls.is_empty(), "nothing runs on bad arity");
    }

    #[test]
    fn res_options_swallow_their_value_but_not_the_last_slot() {
        let mut commands = Commands {
            consolidate_ok: true,
            ..Commands::default()
        };
        assert_eq!(
            run(
                &args(&["--res", "/game/res", "consolidate", "a.sqlite", "1.2.3.4:1"]),
                &mut commands
            ),
            SUCCESS
        );
        assert_eq!(
            commands.calls,
            vec!["consolidate:a.sqlite:1.2.3.4:1".to_owned()]
        );

        // A trailing -r has no value to swallow, so it falls through to
        // command matching — and names no command, so the run succeeds.
        let mut untouched = Commands::default();
        assert_eq!(run(&args(&["-r"]), &mut untouched), SUCCESS);
        assert!(untouched.calls.is_empty());
    }

    #[test]
    fn an_argument_list_without_a_command_exits_successfully() {
        let mut commands = Commands::default();
        assert_eq!(run(&args(&["--help"]), &mut commands), SUCCESS);
        assert_eq!(run(&[], &mut commands), SUCCESS);
        assert!(commands.calls.is_empty());
    }

    // -----------------------------------------------------------------
    // the <ip:port> parse
    // -----------------------------------------------------------------

    #[test]
    fn the_send_to_address_splits_and_converts() {
        assert_eq!(
            parse_send_to_address("192.168.0.7:27015"),
            Ok((0xC0A8_0007, (27015u16).swap_bytes()))
        );
        assert_eq!(parse_send_to_address("1.2.3.4:0"), Ok((0x0102_0304, 0)));
    }

    #[test]
    fn a_colonless_or_unparsable_address_is_refused() {
        assert_eq!(
            parse_send_to_address("no-port-here"),
            Err(SendToAddressError::NoColon("no-port-here".to_owned()))
        );
        assert_eq!(
            parse_send_to_address("not.an.address:27015"),
            Err(SendToAddressError::BadAddress("not.an.address".to_owned()))
        );
        let colonless = parse_send_to_address("no-port-here").unwrap_err().message();
        assert!(colonless.contains("Expecting <ip:port>"));
        let bad = parse_send_to_address("nope:1").unwrap_err().message();
        assert_eq!(
            bad,
            "TransferDB::consolidate: Unable to convert string address 'nope'."
        );
    }

    #[test]
    fn the_port_reads_like_c_atoi() {
        assert_eq!(atoi_prefix("27015abc"), 27015);
        assert_eq!(atoi_prefix("  42"), 42);
        assert_eq!(atoi_prefix("+13"), 13);
        assert_eq!(atoi_prefix("-7"), -7);
        assert_eq!(atoi_prefix("junk"), 0);
        assert_eq!(atoi_prefix(""), 0);
        assert_eq!(
            atoi_prefix("99999999999"),
            i32::MAX,
            "C overflow is UB; the port clamps"
        );
        // A port that overflows 16 bits truncates, as the C cast does.
        assert_eq!(
            parse_send_to_address("1.2.3.4:70000").unwrap().1,
            (70000i32 as u16).swap_bytes()
        );
    }

    #[test]
    fn convert_address_takes_the_numbers_and_dots_forms() {
        assert_eq!(convert_address("255.255.255.255"), Some(0xFFFF_FFFF));
        assert_eq!(convert_address("0.0.0.0"), Some(0));
        assert_eq!(convert_address("1.2.3.4"), Some(0x0102_0304));
        // The short inet_aton forms fold the tail into wider fields.
        assert_eq!(convert_address("127.1"), Some((127 << 24) | 1));
        assert_eq!(convert_address("10.1.2"), Some((10 << 24) | (1 << 16) | 2));
        assert_eq!(convert_address("999.1.1.1"), None);
        assert_eq!(convert_address("a.b.c.d"), None);
        assert_eq!(convert_address("1.2.3.4.5"), None);
        assert_eq!(convert_address(""), None);
    }

    // -----------------------------------------------------------------
    // the consolidate wire protocol
    // -----------------------------------------------------------------

    #[test]
    fn the_receiver_reads_the_announced_file_and_the_file_is_deleted() {
        let path = "/tmp/transferdb-test-flip.sqlite";
        let _ = std::fs::remove_file(path);
        write_sqlite(path, true);
        let original = std::fs::read(path).unwrap();
        let file_size = original.len() as u64;

        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(transfer_to(path, 0x0102_0304, 27015, &mut link, &mut out));
        assert!(out.is_empty());
        assert!(!std::path::Path::new(path).exists(), "the 'd' deleted it");

        // The wire: 'n', the u16 name length, the name, the u32 size —
        // then the whole file, byte for byte.
        let name = path.as_bytes();
        let mut expected = Vec::new();
        expected.push(b'n');
        expected.extend_from_slice(&(name.len() as u16).to_le_bytes());
        expected.extend_from_slice(name);
        expected.extend_from_slice(&(file_size as u32).to_le_bytes());
        expected.extend_from_slice(&original);
        assert_eq!(link.received, expected);
    }

    #[test]
    fn an_error_message_carries_the_supplement_in_its_size() {
        let payload = error_payload("Unable to stat the sqlite file", "boom");
        assert_eq!(payload[0], b'e');
        let size = u16::from_le_bytes([payload[1], payload[2]]) as usize;
        assert_eq!(
            size,
            "Unable to stat the sqlite file".len() + " (boom)".len()
        );
        assert_eq!(&payload[3..], b"Unable to stat the sqlite file (boom)");

        let bare = error_payload("bare", "");
        assert_eq!(u16::from_le_bytes([bare[1], bare[2]]) as usize, 4);
        assert_eq!(&bare[3..], b"bare");
    }

    #[test]
    fn a_missing_file_sends_the_stat_error_and_stops() {
        let path = "/tmp/transferdb-no-such-file.sqlite";
        let _ = std::fs::remove_file(path);
        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert!(link.received.starts_with(b"e"), "the receiver got an 'e'");
        let text = String::from_utf8_lossy(&link.received[3..]);
        assert!(text.starts_with("Unable to stat the sqlite file ("));
        assert!(out.is_empty());
    }

    #[test]
    fn a_undeliverable_error_is_logged_not_fatal() {
        let path = "/tmp/transferdb-undeliverable.sqlite";
        let _ = std::fs::remove_file(path);
        let mut link = FakeLink {
            partial_send: Some(1),
            ..FakeLink::new()
        };
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to send error message")),
            "a short 'e' send is logged: {out:?}"
        );
    }

    #[test]
    fn a_non_database_file_fails_the_journal_flush() {
        let path = "/tmp/transferdb-not-a-db.sqlite";
        std::fs::write(path, b"this is not sqlite at all").unwrap();
        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert_eq!(link.received[0], b'e');
        let text = String::from_utf8_lossy(&link.received[3..]);
        assert!(
            text.starts_with("Unable to flush sqlite journal files ("),
            "got: {text}"
        );
        assert!(
            std::path::Path::new(path).exists(),
            "the failed file stays put"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_database_without_the_checksum_table_is_refused() {
        let path = "/tmp/transferdb-no-checksum.sqlite";
        let _ = std::fs::remove_file(path);
        write_sqlite(path, false);
        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert_eq!(link.received[0], b'e');
        let text = String::from_utf8_lossy(&link.received[3..]);
        assert!(text.starts_with("Unable to flush sqlite journal files ("));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_unexpected_reply_leaves_the_file_in_place() {
        let path = "/tmp/transferdb-keep.sqlite";
        let _ = std::fs::remove_file(path);
        write_sqlite(path, true);
        let mut link = FakeLink {
            delete_reply: Ok(b'x'),
            ..FakeLink::new()
        };
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Received unexpected character 'x'"))
        );
        assert!(std::path::Path::new(path).exists(), "no 'd', no unlink");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_hung_up_receiver_fails_the_delete_wait() {
        let path = "/tmp/transferdb-hangup.sqlite";
        let _ = std::fs::remove_file(path);
        write_sqlite(path, true);
        let mut link = FakeLink {
            delete_reply: Err("connection closed".to_owned()),
            ..FakeLink::new()
        };
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to receive delete command"))
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_lost_file_before_the_delete_command_is_reported() {
        let path = "/tmp/transferdb-vanished.sqlite";
        let _ = std::fs::remove_file(path);
        write_sqlite(path, true);
        let mut link = FakeLink {
            delete_target_before_reply: Some(path.to_owned()),
            ..FakeLink::new()
        };
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Unable to remove the secondary DB file")),
            "got: {out:?}"
        );
    }

    #[test]
    fn short_sends_are_drained_until_everything_is_through() {
        let path = "/tmp/transferdb-partial.sqlite";
        let _ = std::fs::remove_file(path);
        write_sqlite(path, true);
        let original = std::fs::read(path).unwrap();

        let mut link = FakeLink {
            partial_send: Some(7),
            ..FakeLink::new()
        };
        let mut out = Vec::new();
        assert!(transfer_to(path, 1, 2, &mut link, &mut out));

        // The announcement is a single send the reference never drains (it
        // only checks for -1), so a seven-bytes-per-call link keeps just its
        // first seven bytes — but the content pump drains in full.
        let name = path.as_bytes();
        let header_len = 1 + 2 + name.len() + 4;
        assert_eq!(
            link.received.len(),
            7.min(header_len) + original.len(),
            "the one-shot header, then every content byte"
        );
    }

    #[test]
    fn a_mid_stream_send_failure_interrupts_without_an_error_message() {
        let path = "/tmp/transferdb-reset.sqlite";
        let _ = std::fs::remove_file(path);
        write_sqlite(path, true);
        let mut link = FakeLink {
            fail_sends_from: Some(4096),
            ..FakeLink::new()
        };
        let mut out = Vec::new();
        assert!(!transfer_to(path, 1, 2, &mut link, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Transfer interrupted."))
        );
        // The reference sends no 'e' once the content pump has begun.
        assert!(!link.received.starts_with(b"e"));
        assert!(
            std::path::Path::new(path).exists(),
            "no delete command arrived"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_refused_connection_never_touches_the_file() {
        /// Every method records itself, so the test can prove the flow
        /// stopped at `connect` instead of trusting a panic body.
        struct RefusingLink {
            calls: Vec<&'static str>,
        }
        impl ConsolidateLink for RefusingLink {
            fn connect(&mut self, _: u32, _: u16) -> Result<(), String> {
                self.calls.push("connect");
                Err("Connection refused".to_owned())
            }
            fn send(&mut self, _: &[u8]) -> Result<usize, String> {
                self.calls.push("send");
                Ok(0)
            }
            fn recv(&mut self, _: &mut [u8]) -> Result<usize, String> {
                self.calls.push("recv");
                Ok(0)
            }
        }
        let mut link = RefusingLink { calls: Vec::new() };
        let mut out = Vec::new();
        assert!(!transfer_to(
            "/tmp/whatever.sqlite",
            1,
            2,
            &mut link,
            &mut out
        ));
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to establish a connection"))
        );
        assert_eq!(link.calls, ["connect"]);
        // The remaining trait methods still answer when poked directly —
        // the flow above proved it never reaches them.
        assert_eq!(link.send(b"payload"), Ok(0));
        assert_eq!(link.recv(&mut []), Ok(0));
    }

    #[test]
    fn the_consolidate_entry_refuses_bad_addresses_before_connecting() {
        /// Every method records itself, so the test can prove the
        /// bad-address legs never touch the link.
        struct SilentLink {
            calls: Vec<&'static str>,
        }
        impl ConsolidateLink for SilentLink {
            fn connect(&mut self, _: u32, _: u16) -> Result<(), String> {
                self.calls.push("connect");
                Err("must not connect".to_owned())
            }
            fn send(&mut self, _: &[u8]) -> Result<usize, String> {
                self.calls.push("send");
                Ok(0)
            }
            fn recv(&mut self, _: &mut [u8]) -> Result<usize, String> {
                self.calls.push("recv");
                Ok(0)
            }
        }
        let mut link = SilentLink { calls: Vec::new() };
        let mut out = Vec::new();
        assert!(!consolidate(
            "/tmp/x.sqlite",
            "no-colon",
            &mut link,
            &mut out
        ));
        assert!(out[0].contains("Expecting <ip:port>"));
        assert!(!consolidate(
            "/tmp/x.sqlite",
            "badhost:1",
            &mut link,
            &mut out
        ));
        assert_eq!(
            out[1],
            "TransferDB::consolidate: Unable to convert string address 'badhost'."
        );
        assert!(link.calls.is_empty(), "the link stays untouched");
        // The remaining trait methods still answer when poked directly.
        assert_eq!(link.send(b"payload"), Ok(0));
        assert_eq!(link.recv(&mut []), Ok(0));

        // A well-formed address parses and hands off to `transfer_to`; the
        // refusing `connect` proves the handoff happened.
        assert!(!consolidate(
            "/tmp/x.sqlite",
            "127.0.0.1:9",
            &mut link,
            &mut out
        ));
        assert_eq!(link.calls.last(), Some(&"connect"));
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to establish a connection"))
        );
    }

    #[test]
    fn convert_address_rejects_overflow_and_unknown_shapes() {
        // The folded 16-bit tail of a 3-part short form cannot overflow it.
        assert_eq!(convert_address("1.2.70000"), None);
        // The folded 24-bit tail of a 2-part short form cannot overflow it.
        assert_eq!(convert_address("2.17000000"), None);
        // A lone number is the address itself.
        assert_eq!(convert_address("5"), Some(5));
        // Five parts is not an inet_aton form at all.
        assert_eq!(convert_address("1.2.3.4.5"), None);
    }

    #[test]
    fn an_unopenable_secondary_reports_the_sqlite_failure() {
        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(!prepare_db_for_send(
            "/tmp/transferdb-no-such-dir/x.sqlite",
            &mut link,
            &mut out
        ));
        // The failure travels to the receiver as an 'e' message, not a log
        // line.
        let payload = String::from_utf8_lossy(&link.received);
        assert!(payload.contains("Unable to open sqlite file using sqlite3"));
    }

    #[test]
    fn an_unopenable_secondary_reports_the_file_failure() {
        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(!read_and_send_db(
            "/tmp/transferdb-no-such-dir/x.sqlite",
            0,
            &mut link,
            &mut out
        ));
        // The failure travels to the receiver as an 'e' message.
        let payload = String::from_utf8_lossy(&link.received);
        assert!(payload.contains("Unable to open sqlite file"));
    }

    #[test]
    fn a_failed_header_send_aborts_before_the_content_pump() {
        let path = "/tmp/transferdb-header-fail.sqlite";
        let _ = std::fs::remove_file(path);
        std::fs::write(path, b"some bytes").unwrap();
        let mut link = FakeLink {
            fail_sends_from: Some(0),
            ..FakeLink::new()
        };
        let mut out = Vec::new();
        assert!(!read_and_send_db(path, 9, &mut link, &mut out));
        assert_eq!(
            out.last().map(String::as_str),
            Some("Consolidate::readAndSendDB: Unable to send initial file transfer data.")
        );
        // The header failed, so nothing else went out.
        assert!(link.received.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_interrupted_read_retries_the_same_chunk() {
        let path = "/tmp/transferdb-eintr.sqlite";
        let _ = std::fs::remove_file(path);
        std::fs::write(path, b"irrelevant").unwrap();
        let mut reader = ScriptedReader {
            steps: [
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                Ok(3),
                Ok(0),
            ]
            .into_iter()
            .collect(),
        };
        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(pump_db_contents(&mut reader, path, 3, &mut link, &mut out));
        assert!(out.is_empty());
        assert!(link.received.ends_with(&[0xAB, 0xAB, 0xAB]));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_failed_read_announces_the_error_and_aborts() {
        let path = "/tmp/transferdb-read-fail.sqlite";
        let _ = std::fs::remove_file(path);
        std::fs::write(path, b"irrelevant").unwrap();
        let mut reader = ScriptedReader {
            steps: [Err(std::io::Error::other("disk blew up"))]
                .into_iter()
                .collect(),
        };
        let mut link = FakeLink::new();
        let mut out = Vec::new();
        assert!(!pump_db_contents(&mut reader, path, 0, &mut link, &mut out));
        // The failure travels to the receiver as an 'e' message; the run
        // stops without a delete reply.
        let payload = String::from_utf8_lossy(&link.received);
        assert!(payload.contains("Error reading sqlite file"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_tcp_link_round_trips_a_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        assert_eq!(
            address.ip(),
            std::net::IpAddr::from(std::net::Ipv4Addr::LOCALHOST),
            "the literal bind is IPv4"
        );
        let ip = u32::from(std::net::Ipv4Addr::LOCALHOST);
        let port = address.port().swap_bytes();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 5];
            std::io::Read::read_exact(&mut socket, &mut buffer).unwrap();
            buffer[0] = b'd';
            std::io::Write::write_all(&mut socket, &buffer).unwrap();
        });
        let mut link = TcpLink::new();
        assert!(link.connect(ip, port).is_ok());
        assert_eq!(link.send(b"hello"), Ok(5));
        let mut buffer = [0u8; 1];
        assert_eq!(link.recv(&mut buffer), Ok(1));
        assert_eq!(buffer[0], b'd');
        server.join().unwrap();

        // A cold link fails its sends and reads; `Default` mirrors `new`.
        let mut cold = TcpLink::default();
        assert!(cold.send(b"x").is_err());
        assert!(cold.recv(&mut buffer).is_err());
    }

    #[test]
    fn an_empty_recv_buffer_answers_zero_bytes() {
        let mut link = FakeLink::new();
        assert_eq!(link.recv(&mut []), Ok(0));
    }

    // -----------------------------------------------------------------
    // the snapshot faces
    // -----------------------------------------------------------------

    /// The child-process ledger, with scripted results consumed in order.
    #[derive(Default)]
    struct FakeRuns {
        helper_calls: Vec<Vec<String>>,
        helper_results: Vec<Option<String>>,
        rsync_calls: Vec<Vec<String>>,
        rsync_results: Vec<Result<(), String>>,
        /// The rsync invocations record the source path they would ship.
        sent_paths: Vec<String>,
    }

    impl ChildRuns for FakeRuns {
        fn run_snapshot_helper(&mut self, args: &[String]) -> Option<String> {
            self.helper_calls.push(args.to_vec());
            match self.helper_results.is_empty() {
                true => Some(String::new()),
                false => self.helper_results.remove(0),
            }
        }
        fn run_rsync(&mut self, args: &[String]) -> Result<(), String> {
            if args.len() >= 5 {
                self.sent_paths.push(args[3].clone());
            }
            self.rsync_calls.push(args.to_vec());
            match self.rsync_results.is_empty() {
                true => Ok(()),
                false => self.rsync_results.remove(0),
            }
        }
    }

    fn plan() -> SnapshotPlan {
        SnapshotPlan {
            destination_ip: "10.0.0.1".to_owned(),
            destination_path: "/backup/dir".to_owned(),
            limit_kbps: "512".to_owned(),
        }
    }

    /// The probe, the acquire (printing the snapshot path), the release.
    fn scripted_primary_runs() -> FakeRuns {
        FakeRuns {
            helper_results: vec![
                Some("SnapshotHelper started\n".to_owned()),
                Some("SnapshotHelper started\n/mnt/snap0/mysql-data\n".to_owned()),
                Some(String::new()),
            ],
            ..FakeRuns::default()
        }
    }

    /// Every `transfer_primary` test scripts the same `mysql-data`
    /// snapshot, so their real `/tmp/mysql-data.complete` touch and unlink
    /// windows must not overlap — parallel tests would otherwise unlink
    /// each other's marker out from under the run.
    fn lock_completion_marker() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn the_primary_transfer_probes_snapshots_sends_and_releases() {
        let _completion = lock_completion_marker();
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        let mut out = Vec::new();
        assert!(transfer_primary(
            &runs_plan, "bwuser", "bwpass", &mut runs, &mut out
        ));
        assert!(out.is_empty());

        assert_eq!(runs.helper_calls.len(), 3);
        assert_eq!(
            runs.helper_calls[0],
            Vec::<String>::new(),
            "the setuid probe"
        );
        assert_eq!(
            runs.helper_calls[1],
            vec![
                "acquire-snapshot".to_owned(),
                "bwuser".to_owned(),
                "bwpass".to_owned()
            ]
        );
        assert_eq!(runs.helper_calls[2], vec!["release-snapshot".to_owned()]);

        // The snapshot rsync, then the completion-file rsync.
        assert_eq!(runs.rsync_calls.len(), 2);
        assert_eq!(
            runs.rsync_calls[0],
            vec![
                "--archive".to_owned(),
                "--no-specials".to_owned(),
                "--no-devices".to_owned(),
                "/mnt/snap0/mysql-data".to_owned(),
                "10.0.0.1:/backup/dir".to_owned(),
                "--bwlimit=512".to_owned()
            ]
        );
        assert_eq!(
            runs.rsync_calls[1].first().map(String::as_str),
            Some("/tmp/mysql-data.complete")
        );
        assert!(
            !std::path::Path::new("/tmp/mysql-data.complete").exists(),
            "the completion file is always unlinked"
        );
    }

    #[test]
    fn a_zero_bwlimit_omits_the_throttle_argument() {
        let _completion = lock_completion_marker();
        let mut runs_plan = plan();
        runs_plan.limit_kbps = "0".to_owned();
        let mut runs = scripted_primary_runs();
        let mut out = Vec::new();
        assert!(transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(
            runs.rsync_calls[0]
                .iter()
                .all(|arg| !arg.starts_with("--bwlimit"))
        );
    }

    #[test]
    fn a_failed_probe_refuses_before_any_snapshot_is_taken() {
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        runs.helper_results[0] = None;
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("incorrectly installed"))
        );
        assert_eq!(runs.helper_calls.len(), 1, "nothing runs after the probe");
        assert!(runs.rsync_calls.is_empty());
    }

    #[test]
    fn empty_credentials_refuse_before_the_helper_runs() {
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "", "p", &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("username / password empty"))
        );
        assert_eq!(runs.helper_calls.len(), 1, "only the probe ran");
    }

    #[test]
    fn a_failed_acquire_is_reported_without_a_release() {
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        runs.helper_results[1] = None;
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("failed to acquire a snapshot"))
        );
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to take primary snapshot"))
        );
        assert!(runs.rsync_calls.is_empty());
        assert_eq!(
            runs.helper_calls.len(),
            2,
            "no release after a failed acquire"
        );
    }

    #[test]
    fn helper_stdout_without_a_path_is_reported() {
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        runs.helper_results[1] = Some("SnapshotHelper started\n".to_owned());
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Unable to find snapshot path"))
        );
    }

    #[test]
    fn a_failed_rsync_still_releases_the_snapshot() {
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        runs.rsync_results = vec![Err("rsync died".to_owned()), Ok(())];
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("rsync failed to transfer snapshot"))
        );
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to transfer successful snapshot"))
        );
        assert_eq!(runs.helper_calls.len(), 3, "the release ran anyway");
    }

    #[test]
    fn a_failed_release_fails_an_otherwise_clean_transfer() {
        let _completion = lock_completion_marker();
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        runs.helper_results[2] = None;
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to release snapshot"))
        );
        assert_eq!(
            runs.rsync_calls.len(),
            2,
            "the transfer itself went through"
        );
    }

    #[test]
    fn a_failed_completion_rsync_fails_the_transfer_but_still_unlinks() {
        let _completion = lock_completion_marker();
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        runs.rsync_results = vec![Ok(()), Err("stderr text".to_owned())];
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(out.iter().any(|line| line.contains("stderr follows")));
        assert!(out.iter().any(|line| line.contains("stderr text")));
        assert!(
            !std::path::Path::new("/tmp/mysql-data.complete").exists(),
            "the completion file is unlinked whatever the rsync did"
        );
    }

    #[test]
    fn the_secondary_transfer_backs_up_sends_and_cleans_up() {
        let src = "/tmp/transferdb-secondary-src.sqlite";
        let _ = std::fs::remove_file(src);
        write_sqlite(src, true);

        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        let mut out = Vec::new();
        assert!(transfer_secondary(&runs_plan, src, &mut runs, &mut out));
        assert!(out.is_empty());

        // The first rsync carries the backup copy out of the temp dir.
        let sent = &runs.sent_paths[0];
        assert!(
            sent.starts_with("/tmp/transferdb-"),
            "the copy lives in a transferdb temp dir: {sent}"
        );
        assert!(sent.ends_with("/transferdb-secondary-src.sqlite"));

        // The temp dir and its contents are gone after the run.
        let temp_dir = std::path::Path::new(sent).parent().unwrap();
        assert!(!temp_dir.exists(), "the temp dir was removed");

        // The backup mechanism itself: drive it by hand onto a real file
        // and read the copy back — the acceptance's "target data".
        let proof = "/tmp/transferdb-secondary-proof.sqlite";
        let _ = std::fs::remove_file(proof);
        let src_conn = Connection::open(src).unwrap();
        let mut dst_conn = Connection::open(proof).unwrap();
        {
            let proof_backup = rusqlite::backup::Backup::new(&src_conn, &mut dst_conn).unwrap();
            assert!(matches!(
                proof_backup.step(-1),
                Ok(rusqlite::backup::StepResult::Done)
            ));
        }
        let rows: i64 = dst_conn
            .query_row("SELECT COUNT(*) FROM tbl_flip", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 3, "the backup API carries every row");
        let _ = std::fs::remove_file(proof);
        let _ = std::fs::remove_file(src);
    }

    #[test]
    fn an_inaccessible_source_skips_the_rsync_and_cleans_up() {
        let src = "/tmp/transferdb-no-such-dir/missing.sqlite";
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        let mut out = Vec::new();
        assert!(!transfer_secondary(&runs_plan, src, &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to backup the sqlite db")),
            "got: {out:?}"
        );
        assert!(
            runs.rsync_calls.is_empty(),
            "nothing transfers a failed backup"
        );
    }

    #[test]
    fn an_uncreatable_completion_file_fails_the_run() {
        let runs_plan = plan();
        let mut runs = FakeRuns::default();
        let mut out = Vec::new();
        // A component past NAME_MAX makes the `/tmp/<name>.complete` touch
        // fail before any rsync runs.
        let long_name = "t".repeat(300);
        let src_path = format!("/tmp/ledger/{long_name}");
        assert!(!send_completion_file(
            &src_path, &runs_plan, &mut runs, &mut out
        ));
        assert!(
            out.iter()
                .any(|line| line.contains("Unable to create completion file")),
            "got: {out:?}"
        );
        assert!(runs.rsync_calls.is_empty());
    }

    #[test]
    fn an_unopenable_destination_reports_the_failure() {
        let src = "/tmp/transferdb-src-ok.sqlite";
        let _ = std::fs::remove_file(src);
        write_sqlite(src, false);
        let mut out = Vec::new();
        assert!(!backup_sqlite_database(
            src,
            "/tmp/transferdb-no-such-dir/dest.sqlite",
            &mut out
        ));
        assert!(
            out.iter()
                .any(|line| line.contains("Destination database inaccessible"))
        );
        let _ = std::fs::remove_file(src);
    }

    #[test]
    fn an_unscripted_helper_yields_an_empty_probe_and_no_snapshot() {
        let runs_plan = plan();
        let mut runs = FakeRuns::default();
        let mut out = Vec::new();
        assert!(!transfer_primary(&runs_plan, "u", "p", &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Unable to find snapshot path")),
            "got: {out:?}"
        );
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to take primary snapshot"))
        );
    }

    #[test]
    fn a_failed_secondary_rsync_is_reported() {
        let src = "/tmp/transferdb-secondary-fail.sqlite";
        let _ = std::fs::remove_file(src);
        write_sqlite(src, true);
        let runs_plan = plan();
        let mut runs = scripted_primary_runs();
        runs.rsync_results = vec![Err("rsync died".to_owned())];
        let mut out = Vec::new();
        assert!(!transfer_secondary(&runs_plan, src, &mut runs, &mut out));
        assert!(
            out.iter()
                .any(|line| line.contains("Failed to transfer successful secondary"))
        );
        let _ = std::fs::remove_file(src);
    }

    #[test]
    fn the_destination_is_the_rsync_form() {
        assert_eq!(plan().destination(), "10.0.0.1:/backup/dir");
        assert_eq!(RSYNC, "/usr/bin/rsync");
        assert_eq!(APP_NAME, "TransferDB");
    }

    #[test]
    fn basename_takes_the_last_component() {
        assert_eq!(basename("/mnt/snap0/mysql-data"), "mysql-data");
        assert_eq!(basename("/tmp/transferdb-x/a.sqlite"), "a.sqlite");
        assert_eq!(basename("plain.sqlite"), "plain.sqlite");
    }
}
