//! Protocol binary fixture catalog (BW-00-04-01).
//!
//! Records, per interface and per message, the message id the reference
//! assigns (the declaration index, `id = elements_.size()` in
//! `InterfaceMinder::add`), the message name, the declaring macro, the
//! length style and parameter, and the raw frame bytes that identify the
//! message on the wire.
//!
//! The interfaces are the real ones from the reference source tree, curated
//! to the logon and entity paths: the baseapp internal and external
//! interfaces, the cellapp interface (the entity protocol) and the baseapp
//! manager interface. Each header is scanned in declaration order, which is
//! exactly the order the reference's `InterfaceMinder` assigns ids in.
//!
//! The committed catalog (`cases/protocol_fixtures.txt`) is regenerated with
//! the `scan-protocol-fixtures` binary and re-parsed by the catalog test, so
//! the file and the scanner cannot drift.
//!
//! The frames are the reference's own canonical encodings for a minimal
//! (empty body) message, derived from its interface tables and framing rules:
//! a fixed message is `[id]`, a variable message is `[id][length field of
//! `param` zero bytes]`. A request that carries a reply adds six preamble
//! bytes (`replyId:4`, `nextRequest:2`) after the length field; the body is
//! the args' own encoding, so the recorded bytes are the message identifier
//! every such frame starts with. A live over-the-wire capture is blocked (see
//! `blocker.protocol-capture`).
//!
//! Catalog line grammar (tab-separated `item`, `value`, `source`):
//!
//! * `reference-commit` — the reference checkout's git HEAD.
//! * `interface.<n>.name` / `.direction` / `.source` / `.messages` — one
//!   indexed row per scanned interface.
//! * `msg.<n>.<i>.id` / `.name` / `.kind` / `.length-style` / `.length-param`
//!   / `.frame` — one indexed row per message, ids in declaration order.
//! * `blocker.protocol-capture` — why no live capture is recorded.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The committed catalog, embedded so tests and the CLI never touch the
/// reference checkout at run time.
const CATALOG_TEXT: &str = include_str!("../cases/protocol_fixtures.txt");

/// The scanned interfaces: name, direction, reference source header (relative
/// to the reference root, under `programming/bigworld/`).
const INTERFACES: &[(&str, &str, &str)] = &[
    (
        "BaseAppIntInterface",
        "internal",
        "programming/bigworld/server/baseapp/baseapp_int_interface.hpp",
    ),
    (
        "BaseAppExtInterface",
        "external",
        "programming/bigworld/lib/connection/baseapp_ext_interface.hpp",
    ),
    (
        "CellAppInterface",
        "internal",
        "programming/bigworld/server/cellapp/cellapp_interface.hpp",
    ),
    (
        "BaseAppMgrInterface",
        "internal",
        "programming/bigworld/server/baseappmgr/baseappmgr_interface.hpp",
    ),
];

/// The named blocker: no live over-the-wire capture is recorded, because the
/// cluster cannot start here (see BW-00-02-03) and `loginapp.pubkey` is absent
/// (see BW-00-02-04). The recorded frames are the reference's own canonical
/// message encodings from its interface tables and framing rules.
pub const CAPTURE_BLOCKER: &str = "protocol-capture: no live over-the-wire capture is recorded — the full cluster cannot start on this machine (bwmachined and the full server configuration are absent, BW-00-02-03) and loginapp.pubkey is absent repo-wide (BW-00-02-04), so no original process emits a frame that could be captured; the recorded bytes are the reference's own canonical message encodings (message id byte plus length field) from its interface tables and framing rules, which identify every frame carrying that message";

/// One message declaration, as scanned from an interface header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The id the reference assigns: the declaration index.
    pub id: u8,
    /// The message name (the `NAME` the macro declares).
    pub name: String,
    /// The declaring macro (`BW_BEGIN_STRUCT_MSG_EX`, ...).
    pub kind: String,
    /// `fixed` or `variable` (`FIXED_LENGTH_MESSAGE` / `VARIABLE_LENGTH_MESSAGE`).
    pub length_style: String,
    /// The length parameter as the source gives it (`0`, `2`, `4`,
    /// `sizeof(nameArgs)`, ...).
    pub length_param: String,
    /// The raw frame bytes for a minimal (empty body) non-request instance,
    /// as hex.
    pub frame: String,
}

/// One scanned interface: its name, direction, source header and messages in
/// declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// The interface name (`BaseAppIntInterface`, ...).
    pub name: String,
    /// `internal` (server-to-server) or `external` (client-to-server).
    pub direction: String,
    /// The header path in the reference checkout.
    pub source: String,
    /// The messages, ids in declaration order.
    pub messages: Vec<Message>,
}

/// The frame width of a variable-length message.
fn width(param: &str) -> u8 {
    param.parse::<u8>().unwrap_or(2)
}

/// The canonical frame bytes for a minimal non-request instance.
fn frame_bytes(id: u8, length_style: &str, length_param: &str) -> String {
    let width = usize::from(width(length_param));
    let mut bytes = vec![
        0u8;
        if length_style == "variable" {
            width + 1
        } else {
            1
        }
    ];
    bytes[0] = id;
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Classify one declaration line into a message template, or `None` when the
/// line is not a message declaration.
///
/// The `kind` is the macro name, and the length style and parameter follow
/// from the reference's macro expansions (`common_interface_macros.hpp`,
/// `interface_macros.hpp`):
///
/// * `BW_EMPTY_MSG*` / `MF_EMPTY_*_PROXY_MSG` / `MF_EMPTY_*_ENTITY_MSG` —
///   `MERCURY_EMPTY_MESSAGE` → `FIXED_LENGTH_MESSAGE` param `0`.
/// * `BW_BEGIN_STRUCT_MSG*` / `MF_BEGIN_*_PROXY_MSG` / `MF_BEGIN_BASE_STRUCT_MSG_EX`
///   — `MERCURY_STRUCT_MESSAGE` → `FIXED_LENGTH_MESSAGE` param `sizeof(Args)`.
/// * `MF_BEGIN_ENTITY_MSG*` — `MERCURY_PREFIXED_MESSAGE` →
///   `FIXED_LENGTH_MESSAGE` param `sizeof(EntityID)+sizeof(Args)`.
/// * `MF_EMPTY_ENTITY_MSG` — `MERCURY_PREFIXED_EMPTY_MESSAGE` →
///   `FIXED_LENGTH_MESSAGE` param `sizeof(EntityID)`.
/// * `BW_STREAM_MSG*` / `BW_BIG_STREAM_MSG*` / `MF_BASE_STREAM_MSG*` /
///   `MF_VARLEN_*` / `MF_METHOD_RANGE_MSG` / `MF_PROPERTY_RANGE_MSG` /
///   `MF_REVIVER_PING_MSG` / `MERCURY_*_VARIABLE_MESSAGE` —
///   `VARIABLE_LENGTH_MESSAGE` param `2` (or `4` for the big-stream variant).
fn classify(code: &str) -> Option<MessageTemplate> {
    let token = code.split_whitespace().next()?;
    let kind = token.trim_end_matches('(').trim_end().to_string();
    let open = code.find('(')?;
    let args = code[open + 1..].split(')').next().unwrap_or("").trim();
    let arg = |n: usize| {
        args.split(',')
            .nth(n)
            .map(|s| s.trim())
            .unwrap_or("")
            .to_string()
    };

    // (name position, length style, length parameter source)
    //
    // The name position differs: `BW_*_MSG( TYPE, NAME )` declares NAME as
    // the second argument, while the `MF_*` entity/proxy/manager macros
    // declare NAME as the first.
    let (name_pos, style, param): (usize, &str, ParamKind) = match kind.as_str() {
        // Fixed, param 0 (empty); NAME as second arg.
        "BW_EMPTY_MSG" | "BW_EMPTY_MSG_EX" => (2, "fixed", ParamKind::Zero),
        // Fixed, param 0 (empty); NAME as first arg.
        "MF_EMPTY_PROXY_MSG"
        | "MF_EMPTY_BLOCKABLE_PROXY_MSG"
        | "MF_EMPTY_UNBLOCKABLE_PROXY_MSG" => (1, "fixed", ParamKind::Zero),
        // Fixed, param sizeof(Args) (struct); NAME as second arg.
        "BW_BEGIN_STRUCT_MSG" | "BW_BEGIN_STRUCT_MSG_EX" => (2, "fixed", ParamKind::Args),
        // Fixed, param sizeof(Args) (struct); NAME as first arg.
        "MF_BEGIN_PROXY_MSG"
        | "MF_BEGIN_UNBLOCKABLE_PROXY_MSG"
        | "MF_BEGIN_BLOCKABLE_PROXY_MSG"
        | "MF_BEGIN_BASE_STRUCT_MSG_EX"
        | "MF_BEGIN_BASE_STRUCT_MSG" => (1, "fixed", ParamKind::Args),
        // Fixed, param sizeof(EntityID)+sizeof(Args) (prefixed struct).
        "MF_BEGIN_ENTITY_MSG" | "MF_BEGIN_ENTITY_MSG_WITH_FAILURE" => {
            (1, "fixed", ParamKind::ArgsPrefixed)
        }
        // Fixed, param sizeof(EntityID) (prefixed empty).
        "MF_EMPTY_ENTITY_MSG" => (1, "fixed", ParamKind::EntityId),
        // Variable, param 2; NAME as second arg.
        "BW_STREAM_MSG" | "BW_STREAM_MSG_EX" | "MF_BUFFERED_STREAM_MESSAGE_EX" => {
            (2, "variable", ParamKind::Width(2))
        }
        // Variable, param 2; NAME as first arg.
        "MF_BASE_STREAM_MSG"
        | "MF_BASE_STREAM_MSG_EX"
        | "MF_VARLEN_PROXY_MSG"
        | "MF_VARLEN_BLOCKABLE_PROXY_MSG"
        | "MF_VARLEN_UNBLOCKABLE_PROXY_MSG"
        | "MF_VARLEN_ENTITY_MSG"
        | "MF_RAW_VARLEN_ENTITY_MSG"
        | "MF_VARLEN_ENTITY_REQUEST"
        | "MF_CREATE_ENTITY_MSG"
        | "MF_METHOD_RANGE_MSG"
        | "MF_PROPERTY_RANGE_MSG" => (1, "variable", ParamKind::Width(2)),
        // Variable, param 4 (big stream).
        "BW_BIG_STREAM_MSG" | "BW_BIG_STREAM_MSG_EX" => (2, "variable", ParamKind::Width(4)),
        // The reviver ping expands to `MERCURY_VARIABLE_MESSAGE( reviverPing, 2, ... )`.
        "MF_REVIVER_PING_MSG" => {
            return Some(MessageTemplate {
                name: "reviverPing".to_string(),
                kind,
                length_style: "variable".to_string(),
                length_param: "2".to_string(),
            });
        }
        // Direct `MERCURY_HANDLED_VARIABLE_MESSAGE( NAME, PARAM, ... )`.
        "MERCURY_HANDLED_VARIABLE_MESSAGE" | "MERCURY_VARIABLE_MESSAGE" => {
            let param = arg(1);
            return Some(MessageTemplate {
                name: arg(0),
                kind,
                length_style: "variable".to_string(),
                length_param: if param.is_empty() {
                    "2".to_string()
                } else {
                    param
                },
            });
        }
        _ => return None,
    };

    let name = arg(name_pos - 1);
    if name.is_empty() {
        return None;
    }
    Some(MessageTemplate {
        name: name.clone(),
        kind,
        length_style: style.to_string(),
        length_param: param.literal(&name),
    })
}

/// The kind of length parameter a macro expands to.
enum ParamKind {
    /// `0`.
    Zero,
    /// `sizeof(NAMEArgs)`.
    Args,
    /// `sizeof(EntityID)+sizeof(NAMEArgs)`.
    ArgsPrefixed,
    /// `sizeof(EntityID)`.
    EntityId,
    /// A literal width (`2`, `4`).
    Width(u8),
}

impl ParamKind {
    fn literal(&self, name: &str) -> String {
        match self {
            Self::Zero => "0".to_string(),
            Self::Args => format!("sizeof({name}Args)"),
            Self::ArgsPrefixed => format!("sizeof(EntityID)+sizeof({name}Args)"),
            Self::EntityId => "sizeof(EntityID)".to_string(),
            Self::Width(w) => w.to_string(),
        }
    }
}

/// A classified declaration, before the id is assigned.
struct MessageTemplate {
    name: String,
    kind: String,
    length_style: String,
    length_param: String,
}

/// The name inside `BEGIN_MERCURY_INTERFACE( NAME )`.
fn extract_interface_name(code: &str) -> Option<String> {
    let open = code.find('(')?;
    let rest = &code[open + 1..];
    let close = rest.find(')')?;
    Some(rest[..close].trim().to_string())
}

/// Parse one interface header into its name and messages, in declaration
/// order. Preprocessor `#if/#else/#endif` blocks take their first branch
/// (the reference's message sets do not change between the branches, only
/// the arg fields do), and `#define` bodies are skipped so the macro
/// expansion text is not mistaken for a declaration.
fn parse_interface(text: &str) -> (String, Vec<Message>) {
    let mut interface_name = String::new();
    let mut in_interface = false;
    let mut cond: Vec<bool> = Vec::new();
    let mut in_define = false;
    let mut messages: Vec<Message> = Vec::new();

    for raw in text.lines() {
        if in_define {
            if !raw.trim_end().ends_with('\\') {
                in_define = false;
            }
            continue;
        }
        let trimmed = raw.trim();
        if trimmed.starts_with('#') {
            let directive = trimmed.trim_start_matches('#').trim_start();
            if directive.starts_with("define") {
                // Only a backslash-continued `#define` spans the next line; a
                // single-line define (e.g. the `AVATAR_UPDATE_*_FLAG` inside a
                // `#if` block) must not swallow the declaration that follows.
                in_define = raw.trim_end().ends_with('\\');
                continue;
            }
            if directive.starts_with("if")
                || directive.starts_with("ifdef")
                || directive.starts_with("ifndef")
            {
                cond.push(true);
                continue;
            }
            if directive.starts_with("elif") {
                if let Some(top) = cond.last_mut() {
                    *top = false;
                }
                continue;
            }
            if directive.starts_with("else") {
                if let Some(top) = cond.last_mut() {
                    *top = false;
                }
                continue;
            }
            if directive.starts_with("endif") {
                cond.pop();
                continue;
            }
            continue;
        }

        let code = raw.split("//").next().unwrap_or("").trim();
        if code.is_empty() {
            continue;
        }
        if code.contains("BEGIN_MERCURY_INTERFACE(") {
            in_interface = true;
            interface_name = extract_interface_name(code).unwrap_or_default();
            continue;
        }
        if code.contains("END_MERCURY_INTERFACE(") {
            in_interface = false;
            continue;
        }
        if !in_interface || !cond.iter().all(|&b| b) {
            continue;
        }
        if code.starts_with("MERCURY_ISTREAM") || code.starts_with("MERCURY_OSTREAM") {
            continue;
        }
        if code.starts_with("BW_BEGIN_NAMESPACE") || code.starts_with("BW_END_NAMESPACE") {
            continue;
        }
        if let Some(t) = classify(code) {
            let id = messages.len() as u8;
            messages.push(Message {
                id,
                name: t.name,
                kind: t.kind,
                length_style: t.length_style,
                frame: String::new(),
                length_param: t.length_param,
            });
            let msg = messages.last_mut().expect("just pushed");
            msg.frame = frame_bytes(id, &msg.length_style, &msg.length_param);
        }
    }

    (interface_name, messages)
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

/// Scan a reference checkout's protocol binary fixtures.
pub fn scan_reference(root: &Path) -> Vec<ProtocolFact> {
    let mut facts = Vec::new();

    facts.push(ProtocolFact {
        item: "reference-commit".to_string(),
        value: git_head(root),
        source: "git rev-parse HEAD".to_string(),
    });

    for (n, (name, direction, source)) in INTERFACES.iter().enumerate() {
        let idx = n + 1;
        let path = root.join(source);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("cannot read reference header {path:?}"));
        let (parsed_name, messages) = parse_interface(&text);
        assert_eq!(
            &parsed_name, name,
            "interface {idx} name mismatch in {source}"
        );

        facts.push(ProtocolFact {
            item: format!("interface.{idx}.name"),
            value: name.to_string(),
            source: source.to_string(),
        });
        facts.push(ProtocolFact {
            item: format!("interface.{idx}.direction"),
            value: direction.to_string(),
            source: "server-to-server (internal nub) or client-to-server (external)".to_string(),
        });
        facts.push(ProtocolFact {
            item: format!("interface.{idx}.source"),
            value: source.to_string(),
            source: "reference source tree".to_string(),
        });
        facts.push(ProtocolFact {
            item: format!("interface.{idx}.messages"),
            value: messages.len().to_string(),
            source: "declaration order".to_string(),
        });

        for (i, msg) in messages.iter().enumerate() {
            let mi = i + 1;
            for (suffix, value, msg_source) in [
                (
                    "id",
                    msg.id.to_string(),
                    "declaration index (InterfaceMinder::add)".to_string(),
                ),
                ("name", msg.name.clone(), source.to_string()),
                ("kind", msg.kind.clone(), "declaring macro".to_string()),
                (
                    "length-style",
                    msg.length_style.clone(),
                    "FIXED_LENGTH_MESSAGE / VARIABLE_LENGTH_MESSAGE".to_string(),
                ),
                (
                    "length-param",
                    msg.length_param.clone(),
                    "macro expansion".to_string(),
                ),
                (
                    "frame",
                    msg.frame.clone(),
                    "canonical minimal frame (hex)".to_string(),
                ),
            ] {
                facts.push(ProtocolFact {
                    item: format!("msg.{idx}.{mi}.{suffix}"),
                    value,
                    source: msg_source,
                });
            }
        }
    }

    facts.push(ProtocolFact {
        item: "blocker.protocol-capture".to_string(),
        value: CAPTURE_BLOCKER.to_string(),
        source: "runner limitation".to_string(),
    });

    facts
}

/// One protocol-fact row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolFact {
    /// The fact name (`reference-commit`, `msg.1.1.id`, ...).
    pub item: String,
    /// The recorded value.
    pub value: String,
    /// The file or probe the value came from.
    pub source: String,
}

/// Render the scan into the tab-separated catalog text.
pub fn render(facts: &[ProtocolFact]) -> String {
    let mut out = String::new();
    out.push_str("item\tvalue\tsource\n");
    for f in facts {
        let _ = writeln!(out, "{}\t{}\t{}", f.item, f.value, f.source);
    }
    out
}

/// Parse the catalog text back into [`ProtocolFact`]s.
pub fn parse_catalog(text: &str) -> Vec<ProtocolFact> {
    let mut lines = text.lines();
    let _ = lines.next();
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let mut f = line.split('\t');
        out.push(ProtocolFact {
            item: f.next().unwrap_or("").to_string(),
            value: f.next().unwrap_or("-").to_string(),
            source: f.next().unwrap_or("-").to_string(),
        });
    }
    out
}

/// The committed catalog as parsed records.
pub fn catalog() -> Vec<ProtocolFact> {
    parse_catalog(CATALOG_TEXT)
}

/// The committed interfaces and their messages, in indexed order.
pub fn interfaces() -> Vec<Interface> {
    let facts = catalog();
    let get = |item: &str| -> String {
        facts
            .iter()
            .find(|f| f.item == item)
            .map(|f| f.value.clone())
            .unwrap_or_default()
    };
    let count = facts
        .iter()
        .filter(|f| f.item.ends_with(".name") && f.item.starts_with("interface."))
        .count();
    (1..=count)
        .map(|i| {
            let source = get(&format!("interface.{i}.source"));
            let msgs: usize = get(&format!("interface.{i}.messages")).parse().unwrap_or(0);
            let messages = (1..=msgs)
                .map(|m| Message {
                    id: get(&format!("msg.{i}.{m}.id")).parse().unwrap_or(0),
                    name: get(&format!("msg.{i}.{m}.name")),
                    kind: get(&format!("msg.{i}.{m}.kind")),
                    length_style: get(&format!("msg.{i}.{m}.length-style")),
                    length_param: get(&format!("msg.{i}.{m}.length-param")),
                    frame: get(&format!("msg.{i}.{m}.frame")),
                })
                .collect();
            Interface {
                name: get(&format!("interface.{i}.name")),
                direction: get(&format!("interface.{i}.direction")),
                source,
                messages,
            }
        })
        .collect()
}

/// The committed named blocker for the protocol capture.
pub fn blocker() -> String {
    catalog()
        .into_iter()
        .find(|f| f.item == "blocker.protocol-capture")
        .map(|f| f.value)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed catalog records each interface's messages with ids in
    /// declaration order, the direction, and a frame for every message.
    #[test]
    fn committed_catalog_records_interfaces_and_messages() {
        let interfaces = interfaces();
        assert_eq!(
            interfaces.len(),
            INTERFACES.len(),
            "one per curated interface"
        );

        let baseapp_int = interfaces
            .iter()
            .find(|i| i.name == "BaseAppIntInterface")
            .expect("baseapp int");
        assert_eq!(baseapp_int.direction, "internal");
        assert_eq!(
            baseapp_int.source,
            "programming/bigworld/server/baseapp/baseapp_int_interface.hpp"
        );
        assert!(baseapp_int.messages.len() > 40, "baseapp int is large");

        let baseapp_ext = interfaces
            .iter()
            .find(|i| i.name == "BaseAppExtInterface")
            .expect("baseapp ext");
        assert_eq!(baseapp_ext.direction, "external");

        let cellapp = interfaces
            .iter()
            .find(|i| i.name == "CellAppInterface")
            .expect("cellapp");
        assert_eq!(cellapp.direction, "internal");
        assert!(cellapp.messages.len() > 40, "cellapp interface is large");

        let mgr = interfaces
            .iter()
            .find(|i| i.name == "BaseAppMgrInterface")
            .expect("baseappmgr");
        assert_eq!(mgr.direction, "internal");

        // Every message has an id equal to its index, a name, and a frame.
        for i in &interfaces {
            for (idx, m) in i.messages.iter().enumerate() {
                assert_eq!(m.id as usize, idx, "{}:{}", i.name, m.name);
                assert!(!m.name.is_empty(), "{}:{} has no name", i.name, m.name);
                assert!(!m.kind.is_empty(), "{}:{} has no kind", i.name, m.name);
                assert!(!m.length_style.is_empty(), "{}:{}", i.name, m.name);
                assert!(!m.length_param.is_empty(), "{}:{}", i.name, m.name);
                assert!(!m.frame.is_empty(), "{}:{}", i.name, m.name);
            }
        }

        let blocker = blocker();
        assert!(blocker.contains("protocol-capture"), "{blocker}");
        assert!(blocker.contains("BW-00-02-03"), "{blocker}");
        assert!(blocker.contains("BW-00-02-04"), "{blocker}");
    }

    /// The first message of the baseapp internal interface is the known
    /// `createBaseWithCellData` (id 0), and the fixed/stream split is as the
    /// reference declares it.
    #[test]
    fn known_message_ids_match_the_reference() {
        let baseapp_int = interfaces()
            .into_iter()
            .find(|i| i.name == "BaseAppIntInterface")
            .expect("baseapp int");
        assert_eq!(baseapp_int.messages[0].id, 0);
        assert_eq!(baseapp_int.messages[0].name, "createBaseWithCellData");
        assert_eq!(baseapp_int.messages[0].kind, "BW_STREAM_MSG_EX");
        assert_eq!(baseapp_int.messages[0].length_style, "variable");
        assert_eq!(baseapp_int.messages[0].length_param, "2");
        assert_eq!(baseapp_int.messages[0].frame, "000000");

        // logOnAttempt is a stream message in the same interface.
        let logon = baseapp_int
            .messages
            .iter()
            .find(|m| m.name == "logOnAttempt")
            .expect("logOnAttempt");
        assert_eq!(logon.kind, "BW_STREAM_MSG_EX");
        assert_eq!(logon.frame, format!("{:02x}0000", logon.id));
    }

    /// A fixed struct message carries only the id byte in its minimal frame.
    #[test]
    fn fixed_struct_frame_is_the_id_byte() {
        let baseapp_int = interfaces()
            .into_iter()
            .find(|i| i.name == "BaseAppIntInterface")
            .expect("baseapp int");
        let startup = baseapp_int
            .messages
            .iter()
            .find(|m| m.name == "startup")
            .expect("startup");
        assert_eq!(startup.kind, "BW_BEGIN_STRUCT_MSG");
        assert_eq!(startup.length_style, "fixed");
        assert!(startup.length_param.starts_with("sizeof(startupArgs)"));
        assert_eq!(startup.frame, format!("{:02x}", startup.id));
    }

    /// The catalog round-trips through render and parse.
    #[test]
    fn render_parse_round_trip() {
        assert_eq!(parse_catalog(&render(&catalog())), catalog());
    }

    /// The scanner and the committed catalog cannot drift: re-scanning a real
    /// reference checkout reproduces the embedded text. Skipped in CI.
    #[test]
    fn rescanning_reference_reproduces_catalog() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("BigWorld");
        let first = root.join(INTERFACES[0].2);
        if !first.is_file() {
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
