//! Minimal client login baseline (BW-00-02-04).
//!
//! Records the minimal client login's request and callback sequences — the
//! client-side logon flow (`logOnTo` → `login`/`challengeResponse` → BaseApp
//! connect) and the server-side callbacks (`onLoginAppLoginChallengeIssued` →
//! `onLoginChallengeCompleted` → `onLoginAppLoginSuccess` →
//! `onBaseAppLoginSuccess` → `onLoggedOn`/`onLogOnFailure`/`onLoggedOff`) — as
//! described by the reference's client-integration example and the connection
//! library. The committed catalog (`cases/client_login.txt`) is regenerated
//! with the `scan-client-login` binary and re-parsed by the catalog test, so
//! the file and the scanner cannot drift.
//!
//! `login-messages` and `logon-statuses` are re-derived from the reference
//! source (`lib/connection/login_interface.hpp`, `log_on_status.hpp`); the
//! request/callback/blocker sequence (`cases/client_login/sequence.txt`) and
//! the real client run (`cases/client_login/attempt.log`) are frozen
//! artifacts, re-emitted verbatim by the scanner.
//!
//! The real client run is a bounded `simple_python_client` launch against the
//! example's resource tree: it loads the entity definitions, binds a network
//! interface, then fails at `LoginApp` public-key initialisation — the named
//! `loginapp-pubkey` blocker. The `cluster-start` blocker names why a full
//! login cannot be completed even past that point (see BW-00-02-03).
//!
//! Catalog line grammar (tab-separated `item`, `value`, `source`):
//!
//! * `reference-commit` — the reference checkout's git HEAD.
//! * `login-messages` — the `LoginInterface` message names.
//! * `logon-statuses` — the `LogOnStatus` enum member names.
//! * `request.<n>` / `callback.<n>` — one fact per frozen sequence step.
//! * `blocker.<name>` — a named blocker for a full live login.
//! * `attempt.<n>` — one fact per line of the frozen client run.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/client_login.txt");

/// The frozen request/callback/blocker sequence, committed as derived from the
/// reference source.
const SEQUENCE_TEXT: &str = include_str!("../cases/client_login/sequence.txt");

/// The frozen real client run (bounded `simple_python_client` launch).
const ATTEMPT_TEXT: &str = include_str!("../cases/client_login/attempt.log");

/// One client-login baseline fact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientLoginFact {
    /// The fact name (`reference-commit`, `request.1`, ...).
    pub item: String,
    /// The recorded value.
    pub value: String,
    /// The file the value came from (or the probe/command).
    pub source: String,
}

/// One frozen sequence step (request, callback or blocker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceStep {
    /// `request`, `callback` or `blocker`.
    pub kind: String,
    /// The step index (request/callback) or the blocker name.
    pub name: String,
    /// The step's description.
    pub description: String,
    /// The source file the step was read from.
    pub source: String,
}

/// The git HEAD of a checkout, or `-` when it is not a git repository.
fn git_head(root: &Path) -> String {
    match Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("--short=12")
        .arg("HEAD")
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "-".to_string(),
    }
}

/// The `LoginInterface` message names, from a `login_interface.hpp` source.
fn login_messages_from_source(text: &str) -> String {
    let mut names = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        for prefix in ["MERCURY_VARIABLE_MESSAGE(", "MERCURY_EMPTY_MESSAGE("] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name = rest.trim().split(',').next().unwrap_or("").trim();
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    }
    names.join(", ")
}

/// The `LogOnStatus` enum member names, from a `log_on_status.hpp` source.
fn logon_statuses_from_source(text: &str) -> Vec<String> {
    let Some(start) = text.find("enum Status") else {
        return Vec::new();
    };
    let Some(brace) = text[start..].find('{') else {
        return Vec::new();
    };
    let Some(end) = text[start + brace..].find('}') else {
        return Vec::new();
    };
    let body = &text[start + brace + 1..start + brace + end];
    body.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('}'))
        .filter_map(|l| {
            let id = l
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .trim_end_matches(',');
            if id.is_empty() || !id.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                None
            } else {
                Some(id.to_string())
            }
        })
        .collect()
}

/// Parse the frozen sequence table into steps.
pub fn parse_sequence(text: &str) -> Vec<SequenceStep> {
    text.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut f = line.split('\t');
            let kind = f.next()?.to_string();
            let name = f.next()?.to_string();
            let description = f.next()?.to_string();
            let source = f.next().unwrap_or("-").to_string();
            Some(SequenceStep {
                kind,
                name,
                description,
                source,
            })
        })
        .collect()
}

/// Scan a reference checkout's minimal client-login baseline.
pub fn scan_reference(root: &Path) -> Vec<ClientLoginFact> {
    let bw_source = root.join("programming").join("bigworld");
    let mut facts = Vec::new();

    // Reference revision.
    facts.push(ClientLoginFact {
        item: "reference-commit".to_string(),
        value: git_head(root),
        source: "git rev-parse HEAD".to_string(),
    });

    // Login message names (re-derived from LoginInterface).
    let interface = std::fs::read_to_string(bw_source.join("lib/connection/login_interface.hpp"))
        .unwrap_or_default();
    let messages = login_messages_from_source(&interface);
    facts.push(ClientLoginFact {
        item: "login-messages".to_string(),
        value: if messages.is_empty() {
            "-".to_string()
        } else {
            messages
        },
        source: "lib/connection/login_interface.hpp".to_string(),
    });

    // LogOnStatus enum member names (re-derived from LogOnStatus).
    let statuses = std::fs::read_to_string(bw_source.join("lib/connection/log_on_status.hpp"))
        .unwrap_or_default();
    let statuses = logon_statuses_from_source(&statuses);
    facts.push(ClientLoginFact {
        item: "logon-statuses".to_string(),
        value: if statuses.is_empty() {
            "-".to_string()
        } else {
            statuses.join(",")
        },
        source: "lib/connection/log_on_status.hpp".to_string(),
    });

    // The frozen request/callback/blocker sequence, re-emitted verbatim.
    for step in parse_sequence(SEQUENCE_TEXT) {
        facts.push(ClientLoginFact {
            item: format!("{}.{}", step.kind, step.name),
            value: step.description,
            source: step.source,
        });
    }

    // The frozen real client run, re-emitted verbatim.
    for (i, line) in ATTEMPT_TEXT.lines().enumerate() {
        facts.push(ClientLoginFact {
            item: format!("attempt.{}", i + 1),
            value: line.to_string(),
            source: "simple_python_client --res <example>:<game/res> -server <addr>:<port> -user MyAccount -password a (bounded live login attempt, init gap)"
                .to_string(),
        });
    }

    facts
}

/// Render the scan into the tab-separated catalog text.
///
/// A value that itself contains a tab (the captured client log uses a tab as a
/// column separator) is escaped as `\t`, so the round-trip through
/// [`parse_catalog`] recovers it verbatim.
pub fn render(facts: &[ClientLoginFact]) -> String {
    let mut out = String::new();
    out.push_str("item\tvalue\tsource\n");
    for f in facts {
        let value = f.value.replace('\t', "\\t");
        let _ = writeln!(out, "{}\t{}\t{}", f.item, value, f.source);
    }
    out
}

/// Parse the catalog text back into [`ClientLoginFact`]s.
pub fn parse_catalog(text: &str) -> Vec<ClientLoginFact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(ClientLoginFact {
            item: f.next().unwrap_or("").to_string(),
            value: f.next().unwrap_or("-").replace("\\t", "\t"),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<ClientLoginFact> {
    parse_catalog(CATALOG_TEXT)
}

fn facts_with_prefix(prefix: &str) -> Vec<ClientLoginFact> {
    catalog()
        .into_iter()
        .filter(|f| f.item.starts_with(prefix))
        .collect()
}

/// The committed login-message names.
pub fn login_messages() -> String {
    facts_with_prefix("login-messages")
        .first()
        .map(|f| f.value.clone())
        .unwrap_or_default()
}

/// The committed `LogOnStatus` member names.
pub fn logon_statuses() -> Vec<String> {
    facts_with_prefix("logon-statuses")
        .first()
        .map(|f| f.value.clone())
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// The committed request steps, in order.
pub fn request_steps() -> Vec<String> {
    let mut steps: Vec<(u32, String)> = facts_with_prefix("request.")
        .into_iter()
        .filter_map(|f| {
            f.item
                .strip_prefix("request.")
                .and_then(|n| n.parse().ok())
                .map(|n| (n, f.value))
        })
        .collect();
    steps.sort_by_key(|(n, _)| *n);
    steps.into_iter().map(|(_, v)| v).collect()
}

/// The committed callback steps, in order.
pub fn callback_steps() -> Vec<String> {
    let mut steps: Vec<(u32, String)> = facts_with_prefix("callback.")
        .into_iter()
        .filter_map(|f| {
            f.item
                .strip_prefix("callback.")
                .and_then(|n| n.parse().ok())
                .map(|n| (n, f.value))
        })
        .collect();
    steps.sort_by_key(|(n, _)| *n);
    steps.into_iter().map(|(_, v)| v).collect()
}

/// The committed named blockers: `(name, description)`.
pub fn blockers() -> Vec<(String, String)> {
    facts_with_prefix("blocker.")
        .into_iter()
        .filter_map(|f| {
            f.item
                .strip_prefix("blocker.")
                .map(|name| (name.to_string(), f.value))
        })
        .collect()
}

/// The committed frozen real client-run lines.
pub fn attempt_lines() -> Vec<String> {
    let mut lines: Vec<(u32, String)> = facts_with_prefix("attempt.")
        .into_iter()
        .filter_map(|f| {
            f.item
                .strip_prefix("attempt.")
                .and_then(|n| n.parse().ok())
                .map(|n| (n, f.value))
        })
        .collect();
    lines.sort_by_key(|(n, _)| *n);
    lines.into_iter().map(|(_, v)| v).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog retains the request and callback sequences, the
    /// login messages, the status codes, the real client run and the named
    /// blockers.
    #[test]
    fn committed_catalog_retains_request_and_callback_sequences() {
        let requests = request_steps();
        assert!(requests.len() >= 4, "{requests:?}");
        assert!(requests[0].contains("logOnTo"), "{}", requests[0]);
        assert!(requests[1].contains("login message"), "{}", requests[1]);
        assert!(requests[1].contains("LogOnParams"), "{}", requests[1]);
        assert!(requests.iter().any(|r| r.contains("challengeResponse")));
        assert!(requests.iter().any(|r| r.contains("LoginReplyRecord")));

        let callbacks = callback_steps();
        assert!(callbacks.len() >= 5, "{callbacks:?}");
        assert!(callbacks[0].contains("onLoginAppLoginChallengeIssued"));
        assert!(callbacks[1].contains("onLoginChallengeCompleted"));
        assert!(callbacks[2].contains("onLoginAppLoginSuccess"));
        assert!(callbacks[3].contains("onBaseAppLoginSuccess"));
        assert!(
            callbacks[4].contains("onLoggedOn")
                && callbacks[4].contains("onLogOnFailure")
                && callbacks[4].contains("onLoggedOff"),
            "{}",
            callbacks[4]
        );

        let messages = login_messages();
        assert!(messages.contains("login"), "{messages}");
        assert!(messages.contains("challengeResponse"), "{messages}");
        assert!(messages.contains("probe"), "{messages}");

        let statuses = logon_statuses();
        assert!(statuses.contains(&"LOGGED_ON".to_string()));
        assert!(
            statuses.contains(&"LOGIN_REJECTED_NO_SUCH_USER".to_string())
                && statuses.contains(&"LOGIN_REJECTED_INVALID_PASSWORD".to_string())
        );

        let attempt = attempt_lines();
        assert!(!attempt.is_empty());
        let joined = attempt.join("\n");
        assert!(joined.contains("Added res path"), "{joined}");
        assert!(joined.contains("ClientAvatar"), "entity defs loaded");
        assert!(
            joined.contains("Failed to initialise LoginApp public key"),
            "the real gap is recorded: {joined}"
        );

        let blockers = blockers();
        assert!(blockers.iter().any(|(n, _)| n == "loginapp-pubkey"));
        assert!(blockers.iter().any(|(n, _)| n == "cluster-start"));
        let cluster = blockers
            .iter()
            .find(|(n, _)| n == "cluster-start")
            .expect("cluster-start blocker");
        assert!(cluster.1.contains("BW-00-02-03"), "{}", cluster.1);
    }

    /// Every committed fact row carries a value and a source.
    #[test]
    fn every_fact_has_value_and_source() {
        for fact in catalog() {
            assert!(!fact.value.is_empty(), "{} has no value", fact.item);
            assert!(!fact.source.is_empty(), "{} has no source", fact.item);
        }
    }

    /// The catalog round-trips through render and parse.
    #[test]
    fn render_parse_round_trip() {
        assert_eq!(parse_catalog(&render(&catalog())), catalog());
    }

    /// The sequence table parses into request, callback and blocker steps.
    #[test]
    fn sequence_table_parses_into_kinds() {
        let steps = parse_sequence(SEQUENCE_TEXT);
        let kinds: Vec<&str> = steps.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"request"));
        assert!(kinds.contains(&"callback"));
        assert!(kinds.contains(&"blocker"));
        for step in &steps {
            assert!(!step.description.is_empty());
            assert!(!step.source.is_empty());
        }
    }

    /// The login message parser extracts the `LoginInterface` message names.
    #[test]
    fn login_message_parser_extracts_names() {
        let src = "\
MERCURY_VARIABLE_MESSAGE( login, 2, &gLoginHandler )\n\
MERCURY_EMPTY_MESSAGE( probe, &gProbeHandler )\n\
MERCURY_VARIABLE_MESSAGE( challengeResponse, 2, &gChallengeResponseHandler )\n";
        assert_eq!(
            login_messages_from_source(src),
            "login, probe, challengeResponse"
        );
        assert_eq!(login_messages_from_source(""), "");
    }

    /// The status parser extracts the `LogOnStatus` member names, handling
    /// `= N` values and trailing comments.
    #[test]
    fn status_parser_extracts_member_names() {
        let src = "enum Status\n{\n\tNOT_SET,\n\tLOGGED_ON,\n\tLAST_CLIENT_SIDE_VALUE = 63,\n\tLOGIN_REJECTED_NO_SUCH_USER,\n\tLOGIN_REJECTED_IP_ADDRESS_BAN = 244, // comment\n};\n";
        let names = logon_statuses_from_source(src);
        assert!(names.contains(&"NOT_SET".to_string()));
        assert!(names.contains(&"LOGGED_ON".to_string()));
        assert!(names.contains(&"LAST_CLIENT_SIDE_VALUE".to_string()));
        assert!(names.contains(&"LOGIN_REJECTED_NO_SUCH_USER".to_string()));
        assert!(names.contains(&"LOGIN_REJECTED_IP_ADDRESS_BAN".to_string()));
        assert!(!names.contains(&"comment".to_string()));
        assert_eq!(logon_statuses_from_source(""), Vec::<String>::new());
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    ///
    /// The `reference-commit` row is excluded from both sides (the reference
    /// checkout's HEAD is a moving target, as in the runtime-environment and
    /// startup-baseline catalogs).
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let interface = root.join("programming/bigworld/lib/connection/login_interface.hpp");
        if !interface.is_file() {
            eprintln!("reference checkout absent; skipping rescan");
            return;
        }
        let strip_commit = |text: String| -> String {
            text.lines()
                .filter(|l| !l.starts_with("reference-commit\t"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let scan = strip_commit(render(&scan_reference(&root)));
        let catalog = strip_commit(CATALOG_TEXT.to_string());
        assert_eq!(scan, catalog);
    }
}
