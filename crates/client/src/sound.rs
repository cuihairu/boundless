//! BW-11-05-03: the FMOD sound event control/playback surface
//! (`lib/fmodsound/py_sound.cpp`, `sound_manager.cpp`,
//! `py_sound_parameter.cpp`).
//!
//! The acceptance compares *control and playback events*: every call a
//! PySound makes into FMOD (start/stop/mute/pause/pitch/volume/3D
//! attributes/user data/callbacks) and every manager-level resolution step
//! (project load, group lookup, event acquisition) is recorded on an
//! observable operation ledger ([`Op`]) so a scenario can be replayed and
//! compared against the reference call sequence.
//!
//! The port keeps the reference control flow verbatim, including its
//! quirks:
//!
//! * `FMOD_SUPPORT` is on and `ENABLE_JFIQ_HANDLING` is 1
//!   (`fmod_config.hpp`), so the JFIQ ("just fail if quietest") paths are
//!   live code. A consequence locked in the port: the FMOD
//!   `CALLBACKTYPE_STOLEN` handler only assigns `STATUS_STOLEN` when the
//!   sound *is* JFIQ and not stopped (`py_sound.cpp:1150`), so non-JFIQ
//!   sounds never enter the stolen state through the callback. The stolen
//!   branch of `PySound::update` is therefore unreachable under this build
//!   configuration (any stolen sound is virtual and is caught earlier) and
//!   is omitted, like the dead stolen branch of `PySound::name`.
//! * `play()` on a JFIQ sound does not start anything: it flips the status
//!   back to stolen so the manager's `updateJFIQ` tick can hand out a real
//!   handle later.
//! * Getter sentinels are preserved: pitch reads 0 (not -1) when stolen,
//!   volume/duration read -1, `name()` reads "<error>", `state()` reads
//!   "NULL" (with the copy-pasted "PySound::volume" error text).
//! * `SoundManager::dbToLinearLevel` computes the literal `1 / (db / -3)`
//!   — not a real dB conversion — and caps positive inputs at 0 dB, which
//!   yields negative infinity at exactly 0 dB.
//! * Event handles: `FMOD_EVENT_INFOONLY` instances are singletons per
//!   event definition; default instances are freshly allocated on every
//!   `getEvent`.
//!
//! Not ported (no client-side control surface, out of the item's scope):
//! the actual FMOD audio output and device setup of
//! `SoundManager::initialise` (channel counts, speaker modes, DSP buffers),
//! the soundbank binary/FSB registration machinery with its background
//! loader (`registerSoundBank`/`SoundBankLoader`), the music system
//! (`py_music_system.cpp`), occluders (`sound_occluder.cpp`), the reverb
//! preset machinery beyond master-category volume, the net-audition
//! listener, and the PyEventProject/PyEventGroup/PyEventCategory wrapper
//! objects (only their *path resolution* is ported, because handle
//! re-acquisition needs the group and index). Parameter re-acquisition
//! after a handle change is implicit: parameters are name-keyed on the
//! event definition instead of holding an FMOD parameter pointer.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

/// Value of `FMOD_EVENTPROPERTY_MAX_PLAYBACKS_BEHAVIOR` meaning "just fail
/// if quietest" (`sound_manager.cpp:29`).
pub const JUST_FAIL_IF_QUIETEST: i32 = 5;

/// Default `maxSoundSpeed` (`sound_manager.cpp:397`).
pub const DEFAULT_MAX_SOUND_SPEED: f32 = 100.0;

pub type EventId = usize;

/// `FMOD_EVENT_STATE_*` flags (`fmod_event.hpp`).
pub const EVENT_STATE_READY: u32 = 1;
pub const EVENT_STATE_LOADING: u32 = 2;
pub const EVENT_STATE_ERROR: u32 = 4;
pub const EVENT_STATE_PLAYING: u32 = 8;
pub const EVENT_STATE_CHANNELSACTIVE: u32 = 16;
pub const EVENT_STATE_INFOONLY: u32 = 32;

/// The FMOD event callback types exposed through the
/// `FMOD_EVENT_CALLBACKTYPE` enum map (`py_sound.cpp:1106`), in map order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CallbackType {
    SyncPoint,
    SoundDefStart,
    SoundDefEnd,
    Stolen,
    EventFinished,
    NetModified,
    SoundDefCreate,
    SoundDefRelease,
    SoundDefInfo,
    EventStarted,
    SoundDefSelectIndex,
    Occlusion,
}

/// The `PY_ENUM_VALUE` names, in map order (`py_sound.cpp:1107`), paired
/// with the enum values they convert to.
const CALLBACK_ENUM_MAP: [(CallbackType, &str); 12] = [
    (CallbackType::SyncPoint, "FMOD_EVENT_CALLBACKTYPE_SYNCPOINT"),
    (
        CallbackType::SoundDefStart,
        "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_START",
    ),
    (
        CallbackType::SoundDefEnd,
        "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_END",
    ),
    (CallbackType::Stolen, "FMOD_EVENT_CALLBACKTYPE_STOLEN"),
    (
        CallbackType::EventFinished,
        "FMOD_EVENT_CALLBACKTYPE_EVENTFINISHED",
    ),
    (
        CallbackType::NetModified,
        "FMOD_EVENT_CALLBACKTYPE_NET_MODIFIED",
    ),
    (
        CallbackType::SoundDefCreate,
        "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_CREATE",
    ),
    (
        CallbackType::SoundDefRelease,
        "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_RELEASE",
    ),
    (
        CallbackType::SoundDefInfo,
        "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_INFO",
    ),
    (
        CallbackType::EventStarted,
        "FMOD_EVENT_CALLBACKTYPE_EVENTSTARTED",
    ),
    (
        CallbackType::SoundDefSelectIndex,
        "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_SELECTINDEX",
    ),
    (CallbackType::Occlusion, "FMOD_EVENT_CALLBACKTYPE_OCCLUSION"),
];

impl CallbackType {
    /// Script converter lookup by enum-map name.
    pub fn from_name(name: &str) -> Option<CallbackType> {
        CALLBACK_ENUM_MAP
            .iter()
            .find(|(_, map_name)| *map_name == name)
            .map(|(value, _)| *value)
    }

    /// The enum-map name of this callback type.
    pub fn name(self) -> &'static str {
        CALLBACK_ENUM_MAP
            .iter()
            .find(|(value, _)| *value == self)
            .map(|(_, name)| *name)
            .expect("every callback type is in the enum map")
    }
}

/// `PySound::Status` (`py_sound.hpp`): playing, stolen (its event handle
/// was taken away), or stopped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Playing,
    Stolen,
    Stopped,
}

/// What `SoundManager::pyError` turns a pending Python error into:
/// EXCEPTION level propagates, WARNING prints and returns None, SILENT
/// clears and returns None (`sound_manager.cpp:2189`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorLevel {
    Silent,
    Warning,
    Exception,
}

/// A pending Python error that the EXCEPTION error level propagates.
#[derive(Clone, Debug)]
pub struct PyFail {
    pub error: String,
}

/// Minimal `soundMgr` engine_config keys consumed by the port of
/// `SoundManager::initialise`.
#[derive(Clone, Debug, Default)]
pub struct SoundConfig {
    /// `errorLevel`: "silent" | "warning" | "exception"; anything else
    /// logs and falls back to warning.
    pub error_level: Option<String>,
    /// `allowUnload`, default: the manager's current flag (true).
    pub allow_unload: Option<bool>,
    /// `maxSoundSpeed`, default 100.
    pub max_sound_speed: Option<f32>,
    /// `mediaPath`.
    pub media_path: Option<String>,
}

/// One event parameter definition (an `FMOD::EventParameter`).
#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: String,
    pub min: f32,
    pub max: f32,
}

impl ParamDef {
    pub fn new(name: &str, min: f32, max: f32) -> ParamDef {
        ParamDef {
            name: name.to_string(),
            min,
            max,
        }
    }
}

/// One event definition inside a project bank — the fake FMOD content.
#[derive(Clone, Debug)]
pub struct EventDef {
    pub name: String,
    pub index: i32,
    pub max_playbacks_behavior: i32,
    pub lengthms: f32,
    pub params: Vec<ParamDef>,
}

impl EventDef {
    pub fn new(name: &str, index: i32) -> EventDef {
        EventDef {
            name: name.to_string(),
            index,
            max_playbacks_behavior: 0,
            lengthms: 0.0,
            params: Vec::new(),
        }
    }

    /// Marks the definition as "just fail if quietest".
    pub fn jfiq(mut self) -> EventDef {
        self.max_playbacks_behavior = JUST_FAIL_IF_QUIETEST;
        self
    }

    /// Sets the event length in milliseconds (feeds `Sound::duration`).
    pub fn lengthms(mut self, ms: f32) -> EventDef {
        self.lengthms = ms;
        self
    }

    /// Adds an event parameter definition.
    pub fn param(mut self, name: &str, min: f32, max: f32) -> EventDef {
        self.params.push(ParamDef::new(name, min, max));
        self
    }
}

/// One event group of a project bank.
#[derive(Clone, Debug, Default)]
pub struct GroupDef {
    pub name: String,
    pub events: Vec<EventDef>,
}

impl GroupDef {
    pub fn new(name: &str, events: Vec<EventDef>) -> GroupDef {
        GroupDef {
            name: name.to_string(),
            events,
        }
    }
}

/// A minimal 3D vector, only the operations the reference sound code uses
/// (subtraction for velocity deltas, division by the frame time,
/// normalisation of the orientation, and the zero comparisons).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector3 {
    pub const ZERO: Vector3 = Vector3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    pub fn new(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3 { x, y, z }
    }

    pub fn length_squared(self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }

    /// Only ever called on non-zero vectors in the ported paths, matching
    /// the reference call sites.
    pub fn normalise(&mut self) {
        let length = self.length_squared().sqrt();
        *self /= length;
    }
}

impl std::ops::Sub for Vector3 {
    type Output = Vector3;

    fn sub(self, rhs: Vector3) -> Vector3 {
        Vector3::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl std::ops::Div<f32> for Vector3 {
    type Output = Vector3;

    fn div(self, rhs: f32) -> Vector3 {
        Vector3::new(self.x / rhs, self.y / rhs, self.z / rhs)
    }
}

impl std::ops::DivAssign<f32> for Vector3 {
    fn div_assign(&mut self, rhs: f32) {
        self.x /= rhs;
        self.y /= rhs;
        self.z /= rhs;
    }
}

/// The attached-model stand-in (`pModel_`): the pieces of PyModel the
/// sound code touches — its world position and whether `updateSound`
/// succeeds.
#[derive(Debug)]
pub struct Model {
    pub name: String,
    pub position: Vector3,
    pub update_sound_ok: bool,
}

impl Model {
    pub fn new(name: &str) -> Model {
        Model {
            name: name.to_string(),
            position: Vector3::ZERO,
            update_sound_ok: true,
        }
    }
}

/// The operation ledger: one entry per FMOD API call the ported code
/// makes, in call order. This is the correspondence surface the acceptance
/// compares.
#[derive(Clone, PartialEq, Debug)]
pub enum Op {
    LoadProject {
        project: String,
    },
    GetGroup {
        project: String,
        group: String,
    },
    GetEvent {
        project: String,
        group: String,
        name: String,
        event: Option<EventId>,
    },
    GetEventByIndex {
        project: String,
        group: String,
        index: i32,
        info_only: bool,
        event: Option<EventId>,
    },
    SetUserData {
        event: EventId,
        sound: usize,
    },
    ClearUserData {
        event: EventId,
    },
    SetCallback {
        event: EventId,
    },
    ClearCallback {
        event: EventId,
    },
    Start {
        event: EventId,
    },
    Stop {
        event: EventId,
    },
    SetMute {
        event: EventId,
        value: bool,
    },
    SetPaused {
        event: EventId,
        value: bool,
    },
    SetPitch {
        event: EventId,
        value: f32,
    },
    SetVolume {
        event: EventId,
        value: f32,
    },
    Set3DAttributes {
        event: EventId,
        position: Option<Vector3>,
        velocity: Option<Vector3>,
        direction: Option<Vector3>,
    },
    SetListener {
        position: Vector3,
        velocity: Vector3,
        forward: Vector3,
        up: Vector3,
    },
    CategoryVolume {
        name: String,
        value: f32,
    },
    SetParamValue {
        event: EventId,
        name: String,
        value: f32,
    },
    SetParamVelocity {
        event: EventId,
        name: String,
        value: f32,
    },
    SetParamSeekSpeed {
        event: EventId,
        name: String,
        value: f32,
    },
    KeyOffParam {
        event: EventId,
        name: String,
    },
}

/// FMOD result codes the port distinguishes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FmodError {
    /// `FMOD_ERR_INVALID_HANDLE`: the handle was invalidated (e.g. by a
    /// group unload).
    InvalidHandle,
    /// `FMOD_ERR_EVENT_FAILED`: the event "just failed" (JFIQ quietest).
    EventFailed,
    /// Any other failure.
    Generic,
}

/// Injected FMOD failures, one flag per failure the reference code
/// handles. Every flag is consumed once by the next matching call.
#[derive(Clone, Debug, Default)]
pub struct Injections {
    pub fail_next_start: bool,
    pub fail_next_stop: bool,
    pub fail_next_set_user_data: bool,
    pub fail_next_set_callback: bool,
    pub fail_next_get_event: bool,
    pub fail_next_get_event_by_index: bool,
    /// The next default-mode `getEventByIndex` fails with
    /// `FMOD_ERR_EVENT_FAILED` (the JFIQ "just failed" result).
    pub fail_event_failed_once: bool,
    /// The next `set3DAttributes` fails with `FMOD_ERR_INVALID_HANDLE`.
    pub invalid_handle_on_set_3d: bool,
    /// `getState` reports `FMOD_ERR_INVALID_HANDLE` until cleared.
    pub get_state_invalid: bool,
    /// The next `getPropertyByIndex` (max playbacks behaviour) fails.
    pub fail_next_property: bool,
    /// The next default-mode `getEvent` fails (a fresh default instance
    /// cannot be acquired — e.g. max playbacks exhausted) while INFOONLY
    /// acquisitions keep succeeding.
    pub fail_default_get_event: bool,
    /// The "master" event category does not exist.
    pub master_category_missing: bool,
}

/// The group/project pair a sound keeps so it can re-acquire handles by
/// position (`pGroup_` + `index_`).
#[derive(Clone, PartialEq, Eq, Debug)]
struct GroupKey {
    project: String,
    group: String,
}

struct EventData {
    name: String,
    state: u32,
    muted: bool,
    paused: bool,
    pitch: f32,
    volume: f32,
    lengthms: f32,
    def_index: i32,
    max_playbacks_behavior: i32,
    position: Vector3,
    velocity: Option<Vector3>,
    direction: Option<Vector3>,
    user_data: Option<usize>,
    callback_set: bool,
}

impl EventData {
    fn from_def(def: &EventDef, info_only: bool) -> EventData {
        EventData {
            name: def.name.clone(),
            state: EVENT_STATE_READY | if info_only { EVENT_STATE_INFOONLY } else { 0 },
            muted: false,
            paused: false,
            pitch: 1.0,
            volume: 1.0,
            lengthms: def.lengthms,
            def_index: def.index,
            max_playbacks_behavior: def.max_playbacks_behavior,
            position: Vector3::ZERO,
            velocity: None,
            direction: None,
            user_data: None,
            callback_set: false,
        }
    }
}

struct ListenerState {
    position: Vector3,
    velocity: Vector3,
    set: bool,
}

/// The `SoundManager` port: project/group/event resolution, the listener,
/// master volume, the JFIQ list, and the observable FMOD operation ledger.
pub struct SoundSystem {
    initialised: bool,
    error_level: ErrorLevel,
    allow_unload: bool,
    max_sound_speed: f32,
    media_path: String,

    /// Projects "on disk" (the `.fev` files parsePath can auto-load).
    disk: BTreeMap<String, Vec<GroupDef>>,
    /// Loaded projects: name -> group defs.
    projects: BTreeMap<String, Vec<GroupDef>>,
    /// The `eventGroups_` cache: (project, group) pairs already resolved.
    groups: BTreeMap<(String, String), ()>,
    default_project: Option<String>,

    next_event: EventId,
    events: BTreeMap<EventId, EventData>,
    /// `FMOD_EVENT_INFOONLY` instances are singletons per event
    /// definition: (project, group, index) -> handle.
    info_events: BTreeMap<(String, String, i32), EventId>,

    next_sound: usize,
    sounds: BTreeMap<usize, Weak<RefCell<Sound>>>,
    /// The `justFailIfQuietestList_`, in insertion order.
    jfiq: Vec<(usize, Weak<RefCell<Sound>>)>,

    listener: ListenerState,

    /// The FMOD operation ledger.
    pub ops: Vec<Op>,
    /// (sound id, callback type) for every dispatched callback that had a
    /// script registration.
    pub fired_callbacks: Vec<(usize, CallbackType)>,
    /// ERROR_MSG / WARNING_MSG texts, in emit order.
    pub log: Vec<String>,
    pub injections: Injections,
    /// The failure code of the last event-handle acquisition attempt.
    last_acquire_error: Option<FmodError>,
}

impl SoundSystem {
    /// Creates the manager; always wrapped in an `Rc` so sounds can hold a
    /// weak back-reference.
    pub fn new() -> Rc<RefCell<SoundSystem>> {
        Rc::new(RefCell::new(SoundSystem {
            initialised: false,
            error_level: ErrorLevel::Warning,
            allow_unload: true,
            max_sound_speed: DEFAULT_MAX_SOUND_SPEED,
            media_path: String::new(),
            disk: BTreeMap::new(),
            projects: BTreeMap::new(),
            groups: BTreeMap::new(),
            default_project: None,
            next_event: 0,
            events: BTreeMap::new(),
            info_events: BTreeMap::new(),
            next_sound: 0,
            sounds: BTreeMap::new(),
            jfiq: Vec::new(),
            listener: ListenerState {
                position: Vector3::ZERO,
                velocity: Vector3::ZERO,
                set: false,
            },
            ops: Vec::new(),
            fired_callbacks: Vec::new(),
            log: Vec::new(),
            injections: Injections::default(),
            last_acquire_error: None,
        }))
    }

    pub fn is_initialised(&self) -> bool {
        self.initialised
    }

    pub fn error_level(&self) -> ErrorLevel {
        self.error_level
    }

    pub fn set_error_level(&mut self, level: ErrorLevel) {
        self.error_level = level;
    }

    pub fn allow_unload(&self) -> bool {
        self.allow_unload
    }

    pub fn set_allow_unload(&mut self, value: bool) {
        self.allow_unload = value;
    }

    pub fn max_sound_speed(&self) -> f32 {
        self.max_sound_speed
    }

    pub fn set_max_sound_speed(&mut self, value: f32) {
        self.max_sound_speed = value;
    }

    /// `SoundManager::initialise` port: only the config keys the control
    /// surface consumes; the FMOD device setup is not ported.
    pub fn initialise(&mut self, config: Option<&SoundConfig>) -> bool {
        self.initialised = true;
        let Some(config) = config else {
            return true;
        };
        if let Some(level) = &config.error_level {
            match level.as_str() {
                "silent" => self.error_level = ErrorLevel::Silent,
                "warning" => self.error_level = ErrorLevel::Warning,
                "exception" => self.error_level = ErrorLevel::Exception,
                _ => {
                    self.log.push(format!(
                        "SoundManager::initialise: Unrecognised value for soundMgr/errorLevel: {level}"
                    ));
                    self.error_level = ErrorLevel::Warning;
                }
            }
        }
        if let Some(path) = &config.media_path {
            self.set_path(path);
        }
        if let Some(value) = config.allow_unload {
            self.allow_unload = value;
        }
        if let Some(value) = config.max_sound_speed {
            self.max_sound_speed = value;
        }
        true
    }

    /// `SoundManager::setPath`: rejects the empty path, stores the raw
    /// media path.
    pub fn set_path(&mut self, path: &str) -> bool {
        if !self.initialised {
            return false;
        }
        if path.is_empty() {
            self.log
                .push("SoundManager::setPath: Called with an empty path".to_string());
            return false;
        }
        self.media_path = path.to_string();
        true
    }

    /// Installs a project definition "on disk" so parsePath can auto-load
    /// it.
    pub fn add_project_to_disk(&mut self, name: &str, groups: Vec<GroupDef>) {
        self.disk.insert(name.to_string(), groups);
    }

    /// `SoundManager::loadEventProject`: parsePath on "/name" with
    /// project-only resolution. Already-loaded projects succeed without a
    /// new LoadProject op.
    pub fn load_event_project(&mut self, name: &str) -> bool {
        let path = format!("/{name}");
        self.parse_path(&path, false, true, false).is_some()
    }

    /// `SoundManager::unloadEventProject`: refuses when unloading is
    /// disabled, fails with "not currently loaded" when the project is not
    /// loaded (allowLoadProject=false), clears the JFIQ list entries of
    /// that project (marking them unloaded), purges the group cache and
    /// the project, and resets the default project if it pointed there.
    pub fn unload_event_project(&mut self, name: &str) -> bool {
        if !self.allow_unload {
            self.log
                .push("Unloading soundbanks is disabled".to_string());
            return false;
        }
        let path = format!("/{name}");
        if self.parse_path(&path, false, false, false).is_none() {
            self.log
                .push(format!("Soundbank '{name}' is not currently loaded!"));
            return false;
        }

        // Clear JFIQ list of events using this project.
        let mut kept = Vec::new();
        for (id, weak) in self.jfiq.drain(..) {
            let same_project = weak
                .upgrade()
                .and_then(|sound| sound.borrow().group.clone())
                .map(|key| key.project == name)
                .unwrap_or(false);
            if same_project {
                if let Some(sound) = weak.upgrade() {
                    sound.borrow_mut().unloaded = true;
                }
            } else {
                kept.push((id, weak));
            }
        }
        self.jfiq = kept;

        self.groups.retain(|(project, _), _| project != name);
        self.projects.remove(name);
        if self.default_project.as_deref() == Some(name) {
            self.default_project = None;
        }
        true
    }

    /// The number of entries currently on the JFIQ list.
    pub fn jfiq_len(&self) -> usize {
        self.jfiq.len()
    }

    /// The sound ids currently on the JFIQ list.
    pub fn jfiq_ids(&self) -> Vec<usize> {
        self.jfiq
            .iter()
            .filter_map(|(_, weak)| weak.upgrade().map(|sound| sound.borrow().id))
            .collect()
    }

    /// The state flags of an event handle (read-only probe).
    pub fn event_state(&self, event: EventId) -> Option<u32> {
        self.events.get(&event).map(|data| data.state)
    }

    /// Overwrites an event's state flags (fake plumbing so tests can
    /// model LOADING/ERROR/CHANNELSACTIVE combinations).
    pub fn set_event_state(&mut self, event: EventId, state: u32) {
        if let Some(data) = self.events.get_mut(&event) {
            data.state = state;
        }
    }

    /// Whether an event handle has a callback registered.
    pub fn event_has_callback(&self, event: EventId) -> bool {
        self.events
            .get(&event)
            .map(|data| data.callback_set)
            .unwrap_or(false)
    }

    fn py_error_option(&mut self, error: String) -> Result<Option<Rc<RefCell<Sound>>>, PyFail> {
        match self.error_level {
            ErrorLevel::Exception => Err(PyFail { error }),
            ErrorLevel::Warning => {
                // PyErr_PrintEx(0): the error is printed and swallowed.
                self.log.push(format!("pyError printed: {error}"));
                Ok(None)
            }
            ErrorLevel::Silent => Ok(None),
        }
    }

    /// `SoundManager::createPySound` (`sound_manager.cpp:103`): resolves
    /// the INFOONLY singleton first, reads the max-playbacks-behaviour
    /// property (value 5 = JFIQ), and for non-JFIQ events acquires a
    /// fresh default instance. JFIQ sounds start stolen, bound to the
    /// info event, and go onto the manager JFIQ list; non-JFIQ sounds
    /// start playing with user data and the event callback installed.
    pub fn create_sound(
        system: &Rc<RefCell<SoundSystem>>,
        path: &str,
    ) -> Result<Option<Rc<RefCell<Sound>>>, PyFail> {
        let mut sys = system.borrow_mut();
        if !sys.initialised {
            return sys.py_error_option("Sound system is not initialised.".to_string());
        }

        let info = sys.parse_path(path, true, true, true);
        let Some((info_event, project, group)) = info.and_then(|parsed| {
            parsed
                .event
                .map(|event| (event, parsed.project, parsed.group))
        }) else {
            return sys.py_error_option(format!(
                "Could not load sound event '{path}'. See logs for details."
            ));
        };

        if sys.injections.fail_next_property {
            sys.injections.fail_next_property = false;
            return sys.py_error_option("PySound::PySound()".to_string());
        }

        let jfiq = sys.events[&info_event].max_playbacks_behavior == JUST_FAIL_IF_QUIETEST;

        let event = if jfiq {
            // The JFIQ sound keeps the info-only handle; updateJFIQ
            // replaces it with a real instance once one is available.
            Some(info_event)
        } else {
            sys.parse_path(path, true, true, false)
                .and_then(|parsed| parsed.event)
        };
        let Some(event) = event else {
            return sys.py_error_option(format!(
                "Could not load sound event '{path}'. See logs for details."
            ));
        };

        let index = sys.events[&event].def_index;
        let group = group.expect("event resolution always yields a group");
        let group_key = GroupKey { project, group };
        let path_string = sys.abs_path(path).unwrap_or_default();

        let id = sys.next_sound;
        sys.next_sound += 1;

        let sound = Rc::new(RefCell::new(Sound {
            system: Rc::downgrade(system),
            id,
            event: Some(event),
            group: Some(group_key),
            index,
            path: path_string,
            jfiq,
            status: if jfiq {
                Status::Stolen
            } else {
                Status::Playing
            },
            unloaded: false,
            model: None,
            position: None,
            last_position: Vector3::ZERO,
            callbacks: BTreeMap::new(),
        }));

        sys.sounds.insert(id, Rc::downgrade(&sound));
        if jfiq {
            sys.jfiq.push((id, Rc::downgrade(&sound)));
        } else {
            sys.set_user_data(event, id);
            sys.set_callback(event);
        }
        Ok(Some(sound))
    }

    /// `SoundManager::play(name)`: acquire a handle, start it.
    pub fn play(&mut self, name: &str) -> Option<EventId> {
        let event = self.get_event(name)?;
        if self.start_event(event) {
            Some(event)
        } else {
            self.log
                .push(format!("SoundManager::play: Failed to play '{name}'"));
            None
        }
    }

    /// `SoundManager::play(name, pos)`: acquire, set 3D attributes
    /// (failure only logged), start.
    pub fn play_at(&mut self, name: &str, pos: Vector3) -> Option<EventId> {
        let event = self.get_event(name)?;
        if self
            .set_3d_attributes(event, Some(pos), None, None)
            .is_err()
        {
            self.log.push(format!(
                "SoundManager::play: Failed to set 3D attributes for {name}"
            ));
        }
        if self.start_event(event) {
            Some(event)
        } else {
            self.log
                .push(format!("SoundManager::play: Failed to play '{name}'"));
            None
        }
    }

    /// `SoundManager::get(name)`: default-mode event acquisition.
    pub fn get_event(&mut self, path: &str) -> Option<EventId> {
        self.parse_path(path, true, true, false)
            .and_then(|parsed| parsed.event)
    }

    /// `SoundManager::get(pGroup, index)` — by-position acquisition used
    /// for handle re-acquisition and the JFIQ tick. The last failure code
    /// lands in `last_acquire_error` so callers can tell the JFIQ "just
    /// failed" result from other failures.
    pub fn get_event_by_index(
        &mut self,
        project: &str,
        group: &str,
        index: i32,
    ) -> Option<EventId> {
        self.get_event_by_index_impl(project, group, index, false)
    }

    fn get_event_by_index_impl(
        &mut self,
        project: &str,
        group: &str,
        index: i32,
        info_only: bool,
    ) -> Option<EventId> {
        self.last_acquire_error = None;
        let mut event = None;
        if self.injections.fail_next_get_event_by_index {
            self.injections.fail_next_get_event_by_index = false;
            self.last_acquire_error = Some(FmodError::Generic);
        } else if self.injections.fail_event_failed_once && !info_only {
            self.injections.fail_event_failed_once = false;
            self.last_acquire_error = Some(FmodError::EventFailed);
        } else {
            let found = self.projects.get(project).and_then(|defs| {
                defs.iter()
                    .find(|g| g.name == group)
                    .and_then(|g| g.events.iter().find(|e| e.index == index).cloned())
            });
            if let Some(def) = found {
                event = Some(self.allocate_event(project, group, &def, info_only));
            }
        }
        self.ops.push(Op::GetEventByIndex {
            project: project.to_string(),
            group: group.to_string(),
            index,
            info_only,
            event,
        });
        if event.is_none() && self.last_acquire_error.is_none() {
            self.last_acquire_error = Some(FmodError::Generic);
            self.log.push(format!(
                "SoundManager::get: Couldn't get event #{index} from group"
            ));
        }
        event
    }

    /// `SoundManager::setDefaultProject`.
    pub fn set_default_project(&mut self, name: &str) -> bool {
        let path = format!("/{name}");
        self.parse_path(&path, false, true, false).is_some()
    }

    /// `SoundManager::absPath`: absolute paths pass through; relative
    /// paths are prefixed with the default project name.
    pub fn abs_path(&mut self, path: &str) -> Option<String> {
        if path.starts_with('/') {
            return Some(path.to_string());
        }
        match &self.default_project {
            Some(project) => Some(format!("/{project}/{path}")),
            None => {
                // PyErr_Format in the reference; logged here.
                self.log
                    .push("Can't resolve absolute path with no default project".to_string());
                None
            }
        }
    }

    /// `SoundManager::setListenerPosition`: velocity from the position
    /// delta (zero on the first call or for non-positive delta), the
    /// teleport clamp against `maxSoundSpeed`, the FMOD listener call,
    /// then the manager update tick.
    pub fn set_listener_position(
        &mut self,
        position: Vector3,
        forward: Vector3,
        up: Vector3,
        delta_time: f32,
    ) -> bool {
        if !self.initialised {
            return false;
        }
        if self.listener.set {
            if delta_time > 0.0 {
                self.listener.velocity = (position - self.listener.position) / delta_time;
            } else {
                self.listener.velocity = Vector3::ZERO;
            }
            self.listener.position = position;
        } else {
            self.listener.set = true;
            self.listener.position = position;
            self.listener.velocity = Vector3::ZERO;
        }

        let max_speed = self.max_sound_speed;
        if max_speed > 0.0 && self.listener.velocity.length_squared() > max_speed * max_speed {
            // Assume the listener has teleported.
            self.listener.velocity = Vector3::ZERO;
        }

        self.ops.push(Op::SetListener {
            position: self.listener.position,
            velocity: self.listener.velocity,
            forward,
            up,
        });

        self.update(delta_time)
    }

    /// `SoundManager::getListenerPosition`.
    pub fn listener_position(&self) -> (Vector3, Vector3) {
        (self.listener.position, self.listener.velocity)
    }

    /// `SoundManager::setMasterVolume`: the "master" category must exist.
    pub fn set_master_volume(&mut self, volume: f32) -> bool {
        if !self.initialised {
            self.log.push(
                "SoundManager::setMasterVolume: No sound subsystem, can't set master volume"
                    .to_string(),
            );
            return false;
        }
        if self.injections.master_category_missing {
            self.log.push(
                "SoundManager::setMasterVolume: Couldn't get master EventCategory".to_string(),
            );
            return false;
        }
        self.ops.push(Op::CategoryVolume {
            name: "master".to_string(),
            value: volume,
        });
        true
    }

    /// `SoundManager::dbToLinearLevel` — the literal (and not physically
    /// meaningful) `1 / (db / -3)`, with positive levels capped at 0 dB.
    pub fn db_to_linear_level(&mut self, db: f32) -> f32 {
        let db = if db > 0.0 {
            self.log.push(format!(
                "SoundManager::dbToLinearLevel: Level > 0dB passed in ({db}) - capping to 0dB"
            ));
            0.0
        } else {
            db
        };
        1.0 / (db / -3.0)
    }

    /// `SoundManager::update`: the manager tick. Drives the JFIQ list;
    /// the soundbank waiting list and net update are not ported.
    pub fn update(&mut self, _delta_time: f32) -> bool {
        if !self.initialised {
            return false;
        }
        // The tick never mutates the JFIQ list itself, so a snapshot of
        // the weak handles is enough to avoid aliasing `self`.
        let list = self.jfiq.clone();
        for (_, weak) in &list {
            if let Some(sound) = weak.upgrade() {
                let mut sound = sound.borrow_mut();
                sound.update_jfiq_with(self);
            }
        }
        true
    }

    /// Dispatches an FMOD event callback: looks the owning sound up
    /// through the event's user data, runs the status state machine, and
    /// records the callback if the sound registered one for this type.
    pub fn fire_callback(&mut self, event: EventId, kind: CallbackType) {
        let target = self
            .events
            .get(&event)
            .and_then(|data| data.user_data)
            .and_then(|id| self.sounds.get(&id).and_then(Weak::upgrade));
        let Some(sound) = target else {
            return;
        };
        if let Some(id) = sound.borrow_mut().handle_event_callback(kind) {
            self.fired_callbacks.push((id, kind));
        }
    }

    /// parsePath (`sound_manager.cpp:1931`), verbatim:
    ///
    /// * absolute paths (`/project/group/event`) resolve the project by
    ///   name, auto-loading from disk unless `allow_load` is false;
    /// * relative paths resolve against the default project;
    /// * `want_group=false` stops after the project; an empty group part
    ///   (e.g. `/project`) resolves to no group;
    /// * `want_event` splits on the last slash — no slash past
    ///   `group_start` is an "illegal top-level event".
    fn parse_path(
        &mut self,
        path: &str,
        event_mode: bool,
        allow_load: bool,
        info_only: bool,
    ) -> Option<Parsed> {
        if !self.initialised {
            return None;
        }
        if path.is_empty() {
            self.log
                .push(format!("SoundManager::parsePath: Invalid path '{path}'"));
            return None;
        }

        let bytes = path.as_bytes();
        let group_start;
        let project;
        if bytes[0] == b'/' {
            let first_slash = path[1..].find('/').map(|i| i + 1);
            group_start = match first_slash {
                Some(i) => i + 1,
                None => path.len(),
            };
            let name_end = first_slash.unwrap_or(path.len());
            let project_name = &path[1..name_end];
            if !self.projects.contains_key(project_name) {
                if !allow_load {
                    return None;
                }
                let Some(groups) = self.disk.get(project_name) else {
                    // The reference concatenates mediaPath + projectName +
                    // ".fev" without inserting a separator.
                    let media = format!("{}{}.fev", self.media_path, project_name);
                    self.log
                        .push(format!("SoundManager::parsePath: Failed to load '{media}'"));
                    return None;
                };
                let groups = groups.clone();
                self.ops.push(Op::LoadProject {
                    project: project_name.to_string(),
                });
                self.projects.insert(project_name.to_string(), groups);
                if self.default_project.is_none() {
                    self.default_project = Some(project_name.to_string());
                }
            }
            project = project_name.to_string();
        } else {
            group_start = 0;
            match &self.default_project {
                Some(name) => project = name.clone(),
                None => {
                    self.log.push(format!(
                        "SoundManager::parsePath: No project specified and no default project loaded: {path}"
                    ));
                    return None;
                }
            }
        }

        if !event_mode {
            return Some(Parsed {
                project,
                group: None,
                event: None,
            });
        }

        // The reference splits on the last slash at or past group_start;
        // no such slash is an "illegal top-level event".
        let last_slash = path.rfind('/').filter(|i| *i >= group_start);
        let Some(last_slash) = last_slash else {
            self.log.push(format!(
                "SoundManager::parsePath: Asked for illegal top-level event '{path}'"
            ));
            return None;
        };
        let group_name = path[group_start..last_slash].to_string();
        let event_name = path[last_slash + 1..].to_string();

        // An empty group part cannot occur here: for absolute paths the
        // top-level-event check above fires first, for relative paths
        // group_name is non-empty by construction.

        let key = (project.clone(), group_name.clone());
        if !self.groups.contains_key(&key) {
            self.ops.push(Op::GetGroup {
                project: project.clone(),
                group: group_name.clone(),
            });
            if !self
                .projects
                .get(&project)
                .map(|defs| defs.iter().any(|g| g.name == group_name))
                .unwrap_or(false)
            {
                self.log.push(format!(
                    "SoundManager::parsePath: Couldn't get event group '{group_name}'"
                ));
                return None;
            }
            self.groups.insert(key, ());
        }

        let def = self
            .projects
            .get(&project)
            .and_then(|defs| defs.iter().find(|g| g.name == group_name))
            .and_then(|g| g.events.iter().find(|e| e.name == event_name))
            .cloned();
        let Some(def) = def else {
            self.ops.push(Op::GetEvent {
                project,
                group: group_name.clone(),
                name: event_name.clone(),
                event: None,
            });
            self.log.push(format!(
                "SoundManager::parsePath: Couldn't get event {event_name} from group {group_name}"
            ));
            return None;
        };

        if self.injections.fail_next_get_event
            || (self.injections.fail_default_get_event && !info_only)
        {
            if self.injections.fail_next_get_event {
                self.injections.fail_next_get_event = false;
            } else {
                self.injections.fail_default_get_event = false;
            }
            self.ops.push(Op::GetEvent {
                project: project.clone(),
                group: group_name.clone(),
                name: event_name.clone(),
                event: None,
            });
            return None;
        }
        let event = self.allocate_event(&project, &group_name, &def, info_only);
        self.ops.push(Op::GetEvent {
            project: project.clone(),
            group: group_name.clone(),
            name: event_name.clone(),
            event: Some(event),
        });
        Some(Parsed {
            project,
            group: Some(group_name),
            event: Some(event),
        })
    }

    /// Allocates an event instance: fresh for default mode, the singleton
    /// info handle for INFOONLY mode.
    fn allocate_event(
        &mut self,
        project: &str,
        group: &str,
        def: &EventDef,
        info_only: bool,
    ) -> EventId {
        if info_only {
            let key = (project.to_string(), group.to_string(), def.index);
            if let Some(existing) = self.info_events.get(&key) {
                return *existing;
            }
            let id = self.next_event;
            self.next_event += 1;
            self.events.insert(id, EventData::from_def(def, true));
            self.info_events.insert(key, id);
            return id;
        }
        let id = self.next_event;
        self.next_event += 1;
        self.events.insert(id, EventData::from_def(def, false));
        id
    }

    fn start_event(&mut self, event: EventId) -> bool {
        self.ops.push(Op::Start { event });
        if self.injections.fail_next_start {
            self.injections.fail_next_start = false;
            return false;
        }
        if let Some(data) = self.events.get_mut(&event) {
            data.state |= EVENT_STATE_PLAYING;
        }
        true
    }

    fn stop_event(&mut self, event: EventId) -> bool {
        self.ops.push(Op::Stop { event });
        if self.injections.fail_next_stop {
            self.injections.fail_next_stop = false;
            return false;
        }
        if let Some(data) = self.events.get_mut(&event) {
            data.state &= !EVENT_STATE_PLAYING;
        }
        true
    }

    fn set_user_data(&mut self, event: EventId, sound: usize) -> bool {
        self.ops.push(Op::SetUserData { event, sound });
        if self.injections.fail_next_set_user_data {
            self.injections.fail_next_set_user_data = false;
            return false;
        }
        if let Some(data) = self.events.get_mut(&event) {
            data.user_data = Some(sound);
        }
        true
    }

    fn clear_user_data(&mut self, event: EventId) {
        self.ops.push(Op::ClearUserData { event });
        if let Some(data) = self.events.get_mut(&event) {
            data.user_data = None;
        }
    }

    fn set_callback(&mut self, event: EventId) -> bool {
        self.ops.push(Op::SetCallback { event });
        if self.injections.fail_next_set_callback {
            self.injections.fail_next_set_callback = false;
            return false;
        }
        if let Some(data) = self.events.get_mut(&event) {
            data.callback_set = true;
        }
        true
    }

    fn clear_callback(&mut self, event: EventId) {
        self.ops.push(Op::ClearCallback { event });
        if let Some(data) = self.events.get_mut(&event) {
            data.callback_set = false;
        }
    }

    fn set_mute(&mut self, event: EventId, value: bool) -> bool {
        self.ops.push(Op::SetMute { event, value });
        if let Some(data) = self.events.get_mut(&event) {
            data.muted = value;
        }
        true
    }

    fn set_paused(&mut self, event: EventId, value: bool) -> bool {
        self.ops.push(Op::SetPaused { event, value });
        if let Some(data) = self.events.get_mut(&event) {
            data.paused = value;
        }
        true
    }

    fn set_pitch(&mut self, event: EventId, value: f32) -> bool {
        self.ops.push(Op::SetPitch { event, value });
        if let Some(data) = self.events.get_mut(&event) {
            data.pitch = value;
        }
        true
    }

    fn set_volume(&mut self, event: EventId, value: f32) -> bool {
        self.ops.push(Op::SetVolume { event, value });
        if let Some(data) = self.events.get_mut(&event) {
            data.volume = value;
        }
        true
    }

    fn set_3d_attributes(
        &mut self,
        event: EventId,
        position: Option<Vector3>,
        velocity: Option<Vector3>,
        direction: Option<Vector3>,
    ) -> Result<(), FmodError> {
        self.ops.push(Op::Set3DAttributes {
            event,
            position,
            velocity,
            direction,
        });
        if self.injections.invalid_handle_on_set_3d {
            self.injections.invalid_handle_on_set_3d = false;
            return Err(FmodError::InvalidHandle);
        }
        if let Some(data) = self.events.get_mut(&event) {
            if let Some(position) = position {
                data.position = position;
            }
            data.velocity = velocity;
            data.direction = direction;
        }
        Ok(())
    }

    fn get_state(&self, event: EventId) -> Result<u32, FmodError> {
        if self.injections.get_state_invalid {
            return Err(FmodError::InvalidHandle);
        }
        Ok(self.events.get(&event).map(|data| data.state).unwrap_or(0))
    }

    /// The user-data slot of an event (the owning sound id).
    fn user_data_of(&self, event: EventId) -> Option<usize> {
        self.events.get(&event).and_then(|data| data.user_data)
    }

    fn event_is_info_only(&self, event: EventId) -> bool {
        self.events
            .get(&event)
            .map(|data| data.state & EVENT_STATE_INFOONLY != 0)
            .unwrap_or(false)
    }

    /// Removes a destroyed sound's registrations (the `~PySound` port).
    fn destroy_sound(&mut self, id: usize, event: Option<EventId>, info_only: bool) {
        if let Some(old) = event
            && !info_only
        {
            self.clear_user_data(old);
            self.clear_callback(old);
        }
        self.sounds.remove(&id);
        self.jfiq.retain(|(sound_id, _)| *sound_id != id);
    }
}

struct Parsed {
    project: String,
    group: Option<String>,
    event: Option<EventId>,
}

/// The `PySound` port: a handle to a sound event with play/stop, the
/// muted/paused/pitch/volume controls, 3D positioning, the callback map,
/// the stolen/virtual status machine and the JFIQ recovery tick.
pub struct Sound {
    system: Weak<RefCell<SoundSystem>>,
    id: usize,
    event: Option<EventId>,
    group: Option<GroupKey>,
    index: i32,
    path: String,
    jfiq: bool,
    status: Status,
    unloaded: bool,
    model: Option<Rc<RefCell<Model>>>,
    position: Option<Vector3>,
    last_position: Vector3,
    callbacks: BTreeMap<CallbackType, String>,
}

impl Sound {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn is_playing(&self) -> bool {
        self.status == Status::Playing
    }

    pub fn is_stolen(&self) -> bool {
        self.status == Status::Stolen
    }

    pub fn is_stopped(&self) -> bool {
        self.status == Status::Stopped
    }

    /// `isVirtual`: stolen *and* JFIQ (`py_sound.cpp:429`).
    pub fn is_virtual(&self) -> bool {
        self.status == Status::Stolen && self.jfiq
    }

    /// `isValid`: a JFIQ sound is always considered valid; anything else
    /// must not be stolen (`py_sound.cpp:952`).
    pub fn is_valid(&self) -> bool {
        if self.jfiq { true } else { !self.is_stolen() }
    }

    /// The absolute event path stored at construction (used by by-path
    /// handle re-acquisition when unloading is allowed).
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn index(&self) -> i32 {
        self.index
    }

    pub fn is_jfiq(&self) -> bool {
        self.jfiq
    }

    /// The current event handle, if any.
    pub fn event(&self) -> Option<EventId> {
        self.event
    }

    pub fn unloaded(&self) -> bool {
        self.unloaded
    }

    pub fn set_unloaded(&mut self, value: bool) {
        self.unloaded = value;
    }

    pub fn set_model(&mut self, model: Rc<RefCell<Model>>) {
        self.model = Some(model);
    }

    /// Registers a callback for an event callback type
    /// (`PySound::setCallback`): re-registering replaces the entry.
    pub fn set_callback(&mut self, kind: CallbackType, name: &str) {
        self.callbacks.insert(kind, name.to_string());
    }

    /// `PySound::play`: JFIQ sounds just flip back to stolen (so the
    /// manager tick can restart them) and seed their last position from
    /// an attached model; everything else refreshes and starts.
    pub fn play(&mut self) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if self.jfiq {
            self.status = Status::Stolen;
            if let Some(model) = &self.model {
                self.last_position = model.borrow().position;
            }
            return true;
        }
        if !self.refresh_with(&mut sys, EVENT_STATE_READY) {
            return false;
        }
        let event = self.event.expect("refresh guarantees a handle");
        if sys.start_event(event) {
            self.status = Status::Playing;
            true
        } else {
            false
        }
    }

    /// `PySound::stop`: stopping twice is a no-op; a stolen sound only
    /// flips its status (its handle is already gone); a null handle is an
    /// error.
    pub fn stop(&mut self) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if self.is_stopped() {
            return true;
        }
        self.status = Status::Stopped;
        if self.is_stolen() {
            return true;
        }
        let Some(event) = self.event else {
            sys.log.push("PySound::stop: pEvent_ is NULL.".to_string());
            return false;
        };
        sys.stop_event(event)
    }

    /// `PySound::fini`: drop the handle, clearing user data and callback
    /// unless it is an info-only event (whose callback is global).
    pub fn fini(&mut self) {
        let Some(system) = self.system.upgrade() else {
            return;
        };
        let mut sys = system.borrow_mut();
        if let Some(old) = self.event.take()
            && !sys.event_is_info_only(old)
        {
            sys.clear_user_data(old);
            sys.clear_callback(old);
        }
    }

    /// `PySound::muted` getter: stolen sounds read false.
    pub fn muted(&self) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let sys = system.borrow();
        if self.is_stolen() {
            return false;
        }
        match self.event {
            Some(event) => sys.events[&event].muted,
            None => false,
        }
    }

    pub fn set_muted(&mut self, value: bool) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if self.is_stolen() {
            let name = self.name_with(&mut sys);
            sys.log
                .push(format!("PySound {name} is stolen, cannot mute"));
            return false;
        }
        let Some(event) = self.event else {
            sys.log.push("PySound::muted: pEvent_ is NULL.".to_string());
            return false;
        };
        sys.set_mute(event, value)
    }

    /// `PySound::paused` getter: stolen sounds read false.
    pub fn paused(&self) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let sys = system.borrow();
        if self.is_stolen() {
            return false;
        }
        match self.event {
            Some(event) => sys.events[&event].paused,
            None => false,
        }
    }

    pub fn set_paused(&mut self, value: bool) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if self.is_stolen() {
            let name = self.name_with(&mut sys);
            sys.log
                .push(format!("PySound {name} is stolen, cannot pause"));
            return false;
        }
        let Some(event) = self.event else {
            sys.log
                .push("PySound::paused: pEvent_ is NULL.".to_string());
            return false;
        };
        sys.set_paused(event, value)
    }

    /// `PySound::pitch` getter: stolen sounds read 0 (not -1).
    pub fn pitch(&self) -> f32 {
        let Some(system) = self.system.upgrade() else {
            return -1.0;
        };
        let sys = system.borrow();
        if self.is_stolen() {
            return 0.0;
        }
        match self.event {
            Some(event) => sys.events[&event].pitch,
            None => -1.0,
        }
    }

    pub fn set_pitch(&mut self, value: f32) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if self.is_stolen() {
            // The reference error text says "virtual" here even though the
            // check is the stolen one — preserved verbatim.
            let name = self.name_with(&mut sys);
            sys.log
                .push(format!("PySound {name} is virtual, Cannot set pitch"));
            return false;
        }
        let Some(event) = self.event else {
            sys.log.push("PySound::pitch: pEvent_ is NULL.".to_string());
            return false;
        };
        sys.set_pitch(event, value)
    }

    /// `PySound::volume` getter: no handle or stolen reads -1.
    pub fn volume(&self) -> f32 {
        let Some(system) = self.system.upgrade() else {
            return -1.0;
        };
        let sys = system.borrow();
        if self.event.is_none() || self.is_stolen() {
            drop(sys);
            if let Some(system) = self.system.upgrade() {
                let mut sys = system.borrow_mut();
                sys.log
                    .push("PySound::volume: pEvent_ is NULL.".to_string());
            }
            return -1.0;
        }
        sys.events[self.event.as_ref().unwrap()].volume
    }

    pub fn set_volume(&mut self, value: f32) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if self.event.is_none() || self.is_stolen() {
            let name = self.name_with(&mut sys);
            sys.log
                .push(format!("PySound {name} is stolen, Cannot set volume"));
            return false;
        }
        sys.set_volume(self.event.expect("checked above"), value)
    }

    /// `PySound::duration`: length in seconds; -1 without a handle or
    /// when stolen.
    pub fn duration(&self) -> f32 {
        let Some(system) = self.system.upgrade() else {
            return -1.0;
        };
        let sys = system.borrow();
        if self.event.is_none() || self.is_stolen() {
            drop(sys);
            if let Some(system) = self.system.upgrade() {
                let mut sys = system.borrow_mut();
                sys.log
                    .push("PySound::duration: pEvent_ is NULL.".to_string());
            }
            return -1.0;
        }
        sys.events[self.event.as_ref().unwrap()].lengthms / 1000.0
    }

    /// `PySound::name`: the event name, or "<error>" without a handle or
    /// when stolen.
    pub fn name(&self) -> String {
        let Some(system) = self.system.upgrade() else {
            return "<error>".to_string();
        };
        let mut sys = system.borrow_mut();
        self.name_with(&mut sys)
    }

    fn name_with(&self, sys: &mut SoundSystem) -> String {
        if self.event.is_none() || self.is_stolen() {
            sys.log.push("PySound::name: pEvent_ is NULL.".to_string());
            return "<error>".to_string();
        }
        sys.events[self.event.as_ref().unwrap()].name.clone()
    }

    /// `PySound::state`: the `FMOD_EVENT_STATE_*` flag names followed by
    /// the port-side virtual/stolen and stopped/invalid markers, joined
    /// with single spaces.
    pub fn state(&self) -> String {
        let Some(system) = self.system.upgrade() else {
            return "NULL".to_string();
        };
        let sys = system.borrow();
        let Some(event) = self.event else {
            // The reference log text says "PySound::volume" here — a
            // copy-paste artifact preserved verbatim.
            drop(sys);
            if let Some(system) = self.system.upgrade() {
                let mut sys = system.borrow_mut();
                sys.log
                    .push("PySound::volume: pEvent_ is NULL.".to_string());
            }
            return "NULL".to_string();
        };
        let flags = sys.get_state(event).unwrap_or(0);
        let state_error = sys.injections.get_state_invalid;
        let mut ret = String::new();
        if flags & EVENT_STATE_READY != 0 {
            ret += "ready ";
        }
        if flags & EVENT_STATE_LOADING != 0 {
            ret += "loading ";
        }
        if flags & EVENT_STATE_ERROR != 0 {
            ret += "error ";
        }
        if flags & EVENT_STATE_PLAYING != 0 {
            ret += "playing ";
        }
        if flags & EVENT_STATE_CHANNELSACTIVE != 0 {
            ret += "channelsactive ";
        }
        if flags & EVENT_STATE_INFOONLY != 0 {
            ret += "infoonly ";
        }

        if self.is_virtual() {
            ret += "virtual ";
        } else if self.is_stolen() {
            ret += "stolen ";
        }
        if self.is_stopped() {
            ret += "stopped ";
        } else if state_error {
            ret += "invalid ";
        }

        if !ret.is_empty() {
            ret.truncate(ret.len() - 1);
        }
        ret
    }

    /// `PySound::isInfoOnly`.
    pub fn is_info_only(&self) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        let Some(event) = self.event else {
            sys.log
                .push("PySound::isInfoOnly: pEvent_ is NULL.".to_string());
            return false;
        };
        sys.events[&event].state & EVENT_STATE_INFOONLY != 0
    }

    /// `PySound::pyGet_position`: the model position when attached, else
    /// the free position; None (with an error logged) when neither exists.
    pub fn position(&self) -> Option<Vector3> {
        if let Some(model) = &self.model {
            return Some(model.borrow().position);
        }
        match self.position {
            Some(position) => Some(position),
            None => {
                if let Some(system) = self.system.upgrade() {
                    let mut sys = system.borrow_mut();
                    let name = self.name_with(&mut sys);
                    sys.log
                        .push(format!("PySound '{name}' has no 3D position set"));
                }
                None
            }
        }
    }

    /// `PySound::pySet_position`: refuses for attached sounds; on update
    /// failure the stored position is dropped.
    pub fn set_position(&mut self, position: Vector3) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if let Some(model) = &self.model {
            let name = self.name_with(&mut sys);
            let model_name = model.borrow().name.clone();
            sys.log.push(format!(
                "Can't set 3D position for PySound '{name}' (it is already attached to {model_name})"
            ));
            return false;
        }
        if self.update_with(&mut sys, position, Vector3::ZERO, 0.0) {
            self.position = Some(position);
            true
        } else {
            self.position = None;
            false
        }
    }

    /// `PySound::refresh`: cheap breakout when the state matches the mask
    /// and the user data is still ours; JFIQ sounds report stolen;
    /// otherwise the handle is released and re-acquired (by absolute path
    /// when unloading is allowed, else by group + index) and rebound.
    pub fn refresh(&mut self, ok_mask: u32) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        self.refresh_with(&mut sys, ok_mask)
    }

    fn refresh_with(&mut self, sys: &mut SoundSystem, ok_mask: u32) -> bool {
        let Some(event) = self.event else {
            sys.log
                .push("PySound::refresh: pEvent_ is NULL.".to_string());
            return false;
        };
        let state = sys.get_state(event).unwrap_or(0);

        // Cheap breakout if the event is already in an acceptable state
        // and its user data has not been reset by anyone.
        if state & ok_mask != 0 && sys.user_data_of(event) == Some(self.id) {
            if self.is_playing()
                && let Some(model) = self.model.clone()
            {
                return self.model_update_sound(sys, &model);
            }
            return true;
        }

        if self.jfiq {
            // FMOD_EVENT_CALLBACKTYPE_STOLEN should be hit before this.
            self.status = Status::Stolen;
            return false;
        }

        // Release the old sound event.
        if let Some(old) = self.event.take()
            && !sys.event_is_info_only(old)
        {
            sys.clear_user_data(old);
            sys.clear_callback(old);
        }

        // Get a new event handle.
        let acquired = if sys.allow_unload() {
            sys.parse_path(&self.path, true, true, false)
                .and_then(|parsed| parsed.event)
        } else {
            self.group
                .as_ref()
                .and_then(|key| sys.get_event_by_index(&key.project, &key.group, self.index))
        };
        let Some(event) = acquired else {
            sys.log
                .push("PySound::refresh: Couldn't re-acquire Event handle".to_string());
            return false;
        };
        self.event = Some(event);

        if !sys.set_user_data(event, self.id) {
            return false;
        }
        if !sys.set_callback(event) {
            return false;
        }

        if let Some(model) = self.model.clone()
            && !self.model_update_sound(sys, &model)
        {
            return false;
        }

        // Set the 3D position for the new event if necessary.
        if let Some(position) = self.position
            && !self.update_with(sys, position, Vector3::ZERO, 0.0)
        {
            return false;
        }

        self.status = Status::Playing;
        true
    }

    /// `PyModel::updateSound` observable effect: the model position goes
    /// to the event's 3D attributes; the model decides success.
    fn model_update_sound(&mut self, sys: &mut SoundSystem, model: &Rc<RefCell<Model>>) -> bool {
        let model = model.borrow();
        if model.update_sound_ok
            && let Some(event) = self.event
        {
            let _ = sys.set_3d_attributes(event, Some(model.position), None, None);
        }
        model.update_sound_ok
    }

    /// `PySound::update(position, orientation, deltaTime)`: virtual (JFIQ
    /// stolen) sounds only track their last position; stopped sounds stay
    /// valid without touching FMOD; everything else pushes position,
    /// velocity and normalised direction to the event. An invalid handle
    /// triggers refresh-and-restart before the attributes are set again.
    pub fn update(&mut self, position: Vector3, orientation: Vector3, delta_time: f32) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        self.update_with(&mut sys, position, orientation, delta_time)
    }

    fn update_with(
        &mut self,
        sys: &mut SoundSystem,
        position: Vector3,
        orientation: Vector3,
        delta_time: f32,
    ) -> bool {
        if self.is_virtual() {
            // Skip for the JFIQ events.
            self.last_position = position;
            return true;
        }
        // The reference's stolen check after this point is unreachable
        // with ENABLE_JFIQ_HANDLING=1: STATUS_STOLEN is only entered by
        // JFIQ sounds, which are caught by is_virtual above.
        if self.is_stopped() {
            // Still valid; it might be played again later.
            return true;
        }
        let Some(event) = self.event else {
            sys.log
                .push("PySound::update: pEvent_ is NULL.".to_string());
            return false;
        };

        let mut velocity = None;
        if delta_time != 0.0 {
            velocity =
                Some(self.calculate_velocity_with(sys.max_sound_speed(), position, delta_time));
        }

        // After velocity calculation, set the last position.
        self.last_position = position;

        let mut direction = None;
        if orientation != Vector3::ZERO {
            let mut normalised = orientation;
            normalised.normalise();
            direction = Some(normalised);
        }

        let mut result = sys.set_3d_attributes(event, Some(position), velocity, direction);

        // Check if the handle has been invalidated by a group unload.
        if result == Err(FmodError::InvalidHandle) {
            if !self.refresh_with(sys, EVENT_STATE_READY) {
                return self.jfiq;
            }
            let event = self.event.expect("refresh guarantees a handle");
            if !self.is_stopped() {
                sys.start_event(event);
            }
            result = sys.set_3d_attributes(event, Some(position), velocity, direction);
        }

        result.is_ok()
    }

    /// `PySound::calculateVelocity`: zero on the first sample or for
    /// non-positive deltas, (delta / time) otherwise, zeroed when it
    /// exceeds the manager's max sound speed (a teleport).
    pub fn calculate_velocity(&self, position: Vector3, delta_time: f32) -> Vector3 {
        let Some(system) = self.system.upgrade() else {
            return Vector3::ZERO;
        };
        let sys = system.borrow();
        self.calculate_velocity_with(sys.max_sound_speed(), position, delta_time)
    }

    fn calculate_velocity_with(
        &self,
        max_speed: f32,
        position: Vector3,
        delta_time: f32,
    ) -> Vector3 {
        let mut velocity = Vector3::ZERO;
        if delta_time != 0.0 && self.last_position != Vector3::ZERO && delta_time > 0.0 {
            velocity = (position - self.last_position) / delta_time;
        }

        if max_speed > 0.0 && velocity.length_squared() > max_speed * max_speed {
            // Assume the sound source has teleported.
            velocity = Vector3::ZERO;
        }

        velocity
    }

    /// `PySound::updateJFIQ`: virtual sounds poll for a real event
    /// handle — the info event is moved to the latest position (so it can
    /// steal quieter instances), the old handle is released, and a fresh
    /// default handle is started. `FMOD_ERR_EVENT_FAILED` (this instance
    /// is the quietest) is a *success* outcome: keep polling.
    pub fn update_jfiq(&mut self) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        self.update_jfiq_with(&mut sys)
    }

    fn update_jfiq_with(&mut self, sys: &mut SoundSystem) -> bool {
        if !self.is_virtual() {
            return true;
        }

        // Created sounds always carry their resolution (project, group).
        let key = self
            .group
            .clone()
            .expect("created sounds always carry a group");

        let Some(info_event) =
            sys.get_event_by_index_impl(&key.project, &key.group, self.index, true)
        else {
            return false;
        };

        // Set the newest position so the info event can steal quieter ones.
        if sys
            .set_3d_attributes(info_event, Some(self.last_position), None, None)
            .is_err()
        {
            return false;
        }

        if let Some(old) = self.event.take()
            && !sys.event_is_info_only(old)
        {
            sys.clear_user_data(old);
            sys.clear_callback(old);
        }

        let Some(event) = sys.get_event_by_index(&key.project, &key.group, self.index) else {
            if sys.last_acquire_error == Some(FmodError::EventFailed) {
                // Event 'just failed' because it is the quietest.
                return true;
            }
            return false;
        };
        self.event = Some(event);

        if !sys.set_user_data(event, self.id) {
            return false;
        }
        if !sys.set_callback(event) {
            return false;
        }
        if !sys.start_event(event) {
            return false;
        }

        self.status = Status::Playing;
        true
    }

    /// The FMOD event callback state machine (`py_sound.cpp:1130`),
    /// invoked by the manager's `fire_callback`. Returns the sound id when
    /// a script callback is registered for this type (so the manager logs
    /// the dispatch), None otherwise.
    pub fn handle_event_callback(&mut self, kind: CallbackType) -> Option<usize> {
        match kind {
            // With JFIQ handling enabled the stolen assignment only
            // happens for JFIQ sounds that are not stopped; play() will
            // reset everything for a stopped one.
            CallbackType::Stolen => {
                if self.jfiq && self.status != Status::Stopped {
                    self.status = Status::Stolen;
                }
            }
            // When just stolen these are invoked as well; in that case
            // the sound stays stolen.
            CallbackType::SoundDefEnd | CallbackType::EventFinished
                if self.status != Status::Stolen =>
            {
                self.status = Status::Stopped;
            }
            _ => {}
        }
        if self.callbacks.contains_key(&kind) {
            Some(self.id)
        } else {
            None
        }
    }
}

impl Drop for Sound {
    /// The `~PySound` port: release the event's user data and callback
    /// (unless info-only), and leave the manager's sound and JFIQ lists.
    fn drop(&mut self) {
        let Some(system) = self.system.upgrade() else {
            return;
        };
        let Ok(mut sys) = system.try_borrow_mut() else {
            return;
        };
        let info_only = self
            .event
            .map(|event| sys.event_is_info_only(event))
            .unwrap_or(false);
        sys.destroy_sound(self.id, self.event, info_only);
    }
}

/// `PySoundParameter::REFRESH_MASK`: READY | PLAYING
/// (`py_sound_parameter.hpp:56`).
pub const PARAM_REFRESH_MASK: u32 = EVENT_STATE_READY | EVENT_STATE_PLAYING;

/// A sound event parameter (`PySoundParameter`): value/velocity/seekSpeed
/// with min/max range enforcement and key-off.
pub struct SoundParameter {
    system: Weak<RefCell<SoundSystem>>,
    sound: Rc<RefCell<Sound>>,
    name: String,
    minimum: f32,
    maximum: f32,
    value: f32,
    velocity: f32,
    seek_speed: f32,
}

impl SoundParameter {
    /// `PySound::param`: refreshes the sound (READY | PLAYING mask), then
    /// resolves the parameter by name on its event. Missing names fail
    /// like the reference's ValueError.
    pub fn get(
        system: &Rc<RefCell<SoundSystem>>,
        sound: &Rc<RefCell<Sound>>,
        name: &str,
    ) -> Result<Option<SoundParameter>, PyFail> {
        let mut sys = system.borrow_mut();
        if !sound
            .borrow_mut()
            .refresh_with(&mut sys, PARAM_REFRESH_MASK)
        {
            return Err(PyFail {
                error: format!("FMOD::Event::getParameter() failed: {name}"),
            });
        }
        let bound = sound.borrow();
        // Created sounds always carry their resolution (project, group).
        let key = bound
            .group
            .clone()
            .expect("created sounds always carry a group");
        let param = sys
            .projects
            .get(&key.project)
            .and_then(|defs| defs.iter().find(|g| g.name == key.group))
            .and_then(|group| group.events.iter().find(|e| e.index == bound.index))
            .and_then(|event| event.params.iter().find(|def| def.name == name))
            .cloned();
        drop(bound);
        let Some(param) = param else {
            return Err(PyFail {
                error: format!("FMOD::Event::getParameter() failed: {name}"),
            });
        };
        Ok(Some(SoundParameter {
            system: Rc::downgrade(system),
            sound: Rc::clone(sound),
            name: name.to_string(),
            minimum: param.min,
            maximum: param.max,
            value: 0.0,
            velocity: 0.0,
            seek_speed: 0.0,
        }))
    }

    /// `PySoundParameter::refresh`: the sound must refresh under the
    /// READY | PLAYING mask before any parameter operation.
    fn refresh(&mut self, sys: &mut SoundSystem) -> bool {
        self.sound
            .borrow_mut()
            .refresh_with(sys, PARAM_REFRESH_MASK)
    }

    /// Current value; -1 when the refresh fails.
    pub fn value(&mut self) -> f32 {
        let Some(system) = self.system.upgrade() else {
            return -1.0;
        };
        let mut sys = system.borrow_mut();
        if !self.refresh(&mut sys) {
            return -1.0;
        }
        self.value
    }

    /// Out-of-range values are refused (ERROR_MSG, no FMOD call).
    pub fn set_value(&mut self, value: f32) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if !self.refresh(&mut sys) {
            return false;
        }
        if value < self.minimum || value > self.maximum {
            sys.log.push(format!(
                "PySoundParameter::value( {} ): Value {value} is outside valid range [{},{}]",
                self.name, self.minimum, self.maximum
            ));
            return false;
        }
        let event = self
            .sound
            .borrow()
            .event
            .expect("refresh guarantees a handle");
        self.value = value;
        sys.ops.push(Op::SetParamValue {
            event,
            name: self.name.clone(),
            value,
        });
        true
    }

    /// Current velocity; -1 when the refresh fails.
    pub fn velocity(&mut self) -> f32 {
        let Some(system) = self.system.upgrade() else {
            return -1.0;
        };
        let mut sys = system.borrow_mut();
        if !self.refresh(&mut sys) {
            return -1.0;
        }
        self.velocity
    }

    pub fn set_velocity(&mut self, velocity: f32) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if !self.refresh(&mut sys) {
            return false;
        }
        let event = self
            .sound
            .borrow()
            .event
            .expect("refresh guarantees a handle");
        self.velocity = velocity;
        sys.ops.push(Op::SetParamVelocity {
            event,
            name: self.name.clone(),
            value: velocity,
        });
        true
    }

    /// Current seek speed; -1 when the refresh fails.
    pub fn seek_speed(&mut self) -> f32 {
        let Some(system) = self.system.upgrade() else {
            return -1.0;
        };
        let mut sys = system.borrow_mut();
        if !self.refresh(&mut sys) {
            return -1.0;
        }
        self.seek_speed
    }

    pub fn set_seek_speed(&mut self, seek_speed: f32) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if !self.refresh(&mut sys) {
            return false;
        }
        let event = self
            .sound
            .borrow()
            .event
            .expect("refresh guarantees a handle");
        self.seek_speed = seek_speed;
        sys.ops.push(Op::SetParamSeekSpeed {
            event,
            name: self.name.clone(),
            value: seek_speed,
        });
        true
    }

    /// `PySoundParameter::keyOff`: sustain-point release.
    pub fn key_off(&mut self) -> bool {
        let Some(system) = self.system.upgrade() else {
            return false;
        };
        let mut sys = system.borrow_mut();
        if !self.refresh(&mut sys) {
            return false;
        }
        let event = self
            .sound
            .borrow()
            .event
            .expect("refresh guarantees a handle");
        sys.ops.push(Op::KeyOffParam {
            event,
            name: self.name.clone(),
        });
        true
    }

    pub fn minimum(&self) -> f32 {
        self.minimum
    }

    pub fn maximum(&self) -> f32 {
        self.maximum
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL_PATH: &str = "/proj/guns/shoot";
    const JFIQ_PATH: &str = "/proj/guns/reload";
    const PARAM_PATH: &str = "/proj/music/theme";

    fn proj() -> Vec<GroupDef> {
        vec![
            GroupDef::new(
                "guns",
                vec![
                    EventDef::new("shoot", 0).lengthms(2500.0),
                    EventDef::new("reload", 1).jfiq(),
                ],
            ),
            GroupDef::new(
                "music",
                vec![EventDef::new("theme", 0).param("speed", 0.0, 1.0)],
            ),
        ]
    }

    fn make_system() -> Rc<RefCell<SoundSystem>> {
        let system = SoundSystem::new();
        {
            let mut sys = system.borrow_mut();
            sys.add_project_to_disk("proj", proj());
            sys.initialise(None);
            sys.load_event_project("proj");
            sys.ops.clear();
        }
        system
    }

    fn normal_sound(system: &Rc<RefCell<SoundSystem>>) -> Rc<RefCell<Sound>> {
        SoundSystem::create_sound(system, NORMAL_PATH)
            .expect("exception level would fail")
            .expect("warning level returns none")
    }

    fn jfiq_sound(system: &Rc<RefCell<SoundSystem>>) -> Rc<RefCell<Sound>> {
        SoundSystem::create_sound(system, JFIQ_PATH)
            .expect("exception level would fail")
            .expect("warning level returns none")
    }

    fn ops(system: &Rc<RefCell<SoundSystem>>) -> Vec<Op> {
        system.borrow().ops.clone()
    }

    /// `unwrap_err` without a `Debug` bound on the success type.
    fn expect_fail<T>(result: Result<T, PyFail>) -> PyFail {
        match result {
            Err(err) => err,
            Ok(_) => panic!("expected a PyFail"),
        }
    }

    fn logs(system: &Rc<RefCell<SoundSystem>>) -> Vec<String> {
        system.borrow().log.clone()
    }

    #[test]
    fn create_requires_initialisation() {
        let system = SoundSystem::new();
        system.borrow_mut().add_project_to_disk("proj", proj());

        // Default warning level: the error prints and the call yields None.
        let created = SoundSystem::create_sound(&system, NORMAL_PATH);
        assert!(created.expect("warning yields ok").is_none());
        assert!(logs(&system)[0].contains("Sound system is not initialised."));

        // Exception level propagates.
        system.borrow_mut().set_error_level(ErrorLevel::Exception);
        let err = expect_fail(SoundSystem::create_sound(&system, NORMAL_PATH));
        assert!(err.error.contains("not initialised"));
    }

    #[test]
    fn create_non_jfiq_op_sequence_and_defaults() {
        let system = make_system();
        let sound = normal_sound(&system);

        // The first event-path parse resolves the group, then the
        // INFOONLY singleton, then a fresh default instance, then the
        // constructor's user data + callback installation.
        assert_eq!(
            ops(&system),
            vec![
                Op::GetGroup {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                },
                Op::GetEvent {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                    name: "shoot".to_string(),
                    event: Some(0),
                },
                Op::GetEvent {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                    name: "shoot".to_string(),
                    event: Some(1),
                },
                Op::SetUserData { event: 1, sound: 0 },
                Op::SetCallback { event: 1 },
            ]
        );
        let sound = sound.borrow();
        assert_eq!(sound.status(), Status::Playing);
        assert!(sound.is_playing());
        assert!(!sound.is_stolen());
        assert!(!sound.is_stopped());
        assert!(!sound.is_virtual());
        assert!(sound.is_valid());
        assert!(!sound.is_jfiq());
        assert_eq!(sound.event(), Some(1));
        assert_eq!(sound.path(), NORMAL_PATH);
        assert_eq!(sound.index(), 0);
        assert!(!sound.unloaded());
        assert_eq!(sound.id(), 0);
        assert_eq!(sound.name(), "shoot");
        // The PySound reports Playing, but the fresh FMOD instance has not
        // been started yet: only the READY flag is set.
        assert_eq!(sound.state(), "ready");
        assert_eq!(system.borrow().event_state(1), Some(EVENT_STATE_READY));
        assert_eq!(
            system.borrow().event_state(0),
            Some(EVENT_STATE_READY | EVENT_STATE_INFOONLY)
        );
        assert!(!sound.is_info_only());
        assert!(system.borrow().event_has_callback(1));
    }

    #[test]
    fn create_jfiq_binds_info_singleton_and_lists() {
        let system = make_system();
        let sound = jfiq_sound(&system);

        // The JFIQ sound keeps the INFOONLY handle: no default acquisition
        // and no constructor user data / callback.
        assert_eq!(
            ops(&system),
            vec![
                Op::GetGroup {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                },
                Op::GetEvent {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                    name: "reload".to_string(),
                    event: Some(0),
                }
            ]
        );
        let sound = sound.borrow();
        assert_eq!(sound.status(), Status::Stolen);
        assert!(sound.is_stolen());
        assert!(sound.is_virtual());
        assert!(sound.is_valid(), "a JFIQ sound is always valid");
        assert!(sound.is_jfiq());
        assert!(sound.is_info_only());
        assert_eq!(sound.event(), Some(0));
        assert_eq!(sound.state(), "ready infoonly virtual");
        assert_eq!(system.borrow().jfiq_len(), 1);
        assert_eq!(system.borrow().jfiq_ids(), vec![0]);
    }

    #[test]
    fn create_unknown_event_error_levels() {
        let system = make_system();
        let missing = "/nope/guns/shoot";

        // Exception level propagates the KeyError text.
        system.borrow_mut().set_error_level(ErrorLevel::Exception);
        let err = expect_fail(SoundSystem::create_sound(&system, missing));
        assert_eq!(
            err.error,
            "Could not load sound event '/nope/guns/shoot'. See logs for details."
        );

        // Silent level swallows without printing.
        system.borrow_mut().set_error_level(ErrorLevel::Silent);
        assert!(
            SoundSystem::create_sound(&system, missing)
                .unwrap()
                .is_none()
        );
        assert!(!logs(&system).iter().any(|l| l.contains("pyError printed")));

        // Warning level prints.
        system.borrow_mut().set_error_level(ErrorLevel::Warning);
        assert!(
            SoundSystem::create_sound(&system, missing)
                .unwrap()
                .is_none()
        );
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("pyError printed: Could not load sound event"))
        );
    }

    #[test]
    fn create_logs_missing_project_with_media_path() {
        let system = SoundSystem::new();
        {
            let mut sys = system.borrow_mut();
            let config = SoundConfig {
                media_path: Some("sound/".to_string()),
                ..SoundConfig::default()
            };
            sys.initialise(Some(&config));
            sys.set_error_level(ErrorLevel::Exception);
        }
        let err = expect_fail(SoundSystem::create_sound(&system, "/nope/guns/shoot"));
        assert!(err.error.contains("Could not load sound event"));
        assert!(
            logs(&system)
                .contains(&"SoundManager::parsePath: Failed to load 'sound/nope.fev'".to_string())
        );
    }

    #[test]
    fn initialise_applies_config_keys() {
        let system = SoundSystem::new();
        let config = SoundConfig {
            error_level: Some("exception".to_string()),
            allow_unload: Some(false),
            max_sound_speed: Some(55.0),
            media_path: Some("snd".to_string()),
        };
        assert!(system.borrow_mut().initialise(Some(&config)));
        let sys = system.borrow();
        assert!(sys.is_initialised());
        assert_eq!(sys.error_level(), ErrorLevel::Exception);
        assert!(!sys.allow_unload());
        assert_eq!(sys.max_sound_speed(), 55.0);
    }

    #[test]
    fn initialise_unknown_error_level_falls_back() {
        let system = SoundSystem::new();
        let config = SoundConfig {
            error_level: Some("bogus".to_string()),
            ..SoundConfig::default()
        };
        assert!(system.borrow_mut().initialise(Some(&config)));
        assert_eq!(system.borrow().error_level(), ErrorLevel::Warning);
        assert!(logs(&system)[0].contains("Unrecognised value for soundMgr/errorLevel: bogus"));
    }

    #[test]
    fn set_path_rejects_empty() {
        let system = SoundSystem::new();
        assert!(!system.borrow_mut().set_path("snd"));
        assert!(system.borrow_mut().initialise(None));
        assert!(system.borrow_mut().set_path("snd"));
        assert!(!system.borrow_mut().set_path(""));
        assert!(logs(&system)[0].contains("SoundManager::setPath: Called with an empty path"));
    }

    #[test]
    fn play_start_op_and_refresh_breakout() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        // refresh() breaks out cheaply (state READY, user data ours) so
        // play() only appends the Start op.
        assert!(sound.borrow_mut().play());
        assert_eq!(ops(&system).last(), Some(&Op::Start { event }));

        // Playing again refreshes (cheap breakout) and starts again.
        assert!(sound.borrow_mut().play());
        assert_eq!(ops(&system).last(), Some(&Op::Start { event }));
    }

    #[test]
    fn play_start_failure() {
        let system = make_system();
        let sound = normal_sound(&system);
        system.borrow_mut().injections.fail_next_start = true;
        assert!(!sound.borrow_mut().play());
    }

    #[test]
    fn manager_play_and_play_at() {
        let system = make_system();

        let event = system.borrow_mut().play(NORMAL_PATH);
        assert_eq!(event, Some(0));
        assert_eq!(
            ops(&system),
            vec![
                Op::GetGroup {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                },
                Op::GetEvent {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                    name: "shoot".to_string(),
                    event: Some(0),
                },
                Op::Start { event: 0 },
            ]
        );

        system.borrow_mut().injections.invalid_handle_on_set_3d = true;
        let event = system
            .borrow_mut()
            .play_at(NORMAL_PATH, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(event, Some(1));
        // The 3D failure is only logged; the start still happens.
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("Failed to set 3D attributes"))
        );
        assert_eq!(ops(&system).last(), Some(&Op::Start { event: 1 }));
    }

    #[test]
    fn manager_play_failure_logs() {
        let system = make_system();
        system.borrow_mut().injections.fail_next_start = true;
        assert_eq!(system.borrow_mut().play(NORMAL_PATH), None);
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("SoundManager::play: Failed to play '/proj/guns/shoot'"))
        );
    }

    #[test]
    fn stop_sequence_idempotent() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        assert!(sound.borrow_mut().stop());
        assert_eq!(ops(&system).last(), Some(&Op::Stop { event }));
        assert!(sound.borrow().is_stopped());
        assert!(sound.borrow().is_valid());

        // Stopping twice is a no-op.
        let count = ops(&system).len();
        assert!(sound.borrow_mut().stop());
        assert_eq!(ops(&system).len(), count);
    }

    #[test]
    fn stop_stolen_still_stops_the_handle() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        let event = sound.borrow().event().unwrap();

        // The reference sets STATUS_STOPPED *before* checking isStolen(),
        // so that check is dead: a stolen sound still stops its handle.
        assert!(sound.borrow_mut().stop());
        assert_eq!(ops(&system).last(), Some(&Op::Stop { event }));
        assert!(sound.borrow().is_stopped());
        assert!(sound.borrow().is_valid());
    }

    #[test]
    fn stop_after_fini_errors() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().fini();
        assert!(!sound.borrow_mut().stop());
        assert!(logs(&system).contains(&"PySound::stop: pEvent_ is NULL.".to_string()));
    }

    #[test]
    fn stolen_callback_jfiq_recovers_to_stolen() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        sound
            .borrow_mut()
            .set_callback(CallbackType::Stolen, "on_stolen");
        assert!(sound.borrow_mut().update_jfiq());
        let event = sound.borrow().event().unwrap();
        assert_eq!(sound.borrow().status(), Status::Playing);

        system
            .borrow_mut()
            .fire_callback(event, CallbackType::Stolen);
        assert_eq!(sound.borrow().status(), Status::Stolen);
        assert_eq!(
            system.borrow().fired_callbacks,
            vec![(0, CallbackType::Stolen)]
        );
    }

    #[test]
    fn stolen_callback_on_stopped_jfiq_stays_stopped() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        sound
            .borrow_mut()
            .set_callback(CallbackType::Stolen, "on_stolen");
        assert!(sound.borrow_mut().update_jfiq());
        let event = sound.borrow().event().unwrap();
        sound.borrow_mut().stop();

        system
            .borrow_mut()
            .fire_callback(event, CallbackType::Stolen);
        assert!(sound.borrow().is_stopped());
        // The callback still fires for registered listeners.
        assert_eq!(
            system.borrow().fired_callbacks,
            vec![(0, CallbackType::Stolen)]
        );
    }

    #[test]
    fn stolen_callback_non_jfiq_ignored_status() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound
            .borrow_mut()
            .set_callback(CallbackType::Stolen, "on_stolen");
        let event = sound.borrow().event().unwrap();

        // py_sound.cpp:1150 only assigns STATUS_STOLEN inside the JFIQ
        // branch, so a non-JFIQ sound keeps playing while the callback
        // still fires.
        system
            .borrow_mut()
            .fire_callback(event, CallbackType::Stolen);
        assert_eq!(sound.borrow().status(), Status::Playing);
        assert_eq!(
            system.borrow().fired_callbacks,
            vec![(0, CallbackType::Stolen)]
        );
    }

    #[test]
    fn event_finished_stops_playing() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        system
            .borrow_mut()
            .fire_callback(event, CallbackType::SoundDefEnd);
        assert!(sound.borrow().is_stopped());

        sound.borrow_mut().play();
        system
            .borrow_mut()
            .fire_callback(event, CallbackType::EventFinished);
        assert!(sound.borrow().is_stopped());
    }

    #[test]
    fn event_finished_while_stolen_stays() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        sound
            .borrow_mut()
            .set_callback(CallbackType::Stolen, "on_stolen");
        assert!(sound.borrow_mut().update_jfiq());
        let event = sound.borrow().event().unwrap();
        system
            .borrow_mut()
            .fire_callback(event, CallbackType::Stolen);
        assert!(sound.borrow().is_stolen());

        // While stolen, end-of-event callbacks leave the status alone.
        system
            .borrow_mut()
            .fire_callback(event, CallbackType::EventFinished);
        assert!(sound.borrow().is_stolen());
    }

    #[test]
    fn unregistered_callbacks_not_fired() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        for kind in [
            CallbackType::SyncPoint,
            CallbackType::NetModified,
            CallbackType::SoundDefCreate,
            CallbackType::SoundDefRelease,
            CallbackType::SoundDefInfo,
            CallbackType::EventStarted,
            CallbackType::SoundDefSelectIndex,
            CallbackType::Occlusion,
        ] {
            system.borrow_mut().fire_callback(event, kind);
        }
        assert!(system.borrow().fired_callbacks.is_empty());

        sound
            .borrow_mut()
            .set_callback(CallbackType::EventStarted, "on_started");
        system
            .borrow_mut()
            .fire_callback(event, CallbackType::EventStarted);
        assert_eq!(
            system.borrow().fired_callbacks,
            vec![(0, CallbackType::EventStarted)]
        );

        // Re-registering replaces the entry, it does not duplicate.
        sound
            .borrow_mut()
            .set_callback(CallbackType::EventStarted, "on_started2");
        system
            .borrow_mut()
            .fire_callback(event, CallbackType::EventStarted);
        assert_eq!(system.borrow().fired_callbacks.len(), 2);
    }

    #[test]
    fn callbacks_without_user_data_ignored() {
        let system = make_system();

        // A manager-level play leaves the event's user data unset.
        let event = system.borrow_mut().play(NORMAL_PATH).unwrap();
        sound_callback_helper(&system, event);
        assert!(system.borrow().fired_callbacks.is_empty());
    }

    fn sound_callback_helper(system: &Rc<RefCell<SoundSystem>>, event: EventId) {
        system
            .borrow_mut()
            .fire_callback(event, CallbackType::EventFinished);
    }

    #[test]
    fn stolen_refusals() {
        let system = make_system();
        let sound = jfiq_sound(&system);

        assert!(!sound.borrow_mut().set_muted(true));
        assert!(!sound.borrow_mut().set_paused(true));
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("is stolen, cannot mute"))
        );
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("is stolen, cannot pause"))
        );

        // Getters read false while stolen.
        assert!(!sound.borrow().muted());
        assert!(!sound.borrow().paused());
    }

    #[test]
    fn pitch_sentinels_and_setter() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        assert_eq!(sound.borrow().pitch(), 1.0);
        assert!(sound.borrow_mut().set_pitch(2.0));
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetPitch { event, value: 2.0 })
        );
        assert_eq!(sound.borrow().pitch(), 2.0);

        // Stolen sounds read 0, not -1.
        let stolen = jfiq_sound(&system);
        assert_eq!(stolen.borrow().pitch(), 0.0);
    }

    #[test]
    fn volume_duration_sentinels() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        assert_eq!(sound.borrow().duration(), 2.5);
        assert!(sound.borrow_mut().set_volume(0.5));
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetVolume { event, value: 0.5 })
        );
        assert_eq!(sound.borrow().volume(), 0.5);

        // Stolen sounds read -1 for both.
        let stolen = jfiq_sound(&system);
        assert_eq!(stolen.borrow().volume(), -1.0);
        assert_eq!(stolen.borrow().duration(), -1.0);
    }

    #[test]
    fn name_and_state_sentinels() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().fini();

        assert_eq!(sound.borrow().name(), "<error>");
        assert_eq!(sound.borrow().state(), "NULL");
        // The reference state() NULL case logs the copy-pasted volume text.
        assert!(logs(&system).contains(&"PySound::volume: pEvent_ is NULL.".to_string()));
    }

    #[test]
    fn state_flag_strings() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        system.borrow_mut().set_event_state(
            event,
            EVENT_STATE_READY
                | EVENT_STATE_LOADING
                | EVENT_STATE_ERROR
                | EVENT_STATE_PLAYING
                | EVENT_STATE_CHANNELSACTIVE
                | EVENT_STATE_INFOONLY,
        );
        assert_eq!(
            sound.borrow().state(),
            "ready loading error playing channelsactive infoonly"
        );
    }

    #[test]
    fn state_virtual_vs_stopped_vs_invalid() {
        let system = make_system();

        // virtual before stolen: the JFIQ sound is stolen and virtual.
        let jfiq = jfiq_sound(&system);
        assert_eq!(jfiq.borrow().state(), "ready infoonly virtual");
        jfiq.borrow_mut().stop();
        // Stopped is no longer stolen, so neither virtual nor stolen shows.
        assert_eq!(jfiq.borrow().state(), "ready infoonly stopped");

        // stopped before invalid: with the state read failing the flags
        // are unavailable, so only the port-side marker shows.
        let sound = normal_sound(&system);
        sound.borrow_mut().stop();
        system.borrow_mut().injections.get_state_invalid = true;
        assert_eq!(sound.borrow().state(), "stopped");

        // invalid alone: the state read fails while still playing.
        let sound = normal_sound(&system);
        system.borrow_mut().injections.get_state_invalid = true;
        assert_eq!(sound.borrow().state(), "invalid");
    }

    #[test]
    fn position_free_attached_and_refusal() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        assert!(sound.borrow().position().is_none());
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("has no 3D position set"))
        );

        let pos = Vector3::new(1.0, 2.0, 3.0);
        assert!(sound.borrow_mut().set_position(pos));
        assert_eq!(
            ops(&system).last(),
            Some(&Op::Set3DAttributes {
                event,
                position: Some(pos),
                velocity: None,
                direction: None,
            })
        );
        assert_eq!(sound.borrow().position(), Some(pos));

        // Attached sounds refuse a manual position.
        let model = Rc::new(RefCell::new(Model::new("player")));
        model.borrow_mut().position = Vector3::new(9.0, 0.0, 0.0);
        sound.borrow_mut().set_model(model.clone());
        assert!(!sound.borrow_mut().set_position(Vector3::new(5.0, 0.0, 0.0)));
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("it is already attached to player"))
        );
        assert_eq!(sound.borrow().position(), Some(Vector3::new(9.0, 0.0, 0.0)));
    }

    #[test]
    fn update_3d_attributes_velocity_direction() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        // First sample: velocity is zero (last position still ZERO).
        assert!(
            sound
                .borrow_mut()
                .update(Vector3::new(1.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        assert_eq!(
            ops(&system).last(),
            Some(&Op::Set3DAttributes {
                event,
                position: Some(Vector3::new(1.0, 0.0, 0.0)),
                velocity: Some(Vector3::ZERO),
                direction: None,
            })
        );

        // Second sample: velocity is the position delta over time.
        assert!(
            sound
                .borrow_mut()
                .update(Vector3::new(3.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        assert_eq!(
            ops(&system).last(),
            Some(&Op::Set3DAttributes {
                event,
                position: Some(Vector3::new(3.0, 0.0, 0.0)),
                velocity: Some(Vector3::new(2.0, 0.0, 0.0)),
                direction: None,
            })
        );

        // Orientation is normalised; zero delta leaves the velocity unset.
        assert!(
            sound
                .borrow_mut()
                .update(Vector3::ZERO, Vector3::new(0.0, 3.0, 4.0), 0.0)
        );
        assert_eq!(
            ops(&system).last(),
            Some(&Op::Set3DAttributes {
                event,
                position: Some(Vector3::ZERO),
                velocity: None,
                direction: Some(Vector3::new(0.0, 0.6, 0.8)),
            })
        );
    }

    #[test]
    fn update_velocity_accumulates_and_teleport_clamps() {
        let system = make_system();
        let sound = normal_sound(&system);

        // A 1000 unit jump in one second exceeds the default max speed of
        // 100: the velocity clamps to zero (teleport).
        assert!(
            sound
                .borrow_mut()
                .update(Vector3::new(1000.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        assert_eq!(
            ops(&system).last(),
            Some(&Op::Set3DAttributes {
                event: sound.borrow().event().unwrap(),
                position: Some(Vector3::new(1000.0, 0.0, 0.0)),
                velocity: Some(Vector3::ZERO),
                direction: None,
            })
        );

        // calculate_velocity mirrors the same clamp from outside.
        assert_eq!(
            sound
                .borrow()
                .calculate_velocity(Vector3::new(2000.0, 0.0, 0.0), 1.0),
            Vector3::ZERO
        );
        assert_eq!(
            sound
                .borrow()
                .calculate_velocity(Vector3::new(1010.0, 0.0, 0.0), 1.0),
            Vector3::new(10.0, 0.0, 0.0)
        );
        assert_eq!(
            sound
                .borrow()
                .calculate_velocity(Vector3::new(1.0, 0.0, 0.0), 0.0),
            Vector3::ZERO
        );
    }

    #[test]
    fn update_virtual_tracks_position_only() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        let count = ops(&system).len();

        // A virtual (JFIQ stolen) sound only tracks its last position.
        assert!(
            sound
                .borrow_mut()
                .update(Vector3::new(5.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        assert_eq!(ops(&system).len(), count);
        assert_eq!(
            sound
                .borrow()
                .calculate_velocity(Vector3::new(9.0, 0.0, 0.0), 1.0),
            Vector3::new(4.0, 0.0, 0.0),
            "the virtual update seeded last_position"
        );
    }

    #[test]
    fn update_stopped_is_valid_no_ops() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().stop();
        let count = ops(&system).len();

        assert!(
            sound
                .borrow_mut()
                .update(Vector3::new(1.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        assert_eq!(ops(&system).len(), count);
    }

    #[test]
    fn update_invalid_handle_refreshes_and_restarts() {
        let system = make_system();
        let sound = normal_sound(&system);
        let old_event = sound.borrow().event().unwrap();
        sound.borrow_mut().set_position(Vector3::new(1.0, 0.0, 0.0));

        let count = ops(&system).len();
        system.borrow_mut().injections.invalid_handle_on_set_3d = true;
        system.borrow_mut().injections.get_state_invalid = true;

        assert!(
            sound
                .borrow_mut()
                .update(Vector3::new(2.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        let new_event = sound.borrow().event().unwrap();
        assert_ne!(new_event, old_event);
        assert_eq!(sound.borrow().status(), Status::Playing);
        assert_eq!(
            &ops(&system)[count..],
            &[
                Op::Set3DAttributes {
                    event: old_event,
                    position: Some(Vector3::new(2.0, 0.0, 0.0)),
                    velocity: Some(Vector3::new(1.0, 0.0, 0.0)),
                    direction: None,
                },
                Op::ClearUserData { event: old_event },
                Op::ClearCallback { event: old_event },
                Op::GetEvent {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                    name: "shoot".to_string(),
                    event: Some(new_event),
                },
                Op::SetUserData {
                    event: new_event,
                    sound: 0,
                },
                Op::SetCallback { event: new_event },
                Op::Set3DAttributes {
                    event: new_event,
                    position: Some(Vector3::new(1.0, 0.0, 0.0)),
                    velocity: None,
                    direction: None,
                },
                Op::Start { event: new_event },
                Op::Set3DAttributes {
                    event: new_event,
                    position: Some(Vector3::new(2.0, 0.0, 0.0)),
                    velocity: Some(Vector3::new(1.0, 0.0, 0.0)),
                    direction: None,
                },
            ]
        );
        // The old handle's registrations are gone.
        assert!(!system.borrow().event_has_callback(old_event));

        system.borrow_mut().injections.get_state_invalid = false;
        assert_eq!(sound.borrow().state(), "ready playing");
    }

    #[test]
    fn update_jfiq_recovery_sequence() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        let count = ops(&system).len();

        assert!(sound.borrow_mut().update_jfiq());
        let event = sound.borrow().event().unwrap();
        assert_eq!(sound.borrow().status(), Status::Playing);
        assert_eq!(
            &ops(&system)[count..],
            &[
                Op::GetEventByIndex {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                    index: 1,
                    info_only: true,
                    event: Some(0),
                },
                Op::Set3DAttributes {
                    event: 0,
                    position: Some(Vector3::ZERO),
                    velocity: None,
                    direction: None,
                },
                Op::GetEventByIndex {
                    project: "proj".to_string(),
                    group: "guns".to_string(),
                    index: 1,
                    info_only: false,
                    event: Some(event),
                },
                Op::SetUserData { event, sound: 0 },
                Op::SetCallback { event },
                Op::Start { event },
            ]
        );
    }

    #[test]
    fn update_jfiq_just_failed_is_success() {
        let system = make_system();
        let sound = jfiq_sound(&system);

        system.borrow_mut().injections.fail_event_failed_once = true;
        // FMOD_ERR_EVENT_FAILED ("just failed because quietest") is not a
        // failure for the tick: keep polling.
        assert!(sound.borrow_mut().update_jfiq());
        assert!(sound.borrow().is_stolen());
        assert!(!ops(&system).iter().any(|op| matches!(op, Op::Start { .. })));
    }

    #[test]
    fn update_jfiq_non_virtual_noop() {
        let system = make_system();
        let sound = normal_sound(&system);
        let count = ops(&system).len();

        assert!(sound.borrow_mut().update_jfiq());
        assert_eq!(ops(&system).len(), count);
    }

    #[test]
    fn system_update_drives_jfiq_list() {
        let system = make_system();
        let a = jfiq_sound(&system);
        let b = jfiq_sound(&system);
        assert_ne!(a.borrow().id(), b.borrow().id());
        assert_eq!(system.borrow().jfiq_len(), 2);

        assert!(system.borrow_mut().update(0.016));
        assert!(a.borrow().is_playing());
        assert!(b.borrow().is_playing());
        // The list itself survives the tick; entries early-out once playing.
        assert_eq!(system.borrow().jfiq_len(), 2);
        assert!(system.borrow_mut().update(0.016));
    }

    #[test]
    fn listener_position_sequence_and_clamp() {
        let system = make_system();

        // First call: velocity starts at zero.
        assert!(system.borrow_mut().set_listener_position(
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::ZERO,
            Vector3::ZERO,
            1.0
        ));
        assert_eq!(
            system.borrow().listener_position(),
            (Vector3::new(1.0, 0.0, 0.0), Vector3::ZERO)
        );
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetListener {
                position: Vector3::new(1.0, 0.0, 0.0),
                velocity: Vector3::ZERO,
                forward: Vector3::ZERO,
                up: Vector3::ZERO,
            })
        );

        // Second call: velocity is the delta over the frame time.
        system.borrow_mut().set_listener_position(
            Vector3::new(11.0, 0.0, 0.0),
            Vector3::ZERO,
            Vector3::ZERO,
            2.0,
        );
        assert_eq!(
            system.borrow().listener_position(),
            (Vector3::new(11.0, 0.0, 0.0), Vector3::new(5.0, 0.0, 0.0))
        );

        // A jump beyond maxSoundSpeed is a teleport: velocity zeroes.
        system.borrow_mut().set_listener_position(
            Vector3::new(1011.0, 0.0, 0.0),
            Vector3::ZERO,
            Vector3::ZERO,
            1.0,
        );
        assert_eq!(
            system.borrow().listener_position(),
            (Vector3::new(1011.0, 0.0, 0.0), Vector3::ZERO)
        );
    }

    #[test]
    fn listener_requires_initialisation() {
        let system = SoundSystem::new();
        assert!(!system.borrow_mut().set_listener_position(
            Vector3::ZERO,
            Vector3::ZERO,
            Vector3::ZERO,
            1.0
        ));
    }

    #[test]
    fn master_volume_paths() {
        let system = SoundSystem::new();
        assert!(!system.borrow_mut().set_master_volume(0.5));
        assert!(logs(&system)[0].contains("No sound subsystem, can't set master volume"));

        system.borrow_mut().initialise(None);
        assert!(system.borrow_mut().set_master_volume(0.5));
        assert_eq!(
            ops(&system),
            vec![Op::CategoryVolume {
                name: "master".to_string(),
                value: 0.5,
            }]
        );

        system.borrow_mut().injections.master_category_missing = true;
        assert!(!system.borrow_mut().set_master_volume(0.5));
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("Couldn't get master EventCategory"))
        );
    }

    #[test]
    fn db_to_linear_literal() {
        let system = make_system();

        // The literal 1/(db/-3): -6 dB reads 0.5.
        assert_eq!(system.borrow_mut().db_to_linear_level(-6.0), 0.5);
        // Exactly 0 dB divides by negative zero: negative infinity.
        assert_eq!(
            system.borrow_mut().db_to_linear_level(0.0),
            f32::NEG_INFINITY
        );
        // Positive levels are capped at 0 dB (with a warning).
        assert_eq!(
            system.borrow_mut().db_to_linear_level(3.0),
            f32::NEG_INFINITY
        );
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("Level > 0dB passed in (3) - capping to 0dB"))
        );
    }

    #[test]
    fn abs_path_variants() {
        let system = make_system();

        assert_eq!(
            system.borrow_mut().abs_path("/proj/guns/shoot").as_deref(),
            Some("/proj/guns/shoot")
        );
        assert_eq!(
            system.borrow_mut().abs_path("guns/shoot").as_deref(),
            Some("/proj/guns/shoot"),
            "the first loaded project is the default"
        );

        let bare = SoundSystem::new();
        bare.borrow_mut().initialise(None);
        assert!(bare.borrow_mut().abs_path("guns/shoot").is_none());
        assert!(logs(&bare)[0].contains("Can't resolve absolute path with no default project"));
    }

    #[test]
    fn parse_path_rejects_top_level_event() {
        let system = make_system();
        assert_eq!(system.borrow_mut().get_event("/proj/shoot"), None);
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("Asked for illegal top-level event '/proj/shoot'"))
        );
    }

    #[test]
    fn parse_path_requires_default_for_relative() {
        let system = SoundSystem::new();
        system.borrow_mut().add_project_to_disk("proj", proj());
        system.borrow_mut().initialise(None);
        assert_eq!(system.borrow_mut().get_event("guns/shoot"), None);
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("No project specified and no default project loaded"))
        );
    }

    #[test]
    fn group_cache_single_get_group_op() {
        let system = make_system();

        assert_eq!(system.borrow_mut().get_event(NORMAL_PATH), Some(0));
        assert_eq!(system.borrow_mut().get_event(NORMAL_PATH), Some(1));
        let group_ops = ops(&system)
            .iter()
            .filter(|op| matches!(op, Op::GetGroup { .. }))
            .count();
        assert_eq!(group_ops, 1, "the group resolves once");
    }

    #[test]
    fn get_event_missing_logs() {
        let system = make_system();
        assert_eq!(system.borrow_mut().get_event("/proj/guns/missing"), None);
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("Couldn't get event missing from group guns"))
        );
    }

    #[test]
    fn parameter_surface() {
        let system = make_system();
        let sound = {
            let event = system.borrow_mut().get_event(PARAM_PATH).unwrap();
            let sound = SoundSystem::create_sound(&system, PARAM_PATH)
                .unwrap()
                .unwrap();
            let _ = event;
            sound
        };

        let mut param = SoundParameter::get(&system, &sound, "speed")
            .unwrap()
            .unwrap();
        assert_eq!(param.name(), "speed");
        assert_eq!(param.minimum(), 0.0);
        assert_eq!(param.maximum(), 1.0);
        assert_eq!(param.value(), 0.0);

        let event = sound.borrow().event().unwrap();
        assert!(param.set_value(0.5));
        assert_eq!(param.value(), 0.5);
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetParamValue {
                event,
                name: "speed".to_string(),
                value: 0.5,
            })
        );

        // Out-of-range values are refused without an FMOD call.
        let count = ops(&system).len();
        assert!(!param.set_value(2.0));
        assert!(!param.set_value(-0.5));
        assert_eq!(ops(&system).len(), count);
        assert_eq!(param.value(), 0.5);
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("Value 2 is outside valid range [0,1]"))
        );

        assert!(param.set_velocity(0.2));
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetParamVelocity {
                event,
                name: "speed".to_string(),
                value: 0.2,
            })
        );
        assert_eq!(param.velocity(), 0.2);

        assert!(param.set_seek_speed(1.5));
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetParamSeekSpeed {
                event,
                name: "speed".to_string(),
                value: 1.5,
            })
        );
        assert_eq!(param.seek_speed(), 1.5);

        assert!(param.key_off());
        assert_eq!(
            ops(&system).last(),
            Some(&Op::KeyOffParam {
                event,
                name: "speed".to_string(),
            })
        );
    }

    #[test]
    fn parameter_get_missing_name() {
        let system = make_system();
        let sound = SoundSystem::create_sound(&system, PARAM_PATH)
            .unwrap()
            .unwrap();

        system.borrow_mut().set_error_level(ErrorLevel::Exception);
        let err = expect_fail(SoundParameter::get(&system, &sound, "nope"));
        assert_eq!(err.error, "FMOD::Event::getParameter() failed: nope");
    }

    #[test]
    fn parameter_get_fails_on_stolen() {
        let system = make_system();
        let sound = jfiq_sound(&system);

        // The READY|PLAYING refresh reports stolen for JFIQ sounds.
        system.borrow_mut().set_error_level(ErrorLevel::Exception);
        let err = expect_fail(SoundParameter::get(&system, &sound, "speed"));
        assert_eq!(err.error, "FMOD::Event::getParameter() failed: speed");
        assert!(sound.borrow().is_stolen());
    }

    #[test]
    fn parameter_after_system_drop() {
        let system = make_system();
        let sound = SoundSystem::create_sound(&system, PARAM_PATH)
            .unwrap()
            .unwrap();
        let mut param = SoundParameter::get(&system, &sound, "speed")
            .unwrap()
            .unwrap();
        drop(system);
        assert_eq!(param.value(), -1.0);
    }

    #[test]
    fn drop_clears_registrations_and_jfiq_list() {
        let system = make_system();

        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();
        let count = ops(&system).len();
        drop(sound);
        assert_eq!(
            &ops(&system)[count..],
            &[Op::ClearUserData { event }, Op::ClearCallback { event },]
        );

        let stolen = jfiq_sound(&system);
        assert_eq!(system.borrow().jfiq_len(), 1);
        let count = ops(&system).len();
        drop(stolen);
        // The info-only handle is left alone; only the list entry goes.
        assert_eq!(ops(&system).len(), count);
        assert_eq!(system.borrow().jfiq_len(), 0);
    }

    #[test]
    fn unload_event_project_all_paths() {
        let system = make_system();
        let sound = jfiq_sound(&system);

        // Unloading disabled.
        system.borrow_mut().set_allow_unload(false);
        assert!(!system.borrow_mut().unload_event_project("proj"));
        assert!(logs(&system).contains(&"Unloading soundbanks is disabled".to_string()));
        system.borrow_mut().set_allow_unload(true);

        // Not currently loaded.
        assert!(!system.borrow_mut().unload_event_project("nope"));
        assert!(logs(&system).contains(&"Soundbank 'nope' is not currently loaded!".to_string()));

        // The real unload: the JFIQ sound is marked and leaves the list.
        assert!(system.borrow_mut().unload_event_project("proj"));
        assert!(sound.borrow().unloaded());
        assert_eq!(system.borrow().jfiq_len(), 0);

        // The project is gone; reloading issues a fresh LoadProject op.
        // (make_system cleared the ledger, so this reload is the first
        // visible one.)
        let loads = |system: &Rc<RefCell<SoundSystem>>| {
            system
                .borrow()
                .ops
                .iter()
                .filter(|op| matches!(op, Op::LoadProject { .. }))
                .count()
        };
        assert_eq!(loads(&system), 0);
        assert!(system.borrow_mut().load_event_project("proj"));
        assert_eq!(loads(&system), 1);
    }

    #[test]
    fn load_and_default_project_flows() {
        let system = SoundSystem::new();
        system.borrow_mut().add_project_to_disk("proj", proj());
        system.borrow_mut().initialise(None);

        assert!(system.borrow_mut().load_event_project("proj"));
        assert!(
            system.borrow_mut().load_event_project("proj"),
            "already loaded still succeeds"
        );
        let loads = ops(&system)
            .iter()
            .filter(|op| matches!(op, Op::LoadProject { .. }))
            .count();
        assert_eq!(loads, 1);

        assert!(system.borrow_mut().set_default_project("proj"));
        assert!(!system.borrow_mut().set_default_project("nope"));
    }

    #[test]
    fn refresh_jfiq_reports_stolen() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        let count = ops(&system).len();

        assert!(!sound.borrow_mut().refresh(EVENT_STATE_READY));
        assert!(sound.borrow().is_stolen());
        assert_eq!(ops(&system).len(), count);
    }

    #[test]
    fn callback_type_name_round_trip() {
        let all = [
            (CallbackType::SyncPoint, "FMOD_EVENT_CALLBACKTYPE_SYNCPOINT"),
            (
                CallbackType::SoundDefStart,
                "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_START",
            ),
            (
                CallbackType::SoundDefEnd,
                "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_END",
            ),
            (CallbackType::Stolen, "FMOD_EVENT_CALLBACKTYPE_STOLEN"),
            (
                CallbackType::EventFinished,
                "FMOD_EVENT_CALLBACKTYPE_EVENTFINISHED",
            ),
            (
                CallbackType::NetModified,
                "FMOD_EVENT_CALLBACKTYPE_NET_MODIFIED",
            ),
            (
                CallbackType::SoundDefCreate,
                "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_CREATE",
            ),
            (
                CallbackType::SoundDefRelease,
                "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_RELEASE",
            ),
            (
                CallbackType::SoundDefInfo,
                "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_INFO",
            ),
            (
                CallbackType::EventStarted,
                "FMOD_EVENT_CALLBACKTYPE_EVENTSTARTED",
            ),
            (
                CallbackType::SoundDefSelectIndex,
                "FMOD_EVENT_CALLBACKTYPE_SOUNDDEF_SELECTINDEX",
            ),
            (CallbackType::Occlusion, "FMOD_EVENT_CALLBACKTYPE_OCCLUSION"),
        ];
        for (value, name) in all {
            assert_eq!(value.name(), name);
            assert_eq!(CallbackType::from_name(name), Some(value));
        }
        assert_eq!(CallbackType::from_name("NOT_A_CALLBACK"), None);
    }

    #[test]
    fn after_system_drop_sentinels() {
        let system = make_system();
        let sound = normal_sound(&system);
        let jfiq = jfiq_sound(&system);
        drop(system);

        assert!(!sound.borrow().muted());
        assert!(!sound.borrow().paused());
        assert_eq!(sound.borrow().pitch(), -1.0);
        assert_eq!(sound.borrow().volume(), -1.0);
        assert_eq!(sound.borrow().duration(), -1.0);
        assert_eq!(sound.borrow().name(), "<error>");
        assert_eq!(sound.borrow().state(), "NULL");
        assert!(!sound.borrow().is_info_only());
        assert_eq!(sound.borrow().position(), None);
        assert_eq!(
            sound
                .borrow()
                .calculate_velocity(Vector3::new(1.0, 0.0, 0.0), 1.0),
            Vector3::ZERO
        );
        assert!(!sound.borrow_mut().play());
        assert!(!sound.borrow_mut().stop());
        assert!(!sound.borrow_mut().set_muted(true));
        assert!(!sound.borrow_mut().set_paused(true));
        assert!(!sound.borrow_mut().set_pitch(1.0));
        assert!(!sound.borrow_mut().set_volume(1.0));
        assert!(!sound.borrow_mut().set_position(Vector3::ZERO));
        assert!(!sound.borrow_mut().refresh(EVENT_STATE_READY));
        assert!(!sound.borrow_mut().update(Vector3::ZERO, Vector3::ZERO, 0.0));
        assert!(!sound.borrow_mut().update_jfiq());
        sound.borrow_mut().fini();
        assert!(!jfiq.borrow_mut().update_jfiq());
        assert!(sound.borrow().is_valid());

        // The Drop impls survive the missing system.
        drop(sound);
        drop(jfiq);
    }

    #[test]
    fn vector3_operators() {
        let mut v = Vector3::new(3.0, 6.0, 9.0);
        v /= 3.0;
        assert_eq!(v, Vector3::new(1.0, 2.0, 3.0));
        assert_eq!(
            Vector3::new(4.0, 0.0, 0.0) - Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(3.0, 0.0, 0.0)
        );
        assert_eq!(
            Vector3::new(3.0, 0.0, 4.0) / 2.0,
            Vector3::new(1.5, 0.0, 2.0)
        );
        assert_eq!(Vector3::new(3.0, 0.0, 4.0).length_squared(), 25.0);
        let mut n = Vector3::new(0.0, 3.0, 4.0);
        n.normalise();
        assert_eq!(n, Vector3::new(0.0, 0.6, 0.8));
    }

    #[test]
    fn unloaded_accessor_round_trip() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().set_unloaded(true);
        assert!(sound.borrow().unloaded());
        sound.borrow_mut().set_unloaded(false);
        assert!(!sound.borrow().unloaded());
    }

    #[test]
    fn controls_happy_paths() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();

        assert!(sound.borrow_mut().set_muted(true));
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetMute { event, value: true })
        );
        assert!(sound.borrow().muted());
        assert!(sound.borrow_mut().set_muted(false));
        assert!(!sound.borrow().muted());

        assert!(sound.borrow_mut().set_paused(true));
        assert_eq!(
            ops(&system).last(),
            Some(&Op::SetPaused { event, value: true })
        );
        assert!(sound.borrow().paused());
        assert!(sound.borrow_mut().set_paused(false));
        assert!(!sound.borrow().paused());

        // After fini the getters fall into the NULL arms. The reference
        // muted/paused/pitch getters have no null-check log (a documented
        // defensive deviation), duration/volume/name/isInfoOnly log.
        sound.borrow_mut().fini();
        assert!(!sound.borrow().muted());
        assert!(!sound.borrow().paused());
        assert_eq!(sound.borrow().pitch(), -1.0);
        assert_eq!(sound.borrow().duration(), -1.0);
        assert_eq!(sound.borrow().volume(), -1.0);
        assert_eq!(sound.borrow().name(), "<error>");
        assert!(!sound.borrow().is_info_only());
        let log = logs(&system);
        for text in [
            "PySound::duration: pEvent_ is NULL.",
            "PySound::volume: pEvent_ is NULL.",
            "PySound::name: pEvent_ is NULL.",
            "PySound::isInfoOnly: pEvent_ is NULL.",
        ] {
            assert!(log.iter().any(|l| l == text), "missing log entry: {text}");
        }
    }

    #[test]
    fn max_sound_speed_round_trip_and_no_clamp() {
        let system = make_system();
        assert_eq!(system.borrow().max_sound_speed(), DEFAULT_MAX_SOUND_SPEED);
        system.borrow_mut().set_max_sound_speed(0.0);
        assert_eq!(system.borrow().max_sound_speed(), 0.0);

        // A max speed of 0 disables the listener teleport clamp.
        system.borrow_mut().set_listener_position(
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::ZERO,
            Vector3::ZERO,
            1.0,
        );
        system.borrow_mut().set_listener_position(
            Vector3::new(5001.0, 0.0, 0.0),
            Vector3::ZERO,
            Vector3::ZERO,
            1.0,
        );
        assert_eq!(
            system.borrow().listener_position(),
            (
                Vector3::new(5001.0, 0.0, 0.0),
                Vector3::new(5000.0, 0.0, 0.0)
            )
        );
    }

    #[test]
    fn initialise_silent_error_level() {
        let system = SoundSystem::new();
        let config = SoundConfig {
            error_level: Some("silent".to_string()),
            ..SoundConfig::default()
        };
        assert!(system.borrow_mut().initialise(Some(&config)));
        assert_eq!(system.borrow().error_level(), ErrorLevel::Silent);

        // Silent failures print nothing and still yield None: the parse
        // itself logs, but the error level adds no "pyError printed".
        let created = SoundSystem::create_sound(&system, NORMAL_PATH);
        assert!(created.expect("silent level yields ok").is_none());
        let log = logs(&system);
        assert_eq!(log.len(), 1);
        assert!(log[0].contains("SoundManager::parsePath: Failed to load 'proj.fev'"));
    }

    #[test]
    fn unload_keeps_other_projects_jfiq_entries() {
        let system = make_system();
        system.borrow_mut().add_project_to_disk(
            "other",
            vec![GroupDef::new("ui", vec![EventDef::new("beep", 0).jfiq()])],
        );
        assert!(system.borrow_mut().load_event_project("other"));
        let kept = SoundSystem::create_sound(&system, "/other/ui/beep")
            .expect("exception level would fail")
            .expect("warning level returns none");
        let gone = jfiq_sound(&system);
        assert_eq!(system.borrow().jfiq_len(), 2);

        assert!(system.borrow_mut().unload_event_project("proj"));
        assert!(gone.borrow().unloaded());
        assert!(!kept.borrow().unloaded());
        assert_eq!(system.borrow().jfiq_ids(), vec![kept.borrow().id()]);

        // The kept entry still ticks and recovers its handle.
        assert!(system.borrow_mut().update(0.016));
        assert!(kept.borrow().is_playing());
    }

    #[test]
    fn create_property_failure_is_an_error() {
        let system = make_system();
        system.borrow_mut().injections.fail_next_property = true;
        let created = SoundSystem::create_sound(&system, NORMAL_PATH);
        assert!(created.expect("warning level yields ok").is_none());
        assert!(logs(&system).contains(&"pyError printed: PySound::PySound()".to_string()));

        system.borrow_mut().set_error_level(ErrorLevel::Exception);
        system.borrow_mut().injections.fail_next_property = true;
        let err = expect_fail(SoundSystem::create_sound(&system, NORMAL_PATH));
        assert_eq!(err.error, "PySound::PySound()");
    }

    #[test]
    fn create_default_instance_failure_is_an_error() {
        let system = make_system();
        system.borrow_mut().injections.fail_default_get_event = true;
        let created = SoundSystem::create_sound(&system, NORMAL_PATH);
        assert!(created.expect("warning level yields ok").is_none());
        assert!(logs(&system).contains(
            &"pyError printed: Could not load sound event '/proj/guns/shoot'. See logs for details."
                .to_string()
        ));
        // The INFOONLY acquire succeeded, the default acquire did not.
        assert!(
            ops(&system)
                .iter()
                .any(|op| matches!(op, Op::GetEvent { event: Some(_), .. }))
        );
        assert!(
            ops(&system)
                .iter()
                .any(|op| matches!(op, Op::GetEvent { event: None, .. }))
        );
    }

    #[test]
    fn get_event_by_index_failures() {
        let system = make_system();

        // A missing index logs and reports a generic failure.
        assert!(
            system
                .borrow_mut()
                .get_event_by_index("proj", "guns", 7)
                .is_none()
        );
        assert!(
            logs(&system)
                .contains(&"SoundManager::get: Couldn't get event #7 from group".to_string())
        );
        assert_eq!(system.borrow().last_acquire_error, Some(FmodError::Generic));

        // An injected failure skips the log: the error code is set up front.
        system.borrow_mut().injections.fail_next_get_event_by_index = true;
        assert!(
            system
                .borrow_mut()
                .get_event_by_index("proj", "guns", 0)
                .is_none()
        );
        assert_eq!(system.borrow().last_acquire_error, Some(FmodError::Generic));
        assert_eq!(logs(&system).len(), 1);
    }

    #[test]
    fn listener_zero_delta_zeroes_velocity() {
        let system = make_system();
        system.borrow_mut().set_listener_position(
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::ZERO,
            Vector3::ZERO,
            1.0,
        );
        // A zero delta zeroes the velocity instead of dividing by zero.
        system.borrow_mut().set_listener_position(
            Vector3::new(3.0, 0.0, 0.0),
            Vector3::ZERO,
            Vector3::ZERO,
            0.0,
        );
        assert_eq!(
            system.borrow().listener_position(),
            (Vector3::new(3.0, 0.0, 0.0), Vector3::ZERO)
        );
    }

    #[test]
    fn parse_path_guards() {
        let system = make_system();

        // An uninitialised system resolves nothing.
        let fresh = SoundSystem::new();
        fresh.borrow_mut().add_project_to_disk("proj", proj());
        assert!(fresh.borrow_mut().get_event("/proj/guns/shoot").is_none());
        // The manager tick needs an initialised system too.
        assert!(!SoundSystem::new().borrow_mut().update(0.016));

        // The empty path is rejected up front.
        assert!(system.borrow_mut().get_event("").is_none());
        assert!(logs(&system).contains(&"SoundManager::parsePath: Invalid path ''".to_string()));

        // A relative path resolves against the default project.
        let event = system.borrow_mut().get_event("guns/shoot").unwrap();
        assert_eq!(system.borrow().event_state(event), Some(EVENT_STATE_READY));

        // A missing group: the FMOD group acquisition is attempted, then
        // the failure is logged.
        assert!(
            system
                .borrow_mut()
                .get_event("/proj/nogroup/shot")
                .is_none()
        );
        assert!(
            logs(&system).contains(
                &"SoundManager::parsePath: Couldn't get event group 'nogroup'".to_string()
            )
        );
    }

    #[test]
    fn get_event_injection_failure() {
        let system = make_system();
        system.borrow_mut().injections.fail_next_get_event = true;
        assert!(system.borrow_mut().get_event(NORMAL_PATH).is_none());
        assert!(ops(&system).contains(&Op::GetEvent {
            project: "proj".to_string(),
            group: "guns".to_string(),
            name: "shoot".to_string(),
            event: None,
        }));

        // playAt logs the 3D-attribute failure but still starts the event.
        system.borrow_mut().injections.invalid_handle_on_set_3d = true;
        let event = system
            .borrow_mut()
            .play_at(NORMAL_PATH, Vector3::new(1.0, 0.0, 0.0))
            .unwrap();
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("Failed to set 3D attributes for /proj/guns/shoot"))
        );
        assert_eq!(
            system.borrow().event_state(event),
            Some(EVENT_STATE_READY | EVENT_STATE_PLAYING)
        );
    }

    #[test]
    fn stop_injection_failure() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();
        system.borrow_mut().injections.fail_next_stop = true;

        assert!(!sound.borrow_mut().stop());
        assert_eq!(ops(&system).last(), Some(&Op::Stop { event }));
        // The port-side status flipped even though FMOD refused.
        assert!(sound.borrow().is_stopped());
    }

    #[test]
    fn refresh_reacquire_injection_failures() {
        let system = make_system();
        let sound = normal_sound(&system);
        let old = sound.borrow().event().unwrap();
        let count = ops(&system).len();
        system.borrow_mut().injections.get_state_invalid = true;
        system.borrow_mut().injections.fail_next_set_user_data = true;
        assert!(!sound.borrow_mut().play());

        // Released, re-acquired, then the user data bind failed.
        let tail = &ops(&system)[count..];
        assert_eq!(tail.len(), 4);
        assert_eq!(tail[0], Op::ClearUserData { event: old });
        assert_eq!(tail[1], Op::ClearCallback { event: old });
        assert!(matches!(&tail[2], Op::GetEvent { event: Some(_), .. }));
        assert!(matches!(&tail[3], Op::SetUserData { .. }));
        assert!(sound.borrow().event().is_some());

        // The callback bind fails the same way.
        let sound = normal_sound(&system);
        system.borrow_mut().injections.fail_next_set_callback = true;
        assert!(!sound.borrow_mut().play());
        assert!(matches!(ops(&system).last(), Some(Op::SetCallback { .. })));
    }

    #[test]
    fn play_jfiq_flips_to_stolen() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        let model = Rc::new(RefCell::new(Model::new("player")));
        model.borrow_mut().position = Vector3::new(7.0, 8.0, 9.0);
        sound.borrow_mut().set_model(model);
        let count = ops(&system).len();

        assert!(sound.borrow_mut().play());
        assert_eq!(sound.borrow().status(), Status::Stolen);
        // No FMOD traffic: the JFIQ branch returns before any call.
        assert_eq!(ops(&system).len(), count);

        // The attached model seeded the last position updateJFIQ pushes.
        assert!(sound.borrow_mut().update_jfiq());
        assert!(ops(&system).iter().any(|op| matches!(
            op,
            Op::Set3DAttributes {
                position: Some(p),
                ..
            } if *p == Vector3::new(7.0, 8.0, 9.0)
        )));
    }

    #[test]
    fn play_refresh_failure_propagates() {
        let system = make_system();
        let sound = normal_sound(&system);
        let old = sound.borrow().event().unwrap();
        system.borrow_mut().injections.get_state_invalid = true;
        system.borrow_mut().injections.fail_next_get_event = true;

        assert!(!sound.borrow_mut().play());
        assert!(
            logs(&system)
                .contains(&"PySound::refresh: Couldn't re-acquire Event handle".to_string())
        );
        // The old handle was released before the failed re-acquisition.
        assert!(sound.borrow().event().is_none());
        assert!(!system.borrow().event_has_callback(old));
    }

    #[test]
    fn refresh_by_index_when_unload_forbidden() {
        let system = make_system();
        let sound = normal_sound(&system);
        let old = sound.borrow().event().unwrap();
        system.borrow_mut().set_allow_unload(false);
        system.borrow_mut().injections.get_state_invalid = true;

        assert!(sound.borrow_mut().play());
        let new = sound.borrow().event().unwrap();
        assert_ne!(new, old);
        assert!(ops(&system).contains(&Op::GetEventByIndex {
            project: "proj".to_string(),
            group: "guns".to_string(),
            index: 0,
            info_only: false,
            event: Some(new),
        }));
        assert_eq!(ops(&system).last(), Some(&Op::Start { event: new }));
    }

    #[test]
    fn refresh_rebinds_model_and_position() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().set_position(Vector3::new(1.0, 2.0, 3.0));
        let model = Rc::new(RefCell::new(Model::new("player")));
        model.borrow_mut().position = Vector3::new(9.0, 0.0, 0.0);
        sound.borrow_mut().set_model(model);
        system.borrow_mut().injections.get_state_invalid = true;

        assert!(sound.borrow_mut().play());
        let new = sound.borrow().event().unwrap();
        assert_eq!(sound.borrow().status(), Status::Playing);
        // The model position lands first, then the stored free position.
        assert!(ops(&system).iter().any(|op| matches!(
            op,
            Op::Set3DAttributes { event, position: Some(p), .. }
                if *event == new && *p == Vector3::new(9.0, 0.0, 0.0)
        )));
        assert!(ops(&system).iter().any(|op| matches!(
            op,
            Op::Set3DAttributes { event, position: Some(p), .. }
                if *event == new && *p == Vector3::new(1.0, 2.0, 3.0)
        )));
    }

    #[test]
    fn play_breakout_updates_attached_model() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();
        let model = Rc::new(RefCell::new(Model::new("player")));
        model.borrow_mut().position = Vector3::new(4.0, 5.0, 6.0);
        sound.borrow_mut().set_model(model);
        let count = ops(&system).len();

        // The cheap breakout still runs the attached model's updateSound.
        assert!(sound.borrow_mut().play());
        assert_eq!(
            &ops(&system)[count..],
            &[
                Op::Set3DAttributes {
                    event,
                    position: Some(Vector3::new(4.0, 5.0, 6.0)),
                    velocity: None,
                    direction: None,
                },
                Op::Start { event },
            ]
        );
    }

    #[test]
    fn update_without_handle_errors() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().fini();

        assert!(
            !sound
                .borrow_mut()
                .update(Vector3::new(1.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        assert!(logs(&system).contains(&"PySound::update: pEvent_ is NULL.".to_string()));
    }

    #[test]
    fn update_invalid_handle_on_jfiq_recovers_true() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        assert!(sound.borrow_mut().update_jfiq());
        let real = sound.borrow().event().unwrap();
        let count = ops(&system).len();
        system.borrow_mut().injections.invalid_handle_on_set_3d = true;
        system.borrow_mut().injections.get_state_invalid = true;

        // The refresh sees a stolen JFIQ sound and bails; update still
        // reports the JFIQ sound as valid.
        assert!(
            sound
                .borrow_mut()
                .update(Vector3::new(2.0, 0.0, 0.0), Vector3::ZERO, 1.0)
        );
        assert_eq!(sound.borrow().status(), Status::Stolen);
        assert_eq!(sound.borrow().event(), Some(real));
        assert_eq!(
            &ops(&system)[count..],
            &[Op::Set3DAttributes {
                event: real,
                position: Some(Vector3::new(2.0, 0.0, 0.0)),
                velocity: Some(Vector3::ZERO),
                direction: None,
            }]
        );
    }

    #[test]
    fn update_jfiq_info_failures() {
        let system = make_system();

        // The info handle acquisition fails.
        let sound = jfiq_sound(&system);
        system.borrow_mut().injections.fail_next_get_event_by_index = true;
        assert!(!sound.borrow_mut().update_jfiq());
        assert_eq!(
            ops(&system).last(),
            Some(&Op::GetEventByIndex {
                project: "proj".to_string(),
                group: "guns".to_string(),
                index: 1,
                info_only: true,
                event: None,
            })
        );

        // The info-event position push fails.
        let sound = jfiq_sound(&system);
        system.borrow_mut().injections.invalid_handle_on_set_3d = true;
        assert!(!sound.borrow_mut().update_jfiq());
        assert_eq!(
            ops(&system).last(),
            Some(&Op::Set3DAttributes {
                event: 0,
                position: Some(Vector3::ZERO),
                velocity: None,
                direction: None,
            })
        );
    }

    #[test]
    fn update_jfiq_release_clears_real_handle() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        assert!(sound.borrow_mut().update_jfiq());
        let real = sound.borrow().event().unwrap();

        // Stolen again: the next tick releases the real handle.
        sound
            .borrow_mut()
            .handle_event_callback(CallbackType::Stolen);
        let count = ops(&system).len();
        assert!(sound.borrow_mut().update_jfiq());
        let tail = &ops(&system)[count..];
        assert_eq!(tail[2], Op::ClearUserData { event: real });
        assert_eq!(tail[3], Op::ClearCallback { event: real });
        assert_ne!(sound.borrow().event().unwrap(), real);
    }

    #[test]
    fn update_jfiq_bind_failures() {
        let system = make_system();
        let sound = jfiq_sound(&system);
        assert!(sound.borrow_mut().update_jfiq());
        sound
            .borrow_mut()
            .handle_event_callback(CallbackType::Stolen);

        system.borrow_mut().injections.fail_next_set_user_data = true;
        assert!(!sound.borrow_mut().update_jfiq());
        assert!(matches!(ops(&system).last(), Some(Op::SetUserData { .. })));

        sound
            .borrow_mut()
            .handle_event_callback(CallbackType::Stolen);
        system.borrow_mut().injections.fail_next_set_callback = true;
        assert!(!sound.borrow_mut().update_jfiq());
        assert!(matches!(ops(&system).last(), Some(Op::SetCallback { .. })));

        sound
            .borrow_mut()
            .handle_event_callback(CallbackType::Stolen);
        system.borrow_mut().injections.fail_next_start = true;
        assert!(!sound.borrow_mut().update_jfiq());
        assert!(matches!(ops(&system).last(), Some(Op::Start { .. })));
    }

    #[test]
    fn initialise_warning_error_level() {
        let system = SoundSystem::new();
        let config = SoundConfig {
            error_level: Some("warning".to_string()),
            ..SoundConfig::default()
        };
        assert!(system.borrow_mut().initialise(Some(&config)));
        assert_eq!(system.borrow().error_level(), ErrorLevel::Warning);
    }

    #[test]
    fn manager_play_start_failure_logs() {
        let system = make_system();
        system.borrow_mut().injections.fail_next_start = true;
        assert!(system.borrow_mut().play(NORMAL_PATH).is_none());
        assert!(
            logs(&system)
                .iter()
                .any(|l| l.contains("SoundManager::play: Failed to play '/proj/guns/shoot'"))
        );

        // playAt's start failure logs the same text.
        system.borrow_mut().injections.fail_next_start = true;
        assert!(
            system
                .borrow_mut()
                .play_at(NORMAL_PATH, Vector3::ZERO)
                .is_none()
        );
        assert_eq!(
            logs(&system)
                .iter()
                .filter(|l| l.contains("SoundManager::play: Failed to play"))
                .count(),
            2
        );
    }

    #[test]
    fn setters_after_fini_log_null() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().fini();

        assert!(!sound.borrow_mut().set_muted(true));
        assert!(!sound.borrow_mut().set_paused(true));
        assert!(!sound.borrow_mut().set_pitch(1.0));
        let log = logs(&system);
        for text in [
            "PySound::muted: pEvent_ is NULL.",
            "PySound::paused: pEvent_ is NULL.",
            "PySound::pitch: pEvent_ is NULL.",
        ] {
            assert!(log.iter().any(|l| l == text), "missing log entry: {text}");
        }
    }

    #[test]
    fn stolen_setters_and_getters() {
        let system = make_system();
        let sound = jfiq_sound(&system);

        // Stolen pitch/volume setter refusals (the pitch text says
        // "virtual" — reference copy-paste preserved).
        assert!(!sound.borrow_mut().set_pitch(1.0));
        assert!(
            logs(&system).contains(&"PySound <error> is virtual, Cannot set pitch".to_string())
        );
        assert!(!sound.borrow_mut().set_volume(0.5));
        assert!(
            logs(&system).contains(&"PySound <error> is stolen, Cannot set volume".to_string())
        );

        // The getters read their stolen sentinels.
        assert!(!sound.borrow().muted());
        assert!(!sound.borrow().paused());
        assert_eq!(sound.borrow().pitch(), 0.0);
        assert_eq!(sound.borrow().volume(), -1.0);
        assert_eq!(sound.borrow().duration(), -1.0);
        assert_eq!(sound.borrow().name(), "<error>");
    }

    #[test]
    fn set_position_failure_drops_stored_position() {
        let system = make_system();
        let sound = normal_sound(&system);
        // The position push fails with an invalid handle; the inner
        // refresh then fails to re-acquire, so the position is dropped.
        system.borrow_mut().injections.invalid_handle_on_set_3d = true;
        system.borrow_mut().injections.get_state_invalid = true;
        system.borrow_mut().injections.fail_next_get_event = true;
        assert!(!sound.borrow_mut().set_position(Vector3::new(1.0, 0.0, 0.0)));
        assert!(sound.borrow().position().is_none());
        assert!(
            logs(&system)
                .contains(&"PySound::refresh: Couldn't re-acquire Event handle".to_string())
        );
    }

    #[test]
    fn refresh_without_handle_errors() {
        let system = make_system();
        let sound = normal_sound(&system);
        sound.borrow_mut().fini();

        assert!(!sound.borrow_mut().refresh(EVENT_STATE_READY));
        assert!(logs(&system).contains(&"PySound::refresh: pEvent_ is NULL.".to_string()));
    }

    #[test]
    fn refresh_model_update_failure_propagates() {
        let system = make_system();
        let sound = normal_sound(&system);
        let model = Rc::new(RefCell::new(Model::new("player")));
        model.borrow_mut().update_sound_ok = false;
        sound.borrow_mut().set_model(model);
        system.borrow_mut().injections.get_state_invalid = true;

        // Re-acquisition succeeds but the model's updateSound refuses.
        assert!(!sound.borrow_mut().play());
        assert_eq!(sound.borrow().status(), Status::Playing);
        assert!(
            !ops(&system)
                .iter()
                .any(|op| matches!(op, Op::Set3DAttributes { .. }))
        );
    }

    #[test]
    fn refresh_position_update_failure_propagates() {
        let system = make_system();
        let sound = normal_sound(&system);
        let old = sound.borrow().event().unwrap();
        assert!(sound.borrow_mut().set_position(Vector3::new(1.0, 0.0, 0.0)));
        // Breakout disabled, the position push on the re-acquired handle
        // fails with an invalid handle, and the inner refresh inside
        // update then fails to re-acquire again.
        system.borrow_mut().injections.get_state_invalid = true;
        system.borrow_mut().injections.invalid_handle_on_set_3d = true;
        system.borrow_mut().injections.fail_next_get_event = true;

        assert!(!sound.borrow_mut().refresh(EVENT_STATE_READY));
        assert!(
            logs(&system)
                .contains(&"PySound::refresh: Couldn't re-acquire Event handle".to_string())
        );
        assert!(ops(&system).contains(&Op::Set3DAttributes {
            event: old,
            position: Some(Vector3::new(1.0, 0.0, 0.0)),
            velocity: None,
            direction: None,
        }));
        // The re-acquired handle was released by the inner refresh.
        assert!(sound.borrow().event().is_none());
    }

    #[test]
    fn drop_sound_after_system_gone_and_while_borrowed() {
        // Drop with the system already gone: the Drop guard bails. The
        // system Rc dies with this block.
        let sound = {
            let system = make_system();
            normal_sound(&system)
        };
        drop(sound);

        // Drop while the system is mutably borrowed: try_borrow_mut
        // fails and the guard bails without touching FMOD.
        let system = make_system();
        let sound = normal_sound(&system);
        let guard = system.borrow_mut();
        drop(sound);
        drop(guard);
    }

    #[test]
    fn parameter_sentinels_after_system_drop() {
        let (_weak, _sound, mut param) = {
            let system = make_system();
            let sound = {
                system.borrow_mut().get_event(PARAM_PATH).unwrap();
                SoundSystem::create_sound(&system, PARAM_PATH)
                    .unwrap()
                    .unwrap()
            };
            let param = SoundParameter::get(&system, &sound, "speed")
                .unwrap()
                .unwrap();
            (Rc::downgrade(&system), sound, param)
        };
        // The system Rc died with the block above.

        assert_eq!(param.value(), -1.0);
        assert_eq!(param.velocity(), -1.0);
        assert_eq!(param.seek_speed(), -1.0);
        assert!(!param.set_value(0.5));
        assert!(!param.set_velocity(0.5));
        assert!(!param.set_seek_speed(0.5));
        assert!(!param.key_off());
    }

    #[test]
    fn parameter_setters_fail_when_refresh_fails() {
        let system = make_system();
        let sound = {
            system.borrow_mut().get_event(PARAM_PATH).unwrap();
            SoundSystem::create_sound(&system, PARAM_PATH)
                .unwrap()
                .unwrap()
        };
        let mut param = SoundParameter::get(&system, &sound, "speed")
            .unwrap()
            .unwrap();

        // A finished sound cannot refresh, so every parameter operation
        // refuses before touching FMOD.
        sound.borrow_mut().fini();
        assert_eq!(param.value(), -1.0);
        assert_eq!(param.velocity(), -1.0);
        assert_eq!(param.seek_speed(), -1.0);
        assert!(!param.set_value(0.5));
        assert!(!param.set_velocity(0.5));
        assert!(!param.set_seek_speed(0.5));
        assert!(!param.key_off());
        assert!(!ops(&system).iter().any(|op| matches!(
            op,
            Op::SetParamValue { .. }
                | Op::SetParamVelocity { .. }
                | Op::SetParamSeekSpeed { .. }
                | Op::KeyOffParam { .. }
        )));
    }

    #[test]
    fn update_stores_velocity_and_direction_on_event() {
        let system = make_system();
        let sound = normal_sound(&system);
        let event = sound.borrow().event().unwrap();
        // The first sample only seeds last_position; the second carries
        // the real velocity and the normalised direction.
        sound
            .borrow_mut()
            .update(Vector3::new(1.0, 0.0, 0.0), Vector3::ZERO, 1.0);
        sound.borrow_mut().update(
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            1.0,
        );

        // The event data itself carries the pushed velocity/direction
        // (probes the store behind the Set3DAttributes op).
        let sys = system.borrow();
        let data = &sys.events[&event];
        assert_eq!(data.velocity, Some(Vector3::new(1.0, 0.0, 0.0)));
        assert_eq!(data.direction, Some(Vector3::new(1.0, 0.0, 0.0)));
    }
}
