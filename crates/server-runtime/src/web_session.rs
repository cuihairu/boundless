//! BW-14-03-02: web login and session.
//!
//! Ported from FantasyDemo's web front end
//! (`programming/fantasydemo/web/php/`):
//!
//! - `Authenticator.php`: the session token lives in `$_SESSION['token']`
//!   as a map of `ip_address` / `last_activity` / `username`. The
//!   `authenticateSessionToken` checks run in order: no token at all, an
//!   idle session past [`AUTHENTICATOR_INACTIVITY_PERIOD`], then a
//!   spoofed cookie detected by an IP address change.
//! - `BWAuthenticator.php`: `authenticateUserPass` rejects empty fields
//!   with `InvalidFieldError`, calls `RemoteEntity("db")->logOn` and
//!   stamps the token with `entity_type` and
//!   `entity_path = entities_by_id/<type>/<id>`.
//! - `AuthenticatedPage.php`: a page request authenticates the session
//!   token, then `updateLastActivity` and renders - the same session
//!   state keeps working across pages - and boots back to the login page
//!   on any failure. The `errorMsgs` / `statusMsgs` variables are
//!   read-once flash messages.
//! - `Login.php`: `logout=1` invalidates the token.
//!
//! PHP keeps the state in `$_SESSION` and the Authenticator is stateless;
//! the port does the same - [`WebSession`] is the state, the
//! [`WebAuthenticator`] methods take it. The `RemoteEntity("db")` call is
//! the front end's only server dependency, so it arrives as a
//! [`DbLogOn`] implementation instead of a transport.

use std::collections::BTreeMap;
use std::fmt;

use db::DbError;

/// `Constants.php` `AUTHENTICATOR_INACTIVITY_PERIOD` (seconds).
pub const AUTHENTICATOR_INACTIVITY_PERIOD: u64 = 3600;
/// `Constants.php` `LOGIN_PAGE_URL`.
pub const LOGIN_PAGE_URL: &str = "Login.php";
/// `AUTHENTICATOR_SESSION_KEY_TOKEN`.
pub const SESSION_KEY_TOKEN: &str = "token";

/// `BWAuthenticator`'s extra token keys.
pub const TOKEN_KEY_ENTITY_TYPE: &str = "entity_type";
pub const TOKEN_KEY_ENTITY_PATH: &str = "entity_path";

/// The login failure modes, carrying the reference's messages. The
/// reference also has an "Invalid authentication token" error for a token
/// map missing its `last_activity` / `ip_address` keys; the port's typed
/// [`SessionToken`] cannot represent such a token, so that state is
/// unrepresentable here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// `InvalidFieldError` (`BWAuthenticator.php:44-51`).
    InvalidField(&'static str),
    /// `BWAuthenticateError` from `authenticateSessionToken`
    /// (`Authenticator.php:27-67`).
    NoToken,
    Inactive(u64),
    IpChanged,
    /// The `RemoteEntity("db")->logOn` leg failed.
    DbLogOn(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::InvalidField(message) => write!(f, "InvalidFieldError: {message}"),
            AuthError::NoToken => write!(f, "No authentication token exists"),
            AuthError::Inactive(period) => {
                write!(f, "Session inactive for more than {period} seconds")
            }
            AuthError::IpChanged => write!(f, "IP address changed"),
            AuthError::DbLogOn(message) => write!(f, "logOn failed: {message}"),
        }
    }
}

/// The stored token (`$_SESSION['token']`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken {
    /// `AUTHENTICATOR_TOKEN_KEY_IP_ADDRESS`.
    pub ip_address: String,
    /// `AUTHENTICATOR_TOKEN_KEY_LAST_ACTIVITY` (unix seconds).
    pub last_activity: u64,
    /// `AUTHENTICATOR_TOKEN_KEY_USERNAME`.
    pub username: String,
    /// `BW_AUTHENTICATOR_TOKEN_KEY_ENTITY_TYPE`.
    pub entity_type: Option<String>,
    /// `BW_AUTHENTICATOR_TOKEN_KEY_ENTITY_PATH`:
    /// `entities_by_id/<type>/<id>`.
    pub entity_path: Option<String>,
}

/// The front end's only server dependency: `RemoteEntity("db")->logOn`,
/// answered with the entity type and id (`BWAuthenticator.php:53-57`).
pub trait DbLogOn {
    fn log_on(&self, username: &str, password: &str) -> Result<(String, u32), DbError>;
}

/// `$_SESSION` as this front end uses it: the token plus the flash
/// message variables (`errorMsgs` / `statusMsgs`), which are read once
/// and cleared (`AuthenticatedPage.php:137-149`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WebSession {
    token: Option<SessionToken>,
    flash: BTreeMap<String, Vec<String>>,
}

impl WebSession {
    /// An empty session.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Authenticator::doesAuthTokenExist`.
    pub fn does_auth_token_exist(&self) -> bool {
        self.token.is_some()
    }

    /// The token, for tests and session restore.
    pub fn token(&self) -> Option<&SessionToken> {
        self.token.as_ref()
    }

    /// `Authenticator::setSessionToken`.
    pub fn set_session_token(&mut self, token: SessionToken) {
        self.token = Some(token);
    }

    /// `Authenticator::invalidateToken` (`Login.php:24` on logout).
    pub fn invalidate_token(&mut self) {
        self.token = None;
    }

    /// `Authenticator::setVariable` on a flash variable.
    pub fn set_flash(&mut self, key: &str, messages: Vec<String>) {
        self.flash.insert(key.to_string(), messages);
    }

    /// `AuthenticatedPage`'s read-and-clear of a flash variable: the
    /// messages move into the page and the variable resets to null.
    pub fn take_flash(&mut self, key: &str) -> Option<Vec<String>> {
        self.flash.remove(key)
    }
}

/// The `Authenticator` / `BWAuthenticator` method surface. Stateless -
/// everything operates on the [`WebSession`].
pub struct WebAuthenticator;

impl WebAuthenticator {
    /// `BWAuthenticator::authenticateUserPass`
    /// (`BWAuthenticator.php:40-67`): reject empty fields, log on against
    /// the db, then stamp the token with the caller's address, the time
    /// and the entity details.
    pub fn authenticate_user_pass(
        session: &mut WebSession,
        db: &impl DbLogOn,
        username: &str,
        pass: &str,
        remote_addr: &str,
        now: u64,
    ) -> Result<(), AuthError> {
        if username.is_empty() {
            return Err(AuthError::InvalidField("Username is empty"));
        }
        if pass.is_empty() {
            return Err(AuthError::InvalidField("Password is empty"));
        }

        let (entity_type, entity_id) = db
            .log_on(username, pass)
            .map_err(|err| AuthError::DbLogOn(err.to_string()))?;

        let entity_path = format!("entities_by_id/{entity_type}/{entity_id}");
        session.set_session_token(SessionToken {
            ip_address: remote_addr.to_string(),
            last_activity: now,
            username: username.to_string(),
            entity_type: Some(entity_type),
            entity_path: Some(entity_path),
        });
        Ok(())
    }

    /// `Authenticator::authenticateSessionToken`
    /// (`Authenticator.php:27-67`), checks in the reference's order.
    pub fn authenticate_session_token(
        session: &WebSession,
        remote_addr: &str,
        now: u64,
    ) -> Result<(), AuthError> {
        // check that the session token exists
        let token = session.token.as_ref().ok_or(AuthError::NoToken)?;

        // check that they haven't been inactive too long
        if now
            >= token
                .last_activity
                .saturating_add(AUTHENTICATOR_INACTIVITY_PERIOD)
        {
            return Err(AuthError::Inactive(AUTHENTICATOR_INACTIVITY_PERIOD));
        }

        // check that the cookie isn't spoofed by checking against IP
        // address
        if token.ip_address != remote_addr {
            return Err(AuthError::IpChanged);
        }

        Ok(())
    }

    /// `Authenticator::updateLastActivity`
    /// (`AuthenticatedPage.php:135`): every served page slides the
    /// inactivity window.
    pub fn update_last_activity(session: &mut WebSession, now: u64) {
        if let Some(token) = session.token.as_mut() {
            token.last_activity = now;
        }
    }

    /// `BWAuthenticator::entityType`.
    pub fn entity_type(session: &WebSession) -> Option<&str> {
        session
            .token
            .as_ref()
            .and_then(|token| token.entity_type.as_deref())
    }

    /// `BWAuthenticator::entity`: the entity path, `None` where PHP
    /// returns FALSE (`BWAuthenticator.php:27-38`).
    pub fn entity_path(session: &WebSession) -> Option<&str> {
        session
            .token
            .as_ref()
            .and_then(|token| token.entity_path.as_deref())
    }

    /// `AuthenticatedPage::backToLogin`: invalidate and build the login
    /// redirect (`AuthenticatedPage.php:45-71`).
    pub fn back_to_login(session: &mut WebSession, args: &[(&str, &str)]) -> String {
        session.invalidate_token();
        if args.is_empty() {
            LOGIN_PAGE_URL.to_string()
        } else {
            let encoded: Vec<String> = args
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            format!("{LOGIN_PAGE_URL}?{}", encoded.join("&"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A `RemoteEntity("db")` stub: accepts one credential pair.
    struct StubDb {
        reply: Result<(String, u32), DbError>,
        calls: RefCell<usize>,
    }

    impl StubDb {
        fn ok() -> Self {
            StubDb {
                reply: Ok(("Avatar".to_string(), 7)),
                calls: RefCell::new(0),
            }
        }

        fn failing() -> Self {
            StubDb {
                reply: Err(DbError::NotFound),
                calls: RefCell::new(0),
            }
        }
    }

    impl DbLogOn for StubDb {
        fn log_on(&self, _username: &str, _password: &str) -> Result<(String, u32), DbError> {
            *self.calls.borrow_mut() += 1;
            self.reply.clone()
        }
    }

    const ADDR: &str = "10.0.0.7";
    const T0: u64 = 1_000_000;

    fn logged_in_session() -> (WebSession, StubDb) {
        let mut session = WebSession::new();
        let db = StubDb::ok();
        WebAuthenticator::authenticate_user_pass(&mut session, &db, "player", "secret", ADDR, T0)
            .unwrap();
        (session, db)
    }

    #[test]
    fn empty_credentials_fail_before_the_db_is_called() {
        let mut session = WebSession::new();
        let db = StubDb::ok();

        let err =
            WebAuthenticator::authenticate_user_pass(&mut session, &db, "", "secret", ADDR, T0)
                .unwrap_err();
        assert_eq!(err, AuthError::InvalidField("Username is empty"));
        assert_eq!(err.to_string(), "InvalidFieldError: Username is empty");
        let err =
            WebAuthenticator::authenticate_user_pass(&mut session, &db, "player", "", ADDR, T0)
                .unwrap_err();
        assert_eq!(err, AuthError::InvalidField("Password is empty"));
        assert_eq!(*db.calls.borrow(), 0, "no db call on a rejected field");
        assert!(!session.does_auth_token_exist());
    }

    #[test]
    fn log_on_stamps_the_token_with_the_entity_details() {
        let (session, db) = logged_in_session();
        assert_eq!(*db.calls.borrow(), 1);
        let token = session.token().unwrap();
        assert_eq!(token.username, "player");
        assert_eq!(token.ip_address, ADDR);
        assert_eq!(token.last_activity, T0);
        assert_eq!(token.entity_type.as_deref(), Some("Avatar"));
        assert_eq!(
            token.entity_path.as_deref(),
            Some("entities_by_id/Avatar/7")
        );
    }

    #[test]
    fn a_failed_log_on_stores_no_token() {
        let mut session = WebSession::new();
        let db = StubDb::failing();
        let err = WebAuthenticator::authenticate_user_pass(
            &mut session,
            &db,
            "player",
            "secret",
            ADDR,
            T0,
        )
        .unwrap_err();
        assert!(matches!(err, AuthError::DbLogOn(_)));
        assert!(err.to_string().starts_with("logOn failed: "));
        assert!(!session.does_auth_token_exist());
    }

    #[test]
    fn update_last_activity_without_a_token_is_a_no_op() {
        // AuthenticatedPage only runs after a login, but the call must not
        // panic or conjure a token when the session is empty.
        let mut session = WebSession::new();
        WebAuthenticator::update_last_activity(&mut session, T0 + 10);
        assert!(!session.does_auth_token_exist());
    }

    #[test]
    fn the_same_session_state_continues_to_access() {
        // The acceptance: the token that log_on stamped keeps
        // authenticating on later page requests.
        let (session, _) = logged_in_session();

        // A later page: authenticate, read the entity, slide the window.
        WebAuthenticator::authenticate_session_token(&session, ADDR, T0 + 60).unwrap();
        assert_eq!(session.token().unwrap().username, "player");
        assert_eq!(
            WebAuthenticator::entity_path(&session),
            Some("entities_by_id/Avatar/7")
        );
        assert_eq!(WebAuthenticator::entity_type(&session), Some("Avatar"));
    }

    #[test]
    fn an_empty_session_has_no_token() {
        let session = WebSession::new();
        assert!(!session.does_auth_token_exist());
        let err = WebAuthenticator::authenticate_session_token(&session, ADDR, T0).unwrap_err();
        assert_eq!(err, AuthError::NoToken);
        assert_eq!(err.to_string(), "No authentication token exists");
    }

    #[test]
    fn inactivity_past_the_period_boots_to_login() {
        let (session, _) = logged_in_session();

        // Inside the window: fine (3599 seconds of idleness).
        WebAuthenticator::authenticate_session_token(&session, ADDR, T0 + 3599).unwrap();
        // At the boundary the reference fails: now >= last + period.
        let err =
            WebAuthenticator::authenticate_session_token(&session, ADDR, T0 + 3600).unwrap_err();
        assert_eq!(err, AuthError::Inactive(AUTHENTICATOR_INACTIVITY_PERIOD));
        assert_eq!(
            err.to_string(),
            "Session inactive for more than 3600 seconds"
        );
    }

    #[test]
    fn a_served_page_slides_the_inactivity_window() {
        let (mut session, _) = logged_in_session();

        // Pages at +3000 and +5900 keep it alive; a bare session would
        // have timed out at +5900.
        WebAuthenticator::update_last_activity(&mut session, T0 + 3000);
        WebAuthenticator::authenticate_session_token(&session, ADDR, T0 + 5900).unwrap();
        // Past the new window it expires.
        assert!(matches!(
            WebAuthenticator::authenticate_session_token(&session, ADDR, T0 + 6600),
            Err(AuthError::Inactive(_))
        ));
    }

    #[test]
    fn an_ip_address_change_is_rejected() {
        let (session, _) = logged_in_session();
        let err =
            WebAuthenticator::authenticate_session_token(&session, "10.0.0.9", T0 + 1).unwrap_err();
        assert_eq!(err, AuthError::IpChanged);
        assert_eq!(err.to_string(), "IP address changed");
    }

    #[test]
    fn logout_invalidates_and_the_next_page_bounces_to_login() {
        let (mut session, _) = logged_in_session();
        assert!(session.does_auth_token_exist());

        // Login.php?logout=1: invalidate.
        session.invalidate_token();
        assert!(!session.does_auth_token_exist());
        assert_eq!(
            WebAuthenticator::authenticate_session_token(&session, ADDR, T0 + 1),
            Err(AuthError::NoToken)
        );

        // The page boots back with a redirect.
        assert_eq!(
            WebAuthenticator::back_to_login(&mut session, &[]),
            LOGIN_PAGE_URL
        );
        assert_eq!(
            WebAuthenticator::back_to_login(&mut session, &[("reason", "timeout")]),
            "Login.php?reason=timeout"
        );
    }

    #[test]
    fn flash_messages_are_read_once() {
        let mut session = WebSession::new();
        session.set_flash("errorMsgs", vec!["You are not logged in.".to_string()]);

        assert_eq!(
            session.take_flash("errorMsgs"),
            Some(vec!["You are not logged in.".to_string()])
        );
        assert_eq!(session.take_flash("errorMsgs"), None, "cleared after read");
        assert_eq!(session.take_flash("statusMsgs"), None);
    }
}
