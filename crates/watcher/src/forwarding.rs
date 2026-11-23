//! The forwarding watcher family (`lib/server/watcher_forwarding*.hpp/cpp`):
//! the watcher a component manager mounts so one request path fans out to
//! the components it manages -- `cellApps/some/path`, `leastLoaded/...`,
//! `1,2,45,87/...` -- and the collector that aggregates their replies into
//! one tuple result.
//!
//! # Port shape
//!
//! The reference drives the fan-out over Mercury (`UDPBundle` requests to
//! component addresses, replies arriving on the nub). The network stack is
//! out of this crate's scope, so the delivery seam is
//! [`ForwardingTransport`]: one call per component returning that
//! component's reply bytes (or `None`, the reference's `handleException`
//! arm). The synchronous call keeps the collector's aggregation, decoding
//! and `checkSatisfied` shaping byte-for-byte; swapping the trait for real
//! Mercury delivery is a matter of re-entrancy, not encoding.

use std::cell::RefCell;
use std::rc::Rc;

use crate::path_request::WatcherPathRequestV2;
use crate::stream::{
    MODE_READ_ONLY, MemoryIStream, MemoryOStream, StreamValue, TYPE_STRING, TYPE_TUPLE,
    calculate_packed_int_size, value_to_stream,
};
use crate::{Mode, Watcher};

/// `ComponentType` (`watcher_forwarding_types.hpp:8-13`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComponentType {
    /// `CELL_APP`.
    CellApp,
    /// `BASE_APP`.
    BaseApp,
    /// `SERVICE_APP`.
    ServiceApp,
}

/// `std::pair<ComponentType, ComponentID>`
/// (`watcher_forwarding_types.hpp:19`): the routing identity a forwarding
/// request is delivered to. The reference pairs it with a Mercury address;
/// the port's [`ForwardingTransport`] owns the delivery mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentTarget {
    /// `.first`: the kind of component.
    pub component_type: ComponentType,
    /// `.second`: the component ID (`int32`).
    pub id: i32,
}

/// The delivery seam where the reference uses Mercury: one `callWatcher`
/// request to one component -- the destination watcher path plus the
/// forwarded payload -- and that component's reply bytes back. `None` is
/// the reference's `handleException` arm: the request failed to deliver or
/// the component never answered, and the collector simply stops expecting
/// that reply.
pub trait ForwardingTransport {
    /// Deliver one forwarding request; the reply payload on success.
    fn send(&mut self, target: &ComponentTarget, dest_path: &str, data: &[u8]) -> Option<Vec<u8>>;
}

/// `ForwardingWatcher::TARGET_CELL_APPS` (`watcher_forwarding.cpp:11`).
pub const TARGET_CELL_APPS: &str = "cellApps";
/// `ForwardingWatcher::TARGET_BASE_APPS` (`watcher_forwarding.cpp:12`).
pub const TARGET_BASE_APPS: &str = "baseApps";
/// `ForwardingWatcher::TARGET_SERVICE_APPS` (`watcher_forwarding.cpp:13`).
pub const TARGET_SERVICE_APPS: &str = "serviceApps";
/// `ForwardingWatcher::TARGET_BASE_SERVICE_APPS`
/// (`watcher_forwarding.cpp:14`).
pub const TARGET_BASE_SERVICE_APPS: &str = "baseServiceApps";
/// `ForwardingWatcher::TARGET_LEAST_LOADED` (`watcher_forwarding.cpp:15`).
pub const TARGET_LEAST_LOADED: &str = "leastLoaded";

/// `ForwardingWatcher::getComponentIDList` (`watcher_forwarding.cpp:26-61`):
/// parse a comma-separated component ID list with `strtol` semantics --
/// leading whitespace skipped, a sign and digit run consumed, the whole
/// token must convert, and `LONG_MIN`/`LONG_MAX` are rejected. Accepted
/// values truncate into `int32` as the reference's cast does.
pub fn get_component_id_list(target_info: &str) -> Vec<i32> {
    let mut components = Vec::new();
    for token in target_info.split(',') {
        if token.is_empty() {
            // `strtok` collapses runs of separators.
            continue;
        }

        // The `strtol` shape: optional leading whitespace and sign, then a
        // digit run that must reach the end of the token.
        let token = token.trim_start();
        let bytes = token.as_bytes();
        let mut pos = 0;
        if pos < bytes.len() && (bytes[pos] == b'+' || bytes[pos] == b'-') {
            pos += 1;
        }
        let digits_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == digits_start || pos != bytes.len() {
            // No digits, or trailing junk: the ERROR_MSG arm.
            continue;
        }

        if let Ok(result) = token.parse::<i64>()
            && result != i64::MIN
            && result != i64::MAX
        {
            // Conversion worked so add the component ID to the list.
            components.push(result as i32);
        }
    }
    components
}

/// `ForwardingWatcher` (`watcher_forwarding.hpp:19-102`): the watcher the
/// component managers mount. The reference is subclassed per manager
/// (`CAForwardingWatcher`/`BAForwardingWatcher`) to resolve the target
/// string into component addresses; the port takes the resolver as a
/// closure alongside the transport.
/// The manager-side target resolver: which components a target string
/// selects. The reference subclasses per manager to supply this.
pub type ComponentLister = Box<dyn Fn(&str) -> Vec<ComponentTarget>>;

pub struct ForwardingWatcher {
    transport: Rc<RefCell<dyn ForwardingTransport>>,
    /// The manager-side `getComponentAddressList`: which managed
    /// components a target string selects.
    list_components: ComponentLister,
    comment: String,
}

impl ForwardingWatcher {
    /// Build one with the manager's target resolver and delivery transport.
    pub fn new(
        transport: Rc<RefCell<dyn ForwardingTransport>>,
        list_components: ComponentLister,
    ) -> Self {
        Self {
            transport,
            list_components,
            comment: String::new(),
        }
    }
}

impl Watcher for ForwardingWatcher {
    fn get_as_string(
        &self,
        _path: &str,
        _result: &mut String,
        _desc: &mut String,
        _mode: &mut Mode,
    ) -> bool {
        // The forwarding watcher doesn't support the version 1 protocol.
        false
    }

    fn set_from_string(&mut self, _path: &str, _value: &str) -> bool {
        // The forwarding watcher doesn't support the version 1 protocol.
        false
    }

    /// `ForwardingWatcher::setFromStream` (`watcher_forwarding.cpp:65-112`).
    fn set_from_stream(&mut self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        // Our watcher path should now look like;
        // 'all/command/addBot'
        // 'leastLoaded/command/addBot'
        // '1,2,45,87/command/addBot'
        //
        // Separate on the first '/'
        let Some(pos) = path.find('/') else {
            // ERROR_MSG: appear to have received a bad destination path.
            return false;
        };

        // If the separator was the final thing in the string then there is
        // no remaining watcher path to use on the receiving component side.
        if pos + 1 >= path.len() {
            // ERROR_MSG: no destination watcher path provided.
            return false;
        }

        // Now generate a new string containing the destination target(s).
        let target_info = &path[..pos];
        let dest_watcher = &path[pos + 1..];

        // Specialised collector initialisation: the reference's
        // `newCollector` builds the collector and resolves the address
        // list; a failed init answers NULL before anything starts.
        let mut collector = ForwardingCollector::new(Rc::clone(&self.transport), dest_watcher);
        let address_list = (self.list_components)(target_info);
        if !collector.init(address_list) {
            return false;
        }

        collector.start(request)
    }

    /// `ForwardingWatcher::getAsStream` (`watcher_forwarding.cpp:116-136`).
    fn get_as_stream(&self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        if path.is_empty() {
            let mode = Mode::ReadOnly;
            value_to_stream(
                request.result_mut(),
                &StreamValue::Str("Forwarding Watcher".to_string()),
                crate::stream::mode_byte(mode),
            );
            request.set_result_value(&self.comment, mode);
            true
        } else if path == "__doc__" {
            let mode = Mode::ReadOnly;
            value_to_stream(
                request.result_mut(),
                &StreamValue::Str(self.comment.clone()),
                crate::stream::mode_byte(mode),
            );
            request.set_result_value(&self.comment, mode);
            true
        } else {
            false
        }
    }

    fn comment(&self) -> &str {
        &self.comment
    }

    fn set_comment(&mut self, comment: String) {
        self.comment = comment;
    }
}

/// `ForwardingCollector` (`watcher_forwarding_collector.hpp:36-116`):
/// sends one forwarding request to every target and aggregates the replies
/// onto the parent request's result stream as a packed tuple. The reference
/// lives across Mercury's reply callbacks and deletes itself in
/// `checkSatisfied`; the port's synchronous transport lets one `start`
/// drive the whole cycle.
pub struct ForwardingCollector {
    transport: Rc<RefCell<dyn ForwardingTransport>>,
    /// The watcher path to call on each component.
    query_path: String,
    /// The components to forward to (`init`'s `AddressList`).
    address_list: Vec<ComponentTarget>,
    /// Are we currently sending out messages.
    calling_components: bool,
    /// Number of outstanding replies we're expecting.
    pending_requests: u32,
    /// All the data from stdout/stderr.
    output_str: String,
    /// Packed result tuple data.
    result_stream: MemoryOStream,
    /// Number of tuple entries.
    tuple_count: u32,
}

impl ForwardingCollector {
    /// `ForwardingCollector::ForwardingCollector`
    /// (`watcher_forwarding_collector.cpp:28-37`). The reference takes the
    /// parent path request here; the port receives it per call so the
    /// synchronous cycle never holds a borrow across delivery.
    pub fn new(transport: Rc<RefCell<dyn ForwardingTransport>>, dest_watcher: &str) -> Self {
        Self {
            transport,
            query_path: dest_watcher.to_string(),
            address_list: Vec::new(),
            calling_components: false,
            pending_requests: 0,
            output_str: String::new(),
            result_stream: MemoryOStream::new(),
            tuple_count: 0,
        }
    }

    /// `ForwardingCollector::init` (`watcher_forwarding_collector.cpp:67-82`):
    /// claim the target list. An empty list fails the way the reference's
    /// does (the interface element and network interface halves fold into
    /// the transport).
    pub fn init(&mut self, address_list: Vec<ComponentTarget>) -> bool {
        if address_list.is_empty() {
            // ERROR_MSG: failed to initialise due to empty target list.
            return false;
        }
        self.address_list = address_list;
        true
    }

    /// `ForwardingCollector::start` (`watcher_forwarding_collector.cpp:91-144`):
    /// forward the request's remaining payload to every target, then
    /// aggregate.
    pub fn start(&mut self, request: &mut WatcherPathRequestV2) -> bool {
        if self.calling_components {
            // ERROR_MSG: failed to start, already calling components.
            return false;
        }
        self.calling_components = true;

        // The data to forward is everything left on the request's value
        // stream.
        let forwarding_data = match request.value_data() {
            Some(data) => data.to_vec(),
            None => {
                // The forwarding path is only entered from set requests;
                // without a payload there is nothing to forward (the
                // reference would follow a null stream here).
                return false;
            }
        };
        if forwarding_data.is_empty() {
            // ERROR_MSG: no data provided to forward.
            return false;
        }

        let targets = self.address_list.clone();
        for target in targets {
            self.pending_requests += 1;

            // We need to stream on the request: the path to the watcher
            // function, then the data we are forwarding. The reply (or its
            // delivery failure) comes back through the transport; the
            // reference receives both as Mercury callbacks.
            let reply =
                self.transport
                    .borrow_mut()
                    .send(&target, &self.query_path, &forwarding_data);
            match reply {
                Some(reply) => self.handle_reply(&target, &reply),
                None => self.handle_exception(),
            }
        }

        self.calling_components = false;
        self.check_satisfied(request);

        true
    }

    /// `ForwardingCollector::handleMessage`
    /// (`watcher_forwarding_collector.cpp:196-236`): one component's reply.
    /// The address-list search folds away -- the synchronous loop knows
    /// which component it just asked.
    fn handle_reply(&mut self, target: &ComponentTarget, data: &[u8]) {
        self.pending_requests -= 1;

        // Push this off to the stream decoder.
        let mut output = std::mem::take(&mut self.output_str);
        let mut result = std::mem::take(&mut self.result_stream);
        let mut stream = MemoryIStream::new(data);
        let mut decoder = ResponseDecoder {
            component: *target,
            state: WorkState::ExpectingTuple,
            output_str: &mut output,
            result_stream: &mut result,
        };
        if decoder.decode(&mut stream) {
            // We have another complete result tuple entry.
            self.tuple_count += 1;

            // The final element of the stream should be a boolean status.
            let _status = stream.read_bool();
        }
        self.output_str = output;
        self.result_stream = result;

        // The reference calls checkSatisfied here; the synchronous loop
        // runs it once from `start` when every reply is in.
    }

    /// `ForwardingCollector::handleException`
    /// (`watcher_forwarding_collector.cpp:240-261`): a reply that will
    /// never arrive.
    fn handle_exception(&mut self) {
        // Stream error response into our result_ stream.
        self.pending_requests -= 1;
    }

    /// `ForwardingCollector::checkSatisfied`
    /// (`watcher_forwarding_collector.cpp:151-187`): with every reply in,
    /// pack the aggregated `(output, result)` tuple onto the parent
    /// request's stream and complete it.
    fn check_satisfied(&mut self, request: &mut WatcherPathRequestV2) {
        if self.calling_components || self.pending_requests > 0 {
            return;
        }

        let mut tmp_stream = MemoryOStream::new();
        tmp_stream.write_string_length(2); // pair of output string / result
        tmp_stream.write_u8(TYPE_STRING);
        tmp_stream.write_u8(MODE_READ_ONLY);
        tmp_stream.write_string(&self.output_str); // The output string

        // Now the result data.
        tmp_stream.write_u8(TYPE_TUPLE);
        tmp_stream.write_u8(MODE_READ_ONLY);
        tmp_stream.write_string_length(
            (self.result_stream.size() + calculate_packed_int_size(self.tuple_count as i32)) as i32,
        );
        tmp_stream.write_string_length(self.tuple_count as i32); // Tuple entry count
        tmp_stream.add_blob(self.result_stream.data());

        // We now have the contained data packed so add it to the parent.
        let parent_stream = request.result_mut();
        parent_stream.write_u8(TYPE_TUPLE);
        parent_stream.write_u8(MODE_READ_ONLY);
        parent_stream.write_string_length(tmp_stream.size() as i32);
        parent_stream.add_blob(tmp_stream.data());

        // Notify the path request that the result has been filled.
        request.set_result_value("", Mode::ReadOnly);
    }
}

/// `ForwardingCollector::ResponseDecoder::WorkState`
/// (`watcher_forwarding_collector.hpp:85-90`).
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // the names mirror the reference's
enum WorkState {
    /// Should process FUNC_TUPLE next.
    ExpectingTuple,
    /// Expecting output stream now.
    ExpectingOutput,
    /// Any result could come on the stream now.
    ExpectingAny,
}

/// `ForwardingCollector::ResponseDecoder`
/// (`watcher_forwarding_collector.cpp:264-403`), with the
/// `WatcherProtocolDecoder` base's type dispatch folded in
/// (`watcher_protocol.cpp:23-79`).
struct ResponseDecoder<'a> {
    /// The component of this decode.
    component: ComponentTarget,
    /// Current state processing indicator.
    state: WorkState,
    /// All the data from stdout/stderr.
    output_str: &'a mut String,
    /// Packed result tuple data.
    result_stream: &'a mut MemoryOStream,
}

impl ResponseDecoder<'_> {
    /// `ResponseDecoder::decode` (`watcher_forwarding_collector.cpp:278-295`):
    /// a tuple header, then the output string, then the raw result.
    fn decode(&mut self, stream: &mut MemoryIStream) -> bool {
        if !self.decode_next(stream) || self.state != WorkState::ExpectingOutput {
            // ERROR_MSG: first element of component response not a tuple.
            return false;
        }

        if !self.decode_next(stream) || self.state != WorkState::ExpectingAny {
            // ERROR_MSG: second element of component response not a string.
            return false;
        }

        self.pack_result(stream)
    }

    /// `WatcherProtocolDecoder::decodeNext` (`watcher_protocol.cpp:23-79`):
    /// type byte, mode byte, then the typed handler. Only STRING and TUPLE
    /// carry state here; every other type takes the default skip handler.
    fn decode_next(&mut self, stream: &mut MemoryIStream) -> bool {
        let type_byte = stream.read_u8();
        if stream.error() {
            return false;
        }
        let mode = stream.read_u8();
        if stream.error() {
            return false;
        }
        match type_byte {
            TYPE_STRING => self.string_handler(stream),
            TYPE_TUPLE => self.tuple_handler(stream),
            _ => self.default_handler(stream, mode),
        }
    }

    /// `WatcherProtocolDecoder::defaultHandler`
    /// (`watcher_protocol.cpp:88-118`): skip one sized block.
    fn default_handler(&mut self, stream: &mut MemoryIStream, _mode: u8) -> bool {
        let size = stream.read_string_length();
        if stream.error() {
            return false;
        }
        if size < 0 || size as usize > stream.remaining_length() {
            return false;
        }
        stream.retrieve(size as usize);
        !stream.error()
    }

    /// `ResponseDecoder::stringHandler`
    /// (`watcher_forwarding_collector.cpp:342-372`): the stdio output.
    fn string_handler(&mut self, stream: &mut MemoryIStream) -> bool {
        // If we're not expecting output then we've encountered a bad stream.
        if self.state != WorkState::ExpectingOutput {
            return false;
        }

        // Attempt to extract the string and place it on the output stream.
        let mut str = stream.read_string();
        if stream.error() {
            return false;
        }

        // Ensure the final element in the output string is a newline so
        // that any other component data that is appended is separated.
        if !str.ends_with('\n') {
            str.push('\n');
        }

        self.output_str.push_str(&str);

        self.state = WorkState::ExpectingAny;
        true
    }

    /// `ResponseDecoder::tupleHandler`
    /// (`watcher_forwarding_collector.cpp:376-403`): the callable result
    /// tuple header -- a non-zero size and exactly two elements.
    fn tuple_handler(&mut self, stream: &mut MemoryIStream) -> bool {
        // We should only receive a func tuple straight up.
        if self.state != WorkState::ExpectingTuple {
            return false;
        }

        let size = stream.read_string_length();
        if size == 0 || stream.error() {
            return false;
        }

        let count = stream.read_string_length();
        if count != 2 || stream.error() {
            return false;
        }

        // Change our working state.
        self.state = WorkState::ExpectingOutput;
        true
    }

    /// `ResponseDecoder::packResult`
    /// (`watcher_forwarding_collector.cpp:306-338`): the remaining payload
    /// (minus the trailing status byte) becomes one `(id, result, type)`
    /// tuple element on the aggregated result stream.
    fn pack_result(&mut self, stream: &mut MemoryIStream) -> bool {
        if self.state != WorkState::ExpectingAny {
            return false;
        }

        let mut temp_stream = MemoryOStream::new();
        temp_stream.write_string_length(3); // pair of id / result / type

        // Add the component id.
        // NB: the reference streams `component_.second` (int32) through the
        // INT overload, while `component_.first` (the unscoped
        // ComponentType enum) binds the generic STRING template and
        // therefore travels as its rendered number -- its own TODO notes
        // the quirk (`watcher_forwarding_collector.cpp:316-319,328`).
        value_to_stream(
            &mut temp_stream,
            &StreamValue::Int(self.component.id),
            MODE_READ_ONLY,
        );

        // Size of the data should be the rest of the length with the one
        // byte status taken off.
        let size = stream.remaining_length().saturating_sub(1);
        if let Some(blob) = stream.retrieve(size) {
            temp_stream.add_blob(blob);
        }

        // Add the component type.
        value_to_stream(
            &mut temp_stream,
            &StreamValue::Str((self.component.component_type as i32).to_string()),
            MODE_READ_ONLY,
        );

        // Result tuple, ro, size, [count, ...]: now push the component id /
        // result onto the final stream.
        self.result_stream.write_u8(TYPE_TUPLE);
        self.result_stream.write_u8(MODE_READ_ONLY);
        self.result_stream
            .write_string_length(temp_stream.size() as i32);
        self.result_stream.add_blob(temp_stream.data());

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_watcher;
    use crate::fini;
    use crate::stream::{MODE_READ_WRITE, TYPE_INT, TYPE_UNKNOWN};

    /// The delivery log the tests read back after a request.
    type SeenLog = Rc<RefCell<Vec<(ComponentTarget, String, Vec<u8>)>>>;

    /// A scripted transport: records every delivery into the shared log,
    /// answers from a queue.
    struct ScriptedTransport {
        seen: SeenLog,
        replies: Vec<Option<Vec<u8>>>,
    }

    impl ForwardingTransport for ScriptedTransport {
        fn send(
            &mut self,
            target: &ComponentTarget,
            dest_path: &str,
            data: &[u8],
        ) -> Option<Vec<u8>> {
            self.seen
                .borrow_mut()
                .push((*target, dest_path.to_string(), data.to_vec()));
            if self.replies.is_empty() {
                None
            } else {
                self.replies.remove(0)
            }
        }
    }

    /// One component's reply: the callable result tuple
    /// `(output string, raw value stream)` plus the trailing status byte.
    fn component_reply(output: &str, value_stream: &[u8], status: bool) -> Vec<u8> {
        let mut payload = MemoryOStream::new();
        payload.write_string_length(2);
        payload.write_u8(TYPE_STRING);
        payload.write_u8(MODE_READ_ONLY);
        payload.write_string(output);
        payload.add_blob(value_stream);
        let mut reply = MemoryOStream::new();
        reply.write_u8(TYPE_TUPLE);
        reply.write_u8(MODE_READ_ONLY);
        reply.write_string_length(payload.size() as i32);
        reply.add_blob(payload.data());
        reply.write_bool(status);
        reply.data().to_vec()
    }

    /// Decode the parent `(output, results)` tuple of a finished
    /// forwarding request into `(output, entry count)`.
    fn decode_parent(request: &WatcherPathRequestV2) -> (String, i32) {
        let mut input = MemoryIStream::new(request.data());
        let _seq_num = input.read_u32();
        assert_eq!(input.read_u8(), TYPE_TUPLE);
        assert_eq!(input.read_u8(), MODE_READ_ONLY);
        let tmp_len = input.read_string_length() as usize;
        let mut tuple = MemoryIStream::new(input.retrieve(tmp_len).unwrap());
        assert_eq!(tuple.read_string_length(), 2); // output / results pair
        assert_eq!(tuple.read_u8(), TYPE_STRING);
        assert_eq!(tuple.read_u8(), MODE_READ_ONLY);
        let output = tuple.read_string();
        assert_eq!(tuple.read_u8(), TYPE_TUPLE);
        assert_eq!(tuple.read_u8(), MODE_READ_ONLY);
        let _results_size = tuple.read_string_length();
        let entries = tuple.read_string_length();
        (output, entries)
    }

    /// The multi-node routing acceptance: one path fans out to every
    /// selected node, each reply aggregates as one `(id, result, type)`
    /// entry of the `(output, results)` parent tuple.
    #[test]
    fn one_path_routes_to_every_selected_node() {
        fini();
        let targets = vec![
            ComponentTarget {
                component_type: ComponentType::CellApp,
                id: 1,
            },
            ComponentTarget {
                component_type: ComponentType::CellApp,
                id: 2,
            },
        ];
        let seen_log: SeenLog = Rc::new(RefCell::new(Vec::new()));
        let transport: Rc<RefCell<dyn ForwardingTransport>> =
            Rc::new(RefCell::new(ScriptedTransport {
                seen: Rc::clone(&seen_log),
                replies: vec![
                    Some(component_reply(
                        "cell one",
                        &[TYPE_INT, MODE_READ_ONLY, 4, 10, 0, 0, 0],
                        true,
                    )),
                    Some(component_reply(
                        "cell two\n",
                        &[TYPE_INT, MODE_READ_ONLY, 4, 20, 0, 0, 0],
                        true,
                    )),
                ],
            }));
        let watcher = ForwardingWatcher::new(
            Rc::clone(&transport),
            Box::new(move |target| {
                if target == TARGET_CELL_APPS {
                    targets.clone()
                } else {
                    Vec::new()
                }
            }),
        );
        // The reference mounts at "forwardTo"
        // (`baseappmgr.cpp:755`) so the target segment stays in the path
        // this watcher splits apart.
        add_watcher("forwardTo", Rc::new(RefCell::new(watcher)));

        let mut payload = MemoryOStream::new();
        value_to_stream(&mut payload, &StreamValue::Int(9), MODE_READ_WRITE);
        let payload_bytes = payload.data().to_vec();

        let mut request = WatcherPathRequestV2::new("forwardTo/cellApps/command/addBot");
        request.set_sequence_number(1);
        request.set_packet_data(payload_bytes.clone());
        assert!(request.set_watcher_value());
        assert!(request.completed());

        // An unknown target selects no components and fails the set.
        let mut miss = WatcherPathRequestV2::new("forwardTo/nobody/command/addBot");
        miss.set_sequence_number(4);
        miss.set_packet_data(payload_bytes.clone());
        assert!(miss.set_watcher_value());
        assert_eq!(
            miss.data(),
            &[4, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );

        // Every selected node saw the same destination path and payload.
        let seen = seen_log.borrow();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0].0,
            ComponentTarget {
                component_type: ComponentType::CellApp,
                id: 1
            }
        );
        assert_eq!(seen[0].1, "command/addBot");
        assert_eq!(seen[0].2, payload_bytes);
        assert_eq!(seen[1].0.id, 2);
        assert_eq!(seen[1].1, "command/addBot");
        assert_eq!(seen[1].2, payload_bytes);
        drop(seen);

        // The parent answer: per-node entries inside `(output, results)`.
        let mut input = MemoryIStream::new(request.data());
        assert_eq!(input.read_u32(), 1); // sequence number
        assert_eq!(input.read_u8(), TYPE_TUPLE);
        assert_eq!(input.read_u8(), MODE_READ_ONLY);
        let tmp_len = input.read_string_length() as usize;
        let mut tuple = MemoryIStream::new(input.retrieve(tmp_len).unwrap());
        assert_eq!(tuple.read_string_length(), 2);
        assert_eq!(tuple.read_u8(), TYPE_STRING);
        assert_eq!(tuple.read_u8(), MODE_READ_ONLY);
        // The first output gains its separating newline; the second's is kept.
        assert_eq!(tuple.read_string(), "cell one\ncell two\n");
        assert_eq!(tuple.read_u8(), TYPE_TUPLE);
        assert_eq!(tuple.read_u8(), MODE_READ_ONLY);
        let _results_size = tuple.read_string_length();
        assert_eq!(tuple.read_string_length(), 2); // one entry per node
        for (id, value) in [(1, 10u8), (2, 20u8)] {
            assert_eq!(tuple.read_u8(), TYPE_TUPLE);
            assert_eq!(tuple.read_u8(), MODE_READ_ONLY);
            let entry_len = tuple.read_string_length() as usize;
            let mut entry = MemoryIStream::new(tuple.retrieve(entry_len).unwrap());
            assert_eq!(entry.read_string_length(), 3); // id / result / type
            // The component id as INT.
            assert_eq!(entry.read_u8(), TYPE_INT);
            assert_eq!(entry.read_u8(), MODE_READ_ONLY);
            assert_eq!(entry.read_string_length(), 4);
            assert_eq!(entry.read_i32(), id);
            // The forwarded value blob, carried through as raw bytes --
            // the reference's `addBlob` is a plain memcpy with no length
            // prefix (`binary_stream.hpp:119-123`).
            let blob = entry.retrieve(7).unwrap();
            assert_eq!(blob, &[TYPE_INT, MODE_READ_ONLY, 4, value, 0, 0, 0]);
            // The component type as its rendered number, via the generic
            // STRING template overload.
            assert_eq!(entry.read_u8(), TYPE_STRING);
            assert_eq!(entry.read_u8(), MODE_READ_ONLY);
            assert_eq!(entry.read_string(), "0"); // CELL_APP
        }
    }

    /// A component that never answers just lacks a tuple entry.
    #[test]
    fn a_silent_component_just_lacks_a_tuple_entry() {
        fini();
        let seen_log: SeenLog = Rc::new(RefCell::new(Vec::new()));
        let transport: Rc<RefCell<dyn ForwardingTransport>> =
            Rc::new(RefCell::new(ScriptedTransport {
                seen: Rc::clone(&seen_log),
                replies: vec![None],
            }));
        let watcher = ForwardingWatcher::new(
            Rc::clone(&transport),
            Box::new(|target| {
                if target == TARGET_BASE_APPS {
                    vec![ComponentTarget {
                        component_type: ComponentType::BaseApp,
                        id: 42,
                    }]
                } else {
                    Vec::new()
                }
            }),
        );
        add_watcher("mgr", Rc::new(RefCell::new(watcher)));

        let mut payload = MemoryOStream::new();
        value_to_stream(
            &mut payload,
            &StreamValue::Str("go".to_string()),
            MODE_READ_WRITE,
        );
        let mut request = WatcherPathRequestV2::new("mgr/baseApps/command/tick");
        request.set_sequence_number(2);
        request.set_packet_data(payload.data().to_vec());
        assert!(request.set_watcher_value());
        assert!(request.completed());

        // An unknown target selects nothing.
        let mut miss = WatcherPathRequestV2::new("mgr/nobody/command/tick");
        miss.set_sequence_number(3);
        miss.set_packet_data(payload.data().to_vec());
        assert!(miss.set_watcher_value());
        assert_eq!(
            miss.data(),
            &[3, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );

        let seen = seen_log.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.id, 42);
        assert_eq!(seen[0].1, "command/tick");
        drop(seen);

        assert_eq!(decode_parent(&request), (String::new(), 0));
    }

    /// Replies that do not speak the protocol are dropped: the first
    /// element must be the tuple header, the second the output string,
    /// and the string must fit the stream.
    #[test]
    fn malformed_replies_are_dropped() {
        fini();
        // A tuple header then a non-string element.
        let mut wrong_element = MemoryOStream::new();
        wrong_element.write_u8(TYPE_TUPLE);
        wrong_element.write_u8(MODE_READ_ONLY);
        let mut payload = MemoryOStream::new();
        payload.write_string_length(2);
        payload.write_u8(TYPE_INT);
        payload.write_u8(MODE_READ_ONLY);
        payload.write_i32(1);
        wrong_element.write_string_length(payload.size() as i32);
        wrong_element.add_blob(payload.data());
        wrong_element.write_bool(true);
        // A tuple header then an output string longer than what follows.
        let mut truncated = MemoryOStream::new();
        truncated.write_u8(TYPE_TUPLE);
        truncated.write_u8(MODE_READ_ONLY);
        truncated.write_string_length(2);
        truncated.write_string_length(2);
        truncated.write_u8(TYPE_STRING);
        truncated.write_u8(MODE_READ_ONLY);
        truncated.write_string_length(5);
        truncated.add_blob(b"ab");
        truncated.write_bool(true);

        let transport: Rc<RefCell<dyn ForwardingTransport>> =
            Rc::new(RefCell::new(ScriptedTransport {
                seen: Rc::new(RefCell::new(Vec::new())),
                replies: vec![
                    // The first element is not a tuple: the default skip
                    // handler consumes it but the state never advances.
                    Some(vec![TYPE_INT, MODE_READ_ONLY, 4, 1, 0, 0, 0, 1]),
                    Some(wrong_element.data().to_vec()),
                    Some(truncated.data().to_vec()),
                ],
            }));
        let watcher = ForwardingWatcher::new(
            Rc::clone(&transport),
            Box::new(|target| {
                if target == TARGET_CELL_APPS {
                    vec![ComponentTarget {
                        component_type: ComponentType::CellApp,
                        id: 7,
                    }]
                } else {
                    Vec::new()
                }
            }),
        );
        add_watcher("fw", Rc::new(RefCell::new(watcher)));

        let mut payload = MemoryOStream::new();
        value_to_stream(
            &mut payload,
            &StreamValue::Str("go".to_string()),
            MODE_READ_WRITE,
        );
        for leaf in ["a", "b", "c"] {
            let mut request = WatcherPathRequestV2::new(&format!("fw/cellApps/{leaf}"));
            request.set_sequence_number(1);
            request.set_packet_data(payload.data().to_vec());
            assert!(request.set_watcher_value());
            assert!(request.completed());
            assert_eq!(decode_parent(&request), (String::new(), 0));
        }
    }

    /// A comma-separated ID list parses with `strtol` semantics
    /// (`watcher_forwarding.cpp:26-61`).
    #[test]
    fn component_id_lists_parse_with_strtol_semantics() {
        assert_eq!(get_component_id_list("1,2,45,87"), vec![1, 2, 45, 87]);
        // Empty tokens collapse; leading whitespace is skipped.
        assert_eq!(get_component_id_list("1,,2"), vec![1, 2]);
        assert_eq!(get_component_id_list(" 5"), vec![5]);
        // Trailing junk and bare signs fail the whole token.
        assert!(get_component_id_list("5 ").is_empty());
        assert!(get_component_id_list("abc").is_empty());
        assert!(get_component_id_list("+").is_empty());
        // Values overflowing long are rejected; int32 truncation is kept.
        assert!(get_component_id_list("99999999999999999999").is_empty());
        assert!(get_component_id_list("-9223372036854775808").is_empty());
        assert_eq!(get_component_id_list("3000000000"), vec![-1294967296]);
        assert!(get_component_id_list("").is_empty());
    }

    /// The path split and target resolution gates
    /// (`watcher_forwarding.cpp:65-112`).
    #[test]
    fn forwarding_paths_need_a_separator_and_a_tail() {
        let transport: Rc<RefCell<dyn ForwardingTransport>> =
            Rc::new(RefCell::new(ScriptedTransport {
                seen: Rc::new(RefCell::new(Vec::new())),
                replies: vec![None],
            }));
        let mut watcher = ForwardingWatcher::new(
            Rc::clone(&transport),
            Box::new(|target| {
                if target == "all" {
                    vec![ComponentTarget {
                        component_type: ComponentType::BaseApp,
                        id: 1,
                    }]
                } else {
                    Vec::new()
                }
            }),
        );

        let mut request = WatcherPathRequestV2::new("");
        request.set_sequence_number(1);
        request.set_packet_data(vec![TYPE_STRING, MODE_READ_WRITE, 2, b'g', b'o']);

        // No separator, nothing after the separator.
        assert!(!watcher.set_from_stream("nosep", &mut request));
        assert!(!watcher.set_from_stream("tail/", &mut request));
        // An unknown target selects no components.
        assert!(!watcher.set_from_stream("nobody/x", &mut request));
        // A real target with a payload forwards (the transport stays
        // silent, so the answer aggregates empty).
        assert!(watcher.set_from_stream("all/x", &mut request));
        assert!(request.completed());
        assert_eq!(decode_parent(&request), (String::new(), 0));
        // Without a payload there is nothing to forward.
        let mut request = WatcherPathRequestV2::new("");
        assert!(!watcher.set_from_stream("all/x", &mut request));

        assert!(watcher.comment().is_empty());

        // The version 1 protocol is not supported.
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(!watcher.get_as_string("", &mut result, &mut desc, &mut mode));
        assert!(!watcher.set_from_string("", "x"));

        // The self answer and its doc.
        watcher.set_comment("mgr".to_string());
        let mut request = WatcherPathRequestV2::new("");
        assert!(watcher.get_as_stream("", &mut request));
        let mut input = MemoryIStream::new(request.data());
        assert_eq!(input.read_u8(), TYPE_STRING);
        assert_eq!(input.read_u8(), MODE_READ_ONLY);
        assert_eq!(input.read_string(), "Forwarding Watcher");

        let mut request = WatcherPathRequestV2::new("");
        assert!(watcher.get_as_stream("__doc__", &mut request));
        let mut input = MemoryIStream::new(request.data());
        assert_eq!(input.read_u8(), TYPE_STRING);
        assert_eq!(input.read_u8(), MODE_READ_ONLY);
        assert_eq!(input.read_string(), "mgr");

        assert!(!watcher.get_as_stream("__args__", &mut WatcherPathRequestV2::new("")));
    }

    /// Degenerate payloads and protocol degeneracies fail cleanly: an
    /// empty forward payload, a drained reply queue, and every early
    /// exit of the reply decoder.
    #[test]
    fn degenerate_payloads_and_replies_fail_cleanly() {
        fini();
        let seen_log: SeenLog = Rc::new(RefCell::new(Vec::new()));
        // One good reply, then the queue drains to None.
        let transport: Rc<RefCell<dyn ForwardingTransport>> =
            Rc::new(RefCell::new(ScriptedTransport {
                seen: Rc::clone(&seen_log),
                replies: vec![Some(component_reply(
                    "out",
                    &[TYPE_INT, MODE_READ_ONLY, 4, 1, 0, 0, 0],
                    true,
                ))],
            }));
        let watcher = ForwardingWatcher::new(
            Rc::clone(&transport),
            Box::new(|_target| {
                vec![ComponentTarget {
                    component_type: ComponentType::BaseApp,
                    id: 3,
                }]
            }),
        );
        add_watcher("fwd", Rc::new(RefCell::new(watcher)));

        // An empty payload has nothing to forward.
        let mut request = WatcherPathRequestV2::new("fwd/all/x");
        request.set_sequence_number(1);
        request.set_packet_data(Vec::new());
        assert!(request.set_watcher_value());
        assert_eq!(
            request.data(),
            &[1, 0, 0, 0, TYPE_UNKNOWN, MODE_READ_ONLY, 0, 0]
        );

        // The first forward consumes the queued reply.
        let mut payload = MemoryOStream::new();
        value_to_stream(&mut payload, &StreamValue::Int(1), MODE_READ_WRITE);
        let mut request = WatcherPathRequestV2::new("fwd/all/x");
        request.set_sequence_number(2);
        request.set_packet_data(payload.data().to_vec());
        assert!(request.set_watcher_value());
        assert_eq!(decode_parent(&request), ("out\n".to_string(), 1));

        // The second finds the queue drained and aggregates empty.
        let mut request = WatcherPathRequestV2::new("fwd/all/x");
        request.set_sequence_number(3);
        request.set_packet_data(payload.data().to_vec());
        assert!(request.set_watcher_value());
        assert_eq!(decode_parent(&request), (String::new(), 0));

        // Protocol degeneracies, each dropped with zero entries.
        let replies = [
            // Type byte read off an empty stream.
            Vec::new(),
            // Tuple header without a mode byte.
            vec![TYPE_TUPLE],
            // Skip handler without a size byte.
            vec![TYPE_INT, MODE_READ_ONLY],
            // Skip handler with an oversized size.
            vec![TYPE_INT, MODE_READ_ONLY, 0xff, 0xff, 0x00, 0x00],
            // A string as the first element.
            vec![TYPE_STRING, MODE_READ_ONLY, 1, b'x'],
            // A tuple as the second element.
            vec![
                TYPE_TUPLE,
                MODE_READ_ONLY,
                4,
                2,
                TYPE_TUPLE,
                MODE_READ_ONLY,
                2,
                2,
            ],
            // A tuple header with a zero size.
            vec![TYPE_TUPLE, MODE_READ_ONLY, 0, 2],
        ];
        for (idx, reply) in replies.into_iter().enumerate() {
            let transport: Rc<RefCell<dyn ForwardingTransport>> =
                Rc::new(RefCell::new(ScriptedTransport {
                    seen: Rc::new(RefCell::new(Vec::new())),
                    replies: vec![Some(reply)],
                }));
            let watcher = ForwardingWatcher::new(
                Rc::clone(&transport),
                // A target-independent selection: every forwarding path
                // this watcher serves reaches the same component.
                Box::new(|_target| {
                    vec![ComponentTarget {
                        component_type: ComponentType::BaseApp,
                        id: 3,
                    }]
                }),
            );
            let mount = format!("fwd{idx}");
            add_watcher(&mount, Rc::new(RefCell::new(watcher)));
            let mut request = WatcherPathRequestV2::new(&format!("{mount}/all/x"));
            request.set_sequence_number(4);
            request.set_packet_data(payload.data().to_vec());
            assert!(request.set_watcher_value());
            assert_eq!(decode_parent(&request), (String::new(), 0));
        }
    }
}
