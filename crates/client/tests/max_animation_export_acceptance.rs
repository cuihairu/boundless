//! BW-13-05-02 acceptance: the portable Max animation exporter
//! (`asset_pipeline::animation_exporter`) and the engine reader
//! (`client::skeleton::Animation::from_bytes`) agree — the exported
//! `.animation` bytes carry the skeleton's channels (one per bone, in
//! export order) and the time keyframes (key times and their
//! scale/position/rotation values) that the reader reconstructs.
//!
//! The Max host stands in as an in-memory seam: fixed res paths, a
//! settings map and the captured animation bytes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use asset_pipeline::animation_exporter::{AnimationIo, ExportInput, MfxExporter, SceneNode};
use client::skeleton::{Animation, Channel};
use foundation::math::Matrix;
use resources::datasection::DataSection;

/// State shared between the test and the exporter's host seam.
#[derive(Default)]
struct Shared {
    settings: HashMap<String, DataSection>,
    written: HashMap<String, Vec<u8>>,
}

/// The in-memory Max host.
#[derive(Default, Clone)]
struct Host {
    shared: Rc<RefCell<Shared>>,
}

impl AnimationIo for Host {
    fn dissolve_filename(&self, filename: &str) -> String {
        filename
            .strip_prefix("/res/")
            .map(|rest| rest.to_string())
            .unwrap_or_else(|| filename.to_string())
    }

    fn res_paths(&self) -> Vec<String> {
        vec!["C:/game/res".to_string()]
    }

    fn open_section(&self, _filename: &str) -> Option<DataSection> {
        None
    }

    fn read_settings_xml(&self, filename: &str) -> Option<DataSection> {
        self.shared.borrow().settings.get(filename).cloned()
    }

    fn write_settings_xml(&mut self, filename: &str, root: &DataSection) {
        self.shared
            .borrow_mut()
            .settings
            .insert(filename.to_string(), root.clone());
    }

    fn read_animation(&self, _filename: &str) -> Option<Vec<u8>> {
        None
    }

    fn write_animation(&mut self, filename: &str, bytes: &[u8]) -> bool {
        self.shared
            .borrow_mut()
            .written
            .insert(filename.to_string(), bytes.to_vec());
        true
    }

    fn delete_file(&mut self, _filename: &str) {}
}

/// A node whose world transform is the translation `(x, y, z)` at every
/// frame (the scene root's own world is never read: the root channel is
/// the stored identity after `setMaxNode(NULL)`).
fn translated(name: &str, x: f32, y: f32, z: f32, frames: usize) -> SceneNode {
    let mut m = Matrix::identity();
    m.m[3] = [x, y, z, 1.0];
    SceneNode {
        name: name.to_string(),
        world: vec![m; frames],
        ..Default::default()
    }
}

/// A two-bone walk: `Scene -> Hub -> Bone`, two frames, the bone rotating
/// 90 degrees about the Max z axis between them at a fixed offset.
fn walk_scene() -> SceneNode {
    let mut bone_worlds = Vec::new();
    for angle in [0.0f32, std::f32::consts::FRAC_PI_2] {
        let mut m = Matrix::identity();
        m.m[0][0] = angle.cos();
        m.m[0][1] = angle.sin();
        m.m[1][0] = -angle.sin();
        m.m[1][1] = angle.cos();
        m.m[3][0] = 2.0;
        bone_worlds.push(m);
    }
    let bone = SceneNode {
        name: "Bone".to_string(),
        world: bone_worlds,
        ..Default::default()
    };
    let mut hub = translated("Hub", 0.0, 1.0, 0.0, 2);
    hub.children.push(bone);
    let mut scene = translated("Scene", 0.0, 0.0, 0.0, 2);
    scene.children.push(hub);
    scene
}

type Vec3Keys = Vec<(f32, foundation::math::Vector3)>;
type QuatKeys = Vec<(f32, foundation::math::Quaternion)>;

fn interpo(channel: &Channel) -> (&str, &Vec3Keys, &Vec3Keys, &QuatKeys) {
    match channel {
        Channel::Interpolated {
            identifier,
            scale_keys,
            position_keys,
            rotation_keys,
        } => (identifier, scale_keys, position_keys, rotation_keys),
        other => panic!("expected an interpolated channel, got {other:?}"),
    }
}

/// The exported bytes parse back with one channel per bone, in export
/// order, and the key times/values survive the round trip.
#[test]
fn exported_animation_carries_the_skeleton_and_keyframes() {
    let input = ExportInput {
        filename: "/res/chars/acceptance.animation".to_string(),
        scene: walk_scene(),
        anim_range: (0, 1),
        ..Default::default()
    };
    let host = Host::default();
    let mut exporter = MfxExporter::new(host.clone());
    exporter.do_export(&input).expect("export succeeds");

    let bytes = host
        .shared
        .borrow()
        .written
        .get("/res/chars/acceptance.animation")
        .expect("the animation file was written")
        .clone();

    let anim = Animation::from_bytes(&bytes).expect("the engine reads the export");
    assert_eq!(anim.total_time(), 1.0);
    assert_eq!(anim.identifier(), "acceptance");
    assert_eq!(anim.identifier(), anim.internal_identifier());

    // The skeleton: Scene Root first, then the bones in depth order.
    let identifiers: Vec<&str> = anim
        .channels()
        .iter()
        .map(|channel| interpo(channel).0)
        .collect();
    assert_eq!(identifiers, ["Scene Root", "Hub", "Bone"]);

    // Hub: identity relative to the root, sitting 1cm up (the 0.01 unit
    // scale), keys at both frame times.
    let (_, hub_scales, hub_positions, hub_rotations) = interpo(&anim.channels()[1]);
    assert_eq!(
        hub_scales.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        [0.0, 1.0]
    );
    assert_eq!(
        hub_scales[0].1,
        foundation::math::Vector3::new(1.0, 1.0, 1.0)
    );
    assert_eq!(
        hub_positions[0].1,
        foundation::math::Vector3::new(0.0, 0.01, 0.0)
    );
    assert_eq!(
        hub_positions[1].1,
        foundation::math::Vector3::new(0.0, 0.01, 0.0)
    );
    assert_eq!(
        hub_rotations[0].1,
        foundation::math::Quaternion::new(0.0, 0.0, 0.0, 1.0)
    );
    assert_eq!(
        hub_rotations[1].1,
        foundation::math::Quaternion::new(0.0, 0.0, 0.0, 1.0)
    );

    // Bone: relative to the hub, the fixed (2,-1,0) offset shrunk by the
    // unit scale, and the frame-1 z rotation the rearrange maps to -90
    // degrees about y.
    let (_, bone_scales, bone_positions, bone_rotations) = interpo(&anim.channels()[2]);
    assert_eq!(
        bone_scales.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        [0.0, 1.0]
    );
    assert_eq!(
        bone_positions[0].1,
        foundation::math::Vector3::new(0.02, -0.01, 0.0)
    );
    assert_eq!(
        bone_positions[1].1,
        foundation::math::Vector3::new(0.02, -0.01, 0.0)
    );
    assert_eq!(
        bone_rotations[0].1,
        foundation::math::Quaternion::new(0.0, 0.0, 0.0, 1.0)
    );
    let expected = foundation::math::Quaternion::new(
        0.0,
        -std::f32::consts::FRAC_1_SQRT_2,
        0.0,
        std::f32::consts::FRAC_1_SQRT_2,
    );
    assert!(
        bone_rotations[1].1.almost_equal(&expected),
        "{:?}",
        bone_rotations[1].1
    );
}

/// The cue note track lands in the file as a type-6 cue channel the
/// engine reads back.
#[test]
fn cue_note_track_round_trips_into_a_cue_channel() {
    let mut hub = translated("Hub", 0.0, 0.0, 0.0, 1);
    hub.note_keys = vec![(3 * 160, "cue:footsteps".to_string())];
    let mut scene = translated("Scene", 0.0, 0.0, 0.0, 1);
    scene.children.push(hub);

    let mut over = DataSection::new("settings");
    over.write_bool("exportCueTrack", true);

    let input = ExportInput {
        filename: "/res/chars/cues.animation".to_string(),
        scene,
        anim_range: (0, 3),
        override_section: Some(over),
        ..Default::default()
    };
    let host = Host::default();
    let mut exporter = MfxExporter::new(host.clone());
    exporter.do_export(&input).expect("export succeeds");

    let bytes = host.shared.borrow().written["/res/chars/cues.animation"].clone();
    let anim = Animation::from_bytes(&bytes).expect("the engine reads the export");
    assert_eq!(anim.identifier(), "cues");

    // Scene Root + Hub, then the cue channel.
    assert_eq!(anim.channel_count(), 3);
    match anim.channels().last().unwrap() {
        Channel::Cue { cues } => {
            assert_eq!(cues.len(), 1);
            // 480 ticks at 160 ticks/frame = second 3.
            assert_eq!(cues[0].0, 3.0);
            // The "cue:" prefix becomes "c".
            assert_eq!(cues[0].1, "cfootsteps");
            // No additional arguments.
            assert!(cues[0].2.is_empty());
        }
        other => panic!("expected the cue channel, got {other:?}"),
    }
}
