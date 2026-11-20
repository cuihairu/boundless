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

/// One markdown link of a section body, pointed at where it opens.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedLink {
    pub label: String,
    /// The place the link opens: a rendered docs link, or the target itself
    /// when it points outside the docs site.
    pub target: String,
}

/// The links of a body, each resolved to where it opens on the rendered site.
/// Images are not links, so they are left alone.
pub fn links_in(text: &str, page: &str, docs_url: &str) -> Vec<ResolvedLink> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let is_image = bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[');
        let bracket = if bytes[i] == b'[' {
            i
        } else if is_image {
            i + 1
        } else {
            bytes.len()
        };
        if bracket < bytes.len()
            && let Some(close) = text[bracket + 1..].find(']')
        {
            let at = bracket + 1 + close;
            if let Some(rest) = text[at + 1..].strip_prefix('(')
                && let Some(paren) = rest.find(')')
            {
                let label = &text[bracket + 1..at];
                let target = &rest[..paren];
                if !is_image {
                    out.push(ResolvedLink {
                        label: label.to_string(),
                        target: resolve_target(target, page, docs_url),
                    });
                }
                i = at + 2 + paren + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("utf-8 boundary");
        i += ch.len_utf8();
    }
    out
}

/// The place one markdown link target opens, seen from `page`.
fn resolve_target(target: &str, page: &str, docs_url: &str) -> String {
    let target = target.trim();
    if target.is_empty() {
        return source_link(docs_url, page, None);
    }
    if let Some(anchor) = target.strip_prefix('#') {
        return source_link(docs_url, page, Some(anchor));
    }
    if is_external(target) {
        return target.to_string();
    }
    let (path, anchor) = match target.split_once('#') {
        Some((path, anchor)) => (path, Some(anchor)),
        None => (target, None),
    };
    source_link(docs_url, &normalize(page, path), anchor)
}

/// Schemes that leave the docs site, so the link stays untouched.
fn is_external(target: &str) -> bool {
    target
        .split_once(':')
        .is_some_and(|(scheme, _)| {
            matches!(
                scheme,
                "http" | "https" | "mailto" | "ftp" | "tel" | "git" | "ssh"
            )
        })
}

/// Joins `target` onto the directory of `page`, collapsing `.` and `..`.
/// A target starting with `/` is counted from the docs root.
fn normalize(page: &str, target: &str) -> String {
    let absolute = target.starts_with('/');
    let target = target.trim_start_matches('/');
    let mut parts: Vec<&str> = if absolute {
        Vec::new()
    } else {
        page.rsplit_once('/')
            .map(|(dir, _)| dir)
            .unwrap_or("")
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
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

    #[test]
    fn a_bare_anchor_points_at_the_same_page() {
        let links = links_in(
            "See [Renaming](#renaming-a-class) and [Self](#).",
            "guide/entities.md",
            "https://docs.boundless.dev",
        );
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "https://docs.boundless.dev/guide/entities.html#renaming-a-class",
                "https://docs.boundless.dev/guide/entities.html",
            ]
        );
    }

    #[test]
    fn a_relative_link_climbs_and_descends() {
        let links = links_in(
            "[A](../cells/migration.md#steps) [B](./near.md) [C](deep/down.md) \
             [D](/ops/postgresql.md#tuning) [E](../../up.md)",
            "zh/guide/entities.md",
            "https://docs.boundless.dev",
        );
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "https://docs.boundless.dev/zh/cells/migration.html#steps",
                "https://docs.boundless.dev/zh/guide/near.html",
                "https://docs.boundless.dev/zh/guide/deep/down.html",
                "https://docs.boundless.dev/ops/postgresql.html#tuning",
                "https://docs.boundless.dev/up.html",
            ]
        );
    }

    #[test]
    fn external_links_and_images_stay_as_they_are() {
        let links = links_in(
            "[Site](https://vitepress.dev/guide/) [Mail](mailto:x@y.z) ![logo](/img/logo.png)",
            "guide/entities.md",
            "https://docs.boundless.dev",
        );
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "https://vitepress.dev/guide/");
        assert_eq!(links[1].target, "mailto:x@y.z");
    }

    #[test]
    fn a_link_above_the_root_clamps_to_the_root() {
        let links = links_in("[Far](../../../../away.md)", "guide/entities.md", "");
        assert_eq!(links[0].target, "docs/away.html");
    }
}
