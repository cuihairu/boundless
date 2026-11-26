//! The public Python export manifest: every Python module the reference
//! engine puts in front of game scripts, with its process scope and the
//! script-visible surface, plus how much of each the port delivers today.
//!
//! The acceptance is that the manifest is *checkable*, so [`check`] runs it
//! against a live interpreter: every surface marked [`Status::Ported`] must
//! actually import and resolve, and the test that drives it is what keeps the
//! table from drifting away from the code. Items land as [`Status::Pending`]
//! and flip to [`Status::Ported`] when their todo item delivers them.
//!
//! Reference structure, as surveyed:
//!
//! * Module *objects* come from `PyImport_AddModule(name)` inside init-time
//!   jobs (`lib/pyscript/script.cpp:1409` `PyModuleMethodLink::init`,
//!   `:1453` `PyModuleAttrLink::init`, `:1496` `PyModuleResultLink::init`,
//!   `lib/pyscript/py_factory_method_link.cpp:41`), which is how nearly every
//!   module is built; `ScriptModule::create` does not exist in the tree.
//!   `ScriptModule::getOrCreate` pins only `BigWorld`
//!   (`lib/pyscript/script.cpp:589`) and `__main__` (`:1302`).
//! * `PyImport_AppendInittab` has exactly one call site in the whole tree and
//!   it is a tool build (`tools/worldeditor/scripting/world_editor_script.cpp:2165`).
//! * The `pyscript` library is built once and linked by both the client and
//!   the server targets (`lib/pyscript/CMakeLists.txt:41-55` splits only
//!   `py_callback.cpp`/`py_traceback.cpp`/`script_events.cpp`), which is why
//!   scope is decided per source file by `MF_SERVER` rather than per library.
//! * `MF_SERVER` is defined for server builds
//!   (`build/cmake/BWConfiguration_server.cmake:1`), `EDITOR_ENABLED` for
//!   tools (`build/cmake/BWConfiguration_tools.cmake:1`).
//!
//! Two scoping facts this table records because they are easy to get wrong
//! and both opposite to intuition: `BigWorld.callback` is *client only*
//! (`#ifndef MF_SERVER`, `lib/pyscript/py_callback.cpp:9`, "Not available on
//! server yet, pending refactoring." at `:10`), while the listener pair
//! `addEventListener`/`removeEventListener` is *server only* (the file is in
//! the `ELSE()` branch of `lib/pyscript/CMakeLists.txt:49-54`). Project
//! modules the engine only ever imports (`Keys`, `Personality`, `BWTwoWay`,
//! `FixedDictWrappers`, `UserTypes`) have no engine source and are scoped
//! [`Scope::Supplied`]: their surface is an input to the port, not a part of
//! it.

use crate::capi;

/// Which process the reference builds a module or surface into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Built into the client and the server from the same source.
    Both,
    /// `#ifndef MF_SERVER` or a client-only build list.
    Client,
    /// `MF_SERVER` or a server-only build list.
    Server,
    /// Tools only: `EDITOR_ENABLED` targets.
    Tool,
    /// No engine source; the project resource tree supplies the module.
    Supplied,
    /// The app set whose scripts define entity classes: the client, the
    /// cell app and the bots tool each register `BigWorld.Entity` from
    /// their own source file (`client/script_bigworld.cpp:987`,
    /// `server/cellapp/cellapp.cpp:3889`,
    /// `server/tools/bots/entity_type.cpp:110`). Neither the coarse
    /// [`Scope::Client`] nor [`Scope::Server`] can say this without
    /// over-claiming: the cell app carries it while the base app does not.
    EntityApps,
    /// The base app alone: `server/baseapp/script_bigworld.cpp:2140-2143`
    /// is the only site that registers `BigWorld.Base` and
    /// `BigWorld.Proxy` (and `Service`, not yet ported).
    BaseApp,
}

impl Scope {
    /// The manifest's own spelling, so a rendered report reads like the
    /// reference's build vocabulary.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Both => "both",
            Scope::Client => "client",
            Scope::Server => "server",
            Scope::Tool => "tool",
            Scope::Supplied => "supplied",
            Scope::EntityApps => "entity-apps",
            Scope::BaseApp => "baseapp",
        }
    }
}

/// What kind of script-visible name a surface row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A module object.
    Module,
    /// A callable on its module.
    Function,
    /// A non-callable attribute.
    Attribute,
    /// A class object attached to its module.
    Class,
}

/// How much of a row the port delivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The port supplies it today; [`check`] verifies it against the live
    /// interpreter.
    Ported,
    /// No module object of the port's own, but the engine behaviour that
    /// touches the name is already delivered by another item (the `__main__`
    /// blanking, the `bw_site` import). Not checked live here.
    Covered,
    /// Recorded from the reference, nothing in the port yet. The item's todo
    /// entry flips this to [`Status::Ported`].
    Pending,
}

impl Status {
    /// The manifest's own spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ported => "ported",
            Status::Covered => "covered",
            Status::Pending => "pending",
        }
    }
}

/// One script-visible name: a function, attribute or class on a module.
pub struct Item {
    /// The Python name.
    pub name: &'static str,
    /// What kind of name.
    pub kind: Kind,
    /// Which process gets it -- narrower than its module's scope when the
    /// defining source file is gated.
    pub scope: Scope,
    /// How much the port delivers.
    pub status: Status,
    /// The reference's own argument spec where it has one, else a short
    /// prose signature.
    pub signature: &'static str,
    /// What the call returns.
    pub returns: &'static str,
    /// The exception the reference raises on bad input, quoted.
    pub error: &'static str,
    /// `path:line` in the reference tree.
    pub reference: &'static str,
}

/// One module the reference exposes, with everything the port has to say
/// about it.
pub struct Entry {
    /// The Python module name.
    pub module: &'static str,
    /// Which process builds the module object.
    pub scope: Scope,
    /// How much of the module object the port delivers.
    pub status: Status,
    /// Where the module comes into being in the reference.
    pub reference: &'static str,
    /// Anything a reader needs that the rows cannot carry: ambiguity between
    /// a doc marker and the code, a reference defect, a count of surface
    /// deliberately not row-encoded here.
    pub note: &'static str,
    /// The script-visible surface.
    pub items: &'static [Item],
}

#[allow(clippy::too_many_arguments)]
const fn item(
    name: &'static str,
    kind: Kind,
    scope: Scope,
    status: Status,
    signature: &'static str,
    returns: &'static str,
    error: &'static str,
    reference: &'static str,
) -> Item {
    Item {
        name,
        kind,
        scope,
        status,
        signature,
        returns,
        error,
        reference,
    }
}

/// `BigWorld` is the one module the engine also pins with
/// `ScriptModule::getOrCreate` (`lib/pyscript/script.cpp:589`); the server
/// re-asserts it with a `serverMode` attribute
/// (`lib/server/script_app.cpp:219,225`) and the client extends it
/// (`client/script_bigworld.cpp:974-1053`). Nearly everything else on it is
/// a `PY_MODULE_FUNCTION` in a file that is gated by process.
static BIG_WORLD_ITEMS: &[Item] = &[
    // Ported: the module shell and its one attribute.
    item(
        "component",
        Kind::Attribute,
        Scope::Both,
        Status::Ported,
        "str",
        "the role name `Script::init` was given",
        "absent when the name carries a NUL",
        "lib/pyscript/script.cpp:598-600",
    ),
    // The timer pair: client only, the gate that BW-03-03-03 has to respect.
    item(
        "callback",
        Kind::Function,
        Scope::Client,
        Status::Ported,
        "\"dO\" (float time, callable function)",
        "int callback handle",
        "TypeError \"BigWorld.callback: Argument parsing error.\"",
        "lib/pyscript/py_callback.cpp:288 (parse :262, clamp :270, \
         callable :264, counter :202-210)",
    ),
    item(
        "cancelCallback",
        Kind::Function,
        Scope::Client,
        Status::Ported,
        "\"i\" (int handle)",
        "None",
        "TypeError \"py_cancelCallback: Argument parsing error.\"",
        "lib/pyscript/py_callback.cpp:333 (parse :308, error :310, \
         skip current :319-326)",
    ),
    // URL requests.
    item(
        "fetchURL",
        Kind::Function,
        Scope::Both,
        Status::Ported,
        "\"sO|OfsO\" keywords url, callback, headers=None, timeout=0, \
         method=None, postdata=None",
        "None",
        "EnvironmentError \"PyURLRequest has not been initialised\"; \
         ValueError \"Callback is not callable\"; \
         ValueError \"headers argument is not a sequence\"; \
         ValueError \"postData argument should be a dict or string\"; \
         ValueError \"Invalid method %s. Should be 'GET', 'PUT', 'POST', \
         'DELETE' or 'PATCH'\"",
        "common/py_urlrequest.cpp:318 (parse :188-190, errors :167-168,:197,:211-212,:274-275,:307-308)",
    ),
    item(
        "URLResponse",
        Kind::Class,
        Scope::Both,
        Status::Covered,
        "built by the engine; (contentType, body, responseCode)",
        "read-only attributes contentType, body, responseCode",
        "delivered to the callback; a failing callback is printed, not raised",
        "common/py_urlrequest.cpp:26-59 (call :115-119, print :125)",
    ),
    // Listeners: server only, the other side of the callback coin. The
    // auto argument parse is `ARG( BW::string, ARG( PyObjectPtr, OPTARG(
    // int, 0, END ) ) )` for add and `ARG( BW::string, ARG( PyObjectPtr,
    // END ) )` for remove, so the "sO|i" / "sO" parse the port uses.
    item(
        "addEventListener",
        Kind::Function,
        Scope::Server,
        Status::Ported,
        "\"sO|i\" (str eventName, callable callback, int level = 0)",
        "None",
        "TypeError \"Argument is not callable\"; \
         SystemError \"ScriptEvents not initialised\"; \
         ValueError \"Invalid event type '%s'\"",
        "lib/pyscript/script_events.cpp:281-327 (parse :306-308, \
         callable :298-301, not-initialised :302-305, invalid :313-318)",
    ),
    item(
        "removeEventListener",
        Kind::Function,
        Scope::Server,
        Status::Ported,
        "\"sO\" (str eventName, callable callback)",
        "None",
        "TypeError \"Argument is not callable\"; \
         SystemError \"ScriptEvents not initialised\"; \
         ValueError \"Invalid event type '%s'\" (raised for an unknown \
         listener too -- the reference's one sentence)",
        "lib/pyscript/script_events.cpp:330-356 (parse :345-347, \
         callable :343-346, not-initialised :347-350, invalid :352-356)",
    ),
    // Watchers. The ENABLE_WATCHERS shapes are the real ones; the second
    // registration block is runtime no-ops when the watcher system is off.
    item(
        "setWatcher",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "(path, value) through the watcher tree",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/pywatcher.cpp:405 (no-op shape :2036)",
    ),
    item(
        "getWatcher",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "path",
        "the watched value",
        "TypeError from the auto argument parse",
        "lib/pyscript/pywatcher.cpp:466 (no-op shape :2042)",
    ),
    item(
        "getWatcherDir",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "path",
        "the directory listing",
        "TypeError from the auto argument parse",
        "lib/pyscript/pywatcher.cpp:539 (no-op shape :2049)",
    ),
    item(
        "addWatcher",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "(path, base object, member path, watcher type string)",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/pywatcher.cpp:1551 (no-op shape :2062)",
    ),
    item(
        "addFunctionWatcher",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "(path, function, watcher type string); no no-op shape",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/pywatcher.cpp:1635",
    ),
    item(
        "delWatcher",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "path",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/pywatcher.cpp:1658 (no-op shape :2055)",
    ),
    // Logging: the log##NAME macro instantiates five of these.
    item(
        "logDebug",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "message components",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/py_logging.cpp:91-98",
    ),
    item(
        "logInfo",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "message components",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/py_logging.cpp:91-98",
    ),
    item(
        "logWarning",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "message components",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/py_logging.cpp:91-104",
    ),
    item(
        "logError",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "message components",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/py_logging.cpp:91-104",
    ),
    item(
        "logCritical",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "message components",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/py_logging.cpp:91-106",
    ),
    item(
        "defaultLoggers",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "none",
        "the configured default logger set",
        "TypeError from the auto argument parse",
        "lib/pyscript/py_debug_message_file_logger.cpp:460",
    ),
    item(
        "__onThreadStart",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "BW::string",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/script.cpp:255",
    ),
    // Server app timers: the server's answer to the client callback pair.
    item(
        "addTimer",
        Kind::Function,
        Scope::Server,
        Status::Ported,
        "\"Of|fi:addTimer\" (callable, float initialOffset, \
         float repeatOffset = 0, int userArg = 0)",
        "int timer id",
        "TypeError \"Callback function is not callable\"; \
         ValueError \"Unable to add timer\"",
        "lib/server/app_script_timers.cpp:193 (parse :99, callable :105-122, \
         id :132)",
    ),
    item(
        "delTimer",
        Kind::Function,
        Scope::Server,
        Status::Ported,
        "\"i\" (int timerID)",
        "None",
        "TypeError from the auto argument parse; an unknown id only logs",
        "lib/server/app_script_timers.cpp:235 (parse :210, unknown :214-219)",
    ),
    item(
        "addFrequentTask",
        Kind::Function,
        Scope::Server,
        Status::Ported,
        "\"O|i:addFrequentTask\" (callable, int level = 0)",
        "True on success",
        "TypeError \"Argument is not callable\"; \
         ValueError \"Failed to register for update\"",
        "lib/server/script_frequent_tasks.cpp:142 (parse :99, errors :99-104,:120)",
    ),
    // The server's own marker: `ScriptApp::initScript` re-asserts the
    // module with the mode `ServerAppConfig::serverMode()` chose
    // (`script_app.cpp:219-225`), a read-only option defaulting to
    // "standalone" (`server_app_config.cpp:19`). The port has no config
    // source, so `create_big_world_module` writes the default every server
    // build reads.
    item(
        "serverMode",
        Kind::Attribute,
        Scope::Server,
        Status::Ported,
        "str",
        "'standalone' -- the reference's default; a config would choose \
         'center' / 'periphery'",
        "none: the reference sets it unconditionally on every server app",
        "lib/server/script_app.cpp:219-225 \
         (ServerAppConfig::serverMode, server_app_config.cpp:19,32)",
    ),
    item(
        "saveAllocationsToFile",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "BW::string",
        "None",
        "TypeError from the auto argument parse",
        "lib/server/script_app.cpp:354",
    ),
    // Network file-descriptor hooks: server only, per the build list.
    item(
        "registerFileDescriptor",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "file object",
        "int (non-zero on success)",
        "TypeError from the auto argument parse",
        "common/py_network.cpp:301",
    ),
    item(
        "deregisterFileDescriptor",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "PyObjectPtr",
        "int (non-zero on success)",
        "TypeError from the auto argument parse",
        "common/py_network.cpp:325",
    ),
    item(
        "registerWriteFileDescriptor",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "file object",
        "int (non-zero on success)",
        "TypeError from the auto argument parse",
        "common/py_network.cpp:361",
    ),
    item(
        "deregisterWriteFileDescriptor",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "PyObjectPtr",
        "int (non-zero on success)",
        "TypeError from the auto argument parse",
        "common/py_network.cpp:385",
    ),
    item(
        "address",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "none",
        "the app's network address",
        "TypeError from the auto argument parse",
        "common/py_network.cpp:414",
    ),
    item(
        "getNetworkInterfaces",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "optional BW::string",
        "tuple of interface tuples",
        "TypeError from the auto argument parse",
        "common/py_network.cpp:490",
    ),
    // Chunk and terrain access, shared where the build list says so.
    item(
        "findChunkFromPoint",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "point",
        "chunk (owned reference)",
        "TypeError from the auto argument parse",
        "common/py_chunk.cpp:106",
    ),
    item(
        "chunkTransform",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "chunk",
        "chunk (owned reference)",
        "TypeError from the auto argument parse",
        "common/py_chunk.cpp:134",
    ),
    item(
        "getHeightAtPos",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "position",
        "height (owned reference)",
        "TypeError from the auto argument parse",
        "common/py_terrain.cpp:74",
    ),
    item(
        "getMaterialKinds",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "none",
        "material kinds (owned reference)",
        "TypeError from the auto argument parse",
        "common/py_physics2.cpp:63",
    ),
    // The script class bases: each app registers them right after the module
    // comes up, and every demo script's `class X(...)` names one. Direct
    // construction stays refused -- in the reference the engine owns every
    // instance (the scripts only subclass).
    item(
        "Entity",
        Kind::Class,
        Scope::EntityApps,
        Status::Ported,
        "engine-owned; direct construction refused",
        "the entity script base type",
        "TypeError on direct construction",
        "client/script_bigworld.cpp:987 / server/cellapp/cellapp.cpp:3889 / \
         server/tools/bots/entity_type.cpp:110",
    ),
    item(
        "Base",
        Kind::Class,
        Scope::BaseApp,
        Status::Ported,
        "engine-owned; direct construction refused",
        "the base script type (Py_Header(Base, PyObjectPlus))",
        "TypeError on direct construction",
        "server/baseapp/script_bigworld.cpp:2140",
    ),
    item(
        "Proxy",
        Kind::Class,
        Scope::BaseApp,
        Status::Ported,
        "engine-owned; direct construction refused",
        "the proxy script type, subclassing Base (Py_Header(Proxy, Base))",
        "TypeError on direct construction",
        "server/baseapp/script_bigworld.cpp:2142",
    ),
];

/// `Math` is built by `PyImport_AddModule("Math")` inside `TempPyVectorInit`
/// (`lib/pyscript/script_math.cpp:3060`) and `PyFactoryMethodLink::init`
/// (`lib/pyscript/py_factory_method_link.cpp:40-41`), which also
/// `PyModule_AddObject`s each class and rewrites `tp_name` to `Math.<name>`
/// (`:64-76`). Module functions are attached the same way, so the class list
/// below is the module surface.
static MATH_ITEMS: &[Item] = &[
    item(
        "getRegister",
        Kind::Function,
        Scope::Both,
        Status::Ported,
        "uint8 register index, up to MAX_TEMP_REGISTERS-1 = 62",
        "a Vector4Provider over the register",
        "TypeError \"() argument 1 is greater than maximum value\"",
        "lib/pyscript/script_math.cpp:5051 (impl :5026, bound :4846, write :script.hpp:1153-1159)",
    ),
    item(
        "Matrix",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "|O optional MatrixProvider",
        "21 methods, 7 attributes; ValueError \"Matrix could not be inverted\"",
        "TypeError \"Math.Matrix(): Expected an optional MatrixProvider\"",
        "lib/pyscript/script_math.cpp:736 (error :787-788)",
    ),
    item(
        "Plane",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "constructor ignores args; init(p0, p1, p2)",
        "intersectRay(src, dst)",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:5273 (init :5243, ray :5255)",
    ),
    item(
        "MatrixProduct",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "read-write attributes a, b",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:894 (ctor :823)",
    ),
    item(
        "MatrixInverse",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "one MatrixProvider",
        "read-write attribute source",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:1008 (ctor :949)",
    ),
    item(
        "MatrixAnimation",
        Kind::Class,
        Scope::Client,
        Status::Pending,
        "no arguments",
        "keyframe sequence",
        "TypeError \"...must be set to a tuple of a float and a MatrixProvider\"",
        "lib/pyscript/script_math.cpp:1162 (error :1085-1086, guard _WIN32 :1040-1248)",
    ),
    item(
        "Vector2",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "floats, one float, or a tuple",
        "set, scale, dot, normalise, tuple, list, distTo, distSqrTo, \
         __getstate__, __setstate__; x, y, length, lengthSquared",
        "TypeError \"Vector.set must be set to a sequence of %d floats, or \
         one float\"; TypeError \"Vector is read-only\"; \
         IndexError \"Vector index out of range\"",
        "lib/pyscript/script_math.cpp:3044 (Vector_init :3039-3047, errors :1851-1852,:1259,:1696)",
    ),
    item(
        "Vector3",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "floats, one float, or a tuple",
        "as Vector2 plus cross2D, flatDistTo, flatDistSqrTo, setPitchYaw, \
         yaw, pitch, z",
        "as Vector2",
        "lib/pyscript/script_math.cpp:3044",
    ),
    item(
        "Vector4",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "floats, one float, or a tuple",
        "as Vector3 plus w",
        "as Vector2",
        "lib/pyscript/script_math.cpp:3044",
    ),
    item(
        "Vector4Product",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "read-write attributes a, b",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:3395",
    ),
    item(
        "Vector4Animation",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "keyframes, time, duration",
        "TypeError on a bad keyframe",
        "lib/pyscript/script_math.cpp:3630 (error :3447-3448)",
    ),
    item(
        "Vector4Morph",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "optional Vector4",
        "target, duration, time",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:3812 (ctor :3728)",
    ),
    item(
        "Vector4Translation",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "one MatrixProvider",
        "source",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:3953 (ctor :3908)",
    ),
    item(
        "Vector4Distance",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "two MatrixProviders",
        "a, b",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:4048 (ctor :3991)",
    ),
    item(
        "Vector4LFO",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "period, amplitude, phase, time, waveform (SQUARE/TRIANGLE/SINE/SAWTOOTH)",
        "ValueError or TypeError, picked by the enum setter",
        "lib/pyscript/script_math.cpp:4228 (enum :4077-4090, picker script.hpp:826-831)",
    ),
    item(
        "Vector4MatrixAdaptor",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "source, position, style (XYZ_SCALE ... LOOKAT_SCALEZ)",
        "ValueError or TypeError, picked by the enum setter",
        "lib/pyscript/script_math.cpp:4430 (enum :4324-4343)",
    ),
    item(
        "Vector4Swizzle",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "x, y, z, w",
        "TypeError from the auto argument parse",
        "lib/pyscript/script_math.cpp:4599",
    ),
    item(
        "Vector4Combiner",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "fn (MULTIPLY...), a, b",
        "ValueError or TypeError, picked by the enum setter",
        "lib/pyscript/script_math.cpp:4756 (enum :4650-4669)",
    ),
    item(
        "Vector4Shader",
        Kind::Class,
        Scope::Both,
        Status::Ported,
        "no arguments",
        "addOp(opcode, outReg, param1[, param2]), opcodes 0-12",
        "TypeError \"() argument 1 is greater than maximum value\"",
        "lib/pyscript/script_math.cpp:5014 (addOp :5002, opcodes :4975-5001)",
    ),
];

/// `ResMgr` is built lazily by whichever link fires first; its link token is
/// `ResMgr_token = PyDataSection_token | ResourceTable_token`
/// (`lib/pyscript/res_mgr_script.cpp:339`). All three defining sources are
/// unconditional in the pyscript library
/// (`lib/pyscript/CMakeLists.txt:59-74`), so the module is in both processes.
/// The editor-only trio (`remove`, `rename`, `copy`) is inside
/// `#ifdef EDITOR_ENABLED` (`res_mgr_script.cpp:166-260`) and therefore tool
/// scope even though the file is shared.
static RES_MGR_ITEMS: &[Item] = &[
    item(
        "isDir",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "one string path",
        "bool",
        "TypeError from the auto argument parse",
        "lib/pyscript/res_mgr_script.cpp:49",
    ),
    item(
        "isFile",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "one string path",
        "bool",
        "TypeError from the auto argument parse",
        "lib/pyscript/res_mgr_script.cpp:69",
    ),
    item(
        "openSection",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "string path, optional bool makeNew",
        "DataSection or None",
        "ValueError \"ResMgr.openSection(): Could not make new section '%s'\"",
        "lib/pyscript/res_mgr_script.cpp:114 (error :106-107)",
    ),
    item(
        "save",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "one string path",
        "None",
        "IOError \"Save of %s failed\"",
        "lib/pyscript/res_mgr_script.cpp:139 (error :136)",
    ),
    item(
        "purge",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "string path, optional bool",
        "None",
        "TypeError from the auto argument parse",
        "lib/pyscript/res_mgr_script.cpp:162",
    ),
    item(
        "remove",
        Kind::Function,
        Scope::Tool,
        Status::Pending,
        "one string path",
        "bool",
        "IOError \"Removing of %s failed\"",
        "lib/pyscript/res_mgr_script.cpp:191 (error :188, EDITOR_ENABLED :166-260)",
    ),
    item(
        "rename",
        Kind::Function,
        Scope::Tool,
        Status::Pending,
        "two string paths",
        "bool",
        "IOError \"Renaming of %s to %s failed\"",
        "lib/pyscript/res_mgr_script.cpp:220 (error :216-217, EDITOR_ENABLED :166-260)",
    ),
    item(
        "copy",
        Kind::Function,
        Scope::Tool,
        Status::Pending,
        "two string paths",
        "bool",
        "IOError \"Copying of %s to %s failed\"",
        "lib/pyscript/res_mgr_script.cpp:256 (error :252-253, EDITOR_ENABLED :166-260)",
    ),
    item(
        "resolveToAbsolutePath",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "one string path",
        "string",
        "TypeError from the auto argument parse",
        "lib/pyscript/res_mgr_script.cpp:289",
    ),
    item(
        "localise",
        Kind::Function,
        Scope::Both,
        Status::Pending,
        "\"s|ssssssss\" format and up to 8 substitutions",
        "string",
        "TypeError from the auto argument parse",
        "lib/pyscript/res_mgr_script.cpp:329 (parse :312-313)",
    ),
    item(
        "root",
        Kind::Attribute,
        Scope::Both,
        Status::Pending,
        "none",
        "the root DataSection",
        "none",
        "lib/pyscript/res_mgr_script.cpp:354-355",
    ),
    item(
        "DataSection",
        Kind::Class,
        Scope::Both,
        Status::Pending,
        "|s optional name",
        "mapping protocol only (len, ds[\"path\"]) plus read*/write*, \
         createSection, createSectionFromString, deleteSection, save, copy, \
         copyToZip, child, childName, cleanName, isNameClean and the as* \
         attribute family",
        "TypeError \"PyDataSection() expected an optional string argument\"; \
         EnvironmentError \"child index %d '%s' has disappeared\"; \
         IndexError \"child index out of range\"; IOError \"Save failed\"",
        "lib/pyscript/py_data_section.cpp:360 (ctor error :30-31, mapping :64-82, \
         legacy names absent: size/openSection/setProperty/getProperty and friends)",
    ),
    item(
        "ResourceTable",
        Kind::Class,
        Scope::Both,
        Status::Pending,
        "(resourceID[, updateFn])",
        "deprecated; link() binds an update callable",
        "TypeError \"ResourceTable.link() arg must be callable\"",
        "lib/pyscript/resource_table.cpp:666 (link error :425-426, ctor resource_table.hpp:33-35)",
    ),
];

/// `BWConfig` is server only: `common/py_server_lib.cpp` is in the build lists
/// of the cell, base and db servers plus the bots tool
/// (`server/cellapp/CMakeLists.txt:178`, `server/baseapp/CMakeLists.txt:126`,
/// `server/dbapp/CMakeLists.txt:53`, `server/tools/bots/CMakeLists.txt:49`)
/// and in no client list.
static BW_CONFIG_ITEMS: &[Item] = &[
    item(
        "getSections",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "section name",
        "sections",
        "TypeError from the auto argument parse",
        "common/py_server_lib.cpp:45",
    ),
    item(
        "getSection",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "section path",
        "section",
        "TypeError from the auto argument parse",
        "common/py_server_lib.cpp:69",
    ),
    item(
        "readString",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "section path",
        "string",
        "TypeError from the auto argument parse",
        "common/py_server_lib.cpp:86",
    ),
    item(
        "readInt",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "section path",
        "int",
        "TypeError from the auto argument parse",
        "common/py_server_lib.cpp:103",
    ),
    item(
        "readFloat",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "section path",
        "float",
        "TypeError from the auto argument parse",
        "common/py_server_lib.cpp:120",
    ),
    item(
        "readBool",
        Kind::Function,
        Scope::Server,
        Status::Pending,
        "section path",
        "bool",
        "TypeError from the auto argument parse",
        "common/py_server_lib.cpp:137",
    ),
];

/// The full inventory. Module rows with no `items` are name-and-scope records:
/// their surface is either out of this lane's scope (rendering and tool
/// stacks) or supplied by the project, and rows record that in `note`.
static ENTRIES: &[Entry] = &[
    Entry {
        module: "BigWorld",
        scope: Scope::Both,
        status: Status::Ported,
        reference: "lib/pyscript/script.cpp:589 (server re-assert lib/server/script_app.cpp:219,225; client extension client/script_bigworld.cpp:974-1053)",
        note: "the only module also pinned by ScriptModule::getOrCreate; \
               the client side additionally sets protocolVersion/Entity/\
               UserDataObject/digest and the physics and connection-control \
               constants (client/script_bigworld.cpp:979-1021, PyModule_AddIntConstant)",
        items: BIG_WORLD_ITEMS,
    },
    Entry {
        module: "Math",
        scope: Scope::Both,
        status: Status::Ported,
        reference: "lib/pyscript/script_math.cpp:3060 (TempPyVectorInit), lib/pyscript/py_factory_method_link.cpp:40-41",
        note: "the reference documents Math.MatrixProvider (:78), \
               Math.Vector4Provider (:3110), Math.Vector4Basic (:3206-3207) \
               and the colour aliases (:3093-3102) but exports none of them: \
               each has a PY_TYPEOBJECT and no PY_FACTORY, so no Math.X \
               attribute exists. Assigning a tuple to a Vector4Provider-typed \
               attribute is wrapped in a Vector4Basic by \
               Vector4Provider::coerce (:3266-3279) through Script::setData \
               (script.hpp:565-587), which raises TypeError \"%s must be set \
               to a Vector4Provider or None\" (script.hpp:584-585). \
               Math.Vector py_sq_slice (:1704-1770) is dead code in the \
               3.13 migration: only sq_length/sq_item/sq_ass_item are wired \
               (:2403-2408).",
        items: MATH_ITEMS,
    },
    Entry {
        module: "ResMgr",
        scope: Scope::Both,
        status: Status::Pending,
        reference: "lib/pyscript/res_mgr_script.cpp:339 (link token), lib/pyscript/CMakeLists.txt:59-74",
        note: "the task text names lib/crecell/ for resource access; that \
               directory does not exist in this checkout -- resource access \
               lives in res_mgr_script.cpp, py_data_section.cpp and \
               resource_table.cpp plus lib/resmgr/. \
               py_data_section.cpp:815 calls PyErr_Format with the args tuple \
               in place of an exception type: a reference defect to keep out \
               of any port that reads that line.",
        items: RES_MGR_ITEMS,
    },
    Entry {
        module: "BWConfig",
        scope: Scope::Server,
        status: Status::Pending,
        reference: "common/py_server_lib.cpp:11-137",
        note: "@components{ base, cell, db, bots }",
        items: BW_CONFIG_ITEMS,
    },
    Entry {
        module: "_BWp",
        scope: Scope::Both,
        status: Status::Pending,
        reference: "lib/pyscript/script.cpp:1517",
        note: "the pickling shim PyImport_AddModule'd inside \
               Script::buildReduceResult; Pickler::init imports the stdlib \
               pickle module for dumps (:132) and a restricted loads \
               (:141-145) (lib/pyscript/pickler.cpp:126)",
        items: &[],
    },
    Entry {
        module: "__main__",
        scope: Scope::Both,
        status: Status::Covered,
        reference: "lib/pyscript/script.cpp:1302 (getOrCreate), :733 (blanked at fini)",
        note: "stdlib-provided object; the engine's work on it is the fini \
               blanking, which the port delivers and \
               scenario_fini_blanks_main_module checks",
        items: &[],
    },
    Entry {
        module: "bw_site",
        scope: Scope::Both,
        status: Status::Covered,
        reference: "lib/pyscript/script.cpp:617",
        note: "imported by Script::init at its tail; the port delivers the \
               import and fails init with BwSiteImportFailed when it is \
               missing. The module's content is project-supplied",
        items: &[],
    },
    // Name-and-scope records: rendering, audio, UI and tool stacks whose
    // surface is outside this lane. Each carries the registration site so a
    // future lane can start from the line, not from a search.
    Entry {
        module: "GUI",
        scope: Scope::Both,
        status: Status::Pending,
        reference: "lib/ashes/simple_gui.cpp:1919 (+ 19 PY_FACTORY_NAMED sites)",
        note: "re-declared in the world editor tools \
               (tools/worldeditor/scripting/world_editor_script.cpp:73); \
               surface out of lane",
        items: &[],
    },
    Entry {
        module: "Pixie",
        scope: Scope::Both,
        status: Status::Pending,
        reference: "lib/particle/py_meta_particle_system.cpp:811 (+ 23 factories)",
        note: "particle/UI overlay stack; surface out of lane",
        items: &[],
    },
    Entry {
        module: "_FMOD",
        scope: Scope::Client,
        status: Status::Pending,
        reference: "lib/fmodsound/sound_manager.cpp:2235,2256,2282 (+ 4 factories)",
        note: "audio; surface out of lane",
        items: &[],
    },
    Entry {
        module: "_Scaleform",
        scope: Scope::Client,
        status: Status::Pending,
        reference: "lib/scaleform/manager.cpp:402 (+ :683 into BigWorld), lib/scaleform/util.cpp:479-497 (into BigWorld)",
        note: "the doc marker says `module Scaleform` (manager.cpp:381) while \
               the code registers `_Scaleform` (:402); the code wins. Also \
               contributes objects to BigWorld, which is a BigWorld-item \
               concern when that lane lands",
        items: &[],
    },
    Entry {
        module: "_PostProcessing",
        scope: Scope::Client,
        status: Status::Pending,
        reference: "lib/post_processing/manager.cpp:195,239,277,331,408",
        note: "the world editor doc marker says `PostProcessing` \
               (tools/worldeditor/scripting/world_editor_script.cpp:77) while \
               the code registers `_PostProcessing`; the code wins",
        items: &[],
    },
    Entry {
        module: "_AssetProcessor",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "tools/assetprocessor/asset_processor_script.cpp:194-1112",
        note: "the doc marker says `AssetProcessor` \
               (asset_processor_script.hpp:15) while the code registers \
               `_AssetProcessor` (:194); the code wins",
        items: &[],
    },
    Entry {
        module: "BWReplay",
        scope: Scope::Client,
        status: Status::Pending,
        reference: "client/entity_manager.cpp:1469 (doc marker), :1623-1983 (registrations)",
        note: "surface out of lane",
        items: &[],
    },
    Entry {
        module: "WorldEditor",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "tools/worldeditor/world/world_manager.cpp:1",
        note: "tool build only; surface out of lane",
        items: &[],
    },
    Entry {
        module: "ModelEditor",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "tools/modeleditor_core/App/me_app.cpp:1",
        note: "tool build only; surface out of lane",
        items: &[],
    },
    Entry {
        module: "ParticleEditor",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "tools/particle_editor/particle_editor.cpp:1",
        note: "tool build only; surface out of lane",
        items: &[],
    },
    Entry {
        module: "Locator",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "lib/gizmo/tool_locator.cpp:9",
        note: "tool stack; surface out of lane",
        items: &[],
    },
    Entry {
        module: "Functor",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "lib/gizmo/tool_functor.cpp:10",
        note: "tool stack; surface out of lane",
        items: &[],
    },
    Entry {
        module: "View",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "lib/gizmo/tool_view.cpp:19",
        note: "tool stack; surface out of lane",
        items: &[],
    },
    Entry {
        module: "Tool",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "lib/gizmo (tool module family)",
        note: "tool stack; surface out of lane",
        items: &[],
    },
    Entry {
        module: "OpenAutomateWrapper",
        scope: Scope::Tool,
        status: Status::Pending,
        reference: "lib/open_automate/open_automate_wrapper.cpp (6 sites + 3 factories)",
        note: "tool/client automation; surface out of lane",
        items: &[],
    },
    // Project-supplied: the engine imports these but never creates them.
    Entry {
        module: "Keys",
        scope: Scope::Supplied,
        status: Status::Pending,
        reference: "client/script_bigworld.cpp:1053 (PyRun_SimpleString \"import BigWorld, GUI, Math, Pixie, Keys\")",
        note: "no engine source; the project supplies the module",
        items: &[],
    },
    Entry {
        module: "Personality",
        scope: Scope::Supplied,
        status: Status::Pending,
        reference: "lib/pyscript/personality.cpp:65",
        note: "imported by the game-supplied name; no engine source",
        items: &[],
    },
    Entry {
        module: "BWTwoWay",
        scope: Scope::Supplied,
        status: Status::Pending,
        reference: "lib/entitydef/method_description.cpp:243",
        note: "imported by entitydef; no engine source. The BW-04 entitydef \
               lane owns the consumer side",
        items: &[],
    },
    Entry {
        module: "FixedDictWrappers",
        scope: Scope::Supplied,
        status: Status::Pending,
        reference: "lib/entitydef/unit_test/res/FixedDictWrappers.py (fixture only)",
        note: "no engine source; fixtures exist in the entitydef unit tests",
        items: &[],
    },
    Entry {
        module: "UserTypes",
        scope: Scope::Supplied,
        status: Status::Pending,
        reference: "lib/entitydef/unit_test/res/UserTypes.py (fixture only)",
        note: "no engine source; fixtures exist in the entitydef unit tests",
        items: &[],
    },
];

/// The whole manifest.
pub fn entries() -> &'static [Entry] {
    ENTRIES
}

/// The entry for one module name, if the manifest records it.
pub fn find(module: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|entry| entry.module == module)
}

/// The manifest's own spelling of a module row, for reports and failure
/// messages.
pub fn describe(entry: &Entry) -> String {
    format!(
        "{} [{}] {}",
        entry.module,
        entry.scope.as_str(),
        entry.status.as_str()
    )
}

/// Why a check failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    /// A module marked [`Status::Ported`] does not import.
    ModuleMissing {
        /// The module name.
        module: &'static str,
    },
    /// A surface row marked [`Status::Ported`] is not on its module.
    ItemMissing {
        /// The module name.
        module: &'static str,
        /// The surface name.
        item: &'static str,
    },
    /// A [`Scope::Client`] or [`Scope::Server`] row marked
    /// [`Status::Ported`] is live under a component whose build does not
    /// carry it -- the `#ifndef MF_SERVER` gate breached the other way.
    ItemScopeLeak {
        /// The module name.
        module: &'static str,
        /// The surface name.
        item: &'static str,
    },
}

impl Mismatch {
    /// The rendered complaint, naming the manifest row and the reality.
    pub fn render(self) -> String {
        match self {
            Mismatch::ModuleMissing { module } => {
                format!("{module} is marked ported but does not import")
            }
            Mismatch::ItemMissing { module, item } => {
                format!("{module}.{item} is marked ported but is not present")
            }
            Mismatch::ItemScopeLeak { module, item } => {
                format!("{module}.{item} is live under a role that does not carry it")
            }
        }
    }
}

/// Whether a build carrying `component` would link a surface of `scope`:
/// an ungated scope is always carried, the gated ones follow the build
/// split -- the server component list, the client/editor pair, and an
/// unknown name carrying neither (the port cannot tell the build, so it
/// claims neither face). This is the runtime shape of the reference's
/// compile-time gate, and [`crate::timers`] applies the same split to the
/// method tables it installs.
pub fn carries_role(scope: Scope, component: &str) -> bool {
    match scope {
        Scope::Both | Scope::Tool | Scope::Supplied => true,
        Scope::Server => is_server_component(component),
        Scope::Client => matches!(component, "client" | "editor"),
        Scope::EntityApps => matches!(component, "client" | "cell" | "bot"),
        Scope::BaseApp => component == "base",
    }
}

/// Whether a component name belongs to a server build: the role split
/// [`carries_role`] applies, published so the interpreter can gate the
/// surfaces it registers outside the method tables -- the `serverMode`
/// marker is the one that follows this list today.
///
/// A component without a name -- the interpreter's default, or a
/// NUL-mangled one -- is not a server build.
pub fn is_server_component(component: &str) -> bool {
    matches!(component, "base" | "cell" | "database" | "bot")
}

/// The exact non-dunder names a component should see on `BigWorld`: every
/// ported row the role carries, sorted. This is the list BW-03-04-01's
/// scenarios compare against live `dir(BigWorld)` -- the manifest stays
/// the single source of the per-role surface.
pub fn ported_surface_for_role(component: &str) -> Vec<&'static str> {
    let mut names = Vec::new();
    for entry in ENTRIES {
        if entry.module != "BigWorld" {
            continue;
        }
        for surface in entry.items {
            if surface.status != Status::Ported {
                continue;
            }
            if carries_role(surface.scope, component) {
                names.push(surface.name);
            }
        }
    }
    names.sort_unstable();
    names
}

/// The inventory walk: every [`Status::Ported`] module must pass `imports`
/// and every [`Status::Ported`] surface on it must pass `carries` -- unless
/// its scope is gated and `role` says the live build does not carry it, in
/// which case the row makes the opposite claim: the name must be ABSENT,
/// and presence is the [`Mismatch::ItemScopeLeak`] failure. A gated row
/// under its own role makes the plain presence claim.
/// [`Status::Pending`] and [`Status::Covered`] rows make no claim here, so a
/// half-delivered module is not a failure -- the rows that are marked are the
/// contract. The interpreter operations come in as closures so a unit test
/// can walk a table that lies.
fn audit(
    entries: &[Entry],
    imports: &mut dyn FnMut(&str) -> bool,
    carries: &mut dyn FnMut(&str, &str) -> bool,
    role: &mut dyn FnMut(Scope) -> bool,
) -> Vec<Mismatch> {
    let mut mismatches = Vec::new();
    for entry in entries {
        if entry.status != Status::Ported {
            continue;
        }
        if !imports(entry.module) {
            mismatches.push(Mismatch::ModuleMissing {
                module: entry.module,
            });
            continue;
        }
        for surface in entry.items {
            if surface.status != Status::Ported {
                continue;
            }
            let gated = matches!(
                surface.scope,
                Scope::Client | Scope::Server | Scope::EntityApps | Scope::BaseApp
            );
            if gated && !role(surface.scope) {
                // The absence is the contract here; a leak is the failure.
                if carries(entry.module, surface.name) {
                    mismatches.push(Mismatch::ItemScopeLeak {
                        module: entry.module,
                        item: surface.name,
                    });
                }
                continue;
            }
            if !carries(entry.module, surface.name) {
                mismatches.push(Mismatch::ItemMissing {
                    module: entry.module,
                    item: surface.name,
                });
            }
        }
    }
    mismatches
}

/// Run the manifest against the live interpreter: every [`Status::Ported`]
/// module must import and every [`Status::Ported`] surface on it must
/// resolve, with the gated surfaces checked against the role the live
/// `BigWorld.component` names -- the same read `Script::init` based the
/// method-table links on (`script.cpp:1409-1453`). Returns every failure,
/// or `Ok` when the manifest and the interpreter agree.
///
/// # Safety
///
/// The interpreter must be up and the calling thread must own the GIL.
pub unsafe fn check() -> Result<(), Vec<Mismatch>> {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    let component = unsafe {
        let big_world = capi::PyImport_ImportModule(c"BigWorld".as_ptr());
        if big_world.is_null() {
            capi::clear_error();
            String::new()
        } else {
            // SAFETY: `big_world` is a live object and the name has no NUL.
            let value = capi::PyObject_GetAttrString(big_world, c"component".as_ptr());
            capi::dec_ref(big_world);
            if value.is_null() {
                // The NUL-mangled name left the attribute unset.
                capi::clear_error();
                String::new()
            } else {
                // SAFETY: `value` is a live object.
                let name = capi::to_string(value);
                capi::dec_ref(value);
                name
            }
        }
    };
    let mut imports = |name: &str| {
        // SAFETY: the interpreter is up and this thread owns the GIL; the
        // name is a manifest module name, plain ASCII with no interior NUL.
        let module = unsafe {
            capi::PyImport_ImportModule(
                std::ffi::CString::new(name)
                    .expect("a manifest module name has no NUL")
                    .as_ptr(),
            )
        };
        if module.is_null() {
            // A failed import leaves an exception set; the check is an
            // inventory walk, not a script run, so it clears like the
            // reference's ScriptErrorPrint does.
            unsafe { capi::clear_error() };
            return false;
        }
        // SAFETY: the reference the import handed back is dropped here.
        unsafe { capi::dec_ref(module) };
        true
    };
    let mut carries = |module: &str, surface: &str| {
        // The module cache answers the repeat import, so the per-surface
        // probe costs a dictionary hit.
        // SAFETY: the interpreter is up and this thread owns the GIL.
        let handle = unsafe {
            capi::PyImport_ImportModule(
                std::ffi::CString::new(module)
                    .expect("a manifest module name has no NUL")
                    .as_ptr(),
            )
        };
        if handle.is_null() {
            // SAFETY: a failed import leaves an exception set.
            unsafe { capi::clear_error() };
            return false;
        }
        // SAFETY: `handle` is a live object and the name has no interior NUL.
        let present = unsafe {
            capi::PyObject_HasAttrString(
                handle,
                std::ffi::CString::new(surface)
                    .expect("a manifest surface name has no NUL")
                    .as_ptr(),
            )
        };
        // SAFETY: the probe's reference is dropped here.
        unsafe { capi::dec_ref(handle) };
        present == 1
    };
    let mut role = |scope: Scope| carries_role(scope, &component);
    let mismatches = audit(ENTRIES, &mut imports, &mut carries, &mut role);
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row carries a citation, and a function row carries a signature:
    /// the acceptance is that the manifest is checkable, and a row with no
    /// source or no signature is not.
    #[test]
    fn every_row_cites_its_source_and_signs_its_functions() {
        for entry in entries() {
            assert!(
                !entry.reference.is_empty(),
                "{} has no source",
                describe(entry)
            );
            assert!(!entry.note.is_empty(), "{} has no note", describe(entry));
            for surface in entry.items {
                assert!(
                    !surface.reference.is_empty(),
                    "{}.{} has no source",
                    entry.module,
                    surface.name
                );
                assert!(
                    !surface.returns.is_empty(),
                    "{}.{} has no return description",
                    entry.module,
                    surface.name
                );
                if surface.kind == Kind::Function {
                    assert!(
                        !surface.signature.is_empty(),
                        "{}.{} has no signature",
                        entry.module,
                        surface.name
                    );
                }
                if surface.status == Status::Ported {
                    assert!(
                        entry.status == Status::Ported,
                        "{}.{} claims to be ported while its module is not",
                        entry.module,
                        surface.name
                    );
                }
            }
        }
    }

    /// The names the manifest exists to keep straight: the two scoping facts
    /// opposite to intuition, and the module rows a wrong name would strand.
    #[test]
    fn the_inventory_names_the_modules_this_lane_owns() {
        for expected in [
            "BigWorld", "Math", "ResMgr", "BWConfig", "_BWp", "__main__", "bw_site",
        ] {
            assert!(
                find(expected).is_some(),
                "{expected} is not in the manifest"
            );
        }
    }

    /// `BigWorld.callback` is client only and `addEventListener` is server
    /// only: pin the pair, because BW-03-03-03 builds the timer module off
    /// this row and a flip here would send it to the wrong process. The
    /// delivered timer faces carry the same split: the callback pair on the
    /// client rows, the server trio on the server rows.
    #[test]
    fn the_counter_intuitive_scopes_are_pinned() {
        let big_world = find("BigWorld").expect("BigWorld");
        let callback = big_world
            .items
            .iter()
            .find(|item| item.name == "callback")
            .expect("callback row");
        assert_eq!(callback.scope, Scope::Client);
        let listener = big_world
            .items
            .iter()
            .find(|item| item.name == "addEventListener")
            .expect("addEventListener row");
        assert_eq!(listener.scope, Scope::Server);
        for (name, scope) in [
            ("cancelCallback", Scope::Client),
            ("addTimer", Scope::Server),
            ("delTimer", Scope::Server),
            ("addFrequentTask", Scope::Server),
        ] {
            let row = big_world
                .items
                .iter()
                .find(|item| item.name == name)
                .unwrap_or_else(|| panic!("{name} row"));
            assert_eq!(row.scope, scope, "{name}");
        }
        // And its module is shared, which is exactly why the per-item scope
        // exists.
        assert_eq!(big_world.scope, Scope::Both);
    }

    /// The per-role visible surface (BW-03-04-01/02) pinned as the strings
    /// the scenarios compare live `dir(BigWorld)` against: the server
    /// builds carry the listener pair, the timer trio and the mode marker,
    /// the client and the editor carry the callback pair, the script class
    /// bases follow their registration sites (`Entity` on the client, the
    /// cell app and the bots tool, `Base`/`Proxy` on the base app alone),
    /// and an unknown or empty name carries only the shared faces.
    #[test]
    fn the_role_visible_surface_is_pinned() {
        let server = "addEventListener, addFrequentTask, addTimer, component, \
                      delTimer, fetchURL, removeEventListener, serverMode";
        let entity_apps = "Entity, addEventListener, addFrequentTask, addTimer, \
                           component, delTimer, fetchURL, removeEventListener, \
                           serverMode";
        for (component, expected) in [
            (
                "base",
                "Base, Proxy, addEventListener, addFrequentTask, \
                      addTimer, component, delTimer, fetchURL, \
                      removeEventListener, serverMode",
            ),
            ("cell", entity_apps),
            ("database", server),
            ("bot", entity_apps),
            (
                "client",
                "Entity, callback, cancelCallback, component, fetchURL",
            ),
            ("editor", "callback, cancelCallback, component, fetchURL"),
            ("watcher", "component, fetchURL"),
            ("", "component, fetchURL"),
        ] {
            assert_eq!(
                ported_surface_for_role(component).join(", "),
                expected,
                "{component}"
            );
        }
    }

    /// The role gate is the build split in runtime shape: a server
    /// component list against the non-server builds, an unknown name
    /// carrying neither side.
    #[test]
    fn the_role_gate_follows_the_build_split() {
        for server in ["base", "cell", "database", "bot"] {
            assert!(is_server_component(server), "{server}");
            assert!(
                carries_role(Scope::Server, server),
                "server face on {server}"
            );
            assert!(
                !carries_role(Scope::Client, server),
                "client face on {server}"
            );
        }
        for client in ["client", "editor"] {
            assert!(!is_server_component(client), "{client}");
            assert!(
                carries_role(Scope::Client, client),
                "client face on {client}"
            );
            assert!(
                !carries_role(Scope::Server, client),
                "server face on {client}"
            );
        }
        for nameless in ["", "bad\u{0}name", "mystery"] {
            assert!(!carries_role(Scope::Client, nameless), "{nameless:?}");
            assert!(!carries_role(Scope::Server, nameless), "{nameless:?}");
        }
        // Ungated scopes are carried everywhere.
        for scope in [Scope::Both, Scope::Tool, Scope::Supplied] {
            assert!(carries_role(scope, "client"), "{scope:?}");
            assert!(carries_role(scope, "cell"), "{scope:?}");
            assert!(carries_role(scope, ""), "{scope:?}");
        }
        // The entity-class registration split: the client, the cell app and
        // the bots tool carry `Entity`; the base app carries `Base`/`Proxy`
        // alone -- and notably not the database app, whose `BigWorld` never
        // gets either (`dbapp.cpp` only imports the module).
        for entity in ["client", "cell", "bot"] {
            assert!(
                carries_role(Scope::EntityApps, entity),
                "Entity face on {entity}"
            );
            assert!(
                !carries_role(Scope::BaseApp, entity),
                "baseapp face on {entity}"
            );
        }
        assert!(carries_role(Scope::BaseApp, "base"), "baseapp face on base");
        for outside in ["database", "editor", "watcher", ""] {
            assert!(
                !carries_role(Scope::EntityApps, outside),
                "Entity face on {outside}"
            );
            assert!(
                !carries_role(Scope::BaseApp, outside),
                "baseapp face on {outside}"
            );
        }
    }

    /// Project-supplied modules are marked as such rather than given an
    /// invented engine source: their surface is an input, not a deliverable.
    #[test]
    fn supplied_modules_are_not_claimed_by_the_engine() {
        for supplied in [
            "Keys",
            "Personality",
            "BWTwoWay",
            "FixedDictWrappers",
            "UserTypes",
        ] {
            let entry = find(supplied).unwrap_or_else(|| panic!("{supplied} is not recorded"));
            assert_eq!(entry.scope, Scope::Supplied, "{supplied}");
            assert!(
                entry.items.is_empty(),
                "{supplied} claims a surface it does not own"
            );
        }
    }

    /// Ported rows are exactly what the interpreter carries today: the
    /// BigWorld shell with its `component` attribute, the timer faces
    /// BW-03-03-03 delivered -- the client pair gated to the client role,
    /// the server trio gated to the server roles -- the ungated
    /// `fetchURL` BW-03-03-05 delivered, the script class bases added by
    /// BW-16-03-02, plus the Math module with every factory name; the
    /// Windows-only matrix animation is the one row still pending.
    #[test]
    fn only_the_delivered_surface_is_marked_ported() {
        let ported_modules: Vec<_> = entries()
            .iter()
            .filter(|entry| entry.status == Status::Ported)
            .map(|entry| entry.module)
            .collect();
        assert_eq!(ported_modules, vec!["BigWorld", "Math"]);
        let ported_items: Vec<_> = find("BigWorld")
            .unwrap()
            .items
            .iter()
            .filter(|item| item.status == Status::Ported)
            .map(|item| item.name)
            .collect();
        assert_eq!(
            ported_items,
            vec![
                "component",
                "callback",
                "cancelCallback",
                "fetchURL",
                "addEventListener",
                "removeEventListener",
                "addTimer",
                "delTimer",
                "addFrequentTask",
                "serverMode",
                "Entity",
                "Base",
                "Proxy",
            ]
        );
        let math = find("Math").unwrap();
        let ported_math: Vec<_> = math
            .items
            .iter()
            .filter(|item| item.status == Status::Ported)
            .map(|item| item.name)
            .collect();
        assert_eq!(
            ported_math,
            vec![
                "getRegister",
                "Matrix",
                "Plane",
                "MatrixProduct",
                "MatrixInverse",
                "Vector2",
                "Vector3",
                "Vector4",
                "Vector4Product",
                "Vector4Animation",
                "Vector4Morph",
                "Vector4Translation",
                "Vector4Distance",
                "Vector4LFO",
                "Vector4MatrixAdaptor",
                "Vector4Swizzle",
                "Vector4Combiner",
                "Vector4Shader",
            ]
        );
        let pending_math: Vec<_> = math
            .items
            .iter()
            .filter(|item| item.status == Status::Pending)
            .map(|item| item.name)
            .collect();
        assert_eq!(pending_math, vec!["MatrixAnimation"]);
    }

    /// Rendering a row spells the scope and the status the way the
    /// reference's build vocabulary does, for every value the table can
    /// carry -- a report with a hole where a scope should be is not a
    /// report.
    #[test]
    fn rendering_a_row_spells_every_scope_and_status() {
        for (scope, word) in [
            (Scope::Both, "both"),
            (Scope::Client, "client"),
            (Scope::Server, "server"),
            (Scope::Tool, "tool"),
            (Scope::Supplied, "supplied"),
            (Scope::EntityApps, "entity-apps"),
            (Scope::BaseApp, "baseapp"),
        ] {
            assert_eq!(scope.as_str(), word);
        }
        for (status, word) in [
            (Status::Ported, "ported"),
            (Status::Covered, "covered"),
            (Status::Pending, "pending"),
        ] {
            assert_eq!(status.as_str(), word);
        }
        assert_eq!(
            describe(find("BigWorld").expect("BigWorld")),
            "BigWorld [both] ported"
        );
        // Every row renders: no spelling above is left unwalked.
        for entry in entries() {
            assert!(
                describe(entry).starts_with(entry.module),
                "{}",
                describe(entry)
            );
        }
    }

    /// The walk reports every way a table can lie, and each report reads as
    /// a sentence about the row, not a code.
    #[test]
    fn a_walk_reports_both_ways_a_table_can_lie() {
        let big_world = find("BigWorld").expect("BigWorld");
        let mut never_imports = |_: &str| false;
        let mut always_carries = |_: &str, _: &str| true;
        let mut carries_all = |_: Scope| true;
        let missing = audit(
            std::slice::from_ref(big_world),
            &mut never_imports,
            &mut always_carries,
            &mut carries_all,
        );
        assert_eq!(
            missing,
            vec![Mismatch::ModuleMissing { module: "BigWorld" }]
        );
        assert_eq!(
            missing[0].render(),
            "BigWorld is marked ported but does not import"
        );
        let mut imports = |_: &str| true;
        let mut carries_nothing = |_: &str, _: &str| false;
        let mut carries_none = |_: Scope| false;
        let uncarried = audit(
            std::slice::from_ref(big_world),
            &mut imports,
            &mut carries_nothing,
            &mut carries_none,
        );
        // With no role carried, every gated row's absence is the contract
        // and only the ungated `component` and `fetchURL` claims fail.
        assert_eq!(
            uncarried,
            vec![
                Mismatch::ItemMissing {
                    module: "BigWorld",
                    item: "component"
                },
                Mismatch::ItemMissing {
                    module: "BigWorld",
                    item: "fetchURL"
                },
            ]
        );
        assert_eq!(
            uncarried[0].render(),
            "BigWorld.component is marked ported but is not present"
        );
        // The leak direction: a client build carrying the server trio is
        // the breach the role gate exists to catch, and its report reads
        // as its own sentence.
        let mut carries_only_server =
            |_: &str, surface: &str| matches!(surface, "addTimer" | "delTimer" | "addFrequentTask");
        let mut client_role = |scope: Scope| matches!(scope, Scope::Client);
        let leak = audit(
            std::slice::from_ref(big_world),
            &mut imports,
            &mut carries_only_server,
            &mut client_role,
        );
        assert_eq!(
            leak,
            vec![
                Mismatch::ItemMissing {
                    module: "BigWorld",
                    item: "component"
                },
                Mismatch::ItemMissing {
                    module: "BigWorld",
                    item: "callback"
                },
                Mismatch::ItemMissing {
                    module: "BigWorld",
                    item: "cancelCallback"
                },
                Mismatch::ItemMissing {
                    module: "BigWorld",
                    item: "fetchURL"
                },
                Mismatch::ItemScopeLeak {
                    module: "BigWorld",
                    item: "addTimer"
                },
                Mismatch::ItemScopeLeak {
                    module: "BigWorld",
                    item: "delTimer"
                },
                Mismatch::ItemScopeLeak {
                    module: "BigWorld",
                    item: "addFrequentTask"
                },
            ]
        );
        assert_eq!(
            leak[4].render(),
            "BigWorld.addTimer is live under a role that does not carry it"
        );
    }

    /// The row constructor the static tables use is the plain field set, in
    /// order: eight positional arguments are one transposition away from a
    /// silently wrong column, so the mapping is asserted once here.
    #[test]
    fn the_row_constructor_sets_the_fields_in_order() {
        let row = item(
            "probe",
            Kind::Class,
            Scope::Tool,
            Status::Pending,
            "signature",
            "returns",
            "error",
            "reference",
        );
        assert_eq!(row.name, "probe");
        assert_eq!(row.kind, Kind::Class);
        assert_eq!(row.scope, Scope::Tool);
        assert_eq!(row.status, Status::Pending);
        assert_eq!(row.signature, "signature");
        assert_eq!(row.returns, "returns");
        assert_eq!(row.error, "error");
        assert_eq!(row.reference, "reference");
    }
}
