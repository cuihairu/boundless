//! The versioned document index: one list of versions, each a list of pages.
//!
//! A version is identified by its Git ref label and the commit it resolved to,
//! so two versions carrying the same page path never collide. Refreshing a
//! version re-reads only the pages whose Git blob changed, so an index over a
//! large tree stays cheap between runs.

use serde::{Deserialize, Serialize};

use crate::markdown::{extract_document, resolve_lang};
use crate::source::{GitSource, TreeEntry};
use crate::DocsError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SectionEntry {
    pub title: String,
    pub anchor: String,
    pub level: u8,
    /// Zero-based line of the heading in the page source.
    pub line: usize,
    pub body: String,
    /// The text before any nested heading, what this section itself says.
    pub direct: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocEntry {
    /// Path relative to the docs prefix (`guide/entities.md`).
    pub path: String,
    pub lang: String,
    pub title: String,
    /// The Git blob id this page was read from; the change fingerprint.
    pub blob: String,
    pub sections: Vec<SectionEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionDocs {
    /// The ref it was read at (`v1`, `HEAD`, ...).
    pub label: String,
    /// The commit the label resolved to; the stable key.
    pub commit: String,
    pub docs: Vec<DocEntry>,
}

/// What one refresh pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefreshReport {
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocIndex {
    pub versions: Vec<VersionDocs>,
}

impl DocIndex {
    /// Reads one Git version into the index. The first read of a version
    /// reports every page as updated.
    pub fn add_version(
        &mut self,
        source: &GitSource,
        reference: &str,
        prefix: &str,
        locales: &[&str],
    ) -> Result<RefreshReport, DocsError> {
        self.refresh(source, reference, prefix, locales)
    }

    /// Refreshes one version in place: pages whose blob changed are re-read,
    /// pages that disappeared are dropped, untouched pages are kept as-is.
    pub fn refresh(
        &mut self,
        source: &GitSource,
        reference: &str,
        prefix: &str,
        locales: &[&str],
    ) -> Result<RefreshReport, DocsError> {
        let commit = source.rev(reference)?;
        let previous = self.versions.iter().find(|v| v.label == reference);

        let mut report = RefreshReport::default();
        let mut docs: Vec<DocEntry> = Vec::new();
        for entry in source.entries(reference, prefix)? {
            let unchanged = previous
                .and_then(|v| v.docs.iter().find(|d| d.path == entry.path))
                .filter(|old| old.blob == entry.blob)
                .cloned();
            match unchanged {
                Some(doc) => {
                    report.unchanged += 1;
                    docs.push(doc);
                }
                None => {
                    let spec = format!("{prefix}/{}", entry.path);
                    let text = source.read(reference, &spec)?;
                    if let Some(doc) =
                        DocEntry::from_source(&entry, &text, locales)
                    {
                        docs.push(doc);
                        report.updated.push(entry.path.clone());
                    }
                }
            }
        }

        let current: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        if let Some(previous) = previous {
            for doc in &previous.docs {
                if !current.contains(&doc.path.as_str()) {
                    report.removed.push(doc.path.clone());
                }
            }
        }

        match self.versions.iter_mut().find(|v| v.label == reference) {
            Some(version) => {
                version.commit = commit;
                version.docs = docs;
            }
            None => self.versions.push(VersionDocs {
                label: reference.to_string(),
                commit,
                docs,
            }),
        }
        Ok(report)
    }

    /// Looks a page up in a version given as its label or commit.
    pub fn find(&self, version: &str, path: &str) -> Option<&DocEntry> {
        self.versions
            .iter()
            .find(|v| v.label == version || v.commit == version)
            .and_then(|v| v.docs.iter().find(|d| d.path == path))
    }

    /// The version entry for a label or commit.
    pub fn version(&self, version: &str) -> Option<&VersionDocs> {
        self.versions
            .iter()
            .find(|v| v.label == version || v.commit == version)
    }

    /// The indexed page paths of a version.
    pub fn paths(&self, version: &str) -> Vec<String> {
        self.version(version)
            .map(|v| v.docs.iter().map(|d| d.path.clone()).collect())
            .unwrap_or_default()
    }

    pub fn versions(&self) -> &[VersionDocs] {
        &self.versions
    }

    pub fn to_json(&self) -> Result<String, DocsError> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn from_json(text: &str) -> Result<Self, DocsError> {
        Ok(serde_json::from_str(text)?)
    }
}

impl DocEntry {
    /// Extracts a page; `None` when the page is not public.
    fn from_source(entry: &TreeEntry, text: &str, locales: &[&str]) -> Option<Self> {
        let extracted = extract_document(text);
        if extracted.private {
            return None;
        }
        Some(Self {
            path: entry.path.clone(),
            lang: resolve_lang(&entry.path, extracted.lang.as_deref(), locales),
            title: extracted.title,
            blob: entry.blob.clone(),
            sections: extracted
                .sections
                .into_iter()
                .map(|s| SectionEntry {
                    title: s.title,
                    anchor: s.anchor,
                    level: s.level,
                    line: s.line,
                    body: s.body,
                    direct: s.direct,
                })
                .collect(),
        })
    }
}
