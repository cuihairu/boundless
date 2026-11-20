//! BW-28-02-01 / BW-28-02-02: the MCP session over stdio — a client can list
//! the search and read entries, and a fixed query hits the expected section.

#[path = "common/mod.rs"]
mod common;

use std::io::Cursor;

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::mcp::{ServerConfig, Session};
use serde_json::{json, Value};

/// An index over a small bilingual docs tree, tagged as v1.
fn session() -> Session {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Defining properties\n\nThe `Flags` of a property decide cell data.\n",
        "entities",
    );
    repo.commit_doc(
        "zh/guide/entities.md",
        "# 实体\n\n## 定义属性\n\n属性的 Flags 决定 cell 数据。\n",
        "zh entities",
    );
    repo.commit_doc(
        "ops/postgresql.md",
        "# PostgreSQL operations\n\n## Backups\n\nRun pg_dump nightly.\n",
        "ops",
    );
    repo.tag("v1");

    let mut index = DocIndex::default();
    index
        .add_version(&repo.source, "v1", "docs", &["zh"])
        .expect("index");
    Session::new(
        index,
        ServerConfig {
            prefix: "docs".into(),
            locales: vec!["zh".to_string()],
            version: "v1".into(),
                    docs_url: String::new(),
        }
    )
}

fn call(session: &Session, request: Value) -> Value {
    session
        .handle(&request)
        .unwrap_or_else(|| panic!("no response for {request}"))
}

fn result_of(session: &Session, id: i64, method: &str, params: Value) -> Value {
    call(
        session,
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
    )["result"]
        .clone()
}

#[test]
fn initialize_reports_the_server_and_protocol() {
    let session = session();
    let result = result_of(
        &session,
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "boundless-docs-mcp");
    assert!(result["capabilities"]["tools"].is_object());
}

#[test]
fn tools_list_exposes_search_and_read() {
    let session = session();
    let tools = result_of(&session, 2, "tools/list", json!({}))["tools"]
        .as_array()
        .expect("tools array")
        .clone();
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["search_docs", "read_doc"].as_slice());
    for tool in &tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
    let search = tools.iter().find(|t| t["name"] == "search_docs").unwrap();
    assert_eq!(search["inputSchema"]["required"].as_array().unwrap(), &vec![json!("query")]);
}

#[test]
fn notifications_and_ping_behave_like_the_spec() {
    let session = session();
    // A notification gets no reply.
    assert!(
        session
            .handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .is_none()
    );
    // A ping gets an empty result under the request id.
    let reply = call(&session, json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}));
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"], json!({}));
}

#[test]
fn an_unknown_method_is_method_not_found() {
    let session = session();
    let reply = call(&session, json!({"jsonrpc": "2.0", "id": 3, "method": "no/such"}));
    assert_eq!(reply["error"]["code"], -32601);
    assert_eq!(reply["id"], 3);
}

#[test]
fn a_fixed_query_hits_the_expected_section() {
    let session = session();
    let result = result_of(
        &session,
        4,
        "tools/call",
        json!({"name": "search_docs", "arguments": {"query": "Flags cell"}}),
    );
    assert_eq!(result["isError"], false);
    let hits: Vec<Value> = serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
        .expect("hit list");
    assert!(!hits.is_empty());
    let top = &hits[0];
    assert_eq!(top["path"], "guide/entities.md");
    assert_eq!(top["section"], "Defining properties");
    assert_eq!(top["anchor"], "defining-properties");
    assert_eq!(top["version"], "v1");
    assert!(top["snippet"].as_str().unwrap().contains("Flags"));
}

#[test]
fn the_language_filter_selects_the_locale_tree() {
    let session = session();
    let result = result_of(
        &session,
        5,
        "tools/call",
        json!({"name": "search_docs", "arguments": {"query": "属性", "lang": "zh"}}),
    );
    let hits: Vec<Value> = serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
        .expect("hit list");
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h["lang"] == "zh"));
    assert!(hits.iter().all(|h| h["path"].as_str().unwrap().starts_with("zh/")));
}

#[test]
fn an_unknown_tool_is_an_error_result_not_a_crash() {
    let session = session();
    let reply = call(
        &session,
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "nope", "arguments": {}}}),
    );
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("nope"));
}

#[test]
fn missing_required_arguments_are_reported() {
    let session = session();
    let reply = call(
        &session,
        json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "search_docs", "arguments": {}}}),
    );
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply["error"].is_null());
}

#[test]
fn the_stdio_loop_answers_line_by_line() {
    let session = session();
    let input = Cursor::new(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "not json at all\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
    ));
    let mut output = Vec::new();
    boundless_docs_mcp::mcp::serve(session, input, &mut output).expect("serve");
    let text = String::from_utf8(output).expect("utf-8");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "one reply per request, none for the notification");
    let first: Value = serde_json::from_str(lines[0]).expect("parse");
    assert_eq!(first["id"], 1);
    assert_eq!(first["result"]["serverInfo"]["name"], "boundless-docs-mcp");
    let second: Value = serde_json::from_str(lines[1]).expect("parse");
    assert_eq!(second["id"], serde_json::Value::Null);
    assert_eq!(second["error"]["code"], -32700);
    let third: Value = serde_json::from_str(lines[2]).expect("parse");
    assert_eq!(third["id"], 2);
    assert!(third["result"]["tools"].is_array());
}

#[test]
fn an_empty_query_is_rejected() {
    let session = session();
    let reply = call(
        &session,
        json!({"jsonrpc": "2.0", "id": 12, "method": "tools/call", "params": {"name": "search_docs", "arguments": {"query": "   "}}}),
    );
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("non-empty"));
}

#[test]
fn a_tool_call_without_a_name_is_rejected() {
    let session = session();
    let reply = call(
        &session,
        json!({"jsonrpc": "2.0", "id": 13, "method": "tools/call", "params": {}}),
    );
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("missing tool name"));
}

#[test]
fn the_limit_limits_the_hits() {
    let session = session();
    let all = result_of(
        &session,
        10,
        "tools/call",
        json!({"name": "search_docs", "arguments": {"query": "entities"}}),
    );
    let limited = result_of(
        &session,
        11,
        "tools/call",
        json!({"name": "search_docs", "arguments": {"query": "entities", "limit": 1}}),
    );
    let hits = |result: &Value| -> Vec<Value> {
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    assert!(hits(&all).len() >= 2);
    assert_eq!(hits(&limited).len(), 1);
}
