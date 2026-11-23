//! BW-05-02 the reviver: external recovery for the cluster roles.
//!
//! The reference splits the job in two. `ComponentReviver`
//! (`server/reviver/component_reviver.cpp`) watches one component: it pings
//! the component's `reviverPing` interface every `pingPeriod`, tolerates
//! `maxPingsToMiss` unanswered pings, and on expiry or a machined death
//! notification asks machined to create a replacement (`revive`). The
//! watched side, `ReviverSubject` (`lib/server/reviver_subject.cpp`), answers
//! pings and picks which reviver it serves by priority and by how long it
//! has gone without a ping from its current reviver.
//!
//! The reference process wraps both in `Reviver::handleTimeout`'s
//! `TIMEOUT_REATTACH` pass, which keeps the priorities dense (1..n in
//! baseline order) and re-activates the components that lost their target.
//!
//! The ping/create transports are not wired here: the module exposes the
//! state machines and the decisions (`TickOutcome`, `revive`, the subject's
//! accept/reject) so the Mercury lane can drive them when it lands. The
//! create request itself goes through machined and is modelled by the
//! coordinator returning the `create` parameter for `MachinedClient`.

/// `REVIVER_PING_NO` (`lib/server/reviver_common.hpp`): the watched component
/// refuses this reviver.
pub const REVIVER_PING_NO: u8 = 0;
/// `REVIVER_PING_YES`: the watched component accepts this reviver.
pub const REVIVER_PING_YES: u8 = 1;

/// `REVIVER_DEFAULT_PING_PERIOD`, seconds.
pub const DEFAULT_PING_PERIOD_SECS: f32 = 0.1;
/// `REVIVER_DEFAULT_SUBJECT_TIMEOUT`, seconds.
pub const DEFAULT_SUBJECT_TIMEOUT_SECS: f32 = 0.2;
/// `reviver/timeout`, seconds: how long a component may stay silent.
pub const DEFAULT_TIMEOUT_SECS: f32 = 3.0;
/// `reviver/reattachPeriod`, seconds: how often priorities are renumbered.
pub const DEFAULT_REATTACH_PERIOD_SECS: f32 = 10.0;
/// The subject's initial priority: no reviver is served yet.
pub const SUBJECT_PRIORITY_UNATTACHED: u8 = 0xff;

/// `maxPingsToMiss` when `reviver/timeoutInPings` is unset:
/// `ReviverConfig::postInit` rounds `timeout / pingPeriod`.
pub fn default_max_pings_to_miss() -> i32 {
    (DEFAULT_TIMEOUT_SECS / DEFAULT_PING_PERIOD_SECS + 0.5) as i32
}

/// A watched address: the host-order ip and port of the component's
/// reviver-ping interface.
pub type ComponentAddr = (u32, u16);

/// One monitored component. Mirrors `ComponentReviver`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentReviver {
    /// The `bw.xml` key under `reviver/`, e.g. `cellAppMgr`.
    pub config_name: String,
    /// The component's display name, e.g. `CellAppMgr`.
    pub name: String,
    /// What machined is asked to create, e.g. `cellappmgr`.
    pub create_param: String,
    /// Host-order address of the component's interface, when known.
    addr: Option<ComponentAddr>,
    /// The activation order; 0 means not currently activated.
    priority: u8,
    /// Milliseconds between pings.
    pub ping_period_ms: u32,
    /// Pings that may go unanswered before the component counts as dead.
    pub max_pings_to_miss: i32,
    /// The live countdown.
    pings_to_miss: i32,
    /// A ping timer is armed.
    timer_armed: bool,
    /// The component answered at least one ping since activation.
    is_attached: bool,
    /// The component is monitored at all (`--add`/`--del` and machined tags).
    is_enabled: bool,
}

impl ComponentReviver {
    /// Build a monitor with the reference config defaults.
    pub fn new(config_name: &str, name: &str, create_param: &str) -> Self {
        Self {
            config_name: config_name.to_string(),
            name: name.to_string(),
            create_param: create_param.to_string(),
            addr: None,
            priority: 0,
            ping_period_ms: (DEFAULT_PING_PERIOD_SECS * 1000.0) as u32,
            max_pings_to_miss: default_max_pings_to_miss(),
            pings_to_miss: 0,
            timer_armed: false,
            is_attached: false,
            is_enabled: true,
        }
    }

    pub fn priority(&self) -> u8 {
        self.priority
    }

    pub fn addr(&self) -> Option<ComponentAddr> {
        self.addr
    }

    pub fn is_attached(&self) -> bool {
        self.is_attached
    }

    pub fn pings_to_miss(&self) -> i32 {
        self.pings_to_miss
    }

    pub fn is_enabled(&self) -> bool {
        self.is_enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.is_enabled = enabled;
    }

    /// Record the component's interface address, from the machined
    /// `QUERY_INTERFACE` lookup at init or from a birth notification.
    pub fn set_addr(&mut self, addr: ComponentAddr) {
        self.addr = Some(addr);
    }

    /// `ComponentReviver::activate`: arm the ping timer with the given
    /// priority. Fails when the timer is already armed or the address is
    /// still unknown; on failure the priority is not taken.
    pub fn activate(&mut self, priority: u8) -> bool {
        self.is_attached = false;
        if !self.timer_armed && self.addr.is_some() {
            self.pings_to_miss = self.max_pings_to_miss;
            self.timer_armed = true;
            self.priority = priority;
            return true;
        }
        false
    }

    /// `ComponentReviver::deactivate`: drop the attachment and the timer.
    /// Returns whether a timer was cancelled.
    pub fn deactivate(&mut self) -> bool {
        self.is_attached = false;
        if self.timer_armed {
            self.timer_armed = false;
            self.priority = 0;
            return true;
        }
        false
    }

    /// One ping period elapsed (`ComponentReviver::handleTimeout`): while
    /// pings remain to miss, count one down and send the ping; once they
    /// are gone the component is dead and must be revived.
    pub fn tick(&mut self) -> TickOutcome {
        if !self.timer_armed {
            return TickOutcome::Idle;
        }
        if self.pings_to_miss > 0 {
            self.pings_to_miss -= 1;
            TickOutcome::Ping
        } else {
            TickOutcome::Revive
        }
    }

    /// The ping reply arrived (`handleMessage` with the reply arg).
    /// `REVIVER_PING_YES` resets the countdown and attaches; `REVIVER_PING_NO`
    /// detaches. Returns whether this reply attached the component.
    pub fn on_ping_reply(&mut self, code: u8) -> bool {
        if code == REVIVER_PING_YES {
            self.pings_to_miss = self.max_pings_to_miss;
            if !self.is_attached {
                self.is_attached = true;
                return true;
            }
            return false;
        }
        self.deactivate();
        false
    }

    /// `ComponentReviver::revive`: drop the target. The return value is the
    /// reference's `wasAttached`: only an attached component is worth
    /// replacing with a machined create.
    pub fn revive(&mut self) -> bool {
        let was_attached = self.is_attached;
        self.deactivate();
        self.addr = None;
        was_attached
    }

    /// A machined death notification arrived. When it names the watched
    /// address the component is gone; the caller replaces it when the
    /// return value says it had been attached. A death for another address
    /// is only a warning and leaves the state alone.
    pub fn on_death(&mut self, addr: ComponentAddr) -> bool {
        if Some(addr) == self.addr {
            return self.revive();
        }
        false
    }
}

/// What one ping period asked the runtime to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// No timer is armed for this component.
    Idle,
    /// Send a `reviverPing` to the watched address.
    Ping,
    /// The component is dead: replace it.
    Revive,
}

/// The reviver process logic over the baseline component list: priority
/// assignment, the reattach pass, and shutdown suppression. Mirrors `Reviver`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviverCoordinator {
    components: Vec<ComponentReviver>,
    shutting_down: bool,
    /// `reviver/shutDownOnRevive` (default true): the process stops after
    /// issuing one create.
    shut_down_on_revive: bool,
}

impl ReviverCoordinator {
    /// The five reference monitors in `component_reviver.cpp` declaration
    /// order, which is the baseline priority order.
    pub fn with_baseline() -> Self {
        let components = vec![
            ComponentReviver::new("cellAppMgr", "CellAppMgr", "cellappmgr"),
            ComponentReviver::new("baseAppMgr", "BaseAppMgr", "baseappmgr"),
            ComponentReviver::new("dbAppMgr", "DBAppMgr", "dbappmgr"),
            ComponentReviver::new("dbApp", "DBApp", "dbapp"),
            ComponentReviver::new("loginApp", "Login", "loginapp"),
        ];
        Self {
            components,
            shutting_down: false,
            shut_down_on_revive: true,
        }
    }

    pub fn components(&self) -> &[ComponentReviver] {
        &self.components
    }

    pub fn components_mut(&mut self) -> &mut [ComponentReviver] {
        &mut self.components
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    /// The reference init pass: enabled components take the priorities
    /// 1..n in list order (baseline order), whether or not an address is
    /// known yet — a monitor without an address fails to arm and stays at
    /// priority 0 for the reattach pass to pick up.
    pub fn assign_priorities(&mut self) {
        let mut priority = 0u8;
        for component in &mut self.components {
            if component.is_enabled {
                priority += 1;
                component.activate(priority);
            }
        }
    }

    /// `Reviver::handleTimeout`'s `TIMEOUT_REATTACH` pass: renumber the
    /// activated components into a dense 1..n sequence preserving their
    /// priority order, then re-activate the deactivated ones after them.
    /// `order` lists the deactivated components' indices in the order to
    /// re-activate them; the reference shuffles them randomly, callers that
    /// want that pass their own shuffle.
    pub fn reattach_pass(&mut self, order: &[usize]) {
        // Sort the activated components by current priority.
        let mut active: Vec<usize> = (0..self.components.len())
            .filter(|&i| self.components[i].is_enabled && self.components[i].priority > 0)
            .collect();
        active.sort_by_key(|&i| self.components[i].priority);

        let mut priority = 0u8;
        for i in active {
            priority += 1;
            if self.components[i].priority != priority {
                self.components[i].priority = priority;
            }
        }

        for &i in order {
            if i < self.components.len()
                && self.components[i].is_enabled
                && self.components[i].priority == 0
            {
                priority += 1;
                self.components[i].activate(priority);
            }
        }
    }

    /// `Reviver::shutDown`: the process is going away; every monitor is
    /// deactivated so no death notification can trigger a create.
    pub fn shut_down(&mut self) {
        self.shutting_down = true;
        for component in &mut self.components {
            if component.is_enabled {
                component.deactivate();
            }
        }
    }

    /// `Reviver::revive`: ask machined to create the component. Returns the
    /// `create` parameter for the machined `CreateMessage`, or `None` when
    /// the process is shutting down (the reference refuses with
    /// "Trying to revive a process while shutting down") or the name is
    /// unknown. A successful revive ends the process when
    /// `shutDownOnRevive` is on, its default.
    pub fn revive(&mut self, create_param: &str) -> Option<String> {
        if self.shutting_down {
            return None;
        }
        let known = self
            .components
            .iter()
            .any(|c| c.create_param == create_param && c.is_enabled);
        if !known {
            return None;
        }
        if self.shut_down_on_revive {
            self.shut_down();
        }
        Some(create_param.to_string())
    }
}

/// The watched side (`ReviverSubject`): answers pings and chooses which
/// reviver to serve — the current one, or a newcomer with a better
/// priority, or any newcomer once the served reviver has gone quiet for
/// longer than the subject timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviverSubject {
    /// The reviver currently being served.
    reviver_addr: Option<ComponentAddr>,
    /// When the served reviver last pinged, in milliseconds.
    last_ping_ms: u64,
    /// The served reviver's priority; `SUBJECT_PRIORITY_UNATTACHED` until
    /// one is accepted.
    priority: u8,
    /// Milliseconds after which a silent reviver is replaced.
    pub ms_timeout: u32,
}

impl ReviverSubject {
    /// Build with the reference `reviver/subjectTimeout` default.
    pub fn new() -> Self {
        Self {
            reviver_addr: None,
            last_ping_ms: 0,
            priority: SUBJECT_PRIORITY_UNATTACHED,
            ms_timeout: (DEFAULT_SUBJECT_TIMEOUT_SECS * 1000.0) as u32,
        }
    }

    pub fn reviver_addr(&self) -> Option<ComponentAddr> {
        self.reviver_addr
    }

    pub fn priority(&self) -> u8 {
        self.priority
    }

    /// `ReviverSubject::handleMessage`: decide whether `src` may serve this
    /// component and answer its ping. `now_ms` is the current monotonic
    /// time in milliseconds.
    pub fn handle_ping(&mut self, src: ComponentAddr, priority: u8, now_ms: u64) -> u8 {
        let mut accept = self.reviver_addr == Some(src);

        if !accept {
            if priority < self.priority {
                accept = true;
            } else {
                let ms_between_pings = now_ms.saturating_sub(self.last_ping_ms);
                if ms_between_pings > self.ms_timeout as u64 {
                    accept = true;
                }
            }
        }

        if accept {
            self.reviver_addr = Some(src);
            self.last_ping_ms = now_ms;
            self.priority = priority;
            REVIVER_PING_YES
        } else {
            REVIVER_PING_NO
        }
    }
}

impl Default for ReviverSubject {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> ReviverCoordinator {
        ReviverCoordinator::with_baseline()
    }

    // ------------------------------------------------------------------
    // BW-05-02-01 采集探活与过期
    // ------------------------------------------------------------------

    /// A healthy component keeps answering: the countdown resets on every
    /// reply and the monitor stays attached.
    #[test]
    fn answered_pings_keep_the_component_attached() {
        let mut component = ComponentReviver::new("cellAppMgr", "CellAppMgr", "cellappmgr");
        component.set_addr((1, 1000));
        assert!(component.activate(1));

        for _ in 0..(default_max_pings_to_miss() * 2) {
            assert_eq!(component.tick(), TickOutcome::Ping);
            component.on_ping_reply(REVIVER_PING_YES);
            assert!(component.is_attached());
        }
    }

    /// The delayed state: misses accumulate against the allowance, so one
    /// reply is not enough to refill the countdown if pings have been
    /// missing.
    #[test]
    fn delayed_pings_are_cumulative() {
        let mut component = ComponentReviver::new("cellAppMgr", "CellAppMgr", "cellappmgr");
        component.set_addr((1, 1000));
        component.activate(1);
        assert!(component.on_ping_reply(REVIVER_PING_YES));

        // Two unanswered pings are remembered, not reset.
        assert_eq!(component.tick(), TickOutcome::Ping);
        assert_eq!(component.tick(), TickOutcome::Ping);
        assert_eq!(component.pings_to_miss(), component.max_pings_to_miss - 2);

        // One late reply refills the whole allowance.
        assert!(!component.on_ping_reply(REVIVER_PING_YES));
        assert_eq!(component.pings_to_miss(), component.max_pings_to_miss);
    }

    /// Once every allowed ping is missed the monitor reports Revive; the
    /// caller then revives it, dropping the target.
    #[test]
    fn expired_component_reaches_revive() {
        let mut component = ComponentReviver::new("cellAppMgr", "CellAppMgr", "cellappmgr");
        component.set_addr((1, 1000));
        component.activate(1);
        assert!(component.on_ping_reply(REVIVER_PING_YES));

        // The reference allows maxPingsToMiss unanswered pings, then the
        // next tick revives.
        for _ in 0..component.max_pings_to_miss {
            assert_eq!(component.tick(), TickOutcome::Ping);
        }
        assert_eq!(component.tick(), TickOutcome::Revive);
        assert!(component.revive());
        // The revival detached it and cleared the target and priority.
        assert!(!component.is_attached());
        assert_eq!(component.addr(), None);
        assert_eq!(component.priority(), 0);
    }

    /// The recovered state: an outage that ends before the allowance runs
    /// out is healed in place — the monitor keeps its address and stays
    /// attached, so no create is issued.
    #[test]
    fn outage_then_recovery_needs_no_revive() {
        let mut component = ComponentReviver::new("cellAppMgr", "CellAppMgr", "cellappmgr");
        component.set_addr((1, 1000));
        component.activate(1);
        assert!(component.on_ping_reply(REVIVER_PING_YES));

        // Silent for the allowance minus one.
        for _ in 0..(component.max_pings_to_miss - 1) {
            assert_eq!(component.tick(), TickOutcome::Ping);
        }
        // The reply arrives just before expiry.
        assert!(!component.on_ping_reply(REVIVER_PING_YES));
        assert!(component.is_attached());
        assert_eq!(component.addr(), Some((1, 1000)));
        assert_eq!(component.tick(), TickOutcome::Ping);
    }

    // ------------------------------------------------------------------
    // BW-05-02-02 实现重启优先级
    // ------------------------------------------------------------------

    /// Give every monitor its interface address, as `findInterface` does at
    /// init, so the assignment pass can arm them.
    fn address_everyone(coordinator: &mut ReviverCoordinator) {
        for (i, component) in coordinator.components_mut().iter_mut().enumerate() {
            component.set_addr((1, 1000 + i as u16));
        }
    }

    /// The baseline assignment hands out priorities 1..n in the reference
    /// declaration order.
    #[test]
    fn priorities_follow_baseline_order() {
        let mut coordinator = baseline();
        address_everyone(&mut coordinator);
        coordinator.assign_priorities();
        let priorities: Vec<u8> = coordinator
            .components()
            .iter()
            .map(|c| c.priority())
            .collect();
        assert_eq!(priorities, vec![1, 2, 3, 4, 5]);
    }

    /// The reattach pass keeps the surviving components dense and in their
    /// priority order, then re-activates the deactivated ones after them.
    #[test]
    fn reattach_pass_renumbers_in_priority_order() {
        let mut coordinator = baseline();
        address_everyone(&mut coordinator);
        coordinator.assign_priorities();

        // CellAppMgr (1) and DBAppMgr (3) lose their targets.
        assert!(coordinator.components_mut()[0].deactivate());
        assert!(coordinator.components_mut()[2].deactivate());

        // The reference re-activates the shuffled deactivated set after the
        // active ones; this pass feeds them in list order.
        coordinator.reattach_pass(&[0, 2]);

        let priorities: Vec<u8> = coordinator
            .components()
            .iter()
            .map(|c| c.priority())
            .collect();
        // BaseAppMgr, DBApp and LoginApp keep their relative order,
        // renumbered 1..3; CellAppMgr and DBAppMgr rejoin as 4..5.
        assert_eq!(priorities, vec![4, 1, 5, 2, 3]);
    }

    /// A monitor that never found its address stays at priority 0 and is
    /// picked up by the reattach pass.
    #[test]
    fn unaddressed_monitor_is_picked_up_by_reattach() {
        let mut coordinator = baseline();
        // Only the first four find their interface; LoginApp's activate
        // fails and it stays 0.
        for i in 0..4 {
            coordinator.components_mut()[i].set_addr((1, 1000 + i as u16));
        }
        coordinator.assign_priorities();
        assert_eq!(coordinator.components()[4].priority(), 0);

        coordinator.reattach_pass(&[4]);
        assert_eq!(coordinator.components()[4].priority(), 0);

        // Once machined answers the interface query it joins the rotation.
        coordinator.components_mut()[4].set_addr((9, 20991));
        coordinator.reattach_pass(&[4]);
        assert_eq!(coordinator.components()[4].priority(), 5);
    }

    /// A subject serves a newcomer only when its priority is better
    /// (lower) than the served reviver's.
    #[test]
    fn subject_prefers_lower_priority_reviver() {
        let mut subject = ReviverSubject::new();
        let now = 1000u64;

        // The first reviver is always accepted.
        assert_eq!(subject.handle_ping((1, 1), 3, now), REVIVER_PING_YES);
        assert_eq!(subject.reviver_addr(), Some((1, 1)));

        // A better priority wins immediately.
        assert_eq!(subject.handle_ping((2, 1), 2, now + 1), REVIVER_PING_YES);
        assert_eq!(subject.reviver_addr(), Some((2, 1)));

        // A worse priority is refused while the served reviver keeps pinging.
        assert_eq!(subject.handle_ping((3, 1), 5, now + 2), REVIVER_PING_NO);
        assert_eq!(subject.reviver_addr(), Some((2, 1)));
    }

    /// The served reviver must keep pinging: once it is quiet past the
    /// subject timeout a stranger with any priority takes over.
    #[test]
    fn subject_reaccepts_a_reviver_after_timeout() {
        let mut subject = ReviverSubject::new();
        let now = 0u64;
        assert_eq!(subject.handle_ping((1, 1), 5, now), REVIVER_PING_YES);

        // Just inside the timeout the stranger is refused.
        assert_eq!(
            subject.handle_ping((2, 1), 9, now + subject.ms_timeout as u64),
            REVIVER_PING_NO
        );
        // Past it, the stranger takes over even with a worse priority.
        assert_eq!(
            subject.handle_ping((2, 1), 9, now + subject.ms_timeout as u64 + 1),
            REVIVER_PING_YES
        );
        assert_eq!(subject.reviver_addr(), Some((2, 1)));
    }

    // ------------------------------------------------------------------
    // BW-05-02-03 异常退出后重启并重新登记
    // ------------------------------------------------------------------

    /// The full abnormal-exit loop: a death notification for the watched
    /// address produces a machined create; the replacement registers, its
    /// birth notification re-binds the monitor, and the first ping
    /// re-attaches it to serviceable state.
    #[test]
    fn death_triggers_create_and_birth_restores_service() {
        let mut coordinator = baseline();
        let addr = (7u32, 20991u16);
        coordinator.components_mut()[0].set_addr(addr);
        coordinator.assign_priorities();
        assert!(!coordinator.components()[0].is_attached());

        // Attach through a ping reply first.
        assert!(coordinator.components_mut()[0].on_ping_reply(REVIVER_PING_YES));

        // The component dies; the death notification names its address.
        let was_attached = coordinator.components_mut()[0].on_death(addr);
        assert!(was_attached);

        // The reviver asks machined to create a replacement.
        let create = coordinator.revive("cellappmgr");
        assert_eq!(create.as_deref(), Some("cellappmgr"));

        // The replacement registers; machined delivers the birth
        // notification and the monitor re-binds to the new address.
        let new_addr = (7u32, 30001u16);
        coordinator.components_mut()[0].set_addr(new_addr);
        assert!(coordinator.components_mut()[0].activate(1));
        assert_eq!(coordinator.components_mut()[0].tick(), TickOutcome::Ping);
        assert!(coordinator.components_mut()[0].on_ping_reply(REVIVER_PING_YES));
        assert!(coordinator.components()[0].is_attached());
    }

    /// A death notification for some other component's address never
    /// touches this monitor.
    #[test]
    fn death_for_another_address_is_ignored() {
        let mut component = ComponentReviver::new("cellAppMgr", "CellAppMgr", "cellappmgr");
        let addr = (7u32, 20991u16);
        component.set_addr(addr);
        component.activate(1);
        assert!(component.on_ping_reply(REVIVER_PING_YES));

        assert!(!component.on_death((8u32, 20992u16)));
        assert!(component.is_attached());
        assert_eq!(component.addr(), Some(addr));
    }

    // ------------------------------------------------------------------
    // BW-05-02-04 识别正常关服
    // ------------------------------------------------------------------

    /// During a normal shutdown no create is issued, even when death
    /// notifications are still in flight.
    #[test]
    fn shutdown_suppresses_revives() {
        let mut coordinator = baseline();
        coordinator.assign_priorities();

        coordinator.shut_down();
        assert!(coordinator.is_shutting_down());
        // Every enabled monitor was deactivated with the process.
        assert!(
            coordinator
                .components()
                .iter()
                .all(|c| !c.is_attached() && c.priority() == 0)
        );

        // A late death notification cannot start a create.
        assert!(!coordinator.components_mut()[0].on_death((1, 1)));
        assert_eq!(coordinator.revive("cellappmgr"), None);
    }

    /// With `shutDownOnRevive` on (the default) a successful revive stops
    /// the process, so a second failing component cannot start another
    /// create.
    #[test]
    fn revive_stops_the_process_by_default() {
        let mut coordinator = baseline();
        coordinator.assign_priorities();

        assert_eq!(
            coordinator.revive("cellappmgr").as_deref(),
            Some("cellappmgr")
        );
        assert!(coordinator.is_shutting_down());
        assert_eq!(coordinator.revive("baseappmgr"), None);
    }

    /// An unknown or disabled create target is refused outright.
    #[test]
    fn revive_refuses_unknown_targets() {
        let mut coordinator = baseline();
        coordinator.components_mut()[4].set_enabled(false);

        assert_eq!(coordinator.revive("nosuchrole"), None);
        assert_eq!(coordinator.revive("loginapp"), None);
        assert!(!coordinator.is_shutting_down());
    }

    /// A monitor whose timer is not armed ticks idle.
    #[test]
    fn unarmed_monitor_ticks_idle() {
        let mut component = ComponentReviver::new("cellAppMgr", "CellAppMgr", "cellappmgr");
        assert_eq!(component.tick(), TickOutcome::Idle);
    }
}
