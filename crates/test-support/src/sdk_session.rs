//! BW-21-01-01: the client SDK's login and disconnect.
//!
//! One SDK session logs into a server — the reference server or the new one —
//! and later disconnects from it. Both servers accept the same SDK, but their
//! handshakes differ: the reference server answers a login with a challenge
//! the client must respond to before it connects, while the new server
//! connects on the login answer. The session records which server it spoke
//! to and how many rounds that handshake took, so a contract suite runs the
//! same SDK against each flavor.

/// Which server the SDK logs into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerFlavor {
    /// The reference BigWorld server: a login is answered with a challenge.
    Reference,
    /// The new server: a login connects directly.
    New,
}

impl ServerFlavor {
    /// The flavor's name, as a report writes it.
    pub fn name(&self) -> &'static str {
        match self {
            ServerFlavor::Reference => "reference",
            ServerFlavor::New => "new",
        }
    }
}

/// The SDK session's connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkState {
    /// No server connected.
    Disconnected,
    /// A login is in flight.
    Connecting,
    /// Logged in and talking to the server.
    Connected,
    /// A disconnect is in flight.
    LoggingOut,
}

/// Why an SDK call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkError {
    /// The session is already connected.
    AlreadyConnected,
    /// The session is not connected.
    NotConnected,
}

impl SdkError {
    /// The error's stable code.
    pub fn code(&self) -> &'static str {
        match self {
            SdkError::AlreadyConnected => "already-connected",
            SdkError::NotConnected => "not-connected",
        }
    }

    /// The message a person reads.
    pub fn message(&self) -> &'static str {
        match self {
            SdkError::AlreadyConnected => "the session is already connected",
            SdkError::NotConnected => "the session is not connected",
        }
    }
}

/// One entity's identity.
pub type EntityId = u64;

/// One space event the SDK dispatches for an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityEvent {
    /// The entity was created.
    Created,
    /// The entity entered the client's space.
    Entered,
    /// The entity left the client's space.
    Left,
    /// The entity was destroyed.
    Destroyed,
}

/// Why an entity transition was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityError {
    /// The entity is not created — unknown, or already destroyed.
    NotCreated,
    /// The entity already exists.
    AlreadyExists,
    /// The entity never entered the space.
    NotEntered,
    /// The entity is already in the space.
    AlreadyEntered,
}

impl EntityError {
    /// The error's stable code.
    pub fn code(&self) -> &'static str {
        match self {
            EntityError::NotCreated => "entity-not-created",
            EntityError::AlreadyExists => "entity-already-exists",
            EntityError::NotEntered => "entity-not-entered",
            EntityError::AlreadyEntered => "entity-already-entered",
        }
    }

    /// The message a person reads.
    pub fn message(&self) -> &'static str {
        match self {
            EntityError::NotCreated => "the entity is not created",
            EntityError::AlreadyExists => "the entity already exists",
            EntityError::NotEntered => "the entity never entered the space",
            EntityError::AlreadyEntered => "the entity is already in the space",
        }
    }
}

/// One entity the SDK is tracking, and whether it is in the client's space.
#[derive(Debug, Clone, Copy)]
struct LiveEntity {
    /// Whether the entity is in the client's space.
    entered: bool,
}

/// One SDK session against one server.
pub struct SdkSession {
    flavor: ServerFlavor,
    state: SdkState,
    logins: usize,
    disconnects: usize,
    calls: usize,
    reconnects: usize,
    entities: std::collections::BTreeMap<EntityId, LiveEntity>,
    dispatch: Vec<(EntityId, EntityEvent)>,
}

impl SdkSession {
    /// A session, disconnected, against `flavor`.
    pub fn new(flavor: ServerFlavor) -> Self {
        Self {
            flavor,
            state: SdkState::Disconnected,
            logins: 0,
            disconnects: 0,
            calls: 0,
            reconnects: 0,
            entities: std::collections::BTreeMap::new(),
            dispatch: Vec::new(),
        }
    }

    /// The server the session is against.
    pub fn flavor(&self) -> ServerFlavor {
        self.flavor
    }

    /// The session's state.
    pub fn state(&self) -> SdkState {
        self.state
    }

    /// The logins the session completed.
    pub fn logins(&self) -> usize {
        self.logins
    }

    /// The disconnects the session completed.
    pub fn disconnects(&self) -> usize {
        self.disconnects
    }

    /// The rounds the session's handshake takes: the reference server answers
    /// a login with a challenge the client responds to before connecting
    /// (three rounds), the new server connects on the login answer (two).
    pub fn handshake_rounds(&self) -> usize {
        match self.flavor {
            ServerFlavor::Reference => 3,
            ServerFlavor::New => 2,
        }
    }

    /// Log into the server. Both flavors accept the SDK; a session already
    /// connected does not log in twice.
    pub fn login(&mut self) -> Result<(), SdkError> {
        if self.state == SdkState::Connected {
            return Err(SdkError::AlreadyConnected);
        }
        self.state = SdkState::Connecting;
        self.state = SdkState::Connected;
        self.logins += 1;
        Ok(())
    }

    /// Disconnect from the server. A session that is not connected cannot
    /// disconnect.
    pub fn disconnect(&mut self) -> Result<(), SdkError> {
        if self.state != SdkState::Connected {
            return Err(SdkError::NotConnected);
        }
        self.state = SdkState::LoggingOut;
        self.state = SdkState::Disconnected;
        self.disconnects += 1;
        Ok(())
    }

    /// The method calls the session completed.
    pub fn calls(&self) -> usize {
        self.calls
    }

    /// The reconnects the session completed.
    pub fn reconnects(&self) -> usize {
        self.reconnects
    }

    /// Call one method on the server. Only a connected session can call; a
    /// dropped one cannot until it reconnects.
    pub fn call(&mut self) -> Result<(), SdkError> {
        if self.state != SdkState::Connected {
            return Err(SdkError::NotConnected);
        }
        self.calls += 1;
        Ok(())
    }

    /// The connection dropped — the network, or the server, went away. The
    /// session's identity is kept, but nothing can be called until it
    /// reconnects.
    pub fn drop_connection(&mut self) {
        self.state = SdkState::Disconnected;
    }

    /// Restore the dropped session and its identity. A session still
    /// connected cannot reconnect; a restored one does not count as a new
    /// login.
    pub fn reconnect(&mut self) -> Result<(), SdkError> {
        if self.state == SdkState::Connected {
            return Err(SdkError::AlreadyConnected);
        }
        self.state = SdkState::Connecting;
        self.state = SdkState::Connected;
        self.reconnects += 1;
        Ok(())
    }

    /// Record and dispatch one entity event.
    fn dispatch_event(&mut self, id: EntityId, event: EntityEvent) {
        self.dispatch.push((id, event));
    }

    /// The entity's dispatched events, in the order they were dispatched.
    pub fn lifecycle(&self, id: EntityId) -> Vec<EntityEvent> {
        self.dispatch
            .iter()
            .filter(|(event_id, _)| *event_id == id)
            .map(|(_, event)| *event)
            .collect()
    }

    /// Every dispatched event, every entity, in the order dispatched.
    pub fn timeline(&self) -> &[(EntityId, EntityEvent)] {
        &self.dispatch
    }

    /// Create an entity: the first event of its lifecycle. A live entity is
    /// not created twice; a destroyed one can be created again.
    pub fn spawn(&mut self, id: EntityId) -> Result<(), EntityError> {
        if self.entities.contains_key(&id) {
            return Err(EntityError::AlreadyExists);
        }
        self.entities.insert(id, LiveEntity { entered: false });
        self.dispatch_event(id, EntityEvent::Created);
        Ok(())
    }

    /// The entity enters the client's space: it must exist and not be in
    /// already.
    pub fn enter_space(&mut self, id: EntityId) -> Result<(), EntityError> {
        let entity = self.entities.get_mut(&id).ok_or(EntityError::NotCreated)?;
        if entity.entered {
            return Err(EntityError::AlreadyEntered);
        }
        entity.entered = true;
        self.dispatch_event(id, EntityEvent::Entered);
        Ok(())
    }

    /// The entity leaves the client's space: it must exist and be in.
    pub fn leave_space(&mut self, id: EntityId) -> Result<(), EntityError> {
        let entity = self.entities.get_mut(&id).ok_or(EntityError::NotCreated)?;
        if !entity.entered {
            return Err(EntityError::NotEntered);
        }
        entity.entered = false;
        self.dispatch_event(id, EntityEvent::Left);
        Ok(())
    }

    /// Destroy an entity: the last event of its lifecycle. The identity is
    /// freed, so the same id can be created again later.
    pub fn destroy(&mut self, id: EntityId) -> Result<(), EntityError> {
        if self.entities.remove(&id).is_none() {
            return Err(EntityError::NotCreated);
        }
        self.dispatch_event(id, EntityEvent::Destroyed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_starts_disconnected() {
        let session = SdkSession::new(ServerFlavor::New);

        // Nothing is connected until a login runs.
        assert_eq!(session.state(), SdkState::Disconnected);
        assert_eq!(session.logins(), 0);
        assert_eq!(session.disconnects(), 0);
    }

    #[test]
    fn a_session_logs_into_and_out_of_a_reference_server() {
        let mut session = SdkSession::new(ServerFlavor::Reference);

        // The reference server challenges a login before it connects.
        assert_eq!(session.login(), Ok(()));
        assert_eq!(session.state(), SdkState::Connected);
        assert_eq!(session.logins(), 1);
        assert_eq!(session.handshake_rounds(), 3);

        assert_eq!(session.disconnect(), Ok(()));
        assert_eq!(session.state(), SdkState::Disconnected);
        assert_eq!(session.disconnects(), 1);
    }

    #[test]
    fn a_session_logs_into_and_out_of_a_new_server() {
        let mut session = SdkSession::new(ServerFlavor::New);

        // The new server connects the login directly.
        assert_eq!(session.login(), Ok(()));
        assert_eq!(session.state(), SdkState::Connected);
        assert_eq!(session.handshake_rounds(), 2);

        assert_eq!(session.disconnect(), Ok(()));
        assert_eq!(session.state(), SdkState::Disconnected);
    }

    #[test]
    fn the_handshake_differs_by_server_flavor() {
        let reference = SdkSession::new(ServerFlavor::Reference);
        let new = SdkSession::new(ServerFlavor::New);

        // Each server answers differently; a contract suite checks both.
        assert_ne!(reference.handshake_rounds(), new.handshake_rounds());
        assert_eq!(reference.flavor(), ServerFlavor::Reference);
        assert_eq!(reference.flavor().name(), "reference");
        assert_eq!(new.flavor().name(), "new");
    }

    #[test]
    fn a_connected_session_does_not_log_in_twice() {
        let mut session = SdkSession::new(ServerFlavor::New);
        assert_eq!(session.login(), Ok(()));

        // A second login into a connected session is a distinct error.
        let error = session.login().unwrap_err();
        assert_eq!(error, SdkError::AlreadyConnected);
        assert_eq!(error.code(), "already-connected");
        assert_eq!(session.logins(), 1);
    }

    #[test]
    fn a_disconnected_session_cannot_disconnect() {
        let mut session = SdkSession::new(ServerFlavor::Reference);

        // Disconnecting before a login is the other error.
        let error = session.disconnect().unwrap_err();
        assert_eq!(error, SdkError::NotConnected);
        assert_eq!(error.code(), "not-connected");
        assert_eq!(error.message(), "the session is not connected");
    }

    #[test]
    fn a_full_round_reports_both_counts() {
        let mut session = SdkSession::new(ServerFlavor::New);
        session.login().unwrap();
        session.disconnect().unwrap();

        // One login, one disconnect: a round is complete.
        assert_eq!(session.logins(), 1);
        assert_eq!(session.disconnects(), 1);
        assert_eq!(session.state(), SdkState::Disconnected);
    }

    #[test]
    fn a_dropped_session_reconnects_and_calls_again() {
        let mut session = SdkSession::new(ServerFlavor::New);
        session.login().unwrap();
        assert_eq!(session.call(), Ok(()));
        assert_eq!(session.calls(), 1);

        // The connection drops: a call now fails, a reconnect restores it,
        // and the restored session calls again.
        session.drop_connection();
        assert_eq!(session.state(), SdkState::Disconnected);
        assert_eq!(session.call(), Err(SdkError::NotConnected));

        assert_eq!(session.reconnect(), Ok(()));
        assert_eq!(session.state(), SdkState::Connected);
        assert_eq!(session.call(), Ok(()));
        assert_eq!(session.calls(), 2);
    }

    #[test]
    fn a_reconnect_restores_the_session_not_a_new_login() {
        let mut session = SdkSession::new(ServerFlavor::New);
        session.login().unwrap();
        session.drop_connection();
        session.reconnect().unwrap();

        // The session's identity was restored: the login count is untouched,
        // and the reconnect is counted on its own.
        assert_eq!(session.logins(), 1);
        assert_eq!(session.reconnects(), 1);
        assert_eq!(session.state(), SdkState::Connected);
    }

    #[test]
    fn a_connected_session_cannot_reconnect() {
        let mut session = SdkSession::new(ServerFlavor::New);
        session.login().unwrap();

        // Reconnecting a live session is the other error.
        let error = session.reconnect().unwrap_err();
        assert_eq!(error, SdkError::AlreadyConnected);
        assert_eq!(error.code(), "already-connected");
    }

    #[test]
    fn a_method_call_needs_a_connected_session() {
        let mut session = SdkSession::new(ServerFlavor::New);

        // A session that never logged in cannot call.
        let error = session.call().unwrap_err();
        assert_eq!(error, SdkError::NotConnected);
        assert_eq!(error.code(), "not-connected");
        assert_eq!(session.calls(), 0);
    }

    #[test]
    fn an_entity_lifecycle_dispatches_in_order() {
        let mut session = SdkSession::new(ServerFlavor::New);

        // Created, entered, left, destroyed: the lifecycle in order.
        assert_eq!(session.spawn(1), Ok(()));
        assert_eq!(session.enter_space(1), Ok(()));
        assert_eq!(session.leave_space(1), Ok(()));
        assert_eq!(session.destroy(1), Ok(()));
        assert_eq!(
            session.lifecycle(1),
            vec![
                EntityEvent::Created,
                EntityEvent::Entered,
                EntityEvent::Left,
                EntityEvent::Destroyed,
            ]
        );
    }

    #[test]
    fn a_live_entity_is_not_created_twice() {
        let mut session = SdkSession::new(ServerFlavor::New);
        assert_eq!(session.spawn(1), Ok(()));

        let error = session.spawn(1).unwrap_err();
        assert_eq!(error, EntityError::AlreadyExists);
        assert_eq!(error.code(), "entity-already-exists");
    }

    #[test]
    fn a_destroyed_entity_frees_its_identity() {
        let mut session = SdkSession::new(ServerFlavor::New);
        assert_eq!(session.spawn(1), Ok(()));
        assert_eq!(session.destroy(1), Ok(()));

        // The identity is freed: the same id can be created again.
        assert_eq!(session.spawn(1), Ok(()));
        // And an unknown id is not in the space.
        assert_eq!(session.enter_space(2), Err(EntityError::NotCreated));
    }

    #[test]
    fn entering_needs_a_created_entity() {
        let mut session = SdkSession::new(ServerFlavor::New);

        // Nothing was created, so nothing can enter.
        assert_eq!(session.enter_space(1), Err(EntityError::NotCreated));
        assert_eq!(
            session.enter_space(1).unwrap_err().code(),
            "entity-not-created"
        );
    }

    #[test]
    fn leaving_needs_an_entered_entity() {
        let mut session = SdkSession::new(ServerFlavor::New);
        assert_eq!(session.spawn(1), Ok(()));

        // Created but never entered: leaving is refused.
        assert_eq!(session.leave_space(1), Err(EntityError::NotEntered));
        assert_eq!(
            session.leave_space(1).unwrap_err().code(),
            "entity-not-entered"
        );

        // And entering twice is refused too.
        assert_eq!(session.enter_space(1), Ok(()));
        assert_eq!(session.enter_space(1), Err(EntityError::AlreadyEntered));
    }

    #[test]
    fn destroying_needs_a_created_entity() {
        let mut session = SdkSession::new(ServerFlavor::New);
        assert_eq!(session.destroy(1), Err(EntityError::NotCreated));
    }

    #[test]
    fn the_timeline_keeps_every_entitys_events_in_order() {
        let mut session = SdkSession::new(ServerFlavor::New);

        // Two entities, interleaved: the timeline is the whole dispatch,
        // each event in the order it was dispatched.
        session.spawn(1).unwrap();
        session.spawn(2).unwrap();
        session.enter_space(1).unwrap();
        session.enter_space(2).unwrap();
        session.destroy(1).unwrap();

        assert_eq!(
            session.timeline(),
            &[
                (1, EntityEvent::Created),
                (2, EntityEvent::Created),
                (1, EntityEvent::Entered),
                (2, EntityEvent::Entered),
                (1, EntityEvent::Destroyed),
            ]
        );
        assert_eq!(
            session.lifecycle(2),
            vec![EntityEvent::Created, EntityEvent::Entered]
        );
    }
}
