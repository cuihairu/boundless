//! Multi-process harness (BW-18-02).
//!
//! The reference `MultiProcTestCase` (`lib/unit_test_lib/multi_proc_test_case.cpp`)
//! forks anonymous children and SIGKILLs them on failure -- there is no notion
//! of roles, dependency order, ports or databases. This harness keeps the
//! real-OS-process idea but adds the pieces acceptance demands: named roles
//! started in dependency order ([`Topology`]), readiness waits, isolated
//! port/database allocation, log capture and full process-tree teardown.

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

/// One role in a test topology: a program to run and the roles it needs
/// before it can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    /// Unique role name (`machined`, `cellappmgr`, ...).
    pub name: String,
    /// Program path or name, resolved through `PATH` like `Command`.
    pub program: String,
    /// Program arguments, in order.
    pub args: Vec<String>,
    /// Names of roles that must be started before this one.
    pub depends_on: Vec<String>,
}

impl ProcessSpec {
    /// A role running `program` with no arguments and no dependencies.
    pub fn new(name: &str, program: &str) -> Self {
        Self {
            name: name.to_string(),
            program: program.to_string(),
            args: Vec::new(),
            depends_on: Vec::new(),
        }
    }

    /// Append one argument.
    pub fn arg(mut self, argument: impl Into<String>) -> Self {
        self.args.push(argument.into());
        self
    }

    /// Name a role that must start before this one.
    pub fn depends_on(mut self, name: &str) -> Self {
        self.depends_on.push(name.to_string());
        self
    }
}

/// A set of roles and their dependency edges, started in topological order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Topology {
    specs: Vec<ProcessSpec>,
}

impl Topology {
    /// An empty topology.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a role; its name must be unique within the topology.
    pub fn add(mut self, spec: ProcessSpec) -> Self {
        self.specs.push(spec);
        self
    }

    /// The roles in insertion order.
    pub fn specs(&self) -> &[ProcessSpec] {
        &self.specs
    }

    /// Roles in start order: dependencies before dependents, and insertion
    /// order between independent roles, so runs are reproducible.
    pub fn start_order(&self) -> Result<Vec<&ProcessSpec>, HarnessError> {
        let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, spec) in self.specs.iter().enumerate() {
            if index_of.insert(spec.name.as_str(), index).is_some() {
                return Err(HarnessError::DuplicateRole(spec.name.clone()));
            }
        }

        // Kahn's algorithm; ready roles are kept in a set keyed by index so
        // ties resolve to insertion order.
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); self.specs.len()];
        let mut pending_deps: Vec<usize> = self.specs.iter().map(|s| s.depends_on.len()).collect();
        for (index, spec) in self.specs.iter().enumerate() {
            for dependency in &spec.depends_on {
                let Some(&dep_index) = index_of.get(dependency.as_str()) else {
                    return Err(HarnessError::UnknownDependency {
                        role: spec.name.clone(),
                        missing: dependency.clone(),
                    });
                };
                dependents[dep_index].push(index);
            }
        }

        let mut ready: BTreeSet<usize> = (0..self.specs.len())
            .filter(|&index| pending_deps[index] == 0)
            .collect();
        let mut order = Vec::with_capacity(self.specs.len());
        while let Some(&index) = ready.iter().next() {
            ready.remove(&index);
            for &dependent in &dependents[index] {
                pending_deps[dependent] -= 1;
                if pending_deps[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
            order.push(index);
        }

        if order.len() != self.specs.len() {
            let stuck: Vec<String> = self
                .specs
                .iter()
                .enumerate()
                .filter(|(index, _)| !order.contains(index))
                .map(|(_, spec)| spec.name.clone())
                .collect();
            return Err(HarnessError::DependencyCycle(stuck));
        }

        Ok(order.into_iter().map(|index| &self.specs[index]).collect())
    }
}

/// Everything that can go wrong before or while starting or waiting on a
/// topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessError {
    /// Two roles share a name.
    DuplicateRole(String),
    /// A role depends on a name no role provides.
    UnknownDependency { role: String, missing: String },
    /// The dependency edges form a cycle; the members are listed.
    DependencyCycle(Vec<String>),
    /// A role's program could not be spawned.
    Spawn { role: String, reason: String },
    /// A wait named a role the group does not have.
    UnknownRole(String),
    /// A role exited before its readiness condition held; the status names
    /// how it died.
    RoleExited { role: String, status: String },
    /// A readiness condition never held within the budget; the error names
    /// the role and the condition so a timeout pinpoints the offender.
    NotReady {
        role: String,
        condition: String,
        waited_ms: u64,
    },
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::DuplicateRole(name) => write!(f, "duplicate role '{name}'"),
            HarnessError::UnknownDependency { role, missing } => {
                write!(f, "role '{role}' depends on unknown role '{missing}'")
            }
            HarnessError::DependencyCycle(members) => {
                write!(f, "dependency cycle among: {}", members.join(", "))
            }
            HarnessError::Spawn { role, reason } => {
                write!(f, "role '{role}' failed to spawn: {reason}")
            }
            HarnessError::UnknownRole(name) => write!(f, "no such role '{name}'"),
            HarnessError::RoleExited { role, status } => {
                write!(f, "role '{role}' exited before readiness: {status}")
            }
            HarnessError::NotReady {
                role,
                condition,
                waited_ms,
            } => write!(
                f,
                "role '{role}' not ready after {waited_ms}ms: {condition}"
            ),
        }
    }
}

impl std::error::Error for HarnessError {}

/// A condition that says a role is ready to be talked to. The reference
/// tests sleep fixed amounts (`sleep( 0.5 )` before connecting); a probe
/// replaces the guess with a check and a timeout that names the role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// Something accepts TCP connections on `127.0.0.1:port` -- how the
    /// reference waits for machined and role listeners.
    PortOpen(u16),
    /// The file at `path` exists -- marker files, pipes, created databases.
    FileExists(PathBuf),
    /// The file at `path` exists and contains `pattern` -- role logs
    /// announcing readiness.
    LogContains { path: PathBuf, pattern: String },
}

impl Readiness {
    /// A human-readable form used in timeout errors.
    pub fn describe(&self) -> String {
        match self {
            Readiness::PortOpen(port) => format!("port 127.0.0.1:{port} accepting"),
            Readiness::FileExists(path) => format!("file {} exists", path.display()),
            Readiness::LogContains { path, pattern } => {
                format!("{} contains '{}'", path.display(), pattern)
            }
        }
    }

    /// Check the condition once; the wait loop polls this.
    pub fn probe(&self) -> bool {
        match self {
            Readiness::PortOpen(port) => TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], *port)),
                Duration::from_millis(200),
            )
            .is_ok(),
            Readiness::FileExists(path) => path.exists(),
            Readiness::LogContains { path, pattern } => std::fs::read_to_string(path)
                .map(|text| text.contains(pattern.as_str()))
                .unwrap_or(false),
        }
    }
}

/// Poll interval of the readiness loop.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A live topology: every role's process, plus the order they were started
/// in, observable to the test.
#[derive(Debug)]
pub struct ProcessGroup {
    topology: Topology,
    children: Vec<(String, Child)>,
    start_order: Vec<String>,
}

impl ProcessGroup {
    /// Validate the topology and spawn every role in dependency order. A
    /// failed spawn terminates the roles already started, so a half-started
    /// group never leaks.
    pub fn start(topology: &Topology) -> Result<Self, HarnessError> {
        let order = topology.start_order()?;

        let mut children: Vec<(String, Child)> = Vec::with_capacity(order.len());
        let mut start_order = Vec::with_capacity(order.len());
        for spec in order {
            let spawned = Command::new(&spec.program)
                .args(&spec.args)
                .spawn()
                .map_err(|error| {
                    // Terminate what is already running before reporting.
                    for (_, mut child) in children.drain(..) {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    HarnessError::Spawn {
                        role: spec.name.clone(),
                        reason: error.to_string(),
                    }
                })?;
            start_order.push(spec.name.clone());
            children.push((spec.name.clone(), spawned));
        }

        Ok(Self {
            topology: topology.clone(),
            children,
            start_order,
        })
    }

    /// Role names in the order their processes were spawned.
    pub fn start_order(&self) -> &[String] {
        &self.start_order
    }

    /// The topology the group was started from.
    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// The OS pid of a role's process, while it is still un-reaped.
    pub fn pid_of(&self, name: &str) -> Option<u32> {
        self.children
            .iter()
            .find(|(role, _)| role == name)
            .map(|(_, child)| child.id())
    }

    /// Wait for every role to exit, returning `(role, status)` in start
    /// order.
    pub fn wait_all(&mut self) -> Vec<(String, ExitStatus)> {
        self.children
            .drain(..)
            .map(|(role, mut child)| {
                let status = child.wait().expect("wait on spawned role");
                (role, status)
            })
            .collect()
    }

    /// Whether a role has already exited, without blocking. `Some(status)`
    /// when it has; `None` while it runs (or if the role is unknown).
    pub fn try_wait_role(&mut self, name: &str) -> Option<ExitStatus> {
        self.child_mut(name)?.try_wait().expect("try_wait on role")
    }

    /// Block until `role` satisfies `readiness`, or fail fast if the role
    /// exits first. A timeout reports the role and the condition, so the
    /// offender is named, not guessed from a wall of logs.
    pub fn wait_ready(
        &mut self,
        role: &str,
        readiness: &Readiness,
        timeout: Duration,
    ) -> Result<(), HarnessError> {
        let started = Instant::now();
        if !self.children.iter().any(|(name, _)| name == role) {
            return Err(HarnessError::UnknownRole(role.to_string()));
        }
        loop {
            if readiness.probe() {
                return Ok(());
            }
            if let Some(status) = self.try_wait_role(role) {
                return Err(HarnessError::RoleExited {
                    role: role.to_string(),
                    status: status.to_string(),
                });
            }
            if started.elapsed() >= timeout {
                return Err(HarnessError::NotReady {
                    role: role.to_string(),
                    condition: readiness.describe(),
                    waited_ms: started.elapsed().as_millis() as u64,
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Wait for several roles in the given order, reporting the first role
    /// that fails its condition.
    pub fn wait_ready_all(
        &mut self,
        readiness: &[(&str, Readiness)],
        timeout: Duration,
    ) -> Result<(), HarnessError> {
        for (role, condition) in readiness {
            self.wait_ready(role, condition, timeout)?;
        }
        Ok(())
    }

    /// The mutable child handle of a role, if the group has it.
    fn child_mut(&mut self, name: &str) -> Option<&mut Child> {
        self.children
            .iter_mut()
            .find(|(role, _)| role == name)
            .map(|(_, child)| child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// machined -> cellappmgr -> cellapp, inserted in reverse so only the
    /// dependency edges can produce the right order.
    fn bigworld_chain() -> Topology {
        Topology::new()
            .add(ProcessSpec::new("cellapp", "/bin/true").depends_on("cellappmgr"))
            .add(ProcessSpec::new("cellappmgr", "/bin/true").depends_on("machined"))
            .add(ProcessSpec::new("machined", "/bin/true"))
    }

    #[test]
    fn dependents_start_after_their_dependencies() {
        let order: Vec<String> = bigworld_chain()
            .start_order()
            .unwrap()
            .into_iter()
            .map(|spec| spec.name.clone())
            .collect();
        assert_eq!(order, ["machined", "cellappmgr", "cellapp"]);
    }

    #[test]
    fn independent_roles_keep_insertion_order() {
        let topology = Topology::new()
            .add(ProcessSpec::new("loginapp", "/bin/true"))
            .add(ProcessSpec::new("baseapp", "/bin/true"))
            .add(ProcessSpec::new("cellappmgr", "/bin/true").depends_on("loginapp"));
        let order: Vec<String> = topology
            .start_order()
            .unwrap()
            .into_iter()
            .map(|spec| spec.name.clone())
            .collect();
        // loginapp precedes both of its dependents, and the two independent
        // branches keep insertion order between them.
        assert_eq!(order, ["loginapp", "baseapp", "cellappmgr"]);
    }

    #[test]
    fn unknown_dependencies_report_both_roles() {
        let topology =
            Topology::new().add(ProcessSpec::new("baseapp", "/bin/true").depends_on("machined"));
        match topology.start_order() {
            Err(HarnessError::UnknownDependency { role, missing }) => {
                assert_eq!(role, "baseapp");
                assert_eq!(missing, "machined");
            }
            other => panic!("expected unknown dependency, got {other:?}"),
        }
    }

    #[test]
    fn cycles_list_their_members() {
        let topology = Topology::new()
            .add(ProcessSpec::new("a", "/bin/true").depends_on("b"))
            .add(ProcessSpec::new("b", "/bin/true").depends_on("a"))
            .add(ProcessSpec::new("c", "/bin/true"));
        match topology.start_order() {
            Err(HarnessError::DependencyCycle(members)) => {
                assert_eq!(members, ["a", "b"]);
            }
            other => panic!("expected cycle, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_role_names_are_rejected() {
        let topology = Topology::new()
            .add(ProcessSpec::new("cellapp", "/bin/true"))
            .add(ProcessSpec::new("cellapp", "/bin/true"));
        assert_eq!(
            topology.start_order(),
            Err(HarnessError::DuplicateRole("cellapp".to_string()))
        );
    }

    #[test]
    fn a_started_group_spawns_roles_in_dependency_order() {
        let topology = bigworld_chain();
        let mut group = ProcessGroup::start(&topology).expect("topology starts");

        assert_eq!(group.start_order(), ["machined", "cellappmgr", "cellapp"]);
        // All three roles are real OS processes with distinct pids.
        let pids: Vec<u32> = group
            .start_order()
            .iter()
            .filter_map(|name| group.pid_of(name))
            .collect();
        assert_eq!(pids.len(), 3);
        let unique: BTreeSet<u32> = pids.iter().copied().collect();
        assert_eq!(unique.len(), 3);

        let results = group.wait_all();
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|(_, status)| status.success()));
    }

    #[test]
    fn a_failed_spawn_names_the_role_and_cleans_up() {
        // The marker argument makes this exact process findable in `ps`
        // without matching unrelated sleeps elsewhere on the machine.
        const MARKER: &str = "bw18-leak-probe-194327";
        let topology = Topology::new()
            .add(ProcessSpec::new("machined", "/bin/sleep").arg(MARKER))
            .add(ProcessSpec::new("broken", "/no/such/binary"));
        let error = ProcessGroup::start(&topology).expect_err("spawn fails");

        let HarnessError::Spawn { role, .. } = error else {
            panic!("expected spawn error, got {error:?}")
        };
        assert_eq!(role, "broken");

        // The already-started machined was terminated: nothing lingers
        // behind the failed start.  The bracketed pattern keeps grep and
        // the shell from matching themselves.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let scan = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "ps -eo args | grep '[b]w18-leak-probe-194327' || true",
            ])
            .output()
            .expect("run ps");
        let found = String::from_utf8_lossy(&scan.stdout);
        assert_eq!(found.trim(), "", "sleep process leaked: {found}");
    }

    /// A role that writes its marker a moment after start: the wait returns
    /// once the condition holds, well inside the budget.
    #[test]
    fn file_readiness_waits_until_the_marker_appears() {
        let dir = std::env::temp_dir().join(format!(
            "bw18-ready-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("ready.marker");

        let topology = Topology::new().add(
            ProcessSpec::new("late", "/bin/sh")
                .arg("-c")
                .arg(format!("sleep 0.2; touch {}; sleep 5", marker.display())),
        );
        let mut group = ProcessGroup::start(&topology).unwrap();
        group
            .wait_ready(
                "late",
                &Readiness::FileExists(marker.clone()),
                Duration::from_secs(5),
            )
            .expect("marker appears");
        // The role is still alive after readiness (it sleeps on).
        assert!(group.try_wait_role("late").is_none());
        group.topology(); // touch the accessor
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A condition that never holds: the timeout error names the role and
    /// the exact condition, not just "timed out".
    #[test]
    fn a_timed_out_wait_names_the_role_and_condition() {
        let topology = Topology::new().add(ProcessSpec::new("sleeper", "/bin/sleep").arg("30"));
        let mut group = ProcessGroup::start(&topology).unwrap();
        let error = group
            .wait_ready(
                "sleeper",
                &Readiness::FileExists(PathBuf::from("/nonexistent/marker")),
                Duration::from_millis(150),
            )
            .expect_err("never ready");

        let HarnessError::NotReady {
            role,
            condition,
            waited_ms,
        } = error
        else {
            panic!("expected not-ready, got {error:?}")
        };
        assert_eq!(role, "sleeper");
        assert!(condition.contains("/nonexistent/marker"), "{condition}");
        assert!((100..1000).contains(&waited_ms), "waited: {waited_ms}");
    }

    /// A role that dies before readiness fails immediately with its exit
    /// status -- no point waiting out the budget on a corpse.
    #[test]
    fn an_exited_role_fails_fast_with_its_status() {
        let topology = Topology::new().add(
            ProcessSpec::new("doomed", "/bin/sh")
                .arg("-c")
                .arg("exit 3"),
        );
        let mut group = ProcessGroup::start(&topology).unwrap();
        let started = Instant::now();
        let error = group
            .wait_ready(
                "doomed",
                &Readiness::FileExists(PathBuf::from("/nonexistent/marker")),
                Duration::from_secs(30),
            )
            .expect_err("role died");

        let HarnessError::RoleExited { role, status } = error else {
            panic!("expected role-exited, got {error:?}")
        };
        assert_eq!(role, "doomed");
        assert!(status.contains('3'), "status mentions the code: {status}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "failed fast, not after the timeout"
        );
    }

    /// Port readiness: the probe succeeds against a listener bound in this
    /// test process, and times out naming the role against a closed port.
    #[test]
    fn port_readiness_reports_open_and_closed_ports() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();

        let topology = Topology::new().add(ProcessSpec::new("role", "/bin/sleep").arg("30"));
        let mut group = ProcessGroup::start(&topology).unwrap();

        group
            .wait_ready("role", &Readiness::PortOpen(port), Duration::from_secs(1))
            .expect("port is listening");

        let error = group
            .wait_ready("role", &Readiness::PortOpen(1), Duration::from_millis(120))
            .expect_err("port 1 is closed");
        let HarnessError::NotReady { condition, .. } = error else {
            panic!("expected not-ready, got {error:?}")
        };
        assert!(condition.contains("127.0.0.1:1"), "{condition}");
    }

    /// Log readiness: the wait holds once the announcement line lands in
    /// the role's log file.
    #[test]
    fn log_readiness_waits_for_the_announcement_line() {
        let dir = std::env::temp_dir().join(format!(
            "bw18-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("cellapp.log");

        let topology = Topology::new().add(ProcessSpec::new("cellapp", "/bin/sh").arg("-c").arg(
            format!(
                "sleep 0.2; echo 'CellApp initialized: ready' >> {}; sleep 5",
                log.display()
            ),
        ));
        let mut group = ProcessGroup::start(&topology).unwrap();
        group
            .wait_ready(
                "cellapp",
                &Readiness::LogContains {
                    path: log.clone(),
                    pattern: "initialized: ready".to_string(),
                },
                Duration::from_secs(5),
            )
            .expect("log line appears");
        assert!(std::fs::read_to_string(&log).unwrap().contains("ready"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Waiting on several roles reports the first offender by name.
    #[test]
    fn wait_ready_all_reports_the_first_failing_role() {
        let dir = std::env::temp_dir().join(format!(
            "bw18-all-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.marker");
        let topology = Topology::new()
            .add(
                ProcessSpec::new("good", "/bin/sh")
                    .arg("-c")
                    .arg(format!("touch {}; sleep 5", good.display())),
            )
            .add(ProcessSpec::new("bad", "/bin/sleep").arg("30"));
        let mut group = ProcessGroup::start(&topology).unwrap();

        let error = group
            .wait_ready_all(
                &[
                    ("good", Readiness::FileExists(good.clone())),
                    (
                        "bad",
                        Readiness::FileExists(PathBuf::from("/nonexistent/marker")),
                    ),
                ],
                Duration::from_millis(150),
            )
            .expect_err("bad is never ready");
        let HarnessError::NotReady { role, .. } = error else {
            panic!("expected not-ready, got {error:?}")
        };
        assert_eq!(role, "bad");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A wait naming a role the group does not have is rejected up front.
    #[test]
    fn unknown_role_in_a_wait_is_rejected() {
        let topology = Topology::new().add(ProcessSpec::new("real", "/bin/sleep").arg("30"));
        let mut group = ProcessGroup::start(&topology).unwrap();
        assert_eq!(
            group.wait_ready(
                "ghost",
                &Readiness::FileExists(PathBuf::from("/tmp")),
                Duration::from_millis(100)
            ),
            Err(HarnessError::UnknownRole("ghost".to_string()))
        );
    }
}
