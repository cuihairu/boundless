//! BW-15-01-03 the shutdown wiring: run the server topology as child
//! processes and stop it by signal -> kill -> wait.
//!
//! `platform::process` carries the primitive faces (`Process::create`
//! is `fork`+`execvp`, then `kill(pid, sig)` and `waitpid` with the
//! `WIF*`/`W*STATUS` decoding -- `lib/server/process.hpp`). This module
//! is the server-runtime wiring over them the acceptance names: the
//! supervisor spawns the topology's entries as children, and on the way
//! down it signals every child and reaps each one, collecting the exit
//! codes. The three-platform half of the acceptance stays open with the
//! Windows `CreateProcess` backend (BW-15-03).
//!
//! The stop path reuses the role entry's own: each child receives the
//! same signal machined commands for a controlled shutdown -- SIGUSR1
//! (`SignalMessage::setControlledShutdown`,
//! `machine_guard.hpp:714`) -- and each role's `app::run` walks its
//! controlled-stop arm into the shutdown tail with exit 0. The drop
//! path takes the vocabulary's kill signal instead (SIGINT, `setKill`
//! `machine_guard.hpp:715`).

use crate::signals::{Blocker, Set, SignalProcessor};
use platform::process::{Child, ExitStatus};
use std::ffi::OsStr;
use std::io;

/// One topology member: its name, its process, and the exit status the
/// supervisor already collected if a probe reaped it. A reaped child
/// cannot be waited again (`ECHILD`), so its status is remembered and
/// reported on the way down.
struct ChildEntry {
    name: String,
    child: Child,
    probed: Option<ExitStatus>,
}

/// A running server topology: every child process the supervisor
/// spawned, in start order. `stop` reports the same order, so a
/// shutdown report lines up with the entries it was built from.
pub struct Topology {
    children: Vec<ChildEntry>,
}

impl Topology {
    /// Spawn every entry the way the reference machine starts its
    /// configured processes: one `fork`+`execvp` (`Process::create`) per
    /// entry. An entry is a name plus its argv; `argv[0]` is the program
    /// and `PATH` is searched. A failed spawn aborts the start; the
    /// children already spawned are killed and reaped on the way out
    /// (`Drop`), so a topology is either fully up or not at all.
    pub fn start<S: AsRef<OsStr>>(entries: &[(String, Vec<S>)]) -> io::Result<Self> {
        let mut topology = Topology {
            children: Vec::with_capacity(entries.len()),
        };
        for (name, argv) in entries {
            let child = Child::spawn(argv)?;
            topology.children.push(ChildEntry {
                name: name.clone(),
                child,
                probed: None,
            });
        }
        Ok(topology)
    }

    /// How many children the topology holds.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether the topology holds no children.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Every child's name and pid, in start order.
    pub fn pids(&self) -> Vec<(String, libc::pid_t)> {
        self.children
            .iter()
            .map(|entry| (entry.name.clone(), entry.child.pid()))
            .collect()
    }

    /// `waitpid(pid, WNOHANG)` for the named child: `None` while it
    /// still runs, `Some(status)` once it has exited. The reference
    /// machine polls its children the same non-blocking way between
    /// its own work. The probe that sees the exit also reaps it, so
    /// the status is remembered for the shutdown report; a missing name
    /// is `None`.
    pub fn probe(&mut self, name: &str) -> io::Result<Option<ExitStatus>> {
        for entry in &mut self.children {
            if entry.name == name {
                if let Some(status) = entry.probed {
                    return Ok(Some(status));
                }
                let status = entry.child.try_wait()?;
                if status.is_some() {
                    entry.probed = status;
                }
                return Ok(status);
            }
        }
        Ok(None)
    }

    /// Stop the topology: signal every child with the controlled-stop
    /// signal (SIGUSR1, the `setControlledShutdown` vocabulary
    /// `machine_guard.hpp:714`), then reap each one with `waitpid`.
    /// The signal reaches all children before the first wait, so the
    /// topology shuts down as a group; the report holds each entry's
    /// decoded exit status in start order. A child that already exited
    /// on its own is still signalled and reaped the same way -- the wait
    /// reports its real status; a child a probe already reaped reports
    /// the remembered status instead.
    ///
    /// A `kill` failure (the child is already reaped) does not skip the
    /// wait; a `wait` failure on one child still reaps the rest and
    /// surfaces the first error afterwards.
    pub fn stop(mut self) -> io::Result<Vec<(String, ExitStatus)>> {
        for entry in &self.children {
            // The controlled-stop signal: what machined commands for a
            // managed shutdown, and what `app::run`'s SIGUSR1 arm walks
            // into the shutdown tail. A probed child is already reaped:
            // nothing to signal.
            if entry.probed.is_none() {
                let _ = entry.child.signal(platform::signals::SIGUSR1);
            }
        }
        let mut report: Vec<(String, ExitStatus)> = Vec::with_capacity(self.children.len());
        let mut first_err: Option<io::Error> = None;
        for ChildEntry {
            name,
            mut child,
            probed,
        } in self.children.drain(..)
        {
            let status = match probed {
                Some(status) => status,
                None => match child.wait() {
                    Ok(status) => status,
                    Err(err) => {
                        first_err.get_or_insert(err);
                        continue;
                    }
                },
            };
            report.push((name, status));
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(report),
        }
    }

    /// The wiring face of BW-15-01-03: hold the quit set, wait for one
    /// arrival (`SignalProcessor::wait_for_signals`, the supervisor's
    /// own end -- the roles end through their `app::run` arms instead),
    /// then [`stop`](Topology::stop) the topology with the controlled
    /// stop. The returned report is the exit codes collected on the way
    /// down.
    ///
    /// Unix only, like the whole stop path: the signal wait is the
    /// POSIX spelling; the Windows console-control stop is BW-15-03.
    pub fn run(self) -> io::Result<Vec<(String, ExitStatus)>> {
        eprintln!("[topology] starting");
        let set = Set::quit();
        let _blocker = Blocker::new(&set);
        eprintln!("[topology] running ({} processes)", self.len());
        let mut processor = SignalProcessor::new();
        let sig = processor.wait_for_signals(&set);
        if sig < 0 {
            // Nothing was delivered, so the run ends rather than spins.
            eprintln!("[topology] signal wait ended without a signal");
        }
        let report = self.stop()?;
        eprintln!("[topology] has shut down.");
        Ok(report)
    }
}

impl Drop for Topology {
    /// A topology dropped without `stop` still signals and reaps every
    /// child, on the kill signal (`setKill`, `machine_guard.hpp:715`
    /// SIGINT): the supervisor is ending without a managed shutdown, so
    /// the children are told to die, not to run their shutdown tails.
    /// The reference machine does not leave server processes behind when
    /// its own process ends, so neither does the port. Best effort only
    /// -- there is nowhere left to report an error.
    fn drop(&mut self) {
        for entry in &self.children {
            if entry.probed.is_none() {
                let _ = entry.child.signal(platform::signals::SIGINT);
            }
        }
        for ChildEntry {
            name: _,
            mut child,
            probed,
        } in self.children.drain(..)
        {
            // A probed child is already reaped; the rest are waited.
            if probed.is_none() {
                let _ = child.wait();
            }
        }
    }
}

/// [`Topology::start`] plus [`Topology::run`](Topology::run): start the
/// entries, wait for a quit signal, and return the shutdown report.
pub fn run_topology<S: AsRef<OsStr>>(
    entries: &[(String, Vec<S>)],
) -> io::Result<Vec<(String, ExitStatus)>> {
    Topology::start(entries)?.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A topology entry that sleeps: a live child for the whole test,
    /// like a running server role.
    fn sleeper(name: &str) -> (String, Vec<&'static str>) {
        (name.to_string(), vec!["/bin/sh", "-c", "sleep 30"])
    }

    /// A topology entry that exits with `code` on its own.
    fn exiter(name: &str, code: i32) -> (String, Vec<&'static str>) {
        let script: &'static str = match code {
            0 => "exit 0",
            _ => "exit 7",
        };
        (name.to_string(), vec!["/bin/sh", "-c", script])
    }

    /// Poll `probe` until `name` has exited, bounded: the non-blocking
    /// way the reference machine watches its children between its own
    /// work.
    fn wait_for_exit(topology: &mut Topology, name: &str) -> ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(status) = topology.probe(name).expect("probe") {
                return status;
            }
            assert!(
                std::time::Instant::now() <= deadline,
                "{name} did not exit on its own"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn start_runs_every_entry_as_a_live_child() {
        let entries = [sleeper("baseapp"), sleeper("cellapp")];
        let mut topology = Topology::start(&entries).expect("start");
        assert_eq!(topology.len(), 2);
        assert!(!topology.is_empty());

        // Every entry is a distinct live process.
        let pids = topology.pids();
        assert_eq!(pids.len(), 2);
        assert_eq!(pids[0].0, "baseapp");
        assert_eq!(pids[1].0, "cellapp");
        assert_ne!(pids[0].1, pids[1].1);
        for (name, _) in &pids {
            let status = topology.probe(name).expect("probe");
            assert_eq!(status, None, "{name} must still be running");
        }
    }

    #[test]
    fn probe_of_a_missing_name_is_none() {
        let entries = [exiter("baseapp", 0)];
        let mut topology = Topology::start(&entries).expect("start");
        assert_eq!(topology.probe("cellapp").expect("probe"), None);
        topology.stop().expect("stop");
    }

    #[test]
    fn stop_signals_and_reaps_every_child() {
        let entries = [sleeper("baseapp"), sleeper("cellapp")];
        let topology = Topology::start(&entries).expect("start");
        let pids = topology.pids();

        let report = topology.stop().expect("stop");
        assert_eq!(report.len(), 2, "every entry is reported");
        for (name, status) in &report {
            // The stop signal killed them, not an exit code.
            assert_eq!(status.code, None, "{name} exited on its own");
            assert_eq!(status.signal, Some(platform::signals::SIGUSR1), "{name}");
        }
        // The report keeps the start order.
        assert_eq!(report[0].0, "baseapp");
        assert_eq!(report[1].0, "cellapp");

        // Reaped: a zombie would still own a /proc entry. Checked on
        // Linux only, where /proc is the process table.
        #[cfg(target_os = "linux")]
        for (_, pid) in pids {
            assert!(
                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "pid {pid} was not reaped"
            );
        }
    }

    #[test]
    fn stop_collects_each_childs_exit_code() {
        let entries = [exiter("dbapp", 0), exiter("loginapp", 7)];
        let mut topology = Topology::start(&entries).expect("start");
        // Both children leave before the stop, the same way a role
        // that finished its own work does.
        wait_for_exit(&mut topology, "dbapp");
        wait_for_exit(&mut topology, "loginapp");

        let report = topology.stop().expect("stop");
        let codes: Vec<(&str, Option<i32>)> = report
            .iter()
            .map(|(name, status)| (name.as_str(), status.code))
            .collect();
        assert_eq!(
            codes,
            vec![("dbapp", Some(0)), ("loginapp", Some(7))],
            "the report carries each child's exit code"
        );
    }

    #[test]
    fn stop_mixes_own_exits_with_signalled_children() {
        // A topology where one child already ended on its own and one
        // still runs: every entry is still reported, and each report
        // entry says which way it ended.
        let entries = [exiter("reviver", 0), sleeper("baseappmgr")];
        let mut topology = Topology::start(&entries).expect("start");
        let reviver = wait_for_exit(&mut topology, "reviver");
        assert_eq!(reviver.code, Some(0));

        let report = topology.stop().expect("stop");
        assert_eq!(report[0].0, "reviver");
        assert_eq!(report[0].1.code, Some(0));
        assert_eq!(report[1].0, "baseappmgr");
        assert_eq!(report[1].1.signal, Some(platform::signals::SIGUSR1));
    }

    #[test]
    fn a_dropped_topology_kills_and_reaps_its_children() {
        let entries = [sleeper("baseapp"), sleeper("cellapp")];
        let topology = Topology::start(&entries).expect("start");
        let pids = topology.pids();
        drop(topology);

        // The children leave the process table once killed and reaped.
        // On Linux /proc shows a zombie until its parent reaps it, so
        // the entry disappearing is the kill-plus-reap evidence.
        #[cfg(target_os = "linux")]
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let all_gone = pids
                    .iter()
                    .all(|(_, pid)| !std::path::Path::new(&format!("/proc/{pid}")).exists());
                if all_gone || std::time::Instant::now() > deadline {
                    assert!(all_gone, "children outlived the topology: {pids:?}");
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        #[cfg(not(target_os = "linux"))]
        drop(pids);
    }

    #[test]
    fn a_failed_spawn_kills_the_children_it_started() {
        // A valid entry followed by a rejected one: the start fails,
        // and the child it already spawned is killed and reaped rather
        // than left running with no supervisor. The child reports its
        // own pid to a file, so the cleanup is observable when the
        // child reaches its script before the kill does.
        let pidfile =
            std::env::temp_dir().join(format!("topology-start-fail-{}.pid", std::process::id()));
        let pidfile = pidfile.to_str().expect("a printable temp path");
        let entries = vec![
            (
                String::from("baseapp"),
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    format!("echo $$ >'{pidfile}'; sleep 30"),
                ],
            ),
            // A NUL byte in argv is rejected before the second fork.
            (String::from("cellapp"), vec!["/bin/\0sh".to_string()]),
        ];
        let err = match Topology::start(&entries) {
            Ok(_) => panic!("the second entry cannot start"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");

        #[cfg(target_os = "linux")]
        {
            // The rejected entry fails immediately, so the first child
            // may be killed before it execs and never writes the file;
            // that outcome left nothing behind either. When the file
            // did appear, the pid must be killed and reaped.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let pid: Option<i32> = loop {
                if let Ok(text) = std::fs::read_to_string(pidfile) {
                    break Some(text.trim().parse::<i32>().expect("a pid"));
                }
                if std::time::Instant::now() > deadline {
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            };
            if let Some(pid) = pid {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let gone = !std::path::Path::new(&format!("/proc/{pid}")).exists();
                    if gone || std::time::Instant::now() > deadline {
                        assert!(gone, "the first child outlived the failed start: {pid}");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            let _ = std::fs::remove_file(pidfile);
        }
    }

    #[test]
    fn run_topology_waits_for_a_signal_then_stops() {
        // The wiring face: `run_topology` blocks on the quit set, a
        // SIGTERM to its thread ends the wait, and the return carries
        // the stop report. The quit set is held in the worker before
        // the watchdog can fire, so the signal can only pend and be
        // consumed -- it never races the blocker setup.
        use platform::signals::{thread_kill, thread_self};

        let handle = std::thread::spawn(move || {
            let entries = [sleeper("baseapp")];
            let _early = Blocker::new(&Set::quit());
            let target = thread_self();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let _ = thread_kill(target, platform::signals::SIGTERM);
            });
            run_topology(&entries).expect("run")
        });

        let report = handle.join().expect("run thread");
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].0, "baseapp");
        // The supervisor's own end is the quit signal; the child's end
        // is the controlled stop the stop path commands.
        assert_eq!(report[0].1.signal, Some(platform::signals::SIGUSR1));
    }
}
