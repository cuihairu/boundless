//! BW-23-03-01: Chrome Trace Event Format JSON output.
//!
//! Dumps a capture as the reference's JSON artifact — the same shape
//! `Profiler::writeJsonEntries` writes to `<exe>_profile_<timestamp>.json`:
//! a trace-event array of `thread_name` metadata events followed by one event
//! per captured entry, each with `cat`, `pid`, `tid`, `ts` (microseconds
//! since the dump's first entry), `ph` (`B`/`E`/`C`) and `name`; counters
//! carry `args.counter`. A reader of the original format — Chrome's trace
//! viewer, or the WebConsole that downloads these files by that naming
//! schema — consumes this unchanged.

use std::fmt::Write as _;

use crate::capture_session::CapturedEvent;
use foundation::profiler::{Category, EventKind};

/// One thread's name, for the metadata events the reference writes first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadName {
    /// The process the thread ran in.
    pub pid: u32,
    /// The thread index.
    pub tid: u32,
    /// The thread's name.
    pub name: String,
}

/// The reference category strings (`eventCategories_`).
fn category_name(category: Category) -> &'static str {
    match category {
        Category::Cpp => "C++",
        Category::Gpu => "GPU",
        Category::Python => "Python",
    }
}

/// Escape a string for a JSON string literal.
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
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Writes captures as the reference's JSON trace artifact.
pub struct TraceJson {
    /// Clock stamps per microsecond, to convert entry times to `ts`.
    stamps_per_micro: u64,
}

impl Default for TraceJson {
    fn default() -> Self {
        Self::new()
    }
}

impl TraceJson {
    /// A writer assuming the profiler's nanosecond clock (1000 stamps/µs).
    pub fn new() -> Self {
        Self {
            stamps_per_micro: 1000,
        }
    }

    /// Set the clock's stamp rate: `stamps_per_micro` stamps are one µs. A
    /// test clock advancing one stamp per µs passes 1.
    pub fn stamps_per_micro(mut self, stamps_per_micro: u64) -> Self {
        assert!(
            stamps_per_micro > 0,
            "stamps per microsecond must be positive"
        );
        self.stamps_per_micro = stamps_per_micro;
        self
    }

    /// Serialize the capture as a trace-event JSON array, in the reference's
    /// schema. `thread_names` give the `thread_name` metadata events; events
    /// carry their recording process's pid, so a multi-process capture keeps
    /// its processes distinct on the same timeline.
    pub fn write(&self, events: &[CapturedEvent], thread_names: &[ThreadName]) -> String {
        let mut out = String::from("[");
        let mut has_data = false;
        for thread in thread_names {
            if has_data {
                out.push(',');
            }
            let _ = write!(
                out,
                r#"{{"pid":{},"tid":{},"ph":"M","name":"thread_name","args":{{"name":"{}"}}}}"#,
                thread.pid,
                thread.tid,
                escape(&thread.name)
            );
            has_data = true;
        }

        // The dump anchor: the reference subtracts the first entry's time, so
        // `ts` starts at zero and counts up in microseconds.
        let anchor = events
            .iter()
            .map(|event| event.entry.time)
            .min()
            .unwrap_or(0);
        for event in events {
            if has_data {
                out.push(',');
            }
            let ts = (event.entry.time - anchor) / self.stamps_per_micro;
            let ph = match event.entry.kind {
                EventKind::Start | EventKind::StartIdle => "B",
                EventKind::End => "E",
                EventKind::Counter => "C",
            };
            let _ = write!(
                out,
                r#"{{"cat":"{}","pid":{},"tid":{},"ts":{},"ph":"{}","name":"{}""#,
                category_name(event.entry.category),
                event.identity.pid,
                event.entry.thread,
                ts,
                ph,
                escape(&event.entry.name)
            );
            if event.entry.kind == EventKind::Counter {
                let _ = write!(out, r#","args":{{"counter":{}}}}}"#, event.entry.value);
            } else {
                out.push('}');
            }
            has_data = true;
        }
        out.push(']');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_session::CaptureSession;
    use foundation::profiler::Profiler;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    type Pinned = Arc<AtomicU64>;

    fn pinned() -> Pinned {
        Arc::new(AtomicU64::new(0))
    }

    fn profiler_with(source: &Pinned) -> Profiler {
        let source = source.clone();
        Profiler::with_clock(move || source.load(Ordering::SeqCst))
    }

    fn advance(source: &Pinned, by: u64) {
        source.fetch_add(by, Ordering::SeqCst);
    }

    // A strict JSON parser, standing in for the original reader: it accepts
    // exactly the JSON the writer emits and surfaces any malformation.
    #[derive(Debug, Clone, PartialEq)]
    enum Json {
        Null,
        Bool(bool),
        Num(i64),
        Str(String),
        Arr(Vec<Json>),
        Obj(HashMap<String, Json>),
    }

    fn parse(input: &str) -> Json {
        let mut pos = 0usize;
        let value = parse_value(input, &mut pos);
        while input[pos..].chars().next().is_some_and(char::is_whitespace) {
            pos += 1;
        }
        assert_eq!(pos, input.len(), "trailing garbage at {pos}");
        value
    }

    fn parse_value(input: &str, pos: &mut usize) -> Json {
        skip_ws(input, pos);
        match input[*pos..].chars().next().expect("unexpected end") {
            '[' => {
                *pos += 1;
                let mut items = Vec::new();
                skip_ws(input, pos);
                if input[*pos..].starts_with(']') {
                    *pos += 1;
                    return Json::Arr(items);
                }
                loop {
                    items.push(parse_value(input, pos));
                    skip_ws(input, pos);
                    match input[*pos..].chars().next() {
                        Some(',') => *pos += 1,
                        Some(']') => {
                            *pos += 1;
                            return Json::Arr(items);
                        }
                        other => panic!("expected , or ] got {other:?} at {pos}"),
                    }
                }
            }
            '{' => {
                *pos += 1;
                let mut map = HashMap::new();
                skip_ws(input, pos);
                if input[*pos..].starts_with('}') {
                    *pos += 1;
                    return Json::Obj(map);
                }
                loop {
                    skip_ws(input, pos);
                    let key = match parse_value(input, pos) {
                        Json::Str(s) => s,
                        other => panic!("object key not a string: {other:?}"),
                    };
                    skip_ws(input, pos);
                    assert_eq!(
                        input[*pos..].chars().next(),
                        Some(':'),
                        "expected : at {pos}"
                    );
                    *pos += 1;
                    let value = parse_value(input, pos);
                    map.insert(key, value);
                    skip_ws(input, pos);
                    match input[*pos..].chars().next() {
                        Some(',') => *pos += 1,
                        Some('}') => {
                            *pos += 1;
                            return Json::Obj(map);
                        }
                        other => panic!("expected , or }} got {other:?} at {pos}"),
                    }
                }
            }
            '"' => {
                *pos += 1;
                let mut out = String::new();
                loop {
                    match input[*pos..].chars().next().expect("unterminated string") {
                        '"' => {
                            *pos += 1;
                            return Json::Str(out);
                        }
                        '\\' => {
                            *pos += 1;
                            match input[*pos..].chars().next().expect("bad escape") {
                                'n' => out.push('\n'),
                                'r' => out.push('\r'),
                                't' => out.push('\t'),
                                '"' => out.push('"'),
                                '\\' => out.push('\\'),
                                'u' => {
                                    let hex = &input[*pos + 1..*pos + 5];
                                    let code = u32::from_str_radix(hex, 16).expect("bad \\u");
                                    out.push(char::from_u32(code).expect("bad code point"));
                                    *pos += 4;
                                }
                                other => panic!("unknown escape {other:?}"),
                            }
                            *pos += 1;
                        }
                        c => {
                            out.push(c);
                            *pos += 1;
                        }
                    }
                }
            }
            't' => {
                *pos += 4;
                Json::Bool(true)
            }
            'f' => {
                *pos += 5;
                Json::Bool(false)
            }
            'n' => {
                *pos += 4;
                Json::Null
            }
            c if c.is_ascii_digit() || c == '-' => {
                let start = *pos;
                *pos += 1;
                while input[*pos..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
                {
                    *pos += 1;
                }
                let text = &input[start..*pos];
                Json::Num(text.parse().expect("bad number"))
            }
            other => panic!("unexpected token {other:?} at {pos}"),
        }
    }

    fn skip_ws(input: &str, pos: &mut usize) {
        while input[*pos..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            *pos += 1;
        }
    }

    fn field<'a>(obj: &'a HashMap<String, Json>, key: &str) -> &'a Json {
        obj.get(key)
            .unwrap_or_else(|| panic!("missing field {key}"))
    }

    fn as_str(json: &Json) -> &str {
        match json {
            Json::Str(s) => s,
            other => panic!("expected string, got {other:?}"),
        }
    }

    fn as_num(json: &Json) -> i64 {
        match json {
            Json::Num(n) => *n,
            other => panic!("expected number, got {other:?}"),
        }
    }

    fn as_obj(json: &Json) -> &HashMap<String, Json> {
        match json {
            Json::Obj(map) => map,
            other => panic!("expected object, got {other:?}"),
        }
    }

    #[test]
    fn a_representative_sample_is_valid_trace_event_json() {
        let source = pinned();
        let prof = profiler_with(&source);
        prof.set_identity("cellapp01", 100);

        let session = CaptureSession::new(&[&prof]);
        session.start();
        {
            let _s = prof.scoped_category("nativeWork", Category::Cpp);
            advance(&source, 1000); // 1 ms
        }
        {
            let _s = prof.scoped_category("pythonCall", Category::Python);
            advance(&source, 500);
        }
        prof.counter("entities", 42);
        {
            let _s = prof.scoped_category("gpuSubmit", Category::Gpu);
            advance(&source, 500);
        }
        let events = session.stop();

        let json = TraceJson::new().stamps_per_micro(1000).write(
            &events,
            &[ThreadName {
                pid: 100,
                tid: 0,
                name: "MainThread".to_string(),
            }],
        );

        // The reader: parse the array, then follow the trace-event contract.
        let parsed = parse(&json);
        let Json::Arr(items) = parsed else {
            panic!("top-level must be an array");
        };
        assert_eq!(items.len(), 8, "1 metadata + 6 section events + 1 counter");

        // The first event names the thread (metadata, `ph` M).
        let Json::Obj(first) = &items[0] else {
            panic!("event must be an object");
        };
        assert_eq!(as_str(field(first, "ph")), "M");
        assert_eq!(as_str(field(first, "name")), "thread_name");
        assert_eq!(
            as_str(field(as_obj(field(first, "args")), "name")),
            "MainThread"
        );

        // The entries carry the fields a trace viewer reads, in the
        // reference's schema, with microsecond timestamps from zero. The
        // phases pair B/E per thread and the counter is a C.
        let mut ts_seen = Vec::new();
        for (index, item) in items.iter().enumerate().skip(1) {
            let Json::Obj(event) = item else {
                panic!("entry {index} must be an object");
            };
            let cat = as_str(field(event, "cat"));
            assert!(
                matches!(cat, "C++" | "GPU" | "Python"),
                "entry {index} has bad category {cat}"
            );
            assert_eq!(as_num(field(event, "pid")), 100);
            assert_eq!(as_num(field(event, "tid")), 0);
            let ph = as_str(field(event, "ph"));
            assert!(
                matches!(ph, "B" | "E" | "C"),
                "entry {index} has bad phase {ph}"
            );
            if ph != "C" {
                assert!(
                    matches!(
                        as_str(field(event, "name")),
                        "nativeWork" | "pythonCall" | "gpuSubmit"
                    ),
                    "entry {index} has an unexpected name"
                );
            }
            ts_seen.push(as_num(field(event, "ts")));
        }
        // Timestamps never go backwards (the reader's ordering assumption).
        assert!(ts_seen.windows(2).all(|w| w[0] <= w[1]), "{ts_seen:?}");

        // The counter carries its value in args.counter, like the reference.
        let counter = items
            .iter()
            .find(|item| matches!(item, Json::Obj(o) if as_str(field(o, "ph")) == "C"));
        let Json::Obj(counter) = counter.expect("a counter event") else {
            unreachable!()
        };
        assert_eq!(as_num(field(as_obj(field(counter, "args")), "counter")), 42);

        // Timestamps count microseconds from the first entry's time.
        let Json::Obj(first_entry) = &items[1] else {
            unreachable!()
        };
        assert_eq!(as_num(field(first_entry, "ts")), 0);
        let Json::Obj(last_entry) = &items[items.len() - 1] else {
            unreachable!()
        };
        assert_eq!(as_num(field(last_entry, "ts")), 2, "2 ms of captured work");
    }

    #[test]
    fn a_counter_entry_without_a_section_still_serializes() {
        let source = pinned();
        let prof = profiler_with(&source);
        let session = CaptureSession::new(&[&prof]);
        session.start();
        prof.counter("level", 7);
        let json = TraceJson::new()
            .stamps_per_micro(1)
            .write(&session.stop(), &[]);

        let Json::Arr(items) = parse(&json) else {
            unreachable!()
        };
        assert_eq!(items.len(), 1);
        let Json::Obj(event) = &items[0] else {
            unreachable!()
        };
        assert_eq!(as_str(field(event, "ph")), "C");
        assert_eq!(
            as_num(as_obj(field(event, "args")).get("counter").unwrap()),
            7
        );
    }

    #[test]
    fn the_reference_category_strings_are_preserved() {
        let source = pinned();
        let prof = profiler_with(&source);
        let session = CaptureSession::new(&[&prof]);
        session.start();
        {
            let _s = prof.scoped_category("native", Category::Cpp);
            advance(&source, 1);
        }
        {
            let _s = prof.scoped_category("gpu", Category::Gpu);
            advance(&source, 1);
        }
        {
            let _s = prof.scoped_category("py", Category::Python);
            advance(&source, 1);
        }
        let json = TraceJson::new()
            .stamps_per_micro(1)
            .write(&session.stop(), &[]);

        let Json::Arr(items) = parse(&json) else {
            unreachable!()
        };
        let mut cats: Vec<&str> = items
            .iter()
            .filter_map(|item| match item {
                Json::Obj(o) if as_str(field(o, "ph")) != "C" => Some(as_str(field(o, "cat"))),
                _ => None,
            })
            .collect();
        cats.sort_unstable();
        cats.dedup();
        assert_eq!(cats, vec!["C++", "GPU", "Python"]);
    }
}
