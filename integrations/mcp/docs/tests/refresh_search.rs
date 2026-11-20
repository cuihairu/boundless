//! BW-28-01-03 / 01-04: an incremental refresh re-reads the edited sections
//! (search hits the new text and the current anchor) and drops the deleted
//! ones (no hit and no page left for them).

#[path = "common/mod.rs"]
mod common;

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::mcp::{ServerConfig, Session};
use serde_json::{json, Value};

const VERSION: &str = "HEAD";

fn session_with(index: DocIndex) -> Session {
    Session::new(
        index,
        ServerConfig {
            prefix: "docs".into(),
            locales: vec!["zh".into()],
            version: VERSION.into(),
                    docs_url: String::new(),
        }
    )
}

fn hits(session: &Session, id: i64, query: &str) -> Vec<Value> {
    let reply = session
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": "search_docs", "arguments": {"query": query}},
        }))
        .expect("reply");
    assert_eq!(reply["result"]["isError"], false);
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap())
        .expect("hit list")
}

#[test]
fn a_refreshed_page_searches_its_new_text_and_anchor() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Old heading\n\nObsolete marker wording.\n",
        "one",
    );
    let mut index = DocIndex::default();
    let report = index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("index");
    assert!(report.updated.contains(&"guide/entities.md".to_string()));
    assert_eq!(report.unchanged, 0);

    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## **New** heading\n\nCurrent marker wording.\n",
        "two",
    );
    let report = index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("refresh");
    assert_eq!(report.updated, ["guide/entities.md"]);
    assert!(report.removed.is_empty());

    let session = session_with(index);
    let fresh = hits(&session, 1, "Current marker");
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0]["section"], "New heading");
    assert_eq!(fresh[0]["anchor"], "new-heading");
    assert!(fresh[0]["snippet"].as_str().unwrap().contains("Current marker"));

    // The wording of the earlier revision is gone from the index.
    assert!(hits(&session, 2, "obsolete").is_empty());
}

#[test]
fn a_deleted_page_is_out_of_the_index_and_the_results() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/keep.md",
        "# Keep\n\n## Keep section\n\nStays indexed.\n",
        "keep",
    );
    repo.commit_doc(
        "guide/gone.md",
        "# Gone\n\n## Vanished section\n\nUnique vanishing phrase.\n",
        "gone",
    );
    let mut index = DocIndex::default();
    index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("index");

    repo.delete("guide/gone.md", "drop the page");
    let report = index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("refresh");
    assert_eq!(report.removed, ["guide/gone.md"]);

    let session = session_with(index);
    assert!(session.find(VERSION, "guide/gone.md").is_none());
    assert!(!session.paths(VERSION).iter().any(|p| p.contains("gone.md")));
    let reply = session
        .handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "read_doc", "arguments": {"path": "guide/gone.md"}},
        }))
        .expect("reply");
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("gone.md"));
}

#[test]
fn a_deleted_section_leaves_no_hit_or_link() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/two.md",
        "# Two\n\n## Keep\n\nKept body.\n\n## Drop\n\nUnique dropped body.\n",
        "two",
    );
    let mut index = DocIndex::default();
    index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("index");
    assert!(!hits(&session_with(index.clone()), 1, "dropped").is_empty());

    repo.commit_doc("guide/two.md", "# Two\n\n## Keep\n\nKept body.\n", "trim");
    index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("refresh");

    let session = session_with(index);
    assert!(hits(&session, 2, "dropped").is_empty());
    let page = session.find(VERSION, "guide/two.md").expect("page");
    assert_eq!(
        page.sections.iter().map(|s| s.anchor.as_str()).collect::<Vec<_>>(),
        ["two", "keep"]
    );
}
