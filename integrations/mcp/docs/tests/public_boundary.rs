//! BW-28-01-05: a page marked `private: true` in its front matter is not part
//! of the published surface, so it stays out of the index, the search results
//! and the read tool until the flag goes away.

#[path = "common/mod.rs"]
mod common;

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::mcp::{ServerConfig, Session};
use serde_json::{json, Value};

const VERSION: &str = "HEAD";
const MARKER: &str = "Vault clearance phrase";

const DRAFT: &str = "---\ntitle: Draft\nprivate: true\n---\n# Draft\n\n## Secret internals\n\n\
                     Vault clearance phrase.\n";

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

fn call(session: &Session, id: i64, name: &str, arguments: serde_json::Value) -> Value {
    session
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
        .expect("reply")
}

fn hits(session: &Session, id: i64, query: &str) -> Vec<Value> {
    let reply = call(session, id, "search_docs", json!({"query": query}));
    assert_eq!(reply["result"]["isError"], false);
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap())
        .expect("hit list")
}

/// No page anywhere in the index carries the text a private page would have.
fn index_has_no_marker(index: &DocIndex) {
    for version in index.versions() {
        for doc in &version.docs {
            for section in &doc.sections {
                assert!(
                    !section.body.contains(MARKER) && !section.direct.contains(MARKER),
                    "private text leaked into {}/{}",
                    version.label,
                    doc.path
                );
            }
        }
    }
}

#[test]
fn a_private_page_never_enters_the_index() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/draft.md", DRAFT, "draft");
    repo.commit_doc("zh/guide/internal.md", DRAFT, "draft locale");
    repo.commit_doc(
        "guide/public.md",
        "# Public\n\n## Public section\n\nOpen marker wording.\n",
        "public",
    );
    let mut index = DocIndex::default();
    index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("index");

    let paths = index.paths(VERSION);
    assert!(!paths.iter().any(|p| p.contains("draft.md")));
    assert!(!paths.iter().any(|p| p.contains("internal.md")));
    assert!(paths.iter().any(|p| p.contains("public.md")));
    index_has_no_marker(&index);

    let session = session_with(index);
    assert!(hits(&session, 1, MARKER).is_empty());
    let reply = call(
        &session,
        2,
        "read_doc",
        json!({"path": "guide/draft.md"}),
    );
    assert_eq!(reply["result"]["isError"], true);
    let visible = call(&session, 3, "read_doc", json!({"path": "guide/public.md"}));
    assert_eq!(visible["result"]["isError"], false);
}

#[test]
fn a_page_becomes_public_when_the_private_flag_is_dropped() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/draft.md", DRAFT, "draft");
    let mut index = DocIndex::default();
    index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("index");
    assert!(!index.paths(VERSION).iter().any(|p| p.contains("draft.md")));

    repo.commit_doc(
        "guide/draft.md",
        "---\ntitle: Draft\nprivate: false\n---\n# Draft\n\n## Now published\n\n\
         Vault clearance phrase.\n",
        "publish",
    );
    let report = index.add_version(&repo.source, VERSION, "docs", &["zh"]).expect("refresh");
    assert_eq!(report.updated, ["guide/draft.md"]);

    let session = session_with(index);
    assert!(!hits(&session, 1, MARKER).is_empty());
    let page = call(&session, 2, "read_doc", json!({"path": "guide/draft.md"}));
    assert_eq!(page["result"]["isError"], false);
}
