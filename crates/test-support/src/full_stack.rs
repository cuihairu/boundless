//! Full-stack comparison run (BW-00-04-04).
//!
//! Feeds one shared input (a role list) to both stacks and emits two evidence
//! records — the original's and the new's — so the two sides can be diffed row
//! by row. Each row carries the side, the role, whether that stack produced a
//! clean observable start, and the detail (the captured output, or the named
//! reason it could not run here).
//!
//! The original is a full set of processes that must start in sequence with a
//! provisioned res tree, a database and a start orchestration; on this runner
//! that provisioning is absent, so the original rows record the named blocker
//! alongside the binaries' presence. The new stack is `boundless-server`
//! (server-runtime's minimal entry), which validates the role and prints a
//! startup line — real observable output per role.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The committed shared input: the reference server roles, in canonical order.
const ROLES_TEXT: &str = include_str!("../cases/full_stack/roles.txt");

/// The committed original evidence, embedded so tests never re-probe a stack
/// that is not provisioned here.
const EVIDENCE_ORIGINAL: &str = include_str!("../cases/full_stack/original.tsv");

/// The committed new evidence.
const EVIDENCE_NEW: &str = include_str!("../cases/full_stack/new.tsv");

/// The shared input as parsed records.
pub fn roles() -> Vec<String> {
    parse_roles(ROLES_TEXT)
}

/// Parse a role list: one role per line, blank lines ignored.
pub fn parse_roles(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

/// Which stack a row describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The original C++ reference stack.
    Original,
    /// The new Rust port (`boundless-server`).
    New,
}

impl Side {
    /// The side's label, as written in the evidence tables.
    pub fn label(self) -> &'static str {
        match self {
            Side::Original => "original",
            Side::New => "new",
        }
    }
}

/// Whether the stack produced a clean observable start for the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The stack started and produced observable output.
    Ran,
    /// The stack could not start; the row records why.
    Blocked,
}

impl Status {
    /// The status label, as written in the evidence tables.
    pub fn label(self) -> &'static str {
        match self {
            Status::Ran => "ran",
            Status::Blocked => "blocked",
        }
    }
}

/// One evidence row: a side, a role, what happened, and the detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// The stack this row describes.
    pub side: Side,
    /// The role this row was probed with.
    pub role: String,
    /// Whether the stack started for this role.
    pub status: Status,
    /// The captured output, or the named reason the stack could not start.
    pub detail: String,
}

/// The named blocker for the original stack: its binaries are present, but a
/// full start needs provisioning this runner does not have.
pub const ORIGINAL_BLOCKER: &str = "original-full-stack: binary present in game/bin/server/el7/server, but a full start needs a provisioned res tree, BW_RES_PATH, server/bw.xml and a database schema; none is configured here, so the row records the blocker instead of a start";

/// Probe the original stack for a role: the binary's presence and size, plus
/// the named blocker (a full start is not provisioned on this runner).
pub fn probe_original(role: &str, bin_dir: &Path) -> Evidence {
    let binary = bin_dir.join(role);
    let detail = match binary.metadata() {
        Ok(md) if md.is_file() => {
            format!("{ORIGINAL_BLOCKER} [size {} bytes]", md.len())
        }
        _ => "original binary absent".to_string(),
    };
    Evidence {
        side: Side::Original,
        role: role.to_string(),
        status: Status::Blocked,
        detail,
    }
}

/// Probe the new stack for a role by invoking the binary with that role.
pub fn probe_new(role: &str, binary: &Path) -> Evidence {
    match Command::new(binary).arg(role).output() {
        Ok(out) if out.status.success() => {
            let line = String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            Evidence {
                side: Side::New,
                role: role.to_string(),
                status: Status::Ran,
                detail: line,
            }
        }
        Ok(out) => Evidence {
            side: Side::New,
            role: role.to_string(),
            status: Status::Blocked,
            detail: String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
        },
        Err(err) => Evidence {
            side: Side::New,
            role: role.to_string(),
            status: Status::Blocked,
            detail: format!("cannot invoke new binary: {err}"),
        },
    }
}

/// Run both stacks against one shared role list; returns `(original, new)`.
pub fn compare(
    input: &[String],
    original_bin_dir: &Path,
    new_binary: &Path,
) -> (Vec<Evidence>, Vec<Evidence>) {
    let original: Vec<Evidence> = input
        .iter()
        .map(|role| probe_original(role, original_bin_dir))
        .collect();
    let new: Vec<Evidence> = input
        .iter()
        .map(|role| probe_new(role, new_binary))
        .collect();
    (original, new)
}

/// Render one evidence row as a tab-separated line.
pub fn render_row(ev: &Evidence) -> String {
    format!(
        "{}\t{}\t{}\t{}\n",
        ev.role,
        ev.side.label(),
        ev.status.label(),
        ev.detail
    )
}

/// Render a full side's evidence as a tab-separated table (header + rows).
pub fn render_side(evs: &[Evidence]) -> String {
    let mut out = String::from("role\tside\tstatus\tdetail\n");
    for ev in evs {
        let _ = write!(out, "{}", render_row(ev));
    }
    out
}

/// Parse a tab-separated side table back into rows (header skipped).
pub fn parse_side(text: &str) -> Vec<Evidence> {
    text.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            let role = parts.next()?.to_string();
            let side = match parts.next()? {
                "original" => Side::Original,
                "new" => Side::New,
                _ => return None,
            };
            let status = match parts.next()? {
                "ran" => Status::Ran,
                "blocked" => Status::Blocked,
                _ => return None,
            };
            let detail = parts.next().unwrap_or("").to_string();
            Some(Evidence {
                side,
                role,
                status,
                detail,
            })
        })
        .collect()
}

/// The committed original evidence, parsed.
pub fn evidence_original() -> Vec<Evidence> {
    parse_side(EVIDENCE_ORIGINAL)
}

/// The committed new evidence, parsed.
pub fn evidence_new() -> Vec<Evidence> {
    parse_side(EVIDENCE_NEW)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed original evidence covers every reference role, each
    /// blocked with the named provisioning reason (a full start is not
    /// provisioned on this runner).
    #[test]
    fn committed_original_evidence_is_named_blockers() {
        let evs = evidence_original();
        assert_eq!(evs.len(), roles().len(), "one row per reference role");
        for ev in &evs {
            assert_eq!(ev.side, Side::Original);
            assert_eq!(ev.status, Status::Blocked);
            assert!(
                ev.detail.contains("original-full-stack"),
                "original row carries the named blocker: {}",
                ev.detail
            );
        }
    }

    /// The committed new evidence covers every reference role, each ran with
    /// the new stack's real startup line.
    #[test]
    fn committed_new_evidence_records_real_starts() {
        let evs = evidence_new();
        assert_eq!(evs.len(), roles().len(), "one row per reference role");
        for ev in &evs {
            assert_eq!(ev.side, Side::New);
            assert_eq!(ev.status, Status::Ran);
            assert!(
                ev.detail.contains("starting (boundless minimal entry)"),
                "new row captures the startup line: {}",
                ev.detail
            );
        }
    }

    /// The two committed evidence files cover the same role set — the
    /// acceptance's "同一输入产生两份证据" — row by row.
    #[test]
    fn both_evidence_files_share_one_role_set() {
        let original = evidence_original();
        let new = evidence_new();
        let orig_roles: Vec<_> = original.iter().map(|e| e.role.clone()).collect();
        let new_roles: Vec<_> = new.iter().map(|e| e.role.clone()).collect();
        assert_eq!(orig_roles, new_roles, "both sides ran the same input");
        assert_eq!(orig_roles, roles(), "the committed input");
    }

    /// Rendering then parsing a side round-trips.
    #[test]
    fn render_parse_round_trip() {
        let evs = evidence_new();
        let text = render_side(&evs);
        assert_eq!(parse_side(&text), evs);
    }

    /// The role list parser trims and drops blank lines.
    #[test]
    fn parse_roles_trims_and_drops_blanks() {
        let roles = parse_roles(" baseapp \n\nloginapp\n  reviver  \n");
        assert_eq!(roles, vec!["baseapp", "loginapp", "reviver"]);
    }

    /// Probing an original role whose binary is present records the blocker
    /// with the binary's size; one whose binary is absent records that.
    #[test]
    fn probe_original_records_presence_and_blocker() {
        let dir = std::env::temp_dir().join(format!("bw00-full-stack-orig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let bin = dir.join("baseapp");
        std::fs::write(&bin, "not-the-real-binary").expect("write");

        let present = probe_original("baseapp", &dir);
        assert_eq!(present.status, Status::Blocked);
        assert!(present.detail.contains("original-full-stack"));
        assert!(present.detail.contains("size "));

        let absent = probe_original("loginapp", &dir);
        assert!(absent.detail.contains("absent"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
