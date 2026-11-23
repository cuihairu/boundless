//! BW-06-03-04 the proxy's proactive client disconnect and kick-out.
//!
//! The reference disconnect path is `Proxy::onClientDeath`
//! (`server/baseapp/proxy.cpp`), reached when the client asks to go
//! (`Proxy::disconnectClient`), the channel dies or overflows, or the server
//! logs a session off for a re-logon:
//!
//! * the reason code selects the cleanup: a timed-out or rate-limited channel
//!   is condemned without waiting for its finalising acks
//!   (`shouldCondemnClient` is cleared), everything else keeps them;
//! * `logOffClient` aborts any data downloads, tells the client it is logged
//!   off (`ClientInterface::loggedOffArgs { reason = 0 }`, the reason is not
//!   used yet) when it is connected, then detaches;
//! * `detachFromClient` removes the proxy from the BaseApp's proxy list,
//!   clears the channel (`setClientChannel(NULL)` frees the filter, primer and
//!   user data, then the channel is shut down or destroyed), disables the cell
//!   witness when the cell is still being messaged, and resets the session
//!   state (buffered bundle, rate limiter, session key, encryption key,
//!   enabled-entities flags, proxy pusher);
//! * the script's `onClientDeath` callback (or the deprecated `clientDead`)
//!   is invoked last, and the download callbacks fire.
//!
//! The acceptance is that the reason the client is told matches the reference
//! (a `loggedOff` with reason 0) and that the proxy state is cleaned up.

use crate::proxy_client::ClientChannel;

/// `ClientDisconnectReason::CLIENT_DISCONNECT_CLIENT_REQUESTED` (`proxy.hpp`).
pub const CLIENT_DISCONNECT_CLIENT_REQUESTED: u8 = 0;
/// `ClientDisconnectReason::CLIENT_DISCONNECT_GIVEN_TO_OTHER_PROXY`.
pub const CLIENT_DISCONNECT_GIVEN_TO_OTHER_PROXY: u8 = 1;
/// `ClientDisconnectReason::CLIENT_DISCONNECT_RATE_LIMITS_EXCEEDED`.
pub const CLIENT_DISCONNECT_RATE_LIMITS_EXCEEDED: u8 = 2;
/// `ClientDisconnectReason::CLIENT_DISCONNECT_TIMEOUT`.
pub const CLIENT_DISCONNECT_TIMEOUT: u8 = 3;
/// `ClientDisconnectReason::CLIENT_DISCONNECT_BASE_RESTORE`.
pub const CLIENT_DISCONNECT_BASE_RESTORE: u8 = 4;
/// `ClientDisconnectReason::CLIENT_DISCONNECT_SHUTDOWN`.
pub const CLIENT_DISCONNECT_SHUTDOWN: u8 = 5;
/// `ClientDisconnectReason::CLIENT_DISCONNECT_CELL_RESTORE_FAILED`.
pub const CLIENT_DISCONNECT_CELL_RESTORE_FAILED: u8 = 6;

/// `clientDisconnectReasonToString` (`proxy.cpp`).
pub fn client_disconnect_reason_to_string(reason: u8) -> &'static str {
    const REASON_STRINGS: [&str; 7] = [
        "Requested by client",
        "Given to other proxy",
        "Rate limit exceeded",
        "Connection timeout",
        "Proxy entity was restored",
        "BaseApp shutdown",
        "Cell entity restoration failure",
    ];
    REASON_STRINGS
        .get(reason as usize)
        .copied()
        .unwrap_or("Invalid reason")
}

/// The `Proxy` state the disconnect path reads and cleans
/// (`proxy.cpp` `onClientDeath`/`logOffClient`/`detachFromClient`).
#[derive(Clone, Debug, Default)]
pub struct ProxyDisconnectState {
    /// `pClientChannel_` (from `attachToClient`).
    pub channel: Option<ClientChannel>,
    /// Whether a client bundle is buffered (`pBufferedClientBundle_`), so
    /// `hasClient` is true even before the channel is bound.
    pub buffered_bundle: bool,
    /// Whether the client channel is connected.
    pub client_connected: bool,
    /// `isGivingClientAway_`.
    pub is_giving_client_away: bool,
    /// `cellHasWitness_`.
    pub cell_has_witness: bool,
    /// `shouldSendToCell`: the cell is still being messaged.
    pub should_send_to_cell: bool,
    /// `entitiesEnabled_`.
    pub entities_enabled: bool,
    /// `basePlayerCreatedOnClient_`.
    pub base_player_created: bool,
    /// `shouldRunCallbackOnEntitiesEnabled_`.
    pub should_run_callback_on_entities_enabled: bool,
    /// `pProxyPusher_ != NULL`.
    pub proxy_pusher: bool,
    /// Whether data downloads are in flight (`dataDownloads_`).
    pub has_downloads: bool,
}

impl ProxyDisconnectState {
    /// `hasClient()`: the proxy has a client channel or a buffered bundle.
    pub fn has_client(&self) -> bool {
        self.channel.is_some() || self.buffered_bundle
    }

    /// `Proxy::onClientDeath`: route the disconnect reason into the cleanup.
    ///
    /// `should_expect_client` is whether a client was expected (the default);
    /// when it is false and the proxy has no client, only the script callback
    /// still runs.
    pub fn on_client_death(&mut self, reason: u8, should_expect_client: bool) -> ClientDeathPlan {
        // Already dead and told the cell about it: nothing to do.
        if !self.has_client() && should_expect_client {
            return ClientDeathPlan::AlreadyDead;
        }

        if should_expect_client || self.has_client() {
            // A timed-out or rate-limited channel is condemned without waiting
            // for its finalising acks.
            let mut should_condemn_client = should_expect_client;
            if reason == CLIENT_DISCONNECT_TIMEOUT
                || reason == CLIENT_DISCONNECT_RATE_LIMITS_EXCEEDED
            {
                should_condemn_client = false;
            }
            let log_off = self.log_off_client(should_condemn_client);
            ClientDeathPlan::LoggedOff { log_off }
        } else {
            ClientDeathPlan::ScriptCallbackOnly
        }
    }

    /// `Proxy::disconnectClient`: the client asked to be disconnected.
    pub fn disconnect_client(&mut self) -> ClientDeathPlan {
        self.on_client_death(CLIENT_DISCONNECT_CLIENT_REQUESTED, true)
    }

    /// `logOffClient`: abort downloads, tell the client it is logged off, then
    /// detach.
    pub fn log_off_client(&mut self, should_condemn_channel: bool) -> LogOffPlan {
        let downloads_aborted = self.has_downloads;
        self.has_downloads = false;

        // "Send a message to the client telling it that it has been logged
        // off. The reason parameter is not yet used."
        let logged_off = if self.client_connected && self.channel.is_some() {
            Some(LoggedOffMessage { reason: 0 })
        } else {
            None
        };

        let detach = self.detach_from_client(should_condemn_channel);

        LogOffPlan {
            logged_off,
            downloads_aborted,
            detach,
        }
    }

    /// `detachFromClient`: clear the channel binding and reset the session
    /// state.
    pub fn detach_from_client(&mut self, should_condemn: bool) -> DetachPlan {
        self.is_giving_client_away = false;

        let removed_from_proxy_list = self.has_client();

        let channel_disposal = if self.channel.is_some() {
            // The channel listener is cleared, the channel unbound
            // (setClientChannel(NULL)) and, when it is connected, shut down or
            // destroyed.
            let connected = self.client_connected;
            self.channel = None;
            if connected {
                Some(if should_condemn {
                    ChannelDisposal::ShutDown
                } else {
                    ChannelDisposal::Destroy
                })
            } else {
                None
            }
        } else {
            None
        };

        // Disable the cell witness only if the cell is still being messaged.
        let witness_disabled = self.cell_has_witness && self.should_send_to_cell;
        self.cell_has_witness = false;

        // Reset the session state.
        self.buffered_bundle = false;
        self.entities_enabled = false;
        self.base_player_created = false;
        self.should_run_callback_on_entities_enabled = false;
        self.proxy_pusher = false;
        self.has_downloads = false;

        DetachPlan {
            removed_from_proxy_list,
            channel_disposal,
            witness_disabled,
        }
    }
}

/// The `onClientDeath` outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientDeathPlan {
    /// The proxy has no client and one was expected: the reference returns
    /// early ("already dead and told the cell about it").
    AlreadyDead,
    /// The client is logged off; the script's `onClientDeath` callback fires.
    LoggedOff { log_off: LogOffPlan },
    /// No client was attached and none was expected: only the script callback
    /// runs.
    ScriptCallbackOnly,
}

/// The `logOffClient` outcome: the message sent and the detach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogOffPlan {
    /// The `loggedOff { reason }` message, sent only when the client is
    /// connected.
    pub logged_off: Option<LoggedOffMessage>,
    /// Whether pending downloads were aborted (their callbacks fire after the
    /// detach).
    pub downloads_aborted: bool,
    /// The channel and state cleanup.
    pub detach: DetachPlan,
}

/// `ClientInterface::loggedOffArgs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoggedOffMessage {
    /// The reason; the reference always sends 0 ("not yet used").
    pub reason: u8,
}

/// The `detachFromClient` outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetachPlan {
    /// Whether the proxy was removed from the BaseApp's proxy list.
    pub removed_from_proxy_list: bool,
    /// How the bound channel was disposed of.
    pub channel_disposal: Option<ChannelDisposal>,
    /// Whether the cell witness was disabled.
    pub witness_disabled: bool,
}

/// How the disconnected client channel was disposed of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelDisposal {
    /// `shutDown()`: the channel still expects finalising acks.
    ShutDown,
    /// `destroy()`: the channel is destroyed outright.
    Destroy,
}

#[cfg(test)]
mod tests {
    use super::*;

    const REASON_STRINGS: [&str; 7] = [
        "Requested by client",
        "Given to other proxy",
        "Rate limit exceeded",
        "Connection timeout",
        "Proxy entity was restored",
        "BaseApp shutdown",
        "Cell entity restoration failure",
    ];

    fn connected_state() -> ProxyDisconnectState {
        ProxyDisconnectState {
            channel: Some(ClientChannel {
                addr: (0x0a00_0a01, 40000),
                created: true,
                encrypted: true,
                rate_limiter: true,
                inactivity_timeout: 60_000_000_000,
                primer: true,
                proxy_id: 7,
            }),
            client_connected: true,
            ..ProxyDisconnectState::default()
        }
    }

    #[test]
    fn every_reason_has_the_reference_string() {
        for (i, expected) in REASON_STRINGS.iter().enumerate() {
            assert_eq!(client_disconnect_reason_to_string(i as u8), *expected);
        }
        assert_eq!(client_disconnect_reason_to_string(99), "Invalid reason");
    }

    #[test]
    fn a_client_requested_disconnect_logs_the_client_off() {
        let mut state = connected_state();
        let plan = state.disconnect_client();
        let ClientDeathPlan::LoggedOff { log_off } = &plan else {
            panic!("expected a logged-off plan");
        };
        // The client is told it is logged off with reason 0.
        assert_eq!(log_off.logged_off, Some(LoggedOffMessage { reason: 0 }));
        // A normal disconnect keeps the finalising acks.
        assert_eq!(
            log_off.detach.channel_disposal,
            Some(ChannelDisposal::ShutDown)
        );
        assert!(log_off.detach.removed_from_proxy_list);
    }

    #[test]
    fn a_timeout_condemns_without_waiting_for_acks() {
        let mut state = connected_state();
        let plan = state.on_client_death(CLIENT_DISCONNECT_TIMEOUT, true);
        let ClientDeathPlan::LoggedOff { log_off } = &plan else {
            panic!("expected a logged-off plan");
        };
        assert_eq!(
            log_off.detach.channel_disposal,
            Some(ChannelDisposal::Destroy)
        );
    }

    #[test]
    fn rate_limits_are_also_destroyed() {
        let mut state = connected_state();
        let plan = state.on_client_death(CLIENT_DISCONNECT_RATE_LIMITS_EXCEEDED, true);
        let ClientDeathPlan::LoggedOff { log_off } = &plan else {
            panic!("expected a logged-off plan");
        };
        assert_eq!(
            log_off.detach.channel_disposal,
            Some(ChannelDisposal::Destroy)
        );
    }

    #[test]
    fn an_already_dead_proxy_is_left_alone() {
        let mut state = ProxyDisconnectState::default();
        assert_eq!(
            state.on_client_death(CLIENT_DISCONNECT_CLIENT_REQUESTED, true),
            ClientDeathPlan::AlreadyDead
        );
    }

    #[test]
    fn a_disconnect_without_a_client_only_runs_the_callback() {
        let mut state = ProxyDisconnectState::default();
        assert_eq!(
            state.on_client_death(CLIENT_DISCONNECT_CLIENT_REQUESTED, false),
            ClientDeathPlan::ScriptCallbackOnly
        );
    }

    #[test]
    fn detach_resets_the_session_state() {
        let mut state = connected_state();
        state.cell_has_witness = true;
        state.should_send_to_cell = true;
        state.entities_enabled = true;
        state.base_player_created = true;
        state.should_run_callback_on_entities_enabled = true;
        state.proxy_pusher = true;
        state.has_downloads = true;

        let plan = state.detach_from_client(true);

        // The witness is disabled while the cell is still messaged.
        assert!(plan.witness_disabled);
        assert!(plan.removed_from_proxy_list);
        assert_eq!(plan.channel_disposal, Some(ChannelDisposal::ShutDown));

        // Everything is reset.
        assert!(!state.has_client());
        assert!(!state.cell_has_witness);
        assert!(!state.entities_enabled);
        assert!(!state.base_player_created);
        assert!(!state.should_run_callback_on_entities_enabled);
        assert!(!state.proxy_pusher);
        assert!(!state.has_downloads);
        assert!(!state.is_giving_client_away);
    }

    #[test]
    fn downloads_are_aborted_on_logoff() {
        let mut state = connected_state();
        state.has_downloads = true;
        let plan = state.log_off_client(true);
        assert!(plan.downloads_aborted);
        assert!(!state.has_downloads);
    }

    #[test]
    fn a_disconnected_channel_sends_no_logged_off_message() {
        let mut state = connected_state();
        state.client_connected = false;
        let plan = state.log_off_client(true);
        assert_eq!(plan.logged_off, None);
        // Not connected: nothing to shut down or destroy.
        assert_eq!(plan.detach.channel_disposal, None);
    }

    #[test]
    fn a_buffered_bundle_counts_as_a_client_but_sends_nothing() {
        let mut state = connected_state();
        state.channel = None;
        state.buffered_bundle = true;
        let plan = state.log_off_client(true);
        assert_eq!(plan.logged_off, None);
        assert!(plan.detach.removed_from_proxy_list);
        assert_eq!(plan.detach.channel_disposal, None);
    }

    #[test]
    fn the_witness_is_kept_when_the_cell_cannot_be_messaged() {
        let mut state = connected_state();
        state.cell_has_witness = true;
        state.should_send_to_cell = false;
        let plan = state.detach_from_client(true);
        assert!(!plan.witness_disabled);
        assert!(!state.cell_has_witness);
    }
}
