//! Remote watcher queries and forwarding (BW-12-01-04): the
//! `ForwardingWatcher` target routing and the `ForwardingCollector`
//! response aggregation.
//!
//! Ported from `programming/bigworld/lib/server/watcher_forwarding.{hpp,cpp}`,
//! `watcher_forwarding_collector.{hpp,cpp}`, `watcher_forwarding_types.hpp`
//! and `watcher_protocol.{hpp,cpp}`, plus the concrete manager routing in
//! `server/cellappmgr/watcher_forwarding_cellapp.cpp` and
//! `server/baseappmgr/watcher_forwarding_baseapp.cpp`.
//!
//! The port keeps the pure logic that decides *which nodes* a request routes
//! to and *how their replies merge* — the Mercury transport itself is out of
//! scope (it lives in `crates/mercury`, a forbidden zone), so the collector
//! is fed replies directly by the caller instead of by the network.

use super::path_tree::{Mode, WatcherDataType, WATCHER_SEPARATOR};
use foundation::stream::{packed_int_size, BinaryStream};

/// `ForwardingWatcher::TARGET_CELL_APPS` (watcher_forwarding.cpp:10).
pub const TARGET_CELL_APPS: &str = "cellApps";
/// `ForwardingWatcher::TARGET_BASE_APPS` (watcher_forwarding.cpp:11).
pub const TARGET_BASE_APPS: &str = "baseApps";
/// `ForwardingWatcher::TARGET_SERVICE_APPS` (watcher_forwarding.cpp:12).
pub const TARGET_SERVICE_APPS: &str = "serviceApps";
/// `ForwardingWatcher::TARGET_BASE_SERVICE_APPS` (watcher_forwarding.cpp:13).
pub const TARGET_BASE_SERVICE_APPS: &str = "baseServiceApps";
/// `ForwardingWatcher::TARGET_LEAST_LOADED` (watcher_forwarding.cpp:14).
pub const TARGET_LEAST_LOADED: &str = "leastLoaded";

/// `ComponentType` (watcher_forwarding_types.hpp:8-13): which kind of
/// component a `Component` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ComponentType {
    /// `CELL_APP`
    CellApp = 0,
    /// `BASE_APP`
    BaseApp = 1,
    /// `SERVICE_APP`
    ServiceApp = 2,
}

impl ComponentType {
    /// The enum as a protocol `int`: `packResult` streams the component type
    /// through `watcherValueToStream`'s `int` overload (watcher.hpp:982-990).
    pub const fn as_int(self) -> i32 {
        self as i32
    }
}

/// `ComponentID` (watcher_forwarding_types.hpp:15).
pub type ComponentID = i32;

/// `ComponentIDList` (watcher_forwarding_types.hpp:16).
pub type ComponentIDList = Vec<ComponentID>;

/// `Component` (watcher_forwarding_types.hpp:17): a component's kind and
/// its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Component {
    /// `component.first`
    pub component_type: ComponentType,
    /// `component.second`
    pub id: ComponentID,
}

impl Component {
    /// A component of `component_type` with `id`.
    pub const fn new(component_type: ComponentType, id: ComponentID) -> Self {
        Self { component_type, id }
    }
}

/// `ForwardingWatcher::ExposeHints` (watcher_forwarding.hpp:28-37): which
/// managed components a forwarded watcher path is aimed at. This enum is
/// distinct from `CallableWatcher::ExposeHint` in `callable.rs` — the two
/// live in different classes with overlapping names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ExposeHints {
    /// `WITH_ENTITY`
    WithEntity = 0,
    /// `CELL_APPS`
    CellApps,
    /// `WITH_SPACE`
    WithSpace,
    /// `LEAST_LOADED`
    LeastLoaded,
    /// `LOCAL_ONLY`
    LocalOnly,
    /// `BASE_APPS`
    BaseApps,
    /// `SERVICE_APPS`
    ServiceApps,
    /// `BASE_SERVICE_APPS`
    BaseServiceApps,
}

/// `Watcher::isEmptyPath` (watcher.hpp:945-947): a null or empty path.
pub fn is_empty_path(path: &str) -> bool {
    path.is_empty()
}

/// `Watcher::isDocPath` (watcher.hpp:948-954): the `__doc__` path.
pub fn is_doc_path(path: &str) -> bool {
    path == "__doc__"
}

/// The destination split of `ForwardingWatcher::setFromStream`
/// (watcher_forwarding.cpp:81-108): the path is `<target>/<destWatcher>`,
/// e.g. `'cellApps/command/addBot'`.
///
/// Returns `None` when there is no `/` at all (a "bad destination path")
/// or when the `/` is the final character (no destination watcher path
/// remains) — the two error branches of the reference.
pub fn split_forwarding_path(path: &str) -> Option<(&str, &str)> {
    let pos = path.find(WATCHER_SEPARATOR)?;
    let rest = &path[pos + 1..];
    if rest.is_empty() {
        return None;
    }
    Some((&path[..pos], rest))
}

/// `ForwardingWatcher::getComponentIDList` (watcher_forwarding.cpp:26-70):
/// a list of component ids from a comma-separated string.
///
/// Tokens are parsed with `strtol(..., 10)`: the whole token must be an
/// optional-sign integer with no trailing junk, and the value must not be
/// the `long` extremes. Bad tokens are skipped, not fatal — the reference
/// logs an error and carries on.
pub fn get_component_id_list(target_info: &str) -> ComponentIDList {
    let mut components = ComponentIDList::new();
    for token in target_info.split(',') {
        // `strtok` skips empty tokens and collapses consecutive separators.
        if token.is_empty() {
            continue;
        }
        if let Some(value) = parse_long_token(token) {
            components.push(value as i32);
        }
    }
    components
}

/// `strtol(token, &end, 10)` (watcher_forwarding.cpp:36-49): the whole
/// token must be an optional-sign integer, and the value must not be the
/// `long` extremes (which `strtol` saturates to on overflow).
fn parse_long_token(token: &str) -> Option<i64> {
    let bytes = token.as_bytes();
    let mut idx = 0;
    // Leading whitespace is skipped by `strtol`.
    while idx < bytes.len()
        && matches!(bytes[idx], b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
    {
        idx += 1;
    }
    // Optional sign.
    let mut negative = false;
    if idx < bytes.len() && (bytes[idx] == b'+' || bytes[idx] == b'-') {
        negative = bytes[idx] == b'-';
        idx += 1;
    }
    let start = idx;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if start == idx {
        // No digits: no conversion — `endPtr` stays at the start and
        // `*endPtr != '\0'` unless the token itself was empty (which
        // `strtok` never yields).
        return None;
    }
    if idx != bytes.len() {
        // Trailing junk after the number: `*endPtr != '\0'`.
        return None;
    }
    // The digits fit a `long` (i64 on Linux); overflow saturates to the
    // extremes, which the caller rejects.
    let magnitude: i64 = token[start..idx].parse().ok()?;
    let value = if negative { -magnitude } else { magnitude };
    if value == i64::MIN || value == i64::MAX {
        return None;
    }
    Some(value)
}

/// One managed component as the manager sees it: which component it is and
/// the load metric `listLeastLoaded`/`findLeastLoadedCellApp` compare.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComponentEntry {
    /// The component (kind and id).
    pub component: Component,
    /// `CellApp::estimatedLoad()`/`BaseApp::load()` — the load the
    /// least-loaded selection compares (watcher_forwarding_baseapp.cpp:88-123,
    /// cellapps.cpp:446-463).
    pub load: f32,
}

impl ComponentEntry {
    /// An entry for `component` carrying `load`.
    pub const fn new(component: Component, load: f32) -> Self {
        Self { component, load }
    }
}

/// The manager a `ForwardingWatcher` serves: which categories of component
/// the manager owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManagerKind {
    /// `CAForwardingWatcher` (server/cellappmgr/) — the manager of CellApps.
    CellApp,
    /// `BAForwardingWatcher` (server/baseappmgr/) — the manager of
    /// BaseApps/ServiceApps.
    BaseApp,
}

/// A component's decoded reply: its stdio output and the tuple that packs
/// `[component id, its result, component type]` (the `ResponseDecoder`
/// `packResult`, watcher_forwarding_collector.cpp:331-372).
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentReply {
    /// The stdio output, newline-normalised (`stringHandler`).
    pub output: String,
    /// The packed per-component tuple element.
    pub tuple: Vec<u8>,
}

/// A decode failure: the reply did not have the required shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The first element was not the required tuple
    /// (watcher_forwarding_collector.cpp:287-290).
    FirstElementNotTuple,
    /// The second element was not the required output string
    /// (watcher_forwarding_collector.cpp:294-297).
    SecondElementNotString,
    /// A stream element failed to decode.
    Stream,
    /// The tuple header was malformed (`tupleHandler`).
    TupleHeader,
}

/// `ForwardingWatcher` — the watcher that routes a request to the
/// components its manager owns. The v1 protocol is unsupported
/// (watcher_forwarding.hpp:63-95), so reads/writes through the string
/// protocol are refused and only the v2 `getAsStream` shape is honoured.
pub struct ForwardingWatcher {
    /// The manager the watcher serves.
    kind: ManagerKind,
    /// The comment, reported by the `__doc__` path (watcher_forwarding.cpp:120-127).
    comment: String,
}

impl ForwardingWatcher {
    /// A forwarding watcher for `kind` with `comment`.
    pub fn new(kind: ManagerKind, comment: &str) -> Self {
        Self {
            kind,
            comment: comment.to_string(),
        }
    }

    /// The manager the watcher serves.
    pub const fn kind(&self) -> ManagerKind {
        self.kind
    }

    /// `ForwardingWatcher::getAsString` (watcher_forwarding.hpp:74-81): the
    /// version-1 protocol is unsupported, so a string read is always
    /// refused.
    pub fn get_as_string(&self, _path: &str) -> Option<(String, Mode)> {
        None
    }

    /// `ForwardingWatcher::setFromString` (watcher_forwarding.hpp:88-95):
    /// the version-1 protocol is unsupported, so a string write is always
    /// refused.
    pub fn set_string(&mut self, _path: &str, _value_str: &str) -> bool {
        false
    }

    /// `ForwardingWatcher::getAsStream` (watcher_forwarding.cpp:112-131):
    /// an empty path reports `Forwarding Watcher`, the `__doc__` path
    /// reports the comment, and anything else is not a path of this
    /// watcher.
    pub fn get_as_stream(&self, path: &str) -> Option<(String, Mode)> {
        if is_empty_path(path) {
            Some(("Forwarding Watcher".to_string(), Mode::ReadOnly))
        } else if is_doc_path(path) {
            Some((self.comment.clone(), Mode::ReadOnly))
        } else {
            None
        }
    }

    /// `ForwardingWatcher::setFromStream` (watcher_forwarding.cpp:81-108)
    /// split into the destination it would forward to, and the watcher path
    /// the receiving components should run.
    pub fn forwarding_dest(&self, path: &str) -> Option<ForwardingDest> {
        let (target_info, dest_watcher) = split_forwarding_path(path)?;
        Some(ForwardingDest {
            target_info: target_info.to_string(),
            dest_watcher: dest_watcher.to_string(),
        })
    }

    /// The node set a forwarded request targets — the manager's
    /// `getComponentAddressList` (watcher_forwarding_cellapp.cpp:103-135,
    /// watcher_forwarding_baseapp.cpp:56-81), without the addresses.
    pub fn resolve_targets(
        &self,
        target_info: &str,
        registry: &[ComponentEntry],
    ) -> Vec<Component> {
        resolve_targets(self.kind, target_info, registry)
    }
}

/// The destination of a forwarded watcher path: which components to reach
/// and the path to run on them (`setFromStream`'s two halves).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardingDest {
    /// The target information (`'cellApps'`, `'leastLoaded'`, `'1,2,45'`).
    pub target_info: String,
    /// The watcher path on the receiving side (`'command/addBot'`).
    pub dest_watcher: String,
}

/// `ForwardingCollector` — collects the replies of every component a
/// request was sent to and merges them into the result stream the caller
/// reads (watcher_forwarding_collector.hpp:38-75, .cpp:46-190). The
/// Mercury send itself is out of scope: the caller feeds replies in via
/// `handle_response`.
pub struct ForwardingCollector {
    /// The watcher path forwarded to each component.
    query_path: String,
    /// `pendingRequests_`: number of outstanding replies.
    pending: usize,
    /// `outputStr_`: all the data from stdout/stderr.
    output: String,
    /// `resultStream_`: packed per-component tuples.
    result: BinaryStream,
    /// `tupleCount_`: number of tuple entries.
    tuple_count: u32,
}

impl ForwardingCollector {
    /// A collector forwarding `dest_watcher` to the targeted components.
    pub fn new(dest_watcher: &str) -> Self {
        Self {
            query_path: dest_watcher.to_string(),
            pending: 0,
            output: String::new(),
            result: BinaryStream::new(),
            tuple_count: 0,
        }
    }

    /// The watcher path forwarded to each component.
    pub fn query_path(&self) -> &str {
        &self.query_path
    }

    /// `ForwardingCollector::start` (watcher_forwarding_collector.cpp:76-140)
    /// minus the Mercury send: `value` is the data to forward (the value
    /// stream), `count` the number of components targeted. An empty value
    /// stream is refused ("No data provided to forward") and returns false,
    /// exactly as the reference's `start()` returns false. The reference's
    /// `callingComponents_` guard only protects the synchronous send loop,
    /// which the port does not have.
    pub fn start(&mut self, value: &[u8], count: usize) -> bool {
        if value.is_empty() {
            return false;
        }
        self.pending += count;
        true
    }

    /// `ForwardingCollector::handleMessage` (watcher_forwarding_collector.cpp:197-236):
    /// a reply arrived from `component`. The reply is decoded, merged into
    /// the result, and the final status byte consumed. A malformed reply
    /// drops that component's contribution (no `tupleCount_` bump).
    pub fn handle_response(
        &mut self,
        component: Component,
        stream: &mut BinaryStream,
    ) -> Result<(), DecodeError> {
        if self.pending == 0 {
            return Err(DecodeError::Stream);
        }
        self.pending -= 1;
        let reply = decode_component_reply(component, stream)?;
        self.output.push_str(&reply.output);
        self.result.write_blob_raw(&reply.tuple);
        self.tuple_count += 1;
        // The final element of the stream is the boolean status.
        let _status = stream.read_bool().unwrap_or(false);
        Ok(())
    }

    /// `ForwardingCollector::handleException` (watcher_forwarding_collector.cpp:239-259):
    /// a component failed to reply; its contribution is skipped.
    pub fn handle_exception(&mut self) {
        if self.pending > 0 {
            self.pending -= 1;
        }
    }

    /// `ForwardingCollector::checkSatisfied` (watcher_forwarding_collector.cpp:103-166):
    /// once every component has replied (or failed), build the merged
    /// result — a `TUPLE(STRING(output), TUPLE[per-component tuples])`.
    /// Returns `None` while any request is still outstanding.
    pub fn check_satisfied(&mut self) -> Option<Vec<u8>> {
        if self.pending != 0 {
            return None;
        }
        let mut tmp = BinaryStream::new();
        // `writeStringLength( 2 )` — the pair of output string / result.
        tmp.write_packed_int(2).ok()?;
        write_watcher_string(&mut tmp, &self.output, Mode::ReadOnly);
        tmp.write_u8(WatcherDataType::Tuple as u8);
        tmp.write_u8(Mode::ReadOnly as u8);
        let result_len = self.result.length() as u32;
        let size = result_len + packed_int_size(self.tuple_count) as u32;
        tmp.write_packed_int(size).ok()?;
        tmp.write_packed_int(self.tuple_count).ok()?;
        tmp.write_blob_raw(self.result.as_bytes());

        let mut parent = BinaryStream::new();
        parent.write_u8(WatcherDataType::Tuple as u8);
        parent.write_u8(Mode::ReadOnly as u8);
        parent.write_packed_int(tmp.length() as u32).ok()?;
        parent.write_blob_raw(tmp.as_bytes());
        Some(parent.as_bytes().to_vec())
    }

    /// The stdio output merged so far.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// The number of component tuples merged so far (`tupleCount_`).
    pub const fn tuple_count(&self) -> u32 {
        self.tuple_count
    }
}

/// `ForwardingCollector::ResponseDecoder::decode`
/// (watcher_forwarding_collector.cpp:285-301): decode one component's
/// reply. Consumes everything except the final status byte (which
/// `handle_message` reads afterwards).
pub fn decode_component_reply(
    component: Component,
    stream: &mut BinaryStream,
) -> Result<ComponentReply, DecodeError> {
    let mut decoder = ResponseDecoder::new(component);
    if !decoder.decode_next(stream) || decoder.state != WorkState::ExpectingOutput {
        return Err(DecodeError::FirstElementNotTuple);
    }
    if !decoder.decode_next(stream) || decoder.state != WorkState::ExpectingAny {
        return Err(DecodeError::SecondElementNotString);
    }
    decoder.pack_result(stream)
}

/// `WatcherProtocolDecoder::decodeNext` (watcher_protocol.cpp:34-90) with
/// the response decoder's custom handlers.
struct ResponseDecoder {
    component: Component,
    state: WorkState,
    output: String,
}

/// `ForwardingCollector::ResponseDecoder::WorkState`
/// (watcher_forwarding_collector.hpp:87-97).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // the variants mirror the reference's `EXPECTING_*`.
enum WorkState {
    /// `EXPECTING_TUPLE` — should process FUNC_TUPLE next.
    ExpectingTuple,
    /// `EXPECTING_OUTPUT` — expecting the output string now.
    ExpectingOutput,
    /// `EXPECTING_ANY` — any result could come on the stream now.
    ExpectingAny,
}

impl ResponseDecoder {
    fn new(component: Component) -> Self {
        Self {
            component,
            state: WorkState::ExpectingTuple,
            output: String::new(),
        }
    }

    /// `WatcherProtocolDecoder::decodeNext` (watcher_protocol.cpp:34-90):
    /// read the type and mode bytes, then dispatch to the handler for the
    /// type. `WATCHER_TYPE_TYPE` and unknown types fall through to the
    /// default handler (the reference switch has no case for them).
    fn decode_next(&mut self, stream: &mut BinaryStream) -> bool {
        let Ok(ty) = stream.read_u8() else {
            return false;
        };
        let Ok(_mode) = stream.read_u8() else {
            return false;
        };
        let Some(data_type) = WatcherDataType::from_discriminant(ty) else {
            return self.default_handler(stream);
        };
        match data_type {
            WatcherDataType::String => self.string_handler(stream),
            WatcherDataType::Tuple => self.tuple_handler(stream),
            // `intHandler`/`uintHandler`/`floatHandler`/`boolHandler` all
            // fall through to `defaultHandler` in the base decoder, and so
            // do `WATCHER_TYPE_TYPE`/unknown types.
            WatcherDataType::Int
            | WatcherDataType::Uint
            | WatcherDataType::Float
            | WatcherDataType::Bool
            | WatcherDataType::Unknown
            | WatcherDataType::Type => self.default_handler(stream),
        }
    }

    /// `WatcherProtocolDecoder::defaultHandler` (watcher_protocol.cpp:92-119):
    /// read the size of the current data block and skip it.
    fn default_handler(&mut self, stream: &mut BinaryStream) -> bool {
        let Ok(size) = stream.read_packed_int() else {
            return false;
        };
        if size as usize > stream.remaining() {
            return false;
        }
        stream.read_blob_raw(size as usize).is_ok()
    }

    /// `ForwardingCollector::ResponseDecoder::stringHandler`
    /// (watcher_forwarding_collector.cpp:376-404): extract the stdout/
    /// stderr string and append it to the merged output, ensuring the
    /// final element is a newline so any other component data that is
    /// appended is separated.
    fn string_handler(&mut self, stream: &mut BinaryStream) -> bool {
        if self.state != WorkState::ExpectingOutput {
            return false;
        }
        let Ok(str) = stream.read_string() else {
            return false;
        };
        let mut str = str;
        if str.as_bytes().last() != Some(&b'\n') {
            str.push('\n');
        }
        self.output.push_str(&str);
        self.state = WorkState::ExpectingAny;
        true
    }

    /// `ForwardingCollector::ResponseDecoder::tupleHandler`
    /// (watcher_forwarding_collector.cpp:410-438): the first element of a
    /// component response must be the function tuple: a non-empty size and
    /// exactly two entries (output, value).
    fn tuple_handler(&mut self, stream: &mut BinaryStream) -> bool {
        if self.state != WorkState::ExpectingTuple {
            return false;
        }
        let Ok(size) = stream.read_packed_int() else {
            return false;
        };
        if size == 0 {
            return false;
        }
        let Ok(count) = stream.read_packed_int() else {
            return false;
        };
        if count != 2 {
            return false;
        }
        self.state = WorkState::ExpectingOutput;
        true
    }

    /// `ForwardingCollector::ResponseDecoder::packResult`
    /// (watcher_forwarding_collector.cpp:331-372): pack the remaining
    /// contents of the stream (excluding the final status byte) as a new
    /// tuple element `[component id, its result, component type]`.
    fn pack_result(&mut self, stream: &mut BinaryStream) -> Result<ComponentReply, DecodeError> {
        if self.state != WorkState::ExpectingAny {
            return Err(DecodeError::Stream);
        }
        let remaining = stream.remaining();
        if remaining < 1 {
            return Err(DecodeError::Stream);
        }
        // The rest of the stream minus the 1 byte status taken off. The
        // `remaining() >= 1` check above guarantees the read is in bounds.
        let size = remaining - 1;
        let payload = stream
            .read_blob_raw(size)
            .expect("the status byte guarantees the payload is in bounds");

        let mut temp = BinaryStream::new();
        // `writeStringLength( 3 )` — pair of id / result / type. A count of
        // three is always a valid packed int.
        temp.write_packed_int(3)
            .expect("a tiny tuple count always fits");
        // The component id, streamed as an `int` (the reference TODOs it
        // as a string; the port keeps the actual int).
        write_watcher_int(&mut temp, self.component.id, Mode::ReadOnly);
        temp.write_blob_raw(&payload);
        write_watcher_int(
            &mut temp,
            self.component.component_type.as_int(),
            Mode::ReadOnly,
        );

        // result tuple, ro, size, [count, ... ]
        let mut tuple = BinaryStream::new();
        tuple.write_u8(WatcherDataType::Tuple as u8);
        tuple.write_u8(Mode::ReadOnly as u8);
        tuple
            .write_packed_int(temp.length() as u32)
            .expect("the per-component tuple size fits a packed int");
        tuple.write_blob_raw(temp.as_bytes());
        Ok(ComponentReply {
            output: std::mem::take(&mut self.output),
            tuple: tuple.as_bytes().to_vec(),
        })
    }
}

/// `watcherValueToStream`'s `int` overload (watcher.hpp:982-990): type,
/// mode, the `sizeof(int)` length prefix, then the value.
fn write_watcher_int(stream: &mut BinaryStream, value: i32, mode: Mode) {
    stream.write_u8(WatcherDataType::Int as u8);
    stream.write_u8(mode as u8);
    // `sizeof(int)` is 4 and always fits a packed int.
    stream.write_packed_int(4).unwrap();
    stream.write_i32(value);
}

/// `watcherValueToStream`'s string overload (watcher.hpp:962-968): type,
/// mode, then the string (`<< BW::string` is a packed length prefix plus
/// the raw bytes).
fn write_watcher_string(stream: &mut BinaryStream, text: &str, mode: Mode) {
    stream.write_u8(WatcherDataType::String as u8);
    stream.write_u8(mode as u8);
    stream.write_string(text).unwrap();
}

/// The node set a forwarded request targets — the manager's
/// `getComponentAddressList` (watcher_forwarding_cellapp.cpp:103-135,
/// watcher_forwarding_baseapp.cpp:56-81), without the addresses.
///
/// The registry must be in the manager's own iteration order (ascending
/// id, as the reference's `std::map` is); that order decides the
/// least-loaded tie-break.
pub fn resolve_targets(
    kind: ManagerKind,
    target_info: &str,
    registry: &[ComponentEntry],
) -> Vec<Component> {
    match kind {
        ManagerKind::CellApp => resolve_cell_apps(target_info, registry),
        ManagerKind::BaseApp => resolve_base_apps(target_info, registry),
    }
}

/// `CAForwardingWatcher::getComponentAddressList`
/// (watcher_forwarding_cellapp.cpp:103-135).
fn resolve_cell_apps(target_info: &str, registry: &[ComponentEntry]) -> Vec<Component> {
    let as_component =
        |entry: &ComponentEntry| Component::new(ComponentType::CellApp, entry.component.id);
    if target_info == TARGET_LEAST_LOADED {
        // `CellApps::findLeastLoadedCellApp` (cellapps.cpp:446-463): the
        // lowest `estimatedLoad`, first-wins on ties.
        let mut best: Option<&ComponentEntry> = None;
        for entry in registry {
            if best.is_none_or(|b| entry.load < b.load) {
                best = Some(entry);
            }
        }
        best.map(as_component).into_iter().collect()
    } else if target_info == TARGET_CELL_APPS {
        // `CollectAddresses` (watcher_forwarding_cellapp.cpp:35-51): every
        // cell app.
        registry.iter().map(as_component).collect()
    } else {
        // `CollectSomeAddresses` (watcher_forwarding_cellapp.cpp:53-69):
        // the cell apps whose ids are in the explicit target list.
        let target_list = get_component_id_list(target_info);
        registry
            .iter()
            .filter(|entry| target_list.contains(&entry.component.id))
            .map(as_component)
            .collect()
    }
}

/// `BAForwardingWatcher::getComponentAddressList`
/// (watcher_forwarding_baseapp.cpp:56-81): the explicit-id branch is
/// unsupported (returns `NULL`, which makes `newCollector`'s `init` fail),
/// so it yields no targets.
fn resolve_base_apps(target_info: &str, registry: &[ComponentEntry]) -> Vec<Component> {
    let as_component =
        |entry: &ComponentEntry| Component::new(entry.component.component_type, entry.component.id);
    if target_info == TARGET_LEAST_LOADED {
        // `listLeastLoaded( true, false )` (watcher_forwarding_baseapp.cpp:79-123):
        // the lowest `load` among base apps (service apps excluded),
        // last-wins on ties, and it stops as soon as the chosen app is
        // `almostZero`-loaded.
        let mut best: Option<&ComponentEntry> = None;
        for entry in registry {
            if entry.component.component_type == ComponentType::ServiceApp {
                continue;
            }
            match best {
                None => best = Some(entry),
                Some(b) if entry.load <= b.load => best = Some(entry),
                _ => {}
            }
            if best.is_some_and(|b| b.load < 0.0004) {
                break;
            }
        }
        best.map(as_component).into_iter().collect()
    } else if target_info == TARGET_BASE_SERVICE_APPS {
        // `listAll( true, true )`: every base and service app.
        registry.iter().map(as_component).collect()
    } else if target_info == TARGET_BASE_APPS {
        // `listAll( true, false )`: base apps only.
        registry
            .iter()
            .filter(|entry| entry.component.component_type != ComponentType::ServiceApp)
            .map(as_component)
            .collect()
    } else if target_info == TARGET_SERVICE_APPS {
        // `listAll( false, true )`: service apps only.
        registry
            .iter()
            .filter(|entry| entry.component.component_type == ComponentType::ServiceApp)
            .map(as_component)
            .collect()
    } else {
        // Explicit ids are not supported by the base-app manager
        // (`getComponentAddressList` returns `NULL`).
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three cell apps with ascending ids, the manager's map order.
    fn cell_apps() -> Vec<ComponentEntry> {
        vec![
            ComponentEntry::new(Component::new(ComponentType::CellApp, 1), 2.0),
            ComponentEntry::new(Component::new(ComponentType::CellApp, 2), 1.0),
            ComponentEntry::new(Component::new(ComponentType::CellApp, 3), 1.0),
        ]
    }

    /// Base and service apps mixed, the base-app manager's registry.
    fn base_service_apps() -> Vec<ComponentEntry> {
        vec![
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 10), 2.5),
            ComponentEntry::new(Component::new(ComponentType::ServiceApp, 11), 1.5),
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 12), 0.5),
            ComponentEntry::new(Component::new(ComponentType::ServiceApp, 13), 3.5),
        ]
    }

    /// The acceptance: the same watcher path routes to the node set the
    /// target names — `多节点同一路径正确路由`. `cellApps/command/addBot`
    /// reaches all three cell apps, the same path with `1,3` reaches two
    /// of them, and `leastLoaded` reaches the single least-loaded one.
    #[test]
    fn the_same_path_routes_to_the_node_set_the_target_names() {
        let watcher = ForwardingWatcher::new(ManagerKind::CellApp, "ca");
        assert_eq!(watcher.kind(), ManagerKind::CellApp);
        let registry = cell_apps();

        let dest = watcher.forwarding_dest("cellApps/command/addBot").unwrap();
        assert_eq!(dest.target_info, "cellApps");
        assert_eq!(dest.dest_watcher, "command/addBot");
        assert_eq!(
            watcher.resolve_targets(&dest.target_info, &registry),
            vec![
                Component::new(ComponentType::CellApp, 1),
                Component::new(ComponentType::CellApp, 2),
                Component::new(ComponentType::CellApp, 3),
            ]
        );

        // The same path, a different target: only the named ids.
        let dest = watcher.forwarding_dest("1,3/command/addBot").unwrap();
        assert_eq!(
            watcher.resolve_targets(&dest.target_info, &registry),
            vec![
                Component::new(ComponentType::CellApp, 1),
                Component::new(ComponentType::CellApp, 3),
            ]
        );

        // The same path, least loaded: cell apps 2 and 3 tie on load 1.0
        // and the strict `<` of `findLeastLoadedCellApp` keeps the first.
        let dest = watcher
            .forwarding_dest("leastLoaded/command/addBot")
            .unwrap();
        assert_eq!(
            watcher.resolve_targets(&dest.target_info, &registry),
            vec![Component::new(ComponentType::CellApp, 2)]
        );
    }

    /// The base-app manager routes by category: `baseApps` reaches base
    /// apps only, `serviceApps` service apps only, `baseServiceApps`
    /// both, and `leastLoaded` the least-loaded base app
    /// (watcher_forwarding_baseapp.cpp:56-81).
    #[test]
    fn base_apps_route_by_category() {
        let watcher = ForwardingWatcher::new(ManagerKind::BaseApp, "ba");
        let registry = base_service_apps();

        assert_eq!(
            watcher.resolve_targets(TARGET_BASE_APPS, &registry),
            vec![
                Component::new(ComponentType::BaseApp, 10),
                Component::new(ComponentType::BaseApp, 12),
            ]
        );
        assert_eq!(
            watcher.resolve_targets(TARGET_SERVICE_APPS, &registry),
            vec![
                Component::new(ComponentType::ServiceApp, 11),
                Component::new(ComponentType::ServiceApp, 13),
            ]
        );
        assert_eq!(
            watcher.resolve_targets(TARGET_BASE_SERVICE_APPS, &registry),
            vec![
                Component::new(ComponentType::BaseApp, 10),
                Component::new(ComponentType::ServiceApp, 11),
                Component::new(ComponentType::BaseApp, 12),
                Component::new(ComponentType::ServiceApp, 13),
            ]
        );
        // `listLeastLoaded( true, false )` considers base apps only, and
        // `<=` last-wins picks the higher id on a tie — here the loads
        // differ, so it is the least-loaded base app.
        assert_eq!(
            watcher.resolve_targets(TARGET_LEAST_LOADED, &registry),
            vec![Component::new(ComponentType::BaseApp, 12)]
        );
        // Explicit ids are unsupported by the base-app manager
        // (`getComponentAddressList` returns `NULL`).
        assert!(watcher.resolve_targets("10,11", &registry).is_empty());
    }

    /// `listLeastLoaded` (watcher_forwarding_baseapp.cpp:79-123) stops as
    /// soon as the chosen app is `almostZero`-loaded (watcher_forwarding_baseapp.cpp:113-117),
    /// and the `<=` comparison makes a tie pick the last in map order.
    #[test]
    fn least_loaded_base_apps_break_ties_last_and_stop_at_almost_zero() {
        let watcher = ForwardingWatcher::new(ManagerKind::BaseApp, "ba");

        // A zero-load app short-circuits the scan.
        let early = vec![
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 1), 0.0),
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 2), 5.0),
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 3), 0.5),
        ];
        assert_eq!(
            watcher.resolve_targets(TARGET_LEAST_LOADED, &early),
            vec![Component::new(ComponentType::BaseApp, 1)]
        );

        // A tie on the load picks the last app in map (id) order.
        let tied = vec![
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 1), 0.5),
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 2), 0.5),
        ];
        assert_eq!(
            watcher.resolve_targets(TARGET_LEAST_LOADED, &tied),
            vec![Component::new(ComponentType::BaseApp, 2)]
        );

        // A later, more loaded app is skipped (the `<=` comparison fails).
        let later_loaded = vec![
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 1), 0.5),
            ComponentEntry::new(Component::new(ComponentType::BaseApp, 2), 1.0),
        ];
        assert_eq!(
            watcher.resolve_targets(TARGET_LEAST_LOADED, &later_loaded),
            vec![Component::new(ComponentType::BaseApp, 1)]
        );

        // An empty registry has no least-loaded app.
        assert!(watcher.resolve_targets(TARGET_LEAST_LOADED, &[]).is_empty());
    }

    /// `split_forwarding_path` (watcher_forwarding.cpp:81-108) splits on
    /// the first `/` and refuses a missing or trailing separator.
    #[test]
    fn the_forwarding_path_splits_on_the_first_separator() {
        assert_eq!(
            split_forwarding_path("cellApps/command/addBot"),
            Some(("cellApps", "command/addBot"))
        );
        assert_eq!(
            split_forwarding_path("1,2,45,87/command/addBot"),
            Some(("1,2,45,87", "command/addBot"))
        );
        assert_eq!(
            split_forwarding_path("leastLoaded//value"),
            Some(("leastLoaded", "/value"))
        );
        // No separator: "a bad destination path".
        assert_eq!(split_forwarding_path("command"), None);
        // Trailing separator: no destination watcher path remains.
        assert_eq!(split_forwarding_path("cellApps/"), None);
        assert_eq!(split_forwarding_path(""), None);
    }

    /// `getComponentIDList` (watcher_forwarding.cpp:26-70) parses the
    /// comma-separated ids with `strtol` semantics: whitespace and a sign
    /// are allowed, trailing junk is not, and bad tokens are skipped.
    #[test]
    fn component_id_lists_parse_like_strtol() {
        assert_eq!(get_component_id_list("1,2,45,87"), vec![1, 2, 45, 87]);
        assert_eq!(get_component_id_list("1, 2, 3"), vec![1, 2, 3]);
        assert_eq!(get_component_id_list(" 7"), vec![7]);
        // A trailing space leaves `*endPtr != '\0'`, so the token is refused.
        assert_eq!(get_component_id_list(" 7 "), Vec::<i32>::new());
        assert_eq!(get_component_id_list("+7,-8"), vec![7, -8]);
        assert_eq!(get_component_id_list(",1,,3,"), vec![1, 3]);
        assert_eq!(get_component_id_list(""), Vec::<i32>::new());
        // Trailing junk after the number: skipped, not fatal.
        assert_eq!(get_component_id_list("1,abc,3"), vec![1, 3]);
        assert_eq!(get_component_id_list("13abc"), Vec::<i32>::new());
        assert_eq!(get_component_id_list("1e3"), Vec::<i32>::new());
        assert_eq!(get_component_id_list("0x10"), Vec::<i32>::new());
        // The `long` extremes are rejected.
        assert_eq!(
            get_component_id_list("9223372036854775807"),
            Vec::<i32>::new()
        );
        assert_eq!(
            get_component_id_list("9223372036854775808"),
            Vec::<i32>::new()
        );
        assert_eq!(
            get_component_id_list("-9223372036854775808"),
            Vec::<i32>::new()
        );
        // A value that fits a `long` but not an `int` truncates.
        assert_eq!(get_component_id_list("4294967296"), vec![0]);
    }

    /// `ExposeHints` (watcher_forwarding.hpp:28-37) and `ComponentType`
    /// (watcher_forwarding_types.hpp:8-13) carry the reference's protocol
    /// discriminants.
    #[test]
    fn expose_hints_and_component_types_keep_their_discriminants() {
        assert_eq!(ExposeHints::WithEntity as u8, 0);
        assert_eq!(ExposeHints::CellApps as u8, 1);
        assert_eq!(ExposeHints::WithSpace as u8, 2);
        assert_eq!(ExposeHints::LeastLoaded as u8, 3);
        assert_eq!(ExposeHints::LocalOnly as u8, 4);
        assert_eq!(ExposeHints::BaseApps as u8, 5);
        assert_eq!(ExposeHints::ServiceApps as u8, 6);
        assert_eq!(ExposeHints::BaseServiceApps as u8, 7);
        assert_eq!(ComponentType::CellApp as u8, 0);
        assert_eq!(ComponentType::BaseApp as u8, 1);
        assert_eq!(ComponentType::ServiceApp as u8, 2);
        assert_eq!(ComponentType::CellApp.as_int(), 0);
        assert_eq!(ComponentType::ServiceApp.as_int(), 2);
    }

    /// `ForwardingWatcher::getAsString`/`setFromString`
    /// (watcher_forwarding.hpp:74-95) are refused: the version-1 protocol
    /// is unsupported.
    #[test]
    fn the_version_one_protocol_is_refused() {
        let mut watcher = ForwardingWatcher::new(ManagerKind::CellApp, "ca");
        assert_eq!(watcher.get_as_string(""), None);
        assert_eq!(watcher.get_as_string("command/addBot"), None);
        assert!(!watcher.set_string("", "1"));
        assert!(!watcher.set_string("command/addBot", "1"));
    }

    /// `ForwardingWatcher::getAsStream` (watcher_forwarding.cpp:112-131):
    /// the empty path reports `Forwarding Watcher`, the `__doc__` path the
    /// comment, and anything else fails.
    #[test]
    fn get_as_stream_reports_the_forwarding_watcher() {
        let watcher = ForwardingWatcher::new(ManagerKind::CellApp, "ca");
        assert_eq!(
            watcher.get_as_stream(""),
            Some(("Forwarding Watcher".to_string(), Mode::ReadOnly))
        );
        assert_eq!(
            watcher.get_as_stream("__doc__"),
            Some(("ca".to_string(), Mode::ReadOnly))
        );
        assert_eq!(watcher.get_as_stream("command/addBot"), None);
    }

    /// `is_empty_path`/`is_docPath` (watcher.hpp:945-954) match only
    /// their exact paths.
    #[test]
    fn the_special_paths_match_only_themselves() {
        assert!(is_empty_path(""));
        assert!(!is_empty_path("x"));
        assert!(is_doc_path("__doc__"));
        assert!(!is_doc_path("__args__"));
        assert!(!is_doc_path(""));
    }

    /// The acceptance: a forwarded request to several nodes merges their
    /// replies into one result stream — each component's tuple carries its
    /// id, its result and its type, and the stdio outputs concatenate
    /// (`checkSatisfied`, watcher_forwarding_collector.cpp:103-166).
    #[test]
    fn multi_node_replies_merge_into_one_result() {
        let mut collector = ForwardingCollector::new("command/addBot");
        let value = [1u8, 2, 3];
        assert!(collector.start(&value, 2));
        assert_eq!(collector.query_path(), "command/addBot");
        assert!(collector.check_satisfied().is_none());

        let first = Component::new(ComponentType::CellApp, 1);
        let second = Component::new(ComponentType::CellApp, 2);
        let mut first_reply = component_reply_stream(1, 11, "hello\n");
        let mut second_reply = component_reply_stream(2, 22, "world\n");
        collector.handle_response(first, &mut first_reply).unwrap();
        // Still one outstanding reply.
        assert!(collector.check_satisfied().is_none());
        collector
            .handle_response(second, &mut second_reply)
            .unwrap();

        let merged = collector.check_satisfied().expect("all replies in");
        assert_eq!(collector.tuple_count(), 2);
        assert_eq!(collector.output(), "hello\nworld\n");

        // The merged stream is TUPLE(STRING(output), TUPLE[tuple_count,
        // per-component tuples]); parse it back to prove the shape.
        let (output, components) = read_merged_result(&merged);
        assert_eq!(output, "hello\nworld\n");
        assert_eq!(
            components,
            vec![(1, ComponentType::CellApp), (2, ComponentType::CellApp),]
        );
    }

    /// Base and service app replies merge the same way
    /// (watcher_forwarding_collector.cpp:331-372), each carrying its own
    /// component type.
    #[test]
    fn base_and_service_apps_merge_the_same_way() {
        let mut collector = ForwardingCollector::new("command/addBot");
        assert!(collector.start(&[1], 2));
        let base = Component::new(ComponentType::BaseApp, 10);
        let service = Component::new(ComponentType::ServiceApp, 11);
        collector
            .handle_response(base, &mut component_reply_stream(10, 11, "base\n"))
            .unwrap();
        collector
            .handle_response(service, &mut component_reply_stream(11, 12, "service"))
            .unwrap();
        let merged = collector.check_satisfied().unwrap();
        let (output, components) = read_merged_result(&merged);
        assert_eq!(output, "base\nservice\n");
        assert_eq!(
            components,
            vec![
                (10, ComponentType::BaseApp),
                (11, ComponentType::ServiceApp),
            ]
        );
    }

    /// `handleException` (watcher_forwarding_collector.cpp:239-259) drops
    /// a component's contribution without a tuple, but still counts it
    /// towards satisfaction.
    #[test]
    fn a_failed_component_is_skipped_but_still_counts() {
        let mut collector = ForwardingCollector::new("command/addBot");
        assert!(collector.start(&[1], 2));
        let first = Component::new(ComponentType::CellApp, 1);
        let mut reply = component_reply_stream(1, 11, "ok\n");
        collector.handle_response(first, &mut reply).unwrap();
        collector.handle_exception();
        let merged = collector.check_satisfied().expect("satisfied");
        assert_eq!(collector.tuple_count(), 1);
        assert_eq!(collector.output(), "ok\n");
        assert!(!merged.is_empty());
    }

    /// A malformed reply (not a tuple first) is dropped — no tuple is
    /// merged — but the collector still counts the component towards
    /// satisfaction (watcher_forwarding_collector.cpp:285-301, 197-236).
    #[test]
    fn a_malformed_reply_is_dropped() {
        let mut collector = ForwardingCollector::new("command/addBot");
        assert!(collector.start(&[1], 1));
        let mut bad = BinaryStream::new();
        // An INT instead of the required TUPLE.
        bad.write_u8(WatcherDataType::Int as u8);
        bad.write_u8(Mode::ReadOnly as u8);
        bad.write_packed_int(4).unwrap();
        bad.write_i32(7);
        bad.write_u8(1); // status
        let err = collector
            .handle_response(Component::new(ComponentType::CellApp, 1), &mut bad)
            .unwrap_err();
        assert_eq!(err, DecodeError::FirstElementNotTuple);
        assert_eq!(collector.tuple_count(), 0);
        let merged = collector.check_satisfied().expect("satisfied");
        assert!(!merged.is_empty());
    }

    /// `tupleHandler` (watcher_forwarding_collector.cpp:410-438) refuses a
    /// zero-size tuple and a tuple that is not the two-element function
    /// pair.
    #[test]
    fn a_tuple_header_must_be_a_non_empty_two_element_pair() {
        let component = Component::new(ComponentType::CellApp, 1);

        // Zero size.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(0).unwrap();
        stream.write_packed_int(2).unwrap();
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // Not two entries.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(4).unwrap();
        stream.write_packed_int(3).unwrap();
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // A second element that is not the required output string.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(4).unwrap();
        stream.write_packed_int(2).unwrap();
        stream.write_u8(WatcherDataType::Int as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(4).unwrap();
        stream.write_i32(1);
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::SecondElementNotString
        );
    }

    /// `stringHandler` (watcher_forwarding_collector.cpp:376-404)
    /// normalises the output to end in a newline so any other component
    /// data that is appended is separated, and appends without one when it
    /// already ends in one.
    #[test]
    fn output_strings_are_newline_separated() {
        let component = Component::new(ComponentType::CellApp, 1);
        let mut with_newline = component_reply_stream(1, 11, "ok\n");
        let reply = decode_component_reply(component, &mut with_newline).unwrap();
        assert_eq!(reply.output, "ok\n");

        let mut without = component_reply_stream(1, 11, "ok");
        let reply = decode_component_reply(component, &mut without).unwrap();
        assert_eq!(reply.output, "ok\n");
    }

    /// A truncated stream fails cleanly (the reference sets the stream
    /// error and clamps the read cursor).
    #[test]
    fn a_truncated_reply_fails() {
        let component = Component::new(ComponentType::CellApp, 1);
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        // Truncated: no mode byte.
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // Truncated: no bytes at all.
        let mut empty = BinaryStream::new();
        assert_eq!(
            decode_component_reply(component, &mut empty).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // A truncated payload after the tuple header.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(4).unwrap();
        stream.write_packed_int(2).unwrap();
        stream.write_u8(WatcherDataType::String as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(99).unwrap();
        stream.write_bytes(b"short");
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::SecondElementNotString
        );
    }

    /// The base decoder's fall-throughs (watcher_protocol.cpp:34-90): an
    /// unknown type byte goes to `defaultHandler`, a string as the first
    /// element is refused, and a malformed tuple header is refused.
    #[test]
    fn decode_next_dispatches_and_rejects_malformed_elements() {
        let component = Component::new(ComponentType::CellApp, 1);

        // An unknown type byte (9) falls through to `defaultHandler`, which
        // skips the blob but leaves the state at EXPECTING_TUPLE.
        let mut stream = BinaryStream::new();
        stream.write_u8(9);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(1).unwrap();
        stream.write_u8(0xFF);
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // `defaultHandler` with no size byte to read.
        let mut stream = BinaryStream::new();
        stream.write_u8(9);
        stream.write_u8(Mode::ReadOnly as u8);
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // `defaultHandler` with a size past the end of the stream.
        let mut stream = BinaryStream::new();
        stream.write_u8(9);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(10).unwrap();
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // A string as the first element is refused by `stringHandler`
        // (the state is EXPECTING_TUPLE, not EXPECTING_OUTPUT).
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::String as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(1).unwrap();
        stream.write_u8(b'x');
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // A tuple header with no size byte.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // A tuple header with a size but no count byte.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(4).unwrap();
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::FirstElementNotTuple
        );

        // A tuple as the second element (after the first tuple) is refused
        // by `tupleHandler` — the state is EXPECTING_OUTPUT, not
        // EXPECTING_TUPLE.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(4).unwrap();
        stream.write_packed_int(2).unwrap();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        stream.write_packed_int(4).unwrap();
        stream.write_packed_int(2).unwrap();
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::SecondElementNotString
        );
    }

    /// `packResult` requires the EXPECTING_ANY state and at least the final
    /// status byte (watcher_forwarding_collector.cpp:331-372); a reply that
    /// ends right after the output string is refused.
    #[test]
    fn pack_result_requires_any_state_and_a_status_byte() {
        let component = Component::new(ComponentType::CellApp, 1);

        // A reply with a tuple and output string but no trailing status
        // byte: `packResult` sees `remaining() == 0`.
        let mut stream = BinaryStream::new();
        stream.write_u8(WatcherDataType::Tuple as u8);
        stream.write_u8(Mode::ReadOnly as u8);
        let mut tuple = BinaryStream::new();
        tuple.write_packed_int(2).unwrap();
        write_watcher_string(&mut tuple, "ok\n", Mode::ReadOnly);
        // The value entry is missing entirely: no bytes after the string.
        stream.write_packed_int(tuple.length() as u32).unwrap();
        stream.write_blob_raw(tuple.as_bytes());
        assert_eq!(
            decode_component_reply(component, &mut stream).unwrap_err(),
            DecodeError::Stream
        );

        // `packResult` in a state other than EXPECTING_ANY is refused — the
        // decoder's defensive branch.
        let mut decoder = ResponseDecoder::new(component);
        let mut empty = BinaryStream::new();
        assert_eq!(
            decoder.pack_result(&mut empty).unwrap_err(),
            DecodeError::Stream
        );
    }

    /// `start` (watcher_forwarding_collector.cpp:76-140) refuses an empty
    /// value stream ("No data provided to forward"), and a reply with
    /// nothing outstanding is an error.
    #[test]
    fn start_refuses_empty_value() {
        let mut collector = ForwardingCollector::new("command/addBot");
        assert!(!collector.start(&[], 1));
        assert!(collector.start(&[1], 2));
        assert_eq!(collector.tuple_count(), 0);

        // A reply with nothing outstanding is refused.
        let mut collector = ForwardingCollector::new("command/addBot");
        let mut reply = component_reply_stream(1, 11, "ok\n");
        assert_eq!(
            collector
                .handle_response(Component::new(ComponentType::CellApp, 1), &mut reply)
                .unwrap_err(),
            DecodeError::Stream
        );
    }

    /// `handle_exception` with no pending request is a no-op; the collector
    /// is satisfied immediately with an empty (zero-tuple) result.
    #[test]
    fn a_stray_exception_is_ignored() {
        let mut collector = ForwardingCollector::new("command/addBot");
        collector.handle_exception();
        assert!(collector.check_satisfied().is_some());
        assert_eq!(collector.tuple_count(), 0);
    }

    /// A component reply of the exact shape the reference's
    /// `CallableWatcher::startResultStream` (watcher.hpp:687-713) produces:
    /// `TUPLE(STRING(output), INT(value))` plus the trailing status byte.
    fn component_reply_stream(id: ComponentID, value: i32, output: &str) -> BinaryStream {
        let mut reply = BinaryStream::new();
        reply.write_u8(WatcherDataType::Tuple as u8);
        reply.write_u8(Mode::ReadOnly as u8);
        let mut tuple = BinaryStream::new();
        tuple.write_packed_int(2).unwrap();
        write_watcher_string(&mut tuple, output, Mode::ReadOnly);
        write_watcher_int(&mut tuple, value, Mode::ReadOnly);
        reply.write_packed_int(tuple.length() as u32).unwrap();
        reply.write_blob_raw(tuple.as_bytes());
        reply.write_u8(1); // the final status byte
        let _ = id;
        reply
    }

    /// Parse a merged result stream back out: the concatenated stdio output
    /// and the `(component id, component type)` of each per-component tuple
    /// (the `checkSatisfied` shape, watcher_forwarding_collector.cpp:103-166).
    fn read_merged_result(merged: &[u8]) -> (String, Vec<(ComponentID, ComponentType)>) {
        let mut stream = BinaryStream::new();
        stream.write_blob_raw(merged);
        assert_eq!(stream.read_u8().unwrap(), WatcherDataType::Tuple as u8);
        assert_eq!(stream.read_u8().unwrap(), Mode::ReadOnly as u8);
        let size = stream.read_packed_int().unwrap() as usize;
        let mut tmp = BinaryStream::new();
        tmp.write_blob_raw(&stream.read_blob_raw(size).unwrap());
        assert_eq!(tmp.read_packed_int().unwrap(), 2);
        assert_eq!(tmp.read_u8().unwrap(), WatcherDataType::String as u8);
        assert_eq!(tmp.read_u8().unwrap(), Mode::ReadOnly as u8);
        let output = tmp.read_string().unwrap();
        assert_eq!(tmp.read_u8().unwrap(), WatcherDataType::Tuple as u8);
        assert_eq!(tmp.read_u8().unwrap(), Mode::ReadOnly as u8);
        let result_size = tmp.read_packed_int().unwrap() as usize;
        let tuple_count = tmp.read_packed_int().unwrap();
        let mut result = BinaryStream::new();
        // The result blob is `result_size` minus the packed `tuple_count`
        // (1 byte for any count below 255).
        result.write_blob_raw(&tmp.read_blob_raw(result_size - 1).unwrap());
        let mut components = Vec::new();
        for _ in 0..tuple_count {
            components.push(read_component_tuple(&mut result));
        }
        (output, components)
    }

    /// Read a packed per-component tuple back out of a result stream.
    fn read_component_tuple(stream: &mut BinaryStream) -> (ComponentID, ComponentType) {
        assert_eq!(stream.read_u8().unwrap(), WatcherDataType::Tuple as u8);
        assert_eq!(stream.read_u8().unwrap(), Mode::ReadOnly as u8);
        let size = stream.read_packed_int().unwrap() as usize;
        let mut temp = BinaryStream::new();
        temp.write_blob_raw(&stream.read_blob_raw(size).unwrap());
        assert_eq!(temp.read_packed_int().unwrap(), 3);
        assert_eq!(temp.read_u8().unwrap(), WatcherDataType::Int as u8);
        assert_eq!(temp.read_u8().unwrap(), Mode::ReadOnly as u8);
        assert_eq!(temp.read_packed_int().unwrap(), 4);
        let id = temp.read_i32().unwrap();
        // The remaining bytes after the id are the payload plus the trailing
        // int (type + mode + packed length + 4-byte value = 7 bytes).
        let payload_len = temp.remaining() - 7;
        let _payload = temp.read_blob_raw(payload_len).unwrap();
        assert_eq!(temp.read_u8().unwrap(), WatcherDataType::Int as u8);
        assert_eq!(temp.read_u8().unwrap(), Mode::ReadOnly as u8);
        assert_eq!(temp.read_packed_int().unwrap(), 4);
        let component_type = match temp.read_i32().unwrap() {
            0 => ComponentType::CellApp,
            1 => ComponentType::BaseApp,
            _ => ComponentType::ServiceApp,
        };
        (id, component_type)
    }
}
