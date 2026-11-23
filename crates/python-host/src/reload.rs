//! Reload mechanics: the per-entity class swap, the module-window swap-in,
//! and the migrate call that ties them to one live entity.
//!
//! Reference: `Entity::swapScriptClass` (`server/cellapp/entity.ipp:355-370`),
//! `EntityType::swapInModules` (`server/cellapp/entity_type.ipp:74-112`) and
//! `Entity::migrate` (`server/cellapp/entity.cpp:4258-4345`), which lean on
//! `Script::ask` / `Script::call` (`lib/pyscript/script.cpp:1125-1210`,
//! `lib/pyscript/script.ipp:15-30`).
//!
//! The reference reload is driven from the script side (`BigWorld.reloadScript`)
//! with the interpreter, entity-type table and live-entity collection held by a
//! running `CellApp`. The port keeps this crate engine-internal -- there is no
//! `reloadScript` module face, matching the reference where the migrate chain
//! lives behind `CellApp` and not on a `BigWorld` method -- and the host calls
//! these three functions directly. The orchestration around them (scratch
//! interpreter, entity-type table swap, the walk over live entities) is the
//! BW-03-04-05 / BW-04 follow-on; this module is the migrate mechanics it will
//! drive.
//!
//! Deltas kept visible:
//! - The port has no `Entity` C++ shell and no `EntityType` registry, so
//!   `migrate_instance` takes the entity object and the new class directly and
//!   returns the pair the reference packs into an `EntityMigration::
//!   FailureClass`: `(swapped, hook_raised)`. The type-id guard and the
//!   `pEntityType_` / `old()` bookkeeping are the entity-type table's layer
//!   (BW-04) and are not reproduced here.
//! - `swap_script_class` returns `PyObject_SetAttrString(...) != -1`, and
//!   leaves a rejected swap's `TypeError` pending exactly as the reference's
//!   `migrate` finds it; printing is the caller's job there, and
//!   `migrate_instance` does it with `PyErr_PrintEx(1)` (`PyErr_Print` is that
//!   call).
//! - The hook path prints once. The reference prints twice when the hook is
//!   absent: `Script::ask` prints the pending attribute error with
//!   `PyErr_PrintEx(0)` and its `printException` path prints it again in
//!   extended form. The port reports it through the one print this crate uses
//!   everywhere, then clears.
//! - `Script::call` discards the hook's result; the port DECREFs it, which is
//!   the same release.
//! - The 3.13 migration note (`entity_type.ipp:93-101`) is honoured by
//!   construction: the port never touches `interp->builtins`, so there is
//!   nothing to migrate.

use std::ffi::c_char;

use crate::capi;

/// `Entity::swapScriptClass` (`entity.ipp:364-368`): set the entity's
/// `__class__` to the reloaded class. CPython's assignment is all-or-nothing
/// -- when its layout check rejects the swap, `__class__` is left untouched
/// and a `TypeError` is left pending, which is what the caller's rollback
/// relies on.
///
/// Returns whether the swap landed.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and both
/// pointers must be live objects the caller owns a reference to.
pub unsafe fn swap_script_class(
    instance: *mut capi::PyObject,
    new_class: *mut capi::PyObject,
) -> bool {
    // SAFETY: the caller guarantees live objects and the GIL.
    unsafe { capi::PyObject_SetAttrString(instance, c"__class__".as_ptr(), new_class) != -1 }
}

/// `EntityType::swapInModules` (`entity_type.ipp:74-112`): replace a module
/// window with its reloaded counterpart. `modules` is the live `sys.modules`
/// dict; `new_modules` is the window the scratch interpreter built. The dict
/// is cleared, refilled from the reloaded window, `BigWorld` is re-exposed on
/// `__main__`, and the live `sys` module is restored last so the stdout/stderr
/// redirection it carries survives the swap.
///
/// The reference reads `sys` and `__main__` out of `modules` itself
/// (`entity_type.ipp:76,107`), so does the port: the caller must hand it a
/// complete reloaded window, one that still names `__main__` and `sys`, or the
/// restore steps are skipped the way a missing `sys` is skipped there.
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and both
/// pointers must be live dict objects the caller owns a reference to.
pub unsafe fn swap_in_modules(modules: *mut capi::PyObject, new_modules: *mut capi::PyObject) {
    // SAFETY: `entity_type.ipp:76-84` -- the clear below drops this dict's
    // reference on the sys module, so the borrowed pointer has to be upgraded
    // to an owned one before it can survive the function.
    let sys_module = unsafe { capi::PyDict_GetItemString(modules, c"sys".as_ptr()) };
    if !sys_module.is_null() {
        unsafe { capi::inc_ref(sys_module) };
    }
    // SAFETY: both dicts are live.
    unsafe {
        capi::PyDict_Clear(modules);
        capi::PyDict_Update(modules, new_modules);
    }
    // SAFETY: `modules` now holds the reloaded window, so `BigWorld` is the
    // reloaded module.
    let big_world = unsafe { capi::PyDict_GetItemString(modules, c"BigWorld".as_ptr()) };
    if !big_world.is_null() {
        unsafe {
            // `PyModule_AddObject` steals a reference.
            capi::inc_ref(big_world);
            let main = capi::PyDict_GetItemString(modules, c"__main__".as_ptr());
            if main.is_null() {
                // No `__main__` in the reloaded window, so there is nothing to
                // expose on. The reference would hand a NULL module to
                // `PyModule_AddObject`; the port refuses to crash on a
                // short window instead, and releases the reference it took.
                capi::dec_ref(big_world);
            } else {
                capi::PyModule_AddObject(main, c"BigWorld".as_ptr(), big_world);
            }
        }
    }
    // Restore the original sys module last: it carries the remapped
    // stdout/stderr. `PyDict_SetItemString` does not steal, so the reference
    // owned since the INCREF above is released here (`entity_type.ipp:108-112`).
    if !sys_module.is_null() {
        unsafe {
            capi::PyDict_SetItemString(modules, c"sys".as_ptr(), sys_module);
            capi::dec_ref(sys_module);
        }
    }
}

/// `Entity::migrate` (`entity.cpp:4258-4345`) minus its type-id guard and its
/// type-table bookkeeping, which belong to the entity-type table's layer:
/// swap the entity's `__class__` to the reloaded class, then run its
/// `onMigrate` hook with the entity as the argument.
///
/// Returns `(swapped, hook_raised)`. A rejected swap prints its error the way
/// `migrate`'s failure branch does and reports `(false, false)`: `__class__`
/// was left untouched, so the caller only restores the type pointer it already
/// swapped. A swap that lands reports `(true, false)` when the hook is absent
/// or clean and `(true, true)` when it raises -- a hook failure the reference
/// classifies as a migration failure but still counts as success
/// (`entity.cpp:4327-4341`).
///
/// # Safety
///
/// The interpreter must be up, the caller must own the GIL, and both
/// pointers must be live objects the caller owns a reference to.
pub unsafe fn migrate_instance(
    instance: *mut capi::PyObject,
    new_class: *mut capi::PyObject,
) -> (bool, bool) {
    // SAFETY: the caller guarantees live objects and the GIL.
    if !unsafe { swap_script_class(instance, new_class) } {
        // `entity.cpp:4298-4306`: the swap failed, so print the error the way
        // the reference does and leave the class untouched.
        unsafe { capi::PyErr_PrintEx(1) };
        return (false, false);
    }
    // SAFETY: the hook call and its argument tuple stay in this frame.
    let hook_raised = call_on_migrate(instance);
    (true, hook_raised)
}

/// `Script::call(PyObject_GetAttrString(this, "onMigrate"),
/// Py_BuildValue("(O)", this), "onMigrate", true)`
/// (`entity.cpp:4327-4331`): find the hook, call it with the entity as its
/// one argument, report raised or not. Returns whether the hook raised.
fn call_on_migrate(instance: *mut capi::PyObject) -> bool {
    // SAFETY: `instance` is live, and a missing attribute returns NULL with the
    // `AttributeError` left pending -- exactly the state `Script::ask` expects.
    let function = unsafe { capi::PyObject_GetAttrString(instance, c"onMigrate".as_ptr()) };
    // SAFETY: a fresh one-slot tuple taking one owned reference to `instance`;
    // the INCREF makes it one.
    let args = unsafe {
        let tuple = capi::PyTuple_New(1);
        if tuple.is_null() {
            std::ptr::null_mut()
        } else {
            capi::inc_ref(instance);
            // SAFETY: the tuple is fresh and sized for one item, and
            // `PyTuple_SetItem` steals that reference.
            capi::PyTuple_SetItem(tuple, 0, instance);
            tuple
        }
    };
    let result = unsafe { ask(function, args, c"onMigrate".as_ptr(), true) };
    // `Script::call` (`script.ipp:22-27`) discards the result.
    if !result.is_null() {
        unsafe { capi::dec_ref(result) };
    }
    // `call` returns `(result != NULL) || (okIfFunctionNull && function == NULL)`.
    let succeeded = !result.is_null() || function.is_null();
    if !succeeded && unsafe { !capi::PyErr_Occurred().is_null() } {
        // The reference's `printException` path prints the pending error in
        // extended form; the port uses the one print this crate uses, which
        // also clears it.
        unsafe { capi::PyErr_PrintEx(0) };
    }
    !succeeded
}

/// `Script::ask` (`script.cpp:1125-1210`) for a hook call: print a pending
/// error that is not the allowed-to-be-null miss, call the function if it is
/// callable against a tuple, and raise the reference's `TypeError` /
/// `ValueError` otherwise.
///
/// Returns the hook's result, a live object the caller owns one reference to,
/// or NULL. The caller's references to `function` and `args` are not touched
/// here: the reference INCREFs them around `ask` and DECREFs them on the way
/// out (`script.cpp:1200-1201`), a pairing the port lets its caller handle.
unsafe fn ask(
    function: *mut capi::PyObject,
    args: *mut capi::PyObject,
    error_prefix: *const c_char,
    ok_if_function_null: bool,
) -> *mut capi::PyObject {
    // There will have been an attribute error if `function` is NULL; print a
    // stack trace unless that miss is the one the caller allows.
    if unsafe { !capi::PyErr_Occurred().is_null() } && !(ok_if_function_null && function.is_null())
    {
        unsafe { capi::PyErr_PrintEx(0) };
    }
    let mut result: *mut capi::PyObject = std::ptr::null_mut();
    if !function.is_null() && !args.is_null() {
        if unsafe { capi::PyCallable_Check(function) } != 0 && unsafe { capi::py_tuple_check(args) }
        {
            // SAFETY: a callable function against a tuple.
            result = unsafe { capi::PyObject_CallObject(function, args) };
        } else {
            // SAFETY: `script.cpp:1188-1191`.
            unsafe {
                capi::PyErr_Format(
                    capi::PyExc_TypeError,
                    c"%sScript call attempted on a non-callable object or \
                      pArgs is not a tuple."
                        .as_ptr(),
                    error_prefix,
                );
            }
        }
    } else if args.is_null() || !ok_if_function_null {
        // SAFETY: `script.cpp:1194-1198`.
        unsafe {
            capi::PyErr_Format(
                capi::PyExc_ValueError,
                c"%sScript call attempted with a NULL function or argument \
                  pointers: pFunction is %p, pArgs is %p"
                    .as_ptr(),
                error_prefix,
                function,
                args,
            );
        }
    } else {
        // The function is NULL but allowed: clear the error finding it left.
        unsafe { capi::clear_error() };
    }
    result
}
