//! BW-27-01-02: diagnostics that locate a broken `.def` definition.
//!
//! The index from BW-27-01-01 reads whatever the editor sends; when a
//! definition is broken the editor needs to know *where*, not just that
//! something is wrong. `diagnose` walks the same scan and returns one
//! diagnostic per fault, each spanning exactly the text that is at fault:
//! a property without a `<Type>` or a duplicate name is flagged across its
//! whole declaration, an unclosed property or `<Properties>` block at the
//! tag that was left open.

use crate::entitydef_index::{IndexedProperty, scan};
use std::collections::BTreeSet;

/// One diagnostic: what is wrong, and the range it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// What is wrong.
    pub message: String,
    /// The 0-based line the range starts on.
    pub line: usize,
    /// The 0-based column the range starts at.
    pub column: usize,
    /// The 0-based line the range ends on.
    pub end_line: usize,
    /// The 0-based column just past the range's end.
    pub end_column: usize,
}

/// Diagnose one file's content. The diagnostics come back ordered by where
/// they point, so a stable document yields a stable list.
pub fn diagnose(content: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = content.lines().collect();
    let scanned = scan(content);
    let mut diagnostics = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();

    for property in &scanned.properties {
        let range = declaration_range(&lines, property);
        if scanned
            .missing_type
            .iter()
            .any(|(_, line)| *line == property.line)
        {
            diagnostics.push(diagnostic(
                format!("property '{}' has no <Type>", property.name),
                range,
            ));
        }
        if !seen.insert(property.name.as_str()) {
            diagnostics.push(diagnostic(
                format!("duplicate property '{}'", property.name),
                range,
            ));
        }
    }

    if let Some((name, line)) = &scanned.unclosed_property {
        diagnostics.push(diagnostic(
            format!("property '{name}' is never closed"),
            tag_range(&lines, *line),
        ));
    }
    if let Some(line) = scanned.unclosed_properties {
        diagnostics.push(diagnostic(
            "<Properties> is never closed".to_string(),
            tag_range(&lines, line),
        ));
    }

    diagnostics.sort_by_key(|d| (d.line, d.column));
    diagnostics
}

/// A range: start line, start column, end line, end column.
type Range = (usize, usize, usize, usize);

fn diagnostic(message: String, (line, column, end_line, end_column): Range) -> Diagnostic {
    Diagnostic {
        message,
        line,
        column,
        end_line,
        end_column,
    }
}

/// The range of one trimmed tag on its line.
fn tag_range(lines: &[&str], line: usize) -> Range {
    let text = lines.get(line).copied().unwrap_or_default();
    let indent = text.len() - text.trim_start().len();
    (line, indent, line, indent + text.trim().len())
}

/// The range of one whole declaration: its open tag through its close tag.
fn declaration_range(lines: &[&str], property: &IndexedProperty) -> Range {
    let (line, column, _, _) = tag_range(lines, property.line);
    let (_, _, end_line, end_column) = tag_range(lines, property.end_line - 1);
    (line, column, end_line, end_column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_definition_diagnoses_nothing() {
        let avatar = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t\t<Flags>\tOTHER_CLIENTS\t</Flags>\n\t\t\t<Default>\tnobody\t</Default>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tHP\t</Type>\n\t\t\t<Flags>\tOWN_CLIENT\t</Flags>\n\t\t</hp>\n\t</Properties>\n</root>\n";
        assert!(diagnose(avatar).is_empty());

        // A FIXED_DICT nests a <Properties> block inside <Type>: the nested
        // children are type fields, and the definition is still clean.
        let fixed_dict = "<root>\n\t<Properties>\n\t\t<pos>\n\t\t\t<Type>\tFIXED_DICT\n\t\t\t\t<Properties>\n\t\t\t\t\t<x>\n\t\t\t\t\t\t<Type>\tFLOAT32\t</Type>\n\t\t\t\t\t</x>\n\t\t\t\t</Properties>\n\t\t\t</Type>\n\t\t\t<Flags>\tCELL_PUBLIC_AND_OWN\t</Flags>\n\t\t</pos>\n\t</Properties>\n</root>\n";
        assert!(diagnose(fixed_dict).is_empty());
    }

    #[test]
    fn a_property_without_a_type_is_flagged_across_its_declaration() {
        let content = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Flags>\tOWN_CLIENT\t</Flags>\n\t\t</hp>\n\t</Properties>\n</root>\n";
        let diagnostics = diagnose(content);
        assert_eq!(diagnostics.len(), 1);
        let d = &diagnostics[0];
        assert_eq!(d.message, "property 'hp' has no <Type>");
        assert_eq!((d.line, d.column), (5, 2));
        assert_eq!((d.end_line, d.end_column), (7, 7));
    }

    #[test]
    fn a_duplicate_name_is_flagged_at_the_later_declaration() {
        let content = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t</Properties>\n</root>\n";
        let diagnostics = diagnose(content);
        assert_eq!(diagnostics.len(), 1);
        let d = &diagnostics[0];
        assert_eq!(d.message, "duplicate property 'name'");
        assert_eq!((d.line, d.column), (5, 2));
        assert_eq!((d.end_line, d.end_column), (7, 9));
    }

    #[test]
    fn an_unclosed_property_is_flagged_at_its_open_tag() {
        let content = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Type>\tHP\t</Type>\n\t</Properties>\n</root>\n";
        let diagnostics = diagnose(content);
        assert_eq!(diagnostics.len(), 1);
        let d = &diagnostics[0];
        assert_eq!(d.message, "property 'hp' is never closed");
        assert_eq!((d.line, d.column), (5, 2));
        assert_eq!((d.end_line, d.end_column), (5, 6));
    }

    #[test]
    fn an_unclosed_properties_block_is_flagged_at_its_open_tag() {
        let content = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n</root>\n";
        let diagnostics = diagnose(content);
        assert_eq!(diagnostics.len(), 1);
        let d = &diagnostics[0];
        assert_eq!(d.message, "<Properties> is never closed");
        assert_eq!((d.line, d.column), (1, 1));
        assert_eq!((d.end_line, d.end_column), (1, 13));
    }

    #[test]
    fn each_fault_in_one_document_locates_its_own_range() {
        let content = "<root>\n\t<Properties>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<name>\n\t\t\t<Type>\tSTRING\t</Type>\n\t\t</name>\n\t\t<hp>\n\t\t\t<Flags>\tOWN_CLIENT\t</Flags>\n\t\t</hp>\n\t\t<mood>\n\t\t\t<Type>\tSTRING\t</Type>\n</root>\n";
        let diagnostics = diagnose(content);
        let points: Vec<(String, usize, usize)> = diagnostics
            .iter()
            .map(|d| (d.message.clone(), d.line, d.column))
            .collect();
        assert_eq!(
            points,
            vec![
                ("<Properties> is never closed".to_string(), 1, 1),
                ("duplicate property 'name'".to_string(), 5, 2),
                ("property 'hp' has no <Type>".to_string(), 8, 2),
                ("property 'mood' is never closed".to_string(), 11, 2),
            ]
        );
    }
}
