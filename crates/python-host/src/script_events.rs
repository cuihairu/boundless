//! The `BigWorld.addEventListener` / `removeEventListener` faces and the
//! startup events they listen to (`lib/pyscript/script_events.cpp`).
//!
//! The reference's `ScriptEvents` is the registry a server app owns:
//! `ScriptApp`'s constructor creates the `onInit` and `onFini` event
//! types (`script_app.cpp:44-49`), `initPersonality` folds the personality
//! module's same-named attributes in as level-0 listeners
//! (`:104`, `initFromPersonality` `script_events.cpp:248-268`), the app
//! triggers `onInit(isReload)` at startup (`:293`) and `onFini` at fini
//! (`:87`), and the module functions route `addEventListener` /
//! `removeEventListener` to the one singleton instance
//! (`script_events.cpp:281-356`). A listener list fires in level order,
//! stable for equal levels, and every listener's return value lands in the
//! trigger's results list, `None` for a listener that raised.
//!
//! Deltas kept visible from script:
//! - The port has no `ScriptApp` and no personality concept, so the host
//!   drives the startup and fini events: `trigger_on_init` /
//!   `trigger_on_fini` are the hooks, and `install` (called for a server
//!   component when the `BigWorld` module is built) creates the two
//!   standard event types the reference's app constructor creates.
//! - The delayable continuation is not ported: `triggerDelayableEvent`
//!   (`script_app.cpp:243-281`) collects listeners returning Twisted
//!   `Deferred` objects and re-enters the dispatcher when they resolve;
//!   the port triggers synchronously and returns the results to the host,
//!   which has no Twisted integration yet.
//! - The reference is a single `g_pInstance` slot that a second
//!   `ScriptEvents` object refuses (with a warning) and never replaces
//!   (`script_events.cpp:104-143`); the port's registry is one module
//!   static, so there is no second instance to warn about.
//! - The reload entry points are planned, not ported (BW-03-04-03): the
//!   reference reloads from the script side via `BigWorld.reloadScript` --
//!   `fullReload` (default True) to also reparse entity defs,
//!   `expectedDigest` (from `BigWorld.schemaDigest()`, empty = no
//!   constraint) as the versioned-schema barrier, `dryRun` to probe in a
//!   scratch interpreter without migrating -- registered per component:
//!   `cell` (`server/cellapp/cellapp.cpp:3251-3259`) and `base`
//!   (`server/baseapp/script_bigworld.cpp:1483-1587`). The client reloads
//!   via CapsLock+F11 with no Python face (`cellapp.cpp:3211-3212`), and
//!   DBApp has no reload face (it only runs `triggerOnInit()` at startup,
//!   `server/dbapp/dbapp.cpp:335`). A reload fires `onInit(True)` through
//!   `EntityType::init(true)` / `EntityType::reloadScript()` ->
//!   `triggerOnInit(true)` (`baseapp/entity_type.cpp:309,374`,
//!   `cellapp/entity_type.cpp:341,403`); the old-scripts recovery path
//!   (`reloadScript(true)`) does not re-fire it. The port has none of the
//!   scratch-subinterpreter swap (`Script::createInterpreter` /
//!   `swapInterpreter` / `destroyInterpreter`), the `reloadScript` module
//!   face, or the entity-def / UDO / mailbox / entity migrate chain
//!   (`server/cellapp/reload_script_plan.hpp`); those are the BW-03-04-04 /
//!   BW-03-04-05 follow-ons.

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_int};
use std::sync::{Mutex, OnceLock};

use crate::capi;
use crate::pyref::PyRef;

/// One registered listener: the callable and the level it fires at.
#[derive(Clone)]
struct Listener {
    function: PyRef,
    level: c_int,
}

/// One event type's listener list, in the order the listeners fire.
struct EventList {
    listeners: Vec<Listener>,
}

impl EventList {
    fn new() -> Self {
        EventList {
            listeners: Vec::new(),
        }
    }

    /// `ScriptEventList::add` (`script_events.cpp:71-88`): the listener
    /// lands after every element whose level is `<=` its own, so higher
    /// levels fire later and equal levels keep insertion order.
    fn add(&mut self, function: *mut capi::PyObject, level: c_int) {
        let position = self
            .listeners
            .iter()
            .position(|listener| listener.level > level)
            .unwrap_or(self.listeners.len());
        self.listeners.insert(
            position,
            Listener {
                // SAFETY: `function` is a live object the caller owns a
                // reference to.
                function: unsafe { PyRef::borrowed(function).clone() },
                level,
            },
        );
    }

    /// `ScriptEventList::remove` (`script_events.cpp:90-120`): the first
    /// listener matching by pointer identity is detached.
    fn remove(&mut self, function: *mut capi::PyObject) -> bool {
        if let Some(position) = self
            .listeners
            .iter()
            .position(|listener| listener.function.as_ptr() == function)
        {
            self.listeners.remove(position);
            true
        } else {
            false
        }
    }
}

/// The registry singleton: event types to listener lists, plus the
/// "initialised" bit the reference's `g_pInstance` provides.
struct Registry {
    initialised: bool,
    events: BTreeMap<String, EventList>,
}

// SAFETY: the registry's `PyRef`s are raw object pointers; the static they
// feed is only ever touched on the interpreter thread that owns the GIL.
unsafe impl Send for Registry {}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            initialised: false,
            events: BTreeMap::new(),
        })
    })
}

/// The one lock scope helper, the same poisoned-mutex recovery `timers`
/// uses: a listener that panicked must not wedge the registry.
fn guarded() -> std::sync::MutexGuard<'static, Registry> {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `ScriptEvents::createEventType` (`script_events.cpp:145-152`): the
/// registry's `operator[]` overwrites, and the standard types the server
/// app creates are `onInit` and `onFini`.
pub fn create_event_type(name: &str) {
    guarded().events.insert(name.to_string(), EventList::new());
}

/// `ScriptApp`'s constructor (`script_app.cpp:44-49`) plus the module
/// functions: for a server component, stand up the standard event types
/// and hang `addEventListener` / `removeEventListener` on `BigWorld`.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and `module`
/// must be the live `BigWorld` module object.
pub unsafe fn install(module: *mut capi::PyObject) {
    let mut state = guarded();
    state.initialised = true;
    state.events.insert("onInit".to_string(), EventList::new());
    state.events.insert("onFini".to_string(), EventList::new());
    drop(state);
    let methods: &'static mut [capi::PyMethodDef] = Box::leak(
        vec![
            capi::PyMethodDef {
                ml_name: c"addEventListener".as_ptr(),
                ml_meth: Some(method_add_event_listener),
                ml_flags: capi::METH_VARARGS,
                ml_doc: c"Associates an event listener with an event. The \
                        callback will be called whenever the event is \
                        triggered."
                    .as_ptr(),
            },
            capi::PyMethodDef {
                ml_name: c"removeEventListener".as_ptr(),
                ml_meth: Some(method_remove_event_listener),
                ml_flags: capi::METH_VARARGS,
                ml_doc: c"Removes an event listener previously added via \
                        addEventListener"
                    .as_ptr(),
            },
            capi::PyMethodDef::END,
        ]
        .into_boxed_slice(),
    );
    // SAFETY: the module is live and the table is a terminated static array
    // of `PyMethodDef`.
    let added = capi::PyModule_AddFunctions(module, methods.as_ptr());
    if added != 0 {
        // The reference logs through `ScriptErrorPrint` and moves on.
        capi::PyErr_PrintEx(0);
        capi::clear_error();
    }
}

/// `ScriptEvents::triggerEvent` (`script_events.cpp:170-190`): the
/// listeners fire in level order, each result lands in the returned list
/// (`None` for a listener that raised -- the exception is printed and
/// cleared the way `Script::ask` does), and the boolean is false when any
/// listener failed or the event type is unknown. `args` is a fresh tuple
/// whose reference this call steals, the way the reference's
/// `pArgsHolder` does.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and `args`
/// must be a tuple the caller owns one reference to.
pub unsafe fn trigger_event(name: &str, args: *mut capi::PyObject) -> (bool, Vec<PyRef>) {
    // The listeners are copied before the walk, the way
    // `ScriptEventList::triggerEvent` copies its list
    // (`script_events.cpp:48-69`): a listener that re-enters and
    // registers or removes a listener must not disturb this walk.
    let listeners: Vec<Listener> = {
        let state = guarded();
        let Some(list) = state.events.get(name) else {
            return (false, Vec::new());
        };
        list.listeners.clone()
    };
    let mut results = Vec::new();
    let mut is_okay = true;
    for listener in &listeners {
        let function = listener.function.as_ptr();
        // `ScriptEventList::Element::call` (`script_events.cpp:26-39`)
        // INCREFs the function and the arguments before `Script::ask`,
        // which DECREFs both on the way out.
        capi::inc_ref(function);
        capi::inc_ref(args);
        let result = capi::PyObject_CallObject(function, args);
        capi::dec_ref(function);
        capi::dec_ref(args);
        if result.is_null() {
            // `Script::ask` prints the traceback and clears the
            // exception (`script.cpp:1185-1204`).
            is_okay = false;
            capi::PyErr_PrintEx(0);
            results.push(PyRef::borrowed(capi::py_none()).clone());
        } else {
            results.push(PyRef::from_owned(result).expect("a live result"));
        }
    }
    // The reference's `pArgsHolder` releases the caller's reference.
    capi::dec_ref(args);
    (is_okay, results)
}

/// `ScriptEvents::triggerTwoEvents` (`script_events.cpp:192-204`): both
/// events get the same argument tuple.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and `args`
/// must be a tuple the caller owns one reference to.
pub unsafe fn trigger_two_events(first: &str, second: &str, args: *mut capi::PyObject) -> bool {
    capi::inc_ref(args);
    let first_ok = unsafe { trigger_event(first, args) }.0;
    // The second call steals the caller's original reference.
    let second_ok = unsafe { trigger_event(second, args) }.0;
    first_ok && second_ok
}

/// `ScriptEvents::initFromPersonality` (`script_events.cpp:248-268`): for
/// every registered event type, the personality module's same-named
/// attribute becomes a level-0 listener. `personality` is a borrowed
/// module reference the caller owns.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and
/// `personality` must be a live module object.
pub unsafe fn init_from_personality(personality: *mut capi::PyObject) {
    let names: Vec<String> = {
        let state = guarded();
        state.events.keys().cloned().collect()
    };
    for name in names {
        // SAFETY: `personality` is live and the event names are plain
        // ASCII, no interior NUL.
        let attribute = unsafe {
            capi::PyObject_GetAttrString(
                personality,
                std::ffi::CString::new(name.clone())
                    .expect("an event name has no NUL")
                    .as_ptr(),
            )
        };
        if attribute.is_null() {
            // The reference clears a missing attribute and moves on
            // (`ScriptErrorClear`).
            capi::clear_error();
            continue;
        }
        // SAFETY: `attribute` is a live object this call owns.
        let callback = PyRef::from_owned(attribute).expect("a live attribute");
        guarded()
            .events
            .get_mut(&name)
            .expect("the name is a registered event type")
            .add(callback.as_ptr(), 0);
    }
}

/// `ScriptApp::triggerOnInit` (`script_app.cpp:289-295`): the startup
/// event, one boolean argument -- true on a reload, false on a plain
/// start.
pub fn trigger_on_init(is_reload: bool) -> bool {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let args = capi::PyTuple_New(1);
        if args.is_null() {
            return false;
        }
        let flag = capi::PyBool_FromLong(is_reload as std::os::raw::c_long);
        if flag.is_null() {
            capi::dec_ref(args);
            return false;
        }
        // SAFETY: the tuple is fresh and the item is a new reference.
        capi::PyTuple_SetItem(args, 0, flag);
        let (ok, _results) = unsafe { trigger_event("onInit", args) };
        ok
    }
}

/// `ScriptApp::fini` (`script_app.cpp:81-89`): the fini event, no
/// arguments.
pub fn trigger_on_fini() -> bool {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let args = capi::PyTuple_New(0);
        if args.is_null() {
            return false;
        }
        let (ok, _results) = unsafe { trigger_event("onFini", args) };
        ok
    }
}

/// `ScriptApp::~ScriptApp` (`script_app.cpp:50-55`): the registry clears
/// before `Script::fini`, so every held listener releases while the
/// interpreter is still up. The host calls this from its own teardown.
pub fn reset() {
    let mut state = guarded();
    state.initialised = false;
    state.events.clear();
}

/// `BigWorld.addEventListener` (`script_events.cpp:281-327`): parse the
/// event name, callable and optional level, then register. The walls are
/// the reference's, in its order -- a non-callable raises `TypeError`
/// before the registry is consulted.
unsafe extern "C" fn method_add_event_listener(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut name: *const c_char = std::ptr::null();
    let mut callback: *mut capi::PyObject = std::ptr::null_mut();
    let mut level: c_int = 0;
    // SAFETY: the caller owns the GIL and the parse targets are live
    // locals of C ABI sizes.
    if unsafe {
        capi::PyArg_ParseTuple(args, c"sO|i".as_ptr(), &mut name, &mut callback, &mut level)
    } == 0
    {
        // The parser left its own `TypeError` pending.
        return std::ptr::null_mut();
    }
    if callback.is_null() || unsafe { capi::PyCallable_Check(callback) } == 0 {
        // SAFETY: the reference's one sentence (`script_events.cpp:298-301`).
        unsafe {
            capi::PyErr_SetString(capi::PyExc_TypeError, c"Argument is not callable".as_ptr());
        }
        return std::ptr::null_mut();
    }
    let mut state = guarded();
    if !state.initialised {
        // SAFETY: `script_events.cpp:302-305`.
        unsafe {
            capi::PyErr_SetString(
                capi::PyExc_SystemError,
                c"ScriptEvents not initialised".as_ptr(),
            );
        }
        return std::ptr::null_mut();
    }
    let Some(event) = state
        .events
        .get_mut(unsafe { CStr::from_ptr(name) }.to_str().unwrap_or(""))
    else {
        // SAFETY: the name string the parser handed back is NUL-terminated.
        unsafe {
            capi::PyErr_Format(
                capi::PyExc_ValueError,
                c"Invalid event type '%s'".as_ptr(),
                name,
            );
        }
        return std::ptr::null_mut();
    };
    event.add(callback, level);
    capi::py_none()
}

/// `BigWorld.removeEventListener` (`script_events.cpp:330-356`): the walls
/// match the reference, including its one error for both an unknown event
/// type and a listener that was not registered.
unsafe extern "C" fn method_remove_event_listener(
    _module: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    let mut name: *const c_char = std::ptr::null();
    let mut callback: *mut capi::PyObject = std::ptr::null_mut();
    // SAFETY: the caller owns the GIL and the parse targets are live
    // locals of C ABI sizes.
    if unsafe { capi::PyArg_ParseTuple(args, c"sO".as_ptr(), &mut name, &mut callback) } == 0 {
        return std::ptr::null_mut();
    }
    if callback.is_null() || unsafe { capi::PyCallable_Check(callback) } == 0 {
        // SAFETY: `script_events.cpp:343-346`.
        unsafe {
            capi::PyErr_SetString(capi::PyExc_TypeError, c"Argument is not callable".as_ptr());
        }
        return std::ptr::null_mut();
    }
    let mut state = guarded();
    if !state.initialised {
        // SAFETY: `script_events.cpp:347-350`.
        unsafe {
            capi::PyErr_SetString(
                capi::PyExc_SystemError,
                c"ScriptEvents not initialised".as_ptr(),
            );
        }
        return std::ptr::null_mut();
    }
    let Some(event) = state
        .events
        .get_mut(unsafe { CStr::from_ptr(name) }.to_str().unwrap_or(""))
    else {
        // SAFETY: the name string the parser handed back is NUL-terminated.
        unsafe {
            capi::PyErr_Format(
                capi::PyExc_ValueError,
                c"Invalid event type '%s'".as_ptr(),
                name,
            );
        }
        return std::ptr::null_mut();
    };
    if !event.remove(callback) {
        // The reference raises the same sentence whether the event type or
        // the listener was missing (`script_events.cpp:352-356`).
        // SAFETY: the name string the parser handed back is NUL-terminated.
        unsafe {
            capi::PyErr_Format(
                capi::PyExc_ValueError,
                c"Invalid event type '%s'".as_ptr(),
                name,
            );
        }
        return std::ptr::null_mut();
    }
    capi::py_none()
}
