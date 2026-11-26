//! BW-15-02-03's Linux editor leg: copy the fixed map into a temporary
//! directory, edit it through the WorldEditor save path, verify the
//! edit survives a reload, print the evidence and exit.
//!
//! ```sh
//! cargo run -p boundless-editor -- \
//!     [--src docs/review/fixtures/chunk_space/res/space_00010001] \
//!     [--keep]
//! ```
//!
//! The fixture itself is never written; the session runs against a copy
//! under the system temp directory (`--keep` leaves it in place for
//! inspection instead of deleting it).

use std::path::PathBuf;
use std::process::ExitCode;

use boundless_editor::{EditReport, copy_space, run_edit_session};
use foundation::math::Vector3;

/// The edit the leg performs: move the chunk's first model from its
/// fixture transform to a fixed position on the ground plane.
const NEW_POSITION: Vector3 = Vector3::new(25.0, 0.0, 47.0);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut src: Option<String> = None;
    let mut keep = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--src" => match iter.next().cloned() {
                Some(v) => src = Some(v),
                None => {
                    eprintln!("boundless-editor: --src needs a space directory");
                    return ExitCode::FAILURE;
                }
            },
            "--keep" => keep = true,
            other => {
                eprintln!("boundless-editor: unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    let fixture_root = match src {
        Some(src) => PathBuf::from(src),
        None => PathBuf::from("docs/review/fixtures/chunk_space/res/space_00010001"),
    };

    // The session edits a copy; the fixture stays read-only.
    let work_root = std::env::temp_dir().join(format!(
        "bw150203-editor-{}-{}",
        std::process::id(),
        fixture_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("space")
    ));
    let _ = std::fs::remove_dir_all(&work_root);
    if let Err(err) = copy_space(&fixture_root, &work_root) {
        eprintln!("boundless-editor: {err}");
        return ExitCode::FAILURE;
    }

    let report = match run_edit_session(&work_root, NEW_POSITION) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("boundless-editor: {err}");
            let _ = std::fs::remove_dir_all(&work_root);
            return ExitCode::FAILURE;
        }
    };
    print_report(&report);

    // The acceptance: the edit is in the file on disk.
    let ok = report.new_position_reloaded
        && report.processed_stripped
        && report.boundary_stripped
        && report.model_count_preserved
        && report.input_sha != report.output_sha
        && !report.saved_add;
    if !keep {
        let _ = std::fs::remove_dir_all(&work_root);
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        eprintln!("boundless-editor: the reload does not carry the edit");
        ExitCode::FAILURE
    }
}

fn print_report(report: &EditReport) {
    println!("space={}", report.space_root.display());
    println!(
        "chunk={} first-model {} {} {} -> {} {} {}",
        report.chunk,
        report.old_position.x,
        report.old_position.y,
        report.old_position.z,
        report.new_position.x,
        report.new_position.y,
        report.new_position.z,
    );
    println!(
        "save input={:016x} output={:016x} created={}",
        report.input_sha, report.output_sha, report.saved_add
    );
    println!(
        "reload moved={} processed_stripped={} boundary_stripped={} models={} (preserved={})",
        report.new_position_reloaded,
        report.processed_stripped,
        report.boundary_stripped,
        report.model_count,
        report.model_count_preserved,
    );
}
