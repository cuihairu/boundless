//! `volatile_info.hpp` / `volatile_info.cpp`: what changes often enough to be
//! worth its own update.
//!
//! Priorities are squared on their way in (`asPriority`), so all comparisons
//! happen in squared-distance space, matching the LoD thresholds.

use resources::datasection::DataSection;

use crate::diagnostics::Diagnostics;

/// `VolatileInfo::ALWAYS`.
pub const ALWAYS: f32 = f32::MAX;

/// A `Volatile` section (`VolatileInfo`). Every priority starts at -1, the
/// "not volatile at all" marker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolatileInfo {
    position_priority: f32,
    yaw_priority: f32,
    pitch_priority: f32,
    roll_priority: f32,
}

impl Default for VolatileInfo {
    fn default() -> Self {
        Self {
            position_priority: -1.0,
            yaw_priority: -1.0,
            pitch_priority: -1.0,
            roll_priority: -1.0,
        }
    }
}

impl VolatileInfo {
    pub fn position_priority(&self) -> f32 {
        self.position_priority
    }

    pub fn yaw_priority(&self) -> f32 {
        self.yaw_priority
    }

    pub fn pitch_priority(&self) -> f32 {
        self.pitch_priority
    }

    pub fn roll_priority(&self) -> f32 {
        self.roll_priority
    }

    /// `shouldSendPosition()`.
    pub fn should_send_position(&self) -> bool {
        self.position_priority > 0.0
    }

    /// `isLessVolatileThan`: this info needs *more* updates than `info`.
    pub fn is_less_volatile_than(&self, info: &VolatileInfo) -> bool {
        self.position_priority < info.position_priority
            || self.yaw_priority < info.yaw_priority
            || self.pitch_priority < info.pitch_priority
            || self.roll_priority < info.roll_priority
    }

    /// `isValid`: yaw, pitch and roll priorities must be descending.
    pub fn is_valid(&self) -> bool {
        self.yaw_priority >= self.pitch_priority && self.pitch_priority >= self.roll_priority
    }

    /// `VolatileInfo::parse`. An absent section is fine: the instance keeps
    /// its constructor defaults (or, during inheritance, the parent's info).
    pub fn parse(&mut self, section: Option<&DataSection>, diag: &mut Diagnostics) -> bool {
        let Some(section) = section else {
            return true;
        };

        self.position_priority = as_priority(section.open_section("position"));
        self.yaw_priority = as_priority(section.open_section("yaw"));
        self.pitch_priority = as_priority(section.open_section("pitch"));
        self.roll_priority = as_priority(section.open_section("roll"));

        if !self.is_valid() {
            let condition = if self.yaw_priority < self.pitch_priority {
                "yaw < pitch"
            } else {
                "pitch < roll"
            };
            diag.error(format!(
                "VolatileInfo::parse: Invalid direction priorities ({condition}). \
The values for yaw, pitch and roll must be descending.\n"
            ));
            return false;
        }

        true
    }
}

/// `VolatileInfo::asPriority`: an explicit -1 means "always", anything else
/// is squared.
fn as_priority(section: Option<&DataSection>) -> f32 {
    match section {
        Some(section) => {
            let value = section.as_float(-1.0);
            if value == -1.0 {
                ALWAYS
            } else {
                value * value
            }
        }
        None => -1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_xml(xml: &str) -> DataSection {
        resources::datasection::parse("test", xml.as_bytes()).expect("valid fixture xml")
    }

    #[test]
    fn an_absent_section_keeps_the_defaults() {
        let mut info = VolatileInfo::default();
        let mut diag = Diagnostics::new();
        assert!(info.parse(None, &mut diag));
        assert_eq!(info, VolatileInfo::default());
        assert!(!info.should_send_position());
    }

    #[test]
    fn priorities_are_squared_and_minus_one_means_always() {
        let mut info = VolatileInfo::default();
        let section = parse_xml(
            "<root> \
                 <position> 10 </position> \
                 <yaw> -1 </yaw> \
                 <pitch> 3 </pitch> \
                 <roll> 0 </roll> \
             </root>",
        );
        let mut diag = Diagnostics::new();
        assert!(info.parse(Some(&section), &mut diag));
        assert_eq!(info.position_priority(), 100.0);
        assert_eq!(info.yaw_priority(), ALWAYS);
        assert_eq!(info.pitch_priority(), 9.0);
        assert_eq!(info.roll_priority(), 0.0);
        assert!(info.should_send_position());
        assert!(info.is_valid());
    }

    #[test]
    fn absent_directions_stay_not_volatile() {
        let mut info = VolatileInfo::default();
        let section = parse_xml("<root> <position> 5 </position> </root>");
        let mut diag = Diagnostics::new();
        assert!(info.parse(Some(&section), &mut diag));
        assert_eq!(info.position_priority(), 25.0);
        assert_eq!(info.yaw_priority(), -1.0);
        assert_eq!(info.pitch_priority(), -1.0);
        assert_eq!(info.roll_priority(), -1.0);
        assert!(info.is_valid(), "-1 fields stay descending");
    }

    #[test]
    fn an_empty_direction_section_also_means_always() {
        let mut info = VolatileInfo::default();
        let section = parse_xml("<root> <position/> <yaw/> <pitch/> <roll/> </root>");
        let mut diag = Diagnostics::new();
        assert!(info.parse(Some(&section), &mut diag));
        assert_eq!(info.position_priority(), ALWAYS);
        assert_eq!(info.roll_priority(), ALWAYS);
        assert!(info.is_valid());
    }

    #[test]
    fn descending_violations_are_named_by_the_first_bad_pair() {
        // yaw < pitch is the violated pair.
        let mut info = VolatileInfo::default();
        let section =
            parse_xml("<root> <yaw> 1 </yaw> <pitch> 2 </pitch> <roll> 3 </roll> </root>");
        let mut diag = Diagnostics::new();
        assert!(!info.parse(Some(&section), &mut diag));
        assert_eq!(
            diag.errors(),
            vec![
                "VolatileInfo::parse: Invalid direction priorities (yaw < pitch). \
The values for yaw, pitch and roll must be descending.\n"
            ]
        );

        // pitch < roll with yaw fine (values compare squared).
        let mut info = VolatileInfo::default();
        let section =
            parse_xml("<root> <yaw> 5 </yaw> <pitch> 3 </pitch> <roll> 4 </roll> </root>");
        let mut diag = Diagnostics::new();
        assert!(!info.parse(Some(&section), &mut diag));
        assert_eq!(
            diag.errors(),
            vec![
                "VolatileInfo::parse: Invalid direction priorities (pitch < roll). \
The values for yaw, pitch and roll must be descending.\n"
            ]
        );
    }

    #[test]
    fn less_volatile_compares_all_four_priorities() {
        // A higher priority value means MORE volatile, so the defaults
        // (all -1) are the least volatile there is.
        let base = VolatileInfo::default();
        let pos = VolatileInfo {
            position_priority: 4.0,
            ..VolatileInfo::default()
        };
        assert!(!pos.is_less_volatile_than(&base));
        assert!(base.is_less_volatile_than(&pos));

        let roll = VolatileInfo {
            roll_priority: 1.0,
            ..VolatileInfo::default()
        };
        assert!(!roll.is_less_volatile_than(&base));
        assert!(base.is_less_volatile_than(&roll));
    }
}
