//! BW-28-01-03: an incremental refresh re-reads the edited sections, so search
//! hits the new text and the current anchor rather than the old revision.

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
        },
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
