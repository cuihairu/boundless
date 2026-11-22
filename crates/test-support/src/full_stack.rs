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

/// The committed single-component replacement report (BW-00-04-05).
const EVIDENCE_REPLACEMENT: &str = include_str!("../cases/full_stack/replacement.tsv");

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

/// A single-component replacement report (BW-00-04-05): one role runs as the
/// new implementation, every other role stays as the original stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementReport {
    /// The role the new implementation replaces.
    pub replaced_role: String,
    /// One row per input role; the replaced row probes the new stack, the rest
    /// probe the original.
    pub rows: Vec<Evidence>,
}

/// Build a replacement report for one role: that role runs as the new stack,
/// every other role stays as the original stack.
pub fn replacement(
    input: &[String],
    replaced_role: &str,
    original_bin_dir: &Path,
    new_binary: &Path,
) -> ReplacementReport {
    let rows = input
        .iter()
        .map(|role| {
            if role == replaced_role {
                probe_new(role, new_binary)
            } else {
                probe_original(role, original_bin_dir)
            }
        })
        .collect();
    ReplacementReport {
        replaced_role: replaced_role.to_string(),
        rows,
    }
}

/// Render a replacement report: a header naming the replaced role, then rows.
pub fn render_replacement(report: &ReplacementReport) -> String {
    let mut out = format!("replaced-role\t{}\n", report.replaced_role);
    let _ = write!(out, "{}", render_side(&report.rows));
    out
}

/// Parse a replacement report back into its parts.
pub fn parse_replacement(text: &str) -> ReplacementReport {
    let mut lines = text.lines();
    let replaced_role = lines
        .next()
        .and_then(|line| line.strip_prefix("replaced-role\t"))
        .unwrap_or("")
        .to_string();
    let rest: String = lines.map(|line| format!("{line}\n")).collect();
    ReplacementReport {
        replaced_role,
        rows: parse_side(&rest),
    }
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

/// The committed single-component replacement report, parsed.
pub fn replacement_evidence() -> ReplacementReport {
    parse_replacement(EVIDENCE_REPLACEMENT)
}

/// The kind of difference the report located (BW-00-04-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// A field's value differs between the two evidence tables.
    Field,
    /// A probe did not complete (a row is missing, or a detail says it timed
    /// out).
    Timeout,
    /// The role order differs between the two evidence tables.
    Order,
}

impl DiffKind {
    /// The kind label, as written in the difference table.
    pub fn label(self) -> &'static str {
        match self {
            DiffKind::Field => "field",
            DiffKind::Timeout => "timeout",
            DiffKind::Order => "order",
        }
    }
}

/// One located difference between the original and new evidence tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// Which kind of difference this is.
    pub kind: DiffKind,
    /// The role the difference concerns (empty for a whole-table order diff).
    pub role: String,
    /// Where and what the difference is, so it can be located.
    pub summary: String,
}

/// The committed differences report.
const EVIDENCE_DIFFERENCES: &str = include_str!("../cases/full_stack/differences.tsv");

/// Compare the original and new evidence tables and locate field, timeout and
/// order differences, each keyed by kind and role.
pub fn diff_reports(original: &[Evidence], new: &[Evidence]) -> Vec<Diff> {
    let mut diffs = Vec::new();

    // Order difference: the role sequences disagree.
    let orig_roles: Vec<&str> = original.iter().map(|e| e.role.as_str()).collect();
    let new_roles: Vec<&str> = new.iter().map(|e| e.role.as_str()).collect();
    if orig_roles != new_roles {
        diffs.push(Diff {
            kind: DiffKind::Order,
            role: String::new(),
            summary: format!(
                "role order differs (original: {}; new: {})",
                orig_roles.join(","),
                new_roles.join(",")
            ),
        });
    }

    // Per-role field differences (status or detail changed), and a role that
    // appears on one side only is a timeout on the side that is missing it.
    for ev in original {
        match new.iter().find(|n| n.role == ev.role) {
            Some(nv) if ev.status != nv.status || ev.detail != nv.detail => diffs.push(Diff {
                kind: DiffKind::Field,
                role: ev.role.clone(),
                summary: format!(
                    "field differs (original: {} {}; new: {} {})",
                    ev.status.label(),
                    ev.detail,
                    nv.status.label(),
                    nv.detail
                ),
            }),
            Some(_) => {}
            None => diffs.push(Diff {
                kind: DiffKind::Timeout,
                role: ev.role.clone(),
                summary: "role present in original but missing in new".to_string(),
            }),
        }
    }
    for ev in new {
        if !original.iter().any(|o| o.role == ev.role) {
            diffs.push(Diff {
                kind: DiffKind::Timeout,
                role: ev.role.clone(),
                summary: "role present in new but missing in original".to_string(),
            });
        }
    }

    // A probe whose detail names a timeout is located as such.
    for ev in original.iter().chain(new) {
        if ev.detail.to_lowercase().contains("timed out") {
            diffs.push(Diff {
                kind: DiffKind::Timeout,
                role: ev.role.clone(),
                summary: format!("probe timed out ({})", ev.detail),
            });
        }
    }

    diffs
}

/// Render the differences as a tab-separated table.
pub fn render_differences(diffs: &[Diff]) -> String {
    let mut out = String::from("kind\trole\tsummary\n");
    for diff in diffs {
        let _ = writeln!(
            out,
            "{}\t{}\t{}",
            diff.kind.label(),
            diff.role,
            diff.summary
        );
    }
    out
}

/// Parse a difference table back into diffs.
pub fn parse_differences(text: &str) -> Vec<Diff> {
    text.lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let kind = match parts.next()? {
                "field" => DiffKind::Field,
                "timeout" => DiffKind::Timeout,
                "order" => DiffKind::Order,
                _ => return None,
            };
            let role = parts.next().unwrap_or("").to_string();
            let summary = parts.next().unwrap_or("").to_string();
            Some(Diff {
                kind,
                role,
                summary,
            })
        })
        .collect()
}

/// The committed differences between the original and new evidence, parsed.
pub fn differences() -> Vec<Diff> {
    parse_differences(EVIDENCE_DIFFERENCES)
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

    /// A replacement report runs the replaced role as the new stack and every
    /// other role as the original, and names the replaced role.
    #[test]
    fn replacement_replaces_one_role_and_names_it() {
        let dir = std::env::temp_dir().join(format!("bw00-full-stack-repl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let orig = dir.join("orig");
        let new = dir.join("new");
        std::fs::create_dir_all(&orig).expect("mkdir orig");
        std::fs::create_dir_all(&new).expect("mkdir new");
        let orig_bin = orig.join("baseapp");
        std::fs::write(&orig_bin, "fake").expect("write orig");
        let new_bin = new.join("boundless-server");
        std::fs::write(&new_bin, "fake").expect("write new");

        let input = vec!["baseapp".to_string(), "cellapp".to_string()];
        let report = replacement(&input, "cellapp", &orig, &new_bin);
        assert_eq!(report.replaced_role, "cellapp");

        let baseapp = report.rows.iter().find(|e| e.role == "baseapp").unwrap();
        assert_eq!(baseapp.side, Side::Original);
        assert_eq!(baseapp.status, Status::Blocked);

        let cellapp = report.rows.iter().find(|e| e.role == "cellapp").unwrap();
        assert_eq!(cellapp.side, Side::New);
        assert_eq!(cellapp.status, Status::Blocked); // fake binary not executable

        let text = render_replacement(&report);
        assert!(text.starts_with("replaced-role\tcellapp\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The committed replacement report names the replaced role, runs that
    /// role as the new stack and keeps every other role as the original.
    #[test]
    fn committed_replacement_report_notes_the_replaced_role() {
        let report = replacement_evidence();
        assert_eq!(report.replaced_role, "cellapp");
        assert_eq!(report.rows.len(), roles().len());

        let replaced = report
            .rows
            .iter()
            .find(|e| e.role == "cellapp")
            .expect("cellapp row");
        assert_eq!(replaced.side, Side::New);
        assert_eq!(replaced.status, Status::Ran);

        for ev in report
            .rows
            .iter()
            .filter(|e| e.role != report.replaced_role)
        {
            assert_eq!(ev.side, Side::Original);
            assert_eq!(ev.status, Status::Blocked);
            assert!(ev.detail.contains("original-full-stack"));
        }
    }

    /// A replacement report round-trips through render and parse.
    #[test]
    fn replacement_render_parse_round_trip() {
        let dir =
            std::env::temp_dir().join(format!("bw00-full-stack-repl-rt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let new_bin = dir.join("boundless-server");
        std::fs::write(&new_bin, "fake").expect("write new");

        let input = roles();
        let report = replacement(&input, "dbapp", &dir, &new_bin);
        let text = render_replacement(&report);
        assert_eq!(parse_replacement(&text), report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A synthetic pair of evidence rows, with a given status and detail.
    fn row(side: Side, role: &str, status: Status, detail: &str) -> Evidence {
        Evidence {
            side,
            role: role.to_string(),
            status,
            detail: detail.to_string(),
        }
    }

    /// An injected field difference is located as a `field` diff for that
    /// role.
    #[test]
    fn injected_field_difference_is_located() {
        let original = vec![row(Side::Original, "baseapp", Status::Ran, "v1")];
        let new = vec![row(Side::New, "baseapp", Status::Ran, "v2")];
        let diffs = diff_reports(&original, &new);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].kind, DiffKind::Field);
        assert_eq!(diffs[0].role, "baseapp");
        assert!(diffs[0].summary.contains("v1") && diffs[0].summary.contains("v2"));
    }

    /// An injected timeout difference is located as a `timeout` diff for that
    /// role.
    #[test]
    fn injected_timeout_difference_is_located() {
        let original = vec![
            row(Side::Original, "baseapp", Status::Ran, "v1"),
            row(Side::Original, "cellapp", Status::Ran, "v1"),
        ];
        let new = vec![row(Side::New, "baseapp", Status::Ran, "v1")];
        let diffs = diff_reports(&original, &new);
        assert!(
            diffs
                .iter()
                .any(|d| d.kind == DiffKind::Timeout && d.role == "cellapp")
        );

        // A detail that names a timeout is located too.
        let timed_out = vec![row(
            Side::Original,
            "baseapp",
            Status::Blocked,
            "probe timed out after 5s",
        )];
        let missing = vec![row(
            Side::New,
            "baseapp",
            Status::Blocked,
            "probe timed out after 5s",
        )];
        let diffs = diff_reports(&timed_out, &missing);
        assert!(
            diffs
                .iter()
                .any(|d| d.kind == DiffKind::Timeout && d.summary.contains("timed out"))
        );
    }

    /// An injected order difference is located as an `order` diff.
    #[test]
    fn injected_order_difference_is_located() {
        let original = vec![
            row(Side::Original, "a", Status::Ran, "v"),
            row(Side::Original, "b", Status::Ran, "v"),
        ];
        let new = vec![
            row(Side::New, "b", Status::Ran, "v"),
            row(Side::New, "a", Status::Ran, "v"),
        ];
        let diffs = diff_reports(&original, &new);
        assert!(diffs.iter().any(|d| d.kind == DiffKind::Order));
    }

    /// The committed differences report locates the status field difference on
    /// every reference role (the original side is blocked, the new side ran).
    #[test]
    fn committed_differences_locate_the_status_gap() {
        let diffs = differences();
        let roles = roles();
        for role in &roles {
            let diff = diffs
                .iter()
                .find(|d| d.role == *role)
                .unwrap_or_else(|| panic!("{role} not located"));
            assert_eq!(diff.kind, DiffKind::Field);
            assert!(diff.summary.contains("blocked"), "{:?}", diff.summary);
            assert!(diff.summary.contains("ran"), "{:?}", diff.summary);
        }
    }

    /// A difference table round-trips through render and parse.
    #[test]
    fn differences_render_parse_round_trip() {
        let diffs = differences();
        assert_eq!(parse_differences(&render_differences(&diffs)), diffs);
    }
}
