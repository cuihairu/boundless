//! Text search over the index: CJK-aware substring matching ranked by where
//! the terms land (section title first, page title, then the body).

use serde::Serialize;

use crate::index::DocIndex;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    /// The version the hit was read from, as its label.
    pub version: String,
    pub path: String,
    pub lang: String,
    /// The page title the section belongs to.
    pub doc: String,
    pub section: String,
    pub anchor: String,
    pub level: u8,
    /// Zero-based line of the section heading in the page source.
    pub line: usize,
    pub snippet: String,
}

pub const DEFAULT_LIMIT: usize = 10;

/// Finds the best sections of one version for the terms in `query`.
pub fn search(
    index: &DocIndex,
    version: &str,
    query: &str,
    lang: Option<&str>,
    limit: usize,
) -> Vec<SearchHit> {
    let Some(pages) = index.version(version) else {
        return Vec::new();
    };
    let terms = terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u32, SearchHit)> = Vec::new();
    for page in &pages.docs {
        // The language filter keeps only that locale's tree.
        if let Some(wanted) = lang && page.lang != wanted {
            continue;
        }
        for section in &page.sections {
            let score = score_section(&terms, &page.title, &section.title, &section.direct);
            if score == 0 {
                continue;
            }
            hits.push((score, SearchHit {
                version: pages.label.clone(),
                path: page.path.clone(),
                lang: page.lang.clone(),
                doc: page.title.clone(),
                section: section.title.clone(),
                anchor: section.anchor.clone(),
                level: section.level,
                line: section.line,
                snippet: snippet(&section.direct, &terms),
            }));
        }
    }
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.path.cmp(&b.1.path))
            .then_with(|| a.1.line.cmp(&b.1.line))
    });
    hits.into_iter().map(|(_, hit)| hit).take(limit).collect()
}

/// Adds up where each term appears: an exact section title word outweighs a
/// mention in the body.
fn score_section(query_terms: &[String], doc: &str, title: &str, body: &str) -> u32 {
    let doc = doc.to_lowercase();
    let title = title.to_lowercase();
    let body = body.to_lowercase();
    let title_terms = terms(&title);
    let mut score = 0u32;
    for term in query_terms {
        if title_terms.contains(term) {
            score += 8;
        } else if title.contains(term) {
            score += 4;
        }
        if doc.contains(term) {
            score += 2;
        }
        if body.contains(term) {
            score += 1;
        }
    }
    score
}

/// The first body line that carries any term, capped for readability.
fn snippet(body: &str, terms: &[String]) -> String {
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if terms.iter().any(|term| lower.contains(term)) {
            return cap(trimmed);
        }
    }
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(cap)
        .unwrap_or_default()
}

fn cap(text: &str) -> String {
    if text.chars().count() <= 140 {
        text.to_string()
    } else {
        let head: String = text.chars().take(140).collect();
        format!("{head}…")
    }
}

/// Lower-cased alphanumeric runs, so Chinese phrases stay one term.
pub fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.to_lowercase().chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{DocEntry, SectionEntry, VersionDocs, DocIndex};
    use crate::markdown;

    #[test]
    fn terms_keep_cjk_phrases_together() {
        assert_eq!(terms("Cell 迁移 v2!"), vec!["cell", "迁移", "v2"]);
        assert!(terms("").is_empty());
    }

    #[test]
    fn the_snippet_falls_back_to_the_first_line() {
        let terms = vec!["absent".to_string()];
        assert_eq!(
            snippet("First line.\nSecond line.\n", &terms),
            "First line."
        );
    }

    #[test]
    fn the_snippet_caps_long_lines() {
        let long = "word ".repeat(80);
        let terms = vec!["word".to_string()];
        let hit = snippet(&format!("lead\n{long}"), &terms);
        assert!(hit.ends_with('…'));
        assert!(hit.chars().count() <= 141);
    }

    #[test]
    fn an_empty_query_and_a_missing_version_find_nothing() {
        let index = DocIndex { versions: Vec::new() };
        assert!(search(&index, "v1", "Flags", None, DEFAULT_LIMIT).is_empty());
        assert!(search(&index, "v1", "", None, DEFAULT_LIMIT).is_empty());
        assert!(search(&index, "v1", "   ", None, DEFAULT_LIMIT).is_empty());
    }

    #[test]
    fn the_limit_keeps_only_the_best_hits() {
        let mut index = DocIndex { versions: Vec::new() };
        let page = DocEntry {
            path: "guide/x.md".into(),
            lang: "en".into(),
            title: "X".into(),
            blob: "b".into(),
            sections: vec![
                section("First", "one flags mention"),
                section("Second", "two flags mention"),
                section("Third", "three flags mention"),
            ],
        };
        index.versions.push(VersionDocs {
            label: "v1".into(),
            commit: "abc".into(),
            docs: vec![page],
        });
        let hits = search(&index, "v1", "flags", None, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].anchor, "first");
        assert_eq!(hits[1].anchor, "second");
    }

    fn section(title: &str, body: &str) -> SectionEntry {
        SectionEntry {
            title: title.into(),
            anchor: markdown::slugify(title),
            level: 2,
            line: 1,
            body: body.into(),
            direct: body.into(),
        }
    }
}
