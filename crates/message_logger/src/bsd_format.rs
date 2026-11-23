//! The value formatters from `lib/network/bsd_snprintf.cpp` — the OpenSSH
//! descendants `bsdFormatString`, `bsdFormatInt` (`fmtint`) and
//! `bsdFormatFloat` — that `LogStringPrinter` drives to render each piece of
//! a log line. Ported algorithm-for-algorithm, including the loop caps and
//! the padding arithmetic, so the rendered text is byte-identical.

use crate::format_string::format_flags;

/// `bsdFormatString` (`bsd_snprintf.cpp:379-411`): pad, copy, pad. `max`
/// caps both the length taken from `value` and the total characters
/// emitted; `max == -1` is unbounded. A `min` shorter than the text pads on
/// the right unless `DP_F_MINUS` flips it to the left.
pub fn bsd_format_string(value: &str, flags: i32, min: i32, max: i32, out: &mut String) {
    let bytes = value.as_bytes();

    let mut strln = 0usize;
    while (max == -1 || (strln as i32) < max) && strln < bytes.len() {
        strln += 1;
    }

    let mut padlen = min - strln as i32;
    if padlen < 0 {
        padlen = 0;
    }
    if flags & format_flags::DP_F_MINUS != 0 {
        padlen = -padlen; // Left justify.
    }

    let mut cnt = 0i32;
    while padlen > 0 && (cnt < max || max == -1) {
        out.push(' ');
        padlen -= 1;
        cnt += 1;
    }
    let mut taken = 0usize;
    while taken < strln && (cnt < max || max == -1) {
        out.push(bytes[taken] as char);
        taken += 1;
        cnt += 1;
    }
    while padlen < 0 && (cnt < max || max == -1) {
        out.push(' ');
        padlen += 1;
        cnt += 1;
    }
}

/// `bsdFormatInt`/`fmtint` (`bsd_snprintf.cpp:416-476`): digits in `base`,
/// then the sign, zero pad (between sign and digits), space pad, and the
/// left-justify tail. `max` is the minimum digit width before zero padding
/// (0 when unset); `min` the total field width.
pub fn bsd_format_int(value: i64, base: u8, min: i32, max: i32, flags: i32, out: &mut String) {
    let mut signvalue = 0u8;
    let mut max = max;
    if max < 0 {
        max = 0;
    }

    let mut uvalue = value as u64;
    if flags & format_flags::DP_F_UNSIGNED == 0 {
        if value < 0 {
            signvalue = b'-';
            uvalue = value.wrapping_neg() as u64;
        } else if flags & format_flags::DP_F_PLUS != 0 {
            signvalue = b'+';
        } else if flags & format_flags::DP_F_SPACE != 0 {
            signvalue = b' ';
        }
    }

    let caps = flags & format_flags::DP_F_UP != 0;
    let digits: &[u8; 16] = if caps {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };

    let mut convert = [0u8; 20];
    let mut place = 0usize;
    loop {
        convert[place] = digits[(uvalue % base as u64) as usize];
        place += 1;
        uvalue /= base as u64;
        if uvalue == 0 || place == 20 {
            break;
        }
    }
    if place == 20 {
        place -= 1;
    }

    let mut zpadlen = max - place as i32;
    let mut spadlen = min - std::cmp::max(max, place as i32) - i32::from(signvalue != 0);
    if zpadlen < 0 {
        zpadlen = 0;
    }
    if spadlen < 0 {
        spadlen = 0;
    }
    if flags & format_flags::DP_F_ZERO != 0 {
        zpadlen = std::cmp::max(zpadlen, spadlen);
        spadlen = 0;
    }
    if flags & format_flags::DP_F_MINUS != 0 {
        spadlen = -spadlen; // Left justify.
    }

    // Spaces.
    while spadlen > 0 {
        out.push(' ');
        spadlen -= 1;
    }
    // Sign.
    if signvalue != 0 {
        out.push(signvalue as char);
    }
    // Zeros.
    while zpadlen > 0 {
        out.push('0');
        zpadlen -= 1;
    }
    // Digits.
    while place > 0 {
        place -= 1;
        out.push(convert[place] as char);
    }
    // Left justified spaces.
    while spadlen < 0 {
        out.push(' ');
        spadlen += 1;
    }
}

/// `my_modf` (`bsd_snprintf.cpp:505-545`): a math-library-free `modf`,
/// scaling the value down by decades until its integer part fits a `long`.
fn my_modf(x0: f64, iptr: &mut f64) -> f64 {
    let mut x = x0;
    let mut f = 1.0f64;
    let mut last_l = 0i64;

    let mut i = 0;
    while i < 100 {
        last_l = x as i64;
        let l = last_l as f64;
        if l <= x + 1.0 && l >= x - 1.0 {
            break;
        }
        x *= 0.1;
        f *= 10.0;
        i += 1;
    }

    if i == 100 {
        // The number is beyond what we can handle.
        *iptr = 0.0;
        return 0.0;
    }

    if i != 0 {
        let l = last_l as f64;
        let mut i2 = 0.0;
        let ret = my_modf(x0 - l * f, &mut i2);
        *iptr = l * f + i2;
        return ret;
    }

    *iptr = last_l as f64;
    x - *iptr
}

/// `abs_val`.
fn abs_val(value: f64) -> f64 {
    if value < 0.0 { -value } else { value }
}

/// `POW10`.
fn pow10(mut exp: i32) -> f64 {
    let mut result = 1.0f64;
    while exp > 0 {
        result *= 10.0;
        exp -= 1;
    }
    result
}

/// `ROUND`. Values beyond the `int64` range saturate (the C cast is
/// undefined there); keeping the arithmetic defined makes the huge-input
/// path deterministic.
fn round_to_llong(value: f64) -> i64 {
    let intpart = value as i64;
    if value - intpart as f64 >= 0.5 {
        intpart.saturating_add(1)
    } else {
        intpart
    }
}

/// `bsdFormatFloat` (`bsd_snprintf.cpp:563-752`): the two-conversion decimal
/// render — integer digits via repeated `my_modf` division by ten, the
/// fractional part scaled up by `POW10(max)` and rounded, with `DP_F_ZERO`
/// padding between the sign and the digits. `max` defaults to 6 decimals and
/// caps at 16.
pub fn bsd_format_float(fvalue: f64, min: i32, max: i32, flags: i32, out: &mut String) {
    if fvalue.is_nan() {
        out.push_str("nan");
        return;
    }
    if fvalue.is_infinite() {
        out.push_str(if fvalue < 0.0 { "-inf" } else { "inf" });
        return;
    }

    let mut signvalue = 0u8;
    let mut max = max;
    if max < 0 {
        max = 6;
    }

    let ufvalue = abs_val(fvalue);

    if fvalue < 0.0 {
        signvalue = b'-';
    } else if flags & format_flags::DP_F_PLUS != 0 {
        signvalue = b'+';
    } else if flags & format_flags::DP_F_SPACE != 0 {
        signvalue = b' ';
    }

    // The conversion method only supports 16 digits past the decimal.
    if max > 16 {
        max = 16;
    }

    let mut intpart = 0.0f64;
    let temp = ufvalue;
    my_modf(temp, &mut intpart);

    let mut fracpart = round_to_llong(pow10(max) * (ufvalue - intpart)) as f64;
    if fracpart >= pow10(max) {
        intpart += 1.0;
        fracpart -= pow10(max);
    }

    // Convert the integer part, least-significant digit first.
    let mut iconvert = [0u8; 311];
    let mut iplace = 0usize;
    loop {
        let temp = intpart * 0.1;
        my_modf(temp, &mut intpart);
        let idx = ((temp - intpart + 0.05) * 10.0) as i32;
        iconvert[iplace] = b"0123456789abcdef"[idx as usize];
        iplace += 1;
        if !(intpart != 0.0 && iplace < 311) {
            break;
        }
    }
    if iplace == 311 {
        iplace -= 1;
    }

    // Convert the fractional part the same way.
    let mut fconvert = [0u8; 311];
    let mut fplace = 0usize;
    if fracpart != 0.0 {
        loop {
            let temp = fracpart * 0.1;
            my_modf(temp, &mut fracpart);
            let idx = ((temp - fracpart + 0.05) * 10.0) as i32;
            fconvert[fplace] = b"0123456789abcdef"[idx as usize];
            fplace += 1;
            if !(fracpart != 0.0 && fplace < 311) {
                break;
            }
        }
        if fplace == 311 {
            fplace -= 1;
        }
    }

    // One for the decimal point, another if a sign is printed.
    let mut padlen = min - iplace as i32 - max - 1 - i32::from(signvalue != 0);
    let mut zpadlen = max - fplace as i32;
    if zpadlen < 0 {
        zpadlen = 0;
    }
    if padlen < 0 {
        padlen = 0;
    }
    if flags & format_flags::DP_F_MINUS != 0 {
        padlen = -padlen; // Left justify.
    }

    if flags & format_flags::DP_F_ZERO != 0 && padlen > 0 {
        if signvalue != 0 {
            out.push(signvalue as char);
            padlen -= 1;
            signvalue = 0;
        }
        while padlen > 0 {
            out.push('0');
            padlen -= 1;
        }
    }
    while padlen > 0 {
        out.push(' ');
        padlen -= 1;
    }
    if signvalue != 0 {
        out.push(signvalue as char);
    }
    while iplace > 0 {
        iplace -= 1;
        out.push(iconvert[iplace] as char);
    }
    if max > 0 {
        out.push('.');
        while zpadlen > 0 {
            out.push('0');
            zpadlen -= 1;
        }
        while fplace > 0 {
            fplace -= 1;
            out.push(fconvert[fplace] as char);
        }
    }
    while padlen < 0 {
        out.push(' ');
        padlen += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt_str(value: &str, flags: i32, min: i32, max: i32) -> String {
        let mut out = String::new();
        bsd_format_string(value, flags, min, max, &mut out);
        out
    }

    fn fmt_int(value: i64, base: u8, min: i32, max: i32, flags: i32) -> String {
        let mut out = String::new();
        bsd_format_int(value, base, min, max, flags, &mut out);
        out
    }

    fn fmt_float(value: f64, min: i32, max: i32, flags: i32) -> String {
        let mut out = String::new();
        bsd_format_float(value, min, max, flags, &mut out);
        out
    }

    #[test]
    fn string_padding_matches_printf() {
        assert_eq!(fmt_str("hi", 0, -1, -1), "hi");
        assert_eq!(fmt_str("hi", 0, 5, -1), "   hi");
        assert_eq!(fmt_str("hi", 0, -1, 1), "h");
        assert_eq!(fmt_str("hello", format_flags::DP_F_MINUS, 7, -1), "hello  ");
        // max caps the total emission, padding included.
        assert_eq!(fmt_str("hi", 0, 5, 1), " ");
        // A shorter max truncates before padding.
        assert_eq!(fmt_str("hello", format_flags::DP_F_MINUS, 2, 3), "hel");
    }

    #[test]
    fn int_padding_signs_and_bases() {
        assert_eq!(fmt_int(42, 10, -1, -1, 0), "42");
        assert_eq!(fmt_int(42, 10, 5, -1, 0), "   42");
        assert_eq!(fmt_int(-42, 10, -1, -1, 0), "-42");
        assert_eq!(fmt_int(42, 10, 5, -1, format_flags::DP_F_PLUS), "  +42");
        assert_eq!(fmt_int(42, 10, 5, -1, format_flags::DP_F_SPACE), "   42");
        // Zero padding eats the space padding and sits after the sign.
        assert_eq!(fmt_int(42, 10, 5, -1, format_flags::DP_F_ZERO), "00042");
        assert_eq!(fmt_int(-42, 10, 5, -1, format_flags::DP_F_ZERO), "-0042");
        // max pads zeros when it exceeds the digits.
        assert_eq!(fmt_int(42, 10, -1, 5, 0), "00042");
        // Left justify.
        assert_eq!(fmt_int(42, 10, 5, -1, format_flags::DP_F_MINUS), "42   ");
        // Bases and upper case.
        assert_eq!(fmt_int(255, 16, -1, -1, 0), "ff");
        assert_eq!(fmt_int(255, 16, -1, -1, format_flags::DP_F_UP), "FF");
        assert_eq!(fmt_int(8, 8, -1, -1, 0), "10");
        // i64::MIN negates by wrapping, like the two's complement cast.
        assert_eq!(fmt_int(i64::MIN, 10, -1, -1, 0), "-9223372036854775808");
    }

    #[test]
    fn float_renders_the_default_six_decimals() {
        assert_eq!(fmt_float(0.0, -1, -1, 0), "0.000000");
        assert_eq!(fmt_float(1.5, -1, -1, 0), "1.500000");
        assert_eq!(fmt_float(-2.25, -1, -1, 0), "-2.250000");
        assert_eq!(fmt_float(123.456789, -1, -1, 0), "123.456789");
    }

    #[test]
    fn float_honours_width_precision_and_flags() {
        assert_eq!(fmt_float(1.5, 10, -1, 0), "  1.500000");
        assert_eq!(fmt_float(1.5, -1, 2, 0), "1.50");
        // max == 0 still rounds the fraction into the integer part.
        assert_eq!(fmt_float(1.5, -1, 0, 0), "2");
        // Default six decimals leave no room for zero padding at width 5;
        // a width-12 field does.
        assert_eq!(fmt_float(1.5, 5, -1, format_flags::DP_F_ZERO), "1.500000");
        assert_eq!(
            fmt_float(1.5, 12, -1, format_flags::DP_F_ZERO),
            "00001.500000"
        );
        assert_eq!(fmt_float(1.5, 5, -1, format_flags::DP_F_MINUS), "1.500000");
        assert_eq!(fmt_float(1.5, -1, -1, format_flags::DP_F_PLUS), "+1.500000");
        // Precision beyond 16 is capped.
        assert_eq!(fmt_float(0.5, -1, 20, 0), "0.5000000000000000");
        // Carry: rounding the fraction past 1.0 rolls into the integer part.
        assert_eq!(fmt_float(0.99999999, -1, 2, 0), "1.00");
    }

    #[test]
    fn float_special_values_pass_through() {
        assert_eq!(fmt_float(f64::NAN, -1, -1, 0), "nan");
        assert_eq!(fmt_float(f64::INFINITY, -1, -1, 0), "inf");
        assert_eq!(fmt_float(f64::NEG_INFINITY, -1, -1, 0), "-inf");
    }

    #[test]
    fn large_integers_and_floats_survive_the_decade_walk() {
        assert_eq!(fmt_float(4294967296.0, -1, -1, 0), "4294967296.000000");
        assert_eq!(fmt_float(1e20, -1, -1, 0), "100000000000000000000.000000");
        // Astronomically large values bail out of the decade walk after 100
        // attempts; the rounding saturates at the int64 range (the C cast is
        // undefined there), which renders deterministically as a saturated
        // fraction past the carried integer digit.
        assert_eq!(fmt_float(1e300, -1, -1, 0), "1.9223372036853776000");
    }

    #[test]
    fn twenty_digit_integers_hit_the_conversion_cap() {
        // -1 read unsigned is the 20-digit 18446744073709551615; the
        // conversion buffer caps at 19 digits, exactly as the reference's
        // fixed 20-slot buffer does.
        assert_eq!(
            fmt_int(-1, 10, -1, -1, format_flags::DP_F_UNSIGNED),
            "8446744073709551615"
        );
    }

    #[test]
    fn float_zero_padding_eats_the_sign_slot() {
        // DP_F_ZERO with a sign: the sign prints first and the zero pad
        // follows it (the reference's padlen already accounted for it).
        assert_eq!(
            fmt_float(-1.5, 12, -1, format_flags::DP_F_ZERO),
            "-001.500000"
        );
    }

    #[test]
    fn float_left_justify_pads_after_the_number() {
        // padlen is positive at width 12, so MINUS flips it to a tail.
        assert_eq!(
            fmt_float(1.5, 12, -1, format_flags::DP_F_MINUS),
            "1.500000    "
        );
        // The space flag prints a bare space sign.
        assert_eq!(
            fmt_float(1.5, -1, -1, format_flags::DP_F_SPACE),
            " 1.500000"
        );
    }
}
