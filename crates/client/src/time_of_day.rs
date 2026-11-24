//! BW-11-04-04 environment: the day/night cycle, a port of
//! `lib/romp/time_of_day.{hpp,cpp,ipp}` — the half of the
//! terrain-and-environment item that drives a fixed camera's outside
//! lighting from the game clock.
//!
//! Model notes:
//! - The three colour tracks are `LinearAnimation<Vector3>` instances
//!   looping over [`HOURS_IN_DAY`]; `tick` only refreshes the colours when
//!   the *sun* track has keys, divides by 255 and pins alpha to 1. An empty
//!   track therefore leaves its `OutsideLighting` colour untouched rather
//!   than fading to black.
//! - `gameTime(float)` is not an assignment: it wraps through
//!   `fmodf(t + 2400, 24)` (the +2400 lifts negative inputs into range) and
//!   fires the update notifiers, exactly like `secondsPerGameHour(float)`.
//!   `sunColor`/`moonColor` setters instead raise the sun/moon update flag.
//! - A `lightkey` section with no `time` child defaults to
//!   `HOURS_IN_DAY + 1`, which a looping track of `maxTime == 24` silently
//!   rejects — the key is dropped, not clamped.
//! - `load` only re-reads `starttime`/`hourlength` while
//!   `!timeConfigChanged_ || ignoreTimeChanged`, and the *first* successful
//!   `day_night_cycle` load pins the config-changed latch to true; later
//!   loads can still re-read the angle and colour fields.
//! - The `gameTimeToString` buffer field named `secondsTime` actually holds
//!   minutes — the reference comment trap is kept only in the reference;
//!   here the format is `{:02}:{:02}` hours:minutes.

use std::rc::Rc;

use foundation::math::{Matrix, Vector3, Vector4};
use resources::datasection::DataSection;

use crate::lighting::LinearAnimation;

/// `HOURS_IN_DAY` (`time_of_day.cpp:31`).
pub const HOURS_IN_DAY: f32 = 24.0;

/// `DEG_TO_RAD` (`lib/math/mathdef.hpp:37`): `angle * MATH_PI / 180`.
fn deg_to_rad(angle: f32) -> f32 {
    angle * std::f32::consts::PI / 180.0
}

/// An object notified when the time-of-day configuration changes outside of
/// `tick` (`TimeOfDay::UpdateNotifier`).
pub trait UpdateNotifier {
    fn updated(&self, tod: &TimeOfDay);
}

/// The outside lighting state `tick` writes and the renderer reads
/// (`lib/romp/time_of_day.hpp:23-57`): zero colours and identity transforms
/// until the first `tick` with key frames.
#[derive(Debug, Clone, Copy)]
pub struct OutsideLighting {
    pub sun_transform: Matrix,
    pub moon_transform: Matrix,
    pub sun_colour: Vector4,
    pub ambient_colour: Vector4,
    pub fog_colour: Vector4,
    use_moon_colour: bool,
}

impl Default for OutsideLighting {
    fn default() -> Self {
        OutsideLighting::new()
    }
}

impl OutsideLighting {
    /// The constructor (`time_of_day.hpp:28-39`): zeroed colours, identity
    /// transforms, sun selected.
    pub fn new() -> OutsideLighting {
        OutsideLighting {
            sun_transform: Matrix::identity(),
            moon_transform: Matrix::identity(),
            sun_colour: Vector4::ZERO,
            ambient_colour: Vector4::ZERO,
            fog_colour: Vector4::ZERO,
            use_moon_colour: false,
        }
    }

    /// `mainLightTransform` (`time_of_day.hpp:43-46`).
    pub fn main_light_transform(&self) -> &Matrix {
        if self.use_moon_colour {
            &self.moon_transform
        } else {
            &self.sun_transform
        }
    }

    /// `mainLightDir` (`time_of_day.hpp:48-51`): the transform's z axis.
    pub fn main_light_dir(&self) -> Vector3 {
        self.main_light_transform().apply_to_unit_axis_vector(2)
    }

    /// `useMoonColour(bool)` (`time_of_day.hpp:53-55`).
    pub fn set_use_moon_colour(&mut self, value: bool) {
        self.use_moon_colour = value;
    }

    /// The `useMoonColour_` flag behind `mainLightTransform`'s choice.
    pub fn moon_colour_in_use(&self) -> bool {
        self.use_moon_colour
    }
}

/// `TimeOfDay` (`lib/romp/time_of_day.hpp:63`): the game clock and the
/// outside lighting derived from it.
pub struct TimeOfDay {
    time: f32,
    start_time: f32,
    seconds_per_game_hour: f32,
    sun_angle: f32,
    sun_colour: Vector3,
    moon_angle: f32,
    moon_colour: Vector3,
    sun_animation: LinearAnimation<Vector3>,
    ambient_animation: LinearAnimation<Vector3>,
    fog_animation: LinearAnimation<Vector3>,
    lighting: OutsideLighting,
    update_notifiers: Vec<Rc<dyn UpdateNotifier>>,
    update_notifiers_on: bool,
    time_config_changed: bool,
    sun_and_moon_need_update: bool,
}

/// `readFloat(tag, default)` for a child element.
fn read_float(section: &DataSection, tag: &str, default: f32) -> f32 {
    match section.open_section(tag) {
        Some(child) => child.as_float(default),
        None => default,
    }
}

/// `openSections(tag, vector)`: the element children named `tag`, in
/// document order. Attributes are children too in the DataSection model, so
/// they are excluded here the way the reference's separate attribute store
/// keeps them out of `pChild_`.
fn child_sections<'a>(section: &'a DataSection, tag: &str) -> Vec<&'a DataSection> {
    (0..section.count_children())
        .filter_map(|i| section.open_child(i))
        .filter(|child| !child.is_attribute() && child.section_name() == tag)
        .collect()
}

impl Default for TimeOfDay {
    fn default() -> Self {
        TimeOfDay::new(12.0)
    }
}

impl TimeOfDay {
    /// The constructor (`time_of_day.cpp:36-53`): noon, 160 real seconds per
    /// game hour, 80-degree bodies over white default colours, three looping
    /// 24-hour colour tracks.
    pub fn new(current_time: f32) -> TimeOfDay {
        TimeOfDay {
            time: current_time,
            start_time: 0.0,
            seconds_per_game_hour: 160.0,
            sun_angle: 80.0,
            sun_colour: Vector3::new(255.0, 255.0, 255.0),
            moon_angle: 80.0,
            moon_colour: Vector3::new(255.0, 255.0, 255.0),
            sun_animation: LinearAnimation::new(true, HOURS_IN_DAY),
            ambient_animation: LinearAnimation::new(true, HOURS_IN_DAY),
            fog_animation: LinearAnimation::new(true, HOURS_IN_DAY),
            lighting: OutsideLighting::new(),
            update_notifiers: Vec::new(),
            update_notifiers_on: true,
            time_config_changed: false,
            sun_and_moon_need_update: false,
        }
    }

    /// `tick` (`time_of_day.cpp:64-92`): advance the clock, refresh the
    /// colour tracks when the sun track has keys, and re-derive the sun/moon
    /// transforms and which body owns the light.
    pub fn tick(&mut self, d_time: f32) {
        if self.seconds_per_game_hour > 0.0 {
            self.time = (self.time + d_time / self.seconds_per_game_hour) % HOURS_IN_DAY;
        }

        if !self.sun_animation.empty() {
            let sun = self.sun_animation.animate(self.time) / 255.0;
            let ambient = self.ambient_animation.animate(self.time) / 255.0;
            let fog = self.fog_animation.animate(self.time) / 255.0;
            self.lighting.sun_colour = Vector4::new(sun.x, sun.y, sun.z, 1.0);
            self.lighting.ambient_colour = Vector4::new(ambient.x, ambient.y, ambient.z, 1.0);
            self.lighting.fog_colour = Vector4::new(fog.x, fog.y, fog.z, 1.0);
        }

        let day_fraction = deg_to_rad((self.time / HOURS_IN_DAY) * 360.0);

        self.lighting
            .sun_transform
            .set_rotate_x(deg_to_rad(-self.sun_angle));
        self.lighting
            .sun_transform
            .pre_rotate_y(2.0 * std::f32::consts::PI - day_fraction);

        self.lighting
            .moon_transform
            .set_rotate_x(deg_to_rad(-self.moon_angle));
        self.lighting
            .moon_transform
            .pre_rotate_y(std::f32::consts::PI + day_fraction);

        let local_dir = Vector3::new(0.0, 0.0, -1.0);
        let sun_pos = self.lighting.sun_transform.apply_point(&local_dir);
        let moon_pos = self.lighting.moon_transform.apply_point(&local_dir);

        self.lighting.set_use_moon_colour(moon_pos.y > sun_pos.y);
    }

    /// `load` (`time_of_day.cpp:100-204`): clear the tracks and re-read the
    /// day/night settings from a `day_night_cycle` section. Nothing is read
    /// at all when the section is absent.
    pub fn load(&mut self, root: &DataSection, ignore_time_changed: bool) {
        self.sun_animation.clear();
        self.fog_animation.clear();
        self.ambient_animation.clear();

        let day_night = match root.open_section("day_night_cycle") {
            Some(section) => section,
            None => return,
        };

        if !self.time_config_changed || ignore_time_changed {
            self.start_time = read_float(day_night, "starttime", 12.0);
            self.seconds_per_game_hour = read_float(day_night, "hourlength", 160.0);
        }
        self.sun_angle = read_float(day_night, "angle", 80.0);
        self.sun_colour = day_night.read_vector3("sunColor", Vector3::new(255.0, 255.0, 255.0));
        self.moon_angle = read_float(day_night, "moonAngle", 80.0);
        self.moon_colour = day_night.read_vector3("moonColor", Vector3::new(255.0, 255.0, 255.0));

        self.sun_and_moon_need_update = true;

        if !self.time_config_changed {
            self.time = self.start_time;
            self.time_config_changed = true;
        }

        for key in child_sections(day_night, "lightkey") {
            let time = read_float(key, "time", HOURS_IN_DAY + 1.0);
            let colour = key.read_vector3("colour", Vector3::ZERO);
            self.sun_animation.add_key(time, colour);
        }

        for key in child_sections(day_night, "fogkey") {
            let time = read_float(key, "time", HOURS_IN_DAY + 1.0);
            let colour = key.read_vector3("colour", Vector3::ZERO);
            self.fog_animation.add_key(time, colour);
        }

        for key in child_sections(day_night, "ambientkey") {
            let time = read_float(key, "time", HOURS_IN_DAY + 1.0);
            let colour = key.read_vector3("colour", Vector3::ZERO);
            self.ambient_animation.add_key(time, colour);
        }
    }

    /// `gameTime()` (`time_of_day.ipp`).
    pub fn game_time(&self) -> f32 {
        self.time
    }

    /// `gameTime(float)` (`time_of_day.ipp`): wraps through
    /// `fmodf(t + 2400, 24)` and fires the notifiers.
    pub fn set_game_time(&mut self, t: f32) {
        self.time = (t + 2400.0) % HOURS_IN_DAY;
        self.time_config_changed();
    }

    /// `startTime()` (`time_of_day.ipp`).
    pub fn start_time(&self) -> f32 {
        self.start_time
    }

    /// `startTime(float)` (`time_of_day.ipp`): a plain store, no notifier.
    pub fn set_start_time(&mut self, time: f32) {
        self.start_time = time;
    }

    /// `secondsPerGameHour()` (`time_of_day.ipp`).
    pub fn seconds_per_game_hour(&self) -> f32 {
        self.seconds_per_game_hour
    }

    /// `secondsPerGameHour(float)` (`time_of_day.ipp`): a config change, so
    /// the notifiers fire.
    pub fn set_seconds_per_game_hour(&mut self, t: f32) {
        self.seconds_per_game_hour = t;
        self.time_config_changed();
    }

    /// `sunAngle()` (`time_of_day.ipp`).
    pub fn sun_angle(&self) -> f32 {
        self.sun_angle
    }

    /// `sunAngle(float)` (`time_of_day.ipp`).
    pub fn set_sun_angle(&mut self, angle: f32) {
        self.sun_angle = angle;
    }

    /// `sunColor()` (`time_of_day.ipp`).
    pub fn sun_colour(&self) -> Vector3 {
        self.sun_colour
    }

    /// `sunColor(Vector3)` (`time_of_day.ipp`): raises the update flag.
    pub fn set_sun_colour(&mut self, colour: Vector3) {
        self.sun_colour = colour;
        self.sun_and_moon_need_update = true;
    }

    /// `moonAngle()` (`time_of_day.ipp`).
    pub fn moon_angle(&self) -> f32 {
        self.moon_angle
    }

    /// `moonAngle(float)` (`time_of_day.ipp`).
    pub fn set_moon_angle(&mut self, angle: f32) {
        self.moon_angle = angle;
    }

    /// `moonColor()` (`time_of_day.ipp`).
    pub fn moon_colour(&self) -> Vector3 {
        self.moon_colour
    }

    /// `moonColor(Vector3)` (`time_of_day.ipp`): raises the update flag.
    pub fn set_moon_colour(&mut self, colour: Vector3) {
        self.moon_colour = colour;
        self.sun_and_moon_need_update = true;
    }

    /// `sunAndMoonNeedUpdate()` (`time_of_day.hpp:107`).
    pub fn sun_and_moon_need_update(&self) -> bool {
        self.sun_and_moon_need_update
    }

    /// `resetSunMoonUpdate()` (`time_of_day.hpp:108`).
    pub fn reset_sun_moon_update(&mut self) {
        self.sun_and_moon_need_update = false;
    }

    /// `lighting()` (`time_of_day.ipp`).
    pub fn lighting(&self) -> &OutsideLighting {
        &self.lighting
    }

    /// The mutable `lighting()` overload (`time_of_day.ipp`).
    pub fn lighting_mut(&mut self) -> &mut OutsideLighting {
        &mut self.lighting
    }

    /// `getTimeOfDayAsString` (`time_of_day.cpp:222-228`).
    pub fn get_time_of_day_as_string(&self) -> String {
        game_time_to_string(self.time)
    }

    /// `setTimeOfDayAsString` (`time_of_day.cpp:233-239`): parses through
    /// `gameTimeFromString` (falling back to the current time) and routes
    /// through the wrapping `gameTime` accessor.
    pub fn set_time_of_day_as_string(&mut self, new_time: &str) {
        let gt = game_time_from_string(new_time, self.time);
        self.set_game_time(gt);
    }

    /// `addUpdateNotifier` (`time_of_day.cpp:489-494`).
    pub fn add_update_notifier(&mut self, notifier: Rc<dyn UpdateNotifier>) {
        self.update_notifiers.push(notifier);
    }

    /// `delUpdateNotifier` (`time_of_day.cpp:498-505`): removes the first
    /// pointer-equal entry; unknown handles are ignored.
    pub fn del_update_notifier(&mut self, notifier: &Rc<dyn UpdateNotifier>) -> bool {
        match self
            .update_notifiers
            .iter()
            .position(|other| Rc::ptr_eq(other, notifier))
        {
            Some(index) => {
                self.update_notifiers.remove(index);
                true
            }
            None => false,
        }
    }

    /// `updateNotifiersOn(bool)` (`time_of_day.ipp`).
    pub fn update_notifiers_on(&mut self, on: bool) {
        self.update_notifiers_on = on;
    }

    /// `timeConfigChanged` (`time_of_day.cpp:511-521`): latch the flag and
    /// fan out to the notifiers while they are switched on.
    pub fn time_config_changed(&mut self) {
        self.time_config_changed = true;
        if !self.update_notifiers_on {
            return;
        }

        for notifier in &self.update_notifiers {
            notifier.updated(self);
        }
    }

    /// The `timeConfigChanged_` latch (read-only in the reference).
    pub fn is_time_config_changed(&self) -> bool {
        self.time_config_changed
    }
}

/// `TimeOfDay::gameTimeToString` (`time_of_day.cpp:244-259`): `HH:MM` with
/// the minutes derived from the fractional hour.
pub fn game_time_to_string(gt: f32) -> String {
    let hour_time = gt as u32;
    let seconds_time = ((gt - hour_time as f32) * 60.0) as u32;
    format!("{:02}:{:02}", hour_time, seconds_time)
}

/// `sscanf("%d:%d")`: whitespace-skipping signed decimal, a literal colon,
/// another signed decimal. Both conversions must succeed for the two-value
/// path to win.
fn sscanf_hm(new_time: &str) -> Option<(i32, i32)> {
    let bytes = new_time.as_bytes();
    let mut i = 0;

    let next_int = |i: &mut usize| -> Option<i32> {
        while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
            *i += 1;
        }
        let start = *i;
        if *i < bytes.len() && (bytes[*i] == b'+' || bytes[*i] == b'-') {
            *i += 1;
        }
        let digits_start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == digits_start {
            return None;
        }
        new_time[start..*i].parse::<i32>().ok()
    };

    let hours = next_int(&mut i)?;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    i += 1;
    let minutes = next_int(&mut i)?;
    Some((hours, minutes))
}

/// `istringstream >> float` (the C locale): the longest strtof-style prefix
/// — optional sign, digits with an optional fraction, optional exponent.
/// Fails when no digits were consumed, like the stream's fail bit.
fn istr_float(new_time: &str) -> Option<f32> {
    let text = new_time.trim_start();
    let bytes = text.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if digits == 0 {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        let mut exponent_digits = 0;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
            exponent_digits += 1;
        }
        if exponent_digits > 0 {
            i = j;
        }
    }
    text[..i].parse::<f32>().ok()
}

/// `TimeOfDay::gameTimeFromString` (`time_of_day.cpp:266-299`): try
/// `H:MM`, then a bare float; garbage keeps `default_time` (the reference
/// prints a warning). The result is `fmodf(fabsf(t), 24)` either way.
pub fn game_time_from_string(new_time: &str, default_time: f32) -> f32 {
    let time_as_float = match sscanf_hm(new_time) {
        Some((hours, minutes)) => (hours % 24) as f32 + (minutes % 60) as f32 / 60.0,
        None => match istr_float(new_time) {
            Some(value) => value,
            None => default_time,
        },
    };
    (time_as_float.abs()) % 24.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 0.005;

    fn almost(value: f32, expected: f32) {
        assert!((value - expected).abs() < EPSILON, "{value} !~ {expected}");
    }

    fn almost4(value: Vector4, expected: (f32, f32, f32, f32)) {
        almost(value.x, expected.0);
        almost(value.y, expected.1);
        almost(value.z, expected.2);
        almost(value.w, expected.3);
    }

    /// A notifier that counts `updated` calls.
    struct CountingNotifier {
        count: std::cell::Cell<usize>,
    }

    impl CountingNotifier {
        fn new() -> Rc<Self> {
            Rc::new(CountingNotifier {
                count: std::cell::Cell::new(0),
            })
        }
    }

    impl UpdateNotifier for CountingNotifier {
        fn updated(&self, _tod: &TimeOfDay) {
            self.count.set(self.count.get() + 1);
        }
    }

    const DAY_NIGHT_XML: &str = "<root>\
         <day_night_cycle>\
           <starttime>8</starttime>\
           <hourlength>60</hourlength>\
           <angle>45</angle>\
           <sunColor>10 20 30</sunColor>\
           <moonAngle>50</moonAngle>\
           <moonColor>1 2 3</moonColor>\
           <lightkey><time>6</time><colour>0 0 0</colour></lightkey>\
           <lightkey><time>18</time><colour>255 255 255</colour></lightkey>\
           <fogkey><time>12</time><colour>10 10 10</colour></fogkey>\
           <ambientkey><time>12</time><colour>20 20 20</colour></ambientkey>\
         </day_night_cycle>\
       </root>";

    fn day_night_root() -> DataSection {
        resources::datasection::parse("root", DAY_NIGHT_XML.as_bytes()).expect("xml")
    }

    #[test]
    fn constructor_matches_the_reference_defaults() {
        let tod = TimeOfDay::default();
        almost(tod.game_time(), 12.0);
        almost(tod.start_time(), 0.0);
        almost(tod.seconds_per_game_hour(), 160.0);
        almost(tod.sun_angle(), 80.0);
        almost(tod.moon_angle(), 80.0);
        assert_eq!(tod.sun_colour(), Vector3::new(255.0, 255.0, 255.0));
        assert_eq!(tod.moon_colour(), Vector3::new(255.0, 255.0, 255.0));
        assert!(!tod.is_time_config_changed());
        assert!(!tod.sun_and_moon_need_update());

        let lighting = tod.lighting();
        assert_eq!(*lighting.main_light_transform(), Matrix::identity());
        assert_eq!(lighting.sun_colour, Vector4::ZERO);
        assert_eq!(lighting.ambient_colour, Vector4::ZERO);
        assert_eq!(lighting.fog_colour, Vector4::ZERO);
        assert!(!lighting.moon_colour_in_use());

        let defaulted = OutsideLighting::default();
        assert_eq!(*defaulted.main_light_transform(), Matrix::identity());
        assert_eq!(defaulted.sun_colour, Vector4::ZERO);
    }

    #[test]
    fn tick_at_noon_puts_the_sun_above_and_uses_its_colour() {
        let mut tod = TimeOfDay::new(12.0);
        tod.tick(0.0);

        // localDir (0,0,-1) through Ry(2pi - 180deg) Rx(-80deg): the sun is
        // at +sin(80deg) on y, the moon at -sin(80deg), so the sun owns the
        // light (strict >).
        let local_dir = Vector3::new(0.0, 0.0, -1.0);
        let sun_pos = tod.lighting().sun_transform.apply_point(&local_dir);
        almost(sun_pos.y, 80f32.to_radians().sin());
        almost(sun_pos.z, 80f32.to_radians().cos());
        assert!(sun_pos.x.abs() < 1e-4);

        let moon_pos = tod.lighting().moon_transform.apply_point(&local_dir);
        almost(moon_pos.y, -80f32.to_radians().sin());
        assert!(!tod.lighting().moon_colour_in_use());

        // mainLightDir is the selected transform's z axis, the reverse of
        // the body's position.
        let dir = tod.lighting().main_light_dir();
        almost(dir.y, -80f32.to_radians().sin());
        almost(dir.z, -80f32.to_radians().cos());
    }

    #[test]
    fn tick_at_dawn_places_the_sun_on_the_plus_x_horizon() {
        let mut tod = TimeOfDay::new(6.0);
        tod.tick(0.0);

        let local_dir = Vector3::new(0.0, 0.0, -1.0);
        let sun_pos = tod.lighting().sun_transform.apply_point(&local_dir);
        almost(sun_pos.x, 1.0);
        almost(sun_pos.y, 0.0);
        almost(sun_pos.z, 0.0);

        // The moon transform equals the sun's at 06:00 (270deg both), and
        // the tie keeps the sun (strict >).
        let moon_pos = tod.lighting().moon_transform.apply_point(&local_dir);
        almost(moon_pos.x, 1.0);
        assert!(!tod.lighting().moon_colour_in_use());
    }

    #[test]
    fn tick_advances_the_clock_by_seconds_per_game_hour() {
        let mut tod = TimeOfDay::new(0.0);
        tod.tick(160.0);
        almost(tod.game_time(), 1.0);
        tod.tick(80.0);
        almost(tod.game_time(), 1.5);
        // The wrap uses fmod, so a day and a half lands mid-afternoon.
        tod.tick(160.0 * 30.0);
        almost(tod.game_time(), 31.5 % 24.0);
    }

    #[test]
    fn zero_seconds_per_game_hour_freezes_the_clock_but_still_ticks_bodies() {
        let mut tod = TimeOfDay::new(3.0);
        tod.set_seconds_per_game_hour(0.0);
        tod.tick(500.0);
        almost(tod.game_time(), 3.0);
        // The transforms were still derived for time 3: m[2][1] of
        // Ry(360deg - 45deg) Rx(-80deg) is sin(80deg) * cos(315deg).
        let day_fraction = deg_to_rad((3.0 / 24.0) * 360.0);
        almost(
            tod.lighting().sun_transform.get_element(2, 1),
            80f32.to_radians().sin() * (2.0 * std::f32::consts::PI - day_fraction).cos(),
        );
    }

    #[test]
    fn empty_tracks_leave_the_colours_untouched() {
        let mut tod = TimeOfDay::new(12.0);
        tod.tick(0.0);
        // The constructor zeroes the colours; an empty sun track never
        // writes them.
        assert_eq!(tod.lighting().sun_colour, Vector4::ZERO);
        assert_eq!(tod.lighting().ambient_colour, Vector4::ZERO);
        assert_eq!(tod.lighting().fog_colour, Vector4::ZERO);
    }

    #[test]
    fn loaded_keys_drive_all_three_colour_tracks() {
        let mut tod = TimeOfDay::new(0.0);
        tod.load(&day_night_root(), false);
        // The load put the clock on the 8 o'clock start; four game hours of
        // 60 real seconds each bring it to noon.
        tod.tick(240.0);

        // time 12 sits halfway between the 6 and 18 keys.
        almost(tod.game_time(), 12.0);
        almost4(tod.lighting().sun_colour, (0.5, 0.5, 0.5, 1.0));
        // fog/ambient have a single key at 12: the flat value whatever the
        // clock reads, alpha pinned to 1.
        almost4(
            tod.lighting().fog_colour,
            (10.0 / 255.0, 10.0 / 255.0, 10.0 / 255.0, 1.0),
        );
        almost4(
            tod.lighting().ambient_colour,
            (20.0 / 255.0, 20.0 / 255.0, 20.0 / 255.0, 1.0),
        );
    }

    #[test]
    fn load_applies_the_settings_and_pins_the_config_latch() {
        let mut tod = TimeOfDay::new(0.0);
        tod.load(&day_night_root(), false);

        almost(tod.start_time(), 8.0);
        almost(tod.seconds_per_game_hour(), 60.0);
        almost(tod.sun_angle(), 45.0);
        almost(tod.moon_angle(), 50.0);
        assert_eq!(tod.sun_colour(), Vector3::new(10.0, 20.0, 30.0));
        assert_eq!(tod.moon_colour(), Vector3::new(1.0, 2.0, 3.0));
        assert!(tod.sun_and_moon_need_update());
        // First load: the start time became the current time and the latch
        // latched.
        almost(tod.game_time(), 8.0);
        assert!(tod.is_time_config_changed());

        tod.reset_sun_moon_update();
        assert!(!tod.sun_and_moon_need_update());
    }

    #[test]
    fn second_load_keeps_the_time_config_unless_told_to_ignore() {
        let second_xml = "<root><day_night_cycle>\
             <starttime>20</starttime><hourlength>30</hourlength>\
             <angle>10</angle>\
           </day_night_cycle></root>";
        let second = resources::datasection::parse("root", second_xml.as_bytes()).expect("xml");

        // The latch is on: starttime/hourlength stay, the angles re-read.
        let mut tod = TimeOfDay::new(0.0);
        tod.load(&day_night_root(), false);
        tod.load(&second, false);
        almost(tod.start_time(), 8.0);
        almost(tod.seconds_per_game_hour(), 60.0);
        almost(tod.sun_angle(), 10.0);

        // ignoreTimeChanged re-reads everything.
        let mut tod = TimeOfDay::new(0.0);
        tod.load(&day_night_root(), false);
        tod.load(&second, true);
        almost(tod.start_time(), 20.0);
        almost(tod.seconds_per_game_hour(), 30.0);
        almost(tod.sun_angle(), 10.0);
        // The latch was already on from the first load, so the time did not
        // jump to the new start.
        assert!(tod.is_time_config_changed());
        almost(tod.game_time(), 8.0);
    }

    #[test]
    fn missing_day_night_section_only_clears_the_tracks() {
        let empty = resources::datasection::parse("root", b"<root/>").expect("xml");
        let mut tod = TimeOfDay::new(5.0);
        tod.load(&day_night_root(), false);
        tod.load(&empty, false);

        // The absent section reads nothing: the settings from the first
        // load survive, only the tracks were emptied.
        assert!(tod.sun_and_moon_need_update());
        almost(tod.game_time(), 8.0);
        assert_eq!(tod.sun_colour(), Vector3::new(10.0, 20.0, 30.0));
    }

    #[test]
    fn keyless_sections_default_to_a_dropped_hour_and_black() {
        let xml = "<root><day_night_cycle>\
             <lightkey><colour>9 9 9</colour></lightkey>\
           </day_night_cycle></root>";
        let root = resources::datasection::parse("root", xml.as_bytes()).expect("xml");

        let mut tod = TimeOfDay::new(0.0);
        tod.load(&root, false);
        // Default key time 25 >= maxTime 24: the looping track rejects it,
        // so the track stays empty and colours are never written.
        tod.tick(0.0);
        assert_eq!(tod.lighting().sun_colour, Vector4::ZERO);
    }

    #[test]
    fn game_time_wraps_through_2400_and_fires_the_notifiers() {
        let notifier = CountingNotifier::new();
        let mut tod = TimeOfDay::new(0.0);
        tod.add_update_notifier(notifier.clone());

        tod.set_game_time(25.0);
        almost(tod.game_time(), 1.0);
        tod.set_game_time(-1.0);
        almost(tod.game_time(), 23.0);
        // fmod(-30 + 2400, 24) = fmod(2370, 24) = 18.
        tod.set_game_time(-30.0);
        almost(tod.game_time(), 18.0);
        assert_eq!(notifier.count.get(), 3);
        assert!(tod.is_time_config_changed());
    }

    #[test]
    fn seconds_per_game_hour_setter_fires_too_but_start_time_does_not() {
        let notifier = CountingNotifier::new();
        let mut tod = TimeOfDay::new(0.0);
        tod.add_update_notifier(notifier.clone());

        tod.set_start_time(7.0);
        assert_eq!(notifier.count.get(), 0);
        tod.set_seconds_per_game_hour(90.0);
        assert_eq!(notifier.count.get(), 1);
    }

    #[test]
    fn colour_setters_raise_the_sun_moon_flag() {
        let mut tod = TimeOfDay::new(0.0);
        assert!(!tod.sun_and_moon_need_update());
        tod.set_sun_colour(Vector3::new(1.0, 2.0, 3.0));
        assert!(tod.sun_and_moon_need_update());
        tod.reset_sun_moon_update();
        tod.set_moon_colour(Vector3::new(4.0, 5.0, 6.0));
        assert!(tod.sun_and_moon_need_update());
        assert_eq!(tod.moon_colour(), Vector3::new(4.0, 5.0, 6.0));
    }

    #[test]
    fn notifiers_can_be_silenced_and_removed() {
        let notifier = CountingNotifier::new();
        let mut tod = TimeOfDay::new(0.0);
        tod.add_update_notifier(notifier.clone());

        tod.update_notifiers_on(false);
        tod.set_game_time(1.0);
        assert_eq!(notifier.count.get(), 0);
        assert!(tod.is_time_config_changed());

        tod.update_notifiers_on(true);
        tod.set_game_time(2.0);
        assert_eq!(notifier.count.get(), 1);

        let handle: Rc<dyn UpdateNotifier> = notifier.clone();
        assert!(tod.del_update_notifier(&handle));
        assert!(!tod.del_update_notifier(&handle));
        tod.set_game_time(3.0);
        assert_eq!(notifier.count.get(), 1);
    }

    #[test]
    fn main_light_follows_the_moon_flag() {
        let mut tod = TimeOfDay::new(0.0);
        // Both transforms are identity before the first tick, so the z axis
        // selection reads straight through either body.
        assert_eq!(*tod.lighting().main_light_transform(), Matrix::identity());
        assert_eq!(tod.lighting().main_light_dir(), Vector3::new(0.0, 0.0, 1.0));
        assert!(!tod.lighting().moon_colour_in_use());

        tod.lighting_mut().set_use_moon_colour(true);
        assert!(tod.lighting().moon_colour_in_use());
        assert_eq!(*tod.lighting().main_light_transform(), Matrix::identity());
        assert_eq!(tod.lighting().main_light_dir(), Vector3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn angle_and_colour_setters_round_trip() {
        let mut tod = TimeOfDay::new(0.0);
        tod.set_sun_angle(30.0);
        almost(tod.sun_angle(), 30.0);
        tod.set_moon_angle(40.0);
        almost(tod.moon_angle(), 40.0);
        assert_eq!(tod.sun_colour(), Vector3::new(255.0, 255.0, 255.0));
    }

    #[test]
    fn time_strings_round_trip_hours_and_minutes() {
        assert_eq!(game_time_to_string(12.0), "12:00");
        assert_eq!(game_time_to_string(12.5), "12:30");
        assert_eq!(game_time_to_string(6.25), "06:15");
        assert_eq!(game_time_to_string(0.0), "00:00");
        assert_eq!(game_time_to_string(23.999), "23:59");

        assert_eq!(TimeOfDay::new(12.5).get_time_of_day_as_string(), "12:30");
    }

    #[test]
    fn time_string_parsing_accepts_clock_and_bare_floats() {
        almost(game_time_from_string("12:30", 0.0), 12.5);
        almost(game_time_from_string("6:15", 0.0), 6.25);
        // sscanf skips whitespace before the number and before the colon.
        almost(game_time_from_string("  12 : 30", 0.0), 12.5);
        // Mods fold out-of-range inputs: 25%24 = 1, 90%60 = 30.
        almost(game_time_from_string("25:90", 0.0), 1.5);
        // Bare floats parse too, then fmod(abs, 24).
        almost(game_time_from_string("13.5", 0.0), 13.5);
        almost(game_time_from_string("30", 0.0), 6.0);
        almost(game_time_from_string("-1:30", 0.0), 0.5);
        almost(game_time_from_string("-30.5", 0.0), 6.5);
        // Exponent forms are stream-parseable.
        almost(game_time_from_string("1.5e1", 0.0), 15.0);
        almost(game_time_from_string("1E2", 0.0), 4.0); // 100 % 24
        almost(game_time_from_string("2e+1", 0.0), 20.0);
        // A dangling exponent is not consumed, like the stream's parse.
        almost(game_time_from_string("12e", 0.0), 12.0);
        // Garbage keeps the default.
        almost(game_time_from_string("noon", 9.75), 9.75);
        almost(game_time_from_string("", 4.0), 4.0);
        almost(game_time_from_string(":", 3.0), 3.0);

        // set_time_of_day_as_string routes through the wrapping setter.
        let mut tod = TimeOfDay::new(0.0);
        tod.set_time_of_day_as_string("13:30");
        almost(tod.game_time(), 13.5);
        tod.set_time_of_day_as_string("noon");
        almost(tod.game_time(), 13.5);
    }
}
