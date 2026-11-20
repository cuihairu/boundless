//! BW-28-01-01: public VitePress sections extract with locatable titles,
//! anchors and languages.

use boundless_docs_mcp::markdown::extract_document;

const ENTITIES: &str = "---\ntitle: 实体定义\n---\n\n# Entity definitions\n\n\
Def files describe entities.\n\n## Properties\n\nProperties carry `Flags`.\n\n\
```xml\n<Properties>\n  ## not a heading\n</Properties>\n```\n\n\
### Cell and base\n\nCell data vs base data.\n\n\
## **Cell** 迁移 (migration)\n\nEntities move between cell apps.\n";

#[test]
fn front_matter_title_wins_over_headings() {
    let doc = extract_document(ENTITIES);
    assert_eq!(doc.title, "实体定义");
}

#[test]
fn headings_below_h1_become_sections_with_anchors() {
    let doc = extract_document(ENTITIES);
    let sections: Vec<(String, u8, String)> = doc
        .sections
        .iter()
        .map(|s| (s.title.clone(), s.level, s.anchor.clone()))
        .collect();
    assert_eq!(
        sections,
        [
            ("Entity definitions".into(), 1, "entity-definitions".into()),
            ("Properties".into(), 2, "properties".into()),
            ("Cell and base".into(), 3, "cell-and-base".into()),
            ("Cell 迁移 (migration)".into(), 2, "cell-迁移-migration".into()),
        ]
    );
}

#[test]
fn headings_inside_code_fences_are_not_sections() {
    let doc = extract_document(ENTITIES);
    assert!(!doc.sections.iter().any(|s| s.title.contains("not a heading")));
}

#[test]
fn a_section_body_ends_at_the_next_heading_of_its_level_or_higher() {
    let doc = extract_document(ENTITIES);
    let properties = doc
        .sections
        .iter()
        .find(|s| s.title == "Properties")
        .expect("Properties section");
    assert!(properties.body.contains("`Flags`"));
    // The fenced XML belongs to Properties; the later h2 does not.
    assert!(properties.body.contains("<Properties>"));
    assert!(!properties.body.contains("Entities move between"));
    let cell = doc
        .sections
        .iter()
        .find(|s| s.title == "Cell 迁移 (migration)")
        .expect("migration section");
    assert!(cell.body.contains("Entities move between cell apps."));
}

#[test]
fn heading_lines_are_recorded_zero_based() {
    let doc = extract_document(ENTITIES);
    let properties = doc
        .sections
        .iter()
        .find(|s| s.title == "Properties")
        .expect("Properties section");
    let line = ENTITIES.lines().nth(properties.line).expect("line exists");
    assert!(line.starts_with("## Properties"), "got: {line}");
}

#[test]
fn inline_markup_is_stripped_from_anchors() {
    assert_eq!(boundless_docs_mcp::markdown::slugify("`hp` **max** value"), "hp-max-value");
    assert_eq!(boundless_docs_mcp::markdown::slugify("[API](./api.md)"), "api");
}

#[test]
fn front_matter_language_overrides_the_path() {
    assert_eq!(
        boundless_docs_mcp::markdown::resolve_lang("guide/entities.md", Some("zh"), &[]),
        "zh"
    );
}

#[test]
fn locale_directories_in_the_path_pick_the_language() {
    assert_eq!(
        boundless_docs_mcp::markdown::resolve_lang("zh/guide/entities.md", None, &["zh"]),
        "zh"
    );
    assert_eq!(
        boundless_docs_mcp::markdown::resolve_lang("guide/entities.md", None, &["zh"]),
        "en"
    );
}

#[test]
fn private_front_matter_marks_the_page() {
    let doc = extract_document("---\nprivate: true\n---\n\n# Internal notes\n");
    assert!(doc.private);
    let doc = extract_document("# Public page\n");
    assert!(!doc.private);
}

#[test]
fn a_page_without_front_matter_titles_from_its_first_h1() {
    let doc = extract_document("## Secondary first\n\nBody.\n\n# Late top\n");
    assert_eq!(doc.title, "Late top");
}

#[test]
fn an_unterminated_front_matter_block_is_just_body_text() {
    let doc = extract_document("---\ntitle: never closed\n\n# Still a heading\n");
    assert_eq!(doc.title, "Still a heading");
    assert!(doc.sections.iter().any(|s| s.title == "Still a heading"));
}

#[test]
fn front_matter_language_is_extracted_and_unknown_keys_are_ignored() {
    let doc = extract_document("---\nlang: zh\ntitleTemplate: ignored\n---\n\n# Page\n");
    assert_eq!(doc.lang.as_deref(), Some("zh"));
}

#[test]
fn image_links_contribute_only_their_alt_text() {
    assert_eq!(
        boundless_docs_mcp::markdown::slugify("![迁移图](moves.png) overview"),
        "迁移图-overview"
    );
}

#[test]
fn broken_links_pass_through_unchanged() {
    assert_eq!(
        boundless_docs_mcp::markdown::slugify("[orphan text and (unclosed"),
        "orphan-text-and-unclosed"
    );
}

#[test]
fn a_fence_of_the_other_marker_does_not_close_a_fence() {
    let doc = extract_document("## One\n\n```xml\n~~~xml\nstill fenced\n~~~\n```\n\n## Two\n");
    let one = doc.sections.iter().find(|s| s.title == "One").unwrap();
    assert!(one.body.contains("still fenced"));
    assert!(!doc.sections.iter().any(|s| s.title.contains("still fenced")));
}

#[test]
fn a_page_without_any_h1_titles_from_its_first_heading() {
    let doc = extract_document("## Secondary first\n\nBody.\n");
    assert_eq!(doc.title, "Secondary first");
}

#[test]
fn a_link_with_an_unclosed_url_keeps_its_label_and_text() {
    assert_eq!(boundless_docs_mcp::markdown::slugify("[label](never closed"), "labelnever-closed");
}
