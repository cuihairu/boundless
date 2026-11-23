//! BW-23-01-02: Python call capture.
//!
//! Drives a script inside the embedded interpreter (python-host) under a
//! pure-Python `sys.setprofile` shim — the analog of the reference's
//! `_bw_profile` module and `external_profiler.cpp` — and reads back
//! per-function call counts and cumulative monotonic nanoseconds, so a known
//! script can be profiled and the function the time went to is located (the
//! acceptance: 已知脚本可定位函数).
//!
//! The capture runs the shim plus the script as one source string through
//! `Interpreter::run_simple_string`.  python-host allows one interpreter per
//! process (`Interpreter::init` is process-global and never re-initialises),
//! so callers that need a fresh interpreter run the capture in a child
//! process — the `capture-python-profile` binary does exactly that, and the
//! CLI integration test drives it the same way the other `scan-*` bins are
//! driven.  The committed fixtures (`cases/python_profile/shim.py` and
//! `known_script.py`) are embedded, so this module never touches the reference
//! checkout at run time.

use std::path::Path;

use python_host::import_paths::COMMON_PATH;
use python_host::{ImportPaths, InitConfig, Interpreter};

/// The committed profile shim, embedded so tests and the CLI never read the
/// file at run time.
pub const SHIM_TEXT: &str = include_str!("../cases/python_profile/shim.py");

/// The committed known script (one heavy and one cheap function).
pub const KNOWN_SCRIPT: &str = include_str!("../cases/python_profile/known_script.py");

/// The placeholder in the shim that is replaced with the real output path.
pub const OUTPUT_PLACEHOLDER: &str = "__OUTPUT__";

/// One captured function: its name, source file, call count and cumulative
/// monotonic nanoseconds spent in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionTiming {
    /// The function name (`code.co_name`).
    pub name: String,
    /// The source file (`code.co_filename`).
    pub file: String,
    /// How many times it was called.
    pub calls: u32,
    /// Total monotonic nanoseconds across all calls.
    pub total_nanos: u64,
}

/// Substitute the real output path into the shim source.
pub fn shim_with_output(output: &Path) -> String {
    SHIM_TEXT.replace(OUTPUT_PLACEHOLDER, &output.display().to_string())
}

/// Capture a script in a fresh interpreter (one interpreter per process: the
/// caller must be a fresh child process) and write the TSV to `output`.
///
/// Returns `false` if the interpreter failed to initialise or the script did
/// not run to completion.
pub fn capture_script(script: &str, output: &Path) -> bool {
    // The reference always has the engine's script tree on its path, and
    // `Script::init` ends by importing `bw_site` from `scripts/common`, so the
    // interpreter needs a site module to import; this throwaway root stands in
    // for the engine tree (the same fixture the python-host scenario harness
    // builds). One interpreter per process, so the root is process-scoped.
    let root = std::env::temp_dir().join(format!("bw23-python-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    if std::fs::create_dir_all(root.join("scripts/common")).is_err() {
        return false;
    }
    if std::fs::write(
        root.join("scripts/common/bw_site.py"),
        "import sys\nsys.bw_site_imported = True\n",
    )
    .is_err()
    {
        return false;
    }
    let root_path = match root.to_str() {
        Some(path) => path.to_owned(),
        None => return false,
    };
    let mut import_paths = ImportPaths::new(ImportPaths::default_delimiter());
    import_paths.add_res_path(&[&root_path], COMMON_PATH);
    let config = InitConfig {
        argv: Vec::new(),
        import_paths,
        component: "cell".to_string(),
    };
    let interpreter = match Interpreter::init(&config) {
        Ok(interpreter) => interpreter,
        Err(error) => {
            eprintln!("capture_script: Script::init failed: {error:?}");
            return false;
        }
    };
    let source = format!(
        "{}\n_bwp_start()\n{}\n_bwp_stop()\n",
        shim_with_output(output),
        script
    );
    let ran = interpreter.run_simple_string(&source);
    if !ran {
        eprintln!("capture_script: the script did not run to completion");
    }
    ran
}

/// Parse the shim's TSV output: one `name\tfile\tcalls\ttotal_nanos` row per
/// function. Rows are already sorted by descending total time.
pub fn parse_rows(text: &str) -> Vec<FunctionTiming> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            let name = fields.next()?.to_string();
            let file = fields.next()?.to_string();
            let calls = fields.next()?.parse().ok()?;
            let total_nanos = fields.next()?.parse().ok()?;
            Some(FunctionTiming {
                name,
                file,
                calls,
                total_nanos,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shim_embeds_an_output_placeholder() {
        assert!(SHIM_TEXT.contains(OUTPUT_PLACEHOLDER));
    }

    #[test]
    fn substituting_the_output_path_replaces_exactly_the_placeholder() {
        let shim = shim_with_output(Path::new("/tmp/capture.tsv"));
        assert!(!shim.contains(OUTPUT_PLACEHOLDER));
        assert!(shim.contains("_OUTPUT = \"/tmp/capture.tsv\""));
    }

    #[test]
    fn parsing_rows_round_trips_the_shim_format() {
        let rows = parse_rows(
            "burn\t/path/known_script.py\t1\t123456\n\
             cheap\t/path/known_script.py\t1\t42\n",
        );
        assert_eq!(
            rows,
            vec![
                FunctionTiming {
                    name: "burn".to_string(),
                    file: "/path/known_script.py".to_string(),
                    calls: 1,
                    total_nanos: 123_456,
                },
                FunctionTiming {
                    name: "cheap".to_string(),
                    file: "/path/known_script.py".to_string(),
                    calls: 1,
                    total_nanos: 42,
                },
            ]
        );
    }

    #[test]
    fn parsing_ignores_blank_and_malformed_lines() {
        let rows = parse_rows("burn\tf.py\t1\t10\n\nmalformed\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "burn");
    }
}
