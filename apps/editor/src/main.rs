//! BW-15-02-03 the editor entry point's Linux edit leg: load the fixed map
//! into the editor world, move an item through the matrix/undo/save faces
//! and publish the edited chunk back to disk.
//!
//! Usage:
//!
//! ```sh
//! boundless-editor [--res DIR] [--out DIR] [--resource NAME]
//!                  [--dx X] [--dy Y] [--dz Z]
//!                  [--shot-before PATH] [--shot-after PATH]
//!                  [--width W] [--height H]
//! ```
//!
//! The session drives the same faces the WorldEditor uses: the items load
//! through [`SectionItemFactory`], the move runs through
//! [`ChunkItemMatrix::set_matrix`] + `commit_state` (which pushes the undo
//! op and the `GIZMO/UNDO/MOVE_PROPERTY` barrier), the undo/redo pair walks
//! the recorded pose back and forward, and the changed chunk saves through
//! [`UnsavedChunks`] with an [`EditorChunkCache::ed_save`]-backed saver.
//! The published space reloads from the output directory and the moved
//! item's new pose is verified there; stdout carries `metric<TAB>value`
//! lines and any contract breach exits non-zero.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chunk::chunk::read_matrix34;
use chunk::{
    Chunk, ChunkItemMatrix, ChunkSaver, ChunkSpace, EditorChunkCache, EditorWorld, GeometryMapping,
    SectionItemFactory, UnsavedChunks,
};
use client::render::map_scene::MapScene;
use client::render::software::SoftwareDevice;
use client::screen_shot::ScreenCopy;
use foundation::math::Matrix;
use resources::bwresource::BwResource;
use resources::datasection::{self, DataSection};
use resources::file_system::NativeFileSystem;
use resources::multi_file_system::MultiFileSystem;

const DEFAULT_RES: &str = "docs/review/fixtures/chunk_space/res/space_00010001";
const DEFAULT_OUT: &str = "editor-out";
const DEFAULT_RESOURCE: &str = "tree00.model";
const DEFAULT_WIDTH: u32 = 320;
const DEFAULT_HEIGHT: u32 = 240;

/// The loaded space, its bound chunk identifiers, and each bound chunk's
/// root section — the trio the session threads through load, edit and save.
type LoadedSpace = (ChunkSpace, Vec<String>, BTreeMap<String, DataSection>);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = arg_value(&args, "--res").unwrap_or_else(|| DEFAULT_RES.to_string());
    let out = arg_value(&args, "--out").unwrap_or_else(|| DEFAULT_OUT.to_string());
    let resource = arg_value(&args, "--resource").unwrap_or_else(|| DEFAULT_RESOURCE.to_string());
    let dx: f32 = arg_value(&args, "--dx")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30.0);
    let dy: f32 = arg_value(&args, "--dy")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let dz: f32 = arg_value(&args, "--dz")
        .and_then(|v| v.parse().ok())
        .unwrap_or(30.0);
    let shot_before =
        arg_value(&args, "--shot-before").unwrap_or_else(|| "editor-before.tga".to_string());
    let shot_after =
        arg_value(&args, "--shot-after").unwrap_or_else(|| "editor-after.tga".to_string());
    let width: u32 = arg_value(&args, "--width")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_WIDTH);
    let height: u32 = arg_value(&args, "--height")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_HEIGHT);

    let res_root = PathBuf::from(&res);
    let out_root = PathBuf::from(&out);

    // ---- load the fixed map (the same loader path the client drives) ----
    let (space, bound, mut roots) = match load_space(&res_root) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("boundless-editor: {res}: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mapping_path = space
        .chunks()
        .next()
        .map(|c| c.mapping.path.clone())
        .unwrap_or_default();

    // the before view: the loaded map as the client displays it
    let before = MapScene::from_space(&space);
    let (before_frame, before_digest) = render_shot(&before, width, height, &shot_before);

    // ---- the edit session ----
    let mut world = EditorWorld::new(space);
    let mut child_of_item: BTreeMap<usize, usize> = BTreeMap::new();
    for identifier in &bound {
        let Some(section) = roots.get(identifier) else {
            eprintln!("boundless-editor: {identifier}: no root section for the loaded chunk");
            return ExitCode::FAILURE;
        };
        for i in 0..section.count_children() {
            let Some(child) = section.open_child(i) else {
                continue;
            };
            if child.is_attribute() {
                continue;
            }
            let tag = child.section_name().to_string();
            if matches!(tag.as_str(), "transform" | "boundingBox" | "shell") {
                continue;
            }
            let result = world.load_item(&mut SectionItemFactory, identifier, child);
            if let Some(item) = result.item {
                // the factory builds an identity pose; the chunk file's
                // `<transform>` child carries the stored one
                world.items[item].transform = read_matrix34(child, "transform");
                child_of_item.insert(item, i);
            }
        }
    }
    let items_loaded = world.items.len();
    let Some(item) = world.items.iter().position(|i| {
        i.resource
            .as_deref()
            .unwrap_or_default()
            .contains(&resource)
    }) else {
        eprintln!("boundless-editor: no loaded item's resource matches {resource}");
        return ExitCode::FAILURE;
    };
    let description = world.items[item].description.clone();
    let Some(chunk_id) = world.items[item].chunk.clone() else {
        eprintln!("boundless-editor: {description}: not added to a chunk");
        return ExitCode::FAILURE;
    };

    // move through the matrix proxy: transient drag + permanent commit
    let mut proxy = ChunkItemMatrix::new(item, &world);
    let from = proxy.get_matrix(&world, false).apply_to_origin();
    proxy.record_state(&world);
    let mut dest = Matrix::identity();
    dest.set_translate(from.x + dx, from.y + dy, from.z + dz);
    if !proxy.set_matrix(&mut world, &dest) {
        eprintln!("boundless-editor: {description}: the transient move refused");
        return ExitCode::FAILURE;
    }
    if !proxy.commit_state(&mut world, false, true) {
        eprintln!("boundless-editor: {description}: the commit refused");
        return ExitCode::FAILURE;
    }
    let to = world.items[item].transform.apply_to_origin();
    let asked = dest.apply_to_origin();
    if (to.x - asked.x).abs() > 1e-4
        || (to.y - asked.y).abs() > 1e-4
        || (to.z - asked.z).abs() > 1e-4
    {
        eprintln!("boundless-editor: {description}: landed at {to:?} not the requested offset");
        return ExitCode::FAILURE;
    }
    let barrier = world
        .undo_barriers
        .last()
        .cloned()
        .unwrap_or_else(|| String::from("<none>"));
    let changed = world.changed_chunks.clone();

    // ---- the undo/redo walk ----
    if !world.can_undo() {
        eprintln!("boundless-editor: the committed move is not undoable");
        return ExitCode::FAILURE;
    }
    world.undo();
    let undone = world.items[item].transform.apply_to_origin();
    if undone != from {
        eprintln!("boundless-editor: undo landed at {undone:?} not the recorded pose");
        return ExitCode::FAILURE;
    }
    world.redo();
    let redone = world.items[item].transform.apply_to_origin();
    if redone != to {
        eprintln!("boundless-editor: redo landed at {redone:?} not the committed pose");
        return ExitCode::FAILURE;
    }

    // ---- apply the edit to the chunk's root section ----
    let Some(&child_index) = child_of_item.get(&item) else {
        eprintln!("boundless-editor: {description}: no source child recorded");
        return ExitCode::FAILURE;
    };
    {
        let Some(root) = roots.get_mut(&chunk_id) else {
            eprintln!("boundless-editor: {chunk_id}: no root section to edit");
            return ExitCode::FAILURE;
        };
        let Some(child) = root.child_mut(child_index) else {
            eprintln!("boundless-editor: {chunk_id}: child {child_index} vanished");
            return ExitCode::FAILURE;
        };
        child.del_child("transform");
        child.write_matrix34("transform", &world.items[item].transform);
    }

    // ---- save through UnsavedChunks + ed_save ----
    let space_dir = out_root.join(&mapping_path);
    if let Err(err) = std::fs::create_dir_all(&space_dir) {
        eprintln!(
            "boundless-editor: cannot create {}: {err}",
            space_dir.display()
        );
        return ExitCode::FAILURE;
    }
    let settings_src = res_root.join("settings.xml");
    let settings_dst = space_dir.join("settings.xml");
    if let Err(err) = std::fs::copy(&settings_src, &settings_dst) {
        eprintln!(
            "boundless-editor: cannot copy {}: {err}",
            settings_src.display()
        );
        return ExitCode::FAILURE;
    }
    let out_str = out_root.to_str().unwrap_or_default().to_string();
    let mut file_system = MultiFileSystem::new();
    file_system.add_base_file_system(Box::new(NativeFileSystem::new(&out_str)), 0);
    let mut saver = EdSaver {
        chunk: world.space.chunk(&chunk_id).expect("chunk loaded").clone(),
        section: roots.remove(&chunk_id).expect("root section"),
        cache: EditorChunkCache {
            present: true,
            deleted: false,
            locked: true,
            read_only: false,
        },
        file_system,
        saved: Vec::new(),
    };
    let mut unsaved = UnsavedChunks::new();
    unsaved.add(chunk_id.clone());
    if !unsaved.save(&mut saver, None, Vec::new()) {
        eprintln!("boundless-editor: {chunk_id}: the save batch failed");
        return ExitCode::FAILURE;
    }
    if !unsaved.saved_chunks().contains(&chunk_id) || saver.saved != vec![chunk_id.clone()] {
        eprintln!("boundless-editor: the save batch did not settle {chunk_id}");
        return ExitCode::FAILURE;
    }
    if !world.errors.is_empty() {
        eprintln!("boundless-editor: session errors: {:?}", world.errors);
        return ExitCode::FAILURE;
    }

    // ---- reload the published space and verify the stored pose ----
    let (reloaded, reload_bound, _) = match load_space(&space_dir) {
        Ok(found) => found,
        Err(err) => {
            eprintln!("boundless-editor: reload {}: {err}", space_dir.display());
            return ExitCode::FAILURE;
        }
    };
    let reload_chunks = reloaded.chunks().count();
    let Some(reload_chunk) = reloaded.chunk(&chunk_id) else {
        eprintln!("boundless-editor: reload: {chunk_id} not bound");
        return ExitCode::FAILURE;
    };
    let Some(stored) = reload_chunk.items.iter().find(|i| {
        i.resource
            .as_deref()
            .unwrap_or_default()
            .contains(&resource)
    }) else {
        eprintln!("boundless-editor: reload: no reloaded item matches {resource}");
        return ExitCode::FAILURE;
    };
    let reload_pose = stored.transform.apply_to_origin();
    if reload_pose != to {
        eprintln!("boundless-editor: reload pose {reload_pose:?} != committed {to:?}");
        return ExitCode::FAILURE;
    }
    let reload_bound_count = reload_bound.len();

    // the after view: the edited map as the client would display it
    let after = MapScene::from_space(&reloaded);
    let (after_frame, after_digest) = render_shot(&after, width, height, &shot_after);

    println!("resource\t{resource}");
    println!("chunk\t{chunk_id}");
    println!("items_loaded\t{items_loaded}");
    println!("move_from\t{:.3},{:.3},{:.3}", from.x, from.y, from.z);
    println!("move_to\t{:.3},{:.3},{:.3}", to.x, to.y, to.z);
    println!("undo_pose\t{:.3},{:.3},{:.3}", undone.x, undone.y, undone.z);
    println!("redo_pose\t{:.3},{:.3},{:.3}", redone.x, redone.y, redone.z);
    println!("undo_barrier\t{barrier}");
    println!(
        "changed_chunks\t{}",
        changed.iter().cloned().collect::<Vec<_>>().join(",")
    );
    println!("saved_chunks\t{}", saver.saved.join(","));
    println!("reload_chunks\t{reload_chunks}");
    println!("reload_bound\t{reload_bound_count}");
    println!(
        "reload_pose\t{:.3},{:.3},{:.3}",
        reload_pose.x, reload_pose.y, reload_pose.z
    );
    println!("shot_before\t{shot_before}");
    println!("shot_before_frame_digest\t{before_frame:016x}");
    println!("shot_before_digest\t{before_digest:016x}");
    println!("shot_after\t{shot_after}");
    println!("shot_after_frame_digest\t{after_frame:016x}");
    println!("shot_after_digest\t{after_digest:016x}");
    ExitCode::SUCCESS
}

/// `WEChunkSaver` (the WorldEditor's `UnsavedChunks::IChunkSaver`): the
/// delegate body is [`EditorChunkCache::ed_save`] over the chunk's live
/// root section and the publish file system.
struct EdSaver {
    chunk: Chunk,
    section: DataSection,
    cache: EditorChunkCache,
    file_system: MultiFileSystem,
    saved: Vec<String>,
}

impl ChunkSaver for EdSaver {
    fn save(&mut self, identifier: &str) -> bool {
        match self
            .cache
            .ed_save(&self.chunk, Some(&mut self.section), &self.file_system)
        {
            Ok(_created) => {
                self.saved.push(identifier.to_string());
                true
            }
            Err(err) => {
                eprintln!("boundless-editor: save {identifier}: {err}");
                false
            }
        }
    }

    fn is_deleted(&self, _identifier: &str) -> bool {
        false
    }
}

/// Load the space directory and keep every bound chunk's root section (the
/// sections the save path rewrites). Returns the space, the bound chunk
/// identifiers in load order, and the root sections keyed by identifier.
fn load_space(root: &Path) -> Result<LoadedSpace, String> {
    let raw = std::fs::read(root.join("settings.xml"))
        .map_err(|err| format!("cannot read settings.xml: {err}"))?;
    let settings = datasection::parse("", &raw).map_err(|err| format!("settings.xml: {err}"))?;
    let dir_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("space directory name is not utf-8")?
        .to_string();
    let mapping = GeometryMapping::from_settings(format!("{dir_name}/"), &settings);

    let root_str = root.to_str().ok_or("space path is not utf-8")?;
    let resource = BwResource::init_paths(&[root_str]);
    let resolver = |name: &str| {
        resource
            .open_section(name, false)
            .map(|section| (*section).clone())
    };
    let mut space = ChunkSpace::new(0, vec![mapping.clone()]);
    let candidates: Vec<String> = mapping.gather_chunks(root).into_iter().collect();
    let bound = space.load_and_bind(&candidates, &resolver);
    let mut roots = BTreeMap::new();
    for identifier in &bound {
        // the loader resolves bare names against the space root
        // (`load_chunk` prepends nothing; `mapping.path` only names the
        // chunk on disk for the save path)
        let name = format!("{identifier}.chunk");
        let Some(section) = resolver(&name) else {
            return Err(format!("{name}: the resolver found no section"));
        };
        roots.insert(identifier.clone(), section);
    }
    Ok((space, bound, roots))
}

/// Render the scene once and write the TGA screenshot; returns the frame's
/// buffer digest and the written file's FNV-1a digest (the file wraps the
/// buffer, so the two differ by the TGA header).
fn render_shot(scene: &MapScene, width: u32, height: u32, path: &str) -> (u64, u64) {
    let mut device = SoftwareDevice::new(width, height);
    let frame = device.present(scene);
    let tga = ScreenCopy::capture(device.framebuffer()).to_tga();
    let digest = fnv1a(&tga);
    if let Err(err) = std::fs::write(path, &tga) {
        eprintln!("boundless-editor: cannot write {path}: {err}");
    }
    (frame.checksum, digest)
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// FNV-1a over bytes, the digest the framebuffer uses.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
