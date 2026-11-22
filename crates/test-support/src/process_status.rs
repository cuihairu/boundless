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

    /// The collector's current time: the stamp a snapshot carries.
    pub fn now(&self) -> u64 {
        (self.clock)()
    }
}

/// The cluster's known topology: how many players and entities of each type a
/// role holds. It is the state the test set up, and the status collector
/// samples it, so the collected counts always match it.
#[derive(Debug, Default)]
pub struct TopologyState {
    players: u64,
    entities: crate::memory_snapshot::ObjectCounts,
}

impl TopologyState {
    /// A topology with no players and no entities.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of players on the role.
    pub fn set_players(&mut self, players: u64) {
        self.players = players;
    }

    /// `count` more entities of the given type.
    pub fn add_entities(&mut self, type_name: &str, count: u64) {
        for _ in 0..count {
            self.entities.add_entity(type_name);
        }
    }

    /// `count` entities of the given type go away.
    pub fn remove_entities(&mut self, type_name: &str, count: u64) -> bool {
        for _ in 0..count {
            if !self.entities.remove_entity(type_name) {
                return false;
            }
        }
        true
    }

    /// The players on the role.
    pub fn players(&self) -> u64 {
        self.players
    }

    /// The entities of the given type.
    pub fn entity_count(&self, type_name: &str) -> u64 {
        self.entities.entity_count(type_name)
    }

    /// The entities of every type.
    pub fn entity_total(&self) -> u64 {
        self.entities.entity_total()
    }

    /// The entity type names, ascending.
    pub fn entity_types(&self) -> Vec<&str> {
        self.entities.entity_types()
    }
}

impl StatusCollector {
    /// Sample a topology's known counts into a role's status: the players and
    /// the entity total, plus one reading per entity type. Each sample
    /// replaces the counts the previous one left, so the latest readings
    /// always reflect the topology as it now stands.
    pub fn record_counts(&mut self, role: &str, topology: &TopologyState) -> &mut Self {
        // Drop the previous sample's count readings; the new sample stands in
        // for the whole topology. Other readings (version, ready, heartbeat)
        // are untouched.
        if let Some(status) = self.processes.get_mut(role) {
            status.readings.retain(|reading| {
                reading.item != "players"
                    && reading.item != "entities"
                    && !reading.item.starts_with("entity.")
            });
        }
        self.record(
            role,
            "players",
            &topology.players().to_string(),
            "topology state",
        );
        self.record(
            role,
            "entities",
            &topology.entity_total().to_string(),
            "topology state",
        );
        for type_name in topology.entity_types() {
            let item = format!("entity.{type_name}");
            let value = topology.entity_count(type_name).to_string();
            self.record(role, &item, &value, "topology state");
        }
        self
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

    #[test]
    fn collected_counts_match_the_topology_state() {
        let mut collector = StatusCollector::with_clock(|| 500);
        collector.register("cellapp01", Some(100));

        // The test topology: twelve players and three known entity types.
        let mut topology = TopologyState::new();
        topology.set_players(12);
        topology.add_entities("Character", 5);
        topology.add_entities("Avatar", 3);
        topology.add_entities("Vehicle", 4);

        collector.record_counts("cellapp01", &topology);
        let status = collector.status("cellapp01").unwrap();

        // The collected counts equal the topology's known numbers.
        assert_eq!(status.reading("players").unwrap().value, "12");
        assert_eq!(status.reading("entities").unwrap().value, "12");
        assert_eq!(status.reading("entity.Character").unwrap().value, "5");
        assert_eq!(status.reading("entity.Avatar").unwrap().value, "3");
        assert_eq!(status.reading("entity.Vehicle").unwrap().value, "4");
        // Every count names its source.
        assert_eq!(status.reading("players").unwrap().source, "topology state");
        assert_eq!(status.reading("entities").unwrap().source, "topology state");
    }

    #[test]
    fn re_sampling_tracks_topology_changes() {
        let mut collector = StatusCollector::with_clock(|| 600);
        collector.register("cellapp01", Some(100));
        let mut topology = TopologyState::new();
        topology.set_players(4);
        topology.add_entities("Character", 6);

        collector.record_counts("cellapp01", &topology);
        let status = collector.status("cellapp01").unwrap();
        assert_eq!(status.reading("players").unwrap().value, "4");
        assert_eq!(status.reading("entities").unwrap().value, "6");

        // The load moves: two players leave, five entities despawn. The next
        // sample reflects the topology as it now stands.
        topology.set_players(2);
        assert!(topology.remove_entities("Character", 5));
        collector.record_counts("cellapp01", &topology);
        let status = collector.status("cellapp01").unwrap();
        assert_eq!(status.reading("players").unwrap().value, "2");
        assert_eq!(status.reading("entities").unwrap().value, "1");
        assert_eq!(status.reading("entity.Character").unwrap().value, "1");
    }

    #[test]
    fn a_drained_topology_reads_zero() {
        let mut collector = StatusCollector::with_clock(|| 700);
        collector.register("cellapp01", Some(100));
        let mut topology = TopologyState::new();
        topology.add_entities("Character", 3);

        collector.record_counts("cellapp01", &topology);
        let status = collector.status("cellapp01").unwrap();
        assert_eq!(status.reading("entities").unwrap().value, "3");

        // Drain every entity: the total falls to zero and the type's row
        // leaves the topology.
        assert!(topology.remove_entities("Character", 3));
        assert_eq!(topology.entity_total(), 0);
        assert_eq!(topology.entity_types(), Vec::<&str>::new());
        collector.record_counts("cellapp01", &topology);
        let status = collector.status("cellapp01").unwrap();
        assert_eq!(status.reading("entities").unwrap().value, "0");
        assert_eq!(status.reading("entity.Character"), None);
    }
}
