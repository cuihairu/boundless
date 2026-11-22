//! `PyRef`: the RAII reference-count handle for a `PyObject *`.
//!
//! The reference C++ layer keeps the count straight by hand
//! (`Py_IncRef`/`Py_DecRef` around every `PyObject *` that crosses a
//! boundary). `PyRef` is the same contract in Rust: acquiring a handle owns
//! one reference, `Clone` copies it by incrementing, and `Drop` decrements
//! exactly once. Null is the empty handle, so a `PyRef` is always usable
//! without an `Option`.
//!
//! Two rules from the reference are honoured here:
//!
//! - `Py_XDECREF` semantics on the empty handle: dropping a null handle does
//!   nothing, which is how the reference `Py_XDECREF`s a possibly-failed
//!   call result;
//! - no decrement after the interpreter is gone. `Script::fini` clears the
//!   `__main__` module and calls `Py_Finalize` before the host releases its
//!   own bookkeeping, so a handle outliving finalisation must not
//!   `Py_DecRef` into a torn-down allocator (`capi::finalize` records the
//!   transition and `Drop` skips the decrement).

use super::capi::{self, PyObject};

/// A strong reference to a Python object; owns exactly one reference count.
///
/// Not `Send`: the reference is only valid while the owning thread holds the
/// GIL, the same bracket the reference enforces with
/// `Script::acquireLock`/`Script::releaseLock`.
///
/// `repr(transparent)`: the handle is exactly the pointer, so a `PyRef`
/// stored in a provider's `#[repr(C)]` instance body (`script_math.cpp`'s
/// `Vector4ProviderPtr` members, BW-03-03-02) has the layout the body
/// declares, and `Option<PyRef>` folds the null niche the same way the
/// reference's null `SmartPointer` does.
#[derive(Debug)]
#[repr(transparent)]
pub struct PyRef {
    ptr: *mut PyObject,
}

impl Clone for PyRef {
    /// `Py_XINCREF`: a copy owns a reference of its own, so dropping either
    /// handle alone leaves the other one and the object alive. A derived
    /// `Clone` would not increment and two handles would each decrement the
    /// same reference, freeing the object while a handle still named it.
    fn clone(&self) -> Self {
        let ptr = self.ptr;
        if !ptr.is_null() && !capi::is_finalized() {
            // SAFETY: the pointer is one the C API gave out and this thread
            // owns the GIL for the handle's whole lifetime; the finalised
            // check keeps a post-`Script::fini` clone away from a torn-down
            // allocator.
            unsafe {
                capi::inc_ref(ptr);
            }
        }
        Self { ptr }
    }
}

impl Default for PyRef {
    fn default() -> Self {
        Self::empty()
    }
}

impl PyRef {
    /// The empty handle (`PyObject * NULL`), which owns nothing.
    pub fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
        }
    }

    /// Wrap a borrowed pointer without touching the reference count.
    pub fn borrowed(ptr: *mut PyObject) -> Self {
        Self { ptr }
    }

    /// Adopt an owned pointer, taking responsibility for its reference count.
    ///
    /// `Option` form: a null pointer (the C API failure return) becomes the
    /// empty handle instead of a panic, which is how the reference folds a
    /// failed allocation into `Py_XDECREF(pArg)`-style cleanup.
    pub fn from_owned(ptr: *mut PyObject) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr })
        }
    }

    /// The borrowed pointer, null for the empty handle.
    pub fn as_ptr(&self) -> *mut PyObject {
        self.ptr
    }

    /// True for the empty handle.
    pub fn is_empty(&self) -> bool {
        self.ptr.is_null()
    }
}

impl Drop for PyRef {
    fn drop(&mut self) {
        if self.ptr.is_null() || capi::is_finalized() {
            return;
        }
        // SAFETY: a non-null handle is only created from a pointer the C API
        // gave out, and this thread holds the GIL for its whole lifetime.
        unsafe {
            capi::dec_ref(self.ptr);
        }
    }
}

impl PartialEq for PyRef {
    fn eq(&self, other: &Self) -> bool {
        // Object identity, as in the reference: two handles are equal when
        // they name the same `PyObject *`.
        self.ptr == other.ptr
    }
}

impl Eq for PyRef {}

#[cfg(test)]
mod tests {
    use super::PyRef;

    /// `PyObject * NULL`: the empty handle owns nothing, so it must never
    /// `Py_DecRef` -- the reference's `Py_XDECREF` contract.
    #[test]
    fn the_empty_handle_owns_nothing() {
        let empty = PyRef::empty();
        assert!(empty.is_empty());
        assert!(empty.as_ptr().is_null());
        drop(empty);
    }

    #[test]
    fn default_is_the_empty_handle() {
        assert!(PyRef::default().is_empty());
    }

    /// A null borrowed pointer is the empty handle too, so a `PyRef::borrowed`
    /// on a failed C API call is still safe to hold and drop.
    #[test]
    fn borrowing_null_is_the_empty_handle() {
        let borrowed = PyRef::borrowed(std::ptr::null_mut());
        assert!(borrowed.is_empty());
        drop(borrowed);
    }

    /// A failed call result (null) is the empty handle, not a panic: the
    /// reference folds that into `Py_XDECREF(pArg)`-style cleanup.
    #[test]
    fn adopting_null_is_none() {
        assert!(PyRef::from_owned(std::ptr::null_mut()).is_none());
    }

    /// Cloning the empty handle copies a pointer that owns nothing, so the
    /// clone is empty as well and dropping it is a no-op.
    #[test]
    fn cloning_the_empty_handle_keeps_it_empty() {
        let cloned = PyRef::empty().clone();
        assert!(cloned.is_empty());
        drop(cloned);
    }

    /// Handle equality is pointer identity, so two empty handles compare
    /// equal to each other and are not mistaken for a borrowed object.
    #[test]
    fn the_empty_handle_equals_itself() {
        assert_eq!(PyRef::empty(), PyRef::empty());
        assert_eq!(PyRef::empty(), PyRef::borrowed(std::ptr::null_mut()));
    }
}
