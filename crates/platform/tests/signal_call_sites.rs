//! BW-16-02-01 acceptance: the signal syscalls live in
//! `platform::signals` only. This scan walks every workspace source file
//! and fails naming each signal call found outside the landing crate
//! (`libc::sigwait*`, `pthread_sigmask`, `sigaction`, the `sig*set`
//! family, `sigpending`, `pthread_kill`, the `libc::kill`/`raise`/
//! `signal` spellings and the `libc::SIG*` constants), minus the
//! documented transition exceptions below -- the same "report
//! `file:line: kind: detail`" shape as the BW-16-02-05 layering check.

use std::path::{Path, PathBuf};

/// Files still holding signal syscalls while their owning task converts
/// them; every entry names the reason it exists, and an entry whose file
/// has gone signal-free fails the scan so it gets removed.
const TRANSITIONS: &[&str] = &[
    // The reference port of `bw_main` (BW-01-02): `base_on_signalled`,
    // the shutdown probe and their tests keep raw calls until the file's
    // active edits settle; everything else in server-runtime routes
    // through `platform::signals` already.
    "crates/server-runtime/src/server_app.rs",
    // The bw00-generation signal tests (sigwait contract, thread-directed
    // delivery, pthread_kill self-raise) exercise raw syscall semantics that
    // belong to the kernel contract itself; migrating them under
    // `platform::signals` wrappers is pending and tracked with the file's
    // active edits.
    "crates/server-runtime/src/signals.rs",
];

/// Whole-word names that are signal syscalls wherever they appear
/// (`sigwait`/`sigwaitinfo`, the `sig*set` family, `sigaction`, ...).
const CALLS: &[&str] = &[
    "sigwait",
    "sigwaitinfo",
    "sigtimedwait",
    "sigprocmask",
    "pthread_sigmask",
    "sigaction",
    "sigpending",
    "sigemptyset",
    "sigfillset",
    "sigaddset",
    "sigdelset",
    "sigismember",
    "pthread_kill",
];

/// `libc`-prefixed spellings: the generic names only count against
/// `libc::` (a bare `signal`/`kill` word is ordinary Rust elsewhere).
const LIBC_FORMS: &[&str] = &["libc::kill", "libc::raise", "libc::signal", "libc::SIG"];

/// One signal-token hit: where and what.
struct Hit {
    file: PathBuf,
    line: usize,
    text: String,
}

/// Strip comment, string-literal and char-literal spans from a line, so
/// prose that only *names* a call (`///` notes, an assert message like
/// "pthread_sigmask failed", a test name) is not counted as a call site.
fn code_only(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '/' if chars.get(i + 1) == Some(&'/') => break,
            '"' => {
                // Skip to the closing quote, honouring escapes.
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i += 2;
                    } else if chars[i] == '"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            '\'' => {
                // Skip to the closing quote; a lone `'` is a lifetime.
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i += 2;
                    } else if chars[i] == '\'' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Does `text` hold a signal call? Comments and string literals are
/// stripped first: doc prose and assert messages that merely name a call
/// (`"sigwait consumed the arrival"`) are not call sites.
fn is_signal_call(text: &str) -> bool {
    let code = code_only(text);
    let mut words = code.split(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    words.any(|word| CALLS.contains(&word)) || LIBC_FORMS.iter().any(|form| code.contains(form))
}

/// Collect every `.rs` file under `root`, skipping build output.
fn sources(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "target" {
            continue;
        }
        if path.is_dir() {
            sources(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn signal_syscalls_land_in_platform_only() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/platform sits two levels under the workspace root")
        .to_path_buf();

    let mut files = Vec::new();
    for dir in ["crates", "apps"] {
        sources(&workspace.join(dir), &mut files);
    }

    let mut hits = Vec::new();
    let mut unused_transitions: Vec<&str> = TRANSITIONS.to_vec();
    for file in files {
        let relative = file
            .strip_prefix(&workspace)
            .expect("sources come from the workspace tree");
        let relative = relative.to_string_lossy().replace('\\', "/");

        // The landing crate is the allowed surface; this scanner is its
        // own only false positive (the token table lives right here).
        if relative.starts_with("crates/platform/")
            || relative.ends_with("tests/signal_call_sites.rs")
        {
            continue;
        }

        let transitioned = TRANSITIONS.contains(&relative.as_str());
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for (index, line) in text.lines().enumerate() {
            if is_signal_call(line) {
                if transitioned {
                    // A hit under a transition entry keeps it alive.
                    unused_transitions.retain(|entry| *entry != relative);
                } else {
                    hits.push(Hit {
                        file: file.clone(),
                        line: index + 1,
                        text: line.trim().to_string(),
                    });
                }
            }
        }
    }

    let mut report = String::new();
    for hit in &hits {
        report.push_str(&format!(
            "{}:{}: signal-syscall: {}\n",
            hit.file.display(),
            hit.line,
            hit.text
        ));
    }
    for stale in &unused_transitions {
        report.push_str(&format!(
            "{stale}: stale transition entry: the file holds no signal calls\n"
        ));
    }
    assert!(
        hits.is_empty() && unused_transitions.is_empty(),
        "BW-16-02-01: signal syscalls must land in platform::signals only\n{report}"
    );
}
