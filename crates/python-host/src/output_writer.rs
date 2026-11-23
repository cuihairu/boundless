//! `ScriptOutputWriter` and `PyOutputStream`
//! (`lib/script/py_script_output_writer.cpp`) plus the `printTraceBack`
//! excepthook (`lib/pyscript/py_traceback.cpp`).
//!
//! The reference constructs a `ScriptOutputWriter` right after `Py_Initialize`
//! (`script.cpp:531`); it swaps `sys.stdout`/`sys.stderr` for `PyOutputStream`
//! objects whose `write` lands the text in a per-stream buffer, and once a
//! write ends with a newline every complete line goes to the logging system
//! (`MESSAGE_PRIORITY_ERROR` for stderr, `MESSAGE_PRIORITY_INFO` with
//! `MESSAGE_SOURCE_SCRIPT` for stdout) and then to every `ScriptOutputHook`
//! with the raw chunk. Hooks can remove themselves from inside their own
//! callback.
//!
//! `initExceptionHook` (`py_traceback.cpp:575`) publishes a `printTraceBack`
//! builtin as `sys.excepthook`. It prints the standard traceback header and
//! frames through `PySys_WriteStderr` -- which is the hooked `sys.stderr`, so
//! the text lands in the log -- and finishes with `PyErr_Display`. The
//! reference reads each frame's source line through the event dispatcher
//! without blocking; this port reads it synchronously, which is the same
//! output without a dispatcher to defer on.

use super::capi;
use std::ffi::CString;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The reference's `ScriptOutputHook::onScriptOutput`.
pub trait ScriptOutputHook: Send {
    /// `onScriptOutput`: the raw chunk written to stdout or stderr, before
    /// line buffering.
    fn on_script_output(&mut self, output: &str, is_stderr: bool);
}

/// Any `FnMut(&str, bool)` is a hook, which is what the tests and the host
/// glue use.
impl<F: FnMut(&str, bool) + Send> ScriptOutputHook for F {
    fn on_script_output(&mut self, output: &str, is_stderr: bool) {
        self(output, is_stderr)
    }
}

/// The log entry the lines land in: `line` has its trailing newline
/// stripped, `is_stderr` selects the error level over the info level.
///
/// The reference writes each line through `LogMsg` with
/// `MESSAGE_SOURCE_SCRIPT`; the port hands the line to the sink the host
/// installed, and the default sink writes it to the process stderr the way
/// the reference's console handler does.
pub type LogSink = Box<dyn Fn(&str, bool) + Send>;

/// The default sink: the reference's console `LogMsg` handler writes the
/// line to stderr either way, the level only tagging the entry.
fn console_sink(line: &str, _is_stderr: bool) {
    eprintln!("{line}");
}

/// One attached hook, with the stable identity the reference gets from the
/// pointer: `delHook` needs to recognise "the hook that is running now".
struct HookEntry {
    id: u64,
    hook: Box<dyn ScriptOutputHook>,
}

/// `ScriptOutputWriter`: buffers, hooks, and the sink for one interpreter.
struct WriterState {
    stdout_buffer: String,
    stderr_buffer: String,
    hooks: Vec<HookEntry>,
    next_hook_id: u64,
    sink: LogSink,
}

impl WriterState {
    fn new() -> Self {
        Self {
            stdout_buffer: String::new(),
            stderr_buffer: String::new(),
            hooks: Vec::new(),
            next_hook_id: 0,
            sink: Box::new(console_sink),
        }
    }
}

thread_local! {
    /// The hook whose callback is running on this thread: the reference's
    /// `pCurrentHook_`. Hooks run on the GIL thread only, so a thread-local
    /// is exactly the right scope, and it lets a self-removing hook reach
    /// `remove_hook` while `handle_write` already holds the writer lock --
    /// the reference needs no lock at all because it assumes the GIL.
    static CURRENT_HOOK: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// The self-removal request: the reference's `shouldDeleteCurrentHook_`.
    static DELETE_CURRENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

static WRITER: OnceLock<Mutex<WriterState>> = OnceLock::new();

fn writer() -> MutexGuard<'static, WriterState> {
    WRITER
        .get_or_init(|| Mutex::new(WriterState::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether `Script::init` installed the stdio redirect: the port's
/// equivalent of `ScriptOutputWriter::getCurrentStdio() != NULL`, which is
/// what the reference's hook latching goes through.
pub fn stdio_is_redirected() -> bool {
    WRITER.get().is_some() && super::interpreter::Interpreter::is_initialized()
}

/// The reference's `addHook`, on the current stdio writer. Returns the hook
/// identity `remove_hook` takes.
///
/// A hook added twice receives messages twice, exactly as the reference
/// documents.
pub fn add_hook(hook: Box<dyn ScriptOutputHook>) -> Result<u64, HookError> {
    if !stdio_is_redirected() {
        return Err(HookError::NoStdioWriter);
    }
    let mut writer = writer();
    let id = writer.next_hook_id;
    writer.next_hook_id += 1;
    writer.hooks.push(HookEntry { id, hook });
    Ok(id)
}

/// The reference's `delHook`. A hook added twice must be removed twice.
/// Removing the hook that is currently being called, from inside its own
/// callback, is allowed: the removal takes effect after the callback
/// returns.
pub fn remove_hook(id: u64) -> Result<(), HookError> {
    // Reentrant call from inside a callback: the writer lock is already
    // held by this thread's `handle_write`, so only the running hook can
    // act, and its removal waits for the callback to return.
    if let Some(running) = CURRENT_HOOK.get() {
        if running == id {
            DELETE_CURRENT.set(true);
            return Ok(());
        }
        return Err(HookError::NoSuchHook);
    }
    let mut writer = writer();
    let Some(index) = writer.hooks.iter().position(|entry| entry.id == id) else {
        return Err(HookError::NoSuchHook);
    };
    writer.hooks.remove(index);
    Ok(())
}

/// Why a hook could not be attached or removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookError {
    /// `getCurrentStdio()` found no `PyOutputStream`: the redirect is not
    /// installed.
    NoStdioWriter,
    /// `delHook` on a hook that is not attached.
    NoSuchHook,
}

/// Replace the log entry the lines land in. The reference hard-wires
/// `LogMsg`; the port lets the host process name its own entry point and
/// keeps this console writer as the default.
pub fn set_log_sink(sink: impl Fn(&str, bool) + Send + 'static) {
    writer().sink = Box::new(sink);
}

/// `ScriptOutputWriter::handleWrite`, the private half of `py_write`.
pub fn handle_write(msg: &str, is_stderr: bool) {
    let mut writer = writer();
    let complete = {
        let buffer = if is_stderr {
            &mut writer.stderr_buffer
        } else {
            &mut writer.stdout_buffer
        };
        buffer.push_str(msg);
        if buffer.ends_with('\n') {
            Some(std::mem::take(buffer))
        } else {
            None
        }
    };
    if let Some(mut lines) = complete {
        // The reference strips the trailing newline before splitting, so
        // the time-prefixed log format keeps its placeholder.
        lines.pop();
        for line in lines.split('\n') {
            (writer.sink)(line, is_stderr);
        }
    }
    // Hooks receive the raw chunk, not the split lines, and run after the
    // log write. The reference asserts `pCurrentHook_ == NULL` here: writes
    // do not nest.
    run_hooks(&mut writer, msg, is_stderr);
}

fn run_hooks(writer: &mut WriterState, msg: &str, is_stderr: bool) {
    let mut index = 0;
    while index < writer.hooks.len() {
        let id = writer.hooks[index].id;
        CURRENT_HOOK.set(Some(id));
        writer.hooks[index].hook.on_script_output(msg, is_stderr);
        CURRENT_HOOK.set(None);
        if DELETE_CURRENT.replace(false) {
            // The hook removed itself: drop it without advancing, so the
            // next hook shifts into this slot.
            writer.hooks.remove(index);
        } else {
            index += 1;
        }
    }
}

/// `PyOutputStream::py_write`: the `write` method CPython calls.
///
/// # Safety
///
/// CPython calls this with the arguments tuple of a `write(str)` call; the
/// `self_` pointer is the flag object `PyCFunction_New` was given (`Py_True`
/// for the stderr stream, `Py_False` for stdout).
pub unsafe extern "C" fn py_write(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    unsafe {
        let mut msg: *const std::os::raw::c_char = std::ptr::null();
        let mut msg_len: capi::PySsize = 0;
        if capi::PyArg_ParseTuple(args, c"s#:write".as_ptr(), &mut msg, &mut msg_len) == 0 {
            // Bad args: propagate the exception, like the reference.
            return std::ptr::null_mut();
        }
        let bytes = std::slice::from_raw_parts(msg as *const u8, msg_len.max(0) as usize);
        if bytes.contains(&0) {
            // Embedded NUL: not UTF-8, log the repr instead, as the
            // reference does with `PyObject_Repr` on the argument.
            let item = capi::PyTuple_GetItem(args, 0);
            let repr = capi::PyObject_Repr(item);
            if repr.is_null() {
                return std::ptr::null_mut();
            }
            let text = capi::to_string(repr);
            capi::dec_ref(repr);
            handle_write(&text, self_ == capi::py_true());
        } else {
            // The reference carries the bytes verbatim; a non-UTF-8 chunk
            // degrades here, recorded with the script-logging item.
            let text = String::from_utf8_lossy(bytes);
            handle_write(&text, self_ == capi::py_true());
        }
        capi::py_none()
    }
}

/// `initExceptionHook`'s `printTraceBack`: the `sys.excepthook`.
///
/// # Safety
///
/// CPython calls this with the three-object arguments tuple the excepthook
/// contract provides (type, value, traceback); `self_` is always null.
pub unsafe extern "C" fn print_trace_back(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    unsafe {
        let _ = self_;
        let mut exc_type: *mut capi::PyObject = std::ptr::null_mut();
        let mut exc_value: *mut capi::PyObject = std::ptr::null_mut();
        let mut exc_traceback: *mut capi::PyObject = std::ptr::null_mut();
        if capi::PyArg_ParseTuple(
            args,
            c"OOO:printTraceBack".as_ptr(),
            &mut exc_type,
            &mut exc_value,
            &mut exc_traceback,
        ) == 0
        {
            // `ERROR_MSG` + `PyErr_Clear`, then `Py_RETURN_NONE`.
            capi::clear_error();
            return capi::py_none();
        }
        if !capi::py_traceback_check(exc_traceback) {
            // No usable traceback: the default handler renders the
            // exception alone, exactly as the reference does.
            capi::PyErr_Display(exc_type, exc_value, exc_traceback);
            return capi::py_none();
        }
        capi::PySys_WriteStderr(c"Traceback (most recent call last):\n".as_ptr());
        // Frames walk outermost first through `tb_next`. The first
        // traceback is borrowed from the arguments tuple; every `tb_next`
        // read is a fresh reference released by the next pass.
        let mut owned: *mut capi::PyObject = std::ptr::null_mut();
        let mut frame = exc_traceback;
        while !frame.is_null() {
            write_frame(frame);
            let next = getattr_or_null(frame, c"tb_next");
            capi::dec_ref(owned);
            owned = next;
            frame = next;
        }
        // The reference passes NULL for the traceback here so the display
        // stops after the exception line the frames already covered.
        capi::PyErr_Display(exc_type, exc_value, std::ptr::null_mut());
        capi::py_none()
    }
}

/// One `  File "...", line N, in name` entry plus the source line when the
/// file can be read.
unsafe fn write_frame(frame: *mut capi::PyObject) {
    unsafe {
        let Some(lineno) =
            attr_to_string(frame, c"tb_lineno").and_then(|text| text.parse::<u32>().ok())
        else {
            return;
        };
        let Some(tb_frame) = non_null(getattr_or_null(frame, c"tb_frame")) else {
            return;
        };
        let code = getattr_or_null(tb_frame, c"f_code");
        capi::dec_ref(tb_frame);
        let Some(code_object) = non_null(code) else {
            return;
        };
        let filename = attr_to_string(code_object, c"co_filename").unwrap_or_default();
        let name = attr_to_string(code_object, c"co_name").unwrap_or_default();
        capi::dec_ref(code_object);
        // The reference resolves res-relative names through `BWResource`;
        // import machinery names here are sys.path entries joined with the
        // module name, which are absolute whenever the res roots are.
        let source_line = read_source_line(&filename, lineno);
        let text = match source_line {
            Some(line) => format!("  File \"{filename}\", line {lineno}, in {name}\n    {line}\n"),
            None => format!("  File \"{filename}\", line {lineno}, in {name}\n"),
        };
        if let Ok(encoded) = CString::new(text) {
            capi::PySys_WriteStderr(c"%s".as_ptr(), encoded.as_ptr());
        }
    }
}

/// A raw pointer as an `Option`, so the failure arms read linearly.
fn non_null(pointer: *mut capi::PyObject) -> Option<*mut capi::PyObject> {
    (!pointer.is_null()).then_some(pointer)
}

/// The frame's source line with its leading whitespace stripped, or `None`
/// when the file is missing or the line is out of range -- the reference
/// skips the line in exactly those cases.
fn read_source_line(filename: &str, lineno: u32) -> Option<String> {
    if filename.is_empty() || lineno == 0 {
        return None;
    }
    let bytes = std::fs::read(filename).ok()?;
    let line = bytes
        .split(|byte| *byte == b'\n')
        .nth(lineno as usize - 1)?;
    let stripped: &[u8] = {
        let mut start = 0;
        while start < line.len() && matches!(line[start], b' ' | b'\t' | 0x0c) {
            start += 1;
        }
        &line[start..]
    };
    if stripped.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(stripped).into_owned())
}

/// An attribute read back through `str()`, or `None` when the attribute is
/// missing or fails to render; every intermediate reference is released.
unsafe fn attr_to_string(object: *mut capi::PyObject, name: &std::ffi::CStr) -> Option<String> {
    unsafe {
        let attr = non_null(getattr_or_null(object, name))?;
        let text = capi::PyObject_Str(attr);
        capi::dec_ref(attr);
        if text.is_null() {
            capi::clear_error();
            return None;
        }
        let rendered = capi::to_string(text);
        capi::dec_ref(text);
        Some(rendered)
    }
}

/// `PyObject_GetAttrString` that leaks nothing on failure: the caller never
/// has to distinguish "no such attribute" from "attribute is None".
unsafe fn getattr_or_null(
    object: *mut capi::PyObject,
    name: &std::ffi::CStr,
) -> *mut capi::PyObject {
    unsafe {
        let result = capi::PyObject_GetAttrString(object, name.as_ptr());
        if result.is_null() {
            capi::clear_error();
        }
        result
    }
}

/// `ScriptOutputWriter`'s constructor: build the two `PyOutputStream`
/// instances and hand them to `sys.stdout`/`sys.stderr`.
///
/// The reference instantiates a C heap type; the port defines the same
/// class -- `write` plus a read-write `softspace` -- in a bootstrap script
/// and binds the host's `write` builtin into each instance, which keeps the
/// whole `PyType_FromSpec` slot machinery out of the FFI surface while the
/// observable behaviour (`write` reaching the log entry, `softspace`
/// present) stays the same.
///
/// A no-op `flush` is added because the CPython this port embeds finalises
/// `sys.stdout` on interpreter shutdown (the reference's older Python did
/// not), and the stdout protocol requires a `flush` method; the line
/// buffering in `handle_write` already delivers complete lines, so a no-op
/// flush matches the reference's "only complete lines reach the log".
///
/// The flag each `write` builtin carries (`Py_True` for stderr, `Py_False`
/// for stdout) plays the reference's `isStderr_` member.
pub fn install() -> bool {
    // `MF_ASSERT( Py_IsInitialized() )` in the reference aborts the process;
    // the port reports the failure so a test can assert it.
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return false;
    }
    // A fresh writer per init: buffers empty, no hooks -- the reference's
    // hooks are re-added by the caller after `createInterpreter` too -- but
    // the log entry the host chose stays.
    clear_writer();
    let Ok(bootstrap) = CString::new(
        r#"
import sys
class PyOutputStream:
    def __init__(self, write):
        self.write = write
        self.softspace = False
    def flush(self):
        pass
sys.stdout = PyOutputStream(sys._bw_stdout_write)
sys.stderr = PyOutputStream(sys._bw_stderr_write)
del PyOutputStream
"#,
    ) else {
        return false;
    };
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let stdout_write =
            capi::PyCFunction_New(std::ptr::addr_of_mut!(PY_WRITE_DEF), capi::py_false());
        let stderr_write =
            capi::PyCFunction_New(std::ptr::addr_of_mut!(PY_WRITE_DEF), capi::py_true());
        let sys = capi::PyImport_AddModule(c"sys".as_ptr());
        let attr_ok = !stdout_write.is_null()
            && !stderr_write.is_null()
            && !sys.is_null()
            && capi::PyObject_SetAttrString(sys, c"_bw_stdout_write".as_ptr(), stdout_write) == 0
            && capi::PyObject_SetAttrString(sys, c"_bw_stderr_write".as_ptr(), stderr_write) == 0;
        if !attr_ok {
            capi::dec_ref(stdout_write);
            capi::dec_ref(stderr_write);
            return false;
        }
        capi::dec_ref(stdout_write);
        capi::dec_ref(stderr_write);
        let ok = capi::PyRun_SimpleString(bootstrap.as_ptr()) == 0;
        // The bootstrap's temporary attributes come off `sys` whether or
        // not the class assignment landed; clearing through a null value
        // deletes the attribute.
        capi::PyObject_SetAttrString(sys, c"_bw_stdout_write".as_ptr(), std::ptr::null_mut());
        capi::PyObject_SetAttrString(sys, c"_bw_stderr_write".as_ptr(), std::ptr::null_mut());
        capi::clear_error();
        ok
    }
}

/// `initExceptionHook` without the `Mercury::EventDispatcher` parameter: the
/// reference takes a dispatcher to defer source-file reads across event loop
/// iterations, and the port reads them synchronously, so there is nothing to
/// register. The reference's per-process call sites (`cellapp.cpp:3622`,
/// `baseapp.cpp:1304`, `dbapp.cpp:324`, `bots/main_app.cpp:273`) run it right
/// after `Script::init`; the port folds that into init until a process layer
/// exists to take it back.
pub fn install_exception_hook() -> bool {
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return false;
    }
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let function = capi::PyCFunction_New(
            std::ptr::addr_of_mut!(PRINT_TRACE_BACK_DEF),
            std::ptr::null_mut(),
        );
        if function.is_null() {
            return false;
        }
        let sys = capi::PyImport_AddModule(c"sys".as_ptr());
        let ok = !sys.is_null()
            && capi::PyObject_SetAttrString(sys, c"excepthook".as_ptr(), function) == 0;
        // `sys.excepthook` owns its reference once set.
        capi::dec_ref(function);
        ok
    }
}

/// The `write` method descriptor; `PyCFunction_New` copies nothing, so this
/// must live as long as the builtins -- a static.
static mut PY_WRITE_DEF: capi::PyMethodDef = capi::PyMethodDef {
    ml_name: c"write".as_ptr(),
    ml_meth: Some(py_write),
    ml_flags: capi::METH_VARARGS,
    ml_doc: c"Write a string to this stream.".as_ptr(),
};

/// The `printTraceBack` descriptor, the same lifetime rule.
static mut PRINT_TRACE_BACK_DEF: capi::PyMethodDef = capi::PyMethodDef {
    ml_name: c"printTraceBack".as_ptr(),
    ml_meth: Some(print_trace_back),
    ml_flags: capi::METH_VARARGS,
    ml_doc: c"".as_ptr(),
};

/// Drop the buffered text and every hook, keeping the host's log entry:
/// the unit tests install their capture before init runs, and the
/// reference's `LogMsg` is process-wide state that outlives an interpreter
/// cycle too.
fn clear_writer() {
    let mut writer = writer();
    writer.stdout_buffer.clear();
    writer.stderr_buffer.clear();
    writer.hooks.clear();
    writer.next_hook_id = 0;
    CURRENT_HOOK.set(None);
    DELETE_CURRENT.set(false);
}

/// `Script::fini`'s teardown for this module: the reference's writer is
/// destroyed when `Py_Finalize` clears the `sys` module, which notifies every
/// hook; the port's writer is a process static, so fini drops the buffered
/// text and the hooks -- the notification has no recipient left to reach.
pub fn reset() {
    clear_writer();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The static sink the unit tests capture through.
    static CAPTURE: OnceLock<Mutex<Vec<(String, bool)>>> = OnceLock::new();

    fn capture() -> std::sync::MutexGuard<'static, Vec<(String, bool)>> {
        CAPTURE
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
    }

    fn captured() -> Arc<Mutex<Vec<(String, bool)>>> {
        Arc::new(Mutex::new(capture().clone()))
    }

    fn clear_capture() {
        capture().clear();
    }

    fn capture_sink(line: &str, is_stderr: bool) {
        capture().push((line.to_owned(), is_stderr));
    }

    /// A write without its newline stays in the buffer; the newline flushes
    /// the assembled line to the sink, stdout carrying the info level.
    #[test]
    fn partial_lines_buffer_until_the_newline() {
        let _lock = writer_serial();
        *writer() = WriterState::new();
        clear_capture();
        set_log_sink(Box::new(capture_sink));
        handle_write("hel", false);
        assert!(captured().lock().unwrap().is_empty());
        handle_write("lo\n", false);
        assert_eq!(
            *captured().lock().unwrap(),
            vec![("hello".to_owned(), false)]
        );
    }

    /// stderr writes carry the error flag, and the two streams buffer
    /// separately.
    #[test]
    fn stderr_lines_flag_the_error_level() {
        let _lock = writer_serial();
        *writer() = WriterState::new();
        clear_capture();
        set_log_sink(Box::new(capture_sink));
        handle_write("stdout line\n", false);
        handle_write("stderr line\n", true);
        assert_eq!(
            *captured().lock().unwrap(),
            vec![
                ("stdout line".to_owned(), false),
                ("stderr line".to_owned(), true)
            ]
        );
    }

    /// The writer, the log sink and the capture are all process globals, so
    /// the tests that read or mutate them run one at a time: a parallel
    /// sibling would buffer into the same writer state and blur the very
    /// lines they assert on.
    fn writer_serial() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A write with several newlines logs every complete line.
    #[test]
    fn multi_line_writes_log_each_line() {
        let _lock = writer_serial();
        *writer() = WriterState::new();
        clear_capture();
        set_log_sink(Box::new(capture_sink));
        handle_write("one\ntwo\n", false);
        assert_eq!(
            *captured().lock().unwrap(),
            vec![("one".to_owned(), false), ("two".to_owned(), false)]
        );
    }

    /// A partially written stdout line survives the flush of the other
    /// stream and completes later.
    #[test]
    fn the_two_streams_buffer_independently() {
        let _lock = writer_serial();
        *writer() = WriterState::new();
        clear_capture();
        set_log_sink(Box::new(capture_sink));
        handle_write("out partial", false);
        handle_write("err done\n", true);
        assert_eq!(
            *captured().lock().unwrap(),
            vec![("err done".to_owned(), true)]
        );
        handle_write(" done\n", false);
        assert_eq!(
            captured()
                .lock()
                .unwrap()
                .iter()
                .filter(|(line, _)| line == "out partial done")
                .count(),
            1
        );
    }

    /// Without the redirect installed there is no writer to hook, the
    /// reference's `getCurrentStdio() == NULL` case.
    #[test]
    fn hooks_refuse_without_the_redirect() {
        let _lock = writer_serial();
        *writer() = WriterState::new();
        assert!(!stdio_is_redirected());
        assert_eq!(
            add_hook(Box::new(|_: &str, _: bool| {})),
            Err(HookError::NoStdioWriter)
        );
    }

    /// A source line is read back with its leading whitespace stripped, and
    /// a missing file or out-of-range line yields `None` -- the reference
    /// skips the line in those cases.
    #[test]
    fn source_lines_are_read_and_trimmed() {
        let _lock = writer_serial();
        let directory =
            std::env::temp_dir().join(format!("boundless-output-writer-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the directory");
        let path = directory.join("probe_script.py");
        std::fs::write(&path, "VALUE = 1\n    INDENTED = 2\n").expect("write the script");
        let name = path.to_str().expect("a UTF-8 path");
        assert_eq!(read_source_line(name, 2), Some("INDENTED = 2".to_owned()));
        assert_eq!(read_source_line(name, 9), None);
        assert_eq!(read_source_line(&format!("{name}/missing.py"), 1), None);
        assert_eq!(read_source_line("", 1), None);
        std::fs::remove_dir_all(&directory).ok();
    }

    /// A blank or whitespace-only source line yields `None`: the reference
    /// skips frames whose line it cannot show.
    #[test]
    fn a_blank_source_line_reads_as_none() {
        let _lock = writer_serial();
        let directory =
            std::env::temp_dir().join(format!("boundless-output-writer-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the directory");
        let path = directory.join("blank_lines.py");
        std::fs::write(&path, "\n \t \nVALUE = 1\n").expect("write the script");
        let name = path.to_str().expect("a UTF-8 path");
        assert_eq!(read_source_line(name, 1), None);
        assert_eq!(read_source_line(name, 2), None);
        assert_eq!(read_source_line(name, 3), Some("VALUE = 1".to_owned()));
        std::fs::remove_dir_all(&directory).ok();
    }

    /// A poisoned writer lock recovers for the next caller, the way the
    /// other process-wide registries in the reference behave.
    #[test]
    fn a_poisoned_writer_lock_recovers() {
        let _lock = writer_serial();
        let poisoner = std::thread::spawn(|| {
            let _guard = writer();
            panic!("poison the writer lock");
        });
        assert!(poisoner.join().is_err());
        assert!(writer().hooks.is_empty());
    }

    /// Both installs refuse to run without an interpreter: the reference's
    /// `MF_ASSERT( Py_IsInitialized() )` aborts, the port reports instead.
    #[test]
    fn installs_refuse_without_an_interpreter() {
        let _lock = writer_serial();
        assert!(!install());
        assert!(!install_exception_hook());
    }
}
