//! Turning an indexed page and section into a link a reader can open.
//!
//! VitePress renders `docs/guide/entities.md` to `<base>/guide/entities.html`,
//! so a source link is the rendered path plus the section anchor.

/// The path a page renders to on the docs site.
pub fn rendered(page: &str) -> String {
    format!("{}.html", page.strip_suffix(".md").unwrap_or(page))
}

/// A link to `page`, or to one of its sections when `anchor` names one.
///
/// `docs_url` is the base URL of the rendered site; when it is empty the link
/// stays repository-relative, so it still resolves inside a checkout.
pub fn source_link(docs_url: &str, page: &str, anchor: Option<&str>) -> String {
    let base = if docs_url.trim().is_empty() {
        "docs/".to_string()
    } else {
        format!("{}/", docs_url.trim().trim_end_matches('/'))
    };
    match anchor {
        Some(anchor) if !anchor.is_empty() => format!("{base}{}#{anchor}", rendered(page)),
        _ => format!("{base}{}", rendered(page)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_link_points_at_the_rendered_page() {
        assert_eq!(
            source_link("", "guide/entities.md", None),
            "docs/guide/entities.html"
        );
        assert_eq!(
            source_link("https://docs.boundless.dev/", "zh/guide/x.md", Some("intro")),
            "https://docs.boundless.dev/zh/guide/x.html#intro"
        );
        assert_eq!(source_link("https://docs.boundless.dev", "x.md", Some("")), "https://docs.boundless.dev/x.html");
        assert_eq!(rendered("README"), "README.html");
    }
}
