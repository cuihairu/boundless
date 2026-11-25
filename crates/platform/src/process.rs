//! Subprocess control and exit-status decoding (BW-15-01-03,
//! `lib/server/process.hpp`).
//!
//! The reference's `Process` forks and `execvp`s (PATH search), reaps
//! with `waitpid`, and signals with `kill`. Exec failure in the child
//! surfaces as exit code 127, the shell convention, so a server can
//! tell "helper ran and failed" from "helper never ran".
//!
//! Unix uses the reference's POSIX spellings (`fork`/`execvp`/`waitpid`/
//! `kill`); Windows spells the same contract with `CreateProcessW`,
//! `WaitForSingleObject`/`GetExitCodeProcess` and `TerminateProcess`.
//! Both spellings share one API so callers stay platform-agnostic.

#[cfg(unix)]
use std::ffi::CString;
use std::ffi::OsStr;
use std::io;

/// `pid_t` is `i32` on both the unix and the Windows spelling; `libc`
/// is unix-only as a dependency, so the shared field spells it here.
#[cfg(windows)]
#[allow(non_camel_case_types)]
type pid_t = i32;
#[cfg(unix)]
use libc::pid_t;
#[cfg(windows)]
use std::os::raw::c_void;
#[cfg(unix)]
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
    #[cfg(unix)]
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
    pid: pid_t,
    /// The process handle `CreateProcessW` returned, kept for the
    /// lifetime of the child: closing it would let the pid be reused,
    /// and every later call goes through it (windows only).
    #[cfg(windows)]
    handle: *mut c_void,
    /// Whether [`wait`](Child::wait) has already reported the status,
    /// so a second call errors like the unix `ECHILD` (windows only).
    #[cfg(windows)]
    reaped: bool,
}

impl Child {
    /// `fork` + `execvp`, the reference's `Process::create` for a
    /// command line. `argv[0]` is the program, the rest its arguments;
    /// `PATH` is searched. The child `_exit(127)`s when `execvp` fails,
    /// so the parent's `wait` reads 127 instead of hanging on a helper
    /// that never executed.
    #[cfg(unix)]
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

    /// `CreateProcessW` for the same command line: `argv[0]` is the
    /// program, the rest its arguments; with no application name
    /// Windows searches `PATH` for it, matching `execvp`. A launch
    /// failure (no such program) is an error here rather than the
    /// unix 127 exit, because the child never exists to exit.
    #[cfg(windows)]
    pub fn spawn<S: AsRef<OsStr>>(argv: &[S]) -> io::Result<Self> {
        if argv.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "at least the program name is required",
            ));
        }

        let mut command = String::new();
        for (i, arg) in argv.iter().enumerate() {
            if i > 0 {
                command.push(' ');
            }
            command.push_str(&quote_arg(arg.as_ref().to_string_lossy().as_ref()));
        }
        let mut wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();

        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `wide` is NUL-terminated and lives for the call;
        // `CreateProcessW` copies the command line out. The remaining
        // pointers are null/zero as the defaults the reference leaves.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                wide.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                std::ptr::null(),
                &STARTUPINFOW {
                    cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                    ..std::mem::zeroed()
                },
                &mut info,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `h_thread` is a fresh handle we own and need no
        // longer; `h_process` stays in the `Child` and closes on drop.
        unsafe { CloseHandle(info.h_thread) };
        Ok(Child {
            pid: info.dw_process_id as pid_t,
            handle: info.h_process,
            reaped: false,
        })
    }

    /// The child's pid.
    pub fn pid(&self) -> pid_t {
        self.pid
    }

    /// `waitpid(pid, 0)`: block until the child exits. Retries on
    /// `EINTR`; a second wait on the same pid is `ECHILD`.
    #[cfg(unix)]
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

    /// `WaitForSingleObject(handle, INFINITE)` on the handle the spawn
    /// kept; `GetExitCodeProcess` decodes the exit code. A second wait
    /// is `ECHILD`-shaped: the status was already reported once.
    #[cfg(windows)]
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if self.reaped {
            return Err(io::Error::other("child already waited"));
        }
        let result = unsafe { WaitForSingleObject(self.handle, INFINITE) };
        let mut code = 0u32;
        let got = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        if result == WAIT_FAILED || got == 0 {
            return Err(io::Error::last_os_error());
        }
        self.reaped = true;
        Ok(ExitStatus {
            code: Some(code as i32),
            signal: None,
        })
    }

    /// `waitpid(pid, WNOHANG)`: `None` while the child still runs,
    /// `Some(status)` once it has exited.
    #[cfg(unix)]
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

    /// `WaitForSingleObject(handle, 0)`: `None` while the child still
    /// runs, `Some(status)` once it has exited.
    #[cfg(windows)]
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = unsafe { WaitForSingleObject(self.handle, 0) };
        let mut code = 0u32;
        let got = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        if result == WAIT_FAILED || got == 0 {
            return Err(io::Error::last_os_error());
        }
        if result == WAIT_TIMEOUT {
            return Ok(None);
        }
        self.reaped = true;
        Ok(Some(ExitStatus {
            code: Some(code as i32),
            signal: None,
        }))
    }

    /// `kill(pid, sig)`: send a signal to the child.
    #[cfg(unix)]
    pub fn signal(&self, sig: libc::c_int) -> io::Result<()> {
        // SAFETY: `self.pid` is a live child unless already reaped,
        // in which case `kill` reports ESRCH.
        let ret = unsafe { libc::kill(self.pid, sig) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Windows has no signal facility, so the unix `signal(sig)`
    /// contract maps to `TerminateProcess` on the kept handle: the
    /// request means "stop the child", terminated with exit code 1.
    #[cfg(windows)]
    pub fn signal(&self, _sig: std::os::raw::c_int) -> io::Result<()> {
        // SAFETY: `self.handle` is the live process object from the
        // spawn and outlives the call.
        let ok = unsafe { TerminateProcess(self.handle, 1) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: `handle` is the process object the spawn handed us
        // and this is its only close.
        unsafe { CloseHandle(self.handle) };
    }
}

/// Quote one `CreateProcessW` command-line argument the MSVCRT way:
/// backslashes literal before a non-quote, doubled before a quote that
/// itself gets backslashed, and the whole token wrapped in quotes when
/// it holds a space or a quote. The Windows command-line parser inverts
/// these rules, so this is the spelling `argv` survives.
#[cfg(windows)]
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.bytes().any(|b| b == b' ' || b == b'\t' || b == b'"') {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // A quote inside a quoted token is escaped with a
                // backslash, and every pending backslash doubles.
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                out.push(ch);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote double too.
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    out
}

/// `STARTUPINFOW`: the fields `CreateProcessW` reads at construction.
/// Zeroed with `cb` set, matching the reference's defaults.
#[cfg(windows)]
#[repr(C)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
struct STARTUPINFOW {
    cb: u32,
    lp_reserved: *mut u16,
    lp_desktop: *mut u16,
    lp_title: *mut u16,
    dw_x: u32,
    dw_y: u32,
    dw_x_size: u32,
    dw_y_size: u32,
    dw_x_count_chars: u32,
    dw_y_count_chars: u32,
    dw_fill_attribute: u32,
    dw_flags: u32,
    w_show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    h_std_input: *mut c_void,
    h_std_output: *mut c_void,
    h_std_error: *mut c_void,
}

/// `PROCESS_INFORMATION`: the handles and ids `CreateProcessW` returns.
#[cfg(windows)]
#[repr(C)]
#[allow(non_camel_case_types)]
struct PROCESS_INFORMATION {
    h_process: *mut c_void,
    h_thread: *mut c_void,
    dw_process_id: u32,
    dw_thread_id: u32,
}

// The handful of `kernel32` calls behind the Windows `Child`, declared
// the same way as `mmap.rs`'s: a `windows-sys` feature tree for a
// dozen functions is not worth the dependency.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateProcessW(
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *const c_void,
        thread_attributes: *const c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *const c_void,
        current_directory: *const u16,
        startup_info: *const STARTUPINFOW,
        process_information: *mut PROCESS_INFORMATION,
    ) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
    fn TerminateProcess(handle: *mut c_void, exit_code: u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[cfg(windows)]
const INFINITE: u32 = 0xFFFF_FFFF;
#[cfg(windows)]
const WAIT_TIMEOUT: u32 = 0x102;
#[cfg(windows)]
const WAIT_FAILED: u32 = 0xFFFF_FFFF;

#[cfg(test)]
mod tests {
    use super::*;

    /// `spawn` a Bourne shell running `script`, the POSIX way to get a
    /// chosen exit code without a fixture binary.
    #[cfg(unix)]
    fn sh(script: &str) -> Child {
        Child::spawn(&["/bin/sh", "-c", script]).expect("spawn sh")
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn zero_exit_is_success() {
        let mut child = sh("exit 0");
        assert!(child.wait().expect("wait").success());
    }

    #[test]
    #[cfg(unix)]
    fn signal_terminates_the_child() {
        let mut child = sh("sleep 30");
        child.signal(crate::signals::SIGTERM).expect("kill");
        let status = child.wait().expect("wait");
        assert_eq!(status.code, None);
        assert_eq!(status.signal, Some(crate::signals::SIGTERM));
        assert!(!status.success());
    }

    #[test]
    #[cfg(unix)]
    fn try_wait_sees_none_while_alive_then_the_exit() {
        // A one-second window keeps the "still alive" probe reliable
        // under CI load.
        let mut child = sh("sleep 1; exit 3");
        assert_eq!(child.try_wait().expect("try_wait"), None);
        let status = child.wait().expect("wait");
        assert_eq!(status.code, Some(3));
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn wait_after_reap_is_an_error() {
        let mut child = sh("exit 0");
        child.wait().expect("first wait");
        let err = child.wait().expect_err("second wait");
        assert_eq!(err.raw_os_error(), Some(libc::ECHILD));
    }

    /// `spawn` a `cmd` shell running `script`, the Windows way to get a
    /// chosen exit code without a fixture binary.
    #[cfg(windows)]
    fn cmd(script: &str) -> Child {
        Child::spawn(&["cmd", "/d", "/c", script]).expect("spawn cmd")
    }

    #[test]
    #[cfg(windows)]
    fn spawn_and_wait_reports_the_exit_code() {
        let mut child = cmd("exit /b 42");
        let status = child.wait().expect("wait");
        assert_eq!(status.code, Some(42));
        assert_eq!(status.signal, None);
        assert!(!status.success());
    }

    #[test]
    #[cfg(windows)]
    fn zero_exit_is_success() {
        let mut child = cmd("exit /b 0");
        assert!(child.wait().expect("wait").success());
    }

    #[test]
    #[cfg(windows)]
    fn signal_terminates_the_child() {
        // `ping -n 2` sleeps about a second, long enough to terminate.
        let mut child = cmd("ping -n 2 127.0.0.1 >nul");
        child.signal(0).expect("terminate");
        let status = child.wait().expect("wait");
        assert_eq!(status.code, Some(1), "TerminateProcess exit code");
        assert!(!status.success());
    }

    #[test]
    #[cfg(windows)]
    fn try_wait_sees_none_while_alive_then_the_exit() {
        let mut child = cmd("ping -n 2 127.0.0.1 >nul & exit /b 3");
        assert_eq!(child.try_wait().expect("try_wait"), None);
        let status = child.wait().expect("wait");
        assert_eq!(status.code, Some(3));
    }

    #[test]
    #[cfg(windows)]
    fn launch_failure_is_an_error() {
        let err = Child::spawn(&["boundless-no-such-program-xyz"]).expect_err("no such program");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    #[cfg(windows)]
    fn wait_after_reap_is_an_error() {
        let mut child = cmd("exit /b 0");
        child.wait().expect("first wait");
        child.wait().expect_err("second wait");
    }

    #[test]
    #[cfg(windows)]
    fn quoting_survives_spaces_and_quotes() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg("two words"), "\"two words\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        // Trailing backslashes double before the closing quote.
        assert_eq!(quote_arg("dir\\"), "\"dir\\\\\"");
        // Backslashes before an inner quote double plus one escape.
        assert_eq!(quote_arg("a\\\"b"), "\"a\\\\\\\"b\"");
    }
}
