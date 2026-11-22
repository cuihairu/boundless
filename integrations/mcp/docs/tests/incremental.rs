//! BW-28-01-03: a refresh re-reads only the pages whose content changed.

#[path = "common/mod.rs"]
mod common;

use boundless_docs_mcp::index::DocIndex;

#[test]
fn the_first_refresh_reads_every_page() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/alpha.md", "# Alpha\n\nOld alpha text.\n", "alpha");
    repo.commit_doc("guide/beta.md", "# Beta\n\nStable beta text.\n", "beta");

    let mut index = DocIndex::default();
    let report = index.add_version(&repo.source, "HEAD", "docs", &[]).expect("refresh");
    assert_eq!(report.updated, ["guide/alpha.md", "guide/beta.md", "guide/empty.md"]);
    assert_eq!(report.unchanged, 0);
    assert!(report.removed.is_empty());
}

#[test]
fn an_edit_refreshes_only_that_page() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/alpha.md", "# Alpha\n\nOld alpha text.\n", "alpha");
    repo.commit_doc("guide/beta.md", "# Beta\n\nStable beta text.\n", "beta");

    let mut index = DocIndex::default();
    index.add_version(&repo.source, "HEAD", "docs", &[]).expect("first refresh");

    repo.commit_doc("guide/alpha.md", "# Alpha\n\nFresh alpha text.\n", "edit alpha");
    let report = index.add_version(&repo.source, "HEAD", "docs", &[]).expect("refresh");
    assert_eq!(report.updated, ["guide/alpha.md"]);
    assert_eq!(report.unchanged, 2);
    assert!(report.removed.is_empty());

    let alpha = index.find("HEAD", "guide/alpha.md").expect("alpha");
    assert!(alpha.sections[0].body.contains("Fresh alpha text."));
    assert!(!alpha.sections[0].body.contains("Old alpha text."));
    let beta = index.find("HEAD", "guide/beta.md").expect("beta");
    assert!(beta.sections[0].body.contains("Stable beta text."));
}

#[test]
fn a_moved_heading_refreshes_its_anchor_and_body() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/heading.md",
        "# Heading page\n\n## Old heading\n\nBefore the move.\n",
        "one",
    );
    let mut index = DocIndex::default();
    index.add_version(&repo.source, "HEAD", "docs", &[]).expect("first refresh");
    let old = index.find("HEAD", "guide/heading.md").expect("page");
    assert!(old.sections.iter().any(|s| s.anchor == "old-heading"));

    repo.commit_doc(
        "guide/heading.md",
        "# Heading page\n\n## **New** heading\n\nAfter the move.\n",
        "two",
    );
    let report = index.add_version(&repo.source, "HEAD", "docs", &[]).expect("refresh");
    assert_eq!(report.updated, ["guide/heading.md"]);

    let new = index.find("HEAD", "guide/heading.md").expect("page");
    let heading = new
        .sections
        .iter()
        .find(|s| s.level == 2)
        .expect("h2 section");
    assert_eq!(heading.anchor, "new-heading");
    assert_eq!(heading.title, "New heading");
    assert!(heading.body.contains("After the move."));
}

#[test]
fn refreshing_the_same_commit_changes_nothing() {
    let repo = common::Repo::new();
    let mut index = DocIndex::default();
    index.add_version(&repo.source, "HEAD", "docs", &[]).expect("first refresh");
    let report = index.add_version(&repo.source, "HEAD", "docs", &[]).expect("refresh");
    assert!(report.updated.is_empty());
    assert!(report.removed.is_empty());
    assert!(report.unchanged >= 1);
}
