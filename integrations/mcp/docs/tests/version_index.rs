//! BW-28-01-02: the index separates the same document across Git versions.
//! Fixtures are real git repositories built in temp dirs.

use std::path::Path;
use std::process::Command;

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::source::GitSource;

/// Builds a repo with `docs/guide/entities.md` committed twice, tagged v1/v2.
fn versioned_repo() -> (tempfile::TempDir, GitSource) {
    let dir = tempfile::tempdir().expect("tempdir");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .expect("git runs")
    };
    git(&["init", "-q"]);
    let doc = dir.path().join("docs/guide/entities.md");
    std::fs::create_dir_all(doc.parent().unwrap()).expect("mkdir");
    std::fs::write(&doc, "# Entities v1\n\nOld text about props.\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "v1"]);
    git(&["tag", "v1"]);
    std::fs::write(&doc, "# Entities v2\n\nNew text about props.\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "v2"]);
    git(&["tag", "v2"]);
    let source = GitSource::new(dir.path());
    (dir, source)
}

#[test]
fn the_same_path_exists_under_two_distinct_versions() {
    let (_dir, source) = versioned_repo();
    let mut index = DocIndex::default();
    index.add_version(&source, "v1", "docs", &[]).expect("v1");
    index.add_version(&source, "v2", "docs", &[]).expect("v2");
    assert_eq!(
        index.versions().iter().map(|v| v.label.clone()).collect::<Vec<_>>(),
        ["v1", "v2"]
    );
    let old = index.find("v1", "guide/entities.md").expect("old doc");
    let new = index.find("v2", "guide/entities.md").expect("new doc");
    assert!(old.sections[0].body.contains("Old text about props."));
    assert!(new.sections[0].body.contains("New text about props."));
    assert_ne!(old.sections[0].body, new.sections[0].body);
}

#[test]
fn tags_resolve_to_commits_so_versions_are_hashes() {
    let (_dir, source) = versioned_repo();
    let mut index = DocIndex::default();
    index.add_version(&source, "v2", "docs", &[]).expect("v2");
    let commit = &index.versions()[0].commit;
    assert!(
        commit.len() >= 7 && commit.chars().all(|c| c.is_ascii_hexdigit()),
        "not a commit hash: {commit}"
    );
    // The commit is also usable as a lookup key.
    assert!(index.find(commit, "guide/entities.md").is_some());
}

#[test]
fn the_index_round_trips_through_json() {
    let (_dir, source) = versioned_repo();
    let mut index = DocIndex::default();
    index.add_version(&source, "v1", "docs", &["zh"]).expect("v1");
    let json = index.to_json().expect("serialize");
    let back = DocIndex::from_json(&json).expect("deserialize");
    assert_eq!(back, index);
}

#[test]
fn private_pages_do_not_enter_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = dir.path().join("docs/internal/notes.md");
    std::fs::create_dir_all(doc.parent().unwrap()).expect("mkdir");
    std::fs::write(&doc, "---\nprivate: true\n---\n\n# Internal notes\n").expect("write");
    let other = dir.path().join("docs/guide/open.md");
    std::fs::create_dir_all(other.parent().unwrap()).expect("mkdir");
    std::fs::write(&other, "# Open page\n").expect("write");
    Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .arg("-C")
        .arg(dir.path())
        .args(["init", "-q"])
        .output()
        .expect("init");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .expect("git runs")
    };
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);

    let source = GitSource::new(dir.path());
    let mut index = DocIndex::default();
    index.add_version(&source, "HEAD", "docs", &[]).expect("head");
    assert!(index.find("HEAD", "internal/notes.md").is_none());
    assert!(index.find("HEAD", "guide/open.md").is_some());
}

#[test]
fn non_markdown_and_tooling_files_are_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    for rel in [".vitepress/config.ts", "guide/page.md", "img/logo.png"] {
        let file = dir.path().join("docs").join(rel);
        std::fs::create_dir_all(Path::new(&file).parent().unwrap()).expect("mkdir");
        std::fs::write(&file, "# Placeholder\n").expect("write");
    }
    let git = |args: &[&str]| {
        Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .expect("git runs")
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "one"]);

    let source = GitSource::new(dir.path());
    let mut index = DocIndex::default();
    index.add_version(&source, "HEAD", "docs", &[]).expect("head");
    let paths: Vec<_> = index.versions()[0]
        .docs
        .iter()
        .map(|d| d.path.as_str())
        .collect();
    assert_eq!(paths, ["guide/page.md"]);
}

#[test]
fn a_missing_ref_is_reported_not_panicked() {
    let (_dir, source) = versioned_repo();
    let mut index = DocIndex::default();
    let err = index
        .add_version(&source, "no-such-tag", "docs", &[])
        .expect_err("missing ref");
    assert!(err.to_string().contains("no-such-tag"), "got: {err}");
}
