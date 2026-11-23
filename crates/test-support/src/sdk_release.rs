//! BW-21-06-02: the SDK release — the ABI and one package per engine, staged
//! into an install layout and then imported the way a host would.
//!
//! The lane delivers the SDK offline (the reference's online client-download
//! push is deferred — see the release note in the todo): the release is the
//! ABI header plus one package per engine.  Each package's acceptance is
//! "after install, it imports": the ABI header real-compiles under the C
//! toolchains (Linux `cc`, Windows MinGW cross `cc`), the Unity package's
//! binding real-compiles under dotnet and its manifest is well-formed JSON,
//! the Unreal adapter real-compiles under the C++ toolchain, and the Godot
//! package records a named gap (its import needs the engine, absent on a
//! bare runner).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The install root's include directory, where the ABI header lands.
pub const ABI_INCLUDE_DIR: &str = "include";

/// The install root's package directory, one subdirectory per engine.
pub const PACKAGE_DIR: &str = "packages";

/// One package of the release: the engine, its version, its files as
/// `(install path, source name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasePackage {
    /// The engine, or `"abi"` for the shared C header.
    pub engine: &'static str,
    /// The package's version.
    pub version: String,
    /// The package's files, as `(install path, source name)`.
    pub files: Vec<(String, String)>,
}

/// The release inventory: the ABI plus one package per engine, in order.
pub fn release_packages() -> Vec<ReleasePackage> {
    let abi = crate::sdk_header::version();
    let mut packages = vec![ReleasePackage {
        engine: "abi",
        version: format!("{}.{}", abi.major, abi.minor),
        files: vec![(
            format!("{ABI_INCLUDE_DIR}/{}", crate::sdk_header::HEADER_FILE),
            crate::sdk_header::HEADER_FILE.to_string(),
        )],
    }];

    // The Unity package: the UPM manifest plus the binding under Runtime/.
    packages.push(ReleasePackage {
        engine: "unity",
        version: crate::unity_sdk::PACKAGE_VERSION.to_string(),
        files: std::iter::once(("package.json".to_string(), "package.json".to_string()))
            .chain(crate::unity_sdk::PACKAGE_FILES.iter().map(|file| {
                (
                    file.to_string(),
                    file.rsplit('/').next().unwrap().to_string(),
                )
            }))
            .collect(),
    });

    // The Unreal package: the engine-independent adapter and the example.
    packages.push(ReleasePackage {
        engine: "unreal",
        version: "5.4".to_string(),
        files: crate::unreal_sdk::DELIVERED_FILES
            .iter()
            .map(|file| (file.to_string(), file.to_string()))
            .collect(),
    });

    // The Godot package: the GDExtension descriptor, config and scripts.
    packages.push(ReleasePackage {
        engine: "godot",
        version: crate::godot_sdk::COMPATIBILITY_MINIMUM.to_string(),
        files: crate::godot_sdk::DELIVERED_FILES
            .iter()
            .map(|file| (file.to_string(), file.to_string()))
            .collect(),
    });

    packages
}

/// The versioned release manifest, a plain-text inventory.
pub fn render_release_manifest() -> String {
    let mut out = String::new();
    out.push_str("boundless SDK release\n");
    out.push_str("=====================\n");
    for package in release_packages() {
        out.push_str(&format!("- {} {}\n", package.engine, package.version));
        for (path, _) in &package.files {
            out.push_str(&format!("  {path}\n"));
        }
    }
    out
}

/// The runner's C compiler, or `None` when none is present.
pub fn cc_compiler() -> Option<&'static str> {
    ["cc", "gcc", "clang"].iter().copied().find(|name| {
        Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    })
}

/// The runner's Windows C cross-compiler, or `None` when none is present.
pub fn mingw_cc_compiler() -> Option<&'static str> {
    ["x86_64-w64-mingw32-gcc", "x86_64-w64-mingw32-cc"]
        .iter()
        .copied()
        .find(|name| {
            Command::new(name)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
}

/// The staged install root: ABI header plus every engine package, under a
/// throwaway directory.  `None` when any stage step fails.
pub fn stage_release() -> Option<PathBuf> {
    let root = std::env::temp_dir().join(format!(
        "bw21-sdk-release-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    if std::fs::create_dir_all(&root).is_err() {
        return None;
    }

    for package in release_packages() {
        let base = if package.engine == "abi" {
            root.clone()
        } else {
            root.join(PACKAGE_DIR).join(package.engine)
        };
        for (install_path, source) in &package.files {
            let dest = base.join(install_path);
            if let Some(parent) = dest.parent()
                && std::fs::create_dir_all(parent).is_err()
            {
                return None;
            }
            // The ABI header and the UPM manifest are rendered; every other
            // source is copied from the engine's case directory.
            let content = if package.engine == "abi" {
                crate::sdk_header::render_header(crate::sdk_header::version())
            } else if package.engine == "unity" && install_path == "package.json" {
                crate::unity_sdk::package_manifest()
            } else {
                match std::fs::read_to_string(fixture_dir(package.engine).join(source)) {
                    Ok(text) => text,
                    Err(_) => return None,
                }
            };
            if std::fs::write(&dest, content).is_err() {
                return None;
            }
        }
    }
    Some(root)
}

/// The case fixture directory for an engine package (`<engine>_sdk`).
fn fixture_dir(engine: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases")
        .join(format!("{engine}_sdk"))
}

/// Compile a C consumer that `#include`s the installed ABI header.
/// `None` when the compiler is absent.
pub fn compile_abi_consumer(compiler: &str, root: &Path) -> Option<bool> {
    let build = std::env::temp_dir().join(format!(
        "bw21-sdk-abi-import-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    if std::fs::create_dir_all(&build).is_err() {
        return None;
    }
    // The import: a host TU that includes the header and calls the ABI.
    let consumer = r#"
#include "bw_sdk.h"
int main(void) {
    uint32_t major = 0, minor = 0;
    if (bw_sdk_abi_version(&major, &minor) != BW_SDK_OK) return 1;
    if (bw_sdk_abi_negotiate(major, minor) != BW_SDK_OK) return 2;
    bw_handle_t handle = bw_sdk_open();
    if (bw_sdk_release(handle) != BW_SDK_OK) return 3;
    const char *text = bw_sdk_error_string(BW_SDK_OK);
    return (text == 0 || text[0] == 0) ? 4 : 0;
}
"#;
    let source = build.join("abi_import.c");
    if std::fs::write(&source, consumer).is_err() {
        return None;
    }
    // The SDK's functions are provided by the server-side runtime, resolved
    // at link time; the host-side import is the TU compiling against the
    // installed header, so the proof is compile-only (`-c`).
    let out = Command::new(compiler)
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-I")
        .arg(root.join(ABI_INCLUDE_DIR))
        .arg("-c")
        .arg("-o")
        .arg(build.join("abi_import.o"))
        .arg(&source)
        .output();
    let _ = std::fs::remove_dir_all(&build);
    out.ok().map(|o| o.status.success())
}

/// How one package's install-import went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// The package's engine (or `"abi"`).
    pub engine: &'static str,
    /// The package's version.
    pub version: String,
    /// Whether the package imported after install.
    pub imported: bool,
    /// The proof (a compiler name) or the named gap reason.
    pub via: String,
}

/// Stage the release and import each package the way a host would.
pub fn install_and_import() -> Vec<InstallOutcome> {
    let mut outcomes = Vec::new();
    let Some(root) = stage_release() else {
        for package in release_packages() {
            outcomes.push(InstallOutcome {
                engine: package.engine,
                version: package.version,
                imported: false,
                via: "the release did not stage".to_string(),
            });
        }
        return outcomes;
    };

    for package in release_packages() {
        let (imported, via) = match package.engine {
            // The ABI header imports on Linux and Windows (MinGW cross).
            "abi" => {
                let linux = cc_compiler().and_then(|cc| compile_abi_consumer(cc, &root));
                let windows = mingw_cc_compiler().and_then(|cc| compile_abi_consumer(cc, &root));
                match (linux, windows) {
                    (Some(true), Some(true)) => {
                        (true, "cc (linux) + mingw cc (windows cross)".to_string())
                    }
                    (Some(true), Some(false)) => (
                        false,
                        "the ABI consumer did not cross-compile for windows".to_string(),
                    ),
                    (Some(true), None) => (true, "cc (linux)".to_string()),
                    (Some(false), _) => (false, "the ABI consumer did not compile".to_string()),
                    (None, _) => (false, "no C compiler on the runner".to_string()),
                }
            }
            // The Unity binding imports under dotnet, and the manifest is
            // well-formed JSON.
            "unity" => {
                let json_ok =
                    crate::unity_sdk::is_well_formed_json(&crate::unity_sdk::package_manifest());
                match (crate::unity_sdk::compile_binding(), json_ok) {
                    (Some(build), true) if build.success => {
                        (true, "dotnet (binding compiled; manifest JSON)".to_string())
                    }
                    (Some(_), true) => (false, "the C# binding did not compile".to_string()),
                    _ => (
                        false,
                        "the package manifest is not well-formed JSON".to_string(),
                    ),
                }
            }
            // The Unreal adapter imports under the C++ toolchain.
            "unreal" => match crate::sdk_matrix::cpp_compiler()
                .and_then(crate::sdk_matrix::compile_adapter)
            {
                Some(true) => (true, "g++ (adapter compiled)".to_string()),
                Some(false) => (false, "the C++ adapter did not compile".to_string()),
                None => (false, "no C++ compiler on the runner".to_string()),
            },
            // The Godot native needs godot-cpp, absent on a bare runner.
            "godot" => (
                false,
                "godot-cpp is absent (the GDExtension import needs the engine)".to_string(),
            ),
            _ => (false, "unsupported package".to_string()),
        };
        outcomes.push(InstallOutcome {
            engine: package.engine,
            version: package.version,
            imported,
            via,
        });
    }

    let _ = std::fs::remove_dir_all(&root);
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_names_the_abi_and_every_engine() {
        let packages = release_packages();
        let names: Vec<_> = packages.iter().map(|package| package.engine).collect();
        assert_eq!(names, vec!["abi", "unity", "unreal", "godot"]);
        for package in &packages {
            assert!(!package.version.is_empty());
            assert!(!package.files.is_empty());
        }
    }

    #[test]
    fn the_abi_version_is_the_rendered_one() {
        // The ABI package's version is the model's version, and its single
        // file is the header under the include directory.
        let abi = release_packages().remove(0);
        assert_eq!(abi.engine, "abi");
        let version = crate::sdk_header::version();
        assert_eq!(abi.version, format!("{}.{}", version.major, version.minor));
        assert_eq!(
            abi.files,
            vec![(
                format!("{ABI_INCLUDE_DIR}/{}", crate::sdk_header::HEADER_FILE),
                crate::sdk_header::HEADER_FILE.to_string(),
            )],
        );
    }

    #[test]
    fn the_unity_package_matches_the_delivered_manifest() {
        let unity = release_packages()
            .into_iter()
            .find(|package| package.engine == "unity")
            .unwrap();
        assert_eq!(unity.version, crate::unity_sdk::PACKAGE_VERSION);
        // The manifest and the binding under Runtime/ are the package.
        assert!(unity.files.iter().any(|(path, _)| path == "package.json"));
        assert!(
            unity
                .files
                .iter()
                .any(|(path, _)| path == "Runtime/BwUnity.cs")
        );
    }

    #[test]
    fn the_manifest_is_versioned_and_complete() {
        let manifest = render_release_manifest();
        assert!(manifest.contains("boundless SDK release"));
        assert!(manifest.contains("- abi 1.0"));
        assert!(manifest.contains("- unity 0.1.0"));
        assert!(manifest.contains("- unreal 5.4"));
        assert!(manifest.contains("- godot 4.3"));
        assert!(manifest.contains("include/bw_sdk.h"));
        assert!(manifest.contains("package.json"));
    }

    #[test]
    fn the_release_stages_every_file() {
        let Some(root) = stage_release() else {
            panic!("the release did not stage");
        };
        let header = root
            .join(ABI_INCLUDE_DIR)
            .join(crate::sdk_header::HEADER_FILE);
        assert!(header.is_file(), "the ABI header is not installed");
        for package in release_packages()
            .into_iter()
            .filter(|package| package.engine != "abi")
        {
            for (path, _) in &package.files {
                let installed = root.join(PACKAGE_DIR).join(package.engine).join(path);
                assert!(
                    installed.is_file(),
                    "{} {} is not installed",
                    package.engine,
                    path,
                );
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_package_imports_after_install() {
        // The acceptance: after install, each package imports.  The ABI, the
        // Unity binding and the Unreal adapter genuinely import (real
        // toolchains); the Godot package names its gap.
        let outcomes = install_and_import();
        assert_eq!(outcomes.len(), 4);
        let abi = outcomes
            .iter()
            .find(|outcome| outcome.engine == "abi")
            .unwrap();
        assert!(abi.imported, "the ABI did not import: {}", abi.via);
        let unity = outcomes
            .iter()
            .find(|outcome| outcome.engine == "unity")
            .unwrap();
        assert!(
            unity.imported,
            "the Unity package did not import: {}",
            unity.via
        );
        let unreal = outcomes
            .iter()
            .find(|outcome| outcome.engine == "unreal")
            .unwrap();
        assert!(
            unreal.imported,
            "the Unreal package did not import: {}",
            unreal.via
        );
        let godot = outcomes
            .iter()
            .find(|outcome| outcome.engine == "godot")
            .unwrap();
        assert!(
            !godot.imported,
            "the Godot package cannot import without godot-cpp"
        );
        assert!(
            godot.via.contains("godot-cpp"),
            "unexpected godot gap: {}",
            godot.via
        );
    }
}
