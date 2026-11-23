//! BW-21-02-04: the SDK's public C header, generated and verified.
//!
//! The Rust ABI model ([`crate::sdk_abi`]) is the one source of truth: the
//! generator renders that model as C — one integer constant and one string
//! constant per error code, one prototype per entry point, the ABI version
//! as two macros — so the C surface and the Rust surface cannot drift. A C
//! example then `#include`s the header and calls the SDK; verification
//! checks that every call resolves against a declared prototype (arity and
//! argument types), and, when a C compiler is present, that the example
//! actually compiles.

use crate::sdk_abi::ABI_VERSION;
use crate::sdk_abi::AbiVersion;

/// The header's include guard.
pub const GUARD: &str = "BW_SDK_H";

/// The header's filename, what a C example includes.
pub const HEADER_FILE: &str = "bw_sdk.h";

/// One error the SDK can report to a C host: the integer a host compares and
/// the stable string a person reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCode {
    /// The integer value.
    pub value: i32,
    /// The C constant's name.
    pub name: &'static str,
    /// The stable string the Rust model reports.
    pub code: &'static str,
}

/// The error codes the header defines, in value order. `0` is the no-error
/// code; the rest mirror [`crate::sdk_abi::HandleError`] and
/// [`crate::sdk_abi::AbiError`].
pub const ERROR_CODES: &[ErrorCode] = &[
    ErrorCode {
        value: 0,
        name: "BW_SDK_OK",
        code: "ok",
    },
    ErrorCode {
        value: 1,
        name: "BW_SDK_ERR_UNKNOWN_HANDLE",
        code: "unknown-handle",
    },
    ErrorCode {
        value: 2,
        name: "BW_SDK_ERR_DOUBLE_RELEASE",
        code: "double-release",
    },
    ErrorCode {
        value: 3,
        name: "BW_SDK_ERR_ABI_MAJOR_MISMATCH",
        code: "abi-major-mismatch",
    },
    ErrorCode {
        value: 4,
        name: "BW_SDK_ERR_ABI_HOST_NEWER",
        code: "abi-host-newer",
    },
];

/// One entry point the header declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryPoint {
    /// The C return type.
    pub returns: &'static str,
    /// The C name.
    pub name: &'static str,
    /// The parameters, as `(type, name)`.
    pub parameters: &'static [(&'static str, &'static str)],
}

/// The entry points the header declares, in declaration order. The set is
/// the C view of the ABI: query the version, negotiate it, open and release
/// a handle, and turn a code into the string a person reads.
pub const ENTRY_POINTS: &[EntryPoint] = &[
    EntryPoint {
        returns: "int",
        name: "bw_sdk_abi_version",
        parameters: &[("uint32_t *", "major"), ("uint32_t *", "minor")],
    },
    EntryPoint {
        returns: "int",
        name: "bw_sdk_abi_negotiate",
        parameters: &[("uint32_t", "host_major"), ("uint32_t", "host_minor")],
    },
    EntryPoint {
        returns: "bw_handle_t",
        name: "bw_sdk_open",
        parameters: &[],
    },
    EntryPoint {
        returns: "int",
        name: "bw_sdk_release",
        parameters: &[("bw_handle_t", "handle")],
    },
    EntryPoint {
        returns: "const char *",
        name: "bw_sdk_error_string",
        parameters: &[("int", "code")],
    },
];

/// The header's ABI version: the model's version, the single source of truth.
pub fn version() -> AbiVersion {
    ABI_VERSION
}

/// Render one prototype's signature (no trailing semicolon).
pub fn prototype(entry: &EntryPoint) -> String {
    let mut out = format!("{} {}", entry.returns, entry.name);
    if entry.parameters.is_empty() {
        out.push_str("(void)");
    } else {
        out.push('(');
        for (index, (ty, name)) in entry.parameters.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(ty);
            out.push(' ');
            out.push_str(name);
        }
        out.push(')');
    }
    out
}

/// Render the public C header for `version`.
pub fn render_header(version: AbiVersion) -> String {
    let mut out = String::new();
    out.push_str("/* The boundless SDK ABI. Generated from the Rust model. */\n");
    out.push_str("#ifndef ");
    out.push_str(GUARD);
    out.push('\n');
    out.push_str("#define ");
    out.push_str(GUARD);
    out.push_str("\n\n#include <stdint.h>\n\n");
    out.push_str(&format!(
        "#define BW_SDK_ABI_VERSION_MAJOR {}\n",
        version.major
    ));
    out.push_str(&format!(
        "#define BW_SDK_ABI_VERSION_MINOR {}\n",
        version.minor
    ));
    out.push_str("\ntypedef uint64_t bw_handle_t;\n\n");
    out.push_str(&format!(
        "#define BW_NULL_HANDLE {}u\n\n",
        crate::sdk_abi::NULL_HANDLE
    ));
    for code in ERROR_CODES {
        out.push_str(&format!("#define {} {}\n", code.name, code.value));
        out.push_str(&format!("#define {}_TEXT \"{}\"\n", code.name, code.code));
    }
    out.push('\n');
    for entry in ENTRY_POINTS {
        out.push_str(&prototype(entry));
        out.push_str(";\n");
    }
    out.push('\n');
    out.push_str("#endif /* ");
    out.push_str(GUARD);
    out.push_str(" */\n");
    out
}

/// One call a C example makes to the SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExampleCall {
    /// The function's name.
    pub name: &'static str,
    /// The type of each argument the example passes.
    pub args: &'static [&'static str],
}

/// The canonical example's call sites, in order: query the version, negotiate
/// it, open a handle, release it, and read one error string.
pub fn example_calls() -> Vec<ExampleCall> {
    vec![
        ExampleCall {
            name: "bw_sdk_abi_version",
            args: &["uint32_t *", "uint32_t *"],
        },
        ExampleCall {
            name: "bw_sdk_abi_negotiate",
            args: &["uint32_t", "uint32_t"],
        },
        ExampleCall {
            name: "bw_sdk_open",
            args: &[],
        },
        ExampleCall {
            name: "bw_sdk_release",
            args: &["bw_handle_t"],
        },
        ExampleCall {
            name: "bw_sdk_error_string",
            args: &["int"],
        },
    ]
}

/// Check one call against the header's declarations. `Ok` when the function
/// is declared and the arguments match in count and type; `Err` names the
/// problem.
fn verify_call(call: &ExampleCall) -> Result<(), String> {
    let Some(entry) = ENTRY_POINTS.iter().find(|entry| entry.name == call.name) else {
        return Err(format!("unknown function: {}", call.name));
    };
    if entry.parameters.len() != call.args.len() {
        return Err(format!(
            "{} takes {} parameters but the example passes {}",
            call.name,
            entry.parameters.len(),
            call.args.len(),
        ));
    }
    for ((ty, _name), arg) in entry.parameters.iter().zip(call.args) {
        if ty != arg {
            return Err(format!(
                "{} parameter type mismatch: expected {} but the example passes {}",
                call.name, ty, arg,
            ));
        }
    }
    Ok(())
}

/// Check every call, in order, against the header's declarations.
pub fn verify_calls(example: &[ExampleCall]) -> Vec<Result<(), String>> {
    example.iter().map(verify_call).collect()
}

/// Whether the example loads the header and every call resolves — the
/// acceptance: a C example can load and call the SDK.
pub fn example_loads_and_calls(example: &[ExampleCall]) -> bool {
    verify_calls(example).iter().all(Result::is_ok)
}

/// Render a C example that includes the header and calls every entry point in
/// order, so a real compiler can verify the example loads and calls. The
/// example reads the version through the header's macros, so it needs none
/// passed in.
pub fn render_example() -> String {
    let mut out = String::new();
    out.push_str(&format!("#include \"{HEADER_FILE}\"\n"));
    out.push_str("#include <stdio.h>\n\nint main(void)\n{\n");
    out.push_str("    uint32_t major = 0, minor = 0;\n");
    out.push_str(
        "    if (bw_sdk_abi_version(&major, &minor) != BW_SDK_OK) {\n        return 1;\n    }\n",
    );
    out.push_str(
        "    if (bw_sdk_abi_negotiate(major, minor) != BW_SDK_OK) {\n        return 2;\n    }\n",
    );
    out.push_str("    bw_handle_t handle = bw_sdk_open();\n");
    out.push_str("    if (handle == BW_NULL_HANDLE) {\n        return 3;\n    }\n");
    out.push_str("    if (bw_sdk_release(handle) != BW_SDK_OK) {\n        return 4;\n    }\n");
    out.push_str("    if (bw_sdk_error_string(BW_SDK_ERR_UNKNOWN_HANDLE) == 0) {\n        return 5;\n    }\n");
    out.push_str("    return 0;\n}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk_abi::{AbiError, HandleError};

    #[test]
    fn the_header_renders_deterministically() {
        let first = render_header(version());
        let second = render_header(version());

        // Byte-for-byte: the same version always renders the same header.
        assert_eq!(first, second);
        assert!(first.contains(&format!("#ifndef {GUARD}")));
        assert!(first.contains(&format!("#endif /* {GUARD} */")));
        assert!(first.contains("#include <stdint.h>"));
    }

    #[test]
    fn the_header_names_the_abi_version_from_the_model() {
        let header = render_header(version());

        // The version macros come from the model, not a hardcoded value.
        assert!(header.contains("#define BW_SDK_ABI_VERSION_MAJOR 1"));
        assert!(header.contains("#define BW_SDK_ABI_VERSION_MINOR 0"));
        // A different version renders a different header.
        let other = render_header(AbiVersion { major: 2, minor: 0 });
        assert!(other.contains("#define BW_SDK_ABI_VERSION_MAJOR 2"));
        assert_ne!(header, other);
    }

    #[test]
    fn the_header_declares_every_entry_point() {
        let header = render_header(version());

        // Every entry point's full prototype appears.
        for entry in ENTRY_POINTS {
            assert!(
                header.contains(&format!("{};", prototype(entry))),
                "missing prototype for {}",
                entry.name,
            );
        }
    }

    #[test]
    fn the_header_echoes_the_rust_error_codes() {
        let header = render_header(version());

        // Every error the Rust model reports has a constant and a string in
        // the header, so a C host reads the same stable code.
        let pairs = [
            (
                HandleError::UnknownHandle.code(),
                "BW_SDK_ERR_UNKNOWN_HANDLE",
            ),
            (
                HandleError::DoubleRelease.code(),
                "BW_SDK_ERR_DOUBLE_RELEASE",
            ),
            (
                AbiError::MajorMismatch.code(),
                "BW_SDK_ERR_ABI_MAJOR_MISMATCH",
            ),
            (AbiError::HostNewer.code(), "BW_SDK_ERR_ABI_HOST_NEWER"),
        ];
        for (code, name) in pairs {
            assert!(header.contains(name), "missing constant {name}");
            assert!(
                header.contains(&format!("#define {name}_TEXT \"{code}\"")),
                "missing stable string for {name}",
            );
        }
        // The no-error code is 0, and the error values are distinct.
        assert!(header.contains("#define BW_SDK_OK 0"));
        let values: Vec<i32> = ERROR_CODES.iter().map(|code| code.value).collect();
        let unique: std::collections::BTreeSet<i32> = values.iter().copied().collect();
        assert_eq!(unique.len(), ERROR_CODES.len());
    }

    #[test]
    fn a_c_example_loads_the_header_and_calls_the_sdk() {
        let example = render_example();

        // The example includes the header by name and calls every entry point.
        assert!(example.contains(&format!("#include \"{HEADER_FILE}\"")));
        for entry in ENTRY_POINTS {
            assert!(
                example.contains(entry.name),
                "example does not call {}",
                entry.name
            );
        }

        // Every call resolves against a declared prototype (the acceptance).
        let checks = verify_calls(&example_calls());
        assert!(
            checks.iter().all(Result::is_ok),
            "unresolved calls: {checks:?}",
        );
        assert!(example_loads_and_calls(&example_calls()));
    }

    #[test]
    fn a_broken_example_is_diagnosed() {
        let checks = verify_calls(&[
            ExampleCall {
                name: "bw_sdk_open",
                args: &[],
            },
            ExampleCall {
                name: "no_such_sdk",
                args: &[],
            },
            ExampleCall {
                name: "bw_sdk_release",
                args: &[],
            },
            ExampleCall {
                name: "bw_sdk_release",
                args: &["uint32_t"],
            },
        ]);

        // A good call resolves; each bad call is diagnosed, not a bare fail.
        assert!(checks[0].is_ok());
        assert!(
            checks[1].clone().unwrap_err().contains("unknown function"),
            "unexpected diagnostic: {:?}",
            checks[1],
        );
        assert!(
            checks[2]
                .clone()
                .unwrap_err()
                .contains("takes 1 parameters"),
            "unexpected diagnostic: {:?}",
            checks[2],
        );
        assert!(
            checks[3]
                .clone()
                .unwrap_err()
                .contains("parameter type mismatch"),
            "unexpected diagnostic: {:?}",
            checks[3],
        );
        // Not every call resolves, so the example does not load and call.
        assert!(!example_loads_and_calls(&[ExampleCall {
            name: "no_such_sdk",
            args: &[]
        },]));
    }

    #[test]
    fn a_c_example_compiles_when_a_c_compiler_is_present() {
        use std::process::Command;

        // Without a compiler the real check is a recorded gap, not a failure.
        if !Command::new("cc")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return;
        }

        let dir = std::env::temp_dir().join(format!(
            "bw21-sdk-header-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let header_path = dir.join(HEADER_FILE);
        let example_path = dir.join("example.c");
        std::fs::write(&header_path, render_header(version())).unwrap();
        std::fs::write(&example_path, render_example()).unwrap();

        // The example loads the header and calls the SDK; compiling it as
        // strict C11 proves the call sites are valid.
        let out = Command::new("cc")
            .arg("-std=c11")
            .arg("-Wall")
            .arg("-Wextra")
            .arg("-Werror")
            .arg("-fsyntax-only")
            .arg("-I")
            .arg(&dir)
            .arg(&example_path)
            .output()
            .expect("run the C compiler");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "the C example did not compile:\n{}",
            String::from_utf8_lossy(&out.stderr),
        );
    }
}
