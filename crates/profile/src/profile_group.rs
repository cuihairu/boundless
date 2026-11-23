//! The `ProfileGroup` (`cstdmf/profile.hpp:32-74`,
//! `cstdmf/profile.cpp:191-276`): one accounting domain's frames.
//!
//! The group owns its values, keeps the start/stop stack the internal
//! time bookkeeping reads, and exposes every member through the three
//! watcher subtrees the reference mounts — `summaries` (the
//! `operator<<` text), `details` (the raw stamp members) and
//! `detailsInSeconds` (the same tree whose time members render through
//! `watcherValueToString(TimeStamp)`, i.e. seconds). The reference's
//! group *is* a `DirectoryWatcher` (`profile.hpp:40`); the port composes
//! one and delegates the [`Watcher`] impl.
//!
//! The first member is always `RunningTime`, created and started at
//! construction (`profile.cpp:202-203`): the always-running frame the
//! gaps between the real profiles are read against (`runningTime()`,
//! `profile.cpp:248-251`).

use std::cell::RefCell;
use std::rc::Rc;

use foundation::time::stamp_now;
use watcher::{DirectoryWatcher, Mode, Watcher, add_watcher};

use crate::profile_val::{ProfileVal, ProfileValHandle};

/// One group: the values, their frame stack, and the three watcher
/// subtrees (`profile.hpp:63-73`).
pub struct ProfileGroup {
    /// The directory node this group presents as (the reference extends
    /// `DirectoryWatcher`, `profile.hpp:40`).
    dir: DirectoryWatcher,
    /// `pSummaries_`.
    summaries: Rc<RefCell<DirectoryWatcher>>,
    /// `pDetails_`.
    details: Rc<RefCell<DirectoryWatcher>>,
    /// `pDetailsInSeconds_`.
    details_in_seconds: Rc<RefCell<DirectoryWatcher>>,
    /// `profiles_`: every member, `RunningTime` first.
    profiles: Vec<ProfileValHandle>,
    /// `stack_`: the currently executing frames, innermost last.
    stack: Vec<Rc<RefCell<ProfileVal>>>,
}

impl ProfileGroup {
    /// The constructor's wiring (`profile.cpp:194-207`): the three
    /// subdirectories under their exact reference names. The group is
    /// completed by [`ProfileGroup::create`], which adds `RunningTime`.
    fn new() -> Self {
        let mut dir = DirectoryWatcher::new();
        let summaries = Rc::new(RefCell::new(DirectoryWatcher::new()));
        let details = Rc::new(RefCell::new(DirectoryWatcher::new()));
        let details_in_seconds = Rc::new(RefCell::new(DirectoryWatcher::new()));
        let summaries_node: Rc<RefCell<dyn Watcher>> = summaries.clone();
        let details_node: Rc<RefCell<dyn Watcher>> = details.clone();
        let seconds_node: Rc<RefCell<dyn Watcher>> = details_in_seconds.clone();
        dir.add_child("summaries", summaries_node);
        dir.add_child("details", details_node);
        dir.add_child("detailsInSeconds", seconds_node);
        Self {
            dir,
            summaries,
            details,
            details_in_seconds,
            profiles: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// The constructor (`profile.cpp:194-217`) as a two-phase build:
    /// create the group, add and start its `RunningTime` frame at `now`,
    /// then mount it at `watcher_path` under the root watcher. The port
    /// takes the clock explicitly — the reference reads `timestamp()`,
    /// after calling `stampsPerSecond()` to make sure the timers are set
    /// up (the port's rate is a settable constant, `crate`).
    pub fn create(watcher_path: Option<&str>, now: u64) -> Rc<RefCell<ProfileGroup>> {
        let group = Rc::new(RefCell::new(ProfileGroup::new()));

        let running_time = ProfileVal::create("RunningTime", Some(&group));
        running_time.start(now);

        if let Some(path) = watcher_path {
            let node: Rc<RefCell<dyn Watcher>> = group.clone();
            add_watcher(path, node);
        }
        group
    }

    /// `defaultGroup()` (`profile.cpp:271-276`): the group every
    /// group-less value lands in, mounted at the root path `profiles`.
    pub fn default_group() -> Rc<RefCell<ProfileGroup>> {
        thread_local! {
            static DEFAULT_GROUP: RefCell<Option<Rc<RefCell<ProfileGroup>>>> =
                const { RefCell::new(None) };
        }
        DEFAULT_GROUP.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some(group) = slot.as_ref() {
                return Rc::clone(group);
            }
            let group = ProfileGroup::create(Some("profiles"), stamp_now());
            *slot = Some(Rc::clone(&group));
            group
        })
    }

    /// `pRunningTime()` (`profile.hpp:57-58`): the first member.
    pub fn p_running_time(&self) -> ProfileValHandle {
        self.profiles[0].clone()
    }

    /// `runningTime()` (`profile.cpp:248-251`): the window since the
    /// frame started, read against `now` — the reference subtracts from
    /// `timestamp()`.
    pub fn running_time(&self, now: u64) -> u64 {
        now.wrapping_sub(self.p_running_time().value().borrow().last_time)
    }

    /// `ProfileGroup::add` (`profile.cpp:233-241`): register the member
    /// and mount its three watcher leaves under its name — the summary
    /// text leaf, the raw stamp members, and the seconds tree.
    pub fn add(&mut self, val: &ProfileValHandle) {
        self.profiles.push(val.clone());
        let name = val.0.borrow().name.clone();

        self.summaries.borrow_mut().add_child(
            &name,
            Rc::new(RefCell::new(SummaryWatcher::new(Rc::clone(val.value())))),
        );
        self.details
            .borrow_mut()
            .add_child(&name, Rc::new(RefCell::new(val.field_directory(false))));
        self.details_in_seconds
            .borrow_mut()
            .add_child(&name, Rc::new(RefCell::new(val.field_directory(true))));
    }

    /// `ProfileGroup::reset` (`profile.cpp:257-265`): zero every member,
    /// then `pRunningTime()->start()` re-arms the always-running frame.
    /// It is an associated function taking the group's `Rc` because the
    /// restart re-enters the group's cell through [`ProfileValHandle::start`]
    /// — an outer borrow would deadlock it, the way the reference needs
    /// no lock at all.
    pub fn reset(group: &Rc<RefCell<ProfileGroup>>, now: u64) {
        {
            let borrowed = group.borrow();
            for val in &borrowed.profiles {
                val.0.borrow_mut().reset();
            }
        }
        let running_time = group.borrow().p_running_time();
        running_time.start(now);
    }

    /// The frame stack the start/stop bookkeeping runs on
    /// (`profile.hpp:67-68`, `stack()`).
    pub(crate) fn stack(&mut self) -> &mut Vec<Rc<RefCell<ProfileVal>>> {
        &mut self.stack
    }

    /// The member count — the observation point the reference covers
    /// through iteration (`begin()`/`end()`, `profile.hpp:48-51`).
    pub fn profile_count(&self) -> usize {
        self.profiles.len()
    }
}

impl Watcher for ProfileGroup {
    fn get_as_string(
        &self,
        path: &str,
        result: &mut String,
        desc: &mut String,
        mode: &mut Mode,
    ) -> bool {
        self.dir.get_as_string(path, result, desc, mode)
    }

    fn set_from_string(&mut self, path: &str, value: &str) -> bool {
        self.dir.set_from_string(path, value)
    }

    fn comment(&self) -> &str {
        self.dir.comment()
    }

    fn set_comment(&mut self, comment: String) {
        self.dir.set_comment(comment);
    }

    fn add_child(&mut self, path: &str, watcher: Rc<RefCell<dyn Watcher>>) -> bool {
        self.dir.add_child(path, watcher)
    }

    fn get_child(&self, path: &str) -> Option<Rc<RefCell<dyn Watcher>>> {
        self.dir.get_child(path)
    }
}

/// The `summaries` leaf — the bound `DataWatcher<ProfileVal>` of
/// `ProfileVal::pSummaryWatcher()` (`profile.cpp:62-74`, mounted at
/// `profile.cpp:238`): `operator<<` text out, the `operator>>` reset in.
pub struct SummaryWatcher {
    cell: Rc<RefCell<ProfileVal>>,
}

impl SummaryWatcher {
    /// Bind one value the way `addChild(name, pWatcher, pVal)` binds
    /// (`profile.cpp:238`).
    pub fn new(cell: Rc<RefCell<ProfileVal>>) -> Self {
        Self { cell }
    }
}

impl Watcher for SummaryWatcher {
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
        // `operator<<(ostream, ProfileVal)` (`profile.cpp:132-171`); the
        // live frame reads its window against the real clock here, the
        // way the reference reads `timestamp()`.
        *result = self.cell.borrow().summary(stamp_now());
        // The reference's DataWatcher carries no comment
        // (`profile.cpp:70`).
        *desc = String::new();
        *mode = Mode::ReadWrite;
        true
    }

    fn set_from_string(&mut self, path: &str, _value: &str) -> bool {
        if !path.is_empty() {
            return false;
        }
        // `operator>>(istream, ProfileVal)` (`profile.cpp:179-184`): the
        // written text is ignored; the write resets the profile and may
        // trip the global resetter.
        ProfileValHandle(Rc::clone(&self.cell)).reset_from_stream();
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
    use watcher::{get_watcher_string, set_watcher_string};

    /// The tree mounts the three subdirectories and each member's leaves
    /// under the exact reference names; the summary leaf answers with
    /// the `operator<<` text and the write-to-reset path resets.
    #[test]
    fn the_tree_mounts_summaries_details_and_details_in_seconds() {
        crate::set_stamps_per_second(1_000_000.0);
        let group = ProfileGroup::create(Some("perfgroup"), 0);
        let a = ProfileVal::create("A", Some(&group));
        a.start(0);
        a.stop(30, 5);

        // The summaries leaf: the operator<< text, no comment, read
        // write (`profile.cpp:70`, `:238`).
        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        let text = get_watcher_string("perfgroup/summaries/A", &mut desc, &mut mode)
            .expect("the summaries leaf resolves");
        assert_eq!(
            text,
            "Count: 1\n\
             Quantity: 5 (last 5, avg 5)\n\
             Time: 000 030.000us (last 000 030.000us)\n\
             Time/Count: 000 030.000us\n\
             Time/Qty: 000 006.000us\n"
        );
        assert_eq!(mode, Mode::ReadWrite);
        assert_eq!(desc, "");

        // details answers in raw stamps; detailsInSeconds in seconds.
        // The count member is an integer in both trees — only the time
        // members differ (`profile.cpp:89-96` vs `:116-124`).
        assert_eq!(
            get_watcher_string("perfgroup/details/A/sumTime", &mut desc, &mut mode).unwrap(),
            "30"
        );
        assert_eq!(
            get_watcher_string("perfgroup/details/A/count", &mut desc, &mut mode).unwrap(),
            "1"
        );
        assert_eq!(
            get_watcher_string("perfgroup/detailsInSeconds/A/count", &mut desc, &mut mode).unwrap(),
            "1"
        );
        assert_eq!(
            get_watcher_string("perfgroup/detailsInSeconds/A/sumTime", &mut desc, &mut mode)
                .unwrap(),
            "0.00003"
        );

        // The stamps tree's time member writes raw stamps...
        assert!(set_watcher_string("perfgroup/details/A/sumTime", "40"));
        assert_eq!(a.value().borrow().sum_time, 40);
        // ...the seconds tree's writes seconds back into stamps
        // (`timestamp.hpp:298-305`).
        assert!(set_watcher_string(
            "perfgroup/detailsInSeconds/A/lastTime",
            "0.0001"
        ));
        assert_eq!(a.value().borrow().last_time, 100);

        // Writing to the summaries leaf is the operator>> reset
        // (`profile.cpp:179-184`) — and, with no nominee, stays local.
        assert!(set_watcher_string("perfgroup/summaries/A", "whatever"));
        assert_eq!(a.value().borrow().count, 0);

        // The RunningTime leaf is present in every tree; its live frame
        // renders the in-progress window.
        let text = get_watcher_string("perfgroup/summaries/RunningTime", &mut desc, &mut mode)
            .expect("the RunningTime leaf resolves");
        assert!(
            text.starts_with("Count: 0\n inProgress: "),
            "the live frame renders its window: {text}"
        );
        assert!(
            get_watcher_string("perfgroup/details/RunningTime/count", &mut desc, &mut mode)
                .is_some()
        );
        assert!(
            get_watcher_string(
                "perfgroup/detailsInSeconds/RunningTime/count",
                &mut desc,
                &mut mode
            )
            .is_some()
        );
    }

    /// The default group mounts at `profiles` and takes the group-less
    /// members; the unnamed value skips the tree
    /// (`profile.cpp:32-40`, `:271-276`).
    #[test]
    fn the_default_group_mounts_at_profiles() {
        crate::set_stamps_per_second(1_000_000.0);
        let orphan = ProfileVal::create("orphaned", None);
        orphan.start(0);
        orphan.stop(10, 0);

        let mut desc = String::new();
        let mut mode = Mode::Invalid;
        assert_eq!(
            get_watcher_string("profiles/summaries/orphaned", &mut desc, &mut mode)
                .expect("the default group mounted"),
            "Count: 1\nTime: 000 010.000us (last 000 010.000us)\nTime/Count: 000 010.000us\n"
        );

        let before = ProfileGroup::default_group().borrow().profile_count();
        let anonymous = ProfileVal::create("", None);
        anonymous.start(0);
        anonymous.stop(5, 2);
        assert_eq!(anonymous.value().borrow().count, 1);
        assert_eq!(
            ProfileGroup::default_group().borrow().profile_count(),
            before,
            "no tree entry for the unnamed value"
        );
    }

    /// `reset` zeroes every member and restarts the RunningTime frame at
    /// the given instant (`profile.cpp:257-265`).
    #[test]
    fn the_group_reset_restarts_the_running_time_frame() {
        crate::set_stamps_per_second(1_000_000.0);
        let group = ProfileGroup::create(None, 0);
        let a = ProfileVal::create("A", Some(&group));
        a.start(10);
        a.stop(30, 4);
        assert_eq!(
            group.borrow().running_time(50),
            50,
            "the frame started at 0"
        );

        ProfileGroup::reset(&group, 100);
        assert_eq!(a.value().borrow().count, 0, "every member zeroed");
        assert_eq!(
            group.borrow().running_time(150),
            50,
            "the window restarted at 100"
        );
        assert_eq!(group.borrow().profile_count(), 2, "RunningTime plus A");
    }

    /// The group presents as its directory: the comment retitles
    /// through the delegation, and the duplicate child is refused by
    /// the same rules the reference's DirectoryWatcher applies.
    #[test]
    fn the_group_presents_as_its_directory() {
        let group = ProfileGroup::create(None, 0);
        group.borrow_mut().set_comment("perf".to_string());
        let node: Rc<RefCell<dyn Watcher>> = group.clone();
        assert_eq!(node.borrow().comment(), "perf");

        assert!(
            node.borrow().get_child("summaries").is_some(),
            "the summaries subtree resolves through the delegation"
        );
        assert!(
            !node
                .borrow_mut()
                .add_child("summaries", Rc::new(RefCell::new(DirectoryWatcher::new())),),
            "the duplicate child is refused"
        );
        assert!(
            node.borrow_mut()
                .set_from_string("summaries/RunningTime", "whatever"),
            "writes delegate to the directory too"
        );
        assert_eq!(
            group.borrow().p_running_time().value().borrow().count,
            0,
            "the delegated write performed the operator>> reset"
        );
    }

    /// The summary leaf's `set_comment` matches the reference's
    /// comment-less DataWatcher (`profile.cpp:70`).
    #[test]
    fn the_summary_leaf_has_no_comment() {
        let group = ProfileGroup::create(None, 0);
        let a = ProfileVal::create("A", Some(&group));
        let mut leaf = SummaryWatcher::new(Rc::clone(a.value()));
        leaf.set_comment("retitle".to_string());
        assert_eq!(leaf.comment(), "");
    }
}
