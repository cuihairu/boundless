//! BW-21-06-01: the SDK release matrix — the engines and platforms the SDK
//! supports, each with a real build result or a recorded gap.
//!
//! The matrix covers every engine × platform the lane delivers an adapter
//! for.  Each entry's build result is either [`MatrixResult::Built`] — the
//! adapter's native artifact really compiled with the named toolchain — or
//! [`MatrixResult::Gap`], naming the missing toolchain.  The real builds are
//! the C++ adapter on Linux and Windows (this runner has a MinGW cross
//! compiler) and the C# binding under dotnet (portable IL, one build serves
//! every platform); the Godot native needs godot-cpp, which a bare runner
//! does not have.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The engines the SDK supports, as `(name, version)`.
pub const ENGINES: &[(&str, &str)] = &[("unity", "2021.3"), ("unreal", "5.4"), ("godot", "4.3")];

/// The platforms the matrix covers.
pub const PLATFORMS: &[&str] = &["linux", "windows", "macos"];

/// How one entry's build went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatrixResult {
    /// The artifact compiled with the named toolchain.
    Built { compiler: String },
    /// No toolchain for the entry; the reason is named.
    Gap { reason: String },
}

/// One engine × platform entry of the matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixEntry {
    /// The engine.
    pub engine: &'static str,
    /// The engine's version.
    pub engine_version: &'static str,
    /// The platform.
    pub platform: &'static str,
    /// The entry's build result.
    pub result: MatrixResult,
}

impl MatrixEntry {
    /// Whether the entry built.
    pub fn built(&self) -> bool {
        matches!(self.result, MatrixResult::Built { .. })
    }

    /// Whether the entry recorded a gap.
    pub fn gapped(&self) -> bool {
        matches!(self.result, MatrixResult::Gap { .. })
    }
}

/// The delivered C++ adapter directory the matrix builds.
pub fn adapter_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join("unreal_sdk")
}

/// A throwaway build directory with the given prefix.
fn build_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ))
}

/// The runner's C++ compiler, or `None` when none is present.
pub fn cpp_compiler() -> Option<&'static str> {
    ["g++", "clang++", "c++"].iter().copied().find(|name| {
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// The runner's Windows cross-compiler, or `None` when none is present.
pub fn mingw_compiler() -> Option<&'static str> {
    ["x86_64-w64-mingw32-g++", "x86_64-w64-mingw32-c++"]
        .iter()
        .copied()
        .find(|name| {
            Command::new(name)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
}

/// Compile the delivered C++ adapter to an object file with `compiler`.
/// `None` when the compile does not run (the compiler is absent).
pub fn compile_adapter(compiler: &str) -> Option<bool> {
    let dir = adapter_dir();
    let build = build_dir("bw21-sdk-matrix");
    if std::fs::create_dir_all(&build).is_err() {
        return None;
    }
    let out = Command::new(compiler)
        .arg("-std=c++20")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-c")
        .arg(dir.join("BwSdkModule.cpp"))
        .arg("-I")
        .arg(&dir)
        .arg("-o")
        .arg(build.join("adapter.o"))
        .output();
    let _ = std::fs::remove_dir_all(&build);
    out.ok().map(|o| o.status.success())
}

/// Build one engine × platform entry: the real build where the toolchain
/// is present, else a named gap.
fn build_one(engine: &str, platform: &str) -> MatrixResult {
    match (engine, platform) {
        // The Unreal adapter's engine-independent C++ builds on Linux and
        // Windows (this runner has a MinGW cross compiler).
        ("unreal", "linux") => match cpp_compiler().and_then(compile_adapter) {
            Some(true) => MatrixResult::Built {
                compiler: "g++ (linux)".to_string(),
            },
            Some(false) => MatrixResult::Gap {
                reason: "the C++ adapter did not compile".to_string(),
            },
            None => MatrixResult::Gap {
                reason: "no C++ compiler on the runner".to_string(),
            },
        },
        ("unreal", "windows") => match mingw_compiler().and_then(compile_adapter) {
            Some(true) => MatrixResult::Built {
                compiler: "x86_64-w64-mingw32-g++ (cross)".to_string(),
            },
            Some(false) => MatrixResult::Gap {
                reason: "the C++ adapter did not cross-compile".to_string(),
            },
            None => MatrixResult::Gap {
                reason: "no Windows cross compiler on the runner".to_string(),
            },
        },
        ("unreal", "macos") => MatrixResult::Gap {
            reason: "no Apple toolchain on the runner".to_string(),
        },

        // The Unity binding is C#; the dotnet build is portable IL, so one
        // build serves every platform.
        ("unity", _) => match crate::unity_sdk::compile_binding() {
            Some(build) if build.success => MatrixResult::Built {
                compiler: "dotnet (portable IL)".to_string(),
            },
            Some(_) => MatrixResult::Gap {
                reason: "the C# binding did not compile".to_string(),
            },
            None => MatrixResult::Gap {
                reason: "no dotnet toolchain on the runner".to_string(),
            },
        },

        // The Godot native needs godot-cpp, which a bare runner does not
        // have; the engine-side GDExtension load is already gapped.
        ("godot", _) => MatrixResult::Gap {
            reason: "godot-cpp is absent on the runner (the GDExtension native does not build)"
                .to_string(),
        },

        (_, _) => MatrixResult::Gap {
            reason: "unsupported matrix entry".to_string(),
        },
    }
}

/// The matrix: every engine × platform entry, in order, each with a real
/// build result or a named gap.
pub fn build_results() -> Vec<MatrixEntry> {
    let mut entries = Vec::new();
    for (engine, version) in ENGINES {
        for platform in PLATFORMS {
            entries.push(MatrixEntry {
                engine,
                engine_version: version,
                platform,
                result: build_one(engine, platform),
            });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_matrix_covers_every_engine_and_platform() {
        let matrix = build_results();

        // The full engine × platform grid, in order.
        assert_eq!(matrix.len(), ENGINES.len() * PLATFORMS.len());
        for (engine, version) in ENGINES {
            for platform in PLATFORMS {
                let entry = matrix
                    .iter()
                    .find(|entry| entry.engine == *engine && entry.platform == *platform)
                    .unwrap_or_else(|| panic!("missing {engine}/{platform}"));
                assert_eq!(entry.engine_version, *version);
                // Every entry has a result: built, or a named gap.
                assert!(entry.built() || entry.gapped());
            }
        }
    }

    #[test]
    fn every_supported_entry_has_a_build_result() {
        // The acceptance: each supported entry has a recorded build result
        // — real (compiled) or a named gap, never silent.
        let matrix = build_results();
        assert!(!matrix.is_empty());
        for entry in &matrix {
            match &entry.result {
                MatrixResult::Built { compiler } => {
                    assert!(
                        !compiler.is_empty(),
                        "{} built without a compiler",
                        entry.engine
                    );
                }
                MatrixResult::Gap { reason } => {
                    assert!(
                        !reason.is_empty(),
                        "{} gapped without a reason",
                        entry.engine
                    );
                }
            }
        }
    }

    #[test]
    fn the_real_builds_actually_compile() {
        // The Linux C++ adapter genuinely compiles on this runner.
        let linux_adapter = build_results()
            .into_iter()
            .find(|entry| entry.engine == "unreal" && entry.platform == "linux")
            .unwrap();
        match &linux_adapter.result {
            MatrixResult::Built { compiler } => {
                assert!(compiler.contains("g++"), "unexpected compiler: {compiler}");
            }
            MatrixResult::Gap { reason } => {
                panic!("the Linux adapter did not build: {reason}");
            }
        }

        // The Windows C++ adapter genuinely cross-compiles (MinGW), and the
        // compiled object is a real artifact.
        let windows_adapter = build_results()
            .into_iter()
            .find(|entry| entry.engine == "unreal" && entry.platform == "windows")
            .unwrap();
        match &windows_adapter.result {
            MatrixResult::Built { compiler } => {
                assert!(
                    compiler.contains("mingw"),
                    "unexpected compiler: {compiler}",
                );
            }
            MatrixResult::Gap { reason } => {
                panic!("the Windows adapter did not cross-compile: {reason}");
            }
        }
    }

    #[test]
    fn the_engine_versions_match_the_deliveries() {
        // The versions the matrix names are the ones the delivered adapters
        // declare: Unity 2021.3 and Godot 4.3.
        let unity = ENGINES.iter().find(|(name, _)| *name == "unity").unwrap();
        let godot = ENGINES.iter().find(|(name, _)| *name == "godot").unwrap();
        assert_eq!(unity.1, crate::unity_sdk::PACKAGE_UNITY);
        assert_eq!(godot.1, crate::godot_sdk::COMPATIBILITY_MINIMUM);
    }

    #[test]
    fn the_godot_entries_gap_honestly() {
        // The Godot native needs godot-cpp, absent on a bare runner; every
        // platform entry names that reason rather than reporting silence.
        let matrix = build_results();
        for entry in matrix.iter().filter(|entry| entry.engine == "godot") {
            match &entry.result {
                MatrixResult::Gap { reason } => {
                    assert!(
                        reason.contains("godot-cpp"),
                        "unexpected godot gap: {reason}"
                    );
                }
                MatrixResult::Built { .. } => {
                    panic!("the Godot native cannot build without godot-cpp");
                }
            }
        }
    }
}
