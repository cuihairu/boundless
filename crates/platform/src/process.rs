//! Subprocess control and exit-status decoding (BW-15-01-03,
//! `lib/server/process.hpp`).
//!
//! The reference's `Process` forks and `execvp`s (PATH search), reaps
//! with `waitpid`, and signals with `kill`. Exec failure in the child
//! surfaces as exit code 127, the shell convention, so a server can
//! tell "helper ran and failed" from "helper never ran".
//!
//! Unix only: `fork`/`execvp`/`waitpid`/`kill` are the POSIX spellings
//! the reference uses; Windows `CreateProcess` is BW-15-03.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;

/// A decoded `waitpid` status word: how the child ended.
///
/// Exactly one of `code` and `signal` is `Some`: a child that called
/// `_exit`/`exit` or returned from `main` has a `code`; one a signal
/// killed has a `signal`. The reference decodes the same fields with
/// `WIFEXITED`/`WEXITSTATUS`/`WIFSIGNALED`/`WTERMSIG`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    /// `WEXITSTATUS(status)` when the child exited normally.
    pub code: Option<i32>,
    /// `WTERMSIG(status)` when the child was terminated by a signal.
    pub signal: Option<i32>,
}

impl ExitStatus {
    /// Decode a raw status word returned by `waitpid`.
    fn from_raw(raw: i32) -> Self {
        // The WIF*/W*STATUS decoders are pure bit tests and are safe.
        if libc::WIFEXITED(raw) {
            ExitStatus {
                code: Some(libc::WEXITSTATUS(raw)),
                signal: None,
            }
        } else if libc::WIFSIGNALED(raw) {
            ExitStatus {
                code: None,
                signal: Some(libc::WTERMSIG(raw)),
            }
        } else {
            // WIFSTOPPED/WIFCONTINUED are never reaped as statuses
            // here (we wait with WNOHANG or block), so this is a
            // status word we did not expect.
            ExitStatus {
                code: None,
                signal: None,
            }
        }
    }

    /// Whether the child exited zero. A signal death is not success.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// A child process started by [`Child::spawn`], the port of the
/// reference's `Process` for the lifetime between `fork` and `wait`.
///
/// Call [`wait`](Child::wait) exactly once: the OS reports a status
/// once, so a second wait on the same pid is an error (`ECHILD`).
#[derive(Debug)]
pub struct Child {
    pid: libc::pid_t,
}

impl Child {
    /// `fork` + `execvp`, the reference's `Process::create` for a
    /// command line. `argv[0]` is the program, the rest its arguments;
    /// `PATH` is searched. The child `_exit(127)`s when `execvp` fails,
    /// so the parent's `wait` reads 127 instead of hanging on a helper
    /// that never executed.
    pub fn spawn<S: AsRef<OsStr>>(argv: &[S]) -> io::Result<Self> {
        if argv.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "at least the program name is required",
            ));
        }

        // Build the C argv before forking: heap allocation is not
        // async-signal-safe, so nothing between `fork` and `execvp`
        // may allocate.
        let argv: Vec<CString> = argv
            .iter()
            .map(|arg| {
                CString::new(arg.as_ref().as_bytes()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "argument contains a NUL byte")
                })
            })
            .collect::<io::Result<_>>()?;

        let mut c_argv: Vec<*const libc::c_char> = argv.iter().map(|arg| arg.as_ptr()).collect();
        c_argv.push(std::ptr::null());

        // SAFETY: `c_argv` is NUL-terminated and points at NUL-free
        // CStrings that live long enough for `execvp` (which copies
        // them out) in both the parent and the child copy of the
        // address space.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            // Child: only async-signal-safe calls are legal from here.
            unsafe {
                libc::execvp(c_argv[0], c_argv.as_ptr());
                libc::_exit(127);
            }
        }
        Ok(Child { pid })
    }

    /// The child's pid.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// `waitpid(pid, 0)`: block until the child exits. Retries on
    /// `EINTR`; a second wait on the same pid is `ECHILD`.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let mut raw = 0;
        loop {
            // SAFETY: `raw` is a plain output word; `self.pid` is a
            // live child we have not reaped yet.
            let ret = unsafe { libc::waitpid(self.pid, &mut raw, 0) };
            if ret == self.pid {
                return Ok(ExitStatus::from_raw(raw));
            }
            if ret < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            // `waitpid` without `WNOHANG` returns the child pid or -1;
            // 0 is not a valid outcome, so do not spin on it.
            return Err(io::Error::other("waitpid returned 0"));
        }
    }

    /// `waitpid(pid, WNOHANG)`: `None` while the child still runs,
    /// `Some(status)` once it has exited.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let mut raw = 0;
        // SAFETY: as in `wait`.
        let ret = unsafe { libc::waitpid(self.pid, &mut raw, libc::WNOHANG) };
        if ret == 0 {
            return Ok(None);
        }
        if ret == self.pid {
            return Ok(Some(ExitStatus::from_raw(raw)));
        }
        if ret < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                // EINTR: nothing was reaped, report "still running".
                return Ok(None);
            }
            return Err(err);
        }
        Err(io::Error::other("waitpid returned an unexpected pid"))
    }

    /// `kill(pid, sig)`: send a signal to the child.
    pub fn signal(&self, sig: libc::c_int) -> io::Result<()> {
        // SAFETY: `self.pid` is a live child unless already reaped,
        // in which case `kill` reports ESRCH.
        let ret = unsafe { libc::kill(self.pid, sig) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `spawn` a Bourne shell running `script`, the POSIX way to get a
    /// chosen exit code without a fixture binary.
    fn sh(script: &str) -> Child {
        Child::spawn(&["/bin/sh", "-c", script]).expect("spawn sh")
    }

    #[test]
    fn spawn_and_wait_reports_the_exit_code() {
        let mut child = sh("exit 42");
        let status = child.wait().expect("wait");
        assert_eq!(
            status,
            ExitStatus {
                code: Some(42),
                signal: None,
            }
        );
        assert!(!status.success());
    }

    #[test]
    fn zero_exit_is_success() {
        let mut child = sh("exit 0");
        assert!(child.wait().expect("wait").success());
    }

    #[test]
    fn signal_terminates_the_child() {
        let mut child = sh("sleep 30");
        child.signal(libc::SIGTERM).expect("kill");
        let status = child.wait().expect("wait");
        assert_eq!(status.code, None);
        assert_eq!(status.signal, Some(libc::SIGTERM));
        assert!(!status.success());
    }

    #[test]
    fn try_wait_sees_none_while_alive_then_the_exit() {
        // A one-second window keeps the "still alive" probe reliable
        // under CI load.
        let mut child = sh("sleep 1; exit 3");
        assert_eq!(child.try_wait().expect("try_wait"), None);
        let status = child.wait().expect("wait");
        assert_eq!(status.code, Some(3));
    }

    #[test]
    fn exec_failure_surfaces_as_exit_code_127() {
        let mut child = Child::spawn(&["/no/such/program"]).expect("fork");
        let status = child.wait().expect("wait");
        assert_eq!(status.code, Some(127));
        assert_eq!(status.signal, None);
    }

    #[test]
    fn argv_needs_at_least_the_program() {
        let err = Child::spawn::<&str>(&[]).expect_err("empty argv");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn wait_after_reap_is_an_error() {
        let mut child = sh("exit 0");
        child.wait().expect("first wait");
        let err = child.wait().expect_err("second wait");
        assert_eq!(err.raw_os_error(), Some(libc::ECHILD));
    }
}
