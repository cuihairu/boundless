//! BW-15-02-03's Linux editor leg: the fixed map, edited.
//!
//! The acceptance is "固定地图可显示并编辑"; this test pins the edit
//! half: one edit session over a *copy* of the reference fixture space
//! moves the outside chunk `00000000o`'s first model through the
//! WorldEditor save path (`EditorChunkCache::ed_save`), and the file on
//! disk reloads with the edit in it. The session's structural ledger is
//! asserted — the save over an existing file reports "not created", the
//! `processed`/`boundary` strips the reference deletes on save stay
//! deleted, the chunk's other items survive — and an independent second
//! session over a second copy must produce byte-identical output.
//! The fixture itself is never written.
//!
//! ```sh
//! cargo test -p boundless-editor --test edit_reload
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use boundless_editor::{EditReport, copy_space, run_edit_session};
use foundation::math::Vector3;

/// The edit every session performs.
const NEW_POSITION: Vector3 = Vector3::new(25.0, 0.0, 47.0);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("an apps/ parent")
        .parent()
        .expect("the repository root")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    repo_root().join("docs/review/fixtures/chunk_space/res/space_00010001")
}

fn work_root(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("bw150203-edit-reload-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// The fixture's first model transform, read straight off the source
/// file — the before-side the session must report.
fn fixture_first_model_position() -> Vector3 {
    let raw = fs::read(fixture_root().join("00000000o.chunk")).expect("fixture chunk readable");
    let section = resources::datasection::parse("", &raw).expect("fixture chunk parses");
    for index in 0..section.count_children() {
        if let Some(model) = section.open_child(index)
            && model.section_name() == "model"
        {
            return model
                .find_child("transform")
                .map(|t| t.read_vector3("row3", Vector3::ZERO))
                .expect("the first model has a transform");
        }
    }
    panic!("the fixture chunk has no model");
}

#[test]
fn edit_session_moves_the_model_and_survives_a_reload() {
    let old = fixture_first_model_position();
    let root = work_root("one");
    copy_space(&fixture_root(), &root).expect("the fixture copies");

    // The untouched sibling chunks must stay untouched.
    let sibling = root.join("00000001o.chunk");
    let sibling_before = fs::read(&sibling).expect("sibling readable");

    let report = run_edit_session(&root, NEW_POSITION).expect("the session runs");

    assert_eq!(report.chunk, "00000000o");
    assert_eq!(report.old_position, old, "the before-side is the fixture's");
    assert_eq!(report.new_position, NEW_POSITION);
    assert!(
        !report.saved_add,
        "the save edits an existing file, it does not create one"
    );
    assert_ne!(
        report.input_sha, report.output_sha,
        "the file on disk changed"
    );
    assert!(
        report.new_position_reloaded,
        "the reload carries the moved transform"
    );
    assert!(
        report.processed_stripped && report.boundary_stripped,
        "ed_save strips the processed/boundary strips"
    );
    assert!(
        report.model_count_preserved,
        "the chunk keeps its other items ({} models)",
        report.model_count
    );

    // Only the edited chunk changed on disk.
    let sibling_after = fs::read(&sibling).expect("sibling still readable");
    assert_eq!(sibling_before, sibling_after, "the sibling chunk untouched");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn edit_sessions_are_byte_deterministic() {
    let first_root = work_root("det-a");
    let second_root = work_root("det-b");
    copy_space(&fixture_root(), &first_root).expect("the first copy");
    copy_space(&fixture_root(), &second_root).expect("the second copy");

    let first: EditReport = run_edit_session(&first_root, NEW_POSITION).expect("first session");
    let second: EditReport = run_edit_session(&second_root, NEW_POSITION).expect("second session");

    // Same digest chain, same reload ledger — the rerun evidence.
    assert_eq!(first.input_sha, second.input_sha);
    assert_eq!(first.output_sha, second.output_sha);
    assert_eq!(first.old_position, second.old_position);
    assert_eq!(first.model_count, second.model_count);
    assert_eq!(first.new_position_reloaded, second.new_position_reloaded);
    assert_eq!(first.processed_stripped, second.processed_stripped);
    assert_eq!(first.boundary_stripped, second.boundary_stripped);

    // Byte-identical files, including the rewrite of the whole chunk
    // document.
    let first_bytes = fs::read(first_root.join("00000000o.chunk")).expect("first file");
    let second_bytes = fs::read(second_root.join("00000000o.chunk")).expect("second file");
    assert_eq!(first_bytes, second_bytes, "the saved files are identical");

    let _ = fs::remove_dir_all(&first_root);
    let _ = fs::remove_dir_all(&second_root);
}
