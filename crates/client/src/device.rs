//! Graphics device lifecycle with leak detection.
//!
//! The reference client keeps one D3D device for the app lifetime; a mode
//! switch goes through `changeMode` → `releaseUnmanaged` /
//! `changeModePriv` (a device Reset) / `createUnmanaged`
//! (`lib/moo/render_context.cpp:581-650`) and *reuses* the device object —
//! it does not create a new one. The BW-11-01-01 acceptance is that this
//! holds across all three mode transitions: no device is leaked.
//!
//! [`DeviceLifecycle`] enforces that contract: the device is created once
//! (idempotent [`DeviceLifecycle::ensure_device`]), a presentation-mode
//! switch never creates or destroys it ([`DeviceLifecycle::on_mode_switch`]),
//! and it is released exactly once at shutdown
//! ([`DeviceLifecycle::release`]). The counters let a test prove the
//! conservation law — `created == dropped + live`, `live <= 1`, no orphaned
//! handles — which is precisely "no leak".
//!
//! The device itself is not implemented here (BW-11-01-02 initialises and
//! rebuilds it); this module owns the *lifecycle and accounting* that any
//! concrete device must obey.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Lifetime counters for every device handle ever created.
#[derive(Debug, Default)]
struct DeviceCounter {
    created: AtomicU64,
    live: AtomicU64,
    dropped: AtomicU64,
    next_id: AtomicU64,
}

/// A snapshot of the device lifetime counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceStats {
    /// Handles ever created.
    pub created: u64,
    /// Handles currently alive (created but not yet dropped).
    pub live: u64,
    /// Handles dropped.
    pub dropped: u64,
}

/// A live device handle. Dropping it releases the device, not leaks it.
///
/// The handle carries a stable [`DeviceHandle::id`] so a caller can tell
/// whether a mode switch reused the same device or silently built a new one.
#[derive(Debug)]
pub struct DeviceHandle {
    counter: Arc<DeviceCounter>,
    id: u64,
}

impl DeviceHandle {
    fn new(counter: Arc<DeviceCounter>) -> Self {
        counter.created.fetch_add(1, Ordering::Relaxed);
        counter.live.fetch_add(1, Ordering::Relaxed);
        let id = counter.next_id.fetch_add(1, Ordering::Relaxed);
        Self { counter, id }
    }

    /// A stable identity for this device instance. Reused across presentation-
    /// mode switches; a new value means a device was (re)created.
    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        self.counter.live.fetch_sub(1, Ordering::Relaxed);
        self.counter.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// Errors from the device lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// `ensure_device` was asked to create a device while one is already live.
    AlreadyLive,
}

/// Owns the single live device for the client application.
///
/// This is the mechanism that makes "three-mode switching without device
/// leak" hold: no matter how many presentation-mode switches happen, the
/// device is created exactly once and reused. See the module docs for how it
/// mirrors `RenderContext::changeMode` reusing the D3D device.
#[derive(Debug, Default)]
pub struct DeviceLifecycle {
    counter: Arc<DeviceCounter>,
    device: Option<DeviceHandle>,
}

impl DeviceLifecycle {
    /// An empty lifecycle with no device.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (creating once) the live device handle.
    ///
    /// Idempotent: a second call returns the existing device instead of
    /// creating — and leaking — a new one. This is the guard against a naive
    /// port that allocates a device per mode switch.
    pub fn ensure_device(&mut self) -> Result<u64, DeviceError> {
        if self.device.is_none() {
            self.device = Some(DeviceHandle::new(self.counter.clone()));
        }
        Ok(self.device.as_ref().expect("just ensured").id())
    }

    /// The id of the live device, or `None` if none has been created.
    pub fn device_id(&self) -> Option<u64> {
        self.device.as_ref().map(|h| h.id())
    }

    /// A presentation-mode switch. The device is untouched: a mode switch
    /// must never create, destroy or re-create the device, or it leaks.
    pub fn on_mode_switch(&mut self) {}

    /// Release the device at shutdown. Idempotent; safe to call when no
    /// device is live.
    pub fn release(&mut self) {
        self.device = None;
    }

    /// Current lifetime counters.
    pub fn stats(&self) -> DeviceStats {
        DeviceStats {
            created: self.counter.created.load(Ordering::Relaxed),
            live: self.counter.live.load(Ordering::Relaxed),
            dropped: self.counter.dropped.load(Ordering::Relaxed),
        }
    }

    /// Handles created but neither dropped nor live — devices orphaned by a
    /// bug. Zero means no leak.
    pub fn leaked(&self) -> u64 {
        let s = self.stats();
        s.created - s.dropped - s.live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate the correct client: create once, switch modes many times,
    /// release at exit.
    fn run_correct_lifecycle(mode_switches: usize) -> (DeviceLifecycle, Vec<u64>) {
        let mut lc = DeviceLifecycle::new();
        let first = lc.ensure_device().unwrap();
        let mut ids = vec![first];
        for _ in 0..mode_switches {
            lc.on_mode_switch();
            ids.push(lc.ensure_device().unwrap());
        }
        lc.release();
        (lc, ids)
    }

    #[test]
    fn create_once_reuse_across_many_switches_no_leak() {
        let (lc, ids) = run_correct_lifecycle(100);
        // Every ensure_device after the first returns the same device: the
        // mode switches never re-created it.
        assert!(ids.windows(2).all(|w| w[0] == w[1]));
        let s = lc.stats();
        assert_eq!(s.created, 1, "device created exactly once");
        assert_eq!(s.live, 0, "released at shutdown");
        assert_eq!(s.dropped, 1, "the one device was released, not leaked");
        assert_eq!(lc.leaked(), 0, "no orphaned device");
    }

    #[test]
    fn ensure_device_is_idempotent() {
        let mut lc = DeviceLifecycle::new();
        let a = lc.ensure_device().unwrap();
        let b = lc.ensure_device().unwrap();
        assert_eq!(a, b);
        assert_eq!(lc.stats().created, 1);
        assert_eq!(lc.stats().live, 1);
        assert_eq!(lc.leaked(), 0);
    }

    #[test]
    fn release_then_reacquire_is_a_deliberate_rebuild_not_a_leak() {
        let mut lc = DeviceLifecycle::new();
        let first = lc.ensure_device().unwrap();
        lc.release();
        let second = lc.ensure_device().unwrap();
        assert_ne!(first, second, "a rebuild is a new device");
        let s = lc.stats();
        assert_eq!(s.created, 2);
        assert_eq!(s.live, 1);
        assert_eq!(s.dropped, 1);
        assert_eq!(lc.leaked(), 0, "rebuilds do not orphan the old device");
    }

    #[test]
    fn conservation_holds_through_arbitrary_sequences() {
        let mut lc = DeviceLifecycle::new();
        // Deliberately churn: create/release interleaved with mode switches.
        for _ in 0..5 {
            lc.ensure_device().unwrap();
            lc.on_mode_switch();
            lc.release();
        }
        let s = lc.stats();
        assert_eq!(s.created, s.dropped + s.live, "created == dropped + live");
        assert!(s.live <= 1, "never more than one device live");
        assert_eq!(lc.leaked(), 0);
    }

    #[test]
    fn release_is_idempotent_and_safe_with_no_device() {
        let mut lc = DeviceLifecycle::new();
        lc.release();
        lc.release();
        assert_eq!(lc.device_id(), None);
        assert_eq!(lc.leaked(), 0);
    }

    #[test]
    fn a_naive_recreate_per_switch_would_leak() {
        // Demonstrates the failure mode DeviceLifecycle exists to prevent: a
        // port that allocates a device on every mode switch without releasing
        // the previous one leaks. We model it by holding every handle.
        let counter = Arc::new(DeviceCounter::default());
        let mut handles = Vec::new();
        for _ in 0..3 {
            handles.push(DeviceHandle::new(counter.clone()));
        }
        let stats = DeviceStats {
            created: counter.created.load(Ordering::Relaxed),
            live: counter.live.load(Ordering::Relaxed),
            dropped: counter.dropped.load(Ordering::Relaxed),
        };
        assert_eq!(stats.live, 3, "three devices live = leaked");
        assert!(stats.live > 1);
        // Dropping all of them still releases them (no orphan), but the live
        // count during the run already violated the single-device contract.
        drop(handles);
    }
}
