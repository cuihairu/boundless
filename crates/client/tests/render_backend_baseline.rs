//! OPT-15-01-02: 采集各图形后端性能 — per-graphics-backend performance
//! collection: the device record, the driver record and the frame times.
//!
//! The reference client's device is an `IDirect3DDevice9`
//! (`render_context.cpp:1164`); its four reference-side backends (D3D9,
//! navgen OpenGL, cocos2d, Awesomium) are recorded as facts in
//! `docs/review/bw15-06-01-graphics-backend-list.md`. This tree ships
//! exactly one pixel-producing backend today — the deterministic software
//! rasterizer behind [`client::device::Device`] — so the collection is a
//! registry keyed by backend: every shipped backend answers the same three
//! questions, and a backend BW-11 lands later slots into the same report.
//!
//! - **Device** (structural, asserted): the device type, the presentation
//!   parameters (640×480, adapter 0), the frame order, the operational
//!   status, the leak conservation and digest reproducibility from the
//!   frame index alone — plus agreement with the saved OPT-11-01-01
//!   fixed-camera pins, so the backend record and the end baseline can
//!   never drift apart.
//! - **Driver** (probe data, never asserted): a software rasterizer's
//!   "driver" is the execution environment — CPU model, logical cores,
//!   OS, kernel — recorded as text in the report.
//! - **Frame time** (probe data, never asserted): per-frame wall clock
//!   around `check_device` → `step` → `present` over the fixed camera,
//!   printed per frame and summarised min/median/p99/max into the report.
//!
//! The report (TSV, one row per platform × backend) is written to
//! `RENDER_BACKEND_OUT`; the committed record
//! (`tests/baseline/render-backend-baseline.tsv`) carries this host's
//! measured row plus pending rows naming the BW-15-03/BW-15-04 blockers
//! for the platforms that cannot run the workspace yet; the grammar of
//! that record is checked on every run.
//!
//! ```sh
//! cargo test -p client --test render_backend_baseline
//! ```

use std::time::Instant;

use client::device::{Device, DeviceConfig, DeviceStatus};
use client::render::scene::TestScene;

/// Frames in one backend's fixed-camera walk (the OPT-11-01-01 shape).
const FRAMES: u32 = 64;

/// The saved fixed-camera digest pins from OPT-11-01-01
/// (`render_baseline_opt.rs`): frames 1..=8 and 64. A backend's frames
/// must reproduce them — the backend record and the end baseline agree.
const BASELINE_DIGESTS: [u64; 9] = [
    0x69cb_1245_6d1a_e325,
    0x228c_7e64_89e2_e325,
    0xa645_7f3f_bc5c_6325,
    0x4ea0_a845_350b_6325,
    0x6e6b_e175_60a8_e325,
    0x0842_51ac_435d_e325,
    0x2e95_89ed_046c_6325,
    0x93cf_bf1f_9eb1_e325,
    0x6e8d_a4fa_c38f_6325, // frame 64
];

/// One shipped graphics backend in the collection registry.
struct BackendSpec {
    /// The registry id the report keys rows by.
    id: &'static str,
    /// The device type behind the backend's faces.
    device: &'static str,
}

/// The shipped backends. The reference's D3D9 / OpenGL / cocos2d /
/// Awesomium backends are not ports (BW-15-06-01); when a new backend
/// lands it joins this table and the grammar's coverage check pulls its
/// row into the record.
const BACKENDS: &[BackendSpec] = &[BackendSpec {
    id: "software",
    device: "SoftwareDevice",
}];

/// One frame's record: the presented digest and its wall clock.
struct FrameRecord {
    index: u64,
    checksum: u64,
    ns: u128,
}

/// One backend's fixed-camera walk: the device created once, then FRAMES
/// frames of `check_device` → `step` → `present`, each with a wall clock
/// around it. Identical path for every backend — that is what makes the
/// per-backend numbers comparable.
fn backend_walk() -> (Device, Vec<FrameRecord>) {
    let config = DeviceConfig::default();
    let mut device = Device::create(config);
    let mut scene = TestScene::new();

    let mut records = Vec::with_capacity(FRAMES as usize);
    for _ in 0..FRAMES {
        assert!(
            device.check_device(),
            "the fixed-camera walk keeps the device operational"
        );
        scene.step();
        let started = Instant::now();
        let presented = device.present(&scene);
        records.push(FrameRecord {
            index: presented.frame_index,
            checksum: presented.checksum,
            ns: started.elapsed().as_nanos(),
        });
    }
    (device, records)
}

/// Frame N's digest from the frame index alone: a fresh device and scene
/// stepped `steps` times, presented once — the fixed-camera anchor.
fn seek_checksum(steps: u32) -> u64 {
    let mut device = Device::create(DeviceConfig::default());
    let mut scene = TestScene::new();
    for _ in 0..steps {
        scene.step();
    }
    device.present(&scene).checksum
}

/// A percentile of a latency list, in nanoseconds.
fn percentile(samples: &[u128], pct: usize) -> u128 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[sorted.len() * pct / 100]
}

fn platform_name() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The driver record: a software rasterizer executes on the host CPU, so
/// its "driver" is the execution environment — CPU model, logical cores,
/// OS and kernel — recorded as text, never asserted.
fn driver_record() -> String {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let mut driver = format!(
        "cpu_cores={cores} os={} arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    #[cfg(target_os = "linux")]
    {
        if let Some(model) = first_cpu_model() {
            driver.push_str(&format!(" cpu_model={model}"));
        }
        if let Ok(kernel) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            driver.push_str(&format!(" kernel={}", kernel.trim()));
        }
    }
    driver
}

/// The first `model name` in /proc/cpuinfo, spaces kept.
#[cfg(target_os = "linux")]
fn first_cpu_model() -> Option<String> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo
        .lines()
        .find_map(|line| line.strip_prefix("model name"))?
        .strip_prefix("\t: ")
        .map(str::to_string)
}

/// Report fields must not carry the row separator.
fn tsv_escape(text: &str) -> String {
    text.replace(['\t', '\n'], " ")
}

// --- the committed platform × backend record ---------------------------------

const CATALOG_HEADER: &str =
    "platform\tbackend\tdevice\tdriver\tframes\tmin_ns\tmedian_ns\tp99_ns\tmax_ns\tnote";
const MEASURED_FIELDS: usize = 10;

/// The committed record at `tests/baseline/render-backend-baseline.tsv`:
/// the measuring host's row per shipped backend plus one pending row per
/// platform that cannot run the workspace yet (named BW-15 blocker, no
/// invented figures). Grammar only: numbers are never re-verified on
/// another host.
fn check_catalog() {
    let catalog = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/baseline/render-backend-baseline.tsv");
    let text = std::fs::read_to_string(&catalog)
        .unwrap_or_else(|e| panic!("the committed catalog is readable at {catalog:?}: {e}"));
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some(CATALOG_HEADER),
        "the catalog header pins the grammar"
    );
    let mut rows = std::collections::BTreeSet::new();
    for (index, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            fields.len(),
            MEASURED_FIELDS,
            "catalog line {} carries {} fields: {line:?}",
            index + 2,
            MEASURED_FIELDS
        );
        let (platform, backend) = (fields[0], fields[1]);
        assert!(
            rows.insert((platform, backend)),
            "platform {platform} backend {backend} appears twice"
        );
        if fields[2] == "-" {
            assert!(
                fields[2..9].iter().all(|f| *f == "-"),
                "a pending row carries no measurements: {platform} {backend}"
            );
            assert!(
                fields[9].starts_with("pending: BW-15-0"),
                "a pending row names its BW-15 blocker: {platform} {backend}"
            );
        } else {
            assert!(
                BACKENDS.iter().any(|b| b.id == backend),
                "unknown backend {backend} for {platform}"
            );
            assert!(
                fields[4].parse::<u64>().is_ok(),
                "{platform} {backend} frames is numeric: {:?}",
                fields[4]
            );
            for (field, name) in fields[5..9].iter().zip(["min", "median", "p99", "max"]) {
                assert!(
                    field.parse::<u128>().is_ok(),
                    "{platform} {backend} {name}_ns is numeric: {field:?}"
                );
            }
        }
    }
    // A pending platform is one summary row; the measuring platform covers
    // every shipped backend exactly once.
    for backend in BACKENDS {
        assert!(
            rows.contains(&(platform_name().as_str(), backend.id)),
            "the measuring platform {} has a {} row",
            platform_name(),
            backend.id
        );
    }
}

fn write_report(rows: &[String]) {
    let Some(path) = std::env::var_os("RENDER_BACKEND_OUT") else {
        return;
    };
    let mut text = String::from(CATALOG_HEADER);
    text.push('\n');
    for row in rows {
        text.push_str(row);
        text.push('\n');
    }
    std::fs::write(&path, text).expect("report written");
    eprintln!(
        "RENDER-BACKEND report written to {}",
        std::path::Path::new(&path).display()
    );
}

#[test]
fn the_shipped_backends_record_device_driver_and_frame_times() {
    let config = DeviceConfig::default();
    let device_record = format!(
        "{} {}x{} adapter={} ({} px rgba8)",
        "SoftwareDevice",
        config.width,
        config.height,
        config.adapter,
        config.area()
    );
    let driver = driver_record();

    let mut rows = Vec::with_capacity(BACKENDS.len());
    for backend in BACKENDS {
        let (device, records) = backend_walk();

        // the device record: every frame presented, in order, on one
        // device object that stayed operational and leaked nothing
        assert_eq!(
            records.len(),
            FRAMES as usize,
            "{}: every frame ran",
            backend.id
        );
        for (index, record) in records.iter().enumerate() {
            assert_eq!(
                record.index,
                index as u64 + 1,
                "{}: the frame order",
                backend.id
            );
        }
        assert_eq!(
            device.frame_count(),
            FRAMES as u64,
            "{}: frames counted",
            backend.id
        );
        assert_eq!(
            device.status(),
            DeviceStatus::Ok,
            "{}: device operational",
            backend.id
        );
        assert_eq!(device.leaked(), 0, "{}: no device leaked", backend.id);

        // the scene animates under the fixed camera: every consecutive
        // frame differs, and the walk agrees with the saved end baseline
        for pair in records.windows(2) {
            assert_ne!(
                pair[0].checksum, pair[1].checksum,
                "{}: consecutive frames differ",
                backend.id
            );
        }
        let pinned_frames: Vec<u32> = (1u32..=8).chain([FRAMES]).collect();
        for (pin, expected) in pinned_frames.iter().zip(BASELINE_DIGESTS.iter()) {
            assert_eq!(
                records[*pin as usize - 1].checksum,
                *expected,
                "{}: frame {pin} matches the saved fixed-camera pin",
                backend.id
            );
        }
        for &frame in &[1usize, 7, 31, FRAMES as usize] {
            assert_eq!(
                seek_checksum(frame as u32),
                records[frame - 1].checksum,
                "{}: frame {frame} reproduces from the index alone",
                backend.id
            );
        }

        // the frame times (probe data: printed, never asserted)
        for record in &records {
            eprintln!(
                "FRAME-TIME backend={} frame={} checksum={:016x} ns={}",
                backend.id, record.index, record.checksum, record.ns
            );
        }
        let ns: Vec<u128> = records.iter().map(|r| r.ns).collect();
        eprintln!(
            "DEVICE backend={} device={} frames={} status=Ok leaked=0",
            backend.id,
            backend.device,
            records.len()
        );
        eprintln!("DRIVER backend={} driver={driver}", backend.id);
        eprintln!(
            "FRAME-TAIL backend={} p50_us={:.3} p99_us={:.3} max_us={:.3}",
            backend.id,
            percentile(&ns, 50) as f64 / 1_000.0,
            percentile(&ns, 99) as f64 / 1_000.0,
            ns.iter().copied().max().unwrap_or(0) as f64 / 1_000.0,
        );
        rows.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\tbackend walk over the fixed camera",
            platform_name(),
            backend.id,
            tsv_escape(&device_record),
            tsv_escape(&driver),
            records.len(),
            ns.iter().copied().min().unwrap_or(0),
            percentile(&ns, 50),
            percentile(&ns, 99),
            ns.iter().copied().max().unwrap_or(0),
        ));
    }

    write_report(&rows);
    check_catalog();
}

#[test]
fn the_backend_walk_reruns_to_the_same_frames() {
    let (_, first) = backend_walk();
    let (_, second) = backend_walk();
    assert_eq!(first.len(), second.len(), "the rerun presents every frame");
    for (index, (a, b)) in first.iter().zip(second.iter()).enumerate() {
        assert_eq!(a.checksum, b.checksum, "frame {index}: the digest reruns");
        assert_eq!(a.index, b.index, "frame {index}: the order reruns");
    }

    // the saved pins: frames 1..=8 and 64 — the same anchor the end
    // baseline pins, so both records answer to one raster
    let pinned: Vec<u64> = first
        .iter()
        .take(8)
        .chain(first.last())
        .map(|r| r.checksum)
        .collect();
    assert_eq!(pinned, BASELINE_DIGESTS, "the saved digests");
    eprintln!("BACKEND-CHECKSUMS {pinned:016x?}");
}
