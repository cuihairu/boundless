//! The native keep-alive registry: one strong reference per native identity.
//!
//! Reference: the C++ layer pins the objects a script must not lose the same
//! way at every site where a native side holds a callback or a view --
//! `PySTLSequence`'s constructor `Py_XINCREF`s the wrapper of the object that
//! owns the container and its destructor `Py_XDECREF`s it
//! (`lib/pyscript/stl_to_py.cpp:57-75`), the timer table holds owned
//! `function`/`arguments` pairs until `clearTimers()`
//! (`lib/pyscript/py_callback.cpp:62-88`), and a listener registry keeps a
//! strong `PyObjectPtr` per event (`script_events.hpp:47`). The ownership
//! rule all of them share is `Script::setData`
//! (`lib/pyscript/script.cpp:1967-1981`): adopt the new reference, drop the
//! old one -- exactly once each.
//!
//! The reference never frees an object while Python still names it:
//! `PyObjectPlus`'s destructor asserts `ob_refcnt == 0`
//! (`pyobject_plus.cpp:65-75`), so the native side dropping too much is
//! caught there. `zero_reference_count` below reads that same count, which
//! is the port's version of the tripwire: a release that happened exactly
//! once per owned reference leaves zero, and nothing else.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::capi;
use crate::pyref::PyRef;

/// Why a keep-alive entry could not be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinError {
    /// No interpreter is up: `Py_XINCREF` is a C API call.
    NoInterpreter,
    /// The handle was empty: there is no object to hold.
    NoObject,
}

/// The registry: one address per native identity. Addresses are stored as
/// `usize` so the registry stays `Send`, the shape the wrapper cache and the
/// type registry already use.
struct Registry {
    entries: BTreeMap<u64, usize>,
}

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            Mutex::new(Registry {
                entries: BTreeMap::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Give one held reference back: skipped after `Py_Finalize`, where the
/// handle layer also stops decrementing into a torn-down allocator.
fn release_reference(object: *mut capi::PyObject) {
    if capi::is_finalized() {
        return;
    }
    // SAFETY: each stored pointer came out of a handle this registry held.
    unsafe { capi::dec_ref(object) };
}

/// Pin `object` under a native identity: the registry takes a reference of
/// its own, the caller keeps the one it passed. Repinning an identity adopts
/// and replaces -- the old entry's reference goes exactly once, the
/// reference's `Script::setData` rule.
///
/// # Safety
///
/// The caller must hold the GIL while the interpreter is up.
pub unsafe fn pin(id: u64, object: &PyRef) -> Result<(), PinError> {
    if object.is_empty() {
        return Err(PinError::NoObject);
    }
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return Err(PinError::NoInterpreter);
    }
    // The registry's own reference: the same clone-and-forget as the wrapper
    // cache, so the entry outlives every caller's handle.
    let pinned = object.clone();
    let address = pinned.as_ptr() as usize;
    let mut registry = registry();
    // An existing entry is dropped now, once, the `setData` replace.
    if let Some(previous) = registry.entries.insert(id, address) {
        release_reference(previous as *mut capi::PyObject);
    }
    std::mem::forget(pinned);
    Ok(())
}

/// Release the entry an identity holds: the reference goes exactly once and
/// the entry is gone, so a second call for the same identity reports
/// missing.
pub fn unpin(id: u64) -> bool {
    let mut registry = registry();
    registry.entries.remove(&id).is_some_and(|address| {
        release_reference(address as *mut capi::PyObject);
        true
    })
}

/// Whether an identity is currently pinned.
pub fn is_pinned(id: u64) -> bool {
    registry().entries.contains_key(&id)
}

/// The number of pinned identities, for a host asserting its teardown.
pub fn pinned_count() -> usize {
    registry().entries.len()
}

/// Drop an entry and hand its reference back out instead of giving it up:
/// the caller takes ownership of one reference, the way the reference's
/// registry hands a held object to a caller before forgetting it.
pub fn take_out(id: u64) -> Option<PyRef> {
    let mut registry = registry();
    registry.entries.remove(&id).map(|address| {
        // The caller owns the entry's reference outright: adopt it, do not
        // drop it here.
        PyRef::borrowed(address as *mut capi::PyObject)
    })
}

/// `Interpreter::fini`'s native-teardown step for this registry: every held
/// reference goes now, while the allocator is still alive, and the registry
/// starts the next interpreter empty.
pub fn reset() {
    let mut registry = registry();
    for (_, address) in std::mem::take(&mut registry.entries) {
        release_reference(address as *mut capi::PyObject);
    }
}

/// `PyObjectPlus::~PyObjectPlus`'s `MF_ASSERT_DEV(ob_refcnt == 0)`
/// (`pyobject_plus.cpp:70`): true when nothing names the object any more.
/// Reading it is how the port checks that a release happened exactly once
/// per owned reference instead of twice or not at all.
///
/// # Safety
///
/// `object` must be a borrowed object the interpreter handed out, held while
/// it is up.
pub unsafe fn zero_reference_count(object: &PyRef) -> bool {
    unsafe { capi::refcount(object.as_ptr()) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinning is a C API call: without an interpreter there is no
    /// reference to take and the registry stays empty.
    #[test]
    fn pin_refuses_without_an_interpreter() {
        let object = PyRef::borrowed(capi::py_true());
        assert_eq!(unsafe { pin(7, &object) }, Err(PinError::NoInterpreter));
        assert!(!is_pinned(7));
        assert_eq!(pinned_count(), 0);
    }

    /// The empty handle names no object, so there is nothing to hold.
    #[test]
    fn pin_refuses_the_empty_handle() {
        assert_eq!(unsafe { pin(7, &PyRef::empty()) }, Err(PinError::NoObject));
    }

    /// Unpinning an identity the registry does not hold reports missing,
    /// which is what makes a double release visible to a host.
    #[test]
    fn unpin_on_an_empty_registry_reports_missing() {
        assert!(!unpin(7));
        assert!(!is_pinned(7));
        assert!(take_out(7).is_none());
    }

    /// `Interpreter::fini`'s step on an empty registry frees nothing.
    #[test]
    fn reset_on_an_empty_registry_frees_nothing() {
        reset();
        assert_eq!(pinned_count(), 0);
    }

    /// A poisoned registry lock recovers for the next caller, like the
    /// other process-wide registries.
    #[test]
    fn a_poisoned_registry_lock_recovers() {
        let poisoner = std::thread::spawn(|| {
            let _guard = registry();
            panic!("poison the keep-alive lock");
        });
        assert!(poisoner.join().is_err());
        assert_eq!(pinned_count(), 0);
    }
}
