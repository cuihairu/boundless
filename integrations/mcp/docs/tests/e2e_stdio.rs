//! BW-28-02-01 acceptance evidence: a real local process answers a client over
//! stdio — `initialize`, `tools/list`, then a `tools/call` for the search
//! entry. Nothing here talks to the session in-process.

#[path = "common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use serde_json::json;

const REQUESTS: &str = concat!(
    "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"probe\",\"version\":\"0\"}}}\n",
    "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
    "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"search_docs\",\"arguments\":{\"query\":\"Flags cell\"}}}\n",
);

#[test]
fn a_real_process_serves_a_client_over_stdio() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Defining properties\n\nThe `Flags` of a property decide cell data.\n",
        "entities",
    );
    repo.tag("v1");

    let mut child = Command::new(env!("CARGO_BIN_EXE_boundless-docs-mcp"))
        .arg("--root")
        .arg(repo.path())
        .arg("--version")
        .arg("v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the server");
    let mut stdin = child.stdin.take().expect("stdin");
    write!(stdin, "{REQUESTS}").expect("write the requests");
    drop(stdin); // closing the input ends the session

    let stdout = child.stdout.take().expect("stdout");
    let mut lines = Vec::new();
    for line in BufReader::new(stdout).lines() {
        lines.push(line.expect("line"));
    }
    let status = child.wait().expect("wait");
    assert!(status.success(), "server exited {status}");

    assert_eq!(lines.len(), 3, "one reply per request: {lines:?}");

    let initialize: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(initialize["id"], 1);
    assert_eq!(initialize["result"]["serverInfo"]["name"], "boundless-docs-mcp");

    let tools: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["search_docs", "read_doc"].as_slice());

    let search: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
    assert_eq!(search["result"]["isError"], false);
    let hits: Vec<serde_json::Value> =
        serde_json::from_str(search["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(hits[0]["section"], "Defining properties");
    assert_eq!(hits[0]["version"], "v1");
    assert_eq!(json!(hits[0]["anchor"]), json!("defining-properties"));
}
