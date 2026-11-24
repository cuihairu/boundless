//! The Python billing bridge: a script object answers login attempts.
//!
//! Reference: `server/dbapp/py_billing_system.{hpp,cpp}`. The database asks
//! a script object to resolve `getEntityKeyForAccount`/`setEntityKeyForAccount`;
//! the script answers by calling one method on the [`PyBillingResponse`]
//! object the fourth argument carries, and every callback reaches the host
//! through the handler traits this module declares, standing in for the
//! reference's `IGetEntityKeyForAccountHandler`/`ISetEntityKeyForAccountHandler`
//! (`lib/db_storage/billing_system.hpp`).
//!
//! Layering: this crate stays dependency-free, so the reference's
//! `EntityDefs` slice crosses as the [`EntityTypeDefs`] trait and the
//! `Mercury::Address` argument as its rendered string, the way the
//! reference's `Script::getData` builds one.

use std::cell::RefCell;
use std::ffi::c_int;
use std::rc::Rc;

use crate::binding::{self, BindError};
use crate::capi;
use crate::pyref::PyRef;

/// `LogOnStatus::NOT_SET` (`lib/connection/log_on_status.hpp`): the one
/// okay status that is not a success.
pub const LOG_ON_NOT_SET: i32 = 0;

/// `LogOnStatus::LOGGED_ON`.
pub const LOGGED_ON: i32 = 1;

/// `LogOnStatus::LOGIN_REJECTED_NO_SUCH_USER`.
pub const LOGIN_REJECTED_NO_SUCH_USER: i32 = 67;

/// `LogOnStatus::LOGIN_REJECTED_INVALID_PASSWORD`.
pub const LOGIN_REJECTED_INVALID_PASSWORD: i32 = 68;

/// `LogOnStatus::LOGIN_REJECTED_DB_GENERAL_FAILURE`.
pub const LOGIN_REJECTED_DB_GENERAL_FAILURE: i32 = 71;

/// The slice of `EntityDefs` the bridge reads: the type id behind a script
/// name and the canonical name behind an id (`db_entitydefs.hpp`'s
/// `getEntityType` and `getEntityDescription(...).name()`).
pub trait EntityTypeDefs {
    /// `EntityDefs::getEntityType`: the id, or `None` when the name is
    /// unknown, the reference's `INVALID_ENTITY_TYPE_ID`.
    fn type_id(&self, name: &str) -> Option<u16>;

    /// `EntityDefs::getEntityDescription(id).name()`.
    fn type_name(&self, entity_type: u16) -> String;
}

/// `IGetEntityKeyForAccountHandler`: exactly one of the four callbacks
/// fires per login attempt, because the response clears its handler after
/// the first one.
pub trait GetEntityKeyForAccountHandler {
    /// `onGetEntityKeyForAccountSuccess`.
    fn on_success(
        &mut self,
        entity_type: u16,
        db_id: i64,
        data_for_client: &str,
        data_for_base_entity: &str,
    );

    /// `onGetEntityKeyForAccountLoadFromUsername`.
    fn on_load_from_username(
        &mut self,
        entity_type: u16,
        entity_name: &str,
        should_create_unknown: bool,
        should_remember_unknown: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    );

    /// `onGetEntityKeyForAccountCreateNew`.
    fn on_create_new(
        &mut self,
        entity_type: u16,
        should_remember: bool,
        data_for_client: &str,
        data_for_base_entity: &str,
    );

    /// `onGetEntityKeyForAccountFailure`.
    fn on_failure(&mut self, status: i32, error_msg: &str);
}

/// `ISetEntityKeyForAccountHandler`.
pub trait SetEntityKeyForAccountHandler {
    /// `onSetEntityKeyForAccountSuccess`.
    fn on_success(&mut self, entity_type: u16, db_id: i64);

    /// `onSetEntityKeyForAccountFailure`.
    fn on_failure(&mut self, entity_type: u16, db_id: i64);
}

/// The handler one response answers into, shared so the destructor can
/// still report after the host-side call has returned: the reference holds
/// a bare pointer that only stays valid because scripts answer
/// synchronously, and the shared handle removes that window.
type Handler = Rc<RefCell<dyn GetEntityKeyForAccountHandler>>;

/// The dotted type name the response carries.
const RESPONSE_TYPE_NAME: &str = "boundless.PyBillingResponse";

/// The state one live response carries: the handler the four callbacks
/// reach while it is still `Some` (`pHandler_`), and the entity-defs slice
/// the type lookups read (`entityDefs_`).
struct ResponseState {
    handler: Option<Handler>,
    defs: Rc<dyn EntityTypeDefs>,
}

/// The instance body the response type allocates.
#[repr(C)]
struct ResponseBody {
    header: capi::PyObject,
    state: *mut ResponseState,
}

fn spec() -> binding::TypeSpec {
    binding::TypeSpec::new(RESPONSE_TYPE_NAME)
        .method("loadEntity", load_entity)
        .method("loadEntityByName", load_entity_by_name)
        .method("createNewEntity", create_new_entity)
        .method("failureInvalidPassword", failure_invalid_password)
        .method("failureNoSuchUser", failure_no_such_user)
        .method("failureDBError", failure_db_error)
        .method("failure", failure)
        .instance_size(std::mem::size_of::<ResponseBody>() - std::mem::size_of::<capi::PyObject>())
        .dealloc(response_dealloc)
}

/// The state an instance body carries, or null for an object that is not
/// one of this module's responses.
///
/// # Safety
///
/// `object` must be one of this module's instances, whose body is a
/// `ResponseBody`.
unsafe fn state_of(object: *mut capi::PyObject) -> *mut ResponseState {
    // SAFETY: the caller holds an instance of this module's type.
    unsafe { (*object.cast::<ResponseBody>()).state }
}

/// The `RETOK` success return: one new reference of `None`, the shape
/// `PY_AUTO_METHOD_DECLARE(RETOK, ...)` produces.
fn ret_none() -> *mut capi::PyObject {
    // SAFETY: the singleton lives as long as the process.
    unsafe { capi::inc_ref(capi::py_none()) };
    capi::py_none()
}

/// Raise `ValueError` with a static message. The methods only run while a
/// script calls into them, which is when `PyExc_ValueError` exists.
fn raise_value_error(message: &std::ffi::CStr) {
    // SAFETY: the caller runs under a live interpreter.
    unsafe { capi::PyErr_SetString(capi::PyExc_ValueError, message.as_ptr()) };
}

/// `PyErr_Format(PyExc_ValueError, "Invalid entity type '%s'\n", ...)` --
/// the message keeps the reference's trailing newline
/// (`py_billing_system.cpp:305-307`).
fn raise_invalid_entity_type(entity_type: &str) {
    let Ok(name) = std::ffi::CString::new(entity_type) else {
        return;
    };
    // SAFETY: the caller runs under a live interpreter and `name` is a
    // NUL-terminated buffer for the call's duration.
    unsafe {
        capi::PyErr_Format(
            capi::PyExc_ValueError,
            c"Invalid entity type '%s'\n".as_ptr(),
            name.as_ptr(),
        );
    }
}

/// `PyErr_Format(PyExc_ValueError, "Invalid failure status %d", ...)`
/// (`py_billing_system.cpp:287-289`).
fn raise_invalid_failure_status(status: c_int) {
    // SAFETY: the caller runs under a live interpreter.
    unsafe {
        capi::PyErr_Format(
            capi::PyExc_ValueError,
            c"Invalid failure status %d".as_ptr(),
            status,
        );
    }
}

/// A parsed `s` argument as a Rust string; an absent optional (left null)
/// reads as empty, the reference's `BW::string()` default.
///
/// # Safety
///
/// `pointer` must be a buffer `PyArg_ParseTuple`'s `s` handed out, which is
/// NUL-terminated and alive for the call.
unsafe fn parsed_string(pointer: *const std::os::raw::c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: the `s` conversion hands out a NUL-terminated buffer it keeps
    // alive through the call; embedded NULs are refused by the conversion.
    unsafe {
        std::ffi::CStr::from_ptr(pointer)
            .to_string_lossy()
            .into_owned()
    }
}

/// A Rust string as an owned `str` object; a failed allocation folds to the
/// empty handle so the tuple paths stay uniform.
fn owned_string(text: &str) -> PyRef {
    // SAFETY: the bytes stay alive for the call and the API copies them.
    unsafe { PyRef::from_owned(capi::unicode_from_bytes(text.as_bytes())).unwrap_or_default() }
}

/// Take the handler out -- `pHandler_ = nullptr` once taken. `None` is the
/// already-sent state.
fn take_handler(state: *mut ResponseState) -> Option<Handler> {
    // SAFETY: `state` is the state pointer the host handed `wrap_response`.
    unsafe { (*state).handler.take() }
}

/// Deliver one callback to a taken handler. `false` puts it back: the send
/// did not land, so the response is still expected. A re-entrant borrow is
/// the only way this happens, and the reference degrades the same way when
/// a send cannot proceed -- it clears the pending exception
/// (`py_billing_system.cpp:44-47`).
fn deliver(
    state: *mut ResponseState,
    handler: Handler,
    emit: impl FnOnce(&mut dyn GetEntityKeyForAccountHandler),
) -> bool {
    let Ok(mut handler) = handler.try_borrow_mut() else {
        // SAFETY: `state` is the state pointer the host handed `wrap_response`.
        unsafe { (*state).handler = Some(handler) };
        return false;
    };
    emit(&mut *handler);
    true
}

/// `getEntityTypeID` (`py_billing_system.cpp:297-312`): the already-sent
/// refusal, then the entity-defs lookup with its invalid-type refusal.
fn get_entity_type_id(state: *mut ResponseState, entity_type: &str) -> Option<u16> {
    // SAFETY: `state` is the state pointer the host handed `wrap_response`.
    let state = unsafe { &mut *state };
    if state.handler.is_none() {
        raise_value_error(c"Response already sent");
        return None;
    }
    match state.defs.type_id(entity_type) {
        Some(type_id) => Some(type_id),
        None => {
            raise_invalid_entity_type(entity_type);
            None
        }
    }
}

/// `sendFailure` (`py_billing_system.cpp:320-331`): the already-sent
/// refusal, the one failure callback, and the clear.
fn send_failure(state: *mut ResponseState, status: i32, error_msg: &str) -> bool {
    let Some(handler) = take_handler(state) else {
        raise_value_error(c"Response already sent");
        return false;
    };
    deliver(state, handler, |handler| {
        handler.on_failure(status, error_msg)
    })
}

/// `loadEntity` (`py_billing_system.cpp:216-240`).
unsafe extern "C" fn load_entity(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(self_) };
    let mut entity_type: *const std::os::raw::c_char = std::ptr::null();
    let mut db_id: i64 = 0;
    let mut data_for_client: *const std::os::raw::c_char = std::ptr::null();
    let mut data_for_base_entity: *const std::os::raw::c_char = std::ptr::null();
    // SAFETY: `args` is the tuple the interpreter handed the method and the
    // out-pointers live through the call.
    let parsed = unsafe {
        capi::PyArg_ParseTuple(
            args,
            c"sL|ss".as_ptr(),
            &mut entity_type,
            &mut db_id,
            &mut data_for_client,
            &mut data_for_base_entity,
        )
    };
    if state.is_null() || parsed == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the parsed `s` buffers live through the call.
    let (entity_type, data_for_client, data_for_base_entity) = unsafe {
        (
            parsed_string(entity_type),
            parsed_string(data_for_client),
            parsed_string(data_for_base_entity),
        )
    };
    if db_id == 0 {
        // The id is refused before the type lookup
        // (`py_billing_system.cpp:218-223`).
        raise_value_error(c"0 is an invalid database id");
        return std::ptr::null_mut();
    }
    let Some(type_id) = get_entity_type_id(state, &entity_type) else {
        return std::ptr::null_mut();
    };
    if let Some(handler) = take_handler(state) {
        deliver(state, handler, |handler| {
            handler.on_success(type_id, db_id, &data_for_client, &data_for_base_entity);
        });
    }
    ret_none()
}

/// `loadEntityByName` (`py_billing_system.cpp:242-262`).
unsafe extern "C" fn load_entity_by_name(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(self_) };
    let mut entity_type: *const std::os::raw::c_char = std::ptr::null();
    let mut entity_name: *const std::os::raw::c_char = std::ptr::null();
    let mut should_create_unknown: u8 = 0;
    let mut should_remember_unknown: u8 = 0;
    let mut data_for_client: *const std::os::raw::c_char = std::ptr::null();
    let mut data_for_base_entity: *const std::os::raw::c_char = std::ptr::null();
    // SAFETY: `args` is the tuple the interpreter handed the method and the
    // out-pointers live through the call.
    let parsed = unsafe {
        capi::PyArg_ParseTuple(
            args,
            c"ssb|bss".as_ptr(),
            &mut entity_type,
            &mut entity_name,
            &mut should_create_unknown,
            &mut should_remember_unknown,
            &mut data_for_client,
            &mut data_for_base_entity,
        )
    };
    if state.is_null() || parsed == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the parsed `s` buffers live through the call.
    let (entity_type, entity_name, data_for_client, data_for_base_entity) = unsafe {
        (
            parsed_string(entity_type),
            parsed_string(entity_name),
            parsed_string(data_for_client),
            parsed_string(data_for_base_entity),
        )
    };
    let Some(type_id) = get_entity_type_id(state, &entity_type) else {
        return std::ptr::null_mut();
    };
    if let Some(handler) = take_handler(state) {
        deliver(state, handler, |handler| {
            handler.on_load_from_username(
                type_id,
                &entity_name,
                should_create_unknown != 0,
                should_remember_unknown != 0,
                &data_for_client,
                &data_for_base_entity,
            );
        });
    }
    ret_none()
}

/// `createNewEntity` (`py_billing_system.cpp:264-282`).
unsafe extern "C" fn create_new_entity(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(self_) };
    let mut entity_type: *const std::os::raw::c_char = std::ptr::null();
    let mut should_remember: u8 = 0;
    let mut data_for_client: *const std::os::raw::c_char = std::ptr::null();
    let mut data_for_base_entity: *const std::os::raw::c_char = std::ptr::null();
    // SAFETY: `args` is the tuple the interpreter handed the method and the
    // out-pointers live through the call.
    let parsed = unsafe {
        capi::PyArg_ParseTuple(
            args,
            c"sb|ss".as_ptr(),
            &mut entity_type,
            &mut should_remember,
            &mut data_for_client,
            &mut data_for_base_entity,
        )
    };
    if state.is_null() || parsed == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the parsed `s` buffers live through the call.
    let (entity_type, data_for_client, data_for_base_entity) = unsafe {
        (
            parsed_string(entity_type),
            parsed_string(data_for_client),
            parsed_string(data_for_base_entity),
        )
    };
    let Some(type_id) = get_entity_type_id(state, &entity_type) else {
        return std::ptr::null_mut();
    };
    if let Some(handler) = take_handler(state) {
        deliver(state, handler, |handler| {
            handler.on_create_new(
                type_id,
                should_remember != 0,
                &data_for_client,
                &data_for_base_entity,
            );
        });
    }
    ret_none()
}

/// `failureInvalidPassword` (`py_billing_system.cpp:270`-`272` of the
/// wrappers, `py_billing_system.cpp:314-317`).
unsafe extern "C" fn failure_invalid_password(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: `self_` is one of this module's instances.
    let (state, error_msg) = match unsafe { parse_failure_args(self_, args) } {
        Some(parsed) => parsed,
        None => return std::ptr::null_mut(),
    };
    if send_failure(state, LOGIN_REJECTED_INVALID_PASSWORD, &error_msg) {
        ret_none()
    } else {
        std::ptr::null_mut()
    }
}

/// `failureNoSuchUser`.
unsafe extern "C" fn failure_no_such_user(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: `self_` is one of this module's instances.
    let (state, error_msg) = match unsafe { parse_failure_args(self_, args) } {
        Some(parsed) => parsed,
        None => return std::ptr::null_mut(),
    };
    if send_failure(state, LOGIN_REJECTED_NO_SUCH_USER, &error_msg) {
        ret_none()
    } else {
        std::ptr::null_mut()
    }
}

/// `failureDBError`.
unsafe extern "C" fn failure_db_error(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: `self_` is one of this module's instances.
    let (state, error_msg) = match unsafe { parse_failure_args(self_, args) } {
        Some(parsed) => parsed,
        None => return std::ptr::null_mut(),
    };
    if send_failure(state, LOGIN_REJECTED_DB_GENERAL_FAILURE, &error_msg) {
        ret_none()
    } else {
        std::ptr::null_mut()
    }
}

/// The `|s` parse the three named failure methods share.
///
/// # Safety
///
/// `self_` must be one of this module's instances and `args` the tuple the
/// interpreter handed the method.
unsafe fn parse_failure_args(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> Option<(*mut ResponseState, String)> {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(self_) };
    let mut error_msg: *const std::os::raw::c_char = std::ptr::null();
    // SAFETY: the out-pointer lives through the call.
    let parsed = unsafe { capi::PyArg_ParseTuple(args, c"|s".as_ptr(), &mut error_msg) };
    if state.is_null() || parsed == 0 {
        return None;
    }
    // SAFETY: the parsed `s` buffer lives through the call.
    Some((state, unsafe { parsed_string(error_msg) }))
}

/// `failure` (`py_billing_system.cpp:283-292`): the caller's numeric
/// status, refused when it is not a failure.
unsafe extern "C" fn failure(
    self_: *mut capi::PyObject,
    args: *mut capi::PyObject,
) -> *mut capi::PyObject {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(self_) };
    let mut status: c_int = 0;
    let mut error_msg: *const std::os::raw::c_char = std::ptr::null();
    // SAFETY: `args` is the tuple the interpreter handed the method and the
    // out-pointers live through the call.
    let parsed =
        unsafe { capi::PyArg_ParseTuple(args, c"i|s".as_ptr(), &mut status, &mut error_msg) };
    if state.is_null() || parsed == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the parsed `s` buffer lives through the call.
    let error_msg = unsafe { parsed_string(error_msg) };
    // The status must be one that is not okay: `okay()` is `NOT_SET` or
    // `LOGGED_ON` (0/1). The reference's second clause,
    // `statusAsInt != status.value()`, is vacuous on the port -- the enum
    // round trip of an int is the int (`py_billing_system.cpp:286`).
    if status == LOG_ON_NOT_SET || status == LOGGED_ON {
        raise_invalid_failure_status(status);
        return std::ptr::null_mut();
    }
    if send_failure(state, status, &error_msg) {
        ret_none()
    } else {
        std::ptr::null_mut()
    }
}

/// Release the state block one instance owns: an unsent response reports
/// general failure first, the destructor's contract
/// (`py_billing_system.cpp:41-48`).
fn release_state(state: *mut ResponseState) {
    if state.is_null() {
        return;
    }
    let state = unsafe { Box::from_raw(state) };
    let Some(handler) = state.handler else {
        return;
    };
    eprintln!("PyBillingResponse::~PyBillingResponse: No response sent. Sending general failure");
    // A re-entrant borrow cannot happen here: the script no longer holds a
    // reference that could call in, so the send always lands.
    if let Ok(mut handler) = handler.try_borrow_mut() {
        handler.on_failure(LOGIN_REJECTED_DB_GENERAL_FAILURE, "No response from script");
    }
}

/// `tp_dealloc`: release the state block, then free the instance through
/// the binding's teardown -- untrack-if-garbage-collected, the type's own
/// `tp_free`, and the type reference back.
///
/// # Safety
///
/// Only ever runs on instances the response type allocated.
unsafe extern "C" fn response_dealloc(object: *mut capi::PyObject) {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { (*object.cast::<ResponseBody>()).state };
    release_state(state);
    // SAFETY: the state is released and the object is an instance the
    // response type allocated.
    unsafe { binding::free_instance(object) };
}

/// Build the one response a `getEntityKeyForAccount` call hands the script
/// (`py_billing_system.cpp:33-40`, constructed by
/// `getEntityKeyForAccount` at `py_billing_system.cpp:415`).
fn wrap_response(handler: Handler, defs: Rc<dyn EntityTypeDefs>) -> Result<PyRef, BindError> {
    let type_ = binding::create_type(&spec())?;
    let Some(response) = (unsafe { binding::instantiate(&type_) }) else {
        return Err(BindError::CreateFailed);
    };
    let state = Box::into_raw(Box::new(ResponseState {
        handler: Some(handler),
        defs,
    }));
    // SAFETY: the allocated body is exactly a `ResponseBody`, the instance
    // size `spec` asked for.
    unsafe {
        (*response.as_ptr().cast::<ResponseBody>()).state = state;
    }
    Ok(response)
}

/// `PyBillingSystem::getMember` (`py_billing_system.cpp:355-370`): the
/// member, or the empty handle with the failure cleared when the script
/// does not carry it.
fn get_member(script: &PyRef, member: &str) -> PyRef {
    let Ok(encoded) = std::ffi::CString::new(member) else {
        return PyRef::empty();
    };
    // SAFETY: the interpreter is up when a billing system is constructed.
    let found = unsafe { capi::PyObject_GetAttrString(script.as_ptr(), encoded.as_ptr()) };
    if found.is_null() {
        eprintln!("PyBillingSystem::getMember: No such method {member}");
        // SAFETY: the failed lookup left a pending exception.
        unsafe { capi::clear_error() };
        return PyRef::empty();
    }
    // A non-null result comes out owned.
    PyRef::from_owned(found).unwrap_or_default()
}

/// The script billing bridge (`PyBillingSystem`,
/// `py_billing_system.hpp:18-53`): the two members are looked up once at
/// construction, a miss logs and clears.
pub struct PyBillingSystem {
    get_entity_key_func: PyRef,
    set_entity_key_func: PyRef,
}

impl PyBillingSystem {
    /// `PyBillingSystem::PyBillingSystem` (`py_billing_system.cpp:344-352`).
    pub fn new(script: &PyRef) -> Self {
        Self {
            get_entity_key_func: get_member(script, "getEntityKeyForAccount"),
            set_entity_key_func: get_member(script, "setEntityKeyForAccount"),
        }
    }

    /// `isOkay` (`py_billing_system.hpp:33-36`): the get member's presence.
    pub fn is_okay(&self) -> bool {
        !self.get_entity_key_func.is_empty()
    }

    /// Validate a login attempt and hand the answer to the handler
    /// (`py_billing_system.cpp:358-425`). The script answers synchronously
    /// or not at all: when the call returns without a response the
    /// destructor's general failure has already gone out.
    pub fn get_entity_key_for_account<R: GetEntityKeyForAccountHandler + 'static>(
        &self,
        username: &str,
        password: &str,
        client_addr: &str,
        defs: Rc<dyn EntityTypeDefs>,
        handler: Rc<RefCell<R>>,
    ) {
        if self.get_entity_key_func.is_empty() {
            eprintln!("PyBillingSystem::getEntityKeyForAccount: No getEntityKeyForAccount method");
            if let Ok(mut handler) = handler.try_borrow_mut() {
                handler.on_failure(LOGIN_REJECTED_DB_GENERAL_FAILURE, "");
            }
            return;
        }
        let response = wrap_response(handler.clone(), defs)
            .expect("the response type builds while the interpreter is up");
        let username_object = owned_string(username);
        let password_object = owned_string(password);
        // The address crosses as the string the reference's `Script::getData`
        // builds from a `Mercury::Address`.
        let client_addr_object = owned_string(client_addr);
        let args = PyRef::from_owned(unsafe {
            // SAFETY: all four items are live objects and the call is under
            // the GIL; the tuple holds its own references to each.
            capi::PyTuple_Pack(
                4,
                username_object.as_ptr(),
                password_object.as_ptr(),
                client_addr_object.as_ptr(),
                response.as_ptr(),
            )
        })
        .expect("the argument tuple builds while the interpreter is up");
        // SAFETY: the interpreter is up and both handles are live objects.
        let result =
            unsafe { capi::PyObject_CallObject(self.get_entity_key_func.as_ptr(), args.as_ptr()) };
        if result.is_null() {
            eprintln!("PyBillingSystem::getEntityKeyForAccount: Failed. username = {username}");
            // SAFETY: the failed call left a pending exception, which the
            // print consumes.
            unsafe { capi::PyErr_PrintEx(0) };
            unsafe { capi::clear_error() };
        } else {
            // SAFETY: the result came out of the call owned.
            unsafe { capi::dec_ref(result) };
        }
        // The response handle drops with `args`: when the script kept no
        // reference, the destructor fires now and reports the unsent
        // response (`py_billing_system.cpp:416-423`).
    }

    /// Inform the billing system that a new entity has been associated
    /// with a login (`py_billing_system.cpp:427-483`).
    pub fn set_entity_key_for_account(
        &self,
        username: &str,
        password: &str,
        entity_type: u16,
        db_id: i64,
        defs: Rc<dyn EntityTypeDefs>,
        handler: Rc<RefCell<dyn SetEntityKeyForAccountHandler>>,
    ) {
        if self.set_entity_key_func.is_empty() {
            eprintln!(
                "PyBillingSystem::setEntityKeyForAccount: No setEntityKeyForAccount function exists. \
                 sendCreateNew should not be called with shouldRemember as True"
            );
            if let Ok(mut handler) = handler.try_borrow_mut() {
                handler.on_failure(entity_type, db_id);
            }
            return;
        }
        let type_name = defs.type_name(entity_type);
        let username_object = owned_string(username);
        let password_object = owned_string(password);
        let name_object = owned_string(&type_name);
        let db_id_object = PyRef::from_owned(unsafe {
            // SAFETY: the interpreter is up.
            capi::PyLong_FromLongLong(db_id)
        })
        .unwrap_or_default();
        let args = PyRef::from_owned(unsafe {
            // SAFETY: all four items are live objects and the call is under
            // the GIL; the tuple holds its own references to each.
            capi::PyTuple_Pack(
                4,
                username_object.as_ptr(),
                password_object.as_ptr(),
                name_object.as_ptr(),
                db_id_object.as_ptr(),
            )
        })
        .expect("the argument tuple builds while the interpreter is up");
        // SAFETY: the interpreter is up and both handles are live objects.
        let result =
            unsafe { capi::PyObject_CallObject(self.set_entity_key_func.as_ptr(), args.as_ptr()) };
        if result.is_null() {
            eprintln!("PyBillingSystem::setLoginMapping: Failed. username = {username}");
            if let Ok(mut handler) = handler.try_borrow_mut() {
                handler.on_failure(entity_type, db_id);
            }
            // SAFETY: the failed call left a pending exception, which the
            // print consumes.
            unsafe { capi::PyErr_Print() };
        } else {
            if let Ok(mut handler) = handler.try_borrow_mut() {
                handler.on_success(entity_type, db_id);
            }
            // SAFETY: the result came out of the call owned.
            unsafe { capi::dec_ref(result) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import_paths;
    use crate::interpreter::{InitConfig, Interpreter};
    use std::path::PathBuf;
    use std::process::Command;

    /// Every scenario, each in its own process: the interpreter refuses a
    /// second init in one process, the same rule `interpreter`'s harness
    /// follows.
    const SCENARIOS: &[&str] = &[
        "billing::tests::scenario_load_entity_delivers_the_key",
        "billing::tests::scenario_zero_database_id_raises_value_error",
        "billing::tests::scenario_an_unknown_entity_type_raises_value_error",
        "billing::tests::scenario_a_second_response_raises_value_error",
        "billing::tests::scenario_a_second_failure_raises_value_error",
        "billing::tests::scenario_the_named_failures_reach_the_handler",
        "billing::tests::scenario_failure_refuses_the_okay_statuses",
        "billing::tests::scenario_load_entity_by_name_carries_the_flags",
        "billing::tests::scenario_create_new_entity_carries_the_flag",
        "billing::tests::scenario_no_response_reports_general_failure",
        "billing::tests::scenario_a_script_error_reports_general_failure",
        "billing::tests::scenario_a_missing_get_method_reports_general_failure",
        "billing::tests::scenario_set_entity_key_crosses_the_mapping",
        "billing::tests::scenario_a_missing_set_method_fails_the_key",
        "billing::tests::scenario_a_set_script_error_fails_the_key",
        "billing::tests::scenario_a_stored_response_reports_at_teardown",
        "billing::tests::scenario_a_stored_response_still_answers",
        "billing::tests::scenario_wrong_answers_refuse_like_the_reference",
        "billing::tests::scenario_a_borrowed_handler_degrades_to_no_send",
    ];

    /// Runs `scenario` in a fresh process of this test binary and requires
    /// it to exit normally.
    fn run_scenario(scenario: &str) {
        let harness = std::env::current_exe().expect("the test binary path");
        let output = Command::new(&harness)
            .args(["--exact", scenario, "--ignored", "--test-threads=1"])
            .output()
            .expect("spawn the scenario child");
        assert!(
            output.status.success(),
            "scenario {scenario} did not exit normally: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// `Interpreter::init` around one body, then `Script::fini`.
    fn interpreter_scenario(body: impl FnOnce()) {
        let interpreter = Interpreter::init(&scenario_config()).expect("Script::init");
        body();
        interpreter.fini();
    }

    /// `Interpreter::init`'s config with the fixture's res root in the path
    /// list, so `import bw_site` has something to import.
    fn scenario_config() -> InitConfig {
        let root = temp_resource_root();
        let root_path = root.to_str().expect("a UTF-8 root").to_owned();
        let mut paths = InitConfig::default().import_paths;
        paths.add_res_path(&[&root_path], import_paths::COMMON_PATH);
        InitConfig {
            import_paths: paths,
            ..InitConfig::default()
        }
    }

    /// A throw-away resource root laid out the way the engine ships its
    /// scripts.
    fn temp_resource_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "boundless-python-host-billing-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("scripts/common/Lib")).expect("build the script tree");
        std::fs::write(
            root.join("scripts/common/bw_site.py"),
            "import sys\nsys.bw_site_imported = True\n",
        )
        .expect("write bw_site");
        root
    }

    /// Runs `source` and reports whether it ran.
    fn run_simple(source: &str) -> bool {
        // SAFETY: only called while the interpreter is up.
        unsafe {
            let Ok(encoded) = std::ffi::CString::new(source) else {
                return false;
            };
            if capi::PyRun_SimpleString(encoded.as_ptr()) != 0 {
                capi::clear_error();
                return false;
            }
            true
        }
    }

    /// The `__probe` global rendered as a string; scenario functions
    /// declare `global __probe` before assigning it.
    fn read_probe() -> String {
        // SAFETY: the interpreter is up and the module names are static.
        unsafe {
            let main = capi::PyImport_AddModule(c"__main__".as_ptr());
            let dictionary = capi::PyModule_GetDict(main);
            let value = capi::PyDict_GetItemString(dictionary, c"__probe".as_ptr());
            let text = capi::PyObject_Str(value);
            let rendered = capi::to_string(text);
            capi::dec_ref(text);
            rendered
        }
    }

    /// The `__main__` module as the script object a `PyBillingSystem` reads
    /// its members from.
    fn main_module() -> PyRef {
        // SAFETY: the interpreter is up. `PyImport_AddModule` returns a
        // borrowed reference and a `PyRef` owns one count, so the count is
        // taken here -- the `Py_XINCREF`-and-adopt of `binding`'s identity
        // cache (`binding.rs`), not an adopt of a borrowed pointer.
        unsafe {
            let module = capi::PyImport_AddModule(c"__main__".as_ptr());
            capi::inc_ref(module);
            PyRef::borrowed(module)
        }
    }

    /// Every callback the bridge can reach, recorded as text.
    #[derive(Default)]
    struct Recorded {
        entries: Vec<String>,
    }

    impl GetEntityKeyForAccountHandler for Recorded {
        fn on_success(
            &mut self,
            entity_type: u16,
            db_id: i64,
            data_for_client: &str,
            data_for_base_entity: &str,
        ) {
            self.entries.push(format!(
                "success({entity_type}, {db_id}, {data_for_client:?}, {data_for_base_entity:?})"
            ));
        }

        fn on_load_from_username(
            &mut self,
            entity_type: u16,
            entity_name: &str,
            should_create_unknown: bool,
            should_remember_unknown: bool,
            data_for_client: &str,
            data_for_base_entity: &str,
        ) {
            self.entries.push(format!(
                "load_from_username({entity_type}, {entity_name:?}, \
                 {should_create_unknown}, {should_remember_unknown}, \
                 {data_for_client:?}, {data_for_base_entity:?})"
            ));
        }

        fn on_create_new(
            &mut self,
            entity_type: u16,
            should_remember: bool,
            data_for_client: &str,
            data_for_base_entity: &str,
        ) {
            self.entries.push(format!(
                "create_new({entity_type}, {should_remember}, \
                 {data_for_client:?}, {data_for_base_entity:?})"
            ));
        }

        fn on_failure(&mut self, status: i32, error_msg: &str) {
            self.entries
                .push(format!("failure({status}, {error_msg:?})"));
        }
    }

    impl SetEntityKeyForAccountHandler for Recorded {
        fn on_success(&mut self, entity_type: u16, db_id: i64) {
            self.entries
                .push(format!("set success({entity_type}, {db_id})"));
        }

        fn on_failure(&mut self, entity_type: u16, db_id: i64) {
            self.entries
                .push(format!("set failure({entity_type}, {db_id})"));
        }
    }

    /// `Avatar` is type 7; anything else is unknown, the fixture's
    /// `EntityDefs`.
    struct FixtureDefs;

    impl EntityTypeDefs for FixtureDefs {
        fn type_id(&self, name: &str) -> Option<u16> {
            match name {
                "Avatar" => Some(7),
                _ => None,
            }
        }

        fn type_name(&self, entity_type: u16) -> String {
            match entity_type {
                7 => "Avatar".to_owned(),
                _ => String::new(),
            }
        }
    }

    fn recorded() -> Rc<RefCell<Recorded>> {
        Rc::new(RefCell::new(Recorded::default()))
    }

    fn fixture_defs() -> Rc<dyn EntityTypeDefs> {
        Rc::new(FixtureDefs)
    }

    /// One `getEntityKeyForAccount` call over a freshly defined script
    /// function, the way the reference drives the bridge per login.
    fn one_login<R: GetEntityKeyForAccountHandler + 'static>(
        source: &str,
        handler: Rc<RefCell<R>>,
    ) {
        assert!(run_simple(source), "define the script function");
        let system = PyBillingSystem::new(&main_module());
        assert!(system.is_okay());
        system.get_entity_key_for_account("user", "pass", "1.2.3.4:6001", fixture_defs(), handler);
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_load_entity_delivers_the_key() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 response.loadEntity('Avatar', 42, 'for-client', 'for-base')\n",
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["success(7, 42, \"for-client\", \"for-base\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_zero_database_id_raises_value_error() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 global __probe\n    \
                 try:\n        \
                 response.loadEntity('Avatar', 0)\n    \
                 except ValueError as error:\n        \
                 __probe = str(error)\n",
                recorder.clone(),
            );
            assert_eq!(read_probe(), "0 is an invalid database id");
            // The refusal leaves the response unsent, so the destructor
            // reports general failure (`py_billing_system.cpp:41-48`).
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(71, \"No response from script\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_an_unknown_entity_type_raises_value_error() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 global __probe\n    \
                 try:\n        \
                 response.loadEntity('Troll', 5)\n    \
                 except ValueError as error:\n        \
                 __probe = str(error)\n",
                recorder.clone(),
            );
            // The reference formats the message with a trailing newline
            // (`py_billing_system.cpp:305-307`).
            assert_eq!(read_probe(), "Invalid entity type 'Troll'\n");
            // The refusal leaves the response unsent, so the destructor
            // reports general failure (`py_billing_system.cpp:41-48`).
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(71, \"No response from script\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_second_response_raises_value_error() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 global __probe\n    \
                 response.loadEntity('Avatar', 1)\n    \
                 try:\n        \
                 response.loadEntity('Avatar', 2)\n    \
                 except ValueError as error:\n        \
                 __probe = str(error)\n",
                recorder.clone(),
            );
            assert_eq!(read_probe(), "Response already sent");
            assert_eq!(
                recorder.borrow().entries,
                vec!["success(7, 1, \"\", \"\")".to_owned()],
                "only the first response went out"
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_second_failure_raises_value_error() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 global __probe\n    \
                 response.failureNoSuchUser('first')\n    \
                 try:\n        \
                 response.failureDBError('second')\n    \
                 except ValueError as error:\n        \
                 __probe = str(error)\n",
                recorder.clone(),
            );
            assert_eq!(read_probe(), "Response already sent");
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(67, \"first\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_the_named_failures_reach_the_handler() {
        interpreter_scenario(|| {
            for (method, message, status) in [
                ("failureNoSuchUser", "no user", LOGIN_REJECTED_NO_SUCH_USER),
                (
                    "failureInvalidPassword",
                    "bad password",
                    LOGIN_REJECTED_INVALID_PASSWORD,
                ),
                (
                    "failureDBError",
                    "db broke",
                    LOGIN_REJECTED_DB_GENERAL_FAILURE,
                ),
            ] {
                let recorder = recorded();
                one_login(
                    &format!(
                        "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                         response.{method}({message:?})\n"
                    ),
                    recorder.clone(),
                );
                assert_eq!(
                    recorder.borrow().entries,
                    vec![format!("failure({status}, {message:?})")],
                    "{method} maps to status {status}"
                );
            }
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_failure_refuses_the_okay_statuses() {
        interpreter_scenario(|| {
            // 0 and 1 are `okay()` statuses and refuse (`log_on_status.hpp`).
            for (status, expected) in [
                (0, "Invalid failure status 0"),
                (1, "Invalid failure status 1"),
            ] {
                let recorder = recorded();
                one_login(
                    &format!(
                        "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                         global __probe\n    \
                         try:\n        \
                         response.failure({status})\n    \
                         except ValueError as error:\n        \
                         __probe = str(error)\n"
                    ),
                    recorder.clone(),
                );
                assert_eq!(read_probe(), expected);
                // The refusal leaves the response unsent, so the destructor
                // reports general failure (`py_billing_system.cpp:41-48`).
                assert_eq!(
                    recorder.borrow().entries,
                    vec!["failure(71, \"No response from script\")".to_owned()]
                );
            }
            // Any non-okay status goes through.
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 response.failure(67, 'custom')\n",
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(67, \"custom\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_load_entity_by_name_carries_the_flags() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 response.loadEntityByName('Avatar', 'moe', True, False, 'c', 'b')\n",
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["load_from_username(7, \"moe\", true, false, \"c\", \"b\")".to_owned()]
            );

            // The defaults: no remembering, no payload
            // (`py_billing_system.cpp:96-104`).
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 response.loadEntityByName('Avatar', 'curly', False)\n",
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["load_from_username(7, \"curly\", false, false, \"\", \"\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_create_new_entity_carries_the_flag() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 response.createNewEntity('Avatar', True, 'c', 'b')\n",
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["create_new(7, true, \"c\", \"b\")".to_owned()]
            );

            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 response.createNewEntity('Avatar', False)\n",
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["create_new(7, false, \"\", \"\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_no_response_reports_general_failure() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 pass\n",
                recorder.clone(),
            );
            // The destructor's fallback, `py_billing_system.cpp:41-48`.
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(71, \"No response from script\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_script_error_reports_general_failure() {
        interpreter_scenario(|| {
            let recorder = recorded();
            one_login(
                "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                 raise RuntimeError('boom')\n",
                recorder.clone(),
            );
            // The failed call prints, clears, and the response the argument
            // tuple held dies with no answer sent
            // (`py_billing_system.cpp:416-423`).
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(71, \"No response from script\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_missing_get_method_reports_general_failure() {
        interpreter_scenario(|| {
            assert!(run_simple("set_only = 1\n"), "define nothing relevant");
            let system = PyBillingSystem::new(&main_module());
            assert!(!system.is_okay(), "isOkay is the get member's presence");
            let recorder = recorded();
            system.get_entity_key_for_account(
                "user",
                "pass",
                "1.2.3.4:6002",
                fixture_defs(),
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(71, \"\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_set_entity_key_crosses_the_mapping() {
        interpreter_scenario(|| {
            assert!(
                run_simple(
                    "def setEntityKeyForAccount(username, password, entityType, dbID):\n    \
                     global __probe\n    \
                     __probe = '%s|%s|%s|%d' % (username, password, entityType, dbID)\n",
                ),
                "define the set function"
            );
            let system = PyBillingSystem::new(&main_module());
            let recorder = Rc::new(RefCell::new(Recorded::default()));
            system.set_entity_key_for_account(
                "user",
                "pass",
                7,
                42,
                fixture_defs(),
                recorder.clone(),
            );
            assert_eq!(read_probe(), "user|pass|Avatar|42");
            assert_eq!(
                recorder.borrow().entries,
                vec!["set success(7, 42)".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_missing_set_method_fails_the_key() {
        interpreter_scenario(|| {
            assert!(run_simple("nothing = 1\n"), "define nothing relevant");
            let system = PyBillingSystem::new(&main_module());
            let recorder = Rc::new(RefCell::new(Recorded::default()));
            system.set_entity_key_for_account(
                "user",
                "pass",
                7,
                42,
                fixture_defs(),
                recorder.clone(),
            );
            assert_eq!(
                recorder.borrow().entries,
                vec!["set failure(7, 42)".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_set_script_error_fails_the_key() {
        interpreter_scenario(|| {
            assert!(
                run_simple(
                    "def setEntityKeyForAccount(username, password, entityType, dbID):\n    \
                     raise RuntimeError('set boom')\n",
                ),
                "define the failing set function"
            );
            let system = PyBillingSystem::new(&main_module());
            let recorder = Rc::new(RefCell::new(Recorded::default()));
            system.set_entity_key_for_account(
                "user",
                "pass",
                7,
                42,
                fixture_defs(),
                recorder.clone(),
            );
            // The failure goes out before the exception prints
            // (`py_billing_system.cpp:471-474`).
            assert_eq!(
                recorder.borrow().entries,
                vec!["set failure(7, 42)".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_stored_response_reports_at_teardown() {
        interpreter_scenario(|| {
            assert!(
                run_simple(
                    "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                     global held_response\n    \
                     held_response = response\n",
                ),
                "define the storing function"
            );
            let system = PyBillingSystem::new(&main_module());
            let recorder = recorded();
            system.get_entity_key_for_account(
                "user",
                "pass",
                "1.2.3.4:6003",
                fixture_defs(),
                recorder.clone(),
            );
            assert!(
                recorder.borrow().entries.is_empty(),
                "the script holds the response, so nothing fired yet"
            );
            // Dropping the last reference reports the unsent response.
            assert!(run_simple("del held_response\n"), "drop the response");
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(71, \"No response from script\")".to_owned()]
            );
        });
    }

    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_stored_response_still_answers() {
        interpreter_scenario(|| {
            assert!(
                run_simple(
                    "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
                     global held_response\n    \
                     held_response = response\n",
                ),
                "define the storing function"
            );
            let system = PyBillingSystem::new(&main_module());
            let recorder = recorded();
            system.get_entity_key_for_account(
                "user",
                "pass",
                "1.2.3.4:6004",
                fixture_defs(),
                recorder.clone(),
            );
            // The reference answers synchronously and holds a bare pointer
            // into the caller's frame; the port shares the handler, so a
            // late answer still lands instead of dereferencing a dangling
            // pointer.
            assert!(run_simple("held_response.loadEntity('Avatar', 9)\n"));
            assert_eq!(
                recorder.borrow().entries,
                vec!["success(7, 9, \"\", \"\")".to_owned()]
            );
        });
    }

    /// One login whose script answers with `body` and records the outcome
    /// in `__probe`: `answered` when nothing raised, `TypeName: message`
    /// when it did.
    fn answered_login(body: &str, recorder: Rc<RefCell<Recorded>>) -> String {
        let source = format!(
            "def getEntityKeyForAccount(username, password, clientAddr, response):\n    \
             global __probe\n    \
             try:\n        \
             {body}\n        \
             __probe = 'answered'\n    \
             except Exception as error:\n        \
             __probe = type(error).__name__ + ': ' + str(error)\n"
        );
        one_login(&source, recorder);
        read_probe()
    }

    /// A wrong answer -- bad arity, unknown type, a second send -- refuses
    /// the way the reference's parse and guard layers do
    /// (`py_billing_system.cpp:216-332`).
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_wrong_answers_refuse_like_the_reference() {
        interpreter_scenario(|| {
            // The definition helper itself refuses bad source
            // (`Script::runSimple`'s non-zero return).
            assert!(!run_simple("def broken(:\n"), "a syntax error refuses");
            assert!(!run_simple("x = 'a\x00b'\n"), "an embedded nul refuses");

            // Bad arity raises TypeError and leaves the response unsent, so
            // the destructor reports general failure.
            for body in [
                "response.loadEntity()",
                "response.loadEntityByName()",
                "response.createNewEntity()",
                "response.failure()",
                "response.failureNoSuchUser(1)",
                "response.failureInvalidPassword(b'x')",
                "response.failureDBError(1)",
            ] {
                let recorder = recorded();
                let probe = answered_login(body, recorder.clone());
                assert!(
                    probe.starts_with("TypeError:"),
                    "{body} must raise TypeError, got {probe:?}"
                );
                assert_eq!(
                    recorder.borrow().entries,
                    vec!["failure(71, \"No response from script\")".to_owned()],
                    "{body} leaves the response unsent"
                );
            }

            // An unknown entity type refuses every answering path with the
            // reference's formatted message.
            for body in [
                "response.loadEntityByName('Troll', 'x', True)",
                "response.createNewEntity('Troll', True)",
            ] {
                let recorder = recorded();
                let probe = answered_login(body, recorder.clone());
                assert_eq!(probe, "ValueError: Invalid entity type 'Troll'\n");
                assert_eq!(
                    recorder.borrow().entries,
                    vec!["failure(71, \"No response from script\")".to_owned()]
                );
            }

            // A second send of any kind refuses with `Response already
            // sent`; the first answer stands alone.
            let recorder = recorded();
            let probe = answered_login(
                "response.loadEntityByName('Avatar', 'a', True); \
                 response.loadEntityByName('Avatar', 'b', True)",
                recorder.clone(),
            );
            assert_eq!(probe, "ValueError: Response already sent");
            assert_eq!(
                recorder.borrow().entries,
                vec!["load_from_username(7, \"a\", true, false, \"\", \"\")".to_owned()]
            );

            let recorder = recorded();
            let probe = answered_login(
                "response.createNewEntity('Avatar', True); \
                 response.createNewEntity('Avatar', False)",
                recorder.clone(),
            );
            assert_eq!(probe, "ValueError: Response already sent");
            assert_eq!(
                recorder.borrow().entries,
                vec!["create_new(7, true, \"\", \"\")".to_owned()]
            );

            let recorder = recorded();
            let probe = answered_login(
                "response.failure(67); response.failure(67)",
                recorder.clone(),
            );
            assert_eq!(probe, "ValueError: Response already sent");
            assert_eq!(
                recorder.borrow().entries,
                vec!["failure(67, \"\")".to_owned()]
            );
        });
    }

    /// A handler whose cell is already borrowed cannot take a send: every
    /// degrade path skips the callback and carries on, the reference's
    /// clear-instead-of-send margin (`py_billing_system.cpp:44-47`).
    #[test]
    #[ignore = "runs in its own process: see SCENARIOS"]
    fn scenario_a_borrowed_handler_degrades_to_no_send() {
        interpreter_scenario(|| {
            // Missing get with a held borrow: no entry, no panic.
            let recorder = recorded();
            let guard = recorder.borrow();
            let system = PyBillingSystem::new(&main_module());
            assert!(!system.is_okay(), "the get member is missing");
            system.get_entity_key_for_account(
                "user",
                "pass",
                "1.2.3.4:6005",
                fixture_defs(),
                recorder.clone(),
            );
            drop(guard);
            assert!(recorder.borrow().entries.is_empty());

            // Missing set with a held borrow: no entry, no panic.
            let recorder = recorded();
            let guard = recorder.borrow();
            let system = PyBillingSystem::new(&main_module());
            system.set_entity_key_for_account(
                "user",
                "pass",
                7,
                42,
                fixture_defs(),
                recorder.clone(),
            );
            drop(guard);
            assert!(recorder.borrow().entries.is_empty());

            // A failing set call with a held borrow: the script error
            // prints, the callback degrades, no entry.
            assert!(
                run_simple(
                    "def setEntityKeyForAccount(username, password, entityType, dbID):\n    \
                     raise RuntimeError('set boom')\n"
                ),
                "define the failing set function"
            );
            let recorder = recorded();
            let guard = recorder.borrow();
            let system = PyBillingSystem::new(&main_module());
            system.set_entity_key_for_account(
                "user",
                "pass",
                7,
                42,
                fixture_defs(),
                recorder.clone(),
            );
            drop(guard);
            assert!(recorder.borrow().entries.is_empty());

            // A working set call with a held borrow: the script side runs,
            // only the Rust callback is skipped.
            assert!(
                run_simple(
                    "def setEntityKeyForAccount(username, password, entityType, dbID):\n    \
                     global __probe\n    \
                     __probe = 'mapped'\n"
                ),
                "define the set function"
            );
            let recorder = recorded();
            let guard = recorder.borrow();
            let system = PyBillingSystem::new(&main_module());
            system.set_entity_key_for_account(
                "user",
                "pass",
                7,
                42,
                fixture_defs(),
                recorder.clone(),
            );
            drop(guard);
            assert_eq!(read_probe(), "mapped", "the script side still ran");
            assert!(recorder.borrow().entries.is_empty());
        });
    }

    #[test]
    fn the_scenarios_run() {
        for scenario in SCENARIOS {
            run_scenario(scenario);
        }
    }

    /// Building the response type is a C API call: without an interpreter
    /// there is no type object to build.
    #[test]
    fn a_response_needs_an_interpreter() {
        assert!(matches!(
            wrap_response(recorded(), fixture_defs()),
            Err(BindError::NoInterpreter)
        ));
    }

    /// The released state reports the unsent response, the destructor's
    /// contract (`py_billing_system.cpp:41-48`).
    #[test]
    fn the_released_state_reports_an_unsent_response() {
        let recorder = recorded();
        let state = Box::into_raw(Box::new(ResponseState {
            handler: Some(recorder.clone()),
            defs: fixture_defs(),
        }));
        release_state(state);
        assert_eq!(
            recorder.borrow().entries,
            vec!["failure(71, \"No response from script\")".to_owned()]
        );
    }

    /// A released state that already answered sends nothing.
    #[test]
    fn the_released_state_of_an_answered_response_sends_nothing() {
        let recorder = recorded();
        let state = Box::into_raw(Box::new(ResponseState {
            handler: None,
            defs: fixture_defs(),
        }));
        release_state(state);
        assert!(recorder.borrow().entries.is_empty());
    }

    /// A re-entrant borrow puts the handler back: the response is still
    /// expected, the reference's clear-instead-of-send degrade
    /// (`py_billing_system.cpp:44-47`).
    #[test]
    fn a_reentrant_borrow_leaves_the_response_expected() {
        let recorder = recorded();
        let guard = recorder.borrow();
        let mut state = ResponseState {
            handler: Some(recorder.clone()),
            defs: fixture_defs(),
        };
        let handler = state.handler.take().expect("the handler is present");
        assert!(!deliver(&mut state, handler, |handler| {
            handler.on_failure(LOGIN_REJECTED_DB_GENERAL_FAILURE, "")
        }));
        drop(guard);
        assert!(state.handler.is_some(), "the response is still expected");
        // Once the borrow lets go, the deferred send lands.
        let handler = state.handler.take().expect("the handler is still present");
        assert!(deliver(&mut state, handler, |handler| {
            handler.on_failure(LOGIN_REJECTED_DB_GENERAL_FAILURE, "")
        }));
        assert!(state.handler.is_none(), "the send cleared the handler");
    }

    /// A null state block releases nothing; the dealloc guards the same way
    /// before reaching here.
    #[test]
    fn a_null_state_releases_nothing() {
        release_state(std::ptr::null_mut());
    }

    /// A handler under an active borrow cannot take the teardown send: the
    /// degrade leaves no entry and does not panic
    /// (`py_billing_system.cpp:44-47`).
    #[test]
    fn a_borrowed_handler_cannot_take_the_teardown_send() {
        let recorder = recorded();
        let state = Box::into_raw(Box::new(ResponseState {
            handler: Some(recorder.clone()),
            defs: fixture_defs(),
        }));
        let guard = recorder.borrow();
        release_state(state);
        drop(guard);
        assert!(recorder.borrow().entries.is_empty());
    }

    /// A member name with an embedded NUL cannot cross the C API; the empty
    /// handle is the refusal, before any interpreter call.
    #[test]
    fn an_unreadable_member_short_circuits() {
        assert!(get_member(&PyRef::empty(), "a\0b").is_empty());
    }

    /// An entity type name with an embedded NUL cannot be formatted into
    /// the C error message; the raise is skipped rather than mis-formatted.
    #[test]
    fn an_unreadable_type_name_skips_the_raise() {
        raise_invalid_entity_type("a\0b");
    }

    /// The fixture maps only `Avatar`; anything else is nothing.
    #[test]
    fn the_fixture_maps_nothing_else() {
        assert_eq!(FixtureDefs.type_name(999), String::new());
    }

    /// `sendFailure` clears the handler after the one send, the reference's
    /// `pHandler_ = nullptr` discipline (`py_billing_system.cpp:328-330`).
    #[test]
    fn a_send_clears_the_response_handler() {
        let recorder = recorded();
        let mut state = ResponseState {
            handler: Some(recorder.clone()),
            defs: fixture_defs(),
        };
        assert!(send_failure(
            &mut state,
            LOGIN_REJECTED_NO_SUCH_USER,
            "first"
        ));
        assert!(
            state.handler.is_none(),
            "the response is spent after one send"
        );
        assert_eq!(
            recorder.borrow().entries,
            vec!["failure(67, \"first\")".to_owned()]
        );
    }
}
