//! The `BigWorld.fetchURL` face and the `PyURLResponse` it hands the
//! callback (`common/py_urlrequest.cpp`), over the `URL::Manager` singleton
//! of `lib/urlrequest/manager.cpp`.
//!
//! The reference drives requests through libcurl's `curl_multi` handle set,
//! pumped by a frequent task on the app dispatcher; completion runs the
//! callback on the dispatcher thread under the GIL
//! (`manager.cpp:52-99`, `:183-221`, `py_urlrequest.cpp:105-129`). The port
//! keeps the same shape with the standard library alone: `init` stands up a
//! manager owning one completion channel, `fetchURL` spawns one detached
//! worker thread per request that performs a blocking HTTP/1.1 exchange
//! over `TcpStream` (the std stand-in for the curl easy handle), and `tick`
//! is the frequent-task pump the host app drives, draining completions and
//! running the callbacks on the script thread under the GIL.
//!
//! Deltas kept visible from script:
//! - The transport speaks plain `http://` over `TcpStream`; an `https://`
//!   URL answers the curl `CURLE_UNSUPPORTED_PROTOCOL` sentence. The
//!   reference builds curl with TLS.
//! - Timeout maps to the curl sentence "Timeout was reached" with a zero
//!   response code; other transport failures map to the matching
//!   `curl_easy_strerror` sentence (`request.cpp:355-361`). The body a
//!   failure leaves behind survives like the reference's partial `body_`.
//! - The type errors, error sentences and the read-only attribute surface
//!   are the reference's, including the double quote of
//!   `"Invalid method %s. ... 'PATCH''"`.
//! - `URL::Manager::init` is app-driven in the reference
//!   (`client/connection_control.cpp:342` and the server apps); the port
//!   hands the host the same `init`/`fini`/`tick` steps.
//! - The reference's `checkHandles` frequent task runs every frame on the
//!   dispatcher (`manager.cpp:52-99`); the port's `tick` is the host's
//!   frame step.
//!
//! `PY_TYPEOBJECT( PyURLResponse )` (`py_urlrequest.cpp:62`) never links
//! the class onto the `BigWorld` module -- scripts only ever receive
//! instances through the callback -- so the manifest row is `Covered`, not
//! `Ported`.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::binding::{self, TypeSpec};
use crate::capi;
use crate::math::body;
use crate::output_writer;
use crate::pyref::PyRef;

/// `PyURLResponse`'s `tp_name` (`PY_TYPEOBJECT`, `py_urlrequest.cpp:62`):
/// the class name the read-only attribute errors quote.
const RESPONSE_NAME: &str = "PyURLResponse";

/// `PyErr_SetString` on a chosen global exception, with a Rust message
/// (the reference's plain `PyErr_SetString` calls).
fn raise(exception: *mut capi::PyObject, message: &str) {
    // SAFETY: the message is a fresh unicode object.
    unsafe {
        let rendered = capi::unicode_from_bytes(message.as_bytes());
        capi::PyErr_SetObject(exception, rendered);
        capi::dec_ref(rendered);
    }
}

/// The `URL::Method` set (`request.hpp:24-31`).
#[derive(Clone, Copy, PartialEq)]
enum Method {
    Get,
    Put,
    Post,
    Delete,
    Patch,
}

impl Method {
    /// The wire verb.
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
            Method::Post => "POST",
            Method::Delete => "DELETE",
            Method::Patch => "PATCH",
        }
    }
}

/// The `URL::PostData` pair (`request.hpp`): a raw string, or a multipart
/// form's field list, each with its own encoding.
enum PostData {
    Raw(String),
    Multipart(Vec<(String, String)>),
}

/// One request as the parse hands it over: the `Request::fetchURL` call
/// arguments (`request.cpp:246-250`).
struct RequestData {
    url: String,
    method: Method,
    headers: Vec<String>,
    timeout: f32,
    post_data: Option<PostData>,
}

/// One completed exchange: the `PyURLResponse` fields plus the error
/// sentence the reference passes to `Response::onDone`, and the request id
/// the delivery matches against its callback.
struct Completion {
    id: u64,
    content_type: String,
    body: String,
    response_code: i32,
    error: Option<String>,
}

/// The exchange state a worker thread carries; a transport error keeps
/// whatever was already received, the way the reference's partial
/// `body_` survives to `Response::onDone`.
struct Outcome {
    content_type: String,
    body: String,
    response_code: i32,
    error: Option<String>,
}

/// The `URL::Manager` singleton's port state (`manager.cpp:101-115`): the
/// completion channel, the callback of each request still in flight, and
/// the id counter. `receiver` is drained by `tick`, the `CheckHandlesTask`
/// stand-in (`manager.cpp:52-99`).
struct Manager {
    sender: Sender<Completion>,
    receiver: Receiver<Completion>,
    pending: BTreeMap<u64, PyRef>,
    next_id: u64,
}

/// `URL::Manager::s_managerInstance_` (`manager.cpp:129`): `None` until
/// `init` stands it up, the gate `py_fetchURL` checks first
/// (`py_urlrequest.cpp:166-169`).
static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);

// The pointers are interpreter objects handed around only between the
// script thread's calls, like the timer store's (`timers.rs`'s `Store`).
unsafe impl Send for Manager {}

/// The `PyURLResponse` type object's address once `install` built it; the
/// registry holds the reference and the address is what delivery reads
/// back. Zero until then.
static RESPONSE_TYPE: AtomicUsize = AtomicUsize::new(0);

/// `URL::Manager::init` (`manager.cpp:128-136`): stand the singleton up. A
/// second `init` warns and refuses -- the reference's `false` return.
///
/// # Safety
///
/// The interpreter must be up and the caller must own the GIL.
pub fn init() -> bool {
    let mut slot = MANAGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_some() {
        return false;
    }
    let (sender, receiver) = channel();
    *slot = Some(Manager {
        sender,
        receiver,
        pending: BTreeMap::new(),
        next_id: 1,
    });
    true
}

/// `Manager::fini` (`manager.cpp:138-141`) and the `ScriptFini` guard
/// (`manager.cpp:14-24`): take the singleton down, releasing every held
/// callback while the interpreter is still alive.
///
/// # Safety
///
/// The interpreter must still be up: the dropped `PyRef`s decref through
/// it.
pub fn fini() {
    let mut slot = MANAGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = None;
    RESPONSE_TYPE.store(0, Ordering::SeqCst);
}

/// `URL::Manager::instance()` (`manager.cpp:113`): the gate
/// `py_fetchURL` checks before anything else.
fn instance_up() -> bool {
    MANAGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some()
}

/// `BigWorld.fetchURL` (`py_urlrequest.cpp:163-317`): the `sO|OfsO`
/// keywords, the five walls in the reference's order, then one dispatch.
/// `METH_VARARGS | METH_KEYWORDS` answers the third `kwargs` argument the
/// `PY_MODULE_FUNCTION_WITH_KEYWORDS` form passes
/// (`py_urlrequest.cpp:318-319`).
unsafe extern "C" fn method_fetch_url(
    _self: *mut capi::PyObject,
    args: *mut capi::PyObject,
    kwargs: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // The manager gate comes first, before the parse
    // (`py_urlrequest.cpp:166-169`).
    if !instance_up() {
        unsafe {
            raise(
                capi::PyExc_EnvironmentError,
                "PyURLRequest has not been initialised",
            )
        };
        return std::ptr::null_mut();
    }

    let mut url: *mut c_char = std::ptr::null_mut();
    let mut callback: *mut capi::PyObject = std::ptr::null_mut();
    let mut headers: *mut capi::PyObject = std::ptr::null_mut();
    let mut timeout: f32 = 0.0;
    let mut method: *mut c_char = std::ptr::null_mut();
    let mut post_data: *mut capi::PyObject = std::ptr::null_mut();

    // The keyword list, terminated like the reference's
    // (`py_urlrequest.cpp:187-194`).
    let mut keywords: Vec<*mut c_char> = vec![
        c"url".as_ptr() as *mut c_char,
        c"callback".as_ptr() as *mut c_char,
        c"headers".as_ptr() as *mut c_char,
        c"timeout".as_ptr() as *mut c_char,
        c"method".as_ptr() as *mut c_char,
        c"postdata".as_ptr() as *mut c_char,
        std::ptr::null_mut(),
    ];

    // SAFETY: the interpreter holds the GIL, the tuple and the dict are
    // live, and the target slots outlive the call.
    let parsed = unsafe {
        capi::PyArg_ParseTupleAndKeywords(
            args,
            kwargs,
            c"sO|OfsO".as_ptr(),
            keywords.as_mut_ptr(),
            &mut url,
            &mut callback,
            &mut headers,
            &mut timeout,
            &mut method,
            &mut post_data,
        )
    };
    if parsed == 0 {
        // The parser left its own `TypeError` pending.
        return std::ptr::null_mut();
    }

    // The callback wall (`:196-202`).
    if callback.is_null() || unsafe { capi::PyCallable_Check(callback) } == 0 {
        unsafe { raise(capi::PyExc_ValueError, "Callback is not callable") };
        return std::ptr::null_mut();
    }

    // The headers wall (`:204-229`): each item is str()'d and UTF8'd into
    // the header list.
    let mut header_list: Vec<String> = Vec::new();
    if !headers.is_null() && headers != capi::py_none() {
        if unsafe { capi::PySequence_Check(headers) } == 0 {
            unsafe { raise(capi::PyExc_ValueError, "headers argument is not a sequence") };
            return std::ptr::null_mut();
        }
        // SAFETY: the check above guarantees the sequence protocol.
        let count = unsafe { capi::PySequence_Size(headers) };
        let mut index = 0;
        while index < count {
            let item = unsafe { capi::PySequence_GetItem(headers, index) };
            let text = unsafe { capi::PyObject_Str(item) };
            let rendered = unsafe { capi::to_string(text) };
            unsafe { capi::dec_ref(text) };
            unsafe { capi::dec_ref(item) };
            header_list.push(rendered);
            index += 1;
        }
    }

    // The postdata wall (`:231-272`): a unicode string is raw, anything a
    // mapping handles is multipart, the rest is refused.
    let mut post: Option<PostData> = None;
    if !post_data.is_null() && post_data != capi::py_none() {
        if unsafe { capi::py_str_check(post_data) } {
            post = Some(PostData::Raw(unsafe { capi::to_string(post_data) }));
        } else if unsafe { capi::PyMapping_Check(post_data) } == 1 {
            // SAFETY: the check above guarantees the mapping protocol.
            let count = unsafe { capi::PyMapping_Size(post_data) };
            let items = unsafe { capi::PyMapping_Items(post_data) };
            let mut fields = Vec::with_capacity(count as usize);
            let mut index = 0;
            while index < count {
                // A mapping item is a (key, value) tuple
                // (`dict_items` over a dict).
                let pair = unsafe { capi::PySequence_GetItem(items, index) };
                let key = unsafe { capi::PyTuple_GetItem(pair, 0) };
                let value = unsafe { capi::PyTuple_GetItem(pair, 1) };
                let key_str = unsafe { capi::PyObject_Str(key) };
                let value_str = unsafe { capi::PyObject_Str(value) };
                let key_text = unsafe { capi::to_string(key_str) };
                let value_text = unsafe { capi::to_string(value_str) };
                unsafe { capi::dec_ref(value_str) };
                unsafe { capi::dec_ref(key_str) };
                unsafe { capi::dec_ref(value) };
                unsafe { capi::dec_ref(key) };
                unsafe { capi::dec_ref(pair) };
                fields.push((key_text, value_text));
                index += 1;
            }
            unsafe { capi::dec_ref(items) };
            post = Some(PostData::Multipart(fields));
        } else {
            unsafe {
                raise(
                    capi::PyExc_ValueError,
                    "postData argument should be a dict or string",
                )
            };
            return std::ptr::null_mut();
        }
    }

    // The method wall (`:274-307`); no keyword is `Method::GET`'s default.
    let chosen = if method.is_null() {
        Method::Get
    } else {
        // SAFETY: the parser wrote a NUL-terminated string from `method`.
        let text = unsafe { CStr::from_ptr(method) }
            .to_string_lossy()
            .into_owned();
        match text.as_str() {
            "GET" => Method::Get,
            "PUT" => Method::Put,
            "POST" => Method::Post,
            "DELETE" => Method::Delete,
            "PATCH" => Method::Patch,
            _ => {
                // `PyErr_Format` with the reference's sentence, quote for
                // quote (`:305-306`).
                unsafe {
                    capi::PyErr_Format(
                        capi::PyExc_ValueError,
                        c"Invalid method %s. Should be 'GET', 'PUT', 'POST', \
                          'DELETE' or 'PATCH''"
                            .as_ptr(),
                        method,
                    );
                }
                return std::ptr::null_mut();
            }
        }
    };

    // SAFETY: the callback was validated live above.
    let request = RequestData {
        url: unsafe {
            // SAFETY: the parser wrote a NUL-terminated string from `url`.
            CStr::from_ptr(url).to_string_lossy().into_owned()
        },
        method: chosen,
        headers: header_list,
        timeout,
        post_data: post,
    };
    unsafe { dispatch(request, callback) };

    // `Py_RETURN_NONE`.
    capi::py_none()
}

/// `Manager::fetchURL` (`manager.cpp:156-181`): allocate the id, hold one
/// reference of the callback for the delivery, and hand the request to a
/// worker thread -- the reference's `new Request` + curl handle, with the
/// thread in place of the easy handle.
///
/// # Safety
///
/// The interpreter holds the GIL and `callback` is a live object.
unsafe fn dispatch(request: RequestData, callback: *mut capi::PyObject) {
    let (id, sender) = {
        let mut slot = MANAGER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(manager) = slot.as_mut() else {
            return;
        };
        let id = manager.next_id;
        manager.next_id += 1;
        // The held callback: one reference of the callable the engine
        // keeps until `onDone` (`py_urlrequest.cpp:78-98`).
        // SAFETY: the callback is live.
        unsafe { capi::inc_ref(callback) };
        manager
            .pending
            .insert(id, PyRef::from_owned(callback).expect("a live callback"));
        (id, manager.sender.clone())
    };
    thread::spawn(move || {
        let outcome = perform(&request);
        // The receiver can be gone -- `fini` ran while the exchange was in
        // flight; the reference's request would complete onto a fini'd
        // manager and drop silently.
        let _ = sender.send(Completion {
            id,
            content_type: outcome.content_type,
            body: outcome.body,
            response_code: outcome.response_code,
            error: outcome.error,
        });
    });
}

/// The `checkHandles`/`processEvents` pump the app's frequent task drove
/// (`manager.cpp:52-99`, `:183-221`): drain every completed request and run
/// its callback on the script thread under the GIL -- `Response::onDone`
/// (`py_urlrequest.cpp:105-129`). The host app calls this once per frame,
/// the way the reference's dispatcher did.
///
/// # Safety
///
/// The interpreter must be up and the calling thread must own the GIL.
pub unsafe fn tick() {
    let finished: Vec<Completion> = {
        let mut slot = MANAGER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(manager) = slot.as_mut() else {
            return;
        };
        // A nonblocking drain: the callbacks re-enter `fetchURL` below, so
        // the lock is released across every delivery.
        manager.receiver.try_iter().collect()
    };
    for completion in finished {
        let callback = {
            let mut slot = MANAGER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(manager) = slot.as_mut() else {
                break;
            };
            manager.pending.remove(&completion.id)
        };
        let Some(callback) = callback else {
            // The request was dropped with `fini`; nothing to call.
            continue;
        };
        unsafe { deliver(&completion, &callback) };
    }
}

/// `Response::onDone` (`py_urlrequest.cpp:105-129`): print the transport
/// error, build the `PyURLResponse`, call the callback with it, and print a
/// failing callback instead of raising.
///
/// # Safety
///
/// The interpreter must be up and the calling thread must own the GIL.
unsafe fn deliver(completion: &Completion, callback: &PyRef) {
    if let Some(error) = &completion.error {
        // `SCRIPT_ERROR_MSG("Response::onDone: %s\n", error)`: an engine
        // log line, not a script exception. The port's log sink stands in
        // for the debug-message system.
        output_writer::handle_write(&format!("Response::onDone: {error}\n"), true);
    }
    let response = response_object(
        &completion.content_type,
        &completion.body,
        completion.response_code,
    );
    // `PyObject_CallFunctionObjArgs(pCallback, pPyResponse, NULL)`: a
    // one-argument call.
    // SAFETY: the tuple adopts the response handle.
    let arguments = unsafe {
        let tuple = capi::PyTuple_New(1);
        capi::PyTuple_SetItem(tuple, 0, crate::math::own_raw(response));
        tuple
    };
    // SAFETY: the callback is a live callable and the tuple is fresh.
    let result = unsafe { capi::PyObject_CallObject(callback.as_ptr(), arguments) };
    // SAFETY: the tuple is owned by this call only.
    unsafe { capi::dec_ref(arguments) };
    if result.is_null() {
        // `PyErr_Print()` (`:125`): the callback's exception is printed,
        // not raised.
        unsafe {
            capi::PyErr_PrintEx(1);
            capi::clear_error();
        }
    } else {
        // SAFETY: a fresh owned reference.
        unsafe { capi::dec_ref(result) };
    }
}

/// `new PyURLResponse(...)` (`py_urlrequest.cpp:29-38`): one instance of
/// the engine-built type carrying the three fields. The type has no
/// script-visible constructor -- the reference's never is a module
/// attribute either.
fn response_object(content_type: &str, body_text: &str, response_code: i32) -> PyRef {
    let address = RESPONSE_TYPE.load(Ordering::SeqCst);
    let type_object = PyRef::borrowed(address as *mut capi::PyObject);
    // SAFETY: `install` built the type and the interpreter is up on the
    // calling thread.
    let instance = unsafe { binding::instantiate(&type_object) }
        .expect("the PyURLResponse allocation cannot fail");
    // SAFETY: the instance is the fresh allocation this call owns.
    unsafe {
        let fields = &mut *body::<ResponseBody>(instance.as_ptr());
        fields.content_type = content_type.to_owned();
        fields.body = body_text.to_owned();
        fields.response_code = response_code;
    }
    instance
}

/// The `PyURLResponse` instance body: the three read-only fields the
/// callback reads (`py_urlrequest.cpp:27-51`).
struct ResponseBody {
    content_type: String,
    body: String,
    response_code: i32,
}

/// The `PyURLResponse` read-only `contentType` getter.
unsafe extern "C" fn get_content_type(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    // SAFETY: the instance is one of this module's with a live body.
    let text = unsafe { &(*body::<ResponseBody>(self_)).content_type };
    // SAFETY: the bytes are a live Rust buffer copied by the C API.
    unsafe { capi::unicode_from_bytes(text.as_bytes()) }
}

/// The `PyURLResponse` read-only `body` getter.
unsafe extern "C" fn get_body(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    // SAFETY: the instance is one of this module's with a live body.
    let text = unsafe { &(*body::<ResponseBody>(self_)).body };
    // SAFETY: the bytes are a live Rust buffer copied by the C API.
    unsafe { capi::unicode_from_bytes(text.as_bytes()) }
}

/// The `PyURLResponse` read-only `responseCode` getter.
unsafe extern "C" fn get_response_code(
    self_: *mut capi::PyObject,
    _closure: *mut c_void,
) -> *mut capi::PyObject {
    // SAFETY: the instance is one of this module's with a live body.
    let code = unsafe { (*body::<ResponseBody>(self_)).response_code };
    // SAFETY: the interpreter is up and the calling thread owns the GIL.
    unsafe { capi::PyLong_FromLong(code as std::os::raw::c_long) }
}

/// The teardown handlers, keyed on the type object's address.
static TEARDOWNS: OnceLock<Mutex<BTreeMap<usize, Teardown>>> = OnceLock::new();

/// One instance teardown: drops the Rust body the instance carries.
type Teardown = unsafe fn(*mut capi::PyObject);

fn teardowns() -> &'static Mutex<BTreeMap<usize, Teardown>> {
    TEARDOWNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The `tp_dealloc` of the body-carrying type: run the registered
/// teardown, release the memory block, and drop the heap type's own
/// reference -- the three steps the inherited heap-type dealloc does.
///
/// # Safety
///
/// A C `tp_dealloc` call: the interpreter holds the GIL and `object` has a
/// zero reference count.
unsafe extern "C" fn native_dealloc(object: *mut capi::PyObject) {
    let type_ = unsafe { capi::Py_TYPE(object) };
    let teardown = teardowns()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(type_ as usize))
        .copied();
    if let Some(teardown) = teardown {
        // SAFETY: the teardown is registered for exactly this type.
        unsafe { teardown(object) };
    }
    // The spec types are not garbage collected, so the allocation is freed
    // with `PyObject_Free`.
    unsafe { capi::PyObject_Free(object.cast()) };
    // SAFETY: the type object is a heap type; the instance held one
    // reference of it.
    unsafe { capi::dec_ref(type_.cast()) };
}

/// The `drop_in_place` step for the one body type.
unsafe fn drop_body_fields(object: *mut capi::PyObject) {
    // SAFETY: the dealloc runs exactly once per instance and the caller
    // guarantees the instance shape.
    unsafe { std::ptr::drop_in_place(body::<ResponseBody>(object)) };
}

/// Attach the body-carrying dealloc to a spec: the final builder step for
/// the one type whose instances own Rust state.
fn with_dealloc(spec: TypeSpec) -> TypeSpec {
    spec.slot(
        capi::Py_tp_dealloc,
        native_dealloc as *mut std::os::raw::c_void,
    )
}

/// Build the `PyURLResponse` type and link `fetchURL` onto the `BigWorld`
/// module (`PY_MODULE_FUNCTION_WITH_KEYWORDS_WITH_DOC`,
/// `py_urlrequest.cpp:318-319`): the port's stand-in for the static link
/// table `PyURLRequest_token` registers.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and `module`
/// must be the live `BigWorld` module object.
pub unsafe fn install(module: *mut capi::PyObject) {
    // SAFETY: the interpreter is up and this thread owns the GIL.
    unsafe {
        let spec = with_dealloc(
            TypeSpec::new(RESPONSE_NAME)
                .instance_size(std::mem::size_of::<ResponseBody>())
                .attribute("contentType", get_content_type, None)
                .attribute("body", get_body, None)
                .attribute("responseCode", get_response_code, None),
        );
        let response =
            binding::create_type(&spec).expect("PyURLResponse: the type could not be created");
        // The registry holds the type alive; the address is what delivery
        // reads back. Not a module attribute -- the reference's never is.
        let address = response.as_ptr() as usize;
        RESPONSE_TYPE.store(address, Ordering::SeqCst);
        teardowns()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(address, drop_body_fields);

        // The fetchURL method: `METH_VARARGS | METH_KEYWORDS`, stored in
        // the `ml_meth` slot the interpreter calls with three arguments.
        let fetch = capi::PyMethodDef {
            ml_name: c"fetchURL".as_ptr(),
            ml_meth: Some(std::mem::transmute::<
                capi::PyCFunctionWithKeywords,
                capi::PyCFunction,
            >(method_fetch_url)),
            ml_flags: capi::METH_VARARGS | capi::METH_KEYWORDS,
            ml_doc: c"This method asynchronously fetches a URL".as_ptr(),
        };
        let methods: &'static mut [capi::PyMethodDef] =
            Box::leak(vec![fetch, capi::PyMethodDef::END].into_boxed_slice());
        let added = capi::PyModule_AddFunctions(module, methods.as_ptr());
        if added != 0 {
            // The reference logs through `ScriptErrorPrint` and moves on.
            capi::PyErr_PrintEx(0);
            capi::clear_error();
        }
    }
}

// ---------------------------------------------------------------------------
// The transport: one blocking HTTP/1.1 exchange over TCP.
// ---------------------------------------------------------------------------

/// The parts of `scheme://host[:port]/path` the exchange needs.
struct Target {
    host: String,
    port: u16,
    path: String,
}

/// Split the URL the way curl would: refuse anything that is not plain
/// `http://` -- the TLS difference this port keeps visible -- and default
/// the port to 80 and the path to `/`.
fn parse_url(url: &str) -> Result<Target, String> {
    let after_scheme = match url.strip_prefix("http://") {
        Some(rest) => rest,
        None => {
            // The reference's curl understands `https`; the port's TCP
            // transport does not, so it answers the same sentence curl
            // gives for any unsupported scheme.
            return Err("Unsupported protocol".to_owned());
        }
    };
    let (authority, path) = match after_scheme.find('/') {
        Some(slash) => (&after_scheme[..slash], &after_scheme[slash..]),
        None => (after_scheme, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, digits))
            if !host.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (
                host.to_owned(),
                digits
                    .parse()
                    .map_err(|_| "URL using bad/illegal format or missing URL".to_owned())?,
            )
        }
        _ if !authority.is_empty() => (authority.to_owned(), 80),
        _ => return Err("URL using bad/illegal format or missing URL".to_owned()),
    };
    Ok(Target {
        host,
        port,
        path: path.to_owned(),
    })
}

/// The timeout as a `Duration`; zero is no limit, the curl rule
/// (`CURLOPT_TIMEOUT_MS` of 0), rendered here as a generous cap so a
/// stuck socket cannot pin a thread forever.
fn limit(timeout: f32) -> Duration {
    if timeout <= 0.0 {
        Duration::from_secs(600)
    } else {
        Duration::from_secs_f64(f64::from(timeout).min(600.0))
    }
}

/// One blocking exchange: connect with the timeout as the connect
/// timeout, write the request, read to the end, parse the status line and
/// the `Content-Type` header. The curl error sentences come from
/// `curl_easy_strerror` (`request.cpp:355-361`).
fn perform(request: &RequestData) -> Outcome {
    let mut outcome = Outcome {
        content_type: String::new(),
        body: String::new(),
        response_code: 0,
        error: None,
    };
    let target = match parse_url(&request.url) {
        Ok(target) => target,
        Err(sentence) => {
            outcome.error = Some(sentence);
            return outcome;
        }
    };
    let timeout = limit(request.timeout);
    let mut addresses = match (target.host.as_str(), target.port).to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(_) => {
            outcome.error = Some("Couldn't resolve host name".to_owned());
            return outcome;
        }
    };
    let address = match addresses.next() {
        Some(address) => address,
        None => {
            outcome.error = Some("Couldn't resolve host name".to_owned());
            return outcome;
        }
    };
    let mut stream = match TcpStream::connect_timeout(&address, timeout) {
        Ok(stream) => stream,
        Err(error) => {
            outcome.error = Some(connect_sentence(&error));
            return outcome;
        }
    };
    let bytes = wire_request(&target, request);
    let _ = stream.set_write_timeout(Some(timeout));
    if let Err(error) = stream.write_all(&bytes) {
        outcome.error = Some(send_sentence(&error));
        return outcome;
    }
    let _ = stream.set_read_timeout(Some(timeout));
    let mut raw = Vec::new();
    let read_error = match stream.read_to_end(&mut raw) {
        Ok(_) => None,
        Err(error) => Some(receive_sentence(&error)),
    };
    parse_response(&raw, &mut outcome);
    if let Some(sentence) = read_error {
        outcome.error = Some(sentence);
    }
    outcome
}

/// The `curl_easy_strerror` sentence of a connect failure: a refusal is
/// `CURLE_COULDNT_CONNECT`, an expiry is the timeout
/// (`CURLE_OPERATION_TIMEDOUT`).
fn connect_sentence(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            "Timeout was reached".to_owned()
        }
        _ => "Couldn't connect to server".to_owned(),
    }
}

/// The `curl_easy_strerror` sentence of a send failure.
fn send_sentence(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            "Timeout was reached".to_owned()
        }
        _ => "Failure sending data to the peer".to_owned(),
    }
}

/// The `curl_easy_strerror` sentence of a receive failure.
fn receive_sentence(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            "Timeout was reached".to_owned()
        }
        _ => "Failure when receiving data from the peer".to_owned(),
    }
}

/// Build the request bytes: the verb line, the `Host` header curl adds,
/// the caller's header strings verbatim, `Connection: close` (the port
/// closes after one exchange where curl keeps the handle), and the body
/// with its length -- `CURLOPT_POSTFIELDS`' plain bytes, or the
/// `multipart/form-data` encoding of `curl_formadd`
/// (`request.cpp:332-353`).
fn wire_request(target: &Target, request: &RequestData) -> Vec<u8> {
    let mut head = String::new();
    head.push_str(&format!(
        "{} {} HTTP/1.1\r\n",
        request.method.as_str(),
        target.path
    ));
    head.push_str(&format!("Host: {}\r\n", target.host));
    for header in &request.headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    let body: Vec<u8> = match &request.post_data {
        None => Vec::new(),
        Some(PostData::Raw(raw)) => {
            // curl sends raw POST fields with a form-urlencoded default
            // content type unless the caller set one.
            head.push_str("Content-Type: application/x-www-form-urlencoded\r\n");
            head.push_str(&format!("Content-Length: {}\r\n", raw.len()));
            raw.as_bytes().to_vec()
        }
        Some(PostData::Multipart(fields)) => {
            // The `multipart/form-data` body `curl_formadd` builds.
            let boundary = "----BigWorldFormBoundary7dW9A2bQ3x";
            let mut encoded = Vec::new();
            for (name, value) in fields {
                encoded.extend_from_slice(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                        .as_bytes(),
                );
            }
            encoded.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            head.push_str(&format!(
                "Content-Type: multipart/form-data; boundary={boundary}\r\n"
            ));
            head.push_str(&format!("Content-Length: {}\r\n", encoded.len()));
            encoded
        }
    };
    head.push_str("Connection: close\r\n\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(&body);
    bytes
}

/// Read the exchange's reply: the status line's code, the `Content-Type`
/// header value, then everything after the blank line as the body -- the
/// `CURLINFO_RESPONSE_CODE` and header callback reads of the reference.
fn parse_response(raw: &[u8], outcome: &mut Outcome) {
    let Some(gap) = find_header_gap(raw) else {
        return;
    };
    let head = String::from_utf8_lossy(&raw[..gap]);
    let mut lines = head.split("\r\n");
    let Some(status) = lines.next() else {
        return;
    };
    let mut parts = status.split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        return;
    }
    if let Some(code) = parts.next().and_then(|value| value.parse::<i32>().ok()) {
        outcome.response_code = code;
    }
    for line in lines {
        if let Some(value) = strip_content_type(line) {
            outcome.content_type = value;
        }
    }
    outcome.body = String::from_utf8_lossy(&raw[gap + 4..]).into_owned();
}

/// The byte offset just past the blank line that ends the header block.
fn find_header_gap(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

/// The `Content-Type` header value, matched case-insensitively the way
/// curl's header callback does.
fn strip_content_type(line: &str) -> Option<String> {
    let (name, value) = line.split_once(':')?;
    if name.trim().eq_ignore_ascii_case("content-type") {
        Some(value.trim().to_owned())
    } else {
        None
    }
}
