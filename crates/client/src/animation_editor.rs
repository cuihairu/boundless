//! BW-13-04-03: the ModelEditor's animation face
//! (`tools/modeleditor_core/Models/mutant_animations.cpp`, with the
//! animation part of the census walk in `mutant.cpp` `reloadAllLists`).
//!
//! The face is pure document work over the model root: the census walk, the
//! create/change/remove lifecycle, the frame-range and frame-rate edits, the
//! channel backup/restore scratch, the per-bone alpha weights and the
//! read-only probe. The pose engine it checks against — the `.animation`
//! binary load, channel binding and world-pose sampling — is the client's
//! [`crate::skeleton`] module (BW-11-04-01), ported along the reference
//! client path; this module feeds it the frame times, and the acceptance
//! test pins bone poses sampled at fixed clock times against the same
//! oracle the skeleton tests use.
//!
//! The Moo seams this face rides on:
//! - `SuperModelAnimation` / `pSource->duration_` become the entry's
//!   `time` / `duration` fields, fed by [`Animation::total_time`] and the
//!   `NodefullModelAnimation` frame mapping (the editor never samples
//!   channels itself);
//! - `Moo::AnimationManager::find` / `SuperModel::getAnimation` become the
//!   caller's `resolve` closure (nodes resource → `.animation` bytes, `None`
//!   when the resource does not load — the reference then leaves the entry
//!   unrecorded); `testReadOnlyAnim` runs before that lookup, exactly as the
//!   reference census orders it;
//! - `GetFileAttributes(FILE_ATTRIBUTE_READONLY)` becomes the caller's
//!   `file_read_only` closure (a real integration maps it to the filesystem);
//! - `recreateFashions` / `actionQueue_` are the render seam: `set_anim`,
//!   `stop_anim` and `play_anim` record state, no render;
//! - `canCaptureModel(false)` in the frame setter is the capture seam;
//! - the reference's `frameRatesN.xml` scratch files become the entry's
//!   in-memory [`FrameRates`].
//!
//! Two faces are dead at this seam and stay for port fidelity: an entry's
//! `nodes` can never be empty (the census drops those sections before they
//! become entries), so `get_moo_anim`'s empty-file guard and the
//! `frame_num`/`num_frames` "entry without a loaded animation" fall-through
//! are unreachable through the census.

use resources::datasection::DataSection;
use std::collections::BTreeMap;

use crate::skeleton::{Animation, Channel, FrameRange};

/// `StringPair (name, model)` — the animation's table key.
pub type AnimKey = (String, String);

/// The `frameRatesN.xml` scratch the reference keeps per animation (`curr` /
/// `init`), replaced here by plain fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameRates {
    pub curr: f32,
    pub init: f32,
}

impl FrameRates {
    /// A fresh scratch carries the section's frame rate twice (the
    /// reference's `frameRatesN.xml` writer).
    fn from_section(section: &DataSection) -> Self {
        let rate = section.read_float("frameRate", 30.0);
        FrameRates {
            curr: rate,
            init: rate,
        }
    }
}

/// `AnimationInfo` — the editor's per-animation state: the section path, the
/// `<nodes>` resource, the loaded Moo animation, the SuperModel clock
/// (`time` / `duration`), the bone weights, the frame-rate scratch, the
/// channel scratch and the read-only flag.
#[derive(Debug, Clone)]
pub struct AnimationEntry {
    /// The `<animation>` child's index inside the model root.
    pub data: Vec<usize>,
    /// The `<nodes>` resource, the key the channel scratch matches on.
    pub nodes_resource: String,
    /// The loaded animation — always `Some` for census-recorded entries.
    pub animation: Option<Animation>,
    /// `SuperModelAnimation::time` — `-1.0` (the reference's sentinel) until
    /// the frame setter moves the clock.
    pub time: f32,
    /// `pSource->duration_` (the `NodefullModelAnimation` frame mapping).
    pub duration: f32,
    /// The channel scratch: `None` means "cleared" (`channels = NULL` —
    /// recalculated on reload).
    pub channels: Option<Vec<Channel>>,
    /// `<alpha>` bone weights, unsanitised.
    pub bone_weights: BTreeMap<String, f32>,
    /// The frame-rate scratch.
    pub frame_rates: FrameRates,
    /// `testReadOnlyAnim`.
    pub is_read_only: bool,
}

/// `TreeBranch((modelName, modelPath), animNames)` — the per-model branch of
/// the animation list the reference rebuilds every census.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeBranch {
    pub model_name: String,
    pub model_path: String,
    pub anim_names: Vec<String>,
}

/// The `AnimationManager::find` seam: an animation resource name in, its
/// bytes out (`None` when the resource does not load).
type Resolve = Box<dyn Fn(&str) -> Option<Vec<u8>>>;

/// The `GetFileAttributes` seam: an animation resource name in, whether the
/// platform marks it read only.
type FileReadOnly = Box<dyn Fn(&str) -> bool>;

/// `Mutant::reloadAllLists`' animation walk over one model document.
pub struct AnimationEditor {
    pub model: DataSection,
    pub model_path: String,
    resolve: Resolve,
    file_read_only: FileReadOnly,
    /// `animations_`.
    pub animations: BTreeMap<AnimKey, AnimationEntry>,
    /// `animList_` — one branch per census.
    pub anim_list: Vec<TreeBranch>,
    /// The model name derived from the path.
    pub model_name: String,
    /// `lastFrameRate_` — the frame-rate slider's last seen rate.
    pub last_frame_rate: f32,
    /// `animMode_` / `clearAnim_` / `playing_` — the render seam.
    pub anim_mode: bool,
    pub clear_anim: bool,
    pub playing: bool,
    /// `currAnims_` — the current animation per page.
    pub curr_anims: BTreeMap<usize, AnimKey>,
}

/// `SanitiseHelper`: the section-name tokens (`".."` for `" "`).
const SANITISING_TOKEN: &str = "..";
const SPACE_TOKEN: &str = " ";

/// `almostEqual` (`stdmf.hpp:606`): epsilon 0.0004.
const ALMOST_EPSILON: f32 = 0.0004;

fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < ALMOST_EPSILON
}

/// `substringReplace` sanitising (space → token).
fn sanitise(name: &str) -> String {
    name.replace(SPACE_TOKEN, SANITISING_TOKEN)
}

/// `substringReplace` unsanitising (token → space) — the census read face.
fn unsanitise(name: &str) -> String {
    name.replace(SANITISING_TOKEN, SPACE_TOKEN)
}

impl AnimationEditor {
    /// Load the editor over the model root. `resolve` maps a `.animation`
    /// resource name to its bytes (the `AnimationManager::find` seam); a
    /// `None` result leaves the entry unrecorded. `file_read_only` is the
    /// `GetFileAttributes` seam. The census runs once at construction.
    pub fn new<F, G>(model: DataSection, model_path: &str, resolve: F, file_read_only: G) -> Self
    where
        F: Fn(&str) -> Option<Vec<u8>> + 'static,
        G: Fn(&str) -> bool + 'static,
    {
        let model_name = Self::model_name_for(model_path);
        let mut editor = AnimationEditor {
            model,
            model_path: model_path.to_string(),
            resolve: Box::new(resolve),
            file_read_only: Box::new(file_read_only),
            animations: BTreeMap::new(),
            anim_list: Vec::new(),
            model_name,
            last_frame_rate: 30.0,
            anim_mode: false,
            clear_anim: false,
            playing: false,
            curr_anims: BTreeMap::new(),
        };
        editor.census();
        editor
    }

    /// The reference's model-name derivation (`mutant.cpp:844-850`): after
    /// the last `/` and before the last `.`; a dot before the slash
    /// collapses to the empty name.
    fn model_name_for(model_path: &str) -> String {
        let first = model_path.rfind('/').map_or(0, |p| p + 1);
        let last = model_path.rfind('.').unwrap_or(0);
        if first > last {
            String::new()
        } else {
            model_path[first..last].to_string()
        }
    }

    /// `Mutant::canAnim`: the model is loaded and carries a nodefull visual.
    pub fn can_anim(&self, model_path: &str) -> bool {
        model_path == self.model_path && self.model.open_section("nodefullVisual").is_some()
    }

    /// `Mutant::hasAnims`: the model carries an animation section.
    pub fn has_anims(&self, model_path: &str) -> bool {
        model_path == self.model_path && self.model.open_section("animation").is_some()
    }

    /// `reloadAllLists`' animation walk (`mutant.cpp:851-940`): the direct
    /// `<animation>` children in order, each with a `<name>`; the old state
    /// (frame-rate scratch, channel scratch, clock) survives by key, and the
    /// channel scratch additionally by `<nodes>` resource across all old
    /// entries; entries whose `.animation` does not load stay unrecorded —
    /// but only after the read-only probe has run, as in the reference.
    fn census(&mut self) {
        let old = std::mem::take(&mut self.animations);
        let mut anim_names: Vec<String> = Vec::new();
        for i in 0..self.model.count_children() {
            let Some(section) = self.model.open_child(i) else {
                continue;
            };
            if section.section_name() != "animation" {
                continue;
            }
            let name = section.read_string("name", "");
            if name.is_empty() {
                continue;
            }
            let nodes_resource = section.read_string("nodes", "");
            let anim_resource = format!("{nodes_resource}.animation");
            let is_read_only = self.test_read_only_anim(section, &anim_resource);
            let Some(bytes) = (self.resolve)(&anim_resource) else {
                continue;
            };
            let Ok(animation) = Animation::from_bytes(&bytes) else {
                continue;
            };
            let total_time = animation.total_time();
            let duration = FrameRange::from_animation_section(section, total_time).duration;

            // Old state: the key itself, then the nodes-resource match.
            let key = (name.clone(), self.model_path.clone());
            let frame_rates;
            let mut channels = None;
            let mut time = -1.0f32;
            if let Some(old_entry) = old.get(&key) {
                frame_rates = old_entry.frame_rates;
                channels = old_entry.channels.clone();
                time = old_entry.time;
            } else {
                frame_rates = FrameRates::from_section(section);
                for old_entry in old.values() {
                    if old_entry.nodes_resource == nodes_resource {
                        channels = old_entry.channels.clone();
                        break;
                    }
                }
            }

            // The alpha weights, unsanitised.
            let mut bone_weights = BTreeMap::new();
            if let Some(alpha) = section.open_section("alpha") {
                for j in 0..alpha.count_children() {
                    let Some(bone) = alpha.open_child(j) else {
                        continue;
                    };
                    bone_weights.insert(unsanitise(bone.section_name()), bone.as_float(0.0));
                }
            }

            // `AnimationInfo`'s constructor: no scratch yet backs the
            // channels up from the loaded animation.
            if channels.is_none() {
                channels = Some(animation.channels().to_vec());
            }

            self.animations.insert(
                key.clone(),
                AnimationEntry {
                    data: vec![i],
                    nodes_resource,
                    animation: Some(animation),
                    time,
                    duration,
                    channels,
                    bone_weights,
                    frame_rates,
                    is_read_only,
                },
            );
            anim_names.push(name);
        }
        self.anim_list = vec![TreeBranch {
            model_name: self.model_name.clone(),
            model_path: self.model_path.clone(),
            anim_names,
        }];
    }

    /// `testReadOnlyAnim` (`mutant_validation.cpp:303`): empty nodes or a
    /// missing/unloadable file mark the animation read only; the DOS
    /// attribute check is the caller's seam.
    fn test_read_only_anim(&self, section: &DataSection, anim_resource: &str) -> bool {
        if section.read_string("nodes", "").is_empty() {
            return true;
        }
        match (self.resolve)(anim_resource) {
            None => true,
            Some(bytes) => match Animation::from_bytes(&bytes) {
                Err(_) => true,
                Ok(_) => (self.file_read_only)(anim_resource),
            },
        }
    }

    /// `Mutant::getMooAnim` (`mutant_animations.cpp:254`): the loaded
    /// animation, or `None` for an unknown id.
    pub fn get_moo_anim(&self, anim_id: &AnimKey) -> Option<&Animation> {
        if !self.animations.contains_key(anim_id) {
            return None;
        }
        if self.anim_file(anim_id).is_empty() {
            return None;
        }
        self.animations.get(anim_id)?.animation.as_ref()
    }

    /// `Mutant::backupChannels` (`mutant_animations.cpp:283`): the channel
    /// scratch refills from the loaded animation's binders.
    pub fn backup_channels(&mut self, anim_id: &AnimKey) -> Option<Vec<Channel>> {
        if !self.animations.contains_key(anim_id) {
            return None;
        }
        let animation = self.animations.get(anim_id)?.animation.clone()?;
        let channels = animation.channels().to_vec();
        self.animations.get_mut(anim_id)?.channels = Some(channels.clone());
        Some(channels)
    }

    /// `Mutant::restoreChannels` (`mutant_animations.cpp:272`): the backed-up
    /// channels go back into the entry's animation.
    pub fn restore_channels(&mut self, anim_id: &AnimKey) -> Option<Vec<Channel>> {
        if !self.animations.contains_key(anim_id) {
            return None;
        }
        let backup = self.animations.get(anim_id)?.channels.clone()?;
        if let Some(animation) = &mut self.animations.get_mut(anim_id)?.animation {
            animation.set_channels(backup.clone());
        }
        Some(backup)
    }

    /// `Mutant::setAnim` (`mutant_animations.cpp:294`): the page's current
    /// animation (`recreateFashions` is the render seam).
    pub fn set_anim(&mut self, page: usize, anim_id: AnimKey) {
        self.anim_mode = true;
        self.curr_anims.insert(page, anim_id);
    }

    /// `Mutant::getAnim` (`mutant_animations.cpp:303`): the page's current
    /// animation, or `None` for the reference's empty dummy StringPair.
    pub fn get_anim(&self, page: usize) -> Option<&AnimKey> {
        self.curr_anims.get(&page)
    }

    /// `Mutant::stopAnim` (`mutant_animations.cpp:320`): erase the page
    /// (`recreateFashions` is the render seam).
    pub fn stop_anim(&mut self, page: usize) {
        self.curr_anims.remove(&page);
    }

    /// `Mutant::createAnim` (`mutant_animations.cpp:170`): a unique name (the
    /// reference retries `"name 2"`, `"name 3"`, ...), the section written
    /// with `frameRate` 30 and the nodes resource, then the census rerun.
    pub fn create_anim(&mut self, anim_id: &AnimKey, anim_path: &str) -> AnimKey {
        let mut new_first = anim_id.0.clone();
        let mut index = 1;
        while self
            .animations
            .contains_key(&(new_first.clone(), self.model_path.clone()))
        {
            index += 1;
            new_first = format!("{} {}", anim_id.0, index);
        }
        let key = (new_first, self.model_path.clone());
        let section = self.model.add_child("animation");
        section.write_string("name", &key.0);
        section.write_float("frameRate", 30.0);
        section.write_string("nodes", anim_path);
        self.census();
        key
    }

    /// The entry's `<animation>` child, mutable — for a face that has
    /// already checked the id (a census-recorded entry's section index is
    /// always valid).
    fn anim_section_mut(&mut self, anim_id: &AnimKey) -> &mut DataSection {
        let index = self
            .animations
            .get(anim_id)
            .expect("checked by the caller's guard")
            .data[0];
        self.model
            .child_mut(index)
            .expect("a recorded entry's section index")
    }

    /// `Mutant::changeAnim` (`mutant_animations.cpp:201`): rewrite nodes,
    /// clear the clock and the channel scratch so they are recalculated on
    /// reload, then the census rerun.
    pub fn change_anim(&mut self, anim_id: &AnimKey, anim_path: &str) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        self.anim_section_mut(anim_id)
            .write_string("nodes", anim_path);
        let entry = self.animations.get_mut(anim_id).expect("checked above");
        entry.time = 0.0;
        entry.channels = None;
        self.census();
    }

    /// `Mutant::removeAnim` (`mutant_animations.cpp:223`): delete the
    /// section, then the census rerun.
    pub fn remove_anim(&mut self, anim_id: &AnimKey) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        let index = self.animations[anim_id].data[0];
        self.model.del_child_at(index);
        self.census();
    }

    /// `Mutant::cleanAnim` (`mutant_animations.cpp:241`): clear the clock and
    /// the channel scratch (no census rerun).
    pub fn clean_anim(&mut self, anim_id: &AnimKey) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        let entry = self.animations.get_mut(anim_id).expect("checked above");
        entry.time = 0.0;
        entry.channels = None;
    }

    /// `Mutant::animName` (write, `mutant_animations.cpp:332`): rename — the
    /// same name exits true, an unknown id or a colliding name false; the
    /// table, the tree list, the section's `name` and the current pages all
    /// follow.
    pub fn anim_name(&mut self, anim_id: &AnimKey, anim_name: &str) -> bool {
        if anim_id.0 == anim_name {
            return true;
        }
        if !self.animations.contains_key(anim_id) {
            return false;
        }
        let new_key = (anim_name.to_string(), anim_id.1.clone());
        if self.animations.contains_key(&new_key) {
            return false;
        }
        let entry = self.animations.remove(anim_id).expect("checked above");
        self.animations.insert(new_key.clone(), entry);
        for branch in &mut self.anim_list {
            if branch.model_path == anim_id.1 {
                for name in &mut branch.anim_names {
                    if name == &anim_id.0 {
                        *name = anim_name.to_string();
                    }
                }
            }
        }
        // The section's `name` follows (`new_key` was just inserted above).
        self.anim_section_mut(&new_key)
            .write_string("name", anim_name);
        for page in self.curr_anims.keys().copied().collect::<Vec<_>>() {
            if self.curr_anims.get(&page) == Some(anim_id) {
                self.set_anim(page, new_key.clone());
            }
        }
        true
    }

    /// `Mutant::animName` (read, `mutant_animations.cpp:388`): the section's
    /// `name`, empty for an unknown id.
    pub fn anim_name_get(&self, anim_id: &AnimKey) -> String {
        match self.animations.get(anim_id) {
            Some(entry) => self
                .model
                .open_child(entry.data[0])
                .map(|s| s.read_string("name", ""))
                .unwrap_or_default(),
            None => String::new(),
        }
    }

    /// `Mutant::firstFrame` (write, `mutant_animations.cpp:399`): the
    /// no-change skip; `-1` deletes the child, anything else writes it.
    pub fn set_first_frame(&mut self, anim_id: &AnimKey, frame: i32) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        if frame == self.first_frame(anim_id) {
            return;
        }
        let section = self.anim_section_mut(anim_id);
        if frame != -1 {
            section.write_int("firstFrame", frame);
        } else {
            section.del_child("firstFrame");
        }
        self.census();
    }

    /// `Mutant::firstFrame` (read, `mutant_animations.cpp:424`): `-1` default.
    pub fn first_frame(&self, anim_id: &AnimKey) -> i32 {
        match self.animations.get(anim_id) {
            Some(entry) => self
                .model
                .open_child(entry.data[0])
                .map(|s| s.read_int("firstFrame", -1))
                .unwrap_or(-1),
            None => -1,
        }
    }

    /// `Mutant::lastFrame` (write, `mutant_animations.cpp:435`).
    pub fn set_last_frame(&mut self, anim_id: &AnimKey, frame: i32) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        if frame == self.last_frame(anim_id) {
            return;
        }
        let section = self.anim_section_mut(anim_id);
        if frame != -1 {
            section.write_int("lastFrame", frame);
        } else {
            section.del_child("lastFrame");
        }
        self.census();
    }

    /// `Mutant::lastFrame` (read, `mutant_animations.cpp:460`): `-1` default.
    pub fn last_frame(&self, anim_id: &AnimKey) -> i32 {
        match self.animations.get(anim_id) {
            Some(entry) => self
                .model
                .open_child(entry.data[0])
                .map(|s| s.read_int("lastFrame", -1))
                .unwrap_or(-1),
            None => -1,
        }
    }

    /// `Mutant::localFrameRate` (write, `mutant_animations.cpp:471`): the
    /// no-change skip compares against the slider's last rate
    /// (`lastFrameRate_`); `final` commits it first, so the undo barrier can
    /// rewind the drag.
    pub fn set_local_frame_rate(&mut self, anim_id: &AnimKey, rate: f32, final_: bool) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        if rate == self.last_frame_rate {
            return;
        }
        if final_ {
            self.animations
                .get_mut(anim_id)
                .expect("checked above")
                .frame_rates
                .curr = self.last_frame_rate;
            self.last_frame_rate = rate;
        }
        self.animations
            .get_mut(anim_id)
            .expect("checked above")
            .frame_rates
            .curr = rate;
    }

    /// `Mutant::localFrameRate` (read, `mutant_animations.cpp:493`): 30
    /// default.
    pub fn local_frame_rate(&self, anim_id: &AnimKey) -> f32 {
        match self.animations.get(anim_id) {
            Some(entry) => entry.frame_rates.curr,
            None => 30.0,
        }
    }

    /// `Mutant::saveFrameRate` (`mutant_animations.cpp:506`): the scratch's
    /// `curr` becomes `init` and the section's `frameRate`, then the census
    /// rerun.
    pub fn save_frame_rate(&mut self, anim_id: &AnimKey) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        let rate = self
            .animations
            .get(anim_id)
            .expect("checked above")
            .frame_rates
            .curr;
        self.animations
            .get_mut(anim_id)
            .expect("checked above")
            .frame_rates
            .init = rate;
        self.anim_section_mut(anim_id)
            .write_float("frameRate", rate);
        self.census();
    }

    /// `Mutant::frameRate` (read, `mutant_animations.cpp:525`): 30 default.
    pub fn frame_rate(&self, anim_id: &AnimKey) -> f32 {
        match self.animations.get(anim_id) {
            Some(entry) => self
                .model
                .open_child(entry.data[0])
                .map(|s| s.read_float("frameRate", 30.0))
                .unwrap_or(30.0),
            None => 30.0,
        }
    }

    /// `Mutant::frameNum` (write, `mutant_animations.cpp:536`): the frame
    /// number becomes the SuperModel clock, clamped to the duration
    /// (`canCaptureModel(false)` is the capture seam).
    pub fn set_frame_num(&mut self, anim_id: &AnimKey, num: i32) {
        if anim_id.0.is_empty() || anim_id.1.is_empty() {
            return;
        }
        let loads = self
            .animations
            .get(anim_id)
            .map(|e| e.animation.is_some())
            .unwrap_or(false);
        if !loads {
            return;
        }
        let rate = self.frame_rate(anim_id);
        let duration = self
            .animations
            .get(anim_id)
            .map(|e| e.duration)
            .unwrap_or(0.0);
        let mut time = num as f32 / rate;
        if time < 0.0 {
            time = 0.0;
        }
        if time > duration {
            time = duration;
        }
        self.animations
            .get_mut(anim_id)
            .expect("checked above")
            .time = time;
    }

    /// `Mutant::frameNum` (read, `mutant_animations.cpp:559`): the frame the
    /// clock maps to, snapping with the reference's `almostEqual` rule (an
    /// exact value stays integral, anything else rounds up).
    pub fn frame_num(&self, anim_id: &AnimKey) -> i32 {
        if anim_id.0.is_empty() || anim_id.1.is_empty() {
            return 0;
        }
        let Some(entry) = self.animations.get(anim_id) else {
            return 0;
        };
        if entry.animation.is_none() {
            return 0;
        }
        let framef = self.frame_rate(anim_id) * entry.time;
        let framei = framef as i32;
        if almost_equal(framef, framei as f32) {
            framei
        } else {
            framef.ceil() as i32
        }
    }

    /// `Mutant::numFrames` (`mutant_animations.cpp:580`): 100 default.
    pub fn num_frames(&self, anim_id: &AnimKey) -> i32 {
        if anim_id.0.is_empty() || anim_id.1.is_empty() {
            return 100;
        }
        let Some(entry) = self.animations.get(anim_id) else {
            return 100;
        };
        if entry.animation.is_some() {
            ((self.frame_rate(anim_id) * entry.duration).ceil() as i32) + 1
        } else {
            100
        }
    }

    /// `Mutant::animFile` (write, `mutant_animations.cpp:596`): rewrite the
    /// nodes resource only (the reference does not clear the clock here),
    /// then the census rerun.
    pub fn set_anim_file(&mut self, anim_id: &AnimKey, anim_file: &str) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        self.anim_section_mut(anim_id)
            .write_string("nodes", anim_file);
        self.census();
    }

    /// `Mutant::animFile` (read, `mutant_animations.cpp:614`): the resource
    /// with its `.animation` suffix.
    pub fn anim_file(&self, anim_id: &AnimKey) -> String {
        let Some(entry) = self.animations.get(anim_id) else {
            return String::new();
        };
        let nodes = self
            .model
            .open_child(entry.data[0])
            .map(|s| s.read_string("nodes", ""))
            .unwrap_or_default();
        if nodes.is_empty() {
            return String::new();
        }
        format!("{nodes}.animation")
    }

    /// `Mutant::animBoneWeight` (write, `mutant_animations.cpp:636`): the
    /// alpha section's sanitised child; the no-change and the default-one
    /// skips; the map follows unsanitised, then the census rerun.
    pub fn set_anim_bone_weight(&mut self, anim_id: &AnimKey, bone_name: &str, val: f32) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        let sanitised = sanitise(bone_name);
        let index = self.animations[anim_id].data[0];
        let mut has_alpha = false;
        let mut old_val = -1.0f32;
        if let Some(section) = self.model.open_child(index)
            && let Some(alpha) = section.open_section("alpha")
        {
            has_alpha = true;
            old_val = alpha.read_float(&sanitised, -1.0);
        }
        if has_alpha {
            if val == old_val {
                return;
            }
        } else if val == 1.0 {
            return;
        }
        let section = self.anim_section_mut(anim_id);
        let alpha = section
            .open_or_make_section("alpha")
            .expect("the alpha path always resolves");
        alpha.write_float(&sanitised, val);
        self.animations
            .get_mut(anim_id)
            .expect("checked above")
            .bone_weights
            .insert(bone_name.to_string(), val);
        self.census();
    }

    /// `Mutant::animBoneWeight` (read, `mutant_animations.cpp:690`): `-1`
    /// default for an unknown bone.
    pub fn anim_bone_weight(&self, anim_id: &AnimKey, bone_name: &str) -> f32 {
        match self.animations.get(anim_id) {
            Some(entry) => entry.bone_weights.get(bone_name).copied().unwrap_or(-1.0),
            None => -1.0,
        }
    }

    /// `Mutant::removeAnimNode` (`mutant_animations.cpp:717`): the sanitised
    /// bone child goes from the alpha section; the weight leaves the map; the
    /// section dies when its last bone goes; then the census rerun.
    pub fn remove_anim_node(&mut self, anim_id: &AnimKey, bone_name: &str) {
        if !self.animations.contains_key(anim_id) {
            return;
        }
        let sanitised = sanitise(bone_name);
        let section = self.anim_section_mut(anim_id);
        let alpha_index = (0..section.count_children()).find(|i| {
            section
                .open_child(*i)
                .map(|c| c.section_name() == "alpha")
                .unwrap_or(false)
        });
        if let Some(i) = alpha_index
            && let Some(alpha) = section.child_mut(i)
        {
            alpha.del_child(&sanitised);
        }
        self.animations
            .get_mut(anim_id)
            .expect("checked above")
            .bone_weights
            .remove(bone_name);
        if self
            .animations
            .get(anim_id)
            .expect("checked above")
            .bone_weights
            .is_empty()
        {
            self.anim_section_mut(anim_id).del_child("alpha");
        }
        self.census();
    }

    /// `Mutant::isReadOnlyAnim` (`mutant_animations.cpp:752`): false for an
    /// unknown id.
    pub fn is_read_only_anim(&self, anim_id: &AnimKey) -> bool {
        match self.animations.get(anim_id) {
            Some(entry) => entry.is_read_only,
            None => false,
        }
    }

    /// `Mutant::playAnim` (`mutant_animations.cpp:764`): the flags
    /// (`actionQueue_` flush / `recreateFashions` are the render seam).
    pub fn play_anim(&mut self, play: bool) {
        self.anim_mode = true;
        self.clear_anim = false;
        self.playing = play;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::Skeleton;
    use crate::skeleton_oracle_data::{FRAMES, PROBES, WORLD};
    use foundation::math::matrix::Matrix;
    use resources::datasection::parse;

    const CRAB_PATH: &str = "characters/npc/crab/crab.model";
    const WALKFORWARD_RESOURCE: &str = "characters/npc/crab/crab_walkforward";

    fn walkforward_bytes() -> Vec<u8> {
        include_bytes!("../res/crab_walkforward.animation").to_vec()
    }

    fn crab_model() -> DataSection {
        parse("crab.model", include_bytes!("../res/crab.model")).expect("crab.model parses")
    }

    fn crab_key(name: &str) -> AnimKey {
        (name.to_string(), CRAB_PATH.to_string())
    }

    fn crab_editor() -> AnimationEditor {
        AnimationEditor::new(
            crab_model(),
            CRAB_PATH,
            |name| match name {
                "characters/npc/crab/crab_walkforward.animation" => Some(walkforward_bytes()),
                _ => None,
            },
            |_| false,
        )
    }

    /// A crab editor whose super model also carries the same animation
    /// under an alternate resource name.
    fn dual_editor() -> AnimationEditor {
        AnimationEditor::new(
            crab_model(),
            CRAB_PATH,
            |name| match name {
                "characters/npc/crab/crab_walkforward.animation"
                | "characters/npc/crab/crab_alt.animation" => Some(walkforward_bytes()),
                _ => None,
            },
            |_| false,
        )
    }

    /// The census records only animations whose resource loads; unknown ids
    /// read the reference defaults on every face.
    #[test]
    fn census_records_only_resolvable_animations() {
        let editor = crab_editor();
        assert_eq!(editor.animations.len(), 1, "only the walkforward loads");
        assert!(editor.can_anim(CRAB_PATH));
        assert!(editor.has_anims(CRAB_PATH));
        assert!(!editor.can_anim("characters/npc/crab/other.model"));
        assert!(!editor.has_anims("characters/npc/crab/other.model"));
        assert_eq!(editor.model_name, "crab");
        assert_eq!(
            editor.anim_list,
            vec![TreeBranch {
                model_name: "crab".to_string(),
                model_path: CRAB_PATH.to_string(),
                anim_names: vec!["crab_walkforward".to_string()],
            }]
        );
        let key = crab_key("crab_walkforward");
        let entry = &editor.animations[&key];
        assert_eq!(entry.nodes_resource, WALKFORWARD_RESOURCE);
        assert!(entry.animation.is_some());
        assert_eq!(entry.time, -1.0, "the reference's fresh clock sentinel");
        assert!(!entry.is_read_only);
        assert_eq!(
            entry.frame_rates,
            FrameRates {
                curr: 30.0,
                init: 30.0
            }
        );
        assert!(entry.bone_weights.is_empty());
        assert_eq!(entry.channels.as_ref().unwrap().len(), 45);
        assert_eq!(
            editor.anim_file(&key),
            format!("{WALKFORWARD_RESOURCE}.animation")
        );
        assert_eq!(editor.num_frames(&key), 26, "25 raw frames + 1");
        // Unknown ids: the reference defaults on every read face.
        let unknown = crab_key("nope");
        assert_eq!(editor.anim_file(&unknown), "");
        assert_eq!(editor.anim_name_get(&unknown), "");
        assert_eq!(editor.first_frame(&unknown), -1);
        assert_eq!(editor.last_frame(&unknown), -1);
        assert_eq!(editor.local_frame_rate(&unknown), 30.0);
        assert_eq!(editor.frame_rate(&unknown), 30.0);
        assert_eq!(editor.frame_num(&unknown), 0);
        assert_eq!(editor.num_frames(&unknown), 100);
        assert_eq!(editor.anim_bone_weight(&unknown, "bone"), -1.0);
        assert!(!editor.is_read_only_anim(&unknown));
        assert!(editor.get_moo_anim(&unknown).is_none());
    }

    /// `testReadOnlyAnim`: the attribute seam, a missing nodes resource and
    /// a corrupt file; the census drops entries whose resource does not load
    /// either way, and a model whose dot sits before its slash collapses the
    /// name.
    #[test]
    fn read_only_and_unloadable_census_faces() {
        // The read-only attribute seam marks the loaded entry.
        let ro = AnimationEditor::new(
            crab_model(),
            CRAB_PATH,
            |name| match name {
                "characters/npc/crab/crab_walkforward.animation" => Some(walkforward_bytes()),
                _ => None,
            },
            |_| true,
        );
        assert!(ro.is_read_only_anim(&crab_key("crab_walkforward")));
        // An animation without a nodes resource: the probe runs (and reads
        // only), the census records nothing; an unnamed one is skipped before
        // the probe.
        let model = parse(
            "bare.model",
            br"<bare.model><animation><name>	bare	</name></animation>\
              <animation><frameRate>	30.000000	</frameRate></animation></bare.model>",
        )
        .expect("bare.model parses");
        let bare = AnimationEditor::new(model, "bare2", |_| None, |_| false);
        assert!(bare.animations.is_empty(), "no nodes, no entry");
        assert_eq!(bare.model_name, "", "no dot in the path");
        assert_eq!(bare.anim_list.len(), 1);
        assert_eq!(bare.anim_list[0].anim_names.len(), 0);
        // A path with the dot before the slash also collapses to the empty
        // name, and a corrupt file is dropped after the probe.
        let corrupt = AnimationEditor::new(
            crab_model(),
            "x.model/y",
            |name| match name {
                "characters/npc/crab/crab_walkforward.animation" => Some(vec![1u8, 2, 3, 4]),
                _ => None,
            },
            |_| false,
        );
        assert_eq!(corrupt.model_name, "");
        assert!(
            corrupt.animations.is_empty(),
            "corrupt bytes drop the entry"
        );
    }

    /// `createAnim`'s unique-name retry, the fresh section's fields, and
    /// `removeAnim`.
    #[test]
    fn create_and_remove_anim() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        let second = editor.create_anim(&key, WALKFORWARD_RESOURCE);
        assert_eq!(second.0, "crab_walkforward 2", "the unique-name retry");
        assert!(editor.animations.contains_key(&second));
        assert_eq!(
            editor.anim_file(&second),
            format!("{WALKFORWARD_RESOURCE}.animation")
        );
        assert_eq!(editor.frame_rate(&second), 30.0);
        assert_eq!(editor.anim_name_get(&second), "crab_walkforward 2");
        assert_eq!(editor.anim_list[0].anim_names.len(), 2);
        assert_eq!(
            editor.animations[&second].channels.as_ref().unwrap().len(),
            45
        );
        // A name with no collision is kept as-is.
        let fresh = editor.create_anim(&crab_key("crab_pounce"), WALKFORWARD_RESOURCE);
        assert_eq!(fresh.0, "crab_pounce");
        // removeAnim deletes the section and drops the entry.
        editor.remove_anim(&second);
        assert!(!editor.animations.contains_key(&second));
        assert_eq!(editor.anim_list[0].anim_names.len(), 2);
        // An unknown id is a no-op.
        editor.remove_anim(&crab_key("nope"));
        // A second animation over a different nodes resource: the census
        // finds no old entry on that resource, so the channel scratch starts
        // fresh.
        let mut dual = dual_editor();
        let key = crab_key("crab_walkforward");
        let alt = dual.create_anim(&key, "characters/npc/crab/crab_alt");
        assert_eq!(alt.0, "crab_walkforward 2");
        assert_eq!(
            dual.animations[&alt].nodes_resource,
            "characters/npc/crab/crab_alt"
        );
        assert_eq!(dual.animations[&alt].channels.as_ref().unwrap().len(), 45);
        assert_eq!(dual.animations[&alt].time, -1.0, "no clock transfer");
    }

    /// `changeAnim` clears the clock and the channel scratch; `animFile`'s
    /// write face does not; an unloadable nodes drops the entry.
    #[test]
    fn change_anim_and_anim_file_faces() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        editor.set_frame_num(&key, 12);
        assert!(
            (editor.animations[&key].time - 0.4).abs() < 1e-6,
            "the clock moved"
        );
        // animFile rewrites the same nodes: the census keeps the clock.
        editor.set_anim_file(&key, WALKFORWARD_RESOURCE);
        assert!(
            (editor.animations[&key].time - 0.4).abs() < 1e-6,
            "animFile keeps the clock"
        );
        // changeAnim clears the clock and the scratch, which the census then
        // re-backs-up.
        editor.change_anim(&key, WALKFORWARD_RESOURCE);
        assert_eq!(editor.animations[&key].time, 0.0);
        assert_eq!(editor.animations[&key].channels.as_ref().unwrap().len(), 45);
        // An unloadable nodes resource drops the entry.
        editor.change_anim(&key, "characters/npc/crab/crab_missing");
        assert!(!editor.animations.contains_key(&key));
        // An unknown id is a no-op.
        editor.change_anim(&crab_key("nope"), "x");
        editor.set_anim_file(&crab_key("nope"), "x");
    }

    /// The frame-rate scratch: the slider's no-change skip, the `final`
    /// commit, `saveFrameRate`'s init/section write, and the scratch
    /// surviving a census by key.
    #[test]
    fn frame_rate_scratch_faces() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        editor.set_local_frame_rate(&key, 30.0, false);
        assert_eq!(editor.local_frame_rate(&key), 30.0, "no-change skip");
        editor.set_local_frame_rate(&key, 12.0, false);
        assert_eq!(editor.local_frame_rate(&key), 12.0);
        editor.set_local_frame_rate(&key, 15.0, true);
        assert_eq!(editor.local_frame_rate(&key), 15.0);
        assert_eq!(editor.last_frame_rate, 15.0, "the slider's last rate");
        assert_eq!(
            editor.animations[&key].frame_rates.init, 30.0,
            "not saved yet"
        );
        editor.save_frame_rate(&key);
        assert_eq!(
            editor.animations[&key].frame_rates,
            FrameRates {
                curr: 15.0,
                init: 15.0
            }
        );
        assert_eq!(editor.frame_rate(&key), 15.0, "the section carries it");
        // The scratch survives the census by key.
        editor.set_local_frame_rate(&key, 7.0, false);
        editor.save_frame_rate(&key);
        assert_eq!(editor.frame_rate(&key), 7.0);
        assert_eq!(editor.animations[&key].frame_rates.curr, 7.0);
        // Unknown ids are no-ops and read the default.
        editor.set_local_frame_rate(&crab_key("nope"), 5.0, true);
        editor.save_frame_rate(&crab_key("nope"));
        assert_eq!(editor.local_frame_rate(&crab_key("nope")), 30.0);
    }

    /// The clock faces: clamping, the exact-frame read-back and the
    /// `almostEqual` snap-up.
    #[test]
    fn frame_num_clock_faces() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        editor.set_frame_num(&key, -5);
        assert_eq!(editor.animations[&key].time, 0.0, "negative clamps to 0");
        editor.set_frame_num(&key, 1000);
        assert!(
            (editor.animations[&key].time - 25.0 / 30.0).abs() < 1e-6,
            "clamps to the duration"
        );
        editor.set_frame_num(&key, 12);
        assert_eq!(editor.frame_num(&key), 12, "exact frames read back");
        // A fractional clock snaps up unless it is almost an integer.
        editor.animations.get_mut(&key).unwrap().time = 0.505;
        assert_eq!(editor.frame_num(&key), 16, "15.15 snaps up to 16");
        // Empty keys and unknown ids.
        editor.set_frame_num(&crab_key(""), 5);
        editor.set_frame_num(&crab_key("nope"), 5);
        assert_eq!(editor.frame_num(&crab_key("")), 0);
        assert_eq!(editor.num_frames(&crab_key("")), 100);
    }

    /// The frame-range faces: writes, the no-change skip, `-1` deletes and
    /// the duration following the narrowed range.
    #[test]
    fn first_and_last_frame_faces() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        assert_eq!(editor.first_frame(&key), -1);
        assert_eq!(editor.last_frame(&key), -1);
        editor.set_first_frame(&key, -1);
        assert_eq!(editor.first_frame(&key), -1, "no-change skip");
        editor.set_first_frame(&key, 2);
        assert_eq!(editor.first_frame(&key), 2);
        editor.set_first_frame(&key, 2);
        assert_eq!(editor.first_frame(&key), 2, "no-change skip");
        editor.set_first_frame(&key, -1);
        assert_eq!(editor.first_frame(&key), -1, "deleted");
        editor.set_last_frame(&key, 20);
        assert_eq!(editor.last_frame(&key), 20);
        editor.set_last_frame(&key, 20);
        editor.set_last_frame(&key, -1);
        assert_eq!(editor.last_frame(&key), -1, "deleted");
        // A narrowed range shortens the duration.
        editor.set_last_frame(&key, 10);
        assert!(
            (editor.animations[&key].duration - 10.0 / 30.0).abs() < 1e-6,
            "duration follows the range"
        );
        assert_eq!(editor.num_frames(&key), 11, "10 frames + 1");
        // Unknown ids are no-ops.
        editor.set_first_frame(&crab_key("nope"), 3);
        editor.set_last_frame(&crab_key("nope"), 3);
    }

    /// The alpha weights: the default-one skip, sanitised section names,
    /// unsanitised map keys, the no-change skip and the last-weight removal
    /// killing the section.
    #[test]
    fn bone_weight_faces() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        editor.set_anim_bone_weight(&key, "CrabHub", 1.0);
        assert_eq!(
            editor.anim_bone_weight(&key, "CrabHub"),
            -1.0,
            "the default-one write is skipped"
        );
        editor.set_anim_bone_weight(&key, "Crab Hub", 0.5);
        assert_eq!(editor.anim_bone_weight(&key, "Crab Hub"), 0.5);
        let entry = &editor.animations[&key];
        let section = editor.model.open_child(entry.data[0]).unwrap();
        let alpha = section.open_section("alpha").expect("alpha created");
        assert_eq!(alpha.read_float("Crab..Hub", -1.0), 0.5, "sanitised");
        editor.set_anim_bone_weight(&key, "Crab Hub", 0.5);
        assert_eq!(
            editor.anim_bone_weight(&key, "Crab Hub"),
            0.5,
            "no-change skip"
        );
        assert_eq!(editor.animations[&key].bone_weights["Crab Hub"], 0.5);
        // A different value over an existing alpha section rewrites it.
        editor.set_anim_bone_weight(&key, "Crab Hub", 0.7);
        assert_eq!(editor.anim_bone_weight(&key, "Crab Hub"), 0.7);
        let entry = &editor.animations[&key];
        let section = editor.model.open_child(entry.data[0]).unwrap();
        assert_eq!(
            section
                .open_section("alpha")
                .expect("alpha still there")
                .read_float("Crab..Hub", -1.0),
            0.7
        );
        // Two bones: removing one leaves the section (and the other bone).
        editor.set_anim_bone_weight(&key, "CrabHub", 0.3);
        editor.remove_anim_node(&key, "Crab Hub");
        assert_eq!(editor.anim_bone_weight(&key, "CrabHub"), 0.3);
        let entry = &editor.animations[&key];
        let section = editor.model.open_child(entry.data[0]).unwrap();
        let alpha = section.open_section("alpha").expect("alpha survives");
        assert_eq!(alpha.read_float("CrabHub", -1.0), 0.3);
        assert!(alpha.read_float("Crab..Hub", -1.0) < 0.0, "bone removed");
        // Removing the only bone kills the alpha section.
        editor.remove_anim_node(&key, "CrabHub");
        let entry = &editor.animations[&key];
        let section = editor.model.open_child(entry.data[0]).unwrap();
        assert!(
            section.open_section("alpha").is_none(),
            "last weight kills alpha"
        );
        // Unknown ids and unknown bones.
        editor.set_anim_bone_weight(&crab_key("nope"), "b", 0.5);
        editor.remove_anim_node(&crab_key("nope"), "b");
        editor.remove_anim_node(&key, "gone");
        assert_eq!(editor.anim_bone_weight(&key, "gone"), -1.0);
    }

    /// The rename face: same-name and collision exits, the table, the tree
    /// list, the section's `name` and the current pages all following.
    #[test]
    fn rename_faces() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        editor.set_anim(0, key.clone());
        // A second page on another animation and a foreign branch in the
        // tree list (a super model's census carries one branch per member
        // model): only the renamed animation's branch and page follow.
        editor.set_anim(1, crab_key("crab_pounce"));
        assert!(editor.anim_mode, "setAnim flags the mode");
        assert!(
            editor.anim_name(&key, "crab_walkforward"),
            "same name exits true"
        );
        let second = editor.create_anim(&key, WALKFORWARD_RESOURCE);
        assert_eq!(second.0, "crab_walkforward 2");
        assert!(
            !editor.anim_name(&key, "crab_walkforward 2"),
            "a colliding name exits false"
        );
        // A foreign branch in the tree list (a super model's census carries
        // one branch per member model); seeded after the census-triggering
        // create so it survives into the rename.
        editor.anim_list.push(TreeBranch {
            model_name: "other".to_string(),
            model_path: "characters/npc/crab/other.model".to_string(),
            anim_names: vec!["crab_walkforward".to_string()],
        });
        assert!(editor.anim_name(&key, "crab_stride"));
        let renamed = crab_key("crab_stride");
        assert!(editor.animations.contains_key(&renamed));
        assert!(!editor.animations.contains_key(&key));
        assert_eq!(editor.anim_name_get(&renamed), "crab_stride");
        assert_eq!(
            editor.anim_list[0].anim_names,
            vec!["crab_stride".to_string(), "crab_walkforward 2".to_string()]
        );
        assert_eq!(
            editor.anim_list[1].anim_names,
            vec!["crab_walkforward".to_string()],
            "the foreign branch is untouched"
        );
        assert_eq!(editor.get_anim(0), Some(&renamed), "the page followed");
        assert_eq!(
            editor.get_anim(1),
            Some(&crab_key("crab_pounce")),
            "the foreign page keeps its animation"
        );
        assert!(!editor.anim_name(&crab_key("nope"), "x"), "unknown id");
        assert_eq!(editor.anim_name_get(&crab_key("nope")), "");
    }

    /// The page and player flags.
    #[test]
    fn page_and_play_faces() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        assert_eq!(editor.get_anim(0), None);
        editor.set_anim(0, key.clone());
        assert_eq!(editor.get_anim(0), Some(&key));
        editor.stop_anim(0);
        assert_eq!(editor.get_anim(0), None);
        editor.stop_anim(0);
        editor.play_anim(true);
        assert!(editor.anim_mode && editor.playing && !editor.clear_anim);
        editor.play_anim(false);
        assert!(!editor.playing);
    }

    /// The channel scratch: unknown ids, the backup refill and the restore
    /// writing the scratch into the animation.
    #[test]
    fn channel_backup_and_restore() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        assert!(editor.backup_channels(&crab_key("nope")).is_none());
        assert!(editor.restore_channels(&crab_key("nope")).is_none());
        let backup = editor.backup_channels(&key).expect("walkforward loads");
        assert_eq!(backup.len(), 45);
        assert_eq!(editor.animations[&key].channels.as_ref().unwrap().len(), 45);
        assert!(editor.get_moo_anim(&key).is_some());
        // The restore writes the scratch into the animation: an emptied
        // scratch empties the animation's channels.
        editor.animations.get_mut(&key).unwrap().channels = Some(Vec::new());
        let restored = editor.restore_channels(&key).expect("scratch exists");
        assert!(restored.is_empty());
        assert_eq!(
            editor.animations[&key]
                .animation
                .as_ref()
                .unwrap()
                .channel_count(),
            0,
            "the scratch went back into the animation"
        );
        // And the real channels come back.
        editor.backup_channels(&key);
        editor.animations.get_mut(&key).unwrap().channels = Some(backup.clone());
        editor.restore_channels(&key);
        assert_eq!(
            editor.animations[&key]
                .animation
                .as_ref()
                .unwrap()
                .channel_count(),
            45
        );
    }

    /// The census keeps the clock and the frame-rate scratch by key, and the
    /// channel scratch additionally by nodes resource; `cleanAnim` drops
    /// both and the next census re-backs-up.
    #[test]
    fn census_preserves_scratch_by_key_and_matches_channels_by_nodes() {
        let mut editor = crab_editor();
        let key = crab_key("crab_walkforward");
        editor.set_frame_num(&key, 12);
        editor.set_local_frame_rate(&key, 12.0, false);
        let second = editor.create_anim(&key, WALKFORWARD_RESOURCE);
        assert!(
            (editor.animations[&key].time - 0.4).abs() < 1e-6,
            "the clock survives by key"
        );
        assert_eq!(editor.animations[&key].frame_rates.curr, 12.0);
        assert_eq!(
            editor.animations[&second].time, -1.0,
            "no clock for the new key"
        );
        assert_eq!(
            editor.animations[&second].channels, editor.animations[&key].channels,
            "the channel scratch matched by nodes resource"
        );
        // cleanAnim drops the clock and the scratch; the next census
        // re-backs-up.
        editor.clean_anim(&second);
        assert_eq!(editor.animations[&second].time, 0.0);
        assert!(editor.animations[&second].channels.is_none());
        editor.save_frame_rate(&key);
        assert_eq!(
            editor.animations[&second].channels.as_ref().unwrap().len(),
            45,
            "re-backed-up on reload"
        );
        editor.clean_anim(&crab_key("nope"));
    }

    /// BW-13-04-03 acceptance — fixed clock times match the oracle bone
    /// poses: the editor drives the clock, the frame mapping and the pose
    /// sampling of the client skeleton engine land exactly on the oracle
    /// world matrices the reference math produced.
    #[test]
    fn fixed_time_bone_poses_match_the_oracle() {
        let mut editor = crab_editor();
        let visual =
            parse("crab.visual", include_bytes!("../res/crab.visual")).expect("crab.visual parses");
        let skeleton = Skeleton::from_visual_section(&visual).expect("crab skeleton");
        let key = crab_key("crab_walkforward");
        let animation = editor
            .get_moo_anim(&key)
            .expect("the walkforward entry")
            .clone();
        let bound = animation.bind(&skeleton);
        // Oracle indices 0, 3, 4, 5 are the integral frames 0, 12, 18, 25.
        for fi in [0usize, 3, 4, 5] {
            let frame = FRAMES[fi] as i32;
            editor.set_frame_num(&key, frame);
            let time = editor.animations[&key].time;
            assert!(
                (time - frame as f32 / 30.0).abs() < 1e-6,
                "frame {frame} clock"
            );
            let frame_num = editor.frame_num(&key);
            assert_eq!(frame_num, frame, "the clock reads back the fixed frame");
            // The frame mapping the SuperModelAnimation plays through.
            let entry = &editor.animations[&key];
            let section = editor.model.open_child(entry.data[0]).unwrap();
            let frames = FrameRange::from_animation_section(
                section,
                entry.animation.as_ref().unwrap().total_time(),
            );
            let frame_time = frames.frame_at(time);
            assert!(
                (frame_time - frame as f32).abs() < 1e-4,
                "frame {frame} maps to itself"
            );
            let world = bound.world_pose(&skeleton, frame_num as f32);
            for (pi, probe) in PROBES.iter().enumerate() {
                let node = skeleton
                    .find(probe)
                    .unwrap_or_else(|| panic!("probe {probe} is in the skeleton"));
                let expected = Matrix { m: WORLD[fi][pi] };
                assert!(
                    world[node].almost(&expected, 5e-6),
                    "frame {frame} probe {probe}:\ngot      {:?}\nexpected {:?}",
                    world[node].m,
                    expected.m
                );
            }
        }
    }
}
