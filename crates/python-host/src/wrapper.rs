//! The identity cache for native wrapper objects.
//!
//! Reference: a wrapped native object is a `PyObjectPlus` (`lib/pyscript/
//! pyobject_plus.hpp:1087`) -- the C++ object embeds the `PyObject` header,
//! so the native object *is* the Python object and "the same underlying
//! object yields the same instance" holds by construction. The lookup layer
//! never builds a second wrapper: `PyEntities::subscript` (`cellapp/
//! py_entities.cpp`) finds the native entity in the `CellApp` map and hands
//! the same pointer back with `Py_XINCREF`, and the native map owns the
//! objects for their whole life.
//!
//! Rust objects cannot embed the CPython header, so the port keeps the one
//! wrapper per native identity in a native-side registry instead, in exactly
//! the reference's ownership shape: the cache owns one strong reference per
//! wrapper for as long as the native object is registered, and repeated
//! `wrap` calls clone that handle -- the `Py_XINCREF`-and-return of the
//! subscript layer. The wrapper object itself is a `PyCapsule` carrying the
//! native pointer, the port's stand-in for the embedded header until a
//! heap wrapper type exists (the attribute/method binding item).
//!
//! The native registry dying before the script system finalises is the
//! reference's process teardown (`CellApp::shutDown` deletes its entities
//! before `Script::fini`); [`reset`] is `Interpreter::fini`'s equivalent
//! step, so every wrapper `Py_DecRef`s while the allocator is still alive.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::capi;
use crate::pyref::PyRef;

/// The capsule name every wrapper carries: `PyCapsule_IsValid` type-checks
/// against it, so a foreign object never reads as a native wrapper.
const WRAPPER_NAME: &std::ffi::CStr = c"boundless.wrapper";

/// Why a native object could not be wrapped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapError {
    /// No interpreter is up: creating the wrapper object is a C API call.
    NoInterpreter,
    /// `PyCapsule_New` returned null; the pending error is cleared.
    CreateFailed,
}

/// One registered native object: the cache owns this reference for as long
/// as the identity stays registered. The address is stored as `usize` so the
/// registry stays `Send`: a raw `*mut PyObject` would drag the non-`Send`
/// pointertype in, while the handle handed out (`PyRef`) never crosses
/// threads -- `PyRef` is deliberately `!Send`, the reference's
/// `Script::acquireLock` bracket.
struct Cache {
    entries: BTreeMap<u64, usize>,
}

/// The stored address back as the object pointer it names.
fn object_of(address: usize) -> *mut capi::PyObject {
    address as *mut capi::PyObject
}

/// Give one cached reference back: skipped after `Py_Finalize`, where the
/// `PyRef` handle layer also stops decrementing into a torn-down allocator.
fn release_reference(object: *mut capi::PyObject) {
    if capi::is_finalized() {
        return;
    }
    // SAFETY: each stored pointer came out of `PyCapsule_New`.
    unsafe {
        capi::dec_ref(object);
    }
}

fn cache() -> MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            Mutex::new(Cache {
                entries: BTreeMap::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `PyEntities::subscript` for a native id: the one wrapper the registry
/// holds for `id`, cloned out (a fresh owning reference of the same object),
/// or a new capsule registered on first sight.
///
/// The same address under two ids yields two wrappers -- the registry is
/// keyed by identity, not by pointer.
///
/// # Safety
///
/// `native` is dereferenced by `PyCapsule_New` only to be stored: it must
/// stay valid for as long as the wrapper can be reached from Python, and
/// the caller must hold the GIL (an up interpreter on the calling thread).
pub unsafe fn wrap(native: *mut std::os::raw::c_void, id: u64) -> Result<PyRef, WrapError> {
    let mut cache = cache();
    if let Some(existing) = cache.entries.get(&id) {
        // `Py_XINCREF` and return, the subscript layer's move: the caller
        // owns one fresh reference, the registry keeps its own. (`PyRef::
        // borrowed(..).clone()` would net zero -- the temporary's `Drop`
        // gives back what the clone took -- so the increment and the
        // adoption are spelled out.)
        let pointer = object_of(*existing);
        // SAFETY: the interpreter is up and the pointer is one
        // `PyCapsule_New` gave out.
        unsafe { capi::inc_ref(pointer) };
        return Ok(PyRef::borrowed(pointer));
    }
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return Err(WrapError::NoInterpreter);
    }
    // SAFETY: the interpreter is up, this thread owns the GIL, and the
    // capsule owns no other reference.
    let object = unsafe { capi::PyCapsule_New(native, WRAPPER_NAME.as_ptr(), None) };
    let Some(object) = PyRef::from_owned(object) else {
        unsafe { capi::clear_error() };
        return Err(WrapError::CreateFailed);
    };
    // The registry takes its own reference -- the caller's handle and the
    // cached entry must each own one, or the first release would free a
    // wrapper someone still holds.
    let cached = object.clone();
    cache.entries.insert(id, cached.as_ptr() as usize);
    std::mem::forget(cached);
    Ok(object)
}

/// The native address a wrapper carries, or `None` for any object that is
/// not one of this module's capsules -- a foreign object never reads as a
/// native wrapper.
///
/// # Safety
///
/// `object` must be a handle to an object CPython gave out, held while the
/// interpreter is up.
pub unsafe fn native(object: &PyRef) -> Option<*mut std::os::raw::c_void> {
    unsafe {
        if capi::PyCapsule_IsValid(object.as_ptr(), WRAPPER_NAME.as_ptr()) == 0 {
            return None;
        }
        Some(capi::PyCapsule_GetPointer(
            object.as_ptr(),
            WRAPPER_NAME.as_ptr(),
        ))
    }
}

/// The host dropped the native object: the registry forgets its wrapper,
/// releasing the reference it held. Reports whether an identity was there.
pub fn release(id: u64) -> bool {
    let mut cache = cache();
    cache.entries.remove(&id).is_some_and(|address| {
        release_reference(object_of(address));
        true
    })
}

/// `Interpreter::fini`'s native-teardown step: every registered wrapper
/// `Py_DecRef`s now, while the allocator is still alive, and the registry
/// starts the next interpreter empty.
pub fn reset() {
    let mut cache = cache();
    for (_, address) in std::mem::take(&mut cache.entries) {
        release_reference(object_of(address));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrapping is a C API call: without an interpreter there is no capsule
    /// to build and the registry stays empty.
    #[test]
    fn wrap_refuses_without_an_interpreter() {
        // SAFETY: the null native pointer is only stored on success, and
        // the refusal happens before any C API call.
        assert_eq!(
            unsafe { wrap(std::ptr::null_mut(), 1) },
            Err(WrapError::NoInterpreter)
        );
        assert!(!release(1));
    }

    /// Releasing an identity the registry does not hold reports missing.
    #[test]
    fn release_on_an_empty_cache_reports_missing() {
        assert!(!release(64));
    }

    /// A poisoned registry lock recovers for the next caller, like the
    /// other process-wide registries.
    #[test]
    fn a_poisoned_cache_lock_recovers() {
        let poisoner = std::thread::spawn(|| {
            let _guard = cache();
            panic!("poison the cache lock");
        });
        assert!(poisoner.join().is_err());
        assert!(!release(65));
    }
}
