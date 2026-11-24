//! BW-17-01-06: the four bots command callables over the watcher
//! protocol (`server/tools/bots/*_callable_watcher.{hpp,cpp}`).
//!
//! The reference hangs four one-parameter callables off the watcher root's
//! `command` directory (`main_app.cpp:1052-1070`), each driving a
//! `MainApp` fleet method:
//!
//! | path                        | parameter | method                       |
//! |-----------------------------|-----------|------------------------------|
//! | `command/addBots`           | INT       | `MainApp::addBots`           |
//! | `command/delBots`           | INT       | `MainApp::delBots`           |
//! | `command/delTaggedEntities` | STRING    | `MainApp::delTaggedEntities` |
//! | `command/updateMovement`    | STRING    | `MainApp::updateMovement`    |
//!
//! Every callable is a `SimpleCallableWatcher(LOCAL_ONLY, ...)` with one
//! `addArg`, and carries two invocation forms:
//!
//! - the protocol 2 call form -- a `TYPE_TUPLE` argument list; the reply
//!   is the `(stdio output, returnValue)` tuple of
//!   `CallableWatcher::startResultStream`/`endResultStream`
//!   (`watcher.cpp:687-726`), where `output` stays `""` and the return
//!   value keeps the base's `"No value"` default (`watcher.cpp:994-995`)
//!   because the bots' `onCall`s never write either;
//! - the pre-2.6 bare-set form -- a plain typed value where a tuple would
//!   be, kept "for backwards compatibility with WebConsole and PyCommon in
//!   pre-BW2.6 releases"; the reply is the echoed value in `READ_WRITE`.
//!
//! The ported watcher crate's [`CallableWatcher`] is the declarative face
//! only -- its protocol 2 machinery has no `onCall` hook yet -- so this
//! module rides [`Watcher::set_from_stream`] directly and rebuilds the
//! reply tuple from the public stream primitives, pinning the reference
//! byte layout.

use std::cell::RefCell;
use std::rc::Rc;

use watcher::stream::{
    MODE_READ_ONLY, MODE_READ_WRITE, MemoryIStream, StreamValue, TYPE_TUPLE,
    calculate_packed_int_size, stream_to_value, stream_to_value_type, value_to_stream,
};
use watcher::{
    CallableWatcher, DataType, ExposeHint, Mode, Watcher, WatcherPathRequestV2, WatcherValue,
};

use crate::fleet::Fleet;

/// One decoded parameter: the INT pair take a count, the STRING pair a
/// tag.
#[derive(Debug, Clone, PartialEq)]
enum Parameter {
    Int(i32),
    Str(String),
}

/// Which `MainApp` method the callable drives
/// (`main_app.cpp:1052-1070`) and the parameter type its `addArg`
/// declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BotCommand {
    AddBots,
    DelBots,
    DelTaggedEntities,
    UpdateMovement,
}

impl BotCommand {
    /// The `WATCHER_TYPE_*` the constructor declares
    /// (`add_bots_callable_watcher.cpp:26`, `del_bots:26`,
    /// `del_tagged_bots:27`, `update_movement:28`).
    fn parameter_type(self) -> DataType {
        match self {
            BotCommand::AddBots | BotCommand::DelBots => DataType::Int,
            BotCommand::DelTaggedEntities | BotCommand::UpdateMovement => DataType::String,
        }
    }

    /// The `MainApp::instance()` call each `onCall` ends with
    /// (`add_bots_callable_watcher.cpp:76`, `del_bots:76`,
    /// `del_tagged_bots:74`, `update_movement:74`).
    fn execute(self, fleet: &mut Fleet, parameter: Parameter) {
        match (self, parameter) {
            (BotCommand::AddBots, Parameter::Int(num)) => fleet.add_bots(num),
            (BotCommand::DelBots, Parameter::Int(num)) => fleet.del_bots(num),
            (BotCommand::DelTaggedEntities, Parameter::Str(tag)) => {
                fleet.del_tagged_entities(&tag);
            }
            (BotCommand::UpdateMovement, Parameter::Str(tag)) => fleet.update_movement(&tag),
            // The declared parameter type decides which arm decodes, so a
            // mismatched pairing is unreachable.
            _ => unreachable!("parameter type matches the command"),
        }
    }
}

/// The bots' one-parameter callable watcher: the declarative
/// [`CallableWatcher`] face over a [`Fleet`] handle, answering the
/// protocol 2 call form and the pre-2.6 bare-set form.
pub struct BotCallableWatcher {
    callable: CallableWatcher,
    command: BotCommand,
    fleet: Rc<RefCell<Fleet>>,
}

impl BotCallableWatcher {
    /// The shared constructor shape: `SimpleCallableWatcher(LOCAL_ONLY,
    /// comment)` plus one `addArg` (`add_bots_callable_watcher.cpp:22-27`).
    fn new(
        fleet: Rc<RefCell<Fleet>>,
        command: BotCommand,
        arg_description: &str,
        comment: &str,
    ) -> Self {
        let mut callable = CallableWatcher::new(ExposeHint::LocalOnly);
        callable.add_arg(command.parameter_type(), arg_description);
        callable.set_comment(comment.to_string());
        Self {
            callable,
            command,
            fleet,
        }
    }

    /// `AddBotsCallableWatcher` (`add_bots_callable_watcher.cpp:22-27`).
    pub fn add_bots(fleet: Rc<RefCell<Fleet>>) -> Self {
        Self::new(
            fleet,
            BotCommand::AddBots,
            "Number of bots to add",
            "Add bots to this process",
        )
    }

    /// `DelBotsCallableWatcher` (`del_bots_callable_watcher.cpp:22-27`).
    pub fn del_bots(fleet: Rc<RefCell<Fleet>>) -> Self {
        Self::new(
            fleet,
            BotCommand::DelBots,
            "Number of bots to delete",
            "Delete bots from this process",
        )
    }

    /// `DelTaggedBotsCallableWatcher`
    /// (`del_tagged_bots_callable_watcher.cpp:23-29`).
    pub fn del_tagged_entities(fleet: Rc<RefCell<Fleet>>) -> Self {
        Self::new(
            fleet,
            BotCommand::DelTaggedEntities,
            "Tag of bots to delete",
            "Delete all bots from this process with the given tag",
        )
    }

    /// `UpdateMovementCallableWatcher`
    /// (`update_movement_callable_watcher.cpp:23-31`); the comment is the
    /// reference text verbatim.
    pub fn update_movement(fleet: Rc<RefCell<Fleet>>) -> Self {
        Self::new(
            fleet,
            BotCommand::UpdateMovement,
            "Tag of bots to update, or empty for all",
            "Update the movement controller of all bots int this process with the given tag",
        )
    }

    /// `onCall` (`add_bots_callable_watcher.cpp:59-79`): reject any count
    /// but one, decode the parameter, drive the fleet.
    fn on_call(&mut self, parameter_count: i32, parameters: &mut MemoryIStream<'_>) -> bool {
        if parameter_count != 1 {
            // ERROR_MSG "Got %d parameters, expecting 1".
            return false;
        }

        // `watcherStreamToValue(parameters, param)`: the type byte, the
        // mode byte, then the payload.
        let decoded = match self.command.parameter_type() {
            DataType::Int => stream_to_value::<i32>(parameters).map(Parameter::Int),
            DataType::String => stream_to_value::<String>(parameters).map(Parameter::Str),
            _ => unreachable!("only Int and String parameters are declared"),
        };
        let Some(parameter) = decoded else {
            // ERROR_MSG "Failed to read parameter".
            return false;
        };

        self.command
            .execute(&mut self.fleet.borrow_mut(), parameter);
        true
    }

    /// The protocol 2 call form -- `SimpleCallableWatcher::setFromStream`
    /// (`watcher.cpp:989-1075`) entered after the `TYPE_TUPLE` byte was
    /// consumed: the declared size, the parameter count, then `onCall`.
    fn call_form(
        &mut self,
        input: &mut MemoryIStream<'_>,
        request: &mut WatcherPathRequestV2,
    ) -> bool {
        let size = input.read_packed_int();
        let remaining = input.remaining_length();
        let parameter_count = input.read_packed_int();

        if input.error() || remaining < size.unsigned_abs() as usize {
            // ERROR_MSG "called with insufficient data".
            input.finish();
            return false;
        }

        // The non-TUPLE rejection (`watcher.cpp:1031-1037`) is only
        // reachable through the base entry; this form is entered for a
        // TUPLE byte alone.

        if !self.on_call(parameter_count, input) {
            // WARNING_MSG "Function call failed".
            input.finish();
            return false;
        }

        if input.remaining_length() != 0 {
            // WARNING_MSG "did not read all argument data" (`watcher.cpp:1048-1051`);
            // the call still succeeds.
        }
        input.finish();

        // The bots' `onCall`s write neither the stdio capture nor the
        // return value, so the reply carries the base defaults
        // (`watcher.cpp:994-995`): `""` and `"No value"`.
        self.reply_with_callable_tuple(request, "", "No value");
        true
    }

    /// The pre-2.6 bare-set form -- the subclass `setFromStream`'s legacy
    /// arm (`add_bots_callable_watcher.cpp:53-78`; the other three are the
    /// same shape): decode the bare value, drive the fleet, echo the
    /// value in `READ_WRITE`, answer.
    fn legacy_form(
        &mut self,
        input: &mut MemoryIStream<'_>,
        type_byte: u8,
        request: &mut WatcherPathRequestV2,
    ) -> bool {
        // `watcherStreamToValue`'s tail after the type byte
        // (`watcher.hpp:226-260`): skip the mode byte
        // (`watcherStreamSkipMode`), then decode the payload
        // (`watcherStreamToValueType`).
        let _mode = input.read_u8();
        let decoded = if input.error() {
            None
        } else {
            stream_to_value_type(input, type_byte)
        };
        let decoded = decoded.and_then(|value| match self.command.parameter_type() {
            DataType::Int => i32::from_stream_value(value).map(Parameter::Int),
            DataType::String => String::from_stream_value(value).map(Parameter::Str),
            _ => unreachable!("only Int and String parameters are declared"),
        });
        let Some(parameter) = decoded else {
            // The decode failure answers a bare false; nothing is written
            // to the result stream (`add_bots_callable_watcher.cpp:62-65`).
            return false;
        };

        self.command
            .execute(&mut self.fleet.borrow_mut(), parameter.clone());

        // Push the value into the reply stream and answer
        // (`add_bots_callable_watcher.cpp:67-75`).
        let echo = match parameter {
            Parameter::Int(num) => StreamValue::Int(num),
            Parameter::Str(tag) => StreamValue::Str(tag),
        };
        value_to_stream(request.result_mut(), &echo, MODE_READ_WRITE);
        request.set_result_value("", Mode::ReadWrite);
        true
    }

    /// `CallableWatcher::startResultStream`, the return value, and
    /// `endResultStream` (`watcher.cpp:687-726`): the reply is the tuple
    /// `(stdio output, returnValue)`. The watcher crate's own copy is
    /// private, so the port rebuilds the byte layout from the public
    /// stream primitives.
    fn reply_with_callable_tuple(
        &self,
        request: &mut WatcherPathRequestV2,
        output: &str,
        return_value: &str,
    ) {
        // Callable watchers always return a tuple of:
        // ( stdio output string, returnValue )
        const TUPLE_ITEM_COUNT: i32 = 2;

        {
            let result = request.result_mut();

            result.write_u8(TYPE_TUPLE);
            result.write_u8(MODE_READ_ONLY);

            let mut result_size = calculate_packed_int_size(TUPLE_ITEM_COUNT);
            result_size += tuple_element_stream_size(output.len());
            result_size += tuple_element_stream_size(return_value.len());

            result.write_packed_int(result_size as i32);
            result.write_packed_int(TUPLE_ITEM_COUNT);

            value_to_stream(
                result,
                &StreamValue::Str(output.to_string()),
                MODE_READ_ONLY,
            );
            // The caller streams the return value right after the output
            // (`watcher.cpp:928-931`).
            value_to_stream(
                result,
                &StreamValue::Str(return_value.to_string()),
                MODE_READ_ONLY,
            );
        }

        // `CallableWatcher::endResultStream` (`watcher.cpp:722-726`).
        request.set_result_value(self.callable.comment(), Mode::ReadOnly);
    }
}

/// `CallableWatcher::tupleElementStreamSize` (`watcher.hpp:2905-2912`):
/// the wire size of one tuple element -- the type/mode byte pair, the
/// packed payload length and the payload itself.
fn tuple_element_stream_size(payload_size: usize) -> usize {
    const TYPE_OVERHEAD: usize = 2;
    TYPE_OVERHEAD + calculate_packed_int_size(payload_size as i32) + payload_size
}

impl Watcher for BotCallableWatcher {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        self.callable.get_as_string(path, result, desc, mode)
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        // Protocol v1 callable invocation is refused (`watcher.cpp:663-673`).
        self.callable.set_from_string(path, value)
    }

    fn get_as_stream(&self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        self.callable.get_as_stream(path, request)
    }

    /// The subclass `setFromStream` (`add_bots_callable_watcher.cpp:44-80`)
    /// folding `SimpleCallableWatcher::setFromStream`
    /// (`watcher.cpp:989-1075`): a non-empty path or a missing value
    /// stream is the bad-shape rejection of both layers; a `TYPE_TUPLE`
    /// payload is the call form, anything else is the legacy bare-set
    /// form. The reference `peek`s the type byte and rewinds; consuming it
    /// once lands the stream in the same position for both arms.
    fn set_from_stream(&mut self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        if !path.is_empty() || !request.has_value_stream() {
            // ERROR_MSG "Function Watcher called with bad path or stream."
            return false;
        }

        // The reference streams straight off the request's payload; the
        // port copies the bytes so the answer half can borrow the request.
        let payload = request.value_data().expect("checked value stream").to_vec();
        let mut input = MemoryIStream::new(&payload);

        let type_byte = input.read_u8();
        if input.error() {
            return false;
        }

        if type_byte == TYPE_TUPLE {
            self.call_form(&mut input, request)
        } else {
            self.legacy_form(&mut input, type_byte, request)
        }
    }

    fn comment(&self) -> &str {
        self.callable.comment()
    }

    fn set_comment(&mut self, comment: String) {
        self.callable.set_comment(comment);
    }
}

/// `MainApp::initWatchers`' four `command/*` children
/// (`main_app.cpp:1052-1070`): the reference hangs them off the global
/// root via `root.addChild`; the port registers through the same global
/// tree (`watcher::add_watcher`) and hands the handles back for callers
/// that drive the requests.
pub fn register_command_watchers(
    fleet: &Rc<RefCell<Fleet>>,
) -> [Rc<RefCell<BotCallableWatcher>>; 4] {
    let add_bots = Rc::new(RefCell::new(BotCallableWatcher::add_bots(Rc::clone(fleet))));
    let del_bots = Rc::new(RefCell::new(BotCallableWatcher::del_bots(Rc::clone(fleet))));
    let del_tagged = Rc::new(RefCell::new(BotCallableWatcher::del_tagged_entities(
        Rc::clone(fleet),
    )));
    let update_movement = Rc::new(RefCell::new(BotCallableWatcher::update_movement(
        Rc::clone(fleet),
    )));

    let _ = watcher::add_watcher("command/addBots", add_bots.clone());
    let _ = watcher::add_watcher("command/delBots", del_bots.clone());
    let _ = watcher::add_watcher("command/delTaggedEntities", del_tagged.clone());
    let _ = watcher::add_watcher("command/updateMovement", update_movement.clone());

    [add_bots, del_bots, del_tagged, update_movement]
}

#[cfg(test)]
mod tests {
    use super::*;
    use watcher::stream::{MODE_CALLABLE, MemoryOStream, TYPE_INT, TYPE_STRING, TYPE_UNKNOWN};

    /// A fleet with the default FD budget and room to grow.
    fn fleet() -> Rc<RefCell<Fleet>> {
        Rc::new(RefCell::new(Fleet::new("test", 1024)))
    }

    /// The protocol 2 call payload for a one-parameter call: the TUPLE
    /// byte, the declared size of everything after it, the parameter
    /// count, then the typed parameter.
    fn call_payload(parameter_count: i32, parameter: &[u8]) -> Vec<u8> {
        let mut tail = MemoryOStream::new();
        tail.write_packed_int(parameter_count);
        tail.add_blob(parameter);

        let mut payload = MemoryOStream::new();
        payload.write_u8(TYPE_TUPLE);
        payload.write_packed_int(tail.size() as i32);
        payload.add_blob(tail.data());
        payload.data().to_vec()
    }

    /// A typed INT parameter: type byte, mode byte, packed length,
    /// little-endian payload.
    fn int_parameter(value: i32) -> Vec<u8> {
        let mut out = MemoryOStream::new();
        out.write_u8(TYPE_INT);
        out.write_u8(MODE_READ_WRITE);
        out.write_string_length(4);
        out.write_i32(value);
        out.data().to_vec()
    }

    /// A typed STRING parameter.
    fn string_parameter(value: &str) -> Vec<u8> {
        let mut out = MemoryOStream::new();
        out.write_u8(TYPE_STRING);
        out.write_u8(MODE_READ_WRITE);
        // `write_string` carries the packed length itself.
        out.write_string(value);
        out.data().to_vec()
    }

    /// Drive a request through the global root the way the packet layer
    /// does, and hand back the reply bytes.
    fn drive(path: &str, payload: Vec<u8>) -> (bool, Vec<u8>) {
        let mut request = WatcherPathRequestV2::new(path);
        assert!(request.set_packet_data(payload));
        let status = request.set_watcher_value();
        (status, request.data().to_vec())
    }

    /// The failed-set reply the request machinery writes when the watcher
    /// refused (`complete_failed_set`).
    fn assert_failed_reply(reply: &[u8]) {
        let mut input = MemoryIStream::new(reply);
        assert_eq!(input.read_u8(), TYPE_UNKNOWN);
        assert_eq!(input.read_u8(), watcher::stream::MODE_READ_ONLY);
        assert_eq!(input.read_string_length(), 0);
        assert!(!input.read_bool());
    }

    /// The `(stdio, returnValue)` callable reply tuple: the TUPLE header,
    /// the packed total size, the item count, then the two strings, and
    /// the set status boolean.
    fn assert_callable_reply(reply: &[u8]) {
        let mut input = MemoryIStream::new(reply);
        assert_eq!(input.read_u8(), TYPE_TUPLE);
        assert_eq!(input.read_u8(), MODE_READ_ONLY);
        let size = input.read_packed_int();
        let remaining = input.remaining_length();
        // The reference snapshots the remainder before the count and only
        // demands it cover the declared size (`watcher.cpp:1017-1028`).
        assert!(
            remaining >= size.unsigned_abs() as usize,
            "declared size {size} fits in the remaining {remaining} bytes"
        );
        assert_eq!(input.read_packed_int(), 2);
        // The stdio capture is empty; the return value keeps the base's
        // "No value" default (`watcher.cpp:994-995`).
        assert_eq!(stream_to_value::<String>(&mut input), Some(String::new()));
        assert_eq!(
            stream_to_value::<String>(&mut input),
            Some("No value".to_string())
        );
        // The set status boolean (`set_result_value`).
        assert!(input.read_bool());
        assert!(!input.error());
        assert_eq!(input.remaining_length(), 0, "the reply drains exactly");
    }

    #[test]
    fn the_call_form_adds_bots_and_answers_the_output_tuple() {
        let fleet = fleet();
        let [add_bots, ..] = register_command_watchers(&fleet);

        let (status, reply) = drive("command/addBots", call_payload(1, &int_parameter(3)));
        assert!(status);
        assert_eq!(fleet.borrow().len(), 3);
        assert_callable_reply(&reply);
        assert_eq!(add_bots.borrow().comment(), "Add bots to this process");
    }

    #[test]
    fn the_legacy_int_set_form_is_backwards_compatible() {
        // The pre-2.6 form: a bare INT where the tuple would be
        // (`add_bots_callable_watcher.cpp:53-78`); the reply echoes the
        // value in READ_WRITE.
        let fleet = fleet();
        register_command_watchers(&fleet);

        let (status, reply) = drive("command/addBots", int_parameter(2));
        assert!(status);
        assert_eq!(fleet.borrow().len(), 2);

        let mut input = MemoryIStream::new(&reply);
        assert_eq!(input.read_u8(), TYPE_INT);
        assert_eq!(input.read_u8(), MODE_READ_WRITE);
        assert_eq!(input.read_string_length(), 4);
        assert_eq!(input.read_i32(), 2);
        assert!(input.read_bool());
    }

    #[test]
    fn an_eight_byte_int_parameter_downcasts_like_the_reference() {
        // `watcherStreamToValueType(int32)` accepts an INT64 payload and
        // downcasts (`watcher.hpp:313-334`).
        let fleet = fleet();
        register_command_watchers(&fleet);

        let mut parameter = MemoryOStream::new();
        parameter.write_u8(TYPE_INT);
        parameter.write_u8(MODE_READ_WRITE);
        parameter.write_string_length(8);
        parameter.write_u32(5);
        parameter.write_u32(0);

        let (status, reply) = drive("command/addBots", call_payload(1, parameter.data()));
        assert!(status);
        assert_eq!(fleet.borrow().len(), 5);
        assert_callable_reply(&reply);
    }

    #[test]
    fn del_bots_takes_from_the_front_through_both_forms() {
        let fleet = fleet();
        register_command_watchers(&fleet);

        drive("command/addBots", int_parameter(4));
        assert_eq!(fleet.borrow().len(), 4);

        let (status, reply) = drive("command/delBots", call_payload(1, &int_parameter(2)));
        assert!(status);
        assert_eq!(fleet.borrow().len(), 2);
        assert_callable_reply(&reply);

        // The legacy form answers the echo, not the callable tuple.
        let (status, reply) = drive("command/delBots", int_parameter(1));
        assert!(status);
        assert_eq!(fleet.borrow().len(), 1);
        let mut input = MemoryIStream::new(&reply);
        assert_eq!(input.read_u8(), TYPE_INT);
        assert_eq!(input.read_u8(), MODE_READ_WRITE);
        assert_eq!(input.read_string_length(), 4);
        assert_eq!(input.read_i32(), 1);
        assert!(input.read_bool());
    }

    #[test]
    fn del_tagged_entities_takes_the_tag_string() {
        let fleet = fleet();
        {
            let mut fleet = fleet.borrow_mut();
            fleet.settings_mut().tag = "Custom".to_string();
            fleet.add_bot();
            fleet.settings_mut().tag = "Default".to_string();
            fleet.add_bot();
            fleet.add_bot();
        }
        register_command_watchers(&fleet);
        assert_eq!(fleet.borrow().len(), 3);

        let (status, reply) = drive(
            "command/delTaggedEntities",
            call_payload(1, &string_parameter("Custom")),
        );
        assert!(status);
        assert_eq!(fleet.borrow().len(), 2, "only the tagged bot left");
        assert_callable_reply(&reply);

        let (status, reply) = drive("command/delTaggedEntities", string_parameter("Default"));
        assert!(status);
        assert_eq!(fleet.borrow().len(), 0);
        let mut input = MemoryIStream::new(&reply);
        assert_eq!(input.read_u8(), TYPE_STRING);
        assert_eq!(input.read_u8(), MODE_READ_WRITE);
        assert_eq!(input.read_string(), "Default");
        assert!(input.read_bool());
    }

    #[test]
    fn update_movement_takes_the_tag_or_empty_for_all() {
        // The tag is echoed (`update_movement_callable_watcher.cpp:67-75`);
        // the movement semantics themselves are pinned in the fleet's own
        // tests.
        let fleet = fleet();
        fleet.borrow_mut().add_bot();
        register_command_watchers(&fleet);

        let (status, reply) = drive(
            "command/updateMovement",
            call_payload(1, &string_parameter("Default")),
        );
        assert!(status);
        assert_callable_reply(&reply);

        // The empty tag targets all bots (`main_app.cpp:521-547`).
        let (status, reply) = drive("command/updateMovement", string_parameter(""));
        assert!(status);
        let mut input = MemoryIStream::new(&reply);
        assert_eq!(input.read_u8(), TYPE_STRING);
        assert_eq!(input.read_u8(), MODE_READ_WRITE);
        assert_eq!(input.read_string(), "");
        assert!(input.read_bool());
    }

    #[test]
    fn a_wrong_parameter_count_fails_without_touching_the_fleet() {
        let fleet = fleet();
        register_command_watchers(&fleet);

        // parameterCount 2 with one parameter on the stream: the onCall
        // ERROR arm (`add_bots_callable_watcher.cpp:60-66`).
        let (status, reply) = drive("command/addBots", call_payload(2, &int_parameter(3)));
        assert!(status, "the request machinery always answers");
        assert_failed_reply(&reply);
        assert_eq!(fleet.borrow().len(), 0);
    }

    #[test]
    fn a_truncated_tuple_fails() {
        // The declared size outruns the remaining bytes
        // (`watcher.cpp:1021-1028`).
        let fleet = fleet();
        register_command_watchers(&fleet);

        let mut payload = MemoryOStream::new();
        payload.write_u8(TYPE_TUPLE);
        payload.write_packed_int(1000);
        payload.write_packed_int(1);
        payload.add_blob(&int_parameter(3));

        let (status, reply) = drive("command/addBots", payload.data().to_vec());
        assert!(status);
        assert_failed_reply(&reply);
        assert_eq!(fleet.borrow().len(), 0);
    }

    #[test]
    fn an_unparsable_legacy_payload_fails_without_a_reply() {
        // A length the payload cannot satisfy leaves `watcherStreamToValue`
        // false and the subclass answering a bare false
        // (`add_bots_callable_watcher.cpp:62-65`).
        let fleet = fleet();
        register_command_watchers(&fleet);

        let mut payload = MemoryOStream::new();
        payload.write_u8(TYPE_INT);
        payload.write_u8(MODE_READ_WRITE);
        payload.write_string_length(999);

        let (status, reply) = drive("command/addBots", payload.data().to_vec());
        assert!(status);
        assert_failed_reply(&reply);
        assert_eq!(fleet.borrow().len(), 0);
    }

    #[test]
    fn a_bad_path_or_missing_stream_is_rejected() {
        let fleet = fleet();
        let mut watcher = BotCallableWatcher::add_bots(fleet);

        // A non-empty path misses the callable (`isEmptyPath`).
        let mut request = WatcherPathRequestV2::new("ignored");
        assert!(request.set_packet_data(call_payload(1, &int_parameter(1))));
        assert!(!watcher.set_from_stream("junk", &mut request));

        // No value stream attached.
        let mut request = WatcherPathRequestV2::new("");
        assert!(!watcher.set_from_stream("", &mut request));
    }

    #[test]
    fn a_zero_length_payload_is_rejected() {
        // The type byte alone cannot be read (`pInput->error()`).
        let fleet = fleet();
        let mut watcher = BotCallableWatcher::add_bots(fleet);
        let mut request = WatcherPathRequestV2::new("");
        assert!(request.set_packet_data(Vec::new()));
        assert!(!watcher.set_from_stream("", &mut request));
    }

    /// Decodes the `__args__` argument-descriptor blob: the packed count,
    /// then one (description, data type) tuple per declared argument
    /// (`watcher.cpp:776-826`).
    fn decode_single_arg_blob(input: &mut MemoryIStream<'_>) -> (String, u8) {
        assert_eq!(input.read_string_length(), 1, "one declared argument");
        assert_eq!(input.read_u8(), TYPE_TUPLE);
        assert_eq!(input.read_u8(), watcher::stream::MODE_READ_ONLY);
        let _arg_blob_size = input.read_string_length();

        assert_eq!(input.read_string_length(), 2, "arg name + type");
        assert_eq!(input.read_u8(), TYPE_STRING);
        assert_eq!(input.read_u8(), watcher::stream::MODE_READ_ONLY);
        let description = input.read_string();
        assert_eq!(input.read_u8(), watcher::stream::TYPE_TYPE);
        assert_eq!(input.read_u8(), watcher::stream::MODE_READ_ONLY);
        assert_eq!(input.read_string_length(), 1);
        let data_type = input.read_u8();
        (description, data_type)
    }

    #[test]
    fn the_callable_face_reports_marker_args_and_doc() {
        let fleet = fleet();
        let mut watcher = BotCallableWatcher::del_tagged_entities(fleet);

        // The string protocol: the marker and the comment
        // (`watcher.cpp:644-659`).
        let mut result = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(watcher.get_as_string("", &mut result, &mut desc, &mut mode));
        assert_eq!(result, "Callable function");
        assert_eq!(desc, "Delete all bots from this process with the given tag");
        assert_eq!(mode, Mode::Callable);

        // The stream protocol carries the same pair
        // (`watcher.cpp:756-764`).
        let mut request = WatcherPathRequestV2::new("");
        assert!(watcher.get_as_stream("", &mut request));
        let mut input = MemoryIStream::new(request.data());
        assert_eq!(input.read_u8(), TYPE_STRING);
        assert_eq!(input.read_u8(), MODE_CALLABLE);
        assert_eq!(input.read_string(), "Callable function");

        // __args__: the packed argument-descriptor blob.
        let mut request = WatcherPathRequestV2::new("");
        assert!(watcher.get_as_stream("__args__", &mut request));
        let mut input = MemoryIStream::new(request.data());
        assert_eq!(input.read_u8(), TYPE_TUPLE);
        assert_eq!(input.read_u8(), MODE_CALLABLE);
        let _blob_prefix = input.read_string_length();
        let (description, data_type) = decode_single_arg_blob(&mut input);
        assert_eq!(description, "Tag of bots to delete");
        assert_eq!(data_type, TYPE_STRING, "the declared STRING parameter");

        // __doc__: the comment read-only.
        let mut request = WatcherPathRequestV2::new("");
        assert!(watcher.get_as_stream("__doc__", &mut request));
        let mut input = MemoryIStream::new(request.data());
        assert_eq!(input.read_u8(), TYPE_STRING);
        assert_eq!(input.read_u8(), watcher::stream::MODE_READ_ONLY);
        assert_eq!(
            input.read_string(),
            "Delete all bots from this process with the given tag"
        );

        // Protocol v1 writes are refused (`watcher.cpp:663-673`).
        assert!(!watcher.set_from_string("", "whatever"));
    }

    #[test]
    fn registration_lands_all_four_under_command() {
        // `MainApp::initWatchers`' tree shape (`main_app.cpp:1052-1070`):
        // the four callables resolve from the root by their command paths.
        let fleet = fleet();
        register_command_watchers(&fleet);

        for (path, comment) in [
            ("command/addBots", "Add bots to this process"),
            ("command/delBots", "Delete bots from this process"),
            (
                "command/delTaggedEntities",
                "Delete all bots from this process with the given tag",
            ),
            (
                "command/updateMovement",
                "Update the movement controller of all bots int this process with the given tag",
            ),
        ] {
            let mut desc = String::new();
            let mut mode = Mode::Invalid;
            let value = watcher::get_watcher_string(path, &mut desc, &mut mode)
                .unwrap_or_else(|| panic!("{path} resolves"));
            assert_eq!(value, "Callable function");
            assert_eq!(mode, Mode::Callable);
            assert_eq!(desc, comment);
        }
    }
    #[test]
    fn an_undecodable_call_parameter_fails_the_call() {
        // parameterCount is right but the INT payload claims more bytes
        // than the stream carries: the onCall "Failed to read parameter"
        // arm (`add_bots_callable_watcher.cpp:67-72`).
        let fleet = fleet();
        register_command_watchers(&fleet);

        let mut parameter = MemoryOStream::new();
        parameter.write_u8(TYPE_INT);
        parameter.write_u8(MODE_READ_WRITE);
        parameter.write_string_length(999);

        let (status, reply) = drive("command/addBots", call_payload(1, parameter.data()));
        assert!(status);
        assert_failed_reply(&reply);
        assert_eq!(fleet.borrow().len(), 0);
    }

    #[test]
    fn a_type_byte_only_payload_fails_the_legacy_form() {
        // The type byte is readable but the mode byte behind it is not
        // (`watcherStreamSkipMode` fails, `watcher.hpp:181-188`).
        let fleet = fleet();
        let mut watcher = BotCallableWatcher::add_bots(fleet);
        let mut request = WatcherPathRequestV2::new("");
        assert!(request.set_packet_data(vec![TYPE_INT]));
        assert!(!watcher.set_from_stream("", &mut request));
    }

    #[test]
    fn leftover_argument_data_still_succeeds() {
        // A call that leaves unread bytes after the parameter only draws
        // the reference's "did not read all argument data" warning
        // (`watcher.cpp:1048-1051`); the reply is the normal tuple.
        let fleet = fleet();
        register_command_watchers(&fleet);

        let mut parameter = int_parameter(2);
        parameter.push(0xff);

        let (status, reply) = drive("command/addBots", call_payload(1, &parameter));
        assert!(status);
        assert_eq!(fleet.borrow().len(), 2);
        assert_callable_reply(&reply);
    }

    #[test]
    fn set_comment_retitles_the_node() {
        // `Watcher::setComment` reaches the declarative face's comment,
        // the text `endResultStream` reports back with
        // (`watcher.cpp:722-726`).
        let fleet = fleet();
        let mut watcher = BotCallableWatcher::add_bots(fleet);
        watcher.set_comment("Retitled".to_string());
        assert_eq!(watcher.comment(), "Retitled");
    }

    /// Drive a payload into a watcher instance the way the packet layer
    /// does, without touching the global root: the same bytes two
    /// separate stacks must be able to replay.
    fn drive_into(watcher: &RefCell<BotCallableWatcher>, payload: Vec<u8>) -> bool {
        let mut request = WatcherPathRequestV2::new("command/addBots");
        assert!(request.set_packet_data(payload));
        watcher.borrow_mut().set_from_stream("", &mut request)
    }

    /// The batch add/remove acceptance through the wire (BW-12-04-02):
    /// the same packed command payloads — a legacy bare add, a protocol 2
    /// add, a protocol 2 delete, another legacy add — driven into two
    /// separately built fleet/watcher stacks leave the same active set,
    /// names in order, tags and seed-drawn speeds included.
    #[test]
    fn the_same_wire_commands_produce_the_same_active_set() {
        let build = || {
            let fleet = Rc::new(RefCell::new(Fleet::with_seed("0", 1024, 4242)));
            (
                Rc::new(RefCell::new(BotCallableWatcher::add_bots(Rc::clone(
                    &fleet,
                )))),
                Rc::new(RefCell::new(BotCallableWatcher::del_bots(Rc::clone(
                    &fleet,
                )))),
                fleet,
            )
        };
        let (add_left, del_left, fleet_left) = build();
        let (add_right, del_right, fleet_right) = build();

        let commands: [(
            &RefCell<BotCallableWatcher>,
            &RefCell<BotCallableWatcher>,
            Vec<u8>,
        ); 4] = [
            // legacy bare add: 2 (Bot_1, Bot_2)
            (&add_left, &add_right, int_parameter(2)),
            // protocol 2 call: add 2 (Bot_3, Bot_4)
            (&add_left, &add_right, call_payload(1, &int_parameter(2))),
            // protocol 2 call: delete 3 (Bot_1..Bot_3)
            (&del_left, &del_right, call_payload(1, &int_parameter(3))),
            // legacy bare add: 1 (Bot_5)
            (&add_left, &add_right, int_parameter(1)),
        ];
        for (left_watcher, right_watcher, payload) in commands {
            assert!(drive_into(left_watcher, payload.clone()));
            assert!(drive_into(right_watcher, payload));
        }

        let active = |fleet: &Fleet| -> Vec<(String, String, f32)> {
            fleet
                .bots()
                .iter()
                .map(|bot| {
                    (
                        bot.user_name().to_string(),
                        bot.tag().to_string(),
                        bot.speed(),
                    )
                })
                .collect()
        };
        assert_eq!(active(&fleet_left.borrow()), active(&fleet_right.borrow()));
        let binding = fleet_left.borrow();
        let names: Vec<&str> = binding.bots().iter().map(|bot| bot.user_name()).collect();
        assert_eq!(names, vec!["Bot_4_0", "Bot_5_0"]);
    }
}
