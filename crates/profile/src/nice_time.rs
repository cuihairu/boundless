//! The `NiceTime` rendering of a stamp delta
//! (`cstdmf/profile.cpp:355-434`).
//!
//! `operator<<(ostream, NiceTime)` splits a stamp into whole microseconds
//! and a remainder below the microsecond ("picoseconds"), through a
//! double-based conversion the comments call out as the only scheme that
//! survives floating-point underflow. The output grows the reference's
//! units as it climbs: `0us`, `000 015.000us`-style sub-second renders,
//! `12sec 345 678.901us`, and `1min 2sec 003 456.789us` past the minute.

use crate::{stamps_per_second, stamps_to_seconds};

/// `operator<<(ostream, NiceTime)` (`profile.cpp:355-434`): render a
/// stamp delta the reference's way.
pub fn nice_time(stamps: u64) -> String {
    // "Overflows after 12 hours if we don't do this with doubles."
    // (profile.cpp:358). The reference freezes `microsecondsPerStamp`
    // into a static on first use; the port reads the live rate.
    let microseconds_per_stamp = 1_000_000.0 / stamps_per_second();

    let micro_double = (stamps as i64) as f64 * microseconds_per_stamp;
    let mut micros = micro_double as u64;

    // "turn micros value we have back into stamps, subtract it from the
    // total number of stamps, and turn the result into seconds just like
    // before (except in picoseconds this time)" (profile.cpp:363-371).
    let micros_back_to_stamps = ((micros as f64) / microseconds_per_stamp) as u64;
    let truncated_stamp_double = ((stamps.wrapping_sub(micros_back_to_stamps)) as i64) as f64;
    // The reference's `(uint32)` cast of an out-of-range double is UB; the
    // port's `as` saturates, which keeps the fixup below deterministic.
    let mut picos = (truncated_stamp_double * 1_000_000.0 * microseconds_per_stamp) as u32;

    // "Now see if micros was rounded and if so square it (hehe :)"
    // (profile.cpp:373-388): a negated picos value folds back into the
    // microseconds. `1 + ~picos` is the two's-complement negation.
    if picos > 1_000_000 {
        let neg_picos = picos.wrapping_neg();
        if neg_picos > 1_000_000 {
            picos = 0; // sanity check
        } else {
            picos = neg_picos;
            micros -= 1;
        }
    }

    let mut out = String::new();
    if micros == 0 && picos == 0 {
        out.push_str("0us");
        return out;
    }

    if micros >= 1_000_000 {
        let s = (micros / 1_000_000) as u32;
        if s >= 60 {
            out.push_str(&format!("{}min ", s / 60));
        }
        out.push_str(&format!("{}sec ", s % 60));
    }

    // (profile.cpp:418-422): the "%03d %03d" milliseconds/microseconds
    // pair, then the ".%03d" nanosecond tail (profile.cpp:426-428) —
    // "Only display this precision if <1min" is commented out upstream,
    // so the tail always renders (profile.cpp:424-425).
    out.push_str(&format!(
        "{:03} {:03}",
        (micros / 1000) % 1000,
        micros % 1000
    ));
    out.push_str(&format!(".{:03}", (picos / 1000) % 1000));
    out.push_str("us");
    out
}

/// `stampsToSeconds` re-exported beside the rendering it feeds.
pub fn seconds(stamps: u64) -> f64 {
    stamps_to_seconds(stamps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::set_stamps_per_second;

    /// At one stamp per microsecond the split is exact and the units grow
    /// with the magnitude the way the reference's branches do
    /// (`profile.cpp:393-431`).
    #[test]
    fn the_rate_of_one_micro_per_stamp_renders_the_unit_ladder() {
        set_stamps_per_second(1_000_000.0);

        assert_eq!(nice_time(0), "0us");
        assert_eq!(nice_time(15), "000 015.000us");
        assert_eq!(nice_time(999), "000 999.000us");
        // One second drops the leading zero-fill of the seconds field.
        assert_eq!(nice_time(1_000_000), "1sec 000 000.000us");
        assert_eq!(nice_time(1_234_567), "1sec 234 567.000us");
        // Past the minute the minutes prefix joins.
        assert_eq!(nice_time(60_000_000), "1min 0sec 000 000.000us");
        assert_eq!(nice_time(61_234_567), "1min 1sec 234 567.000us");
        assert_eq!(nice_time(125_000_000), "2min 5sec 000 000.000us");
    }

    /// An inexact rate leaves a picosecond remainder, split the way the
    /// double math splits it (`profile.cpp:360-366`): the whole
    /// microseconds truncate, and the leftover stamps come back as
    /// sub-microsecond picos.
    #[test]
    fn an_inexact_rate_leaves_a_pico_remainder() {
        // 2 MHz: half a microsecond per stamp. Three stamps are 1.5us —
        // one whole microsecond plus 500000 picos.
        set_stamps_per_second(2_000_000.0);
        assert_eq!(nice_time(3), "000 001.500us");
        assert_eq!(nice_time(7), "000 003.500us");
    }

    /// The rounding fixup (`profile.cpp:373-388`): a picos value that
    /// folded past its type's range negates back into the microseconds —
    /// in the reference the `(uint32)` cast overflows (UB), in the port
    /// it saturates and the negation squares it away.
    #[test]
    fn a_folded_picos_value_negates_back_into_the_micros() {
        // A tiny rate makes the picos product overflow `u32` massively;
        // the saturated value negates to 1, one microsecond steps back.
        set_stamps_per_second(3.0);
        // micro_double = 4 * 333333.33 = 1333333.33 -> micros 1333333;
        // micros-back-to-stamps truncates to 3; remainder 1 stamp ->
        // picos product 333333333333 saturates `u32`, negates to 1, and
        // one microsecond is returned to the micros side.
        assert_eq!(nice_time(4), "1sec 333 332.000us");
        // The value rounds to plain seconds rendering either way.
        assert_eq!(seconds(4), 4.0 / 3.0);
    }

    /// The sanity arm (`profile.cpp:379-382`): when even the negated
    /// picos exceeds its range, the picos zero out and only the
    /// truncated microseconds render.
    #[test]
    fn the_sanity_arm_zeroes_a_wild_picos_value() {
        // 700 kHz: ten stamps per seven microseconds. One stamp leaves
        // a remainder whose picos product is 1428571 — past a million,
        // and its negation (about 4.29e9) is past a million too, so the
        // value is unrecoverable and zeroed.
        set_stamps_per_second(700_000.0);
        assert_eq!(nice_time(1), "000 001.000us");
    }
}
