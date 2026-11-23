//! BW-06-03-03 binding a client channel, a Proxy and the player.
//!
//! `Proxy::attachToClient` (`server/baseapp/proxy.cpp`) is the first-time
//! binding of a proxy to the client that logged in (local handoffs go through
//! `giveClientTo`). It runs once `LoginHandler::login` has matched the pending
//! login, and its order is the login-completion sequence:
//!
//! * the channel is created when none was supplied (a UDP external channel;
//!   TCP channels arrive already made), the session block cipher is applied,
//!   and `setClientChannel` binds the two ways (`userData` names the proxy);
//! * a `RateLimitMessageFilter` is created for the channel and installed as
//!   its message filter, and inactivity detection starts;
//! * `BaseApp::addProxy` registers the proxy as a player, and a `ProxyPusher`
//!   is created when the base has no cell entity yet;
//! * the login reply — `startReply(loginReplyID)` followed by the proxy's
//!   *current* session key, which differs from the login key the client
//!   presented — must be the first message on the channel and goes out on a
//!   bundle of its own;
//! * the client bundle primer is installed so `ClientInterface` messages can
//!   be primed, and `updateFrequencyNotification { hertz }` plus
//!   `setGameTime { gameTime }` are sent;
//! * a client bundle buffered while the base player was being created is
//!   flushed last, with the primer temporarily removed.

use crate::baseapp_continuation::SessionKey;
use crate::baseappmgr::Addr;
use crate::shutdown::GameTime;

/// `Mercury::Address::NONE`: no client address.
pub const ADDR_NONE: Addr = (0, 0);

/// The bound client channel: the `pClientChannel_` side of a proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientChannel {
    /// The client's address.
    pub addr: Addr,
    /// Whether the channel was created here (a UDP external channel) rather
    /// than supplied by the transport.
    pub created: bool,
    /// Whether the session block cipher was applied.
    pub encrypted: bool,
    /// Whether the `RateLimitMessageFilter` is installed.
    pub rate_limiter: bool,
    /// The inactivity timeout armed by `startInactivityDetection`, in stamps.
    pub inactivity_timeout: u64,
    /// Whether the client bundle primer is installed.
    pub primer: bool,
    /// `userData`: the proxy bound to this channel.
    pub proxy_id: u32,
}

/// One ordered message the proxy sends the client while attaching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientSend {
    /// The login reply: `startReply(loginReplyID)` then the session key. When
    /// a reply id is present this is the first message on the channel, on a
    /// bundle by itself.
    LoginReply {
        /// The login request's reply id.
        reply_id: u32,
        /// The proxy's current session key, not the login key.
        session_key: SessionKey,
    },
    /// `updateFrequencyNotification { hertz }` followed by
    /// `setGameTime { gameTime }`, both on one bundle.
    FrequencyAndGameTime {
        /// `BaseAppConfig::updateHertz`.
        hertz: u8,
        /// `BaseApp::time()`.
        game_time: GameTime,
    },
    /// The client bundle buffered while the base player was created, flushed
    /// with the primer temporarily removed.
    BufferedClientBundle,
}

/// `Proxy::attachToClient`'s outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachResult {
    /// The proxy is bound to the client: the ordered handshake and the
    /// channel state.
    Bound {
        /// The messages to send, in order.
        sends: SendList,
        /// The bound channel.
        channel: ClientChannel,
        /// `BaseApp::addProxy`: the proxy is registered as a player.
        registered: bool,
        /// A `ProxyPusher` was created (the base has no cell entity yet).
        proxy_pusher: bool,
    },
    /// The client address is `NONE`: no channel is created ("No client yet.").
    NoClient,
    /// The proxy already has a client channel (`attachToClient` is
    /// first-time only); the reference's development assert fires and `false`
    /// is returned.
    AlreadyAttached,
}

/// The ordered handshake of one attach.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendList(pub Vec<ClientSend>);

impl SendList {
    /// Constructor.
    pub fn new(sends: Vec<ClientSend>) -> Self {
        Self(sends)
    }

    /// The sends, in order.
    pub fn as_slice(&self) -> &[ClientSend] {
        &self.0
    }
}

/// The arguments of one attach.
pub struct AttachArgs {
    /// The proxy being attached.
    pub proxy_id: u32,
    /// The client's address; `ADDR_NONE` means no client yet.
    pub client_addr: Addr,
    /// The login request's reply id; `None` is `REPLY_ID_NONE` (no reply).
    pub login_reply_id: Option<u32>,
    /// Whether the transport supplied a channel (TCP); UDP channels are
    /// created here.
    pub supplied_channel: bool,
    /// Whether a session block cipher is applied to the channel.
    pub encrypted: bool,
    /// `inactivityTimeout`.
    pub inactivity_timeout: u64,
    /// The proxy's current session key, sent with the login reply.
    pub session_key: SessionKey,
    /// `BaseAppConfig::updateHertz`.
    pub update_hertz: u8,
    /// `BaseApp::time()`.
    pub game_time: GameTime,
    /// `basePlayerCreatedOnClient_`.
    pub base_player_created: bool,
    /// Whether a client bundle was buffered while the base player was created.
    pub buffered_bundle: bool,
    /// `hasCellEntity`.
    pub has_cell_entity: bool,
}

/// The Proxy's client side: the bound channel and the push object.
#[derive(Clone, Debug, Default)]
pub struct ProxyClient {
    channel: Option<ClientChannel>,
    proxy_pusher: bool,
}

impl ProxyClient {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `hasClient`.
    pub fn has_client(&self) -> bool {
        self.channel.is_some()
    }

    /// `pClientChannel_`.
    pub fn channel(&self) -> Option<&ClientChannel> {
        self.channel.as_ref()
    }

    /// `pProxyPusher_ != NULL`.
    pub fn proxy_pusher(&self) -> bool {
        self.proxy_pusher
    }

    /// `Proxy::attachToClient`: bind this proxy to the client at the given
    /// address and produce the login-completion sequence.
    pub fn attach_to_client(&mut self, args: &AttachArgs) -> AttachResult {
        if self.channel.is_some() {
            return AttachResult::AlreadyAttached;
        }

        if args.client_addr == ADDR_NONE {
            // "Channel not created for %u. No client yet."
            return AttachResult::NoClient;
        }

        let mut channel = ClientChannel {
            addr: args.client_addr,
            created: !args.supplied_channel,
            encrypted: args.encrypted,
            rate_limiter: false,
            inactivity_timeout: 0,
            primer: false,
            // setClientChannel: userData( this )
            proxy_id: args.proxy_id,
        };

        // The rate limiter is created for the channel's address and installed
        // as its message filter.
        channel.rate_limiter = true;
        channel.inactivity_timeout = args.inactivity_timeout;

        // A ProxyPusher is created when the base has no cell entity yet.
        self.proxy_pusher = !args.has_cell_entity;

        // The login reply goes first, on a bundle of its own.
        let mut sends = Vec::new();
        if let Some(reply_id) = args.login_reply_id {
            sends.push(ClientSend::LoginReply {
                reply_id,
                session_key: args.session_key,
            });
        }

        // The primer is installed, then the frequency and game time follow.
        channel.primer = true;
        sends.push(ClientSend::FrequencyAndGameTime {
            hertz: args.update_hertz,
            game_time: args.game_time,
        });

        // A bundle buffered while the base player was created is flushed last.
        if args.base_player_created && args.buffered_bundle {
            sends.push(ClientSend::BufferedClientBundle);
        }

        self.channel = Some(channel);

        AttachResult::Bound {
            sends: SendList::new(sends),
            channel,
            // BaseApp::addProxy( this )
            registered: true,
            proxy_pusher: self.proxy_pusher,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY_ID: u32 = 7;
    const CLIENT: Addr = (0x0a00_0a01, 40000);
    const SESSION_KEY: SessionKey = 0x1234_5678;
    const GAME_TIME: GameTime = 1234;
    const HERTZ: u8 = 20;
    const INACTIVITY: u64 = 60_000_000_000;

    fn args() -> AttachArgs {
        AttachArgs {
            proxy_id: PROXY_ID,
            client_addr: CLIENT,
            login_reply_id: Some(9),
            supplied_channel: false,
            encrypted: true,
            inactivity_timeout: INACTIVITY,
            session_key: SESSION_KEY,
            update_hertz: HERTZ,
            game_time: GAME_TIME,
            base_player_created: false,
            buffered_bundle: false,
            has_cell_entity: false,
        }
    }

    #[test]
    fn a_client_is_bound_with_the_reference_sequence() {
        let mut client = ProxyClient::new();
        assert!(!client.has_client());
        let result = client.attach_to_client(&args());
        let AttachResult::Bound {
            sends,
            channel,
            registered,
            proxy_pusher,
        } = result
        else {
            panic!("expected a bound client");
        };

        // The login-completion sequence: reply first, then frequency and game
        // time.
        assert_eq!(
            sends.as_slice(),
            &[
                ClientSend::LoginReply {
                    reply_id: 9,
                    session_key: SESSION_KEY,
                },
                ClientSend::FrequencyAndGameTime {
                    hertz: HERTZ,
                    game_time: GAME_TIME,
                },
            ]
        );

        // The channel is created (no channel was supplied), encrypted, rate
        // limited and attached to the proxy.
        assert_eq!(
            channel,
            ClientChannel {
                addr: CLIENT,
                created: true,
                encrypted: true,
                rate_limiter: true,
                inactivity_timeout: INACTIVITY,
                primer: true,
                proxy_id: PROXY_ID,
            }
        );
        assert!(registered);
        // No cell entity: a ProxyPusher is created.
        assert!(proxy_pusher);

        assert!(client.has_client());
        assert_eq!(client.channel(), Some(&channel));
        assert!(client.proxy_pusher());
    }

    #[test]
    fn the_login_reply_carries_the_current_key_not_the_login_key() {
        let mut client = ProxyClient::new();
        let a = args();
        let result = client.attach_to_client(&a);
        let AttachResult::Bound { sends, .. } = result else {
            panic!("expected a bound client");
        };
        // The reply's key is the proxy's current session key (the login key
        // was consumed by the pending login lookup).
        assert_eq!(
            sends.as_slice().first(),
            Some(&ClientSend::LoginReply {
                reply_id: 9,
                session_key: SESSION_KEY,
            })
        );
    }

    #[test]
    fn a_supplied_channel_is_not_created() {
        let mut client = ProxyClient::new();
        let mut a = args();
        a.supplied_channel = true;
        let result = client.attach_to_client(&a);
        let AttachResult::Bound { channel, .. } = result else {
            panic!("expected a bound client");
        };
        assert!(!channel.created);
    }

    #[test]
    fn a_second_attach_is_refused() {
        let mut client = ProxyClient::new();
        assert!(matches!(
            client.attach_to_client(&args()),
            AttachResult::Bound { .. }
        ));
        assert_eq!(
            client.attach_to_client(&args()),
            AttachResult::AlreadyAttached
        );
    }

    #[test]
    fn no_client_address_means_no_channel() {
        let mut client = ProxyClient::new();
        let mut a = args();
        a.client_addr = ADDR_NONE;
        assert_eq!(client.attach_to_client(&a), AttachResult::NoClient);
        assert!(!client.has_client());
    }

    #[test]
    fn without_a_reply_id_the_handshake_starts_at_the_frequency() {
        let mut client = ProxyClient::new();
        let mut a = args();
        a.login_reply_id = None;
        let result = client.attach_to_client(&a);
        let AttachResult::Bound { sends, .. } = result else {
            panic!("expected a bound client");
        };
        assert_eq!(
            sends.as_slice(),
            &[ClientSend::FrequencyAndGameTime {
                hertz: HERTZ,
                game_time: GAME_TIME,
            }]
        );
    }

    #[test]
    fn a_buffered_bundle_is_flushed_last() {
        let mut client = ProxyClient::new();
        let mut a = args();
        a.base_player_created = true;
        a.buffered_bundle = true;
        let result = client.attach_to_client(&a);
        let AttachResult::Bound { sends, .. } = result else {
            panic!("expected a bound client");
        };
        assert_eq!(
            sends.as_slice().last(),
            Some(&ClientSend::BufferedClientBundle)
        );
    }

    #[test]
    fn an_unbuffered_bundle_is_not_flushed() {
        let mut client = ProxyClient::new();
        let mut a = args();
        a.base_player_created = true;
        a.buffered_bundle = false;
        let result = client.attach_to_client(&a);
        let AttachResult::Bound { sends, .. } = result else {
            panic!("expected a bound client");
        };
        assert!(!sends.as_slice().contains(&ClientSend::BufferedClientBundle));
    }

    #[test]
    fn a_cell_entity_needs_no_proxy_pusher() {
        let mut client = ProxyClient::new();
        let mut a = args();
        a.has_cell_entity = true;
        let result = client.attach_to_client(&a);
        let AttachResult::Bound { proxy_pusher, .. } = result else {
            panic!("expected a bound client");
        };
        assert!(!proxy_pusher);
        assert!(!client.proxy_pusher());
    }
}
