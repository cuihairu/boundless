//! BW-25: process and cluster running state.
//!
//! The port answers the questions the reference's Watcher, MachineGuard,
//! `status_check_watcher.cpp` and `cell_viewer_server.cpp` answer: what each
//! process is (its version, whether it is ready, whether its heart still
//! beats), what it holds (players, entities, spaces), what its queues look
//! like, and where it is in a recovery. Every reading carries its source and
//! the time it was taken, so a status is always traceable (BW-25-01-01).

/// One collected status reading: the item, its value, where it came from, and
/// when it was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReading {
    /// The item's name (`version`, `ready`, `heartbeat`, ...).
    pub item: String,
    /// The item's value as collected.
    pub value: String,
    /// Where the reading came from.
    pub source: String,
    /// When the reading was taken.
    pub collected_at: u64,
}

/// One process's collected status.
#[derive(Debug, Default)]
pub struct ProcessStatus {
    /// The process's role (`cellapp01`).
    pub role: String,
    /// The process id, when it is a live process.
    pub pid: Option<u32>,
    /// The readings collected so far, in the order taken.
    pub readings: Vec<StatusReading>,
}

impl ProcessStatus {
    /// A reading by item name, the latest one taken.
    pub fn reading(&self, item: &str) -> Option<&StatusReading> {
        self.readings.iter().rev().find(|r| r.item == item)
    }

    /// Whether the process has announced readiness.
    pub fn is_ready(&self) -> bool {
        self.reading("ready").is_some()
    }

    /// How long since the last heartbeat, measured at `now`.
    pub fn heartbeat_age_at(&self, now: u64) -> Option<u64> {
        self.reading("heartbeat")
            .map(|reading| now.saturating_sub(reading.collected_at))
    }
}

/// The cluster's status collector. It registers processes and records their
/// readings, stamping each with the time it was taken.
pub struct StatusCollector {
    processes: std::collections::BTreeMap<String, ProcessStatus>,
    clock: Box<dyn Fn() -> u64>,
}

impl Default for StatusCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusCollector {
    /// A collector stamping readings from the wall clock.
    pub fn new() -> Self {
        Self::with_clock(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos() as u64)
                .unwrap_or(0)
        })
    }

    /// A collector whose reading times come from `clock`, so a test can drive
    /// when each reading was taken.
    pub fn with_clock(clock: impl Fn() -> u64 + 'static) -> Self {
        Self {
            processes: std::collections::BTreeMap::new(),
            clock: Box::new(clock),
        }
    }

    /// Register a process by role, with its pid when it is live.
    pub fn register(&mut self, role: &str, pid: Option<u32>) -> &mut Self {
        self.processes.insert(
            role.to_string(),
            ProcessStatus {
                role: role.to_string(),
                pid,
                readings: Vec::new(),
            },
        );
        self
    }

    /// Record a reading for a process: the item, its value and where it came
    /// from. The collection time is stamped here.
    pub fn record(&mut self, role: &str, item: &str, value: &str, source: &str) -> &mut Self {
        let collected_at = (self.clock)();
        let reading = StatusReading {
            item: item.to_string(),
            value: value.to_string(),
            source: source.to_string(),
            collected_at,
        };
        self.processes
            .entry(role.to_string())
            .or_insert_with(|| ProcessStatus {
                role: role.to_string(),
                pid: None,
                readings: Vec::new(),
            })
            .readings
            .push(reading);
        self
    }

    /// Record one heartbeat beat for a process.
    pub fn beat(&mut self, role: &str) -> &mut Self {
        let beat = self
            .processes
            .get(role)
            .and_then(|status| status.reading("heartbeat"))
            .and_then(|reading| reading.value.parse::<u64>().ok())
            .unwrap_or(0)
            + 1;
        self.record(role, "heartbeat", &beat.to_string(), "process heartbeat")
    }

    /// A process's collected status.
    pub fn status(&self, role: &str) -> Option<&ProcessStatus> {
        self.processes.get(role)
    }

    /// The collected roles, ascending.
    pub fn roles(&self) -> Vec<&str> {
        self.processes.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_readiness_and_heartbeat_are_traceable() {
        // A driven clock stamps when each reading was taken.
        let mut collector = StatusCollector::with_clock(|| 1_000);
        collector.register("cellapp01", Some(100));

        // Version from the build, readiness from the announcement, and two
        // heartbeats over time.
        collector.record("cellapp01", "version", "0.1.0", "build metadata");
        collector.record("cellapp01", "ready", "true", "readiness announcement");

        let status = collector.status("cellapp01").unwrap();
        assert_eq!(status.role, "cellapp01");
        assert_eq!(status.pid, Some(100));

        // Every reading carries its source and its collection time.
        let version = status.reading("version").unwrap();
        assert_eq!(version.value, "0.1.0");
        assert_eq!(version.source, "build metadata");
        assert_eq!(version.collected_at, 1_000);
        let ready = status.reading("ready").unwrap();
        assert_eq!(ready.source, "readiness announcement");
        assert_eq!(ready.collected_at, 1_000);
        assert!(status.is_ready());
    }

    #[test]
    fn heartbeats_age_from_their_last_beat() {
        // A clock the test can advance: the collector reads the shared value.
        let now = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000));
        let mut collector = {
            let now = std::sync::Arc::clone(&now);
            StatusCollector::with_clock(move || now.load(std::sync::atomic::Ordering::SeqCst))
        };
        collector.register("baseapp01", Some(200));

        // Two beats, one second apart: the age is measured from the last.
        collector.beat("baseapp01");
        now.store(1_100, std::sync::atomic::Ordering::SeqCst);
        collector.beat("baseapp01");

        let status = collector.status("baseapp01").unwrap();
        let heartbeat = status.reading("heartbeat").unwrap();
        assert_eq!(heartbeat.collected_at, 1_100);
        assert_eq!(heartbeat.value, "2", "the second beat");
        assert_eq!(heartbeat.source, "process heartbeat");
        assert_eq!(status.heartbeat_age_at(1_250), Some(150));
        assert_eq!(status.heartbeat_age_at(1_050), Some(0), "never negative");
    }

    #[test]
    fn an_unannounced_process_is_not_ready() {
        let mut collector = StatusCollector::with_clock(|| 5);
        collector.register("cellapp02", Some(101));
        let status = collector.status("cellapp02").unwrap();
        assert!(!status.is_ready());
        assert_eq!(status.heartbeat_age_at(100), None);
        assert_eq!(status.reading("version"), None);
    }

    #[test]
    fn processes_are_collected_independently_and_by_role() {
        let mut collector = StatusCollector::with_clock(|| 7);
        collector
            .register("cellapp01", Some(100))
            .register("baseapp01", Some(200));
        collector
            .record("cellapp01", "version", "0.1.0", "build metadata")
            .record("baseapp01", "version", "0.1.0", "build metadata");

        // Roles are sorted and isolated: one process's readiness says nothing
        // about the other's.
        assert_eq!(collector.roles(), vec!["baseapp01", "cellapp01"]);
        collector.record("cellapp01", "ready", "true", "readiness announcement");
        assert!(collector.status("cellapp01").unwrap().is_ready());
        assert!(!collector.status("baseapp01").unwrap().is_ready());
    }
}
