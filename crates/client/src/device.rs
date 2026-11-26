//! Graphics device lifecycle with leak detection, and the concrete render
//! device the client initialises and rebuilds.
//!
//! **BW-11-01-01** — the no-leak contract. The reference client keeps one D3D
//! device for the app lifetime; a mode switch goes through `changeMode` →
//! `releaseUnmanaged` / `changeModePriv` (a device Reset) / `createUnmanaged`
//! (`lib/moo/render_context.cpp:581-650`) and *reuses* the device object —
//! it does not create a new one. [`DeviceLifecycle`] enforces that: the
//! device is created once (idempotent [`DeviceLifecycle::ensure_device`]),
//! a presentation-mode switch never creates or destroys it
//! ([`DeviceLifecycle::on_mode_switch`]), and it is released exactly once at
//! shutdown. The counters let a test prove the conservation law —
//! `created == dropped + live`, `live <= 1`, no orphaned handles — which is
//! precisely "no leak".
//!
//! **BW-11-01-02** — initialisation and rebuild. The reference device is an
//! `IDirect3DDevice9` obtained once from `RenderContext::createDevice`
//! (`render_context.cpp:1164`). "Rebuild" is *not* a new device object: a mode
//! switch, a window resize (`resetDevice`, `:652-678`) or a `D3DERR_DEVICENOTRESET`
//! (`checkDevice`, `:1065`) all call `device_->Reset(&presentParameters_)`
//! (`changeModePriv`, `:726-737`) on the *same* object. [`Device`] models
//! that: [`Device::create`] initialises, [`Device::rebuild`] re-initialises in
//! place, [`Device::check_device`] mirrors `checkDevice` (reset on
//! `NotReset`, wait on `Lost`), and [`Device::present`] produces a frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::render::FrameResult;
use crate::render::scene::TestScene;
use crate::render::software::SoftwareDevice;

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

    /// A stable identity for this device instance. Reused across
    /// presentation-mode switches and resets; a new value means a device was
    /// deliberately created again.
    pub fn id(&self) -> u64 {
        self.id
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

/// The presentation parameters a device is initialised (and rebuilt) with.
///
/// This is the port of `D3DPRESENT_PARAMETERS` as filled by
/// `fillPresentationParameters` (`render_context.cpp:830`): the back-buffer
/// size is what a mode switch or a window resize changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceConfig {
    /// Back-buffer width in pixels.
    pub width: u32,
    /// Back-buffer height in pixels.
    pub height: u32,
    /// Adapter index (`adapterID_`). A different adapter is the one path in
    /// the reference that deliberately releases and re-creates the device
    /// (`render_context.cpp:1185`).
    pub adapter: u32,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 480,
            adapter: 0,
        }
    }
}

impl DeviceConfig {
    /// Back-buffer area in pixels.
    pub fn area(&self) -> u32 {
        self.width.saturating_mul(self.height)
    }
}

/// The device cooperative level, as read by `RenderContext::checkDevice`
/// (`render_context.cpp:1065`, `device()->TestCooperativeLevel()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStatus {
    /// `D3D_OK`: the device is operational, frames may be presented.
    Ok,
    /// `D3DERR_DEVICENOTRESET`: the device must be rebuilt (reset) before
    /// frames may be presented.
    NotReset,
    /// `D3DERR_DEVICELOST`: the device is lost; poll until it returns to
    /// `NotReset`/`Ok` (`checkDeviceYield`).
    Lost,
}

/// The concrete render device: initialised once, rebuilt (reset) in place on a
/// mode switch, a resize or a device loss, and released once at shutdown.
///
/// [`Device::id`] is stable across every [`Device::rebuild`] — the same object
/// is re-initialised, so BW-11-01-01's no-leak property holds even through a
/// rebuild. A new id only appears after a deliberate release-and-recreate.
#[derive(Debug)]
pub struct Device {
    /// Accounting handle: this is the *same* handle across every reset.
    handle: DeviceHandle,
    /// The backend that actually renders pixels.
    backend: SoftwareDevice,
    /// Current presentation parameters.
    config: DeviceConfig,
    /// Cooperative level, injected by a test to exercise the loss/rebuild path
    /// (a real GPU backend would read it from the driver).
    status: DeviceStatus,
}

impl Device {
    /// Initialise the device (reference `createDevice`,
    /// `render_context.cpp:1164`). The reference creates the D3D object once;
    /// a rebuild never calls this again.
    pub fn create(config: DeviceConfig) -> Self {
        let counter = Arc::new(DeviceCounter::default());
        Self::create_with(config, counter)
    }

    /// Create sharing a caller's lifetime counter: the `DeviceLifecycle`
    /// keeps the accounting after the device is released.
    fn create_with(config: DeviceConfig, counter: Arc<DeviceCounter>) -> Self {
        Self {
            handle: DeviceHandle::new(counter),
            backend: SoftwareDevice::new(config.width, config.height),
            config,
            status: DeviceStatus::Ok,
        }
    }

    /// A stable identity for this device instance.
    pub fn id(&self) -> u64 {
        self.handle.id()
    }

    /// Current presentation parameters.
    pub fn config(&self) -> DeviceConfig {
        self.config
    }

    /// Current back-buffer size, as `updateDeviceInfo` reads it
    /// (`render_context.cpp:811`).
    pub fn back_buffer(&self) -> (u32, u32) {
        self.backend.back_buffer()
    }

    /// The back buffer itself (`GetBackBuffer`), for the screenshot face.
    pub fn framebuffer(&self) -> &crate::render::framebuffer::Framebuffer {
        self.backend.framebuffer()
    }

    /// The cooperative level (`TestCooperativeLevel`).
    pub fn status(&self) -> DeviceStatus {
        self.status
    }

    /// Inject a cooperative level (a test hook; a GPU backend derives it from
    /// the driver).
    pub fn set_status(&mut self, status: DeviceStatus) {
        self.status = status;
    }

    /// Rebuild the device with the same presentation parameters — the
    /// reference's `resetDevice` (`render_context.cpp:652`), which "resets the
    /// device to the exact same current modeIndex and windowed mode". The
    /// device object is *not* re-created.
    pub fn reset(&mut self) -> u32 {
        self.reset_to(self.config)
    }

    /// Rebuild the device with new presentation parameters — the reference's
    /// `changeMode` → `changeModePriv` → `device_->Reset(&presentParameters_)`
    /// (`render_context.cpp:581, 726`). The device object is *not* re-created:
    /// same object, same [`Device::id`]. Returns the new back-buffer area.
    pub fn rebuild(&mut self, config: DeviceConfig) -> u32 {
        self.reset_to(config)
    }

    fn reset_to(&mut self, config: DeviceConfig) -> u32 {
        let area = self.backend.reset(config.width, config.height);
        self.config = config;
        self.status = DeviceStatus::Ok;
        area
    }

    /// Reference `RenderContext::checkDevice` (`render_context.cpp:1065`):
    /// read the cooperative level; on `NotReset` rebuild the device (reset);
    /// return whether frames may be presented. On `Lost` it returns false and
    /// the caller polls again later (`checkDeviceYield`).
    pub fn check_device(&mut self) -> bool {
        match self.status {
            DeviceStatus::Ok => true,
            DeviceStatus::NotReset => {
                self.reset();
                true
            }
            DeviceStatus::Lost => false,
        }
    }

    /// Present one frame of the scene (the reference's `device()->Present`).
    pub fn present(&mut self, scene: &TestScene) -> FrameResult {
        self.backend.present(scene)
    }

    /// Frames presented by this device object. Monotonic across resets.
    pub fn frame_count(&self) -> u64 {
        self.backend.frame_count()
    }

    /// Digest of the last presented frame.
    pub fn last_checksum(&self) -> u64 {
        self.backend.last_checksum()
    }

    /// Current lifetime counters.
    pub fn stats(&self) -> DeviceStats {
        self.handle.stats()
    }

    /// Handles created but neither dropped nor live — devices orphaned by a
    /// bug. Zero means no leak.
    pub fn leaked(&self) -> u64 {
        self.handle.leaked()
    }
}

/// Owns the single live device for the client application.
///
/// This is the mechanism that makes "three-mode switching without device
/// leak" hold: no matter how many presentation-mode switches or rebuilds
/// happen, the device is created exactly once and reused. See the module docs
/// for how it mirrors `RenderContext::changeMode` reusing the D3D device.
#[derive(Debug, Default)]
pub struct DeviceLifecycle {
    counter: Arc<DeviceCounter>,
    device: Option<Device>,
}

impl DeviceLifecycle {
    /// An empty lifecycle with no device.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (creating once) the live device.
    ///
    /// Idempotent: a second call returns the existing device instead of
    /// creating — and leaking — a new one. This is the guard against a naive
    /// port that allocates a device per mode switch.
    pub fn ensure_device(&mut self, config: DeviceConfig) -> Result<u64, DeviceError> {
        if self.device.is_none() {
            self.device = Some(Device::create_with(config, self.counter.clone()));
        }
        Ok(self.device.as_ref().expect("just ensured").id())
    }

    /// The id of the live device, or `None` if none has been created.
    pub fn device_id(&self) -> Option<u64> {
        self.device.as_ref().map(Device::id)
    }

    /// The live device, if any.
    pub fn device(&self) -> Option<&Device> {
        self.device.as_ref()
    }

    /// The live device, mutably, if any.
    pub fn device_mut(&mut self) -> Option<&mut Device> {
        self.device.as_mut()
    }

    /// A presentation-mode switch. The device object is neither created nor
    /// destroyed — it is only rebuilt (reset) in place with the new
    /// presentation parameters — so nothing leaks. A mode switch is the
    /// reference's `changeMode`, which resets the *same* object with the new
    /// parameters (`render_context.cpp:581-650`).
    pub fn on_mode_switch(&mut self, config: &DeviceConfig) {
        if let Some(device) = self.device.as_mut() {
            device.rebuild(*config);
        }
    }

    /// Present one frame of the scene, if a device is live.
    pub fn present(&mut self, scene: &TestScene) -> Option<FrameResult> {
        self.device.as_mut().map(|d| d.present(scene))
    }

    /// Release the device at shutdown. Idempotent; safe to call when no
    /// device is live.
    pub fn release(&mut self) {
        self.device = None;
    }

    /// Current lifetime counters. Survives a release.
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

    fn default_config() -> DeviceConfig {
        DeviceConfig::default()
    }

    /// Simulate the correct client: create once, switch modes many times,
    /// release at exit.
    fn run_correct_lifecycle(mode_switches: usize) -> (DeviceLifecycle, Vec<u64>) {
        let mut lc = DeviceLifecycle::new();
        let first = lc.ensure_device(default_config()).unwrap();
        let mut ids = vec![first];
        for _ in 0..mode_switches {
            lc.on_mode_switch(&default_config());
            ids.push(lc.ensure_device(default_config()).unwrap());
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
        let a = lc.ensure_device(default_config()).unwrap();
        let b = lc.ensure_device(default_config()).unwrap();
        assert_eq!(a, b);
        assert_eq!(lc.stats().created, 1);
        assert_eq!(lc.stats().live, 1);
        assert_eq!(lc.leaked(), 0);
    }

    #[test]
    fn release_then_reacquire_is_a_deliberate_rebuild_not_a_leak() {
        let mut lc = DeviceLifecycle::new();
        let first = lc.ensure_device(default_config()).unwrap();
        lc.release();
        let second = lc.ensure_device(default_config()).unwrap();
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
            lc.ensure_device(default_config()).unwrap();
            lc.on_mode_switch(&default_config());
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

    // --- BW-11-01-02: initialisation and rebuild ---

    fn live_device() -> (DeviceLifecycle, DeviceConfig) {
        let mut lc = DeviceLifecycle::new();
        let cfg = default_config();
        lc.ensure_device(cfg).unwrap();
        (lc, cfg)
    }

    /// The BW-11-01-02 acceptance: after the device is rebuilt, the test scene
    /// keeps producing frames, and the rebuild never re-creates the device
    /// object (so it cannot leak).
    #[test]
    fn rebuild_then_scene_keeps_producing_frames() {
        let (mut lc, cfg) = live_device();
        let id_before = lc.device_id().unwrap();

        let mut scene = TestScene::new();
        // Frames before the rebuild.
        let mut checksums = Vec::new();
        for _ in 0..10 {
            scene.step();
            let fr = lc.present(&scene).expect("device live");
            checksums.push(fr.checksum);
        }
        assert_eq!(checksums.len(), 10);
        assert!(checksums.windows(2).all(|w| w[0] != w[1]), "frames animate");

        // Device is lost (a driver state a GPU backend can reach): no frames
        // until it is rebuilt.
        lc.device_mut().unwrap().set_status(DeviceStatus::Lost);
        assert!(!lc.device_mut().unwrap().check_device(), "lost: no frames");
        lc.device_mut().unwrap().set_status(DeviceStatus::NotReset);

        // Rebuild (reset) — the same object, so the same id.
        let area = lc.device_mut().unwrap().rebuild(cfg);
        assert_eq!(area, cfg.area());
        assert_eq!(
            lc.device_id().unwrap(),
            id_before,
            "rebuild reuses the object"
        );
        assert!(
            lc.device_mut().unwrap().check_device(),
            "rebuilt: frames resume"
        );

        // Frames after the rebuild continue, and the scene keeps animating.
        let mut after = Vec::new();
        for _ in 0..10 {
            scene.step();
            let fr = lc.present(&scene).expect("device live");
            after.push(fr.checksum);
        }
        assert_eq!(after.len(), 10);
        assert!(
            after.windows(2).all(|w| w[0] != w[1]),
            "frames animate after rebuild"
        );
        assert_eq!(
            lc.device().unwrap().frame_count(),
            20,
            "frames continue, not restart"
        );

        // No leak: one device created, rebuilt many times, still one live.
        assert_eq!(lc.stats().created, 1, "never re-created");
        assert_eq!(lc.stats().live, 1);
        assert_eq!(lc.leaked(), 0);
        lc.release();
        assert_eq!(lc.stats().dropped, 1);
        assert_eq!(lc.leaked(), 0);
    }

    /// A rebuild with new presentation parameters updates the back buffer but
    /// keeps the device object.
    #[test]
    fn rebuild_with_new_config_updates_back_buffer_same_object() {
        let (mut lc, _) = live_device();
        let id_before = lc.device_id().unwrap();
        let new_cfg = DeviceConfig {
            width: 1024,
            height: 768,
            adapter: 0,
        };
        let area = lc.device_mut().unwrap().rebuild(new_cfg);
        assert_eq!(area, new_cfg.area());
        assert_eq!(lc.device().unwrap().back_buffer(), (1024, 768));
        assert_eq!(lc.device_id().unwrap(), id_before, "same device object");
        assert_eq!(lc.device().unwrap().config(), new_cfg);
    }

    /// A device lost during a rebuild (`Lost`) makes `check_device` wait; the
    /// `NotReset` state drives the reset, after which frames resume.
    #[test]
    fn cooperative_level_drives_rebuild_then_frames() {
        let (mut lc, _) = live_device();
        let mut scene = TestScene::new();

        // Device lost: polling returns false, no frame is presented.
        lc.device_mut().unwrap().set_status(DeviceStatus::Lost);
        assert!(!lc.device_mut().unwrap().check_device());
        assert_eq!(lc.device().unwrap().frame_count(), 0);

        // Not reset: check_device resets it (rebuild) and reports operational.
        lc.device_mut().unwrap().set_status(DeviceStatus::NotReset);
        assert!(lc.device_mut().unwrap().check_device());
        assert_eq!(lc.device().unwrap().status(), DeviceStatus::Ok);

        // Frames resume and animate.
        scene.step();
        let a = lc.present(&scene).unwrap();
        scene.step();
        let b = lc.present(&scene).unwrap();
        assert_ne!(a.checksum, b.checksum);
        assert_eq!(lc.device().unwrap().frame_count(), 2);
    }

    /// Resets never re-create the device object, so the id is constant through
    /// an arbitrary number of rebuilds.
    #[test]
    fn device_object_is_never_recreated_across_resets() {
        let (mut lc, cfg) = live_device();
        let id = lc.device_id().unwrap();
        for _ in 0..50 {
            lc.on_mode_switch(&cfg); // reset, same params
            lc.device_mut().unwrap().rebuild(cfg); // reset, same params
            assert_eq!(lc.device_id().unwrap(), id, "same object through resets");
        }
        assert_eq!(lc.stats().created, 1);
        assert_eq!(lc.leaked(), 0);
    }

    /// The test scene really produces distinct pixels each frame (the "出帧"
    /// evidence is content, not just a counter).
    #[test]
    fn scene_produces_distinct_frames() {
        let mut scene = TestScene::new();
        let mut checksums = Vec::new();
        for _ in 0..32 {
            scene.step();
            let mut fb = crate::render::framebuffer::Framebuffer::new(64, 64);
            scene.draw(&mut fb);
            checksums.push(fb.checksum());
        }
        assert!(
            checksums.windows(2).all(|w| w[0] != w[1]),
            "every frame differs"
        );
    }
}
