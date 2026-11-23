//! The per-frame counter `ProfileVal`
//! (`cstdmf/profile.hpp:80-242`, `cstdmf/profile.cpp:23-184`) and the
//! watcher leaves that expose it.
//!
//! A profile counts a named piece of work: `start`/`stop` pairs feed the
//! wall-clock fields (`lastTime`/`sumTime`), the per-stop payload
//! (`lastQuantity`/`sumQuantity`) and the call count (`count`), while the
//! "internal time" fields (`lastIntTime`/`sumIntTime`) track only the
//! stretches where this profile was the innermost running frame of its
//! group — the group closes the interrupted frame's internal clock on
//! every switch (`profile.hpp:89-113` start, `:119-148` stop). The fields
//! are the watcher-visible truth the acceptance pins: 同负载下关键字段
//! 与原版吻合.
//!
//! [`ProfileValHandle`] wraps one value in the `Rc<RefCell<_>>` the group
//! tree shares and carries the operations, because `start`/`stop` must
//! push `self` onto the group's stack — a borrow of the value alone
//! cannot produce that handle. The reference methods this maps to are
//! `ProfileVal::start`/`stop`/`stop(filename, line)`/`reset` and the
//! `operator<<`/`operator>>` stream pair.

use std::cell::RefCell;
use std::rc::Rc;

use watcher::{DirectoryWatcher, Mode, Watcher};

use crate::nice_time::nice_time;
use crate::profile_group::ProfileGroup;
use crate::resetter::profile_group_resetter;
use crate::{stamps_to_seconds, warning_period};
use foundation::time::stamp_now;

/// The counter state of one named piece of work — the reference's public
/// fields (`profile.hpp:219-231`). Arithmetic is `u64` wrapping, the way
/// `TimeStamp` wraps.
pub struct ProfileVal {
    /// `name_`: the watcher label and the summary text's name.
    pub name: String,
    /// `lastTime_`: the last completed start→stop stretch, or the start
    /// stamp while running (`lastTime()` reads it as 0 then,
    /// `profile.hpp:203-206`).
    pub last_time: u64,
    /// `sumTime_`: the total of every completed start→stop stretch.
    pub sum_time: u64,
    /// `lastIntTime_`: the last innermost-running stretch of this frame.
    pub last_int_time: u64,
    /// `sumIntTime_`: the total of every innermost-running stretch.
    pub sum_int_time: u64,
    /// `lastQuantity_`: the last `stop` payload.
    pub last_quantity: u32,
    /// `sumQuantity_`: the total of every `stop` payload.
    pub sum_quantity: u32,
    /// `count_`: the number of completed stops.
    pub count: u32,
    /// `inProgress_`: start-minus-stop depth.
    pub in_progress: i32,
    pub(crate) group: std::rc::Weak<RefCell<ProfileGroup>>,
}

impl ProfileVal {
    /// `ProfileVal::reset()` (`profile.hpp:180-190`): zero every counter.
    /// The plain reset — the stream-in path that also triggers the
    /// resetter is [`ProfileValHandle::reset_from_stream`].
    pub fn reset(&mut self) {
        self.last_time = 0;
        self.sum_time = 0;
        self.last_int_time = 0;
        self.sum_int_time = 0;
        self.last_quantity = 0;
        self.sum_quantity = 0;
        self.count = 0;
        self.in_progress = 0;
    }

    /// `ProfileVal::running()` (`profile.hpp:197-200`).
    pub fn running(&self) -> bool {
        self.in_progress > 0
    }

    /// `lastTimeInSeconds()` … `sumIntTimeInSeconds()`
    /// (`profile.hpp:213-216`): the stamps fields at the current rate.
    pub fn last_time_in_seconds(&self) -> f64 {
        stamps_to_seconds(self.last_time)
    }
    /// `sumTimeInSeconds()`.
    pub fn sum_time_in_seconds(&self) -> f64 {
        stamps_to_seconds(self.sum_time)
    }
    /// `lastIntTimeInSeconds()`.
    pub fn last_int_time_in_seconds(&self) -> f64 {
        stamps_to_seconds(self.last_int_time)
    }
    /// `sumIntTimeInSeconds()`.
    pub fn sum_int_time_in_seconds(&self) -> f64 {
        stamps_to_seconds(self.sum_int_time)
    }

    /// `ProfileVal::isTooLong()` (`profile.hpp:170-174`): stopped, and
    /// the last stretch outlived the warning period.
    pub fn is_too_long(&self) -> bool {
        !self.running() && self.last_time > warning_period()
    }

    /// The `WARNING_MSG` body of the checked stop
    /// (`profile.hpp:161-164`): "%s:%d: Profile %s took %.2f seconds".
    pub fn too_long_message(&self, file: &str, line: u32) -> String {
        format!(
            "{file}:{line}: Profile {} took {:.2} seconds",
            self.name,
            self.last_time_in_seconds()
        )
    }

    /// `operator<<(ostream, ProfileVal)` (`profile.cpp:132-171`): the
    /// summary text a `summaries` watcher answers with. `now` stands in
    /// for the `timestamp()` the in-progress arm reads
    /// (`profile.cpp:134-135`).
    pub fn summary(&self, now: u64) -> String {
        let last_time = if self.in_progress != 0 {
            now.wrapping_sub(self.last_time)
        } else {
            self.last_time
        };

        let mut out = format!("Count: {}\n", self.count);
        if self.in_progress != 0 {
            out.push_str(&format!(" inProgress: {}\n", nice_time(last_time)));
        }
        if self.count == 0 {
            return out;
        }

        if self.sum_quantity != 0 {
            out.push_str(&format!(
                "Quantity: {} (last {}, avg {})\n",
                self.sum_quantity,
                self.last_quantity,
                self.sum_quantity as f64 / self.count as f64
            ));
        }

        out.push_str(&format!(
            "Time: {} (last {})\n",
            nice_time(self.sum_time),
            nice_time(last_time)
        ));

        if self.sum_time != 0 {
            out.push_str(&format!(
                "Time/Count: {}\n",
                nice_time(self.sum_time / self.count as u64)
            ));
            if self.sum_quantity != 0 {
                out.push_str(&format!(
                    "Time/Qty: {}\n",
                    nice_time(self.sum_time / self.sum_quantity as u64)
                ));
            }
        }
        out
    }
}

/// One value in the shared tree: the `Rc<RefCell<ProfileVal>>` the group,
/// its watcher leaves and the caller all hold.
#[derive(Clone)]
pub struct ProfileValHandle(pub(crate) Rc<RefCell<ProfileVal>>);

impl ProfileVal {
    /// `ProfileVal::ProfileVal(name, pGroup)` (`profile.cpp:23-41`):
    /// zeroed counters, the default group when none is given, and — for
    /// a named value — the registration into the group's tree
    /// (`profile.cpp:37-40`). Named `create` because the reference's
    /// constructor hands back the shared handle, not the bare value.
    pub fn create(name: &str, group: Option<&Rc<RefCell<ProfileGroup>>>) -> ProfileValHandle {
        let group = group
            .map(Rc::downgrade)
            .unwrap_or_else(|| Rc::downgrade(&ProfileGroup::default_group()));
        let value = ProfileVal {
            name: name.to_string(),
            last_time: 0,
            sum_time: 0,
            last_int_time: 0,
            sum_int_time: 0,
            last_quantity: 0,
            sum_quantity: 0,
            count: 0,
            in_progress: 0,
            group,
        };
        let handle = ProfileValHandle(Rc::new(RefCell::new(value)));
        if !name.is_empty() {
            handle
                .0
                .borrow()
                .group
                .upgrade()
                .expect("the group lives")
                .borrow_mut()
                .add(&handle);
        }
        handle
    }
}

impl ProfileValHandle {
    /// The shared value.
    pub fn value(&self) -> &Rc<RefCell<ProfileVal>> {
        &self.0
    }

    /// `ProfileVal::start()` (`profile.hpp:89-113`): take the wall clock
    /// from `now`. The group's innermost frame loses its internal clock
    /// to this one; the frame stack records the switch.
    pub fn start(&self, now: u64) {
        let group = self.0.borrow().group.upgrade().expect("the group lives");
        let mut group = group.borrow_mut();

        // Disable the existing internal profile
        // (profile.hpp:102-108). A re-entrant start closes its own
        // outer stretch here — the borrows below are scoped so the
        // self-overlap stays legal.
        if let Some(top) = group.stack().last() {
            let mut top = top.borrow_mut();
            top.last_int_time = now.wrapping_sub(top.last_int_time);
            top.sum_int_time = top.sum_int_time.wrapping_add(top.last_int_time);
        }

        // This profile is now the active internal profile
        // (profile.hpp:110-112).
        group.stack().push(Rc::clone(&self.0));

        let mut value = self.0.borrow_mut();
        if value.in_progress == 0 {
            value.last_time = now;
        }
        value.in_progress += 1;
        value.last_int_time = now;
    }

    /// `ProfileVal::stop(qty)` (`profile.hpp:119-148`): close the
    /// stretch, count the call and the payload, and hand the internal
    /// clock back to the frame above.
    ///
    /// # Panics
    /// When the group's frame stack does not top out at this value — the
    /// `MF_ASSERT( stack.back() == this )` (`profile.hpp:136`); profiles
    /// of one group run stack-like or not at all.
    pub fn stop(&self, now: u64, qty: u32) {
        let group = self.0.borrow().group.upgrade().expect("the group lives");
        let mut group = group.borrow_mut();

        // Close the wall-clock stretch and count the call and the
        // payload (profile.hpp:123-133).
        {
            let mut value = self.0.borrow_mut();
            value.in_progress -= 1;
            if value.in_progress == 0 {
                value.last_time = now.wrapping_sub(value.last_time);
                value.sum_time = value.sum_time.wrapping_add(value.last_time);
            }
            value.last_quantity = qty;
            value.sum_quantity = value.sum_quantity.wrapping_add(qty);
            value.count += 1;
        }

        assert!(
            group
                .stack()
                .last()
                .map(|top| Rc::ptr_eq(top, &self.0))
                .unwrap_or(false),
            "ProfileVal::stop: MF_ASSERT( stack.back() == this )"
        );
        group.stack().pop();

        // Disable internal time counting for this profile
        // (profile.hpp:139-141). The borrow is scoped: the frame above
        // can be this very value on a re-entrant stop.
        {
            let mut value = self.0.borrow_mut();
            value.last_int_time = now.wrapping_sub(value.last_int_time);
            value.sum_int_time = value.sum_int_time.wrapping_add(value.last_int_time);
        }

        // Re-enable the internal counter for the frame above this one
        // (profile.hpp:143-147).
        if let Some(top) = group.stack().last() {
            top.borrow_mut().last_int_time = now;
        }
    }

    /// `ProfileVal::stop(filename, lineNum, qty)` (`profile.hpp:153-168`):
    /// the plain stop, then the too-long answer the checked-stop macros
    /// branch on.
    pub fn stop_checked(&self, now: u64, qty: u32, file: &str, line: u32) -> bool {
        self.stop(now, qty);
        let too_long = self.0.borrow().is_too_long();
        if too_long {
            eprintln!("{}", self.0.borrow().too_long_message(file, line));
        }
        too_long
    }

    /// `operator>>(istream, ProfileVal)` (`profile.cpp:179-184`): the
    /// stream-in reset — zero the counters, then let the resetter see
    /// whether this value is the nominated global-reset trigger.
    pub fn reset_from_stream(&self) {
        self.0.borrow_mut().reset();
        profile_group_resetter().borrow_mut().reset_if_desired(self);
    }

    /// `pWatcherStamps()`/`pWatcherSeconds()` (`profile.cpp:80-128`):
    /// the per-member directory under the exact reference child names,
    /// in the reference's order. `seconds` selects the seconds tree,
    /// whose time members render and round-trip through seconds.
    pub(crate) fn field_directory(&self, seconds: bool) -> DirectoryWatcher {
        let mut dir = DirectoryWatcher::new();
        for field in [
            Field::Count,
            Field::InProgress,
            Field::SumQuantity,
            Field::LastQuantity,
            Field::SumTime,
            Field::LastTime,
            Field::SumIntTime,
            Field::LastIntTime,
        ] {
            dir.add_child(
                field.label(),
                Rc::new(RefCell::new(FieldWatcher::new(
                    Rc::clone(&self.0),
                    field,
                    seconds,
                ))),
            );
        }
        dir
    }
}

/// The `ScopedProfile` helper of the `SCOPED_PROFILE` macros
/// (`profile.hpp:295-315`): start at construction, checked-stop at drop
/// — the reference warns through `WARNING_MSG`; the port prints the same
/// text to stderr.
pub struct ScopedProfile {
    profile: ProfileValHandle,
    file: &'static str,
    line: u32,
}

impl ScopedProfile {
    /// `ScopedProfile::ScopedProfile(...)` (`profile.hpp:298-304`): the
    /// clock is the real one here — `timestamp()` at construction.
    pub fn new(profile: ProfileValHandle, file: &'static str, line: u32) -> Self {
        profile.start(stamp_now());
        Self {
            profile,
            file,
            line,
        }
    }
}

impl Drop for ScopedProfile {
    /// `ScopedProfile::~ScopedProfile()` (`profile.hpp:306-309`).
    fn drop(&mut self) {
        self.profile
            .stop_checked(stamp_now(), 0, self.file, self.line);
    }
}

/// One watcher-exposed counter field (`profile.cpp:89-96`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// `count`.
    Count,
    /// `inProgress`.
    InProgress,
    /// `sumQuantity`.
    SumQuantity,
    /// `lastQuantity`.
    LastQuantity,
    /// `sumTime`.
    SumTime,
    /// `lastTime`.
    LastTime,
    /// `sumIntTime`.
    SumIntTime,
    /// `lastIntTime`.
    LastIntTime,
}

impl Field {
    /// Whether this field is a `TimeStamp` member — the seconds tree
    /// renders and round-trips exactly these through seconds, while the
    /// integer members print the same in both trees
    /// (`profile.cpp:116-124`, `timestamp.hpp:294-305`).
    fn is_time(self) -> bool {
        matches!(
            self,
            Field::SumTime | Field::LastTime | Field::SumIntTime | Field::LastIntTime
        )
    }

    fn label(self) -> &'static str {
        match self {
            Field::Count => "count",
            Field::InProgress => "inProgress",
            Field::SumQuantity => "sumQuantity",
            Field::LastQuantity => "lastQuantity",
            Field::SumTime => "sumTime",
            Field::LastTime => "lastTime",
            Field::SumIntTime => "sumIntTime",
            Field::LastIntTime => "lastIntTime",
        }
    }

    fn read_u64(self, value: &ProfileVal) -> u64 {
        match self {
            Field::Count => value.count as u64,
            Field::InProgress => value.in_progress as u64,
            Field::SumQuantity => value.sum_quantity as u64,
            Field::LastQuantity => value.last_quantity as u64,
            Field::SumTime => value.sum_time,
            Field::LastTime => value.last_time,
            Field::SumIntTime => value.sum_int_time,
            Field::LastIntTime => value.last_int_time,
        }
    }

    fn write_u64(self, value: &mut ProfileVal, raw: u64) {
        match self {
            Field::Count => value.count = raw as u32,
            Field::InProgress => value.in_progress = raw as i32,
            Field::SumQuantity => value.sum_quantity = raw as u32,
            Field::LastQuantity => value.last_quantity = raw as u32,
            Field::SumTime => value.sum_time = raw,
            Field::LastTime => value.last_time = raw,
            Field::SumIntTime => value.sum_int_time = raw,
            Field::LastIntTime => value.last_int_time = raw,
        }
    }
}

/// The `details` leaves: `makeWatcher` on one member, stamps as the
/// string form (`profile.cpp:80-100`). The `detailsInSeconds` leaves are
/// the same members with the seconds-rendering `TimeStamp` watcher
/// (`profile.cpp:107-128`).
pub struct FieldWatcher {
    cell: Rc<RefCell<ProfileVal>>,
    field: Field,
    seconds: bool,
}

impl FieldWatcher {
    /// Bind one field of `cell`.
    pub fn new(cell: Rc<RefCell<ProfileVal>>, field: Field, seconds: bool) -> Self {
        Self {
            cell,
            field,
            seconds,
        }
    }
}

impl Watcher for FieldWatcher {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        if !path.is_empty() {
            return false;
        }
        let value = self.cell.borrow();
        // The seconds tree renders exactly the TimeStamp members as
        // seconds — `watcherValueToString(TimeStamp)` maps to
        // `inSeconds()` (`timestamp.hpp:294-296`); the integer members
        // print the same in both trees (`profile.cpp:116-124`).
        *result = if self.seconds && self.field.is_time() {
            format!("{}", stamps_to_seconds(self.field.read_u64(&value)))
        } else {
            format!("{}", self.field.read_u64(&value))
        };
        *desc = String::new();
        *mode = Mode::ReadWrite;
        true
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        if !path.is_empty() {
            return false;
        }
        // The seconds tree's time members round-trip through seconds
        // (`timestamp.hpp:298-305`): seconds in, `setInSeconds` back to
        // stamps.
        let raw = if self.seconds && self.field.is_time() {
            let Ok(seconds) = value.parse::<f64>() else {
                return false;
            };
            (seconds * crate::stamps_per_second()) as u64
        } else {
            let Ok(raw) = value.parse::<u64>() else {
                return false;
            };
            raw
        };
        let mut cell = self.cell.borrow_mut();
        self.field.write_u64(&mut cell, raw);
        true
    }

    fn comment(&self) -> &str {
        ""
    }

    fn set_comment(&mut self, _comment: String) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_group::ProfileGroup;
    use crate::set_stamps_per_second;
    use crate::set_warning_period;

    fn group() -> Rc<RefCell<ProfileGroup>> {
        ProfileGroup::create(None, 0)
    }

    /// The scripted load and its bookkeeping, stop by stop — the
    /// acceptance's reference trace. Every field lands where the C++
    /// bookkeeping puts it for the same stamps
    /// (`profile.hpp:89-148`).
    #[test]
    fn the_scripted_load_lands_every_field_where_the_reference_puts_it() {
        set_stamps_per_second(1_000_000.0);
        let group = group();
        let a = ProfileVal::create("A", Some(&group));
        let b = ProfileVal::create("B", Some(&group));

        // A starts at 10; the running RunningTime frame loses its
        // internal clock (closed: 10 stamps, accumulated: 10).
        a.start(10);
        {
            let rt = group.borrow().p_running_time();
            let rt = rt.value().borrow();
            assert_eq!(
                (rt.in_progress, rt.last_int_time, rt.sum_int_time),
                (1, 10, 10)
            );
        }

        // B runs 20→35 with payload 7: 15 stamps of wall and internal
        // time each.
        b.start(20);
        b.stop(35, 7);
        {
            let b = b.value().borrow();
            assert_eq!(
                (
                    b.count,
                    b.in_progress,
                    b.sum_quantity,
                    b.last_quantity,
                    b.sum_time,
                    b.last_time,
                    b.sum_int_time,
                    b.last_int_time
                ),
                (1, 0, 7, 7, 15, 15, 15, 15)
            );
        }
        // B's stop handed the internal clock back to A at 35.
        {
            let a = a.value().borrow();
            assert_eq!((a.in_progress, a.last_int_time), (1, 35));
        }

        // A closes 10→40: 30 stamps of wall time, 15 of internal — the
        // 10→20 stretch plus the 35→40 stretch B's run interrupted.
        a.stop(40, 0);
        {
            let a = a.value().borrow();
            assert_eq!(
                (
                    a.count,
                    a.sum_time,
                    a.last_time,
                    a.sum_int_time,
                    a.last_int_time
                ),
                (1, 30, 30, 15, 5)
            );
        }

        // A again at 50; the RunningTime frame's gap 40→50 closes.
        a.start(50);
        {
            let rt = group.borrow().p_running_time();
            let rt = rt.value().borrow();
            assert_eq!((rt.last_int_time, rt.sum_int_time), (10, 20));
        }

        // A re-enters at 60: the depth climbs, the outer stretch's
        // internal time closes at the switch (50→60), and the wall clock
        // keeps its original start.
        a.start(60);
        {
            let a = a.value().borrow();
            assert_eq!(
                (a.in_progress, a.last_time, a.last_int_time, a.sum_int_time),
                (2, 50, 60, 25)
            );
        }

        // The inner A closes 60→70 — the payload and the count land on
        // every stop, the wall clock only below depth 1.
        a.stop(70, 3);
        {
            let a = a.value().borrow();
            assert_eq!(
                (
                    a.in_progress,
                    a.count,
                    a.sum_time,
                    a.last_quantity,
                    a.sum_quantity,
                    a.sum_int_time,
                    a.last_int_time
                ),
                (1, 2, 30, 3, 3, 35, 70)
            );
        }

        // The outer A closes 50→80 — the third completed stop.
        a.stop(80, 4);
        {
            let a = a.value().borrow();
            assert_eq!(
                (
                    a.count,
                    a.in_progress,
                    a.sum_quantity,
                    a.last_quantity,
                    a.sum_time,
                    a.last_time,
                    a.sum_int_time,
                    a.last_int_time
                ),
                (3, 0, 7, 4, 60, 30, 45, 10)
            );
        }

        // The RunningTime frame never stopped: its wall clock is the
        // start stamp, running_time reads it against now, and the last
        // switch left its internal clock at 80.
        let rt = group.borrow().p_running_time();
        {
            let rt = rt.value().borrow();
            assert_eq!(
                (
                    rt.count,
                    rt.in_progress,
                    rt.last_time,
                    rt.sum_time,
                    rt.sum_int_time,
                    rt.last_int_time
                ),
                (0, 1, 0, 0, 20, 80)
            );
        }
        assert_eq!(group.borrow().running_time(80), 80);
        assert_eq!(group.borrow().running_time(155), 155);
    }

    /// The summary text is the `operator<<` shape
    /// (`profile.cpp:132-171`): the count line, the in-progress arm, the
    /// quantity line with its average, the time line, and the per-count /
    /// per-quantity lines that appear only when they mean something.
    #[test]
    fn the_summary_text_matches_the_reference_stream_shape() {
        set_stamps_per_second(1_000_000.0);
        let group = group();
        let b = ProfileVal::create("B", Some(&group));

        // Never run: count 0, and the stream stops after the count line
        // (`profile.cpp:144-147`).
        assert_eq!(b.value().borrow().summary(999), "Count: 0\n");

        // The scripted B of the bookkeeping trace.
        b.start(20);
        b.stop(35, 7);
        assert_eq!(
            b.value().borrow().summary(999),
            "Count: 1\n\
             Quantity: 7 (last 7, avg 7)\n\
             Time: 000 015.000us (last 000 015.000us)\n\
             Time/Count: 000 015.000us\n\
             Time/Qty: 000 002.000us\n"
        );

        // A payload-less run drops the Quantity and Time/Qty lines
        // (`profile.cpp:149-154`, `:163-167`).
        let c = ProfileVal::create("C", Some(&group));
        c.start(100);
        c.stop(130, 0);
        assert_eq!(
            c.value().borrow().summary(999),
            "Count: 1\nTime: 000 030.000us (last 000 030.000us)\nTime/Count: 000 030.000us\n"
        );

        // A running value renders its live stretch against `now`
        // (`profile.cpp:134-141`); the quantity and per-count lines
        // still answer from the completed history, in the reference's
        // order (`profile.cpp:137-167`).
        b.start(500);
        assert_eq!(
            b.value().borrow().summary(510),
            "Count: 1\n\
             \x20inProgress: 000 010.000us\n\
             Quantity: 7 (last 7, avg 7)\n\
             Time: 000 015.000us (last 000 010.000us)\n\
             Time/Count: 000 015.000us\n\
             Time/Qty: 000 002.000us\n"
        );
        b.stop(520, 0);
    }

    /// `isTooLong` and the checked stop (`profile.hpp:153-174`): the
    /// warning period defaults to never, a set period flags an
    /// over-long stop, and the message names file, line, profile and
    /// seconds.
    #[test]
    fn the_checked_stop_warns_over_the_warning_period() {
        set_stamps_per_second(1_000_000.0);
        set_warning_period(u64::MAX);
        let group = group();
        let a = ProfileVal::create("A", Some(&group));

        a.start(0);
        assert!(
            !a.stop_checked(30, 0, "frame.rs", 12),
            "the default period never flags"
        );

        set_warning_period(20);
        a.start(100);
        assert!(
            !a.stop_checked(120, 0, "frame.rs", 12),
            "a stretch at the period is not over it"
        );
        a.start(200);
        assert!(
            a.stop_checked(221, 0, "frame.rs", 12),
            "21 stamps over a 20-stamp period is too long"
        );
        assert_eq!(
            a.value().borrow().too_long_message("frame.rs", 12),
            "frame.rs:12: Profile A took 0.00 seconds",
            "21us prints as 0.00 at two decimals"
        );

        // The seconds fields read the rate (`profile.hpp:213-216`): the
        // scripted stretches accumulated 71 stamps of wall and internal
        // time, the last of them 21.
        set_stamps_per_second(1000.0);
        let a = a.value().borrow();
        assert_eq!(a.sum_time_in_seconds(), 0.071);
        assert_eq!(a.last_time_in_seconds(), 0.021);
        assert_eq!(a.sum_int_time_in_seconds(), 0.071);
        assert_eq!(a.last_int_time_in_seconds(), 0.021);
    }

    /// `operator>>` (`profile.cpp:179-184`): the stream-in reset zeroes
    /// the counters, and the nominated value's reset triggers the
    /// registered groups' resets.
    #[test]
    fn the_stream_in_reset_zeroes_and_triggers_the_resetter() {
        set_stamps_per_second(1_000_000.0);
        let group = group();
        crate::resetter::profile_group_resetter()
            .borrow_mut()
            .add_profile_group(&group);

        let a = ProfileVal::create("A", Some(&group));
        let nominated = ProfileVal::create("nominee", Some(&group));
        crate::resetter::profile_group_resetter()
            .borrow_mut()
            .nominate_profile_val(Some(&nominated));

        a.start(10);
        a.stop(20, 5);
        nominated.start(10);
        nominated.stop(30, 9);

        // Resetting another value zeroes only itself.
        a.reset_from_stream();
        assert_eq!(a.value().borrow().count, 0);
        assert_eq!(nominated.value().borrow().count, 1);
        assert_eq!(
            group.borrow().p_running_time().value().borrow().in_progress,
            1,
            "the group itself did not reset"
        );

        // Resetting the nominee zeroes it and resets the whole group —
        // the RunningTime frame restarts at the reset instant, which
        // this path reads from the real clock (`profile.cpp:264`).
        let before = stamp_now();
        nominated.reset_from_stream();
        assert_eq!(nominated.value().borrow().count, 0);
        assert_eq!(
            a.value().borrow().count,
            0,
            "the group reset took A with it"
        );
        {
            let rt = group.borrow().p_running_time();
            let rt = rt.value().borrow();
            assert_eq!(rt.in_progress, 1, "the RunningTime frame restarted");
            assert!(
                (before..=stamp_now()).contains(&rt.last_time),
                "RunningTime restarted at the reset instant"
            );
        }

        // The explicit-instant reset pins the restart point: the next
        // window measures from there (`profile.cpp:257-265`).
        ProfileGroup::reset(&group, 40);
        assert_eq!(group.borrow().running_time(90), 50);
    }

    /// The unnamed value skips the group registration but lands in the
    /// default group's stack machinery (`profile.cpp:30-40`).
    #[test]
    fn an_unnamed_value_stays_out_of_the_tree() {
        let group = group();
        let before = group.borrow().profile_count();
        let anonymous = ProfileVal::create("", Some(&group));
        anonymous.start(5);
        anonymous.stop(9, 0);
        assert_eq!(anonymous.value().borrow().count, 1);
        assert_eq!(group.borrow().profile_count(), before, "no tree entry");

        // No group at all: the default group takes it.
        let orphan = ProfileVal::create("orphan", None);
        orphan.start(1);
        orphan.stop(2, 0);
        assert_eq!(orphan.value().borrow().count, 1);
    }

    /// The `details` leaves read and write one member each, the
    /// `detailsInSeconds` leaves render the same stamps as seconds
    /// (`profile.cpp:80-128`).
    #[test]
    fn the_field_leaves_read_and_write_their_member() {
        set_stamps_per_second(1_000_000.0);
        let group = group();
        let a = ProfileVal::create("A", Some(&group));
        a.start(0);
        a.stop(25, 5);

        let mut leaf = FieldWatcher::new(Rc::clone(a.value()), Field::SumTime, false);
        let mut out = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(leaf.get_as_string("", &mut out, &mut desc, &mut mode));
        assert_eq!(out, "25");
        assert_eq!(mode, Mode::ReadWrite);

        assert!(leaf.set_from_string("", "77"));
        assert_eq!(a.value().borrow().sum_time, 77);
        assert!(!leaf.set_from_string("", "not a number"), "junk is refused");
        assert_eq!(a.value().borrow().sum_time, 77);

        let seconds_leaf = FieldWatcher::new(Rc::clone(a.value()), Field::SumTime, true);
        let mut out = String::new();
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert!(seconds_leaf.get_as_string("", &mut out, &mut desc, &mut mode));
        assert_eq!(out, "0.000077", "the seconds form of 77 stamps at 1e6/s");

        // A sub-path does not resolve and a non-empty path is refused,
        // like every leaf.
        let mut out = String::new();
        assert!(!leaf.get_as_string("count", &mut out, &mut desc, &mut mode));
        assert!(!leaf.set_from_string("count", "1"));
    }

    /// Every member round-trips through its leaf, and the seconds tree
    /// keeps the integer members plain while routing the time members
    /// through seconds (`profile.cpp:116-124`, `timestamp.hpp:294-305`).
    #[test]
    fn every_field_leaf_round_trips() {
        set_stamps_per_second(1_000_000.0);
        let group = group();
        let a = ProfileVal::create("A", Some(&group));
        a.start(0);
        a.stop(25, 5);

        let fields = [
            (Field::Count, 1u64),
            (Field::InProgress, 0),
            (Field::SumQuantity, 5),
            (Field::LastQuantity, 5),
            (Field::SumTime, 25),
            (Field::LastTime, 25),
            (Field::SumIntTime, 25),
            (Field::LastIntTime, 25),
        ];
        for (field, expected) in fields {
            let mut stamps_leaf = FieldWatcher::new(Rc::clone(a.value()), field, false);
            let seconds_leaf = FieldWatcher::new(Rc::clone(a.value()), field, true);
            let mut out = String::new();
            let mut desc = String::new();
            let mut mode = Mode::Invalid;

            assert!(stamps_leaf.get_as_string("", &mut out, &mut desc, &mut mode));
            assert_eq!(out, expected.to_string(), "{field:?} stamps");

            // The leaf retitles like the reference's comment-less
            // member watchers: the write lands, the comment stays.
            stamps_leaf.set_comment("ignored".to_string());
            assert_eq!(stamps_leaf.comment(), "");

            assert!(stamps_leaf.set_from_string("", "99"), "{field:?} accepts");
            assert!(seconds_leaf.get_as_string("", &mut out, &mut desc, &mut mode));
            let expected_seconds = if field.is_time() {
                format!("{}", 99_f64 / 1_000_000.0)
            } else {
                "99".to_string()
            };
            assert_eq!(out, expected_seconds, "{field:?} seconds tree");

            assert!(stamps_leaf.set_from_string("", &expected.to_string()));
            assert_eq!(field.read_u64(&a.value().borrow()), expected, "restored");
        }
    }

    /// The scoped guard starts with the real clock and stops at drop
    /// (`profile.hpp:295-315`).
    #[test]
    fn the_scoped_guard_stops_at_drop() {
        let group = group();
        let a = ProfileVal::create("A", Some(&group));
        {
            let _guard = ScopedProfile::new(a.clone(), "frame.rs", 3);
            assert!(a.value().borrow().running());
        }
        let a = a.value().borrow();
        assert_eq!(a.count, 1, "the drop performed the checked stop");
        assert!(!a.running());
    }
}
