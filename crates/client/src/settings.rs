//! Display settings and the object LOD setting (BW-11-01-05).
//!
//! The reference registers an "OBJECT_LOD" graphics setting in
//! `LodSettings::init` (`lib/moo/lod_settings.cpp:22-47`): a set of options,
//! each with a label and a float value, one active. The active option is what
//! `applyLodBias` and `calculateLod` use (`lod_settings.cpp:80-165`):
//!
//! * `applyLodBias(distance) = distance / lodOptions_[activeOption]`
//!   (`:96-103`) — the object's distance is scaled by the active LOD factor;
//! * `applyLodBias(lodNear, lodFar)` multiplies the culling bounds by the same
//!   factor (`:110-121`);
//! * `calculateLod(position, yScale)`: view-space depth, clamped to 0, biased,
//!   then divided by `max(1, yScale)` and multiplied by `lodZoomFactor()`
//!   (`:140-165`).
//!
//! The caller (a model) picks a LOD level from that value against its own
//! fixed distance thresholds, so the same threshold always produces the same
//! switch — the acceptance. "Applying the display settings" is the other half:
//! [`DisplaySettings`] carries the resolution / refresh / windowed / vsync a
//! graphics-settings change applies to the device.

/// The present interval a display setting requests (the reference's
/// `D3DPRESENT_INTERVAL_IMMEDIATE` vs `D3DPRESENT_INTERVAL_ONE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentInterval {
    /// `D3DPRESENT_INTERVAL_IMMEDIATE`: no vsync.
    Immediate,
    /// `D3DPRESENT_INTERVAL_ONE`: vsync to the refresh rate.
    One,
}

/// The display settings a change applies (resolution, refresh, windowed,
/// vsync). `device_config` maps them onto the device's presentation
/// parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplaySettings {
    /// Back-buffer width in pixels.
    pub width: u32,
    /// Back-buffer height in pixels.
    pub height: u32,
    /// Refresh rate in Hz (0 = the desktop default, as the reference uses for
    /// windowed mode).
    pub refresh_hz: u32,
    /// Windowed vs fullscreen.
    pub windowed: bool,
    /// `PresentationInterval`.
    pub present_interval: PresentInterval,
}

impl DisplaySettings {
    /// The default windowed settings at the given size.
    pub fn windowed(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            refresh_hz: 0,
            windowed: true,
            present_interval: PresentInterval::One,
        }
    }

    /// Fullscreen at the given mode.
    pub fn fullscreen(width: u32, height: u32, refresh_hz: u32) -> Self {
        Self {
            width,
            height,
            refresh_hz,
            windowed: false,
            present_interval: PresentInterval::One,
        }
    }
}

/// A single LOD option (label + value), as `LodSettings::init` reads from the
/// res XML (`lod_settings.cpp:27-45`) or from the default HIGH.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodOption {
    /// The option label, e.g. "HIGH".
    pub label: &'static str,
    /// The LOD factor, e.g. `1.0` for HIGH.
    pub value: f32,
}

/// The OBJECT_LOD setting: a list of LOD options, one active.
///
/// The active option is what [`LodSettings::apply_lod_bias`] and
/// [`LodSettings::calculate_lod`] use; switching it is the "apply the display
/// setting" action (`GraphicsSetting::setOption`).
#[derive(Debug, Clone, PartialEq)]
pub struct LodSettings {
    options: Vec<LodOption>,
    active: usize,
}

impl LodSettings {
    /// The reference's no-XML default: a single HIGH option at factor 1.0
    /// (`lod_settings.cpp:49-52`).
    pub fn high() -> Self {
        Self {
            options: vec![LodOption {
                label: "HIGH",
                value: 1.0,
            }],
            active: 0,
        }
    }

    /// From a set of options, the first active.
    pub fn new(options: Vec<LodOption>) -> Self {
        assert!(
            !options.is_empty(),
            "a LOD setting needs at least one option"
        );
        Self { options, active: 0 }
    }

    /// The options.
    pub fn options(&self) -> &[LodOption] {
        &self.options
    }

    /// The active option index.
    pub fn active(&self) -> usize {
        self.active
    }

    /// Select the active option (apply the display setting). Returns false if
    /// the index is out of range (the change is ignored, as `GraphicsSetting`
    /// rejects an out-of-range `setOption`).
    pub fn set_active(&mut self, index: usize) -> bool {
        if index < self.options.len() {
            self.active = index;
            true
        } else {
            false
        }
    }

    /// The active LOD factor (`lodOptions_[activeOption]`).
    pub fn lod_factor(&self) -> f32 {
        self.options[self.active].value
    }

    /// `applyLodBias(distance)` (`lod_settings.cpp:96-103`).
    pub fn apply_lod_bias(&self, distance: f32) -> f32 {
        distance / self.lod_factor()
    }

    /// `calculateLod` (`lod_settings.cpp:140-165`): the deterministic LOD
    /// value for a view-space depth and object y-scale.
    pub fn calculate_lod(&self, distance: f32, y_scale: f32, zoom: f32) -> f32 {
        let distance = self.apply_lod_bias(distance.max(0.0));
        (distance / y_scale.max(1.0)) * zoom
    }
}

/// A model's fixed LOD distance thresholds. The caller switches LOD level `k`
/// → `k+1` when the calculated LOD value crosses `thresholds[k]`.
#[derive(Debug, Clone, PartialEq)]
pub struct LodLevels {
    /// Ascending thresholds between consecutive LOD levels.
    thresholds: Vec<f32>,
}

impl LodLevels {
    /// LOD levels separated by the given ascending thresholds.
    pub fn new(thresholds: Vec<f32>) -> Self {
        assert!(
            thresholds.windows(2).all(|w| w[0] < w[1]),
            "LOD thresholds must be strictly ascending"
        );
        Self { thresholds }
    }

    /// The LOD index for a calculated value against the fixed thresholds: the
    /// first level whose threshold is above the value, else the last level.
    pub fn lod_for(&self, value: f32) -> usize {
        self.thresholds
            .iter()
            .position(|t| *t > value)
            .unwrap_or(self.thresholds.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference's typical three options: HIGH 1.0, MEDIUM 0.5, LOW 0.25.
    fn typical_options() -> Vec<LodOption> {
        vec![
            LodOption {
                label: "HIGH",
                value: 1.0,
            },
            LodOption {
                label: "MEDIUM",
                value: 0.5,
            },
            LodOption {
                label: "LOW",
                value: 0.25,
            },
        ]
    }

    #[test]
    fn fixed_thresholds_switch_the_same_way_every_time() {
        // The acceptance: the same distance always selects the same LOD level.
        let lod = LodSettings::new(typical_options());
        let levels = LodLevels::new(vec![10.0, 20.0]);
        // For HIGH (factor 1.0) at zoom 1, yScale 1: value == distance.
        for _ in 0..10 {
            assert_eq!(levels.lod_for(lod.calculate_lod(5.0, 1.0, 1.0)), 0);
            assert_eq!(levels.lod_for(lod.calculate_lod(15.0, 1.0, 1.0)), 1);
            assert_eq!(levels.lod_for(lod.calculate_lod(25.0, 1.0, 1.0)), 2);
        }
        // The thresholds are fixed: crossing exactly at the threshold value
        // selects the next level (strictly greater).
        assert_eq!(levels.lod_for(10.0), 1);
        assert_eq!(levels.lod_for(9.999), 0);
        assert_eq!(levels.lod_for(20.0), 2);
    }

    #[test]
    fn switching_the_lod_option_shifts_the_thresholds_predictably() {
        let mut lod = LodSettings::new(typical_options());
        let levels = LodLevels::new(vec![10.0, 20.0]);
        // HIGH (factor 1.0): nominal thresholds.
        assert_eq!(levels.lod_for(lod.calculate_lod(8.0, 1.0, 1.0)), 0);
        // MEDIUM (factor 0.5) halves the effective distance: the same nominal
        // threshold is crossed at half the distance (8.0 → biased to 16.0).
        assert!(lod.set_active(1));
        assert_eq!(lod.lod_factor(), 0.5);
        assert_eq!(lod.calculate_lod(8.0, 1.0, 1.0), 16.0);
        assert_eq!(levels.lod_for(lod.calculate_lod(8.0, 1.0, 1.0)), 1);
        // LOW (factor 0.25) quarters it: 8.0 → 32.0 → last level.
        assert!(lod.set_active(2));
        assert_eq!(levels.lod_for(lod.calculate_lod(8.0, 1.0, 1.0)), 2);
        // Back to HIGH restores the nominal switch.
        assert!(lod.set_active(0));
        assert_eq!(levels.lod_for(lod.calculate_lod(8.0, 1.0, 1.0)), 0);
    }

    #[test]
    fn out_of_range_option_is_ignored() {
        let mut lod = LodSettings::new(typical_options());
        assert!(!lod.set_active(3), "out-of-range change is rejected");
        assert_eq!(lod.active(), 0, "active option unchanged");
    }

    #[test]
    fn lod_bias_scales_the_culling_bounds() {
        let mut lod = LodSettings::new(typical_options());
        // applyLodBias(distance) divides by the factor (lod_settings.cpp:96).
        assert_eq!(lod.apply_lod_bias(10.0), 10.0);
        lod.set_active(1);
        assert_eq!(lod.apply_lod_bias(10.0), 20.0);
    }

    #[test]
    fn display_settings_map_to_the_device() {
        // The 画面设置 applied to the device: windowed vs fullscreen and the
        // back-buffer size, with the refresh only for fullscreen.
        let win = DisplaySettings::windowed(800, 600);
        assert!(win.windowed);
        assert_eq!(win.width, 800);
        assert_eq!(win.height, 600);
        assert_eq!(win.refresh_hz, 0, "windowed uses the desktop default");
        let fs = DisplaySettings::fullscreen(1280, 720, 60);
        assert!(!fs.windowed);
        assert_eq!(fs.refresh_hz, 60);
        assert_eq!(fs.present_interval, PresentInterval::One);
    }

    #[test]
    fn default_lod_is_high() {
        let lod = LodSettings::high();
        assert_eq!(lod.options().len(), 1);
        assert_eq!(lod.options()[0].label, "HIGH");
        assert_eq!(lod.lod_factor(), 1.0);
        assert_eq!(lod.active(), 0);
    }
}
