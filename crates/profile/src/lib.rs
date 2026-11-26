//! BW-12-03 Profiler 与故障注入兼容入口: the frames and counters
//! compatibility layer (`lib/cstdmf/profile`).
//!
//! The reference's per-frame accounting is `ProfileVal`
//! (`cstdmf/profile.hpp:80-242`): a named counter in a [`ProfileGroup`]
//! with the eight watcher-visible fields — `count`, `inProgress`,
//! `sumQuantity`, `lastQuantity`, `sumTime`, `lastTime`, `sumIntTime`,
//! `sumIntTime`'s render — where "internal time" is the time a profile
//! was the innermost running frame of its group (`profile.hpp:89-113`
//! start, `:119-148` stop). Profiles of one group run stack-like; the
//! group keeps the [`ProfileVal::start`]/stop order and closes the
//! interrupted frame's internal clock on every switch.
//!
//! The acceptance, 同负载下关键字段与原版吻合, is pinned by driving a
//! scripted load of start/stop stamps and comparing every field against
//! the values the C++ bookkeeping produces for the same stamps. The port
//! therefore does not read the CPU counter itself: every entry point
//! takes the current stamp, and the stamps-per-second rate is settable
//! ([`set_stamps_per_second`], defaulting to 1e6 so
//! `foundation::time::stamp_now()` micros compose directly). The
//! reference's `timestamp()`/`stampsPerSecond()` are the real-clock
//! equivalents; arithmetic is `u64` wrapping exactly like `TimeStamp`.
//!
//! Around the counters the crate carries the reference's compat surface:
//! the [`ProfileGroup`] watcher tree (`summaries`, `details`,
//! `detailsInSeconds`, `profile.cpp:194-241`), the summary text of
//! `operator<<` (`profile.cpp:132-171`), the [`nice_time`] rendering of
//! `NiceTime` (`profile.cpp:355-434`), the write-a-value-to-reset path
//! of `operator>>` (`profile.cpp:179-184`) and the
//! [`ProfileGroupResetter`] it triggers (`profile.cpp:283-346`), and the
//! [`ScopedProfile`] guard of the `SCOPED_PROFILE` macros
//! (`profile.hpp:288-341`).
//!
//! The entry-frame collector — the [`profiler::Profiler`] the
//! `_bw_profile` module drives and its `g_profiler` singleton
//! (`cstdmf/profiler.hpp`/`.cpp`, `server/tools/bw_profile`) — is the
//! start/stop interface of BW-12-03-02.
//!
//! Layering: a domain library (layer 2) over [`watcher`]'s path tree and
//! [`foundation`]'s clock vocabulary, consumed by the role runtimes and
//! the BW-23 acquisition layer.

pub mod nice_time;
pub mod profile_group;
pub mod profile_val;
pub mod profiler;
pub mod resetter;

pub use nice_time::nice_time;
pub use profile_group::{ProfileGroup, SummaryWatcher};
pub use profile_val::{Field, FieldWatcher, ProfileVal, ProfileValHandle, ScopedProfile};
pub use profiler::{
    EventCategory, EventType, JsonDumpState, MAXIMUM_NUMBER_OF_THREADS, ProfileEntry, Profiler,
    ScopedProfiler, g_profiler, mount_enabled_watcher, our_thread_id,
};
pub use resetter::ProfileGroupResetter;

use std::cell::Cell;

thread_local! {
    /// The stamps-per-second rate of the port's virtual clock. The
    /// reference's `stampsPerSecond()` measures the CPU counter once
    /// (`timestamp.hpp`); the port fixes the rate at 1e6 — one stamp per
    /// microsecond — so `foundation::time::stamp_now()` stamps compose
    /// directly and the scripted loads stay deterministic.
    static STAMPS_PER_SECOND: Cell<f64> = const { Cell::new(1_000_000.0) };
    /// `ProfileVal::s_warningPeriod_`
    /// (`profile.cpp:14`): "took too long" threshold, defaulting to
    /// never (`uint64 max`).
    static WARNING_PERIOD: Cell<u64> = const { Cell::new(u64::MAX) };
}

/// Retune the virtual clock's rate. Set it once, before any seconds
/// rendering — the reference's rate is a machine constant.
pub fn set_stamps_per_second(stamps_per_second: f64) {
    STAMPS_PER_SECOND.with(|rate| rate.set(stamps_per_second));
}

/// The current `stampsPerSecond()` rate.
pub fn stamps_per_second() -> f64 {
    STAMPS_PER_SECOND.with(Cell::get)
}

/// `stampsToSeconds()` (`timestamp.hpp`): stamps into seconds at the
/// current rate.
pub fn stamps_to_seconds(stamps: u64) -> f64 {
    stamps as f64 / stamps_per_second()
}

/// `ProfileVal::setWarningPeriod()` (`profile.hpp:237-238`).
pub fn set_warning_period(stamps: u64) {
    WARNING_PERIOD.with(|period| period.set(stamps));
}

/// The current `s_warningPeriod_` (`profile.cpp:14`).
pub fn warning_period() -> u64 {
    WARNING_PERIOD.with(Cell::get)
}
