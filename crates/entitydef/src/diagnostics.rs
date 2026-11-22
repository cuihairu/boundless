//! Diagnostic sink for the definition loader.
//!
//! The reference funnels every loader problem into the global debug component
//! (`DECLARE_DEBUG_COMPONENT2( "DataDescription", 0 )` in both
//! `entity_description.cpp` and `data_type.cpp`) through `NOTICE_MSG`,
//! `INFO_MSG`, `WARNING_MSG`, `ERROR_MSG` and `CRITICAL_MSG`. Each call site
//! writes a self-describing format string, e.g.
//!
//! ```text
//! ERROR_MSG( "EntityDescription::parse: Could not open %s\n", filename.c_str() );
//! ```
//!
//! Rather than print, the loader collects each formatted message here so that
//! "does this definition produce the same complaints as the reference" is a
//! diffable, assertable question.

/// `NOTICE_MSG`, `INFO_MSG`, `WARNING_MSG`, `ERROR_MSG`, `CRITICAL_MSG`
/// priorities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Notice,
    Info,
    Warning,
    Error,
    Critical,
}

/// One captured message. `message` is the reference's format string with its
/// arguments substituted, trailing newline included, so a ported loader and
/// the reference produce byte-identical lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// `DECLARE_DEBUG_COMPONENT2` component of the producing translation unit.
    pub component: &'static str,
    pub message: String,
}

/// Collects diagnostics raised while parsing definitions.
#[derive(Debug, Default)]
pub struct Diagnostics {
    /// The debug component string of the module currently logging.
    component: &'static str,
    messages: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    /// The reference component both `entity_description.cpp` and
    /// `data_type.cpp` declare.
    pub const DATA_DESCRIPTION_COMPONENT: &'static str = "DataDescription";

    /// Switch the component label of subsequently recorded messages.
    pub fn set_component(&mut self, component: &'static str) {
        self.component = component;
    }

    pub fn notice(&mut self, message: impl Into<String>) {
        self.record(Severity::Notice, message);
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.record(Severity::Info, message);
    }

    pub fn warning(&mut self, message: impl Into<String>) {
        self.record(Severity::Warning, message);
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.record(Severity::Error, message);
    }

    pub fn critical(&mut self, message: impl Into<String>) {
        self.record(Severity::Critical, message);
    }

    fn record(&mut self, severity: Severity, message: impl Into<String>) {
        self.messages.push(Diagnostic {
            severity,
            component: self.component,
            message: message.into(),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.messages.iter()
    }

    /// The formatted text of every recorded message, in order.
    pub fn messages(&self) -> Vec<String> {
        self.messages.iter().map(|d| d.message.clone()).collect()
    }

    /// `ERROR_MSG` lines only: the reference treats any of these as fatal for
    /// the definition set.
    pub fn errors(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message.clone())
            .collect()
    }

    pub fn warnings(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.message.clone())
            .collect()
    }

    pub fn has_errors(&self) -> bool {
        self.messages.iter().any(|d| d.severity == Severity::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_are_ordered_like_the_reference_priorities() {
        assert!(Severity::Notice < Severity::Info);
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Critical);
    }

    #[test]
    fn messages_are_recorded_in_call_order_with_their_component() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.set_component(Diagnostics::DATA_DESCRIPTION_COMPONENT);
        diagnostics.error("EntityDescription::parse: Could not open scripts/entity_defs/A.def\n");
        diagnostics.warning("Couldn't open aliases file 'scripts/entity_defs/alias.xml'\n");

        let recorded = diagnostics.iter().collect::<Vec<_>>();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].severity, Severity::Error);
        assert_eq!(recorded[0].component, "DataDescription");
        assert_eq!(
            recorded[1].message,
            "Couldn't open aliases file 'scripts/entity_defs/alias.xml'\n"
        );

        assert_eq!(diagnostics.errors().len(), 1);
        assert_eq!(diagnostics.warnings().len(), 1);
        assert!(diagnostics.has_errors());
        assert_eq!(diagnostics.len(), 2);
        assert!(!diagnostics.is_empty());
    }

    #[test]
    fn a_critical_message_is_recorded_at_the_highest_severity() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.critical("out of memory\n");

        let recorded = diagnostics.iter().collect::<Vec<_>>();
        assert_eq!(recorded[0].severity, Severity::Critical);
        // Only Severity::Error counts for has_errors(), as in the reference.
        assert!(!diagnostics.has_errors());
        assert_eq!(diagnostics.messages(), vec!["out of memory\n"]);
    }

    #[test]
    fn clean_run_has_no_errors() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.info("parsing\n");
        assert!(!diagnostics.is_empty());
        assert!(!diagnostics.has_errors());
        assert_eq!(diagnostics.messages(), vec!["parsing\n"]);
    }
}
