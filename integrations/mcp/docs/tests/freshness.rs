//! BW-28-03-02 acceptance evidence: the freshness gate, run as a real process,
//! passes on an index that matches its ref and fails when a document changed
//! without a rebuild.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

use boundless_docs_mcp::index::DocIndex;

const PAGE: &str = "# Entities\n\n## Defining properties\n\n\
                   Flags decide the data of a cell.\n";

fn index_json(repo: &common::Repo, version: &str) -> PathBuf {
    let mut index = DocIndex::default();
    index
        .add_version(&repo.source, version, "docs", &["zh"])
        .expect("index");
    let file = repo.path().join("index.json");
    std::fs::write(&file, index.to_json().expect("json")).expect("write the index");
    file
}

fn check(repo: &common::Repo, file: &std::path::Path, version: &str) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_boundless-docs-mcp"))
        .arg("--check")
        .arg(file)
        .arg("--root")
        .arg(repo.path())
        .arg("--version")
        .arg(version)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the gate runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_index_that_matches_its_ref_passes() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "entities");
    let file = index_json(&repo, "HEAD");

    let (code, stdout, stderr) = check(&repo, &file, "HEAD");
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("matches HEAD"), "{stdout}");
}

#[test]
fn a_document_change_without_a_rebuild_fails() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "entities");
    let file = index_json(&repo, "HEAD");

    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Defining properties\n\n\
         Attributes decide the data of a cell.\n",
        "edited",
    );

    let (code, _, stderr) = check(&repo, &file, "HEAD");
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("the index no longer matches"), "{stderr}");
    assert!(stderr.contains("changed: guide/entities.md"), "{stderr}");
}

#[test]
fn a_document_added_without_a_rebuild_fails() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "entities");
    let file = index_json(&repo, "HEAD");

    repo.commit_doc("guide/cells.md", "# Cells\n\n## Cell data\n\nReplicated.\n", "cells");

    let (code, _, stderr) = check(&repo, &file, "HEAD");
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("added: guide/cells.md"), "{stderr}");
}

#[test]
fn a_document_removed_without_a_rebuild_fails() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "entities");
    let file = index_json(&repo, "HEAD");

    repo.delete("guide/entities.md", "drop the page");

    let (code, _, stderr) = check(&repo, &file, "HEAD");
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("removed: guide/entities.md"), "{stderr}");
}

#[test]
fn a_version_the_index_never_saw_fails() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "entities");
    repo.tag("v1");
    let file = index_json(&repo, "v1");
    repo.commit_doc("guide/later.md", "# Later\n\n## Added later\n\nNew page.\n", "later");
    repo.tag("v2");

    let (code, _, stderr) = check(&repo, &file, "v2");
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("missing version v2"), "{stderr}");
}

#[test]
fn an_unresolvable_ref_fails_with_what_git_says() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "entities");
    repo.tag("v1");
    let file = index_json(&repo, "v1");

    let (code, _, stderr) = check(&repo, &file, "v9");
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("reference v9"), "{stderr}");
}
