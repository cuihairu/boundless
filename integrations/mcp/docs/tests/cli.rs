//! The command line the editor and CI drive: what the flags mean, and how the
//! server refuses a repository it cannot read instead of serving an empty index.

#[path = "common/mod.rs"]
mod common;

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_boundless-docs-mcp"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the server")
        .wait_with_output()
        .expect("wait")
}

#[test]
fn an_unknown_flag_is_refused() {
    let out = run(&["--nope"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown argument: --nope"));
}

#[test]
fn a_flag_without_a_value_is_refused() {
    let out = run(&["--root"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--root needs a value"));
}

#[test]
fn an_unresolvable_version_ends_without_serving() {
    let repo = common::Repo::new();
    let out = run(&["--root", repo.path().to_str().unwrap(), "--version", "no-such-tag"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no-such-tag"));
    assert!(out.stdout.is_empty(), "no index to serve: {:?}", out.stdout);
}

#[test]
fn a_check_without_an_index_file_fails() {
    let out = run(&["--check", "/no/such/index.json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("the index matches"));
}

#[test]
fn a_check_with_an_unreadable_index_fails() {
    let repo = common::Repo::new();
    let broken = repo.path().join("index.json");
    std::fs::write(&broken, "{ this is not an index").expect("write");
    let out = run(&[
        "--root",
        repo.path().to_str().unwrap(),
        "--check",
        broken.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not readable"));
}

#[test]
fn a_fresh_index_matches_the_version_it_came_from() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Defining properties\n\nA property carries `Flags`.\n",
        "entities",
    );
    repo.tag("v1");
    let mut index = boundless_docs_mcp::index::DocIndex::default();
    index
        .add_version(&repo.source, "v1", "docs", &["zh"])
        .expect("index");
    let file = repo.path().join("index.json");
    std::fs::write(&file, index.to_json().expect("json")).expect("write");

    let out = run(&[
        "--root",
        repo.path().to_str().unwrap(),
        "--version",
        "v1",
        "--check",
        file.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("the index matches v1"));
}

#[test]
fn the_locale_list_decides_which_tree_is_indexed() {
    let repo = common::Repo::new();
    repo.commit_doc("zh/guide/entities.md", "# 实体\n\n## 定义属性\n\n属性决定 cell 数据。\n", "zh");
    repo.commit_doc("guide/entities.md", "# Entities\n\n## Defining\n\nA property carries flags.\n", "en");

    // Only the chinese tree is asked for, so that is the tree the search runs on.
    let request = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"search_docs\",\"arguments\":{\"query\":\"cell\"}}}\n";
    let mut child = Command::new(env!("CARGO_BIN_EXE_boundless-docs-mcp"))
        .args(["--root", repo.path().to_str().unwrap(), "--langs", "zh"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the server");
    let mut stdin = child.stdin.take().expect("stdin");
    std::io::Write::write_all(&mut stdin, request.as_bytes()).expect("write");
    drop(stdin);
    let out = child.wait_with_output().expect("wait");

    let text = String::from_utf8_lossy(&out.stdout);
    let reply: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("one reply")).expect("parse");
    let hits: Vec<serde_json::Value> =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        !hits.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(hits.iter().all(|hit| hit["lang"] == "zh"));
    assert!(hits.iter().all(|hit| hit["path"].as_str().unwrap().starts_with("zh/")));
}

#[test]
fn the_document_root_and_the_docs_url_are_passed_on() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Defining properties\n\nA property carries `Flags`.\n",
        "entities",
    );

    // The docs may live anywhere in the repository, and the site may sit on
    // its own host: both are flags, and a source link must show both.
    let request = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"read_doc\",\"arguments\":{\"path\":\"entities.md\"}}}\n";
    let mut child = Command::new(env!("CARGO_BIN_EXE_boundless-docs-mcp"))
        .args([
            "--root",
            repo.path().to_str().unwrap(),
            "--prefix",
            "docs/guide",
            "--docs-url",
            "https://docs.boundless.example/",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the server");
    let mut stdin = child.stdin.take().expect("stdin");
    std::io::Write::write_all(&mut stdin, request.as_bytes()).expect("write");
    drop(stdin);
    let out = child.wait_with_output().expect("wait");

    let text = String::from_utf8_lossy(&out.stdout);
    let reply: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("one reply")).expect("parse");
    assert_eq!(reply["result"]["isError"], false);
    let doc: serde_json::Value = serde_json::from_str(
        reply["result"]["content"][0]["text"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(doc["source"], "https://docs.boundless.example/entities.html");
    assert_eq!(
        doc["sections"][0]["source"],
        "https://docs.boundless.example/entities.html#entities"
    );
    assert_eq!(doc["sections"][1]["source"], "https://docs.boundless.example/entities.html#defining-properties");
}

#[test]
fn an_input_that_cannot_be_read_ends_the_session_with_an_error() {
    // The session reads until the input gives up. A directory opens happily but
    // cannot be read, which is the failure `serve` has to report.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_boundless-docs-mcp"))
        .stdin(Stdio::from(std::fs::File::open(dir.path()).expect("open the directory")))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the server");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("boundless-docs-mcp:"));
}
