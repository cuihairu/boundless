//! Case registry: trace boundless tests back to their original case names
//! (BW-18-01-04).
//!
//! The reference ships ~900 `TEST()` registrations across `lib/*/unit_test`;
//! this crate embeds the generated inventory (`cases/original_inventory.txt`,
//! regenerated with the `scan-cases` binary) and a mapping table. Every lane
//! extends [`PORTED`] as it ports cases: the *original* is the reference's
//! case name, the *target* is this repository's test path for Rust cases or
//! a pytest node id for Python cases. Validation tests keep the table honest
//! -- a mapping may only name a case that exists in the inventory, and no
//! target may be claimed by two different originals.

use std::sync::LazyLock;

/// The generated inventory, committed next to this crate.
const INVENTORY_TEXT: &str = include_str!("../cases/original_inventory.txt");

/// One original `TEST()` registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginalCase {
    /// Reference module the case lives in (`lib/<module>/unit_test`).
    pub module: String,
    /// Reference file, relative to the reference root.
    pub file: String,
    /// 1-based source line of the registration.
    pub line: u32,
    /// The case name inside `TEST( ... )`.
    pub name: String,
}

/// A traceability edge: original case name -> boundless test target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortedCase {
    /// The original case name, exactly as registered in the reference.
    pub original: &'static str,
    /// The covering test here: a Rust test path
    /// (`crate::module::tests::name`) or a pytest node id.
    pub target: &'static str,
}

/// Cases already covered in this repository. Lanes append their own rows.
pub static PORTED: &[PortedCase] = &[
    PortedCase {
        original: "MGM_processMessageFullRoundTrip",
        target: "contracts::machined::tests::process_message_matches_reference_bytes",
    },
    PortedCase {
        original: "MGM_processMessageLegacyBodyWithoutExtra",
        target: "contracts::machined::tests::process_message_without_extra_block_is_accepted",
    },
    PortedCase {
        original: "MGMMSG_processRoundTripWithExtra",
        target: "contracts::machined::tests::process_message_round_trips",
    },
    PortedCase {
        original: "MGMMSG_processExtraSkipsFutureBytes",
        target: "contracts::machined::tests::future_extra_fields_are_skipped",
    },
    PortedCase {
        original: "MGMMSG_processMatchesPerParamBit",
        target: "contracts::machined::tests::matches_honors_only_requested_fields",
    },
    PortedCase {
        original: "MGMMSG_listenerRoundTrip",
        target: "contracts::machined::tests::listener_message_round_trips",
    },
    PortedCase {
        original: "MGMMSG_signalRoundTripAndSetters",
        target: "contracts::machined::tests::signal_message_round_trips_and_shutdown_levels",
    },
];

static INVENTORY: LazyLock<Vec<OriginalCase>> = LazyLock::new(|| parse_inventory(INVENTORY_TEXT));

/// Parse inventory records; blank and `#` comment lines are skipped, a
/// malformed record is a hard error so a corrupted inventory cannot shrink
/// the population silently.
pub fn parse_inventory(text: &str) -> Vec<OriginalCase> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|(index, line)| {
            let fields: Vec<&str> = line.split('\t').collect();
            let expected = format!(
                "inventory line {}: 'module<TAB>file<TAB>line<TAB>name'",
                index + 1
            );
            let [module, file, line, name] = fields.as_slice() else {
                panic!("expected {expected}, got {line:?}");
            };
            let line: u32 = line
                .parse()
                .unwrap_or_else(|_| panic!("expected {expected}, bad line number {line:?}"));
            OriginalCase {
                module: module.to_string(),
                file: file.to_string(),
                line,
                name: name.to_string(),
            }
        })
        .collect()
}

/// The embedded original population.
pub fn original_cases() -> &'static [OriginalCase] {
    &INVENTORY
}

/// Look up an original case by its exact name.
pub fn case_named(name: &str) -> Option<&'static OriginalCase> {
    INVENTORY.iter().find(|case| case.name == name)
}

/// The boundless targets covering an original case.
pub fn targets_for(original: &str) -> Vec<&'static str> {
    PORTED
        .iter()
        .filter(|ported| ported.original == original)
        .map(|ported| ported.target)
        .collect()
}

/// The original cases a target covers (reverse lookup).
pub fn originals_covered_by(target: &str) -> Vec<&'static str> {
    PORTED
        .iter()
        .filter(|ported| ported.target == target)
        .map(|ported| ported.original)
        .collect()
}

/// All original cases of one reference module.
pub fn cases_in_module(module: &str) -> Vec<&'static OriginalCase> {
    INVENTORY
        .iter()
        .filter(|case| case.module == module)
        .collect()
}

/// How much of the original population is mapped so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    /// Total original cases in the inventory.
    pub total: usize,
    /// Distinct original cases with at least one target.
    pub mapped: usize,
    /// Distinct targets in the mapping table.
    pub targets: usize,
}

/// Coverage summary over [`PORTED`].
pub fn coverage() -> Coverage {
    let mapped: std::collections::BTreeSet<_> = PORTED.iter().map(|p| p.original).collect();
    let targets: std::collections::BTreeSet<_> = PORTED.iter().map(|p| p.target).collect();
    Coverage {
        total: INVENTORY.len(),
        mapped: mapped.len(),
        targets: targets.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded population parses fully and every record is sane:
    /// nothing silently dropped, no placeholder fields.
    #[test]
    fn the_inventory_parses_with_the_reference_population() {
        let cases = original_cases();
        assert!(cases.len() > 900, "population: {}", cases.len());
        for case in cases {
            assert!(!case.module.is_empty());
            assert!(case.file.starts_with("lib/"), "{case:?}");
            assert!(case.line >= 1);
            assert!(!case.name.is_empty());
        }
        let unique: std::collections::BTreeSet<_> =
            cases.iter().map(|c| (&*c.file, c.line)).collect();
        assert_eq!(unique.len(), cases.len(), "duplicate (file, line) records");
    }

    /// A mapping row may only name a case that exists in the inventory,
    /// with a non-empty target.
    #[test]
    fn every_mapping_names_a_real_original_case() {
        for ported in PORTED {
            let case = case_named(ported.original)
                .unwrap_or_else(|| panic!("unknown original {}", ported.original));
            assert!(!ported.target.is_empty());
            assert!(
                case.name == ported.original,
                "inventory mismatch for {}",
                ported.original
            );
        }
    }

    /// One target is claimed by at most one original: two originals sharing
    /// a target means a lane copied a row instead of porting the case.
    #[test]
    fn no_target_is_claimed_by_two_originals() {
        let mut claims: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
        for ported in PORTED {
            let previous = claims.insert(ported.target, ported.original);
            assert!(
                previous.is_none_or(|original| original == ported.original),
                "target {} claimed by {} and {}",
                ported.target,
                previous.unwrap(),
                ported.original
            );
        }
    }

    /// The machined lane's traceability chain, end to end: inventory entry,
    /// forward lookup, reverse lookup.
    #[test]
    fn machined_cases_trace_through_the_registry() {
        let case = case_named("MGMMSG_listenerRoundTrip").expect("case in inventory");
        assert_eq!(case.module, "network");
        assert!(case.file.ends_with("test_machine_guard_messages.cpp"));

        assert_eq!(
            targets_for("MGMMSG_listenerRoundTrip"),
            ["contracts::machined::tests::listener_message_round_trips"]
        );
        assert_eq!(
            originals_covered_by("contracts::machined::tests::listener_message_round_trips"),
            ["MGMMSG_listenerRoundTrip"]
        );
    }

    /// Coverage arithmetic stays consistent with the table.
    #[test]
    fn coverage_counts_match_the_mapping_table() {
        let summary = coverage();
        let distinct_originals: std::collections::BTreeSet<_> =
            PORTED.iter().map(|p| p.original).collect();
        let distinct_targets: std::collections::BTreeSet<_> =
            PORTED.iter().map(|p| p.target).collect();

        assert_eq!(summary.mapped, distinct_originals.len());
        assert_eq!(summary.targets, distinct_targets.len());
        assert!(summary.mapped < summary.total);
        // Every module in the inventory is reachable for future lanes.
        assert!(cases_in_module("network").len() > 100);
    }
}
