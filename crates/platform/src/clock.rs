//! Monotonic and wall clocks, the BW-15-01 basis for `cstdmf::getSystemTime`
//! style queries. OS calls live only in this crate (BW-16-02-01); consumers
//! see plain microsecond stamps.

use foundation::time::Stamp;

/// Monotonic microseconds since an arbitrary process-fixed origin (the
/// reference uses `mach_absolute_time`/`clock_gettime(CLOCK_MONOTONIC)`).
pub fn monotonic_now_micros() -> Stamp {
    use std::time::Instant;
    // Instant::now is monotonic on every supported platform; the elapsed
    // duration since a lazily-frozen epoch keeps the value in u64 micros.
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    Stamp::from_micros(origin.elapsed().as_micros() as u64)
}

/// Wall-clock microseconds since the Unix epoch
/// (`gettimeofday` in the reference).
pub fn wall_now_micros() -> Stamp {
    use std::time::{SystemTime, UNIX_EPOCH};
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        // Clocks before the epoch collapse to zero rather than panicking.
        .unwrap_or(0);
    Stamp::from_micros(micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_clock_never_goes_backwards() {
        let a = monotonic_now_micros();
        let b = monotonic_now_micros();
        assert!(b.micros() >= a.micros(), "{a:?} then {b:?}");
    }

    #[test]
    fn monotonic_clock_starts_near_zero() {
        // First call in a fresh process is within a second of the origin.
        let t = monotonic_now_micros();
        assert!(t.micros() < 1_000_000, "unexpected origin drift: {t:?}");
    }

    #[test]
    fn wall_clock_is_past_2020() {
        // 2020-01-01T00:00:00Z = 1577836800000000 micros; the test binary
        // cannot plausibly run before that on a CI host.
        assert!(wall_now_micros().micros() >= 1_577_836_800_000_000);
    }

    #[test]
    fn wall_and_monotonic_advance() {
        let a = wall_now_micros();
        let m0 = monotonic_now_micros();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let m1 = monotonic_now_micros();
        let b = wall_now_micros();
        assert!(b.micros() > a.micros(), "wall must advance");
        assert!(m1.micros() > m0.micros(), "monotonic must advance");
    }
}
