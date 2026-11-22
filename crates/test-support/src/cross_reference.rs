//! BW-25-02-04: cross-locating the same object across sources.
//!
//! A viewer follows one object — an entity — across every source that knows
//! it: the Space/Cell registry holds where it is, the cell placement names
//! the process hosting that cell, the Watcher reports that process's status,
//! and the log capture holds the lines written about the object. The
//! cross-reference joins them, so the same object is locatable in every
//! source at once — and where a source does not know it, the gap shows
//! instead of a guess.

use crate::process_status::{ProcessStatus, StatusCollector};
use crate::space_cell::SpaceCellRegistry;
use std::collections::BTreeMap;

/// One captured log line: when it was written, by which process, about which
/// object, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// When the line was written.
    pub at: u64,
    /// The process that wrote it (`cellapp01`).
    pub role: String,
    /// The object the line is about (`entity:7`, `space:1`).
    pub object: String,
    /// The line itself.
    pub message: String,
}

/// The log capture: lines are recorded about objects, and read back per
/// object or per process.
#[derive(Default)]
pub struct LogCapture {
    lines: Vec<LogLine>,
}

impl LogCapture {
    /// A capture with no lines.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a line: when, by which process, about which object, saying
    /// what.
    pub fn record(&mut self, at: u64, role: &str, object: &str, message: &str) -> &mut Self {
        self.lines.push(LogLine {
            at,
            role: role.to_string(),
            object: object.to_string(),
            message: message.to_string(),
        });
        self
    }

    /// The lines about an object, in the order written.
    pub fn about(&self, object: &str) -> Vec<&LogLine> {
        self.lines
            .iter()
            .filter(|line| line.object == object)
            .collect()
    }

    /// The lines a process wrote, in the order written.
    pub fn for_role(&self, role: &str) -> Vec<&LogLine> {
        self.lines.iter().filter(|line| line.role == role).collect()
    }
}

/// Which process hosts which cell: the (space, cell) to role placement.
#[derive(Default)]
pub struct CellPlacement {
    hosts: BTreeMap<(u64, u64), String>,
}

impl CellPlacement {
    /// A placement with nothing hosted.
    pub fn new() -> Self {
        Self::default()
    }

    /// Place a cell on the process that hosts it.
    pub fn place(&mut self, space: u64, cell: u64, role: &str) -> &mut Self {
        self.hosts.insert((space, cell), role.to_string());
        self
    }

    /// The role hosting a cell, when the cell is placed.
    pub fn host(&self, space: u64, cell: u64) -> Option<&str> {
        self.hosts.get(&(space, cell)).map(String::as_str)
    }

    /// The cells a role hosts, ascending.
    pub fn cells_of(&self, role: &str) -> Vec<(u64, u64)> {
        self.hosts
            .iter()
            .filter(|(_, host)| host.as_str() == role)
            .map(|(&(space, cell), _)| (space, cell))
            .collect()
    }
}

/// One object located in every source that knows it.
#[derive(Debug)]
pub struct ObjectLocation<'a> {
    /// The entity that was located.
    pub entity: u64,
    /// Where it is: the space and cell holding it, when it is assigned.
    pub space_cell: Option<(u64, u64)>,
    /// The role hosting that cell, when the cell is placed.
    pub host: Option<&'a str>,
    /// The host's Watcher status, when the host is registered.
    pub status: Option<&'a ProcessStatus>,
    /// The log lines written about the entity, oldest first.
    pub logs: Vec<&'a LogLine>,
}

/// The join across Space/Cell, the cell placement, the Watcher and the logs,
/// so the same object is locatable in every source at once.
pub struct CrossReference<'a> {
    registry: &'a SpaceCellRegistry,
    placement: &'a CellPlacement,
    status: &'a StatusCollector,
    logs: &'a LogCapture,
}

impl<'a> CrossReference<'a> {
    /// A cross-reference over the sources as they now stand.
    pub fn new(
        registry: &'a SpaceCellRegistry,
        placement: &'a CellPlacement,
        status: &'a StatusCollector,
        logs: &'a LogCapture,
    ) -> Self {
        Self {
            registry,
            placement,
            status,
            logs,
        }
    }

    /// Locate an entity in every source: where it is, who hosts that cell,
    /// the host's Watcher status, and the logs about it. A source that does
    /// not know the entity leaves its field empty — the gap shows instead of
    /// a guess.
    pub fn locate(&self, entity: u64) -> ObjectLocation<'a> {
        let space_cell = self.registry.owner(entity);
        let host = space_cell.and_then(|(space, cell)| self.placement.host(space, cell));
        let status = host.and_then(|role| self.status.status(role));
        ObjectLocation {
            entity,
            space_cell,
            host,
            status,
            logs: self.logs.about(&object_key(entity)),
        }
    }
}

/// The log key an entity's lines are recorded under.
fn object_key(entity: u64) -> String {
    format!("entity:{entity}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_entity_is_locatable_in_every_source() {
        let mut registry = SpaceCellRegistry::new();
        registry.assign(7, 1, 10);
        let mut placement = CellPlacement::new();
        placement.place(1, 10, "cellapp01");
        let mut status = StatusCollector::with_clock(|| 1_000);
        status.register("cellapp01", Some(100));
        status.record("cellapp01", "version", "0.1.0", "build metadata");
        status.record("cellapp01", "ready", "true", "readiness announcement");
        let mut logs = LogCapture::new();
        logs.record(
            1_000,
            "cellapp01",
            "entity:7",
            "entity 7 entered cell (1, 10)",
        );
        logs.record(1_050, "baseapp01", "entity:7", "proxy saw entity 7 log in");

        let xref = CrossReference::new(&registry, &placement, &status, &logs);
        let located = xref.locate(7);

        // The same object, located in every source at once: where it is, who
        // hosts it, the host's status, and the lines written about it.
        assert_eq!(located.space_cell, Some((1, 10)));
        assert_eq!(located.host, Some("cellapp01"));
        let host_status = located.status.unwrap();
        assert_eq!(host_status.pid, Some(100));
        assert!(host_status.is_ready());
        assert_eq!(host_status.reading("version").unwrap().value, "0.1.0");
        assert_eq!(located.logs.len(), 2);
        assert_eq!(located.logs[0].role, "cellapp01");
        assert_eq!(located.logs[1].role, "baseapp01");
        assert_eq!(located.logs[1].at, 1_050);
    }

    #[test]
    fn an_unknown_entity_locates_with_its_gaps_showing() {
        let registry = SpaceCellRegistry::new();
        let placement = CellPlacement::new();
        let status = StatusCollector::with_clock(|| 0);
        let logs = LogCapture::new();
        let xref = CrossReference::new(&registry, &placement, &status, &logs);

        // No source knows the entity: every field is empty, and no source
        // invents a location.
        let located = xref.locate(4242);
        assert_eq!(located.entity, 4242);
        assert_eq!(located.space_cell, None);
        assert_eq!(located.host, None);
        assert!(located.status.is_none());
        assert!(located.logs.is_empty());
    }

    #[test]
    fn a_hosting_role_locates_its_cells_and_its_log_lines() {
        let mut placement = CellPlacement::new();
        placement
            .place(1, 10, "cellapp01")
            .place(2, 20, "cellapp01")
            .place(1, 11, "cellapp02");
        let mut logs = LogCapture::new();
        logs.record(5, "cellapp01", "space:1", "cell (1, 10) created");
        logs.record(6, "cellapp02", "space:1", "cell (1, 11) created");
        logs.record(7, "cellapp01", "space:2", "cell (2, 20) created");

        // The Watcher names cellapp01; the placement and the logs answer with
        // everything that process holds and wrote.
        assert_eq!(placement.cells_of("cellapp01"), vec![(1, 10), (2, 20)]);
        assert_eq!(placement.host(1, 11), Some("cellapp02"));
        let lines = logs.for_role("cellapp01");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].object, "space:1");
        assert_eq!(lines[1].object, "space:2");
    }

    #[test]
    fn after_a_migration_the_registry_and_the_logs_agree() {
        let mut registry = SpaceCellRegistry::new();
        registry.assign(7, 1, 10);
        let mut placement = CellPlacement::new();
        placement
            .place(1, 10, "cellapp01")
            .place(1, 11, "cellapp02");
        let status = StatusCollector::with_clock(|| 0);
        let mut logs = LogCapture::new();

        let xref = CrossReference::new(&registry, &placement, &status, &logs);
        assert_eq!(xref.locate(7).host, Some("cellapp01"));

        // The entity migrates; both sources record where it now is.
        registry.migrate(7, 1, 11);
        logs.record(
            900,
            "cellapp02",
            "entity:7",
            "entity 7 arrived from cell (1, 10)",
        );

        let located = CrossReference::new(&registry, &placement, &status, &logs).locate(7);
        assert_eq!(located.space_cell, Some((1, 11)));
        assert_eq!(located.host, Some("cellapp02"));
        assert_eq!(located.logs.len(), 1);
        assert_eq!(
            located.logs[0].message,
            "entity 7 arrived from cell (1, 10)"
        );
    }
}
