//! The `ClientApp` entry face (`server/tools/bots/client_app.{hpp,cpp}`):
//! one simulated client inside the Bots process's active set.
//!
//! BW-12-04-02 carries the entry itself -- the identity fields the batch
//! add/remove commands write, the constructor state, and the destroy
//! guard. The connection face (`logOnTo`, the entity maps, the retry
//! timer) lands with BW-12-04-03.

use crate::config::ConnectionTransport;

/// `NULL_ENTITY_ID` (`lib/entity/entity_id.hpp`): the id a bot carries
/// until its log-on hands one out.
pub const NULL_ENTITY_ID: i32 = 0;

/// One simulated client (`ClientApp`, `client_app.cpp:9-35`).
#[derive(Clone, Debug, PartialEq)]
pub struct ClientApp {
    /// `userName_`.
    pub user_name: String,
    /// `userPasswd_`.
    pub user_passwd: String,
    /// `transport_`.
    pub transport: ConnectionTransport,
    /// `tag_`.
    pub tag: String,
    /// `id_` -- NULL until the log-on completes.
    pub id: i32,
    /// `isDestroyed_`.
    pub is_destroyed: bool,
    /// `isDormant_` -- starts true, the constructor's `logOn` clears it.
    pub is_dormant: bool,
    /// `speed_` -- `6.f + float(rand())*2.f/float(RAND_MAX)`
    /// (`client_app.cpp:30`), so a draw from [6, 8).
    pub speed: f32,
}

impl ClientApp {
    /// The reference constructor's state, including its trailing
    /// `logOn()` call (`client_app.cpp:37`).
    pub fn new(
        user_name: String,
        user_passwd: String,
        transport: ConnectionTransport,
        tag: String,
        speed: f32,
    ) -> Self {
        let mut app = Self {
            user_name,
            user_passwd,
            transport,
            tag,
            id: NULL_ENTITY_ID,
            is_destroyed: false,
            is_dormant: true,
            speed,
        };
        app.log_on();
        app
    }

    /// `ClientApp::logOn`'s state half (`client_app.cpp`): a destroyed
    /// bot never un-dormants; the connection query
    /// (`isLoggingIn`/`isOnline`) and the `logOnTo` call land with
    /// BW-12-04-03.
    pub fn log_on(&mut self) {
        if self.is_destroyed {
            return;
        }
        self.is_dormant = false;
    }

    /// `ClientApp::destroy` (`client_app.cpp`): idempotent. The
    /// personality hook, `logOff` and `clearAllEntities` land with
    /// BW-12-04-03.
    pub fn destroy(&mut self) {
        if self.is_destroyed {
            return;
        }
        self.is_destroyed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acceptance: the constructor leaves a live bot dormant-free
    /// (the trailing `logOn`), NULL-id'd, and carrying the identity
    /// it was built with (`client_app.cpp:9-37`).
    #[test]
    fn the_constructor_logons_and_carries_the_identity() {
        let app = ClientApp::new(
            "Bot_1_App".to_string(),
            "pw".to_string(),
            ConnectionTransport::Tcp,
            "Swarm".to_string(),
            6.5,
        );
        assert_eq!(app.user_name, "Bot_1_App");
        assert_eq!(app.user_passwd, "pw");
        assert_eq!(app.transport, ConnectionTransport::Tcp);
        assert_eq!(app.tag, "Swarm");
        assert_eq!(app.id, NULL_ENTITY_ID);
        assert!(!app.is_destroyed);
        assert!(!app.is_dormant, "the constructor calls logOn");
    }

    /// Acceptance: `destroy` is guarded -- the second call is a no-op
    /// (`client_app.cpp`, the `isDestroyed_` early return), and a
    /// destroyed bot's `logOn` keeps it destroyed (`logOn`'s first
    /// check).
    #[test]
    fn destroy_is_idempotent_and_logon_respects_it() {
        let mut app = ClientApp::new(
            "u".to_string(),
            "p".to_string(),
            ConnectionTransport::Udp,
            "Default".to_string(),
            7.0,
        );
        app.destroy();
        assert!(app.is_destroyed);
        // A dormant bot that was never logon'd stays so after destroy.
        let mut dormant = ClientApp::new(
            "u".to_string(),
            "p".to_string(),
            ConnectionTransport::Udp,
            "Default".to_string(),
            7.0,
        );
        dormant.is_dormant = true;
        dormant.destroy();
        dormant.log_on();
        assert!(dormant.is_destroyed);
        assert!(dormant.is_dormant, "logOn returns early when destroyed");
    }
}
