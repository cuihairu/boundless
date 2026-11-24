//! The timer controller (`server/cellapp/timer_controller.*`): the
//! `Entity.addTimer` face — a `DOMAIN_REAL` controller holding
//! `start_`/`interval_` game ticks, so it moves between cells with its
//! entity and is cancellable with `Entity.cancel(controllerID)`. The
//! once-off form (`interval == 0`) auto-cancels through the handler's
//! release (`timer_controller.cpp:209-214`).
//!
//! The BW-08-02-04 acceptance is "退出或迁移后状态满足基线": the state
//! is just the two ticks (`writeRealToStream`/`readRealFromStream`,
//! `timer_controller.cpp:160-176`), and the restart clamps a start that
//! passed while the entity was in flight to "now"
//! (`timer_controller.cpp:105-112`) so the next callback lands on the
//! current tick instead of a dead deadline.

use std::cell::RefCell;
use std::rc::Rc;

use foundation::stream::{BinaryStream, StreamError};

use crate::controllers::ControllerId;
use crate::entity_dispatch::EntityId;
use crate::movement::ScriptCallback;
use crate::timers::{TimeQueue, TimerHandle, TimerHandler};

/// One `TimerController` outcome, in firing order: the `onTimer` script
/// callbacks, the `New` rounding warning, and the once-off auto-cancel.
#[derive(Clone, Debug, PartialEq)]
pub enum TimerEvent {
    /// `standardCallback("onTimer")` (`timer_controller.cpp:197-204`).
    Callback(ScriptCallback),
    /// The `New` warning: a non-zero repeat offset below half a tick
    /// rounds up to 1 (`timer_controller.cpp:55-60`).
    RepeatRoundedUp,
    /// `onHandlerRelease` (`timer_controller.cpp:209-214`): the handler
    /// released the controller — the once-off timer has fired and the
    /// controller cancelled itself ("this call deletes this object").
    AutoCancelled,
}

/// The controller state, shared between the public handle and the
/// time-queue handler. The reference's `Handler` holds a raw
/// `TimerController*` that `stopReal` detaches; the borrow-checked face
/// keeps the state in an `Rc<RefCell<>>` and gates the forwarding on the
/// same `pHandler_ != NULL` flag.
struct TimerCore {
    /// `entity().id()`.
    entity_id: EntityId,
    /// `controllerID_`.
    id: ControllerId,
    /// `userArg_`.
    user_arg: i32,
    /// `start_`: the tick of the next callback.
    start: u32,
    /// `interval_`: ticks between callbacks; zero is the once-off form.
    interval: u32,
    /// `timerHandle_`.
    timer_handle: TimerHandle<u32>,
    /// `pHandler_ != NULL`.
    handler_attached: bool,
    /// The controller was cancelled (`Controllers::delController` ran).
    cancelled: bool,
    events: Vec<TimerEvent>,
}

/// The time-queue handler (`TimerController::Handler`).
struct ControllerHandler {
    core: Rc<RefCell<TimerCore>>,
}

impl TimerHandler<u32> for ControllerHandler {
    /// `Handler::handleTimeout` (`timer_controller.cpp:225-236`): forward
    /// to the controller unless `stopReal` detached it.
    fn handle_timeout(&self, _handle: TimerHandle<u32>, _user: usize) {
        if !self.core.borrow().handler_attached {
            // WARNING_MSG( "TimerController::Handler::handleTimeout: "
            //     "pController_ is NULL" )
            return;
        }
        let (entity_id, id, user_arg, interval) = {
            let core = self.core.borrow();
            (core.entity_id, core.id, core.user_arg, core.interval)
        };
        let mut core = self.core.borrow_mut();
        // `start_ += interval_`, so the start stays correct across a
        // stream ("so it is correct if we are streamed across the
        // network").
        core.start += interval;
        core.events.push(TimerEvent::Callback(ScriptCallback {
            entity_id,
            method: "onTimer",
            controller_id: id,
            user_arg,
        }));
    }

    /// `Handler::onRelease` (`timer_controller.cpp:243-252`): the timer
    /// will never fire again — for a once-off this is the auto-cancel.
    fn on_release(&self, _handle: TimerHandle<u32>, _user: usize) {
        if self.core.borrow().handler_attached {
            TimerController {
                core: Rc::clone(&self.core),
            }
            .on_handler_release();
        }
    }
}

/// The `TimerController` handle. Methods take `&self` because the state
/// lives in the shared [`TimerCore`]; the reference mutates through the
/// same single object.
#[derive(Clone)]
pub struct TimerController {
    core: Rc<RefCell<TimerCore>>,
}

impl TimerController {
    /// `TimerController::TimerController( start, interval )`
    /// (`timer_controller.cpp:67-77`) — the raw tick form.
    pub fn new(
        id: ControllerId,
        entity_id: EntityId,
        user_arg: i32,
        start: u32,
        interval: u32,
    ) -> Self {
        Self {
            core: Rc::new(RefCell::new(TimerCore {
                entity_id,
                id,
                user_arg,
                start,
                interval,
                timer_handle: TimerHandle::default(),
                handler_attached: false,
                cancelled: false,
                events: Vec::new(),
            })),
        }
    }

    /// `TimerController::New` (`timer_controller.cpp:31-64`), the
    /// `Entity.addTimer` factory: seconds to ticks with the half-tick
    /// round, `initialTicks <= 0` rounds up to 1, and a non-zero repeat
    /// below half a tick warns and rounds up to 1.
    #[allow(clippy::too_many_arguments)]
    pub fn add_timer(
        id: ControllerId,
        entity_id: EntityId,
        user_arg: i32,
        now: u32,
        hertz: f32,
        initial_offset: f32,
        repeat_offset: f32,
    ) -> Self {
        // (GameTime)( initialOffset * updateHertz() + 0.5f ); a negative
        // offset saturates to 0 here, which the `<= 0` round-up below
        // folds to 1 like the reference's `int(initialTicks) <= 0`.
        let mut initial_ticks = (initial_offset * hertz + 0.5) as u32;
        if initial_ticks == 0 {
            // WARNING_MSG("Rounding up initial offset to 1") is commented
            // out in the reference; the round-up itself stays.
            initial_ticks = 1;
        }
        // `initialTicks += CellApp::instance().time()`.
        let start = initial_ticks.saturating_add(now);

        let mut repeat_rounded = false;
        let mut interval = 0u32;
        if repeat_offset > 0.0 {
            interval = (repeat_offset * hertz + 0.5) as u32;
            if interval < 1 {
                // WARNING_MSG( "Rounding up repeatTicks to 1" ).
                repeat_rounded = true;
                interval = 1;
            }
        }

        let controller = Self::new(id, entity_id, user_arg, start, interval);
        if repeat_rounded {
            controller
                .core
                .borrow_mut()
                .events
                .push(TimerEvent::RepeatRoundedUp);
        }
        controller
    }

    /// `startReal` (`timer_controller.cpp:100-114`): attach the handler
    /// and queue the timer. On a restart after offload (`isInitialStart`
    /// false) a start that already passed is clamped to `now`, so the
    /// callback fires on the current tick instead of a dead deadline.
    pub fn start(&self, is_initial_start: bool, now: u32, queue: &TimeQueue) {
        debug_assert!(
            !self.core.borrow().timer_handle.is_set(),
            "MF_ASSERT( !timerHandle_.isSet() ): already running"
        );

        // The offload clamp: "if we were offloaded just as our timer was
        // going off - we set the timer start time to be now".
        if !is_initial_start {
            let mut core = self.core.borrow_mut();
            if core.start < now {
                core.start = now;
            }
        }

        // `pHandler_ = new Handler( this )`.
        let handler: Rc<dyn TimerHandler<u32>> = Rc::new(ControllerHandler {
            core: Rc::clone(&self.core),
        });
        self.core.borrow_mut().handler_attached = true;
        let (start, interval) = {
            let core = self.core.borrow();
            (core.start, core.interval)
        };
        let handle = queue.add(start, interval, handler, 0, "TimerController");
        self.core.borrow_mut().timer_handle = handle;
    }

    /// `stopReal` (`timer_controller.cpp:116-131`): detach the handler
    /// first so the cancel cannot re-enter, then cancel the queued timer.
    /// This is the offload stop (`Controllers::stopReals`), not a
    /// cancel — the controller stays alive to be restarted.
    pub fn stop(&self) {
        debug_assert!(
            self.core.borrow().timer_handle.is_set(),
            "MF_ASSERT( timerHandle_.isSet() ): not running"
        );
        if self.core.borrow().handler_attached {
            // `pHandler_->pController( NULL ); pHandler_ = nullptr;`
            self.core.borrow_mut().handler_attached = false;
        }
        // The handle is cloned out so no `TimerCore` borrow is held
        // across `cancel`: the queue's release path re-enters the
        // handler. The reference's `timerHandle_.cancel()` clears the
        // member's node pointer — store the now-unset handle back.
        let mut handle = self.core.borrow().timer_handle.clone();
        handle.cancel();
        self.core.borrow_mut().timer_handle = handle;
    }

    /// `Controller::cancel` — an unattached controller is a no-op
    /// (`isAttached()`); otherwise `Controllers::delController` runs
    /// `stopReal`.
    pub fn cancel(&self) {
        if !self.core.borrow().handler_attached {
            return;
        }
        self.core.borrow_mut().cancelled = true;
        self.stop();
    }

    /// `handleTimeout` (`timer_controller.cpp:180-204`): advance `start_`
    /// and fire the `onTimer` callback. Driven by the queue; also callable
    /// directly for tests.
    pub fn handle_timeout(&self) {
        let (entity_id, id, user_arg, interval) = {
            let core = self.core.borrow();
            (core.entity_id, core.id, core.user_arg, core.interval)
        };
        let mut core = self.core.borrow_mut();
        core.start += interval;
        core.events.push(TimerEvent::Callback(ScriptCallback {
            entity_id,
            method: "onTimer",
            controller_id: id,
            user_arg,
        }));
    }

    /// `onHandlerRelease` (`timer_controller.cpp:206-214`): the queue
    /// released the handler — detach and cancel. For a once-off timer
    /// this is the auto-cancel that follows its single callback.
    pub fn on_handler_release(&self) {
        let attached = self.core.borrow().handler_attached;
        debug_assert!(attached, "MF_ASSERT( pHandler_ )");
        if attached {
            self.core
                .borrow_mut()
                .events
                .push(TimerEvent::AutoCancelled);
            self.cancel();
        }
    }

    /// `start_`.
    pub fn start_tick(&self) -> u32 {
        self.core.borrow().start
    }

    /// `interval_`.
    pub fn interval(&self) -> u32 {
        self.core.borrow().interval
    }

    /// Whether `Controllers::delController` ran.
    pub fn is_cancelled(&self) -> bool {
        self.core.borrow().cancelled
    }

    /// Whether the timer is queued (`timerHandle_.isSet()`).
    pub fn is_running(&self) -> bool {
        self.core.borrow().timer_handle.is_set()
    }

    /// Drain the recorded events, in firing order.
    pub fn take_events(&self) -> Vec<TimerEvent> {
        std::mem::take(&mut self.core.borrow_mut().events)
    }

    /// `writeRealToStream` (`timer_controller.cpp:160-166`): `start_` and
    /// `interval_`.
    pub fn write_real_to_stream(&self, stream: &mut BinaryStream) {
        let core = self.core.borrow();
        stream.write_u32(core.start);
        stream.write_u32(core.interval);
    }

    /// `readRealFromStream` (`timer_controller.cpp:168-176`).
    pub fn read_real_from_stream(&self, stream: &mut BinaryStream) -> Result<(), StreamError> {
        let start = stream.read_u32()?;
        let interval = stream.read_u32()?;
        let mut core = self.core.borrow_mut();
        core.start = start;
        core.interval = interval;
        Ok(())
    }
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const HERTZ: f32 = 10.0;

    // -------------------------------------------------------------------
    // TDD: BW-08-02-04 定时器与寻路控制器接续 — 退出或迁移后状态满足基线
    // -------------------------------------------------------------------

    /// The `New` arithmetic: seconds to ticks with the half-tick round, a
    /// non-positive initial offset rounds up to 1, a sub-half-tick repeat
    /// warns and rounds up to 1, and a zero repeat stays once-off.
    #[test]
    fn add_timer_rounds_offsets_to_ticks() {
        let controller = TimerController::add_timer(4, 7, 9, 100, HERTZ, 1.25, 0.5);
        // 1.25s * 10 + 0.5 = 13 ticks, plus now = 113.
        assert_eq!(controller.start_tick(), 113);
        assert_eq!(controller.interval(), 5);

        // A sub-tick initial offset rounds up to 1 (start = now + 1).
        let controller = TimerController::add_timer(1, 7, 0, 100, HERTZ, 0.04, 0.0);
        assert_eq!(controller.start_tick(), 101);
        assert_eq!(controller.interval(), 0);

        // A negative initial offset rounds up the same way.
        let controller = TimerController::add_timer(1, 7, 0, 100, HERTZ, -2.0, 0.0);
        assert_eq!(controller.start_tick(), 101);

        // A non-zero repeat below half a tick warns and rounds up.
        let controller = TimerController::add_timer(1, 7, 0, 100, HERTZ, 1.0, 0.04);
        assert_eq!(controller.interval(), 1);
        assert_eq!(controller.take_events(), vec![TimerEvent::RepeatRoundedUp]);

        // A negative repeat offset is not a repeat: the once-off form.
        let controller = TimerController::add_timer(1, 7, 0, 100, HERTZ, 1.0, -0.5);
        assert_eq!(controller.interval(), 0);
        assert!(controller.take_events().is_empty());
    }

    /// The once-off form: exactly one `onTimer`, then the handler release
    /// auto-cancels the controller.
    #[test]
    fn a_once_off_timer_fires_once_and_auto_cancels() {
        let queue = TimeQueue::new();
        let controller = TimerController::add_timer(3, 7, 5, 10, HERTZ, 1.0, 0.0);
        controller.start(true, 10, &queue);
        assert!(controller.is_running());

        // Not due yet.
        assert_eq!(queue.process(19), 0);
        assert!(controller.take_events().is_empty());
        assert!(!controller.is_cancelled());

        // Fire once; the once-off auto-cancel follows the callback.
        assert_eq!(queue.process(20), 1);
        assert_eq!(
            controller.take_events(),
            vec![
                TimerEvent::Callback(ScriptCallback {
                    entity_id: 7,
                    method: "onTimer",
                    controller_id: 3,
                    user_arg: 5,
                }),
                TimerEvent::AutoCancelled,
            ]
        );
        assert!(controller.is_cancelled());

        // Nothing ever fires again.
        assert_eq!(queue.process(1000), 0);
        assert!(controller.take_events().is_empty());
    }

    /// The repeating form: `onTimer` at `start_`, `start_ + interval_`,
    /// ... and `start_` advances with every fire so an offload mid-flight
    /// streams the right next tick.
    #[test]
    fn a_repeating_timer_fires_every_interval() {
        let queue = TimeQueue::new();
        let controller = TimerController::add_timer(2, 7, 0, 100, HERTZ, 0.5, 0.5);
        assert_eq!(controller.start_tick(), 105);
        assert_eq!(controller.interval(), 5);
        controller.start(true, 100, &queue);

        assert_eq!(queue.process(104), 0);
        assert_eq!(queue.process(105), 1);
        assert_eq!(controller.start_tick(), 110);
        assert_eq!(queue.process(109), 0);
        assert_eq!(queue.process(110), 1);
        assert_eq!(queue.process(115), 1);
        assert_eq!(controller.start_tick(), 120);

        let callbacks = controller
            .take_events()
            .into_iter()
            .filter(|event| matches!(event, TimerEvent::Callback(_)))
            .count();
        assert_eq!(callbacks, 3);
        assert!(!controller.is_cancelled());
    }

    /// The offload stop (`stopReal`): the timer detaches without
    /// cancelling the controller, and nothing fires afterwards.
    #[test]
    fn stop_detaches_without_cancelling() {
        let queue = TimeQueue::new();
        let controller = TimerController::add_timer(1, 7, 0, 0, HERTZ, 2.0, 1.0);
        controller.start(true, 0, &queue);
        assert_eq!(queue.process(20), 1);
        assert_eq!(controller.take_events().len(), 1);

        controller.stop();
        assert!(!controller.is_running());
        assert!(!controller.is_cancelled());
        // The stop's release finds the handler detached: no auto-cancel.
        assert!(controller.take_events().is_empty());

        assert_eq!(queue.process(500), 0);
        assert!(controller.take_events().is_empty());
    }

    /// `Entity.cancel`: the queued timer detaches and the controller
    /// never fires again. A cancel on a never-started controller is the
    /// reference's `isAttached()` no-op.
    #[test]
    fn an_external_cancel_stops_the_timer() {
        let queue = TimeQueue::new();
        let controller = TimerController::add_timer(6, 7, 1, 0, HERTZ, 1.0, 1.0);
        controller.start(true, 0, &queue);

        controller.cancel();
        assert!(controller.is_cancelled());
        assert!(!controller.is_running());

        assert_eq!(queue.process(100), 0);
        assert!(controller.take_events().is_empty());

        // The unattached no-op face.
        let fresh = TimerController::new(8, 7, 0, 5, 5);
        fresh.cancel();
        assert!(!fresh.is_cancelled());
        assert!(fresh.take_events().is_empty());
    }

    /// The BW-08-02-04 acceptance face: a timer streamed across an
    /// offload restarts with its baseline — a start that passed while the
    /// entity was in flight is clamped to "now" (`startReal`'s
    /// `!isInitialStart` branch), and the repeat continues from there.
    #[test]
    fn offload_restart_clamps_a_passed_start_to_now() {
        // Offload at tick 100 with the first callback due at 105.
        let controller = TimerController::add_timer(4, 7, 8, 100, HERTZ, 0.5, 0.5);
        assert_eq!(controller.start_tick(), 105);
        let mut stream = BinaryStream::new();
        controller.write_real_to_stream(&mut stream);
        stream.rewind();

        // ...the entity arrives at tick 400.
        let restored = TimerController::new(4, 7, 8, 0, 0);
        restored.read_real_from_stream(&mut stream).unwrap();
        assert_eq!(restored.start_tick(), 105);
        assert_eq!(restored.interval(), 5);

        let queue = TimeQueue::new();
        restored.start(false, 400, &queue);
        assert_eq!(restored.start_tick(), 400, "the clamp");
        assert_eq!(queue.process(400), 1);
        assert_eq!(restored.start_tick(), 405);
        assert_eq!(queue.process(404), 0);
        assert_eq!(queue.process(405), 1);
        assert_eq!(restored.take_events().len(), 2);
    }

    /// The clamp only applies to a restart: an initial start keeps its
    /// start tick even when "now" is already past it, and a restart whose
    /// start is still in the future is left alone.
    #[test]
    fn the_clamp_is_restart_only_and_future_starts_stay() {
        let queue = TimeQueue::new();
        let controller = TimerController::add_timer(1, 7, 0, 100, HERTZ, 0.5, 0.0);
        // Initial start at "now" = 400: no clamp.
        controller.start(true, 400, &queue);
        assert_eq!(controller.start_tick(), 105);
        assert_eq!(queue.process(105), 1);

        // A restart with the start still ahead of "now" is untouched.
        let controller = TimerController::add_timer(1, 7, 0, 100, HERTZ, 0.5, 0.0);
        let queue = TimeQueue::new();
        controller.start(false, 103, &queue);
        assert_eq!(controller.start_tick(), 105);
        assert_eq!(queue.process(104), 0);
        assert_eq!(queue.process(105), 1);
    }

    /// The state round-trip is exactly the two ticks; a truncated stream
    /// refuses.
    #[test]
    fn stream_roundtrip_and_truncation() {
        let controller = TimerController::add_timer(9, 7, 3, 100, HERTZ, 1.5, 0.25);
        let mut stream = BinaryStream::new();
        controller.write_real_to_stream(&mut stream);
        stream.rewind();

        let restored = TimerController::new(9, 7, 3, 0, 0);
        restored.read_real_from_stream(&mut stream).unwrap();
        assert_eq!(restored.start_tick(), controller.start_tick());
        assert_eq!(restored.interval(), controller.interval());

        stream.rewind();
        let mut truncated = BinaryStream::new();
        // One byte short: two u32s need eight bytes.
        for _ in 0..7 {
            truncated.write_u8(stream.read_u8().unwrap());
        }
        truncated.rewind();
        let restored = TimerController::new(9, 7, 3, 0, 0);
        assert!(restored.read_real_from_stream(&mut truncated).is_err());
    }

    /// A `start()` on a running timer asserts in the reference
    /// (`MF_ASSERT( !timerHandle_.isSet() )`); the port records the same
    /// face as a debug assertion. `handle_timeout` and
    /// `on_handler_release` are reachable directly, matching the
    /// reference's own unit-callable methods.
    #[test]
    fn direct_handle_timeout_advances_and_fires() {
        let controller = TimerController::new(5, 7, 2, 50, 10);
        controller.handle_timeout();
        assert_eq!(controller.start_tick(), 60);
        assert_eq!(
            controller.take_events(),
            vec![TimerEvent::Callback(ScriptCallback {
                entity_id: 7,
                method: "onTimer",
                controller_id: 5,
                user_arg: 2,
            })]
        );

        // `on_handler_release` on a detached handler is the reference's
        // MF_ASSERT face; attached, it cancels.
        let queue = TimeQueue::new();
        let controller = TimerController::add_timer(5, 7, 2, 0, HERTZ, 1.0, 0.0);
        controller.start(true, 0, &queue);
        controller.on_handler_release();
        assert!(controller.is_cancelled());
        assert!(!controller.is_running());
    }
}
