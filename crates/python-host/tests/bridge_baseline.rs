//! OPT-03-01-01: the Python bridge baseline measurements.
//!
//! The acceptance saves three record families — native↔Python conversion
//! timing, cross-language callback timing, and GIL-queue occupancy —
//! against the embedded interpreter the way the engine runs it: the
//! main thread owns the lock from `Script::init` and the posted-task
//! queue is drained by the thread that owns the GIL.
//!
//! Structural invariants are asserted on every run: every conversion
//! round-trip must return the value it was given, every callback must
//! produce its object, every posted callback must complete, and the
//! queue must drain to empty. The wall-clock numbers are only recorded
//! (never asserted tight — like the network baseline, the raw
//! nanoseconds are machine-dependent) and pass through
//! `BRIDGE_BASELINE_OUT` as a `metric<TAB>value` TSV when set, matching
//! the network/load fixture format. The canonical record comes from the
//! runner leg alone:
//!
//! ```sh
//! BRIDGE_BASELINE_OUT=<abs path> cargo test -p python-host \
//!     --test bridge_baseline the_python_bridge_baseline_runs_all_legs -- --exact
//! ```

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::time::Instant;

use python_host::capi;
use python_host::import_paths;
use python_host::interpreter::{InitConfig, Interpreter};
use python_host::pyref::PyRef;

/// Round-trips per conversion family.
const CONVERT_ROUNDS: usize = 20_000;
/// No-argument Python calls timed.
const CALLBACK_CALLS: usize = 5_000;
/// Callbacks posted cross-thread and drained on the GIL thread.
const POSTED_CALLBACKS: usize = 2_000;

// ---------------------------------------------------------------------------
// The record sink
// ---------------------------------------------------------------------------

/// Collected `metric<TAB>value` rows. Printed to stdout on flush and
/// written to `$BRIDGE_BASELINE_OUT` when set (the network/load fixture
/// format).
#[derive(Default)]
struct Record(Vec<(String, String)>);

impl Record {
    fn put(&mut self, metric: &str, value: impl std::fmt::Display) {
        self.0.push((metric.to_string(), value.to_string()));
    }

    fn flush(&self) {
        let mut text = String::new();
        for (k, v) in &self.0 {
            text.push_str(&format!("{k}\t{v}\n"));
        }
        print!("{text}");
        if let Ok(path) = std::env::var("BRIDGE_BASELINE_OUT") {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .expect("write BRIDGE_BASELINE_OUT");
            println!("record: {path}");
        }
        let _ = std::io::stdout().flush();
    }
}

/// The p50/p90/p99/max/min summary of a sample set, in nanoseconds.
fn summarize(samples: &mut [f64]) -> [f64; 5] {
    samples.sort_by(|a, b| a.total_cmp(b));
    let pick = |p: f64| {
        let idx = ((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1;
        samples[idx]
    };
    [
        pick(0.50),
        pick(0.90),
        pick(0.99),
        samples[samples.len() - 1],
        samples[0],
    ]
}

/// Records one family's summary under `prefix_{p50,p90,p99,max,min}_ns`.
fn put_summary(record: &mut Record, prefix: &str, summary: [f64; 5]) {
    let names = ["p50", "p90", "p99", "max", "min"];
    for (name, value) in names.iter().zip(summary) {
        record.put(&format!("{prefix}_{name}_ns"), format!("{value:.1}"));
    }
}

// ---------------------------------------------------------------------------
// The legs
// ---------------------------------------------------------------------------

/// Leg 1: native→Python→native round-trips for the int, float and str
/// families. Every round-trip is checked for identity; the timed sample
/// is the full there-and-back pair.
fn conversion_leg(record: &mut Record) {
    let mut int_samples = Vec::with_capacity(CONVERT_ROUNDS);
    let mut float_samples = Vec::with_capacity(CONVERT_ROUNDS);
    let mut str_samples = Vec::with_capacity(CONVERT_ROUNDS);

    for i in 0..CONVERT_ROUNDS as i64 {
        // int: PyLong_FromLong → PyLong_AsLongLong.
        let started = Instant::now();
        let number = unsafe { capi::PyLong_FromLong(i) };
        assert!(!number.is_null(), "PyLong_FromLong failed");
        let read = unsafe { capi::PyLong_AsLongLong(number) };
        unsafe { capi::Py_DecRef(number) };
        assert_eq!(read, i, "the int round-trip must return its input");
        int_samples.push(started.elapsed().as_nanos() as f64);

        // float: PyFloat_FromDouble → PyFloat_AsDouble.
        let value = i as f64 + 0.5;
        let started = Instant::now();
        let number = unsafe { capi::PyFloat_FromDouble(value) };
        assert!(!number.is_null(), "PyFloat_FromDouble failed");
        let read = unsafe { capi::PyFloat_AsDouble(number) };
        unsafe { capi::Py_DecRef(number) };
        assert_eq!(read, value, "the float round-trip must return its input");
        float_samples.push(started.elapsed().as_nanos() as f64);

        // str: PyUnicode_FromStringAndSize → PyUnicode_AsUTF8AndSize.
        let text = format!("bot-{i}");
        let started = Instant::now();
        let unicode = unsafe {
            capi::PyUnicode_FromStringAndSize(text.as_ptr().cast(), text.len() as capi::PySsize)
        };
        assert!(!unicode.is_null(), "PyUnicode_FromStringAndSize failed");
        let mut size: capi::PySsize = 0;
        let chars = unsafe { capi::PyUnicode_AsUTF8AndSize(unicode, &mut size) };
        assert!(!chars.is_null(), "PyUnicode_AsUTF8AndSize failed");
        let read = unsafe { std::ffi::CStr::from_ptr(chars).to_bytes() };
        assert_eq!(
            read,
            text.as_bytes(),
            "the str round-trip must return its input"
        );
        unsafe { capi::Py_DecRef(unicode) };
        str_samples.push(started.elapsed().as_nanos() as f64);
    }

    record.put("convert_round_trips", CONVERT_ROUNDS);
    put_summary(record, "convert_int", summarize(&mut int_samples));
    put_summary(record, "convert_float", summarize(&mut float_samples));
    put_summary(record, "convert_str", summarize(&mut str_samples));
}

/// Leg 2: a Python `noop()` defined through `PyRun_SimpleString`, then
/// called from the native side through `PyObject_CallNoArgs`. The
/// reference cross-language call shape: fetch the callable once, call it
/// many times, drop each result.
fn callback_leg(record: &mut Record, interpreter: &Interpreter) {
    assert!(
        interpreter.run_simple_string("def noop():\n    pass\n"),
        "defining the callback failed"
    );
    let module = unsafe { capi::PyImport_AddModule(c"__main__".as_ptr()) };
    assert!(!module.is_null(), "fetching __main__ failed");
    // `PyImport_AddModule` returns a borrowed reference; the attribute
    // fetch hands back a new one.
    let holder = PyRef::borrowed(module);
    let function = unsafe { capi::PyObject_GetAttrString(holder.as_ptr(), c"noop".as_ptr()) };
    let function = PyRef::from_owned(function).expect("fetching noop failed");

    let mut samples = Vec::with_capacity(CALLBACK_CALLS);
    for _ in 0..CALLBACK_CALLS {
        let started = Instant::now();
        let result = unsafe { capi::PyObject_CallNoArgs(function.as_ptr()) };
        assert!(!result.is_null(), "the noop call failed");
        unsafe { capi::Py_DecRef(result) };
        samples.push(started.elapsed().as_nanos() as f64);
    }

    record.put("callback_calls", CALLBACK_CALLS);
    put_summary(record, "callback_noarg", summarize(&mut samples));
}

/// Leg 3: GIL-queue occupancy. A poster thread runs flat-out posting
/// [`POSTED_CALLBACKS`] callbacks through [`python_host::gil::post`]
/// while the GIL thread drains them in `process()` passes. Recorded per
/// callback: the post→run end-to-end latency, and the drain throughput
/// over the whole window.
fn gil_leg(record: &mut Record) {
    let (sender, receiver) = channel::<(Instant, Instant)>();
    let poster = std::thread::spawn(move || {
        for _ in 0..POSTED_CALLBACKS {
            let posted = Instant::now();
            // The closure runs on the GIL thread; it reports its own run
            // time back to the measurer.
            let report: Sender<(Instant, Instant)> = sender.clone();
            python_host::gil::post(move || {
                let _ = report.send((posted, Instant::now()));
            });
        }
    });

    let started = Instant::now();
    let mut completed = 0;
    while completed < POSTED_CALLBACKS {
        // Safe: this is the thread that owns the GIL, and the posted
        // closures only touch the report channel.
        let ran = unsafe { python_host::gil::process() };
        completed += ran;
    }
    let drained = started.elapsed();

    poster.join().expect("the poster thread");
    assert_eq!(
        python_host::gil::pending(),
        0,
        "the queue must drain to empty"
    );

    let mut latencies = receiver
        .try_iter()
        .map(|(posted, ran)| ran.duration_since(posted).as_nanos() as f64)
        .collect::<Vec<_>>();
    assert_eq!(
        latencies.len(),
        POSTED_CALLBACKS,
        "every posted callback must report a run time"
    );

    record.put("gil_posted", POSTED_CALLBACKS);
    put_summary(record, "gil_e2e", summarize(&mut latencies));
    record.put(
        "gil_drain_ns_per_callback",
        format!("{:.1}", drained.as_nanos() as f64 / POSTED_CALLBACKS as f64),
    );
}

// ---------------------------------------------------------------------------
// The runner leg
// ---------------------------------------------------------------------------

/// A throw-away resource root laid out the way the engine ships its
/// scripts, so `Script::init`'s `bw_site` import finds its tree (the
/// billing scenarios' shape).
fn temp_resource_root() -> PathBuf {
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
    root
}

/// `Script::init`'s config with the fixture root in the path list.
fn baseline_config() -> InitConfig {
    let root = temp_resource_root();
    let root_path = root.to_str().expect("a UTF-8 root").to_owned();
    let mut paths = InitConfig::default().import_paths;
    paths.add_res_path(&[&root_path], import_paths::COMMON_PATH);
    InitConfig {
        import_paths: paths,
        ..InitConfig::default()
    }
}

/// Runs every leg in fixed order against one interpreter.
#[test]
fn the_python_bridge_baseline_runs_all_legs() {
    let mut record = Record::default();

    // The main thread owns the GIL from here on; every leg below relies
    // on that.
    let interpreter = Interpreter::init(&baseline_config()).expect("Script::init");

    conversion_leg(&mut record);
    callback_leg(&mut record, &interpreter);
    gil_leg(&mut record);

    assert_eq!(
        python_host::gil::clear(),
        0,
        "nothing may be left behind after the legs drain"
    );

    interpreter.fini();
    record.flush();
}
