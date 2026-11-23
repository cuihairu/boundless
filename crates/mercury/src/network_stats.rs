//! `lib/network/network_stats.cpp`: the `/proc/net/udp` reader behind
//! `Endpoint::getQueueSizes` -- how many bytes the kernel currently holds
//! in a UDP socket's transmit and receive queues.
//!
//! The reference parses `/proc/net/udp` on demand behind a 400ms cache,
//! keyed by the socket's local address, and force-clears the map once it
//! has grown past twice the record count of the last read.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::{Mutex, OnceLock};

use crate::endpoint::Endpoint;

/// `Updater::DEFAULT_UPDATE_INTERVAL` (network_stats.cpp:43), 400ms.
const DEFAULT_UPDATE_INTERVAL_MICROS: u64 = 400_000;
const DEFAULT_UPDATE_INTERVAL_SECONDS: f64 =
    DEFAULT_UPDATE_INTERVAL_MICROS as f64 / 1_000_000.0;

/// `Updater::StatsBlock`: the queued tx/rx byte counts of one socket.
type QueueSizes = (i32, i32);

/// The `Updater` singleton state (network_stats.cpp:14-41). Keyed by the
/// raw `(ip, port)` word pair of `Mercury::Address` -- the salt plays no
/// part in a queue lookup, and the reference builds the key the same way.
struct Updater {
    stats_map: HashMap<(u32, u16), QueueSizes>,
    last_active_record_count: usize,
    last_update_micros: u64,
}

impl Updater {
    fn new() -> Self {
        Self {
            stats_map: HashMap::new(),
            last_active_record_count: 0,
            last_update_micros: 0,
        }
    }

    /// `Updater::getQueueSizes` (network_stats.cpp:72): refresh the table
    /// when the cache is 400ms old, then look the address up. A zero
    /// timestamp means never refreshed: the reference's zero
    /// `lastUpdate_` is always older than the interval because its
    /// `timestamp()` counts since boot, while the monotonic stamp here is
    /// process-relative and starts near zero, so the never-refreshed
    /// state is carried explicitly instead of through the clock math.
    fn get_queue_sizes(&mut self, ip: u32, port: u16) -> Option<QueueSizes> {
        let now = platform::clock::monotonic_now_micros().micros();
        let never_refreshed = self.last_update_micros == 0;
        let stale = never_refreshed
            || now.saturating_sub(self.last_update_micros) as f64 / 1_000_000.0
                >= DEFAULT_UPDATE_INTERVAL_SECONDS;
        if stale {
            self.update();
            self.last_update_micros = now;
        }
        self.stats_map.get(&(ip, port)).copied()
    }

    /// `Updater::update` (network_stats.cpp:101): re-read `/proc/net/udp`.
    fn update(&mut self) -> bool {
        self.update_at("/proc/net/udp")
    }

    /// The open half of `update`, split so tests can drive the failure
    /// arm with a path that does not exist.
    fn update_at(&mut self, path: &str) -> bool {
        match std::fs::File::open(path) {
            Ok(file) => self.update_from(BufReader::new(file)),
            Err(err) => {
                eprintln!("Updater::update: could not open {}: {}", path, err);
                false
            }
        }
    }

    /// The parse loop of `Updater::update`, split so tests can feed text
    /// instead of the kernel file.
    fn update_from<R: BufRead>(&mut self, mut reader: R) -> bool {
        // The map is a reuse buffer that would otherwise only grow: once it
        // holds more than twice the last read's record count, clear it as a
        // forced garbage collection (network_stats.cpp:116-128).
        if self.stats_map.len() > 2 * self.last_active_record_count {
            self.stats_map.clear();
        }
        self.last_active_record_count = 0;

        let mut line = String::new();
        // Skip the header; a short read here just fails the update.
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return false;
        }
        // Use the first data line to get the column offsets
        // (network_stats.cpp:149-154): the ": " anchor ends the
        // sequence-number column, then three single-space advances land on
        // `tx_queue:rx_queue`.
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return false;
        }
        let addr_offset = match line.find(": ") {
            Some(index) => index + 2,
            None => return invalid_format(&line),
        };
        let mut txrx_offset = addr_offset;
        for _ in 0..3 {
            txrx_offset = match line[txrx_offset..].find(' ') {
                Some(index) => txrx_offset + index + 1,
                None => return invalid_format(&line),
            };
        }

        loop {
            // `local_address` prints `%08X:%04X`: the ip hex is the raw
            // in-memory word of `sin_addr` (kept as-is), the port hex is
            // the host-order value (byteswapped back into the
            // network-order field, network_stats.cpp:170-173).
            let addr_field = &line[addr_offset..];
            let (ip, consumed) = parse_hex(addr_field);
            let rest = &addr_field[consumed..];
            let (port_hex, _) = parse_hex(rest.strip_prefix(':').unwrap_or(rest));
            let port = platform::socket::htons(port_hex as u16);

            let txrx_field = &line[txrx_offset..];
            let (tx, consumed) = parse_hex(txrx_field);
            let rest = &txrx_field[consumed..];
            let (rx, _) = parse_hex(rest.strip_prefix(':').unwrap_or(rest));

            self.stats_map.insert((ip as u32, port), (tx as i32, rx as i32));
            self.last_active_record_count += 1;

            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
        }
        true
    }
}

/// `strtoul(text, &endptr, 16)`: the leading hex run, saturating like the
/// C call, zero when there is no run at all.
fn parse_hex(text: &str) -> (u64, usize) {
    let end = text
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(text.len());
    let value = u64::from_str_radix(&text[..end], 16).unwrap_or(u64::MAX);
    (value, end)
}

/// `Updater::update: /proc/net/udp invalid format` (network_stats.cpp:158).
fn invalid_format(line: &str) -> bool {
    eprintln!(
        "Updater::update: /proc/net/udp invalid format: {}",
        line
    );
    false
}

/// `s_updater` (network_stats.cpp:193).
fn updater() -> &'static Mutex<Updater> {
    static UPDATER: OnceLock<Mutex<Updater>> = OnceLock::new();
    UPDATER.get_or_init(|| Mutex::new(Updater::new()))
}

/// `NetworkStats::getQueueSizes` (network_stats.cpp:219): the tx/rx byte
/// counts queued for the endpoint's local address, refreshed at most every
/// 400ms.
pub fn get_queue_sizes(ep: &Endpoint, tx: &mut i32, rx: &mut i32) -> bool {
    let mut port = 0u16;
    let mut ip = 0u32;
    ep.local_address(Some(&mut port), Some(&mut ip));
    match updater().lock().unwrap().get_queue_sizes(ip, port) {
        Some((found_tx, found_rx)) => {
            *tx = found_tx;
            *rx = found_rx;
            true
        }
        None => false,
    }
}

/// Test hook: forget the cached table and read the kernel file at once.
/// A fresh zero timestamp would also force the next query to refresh, but
/// sibling tests share this singleton and could refresh it first, leaving
/// this test staring at an empty cache; doing the read here makes the
/// table deterministic for every interleaving.
#[cfg(test)]
fn reset_for_test() {
    let mut guard = updater().lock().unwrap();
    *guard = Updater::new();
    guard.update();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// `127.0.0.1` as the repo stores it.
    const LOOPBACK: u32 = 0x0100_007f;

    /// A `/proc/net/udp`-shaped table: header, then records whose columns
    /// sit where the kernel puts them.
    fn proc_table(records: &[&str]) -> Cursor<String> {
        let mut text =
            String::from("  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt uid  timeout inode ref pointer drops\n");
        for record in records {
            text.push_str(record);
            text.push('\n');
        }
        Cursor::new(text)
    }

    /// The record for loopback port 40001 with 42 tx / 31 rx bytes queued.
    fn loopback_record() -> String {
        "   0: 0100007F:9C41 00000000:0000 01 0000002A:0000001F 00:00000000 00000000     0        0 12345 1 ffff8800 0 100 0".to_string()
    }

    /// `port` as the address word the parser must produce: the hex in the
    /// file is the host-order value, the field keeps it network order.
    fn port_word(port: u16) -> u16 {
        platform::socket::htons(port)
    }

    /// A fresh updater that will not trigger a kernel read: the tests
    /// below drive the parse loop directly. `u64::MAX` keeps the gate's
    /// saturating age at zero forever -- a plain "now" stamp goes stale
    /// if the test thread is descheduled past the interval mid-test.
    fn quiet_updater() -> Updater {
        let mut updater = Updater::new();
        updater.last_update_micros = u64::MAX;
        updater
    }

    /// The column offsets come from the first data line, the records from
    /// the rest (network_stats.cpp:149-185).
    #[test]
    fn the_parser_walks_the_kernel_columns() {
        let mut updater = quiet_updater();
        // A second socket on port 40002 with 3/4 bytes queued.
        let second = "   1: 0100007F:9C42 00000000:0000 07 00000003:00000004 00:00000000 00000000     0        0 12346 1 ffff8800 0 100 0";
        assert!(updater.update_from(proc_table(&[&loopback_record(), second])));

        // Port 40001 (`9C41` in host-order hex) with 42/31 bytes queued,
        // port 40002 with 3/4.
        assert_eq!(
            updater.get_queue_sizes(LOOPBACK, port_word(40001)),
            Some((42, 31))
        );
        assert_eq!(
            updater.get_queue_sizes(LOOPBACK, port_word(40002)),
            Some((3, 4))
        );
        assert_eq!(updater.last_active_record_count, 2);
        assert_eq!(updater.stats_map.len(), 2);
    }

    /// An address the table does not hold has no record.
    #[test]
    fn an_unknown_address_has_no_record() {
        let mut updater = quiet_updater();
        assert_eq!(updater.get_queue_sizes(LOOPBACK, port_word(40001)), None);
    }

    /// Malformed input fails the update with the reference's message
    /// (network_stats.cpp:158); a short file just fails.
    #[test]
    fn malformed_input_fails_the_update() {
        let mut updater = quiet_updater();

        // No ": " anchor: the offset walk cannot start.
        assert!(!updater.update_from(Cursor::new("header only\nno anchor here\n")));
        // Anchor but fewer than three space-separated columns.
        assert!(!updater.update_from(Cursor::new("header\n   0: 0100007F:9C41\n")));
        // Header alone.
        assert!(!updater.update_from(Cursor::new("header\n")));
        // Nothing at all.
        assert!(!updater.update_from(Cursor::new("")));
    }

    /// Once the map holds more than twice the last record count it is
    /// cleared as a forced GC (network_stats.cpp:125-128).
    #[test]
    fn the_map_gcs_when_it_outgrows_its_records() {
        let mut updater = quiet_updater();
        assert!(updater.update_from(proc_table(&[&loopback_record()])));
        assert_eq!(updater.stats_map.len(), 1);

        // Bloat the map past 2 * last_active_record_count (1): stale
        // entries nobody has seen in the latest read.
        updater.stats_map.insert((1, 1), (0, 0));
        updater.stats_map.insert((2, 2), (0, 0));

        assert!(updater.update_from(proc_table(&[&loopback_record()])));
        assert_eq!(updater.stats_map.len(), 1, "stale entries collected");
        assert_eq!(
            updater.get_queue_sizes(LOOPBACK, port_word(40001)),
            Some((42, 31))
        );
    }

    /// The 400ms gate: a just-refreshed cache answers from the map without
    /// re-reading; a stale cache refreshes from the kernel file.
    #[test]
    fn the_cache_only_refreshes_every_interval() {
        let mut updater = quiet_updater();
        assert_eq!(
            updater.get_queue_sizes(LOOPBACK, port_word(40001)),
            None,
            "answered without a kernel read"
        );
        assert!(updater.stats_map.is_empty());

        // Age the cache past the interval: the next query must re-read.
        // (The real kernel file lists bound sockets only, so port 40001
        // has no record unless something bound it.)
        updater.last_update_micros = platform::clock::monotonic_now_micros()
            .micros()
            .saturating_sub(DEFAULT_UPDATE_INTERVAL_MICROS + 1);
        assert_eq!(updater.get_queue_sizes(LOOPBACK, port_word(40001)), None);
        assert!(
            !updater.stats_map.is_empty(),
            "the kernel file was read"
        );
    }

    /// A fresh updater has a zero timestamp, so its first query
    /// refreshes like the reference's zero-initialized `lastUpdate_`.
    #[test]
    fn a_fresh_updater_refreshes_on_its_first_query() {
        let mut updater = Updater::new();
        assert!(updater.stats_map.is_empty());
        assert_eq!(updater.get_queue_sizes(LOOPBACK, port_word(40001)), None);
        assert!(!updater.stats_map.is_empty(), "the first query refreshed");
    }

    /// A missing kernel file fails the update with the reference's
    /// message and leaves the cached table alone.
    #[test]
    fn a_missing_table_file_fails_the_update() {
        let mut updater = quiet_updater();
        assert!(!updater.update_at("/proc/net/udp-does-not-exist"));
        assert!(updater.stats_map.is_empty());
    }

    /// `NetworkStats::getQueueSizes` answers for a live bound socket,
    /// and `Endpoint`'s accessors answer 0 for a socket it does not know
    /// (endpoint.ipp:726,740 ignore the lookup result).
    #[test]
    fn a_live_socket_reports_its_queues() {
        let mut live = crate::endpoint::Endpoint::new();
        live.socket(platform::socket::SOCK_DGRAM);
        assert_eq!(live.bind(0, LOOPBACK), 0);

        // Forget the cache so the next query must read the kernel file,
        // which lists every bound socket.
        reset_for_test();
        let mut tx = -1;
        let mut rx = -1;
        assert_eq!(live.get_queue_sizes(&mut tx, &mut rx), 0);
        assert!(tx >= 0);
        assert!(rx >= 0);
        assert!(live.transmit_queue_size() >= 0);
        assert!(live.receive_queue_size() >= 0);

        // No socket, no record: the method refuses, the accessors answer 0.
        let unknown = crate::endpoint::Endpoint::new();
        let mut tx = -1;
        let mut rx = -1;
        assert_eq!(unknown.get_queue_sizes(&mut tx, &mut rx), -1);
        assert_eq!(unknown.transmit_queue_size(), 0);
        assert_eq!(unknown.receive_queue_size(), 0);
    }
}
