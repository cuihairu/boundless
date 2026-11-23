//! BW-06-04-06 the headless client's connection lifecycle.
//!
//! `HeadlessClient` drives and records the client connection lifecycle that the
//! other BW-06-04 modules model, so the headless and the original client can be
//! exercised against the same message sequences:
//!
//! * **login** — `start(loginAppAddr)`, the LoginApp reply, then the BaseApp
//!   reply (`client_login::LoginHandler`); a `LOGGED_ON` status records
//!   `LoggedIn`;
//! * **enter world** — the server's `createCellPlayer` (sent by the cell's
//!   `Witness`, handled by `ServerConnection::createCellPlayer`); the client
//!   now controls the player in a space and records `EnteredWorld`;
//! * **quit** — the client disconnects and records `Quit`;
//! * **re-login** — a fresh `LoginHandler` runs again and records
//!   `ReloggedIn`.
//!
//! The recorded events are the acceptance of the joint test: 登录、进世界、退出
//! 及重登均记录.
//!
//! `createCellPlayer`'s wire layout is the host-order stream of `witness.cpp`:
//! `space_id` (u32), `vehicle_id` (u32), `pos` (3 f32), `packed_xz_scale`
//! (f32), `dir` (3 f32), then the property data.

use crate::baseapp_continuation::SessionKey;
use crate::baseappmgr::Addr;
use crate::client_login::{
    BaseAppSuccessOutcome, LogOnStatus, LoginAppSuccessOutcome, LoginHandler, LoginReplyRecord,
    StartOutcome, log_on_status,
};

/// The connected entity's id.
pub type EntityId = u32;

/// The space the client is in.
pub type SpaceId = u32;

/// One recorded lifecycle event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// 登录: the BaseApp accepted the login.
    LoggedIn {
        /// The BaseApp the client is connected to.
        base_app_addr: Addr,
        /// The adopted session key.
        session_key: SessionKey,
    },
    /// 进世界: `createCellPlayer` made the client control the player in a space.
    EnteredWorld {
        /// The space the client entered.
        space_id: SpaceId,
    },
    /// 退出: the client disconnected.
    Quit,
    /// 重登: a fresh login was accepted.
    ReloggedIn {
        /// The BaseApp the client re-connected to.
        base_app_addr: Addr,
        /// The new session key.
        session_key: SessionKey,
    },
}

/// The decoded `createCellPlayer` body (before the property data).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CreateCellPlayer {
    /// The space the player entered.
    pub space_id: SpaceId,
    /// The innermost vehicle, or `NULL_ENTITY_ID`.
    pub vehicle_id: EntityId,
    /// The position.
    pub pos: [f32; 3],
    /// The packed XZ scale.
    pub packed_xz_scale: f32,
    /// The direction.
    pub dir: [f32; 3],
}

/// Decode the `createCellPlayer` body. The reference streams the fields in
/// host order, so this reads little-endian; the rest of the payload is the
/// property data the entity's creation callback receives.
pub fn parse_create_cell_player(payload: &[u8]) -> Option<(CreateCellPlayer, &[u8])> {
    let mut reader = Reader::new(payload);
    let space_id = reader.read_u32()?;
    let vehicle_id = reader.read_u32()?;
    let mut pos = [0.0; 3];
    for component in &mut pos {
        *component = reader.read_f32()?;
    }
    let packed_xz_scale = reader.read_f32()?;
    let mut dir = [0.0; 3];
    for component in &mut dir {
        *component = reader.read_f32()?;
    }
    Some((
        CreateCellPlayer {
            space_id,
            vehicle_id,
            pos,
            packed_xz_scale,
            dir,
        },
        reader.rest(),
    ))
}

/// The headless client: the login state machine plus the world/exit transitions
/// and the recorded lifecycle.
#[derive(Debug)]
pub struct HeadlessClient {
    login: LoginHandler,
    player_id: EntityId,
    logged_in: bool,
    in_world: bool,
    space_id: SpaceId,
    has_logged_in_before: bool,
    events: Vec<LifecycleEvent>,
}

impl HeadlessClient {
    /// Constructor with the connected (player) entity id.
    pub fn new(player_id: EntityId) -> Self {
        Self {
            login: LoginHandler::new(LogOnStatus::new(log_on_status::NOT_SET)),
            player_id,
            logged_in: false,
            in_world: false,
            space_id: 0,
            has_logged_in_before: false,
            events: Vec::new(),
        }
    }

    /// The connected entity id.
    pub fn player_id(&self) -> EntityId {
        self.player_id
    }

    /// The lifecycle recorded so far.
    pub fn events(&self) -> &[LifecycleEvent] {
        &self.events
    }

    /// Whether the client is logged in.
    pub fn is_logged_in(&self) -> bool {
        self.logged_in
    }

    /// Whether the client has entered the world.
    pub fn is_in_world(&self) -> bool {
        self.in_world
    }

    /// The space the client is in.
    pub fn space_id(&self) -> SpaceId {
        self.space_id
    }

    /// `start`: begin the login.
    pub fn login(&mut self, login_app_addr: Addr) -> StartOutcome {
        self.login.start(login_app_addr)
    }

    /// `onLoginAppLoginSuccess`.
    pub fn on_login_app_login_success(
        &mut self,
        reply: LoginReplyRecord,
        message: &str,
    ) -> LoginAppSuccessOutcome {
        self.login.on_login_app_login_success(reply, message)
    }

    /// `onBaseAppLoginSuccess`: record the login once accepted.
    pub fn on_base_app_login_success(&mut self, session_key: SessionKey) -> BaseAppSuccessOutcome {
        let outcome = self.login.on_base_app_login_success(session_key);
        if let BaseAppSuccessOutcome::LoggedOn { session_key } = &outcome {
            self.logged_in = true;
            let base_app_addr = self.login.base_app_addr();
            let event = if self.has_logged_in_before {
                LifecycleEvent::ReloggedIn {
                    base_app_addr,
                    session_key: *session_key,
                }
            } else {
                LifecycleEvent::LoggedIn {
                    base_app_addr,
                    session_key: *session_key,
                }
            };
            self.has_logged_in_before = true;
            self.events.push(event);
        }
        outcome
    }

    /// `createCellPlayer`: enter the world and control the player.
    pub fn enter_world(&mut self, create: CreateCellPlayer) {
        if !self.logged_in {
            return;
        }
        self.in_world = true;
        self.space_id = create.space_id;
        self.events.push(LifecycleEvent::EnteredWorld {
            space_id: create.space_id,
        });
    }

    /// Disconnect: the client quits. A no-op when it is already offline.
    pub fn quit(&mut self) {
        if !self.logged_in && !self.in_world {
            return;
        }
        self.logged_in = false;
        self.in_world = false;
        self.events.push(LifecycleEvent::Quit);
    }

    /// Re-login: a fresh login state machine.
    pub fn relogin(&mut self, login_app_addr: Addr) -> StartOutcome {
        self.login = LoginHandler::new(LogOnStatus::new(log_on_status::NOT_SET));
        self.logged_in = false;
        self.in_world = false;
        self.login.start(login_app_addr)
    }
}

/// A little-endian read cursor for the `createCellPlayer` body.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn read_u32(&mut self) -> Option<u32> {
        let bytes = self.read(4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read_f32(&mut self) -> Option<f32> {
        let bytes = self.read(4)?;
        Some(f32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIN_APP: Addr = (0x7f00_0001, 20015);
    const BASE_APP: Addr = (0x7f00_0001, 20016);

    fn in_world(client: &mut HeadlessClient) {
        client.enter_world(CreateCellPlayer {
            space_id: 100,
            vehicle_id: 0,
            pos: [1.0, 2.0, 3.0],
            packed_xz_scale: 1.0,
            dir: [0.1, 0.2, 0.3],
        });
    }

    fn login_through_base_app(client: &mut HeadlessClient, key: SessionKey) {
        client.on_login_app_login_success(
            LoginReplyRecord {
                server_addr: BASE_APP,
                session_key: key - 1,
            },
            "",
        );
        client.on_base_app_login_success(key);
    }

    #[test]
    fn the_lifecycle_records_login_enter_quit_and_relogin() {
        let mut client = HeadlessClient::new(7);
        assert_eq!(client.player_id(), 7);
        assert!(client.events().is_empty());

        // Login.
        assert_eq!(client.login(LOGIN_APP), StartOutcome::Started);
        login_through_base_app(&mut client, 2);
        assert!(client.is_logged_in());
        assert!(!client.is_in_world());

        // Enter world.
        in_world(&mut client);
        assert!(client.is_in_world());
        assert_eq!(client.space_id(), 100);

        // Quit.
        client.quit();
        assert!(!client.is_logged_in());
        assert!(!client.is_in_world());

        // Re-login: a fresh login is accepted.
        assert_eq!(client.relogin(LOGIN_APP), StartOutcome::Started);
        login_through_base_app(&mut client, 4);
        assert!(client.is_logged_in());

        assert_eq!(
            client.events(),
            &[
                LifecycleEvent::LoggedIn {
                    base_app_addr: BASE_APP,
                    session_key: 2,
                },
                LifecycleEvent::EnteredWorld { space_id: 100 },
                LifecycleEvent::Quit,
                LifecycleEvent::ReloggedIn {
                    base_app_addr: BASE_APP,
                    session_key: 4,
                },
            ]
        );
    }

    #[test]
    fn quitting_while_offline_records_nothing() {
        let mut client = HeadlessClient::new(7);
        client.quit();
        assert!(client.events().is_empty());
    }

    #[test]
    fn entering_the_world_requires_a_login() {
        let mut client = HeadlessClient::new(7);
        in_world(&mut client);
        assert!(!client.is_in_world());
        assert!(client.events().is_empty());
    }

    #[test]
    fn create_cell_player_decodes_host_order_fields() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&100u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        for value in [1.0f32, 2.0, 3.0] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.extend_from_slice(&1.0f32.to_le_bytes());
        for value in [0.1f32, 0.2, 0.3] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        payload.extend_from_slice(&[9, 8, 7]);

        let (create, rest) = parse_create_cell_player(&payload).unwrap();
        assert_eq!(
            create,
            CreateCellPlayer {
                space_id: 100,
                vehicle_id: 0,
                pos: [1.0, 2.0, 3.0],
                packed_xz_scale: 1.0,
                dir: [0.1, 0.2, 0.3],
            }
        );
        assert_eq!(rest, &[9, 8, 7]);
    }

    #[test]
    fn a_short_payload_is_rejected() {
        assert_eq!(parse_create_cell_player(&[0, 0]), None);
    }
}
