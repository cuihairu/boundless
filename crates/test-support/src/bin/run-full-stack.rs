//! Full-stack comparison run (BW-00-04-04).
//!
//! Usage: `run-full-stack <input-file> <original-bin-dir> <new-binary> <out-dir>`
//!
//! Feeds one shared input (a role list, one role per line) to both stacks and
//! writes two evidence tables into `<out-dir>`: `original.tsv` and `new.tsv`.
//! Each row records whether that stack produced a clean observable start for
//! the role, and the detail — the captured output, or the named reason it
//! could not run here.

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: run-full-stack <input-file> <original-bin-dir> <new-binary> <out-dir>");
        return ExitCode::from(2);
    }

    let input_path = PathBuf::from(&args[1]);
    let original_dir = PathBuf::from(&args[2]);
    let new_binary = PathBuf::from(&args[3]);
    let out_dir = PathBuf::from(&args[4]);

    let input_text = match std::fs::read_to_string(&input_path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: cannot read {}: {err}", input_path.display());
            return ExitCode::from(2);
        }
    };
    let roles = test_support::full_stack::parse_roles(&input_text);
    if roles.is_empty() {
        eprintln!("error: {} has no roles", input_path.display());
        return ExitCode::from(2);
    }
    if !new_binary.is_file() {
        eprintln!("error: new stack binary {} not found", new_binary.display());
        return ExitCode::from(2);
    }
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        eprintln!("error: cannot create {}: {err}", out_dir.display());
        return ExitCode::from(1);
    }

    let (original, new) = test_support::full_stack::compare(&roles, &original_dir, &new_binary);

    let original_tsv = test_support::full_stack::render_side(&original);
    let new_tsv = test_support::full_stack::render_side(&new);
    for (name, text) in [("original.tsv", original_tsv), ("new.tsv", new_tsv)] {
        let path = out_dir.join(name);
        if let Err(err) = std::fs::write(&path, &text) {
            eprintln!("error: cannot write {}: {err}", path.display());
            return ExitCode::from(1);
        }
    }

    let ran = new
        .iter()
        .filter(|e| e.status == test_support::full_stack::Status::Ran)
        .count();
    println!(
        "{} roles probed on both stacks; evidence written to {}",
        roles.len(),
        Path::new(&args[4]).display()
    );
    println!(
        "  new stack started cleanly for {ran}/{} roles",
        roles.len()
    );
    for ev in &original {
        println!(
            "  original {}: {}",
            ev.role,
            if ev.detail.contains("original-full-stack") {
                "blocked (full start not provisioned here)"
            } else {
                "binary absent"
            }
        );
    }
    ExitCode::SUCCESS
}
