//! BW-28-02-04: the links of a read result are resolved, so an anchor a reader
//! can click from a page exists on the rendered site.

#[path = "common/mod.rs"]
mod common;

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::mcp::{ServerConfig, Session};
use serde_json::{json, Value};

const BASE: &str = "https://docs.boundless.dev";
const VERSION: &str = "v1";

fn session(repo: &common::Repo, docs_url: &str) -> Session {
    let mut index = DocIndex::default();
    index
        .add_version(&repo.source, VERSION, "docs", &["zh"])
        .expect("index");
    Session::new(
        index,
        ServerConfig {
            prefix: "docs".into(),
            locales: vec!["zh".into()],
            version: VERSION.into(),
            docs_url: docs_url.into(),
        },
    )
}

fn read_page(session: &Session, path: &str) -> Value {
    let reply = session.handle(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "read_doc", "arguments": {"path": path}},
    }));
    let reply = reply.expect("reply");
    assert_eq!(reply["result"]["isError"], false);
    serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap())
        .expect("payload")
}

#[test]
fn a_link_of_a_read_opens_on_the_rendered_site() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Defining properties\n\nSee [Renaming](#renaming-a-class) and \
         [Cell migration](../cells/migration.md#steps), or the [Site](https://vitepress.dev).\n\n\
         ## Renaming a class\n\nOld names keep working.\n",
        "one",
    );
    repo.commit_doc(
        "cells/migration.md",
        "# Cell migration\n\n## Steps\n\nOne, two, three.\n",
        "two",
    );
    repo.commit_doc(
        "zh/guide/x.md",
        "# 中文页\n\n## 链接节\n\nSee [第一节](#链接节).\n",
        "three",
    );
    repo.tag(VERSION);
    let session = session(&repo, BASE);

    let doc = read_page(&session, "guide/entities.md");
    let links = doc["sections"][1]["links"].as_array().unwrap();
    let targets: Vec<&str> = links.iter().map(|l| l["target"].as_str().unwrap()).collect();
    assert_eq!(
        targets,
        [
            "https://docs.boundless.dev/guide/entities.html#renaming-a-class",
            "https://docs.boundless.dev/cells/migration.html#steps",
            "https://vitepress.dev",
        ]
    );
    assert_eq!(links[1]["label"], "Cell migration");

    // An anchor inside a locale page stays inside that locale.
    let locale = read_page(&session, "zh/guide/x.md");
    assert_eq!(
        locale["sections"][1]["links"][0]["target"],
        "https://docs.boundless.dev/zh/guide/x.html#链接节"
    );

    // Every internal link lands on a page and an anchor that both exist.
    let mut opened = 0;
    for path in ["guide/entities.md", "cells/migration.md", "zh/guide/x.md"] {
        let doc = read_page(&session, path);
        for section in doc["sections"].as_array().unwrap().iter() {
            for link in section["links"].as_array().unwrap().iter() {
                let target = link["target"].as_str().unwrap();
                if !target.starts_with(BASE) {
                    continue;
                }
                let rest = &target[BASE.len() + 1..];
                let anchor = rest.split_once('#').map(|(_, anchor)| anchor);
                let file = rest.split('#').next().unwrap();
                let markdown = format!("{}.md", file.trim_end_matches(".html"));
                assert!(!markdown.is_empty(), "link without a page: {target}");
                let found = session.find(VERSION, &markdown)
                    .unwrap_or_else(|| panic!("no page for {target}"));
                if let Some(anchor) = anchor {
                    assert!(
                        found.sections.iter().any(|s| s.anchor == anchor),
                        "dangling anchor {target}"
                    );
                }
                opened += 1;
            }
        }
    }
    assert!(opened > 0);
}

#[test]
fn a_link_to_an_unknown_anchor_is_seen_as_dangling() {
    let repo = common::Repo::new();
    repo.commit_doc(
        "guide/entities.md",
        "# Entities\n\n## Only section\n\nSee [Nowhere](#no-such-section).\n",
        "one",
    );
    repo.tag(VERSION);
    let session = session(&repo, BASE);
    let doc = read_page(&session, "guide/entities.md");
    let links = doc["sections"][1]["links"].as_array().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0]["target"],
        "https://docs.boundless.dev/guide/entities.html#no-such-section"
    );
    let found = session
        .find(VERSION, "guide/entities.md")
        .expect("the page itself");
    assert!(!found.sections.iter().any(|s| s.anchor == "no-such-section"));
}
