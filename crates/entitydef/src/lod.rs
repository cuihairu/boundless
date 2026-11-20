//! `data_lod_level.hpp` / `data_lod_level.cpp`: the property detail levels.
//!
//! Levels are declared inside `<LoDLevels>` as sections whose *text* is the
//! start distance, with optional `<hyst>` and `<label>` children. After
//! parsing they are sorted by start distance, but each level keeps the
//! *insertion* index it was created with — that is what `findLevel` reports
//! and what the entity parse remaps detail levels through.

use resources::datasection::DataSection;

use crate::data_type::children;
use crate::diagnostics::Diagnostics;

/// `MAX_DATA_LOD_LEVELS` (`network/basictypes.hpp`).
pub const MAX_DATA_LOD_LEVELS: usize = 6;

/// `DataLoDLevel::NO_LEVEL`.
pub const NO_LEVEL: i32 = -1;
/// `DataLoDLevel::OUTER_LEVEL`.
pub const OUTER_LEVEL: i32 = -2;

/// `<hyst>` default (`DataLoDLevels::addLevels`).
const DEFAULT_HYST: f32 = 10.0;

/// One `<LoDLevels>` entry (`DataLoDLevel`).
#[derive(Debug, Clone, PartialEq)]
pub struct DataLoDLevel {
    label: String,
    start: f32,
    hyst: f32,
    /// `finalise` outputs, kept for the runtime detail queries.
    low: f32,
    high: f32,
    /// The slot this level was created in; never updated after parsing.
    index: i32,
}

impl DataLoDLevel {
    /// `DataLoDLevel()`: the pristine sentinel.
    fn sentinel(index: i32) -> Self {
        Self {
            label: String::new(),
            start: f32::MAX,
            hyst: 0.0,
            low: f32::MAX,
            high: f32::MAX,
            index,
        }
    }

    /// The label this level was declared under (`label()`).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// `start_`.
    pub fn start(&self) -> f32 {
        self.start
    }

    /// The creation slot (`index()`), which `findLevel` reports.
    pub fn index(&self) -> i32 {
        self.index
    }

    /// `low_`.
    pub fn low(&self) -> f32 {
        self.low
    }

    /// `high_`.
    pub fn high(&self) -> f32 {
        self.high
    }

    /// `set( label, start, hyst )`; the index is left untouched.
    fn set(&mut self, label: &str, start: f32, hyst: f32) {
        self.label = label.to_string();
        self.start = start;
        self.hyst = hyst;
    }

    /// `finalise( pPrev, isLast )`: square the neighbour thresholds.
    fn finalise(&mut self, prev_start: Option<f32>, is_last: bool) {
        self.low = match prev_start {
            Some(start) => start * start,
            None => -1.0,
        };
        if !is_last {
            let v = self.start + self.hyst;
            self.high = v * v;
        }
    }
}

/// `DataLoDLevels`: up to [`MAX_DATA_LOD_LEVELS`] declared levels plus the
/// trailing sentinel slot.
#[derive(Debug, Clone, PartialEq)]
pub struct DataLoDLevels {
    /// `level_[ MAX_DATA_LOD_LEVELS + 1 ]`: the declared levels, sorted by
    /// start after parsing, followed by the sentinel.
    levels: Vec<DataLoDLevel>,
}

impl Default for DataLoDLevels {
    fn default() -> Self {
        Self::new()
    }
}

impl DataLoDLevels {
    /// `DataLoDLevels()`: `size_( 1 )` — one used slot, the sentinel, with
    /// the rest of the fixed array only reachable as levels are declared.
    pub fn new() -> Self {
        let mut levels = vec![DataLoDLevel::sentinel(0)];
        // Make the initial state valid.
        levels[0].finalise(None, true);
        Self { levels }
    }

    /// `size()`: defined levels plus the sentinel.
    pub fn size(&self) -> i32 {
        self.levels.len() as i32
    }

    /// `getLevel( i )`, the sorted-slot accessor.
    pub fn get_level(&self, i: i32) -> Option<&DataLoDLevel> {
        self.levels.get(i as usize)
    }

    /// `find( label )`: declared levels only, the sentinel is unsearchable.
    fn find(&self, label: &str) -> Option<&DataLoDLevel> {
        let defined = self.levels.len() - 1;
        self.levels[..defined].iter().find(|l| l.label == label)
    }

    /// `addLevels( pSection )`: an absent section means one LoD level.
    pub fn add_levels(&mut self, section: Option<&DataSection>, diag: &mut Diagnostics) -> bool {
        let Some(section) = section else {
            return true;
        };

        for child in children(section) {
            let start = child.as_float(0.0);
            let hyst = child
                .open_section("hyst")
                .map_or(DEFAULT_HYST, |s| s.as_float(DEFAULT_HYST));
            let label = child.read_string("label", "");

            let slot = if self.find(&label).is_none() {
                if self.levels.len() <= MAX_DATA_LOD_LEVELS {
                    // Reuse the last (sentinel) slot, then grow it back.
                    let slot = self.levels.len() - 1;
                    self.levels
                        .push(DataLoDLevel::sentinel(self.levels.len() as i32));
                    slot
                } else {
                    diag.error(format!(
                        "DataLoDLevels::addLevels: Only allowed {MAX_DATA_LOD_LEVELS} levels.\n"
                    ));
                    return false;
                }
            } else {
                let defined = self.levels.len() - 1;
                self.levels[..defined]
                    .iter()
                    .position(|l| l.label == label)
                    .expect("label found just above")
            };

            self.levels[slot].set(&label, start, hyst);
        }

        // Sort the declared levels by start (the trailing sentinel stays
        // out of the sort range), then re-finalise every slot including
        // the sentinel.
        let defined = self.levels.len() - 1;
        self.levels[..defined].sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut prev_start: Option<f32> = None;
        for position in 0..self.levels.len() {
            let is_last = position + 1 == self.levels.len();
            let level = &mut self.levels[position];
            level.finalise(prev_start, is_last);
            prev_start = Some(level.start);
        }
        true
    }

    /// `findLevel( level, pSection )`: no `<DetailLevel>` section means the
    /// outer level; an unknown label errors, reports level 0 and fails.
    pub fn find_level(
        &self,
        section: Option<&DataSection>,
        level: &mut i32,
        diag: &mut Diagnostics,
    ) -> bool {
        match section {
            Some(section) => match self.find(section.as_string()) {
                Some(found) => {
                    *level = found.index();
                    true
                }
                None => {
                    *level = 0;
                    diag.error(format!(
                        "DataLoDLevels:findLevel: Did not find '{}'\n",
                        section.as_string()
                    ));
                    false
                }
            },
            None => {
                *level = OUTER_LEVEL;
                true
            }
        }
    }

    /// `needsMoreDetail( level, priority )`.
    pub fn needs_more_detail(&self, level: i32, priority: f32) -> bool {
        priority < self.levels[level as usize].low
    }

    /// `needsLessDetail( level, priority )`.
    pub fn needs_less_detail(&self, level: i32, priority: f32) -> bool {
        priority > self.levels[level as usize].high
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_xml(xml: &str) -> DataSection {
        resources::datasection::parse("", xml.as_bytes()).expect("valid fixture xml")
    }

    fn add(xml: &str, diag: &mut Diagnostics) -> DataLoDLevels {
        let mut levels = DataLoDLevels::new();
        let section = parse_xml(xml);
        assert!(levels.add_levels(Some(&section), diag));
        levels
    }

    #[test]
    fn an_absent_section_leaves_one_level() {
        let mut diag = Diagnostics::new();
        let mut levels = DataLoDLevels::new();
        assert!(levels.add_levels(None, &mut diag));
        assert_eq!(levels.size(), 1);
        // The initial state is valid: low is -1, high is FLT_MAX.
        assert_eq!(levels.get_level(0).unwrap().low(), -1.0);
    }

    #[test]
    fn level_text_is_the_start_and_children_are_hyst_and_label() {
        let mut diag = Diagnostics::new();
        let levels = add(
            "<root> <level> 30 <label> near </label> <hyst> 5 </hyst> </level> </root>",
            &mut diag,
        );
        // size counts defined levels plus the sentinel.
        assert_eq!(levels.size(), 2);
        let near = levels.get_level(0).expect("declared level");
        assert_eq!(near.label(), "near");
        assert_eq!(near.start(), 30.0);
        assert_eq!(near.index(), 0);
        // high = (start + hyst)^2; the sentinel slot keeps FLT_MAX.
        assert_eq!(near.high(), 35.0 * 35.0);
        let sentinel = levels.get_level(1).expect("sentinel");
        assert_eq!(sentinel.low(), 30.0 * 30.0);
        assert_eq!(sentinel.high(), f32::MAX);
    }

    #[test]
    fn hyst_defaults_to_ten() {
        let mut diag = Diagnostics::new();
        let levels = add(
            "<root> <level> 4 <label> far </label> </level> </root>",
            &mut diag,
        );
        let far = levels.get_level(0).expect("declared level");
        assert_eq!(far.high(), 14.0 * 14.0);
    }

    #[test]
    fn absent_detail_level_is_the_outer_level() {
        let mut diag = Diagnostics::new();
        let levels = DataLoDLevels::new();
        let mut level = 0;
        assert!(levels.find_level(None, &mut level, &mut diag));
        assert_eq!(level, OUTER_LEVEL);
    }

    #[test]
    fn unknown_label_is_an_error_that_reports_zero() {
        let mut diag = Diagnostics::new();
        let levels = add(
            "<root> <level> 4 <label> near </label> </level> </root>",
            &mut diag,
        );
        let detail = parse_xml("<root> <far> far </far> </root>");
        let mut level = 99;
        assert!(!levels.find_level(detail.find_child("far"), &mut level, &mut diag));
        assert_eq!(level, 0);
        assert_eq!(
            diag.errors(),
            vec!["DataLoDLevels:findLevel: Did not find 'far'\n"]
        );
    }

    #[test]
    fn levels_sort_by_start_while_indexes_keep_insertion_order() {
        let mut diag = Diagnostics::new();
        let levels = add(
            "<root> \
                 <level> 10 <label> far </label> </level> \
                 <level> 1 <label> near </label> </level> \
                 <level> 5 <label> mid </label> </level> \
             </root>",
            &mut diag,
        );
        // Sorted by start.
        let labels: Vec<&str> = (0..3)
            .map(|i| levels.get_level(i).expect("level").label())
            .collect();
        assert_eq!(labels, vec!["near", "mid", "far"]);
        // findLevel reports the original insertion slot.
        for (label, expected_index) in [("far", 0), ("near", 1), ("mid", 2)] {
            let section = parse_xml(&format!("<root> <{label}> {label} </{label}> </root>"));
            let mut level = 0;
            assert!(levels.find_level(section.find_child(label), &mut level, &mut diag));
            assert_eq!(level, expected_index, "{label} keeps its insertion index");
        }
    }

    #[test]
    fn redeclaring_a_label_overwrites_in_place() {
        let mut diag = Diagnostics::new();
        let levels = add(
            "<root> \
                 <level> 1 <label> near </label> </level> \
                 <level> 2 <label> near </label> </level> \
             </root>",
            &mut diag,
        );
        assert_eq!(levels.size(), 2, "no second slot for a known label");
        assert_eq!(levels.get_level(0).expect("level").start(), 2.0);
    }

    #[test]
    fn the_seventh_distinct_level_is_rejected() {
        let mut diag = Diagnostics::new();
        let mut levels = DataLoDLevels::new();
        let section = parse_xml(
            "<root> \
                 <level> 0 <label> a </label> </level> \
                 <level> 1 <label> b </label> </level> \
                 <level> 2 <label> c </label> </level> \
                 <level> 3 <label> d </label> </level> \
                 <level> 4 <label> e </label> </level> \
                 <level> 5 <label> f </label> </level> \
                 <level> 6 <label> g </label> </level> \
             </root>",
        );
        assert!(!levels.add_levels(Some(&section), &mut diag));
        assert!(
            diag.errors()
                .iter()
                .any(|m| m.contains("Only allowed 6 levels"))
        );
    }

    #[test]
    fn the_default_levels_carry_only_the_sentinel() {
        let levels = DataLoDLevels::default();
        // The constructor's single slot is already a finalised sentinel.
        assert_eq!(levels.size(), 1, "only the sentinel slot");
        assert!(levels.get_level(0).is_some());
        assert!(levels.get_level(1).is_none());
    }

    #[test]
    fn detail_queries_compare_against_the_finalised_thresholds() {
        let mut diag = Diagnostics::new();
        let levels = add(
            "<root> \
                 <level> 10 <label> near </label> <hyst> 0 </hyst> </level> \
             </root>",
            &mut diag,
        );
        // low = -1 (first level), high = (10 + 0)^2.
        assert!(levels.needs_more_detail(0, -2.0));
        assert!(!levels.needs_more_detail(0, 5.0));
        assert!(levels.needs_less_detail(0, 101.0));
        assert!(!levels.needs_less_detail(0, 99.0));
    }
}
