//! `RateLimitConfig` (`server/baseapp/rate_limit_config.{hpp,cpp}`): the
//! configured limits `RateLimitMessageFilter` enforces.
//!
//! The reference holds twelve `ServerAppOption<uint>` statics read from
//! `bw.xml` under `baseApp/clientUpstreamLimits/...`: four per-second
//! rates the operator configures, four per-tick budgets derived from them
//! in `postInit` (`PerSecond() / BaseAppConfig::updateHertz()`), and four
//! buffered-queue ceilings that are used as configured. This port keeps
//! the same twelve numbers with the reference defaults; the `bw.xml`
//! read arrives with the baseapp configuration layer, so the values are
//! a plain struct the caller builds -- the defaults are
//! [`RateLimitConfig::with_hertz`], any set of limits
//! [`RateLimitConfig::from_per_second`].

/// `DEFAULT_GAME_UPDATE_HERTZ` (`lib/server/common.hpp:55`): the update
/// rate the per-tick budgets derive from when nothing else is configured.
pub const DEFAULT_GAME_UPDATE_HERTZ: u32 = 10;

/// `RateLimitConfig`: the four per-second rates, their per-tick
/// derivations, and the buffered-queue ceilings, all in the reference's
/// units (messages or bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitConfig {
    warn_messages_per_second: u32,
    max_messages_per_second: u32,
    warn_bytes_per_second: u32,
    max_bytes_per_second: u32,

    warn_messages_per_tick: u32,
    max_messages_per_tick: u32,
    warn_bytes_per_tick: u32,
    max_bytes_per_tick: u32,

    warn_messages_buffered: u32,
    max_messages_buffered: u32,
    warn_bytes_buffered: u32,
    max_bytes_buffered: u32,
}

impl RateLimitConfig {
    /// The reference defaults (`BW_OPTION` initialisers in
    /// rate_limit_config.cpp) with the per-tick budgets derived from
    /// `updateHertz` the way `postInit` derives them.
    pub fn with_hertz(update_hertz: u32) -> Self {
        Self::from_per_second(
            250,    // warnMessagesPerSecond
            500,    // maxMessagesPerSecond
            20480,  // warnBytesPerSecond
            40960,  // maxBytesPerSecond
            500,    // warnMessagesBuffered
            1000,   // maxMessagesBuffered
            262144, // warnBytesBuffered
            524288, // maxBytesBuffered
            update_hertz,
        )
    }

    /// `RateLimitConfig::postInit`'s derivation over an arbitrary set of
    /// configured per-second rates: `NAMEPerTick.set( NAMEPerSecond() /
    /// updateHertz() )`. The buffered ceilings are taken as configured.
    #[allow(clippy::too_many_arguments)]
    pub fn from_per_second(
        warn_messages_per_second: u32,
        max_messages_per_second: u32,
        warn_bytes_per_second: u32,
        max_bytes_per_second: u32,
        warn_messages_buffered: u32,
        max_messages_buffered: u32,
        warn_bytes_buffered: u32,
        max_bytes_buffered: u32,
        update_hertz: u32,
    ) -> Self {
        let hertz = update_hertz.max(1);
        RateLimitConfig {
            warn_messages_per_second,
            max_messages_per_second,
            warn_bytes_per_second,
            max_bytes_per_second,
            warn_messages_per_tick: warn_messages_per_second / hertz,
            max_messages_per_tick: max_messages_per_second / hertz,
            warn_bytes_per_tick: warn_bytes_per_second / hertz,
            max_bytes_per_tick: max_bytes_per_second / hertz,
            warn_messages_buffered,
            max_messages_buffered,
            warn_bytes_buffered,
            max_bytes_buffered,
        }
    }

    /// `warnMessagesPerSecond`.
    pub fn warn_messages_per_second(&self) -> u32 {
        self.warn_messages_per_second
    }

    /// `maxMessagesPerSecond`.
    pub fn max_messages_per_second(&self) -> u32 {
        self.max_messages_per_second
    }

    /// `warnBytesPerSecond`.
    pub fn warn_bytes_per_second(&self) -> u32 {
        self.warn_bytes_per_second
    }

    /// `maxBytesPerSecond`.
    pub fn max_bytes_per_second(&self) -> u32 {
        self.max_bytes_per_second
    }

    /// `warnMessagesPerTick`.
    pub fn warn_messages_per_tick(&self) -> u32 {
        self.warn_messages_per_tick
    }

    /// `maxMessagesPerTick`.
    pub fn max_messages_per_tick(&self) -> u32 {
        self.max_messages_per_tick
    }

    /// `warnBytesPerTick`.
    pub fn warn_bytes_per_tick(&self) -> u32 {
        self.warn_bytes_per_tick
    }

    /// `maxBytesPerTick`.
    pub fn max_bytes_per_tick(&self) -> u32 {
        self.max_bytes_per_tick
    }

    /// `warnMessagesBuffered`.
    pub fn warn_messages_buffered(&self) -> u32 {
        self.warn_messages_buffered
    }

    /// `maxMessagesBuffered`.
    pub fn max_messages_buffered(&self) -> u32 {
        self.max_messages_buffered
    }

    /// `warnBytesBuffered`.
    pub fn warn_bytes_buffered(&self) -> u32 {
        self.warn_bytes_buffered
    }

    /// `maxBytesBuffered`.
    pub fn max_bytes_buffered(&self) -> u32 {
        self.max_bytes_buffered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference defaults with the default update rate: the per-second
    /// options and the `postInit` divisions both land on the documented
    /// numbers.
    #[test]
    fn the_reference_defaults_derive_the_documented_per_tick_budgets() {
        let config = RateLimitConfig::with_hertz(DEFAULT_GAME_UPDATE_HERTZ);

        assert_eq!(config.warn_messages_per_second(), 250);
        assert_eq!(config.max_messages_per_second(), 500);
        assert_eq!(config.warn_bytes_per_second(), 20_480);
        assert_eq!(config.max_bytes_per_second(), 40_960);
        assert_eq!(config.warn_messages_buffered(), 500);
        assert_eq!(config.max_messages_buffered(), 1000);
        assert_eq!(config.warn_bytes_buffered(), 262_144);
        assert_eq!(config.max_bytes_buffered(), 524_288);

        // `postInit`: NAMEPerSecond() / updateHertz(), integer division.
        assert_eq!(config.warn_messages_per_tick(), 25);
        assert_eq!(config.max_messages_per_tick(), 50);
        assert_eq!(config.warn_bytes_per_tick(), 2048);
        assert_eq!(config.max_bytes_per_tick(), 4096);
    }

    /// A configured rate set derives its per-tick budgets the same way,
    /// and a zero update rate cannot divide by zero -- the derivation
    /// floors the hertz at one, one tick being one second.
    #[test]
    fn a_configured_rate_set_derives_its_own_budgets() {
        let config = RateLimitConfig::from_per_second(100, 200, 1000, 2000, 10, 20, 300, 400, 4);

        assert_eq!(config.warn_messages_per_tick(), 25);
        assert_eq!(config.max_messages_per_tick(), 50);
        assert_eq!(config.warn_bytes_per_tick(), 250);
        assert_eq!(config.max_bytes_per_tick(), 500);
        assert_eq!(config.warn_messages_buffered(), 10);
        assert_eq!(config.max_messages_buffered(), 20);
        assert_eq!(config.warn_bytes_buffered(), 300);
        assert_eq!(config.max_bytes_buffered(), 400);

        let zero_hertz =
            RateLimitConfig::from_per_second(100, 200, 1000, 2000, 10, 20, 300, 400, 0);
        assert_eq!(
            zero_hertz.max_messages_per_tick(),
            200,
            "a zero hertz is one second"
        );
    }
}
