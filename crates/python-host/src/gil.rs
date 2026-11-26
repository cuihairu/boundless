//! The GIL bracket and the cross-thread callback queue.
//!
//! Reference: the lock is two globals and nothing else. `s_pMainThreadState`
//! is captured at init (`lib/pyscript/script.cpp:88,603`) and
//! `s_defaultContext` is a `THREADLOCAL( PyThreadState * )` (`:89`, one per
//! thread). `Script::acquireLock` is `PyEval_RestoreThread( s_defaultContext )`
//! (`:932-942`) and `Script::releaseLock` is `PyEval_SaveThread()` (`:949-958`).
//! Both are no-ops on a thread that carries no context -- `if
//! (s_defaultContext == NULL) return;` -- and `releaseLock` verifies the state
//! `PyEval_SaveThread` gave back is exactly the one installed, exiting with
//! `"releaseLock: default context is incorrect"` when it is not.
//!
//! The reference never releases the lock around a script call: `Script::call`
//! (`lib/pyscript/script.ipp:22-28`) hands straight to `Script::ask` with no
//! lock of its own, so a call runs with whatever GIL its caller already
//! holds. `PyEval_SaveThread` is reached only through the poller's idling
//! hooks -- `script.cpp:612-613` installs
//! `setMainThreadIdleFunctions( &releaseLock, &acquireLock )`, and
//! `event_poller.cpp:482,500` wrap the blocking `select` at `:496` in
//! `startMainThreadIdling`/`endMainThreadIdling`. `fini` reverts those to
//! no-ops (`:685`) before finalising.
//!
//! The second half is the port of `TaskManager::addMainThreadTask`
//! (`lib/cstdmf/bgtask_manager.cpp:709-715`, "@note any thread can access
//! this.", a mutex-guarded list) drained by `TaskManager::tick` (`:744-749`,
//! "should only be called by the owning thread"). A callback the native side
//! wants run on the script thread is posted from any thread and executed by
//! the thread that owns the GIL, one pass at a time.
//!
//! `Script::fini` does not touch this queue: the reference purges it in an
//! explicit `Script::clearTimers()` (`lib/pyscript/py_callback.cpp:81-91`,
//! "this has to be called at a different time than fini, that's why it's a
//! separate method"), reached from the app's shutdown before anything else.
//! [`clear`] is that step here, and it is what makes a posted-but-never-run
//! callback a release instead of a leak.

use std::collections::VecDeque;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::capi;

/// Why a thread could not be given the scripting context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockError {
    /// No interpreter is up: `PyThreadState_Get` is a C API call.
    NoInterpreter,
    /// The thread already carries a context. The reference exits with
    /// `"trying to initialise scripting when already initialised"`
    /// (`script.cpp:831-834`); the port reports it instead.
    AlreadyInstalled,
}

/// Why the lock could not be given back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseError {
    /// `PyEval_SaveThread` handed back a thread state other than the one this
    /// thread was given. The reference exits with `"releaseLock: default
    /// context is incorrect"` (`script.cpp:955-956`).
    WrongContext,
}

/// The queue of callbacks waiting for the script thread, the reference's
/// `fgTaskList_` under `fgTaskListMutex_`.
struct Queue {
    callbacks: VecDeque<Callback>,
}

/// One posted callback. It carries no `PyRef`: the reference's
/// `Py_XINCREF` of a queued function is only right because the reference
/// queues a `PyObject *`, and `PyRef` is deliberately `!Send`. A posted
/// closure therefore takes its objects from the interpreter when it runs, on
/// the thread that owns the GIL.
type Callback = Box<dyn FnOnce() + Send + 'static>;

fn queue() -> MutexGuard<'static, Queue> {
    static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();
    QUEUE
        .get_or_init(|| {
            Mutex::new(Queue {
                callbacks: VecDeque::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The number of callbacks waiting, kept in step with the queue under its
/// lock. It feeds the idle-frame fast path of [`process`]: the poll-side
/// steady state is a pass over an empty queue, and the reference's tick
/// (`bgtask_manager.cpp:744-749`) pays one lock for that check per frame --
/// here an idle frame pays one atomic load and never takes the mutex.
/// Writers update it while holding the queue lock, so whenever the lock is
/// not held the count and the queue agree; a reader that races a poster may
/// transiently see the older count, which can only delay a callback to the
/// next pass -- the reference's own "always wait at least 1 frame" rule.
static QUEUED: AtomicUsize = AtomicUsize::new(0);

// The thread-local context `Script::s_defaultContext` stands for: one entry
// per thread, null until the thread that owns the interpreter installs one.
thread_local! {
    static CONTEXT: std::cell::Cell<*mut c_void> = const {
        std::cell::Cell::new(std::ptr::null_mut())
    };
}

/// Whether this thread carries the scripting context.
pub fn has_context() -> bool {
    !CONTEXT.with(std::cell::Cell::get).is_null()
}

/// `Script::init`'s context capture (`script.cpp:603-604`,
/// `s_pMainThreadState = PyThreadState_Get(); s_defaultContext =
/// s_pMainThreadState;`), for the thread that brings the interpreter up:
/// record the current thread state so [`acquire_lock`] and [`release_lock`]
/// have something to restore.
///
/// The reference's `Script::initThread` (`script.cpp:829-882`) is a wider
/// thing: a thread bootstrap that mints its caller a fresh thread state
/// through `Py_NewInterpreter` or `PyThreadState_New` and ends with
/// `acquireLock` (`:880-881`). It has no caller outside `Script` in the
/// reference, and this port has no equivalent of it, because one thread owns
/// the interpreter and subinterpreter swapping is the server's reload path.
pub fn init_thread() -> Result<(), LockError> {
    if unsafe { capi::Py_IsInitialized() } == 0 {
        return Err(LockError::NoInterpreter);
    }
    // SAFETY: the interpreter is up, so the thread state is valid to hold.
    let state = unsafe { capi::PyThreadState_Get() };
    CONTEXT.with(|context| {
        if !context.get().is_null() {
            return Err(LockError::AlreadyInstalled);
        }
        if state.is_null() {
            return Err(LockError::NoInterpreter);
        }
        context.set(state);
        Ok(())
    })
}

/// Drop this thread's context and give the GIL back: the pair the reference
/// sets up through `BWConcurrency::setMainThreadIdleFunctions` and reverts to
/// no-ops in `Script::fini` (`script.cpp:612-613,685`). After this the thread
/// carries no context, and both lock calls are no-ops on it.
pub fn fini_thread() {
    CONTEXT.with(|context| {
        if context.get().is_null() {
            return;
        }
        // SAFETY: the interpreter is up and this thread owns the GIL.
        unsafe { capi::PyEval_SaveThread() };
        context.set(std::ptr::null_mut());
    })
}

/// `Script::acquireLock` (`script.cpp:932-942`): take the GIL on this
/// thread's own context. A thread with no context does nothing.
///
/// There is deliberately no check that the thread currently holds no state
/// before restoring: `PyThreadState_Get` is a fatal error while the state is
/// null, and the reference records the same dead end right where the check
/// would go (`script.cpp:939-941`, "there doesn't appear to be a good way to
/// assert this here").
///
/// # Safety
///
/// Callers must not hold the GIL already: `PyEval_RestoreThread` makes the
/// calling thread the owner of the context it names.
pub unsafe fn acquire_lock() {
    let context = CONTEXT.with(std::cell::Cell::get);
    if context.is_null() {
        return;
    }
    // SAFETY: the context is a thread state CPython gave this thread.
    unsafe { capi::PyEval_RestoreThread(context) };
}

/// `Script::releaseLock` (`script.cpp:949-958`): give the GIL back and check
/// the state that came off is the one this thread was given.
///
/// # Safety
///
/// Callers must own the GIL while the interpreter is up.
pub unsafe fn release_lock() -> Result<(), ReleaseError> {
    let context = CONTEXT.with(std::cell::Cell::get);
    if context.is_null() {
        return Ok(());
    }
    // SAFETY: this thread owns the GIL, so the state it saves is valid.
    let saved = unsafe { capi::PyEval_SaveThread() };
    if saved != context {
        return Err(ReleaseError::WrongContext);
    }
    Ok(())
}

/// Post a callback for the script thread: `TaskManager::addMainThreadTask`,
/// reachable from any thread (`bgtask_manager.cpp:709-715`). The callback
/// runs on the thread that owns the GIL, during the next [`process`].
pub fn post(callback: impl FnOnce() + Send + 'static) {
    // The allocation lands before the lock, so the critical section is the
    // push and the count step, not the heap.
    let callback: Callback = Box::new(callback);
    let mut queue = queue();
    queue.callbacks.push_back(callback);
    QUEUED.fetch_add(1, Ordering::Release);
}

/// The number of callbacks waiting for the script thread. Exact whenever no
/// post or pass is in flight; a caller racing them may see the older count.
pub fn pending() -> usize {
    QUEUED.load(Ordering::Acquire)
}

/// `TaskManager::tick` (`bgtask_manager.cpp:744-749`): run every waiting
/// callback in posted order on the calling thread, which must own the GIL.
///
/// A callback posted during this pass waits for the next one, the reference's
/// "always wait at least 1 frame" rule (`py_callback.cpp:161-162`): the pass
/// runs the snapshot it took, and anything queued from inside a callback
/// stays queued.
///
/// A callback that aborts is contained: the rest of the pass still runs, so an
/// aborting callback never strands the ones posted behind it or leaves the
/// queue holding a callback that will never be visited. The reference has no
/// aborting callbacks (its handlers `MF_EXIT` rather than unwind), so this is
/// a port-side hardening, not a change of its observable ordering.
///
/// Returns how many callbacks completed.
///
/// # Safety
///
/// The calling thread must own the GIL: every callback may touch the
/// interpreter.
pub unsafe fn process() -> usize {
    // The idle-frame fast path: nothing queued means nothing to take, and
    // the frame pays an atomic load instead of the queue lock. A post that
    // races this load holds the count step until it can take the lock, so
    // the worst race outcome is this pass returns 0 and the callback runs
    // next pass -- the reference's own ordering rule.
    if QUEUED.load(Ordering::Acquire) == 0 {
        return 0;
    }
    // The pass runs the snapshot it took out of the queue, so a callback that
    // posts another does not run in this pass and cannot deadlock on the
    // queue lock; and the lock is free by the time the snapshot runs, so an
    // aborting callback cannot poison it for its neighbours.
    let due = {
        let mut queue = queue();
        // The take empties the deque, so the count that agrees with the
        // queue at this point is zero: posters that ran before us under the
        // lock are all in `due`, posters that run after us re-count from 0.
        let due = std::mem::take(&mut queue.callbacks);
        QUEUED.store(0, Ordering::Release);
        due
    };
    let mut completed = 0;
    for callback in due {
        // A callback that aborts is logged on stderr and the pass goes on to
        // the ones posted behind it.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)).is_err() {
            crate::output_writer::handle_write("a posted script callback aborted\n", true);
            continue;
        }
        completed += 1;
    }
    completed
}

/// `Script::clearTimers` (`py_callback.cpp:81-91`): drop every waiting
/// callback without running it. This is what keeps a posted-but-never-run
/// callback from outliving the script system.
///
/// Returns how many callbacks were dropped.
pub fn clear() -> usize {
    let mut queue = queue();
    let dropped = queue.callbacks.len();
    queue.callbacks.clear();
    QUEUED.store(0, Ordering::Release);
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    // The queue is process-wide and the harness runs these tests in parallel
    // threads, so the ones that count what is queued take this test-only lock
    // in turn instead of racing each other's posts.
    static QUEUE_GUARD: Mutex<()> = Mutex::new(());

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        QUEUE_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Without an interpreter there is no thread state to install, and the
    /// lock calls stay no-ops on a context-free thread.
    #[test]
    fn installing_without_an_interpreter_refuses() {
        assert_eq!(init_thread(), Err(LockError::NoInterpreter));
        assert!(!has_context());
        assert!(unsafe { release_lock() }.is_ok());
        unsafe { acquire_lock() };
    }

    /// A thread with no context acquires and releases without complaint, the
    /// reference's `if (s_defaultContext == NULL) return;` arms.
    #[test]
    fn the_lock_is_a_no_op_without_a_context() {
        unsafe {
            acquire_lock();
        }
        assert!(unsafe { release_lock() }.is_ok());
    }

    /// `fini_thread` on a context-free thread is a no-op, never a save of a
    /// state that was never installed.
    #[test]
    fn finalising_a_context_free_thread_is_harmless() {
        fini_thread();
        assert!(!has_context());
    }

    /// Clearing an empty queue drops nothing, which is how a host can call it
    /// unconditionally at shutdown.
    #[test]
    fn clearing_an_empty_queue_drops_nothing() {
        let _guard = guard();
        assert_eq!(pending(), 0);
        assert_eq!(clear(), 0);
        assert_eq!(unsafe { process() }, 0);
    }

    /// A callback posted from a thread that then exits still waits for the
    /// script thread: it is the queue's to run, not the thread's to keep.
    #[test]
    fn a_callback_survives_the_posting_thread_exiting() {
        let _guard = guard();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spawner = ran.clone();
        let poster = std::thread::spawn(move || {
            post(move || {
                spawner.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        });
        assert!(poster.join().is_ok());
        // The thread is gone and the callback still is.
        assert_eq!(pending(), 1);
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(unsafe { process() }, 1);
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// A callback that aborts does not lose the callbacks behind it: the pass
    /// takes its snapshot out of the queue first, so the abort is contained to
    /// its own callback and nothing is left in the queue that will never be
    /// visited.
    #[test]
    fn an_aborting_callback_does_not_strand_the_rest() {
        let _guard = guard();
        let order = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
        let first = order.clone();
        let third = order.clone();
        post(move || {
            first.lock().unwrap().push(1);
        });
        post(|| panic!("a callback raises"));
        post(move || {
            third.lock().unwrap().push(3);
        });
        // The middle one aborts; the pass goes on to the third and reports
        // two completions out of three.
        assert_eq!(unsafe { process() }, 2);
        assert_eq!(*order.lock().unwrap(), vec![1, 3]);
        assert_eq!(pending(), 0);
    }

    /// A callback posted from inside a callback waits for the next pass, the
    /// reference's `addTimer` "always wait at least 1 frame" rule.
    #[test]
    fn a_callback_posted_during_a_pass_waits() {
        let _guard = guard();
        let ticks = std::sync::Arc::new(Mutex::new(0u8));
        let nested = ticks.clone();
        post(move || {
            let count = *nested.lock().unwrap();
            *nested.lock().unwrap() += 1;
            if count == 0 {
                let inner = nested.clone();
                post(move || {
                    *inner.lock().unwrap() += 1;
                });
            }
        });
        assert_eq!(unsafe { process() }, 1);
        // The nested post is still waiting, not already run.
        assert_eq!(unsafe { process() }, 1);
    }

    /// A poisoned queue lock recovers for the next caller, so one aborting
    /// callback cannot lock the rest of the process out of the queue.
    #[test]
    fn a_poisoned_queue_lock_recovers() {
        let _guard = guard();
        let poisoner = std::thread::spawn(|| {
            let _guard = queue();
            panic!("poison the callback queue");
        });
        assert!(poisoner.join().is_err());
        assert_eq!(pending(), 0);
        assert_eq!(clear(), 0);
    }

    /// The test lock recovers the same way, so one aborting neighbour cannot
    /// wedge the rest of the queue's tests for the run. The poisoner takes the
    /// lock on its own: a caller holding it while a neighbour tries to take it
    /// would block, not poison.
    #[test]
    fn a_poisoned_test_lock_recovers() {
        let poisoner = std::thread::spawn(|| {
            let _guard = QUEUE_GUARD.lock();
            panic!("poison the test lock");
        });
        assert!(poisoner.join().is_err());
        // The next caller still gets the lock rather than a panic.
        let recovered = guard();
        assert_eq!(pending(), 0);
        drop(recovered);
    }
}
