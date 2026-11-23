//! BW-25-01-04: network and database queue depth.
//!
//! The port reports the backlog the reference's Watcher reports for the
//! network and database queues: how many messages a queue holds. A known
//! backlog — however many messages were queued — is visible in the snapshot,
//! and each reading names its source and when it was taken.

/// One queue's backlog reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueReading {
    /// The queue's name (`network.send`, `db.write`, ...).
    pub queue: String,
    /// How many messages the queue holds.
    pub depth: u64,
    /// Where the reading came from.
    pub source: String,
    /// When the reading was taken.
    pub collected_at: u64,
}

/// The queue-depth collector. It tracks each queue's backlog and snapshots it.
pub struct QueueCollector {
    queues: std::collections::BTreeMap<String, u64>,
    clock: Box<dyn Fn() -> u64>,
}

impl Default for QueueCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl QueueCollector {
    /// A collector stamping readings from the wall clock.
    pub fn new() -> Self {
        Self::with_clock(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos() as u64)
                .unwrap_or(0)
        })
    }

    /// A collector whose reading times come from `clock`.
    pub fn with_clock(clock: impl Fn() -> u64 + 'static) -> Self {
        Self {
            queues: std::collections::BTreeMap::new(),
            clock: Box::new(clock),
        }
    }

    /// One message into the queue.
    pub fn push(&mut self, queue: &str) {
        *self.queues.entry(queue.to_string()).or_insert(0) += 1;
    }

    /// One message out of the queue. `false` when the queue was already empty
    /// (a depth never underflows).
    pub fn pop(&mut self, queue: &str) -> bool {
        let Some(depth) = self.queues.get_mut(queue) else {
            return false;
        };
        if *depth == 0 {
            return false;
        }
        *depth -= 1;
        true
    }

    /// Set a queue's backlog directly, for a known load.
    pub fn set_depth(&mut self, queue: &str, depth: u64) {
        self.queues.insert(queue.to_string(), depth);
    }

    /// A queue's current depth.
    pub fn depth(&self, queue: &str) -> u64 {
        self.queues.get(queue).copied().unwrap_or(0)
    }

    /// Snapshot every queue's backlog, ascending by name, each reading stamped
    /// with its source and the time it was taken.
    pub fn snapshot(&self) -> Vec<QueueReading> {
        let collected_at = (self.clock)();
        self.queues
            .iter()
            .map(|(queue, depth)| QueueReading {
                queue: queue.clone(),
                depth: *depth,
                source: "queue depth".to_string(),
                collected_at,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_backlog_is_visible_in_the_snapshot() {
        let mut collector = QueueCollector::with_clock(|| 900);
        // A known load: 250 messages on the send queue, 40 on the db write.
        collector.set_depth("network.send", 250);
        collector.set_depth("db.write", 40);

        let snapshot = collector.snapshot();
        assert_eq!(snapshot.len(), 2);
        // Ascending by queue name.
        assert_eq!(snapshot[0].queue, "db.write");
        assert_eq!(snapshot[0].depth, 40);
        assert_eq!(snapshot[1].queue, "network.send");
        assert_eq!(snapshot[1].depth, 250);
        // Every reading names its source and when it was taken.
        assert_eq!(snapshot[0].source, "queue depth");
        assert_eq!(snapshot[0].collected_at, 900);
        assert_eq!(snapshot[1].collected_at, 900);
        assert_eq!(collector.depth("network.send"), 250);
        assert_eq!(collector.depth("db.write"), 40);
    }

    #[test]
    fn push_and_pop_track_the_queue_depth() {
        let mut collector = QueueCollector::with_clock(|| 1);
        collector.push("network.receive");
        collector.push("network.receive");
        collector.push("network.receive");
        assert_eq!(collector.depth("network.receive"), 3);

        assert!(collector.pop("network.receive"));
        assert_eq!(collector.depth("network.receive"), 2);

        // Draining to zero, then popping an empty queue refuses.
        assert!(collector.pop("network.receive"));
        assert!(collector.pop("network.receive"));
        assert_eq!(collector.depth("network.receive"), 0);
        assert!(!collector.pop("network.receive"));
        assert_eq!(collector.depth("network.receive"), 0);
    }

    #[test]
    fn an_unknown_queue_reads_zero() {
        let collector = QueueCollector::with_clock(|| 2);
        assert_eq!(collector.depth("db.read"), 0);
        assert!(collector.snapshot().is_empty());
        assert!(!QueueCollector::with_clock(|| 3).pop("db.read"));
    }
}
