//! Log fields and queries (`server/tools/message_logger/query_params.cpp`,
//! `py_query.cpp`, `query_range.cpp`): the `QueryParams` validators, the
//! `getResultForEntry` filter pipeline in its exact reference order, and the
//! direction-aware walk that decides which entries a query visits.
//!
//! The storage behind this module is in-memory: the entry records, format
//! strings, component table and category table a user log would expose.
//! File and segment formats are ported separately.
//!
//! One documented divergence: the reference compiles POSIX `REG_EXTENDED`
//! patterns; this port uses the `regex` crate, so pattern syntax follows
//! Rust regex (case insensitivity via the builder replaces `REG_ICASE`, and
//! `REG_NEWLINE`'s line-anchored `.` is not reproduced).

use std::collections::{BTreeMap, BTreeSet};

use crate::entry::{LOG_BEGIN, LOG_END, LogEntry, LogTime};
use crate::interpolator::{LogStringInterpolator, stream_to_string};
use crate::stream::MemoryIStream;
use crate::{MessageSource, message_priority};

/// `SearchDirection` (`constants.hpp:58-59`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchDirection {
    /// Oldest to newest.
    Forwards,
    /// Newest to oldest — results come back in reverse order.
    Backwards,
}

/// `DONT_INTERPOLATE`/`POST_INTERPOLATE`/`PRE_INTERPOLATE`
/// (`constants.hpp:65-67`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum InterpolateMode {
    /// Filter on and report the raw format string.
    #[default]
    DontInterpolate,
    /// Interpolate after filtering (regexes match the raw format string).
    PostInterpolate,
    /// Interpolate before filtering (regexes match the final message).
    PreInterpolate,
}

/// The per-component details queries need (`LoggingComponent`): the
/// identity a registered process had in the logs.
#[derive(Clone, Debug, Default)]
pub struct LoggingComponent {
    /// The component's IP address, as a host-order `uint32`.
    pub ip: u32,
    /// The resolved hostname (`Hostnames::getHostByAddr`).
    pub host: String,
    /// The OS process ID.
    pub pid: u32,
    /// `AppInstanceID` — zero when the component never announced one.
    pub app_instance_id: u32,
    /// `AppTypeID` — the process-type bit tested by the process filter.
    pub app_type_id: u32,
    /// The Linux username that owned the process.
    pub username: String,
    /// The component name, e.g. `cellapp01`.
    pub name: String,
}

/// One stored log line: the fixed entry record plus the pieces that live
/// beside it in the log (the argument blob and the metadata string).
#[derive(Clone, Debug, Default)]
pub struct StoredLogLine {
    /// The fixed-length entry record.
    pub entry: LogEntry,
    /// The encoded format arguments (`argsOffset`/`argsLen` point here).
    pub args: Vec<u8>,
    /// The metadata string (empty for versions below 8).
    pub metadata: String,
}

/// The in-memory stand-in for a `UserLogReader`: the tables a query walks.
#[derive(Clone, Debug, Default)]
pub struct QueryableLog {
    /// `userComponentID` → component details.
    pub components: BTreeMap<u32, LoggingComponent>,
    /// `stringOffset` → the parsed format string.
    pub format_strings: BTreeMap<u32, LogStringInterpolator>,
    /// `categoryID` → category name.
    pub categories: BTreeMap<u16, String>,
    /// The entries in log order (ascending time, as written).
    pub entries: Vec<StoredLogLine>,
}

impl QueryableLog {
    /// Register a component and return its `userComponentID`.
    pub fn add_component(&mut self, id: u32, component: LoggingComponent) {
        self.components.insert(id, component);
    }

    /// Register a format string and return its offset.
    pub fn add_format_string(&mut self, offset: u32, format: &str) {
        self.format_strings
            .insert(offset, LogStringInterpolator::new(format));
    }

    /// Register a category name under the next free ID
    /// (`Categories::resolveOrCreateNameToID`).
    pub fn add_category(&mut self, name: &str) -> u16 {
        if let Some((id, _)) = self.categories.iter().find(|(_, n)| n.as_str() == name) {
            return *id;
        }
        let id = self
            .categories
            .last_key_value()
            .map(|(id, _)| id + 1)
            .unwrap_or(1);
        self.categories.insert(id, name.to_string());
        id
    }

    /// Append one entry in log order.
    pub fn push_entry(&mut self, line: StoredLogLine) {
        self.entries.push(line);
    }
}

/// A query's parameters (`QueryParams`), with the reference defaults:
/// the full log period, no address/PID/app filter, all severities and
/// process types, both message sources, no category filter, no regexes,
/// pre-interpolation, case-sensitive patterns, forwards.
#[derive(Clone, Debug)]
pub struct QueryParams {
    /// The user ID filter (`getUID`/`setUID` live on the log, not here).
    pub uid: u16,
    /// The period start, as a floating-point timestamp.
    pub start: f64,
    /// The period end; `LOG_END` runs to positive infinity.
    pub end: f64,
    /// The host IP filter; zero means unset.
    pub ip_address: u32,
    /// Invert the host match.
    pub negate_host: bool,
    /// The PID filter; zero means unset.
    pub pid: u32,
    /// Invert the PID match.
    pub negate_pid: bool,
    /// The app instance ID filter; zero means unset.
    pub appid: u32,
    /// Invert the app ID match.
    pub negate_appid: bool,
    /// The process-type bit mask; -1 means all.
    pub procs: i64,
    /// The severity bit mask; -1 means all.
    pub severities: i64,
    /// The message-source bit mask (`1 << source`).
    pub log_source_type_flags: u32,
    /// The category specifier names, resolved against the log at query time.
    pub category_names: Vec<String>,
    /// The include regex; `None` passes everything.
    pub include: Option<regex::Regex>,
    /// The exclude regex; `None` passes everything.
    pub exclude: Option<regex::Regex>,
    /// When to interpolate the arguments.
    pub interpolate: InterpolateMode,
    /// Whether patterns are case-sensitive.
    pub case_sensitive: bool,
    /// Which way the query walks.
    pub direction: SearchDirection,
    /// Lines of context around each result.
    pub num_lines_of_context: i32,
}

impl Default for QueryParams {
    fn default() -> Self {
        Self {
            uid: 0,
            start: LOG_BEGIN,
            end: LOG_END,
            ip_address: 0,
            negate_host: false,
            pid: 0,
            negate_pid: false,
            appid: 0,
            negate_appid: false,
            procs: -1,
            severities: -1,
            log_source_type_flags: (1 << 2) - 1, // NUM_MESSAGE_SOURCE
            category_names: Vec::new(),
            include: None,
            exclude: None,
            interpolate: InterpolateMode::PreInterpolate,
            case_sensitive: true,
            direction: SearchDirection::Forwards,
            num_lines_of_context: 0,
        }
    }
}

impl QueryParams {
    /// `initRegex`: compile both patterns; failure rejects the query.
    pub fn set_regexes(
        &mut self,
        include: Option<&str>,
        exclude: Option<&str>,
    ) -> Result<(), String> {
        let compile = |pattern: Option<&str>| -> Result<Option<regex::Regex>, String> {
            match pattern {
                None => Ok(None),
                Some(pattern) => regex::RegexBuilder::new(pattern)
                    .case_insensitive(!self.case_sensitive)
                    .build()
                    .map(Some)
                    .map_err(|e| e.to_string()),
            }
        };
        self.include = compile(include)?;
        self.exclude = compile(exclude)?;
        Ok(())
    }

    /// `isPreInterpolate()`.
    pub fn is_pre_interpolate(&self) -> bool {
        self.interpolate == InterpolateMode::PreInterpolate
    }

    /// `isPostInterpolate()`.
    pub fn is_post_interpolate(&self) -> bool {
        self.interpolate == InterpolateMode::PostInterpolate
    }

    /// `validateAddress` (`query_params.cpp:551-564`): an unset filter
    /// passes everything; otherwise the (possibly negated) equality.
    pub fn validate_address(&self, ip_address: u32) -> bool {
        if self.ip_address == 0 {
            return true;
        }
        if self.negate_host {
            self.ip_address != ip_address
        } else {
            self.ip_address == ip_address
        }
    }

    /// `validatePID`.
    pub fn validate_pid(&self, pid: u32) -> bool {
        if self.pid == 0 {
            return true;
        }
        if self.negate_pid {
            self.pid != pid
        } else {
            self.pid == pid
        }
    }

    /// `validateAppID`.
    pub fn validate_app_id(&self, appid: u32) -> bool {
        if self.appid == 0 {
            return true;
        }
        if self.negate_appid {
            self.appid != appid
        } else {
            self.appid == appid
        }
    }

    /// `validateProcessType`: -1 means all, otherwise the bit test.
    pub fn validate_process_type(&self, app_type_id: u32) -> bool {
        if self.procs == -1 {
            return true;
        }
        self.procs & (1i64 << app_type_id) != 0
    }

    /// `validateMessagePriority`: -1 means all, otherwise the bit test.
    pub fn validate_message_priority(&self, priority: u8) -> bool {
        if self.severities == -1 {
            return true;
        }
        self.severities & (1i64 << priority) != 0
    }

    /// `validateIncludeRegex`: unset passes; otherwise the text must match.
    pub fn validate_include_regex(&self, search: &str) -> bool {
        match &self.include {
            None => true,
            Some(regex) => regex.is_match(search),
        }
    }

    /// `validateExcludeRegex`: unset passes; otherwise the text must NOT
    /// match.
    pub fn validate_exclude_regex(&self, search: &str) -> bool {
        match &self.exclude {
            None => true,
            Some(regex) => !regex.is_match(search),
        }
    }

    /// `validateLogMessageType`: the source bit test.
    pub fn validate_log_message_type(&self, source: MessageSource) -> bool {
        self.log_source_type_flags & (1 << source.to_byte()) != 0
    }

    /// `validateCategory`: an empty multiset passes everything; otherwise
    /// the entry's ID must be one of the requested IDs.
    pub fn validate_category(&self, category_ids: &BTreeSet<u16>, category_id: u16) -> bool {
        if category_ids.is_empty() {
            return true;
        }
        category_ids.contains(&category_id)
    }
}

/// One query hit (`PyQueryResult`'s fields).
#[derive(Clone, Debug)]
pub struct QueryResult {
    /// `entry.time.asDouble()`.
    pub time: f64,
    /// The component's hostname.
    pub host: String,
    /// The component's OS PID.
    pub pid: u32,
    /// The component's app instance ID.
    pub app_instance_id: u32,
    /// The owning username.
    pub username: String,
    /// The component name.
    pub component: String,
    /// The resolved category name (empty when unknown).
    pub category: String,
    /// `"CPP"` or `"SCRIPT"`.
    pub source: &'static str,
    /// The raw severity byte (`DebugMessagePriority`).
    pub severity: u8,
    /// The (possibly interpolated) message text.
    pub message: String,
    /// The metadata string.
    pub metadata: String,
    /// The format string offset the line came from.
    pub string_offset: u32,
}

/// Why a query could not run, or an entry could not be rendered
/// (`PyQuery::getResultForEntry`'s error paths).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum QueryError {
    /// The entry's string offset has no format string.
    UnknownStringOffset(u32),
    /// The entry's component ID is not registered.
    UnknownComponentId(u32),
    /// A requested category name is unknown to the log.
    UnknownCategory(String),
}

/// The `getResultForEntry` filter pipeline (`py_query.cpp:576-627`), in the
/// reference order: address, PID, app ID (skipped when the component never
/// announced one), process type, priority, message source, category, then
/// the include and exclude regexes over `match_text`.
pub fn passes_filters(
    params: &QueryParams,
    category_ids: &BTreeSet<u16>,
    component: &LoggingComponent,
    entry: &LogEntry,
    match_text: &str,
) -> bool {
    params.validate_address(component.ip)
        && params.validate_pid(component.pid)
        && (component.app_instance_id == 0 || params.validate_app_id(component.app_instance_id))
        && params.validate_process_type(component.app_type_id)
        && params.validate_message_priority(entry.message_priority)
        && params.validate_log_message_type(entry.message_source)
        && params.validate_category(category_ids, entry.category_id)
        && params.validate_include_regex(match_text)
        && params.validate_exclude_regex(match_text)
}

/// `PyQuery::interpolate`: clear `dest` and render the entry's arguments
/// into it (`dest.clear()` then the printer appends). A short argument
/// stream leaves a partially rendered line, exactly like the reference —
/// only a missing stream fails there, and in this storage model the blob
/// always backs the entry.
fn interpolate_into(
    interpolator: &LogStringInterpolator,
    args: &[u8],
    version: u8,
    dest: &mut String,
) {
    dest.clear();
    let mut stream = MemoryIStream::new(args);
    let _ = stream_to_string(interpolator, &mut stream, version, dest);
}

/// Run a query over an in-memory log: walk the entries in the query's
/// direction between the time sentinels, filter each candidate, and render
/// the survivors (`QueryRange` + `PyQuery::getResultForEntry`).
pub fn run_query(log: &QueryableLog, params: &QueryParams) -> Result<Vec<QueryResult>, QueryError> {
    // initCategories: resolve the specifier names against the log's table.
    let mut category_ids = BTreeSet::new();
    for name in &params.category_names {
        let Some(id) = log
            .categories
            .iter()
            .find_map(|(id, n)| if n == name { Some(*id) } else { None })
        else {
            return Err(QueryError::UnknownCategory(name.clone()));
        };
        category_ids.insert(id);
    }

    let start = LogTime::from_double(params.start);
    let end = LogTime::from_double(params.end);
    let entries = &log.entries;

    if entries.is_empty() {
        return Ok(Vec::new());
    }

    // Single-segment span check (`QueryRange::findEntryInSegment`): the
    // segment qualifies when either bound falls inside it or it is spanned.
    let first_time = entries[0].entry.time;
    let last_time = entries[entries.len() - 1].entry.time;
    let starteq = first_time <= start && start <= last_time;
    let endeq = first_time <= end && end <= last_time;
    let startlt = start < first_time;
    let endgt = last_time < end;
    if !(starteq || endeq || (startlt && endgt)) {
        return Ok(Vec::new());
    }

    // Sentinel positions: the first entry at or after a time, or the last
    // entry at or before one (`findEntryNumber` per direction).
    let first_at_or_after = |time: LogTime| -> Option<usize> {
        let index = entries.partition_point(|line| line.entry.time < time);
        if index < entries.len() {
            Some(index)
        } else {
            None
        }
    };
    let last_at_or_before = |time: LogTime| -> Option<usize> {
        let index = entries.partition_point(|line| line.entry.time <= time);
        if index > 0 { Some(index - 1) } else { None }
    };

    // findSentinel: forwards starts at the left sentinel and stops at the
    // right; backwards starts at the right and stops at the left. The walk
    // visits the same inclusive span, in the direction's order.
    let (begin, stop) = match params.direction {
        SearchDirection::Forwards => {
            let Some(begin) = first_at_or_after(start) else {
                return Ok(Vec::new());
            };
            let Some(stop) = last_at_or_before(end) else {
                return Ok(Vec::new());
            };
            if begin > stop {
                return Ok(Vec::new());
            }
            (begin, stop)
        }
        SearchDirection::Backwards => {
            let Some(begin) = last_at_or_before(end) else {
                return Ok(Vec::new());
            };
            let Some(stop) = first_at_or_after(start) else {
                return Ok(Vec::new());
            };
            if stop > begin {
                return Ok(Vec::new());
            }
            (begin, stop)
        }
    };

    let indices: Box<dyn Iterator<Item = usize>> = match params.direction {
        SearchDirection::Forwards => Box::new(begin..=stop),
        SearchDirection::Backwards => Box::new((stop..=begin).rev()),
    };

    let mut results = Vec::new();
    for index in indices {
        let line = &entries[index];
        let entry = &line.entry;

        // getHandlerForLogEntry
        let Some(interpolator) = log.format_strings.get(&entry.string_offset) else {
            return Err(QueryError::UnknownStringOffset(entry.string_offset));
        };
        // getComponentByID
        let Some(component) = log.components.get(&entry.user_component_id) else {
            return Err(QueryError::UnknownComponentId(entry.user_component_id));
        };

        let mut match_text = interpolator.format_string().to_string();
        // The argument blob always backs the entry in storage (empty when
        // the record carried none), so interpolation cannot fail here —
        // only a partially rendered line on a short blob.
        if params.is_pre_interpolate() {
            interpolate_into(interpolator, &line.args, 10, &mut match_text);
        }

        if !passes_filters(params, &category_ids, component, entry, &match_text) {
            continue;
        }

        if params.is_post_interpolate() {
            interpolate_into(interpolator, &line.args, 10, &mut match_text);
        }

        results.push(QueryResult {
            time: entry.time.as_double(),
            host: component.host.clone(),
            pid: component.pid,
            app_instance_id: component.app_instance_id,
            username: component.username.clone(),
            component: component.name.clone(),
            category: log
                .categories
                .get(&entry.category_id)
                .cloned()
                .unwrap_or_default(),
            source: match entry.message_source {
                MessageSource::Cpp => "CPP",
                MessageSource::Script => "SCRIPT",
            },
            severity: entry.message_priority,
            message: match_text,
            metadata: line.metadata.clone(),
            string_offset: entry.string_offset,
        });
    }

    Ok(results)
}

/// The severity byte's reference name, for display
/// (`DebugMessagePriority`).
pub fn severity_name(priority: u8) -> &'static str {
    match priority {
        message_priority::TRACE => "TRACE",
        message_priority::DEBUG => "DEBUG",
        message_priority::INFO => "INFO",
        message_priority::NOTICE => "NOTICE",
        message_priority::WARNING => "WARNING",
        message_priority::ERROR => "ERROR",
        message_priority::CRITICAL => "CRITICAL",
        message_priority::HACK => "HACK",
        message_priority::DEPRECATED_SCRIPT => "SCRIPT",
        message_priority::ASSET => "ASSET",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::LogEntry;
    use crate::message_priority;
    use crate::stream::MemoryOStream;

    /// One component, three format strings, a handful of entries spread
    /// over times 100..105. Argument blobs are stored in the component's
    /// wire encoding (packed-length strings, little-endian scalars).
    fn sample_log() -> QueryableLog {
        let mut log = QueryableLog::default();
        log.add_component(
            1,
            LoggingComponent {
                ip: 0x7f_00_00_01, // 127.0.0.1
                host: "localhost".to_string(),
                pid: 4242,
                app_instance_id: 0,
                app_type_id: 2,
                username: "bigworld".to_string(),
                name: "cellapp01".to_string(),
            },
        );
        log.add_category("combat");
        log.add_category("movement");
        log.add_format_string(0, "damage %d");
        log.add_format_string(1, "moved to %s");
        log.add_format_string(2, "plain line");

        let encoded = |build: &dyn Fn(&mut MemoryOStream)| -> Vec<u8> {
            let mut stream = MemoryOStream::new();
            build(&mut stream);
            stream.data().to_vec()
        };

        let push_at = |log: &mut QueryableLog,
                       secs: i64,
                       msecs: u16,
                       offset: u32,
                       source: MessageSource,
                       args: Vec<u8>| {
            log.push_entry(StoredLogLine {
                entry: LogEntry::new(secs, msecs, 1, 1, source, 2, offset),
                args,
                metadata: String::new(),
            });
        };

        push_at(
            &mut log,
            100,
            0,
            0,
            MessageSource::Cpp,
            encoded(&|out| {
                out.write_u32(0);
            }),
        );
        push_at(
            &mut log,
            101,
            0,
            1,
            MessageSource::Cpp,
            encoded(&|out| {
                out.write_string("spawn");
            }),
        );
        push_at(&mut log, 102, 500, 2, MessageSource::Cpp, Vec::new());
        push_at(
            &mut log,
            103,
            0,
            0,
            MessageSource::Cpp,
            encoded(&|out| {
                out.write_u32(33);
            }),
        );
        push_at(
            &mut log,
            104,
            0,
            1,
            MessageSource::Script,
            encoded(&|out| {
                out.write_string("py");
            }),
        );
        push_at(
            &mut log,
            105,
            0,
            1,
            MessageSource::Cpp,
            encoded(&|out| {
                out.write_string("home");
            }),
        );
        log
    }

    #[test]
    fn default_params_match_the_reference_constructor() {
        let params = QueryParams::default();
        assert_eq!(params.start, LOG_BEGIN);
        assert_eq!(params.end, LOG_END);
        assert_eq!(params.procs, -1);
        assert_eq!(params.severities, -1);
        assert_eq!(params.log_source_type_flags, 0b11);
        assert!(params.is_pre_interpolate());
        assert!(!params.is_post_interpolate());
        assert!(params.case_sensitive);
        assert_eq!(params.direction, SearchDirection::Forwards);
    }

    #[test]
    fn the_full_log_returns_every_line_in_order() {
        let log = sample_log();
        let results = run_query(&log, &QueryParams::default()).unwrap();
        assert_eq!(results.len(), 6);
        assert_eq!(results[0].message, "damage 0");
        assert_eq!(results[1].message, "moved to spawn");
        assert_eq!(results[2].message, "plain line");
        assert_eq!(results[3].message, "damage 33");
        assert_eq!(results[4].message, "moved to py");
        assert_eq!(results[4].source, "SCRIPT");
        assert_eq!(results[5].message, "moved to home");
        assert_eq!(results[0].component, "cellapp01");
        assert_eq!(results[0].host, "localhost");
        assert_eq!(results[0].pid, 4242);
        assert_eq!(results[0].username, "bigworld");
        assert_eq!(results[0].source, "CPP");
        assert_eq!(results[0].severity, message_priority::INFO);
        assert_eq!(results[0].category, "combat");
    }

    #[test]
    fn a_time_window_selects_its_entries() {
        let log = sample_log();
        let params = QueryParams {
            start: 101.0,
            end: 103.0,
            ..QueryParams::default()
        };
        let results = run_query(&log, &params).unwrap();
        let times: Vec<f64> = results.iter().map(|r| r.time).collect();
        assert_eq!(times, vec![101.0, 102.5, 103.0]);
    }

    #[test]
    fn backwards_queries_run_in_reverse_order() {
        let log = sample_log();
        let params = QueryParams {
            direction: SearchDirection::Backwards,
            ..QueryParams::default()
        };
        let results = run_query(&log, &params).unwrap();
        let times: Vec<f64> = results.iter().map(|r| r.time).collect();
        assert_eq!(times, vec![105.0, 104.0, 103.0, 102.5, 101.0, 100.0]);

        // A backwards window is bounded the same way.
        let params = QueryParams {
            start: 101.0,
            end: 103.0,
            direction: SearchDirection::Backwards,
            ..QueryParams::default()
        };
        let results = run_query(&log, &params).unwrap();
        let times: Vec<f64> = results.iter().map(|r| r.time).collect();
        assert_eq!(times, vec![103.0, 102.5, 101.0]);
    }

    #[test]
    fn a_period_outside_the_log_is_empty() {
        let log = sample_log();
        let params = QueryParams {
            start: 200.0,
            end: 300.0,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        // A period before the log misses the segment, like the reference's
        // span check.
        let params = QueryParams {
            start: 1.0,
            end: 50.0,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        // A period that spans the whole log still matches.
        let params = QueryParams {
            start: 1.0,
            end: 200.0,
            ..QueryParams::default()
        };
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);

        // And LOG_END runs to infinity.
        let params = QueryParams {
            start: 102.6,
            ..QueryParams::default()
        };
        let results = run_query(&log, &params).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn inverted_or_one_sided_windows_collapse_to_empty() {
        let log = sample_log();

        // Both bounds inside the span but the start past the stop: the
        // forwards walk finds begin after stop.
        let params = QueryParams {
            start: 104.0,
            end: 101.0,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        // A start beyond the log with an end inside it qualifies via the
        // span check, but the left sentinel misses.
        let params = QueryParams {
            start: 200.0,
            end: 102.0,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        // An end before the log with a start inside it: the right sentinel
        // misses.
        let params = QueryParams {
            start: 103.0,
            end: 50.0,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        // The same shapes backwards.
        let params = QueryParams {
            start: 104.0,
            end: 101.0,
            direction: SearchDirection::Backwards,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        let params = QueryParams {
            start: 200.0,
            end: 102.0,
            direction: SearchDirection::Backwards,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        let params = QueryParams {
            start: 103.0,
            end: 50.0,
            direction: SearchDirection::Backwards,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());
    }

    #[test]
    fn address_pid_and_app_filters_match_and_negate() {
        let log = sample_log();
        let params = QueryParams {
            ip_address: 0x7f_00_00_01,
            ..QueryParams::default()
        };
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);
        let params = QueryParams {
            ip_address: 0x7f_00_00_01,
            negate_host: true,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        let params = QueryParams {
            pid: 4242,
            ..QueryParams::default()
        };
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);
        let params = QueryParams {
            pid: 4242,
            negate_pid: true,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        // The app filter only applies to components that announced an ID;
        // cellapp01 never did, so even a mismatching appid passes.
        let params = QueryParams {
            appid: 99,
            ..QueryParams::default()
        };
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);

        let mut log = log.clone();
        log.components.get_mut(&1).unwrap().app_instance_id = 7;
        let mut params = QueryParams {
            appid: 7,
            ..QueryParams::default()
        };
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);
        params.appid = 99;
        assert!(run_query(&log, &params).unwrap().is_empty());
        params.negate_appid = true;
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);
    }

    #[test]
    fn process_type_and_severity_masks_select_bits() {
        let log = sample_log();
        let mut params = QueryParams {
            procs: 1 << 2,
            ..QueryParams::default()
        };
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);
        params.procs = 1 << 3;
        assert!(run_query(&log, &params).unwrap().is_empty());

        let mut params = QueryParams {
            severities: 1 << message_priority::WARNING,
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());
        params.severities = (1 << message_priority::INFO) | (1 << message_priority::WARNING);
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);
    }

    #[test]
    fn source_filter_selects_the_call_site_kind() {
        let log = sample_log();
        let params = QueryParams {
            log_source_type_flags: 1 << MessageSource::Script.to_byte(),
            ..QueryParams::default()
        };
        // One script call site in the sample log.
        assert_eq!(run_query(&log, &params).unwrap().len(), 1);
        let params = QueryParams {
            log_source_type_flags: 1 << MessageSource::Cpp.to_byte(),
            ..QueryParams::default()
        };
        assert_eq!(run_query(&log, &params).unwrap().len(), 5);
    }

    #[test]
    fn category_filters_resolve_names_to_ids() {
        let log = sample_log();
        let params = QueryParams {
            category_names: vec!["combat".to_string()],
            ..QueryParams::default()
        };
        // The sample entries all carry categoryID 1 ("combat").
        assert_eq!(run_query(&log, &params).unwrap().len(), 6);
        let params = QueryParams {
            category_names: vec!["movement".to_string()],
            ..QueryParams::default()
        };
        assert!(run_query(&log, &params).unwrap().is_empty());

        let params = QueryParams {
            category_names: vec!["missing".to_string()],
            ..QueryParams::default()
        };
        assert!(matches!(
            run_query(&log, &params),
            Err(QueryError::UnknownCategory(_))
        ));
    }

    #[test]
    fn include_and_exclude_regexes_filter_the_message_text() {
        let log = sample_log();
        let mut params = QueryParams::default();
        params.set_regexes(Some("damage"), None).unwrap();
        assert_eq!(run_query(&log, &params).unwrap().len(), 2);

        let mut params = QueryParams::default();
        params.set_regexes(Some("DAMAGE"), None).unwrap();
        // Case sensitive by default: no match.
        assert!(run_query(&log, &params).unwrap().is_empty());
        params.case_sensitive = false;
        params.set_regexes(Some("DAMAGE"), None).unwrap();
        assert_eq!(run_query(&log, &params).unwrap().len(), 2);

        let mut params = QueryParams::default();
        params.set_regexes(None, Some("plain")).unwrap();
        assert_eq!(run_query(&log, &params).unwrap().len(), 5);

        let mut params = QueryParams::default();
        assert!(params.set_regexes(Some("("), None).is_err());
    }

    #[test]
    fn dont_interpolate_filters_on_the_raw_format_string() {
        let log = sample_log();
        let mut params = QueryParams {
            interpolate: InterpolateMode::DontInterpolate,
            ..QueryParams::default()
        };
        let results = run_query(&log, &params).unwrap();
        assert_eq!(results[0].message, "damage %d");

        // And the include regex now sees the raw format string.
        params.set_regexes(Some("%d"), None).unwrap();
        let results = run_query(&log, &params).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn post_interpolate_filters_before_rendering() {
        let log = sample_log();
        let mut params = QueryParams {
            interpolate: InterpolateMode::PostInterpolate,
            ..QueryParams::default()
        };
        // The regex runs against the raw format string.
        params.set_regexes(Some("moved to"), None).unwrap();
        let results = run_query(&log, &params).unwrap();
        assert_eq!(results.len(), 3);
        // But the reported message is still rendered.
        assert_eq!(results[0].message, "moved to spawn");
    }

    #[test]
    fn unknown_references_are_reported() {
        let mut log = sample_log();
        log.entries[0].entry.string_offset = 99;
        assert!(matches!(
            run_query(&log, &QueryParams::default()),
            Err(QueryError::UnknownStringOffset(99))
        ));

        let mut log = sample_log();
        log.entries[0].entry.user_component_id = 99;
        assert!(matches!(
            run_query(&log, &QueryParams::default()),
            Err(QueryError::UnknownComponentId(99))
        ));
    }

    #[test]
    fn an_empty_log_is_an_empty_result() {
        let log = QueryableLog::default();
        assert!(run_query(&log, &QueryParams::default()).unwrap().is_empty());
    }

    #[test]
    fn category_registration_is_idempotent_and_sequential() {
        let mut log = QueryableLog::default();
        assert_eq!(log.add_category("combat"), 1);
        assert_eq!(log.add_category("movement"), 2);
        // Re-registering resolves to the existing ID.
        assert_eq!(log.add_category("combat"), 1);
        assert_eq!(log.categories.get(&1).map(String::as_str), Some("combat"));
        assert_eq!(log.categories.get(&2).map(String::as_str), Some("movement"));
    }

    #[test]
    fn severity_names_cover_the_table() {
        assert_eq!(severity_name(message_priority::TRACE), "TRACE");
        assert_eq!(severity_name(message_priority::DEBUG), "DEBUG");
        assert_eq!(severity_name(message_priority::INFO), "INFO");
        assert_eq!(severity_name(message_priority::NOTICE), "NOTICE");
        assert_eq!(severity_name(message_priority::WARNING), "WARNING");
        assert_eq!(severity_name(message_priority::ERROR), "ERROR");
        assert_eq!(severity_name(message_priority::CRITICAL), "CRITICAL");
        assert_eq!(severity_name(message_priority::HACK), "HACK");
        assert_eq!(severity_name(message_priority::DEPRECATED_SCRIPT), "SCRIPT");
        assert_eq!(severity_name(message_priority::ASSET), "ASSET");
        assert_eq!(severity_name(42), "UNKNOWN");
    }

    #[test]
    fn validators_pass_everything_while_a_filter_is_unset() {
        // Through run_query the app check is guarded by a nonzero instance
        // ID (matching the reference's `&&` short circuit), so the unset
        // arm only shows through direct calls.
        let params = QueryParams::default();
        assert!(params.validate_address(0x0a_00_00_01));
        assert!(params.validate_pid(999));
        assert!(params.validate_app_id(999));

        // The negated arms invert the equality.
        let negated = QueryParams {
            appid: 7,
            negate_appid: true,
            ..QueryParams::default()
        };
        assert!(negated.validate_app_id(9));
        assert!(!negated.validate_app_id(7));
    }
}
