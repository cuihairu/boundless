//! The four bots command callables (`server/tools/bots/*_callable_watcher.cpp`),
//! registered in `MainApp::initWatchers` (main_app.cpp:1044-1075).
//!
//! Each reference class extends `SimpleCallableWatcher` with one extra
//! argument and overrides `setFromStream` to keep serving the pre-BW2.6
//! bare-value set messages WebConsole/PyCommon sent: a non-tuple payload
//! is decoded straight into the fleet call and the value is echoed back
//! (`MemberWatcher::setFromStream` shape), while a `WATCHER_TYPE_TUPLE`
//! payload falls through to the callable machinery. The port cannot
//! subclass, so [`FleetCommandWatcher`] wraps a [`SimpleCallableWatcher`]
//! and mirrors the override on its `set_from_stream`.

use std::cell::RefCell;
use std::rc::Rc;

use watcher::{
    DataType, ExposeHint, Mode, SimpleCallableWatcher, StreamValue, TYPE_TUPLE, Watcher,
    WatcherPathRequestV2, add_watcher,
};
use watcher::{MODE_READ_WRITE, MemoryIStream, stream_to_value, value_to_stream};

use crate::fleet::Fleet;

/// The registration paths (`main_app.cpp:1050-1070`).
pub const ADD_BOTS_PATH: &str = "command/addBots";
/// `main_app.cpp:1052`.
pub const DEL_BOTS_PATH: &str = "command/delBots";
/// `main_app.cpp:1057` — the path carries the *entity* spelling even
/// though the class is `DelTaggedBotsCallableWatcher`.
pub const DEL_TAGGED_ENTITIES_PATH: &str = "command/delTaggedEntities";
/// `main_app.cpp:1069`.
pub const UPDATE_MOVEMENT_PATH: &str = "command/updateMovement";

/// The fleet handle the callables drive (`MainApp::instance()`).
type FleetHandle = Rc<RefCell<Fleet>>;

/// Which fleet method the callable serves; one variant per reference
/// class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    AddBots,
    DelBots,
    DelTaggedEntities,
    UpdateMovement,
}

impl Command {
    /// The `(comment, arg type, arg description)` of the reference
    /// constructors (`*_callable_watcher.cpp`, each ctor block).
    fn face(self) -> (&'static str, DataType, &'static str) {
        match self {
            Command::AddBots => (
                "Add bots to this process",
                DataType::Int,
                "Number of bots to add",
            ),
            Command::DelBots => (
                "Delete bots from this process",
                DataType::Int,
                "Number of bots to delete",
            ),
            Command::DelTaggedEntities => (
                "Delete all bots from this process with the given tag",
                DataType::String,
                "Tag of bots to delete",
            ),
            Command::UpdateMovement => (
                // The reference text carries its own "int" typo.
                "Update the movement controller of all bots int this process with the given tag",
                DataType::String,
                "Tag of bots to update, or empty for all",
            ),
        }
    }
}

/// One bots command callable.
pub struct FleetCommandWatcher {
    callable: SimpleCallableWatcher,
    fleet: FleetHandle,
    command: Command,
}

impl FleetCommandWatcher {
    fn new(fleet: FleetHandle, command: Command) -> Self {
        let fleet_for_call = Rc::clone(&fleet);
        let func = Box::new(
            move |output: &mut String,
                  value: &mut String,
                  parameter_count: i32,
                  parameters: &mut MemoryIStream|
                  -> bool {
                if parameter_count != 1 {
                    return false;
                }
                let _ = (output, value);
                match command {
                    Command::AddBots => match stream_to_value::<i32>(parameters) {
                        Some(n) => fleet_for_call.borrow_mut().add_bots(n),
                        None => return false,
                    },
                    Command::DelBots => match stream_to_value::<i32>(parameters) {
                        Some(n) => fleet_for_call.borrow_mut().del_bots(n),
                        None => return false,
                    },
                    Command::DelTaggedEntities => match stream_to_value::<String>(parameters) {
                        Some(tag) => fleet_for_call.borrow_mut().del_tagged_entities(&tag),
                        None => return false,
                    },
                    Command::UpdateMovement => match stream_to_value::<String>(parameters) {
                        Some(tag) => fleet_for_call.borrow_mut().update_movement(&tag),
                        None => return false,
                    },
                }
                true
            },
        );

        let (comment, arg_type, arg_description) = command.face();
        let mut callable = SimpleCallableWatcher::new(func, ExposeHint::LocalOnly);
        callable.callable_mut().add_arg(arg_type, arg_description);
        callable.set_comment(comment.to_string());

        Self {
            callable,
            fleet,
            command,
        }
    }

    /// The legacy bare-value arm (each reference `setFromStream` after the
    /// tuple check): decode the typed value, run the fleet call, echo the
    /// value back READ_WRITE and close the request through `setResult`.
    /// `false` either means "not a legacy message" (the caller must
    /// delegate) or "a legacy payload that failed to decode" (the caller
    /// must fail); the decode result disambiguates.
    fn run_legacy(&mut self, request: &mut WatcherPathRequestV2) -> Option<bool> {
        let data = request.value_data()?;
        if data.first().copied() == Some(TYPE_TUPLE) {
            // A modern call message: hand it to the callable machinery.
            return None;
        }

        let fleet = Rc::clone(&self.fleet);
        let mut input = MemoryIStream::new(data);
        let value = match self.command {
            Command::AddBots => stream_to_value::<i32>(&mut input).map(|n| {
                fleet.borrow_mut().add_bots(n);
                StreamValue::Int(n)
            }),
            Command::DelBots => stream_to_value::<i32>(&mut input).map(|n| {
                fleet.borrow_mut().del_bots(n);
                StreamValue::Int(n)
            }),
            Command::DelTaggedEntities => stream_to_value::<String>(&mut input).map(|tag| {
                fleet.borrow_mut().del_tagged_entities(&tag);
                StreamValue::Str(tag)
            }),
            Command::UpdateMovement => stream_to_value::<String>(&mut input).map(|tag| {
                fleet.borrow_mut().update_movement(&tag);
                StreamValue::Str(tag)
            }),
        };

        let value = value?;
        // watcherValueToStream( pathRequest.getResultStream(), v, mode );
        value_to_stream(request.result_mut(), &value, MODE_READ_WRITE);
        // pathRequest.setResult( "", mode, this, base );
        request.set_result_value("", Mode::ReadWrite);
        Some(true)
    }
}

impl Watcher for FleetCommandWatcher {
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
        self.callable.set_from_string(path, value)
    }

    fn get_as_stream(&self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        self.callable.get_as_stream(path, request)
    }

    /// The override each reference class installs
    /// (`add_bots_callable_watcher.cpp:35-77` et cetera): anything but an
    /// empty path over a live value stream, and any TUPLE-typed payload,
    /// is the callable machinery's business; the rest is the legacy arm.
    fn set_from_stream(&mut self, path: &str, request: &mut WatcherPathRequestV2) -> bool {
        if !path.is_empty() || !request.has_value_stream() {
            return self.callable.set_from_stream(path, request);
        }
        match self.run_legacy(request) {
            Some(handled) => handled,
            None => self.callable.set_from_stream(path, request),
        }
    }

    fn comment(&self) -> &str {
        self.callable.comment()
    }

    fn set_comment(&mut self, comment: String) {
        self.callable.set_comment(comment);
    }
}

/// `MainApp::initWatchers` (main_app.cpp:1050-1070): land the four
/// command callables on the watcher root.
pub fn register_fleet_commands(fleet: &FleetHandle) {
    for (path, command) in [
        (ADD_BOTS_PATH, Command::AddBots),
        (DEL_BOTS_PATH, Command::DelBots),
        (DEL_TAGGED_ENTITIES_PATH, Command::DelTaggedEntities),
        (UPDATE_MOVEMENT_PATH, Command::UpdateMovement),
    ] {
        add_watcher(
            path,
            Rc::new(RefCell::new(FleetCommandWatcher::new(
                Rc::clone(fleet),
                command,
            ))),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beeline::BeelineControllerFactory;
    use watcher::{MODE_READ_ONLY, MemoryOStream, TYPE_TUPLE, TYPE_UNKNOWN, fini, get_watcher};

    use crate::ConnState;

    /// A fleet with one bot carried to the Online state with an entity id,
    /// the gate `set_movement_controller` requires (bot.rs:516-520).
    fn online_fleet_one_bot() -> Rc<RefCell<Fleet>> {
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        fleet.borrow_mut().add_bots(1);
        {
            let mut borrow = fleet.borrow_mut();
            let bot = &mut borrow.bots_mut()[0];
            bot.log_on();
            bot.feed_log_on_result(5, 1);
            assert!(bot.tick(0.1));
        }
        fleet
    }

    /// A legacy bare-int payload (`watcherValueToStream` shape with the
    /// WRITE mode the old consoles stamped on set messages).
    fn legacy_int_payload(value: i32) -> Vec<u8> {
        let mut payload = MemoryOStream::new();
        value_to_stream(&mut payload, &StreamValue::Int(value), MODE_READ_WRITE);
        payload.data().to_vec()
    }

    /// The legacy bare-string payload.
    fn legacy_string_payload(value: &str) -> Vec<u8> {
        let mut payload = MemoryOStream::new();
        value_to_stream(
            &mut payload,
            &StreamValue::Str(value.to_string()),
            MODE_READ_WRITE,
        );
        payload.data().to_vec()
    }

    /// A modern tuple call payload with one element (the
    /// `SimpleCallableWatcher::setFromStream` wire shape).
    fn tuple_call_payload(value: StreamValue) -> Vec<u8> {
        let mut args = MemoryOStream::new();
        value_to_stream(&mut args, &value, MODE_READ_WRITE);
        let mut payload = MemoryOStream::new();
        payload.write_u8(TYPE_TUPLE);
        payload.write_packed_int(args.size() as i32);
        payload.write_packed_int(1);
        payload.add_blob(args.data());
        payload.data().to_vec()
    }

    /// The default failed-set reply `completeFailedSet` fills after a
    /// refused set (path_request.rs:207-229).
    fn failed_reply(seq: u32) -> Vec<u8> {
        let mut expected = MemoryOStream::new();
        expected.write_u32(seq);
        expected.write_u8(TYPE_UNKNOWN);
        expected.write_u8(MODE_READ_ONLY);
        expected.write_string_length(0);
        expected.write_bool(false);
        expected.data().to_vec()
    }

    /// Drive a set request through the registered tree and return the
    /// request for reply inspection.
    fn set_request(path: &str, seq: u32, payload: &[u8]) -> WatcherPathRequestV2 {
        let mut request = WatcherPathRequestV2::new(path);
        request.set_sequence_number(seq);
        request.set_packet_data(payload.to_vec());
        request.set_watcher_value();
        request
    }

    /// The legacy reply shape: the sequence number, the echoed value in
    /// READ_WRITE form, and the status boolean `setResult` appends.
    fn legacy_reply(seq: u32, value: StreamValue) -> Vec<u8> {
        let mut expected = MemoryOStream::new();
        expected.write_u32(seq);
        value_to_stream(&mut expected, &value, MODE_READ_WRITE);
        expected.write_bool(true);
        expected.data().to_vec()
    }

    /// `AddBotsCallableWatcher::setFromStream`'s legacy arm
    /// (add_bots_callable_watcher.cpp:47-70): an int set on
    /// `command/addBots` adds the bots and echoes the count.
    #[test]
    fn a_legacy_int_set_adds_bots_and_echoes_the_count() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        register_fleet_commands(&fleet);

        let request = set_request(ADD_BOTS_PATH, 9, &legacy_int_payload(3));

        assert_eq!(fleet.borrow().bots().len(), 3);
        assert_eq!(request.data(), legacy_reply(9, StreamValue::Int(3)));
    }

    /// `DelBotsCallableWatcher::setFromStream`'s legacy arm
    /// (del_bots_callable_watcher.cpp:47-70): an int set on
    /// `command/delBots` destroys bots from the front and echoes the
    /// count.
    #[test]
    fn a_legacy_int_set_deletes_bots_from_the_front() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        fleet.borrow_mut().add_bots(2);
        register_fleet_commands(&fleet);

        let request = set_request(DEL_BOTS_PATH, 4, &legacy_int_payload(1));

        assert_eq!(fleet.borrow().bots().len(), 1);
        assert_eq!(request.data(), legacy_reply(4, StreamValue::Int(1)));
    }

    /// `DelTaggedBotsCallableWatcher::setFromStream`'s legacy arm
    /// (del_tagged_bots_callable_watcher.cpp:47-74): a string set on
    /// `command/delTaggedEntities` destroys every bot carrying the tag and
    /// echoes the tag.
    #[test]
    fn a_legacy_string_set_deletes_only_the_tagged_bots() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        fleet.borrow_mut().add_bots(2);
        fleet.borrow_mut().settings_mut().tag = "Other".to_string();
        fleet.borrow_mut().add_bots(1);
        register_fleet_commands(&fleet);
        assert_eq!(fleet.borrow().bots().len(), 3);

        let request = set_request(
            DEL_TAGGED_ENTITIES_PATH,
            11,
            &legacy_string_payload("Other"),
        );

        assert_eq!(fleet.borrow().bots().len(), 2);
        assert!(fleet.borrow().bots().iter().all(|b| b.tag() == "Default"));
        assert_eq!(
            request.data(),
            legacy_reply(11, StreamValue::Str("Other".to_string()))
        );
    }

    /// `UpdateMovementCallableWatcher::setFromStream`'s legacy arm
    /// (update_movement_callable_watcher.cpp:49-75): a string set on
    /// `command/updateMovement` re-resolves the settings' controller for
    /// the matching bots; an empty tag targets all of them.
    #[test]
    fn a_legacy_string_set_retargets_the_online_bots() {
        fini();
        let fleet = online_fleet_one_bot();
        {
            let mut borrow = fleet.borrow_mut();
            borrow
                .registry_mut()
                .register("Beeline", Box::new(BeelineControllerFactory));
            borrow.settings_mut().controller_type = "Beeline".to_string();
            borrow.settings_mut().controller_data = "0,0,0".to_string();
            assert!(!borrow.bots()[0].controller_installed());
        }
        register_fleet_commands(&fleet);

        let request = set_request(UPDATE_MOVEMENT_PATH, 6, &legacy_string_payload(""));

        assert!(fleet.borrow().bots()[0].controller_installed());
        assert_eq!(
            request.data(),
            legacy_reply(6, StreamValue::Str(String::new()))
        );
    }

    /// The modern face survives the override: a TUPLE-typed payload on
    /// `command/addBots` falls through to `SimpleCallableWatcher` and the
    /// reply is the `(output, value)` tuple — "No value", per
    /// `onCall` never touching the strings (add_bots_callable_watcher.cpp:79-101).
    #[test]
    fn a_tuple_call_falls_through_to_the_callable_machinery() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        register_fleet_commands(&fleet);

        let request = set_request(ADD_BOTS_PATH, 3, &tuple_call_payload(StreamValue::Int(2)));

        assert_eq!(fleet.borrow().bots().len(), 2);
        let mut body = MemoryOStream::new();
        body.write_packed_int(2);
        value_to_stream(&mut body, &StreamValue::Str(String::new()), MODE_READ_ONLY);
        value_to_stream(
            &mut body,
            &StreamValue::Str("No value".to_string()),
            MODE_READ_ONLY,
        );
        let mut expected = MemoryOStream::new();
        expected.write_u32(3);
        expected.write_u8(TYPE_TUPLE);
        expected.write_u8(MODE_READ_ONLY);
        expected.write_packed_int(body.size() as i32);
        expected.add_blob(body.data());
        expected.write_bool(true);
        assert_eq!(request.data(), expected.data());
    }

    /// The modern call face drives the other three commands too: a
    /// tuple on `command/delBots` consumes from the front, on
    /// `command/delTaggedEntities` the tagged bots, and on
    /// `command/updateMovement` the online bots get the settings'
    /// controller (del_bots_callable_watcher.cpp:82-104 et cetera).
    #[test]
    fn the_other_three_commands_answer_the_tuple_call_face() {
        fini();
        let fleet = online_fleet_one_bot();
        {
            let mut borrow = fleet.borrow_mut();
            borrow
                .registry_mut()
                .register("Beeline", Box::new(BeelineControllerFactory));
            borrow.settings_mut().controller_type = "Beeline".to_string();
            borrow.settings_mut().controller_data = "0,0,0".to_string();
            borrow.add_bots(1);
            {
                // The front bot is consumed by the delBots call below;
                // carry the survivor online so updateMovement can
                // actually install a controller (bot.rs:516-520).
                let bot = &mut borrow.bots_mut()[1];
                bot.log_on();
                bot.feed_log_on_result(6, 1);
                assert!(bot.tick(0.1));
            }
            borrow.settings_mut().tag = "Other".to_string();
            borrow.add_bots(1);
        }
        assert_eq!(fleet.borrow().bots().len(), 3);
        register_fleet_commands(&fleet);

        set_request(DEL_BOTS_PATH, 1, &tuple_call_payload(StreamValue::Int(1)));
        assert_eq!(fleet.borrow().bots().len(), 2);
        assert_eq!(fleet.borrow().bots()[0].conn_state(), ConnState::Online);

        set_request(
            DEL_TAGGED_ENTITIES_PATH,
            2,
            &tuple_call_payload(StreamValue::Str("Other".to_string())),
        );
        assert_eq!(fleet.borrow().bots().len(), 1);

        assert!(!fleet.borrow().bots()[0].controller_installed());
        set_request(
            UPDATE_MOVEMENT_PATH,
            3,
            &tuple_call_payload(StreamValue::Str(String::new())),
        );
        assert!(fleet.borrow().bots()[0].controller_installed());
    }

    /// A legacy payload of the wrong type for the command fails the set
    /// without touching the fleet (add_bots_callable_watcher.cpp:57-60:
    /// `watcherStreamToValue` false → plain `return false`), and the
    /// request machinery fills the default failed-set reply.
    #[test]
    fn a_wrong_typed_legacy_payload_fails_and_touches_nothing() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        register_fleet_commands(&fleet);

        let request = set_request(ADD_BOTS_PATH, 2, &legacy_string_payload("three"));

        assert!(fleet.borrow().bots().is_empty());
        assert_eq!(request.data(), failed_reply(2));
    }

    /// `onCall`'s refusals (add_bots_callable_watcher.cpp:88-100 et
    /// cetera): a zero-element tuple trips the arity guard, and a
    /// one-element tuple of the wrong type for the command fails the
    /// typed decode — both fail the set without touching the fleet.
    #[test]
    fn the_callable_refuses_wrong_arity_or_argument_types() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        register_fleet_commands(&fleet);

        // A zero-element tuple: `parameter_count != 1`.
        let mut payload = MemoryOStream::new();
        payload.write_u8(TYPE_TUPLE);
        payload.write_packed_int(0);
        payload.write_packed_int(0);
        let request = set_request(ADD_BOTS_PATH, 5, payload.data());
        assert_eq!(request.data(), failed_reply(5));

        // A one-element tuple of the wrong type for each command: the
        // typed decode inside `onCall` fails.
        for (path, payload) in [
            (
                ADD_BOTS_PATH,
                tuple_call_payload(StreamValue::Str("three".to_string())),
            ),
            (
                DEL_BOTS_PATH,
                tuple_call_payload(StreamValue::Str("three".to_string())),
            ),
            (
                DEL_TAGGED_ENTITIES_PATH,
                tuple_call_payload(StreamValue::Int(1)),
            ),
            (
                UPDATE_MOVEMENT_PATH,
                tuple_call_payload(StreamValue::Int(1)),
            ),
        ] {
            let request = set_request(path, 6, &payload);
            assert_eq!(request.data(), failed_reply(6));
            assert!(fleet.borrow().bots().is_empty());
        }
    }

    /// The faces the reference subclass inherits unmodified delegate to
    /// the callable machinery: a string protocol write is a hard
    /// rejection, the protocol-2 get is unsupported, and the comment is
    /// retitleable (`setComment`, watcher.hpp).
    #[test]
    fn the_remaining_faces_delegate_to_the_callable() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        register_fleet_commands(&fleet);

        let watcher = get_watcher(ADD_BOTS_PATH).expect("registered");
        assert!(!watcher.borrow_mut().set_from_string("", "3"));

        // The protocol-2 get answers the callable marker
        // (`CallableWatcher::getAsStream`, watcher.cpp:756-837).
        let mut request = WatcherPathRequestV2::new(ADD_BOTS_PATH);
        assert!(watcher.borrow().get_as_stream("", &mut request));
        let mut expected = MemoryOStream::new();
        value_to_stream(
            &mut expected,
            &StreamValue::Str("Callable function".to_string()),
            watcher::mode_byte(Mode::Callable),
        );
        // A get request carries no set payload, so `setResult` pushes no
        // status boolean here.
        assert_eq!(request.data(), expected.data());

        watcher.borrow_mut().set_comment("renamed".to_string());
        assert_eq!(watcher.borrow().comment(), "renamed");
    }

    /// A child path is not ours to answer: the override hands it to the
    /// callable machinery, whose empty-path guard refuses it
    /// (add_bots_callable_watcher.cpp:41-46 → watcher.cpp:994-998).
    #[test]
    fn a_child_path_falls_through_to_the_callable_guard() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        register_fleet_commands(&fleet);

        let request = set_request("command/addBots/deeper", 8, &legacy_int_payload(4));

        assert!(fleet.borrow().bots().is_empty());
        assert_eq!(request.data(), failed_reply(8));
    }

    /// The registration face (main_app.cpp:1050-1070): all four paths
    /// resolve on the root, each carrying its reference comment and one
    /// declared argument.
    #[test]
    fn the_four_commands_land_on_the_root_with_their_faces() {
        fini();
        let fleet = Rc::new(RefCell::new(Fleet::new("app", 512)));
        register_fleet_commands(&fleet);

        let faces = [
            (ADD_BOTS_PATH, "Add bots to this process"),
            (DEL_BOTS_PATH, "Delete bots from this process"),
            (
                DEL_TAGGED_ENTITIES_PATH,
                "Delete all bots from this process with the given tag",
            ),
            (
                UPDATE_MOVEMENT_PATH,
                "Update the movement controller of all bots int this process with the given tag",
            ),
        ];
        for (path, comment) in faces {
            let watcher = get_watcher(path).expect("registered");
            assert_eq!(watcher.borrow().comment(), comment, "{path}");
            // The string protocol answers a callable read with the marker
            // text (watcher.cpp:663-673 shape via CallableWatcher).
            let (mut result, mut desc, mut mode) = (String::new(), String::new(), Mode::Invalid);
            assert!(
                watcher
                    .borrow()
                    .get_as_string("", &mut result, &mut desc, &mut mode)
            );
            assert_eq!(result, "Callable function", "{path}");
        }
    }
}
