//! BW-28-02-03: a versioned read answers with the page path, the version it
//! came from, its sections and a link to the rendered source.

#[path = "common/mod.rs"]
mod common;

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::mcp::{ServerConfig, Session};
use serde_json::{json, Value};

const PAGE: &str = "# Entities\n\n## Defining properties\n\nFlags and attributes.\n\n\
                   ## Renaming a class\n\nOld names keep working.\n";

fn index_of(repo: &common::Repo, version: &str) -> (DocIndex, String) {
    let mut index = DocIndex::default();
    index
        .add_version(&repo.source, version, "docs", &["zh"])
        .expect("index");
    let commit = index.version(version).expect("version").commit.clone();
    (index, commit)
}

fn session(index: DocIndex, version: &str, docs_url: &str) -> Session {
    Session::new(
        index,
        ServerConfig {
            prefix: "docs".into(),
            locales: vec!["zh".into()],
            version: version.into(),
            docs_url: docs_url.into(),
        },
    )
}

fn read_doc(session: &Session, id: i64, arguments: Value) -> Value {
    session
        .handle(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": "read_doc", "arguments": arguments},
        }))
        .expect("reply")
}

fn payload(reply: &Value) -> Value {
    assert_eq!(reply["result"]["isError"], false);
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap())
        .expect("page payload")
}

#[test]
fn a_read_returns_path_version_sections_and_a_source_link() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "one");
    repo.commit_doc("zh/guide/x.md", "# 中文页\n\n## 第一节\n\n中文正文。\n", "locale");
    repo.tag("v1");
    let (index, commit) = index_of(&repo, "v1");
    let session = session(index, "v1", "https://docs.boundless.dev");

    let page = payload(&read_doc(
        &session,
        1,
        json!({"path": "guide/entities.md"}),
    ));
    assert_eq!(page["path"], "guide/entities.md");
    assert_eq!(page["version"], "v1");
    assert_eq!(page["commit"], commit);
    assert_eq!(page["title"], "Entities");
    assert_eq!(page["source"], "https://docs.boundless.dev/guide/entities.html");
    let sections = page["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 3);
    assert_eq!(sections[2]["title"], "Renaming a class");
    assert_eq!(sections[2]["anchor"], "renaming-a-class");
    assert_eq!(
        sections[2]["source"],
        "https://docs.boundless.dev/guide/entities.html#renaming-a-class"
    );
    assert!(sections[2]["body"].as_str().unwrap().contains("Old names"));

    // A locale page answers for its own language and its own rendered path.
    let locale = payload(&read_doc(&session, 2, json!({"path": "zh/guide/x.md"})));
    assert_eq!(locale["lang"], "zh");
    assert_eq!(locale["source"], "https://docs.boundless.dev/zh/guide/x.html");
}

#[test]
fn a_read_at_a_pinned_version_sees_that_versions_text() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "one");
    repo.tag("v1");
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Only here in v2\n\nLate text.\n",
        "two",
    );
    repo.tag("v2");

    let (index, v1_commit) = index_of(&repo, "v1");
    let v1 = session(index, "v1", "");
    let page = payload(&read_doc(&v1, 1, json!({"path": "guide/entities.md"})));
    assert_eq!(page["version"], "v1");
    assert_eq!(page["commit"], v1_commit);
    let sections = page["sections"].as_array().unwrap();
    assert!(sections.iter().any(|s| s["title"] == "Renaming a class"));
    assert!(!sections.iter().any(|s| s["title"] == "Only here in v2"));

    let (index, _) = index_of(&repo, "v2");
    let v2 = session(index, "v2", "");
    let page = payload(&read_doc(&v2, 1, json!({"path": "guide/entities.md"})));
    assert_eq!(page["version"], "v2");
    let sections = page["sections"].as_array().unwrap();
    assert!(sections.iter().any(|s| s["title"] == "Only here in v2"));
    assert!(!sections.iter().any(|s| s["title"] == "Renaming a class"));

    // The commit a label resolved to answers the same way as the label does.
    let (index, commit) = index_of(&repo, "v1");
    let reader = session(index, "v1", "");
    let by_commit = payload(&read_doc(
        &reader,
        1,
        json!({"path": "guide/entities.md", "version": commit}),
    ));
    assert_eq!(by_commit["version"], "v1");
    assert_eq!(by_commit["commit"], v1_commit);
}

#[test]
fn an_anchored_read_returns_only_that_section() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "one");
    repo.tag("v1");
    let (index, _) = index_of(&repo, "v1");
    let reader = session(index, "v1", "");

    let page = payload(&read_doc(
        &reader,
        1,
        json!({"path": "guide/entities.md", "anchor": "renaming-a-class"}),
    ));
    let sections = page["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0]["anchor"], "renaming-a-class");

    let reply = read_doc(
        &reader,
        2,
        json!({"path": "guide/entities.md", "anchor": "no-such-anchor"}),
    );
    assert_eq!(reply["result"]["isError"], true);
}

#[test]
fn a_read_without_a_docs_url_links_repository_relative() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "one");
    repo.tag("v1");
    let (index, _) = index_of(&repo, "v1");
    let reader = session(index, "v1", "");
    let page = payload(&read_doc(&reader, 1, json!({"path": "guide/entities.md"})));
    assert_eq!(page["source"], "docs/guide/entities.html");
    assert_eq!(page["sections"][2]["source"], "docs/guide/entities.html#renaming-a-class");
}

#[test]
fn a_read_reports_missing_pages_and_versions_as_tool_errors() {
    let repo = common::Repo::new();
    repo.commit_doc("guide/entities.md", PAGE, "one");
    repo.tag("v1");
    let (index, _) = index_of(&repo, "v1");
    let reader = session(index, "v1", "");

    let missing = read_doc(&reader, 1, json!({"path": "guide/absent.md"}));
    assert_eq!(missing["result"]["isError"], true);
    assert!(
        missing["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("absent.md")
    );
    let bad_version = read_doc(
        &reader,
        2,
        json!({"path": "guide/entities.md", "version": "v9"}),
    );
    assert_eq!(bad_version["result"]["isError"], true);
    assert!(bad_version["result"]["content"][0]["text"].as_str().unwrap().contains("v9"));
}
