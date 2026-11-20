//! `PyRef`: the RAII reference-count handle for a `PyObject *`.
//!
//! The reference C++ layer keeps the count straight by hand
//! (`Py_IncRef`/`Py_DecRef` around every `PyObject *` that crosses a
//! boundary). `PyRef` is the same contract in Rust: an owning handle owns
//! exactly one reference, `Clone` copies it by incrementing, and `Drop`
//! decrements exactly once. Null is the empty handle, so a `PyRef` is always
//! usable without an `Option`.
//!
//! A handle is one of two forms, and the form decides what `Drop` does:
//!
//! - an owned handle ([`PyRef::from_owned`], every `clone()` result) is the
//!   `Py_INCREF`-and-carry side of the contract: `Drop` decrements;
//! - a borrowed handle ([`PyRef::borrowed`]) is a *view* of a pointer
//!   someone else owns -- the C call scope a `tp_new`/getter argument lives
//!   in. `Drop` does not touch the count: a view that decremented would
//!   steal a reference it never took, and one dropped view per call drains
//!   the count one per call -- the premature-free corruption the borrowed
//!   form once had.
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
/// The borrowed form owns nothing: see the module documentation for the two
/// forms.
///
/// Not `Send`: the reference is only valid while the owning thread holds the
/// GIL, the same bracket the reference enforces with
/// `Script::acquireLock`/`Script::releaseLock`.
#[derive(Debug)]
pub struct PyRef {
    ptr: *mut PyObject,
    owned: bool,
}

impl Clone for PyRef {
    /// `Py_XINCREF`: a copy owns a reference of its own, so dropping either
    /// handle alone leaves the other one and the object alive. The copy is
    /// always an owned handle -- even cloned from a borrowed view, the new
    /// handle took the count `Py_IncRef` gave it.
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
        Self { ptr, owned: true }
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
            owned: false,
        }
    }

    /// Wrap a borrowed pointer: a view of a reference the caller owns for
    /// the call's scope. Dropping the view does not touch the count -- it
    /// never owned one -- so a temporary view (the
    /// `PyRef::borrowed(p).clone()` idiom) hands out the clone's fresh
    /// reference whole.
    pub fn borrowed(ptr: *mut PyObject) -> Self {
        Self { ptr, owned: false }
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
            Some(Self { ptr, owned: true })
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

    /// True when this handle owns its reference: every `clone()` result and
    /// every [`PyRef::from_owned`] adoption, the handles a `Drop` may
    /// decrement for.
    pub fn is_owned(&self) -> bool {
        self.owned && !self.ptr.is_null()
    }
}

impl Drop for PyRef {
    fn drop(&mut self) {
        if !self.owned || self.ptr.is_null() || capi::is_finalized() {
            return;
        }
        // SAFETY: an owned non-null handle is only created from a pointer
        // the C API gave out, and this thread holds the GIL for its whole
        // lifetime.
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
    use crate::capi;

    /// `PyObject * NULL`: the empty handle owns nothing, so it must never
    /// `Py_DecRef` -- the reference's `Py_XDECREF` contract.
    #[test]
    fn the_empty_handle_owns_nothing() {
        let empty = PyRef::empty();
        assert!(empty.is_empty());
        assert!(empty.as_ptr().is_null());
        assert!(!empty.is_owned());
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

    /// A view of an object owns nothing, so dropping it must not take a
    /// reference: the borrowed form is the call-scope view, and a view that
    /// decremented would steal the caller's count once per call. The view is
    /// `Py_True`, whose immortal count a drop could not move either way.
    #[test]
    fn a_view_owns_nothing() {
        let view = PyRef::borrowed(capi::py_true());
        assert!(!view.is_empty());
        assert!(!view.is_owned());
        drop(view);
    }

    /// An adoption owns its count: the handle is the owner of the reference
    /// the C API handed out, so a later `Drop` may decrement for it. The
    /// adopted object is `Py_True`, the one object whose count a decrement
    /// cannot move.
    #[test]
    fn an_adoption_owns_its_count() {
        let owned = PyRef::from_owned(capi::py_true()).unwrap();
        assert!(!owned.is_empty());
        assert!(owned.is_owned());
        drop(owned);
    }

    /// Handle equality is pointer identity, so two empty handles compare
    /// equal to each other and are not mistaken for a borrowed object.
    #[test]
    fn the_empty_handle_equals_itself() {
        assert_eq!(PyRef::empty(), PyRef::empty());
        assert_eq!(PyRef::empty(), PyRef::borrowed(std::ptr::null_mut()));
    }
}
