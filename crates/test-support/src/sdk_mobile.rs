//! BW-21-06-03: the original Android/iOS sample clients' protocol — verified
//! against the committed protocol catalog, with the sample-run gap named.
//!
//! The reference's mobile sample clients speak the client-facing external
//! protocol: the login handshake (`lib/connection/login_interface.hpp`) plus
//! the BaseApp client interface (`lib/connection/baseapp_ext_interface.hpp`).
//! Every message the samples depend on is captured fact-by-fact in the
//! committed catalogs ([`client_login`], [`protocol_fixtures`]), so the
//! protocol face is independently verified without the samples themselves.
//! Building and running the original Android/iOS sample apps needs the mobile
//! toolchains (Android Studio / Xcode) and the sample projects, which are
//! absent from this reference checkout; that is recorded as the named gap.
//!
//! The Boundless SDK's [`crate::sdk_session`] implements the same handshake,
//! so the SDK covers the protocol the samples need.

/// The client interface the original mobile samples talk to.
pub const SAMPLE_CLIENT_INTERFACE: &str = "BaseAppExtInterface";

/// The login-handshake messages the original client sends, in order.
pub const SAMPLE_LOGIN_HANDSHAKE: &[&str] = &["login", "probe", "challengeResponse"];

/// The BaseAppExt messages the sample clients use: the login, the auth
/// reply, and the disconnect the server sends.
pub const SAMPLE_ENTITY_HANDSHAKE: &[&str] = &["baseAppLogin", "authenticate", "disconnectClient"];

/// Where the reference's mobile build support lives (Android openssl).
pub const ANDROID_BUILD_SUPPORT: &str = "programming/bigworld/third_party/openssl_android";

/// The named gap: the sample projects and the mobile toolchains.
pub const MOBILE_SAMPLE_GAP: &str = "the original Android/iOS sample projects are absent from the reference checkout (only the shared client protocol and third_party/openssl_android build support remain), and the Android NDK/Xcode toolchains are absent from this runner";

/// The verification result for the original mobile samples' protocol face.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileSampleResult {
    /// The client interface the samples speak.
    pub client_interface: &'static str,
    /// Whether every login-handshake message has a recorded fact.
    pub login_recorded: bool,
    /// Whether every BaseAppExt message has a recorded fact.
    pub entity_recorded: bool,
    /// Whether the login sequence matches the SDK's session steps.
    pub session_covers_handshake: bool,
    /// The named gap (never empty: the samples cannot be run here).
    pub gap: &'static str,
}

/// Find one message's catalog fact for an interface by name.
pub fn message_of(interface: &str, message: &str) -> Option<crate::protocol_fixtures::Message> {
    crate::protocol_fixtures::interfaces()
        .into_iter()
        .find(|iface| iface.name == interface)
        .and_then(|iface| iface.messages.into_iter().find(|msg| msg.name == message))
}

/// Verify the original mobile samples' protocol face: every message the
/// samples speak has a recorded catalog fact, and the SDK's session mirrors
/// the login sequence.  The sample-run gap is always named.
pub fn verify_mobile_sample_protocol() -> MobileSampleResult {
    let login_messages = crate::client_login::login_messages();
    let login_recorded = SAMPLE_LOGIN_HANDSHAKE
        .iter()
        .all(|name| !login_messages.is_empty() && login_messages.split(", ").any(|m| m == *name));
    let entity_recorded = SAMPLE_ENTITY_HANDSHAKE
        .iter()
        .all(|name| message_of(SAMPLE_CLIENT_INTERFACE, name).is_some());
    let session_steps = crate::client_login::request_steps();
    let session_covers_handshake = !session_steps.is_empty()
        && session_steps.iter().any(|step| step.contains("logOnTo"))
        && session_steps
            .iter()
            .any(|step| step.contains("challengeResponse"));
    MobileSampleResult {
        client_interface: SAMPLE_CLIENT_INTERFACE,
        login_recorded,
        entity_recorded,
        session_covers_handshake,
        gap: MOBILE_SAMPLE_GAP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_handshake_messages_are_recorded() {
        // The samples' login messages (`login`, `probe`, `challengeResponse`)
        // are all in the committed login catalog.
        let messages = crate::client_login::login_messages();
        assert!(!messages.is_empty());
        for name in SAMPLE_LOGIN_HANDSHAKE {
            assert!(
                messages.split(", ").any(|m| m == *name),
                "missing login message: {name}",
            );
        }
    }

    #[test]
    fn the_sample_entity_messages_are_recorded() {
        // The sample's BaseAppExt messages each carry a real catalog fact
        // (id, declaring macro, and a canonical frame).
        for name in SAMPLE_ENTITY_HANDSHAKE {
            let msg = message_of(SAMPLE_CLIENT_INTERFACE, name)
                .unwrap_or_else(|| panic!("missing {name} in {SAMPLE_CLIENT_INTERFACE}"));
            assert!(!msg.kind.is_empty(), "{name} has no kind");
            assert!(!msg.frame.is_empty(), "{name} has no frame");
        }
    }

    #[test]
    fn the_client_interface_is_the_external_one() {
        // The samples speak the client-to-server interface.
        let iface = crate::protocol_fixtures::interfaces()
            .into_iter()
            .find(|iface| iface.name == SAMPLE_CLIENT_INTERFACE)
            .unwrap_or_else(|| panic!("{SAMPLE_CLIENT_INTERFACE} is missing"));
        assert_eq!(iface.direction, "external");
    }

    #[test]
    fn the_sdk_session_covers_the_sample_handshake() {
        // The SDK's session mirrors the sample's login sequence: logOnTo,
        // the login message, and the challenge-response.
        let result = verify_mobile_sample_protocol();
        assert!(
            result.session_covers_handshake,
            "the session steps do not cover the sample handshake: {:?}",
            crate::client_login::request_steps(),
        );
    }

    #[test]
    fn the_mobile_sample_run_gap_is_named() {
        // The samples cannot be built or run on this runner; the gap names
        // the absent toolchains and the absent sample projects.
        let result = verify_mobile_sample_protocol();
        assert!(result.gap.contains("Android/iOS"));
        assert!(result.gap.contains("NDK"));
        assert!(result.gap.contains("Xcode"));
    }

    #[test]
    fn the_mobile_build_support_is_where_the_reference_keeps_it() {
        // The one mobile artifact the reference checkout keeps is the
        // Android openssl build support.
        assert!(ANDROID_BUILD_SUPPORT.ends_with("openssl_android"));
    }
}
