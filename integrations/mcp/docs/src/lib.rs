//! Versioned documentation index and the Model Context Protocol server that
//! queries it (`integrations/mcp/docs`).
//!
//! The index is built from the public VitePress markdown source (`docs/**/*.md`)
//! at one Git version per indexed version, refreshed incrementally, and served
//! over stdio JSON-RPC so VS Code or any MCP client can search and read the
//! docs of the version a workspace is at.

pub mod errors;
pub mod index;
pub mod links;
pub mod markdown;
pub mod mcp;
pub mod search;
pub mod source;

pub use errors::DocsError;
