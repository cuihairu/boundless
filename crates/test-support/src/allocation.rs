//! Isolated port and database allocation for parallel runs (BW-18-02-03).
//!
//! The reference tests hard-code port ranges and share the checkout's data
//! directories, so two runs collide. Here a run opens its own
//! [`RunRoot`](struct.RunRoot.html) (logs, databases, artifacts), database
//! slots hang off it, and ports are leased by binding `127.0.0.1:0` --
//! the kernel hands out a port nobody else holds while the lease lives.

use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::workspace::BASE_DIRECTORY;

static NEXT_SLOT: AtomicU64 = AtomicU64::new(1);

/// Per-run directory tree: `logs/`, `db/`, `artifacts/`, removed on drop
/// unless kept for a failure report.
#[derive(Debug)]
pub struct RunRoot {
    path: Option<PathBuf>,
    keep: bool,
}

impl RunRoot {
    /// Open a uniquely named run root.
    pub fn open(label: &str) -> io::Result<Self> {
        let base = std::env::temp_dir().join(BASE_DIRECTORY).join("runs");
        std::fs::create_dir_all(&base)?;
        let unique = format!(
            "{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        );
        let path = base.join(unique);
        std::fs::create_dir(&path)?;
        for sub in ["logs", "db", "artifacts"] {
            std::fs::create_dir(path.join(sub))?;
        }
        Ok(Self {
            path: Some(path),
            keep: false,
        })
    }

    /// The run root path.
    pub fn path(&self) -> &Path {
        self.path.as_deref().expect("run root still owned")
    }

    /// Directory where role logs are collected.
    pub fn logs_dir(&self) -> PathBuf {
        self.path().join("logs")
    }

    /// Parent of the run's database slots.
    pub fn db_dir(&self) -> PathBuf {
        self.path().join("db")
    }

    /// Directory for preserved failure scenes.
    pub fn artifacts_dir(&self) -> PathBuf {
        self.path().join("artifacts")
    }

    /// Allocate a database slot: `db/<label>-<n>/` with `db/` and
    /// `journal/` subdirectories, empty when handed out. Distinct slots for
    /// the same label, so parallel tests never share state.
    pub fn database_slot(&self, label: &str) -> io::Result<PathBuf> {
        let slot = self.db_dir().join(format!(
            "{label}-{}",
            NEXT_SLOT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(slot.join("db"))?;
        std::fs::create_dir_all(slot.join("journal"))?;
        Ok(slot)
    }

    /// Keep the tree on drop so a failure report can open it.
    pub fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for RunRoot {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// Hands out port leases for role listeners.
#[derive(Debug, Default, Clone, Copy)]
pub struct PortAllocator;

impl PortAllocator {
    /// A fresh allocator; leases live as long as the caller holds them.
    pub fn new() -> Self {
        Self
    }

    /// Lease one free port. The lease keeps a listener bound to it, so no
    /// other run can be handed the same port until [`PortLease::release`].
    pub fn take(&self) -> io::Result<PortLease> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        Ok(PortLease {
            port,
            listener: Some(listener),
        })
    }

    /// Lease several ports at once, all distinct while held.
    pub fn take_many(&self, count: usize) -> io::Result<Vec<PortLease>> {
        (0..count).map(|_| self.take()).collect()
    }
}

/// A reserved port. Dropping the lease also releases the port.
#[derive(Debug)]
pub struct PortLease {
    port: u16,
    listener: Option<TcpListener>,
}

impl PortLease {
    /// The reserved port number.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// True while the reservation is held.
    pub fn is_held(&self) -> bool {
        self.listener.is_some()
    }

    /// Give the port back to the OS so a role can bind it; returns the
    /// port number.
    pub fn release(mut self) -> u16 {
        self.listener = None;
        self.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_roots_are_isolated_and_laid_out() {
        let first = RunRoot::open("alpha").unwrap();
        let second = RunRoot::open("beta").unwrap();

        assert_ne!(first.path(), second.path());
        for root in [&first, &second] {
            assert!(root.logs_dir().is_dir());
            assert!(root.db_dir().is_dir());
            assert!(root.artifacts_dir().is_dir());
        }
    }

    #[test]
    fn a_dropped_run_root_is_removed_and_a_kept_one_survives() {
        let dropped = RunRoot::open("dropped").unwrap();
        let path = dropped.path().to_path_buf();
        drop(dropped);
        assert!(!path.exists());

        let mut kept = RunRoot::open("kept").unwrap();
        let kept_path = kept.path().to_path_buf();
        kept.keep();
        drop(kept);
        assert!(kept_path.exists());
        let _ = std::fs::remove_dir_all(&kept_path);
    }

    #[test]
    fn database_slots_are_distinct_within_a_run() {
        let run = RunRoot::open("slots").unwrap();
        let first = run.database_slot("baseapp").unwrap();
        let second = run.database_slot("baseapp").unwrap();
        let cellapp = run.database_slot("cellapp").unwrap();

        assert_ne!(first, second);
        assert_ne!(first, cellapp);
        for slot in [&first, &second, &cellapp] {
            assert!(slot.join("db").is_dir());
            assert!(slot.join("journal").is_dir());
            let empty = std::fs::read_dir(slot.join("db")).unwrap().count();
            assert_eq!(empty, 0, "slots start empty");
        }
        assert!(first.starts_with(run.db_dir()));
    }

    #[test]
    fn port_leases_are_distinct_while_held() {
        let allocator = PortAllocator::new();
        let first = allocator.take().unwrap();
        let second = allocator.take().unwrap();

        // Two leases in the same test, and two tests in parallel processes,
        // can never be handed the same port while both hold them.
        assert_ne!(first.port(), second.port());
        assert!(first.is_held() && second.is_held());
    }

    #[test]
    fn bulk_leases_come_back_all_distinct() {
        let leases = PortAllocator::new().take_many(5).unwrap();
        let ports: std::collections::BTreeSet<u16> =
            leases.iter().map(|lease| lease.port()).collect();
        assert_eq!(ports.len(), 5);
        assert!(leases.iter().all(PortLease::is_held));
    }

    #[test]
    fn a_released_lease_gives_up_the_reservation() {
        let lease = PortAllocator::new().take().unwrap();
        let port = lease.port();
        assert!(lease.is_held());
        // Releasing consumes the lease and returns the port for the role
        // configuration to use.
        let released = lease.release();
        assert_eq!(released, port);
    }

    /// The two-tests-in-parallel acceptance, simulated inside one test:
    /// two independent runs take ports and slots and nothing overlaps.
    #[test]
    fn two_parallel_runs_never_overlap() {
        let run_a = RunRoot::open("parallel-a").unwrap();
        let run_b = RunRoot::open("parallel-b").unwrap();
        let allocator_a = PortAllocator::new();
        let allocator_b = PortAllocator::new();

        let ports_a: Vec<u16> = allocator_a
            .take_many(3)
            .unwrap()
            .iter()
            .map(|lease| lease.port())
            .collect();
        let ports_b: Vec<u16> = allocator_b
            .take_many(3)
            .unwrap()
            .iter()
            .map(|lease| lease.port())
            .collect();
        assert!(ports_a.iter().all(|port| !ports_b.contains(port)));

        let slot_a = run_a.database_slot("baseapp").unwrap();
        let slot_b = run_b.database_slot("baseapp").unwrap();
        assert!(slot_a.starts_with(run_a.db_dir()));
        assert!(slot_b.starts_with(run_b.db_dir()));
        assert_ne!(slot_a, slot_b);
    }
}
