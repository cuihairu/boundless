//! The `ProfileGroupResetter` singleton (`cstdmf/profile.hpp:259-285`,
//! `cstdmf/profile.cpp:279-346`): reset one nominated value, reset
//! every registered group.
//!
//! The reference records raw `ProfileVal*`/`ProfileGroup*`: a reset of
//! the nominated value — through the `operator>>` stream-in path
//! (`profile.cpp:179-184`) — is the trigger, compared by pointer
//! identity, and the `doingReset_` flag keeps the sweep from
//! re-entering. The port stores the nominee as the value's heap address
//! (the group holds a strong handle for the lifetime the reference's
//! raw pointer assumed) and the groups as `Weak` (a dead group drops
//! out of the sweep instead of dangling).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use foundation::time::stamp_now;

use crate::profile_group::ProfileGroup;
use crate::profile_val::ProfileValHandle;

/// The singleton's state (`profile.hpp:274-279`).
pub struct ProfileGroupResetter {
    /// `nominee_`: the address of the value whose reset sweeps.
    nominee: Option<usize>,
    /// `groups_`.
    groups: Vec<Weak<RefCell<ProfileGroup>>>,
    /// `doingReset_`.
    doing_reset: bool,
}

impl ProfileGroupResetter {
    /// `ProfileGroupResetter::ProfileGroupResetter()`
    /// (`profile.cpp:283-289`).
    fn new() -> Self {
        Self {
            nominee: None,
            groups: Vec::new(),
            doing_reset: false,
        }
    }

    /// `nominateProfileVal` (`profile.cpp:302-305`); `None` clears the
    /// nomination, matching the reference's default `NULL` argument.
    pub fn nominate_profile_val(&mut self, val: Option<&ProfileValHandle>) {
        self.nominee = val.map(|val| Rc::as_ptr(val.value()) as usize);
    }

    /// `addProfileGroup` (`profile.cpp:311-316`): a group registered
    /// mid-sweep is refused, exactly the reference's `doingReset_` guard.
    pub fn add_profile_group(&mut self, group: &Rc<RefCell<ProfileGroup>>) {
        if self.doing_reset {
            return;
        }
        self.groups.push(Rc::downgrade(group));
    }

    /// `resetIfDesired` (`profile.cpp:331-346`): the sweep the
    /// nominee's reset triggers. Every registered group restarts its
    /// RunningTime frame at its own now — the reference's `reset()`
    /// reads `timestamp()` per group (`profile.cpp:264`).
    pub fn reset_if_desired(&mut self, val: &ProfileValHandle) {
        if self.doing_reset {
            return;
        }
        if self.nominee != Some(Rc::as_ptr(val.value()) as usize) {
            return;
        }

        self.doing_reset = true;

        for weak in &self.groups {
            if let Some(group) = weak.upgrade() {
                ProfileGroup::reset(&group, stamp_now());
            }
        }

        self.doing_reset = false;
    }
}

thread_local! {
    static RESETTER: RefCell<Option<Rc<RefCell<ProfileGroupResetter>>>> =
        const { RefCell::new(None) };
}

/// `ProfileGroupResetter::instance()` (`profile.cpp:321-325`): the
/// thread's singleton.
pub fn profile_group_resetter() -> Rc<RefCell<ProfileGroupResetter>> {
    RESETTER.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| Rc::new(RefCell::new(ProfileGroupResetter::new())))
            .clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_val::ProfileVal;

    fn group() -> Rc<RefCell<ProfileGroup>> {
        ProfileGroup::create(None, 0)
    }

    /// Without a nomination nothing sweeps: the reset stays local and
    /// the group's RunningTime window keeps its original start.
    #[test]
    fn without_a_nomination_no_group_resets() {
        let group = group();
        profile_group_resetter()
            .borrow_mut()
            .add_profile_group(&group);

        let a = ProfileVal::create("A", Some(&group));
        a.start(10);
        a.stop(20, 3);
        assert_eq!(a.value().borrow().count, 1);

        a.reset_from_stream();
        assert_eq!(a.value().borrow().count, 0, "the reset itself still ran");
        assert_eq!(
            group.borrow().running_time(1000),
            1000,
            "the RunningTime frame never restarted"
        );
    }

    /// The nomination sweeps every registered group, each RunningTime
    /// frame restarting at its own sweep instant
    /// (`profile.cpp:336-343`).
    #[test]
    fn the_nominees_reset_sweeps_every_registered_group() {
        let g1 = group();
        let g2 = group();
        {
            let resetter = profile_group_resetter();
            let mut resetter = resetter.borrow_mut();
            resetter.add_profile_group(&g1);
            resetter.add_profile_group(&g2);
        }

        let a = ProfileVal::create("A", Some(&g1));
        let b = ProfileVal::create("B", Some(&g2));
        let nominated = ProfileVal::create("nominee", Some(&g1));
        profile_group_resetter()
            .borrow_mut()
            .nominate_profile_val(Some(&nominated));

        a.start(10);
        a.stop(30, 2);
        b.start(40);
        b.stop(60, 3);
        nominated.start(10);
        nominated.stop(20, 1);
        assert_eq!(a.value().borrow().count, 1);
        assert_eq!(b.value().borrow().count, 1);

        let before = stamp_now();
        nominated.reset_from_stream();
        let after = stamp_now();

        assert_eq!(a.value().borrow().count, 0, "g1 swept");
        assert_eq!(b.value().borrow().count, 0, "g2 swept");
        for group in [&g1, &g2] {
            let rt = group.borrow().p_running_time();
            let rt = rt.value().borrow();
            assert!(
                (before..=after).contains(&rt.last_time),
                "the RunningTime frame restarted at the sweep instant"
            );
            assert_eq!(rt.in_progress, 1, "the frame is running again");
        }
    }

    /// `None` clears the nomination — `nominateProfileVal`'s default
    /// argument (`profile.hpp:269`).
    #[test]
    fn clearing_the_nomination_stops_the_sweeps() {
        let group = group();
        profile_group_resetter()
            .borrow_mut()
            .add_profile_group(&group);

        let a = ProfileVal::create("A", Some(&group));
        let nominated = ProfileVal::create("nominee", Some(&group));
        {
            let resetter = profile_group_resetter();
            let mut resetter = resetter.borrow_mut();
            resetter.nominate_profile_val(Some(&nominated));
            resetter.nominate_profile_val(None);
        }

        a.start(10);
        a.stop(20, 3);
        nominated.start(10);
        nominated.stop(20, 2);

        nominated.reset_from_stream();
        assert_eq!(nominated.value().borrow().count, 0, "own reset still ran");
        assert_eq!(a.value().borrow().count, 1, "no sweep without a nominee");
    }

    /// A dropped group leaves the sweep without a dangling read — the
    /// port keeps the registered groups as `Weak`.
    #[test]
    fn a_dropped_group_exits_the_sweep() {
        let alive = group();
        let dropped = group();
        {
            let resetter = profile_group_resetter();
            let mut resetter = resetter.borrow_mut();
            resetter.add_profile_group(&alive);
            resetter.add_profile_group(&dropped);
        }

        let a = ProfileVal::create("A", Some(&alive));
        let b = ProfileVal::create("B", Some(&dropped));
        let nominated = ProfileVal::create("nominee", Some(&alive));
        {
            let resetter = profile_group_resetter();
            let mut resetter = resetter.borrow_mut();
            resetter.nominate_profile_val(Some(&nominated));
        }

        a.start(10);
        a.stop(20, 0);
        b.start(10);
        b.stop(20, 0);

        drop(b);
        drop(dropped);

        nominated.reset_from_stream();
        assert_eq!(a.value().borrow().count, 0, "the living group swept");
    }
}
