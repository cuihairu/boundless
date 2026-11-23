//! BW-28-03-01: three real questions — EntityDef properties, Cell migration and
//! PostgreSQL operations — each land on the reference that answers it, and each
//! query stays inside its own topic.

#[path = "common/mod.rs"]
mod common;

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::mcp::{ServerConfig, Session};
use serde_json::{json, Value};

const VERSION: &str = "v1";

const ENTITIES: &str = "# 实体\n\n## 定义属性\n\n\
                        EntityDef 用 Flags 声明属性，例如 `CELL_PUBLIC_BASE` 和 `ALL_CLIENTS`。\n\
                        属性名必须是合法的 Python 标识符。\n\n\
                        ## 继承与改名\n\n旧属性名继续可用，改名时保留兼容别名。\n";

const MIGRATION: &str = "# Cell 迁移\n\n## 迁移步骤\n\n\
                        先做快照，再按 v1 到 v2 的顺序执行，最后校验版本号。\n\n\
                        ## 回滚\n\n任意阶段失败都可以回滚到迁移前的快照。\n";

const POSTGRESQL: &str = "# 数据库运维\n\n## PostgreSQL 备份与恢复\n\n\
                        使用 pg_dump 做每日备份，pg_restore 从备份恢复。\n\n\
                        ## 索引维护\n\n定期重建索引和统计信息。\n";

fn session(repo: &common::Repo, version: &str, docs_url: &str) -> Session {
    let mut index = DocIndex::default();
    index
        .add_version(&repo.source, version, "docs", &["zh"])
        .expect("index");
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

fn search(session: &Session, id: i64, query: &str) -> Vec<Value> {
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
fn the_three_references_are_all_indexed() {
    let repo = common::Repo::new();
    repo.commit_doc("zh/guide/entities.md", ENTITIES, "entities");
    repo.commit_doc("zh/guide/cell/migration.md", MIGRATION, "migration");
    repo.commit_doc("zh/ops/postgresql.md", POSTGRESQL, "postgresql");
    repo.tag(VERSION);
    let session = session(&repo, VERSION, "");

    let paths = session.paths(VERSION);
    // Four pages: the three references plus the placeholder the fixture seeds.
    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&"zh/guide/entities.md".to_string()));
    assert!(paths.contains(&"zh/guide/cell/migration.md".to_string()));
    assert!(paths.contains(&"zh/ops/postgresql.md".to_string()));
}

#[test]
fn an_entity_def_query_hits_the_property_reference() {
    let repo = common::Repo::new();
    repo.commit_doc("zh/guide/entities.md", ENTITIES, "entities");
    repo.commit_doc("zh/guide/cell/migration.md", MIGRATION, "migration");
    repo.commit_doc("zh/ops/postgresql.md", POSTGRESQL, "postgresql");
    repo.tag(VERSION);
    let session = session(&repo, VERSION, "");

    let hits = search(&session, 1, "EntityDef Flags");
    assert_eq!(hits.len(), 1);
    assert!(hits.iter().all(|hit| hit["path"] == "zh/guide/entities.md"));
    assert_eq!(hits[0]["path"], "zh/guide/entities.md");
    assert_eq!(hits[0]["version"], "v1");
    assert_eq!(hits[0]["section"], "定义属性");
    assert!(hits[0]["snippet"].as_str().unwrap().contains("EntityDef"));
}

#[test]
fn a_cell_migration_query_hits_the_migration_reference() {
    let repo = common::Repo::new();
    repo.commit_doc("zh/guide/entities.md", ENTITIES, "entities");
    repo.commit_doc("zh/guide/cell/migration.md", MIGRATION, "migration");
    repo.commit_doc("zh/ops/postgresql.md", POSTGRESQL, "postgresql");
    repo.tag(VERSION);
    let session = session(&repo, VERSION, "");

    let hits = search(&session, 1, "迁移步骤 快照");
    assert_eq!(hits[0]["path"], "zh/guide/cell/migration.md");
    assert_eq!(hits[0]["section"], "迁移步骤");
    assert!(hits.iter().all(|hit| hit["path"] == "zh/guide/cell/migration.md"));
}

#[test]
fn a_postgresql_query_hits_the_operations_reference() {
    let repo = common::Repo::new();
    repo.commit_doc("zh/guide/entities.md", ENTITIES, "entities");
    repo.commit_doc("zh/guide/cell/migration.md", MIGRATION, "migration");
    repo.commit_doc("zh/ops/postgresql.md", POSTGRESQL, "postgresql");
    repo.tag(VERSION);
    let session = session(&repo, VERSION, "");

    let hits = search(&session, 1, "PostgreSQL 备份");
    assert_eq!(hits[0]["path"], "zh/ops/postgresql.md");
    assert_eq!(hits[0]["section"], "PostgreSQL 备份与恢复");
    assert!(
        hits.iter().all(|hit| hit["path"] == "zh/ops/postgresql.md"),
        "a query about PostgreSQL should not leave its reference"
    );
    assert!(hits[0]["snippet"].as_str().unwrap().contains("pg_dump"));
}

#[test]
fn a_hit_reads_back_through_the_read_tool() {
    let repo = common::Repo::new();
    repo.commit_doc("zh/guide/entities.md", ENTITIES, "entities");
    repo.tag(VERSION);
    let session = session(&repo, VERSION, "https://docs.boundless.dev");

    let hits = search(&session, 1, "EntityDef Flags");
    assert_eq!(hits.len(), 1);
    let path = hits[0]["path"].as_str().unwrap().to_string();
    let anchor = hits[0]["anchor"].as_str().unwrap().to_string();

    let reply = session.handle(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "read_doc",
            "arguments": {"path": path, "version": "v1", "anchor": anchor},
        },
    })).expect("reply");
    assert_eq!(reply["result"]["isError"], false);
    let page: Value = serde_json::from_str(
        reply["result"]["content"][0]["text"].as_str().unwrap(),
    )
    .expect("payload");
    assert_eq!(page["sections"].as_array().unwrap().len(), 1);
    assert_eq!(page["sections"][0]["anchor"], anchor);
    assert_eq!(
        page["sections"][0]["source"],
        "https://docs.boundless.dev/zh/guide/entities.html#定义属性"
    );
}
