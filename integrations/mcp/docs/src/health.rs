//! Telling whether an index still matches the repository it was read from.
//!
//! An index is built once and served many times, so a workspace can move on
//! without anyone rebuilding it. `freshness` compares a stored index against
//! the ref it was built at and names every way the two have diverged, which is
//! the gate CI runs instead of trusting that a build happened.

use crate::index::DocIndex;
use crate::links;
use crate::markdown;
use crate::source::GitSource;
use crate::DocsError;

/// Whether one version of an index still describes the repository.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FreshnessReport {
    /// Every way the index and the repository disagree.
    pub reasons: Vec<String>,
}

impl FreshnessReport {
    pub fn stale(&self) -> bool {
        !self.reasons.is_empty()
    }

    /// One line per reason, for a log or a CI failure message.
    pub fn describe(&self) -> Vec<String> {
        self.reasons.clone()
    }
}

/// Compares `reference` of the index with the repository at that same ref.
pub fn freshness(
    index: &DocIndex,
    source: &GitSource,
    reference: &str,
    prefix: &str,
) -> Result<FreshnessReport, DocsError> {
    let current = source.rev(reference)?;
    let mut report = FreshnessReport::default();
    let version = match index.version(reference) {
        None => {
            report
                .reasons
                .push(format!("missing version {reference}"));
            return Ok(report);
        }
        Some(version) => version,
    };
    if version.commit != current {
        report
            .reasons
            .push(format!("moved: {} -> {current}", version.commit));
    }
    let entries = source.entries(reference, prefix)?;
    let on_disk: std::collections::HashMap<&str, &str> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.blob.as_str()))
        .collect();
    let in_index: std::collections::HashMap<&str, &str> = version
        .docs
        .iter()
        .map(|doc| (doc.path.as_str(), doc.blob.as_str()))
        .collect();

    for (path, blob) in &in_index {
        match on_disk.get(path) {
            None => report.reasons.push(format!("removed: {path}")),
            Some(other) if *other != *blob => report.reasons.push(format!("changed: {path}")),
            _ => {}
        }
    }
    for path in on_disk.keys() {
        if !in_index.contains_key(path) {
            report.reasons.push(format!("added: {path}"));
        }
    }
    report.reasons.sort();
    Ok(report)
}

/// One way a link of an indexed page can be wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenLink {
    /// The page the link lives in.
    pub path: String,
    /// The section it lives in.
    pub section: String,
    /// The link as the server answers it.
    pub target: String,
    pub kind: BrokenKind,
}

/// Why a link does not open inside the docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokenKind {
    /// The link points at a page the index does not carry.
    MissingPage,
    /// The page is indexed but carries no such anchor.
    DanglingAnchor,
}

impl BrokenLink {
    /// One line for a log or a CI failure message.
    pub fn describe(&self) -> String {
        let what = match self.kind {
            BrokenKind::MissingPage => "no such page",
            BrokenKind::DanglingAnchor => "no such anchor",
        };
        format!("{} > {}: {what} at {}", self.path, self.section, self.target)
    }
}

/// What a check of one version's public scope and links found.
#[derive(Debug, Clone, Default)]
pub struct ScopeReport {
    /// Pages the index carries although the repository marks them private.
    pub private: Vec<String>,
    /// Links that do not open inside the indexed docs.
    pub broken: Vec<BrokenLink>,
}

impl ScopeReport {
    /// Whether nothing at all needs fixing.
    pub fn clean(&self) -> bool {
        self.private.is_empty() && self.broken.is_empty()
    }

    /// One line per finding, for a log or a CI failure message.
    pub fn describe(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .private
            .iter()
            .map(|path| format!("private page indexed: {path}"))
            .collect();
        lines.extend(self.broken.iter().map(BrokenLink::describe));
        lines
    }
}

/// Whether the version an index carries stays public and whether every link of
/// it opens on a page and anchor the index also carries.
pub fn scope(
    index: &DocIndex,
    source: &GitSource,
    reference: &str,
    prefix: &str,
    docs_url: &str,
) -> Result<ScopeReport, DocsError> {
    let version = index
        .version(reference)
        .cloned()
        .ok_or_else(|| DocsError::Version(format!("not indexed: {reference}")))?;
    let mut report = ScopeReport::default();
    for doc in &version.docs {
        // A page the repository no longer serves cannot be inspected here; the
        // freshness gate names it.
        let Ok(text) = source.read(reference, &format!("{prefix}/{}", doc.path)) else {
            continue;
        };
        let (matter, _) = markdown::parse_front_matter(&text);
        if matter.private {
            report.private.push(doc.path.clone());
            continue;
        }
        for section in &doc.sections {
            for link in links::links_in(&section.direct, &doc.path, docs_url) {
                let Some((page, anchor)) = links::rendered_target(&link.target, docs_url) else {
                    continue; // a link to another host is not a doc link
                };
                match index.find(reference, &page) {
                    None => report.broken.push(BrokenLink {
                        path: doc.path.clone(),
                        section: section.title.clone(),
                        target: link.target,
                        kind: BrokenKind::MissingPage,
                    }),
                    Some(found) => {
                        if let Some(anchor) = anchor
                            && !found.sections.iter().any(|section| section.anchor == anchor)
                        {
                            report.broken.push(BrokenLink {
                                path: doc.path.clone(),
                                section: section.title.clone(),
                                target: link.target,
                                kind: BrokenKind::DanglingAnchor,
                            });
                        }
                    }
                }
            }
        }
    }
    report.private.sort();
    report.broken.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.section.cmp(&b.section))
            .then_with(|| a.target.cmp(&b.target))
    });
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A throw-away repository with one page, good enough to move and edit.
    struct Repo {
        dir: tempfile::TempDir,
        source: GitSource,
    }

    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let source = GitSource::new(dir.path());
            let repo = Self { dir, source };
            git(repo.dir.path(), &["init", "-q"]);
            repo.commit("docs/guide/page.md", "# Page\n\n## One\n\nBody.\n", "one");
            repo
        }

        fn commit(&self, path: &str, text: &str, message: &str) {
            let file = self.dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).expect("mkdir");
            std::fs::write(&file, text).expect("write");
            git(self.dir.path(), &["add", "-A"]);
            git(self.dir.path(), &["commit", "-q", "-m", message]);
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn a_rebuilt_index_is_fresh() {
        let repo = Repo::new();
        let mut index = DocIndex::default();
        index
            .add_version(&repo.source, "HEAD", "docs", &["zh"])
            .expect("index");
        let report = freshness(&index, &repo.source, "HEAD", "docs").expect("report");
        assert!(!report.stale());
    }

    #[test]
    fn an_unrebuilt_edit_is_stale() {
        let repo = Repo::new();
        let mut index = DocIndex::default();
        index
            .add_version(&repo.source, "HEAD", "docs", &["zh"])
            .expect("index");
        repo.commit("docs/guide/page.md", "# Page\n\n## Two\n\nNew body.\n", "two");
        let report = freshness(&index, &repo.source, "HEAD", "docs").expect("report");
        assert!(report.stale());
        // The ref moved as well as the page changed; both are reported.
        assert_eq!(report.reasons.len(), 2);
        assert!(report
            .reasons
            .contains(&"changed: guide/page.md".to_string()));
        assert!(report.reasons.iter().any(|reason| reason.starts_with("moved: ")));
    }

    #[test]
    fn an_unknown_version_is_stale_too() {
        let repo = Repo::new();
        let report = freshness(&DocIndex::default(), &repo.source, "HEAD", "docs").expect("report");
        assert_eq!(report.reasons, ["missing version HEAD"]);
    }

    #[test]
    fn a_clean_scope_passes() {
        let repo = Repo::new();
        repo.commit(
            "docs/guide/near.md",
            "# Near\n\n## Here\n\nSee [One](./page.md#one).\n",
            "near",
        );
        let mut index = DocIndex::default();
        index
            .add_version(&repo.source, "HEAD", "docs", &["zh"])
            .expect("index");
        let report = scope(&index, &repo.source, "HEAD", "docs", "").expect("report");
        assert!(report.clean());
    }

    #[test]
    fn a_link_to_a_missing_page_is_reported() {
        let repo = Repo::new();
        repo.commit(
            "docs/guide/near.md",
            "# Near\n\n## Here\n\nSee [Gone](./gone.md).\n",
            "near",
        );
        let mut index = DocIndex::default();
        index
            .add_version(&repo.source, "HEAD", "docs", &["zh"])
            .expect("index");
        let report = scope(&index, &repo.source, "HEAD", "docs", "").expect("report");
        assert_eq!(report.broken.len(), 1);
        assert_eq!(report.broken[0].kind, BrokenKind::MissingPage);
        assert!(report.describe().iter().any(|line| line.contains("no such page")));
    }

    #[test]
    fn a_link_to_a_missing_anchor_is_reported() {
        let repo = Repo::new();
        repo.commit(
            "docs/guide/near.md",
            "# Near\n\n## Here\n\nSee [Nope](./page.md#nope).\n",
            "near",
        );
        let mut index = DocIndex::default();
        index
            .add_version(&repo.source, "HEAD", "docs", &["zh"])
            .expect("index");
        let report = scope(&index, &repo.source, "HEAD", "docs", "").expect("report");
        assert_eq!(report.broken.len(), 1);
        assert_eq!(report.broken[0].kind, BrokenKind::DanglingAnchor);
        assert!(
            report
                .describe()
                .iter()
                .any(|line| line.contains("no such anchor"))
        );
    }

    #[test]
    fn a_private_page_in_the_index_is_reported() {
        use crate::index::{DocEntry, SectionEntry, VersionDocs};

        let repo = Repo::new();
        repo.commit(
            "docs/guide/secret.md",
            "---\ntitle: Secret\nprivate: true\n---\n# Secret\n\n## Hidden\n\nHidden body.\n",
            "secret",
        );
        let mut index = DocIndex::default();
        index.versions.push(VersionDocs {
            label: "HEAD".into(),
            commit: repo.source.rev("HEAD").expect("rev"),
            docs: vec![DocEntry {
                path: "guide/secret.md".into(),
                lang: "en".into(),
                title: "Secret".into(),
                blob: "anything".into(),
                sections: vec![SectionEntry {
                    title: "Hidden".into(),
                    anchor: "hidden".into(),
                    level: 2,
                    line: 5,
                    body: "Hidden body.".into(),
                    direct: "Hidden body.".into(),
                }],
            }],
        });
        let report = scope(&index, &repo.source, "HEAD", "docs", "").expect("report");
        assert_eq!(report.private, ["guide/secret.md"]);
        assert!(!report.clean());
        assert!(
            report
                .describe()
                .iter()
                .any(|line| line.contains("private page indexed"))
        );
    }
}
