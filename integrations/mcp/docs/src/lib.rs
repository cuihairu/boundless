//! Versioned documentation index and the Model Context Protocol server that
//! queries it (`integrations/mcp/docs`).
//!
//! The index is built from the public VitePress markdown source (`docs/**/*.md`)
//! at one Git version per index, refreshed incrementally, and served over stdio
//! JSON-RPC so VS Code or any MCP client can search and read the docs of the
//! version a workspace is at.

pub mod markdown;
