//! BW-28-03-03 acceptance evidence: the gate, run as a real process, names the
//! link that does not open — and passes when every link opens.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

use boundless_docs_mcp::index::DocIndex;

const ENTITIES: &str = "# Entities\n\n## Defining properties\n\n\
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
fn a_link_to_a_missing_page_fails_the_gate() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", ENTITIES, "entities");
    repo.commit_doc(
        "guide/near.md",
        "# Near\n\n## Here\n\nSee [Gone](./gone.md).\n",
        "near",
    );
    let file = index_json(&repo, "HEAD");

    let (code, _, stderr) = check(&repo, &file, "HEAD");
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("no such page"), "{stderr}");
    assert!(stderr.contains("docs/guide/gone.html"), "{stderr}");
}

#[test]
fn a_link_to_a_missing_anchor_fails_the_gate() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", ENTITIES, "entities");
    repo.commit_doc(
        "guide/near.md",
        "# Near\n\n## Here\n\nSee [Nope](./entities.md#nope).\n",
        "near",
    );
    let file = index_json(&repo, "HEAD");

    let (code, _, stderr) = check(&repo, &file, "HEAD");
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("no such anchor"), "{stderr}");
    assert!(stderr.contains("docs/guide/entities.html#nope"), "{stderr}");
}

#[test]
fn a_link_that_opens_passes_the_gate() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", ENTITIES, "entities");
    repo.commit_doc(
        "guide/near.md",
        "# Near\n\n## Here\n\nSee [Defining properties](./entities.md#defining-properties).\n",
        "near",
    );
    let file = index_json(&repo, "HEAD");

    let (code, stdout, stderr) = check(&repo, &file, "HEAD");
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("the index matches HEAD"), "{stdout}");
}
