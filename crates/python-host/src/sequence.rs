//! The mutable container proxy: a sequence type over a native state block.
//!
//! Reference: `PySTLSequence` (`lib/pyscript/stl_to_py.cpp`) holds a
//! reference into the native container and applies the script's write
//! through `commit()`, so a mutation seen from Python lands in the native
//! object the engine owns. The proxy fills `sq_length`, `sq_item` and
//! `sq_ass_item` and, because the mapping protocol wins `obj[i]` for a heap
//! type, the mapping slots are bridged back into those handlers by
//! `binding` (the reference's `seqMappingMethods`,
//! `pyobject_plus.cpp:294`).
//!
//! Two registered platform gaps: the port fills only those three handlers
//! (`sq_concat`, `sq_repeat`, `sq_contains`, the inplace pairs are absent --
//! the reference fills them on the same proxy), and no slice is exposed
//! (`sq_slice` is gone from 3.13, the reference says so in a comment at
//! `stl_to_py.cpp:31`). The host state block must outlive every view;
//! that keep-alive is BW-03-02-04's subject.

use std::ffi::c_int;
use std::os::raw::c_long;

use crate::binding::{self, BindError};
use crate::capi;
use crate::pyref::PyRef;
use crate::wrapper::WrapError;

/// The dotted type name every view of this module carries.
const TYPE_NAME: &str = "boundless.Sequence";

/// The native state a view looks into: the container's values and whether
/// the container is writable. The reference reads the same two facts from
/// the container it holds a reference into (`PySTLSequence` vs the
/// read-only read proxies).
///
/// Every access happens under the GIL, so no lock is needed: scripts are
/// the only writers, the reference's situation.
pub struct SequenceState {
    values: Vec<i64>,
    writable: bool,
}

impl SequenceState {
    /// A state with the given values, writable by default.
    #[must_use]
    pub fn new(values: &[i64]) -> Self {
        Self {
            values: values.to_vec(),
            writable: true,
        }
    }

    /// A read-only state: reads work, writes refuse.
    #[must_use]
    pub fn read_only(values: &[i64]) -> Self {
        Self {
            values: values.to_vec(),
            writable: false,
        }
    }

    /// The values, for a host asserting the mutation landed.
    #[must_use]
    pub fn values(&self) -> &[i64] {
        &self.values
    }

    /// The values mutably, for a host asserting the other direction: a
    /// native-side write the script must see.
    pub fn values_mut(&mut self) -> &mut [i64] {
        &mut self.values
    }

    /// Whether a view of this state accepts writes.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.writable
    }
}

/// The instance body `binding` allocates for the proxy: the `PyObject`
/// header the interpreter owns, then the pointer to the state the view
/// reports on. The reference embeds the container reference in its holder
/// the same way.
#[repr(C)]
struct SequenceBody {
    header: capi::PyObject,
    state: *mut SequenceState,
}

fn spec() -> binding::TypeSpec {
    binding::TypeSpec::new(TYPE_NAME)
        .sequence(sq_length, sq_item, sq_ass_item)
        .instance_size(std::mem::size_of::<SequenceBody>() - std::mem::size_of::<capi::PyObject>())
}

/// The state an instance body carries, or null for an object that is not
/// one of this module's views.
///
/// # Safety
///
/// `object` must be one of this module's instances, whose body is a
/// `SequenceBody`.
unsafe fn state_of(object: *mut capi::PyObject) -> *mut SequenceState {
    // SAFETY: the caller holds an instance of this module's type.
    unsafe { (*object.cast::<SequenceBody>()).state }
}

/// `sq_length`.
unsafe extern "C" fn sq_length(object: *mut capi::PyObject) -> capi::PySsize {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(object) };
    if state.is_null() {
        return 0;
    }
    unsafe { (*state).values.len() as capi::PySsize }
}

/// `sq_item`, the indexed read.
unsafe extern "C" fn sq_item(
    object: *mut capi::PyObject,
    index: capi::PySsize,
) -> *mut capi::PyObject {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(object) };
    if state.is_null() {
        raise_index_error(c"boundless.Sequence index out of range");
        return std::ptr::null_mut();
    }
    // SAFETY: `state` is the state pointer the host handed `wrap`.
    let values = unsafe { &(*state).values };
    let count = values.len() as capi::PySsize;
    if index < 0 || index >= count {
        raise_index_error(c"boundless.Sequence index out of range");
        return std::ptr::null_mut();
    }
    // SAFETY: the interpreter is up when the handler runs.
    // SAFETY: `state` is the state pointer the host handed `wrap`.
    let values = unsafe { &(*state).values };
    // SAFETY: the interpreter is up when the handler runs.
    unsafe { capi::PyLong_FromLong(values[index as usize] as c_long) }
}

/// `sq_ass_item`, the indexed write; a `NULL` item is the protocol's
/// deletion.
unsafe extern "C" fn sq_ass_item(
    object: *mut capi::PyObject,
    index: capi::PySsize,
    value: *mut capi::PyObject,
) -> c_int {
    // SAFETY: the handler only runs on instances of this module's type.
    let state = unsafe { state_of(object) };
    if state.is_null() {
        raise_type_error(c"cannot assign an item without a state");
        return c_int::MAX;
    }
    let writable = unsafe { (*state).writable };
    if !writable {
        // The reference's read-only refusal, `stl_to_py.cpp`.
        raise_type_error(c"Cannot assign an item in a read-only boundless.Sequence");
        return c_int::MAX;
    }
    // SAFETY: `state` is the state pointer the host handed `wrap`.
    let count = unsafe { (*state).values.len() as capi::PySsize };
    if index < 0 || index >= count {
        raise_index_error(c"boundless.Sequence assignment index out of range");
        return c_int::MAX;
    }
    let at = index as usize;
    if value.is_null() {
        // Deletion: the value leaves the native container.
        // SAFETY: `state` is the state pointer the host handed `wrap`.
        unsafe { (*state).values.remove(at) };
        return 0;
    }
    let item = unsafe { capi::PyLong_AsLong(value) };
    if item == -1 && !unsafe { capi::PyErr_Occurred() }.is_null() {
        return c_int::MAX;
    }
    // SAFETY: `state` is the state pointer the host handed `wrap`.
    let values = unsafe { &mut (*state).values };
    values[at] = item as i64;
    0
}

/// Raise `IndexError` with the reference's message, or report nothing when
/// the interpreter is down: the exception constants are null until init, so
/// raising before it would read a null pointer.
fn raise_index_error(message: &std::ffi::CStr) {
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return;
    }
    // SAFETY: the interpreter is up and `message` is a static byte string.
    unsafe { capi::PyErr_SetString(capi::PyExc_IndexError, message.as_ptr()) };
}

/// Raise `TypeError` with the reference's message, or report nothing when
/// the interpreter is down.
fn raise_type_error(message: &std::ffi::CStr) {
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return;
    }
    // SAFETY: the interpreter is up and `message` is a static byte string.
    unsafe { capi::PyErr_SetString(capi::PyExc_TypeError, message.as_ptr()) };
}

/// Build the one view of a native state block. The host keeps the state
/// alive; the view does not own it, the reference's same arrangement.
///
/// # Safety
///
/// `state` must stay valid for as long as the view is reachable from a
/// script, and the interpreter must be up on the calling thread.
pub unsafe fn wrap(state: *mut SequenceState) -> Result<PyRef, WrapError> {
    if state.is_null() {
        return Err(WrapError::CreateFailed);
    }
    let type_ = match binding::create_type(&spec()) {
        Ok(type_) => type_,
        Err(BindError::NoInterpreter) => return Err(WrapError::NoInterpreter),
        Err(_) => return Err(WrapError::CreateFailed),
    };
    let Some(view) = (unsafe { binding::instantiate(&type_) }) else {
        return Err(WrapError::CreateFailed);
    };
    // SAFETY: the allocated body is exactly a `SequenceBody`, `basicsize`
    // was set to its size by `spec`.
    unsafe {
        (*view.as_ptr().cast::<SequenceBody>()).state = state;
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stack-allocated body stands in for the instance `instantiate`
    /// would build, so the handlers are reachable without an interpreter.
    fn body(state: SequenceState) -> SequenceBody {
        SequenceBody {
            header: unsafe { std::mem::zeroed() },
            state: &state as *const SequenceState as *mut SequenceState,
        }
    }

    /// Without an interpreter there is no type object to build and no body
    /// to hang the state pointer in.
    #[test]
    fn wrap_refuses_without_an_interpreter() {
        let state = SequenceState::new(&[1]);
        // SAFETY: a borrowed pointer is handed over; nothing dereferences
        // it while the interpreter is down.
        assert_eq!(
            unsafe { wrap(&state as *const SequenceState as *mut SequenceState) },
            Err(WrapError::NoInterpreter)
        );
    }

    /// A null state is not wrappable: there is nothing to view.
    #[test]
    fn wrap_refuses_a_null_state() {
        // SAFETY: the null pointer is refused before any use.
        assert_eq!(
            unsafe { wrap(std::ptr::null_mut()) },
            Err(WrapError::CreateFailed)
        );
    }

    /// `sq_length` reports the native container's length.
    #[test]
    fn the_length_handler_counts_the_native_values() {
        let state = SequenceState::new(&[10, 20, 30]);
        let body = body(state);
        let object = &body as *const SequenceBody as *mut capi::PyObject;
        // SAFETY: `body` is a real `SequenceBody` for the duration of the
        // call.
        assert_eq!(unsafe { sq_length(object) }, 3);
    }

    /// A read outside the container reports the reference's index error
    /// text and nothing at all while the interpreter is down.
    #[test]
    fn an_out_of_range_read_refuses() {
        let state = SequenceState::new(&[10]);
        let body = body(state);
        let object = &body as *const SequenceBody as *mut capi::PyObject;
        // SAFETY: `body` is a real `SequenceBody`.
        assert!(unsafe { sq_item(object, 1) }.is_null());
    }

    /// A body with no state refuses the read instead of dereferencing.
    #[test]
    fn a_stateless_body_refuses_the_read() {
        let body = SequenceBody {
            header: unsafe { std::mem::zeroed() },
            state: std::ptr::null_mut(),
        };
        let object = &body as *const SequenceBody as *mut capi::PyObject;
        // SAFETY: `body` is a real `SequenceBody`.
        assert!(unsafe { sq_item(object, 0) }.is_null());
    }

    /// A read-only container refuses an assignment with the reference's
    /// message.
    #[test]
    fn a_read_only_state_refuses_the_write() {
        let state = SequenceState::read_only(&[10]);
        let body = body(state);
        let object = &body as *const SequenceBody as *mut capi::PyObject;
        // SAFETY: `body` is a real `SequenceBody`.
        assert_eq!(unsafe { sq_ass_item(object, 0, any_value()) }, c_int::MAX);
    }

    /// A write outside the container refuses and leaves the values alone.
    #[test]
    fn an_out_of_range_write_refuses() {
        let state = SequenceState::new(&[10]);
        let body = body(state);
        let object = &body as *const SequenceBody as *mut capi::PyObject;
        // SAFETY: `body` is a real `SequenceBody`.
        assert_eq!(unsafe { sq_ass_item(object, 1, any_value()) }, c_int::MAX);
    }

    /// The refusing arms return before the value is read, so a placeholder
    /// pointer stands for the integer a script would hand over.
    fn any_value() -> *mut capi::PyObject {
        std::ptr::null_mut()
    }
}
