//! BW-11-05-04 VOIP branch: the voice-over-IP DLL contract, the shipped
//! empty implementation, the `BigWorld.VOIP` python wrapper face and the
//! `VOIPApp` main-loop integration.
//!
//! The reference splits the feature over four files:
//!
//! * `client/voip.hpp` — the DLL interface (`VOIPDataDict`, `VOIPResponse`,
//!   `VOIPClient`) exported from `voip.dll`;
//! * `lib/emptyvoip/voip.cpp` — the *only* implementation shipped in the
//!   reference tree: every method is an empty body, `initialise`/`finalise`
//!   return 0, `numberChannels()` returns 0 and `getChannel()` returns ""
//!   (built as `voip` / `voip_d` / `voip_h`);
//! * `client/bwvoip.cpp` — the `BigWorld.VOIP` python wrapper, including two
//!   preserved verbatim defects: `py_kickUser`'s dict check is inverted (a
//!   *valid* dict raises, and the error text names `inviteUser`), and
//!   `py_setHandler`'s `if (pCallback)` is always true so `None` still gets
//!   a response object installed;
//! * `client/voip_app.cpp` — the `VOIPApp` main-loop task that owns the
//!   client, ticks it with the player position/rotation/space and must be
//!   re-initialised through python.
//!
//! No third-party voice SDK (Vivox etc.) source exists in the reference
//! tree — see [`voip_targets`] for the enabled/missing registry this port
//! records. The acceptance is exactly this: the enabled target passes the
//! contract, and absent SDK implementations are registered as missing.
//!
//! The port models script-callable argument validation as typed entry
//! points returning verbatim `TypeError` texts, and the backend as an
//! observable op ledger (the 对照 mechanism).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// A python-level argument value, as far as the VOIP wrapper inspects it.
#[derive(Clone, Debug, PartialEq)]
pub enum PyValue {
    Str(String),
    Float(f32),
    Dict(BTreeMap<String, PyValue>),
    /// A callable object (the wrapper only records its name).
    Callable(String),
    /// `Py_None` — `parse_tuple("O")` yields it, never a NULL pointer.
    NoneValue,
    /// Any other object kind (int, list, ...).
    Other,
}

/// The python-level error the wrapper raises (`PyErr_SetString(TypeError, ..)`).
#[derive(Clone, PartialEq, Debug)]
pub struct TypeError {
    pub message: String,
}

impl TypeError {
    fn new(message: &str) -> TypeError {
        TypeError {
            message: message.to_string(),
        }
    }
}

/// `VOIPDataDict` (`client/voip.hpp:19`): pairs of string values passed to
/// the VOIP implementation.
///
/// Both the client-side `BWVOIPDataDict` and the stub-side
/// `SimpleVOIPDataDict` back it with a sorted string map, so indexed access
/// iterates in *sorted* key order (not insertion order), and `insert` never
/// overwrites an existing key (first writer wins).
#[derive(Clone, Default, PartialEq, Debug)]
pub struct VoipDataDict {
    dict: BTreeMap<String, String>,
}

impl VoipDataDict {
    pub fn new() -> VoipDataDict {
        VoipDataDict::default()
    }

    /// `BWVOIPDataDict` construction (`bwvoip.cpp:72`): every *string:string*
    /// entry of the python dict enters the map; other value kinds are
    /// skipped, and `dict_.insert` keeps the first value for a key.
    pub fn from_py_dict(py_dict: &BTreeMap<String, PyValue>) -> VoipDataDict {
        let mut dict = VoipDataDict::new();
        for (key, value) in py_dict {
            if let PyValue::Str(value) = value {
                dict.dict
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        dict
    }

    /// `get(key)`: NULL (here `None`) when the key is missing.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.dict.get(key).map(|value| value.as_str())
    }

    /// `get(index, &key, &value)`: the index-th pair in sorted key order.
    pub fn get_index(&self, index: usize) -> Option<(&str, &str)> {
        self.dict
            .iter()
            .nth(index)
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// `size()`.
    pub fn size(&self) -> usize {
        self.dict.len()
    }

    /// The pairs in sorted key order (what `PyVOIPResponse::response`
    /// rebuilds into the script dictionary).
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.dict
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// `VOIPResponse` (`client/voip.hpp:65`): the callback channel the VOIP
/// implementation uses to reach script code.
pub trait VoipResponse {
    fn response(&self, message: i32, data: Option<&VoipDataDict>);
}

/// One recorded response dispatch: the message id and the dict pairs in
/// sorted order — the `(int, dictionary)` tuple
/// `PyVOIPResponse::response` feeds the callback.
pub type ResponseCall = (i32, Vec<(String, String)>);

/// The `PyVOIPResponse` face (`bwvoip.cpp:1010`): converts the dict into a
/// python dictionary (sorted pairs) and calls the script callable with
/// `(int, dictionary)`.
///
/// The wrapper's `if (pCallback)` is always true after `parse_tuple("O")`,
/// so a `None` handler is *also* wrapped here; the port records nothing for
/// it where the C++ would call a NULL `PyObject*`.
pub struct ScriptResponse {
    callback: Option<String>,
    calls: RefCell<Vec<ResponseCall>>,
}

impl ScriptResponse {
    pub fn new(callback: Option<&str>) -> ScriptResponse {
        ScriptResponse {
            callback: callback.map(|name| name.to_string()),
            calls: RefCell::new(Vec::new()),
        }
    }

    pub fn callback(&self) -> Option<&str> {
        self.callback.as_deref()
    }

    /// The recorded `(message, sorted pairs)` calls, in dispatch order.
    pub fn calls(&self) -> Vec<ResponseCall> {
        self.calls.borrow().clone()
    }
}

impl VoipResponse for ScriptResponse {
    fn response(&self, message: i32, data: Option<&VoipDataDict>) {
        // The reference builds the dict then `PyObject_CallObject`; with a
        // None callback that call would crash — the port records nothing.
        if self.callback.is_none() {
            return;
        }
        let pairs = data.map(|dict| dict.pairs()).unwrap_or_default();
        self.calls.borrow_mut().push((message, pairs));
    }
}

/// One backend API call with its arguments — the observable contract
/// ledger (the acceptance's 对照 surface).
#[derive(Clone, PartialEq, Debug)]
pub enum VoipOp {
    Initialise {
        data: Vec<(String, String)>,
    },
    Finalise,
    Tick {
        x: f32,
        y: f32,
        z: f32,
        yaw: f32,
        pitch: f32,
        roll: f32,
        space_id: i32,
    },
    Command {
        data: Vec<(String, String)>,
    },
    GetStatistics,
    SetHandler {
        installed: bool,
    },
    Login {
        username: String,
        password: String,
        data: Vec<(String, String)>,
    },
    Logout {
        data: Vec<(String, String)>,
    },
    CreateChannel {
        channel: String,
        password: String,
        data: Vec<(String, String)>,
    },
    JoinChannel {
        channel: String,
        password: String,
        data: Vec<(String, String)>,
    },
    LeaveChannel {
        channel: String,
        data: Vec<(String, String)>,
    },
    InviteUser {
        user: String,
        channel: String,
        data: Vec<(String, String)>,
    },
    KickUser {
        user: String,
        channel: String,
        data: Vec<(String, String)>,
    },
    EnableActiveChannel {
        channel: String,
        data: Vec<(String, String)>,
    },
    DisableActiveChannel {
        channel: String,
        data: Vec<(String, String)>,
    },
    EnablePositional,
    DisablePositional,
    EnableMicrophone,
    DisableMicrophone,
    SetMasterVolume {
        attenuation_db: f32,
    },
    SetChannelVolume {
        channel: String,
        attenuation_db: f32,
    },
    SetMicrophoneVolume {
        attenuation_db: f32,
    },
}

fn dict_pairs(data: Option<&VoipDataDict>) -> Vec<(String, String)> {
    data.map(VoipDataDict::pairs).unwrap_or_default()
}

/// The `VOIPClient` DLL contract (`client/voip.hpp:85`) — the interface
/// `voip.dll` exports and the client links against.
pub trait VoipBackend {
    fn initialise(&mut self, data: Option<&VoipDataDict>) -> i32;
    fn finalise(&mut self) -> i32;
    // The DLL signature is seven positional parameters (voip.hpp:118).
    #[allow(clippy::too_many_arguments)]
    fn tick(&mut self, x: f32, y: f32, z: f32, yaw: f32, pitch: f32, roll: f32, space_id: i32);
    fn command(&mut self, data: Option<&VoipDataDict>);
    fn get_statistics(&mut self);
    fn set_handler(&mut self, handler: Option<Rc<dyn VoipResponse>>);
    fn login(&mut self, username: &str, password: &str, data: Option<&VoipDataDict>);
    fn logout(&mut self, data: Option<&VoipDataDict>);
    fn create_channel(&mut self, channel: &str, password: &str, data: Option<&VoipDataDict>);
    fn join_channel(&mut self, channel: &str, password: &str, data: Option<&VoipDataDict>);
    fn leave_channel(&mut self, channel: &str, data: Option<&VoipDataDict>);
    fn invite_user(&mut self, user: &str, channel: &str, data: Option<&VoipDataDict>);
    fn kick_user(&mut self, user: &str, channel: &str, data: Option<&VoipDataDict>);
    fn number_channels(&self) -> i32;
    fn get_channel(&self, index: usize) -> String;
    fn enable_active_channel(&mut self, channel: &str, data: Option<&VoipDataDict>);
    fn disable_active_channel(&mut self, channel: &str, data: Option<&VoipDataDict>);
    fn enable_positional(&mut self, data: Option<&VoipDataDict>);
    fn disable_positional(&mut self, data: Option<&VoipDataDict>);
    fn enable_microphone(&mut self, data: Option<&VoipDataDict>);
    fn disable_microphone(&mut self, data: Option<&VoipDataDict>);
    fn set_master_volume(&mut self, attenuation_db: f32, data: Option<&VoipDataDict>);
    fn set_channel_volume(
        &mut self,
        channel: &str,
        attenuation_db: f32,
        data: Option<&VoipDataDict>,
    );
    fn set_microphone_volume(&mut self, attenuation_db: f32, data: Option<&VoipDataDict>);
}

/// The enabled target: `lib/emptyvoip/voip.cpp` verbatim. Every method is
/// an empty body; `initialise`/`finalise` return 0; `numberChannels()`
/// returns 0; `getChannel()` returns ""; `setHandler` stores the handler.
/// The port adds the op ledger so the contract is observable.
#[derive(Default)]
pub struct EmptyVoip {
    ops: Vec<VoipOp>,
    handler: Option<Rc<dyn VoipResponse>>,
}

impl EmptyVoip {
    pub fn new() -> EmptyVoip {
        EmptyVoip::default()
    }

    /// The recorded backend calls, in call order.
    pub fn ops(&self) -> &[VoipOp] {
        &self.ops
    }

    pub fn clear_ops(&mut self) {
        self.ops.clear();
    }

    pub fn handler(&self) -> Option<&Rc<dyn VoipResponse>> {
        self.handler.as_ref()
    }
}

impl VoipBackend for EmptyVoip {
    fn initialise(&mut self, data: Option<&VoipDataDict>) -> i32 {
        self.ops.push(VoipOp::Initialise {
            data: dict_pairs(data),
        });
        0
    }

    fn finalise(&mut self) -> i32 {
        self.ops.push(VoipOp::Finalise);
        0
    }

    fn tick(&mut self, x: f32, y: f32, z: f32, yaw: f32, pitch: f32, roll: f32, space_id: i32) {
        self.ops.push(VoipOp::Tick {
            x,
            y,
            z,
            yaw,
            pitch,
            roll,
            space_id,
        });
    }

    fn command(&mut self, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::Command {
            data: dict_pairs(data),
        });
    }

    fn get_statistics(&mut self) {
        self.ops.push(VoipOp::GetStatistics);
    }

    fn set_handler(&mut self, handler: Option<Rc<dyn VoipResponse>>) {
        self.handler = handler;
        self.ops.push(VoipOp::SetHandler {
            installed: self.handler.is_some(),
        });
    }

    fn login(&mut self, username: &str, password: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::Login {
            username: username.to_string(),
            password: password.to_string(),
            data: dict_pairs(data),
        });
    }

    fn logout(&mut self, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::Logout {
            data: dict_pairs(data),
        });
    }

    fn create_channel(&mut self, channel: &str, password: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::CreateChannel {
            channel: channel.to_string(),
            password: password.to_string(),
            data: dict_pairs(data),
        });
    }

    fn join_channel(&mut self, channel: &str, password: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::JoinChannel {
            channel: channel.to_string(),
            password: password.to_string(),
            data: dict_pairs(data),
        });
    }

    fn leave_channel(&mut self, channel: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::LeaveChannel {
            channel: channel.to_string(),
            data: dict_pairs(data),
        });
    }

    fn invite_user(&mut self, user: &str, channel: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::InviteUser {
            user: user.to_string(),
            channel: channel.to_string(),
            data: dict_pairs(data),
        });
    }

    fn kick_user(&mut self, user: &str, channel: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::KickUser {
            user: user.to_string(),
            channel: channel.to_string(),
            data: dict_pairs(data),
        });
    }

    fn number_channels(&self) -> i32 {
        0
    }

    fn get_channel(&self, _index: usize) -> String {
        String::new()
    }

    fn enable_active_channel(&mut self, channel: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::EnableActiveChannel {
            channel: channel.to_string(),
            data: dict_pairs(data),
        });
    }

    fn disable_active_channel(&mut self, channel: &str, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::DisableActiveChannel {
            channel: channel.to_string(),
            data: dict_pairs(data),
        });
    }

    fn enable_positional(&mut self, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::EnablePositional);
        let _ = data;
    }

    fn disable_positional(&mut self, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::DisablePositional);
        let _ = data;
    }

    fn enable_microphone(&mut self, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::EnableMicrophone);
        let _ = data;
    }

    fn disable_microphone(&mut self, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::DisableMicrophone);
        let _ = data;
    }

    fn set_master_volume(&mut self, attenuation_db: f32, data: Option<&VoipDataDict>) {
        self.ops.push(VoipOp::SetMasterVolume { attenuation_db });
        let _ = data;
    }

    fn set_channel_volume(
        &mut self,
        channel: &str,
        attenuation_db: f32,
        data: Option<&VoipDataDict>,
    ) {
        self.ops.push(VoipOp::SetChannelVolume {
            channel: channel.to_string(),
            attenuation_db,
        });
        let _ = data;
    }

    fn set_microphone_volume(&mut self, attenuation_db: f32, data: Option<&VoipDataDict>) {
        self.ops
            .push(VoipOp::SetMicrophoneVolume { attenuation_db });
        let _ = data;
    }
}

/// The player state `VOIPApp::tick` reads (`voip_app.cpp:101`).
#[derive(Clone, Copy, Debug)]
pub struct PlayerTick {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub space_id: i32,
    /// `Entity::isInWorld()`.
    pub in_world: bool,
}

/// The `VOIPApp` main-loop task face (`client/voip_app.cpp`).
///
/// The singleton owns the VOIPClient created in `init()` from the DLL,
/// destroys it in `fini()`, and ticks it each frame with the player's
/// world position, head rotation and space.
pub struct VoipApp {
    client: Option<Rc<RefCell<EmptyVoip>>>,
}

impl VoipApp {
    pub fn new() -> VoipApp {
        // The reference adds itself to the main loop as "VOIP/App"; the
        // "VOIP" group sits between ">Device" and the "Web" group
        // (`app.cpp:1114-1115`).
        VoipApp { client: None }
    }

    /// `VOIPApp::init`: refuses while the app is quiting, otherwise creates
    /// the VOIPClient (and asserts it — `MF_ASSERT_DEV`).
    pub fn init(&mut self, is_quiting: bool) -> bool {
        if is_quiting {
            return false;
        }
        self.client = Some(Rc::new(RefCell::new(EmptyVoip::new())));
        true
    }

    /// `VOIPApp::fini`: destroys the client; the python module must not be
    /// used after this point.
    pub fn fini(&mut self) {
        self.client = None;
    }

    /// `VOIPApp::getVOIPClient` (`voip_app.cpp:142`): `MF_ASSERT_DEV` on a
    /// NULL client — the port panics with the same condition.
    pub fn client(&self) -> Rc<RefCell<EmptyVoip>> {
        self.client
            .as_ref()
            .expect("MF_ASSERT_DEV: voipClient_ is NULL")
            .clone()
    }

    /// `VOIPApp::tick` (`voip_app.cpp:97`): no player entity, or one not in
    /// the world, skips the backend tick entirely.
    pub fn tick(&self, player: Option<&PlayerTick>) {
        let Some(player) = player else {
            return;
        };
        if !player.in_world {
            return;
        }
        self.client().borrow_mut().tick(
            player.x,
            player.y,
            player.z,
            player.yaw,
            player.pitch,
            player.roll,
            player.space_id,
        );
    }
}

impl Default for VoipApp {
    fn default() -> VoipApp {
        VoipApp::new()
    }
}

/// The `BigWorld.VOIP` wrapper face (`client/bwvoip.cpp`).
///
/// Every script method validates its optional `data` dictionary and raises
/// the reference `TypeError` texts verbatim before forwarding to the
/// backend. The reference's two wrapper defects are preserved:
///
/// * `kickUser`'s dict check lacks its `!` — a *valid* dict raises (and the
///   text names `inviteUser`);
/// * `setHandler(None)` still installs a response object (the `if
///   (pCallback)` is always true), wrapping a NULL callback.
pub struct PyVoip {
    app: Rc<RefCell<VoipApp>>,
    response: Option<Rc<ScriptResponse>>,
}

impl PyVoip {
    /// `PY_MODULE_ATTRIBUTE(BigWorld, VOIP, new PyVOIP)` — the attribute is
    /// one long-lived object instance.
    pub fn new(app: Rc<RefCell<VoipApp>>) -> PyVoip {
        PyVoip {
            app,
            response: None,
        }
    }

    fn backend(&self) -> Rc<RefCell<EmptyVoip>> {
        self.app.borrow().client()
    }

    /// `py_initialise`: the optional data dict — note this text says
    /// *expected* where every other method says *expecting*.
    pub fn initialise(&mut self, data: Option<PyValue>) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend().borrow_mut().initialise(Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.initialise: expected dict for argument 1 (data)",
            ));
        }
        self.backend().borrow_mut().initialise(None);
        Ok(())
    }

    /// `py_finalise`: no arguments are parsed.
    pub fn finalise(&mut self) -> Result<(), TypeError> {
        self.backend().borrow_mut().finalise();
        Ok(())
    }

    /// `py_command`: the data dict is required (`"O"` parse).
    pub fn command(&mut self, data: PyValue) -> Result<(), TypeError> {
        match data {
            PyValue::Dict(py_dict) => {
                let dict = VoipDataDict::from_py_dict(&py_dict);
                self.backend().borrow_mut().command(Some(&dict));
                Ok(())
            }
            _ => Err(TypeError::new(
                "VOIP.command: expecting dict for argument 1 (data)",
            )),
        }
    }

    /// `py_getStatistics`.
    pub fn get_statistics(&mut self) -> Result<(), TypeError> {
        self.backend().borrow_mut().get_statistics();
        Ok(())
    }

    /// `py_setHandler` (`bwvoip.cpp:310`): accepts a callable or `None`.
    /// The `if (pCallback)` guard is always true, so `None` still installs
    /// a response object (wrapping a NULL callback).
    pub fn set_handler(&mut self, callback: PyValue) -> Result<(), TypeError> {
        if callback != PyValue::NoneValue && !matches!(callback, PyValue::Callable(_)) {
            return Err(TypeError::new(
                "VOIP.setHandler: expecting a callable object or None for argument 1",
            ));
        }
        // bw_safe_delete(response_) — the old object goes first.
        self.response = None;
        if let PyValue::Callable(name) = &callback {
            self.response = Some(Rc::new(ScriptResponse::new(Some(name))));
        } else {
            // The reference wraps Py_None here too.
            self.response = Some(Rc::new(ScriptResponse::new(None)));
        }
        let installed: Option<Rc<dyn VoipResponse>> =
            self.response.clone().map(|response| response as _);
        self.backend().borrow_mut().set_handler(installed);
        Ok(())
    }

    /// `py_login`.
    pub fn login(
        &mut self,
        username: &str,
        password: &str,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .login(username, password, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.login: expecting dict for optional argument 3 (data)",
            ));
        }
        self.backend().borrow_mut().login(username, password, None);
        Ok(())
    }

    /// `py_logout`.
    pub fn logout(&mut self, data: Option<PyValue>) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend().borrow_mut().logout(Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.logout: expecting dict for optional argument 1 (data)",
            ));
        }
        self.backend().borrow_mut().logout(None);
        Ok(())
    }

    /// `py_createChannel`: the password defaults to "".
    pub fn create_channel(
        &mut self,
        channel: &str,
        password: &str,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .create_channel(channel, password, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.createChannel: expecting dict for optional argument 3 (data)",
            ));
        }
        self.backend()
            .borrow_mut()
            .create_channel(channel, password, None);
        Ok(())
    }

    /// `py_joinChannel`: the password defaults to "".
    pub fn join_channel(
        &mut self,
        channel: &str,
        password: &str,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .join_channel(channel, password, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.joinChannel: expecting dict for optional argument 3 (data)",
            ));
        }
        self.backend()
            .borrow_mut()
            .join_channel(channel, password, None);
        Ok(())
    }

    /// `py_leaveChannel`.
    pub fn leave_channel(&mut self, channel: &str, data: Option<PyValue>) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .leave_channel(channel, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.leaveChannel: expecting dict for optional argument 2 (data)",
            ));
        }
        self.backend().borrow_mut().leave_channel(channel, None);
        Ok(())
    }

    /// `py_queryChannels`: `numberChannels()` × `getChannel(i)`.
    pub fn query_channels(&self) -> Vec<String> {
        let backend = self.backend();
        let backend = backend.borrow();
        let number = backend.number_channels();
        // With the shipped emptyvoip DLL `numberChannels()` is hard-wired
        // to 0, so this loop never iterates against the only enabled
        // target; a real voice SDK supplies the channels.
        (0..number)
            .map(|i| backend.get_channel(i as usize))
            .collect()
    }

    /// `py_inviteUser`.
    pub fn invite_user(
        &mut self,
        user: &str,
        channel: &str,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .invite_user(user, channel, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.inviteUser: expecting dict for optional argument 3 (data)",
            ));
        }
        self.backend().borrow_mut().invite_user(user, channel, None);
        Ok(())
    }

    /// `py_kickUser` (`bwvoip.cpp:618`): the reference's dict check lost
    /// its `!` (and names `inviteUser`), so a **valid** dict raises and any
    /// non-dict value passes through. Preserved verbatim.
    pub fn kick_user(
        &mut self,
        user: &str,
        channel: &str,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(_)) = &data {
            return Err(TypeError::new(
                "VOIP.inviteUser: expecting dict for optional argument 3 (data)",
            ));
        }
        // The broken check raised on a valid dict above, so whatever
        // remains is None or a non-dict object; the reference hands it to
        // BWVOIPDataDict, which extracts no pairs from either.
        let dict = data.as_ref().map(|_| VoipDataDict::new());
        self.backend()
            .borrow_mut()
            .kick_user(user, channel, dict.as_ref());
        Ok(())
    }

    /// `py_enableActiveChannel`.
    pub fn enable_active_channel(
        &mut self,
        channel: &str,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .enable_active_channel(channel, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.enableActiveChannel: expecting dict for optional argument 2 (data)",
            ));
        }
        self.backend()
            .borrow_mut()
            .enable_active_channel(channel, None);
        Ok(())
    }

    /// `py_disableActiveChannel`.
    pub fn disable_active_channel(
        &mut self,
        channel: &str,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .disable_active_channel(channel, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.disableActiveChannel: expecting dict for optional argument 2 (data)",
            ));
        }
        self.backend()
            .borrow_mut()
            .disable_active_channel(channel, None);
        Ok(())
    }

    /// `py_enablePositional`.
    pub fn enable_positional(&mut self, data: Option<PyValue>) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend().borrow_mut().enable_positional(Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.enablePositional: expecting dict for optional argument 1 (data)",
            ));
        }
        self.backend().borrow_mut().enable_positional(None);
        Ok(())
    }

    /// `py_disablePositional`.
    pub fn disable_positional(&mut self, data: Option<PyValue>) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend().borrow_mut().disable_positional(Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.disablePositional: expecting dict for optional argument 1 (data)",
            ));
        }
        self.backend().borrow_mut().disable_positional(None);
        Ok(())
    }

    /// `py_enableMicrophone`.
    pub fn enable_microphone(&mut self, data: Option<PyValue>) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend().borrow_mut().enable_microphone(Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.enableMicrophone: expecting dict for optional argument 1 (data)",
            ));
        }
        self.backend().borrow_mut().enable_microphone(None);
        Ok(())
    }

    /// `py_disableMicrophone`.
    pub fn disable_microphone(&mut self, data: Option<PyValue>) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend().borrow_mut().disable_microphone(Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.disableMicrophone: expecting dict for optional argument 1 (data)",
            ));
        }
        self.backend().borrow_mut().disable_microphone(None);
        Ok(())
    }

    /// `py_setMasterVolume`.
    pub fn set_master_volume(
        &mut self,
        attenuation_db: f32,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .set_master_volume(attenuation_db, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.setMasterVolume: expecting dict for optional argument 2 (data)",
            ));
        }
        self.backend()
            .borrow_mut()
            .set_master_volume(attenuation_db, None);
        Ok(())
    }

    /// `py_setChannelVolume`.
    pub fn set_channel_volume(
        &mut self,
        channel: &str,
        attenuation_db: f32,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .set_channel_volume(channel, attenuation_db, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.setChannelVolume: expecting dict for optional argument 3 (data)",
            ));
        }
        self.backend()
            .borrow_mut()
            .set_channel_volume(channel, attenuation_db, None);
        Ok(())
    }

    /// `py_setMicrophoneVolume`.
    pub fn set_microphone_volume(
        &mut self,
        attenuation_db: f32,
        data: Option<PyValue>,
    ) -> Result<(), TypeError> {
        if let Some(PyValue::Dict(py_dict)) = &data {
            let dict = VoipDataDict::from_py_dict(py_dict);
            self.backend()
                .borrow_mut()
                .set_microphone_volume(attenuation_db, Some(&dict));
            return Ok(());
        }
        if data.is_some() {
            return Err(TypeError::new(
                "VOIP.setMicrophoneVolume: expecting dict for optional argument 2 (data)",
            ));
        }
        self.backend()
            .borrow_mut()
            .set_microphone_volume(attenuation_db, None);
        Ok(())
    }

    /// `PyVOIP::tick` (`bwvoip.cpp:997`): a plain passthrough; the script
    /// signature is the same seven positional parameters as the DLL.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(&self, x: f32, y: f32, z: f32, yaw: f32, pitch: f32, roll: f32, space_id: i32) {
        self.backend()
            .borrow_mut()
            .tick(x, y, z, yaw, pitch, roll, space_id);
    }

    /// The installed script response object (for dispatching probes).
    pub fn response(&self) -> Option<Rc<ScriptResponse>> {
        self.response.clone()
    }
}

/// The VOIP implementation registry — the acceptance's 缺 SDK ledger.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VoipTarget {
    /// Implementation name as built in the reference tree.
    pub name: &'static str,
    /// The shared-library output names configured in its CMakeLists.
    pub binary_names: &'static [&'static str],
    /// Whether the implementation's source ships in the reference tree.
    pub sdk_source_available: bool,
}

/// The targets the reference tree actually provides: only the `emptyvoip`
/// stub exists (built as `voip`/`voip_d`/`voip_h` per
/// `lib/emptyvoip/CMakeLists.txt`). No third-party voice SDK source ships
/// with the tree, so any real implementation is registered here as
/// missing — recorded, not guessed at.
pub const VOIP_TARGETS: &[VoipTarget] = &[
    VoipTarget {
        name: "emptyvoip",
        binary_names: &["voip", "voip_d", "voip_h"],
        sdk_source_available: true,
    },
    VoipTarget {
        name: "third-party voice sdk",
        binary_names: &[],
        sdk_source_available: false,
    },
];

/// The enabled target: the only one whose source ships.
pub fn enabled_voip_target() -> &'static VoipTarget {
    &VOIP_TARGETS[0]
}

/// The main-loop group list around the VOIP slot (`app.cpp:1113-1116`):
/// "VOIP" runs after "Device" and before "Web", and `VOIPApp` registers
/// itself inside it as "VOIP/App".
pub const MAIN_LOOP_GROUPS: &[&str] = &[
    "Device", "VOIP", "Web", "Script", "Camera", "Canvas", "World", "Flora", "Facade", "Lens",
    "GUI", "Debug", "Finale",
];

pub const VOIP_MAIN_LOOP_TASK: &str = "VOIP/App";

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Rc<RefCell<VoipApp>> {
        let app = Rc::new(RefCell::new(VoipApp::new()));
        assert!(app.borrow_mut().init(false));
        app
    }

    fn wrapper(app: &Rc<RefCell<VoipApp>>) -> PyVoip {
        PyVoip::new(Rc::clone(app))
    }

    fn str_dict(entries: &[(&str, &PyValue)]) -> BTreeMap<String, PyValue> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), (*v).clone()))
            .collect()
    }

    fn ops(app: &Rc<RefCell<VoipApp>>) -> Vec<VoipOp> {
        app.borrow().client().borrow().ops().to_vec()
    }

    fn empty_pairs() -> Vec<(String, String)> {
        Vec::new()
    }

    #[test]
    fn data_dict_sorted_indexed_and_first_wins() {
        let py_dict = str_dict(&[
            ("zeta", &PyValue::Str("2".to_string())),
            ("alpha", &PyValue::Str("1".to_string())),
            // Non-string values are filtered out.
            ("count", &PyValue::Float(3.0)),
            ("obj", &PyValue::Other),
        ]);
        let dict = VoipDataDict::from_py_dict(&py_dict);
        assert_eq!(dict.size(), 2);
        assert_eq!(dict.get("alpha"), Some("1"));
        assert_eq!(dict.get("zeta"), Some("2"));
        assert_eq!(dict.get("missing"), None);
        // Indexed access iterates in sorted key order, not insertion order.
        assert_eq!(dict.get_index(0), Some(("alpha", "1")));
        assert_eq!(dict.get_index(1), Some(("zeta", "2")));
        assert_eq!(dict.get_index(2), None);
        assert_eq!(
            dict.pairs(),
            vec![
                ("alpha".to_string(), "1".to_string()),
                ("zeta".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn empty_dict_construction_is_allowed() {
        let dict = VoipDataDict::from_py_dict(&BTreeMap::new());
        assert_eq!(dict.size(), 0);
        assert_eq!(dict.get("any"), None);
        assert_eq!(dict.get_index(0), None);
    }

    #[test]
    fn enabled_target_passes_the_contract() {
        // The acceptance: the enabled target (emptyvoip) through every
        // contract method.
        let mut backend = EmptyVoip::new();

        // Lifecycle returns 0 and records.
        assert_eq!(backend.initialise(None), 0);
        let data = VoipDataDict::from_py_dict(&str_dict(&[("k", &PyValue::Str("v".to_string()))]));
        assert_eq!(backend.initialise(Some(&data)), 0);
        assert_eq!(backend.finalise(), 0);

        // Channel queries are permanently empty.
        assert_eq!(backend.number_channels(), 0);
        assert_eq!(backend.get_channel(0), "");
        assert_eq!(backend.get_channel(3), "");

        // The handler is stored, not invoked.
        let response = Rc::new(ScriptResponse::new(Some("on_voip")));
        backend.set_handler(Some(response.clone()));
        assert!(backend.handler().is_some());

        // The stub never dispatches anything on its own.
        assert!(response.calls().is_empty());

        assert_eq!(
            backend.ops(),
            &[
                VoipOp::Initialise {
                    data: empty_pairs()
                },
                VoipOp::Initialise {
                    data: vec![("k".to_string(), "v".to_string())]
                },
                VoipOp::Finalise,
                VoipOp::SetHandler { installed: true },
            ]
        );

        backend.clear_ops();
        assert!(backend.ops().is_empty());
    }

    #[test]
    fn wrapper_lifecycle_and_dict_forwarding() {
        let app = app();
        let mut voip = wrapper(&app);

        // The reference error verb here is "expected", not "expecting".
        let err = voip
            .initialise(Some(PyValue::Float(1.0)))
            .expect_err("non-dict raises");
        assert_eq!(
            err.message,
            "VOIP.initialise: expected dict for argument 1 (data)"
        );
        assert!(voip.initialise(None).is_ok());

        let py_dict = str_dict(&[("room", &PyValue::Str("lobby".to_string()))]);
        assert!(voip.initialise(Some(PyValue::Dict(py_dict))).is_ok());
        assert!(voip.finalise().is_ok());

        assert_eq!(
            ops(&app),
            &[
                VoipOp::Initialise {
                    data: empty_pairs()
                },
                VoipOp::Initialise {
                    data: vec![("room".to_string(), "lobby".to_string())]
                },
                VoipOp::Finalise,
            ]
        );
    }

    #[test]
    fn wrapper_command_and_statistics() {
        let app = app();
        let mut voip = wrapper(&app);

        // command's dict is required and its text has no "optional".
        let err = voip
            .command(PyValue::Str("nope".to_string()))
            .expect_err("non-dict raises");
        assert_eq!(
            err.message,
            "VOIP.command: expecting dict for argument 1 (data)"
        );

        let py_dict = str_dict(&[("cmd", &PyValue::Str("mute-all".to_string()))]);
        assert!(voip.command(PyValue::Dict(py_dict)).is_ok());
        assert!(voip.get_statistics().is_ok());
        assert_eq!(
            ops(&app),
            &[
                VoipOp::Command {
                    data: vec![("cmd".to_string(), "mute-all".to_string())]
                },
                VoipOp::GetStatistics,
            ]
        );
    }

    #[test]
    fn set_handler_accepts_callable_none_and_rejects_other() {
        let app = app();

        let err = {
            let mut voip = wrapper(&app);
            voip.set_handler(PyValue::Str("nope".to_string()))
                .expect_err("non-callable raises")
        };
        assert_eq!(
            err.message,
            "VOIP.setHandler: expecting a callable object or None for argument 1"
        );
        // The rejected call installed nothing.
        assert!(app.borrow().client().borrow().handler().is_none());

        let mut voip = wrapper(&app);
        assert!(
            voip.set_handler(PyValue::Callable("on_voip".to_string()))
                .is_ok()
        );
        assert!(
            voip.set_handler(PyValue::NoneValue).is_ok(),
            "None is accepted"
        );
        // `if (pCallback)` is always true: None still installs a response
        // object (with a NULL callback inside).
        let installed = app.borrow().client().borrow().handler().cloned();
        assert!(installed.is_some());
        let ops = ops(&app);
        assert_eq!(ops.len(), 2);
        assert!(matches!(&ops[1], VoipOp::SetHandler { installed: true }));

        // Dispatching through the None-callback response records nothing
        // (the C++ would call a NULL PyObject).
        let response = voip.response().expect("a response is installed");
        response.response(1, None);
        assert!(response.calls().is_empty());
        assert_eq!(response.callback(), None);
    }

    #[test]
    fn response_dispatch_builds_sorted_script_dict() {
        let app = app();
        let mut voip = wrapper(&app);
        assert!(
            voip.set_handler(PyValue::Callable("on_voip".to_string()))
                .is_ok()
        );
        let response = voip.response().unwrap();

        let data = VoipDataDict::from_py_dict(&str_dict(&[
            ("b", &PyValue::Str("2".to_string())),
            ("a", &PyValue::Str("1".to_string())),
        ]));
        response.response(7, Some(&data));
        response.response(9, None);
        assert_eq!(
            response.calls(),
            &[
                (
                    7,
                    vec![
                        ("a".to_string(), "1".to_string()),
                        ("b".to_string(), "2".to_string()),
                    ]
                ),
                (9, Vec::new()),
            ]
        );
        // The calls land sorted: "a" before "b" (map order, not the
        // b-then-a insertion order).
        let full = VoipDataDict::from_py_dict(&str_dict(&[
            ("b", &PyValue::Str("2".to_string())),
            ("a", &PyValue::Str("1".to_string())),
            ("c", &PyValue::Str("3".to_string())),
        ]));
        response.response(0, Some(&full));
        assert_eq!(
            response.calls()[2].1,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string()),
                ("c".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn wrapper_login_logout_channel_flows() {
        let app = app();
        let mut voip = wrapper(&app);

        assert!(voip.login("player", "secret", None).is_ok());
        let bad = voip.login("player", "secret", Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.login: expecting dict for optional argument 3 (data)"
        );
        let py_dict = str_dict(&[("srv", &PyValue::Str("eu".to_string()))]);
        assert!(
            voip.login("player", "secret", Some(PyValue::Dict(py_dict)))
                .is_ok()
        );

        assert!(voip.logout(None).is_ok());
        let bad = voip.logout(Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.logout: expecting dict for optional argument 1 (data)"
        );
        let logout_data = str_dict(&[("reason", &PyValue::Str("quit".to_string()))]);
        assert!(voip.logout(Some(PyValue::Dict(logout_data))).is_ok());

        // Channels: password defaults to "" when omitted.
        assert!(voip.create_channel("squad", "", None).is_ok());
        let channel_data = str_dict(&[("codec", &PyValue::Str("opus".to_string()))]);
        assert!(
            voip.create_channel("squad", "", Some(PyValue::Dict(channel_data.clone())))
                .is_ok()
        );
        let bad = voip.create_channel("squad", "", Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.createChannel: expecting dict for optional argument 3 (data)"
        );
        assert!(voip.join_channel("raid", "", None).is_ok());
        assert!(
            voip.join_channel("raid", "pw", Some(PyValue::Dict(channel_data.clone())))
                .is_ok()
        );
        let bad = voip.join_channel("raid", "", Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.joinChannel: expecting dict for optional argument 3 (data)"
        );
        assert!(voip.leave_channel("raid", None).is_ok());
        assert!(
            voip.leave_channel("raid", Some(PyValue::Dict(channel_data.clone())))
                .is_ok()
        );
        let bad = voip.leave_channel("raid", Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.leaveChannel: expecting dict for optional argument 2 (data)"
        );

        // The stub keeps the channel list empty.
        assert!(voip.query_channels().is_empty());

        let user = "buddy".to_string();
        let squad = "squad".to_string();
        assert_eq!(
            ops(&app),
            &[
                VoipOp::Login {
                    username: "player".to_string(),
                    password: "secret".to_string(),
                    data: empty_pairs(),
                },
                VoipOp::Login {
                    username: "player".to_string(),
                    password: "secret".to_string(),
                    data: vec![("srv".to_string(), "eu".to_string())],
                },
                VoipOp::Logout {
                    data: empty_pairs()
                },
                VoipOp::Logout {
                    data: vec![("reason".to_string(), "quit".to_string())],
                },
                VoipOp::CreateChannel {
                    channel: squad.clone(),
                    password: String::new(),
                    data: empty_pairs(),
                },
                VoipOp::CreateChannel {
                    channel: squad.clone(),
                    password: String::new(),
                    data: vec![("codec".to_string(), "opus".to_string())],
                },
                VoipOp::JoinChannel {
                    channel: "raid".to_string(),
                    password: String::new(),
                    data: empty_pairs(),
                },
                VoipOp::JoinChannel {
                    channel: "raid".to_string(),
                    password: "pw".to_string(),
                    data: vec![("codec".to_string(), "opus".to_string())],
                },
                VoipOp::LeaveChannel {
                    channel: "raid".to_string(),
                    data: empty_pairs(),
                },
                VoipOp::LeaveChannel {
                    channel: "raid".to_string(),
                    data: vec![("codec".to_string(), "opus".to_string())],
                },
            ]
        );
        let _ = user;
    }

    #[test]
    fn kick_user_inverted_check_raises_on_a_valid_dict() {
        let app = app();
        let mut voip = wrapper(&app);

        // The reference check lost its `!`: a *valid* dict raises, and the
        // text names inviteUser — both preserved verbatim.
        let py_dict = str_dict(&[("why", &PyValue::Str("afk".to_string()))]);
        let err = voip
            .kick_user("griefer", "squad", Some(PyValue::Dict(py_dict)))
            .expect_err("a valid dict raises");
        assert_eq!(
            err.message,
            "VOIP.inviteUser: expecting dict for optional argument 3 (data)"
        );

        // A non-dict (or absent) value passes through untouched.
        assert!(voip.kick_user("griefer", "squad", None).is_ok());
        assert!(
            voip.kick_user("griefer", "squad", Some(PyValue::Other))
                .is_ok()
        );
        assert_eq!(
            ops(&app),
            &[
                VoipOp::KickUser {
                    user: "griefer".to_string(),
                    channel: "squad".to_string(),
                    data: empty_pairs(),
                },
                VoipOp::KickUser {
                    user: "griefer".to_string(),
                    channel: "squad".to_string(),
                    data: empty_pairs(),
                },
            ]
        );
    }

    #[test]
    fn invite_user_validates_normally() {
        let app = app();
        let mut voip = wrapper(&app);

        let bad = voip.invite_user("buddy", "squad", Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.inviteUser: expecting dict for optional argument 3 (data)"
        );
        let py_dict = str_dict(&[("msg", &PyValue::Str("hi".to_string()))]);
        assert!(
            voip.invite_user("buddy", "squad", Some(PyValue::Dict(py_dict)))
                .is_ok()
        );
        assert!(voip.invite_user("buddy", "squad", None).is_ok());
        assert_eq!(ops(&app).len(), 2);
    }

    #[test]
    fn active_channel_switching_validates() {
        let app = app();
        let mut voip = wrapper(&app);

        assert!(voip.enable_active_channel("squad", None).is_ok());
        let bad = voip.enable_active_channel("squad", Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.enableActiveChannel: expecting dict for optional argument 2 (data)"
        );
        let switch_data = str_dict(&[("x", &PyValue::Str("1".to_string()))]);
        assert!(
            voip.enable_active_channel("squad", Some(PyValue::Dict(switch_data.clone())))
                .is_ok()
        );
        assert!(voip.disable_active_channel("squad", None).is_ok());
        assert!(
            voip.disable_active_channel("squad", Some(PyValue::Dict(switch_data)))
                .is_ok()
        );
        let bad = voip.disable_active_channel("squad", Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.disableActiveChannel: expecting dict for optional argument 2 (data)"
        );
        assert_eq!(
            ops(&app),
            &[
                VoipOp::EnableActiveChannel {
                    channel: "squad".to_string(),
                    data: empty_pairs(),
                },
                VoipOp::EnableActiveChannel {
                    channel: "squad".to_string(),
                    data: vec![("x".to_string(), "1".to_string())],
                },
                VoipOp::DisableActiveChannel {
                    channel: "squad".to_string(),
                    data: empty_pairs(),
                },
                VoipOp::DisableActiveChannel {
                    channel: "squad".to_string(),
                    data: vec![("x".to_string(), "1".to_string())],
                },
            ]
        );
    }

    #[test]
    fn positional_and_microphone_switches() {
        let app = app();
        let mut voip = wrapper(&app);
        let switch_data = str_dict(&[("x", &PyValue::Str("1".to_string()))]);

        assert!(voip.enable_positional(None).is_ok());
        let bad = voip.enable_positional(Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.enablePositional: expecting dict for optional argument 1 (data)"
        );
        assert!(
            voip.enable_positional(Some(PyValue::Dict(switch_data.clone())))
                .is_ok()
        );
        assert!(voip.disable_positional(None).is_ok());
        assert!(
            voip.disable_positional(Some(PyValue::Dict(switch_data.clone())))
                .is_ok()
        );
        let bad = voip.disable_positional(Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.disablePositional: expecting dict for optional argument 1 (data)"
        );
        assert!(voip.enable_microphone(None).is_ok());
        assert!(
            voip.enable_microphone(Some(PyValue::Dict(switch_data.clone())))
                .is_ok()
        );
        let bad = voip.enable_microphone(Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.enableMicrophone: expecting dict for optional argument 1 (data)"
        );
        assert!(voip.disable_microphone(None).is_ok());
        assert!(
            voip.disable_microphone(Some(PyValue::Dict(switch_data)))
                .is_ok()
        );
        let bad = voip.disable_microphone(Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.disableMicrophone: expecting dict for optional argument 1 (data)"
        );
        assert_eq!(
            ops(&app),
            &[
                VoipOp::EnablePositional,
                VoipOp::EnablePositional,
                VoipOp::DisablePositional,
                VoipOp::DisablePositional,
                VoipOp::EnableMicrophone,
                VoipOp::EnableMicrophone,
                VoipOp::DisableMicrophone,
                VoipOp::DisableMicrophone,
            ]
        );
    }

    #[test]
    fn volume_controls_validate_and_forward() {
        let app = app();
        let mut voip = wrapper(&app);
        let volume_data = str_dict(&[("curve", &PyValue::Str("linear".to_string()))]);

        assert!(voip.set_master_volume(-6.0, None).is_ok());
        let bad = voip.set_master_volume(-6.0, Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.setMasterVolume: expecting dict for optional argument 2 (data)"
        );
        assert!(
            voip.set_master_volume(-6.0, Some(PyValue::Dict(volume_data.clone())))
                .is_ok()
        );
        assert!(voip.set_channel_volume("squad", -3.0, None).is_ok());
        assert!(
            voip.set_channel_volume("squad", -3.0, Some(PyValue::Dict(volume_data.clone())))
                .is_ok()
        );
        let bad = voip.set_channel_volume("squad", -3.0, Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.setChannelVolume: expecting dict for optional argument 3 (data)"
        );
        assert!(voip.set_microphone_volume(-12.0, None).is_ok());
        assert!(
            voip.set_microphone_volume(-12.0, Some(PyValue::Dict(volume_data)))
                .is_ok()
        );
        let bad = voip.set_microphone_volume(-12.0, Some(PyValue::Other));
        assert_eq!(
            bad.expect_err("non-dict raises").message,
            "VOIP.setMicrophoneVolume: expecting dict for optional argument 2 (data)"
        );
        assert_eq!(
            ops(&app),
            &[
                VoipOp::SetMasterVolume {
                    attenuation_db: -6.0,
                },
                VoipOp::SetMasterVolume {
                    attenuation_db: -6.0,
                },
                VoipOp::SetChannelVolume {
                    channel: "squad".to_string(),
                    attenuation_db: -3.0,
                },
                VoipOp::SetChannelVolume {
                    channel: "squad".to_string(),
                    attenuation_db: -3.0,
                },
                VoipOp::SetMicrophoneVolume {
                    attenuation_db: -12.0,
                },
                VoipOp::SetMicrophoneVolume {
                    attenuation_db: -12.0,
                },
            ]
        );
    }

    #[test]
    fn wrapper_tick_passthrough_records_exact_arguments() {
        let app = app();
        let voip = wrapper(&app);
        voip.tick(1.0, 2.0, 3.0, 0.5, -0.25, 0.75, 42);
        assert_eq!(
            ops(&app),
            &[VoipOp::Tick {
                x: 1.0,
                y: 2.0,
                z: 3.0,
                yaw: 0.5,
                pitch: -0.25,
                roll: 0.75,
                space_id: 42,
            }]
        );
    }

    #[test]
    fn app_init_refuses_while_quiting() {
        let mut app = VoipApp::default();
        assert!(!app.init(true));
        // No client was created; reaching it asserts like MF_ASSERT_DEV.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.client();
        }));
        assert!(result.is_err());

        assert!(app.init(false));
        assert!(app.client().borrow().ops().is_empty());
        app.fini();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.client();
        }));
        assert!(result.is_err());
    }

    #[test]
    fn app_tick_gates_on_player_presence_and_world() {
        let app = app();

        // No player entity: no backend call.
        app.borrow().tick(None);
        // Not in the world: no backend call.
        app.borrow().tick(Some(&PlayerTick {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            space_id: 7,
            in_world: false,
        }));
        assert!(ops(&app).is_empty());

        // In the world: the position, head rotation and space go through.
        app.borrow().tick(Some(&PlayerTick {
            x: 10.0,
            y: 20.0,
            z: 30.0,
            yaw: 1.5,
            pitch: -0.1,
            roll: 0.0,
            space_id: 7,
            in_world: true,
        }));
        assert_eq!(
            ops(&app),
            &[VoipOp::Tick {
                x: 10.0,
                y: 20.0,
                z: 30.0,
                yaw: 1.5,
                pitch: -0.1,
                roll: 0.0,
                space_id: 7,
            }]
        );
    }

    #[test]
    fn sdk_registry_records_enabled_and_missing_targets() {
        // The enabled target is the shipped stub and it is first.
        let enabled = enabled_voip_target();
        assert_eq!(enabled.name, "emptyvoip");
        assert!(enabled.sdk_source_available);
        assert_eq!(enabled.binary_names, &["voip", "voip_d", "voip_h"]);

        // No third-party voice SDK source ships in the tree: registered as
        // missing rather than silently dropped.
        assert_eq!(VOIP_TARGETS.len(), 2);
        assert!(!VOIP_TARGETS[1].sdk_source_available);
        assert!(VOIP_TARGETS[1].binary_names.is_empty());
    }

    #[test]
    fn main_loop_slot_sits_between_device_and_web() {
        let voip_index = MAIN_LOOP_GROUPS
            .iter()
            .position(|name| *name == "VOIP")
            .expect("VOIP group registered");
        assert_eq!(MAIN_LOOP_GROUPS[voip_index - 1], "Device");
        assert_eq!(MAIN_LOOP_GROUPS[voip_index + 1], "Web");
        assert_eq!(VOIP_MAIN_LOOP_TASK, "VOIP/App");
    }
}
