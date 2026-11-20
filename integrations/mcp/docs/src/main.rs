//! `boundless-docs-mcp`: serves the versioned documentation index over stdio.
//!
//! The default version is the ref the workspace is at (`HEAD`), so every query
//! answers for the version the workspace actually carries.

use std::io::{self};

use boundless_docs_mcp::index::DocIndex;
use boundless_docs_mcp::mcp::{serve, ServerConfig, Session};
use boundless_docs_mcp::source::GitSource;

struct Options {
    root: String,
    prefix: String,
    version: String,
    locales: Vec<String>,
    docs_url: String,
}

fn parse_args(args: Vec<String>) -> Result<Options, String> {
    let mut options = Options {
        root: std::env::var("BW_DOCS_ROOT").unwrap_or_else(|_| ".".to_string()),
        prefix: "docs".to_string(),
        version: "HEAD".to_string(),
        locales: vec!["zh".to_string()],
        docs_url: String::new(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--root" | "--prefix" | "--version" | "--langs" | "--docs-url" => {
                args.next().ok_or_else(|| format!("{arg} needs a value"))?
            }
            other => return Err(format!("unknown argument: {other}")),
        };
        match arg.as_str() {
            "--root" => options.root = value,
            "--prefix" => options.prefix = value,
            "--version" => options.version = value,
            "--docs-url" => options.docs_url = value,
            "--langs" => {
                options.locales = value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
            }
            _ => unreachable!("matched above"),
        }
    }
    Ok(options)
}

fn main() {
    let options = match parse_args(std::env::args().skip(1).collect()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("boundless-docs-mcp: {message}");
            std::process::exit(2);
        }
    };
    let source = GitSource::new(&options.root);
    let mut index = DocIndex::default();
    let locales: Vec<&str> = options.locales.iter().map(String::as_str).collect();
    if let Err(err) = index.add_version(&source, &options.version, &options.prefix, &locales) {
        eprintln!("boundless-docs-mcp: {err}");
        std::process::exit(1);
    }
    let session = Session::new(
        index,
        ServerConfig {
            prefix: options.prefix,
            locales: options.locales,
            version: options.version,
            docs_url: options.docs_url,
        },
    );
    if let Err(err) = serve(session, io::stdin().lock(), io::stdout().lock()) {
        eprintln!("boundless-docs-mcp: {err}");
        std::process::exit(1);
    }
}
