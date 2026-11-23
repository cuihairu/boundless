//! Extraction of VitePress-flavored markdown into indexable sections.
//!
//! A section is one heading plus its body, ending at the next heading of the
//! same or a higher level. Anchors follow the VitePress/GitHub slug rules the
//! site itself uses when rendering `#`-links, so a section's anchor is openable
//! on the rendered page.

/// The subset of front matter the index needs.
#[derive(Debug, Default, PartialEq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub lang: Option<String>,
    pub private: bool,
}

/// One heading and the text it owns.
#[derive(Debug, PartialEq)]
pub struct ExtractedSection {
    pub level: u8,
    pub title: String,
    pub anchor: String,
    /// Zero-based line of the heading inside the document.
    pub line: usize,
    pub body: String,
    /// The text before any nested heading: what this section itself says.
    pub direct: String,
}

/// Everything the index needs about one markdown page.
#[derive(Debug, PartialEq)]
pub struct ExtractedDoc {
    pub title: String,
    pub lang: Option<String>,
    pub private: bool,
    pub sections: Vec<ExtractedSection>,
}

/// Splits the front matter block off the body. An unterminated block is not
/// front matter; the body is always a suffix of `text` so callers can recover
/// the line offset with `text[..text.len() - body.len()]`.
pub fn parse_front_matter(text: &str) -> (FrontMatter, &str) {
    let mut matter = FrontMatter::default();
    let Some(rest) = text.strip_prefix("---\n") else {
        return (matter, text);
    };
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if bare == "---" {
            return (matter, &rest[offset + line.len()..]);
        }
        if let Some((key, value)) = bare.split_once(':') {
            let value = value.trim();
            match key.trim() {
                "title" => matter.title = Some(value.to_string()),
                "lang" => matter.lang = Some(value.to_string()),
                "private" => matter.private = value == "true",
                _ => {}
            }
        }
        offset += line.len();
    }
    (FrontMatter::default(), text)
}

/// The anchor a rendered site would give this heading text.
pub fn slugify(heading: &str) -> String {
    let plain = strip_inline(heading);
    let mut out = String::new();
    let mut pending_space = false;
    for ch in plain.trim().chars() {
        if ch.is_whitespace() {
            pending_space = true;
        } else if ch == '-' || ch == '_' || ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push('-');
            }
            pending_space = false;
            if ch.is_ascii() {
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push(ch);
            }
        }
    }
    out.trim_matches('-').to_string()
}

/// Reduces inline markup (`code`, **bold**, [text](url)) to its text.
fn strip_inline(text: &str) -> String {
    let without_links = replace_links(text);
    without_links.replace(['`', '*', '~'], "")
}

/// `[text](url)` and `![alt](url)` become `text` / `alt`.
fn replace_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `[text](url)` and `![alt](url)` reduce to their label; the image
        // form starts one character earlier, at the `!`.
        let bracket = if bytes[i] == b'[' {
            i
        } else if bytes[i] == b'!' && bytes.get(i + 1) == Some(&b'[') {
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
                out.push_str(&text[bracket + 1..at]);
                i = at + 2 + paren + 1;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("utf-8 boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Extracts the page title, language and sections from markdown source.
/// Section lines are zero-based positions in the whole document, front matter
/// included, so they point at the file a user opens.
pub fn extract_document(text: &str) -> ExtractedDoc {
    let (matter, body) = parse_front_matter(text);
    let base = text[..text.len() - body.len()].matches('\n').count();
    let lines: Vec<&str> = body.lines().collect();

    let mut headings: Vec<(u8, String, usize)> = Vec::new();
    let mut in_fence = false;
    let mut fence_marker = "";
    for (no, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = &trimmed[..3];
            if !in_fence {
                in_fence = true;
                fence_marker = marker;
            } else if trimmed.starts_with(fence_marker) {
                in_fence = false;
            }
            continue;
        }
        if in_fence {
            continue;
        }
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
            let raw = trimmed[level + 1..].trim();
            let title = strip_inline(raw).trim().to_string();
            headings.push((level as u8, title, no + base));
        }
    }

    let mut sections = Vec::new();
    for (idx, &(level, ref title, line)) in headings.iter().enumerate() {
        // The body ends at the next heading of this level or higher, which is
        // the next entry in the list unless every heading until then is deeper.
        let mut end = lines.len() + base;
        for &(other, _, other_line) in headings[idx + 1..].iter() {
            if other <= level {
                end = other_line;
                break;
            }
        }
        // The direct body stops at the next heading of any level, so the
        // section that owns the text is the one search attributes it to.
        let next_any = headings.get(idx + 1).map(|(_, _, l)| *l).unwrap_or(end);
        let own_end = next_any.min(end);
        let body: Vec<&str> = lines[line + 1 - base..end - base].to_vec();
        let direct: Vec<&str> = lines[line + 1 - base..own_end - base].to_vec();
        sections.push(ExtractedSection {
            level,
            title: title.clone(),
            anchor: slugify(title),
            line,
            body: body.join("\n"),
            direct: direct.join("\n"),
        });
    }

    let title = matter
        .title
        .clone()
        .or_else(|| {
            sections
                .iter()
                .find(|s| s.level == 1)
                .map(|s| s.title.clone())
        })
        .or_else(|| sections.first().map(|s| s.title.clone()))
        .unwrap_or_default();

    ExtractedDoc {
        title,
        lang: matter.lang,
        private: matter.private,
        sections,
    }
}

/// The page language: front matter first, then a locale directory at the top
/// of the path (VitePress's `zh/guide/...` layout), then the site default.
pub fn resolve_lang(rel_path: &str, front: Option<&str>, locale_dirs: &[&str]) -> String {
    if let Some(lang) = front {
        return lang.to_string();
    }
    if let Some((first, _)) = rel_path.split_once('/')
        && locale_dirs.contains(&first) {
            return first.to_string();
        }
    "en".to_string()
}
