//! OPT-13-01-03: optimize the map load — compare the EDITOR's wait
//! time when the space is (re)loaded from disk through the shipped
//! section cache versus the raw read+parse resolver.
//!
//! The editor's load gesture is a repeated one: open a space, walk its
//! chunks (a focus/query pass), close it, and open it again after an
//! edit. The large synthetic space here (a 12×12 grid: 144 chunk files
//! carved out of the committed fixture's XML shape) makes that wait
//! measurable. Two resolvers serve the same `ChunkSpace::load_and_bind`
//! face: the raw read+parse resolver (the OPT-10-01-01 baseline) and
//! the shipped `BwResource::open_section` LRU section cache
//! (OPT-04-01-02 / OPT-10-01-02). The first open fills the cache;
//! every later open of the same file is a hit.
//!
//! Structural identity is asserted: both resolvers bind the same
//! files in the same order, every bound chunk reports the same item
//! tags/resources, the focus/query pass answers the same owning
//! chunk identifiers, and the cached parse equals the raw parse.
//! Wall-clocks and allocation counts are printed (`MAP-EDIT` lines)
//! as probe data only; the contracts are the equality assertions.
//!
//! ```sh
//! cargo test -p chunk --test editor_map_load_opt
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use chunk::{Chunk, ChunkSpace, GeometryMapping};
use foundation::math::Vector3;
use resources::bwresource::BwResource;
use resources::datasection::{self, DataSection};

// ---------------------------------------------------------------------------
// The counting allocator (the OPT-01-01-01 pattern).
// ---------------------------------------------------------------------------

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if TRACKING.with(Cell::get) {
            ALLOCS.with(|a| a.set(a.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn tracked<T>(body: impl FnOnce() -> T) -> (T, usize) {
    TRACKING.with(|t| t.set(true));
    ALLOCS.with(|a| a.set(0));
    let value = body();
    TRACKING.with(|t| t.set(false));
    (value, ALLOCS.with(Cell::get))
}

// ---------------------------------------------------------------------------
// The large synthetic space: a 12×12 grid of chunk files built from
// the committed fixture's XML shape, each with a grid-tagged model.
// ---------------------------------------------------------------------------

static WALKS: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ap-editor-map-{tag}-{}-{}",
            std::process::id(),
            WALKS.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Per-chunk structural ledger: identifier, item tags and item resources.
type ItemLedger = Vec<(String, Vec<String>, Vec<String>)>;

/// Grid width/depth of the synthetic space (144 chunk files). The
/// serialized sections fit the shipped 100 KiB section-cache budget.
const GRID: i32 = 12;
/// The shipped cell size (`chunkSize=100` in the committed fixture).
const CELL: f32 = 100.0;

/// One chunk file's XML — the committed fixture's tags (model, light,
/// sound) with grid-tagged resource names and positions.
fn chunk_xml(gx: i32, gz: i32) -> String {
    format!(
        "<{file}>\n\
         \t<model>\n\
         \t\t<resource>model_{gx}_{gz}.model</resource>\n\
         \t\t<transform><row3>{x} 0 {z}</row3></transform>\n\
         \t</model>\n\
         \t<light>\n\
         \t\t<type>Omni</type>\n\
         \t\t<colour>1 0.8 0.6</colour>\n\
         \t\t<innerRadius>6.6</innerRadius>\n\
         \t\t<outerRadius>13.5</outerRadius>\n\
         \t\t<transform><row3>{x} 10 {z}</row3></transform>\n\
         \t</light>\n\
         \t<sound>\n\
         \t\t<resource>amb_{gx}_{gz}.sound</resource>\n\
         \t\t<position>{x} 0 {z}</position>\n\
         \t\t<period>1.0</period>\n\
         \t</sound>\n\
         </{file}>\n",
        file = identifier(gx, gz),
        x = gx as f32 * CELL + CELL / 2.0,
        z = gz as f32 * CELL + CELL / 2.0,
    )
}

/// The outside chunk identifier for a flat (single_dir=false) grid.
fn identifier(gx: i32, gz: i32) -> String {
    format!("{gx:04x}{gz:04x}o")
}

fn settings_xml() -> String {
    format!(
        "<root>\n\
         \t<chunkSize>{CELL}</chunkSize>\n\
         \t<bounds>\n\
         \t\t<minX>0</minX><minY>0</minY>\n\
         \t\t<maxX>{}</maxX><maxY>{}</maxY>\n\
         \t</bounds>\n\
         \t<singleDir>false</singleDir>\n\
         </root>\n",
        GRID - 1,
        GRID - 1
    )
}

/// Plant a fresh 12×12 space under `{dir}/res/space_00010001/`.
fn plant_space(dir: &Path) -> PathBuf {
    let root = dir.join("res/space_00010001");
    std::fs::create_dir_all(&root).expect("space directory");
    std::fs::write(root.join("settings.xml"), settings_xml()).expect("settings written");
    for gz in 0..GRID {
        for gx in 0..GRID {
            let id = identifier(gx, gz);
            std::fs::write(root.join(format!("{id}.chunk")), chunk_xml(gx, gz))
                .expect("chunk written");
        }
    }
    root
}

fn load_mapping(root: &Path) -> GeometryMapping {
    let raw = std::fs::read(root.join("settings.xml")).expect("settings.xml readable");
    let settings = datasection::parse("", &raw).expect("settings.xml parses");
    GeometryMapping::from_settings("space_00010001/", &settings)
}

/// The raw resolver: every call reads the file and parses it (the
/// OPT-10-01-01 baseline face).
fn raw_resolver(root: PathBuf) -> impl Fn(&str) -> Option<DataSection> {
    move |name: &str| {
        let raw = std::fs::read(root.join(name)).ok()?;
        datasection::parse("", &raw).ok()
    }
}

/// The optimized resolver: the shipped section LRU. The first open
/// fills the cache; every later open of the same file is a hit. The
/// resolver is paired with its resource so the test can read the LRU's
/// hit/miss ledger after the walk.
fn cached_resolver(
    root: &Path,
) -> (
    std::rc::Rc<BwResource>,
    impl Fn(&str) -> Option<DataSection>,
) {
    let root = root.to_string_lossy().into_owned();
    let resource = std::rc::Rc::new(BwResource::init_paths(&[root.as_str()]));
    let resolver = {
        let resource = std::rc::Rc::clone(&resource);
        move |name: &str| resource.open_section(name, false).map(|s| (*s).clone())
    };
    (resource, resolver)
}

/// The editor's open-walk-close gesture over one resolver: enumerate
/// the space, load and bind all 144 cells, capture the structural
/// ledger, focus every cell and query the owning chunk at each centre,
/// then drop the space. Every leg runs this exact same gesture so the
/// two channels' wall-clocks measure the same work.
fn editor_open(
    root: &Path,
    mapping: GeometryMapping,
    resolver: &dyn Fn(&str) -> Option<DataSection>,
) -> (Vec<String>, Vec<String>, ItemLedger) {
    let candidates: Vec<String> = mapping.gather_chunks(root).into_iter().collect();
    let mut space = ChunkSpace::new(1, vec![mapping]);
    let bound = space.load_and_bind(&candidates, resolver);
    assert_eq!(bound.len(), (GRID * GRID) as usize, "every cell bound");
    let (_bound, items) = ledger(&space);

    let mut focused = Vec::new();
    let mut owners = Vec::new();
    for gz in 0..GRID {
        for gx in 0..GRID {
            let id = identifier(gx, gz);
            space.focus_chunk(&id);
            focused.push(id.clone());
            let point = Vector3::new(
                gx as f32 * CELL + CELL / 2.0,
                0.0,
                gz as f32 * CELL + CELL / 2.0,
            );
            let owner = space
                .find_chunk_from_point(&point)
                .expect("the focussed point resolves to a chunk")
                .identifier
                .clone();
            owners.push(owner);
        }
    }
    let _ = focused;
    (bound, owners, items)
}

/// The structural ledger one loaded space exposes: bind order, per-chunk
/// item tags/resources, and the query answers.
fn ledger(space: &ChunkSpace) -> (Vec<String>, ItemLedger) {
    let bound = space.bind_order().to_vec();
    let items = space
        .chunks()
        .map(|chunk: &Chunk| {
            (
                chunk.identifier.clone(),
                chunk.items.iter().map(|i| i.tag.clone()).collect(),
                chunk
                    .items
                    .iter()
                    .filter_map(|i| i.resource.clone())
                    .collect(),
            )
        })
        .collect();
    (bound, items)
}

#[test]
fn the_section_cache_cuts_the_editor_map_wait_without_changing_the_map() {
    let dir = TempDir::new("wait");
    let root = plant_space(dir.path());

    // --- the baseline: the same edit-reopen gesture twice, both raw.
    // Every reopen re-reads and re-parses all 144 files from disk.
    let mut raw_ledgers = Vec::new();
    let mut raw_ops = Vec::new();
    for _ in 0..2 {
        let started = Instant::now();
        let (result, allocs) =
            tracked(|| editor_open(&root, load_mapping(&root), &raw_resolver(root.clone())));
        let reopen_raw_ns = started.elapsed().as_nanos();
        let (bound, owners, items) = result;
        assert_eq!(bound.len(), (GRID * GRID) as usize, "raw pass binds");
        assert_eq!(owners.len(), (GRID * GRID) as usize, "raw pass queries");
        raw_ledgers.push(items);
        raw_ops.push((bound, owners));
        println!("MAP-EDIT channel=raw reopen_ns={reopen_raw_ns} allocs={allocs}");
    }
    assert_eq!(raw_ops[0], raw_ops[1], "raw reopens rerun identically");

    // --- the optimized face: one shared shipped LRU. The first open
    // fills it; both edit-reopens hit it. The map and the ledger must
    // match the baseline exactly.
    let (resource, resolver) = cached_resolver(&root);
    let mut cached_ledgers = Vec::new();
    let mut cached_ops = Vec::new();
    for leg in 0..2 {
        let started = Instant::now();
        let (result, allocs) = tracked(|| editor_open(&root, load_mapping(&root), &resolver));
        let reopen_cache_ns = started.elapsed().as_nanos();
        let (bound, owners, items) = result;
        cached_ledgers.push(items);
        cached_ops.push((bound, owners));
        println!("MAP-EDIT channel=cached reopen_ns={reopen_cache_ns} allocs={allocs}");
        let (hits, misses, bytes) = resource.section_cache_stats();
        println!("MAP-EDIT cache after leg {leg}: hits={hits} misses={misses} bytes={bytes}");
    }
    assert_eq!(
        cached_ops[0], cached_ops[1],
        "cached reopens rerun identically"
    );
    assert_eq!(
        cached_ops[0], raw_ops[0],
        "the cached and raw maps bind and query identically"
    );
    assert_eq!(
        cached_ledgers[0], raw_ledgers[0],
        "the cached and raw maps carry the same chunks and items"
    );
    // the shipped LRU holds the whole working set, so the walk is one
    // miss per file and both reopen legs are full hits; the cache stays
    // inside the shipped budget.
    let (hits, misses, bytes) = resource.section_cache_stats();
    assert_eq!(
        misses,
        (GRID * GRID) as usize,
        "exactly one miss per file across the three passes"
    );
    assert_eq!(
        hits,
        (GRID * GRID) as usize,
        "the reopen leg answers every file from cache"
    );
    assert!(
        bytes <= resources::data_section_cache::DEFAULT_CACHE_SIZE,
        "the working set fits the shipped budget"
    );

    // the cached parse carries the same tree as the raw parse for
    // every file (BwResource roots the tree at the leaf file name, the
    // raw datasection::parse at the XML tag — normalise the root name
    // and compare the serialised trees)
    let raw = raw_resolver(root.clone());
    let (_stats_resource, cached) = cached_resolver(&root);
    for gz in 0..GRID {
        for gx in 0..GRID {
            let file = format!("{}.chunk", identifier(gx, gz));
            let mut a = raw(&file).expect("raw parse");
            let mut b = cached(&file).expect("cached parse");
            a.set_section_name(b.section_name());
            assert_eq!(
                a.write_to_string(),
                b.write_to_string(),
                "{file}: the section cache answers the same tree"
            );
        }
    }

    println!(
        "MAP-EDIT cells={} cache_hits={hits} cache_misses={misses} cache_bytes={bytes}",
        GRID * GRID,
    );
}

#[test]
fn the_editor_map_wait_reruns_to_the_same_ledger() {
    let dir = TempDir::new("rerun");
    let root = plant_space(dir.path());
    let (_resource, resolver) = cached_resolver(&root);
    let mut ledgers = Vec::new();
    for _ in 0..2 {
        let (bound, owners, _items) = editor_open(&root, load_mapping(&root), &resolver);
        ledgers.push((bound, owners));
    }
    assert_eq!(ledgers[0], ledgers[1], "the large space reruns identically");
    println!(
        "MAP-EDIT-RERUN cells={} bind_equal=true owners_equal=true",
        GRID * GRID
    );
}
