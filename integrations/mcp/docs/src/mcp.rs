//! The Model Context Protocol session: JSON-RPC 2.0, one message per line,
//! over stdio. Tool-level failures are results with `isError`, protocol-level
//! failures are JSON-RPC errors, as the spec draws that line.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::index::DocIndex;
use crate::links::source_link;
use crate::search::{search, DEFAULT_LIMIT};
use crate::DocsError;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "boundless-docs-mcp";

/// How the server is pointed at a documentation tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    /// The docs directory inside the repository (`docs`).
    pub prefix: String,
    /// Locale directories inside the docs root.
    pub locales: Vec<String>,
    /// The ref label that tools default to.
    pub version: String,
    /// The base URL of the rendered docs site, for source links. Empty keeps
    /// links repository-relative.
    pub docs_url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            prefix: "docs".to_string(),
            locales: vec!["zh".to_string()],
            version: "HEAD".to_string(),
            docs_url: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    index: DocIndex,
    config: ServerConfig,
}

/// The params of a request, defaulting to an empty object.
fn params(request: &Value) -> Value {
    request.get("params").cloned().unwrap_or_else(|| json!({}))
}

impl Session {
    pub fn new(index: DocIndex, config: ServerConfig) -> Self {
        Self { index, config }
    }

    /// The page at `path` in `version`, if it is indexed.
    pub fn find(&self, version: &str, path: &str) -> Option<&crate::index::DocEntry> {
        self.index.find(version, path)
    }

    /// The page paths indexed for `version`.
    pub fn paths(&self, version: &str) -> Vec<String> {
        self.index.paths(version)
    }

    /// Answers one request, or `None` for notifications.
    pub fn handle(&self, request: &Value) -> Option<Value> {
        // A notification, or a message without an id: never reply.
        let id = request.get("id")?.clone();
        let method = request.get("method").and_then(Value::as_str);
        let result = match method {
            Some("initialize") => self.initialize(),
            Some("ping") => json!({}),
            Some("tools/list") => json!({"tools": self.tools()}),
            Some("tools/call") => {
                let args = params(request);
                self.call_tool(&args)
            }
            Some(other) => {
                return Some(error_response(
                    id,
                    -32601,
                    format!("method not found: {other}"),
                ))
            }
            None => {
                return Some(error_response(
                    id,
                    -32600,
                    "invalid request: missing method".to_string(),
                ))
            }
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn initialize(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
        })
    }

    fn tools(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "search_docs",
                "description": "Search the documentation of one version; returns sections with their anchors.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Terms to find in section titles and bodies"},
                        "version": {"type": "string", "description": "Git ref label or commit; defaults to the workspace version"},
                        "lang": {"type": "string", "description": "Only pages of this language, e.g. en or zh"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Maximum hits (default 10)"}
                    },
                    "required": ["query"]
                }
            }),
            json!({
                "name": "read_doc",
                "description": "Read one page, or one anchored section of it, at one version.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Page path relative to the docs root, e.g. guide/entities.md"},
                        "version": {"type": "string", "description": "Git ref label or commit; defaults to the workspace version"},
                        "anchor": {"type": "string", "description": "Only the section carrying this anchor"}
                    },
                    "required": ["path"]
                }
            }),
        ]
    }

    fn call_tool(&self, params: &Value) -> Value {
        let name = match params.get("name").and_then(Value::as_str) {
            Some(name) => name,
            None => return tool_error("missing tool name"),
        };
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        match name {
            "search_docs" => self.search_docs(&args),
            "read_doc" => self.read_doc(&args),
            other => tool_error(&format!("unknown tool: {other}")),
        }
    }

    fn search_docs(&self, args: &Value) -> Value {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return tool_error("search_docs requires a query");
        };
        if query.trim().is_empty() {
            return tool_error("search_docs requires a non-empty query");
        }
        let version = args
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or(&self.config.version);
        let lang = args.get("lang").and_then(Value::as_str);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|limit| limit.clamp(1, 50) as usize)
            .unwrap_or(DEFAULT_LIMIT);
        let hits = search(&self.index, version, query, lang, limit);
        ok_result(&json!(hits))
    }

    fn read_doc(&self, args: &Value) -> Value {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return tool_error("read_doc requires a path");
        };
        let version = args
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or(&self.config.version);
        let Some(pages) = self.index.version(version) else {
            return tool_error(&format!("unknown version: {version}"));
        };
        let Some(page) = pages.docs.iter().find(|doc| doc.path == path) else {
            return tool_error(&format!("no page at {path} in version {version}"));
        };
        let base = &self.config.docs_url;
        let sections: Vec<Value> = page
            .sections
            .iter()
            .map(|section| section_json(section, base, &page.path))
            .collect();
        let sections = match args.get("anchor").and_then(Value::as_str) {
            Some(anchor) => {
                let picked: Vec<Value> = sections
                    .iter()
                    .filter(|section| section["anchor"] == anchor)
                    .cloned()
                    .collect();
                if picked.is_empty() {
                    return tool_error(&format!(
                        "no section #{anchor} in {path} at {version}"
                    ));
                }
                picked
            }
            None => sections,
        };
        ok_result(&json!({
            "version": pages.label,
            "commit": pages.commit,
            "path": page.path,
            "lang": page.lang,
            "title": page.title,
            "source": source_link(base, &page.path, None),
            "sections": sections,
        }))
    }
}

fn section_json(section: &crate::index::SectionEntry, docs_url: &str, page: &str) -> Value {
    json!({
        "title": section.title,
        "anchor": section.anchor,
        "level": section.level,
        "line": section.line,
        "body": section.body,
        "source": source_link(docs_url, page, Some(&section.anchor)),
    })
}

fn ok_result(payload: &Value) -> Value {
    json!({
        "content": [{"type": "text", "text": payload.to_string()}],
        "isError": false,
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true,
    })
}

fn error_response(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}

/// Serves one session, reading one JSON message per line until input ends.
pub fn serve(
    session: Session,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<(), DocsError> {
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(trimmed) {
            Ok(request) => session.handle(&request),
            Err(err) => Some(error_response(
                Value::Null,
                -32700,
                format!("parse error: {err}"),
            )),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
}
