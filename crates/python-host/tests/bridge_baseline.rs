//! OPT-03-01-01: the Python bridge baseline — conversion, GIL and callback
//! timings against the real CPython the crate links, one interpreter, one
//! process.
//!
//! The single-op C-API faces cost tens of nanoseconds, below the resolution
//! an honest `Instant`-per-op report could carry, so every face is measured
//! as windows: a fixed count of back-to-back operations timed together, the
//! window divided into a per-op figure, percentiles taken across the
//! windows. Structural assertions ride along — every conversion face
//! round-trips its exact value, the callback returns the sum, the posted
//! callbacks run in posted order — so the binary is a test, not just a
//! probe.
//!
//! Timings are machine-dependent and are recorded, never banded: with
//! `PYTHON_BRIDGE_OUT` set to an absolute path the records are appended as
//! `key\tvalue` lines (clear the file before a rerun — append mode):
//!
//! ```sh
//! PYTHON_BRIDGE_OUT=<absolute path> cargo test -p python-host \
//!     --test bridge_baseline the_bridge_baseline_records_all_faces \
//!     -- --exact --nocapture
//! ```

use std::io::Write as _;
use std::os::raw::{c_char, c_long};
use std::time::{Duration, Instant};

use python_host::capi;
use python_host::gil;
use python_host::interpreter::{InitConfig, Interpreter};
use python_host::pyref::PyRef;

/// Marker text the string faces convert; long enough that the copy is a
/// real cost, short enough to stay trivial.
const MARKER: &[u8] = b"bridge baseline marker 0123456789";

/// The tuple index the subscript face reads: component 0.
const SUBSCRIPT_INDEX: c_long = 0;

/// The tuple face's payload.
const TUPLE_VALUES: (i64, i64, i64) = (42, -7, 1000);

/// Conversions per timed window. Fifty windows give the percentiles fifty
/// per-op figures.
const WINDOW_OPS: usize = 1_000;
const WINDOWS: usize = 50;

/// Calls per window for the faces that cost more than a bare conversion.
const CALL_WINDOW_OPS: usize = 1_000;
const CALL_WINDOWS: usize = 10;

/// Posted callbacks per drain pass.
const POSTED_PER_PASS: usize = 500;
const DRAIN_PASSES: usize = 10;

/// One face: a name, and a closure that runs `ops` back-to-back operations.
/// The return is the window's wall time.
type Face<'a> = (&'a str, usize, Box<dyn FnMut() + 'a>);

/// Runs `face` and appends its per-op percentiles, in nanoseconds, to
/// `records`.
fn measure(face: Face, records: &mut Vec<(String, String)>) {
    let (name, ops, mut run) = face;
    let mut figures = Vec::with_capacity(WINDOWS);
    for _ in 0..WINDOWS {
        let started = Instant::now();
        for _ in 0..ops {
            run();
        }
        figures.push(started.elapsed() / ops as u32);
    }
    figures.sort();
    let at = |q: f64| figures[((figures.len() - 1) as f64 * q).round() as usize];
    for (label, value) in [
        ("p50", at(0.50)),
        ("p90", at(0.90)),
        ("p99", at(0.99)),
        ("max", figures[figures.len() - 1]),
    ] {
        records.push((format!("{name}_{label}_ns"), value.as_nanos().to_string()));
    }
}

/// The init config with a throw-away resource root, so `Script::init`'s own
/// `import bw_site` finds a module (`script.cpp:617`).
fn scenario_config() -> InitConfig {
    let root = std::env::temp_dir().join(format!(
        "boundless-python-host-bridge-baseline-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
    std::fs::write(
        root.join("scripts/common/bw_site.py"),
        "import sys\nsys.bw_site_imported = True\n",
    )
    .expect("write bw_site");
    let mut config = InitConfig::default();
    config.import_paths.add_res_path(
        &[root.to_str().expect("a UTF-8 root")],
        python_host::import_paths::COMMON_PATH,
    );
    config
}

/// Defines the callback faces' functions in `__main__`: a no-argument
/// callable and the three-argument sum. Returns the borrowed `add3`.
fn define_callbacks() -> *mut capi::PyObject {
    // SAFETY: only called while the interpreter is up; the definitions run
    // on the thread that owns the GIL.
    unsafe {
        let source = c"def probe():\n    return None\n\n\
                       def add3(a, b, c):\n    return a + b + c\n";
        if capi::PyRun_SimpleString(source.as_ptr()) != 0 {
            panic!("the callback definitions did not run");
        }
        let module = capi::PyImport_AddModule(c"__main__".as_ptr());
        let dictionary = capi::PyModule_GetDict(module);
        capi::PyDict_GetItemString(dictionary, c"add3".as_ptr())
    }
}

#[test]
fn the_bridge_baseline_records_all_faces() {
    let interpreter = Interpreter::init(&scenario_config()).expect("Script::init");
    let mut records: Vec<(String, String)> = vec![
        ("window_ops".into(), WINDOW_OPS.to_string()),
        ("windows".into(), WINDOWS.to_string()),
        ("call_window_ops".into(), CALL_WINDOW_OPS.to_string()),
        ("call_windows".into(), CALL_WINDOWS.to_string()),
        ("posted_per_pass".into(), POSTED_PER_PASS.to_string()),
    ];

    // ---- conversion, native -> Python ------------------------------------
    // The products of the building faces are held and released after the
    // timed loops, so a face measures its conversion and not the decref.
    let mut to_python: Vec<Face> = vec![
        (
            "to_python_int",
            WINDOW_OPS,
            Box::new(|| {
                let object = unsafe { capi::PyLong_FromLong(42 as c_long) };
                assert!(!object.is_null());
                unsafe { capi::Py_DecRef(object) };
            }),
        ),
        (
            "to_python_float",
            WINDOW_OPS,
            Box::new(|| {
                let object = unsafe { capi::PyFloat_FromDouble(std::f64::consts::PI) };
                assert!(!object.is_null());
                unsafe { capi::Py_DecRef(object) };
            }),
        ),
        (
            "to_python_string",
            WINDOW_OPS,
            Box::new(|| {
                let object = unsafe {
                    capi::PyUnicode_FromStringAndSize(
                        MARKER.as_ptr() as *const c_char,
                        MARKER.len() as _,
                    )
                };
                assert!(!object.is_null());
                unsafe { capi::Py_DecRef(object) };
            }),
        ),
    ];
    // The tuple face needs its elements first, and a fresh tuple each op.
    // SetItem steals the references, so each built tuple increfs the three
    // elements once.
    let elements = [
        PyRef::from_owned(unsafe { capi::PyLong_FromLong(TUPLE_VALUES.0 as c_long) })
            .expect("the first element"),
        PyRef::from_owned(unsafe { capi::PyLong_FromLong(TUPLE_VALUES.1 as c_long) })
            .expect("the second element"),
        PyRef::from_owned(unsafe { capi::PyLong_FromLong(TUPLE_VALUES.2 as c_long) })
            .expect("the third element"),
    ];
    to_python.push((
        "to_python_tuple3",
        WINDOW_OPS,
        Box::new(move || {
            let tuple = unsafe { capi::PyTuple_New(3) };
            assert!(!tuple.is_null());
            for (index, element) in elements.iter().enumerate() {
                unsafe { capi::Py_IncRef(element.as_ptr()) };
                assert_eq!(
                    unsafe { capi::PyTuple_SetItem(tuple, index as _, element.as_ptr()) },
                    0
                );
            }
            unsafe { capi::Py_DecRef(tuple) };
        }),
    ));

    // ---- conversion, Python -> native ------------------------------------
    let int_object =
        PyRef::from_owned(unsafe { capi::PyLong_FromLong(42 as c_long) }).expect("an int object");
    let float_object = PyRef::from_owned(unsafe { capi::PyFloat_FromDouble(std::f64::consts::PI) })
        .expect("a float object");
    let string_object = PyRef::from_owned(unsafe {
        capi::PyUnicode_FromStringAndSize(MARKER.as_ptr() as *const c_char, MARKER.len() as _)
    })
    .expect("a string object");
    let tuple_object = PyRef::from_owned({
        let tuple = unsafe { capi::PyTuple_New(3) };
        assert!(!tuple.is_null());
        let (a, b, c) = TUPLE_VALUES;
        for (index, value) in [a, b, c].into_iter().enumerate() {
            let item = unsafe { capi::PyLong_FromLong(value as c_long) };
            assert!(!item.is_null());
            assert_eq!(unsafe { capi::PyTuple_SetItem(tuple, index as _, item) }, 0);
        }
        tuple
    })
    .expect("a tuple object");

    // Each extraction face asserts its exact value once, before the timed
    // loop; the loop itself re-extracts.
    assert_eq!(unsafe { capi::PyLong_AsLongLong(int_object.as_ptr()) }, 42);
    assert_eq!(
        unsafe { capi::PyFloat_AsDouble(float_object.as_ptr()) },
        std::f64::consts::PI
    );
    {
        let mut size = 0;
        let text = unsafe { capi::PyUnicode_AsUTF8AndSize(string_object.as_ptr(), &mut size) };
        let text = unsafe { std::slice::from_raw_parts(text as *const u8, size as usize) };
        assert_eq!(text, MARKER);
    }
    assert_eq!(unsafe { capi::PyTuple_Size(tuple_object.as_ptr()) }, 3);
    assert_eq!(
        unsafe { capi::PyLong_AsLongLong(capi::PyTuple_GetItem(tuple_object.as_ptr(), 1)) },
        -7
    );

    let mut from_python: Vec<Face> = vec![
        (
            "from_python_int",
            WINDOW_OPS,
            Box::new(|| {
                assert_eq!(unsafe { capi::PyLong_AsLongLong(int_object.as_ptr()) }, 42);
            }),
        ),
        (
            "from_python_float",
            WINDOW_OPS,
            Box::new(|| {
                assert_eq!(
                    unsafe { capi::PyFloat_AsDouble(float_object.as_ptr()) },
                    std::f64::consts::PI
                );
            }),
        ),
        (
            "from_python_string",
            WINDOW_OPS,
            Box::new(|| {
                let mut size = 0;
                let text =
                    unsafe { capi::PyUnicode_AsUTF8AndSize(string_object.as_ptr(), &mut size) };
                assert!(!text.is_null());
                assert_eq!(size as usize, MARKER.len());
            }),
        ),
        (
            "from_python_tuple3",
            WINDOW_OPS,
            Box::new(|| {
                assert_eq!(unsafe { capi::PyTuple_Size(tuple_object.as_ptr()) }, 3);
                assert_eq!(
                    unsafe {
                        capi::PyLong_AsLongLong(capi::PyTuple_GetItem(tuple_object.as_ptr(), 2))
                    },
                    TUPLE_VALUES.2
                );
            }),
        ),
    ];

    // ---- handle layer ------------------------------------------------------
    let mut handle: Vec<Face> = vec![(
        "handle_clone_drop",
        WINDOW_OPS,
        Box::new(|| {
            let cloned = int_object.clone();
            assert_eq!(cloned.as_ptr(), int_object.as_ptr());
            drop(cloned);
        }),
    )];

    // ---- GIL bracket -------------------------------------------------------
    // The reference's main-thread idle pair: release, then take it back.
    let mut gil_faces: Vec<Face> = vec![(
        "gil_release_acquire",
        CALL_WINDOW_OPS,
        Box::new(|| {
            unsafe { gil::release_lock().expect("release") };
            unsafe { gil::acquire_lock() };
        }),
    )];

    // ---- callbacks ---------------------------------------------------------
    let add3 = define_callbacks();
    assert!(!add3.is_null(), "the callback was not found");
    // The noargs face calls `probe`, which needs no arguments.
    let probe = unsafe {
        let module = capi::PyImport_AddModule(c"__main__".as_ptr());
        let dictionary = capi::PyModule_GetDict(module);
        capi::PyDict_GetItemString(dictionary, c"probe".as_ptr())
    };
    assert!(!probe.is_null(), "the noargs callback was not found");
    let noargs_result = unsafe { capi::PyObject_CallNoArgs(probe) };
    assert!(!noargs_result.is_null());
    unsafe { capi::Py_DecRef(noargs_result) };

    let call_args = PyRef::from_owned({
        let tuple = unsafe { capi::PyTuple_New(3) };
        assert!(!tuple.is_null());
        let (a, b, c) = (1, 2, 3);
        for (index, value) in [a, b, c].into_iter().enumerate() {
            let item = unsafe { capi::PyLong_FromLong(value as c_long) };
            assert!(!item.is_null());
            assert_eq!(unsafe { capi::PyTuple_SetItem(tuple, index as _, item) }, 0);
        }
        tuple
    })
    .expect("the argument tuple");
    let call_result = unsafe { capi::PyObject_CallObject(add3, call_args.as_ptr()) };
    assert!(!call_result.is_null(), "the callback did not run");
    assert_eq!(unsafe { capi::PyLong_AsLongLong(call_result) }, 6);
    unsafe { capi::Py_DecRef(call_result) };

    let mut callback_faces: Vec<Face> = vec![
        (
            "call_noargs",
            CALL_WINDOW_OPS,
            Box::new(|| {
                let result = unsafe { capi::PyObject_CallNoArgs(probe) };
                assert!(!result.is_null());
                unsafe { capi::Py_DecRef(result) };
            }),
        ),
        (
            "call_3args",
            CALL_WINDOW_OPS,
            Box::new(|| {
                let result = unsafe { capi::PyObject_CallObject(add3, call_args.as_ptr()) };
                assert!(!result.is_null());
                unsafe { capi::Py_DecRef(result) };
            }),
        ),
    ];

    for face in to_python.drain(..) {
        measure(face, &mut records);
    }
    for face in from_python.drain(..) {
        measure(face, &mut records);
    }
    for face in handle.drain(..) {
        measure(face, &mut records);
    }
    for face in gil_faces.drain(..) {
        measure(face, &mut records);
    }
    for face in callback_faces.drain(..) {
        measure(face, &mut records);
    }

    // ---- posted main-thread callbacks -------------------------------------
    // `gil::post` is the cross-thread entry; the drain pass is the script
    // thread's cost. Each pass asserts the posted count and the order.
    let mut drain_figures = Vec::with_capacity(DRAIN_PASSES);
    for _ in 0..DRAIN_PASSES {
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        for index in 0..POSTED_PER_PASS {
            let order = std::sync::Arc::clone(&order);
            gil::post(move || order.lock().unwrap().push(index));
        }
        assert_eq!(gil::pending(), POSTED_PER_PASS);
        let started = Instant::now();
        let completed = unsafe { gil::process() };
        drain_figures.push(started.elapsed());
        assert_eq!(completed, POSTED_PER_PASS);
        assert_eq!(gil::pending(), 0);
        assert_eq!(
            order.lock().unwrap().clone(),
            (0..POSTED_PER_PASS).collect::<Vec<_>>()
        );
    }
    let per_callback: Vec<Duration> = drain_figures
        .iter()
        .map(|d| *d / POSTED_PER_PASS as u32)
        .collect();
    for (label, value) in [("p50", 0.50), ("p90", 0.90), ("p99", 0.99)] {
        let mut sorted = per_callback.clone();
        sorted.sort();
        let at = sorted[((sorted.len() - 1) as f64 * value).round() as usize];
        records.push((
            format!("post_drain_per_callback_{label}_ns"),
            at.as_nanos().to_string(),
        ));
    }

    // ---- port-level conversion: the Math.Matrix translation faces --------
    // `m.translation = (1, 2, 3)` and `m.translation` are the engine's
    // hottest conversion paths: every assignment walks the type check and
    // the tuple parse, every read builds a Vector3 back. The round trip is
    // asserted once, before the timed loops.
    let matrix = {
        // SAFETY: only called while the interpreter is up.
        unsafe {
            let module = capi::PyImport_ImportModule(c"Math".as_ptr());
            assert!(!module.is_null(), "the Math module did not import");
            let dictionary = capi::PyModule_GetDict(module);
            let type_ = capi::PyDict_GetItemString(dictionary, c"Matrix".as_ptr());
            assert!(!type_.is_null(), "the Matrix type was not found");
            let instance = capi::PyObject_CallNoArgs(type_);
            assert!(!instance.is_null(), "the Matrix instance was not built");
            instance
        }
    };
    // The setter's value is a live Vector3, the engine's hottest shape: the
    // assignment walks `check_type` against the registered type object and
    // takes the fast body copy (`script_math.cpp` accessor set shape).
    let vector3 = PyRef::from_owned({
        let type_ = unsafe {
            let module = capi::PyImport_AddModule(c"Math".as_ptr());
            let dictionary = capi::PyModule_GetDict(module);
            capi::PyDict_GetItemString(dictionary, c"Vector3".as_ptr())
        };
        assert!(!type_.is_null(), "the Vector3 type was not found");
        let arguments = unsafe {
            let tuple = capi::PyTuple_New(3);
            assert!(!tuple.is_null());
            for (index, value) in [1.0f64, 2.0, 3.0].into_iter().enumerate() {
                let item = capi::PyFloat_FromDouble(value);
                assert!(!item.is_null());
                assert_eq!(capi::PyTuple_SetItem(tuple, index as _, item), 0);
            }
            tuple
        };
        let instance = unsafe { capi::PyObject_CallObject(type_, arguments) };
        unsafe { capi::Py_DecRef(arguments) };
        assert!(!instance.is_null(), "the Vector3 instance was not built");
        instance
    })
    .expect("the translation vector");
    // SAFETY: the interpreter is up; `matrix` and `vector3` are live.
    unsafe {
        assert_eq!(
            capi::PyObject_SetAttrString(matrix, c"translation".as_ptr(), vector3.as_ptr()),
            0,
            "the translation assignment was refused"
        );
        let read = capi::PyObject_GetAttrString(matrix, c"translation".as_ptr());
        assert!(!read.is_null(), "the translation read failed");
        assert!(
            !std::ptr::eq(read, vector3.as_ptr()),
            "the getter must hand back a fresh copy"
        );
        assert_eq!(capi::PySequence_Size(read), 3);
        for (index, expected) in [1.0f64, 2.0, 3.0].into_iter().enumerate() {
            let component = capi::PySequence_GetItem(read, index as _);
            assert!(!component.is_null());
            assert_eq!(capi::PyFloat_AsDouble(component), expected);
            capi::Py_DecRef(component);
        }
        capi::Py_DecRef(read);
        let component =
            capi::PySequence_GetItem(vector3.as_ptr(), SUBSCRIPT_INDEX as capi::PySsize);
        assert_eq!(
            capi::PyFloat_AsDouble(component),
            1.0,
            "the subscript read failed"
        );
        capi::Py_DecRef(component);
    }
    let mut math_faces: Vec<Face> = vec![
        (
            "matrix_set_translation",
            WINDOW_OPS,
            Box::new(|| {
                // SAFETY: the interpreter is up; the operands are live.
                assert_eq!(
                    unsafe {
                        capi::PyObject_SetAttrString(
                            matrix,
                            c"translation".as_ptr(),
                            vector3.as_ptr(),
                        )
                    },
                    0
                );
            }),
        ),
        (
            "matrix_get_translation",
            WINDOW_OPS,
            Box::new(|| {
                // SAFETY: the interpreter is up; the operand is live.
                let read = unsafe { capi::PyObject_GetAttrString(matrix, c"translation".as_ptr()) };
                assert!(!read.is_null());
                unsafe { capi::Py_DecRef(read) };
            }),
        ),
        (
            "vector3_getitem",
            WINDOW_OPS,
            Box::new(|| {
                // SAFETY: the interpreter is up; the operand is live.
                let component =
                    unsafe { capi::PySequence_GetItem(vector3.as_ptr(), SUBSCRIPT_INDEX as _) };
                assert!(!component.is_null());
                unsafe { capi::Py_DecRef(component) };
            }),
        ),
    ];
    for face in math_faces.drain(..) {
        measure(face, &mut records);
    }

    records.push((
        "faces_measured".into(),
        "to_python 4, from_python 4, handle 1, gil 1, callback 2, post_drain 1, math 3".into(),
    ));

    if let Ok(path) = std::env::var("PYTHON_BRIDGE_OUT") {
        let mut text = String::new();
        for (key, value) in &records {
            text.push_str(key);
            text.push('\t');
            text.push_str(value);
            text.push('\n');
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open the record file")
            .write_all(text.as_bytes())
            .expect("write the records");
    }

    interpreter.fini();
}
