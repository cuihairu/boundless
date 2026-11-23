//! BW-26-01-01: the login, session and movement event contract.
//!
//! Game and operations events share one envelope: a schema version, an event
//! id unique to its emission, the time it happened, and the source that
//! emitted it. The envelope is verifiable — a version the contract knows, an
//! id well-formed for its kind, a source that is named, and the payload
//! fields its kind requires — so a consumer rejects what does not meet the
//! contract before reading the payload.

use std::collections::BTreeMap;

/// The schema version every event under this contract carries.
pub const EVENT_VERSION: &str = "1";

/// The event kinds the contract defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventKind {
    /// A player logged in.
    Login,
    /// A session started or ended.
    Session,
    /// A player moved between spaces.
    Movement,
    /// A business-custom event reported by the Python interface.
    Custom,
}

impl EventKind {
    /// The id prefix a kind's events carry.
    pub fn prefix(self) -> &'static str {
        match self {
            EventKind::Login => "login",
            EventKind::Session => "session",
            EventKind::Movement => "movement",
            EventKind::Custom => "custom",
        }
    }

    /// The payload fields the kind requires.
    pub fn required_fields(self) -> &'static [&'static str] {
        match self {
            EventKind::Login => &["account"],
            EventKind::Session => &["session", "player"],
            EventKind::Movement => &["player", "from_space", "to_space"],
            EventKind::Custom => &["name"],
        }
    }
}

/// One event envelope: the contract fields plus the kind's payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEvent {
    /// The schema version the event was written under.
    pub version: String,
    /// The event id, unique to its emission (`login:1`).
    pub id: String,
    /// The event kind.
    pub kind: EventKind,
    /// When the event happened.
    pub at: u64,
    /// The source that emitted it (`cellapp01`).
    pub source: String,
    /// The payload fields the contract knows.
    pub fields: Vec<(String, String)>,
    /// Fields the contract does not know: carried and marked, never dropped.
    pub unknown: Vec<(String, String)>,
}

impl GameEvent {
    /// The payload value of a field, when the event carries it.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.as_str())
    }

    /// Whether the event carries a field the contract does not know.
    pub fn has_unknown(&self) -> bool {
        !self.unknown.is_empty()
    }
}

/// Why an event does not meet the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractError {
    /// The version is not one the contract knows.
    UnknownVersion,
    /// The event id is not well-formed for its kind.
    MalformedId,
    /// The source is missing.
    MissingSource,
    /// The payload lacks a field its kind requires.
    MissingField(&'static str),
}

/// Verify an event against the contract: the version, the event id, the
/// source and the kind's required fields each check out, or the event names
/// the first field that failed.
pub fn verify(event: &GameEvent) -> Result<(), ContractError> {
    if event.version != EVENT_VERSION {
        return Err(ContractError::UnknownVersion);
    }
    let prefix = event.kind.prefix();
    let serial = event
        .id
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(':'));
    if !serial.is_some_and(|serial| serial.parse::<u64>().is_ok()) {
        return Err(ContractError::MalformedId);
    }
    if event.source.is_empty() {
        return Err(ContractError::MissingSource);
    }
    for field in event.kind.required_fields() {
        if event.field(field).is_none() {
            return Err(ContractError::MissingField(field));
        }
    }
    Ok(())
}

/// Records events under the contract, minting each emission's id: one serial
/// per kind, so no two events share an id.
#[derive(Default)]
pub struct EventRecorder {
    next_serial: BTreeMap<EventKind, u64>,
}

impl EventRecorder {
    /// A recorder that has emitted nothing.
    pub fn new() -> Self {
        Self::default()
    }

    fn mint(&mut self, kind: EventKind) -> String {
        let serial = self.next_serial.entry(kind).or_insert(0);
        *serial += 1;
        format!("{}:{serial}", kind.prefix())
    }

    /// Record a login event: which account logged in, when, from which
    /// source.
    pub fn login(&mut self, at: u64, source: &str, account: &str) -> GameEvent {
        self.event(EventKind::Login, at, source, vec![("account", account)])
    }

    /// Record a session event: which session and player it is about.
    pub fn session(&mut self, at: u64, source: &str, session: &str, player: &str) -> GameEvent {
        self.event(
            EventKind::Session,
            at,
            source,
            vec![("session", session), ("player", player)],
        )
    }

    /// Record a movement event: which player moved from one space to another.
    pub fn movement(
        &mut self,
        at: u64,
        source: &str,
        player: &str,
        from_space: u64,
        to_space: u64,
    ) -> GameEvent {
        let from = from_space.to_string();
        let to = to_space.to_string();
        self.event(
            EventKind::Movement,
            at,
            source,
            vec![("player", player), ("from_space", &from), ("to_space", &to)],
        )
    }

    /// Record a business-custom event as the Python interface reports it: its
    /// name, when it happened, the source, and its free-form fields. A field
    /// the contract knows (listed in `known`) is kept as a contract field; a
    /// field it does not know is carried and marked unknown, per the
    /// contract — never dropped, never mistaken for a contract field.
    pub fn custom(
        &mut self,
        at: u64,
        source: &str,
        name: &str,
        fields: &[(&str, &str)],
        known: &[&str],
    ) -> GameEvent {
        let mut known_fields = Vec::new();
        let mut unknown_fields = Vec::new();
        for &(field, value) in fields {
            if known.contains(&field) {
                known_fields.push((field.to_string(), value.to_string()));
            } else {
                unknown_fields.push((field.to_string(), value.to_string()));
            }
        }
        let mut event = self.event(EventKind::Custom, at, source, vec![("name", name)]);
        event.fields.extend(known_fields);
        event.unknown = unknown_fields;
        event
    }

    /// Record an event of any kind with its payload fields.
    pub fn event(
        &mut self,
        kind: EventKind,
        at: u64,
        source: &str,
        fields: Vec<(&str, &str)>,
    ) -> GameEvent {
        let id = self.mint(kind);
        GameEvent {
            version: EVENT_VERSION.to_string(),
            id,
            kind,
            at,
            source: source.to_string(),
            fields: fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            unknown: Vec::new(),
        }
    }
}

/// The marker a sensitive value takes at the export boundary.
pub const REDACTED: &str = "[redacted]";

/// What the export boundary does to an event.
///
/// An export hands a sample or a batch to someone outside the recording
/// process, so a value that names a credential or a contact never crosses
/// unchanged: the policy names the fields whose values are masked, and decides
/// whether a custom event's unknown fields are carried at all. An unknown
/// field is exactly the free-form one a producer added, and a free-form field
/// is where a sensitive value is likeliest to hide.
#[derive(Debug, Clone)]
pub struct ExportPolicy {
    /// Field names, matched case-insensitively, whose values are masked on
    /// export.
    pub redacted: Vec<String>,
    /// Whether a custom event's unknown fields cross the boundary. When false
    /// they are dropped.
    pub carry_unknown: bool,
}

impl ExportPolicy {
    /// The default policy: the credential and contact fields are masked and
    /// unknown custom fields do not cross the boundary.
    pub fn default_policy() -> Self {
        Self {
            redacted: ["password", "token", "secret", "email", "phone", "ip"]
                .iter()
                .map(|name| name.to_string())
                .collect(),
            carry_unknown: false,
        }
    }

    /// Whether the policy masks a field's value, by field name.
    pub fn redacts(&self, name: &str) -> bool {
        self.redacted
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    }
}

/// One event at the export boundary: a single NDJSON line, its sensitive
/// values masked and its unknown fields handled per the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedEvent {
    line: String,
}

impl ExportedEvent {
    /// The event as one NDJSON line.
    pub fn line(&self) -> &str {
        &self.line
    }
}

/// A batch export of events: one NDJSON line per event, in the order given.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct EventExport {
    lines: Vec<String>,
}

impl EventExport {
    /// An empty export.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many events the export carries.
    pub fn count(&self) -> usize {
        self.lines.len()
    }

    /// The export's lines, one per event.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The export as NDJSON text.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The first of the given values the export still carries, when any — the
    /// acceptance that a sample leaks nothing sensitive. The value borrowed is
    /// the one passed in, never one composed from the export.
    pub fn leaks<'a>(&self, values: &[&'a str]) -> Option<&'a str> {
        values
            .iter()
            .find(|value| self.lines.iter().any(|line| line.contains(**value)))
            .copied()
    }
}

/// Export events under a policy: one line each, sensitive values masked and
/// unknown fields handled at the boundary.
pub fn export(events: &[GameEvent], policy: &ExportPolicy) -> EventExport {
    let mut batch = EventExport::new();
    for event in events {
        batch.lines.push(export_event(event, policy).line);
    }
    batch
}

/// Export one event under a policy: its envelope is carried verbatim, a
/// sensitive field's value is replaced by [`REDACTED`], and a custom event's
/// unknown field is dropped or carried per the policy — always masked when
/// carried and sensitive.
pub fn export_event(event: &GameEvent, policy: &ExportPolicy) -> ExportedEvent {
    let mut line = String::from("{\"version\":\"");
    line.push_str(&escape(&event.version));
    line.push_str("\",\"id\":\"");
    line.push_str(&escape(&event.id));
    line.push_str("\",\"kind\":\"");
    line.push_str(event.kind.prefix());
    line.push_str("\",\"at\":");
    line.push_str(&event.at.to_string());
    line.push_str(",\"source\":\"");
    line.push_str(&escape(&event.source));
    line.push_str("\",\"fields\":{");
    let mut first = true;
    for (name, value) in &event.fields {
        if !first {
            line.push(',');
        }
        first = false;
        line.push('"');
        line.push_str(&escape(name));
        line.push_str("\":\"");
        let shown = if policy.redacts(name) {
            REDACTED
        } else {
            value.as_str()
        };
        line.push_str(&escape(shown));
        line.push('"');
    }
    line.push('}');
    if policy.carry_unknown && !event.unknown.is_empty() {
        line.push_str(",\"unknown\":{");
        let mut first = true;
        for (name, value) in &event.unknown {
            if !first {
                line.push(',');
            }
            first = false;
            line.push('"');
            line.push_str(&escape(name));
            line.push_str("\":\"");
            let shown = if policy.redacts(name) {
                REDACTED
            } else {
                value.as_str()
            };
            line.push_str(&escape(shown));
            line.push('"');
        }
        line.push('}');
    }
    line.push('}');
    ExportedEvent { line }
}

/// Quotes and escapes a string as a JSON string value.
fn escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_session_and_movement_events_verify_under_the_contract() {
        let mut recorder = EventRecorder::new();

        let login = recorder.login(1_000, "baseapp01", "alice");
        let session = recorder.session(1_100, "baseapp01", "s-1", "alice");
        let movement = recorder.movement(1_200, "cellapp01", "alice", 1, 2);

        // Every kind verifies: the version, the id, the source and the
        // required fields each check out.
        assert_eq!(verify(&login), Ok(()));
        assert_eq!(verify(&session), Ok(()));
        assert_eq!(verify(&movement), Ok(()));

        // The envelope names what it is: the contract version, a unique id
        // per emission, the time, and the source that emitted it.
        assert_eq!(login.version, "1");
        assert_eq!(login.id, "login:1");
        assert_eq!(login.at, 1_000);
        assert_eq!(login.source, "baseapp01");
        assert_eq!(session.id, "session:1");
        assert_eq!(movement.id, "movement:1");

        // The payload carries its kind's fields.
        assert_eq!(login.field("account"), Some("alice"));
        assert_eq!(session.field("session"), Some("s-1"));
        assert_eq!(session.field("player"), Some("alice"));
        assert_eq!(movement.field("player"), Some("alice"));
        assert_eq!(movement.field("from_space"), Some("1"));
        assert_eq!(movement.field("to_space"), Some("2"));
    }

    #[test]
    fn each_emission_mints_its_own_id() {
        let mut recorder = EventRecorder::new();
        let first = recorder.login(1, "baseapp01", "alice");
        let second = recorder.login(2, "baseapp01", "bob");
        let session = recorder.session(3, "baseapp01", "s-1", "alice");

        // Serials run per kind: two logins, then the first session.
        assert_eq!(first.id, "login:1");
        assert_eq!(second.id, "login:2");
        assert_eq!(session.id, "session:1");
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn a_tampered_version_id_or_source_fails_verification() {
        let mut recorder = EventRecorder::new();
        let login = recorder.login(1_000, "baseapp01", "alice");

        // A version the contract does not know.
        let mut future = login.clone();
        future.version = "2".to_string();
        assert_eq!(verify(&future), Err(ContractError::UnknownVersion));

        // An id that is not well-formed for its kind.
        let mut mismatched = login.clone();
        mismatched.id = "session:1".to_string();
        assert_eq!(verify(&mismatched), Err(ContractError::MalformedId));
        let mut unnumbered = login.clone();
        unnumbered.id = "login:x".to_string();
        assert_eq!(verify(&unnumbered), Err(ContractError::MalformedId));

        // A source that is missing.
        let mut anonymous = login.clone();
        anonymous.source = String::new();
        assert_eq!(verify(&anonymous), Err(ContractError::MissingSource));

        // The honest event still verifies.
        assert_eq!(verify(&login), Ok(()));
    }

    #[test]
    fn a_kind_must_carry_its_required_fields() {
        let mut recorder = EventRecorder::new();
        let mut movement = recorder.movement(1_000, "cellapp01", "alice", 1, 2);

        // A movement without its destination does not meet the contract, and
        // the verification names the field that is missing.
        movement.fields.retain(|(name, _)| name != "to_space");
        assert_eq!(
            verify(&movement),
            Err(ContractError::MissingField("to_space"))
        );

        // A custom event of no required kind verifies on its envelope alone.
        let custom = recorder.event(EventKind::Login, 2_000, "cellapp01", vec![("account", "x")]);
        assert_eq!(verify(&custom), Ok(()));
    }

    #[test]
    fn a_custom_event_keeps_unknown_fields_marked_per_the_contract() {
        let mut recorder = EventRecorder::new();
        let purchase = recorder.custom(
            1_000,
            "cellapp01",
            "purchase",
            &[("item", "sword"), ("price", "100"), ("loyalty", "gold")],
            &["item", "price"],
        );

        // The custom event meets the contract: the envelope checks out and it
        // carries its name.
        assert_eq!(verify(&purchase), Ok(()));
        assert_eq!(purchase.kind, EventKind::Custom);
        assert_eq!(purchase.id, "custom:1");
        assert_eq!(purchase.source, "cellapp01");
        assert_eq!(purchase.field("name"), Some("purchase"));

        // Known fields are contract fields; the unknown one is carried and
        // marked, per the contract — not dropped, not read as a contract
        // field.
        assert_eq!(purchase.field("item"), Some("sword"));
        assert_eq!(purchase.field("price"), Some("100"));
        assert_eq!(purchase.field("loyalty"), None);
        assert_eq!(
            purchase.unknown,
            vec![("loyalty".to_string(), "gold".to_string())]
        );
        assert!(purchase.has_unknown());
    }

    #[test]
    fn a_custom_event_verifies_with_unknown_only_fields() {
        let mut recorder = EventRecorder::new();
        let telemetry = recorder.custom(
            2_000,
            "baseapp01",
            "telemetry",
            &[("heartbeat_ms", "50")],
            &["item"],
        );

        // An event whose every field is unknown still meets the contract: the
        // envelope is what the contract checks.
        assert_eq!(verify(&telemetry), Ok(()));
        assert!(telemetry.has_unknown());
        assert_eq!(telemetry.unknown.len(), 1);
        assert_eq!(
            telemetry.unknown[0],
            ("heartbeat_ms".to_string(), "50".to_string())
        );
    }

    #[test]
    fn a_custom_event_requires_its_name() {
        let mut recorder = EventRecorder::new();
        let mut custom = recorder.custom(
            1_000,
            "cellapp01",
            "purchase",
            &[("item", "sword")],
            &["item"],
        );
        custom.fields.retain(|(name, _)| name != "name");

        // Without the name that makes a custom event a named one, the event
        // does not meet the contract, and verification names the field.
        assert_eq!(verify(&custom), Err(ContractError::MissingField("name")));
    }

    #[test]
    fn the_default_policy_names_the_sensitive_fields() {
        let policy = ExportPolicy::default_policy();

        // The credential and contact fields are masked, matched
        // case-insensitively.
        assert!(policy.redacts("password"));
        assert!(policy.redacts("TOKEN"));
        assert!(policy.redacts("Email"));
        assert!(policy.redacts("ip"));
        assert!(policy.redacts("secret"));
        assert!(policy.redacts("phone"));

        // A field an analysis needs is not masked.
        assert!(!policy.redacts("account"));
        assert!(!policy.redacts("player"));
        assert!(!policy.redacts("item"));
    }

    #[test]
    fn a_sample_export_masks_sensitive_fields() {
        let mut recorder = EventRecorder::new();
        let audit = recorder.custom(
            1_000,
            "baseapp01",
            "login_audit",
            &[
                ("account", "alice"),
                ("password", "hunter2"),
                ("email", "alice@example.com"),
                ("ip", "10.0.0.7"),
                ("token", "abc123"),
            ],
            &["account", "password", "email", "ip", "token"],
        );
        let login = recorder.login(1_100, "baseapp01", "alice");

        // The batch is one line per event, in the order given.
        let batch = export(&[audit, login], &ExportPolicy::default_policy());
        assert_eq!(batch.count(), 2);
        assert_eq!(batch.lines().len(), 2);

        // The core acceptance: no sensitive value leaks in the sample. Each is
        // replaced by the redaction marker instead.
        assert_eq!(
            batch.leaks(&["hunter2", "alice@example.com", "10.0.0.7", "abc123"]),
            None
        );
        let line = &batch.lines()[0];
        assert_eq!(line.matches(REDACTED).count(), 4);
        assert!(line.contains("\"account\":\"alice\""));

        // The non-sensitive login line is untouched.
        assert!(batch.lines()[1].contains("\"kind\":\"login\""));
        assert!(batch.lines()[1].contains("\"account\":\"alice\""));
    }

    #[test]
    fn the_export_carries_the_envelope_and_contract_fields() {
        let mut recorder = EventRecorder::new();
        let movement = recorder.movement(2_000, "cellapp01", "alice", 1, 2);

        let exported = export_event(&movement, &ExportPolicy::default_policy());
        let line = exported.line();
        assert!(line.contains("\"version\":\"1\""));
        assert!(line.contains("\"id\":\"movement:1\""));
        assert!(line.contains("\"kind\":\"movement\""));
        assert!(line.contains("\"at\":2000"));
        assert!(line.contains("\"source\":\"cellapp01\""));
        assert!(line.contains("\"player\":\"alice\""));
        assert!(line.contains("\"from_space\":\"1\""));
        assert!(line.contains("\"to_space\":\"2\""));
    }

    #[test]
    fn unknown_fields_cross_the_boundary_only_when_the_policy_allows() {
        let mut recorder = EventRecorder::new();
        let purchase = recorder.custom(
            1_000,
            "cellapp01",
            "purchase",
            &[("item", "sword"), ("loyalty", "gold")],
            &["item"],
        );

        // The default policy drops a custom event's unknown field.
        let dropped = export_event(&purchase, &ExportPolicy::default_policy());
        let default = dropped.line();
        assert!(!default.contains("loyalty"));
        assert!(!default.contains("gold"));

        // A policy that carries unknown fields keeps it.
        let mut permissive = ExportPolicy::default_policy();
        permissive.carry_unknown = true;
        let carried = export_event(&purchase, &permissive);
        let line = carried.line();
        assert!(line.contains("\"unknown\":{\"loyalty\":\"gold\"}"));
    }

    #[test]
    fn a_carried_sensitive_unknown_field_is_still_masked() {
        let mut recorder = EventRecorder::new();
        let telemetry =
            recorder.custom(2_000, "baseapp01", "telemetry", &[("token", "abc123")], &[]);

        let mut permissive = ExportPolicy::default_policy();
        permissive.carry_unknown = true;
        let exported = export_event(&telemetry, &permissive);
        let line = exported.line();

        // The unknown field crosses, but its sensitive value is masked: the
        // field name is there, the value is not.
        assert!(line.contains("\"unknown\":{\"token\":\"[redacted]\"}"));
        assert!(!line.contains("abc123"));
    }
}
