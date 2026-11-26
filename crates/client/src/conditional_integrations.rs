//! BW-11-05-06: audit of the conditional integrations outside the VOIP stub
//! (BW-11-05-04, `voip.rs`) and the Awesomium web renderer (BW-11-05-05,
//! `web_render.rs`).
//!
//! The reference tree gates optional integrations behind CMake options set at
//! the top of `CMakeLists.txt:162-167` — every one of them OFF by default —
//! and each `IF( BW_*_SUPPORT )` block adds exactly one compile definition:
//! `FMOD_SUPPORT=1` (:186-190, with fmod include dirs under `third_party/fmod`),
//! `SCALEFORM_SUPPORT=1` (:192-194), `SPEEDTREE_SUPPORT=1` (:195-197) and
//! `UMBRA_ENABLE=1` (:198-200). Note the Umbra macro is `UMBRA_ENABLE`, not a
//! `*_SUPPORT` name.
//!
//! The audit verdict this module encodes: **none of the five optional SDKs
//! ships in the reference tree** — `third_party/fmod`, `third_party/scaleform`,
//! `third_party/speedtree`, `third_party/umbra` and `third_party/awesomium` are
//! all absent, so no `BW_*_SUPPORT` option can build. `third_party/openautomate`
//! is the only optional SDK present, and `open_automate` links into bwclient
//! unconditionally (`client/CMakeLists.txt:298`). VOIP has no option at all:
//! `lib/emptyvoip` is the always-built stub. And `USE_MEMHOOK` is the one
//! optional integration the client target *enables*: the top default is OFF
//! (`CMakeLists.txt:162`) but `BWConfiguration_client.cmake:9` re-sets it ON
//! and adds `-DUSE_MEMHOOK` (:15).
//!
//! Two capability switches are defined per-platform inside cstdmf rather than
//! by CMake: `BW_PLATFORM_SUPPORTS_CONSOLE` is 1 for `MF_SERVER || PLAYSTATION3
//! || __APPLE__` (`lib/cstdmf/debug_filter.cpp:31-35`, seeding
//! `s_shouldWriteToConsole_` true only on those, :38-42) and
//! `BW_PLATFORM_SUPPORTS_SYSLOG` is 1 for `!defined( _WIN32 ) &&
//! !defined( PLAYSTATION3 )` (`lib/cstdmf/log_msg.cpp:30-34`).
//!
//! The acceptance — every valid target has a platform and evidence — is the
//! [`audit`] contract: one row per target, both fields non-empty.

/// One conditional integration target from the reference tree.
pub struct Integration {
    /// Short target name (the audit row's key).
    pub name: &'static str,
    /// CMake option that turns it on; empty for always-on targets.
    pub cmake_option: &'static str,
    /// Compile definition the option adds; empty when none.
    pub compile_definition: &'static str,
    /// Config header documenting the switch; empty when none.
    pub config_header: &'static str,
    /// Top-of-tree default (`CMakeLists.txt:162-167`).
    pub default_enabled: bool,
    /// Third-party SDK directory (or in-tree lib) the target needs.
    pub sdk_dir: &'static str,
    /// Whether that directory exists in the reference tree.
    pub sdk_present: bool,
    /// Where the client target wires it in; empty when not wired.
    pub client_gate: &'static str,
    /// Source gates that compile the feature away when disabled.
    pub code_gates: &'static [&'static str],
    /// Platforms the integration applies to (the acceptance's platform field).
    pub platform: &'static str,
    /// Why the audit says what it says (the acceptance's evidence field).
    pub evidence: &'static str,
    /// Port cross-reference into this lane; empty when not delivered here.
    pub ported_as: &'static str,
}

/// The full audit inventory: the five `BW_*_SUPPORT` options, the VOIP stub,
/// memhook and open_automate.
pub const INTEGRATIONS: &[Integration] = &[
    Integration {
        name: "fmod",
        cmake_option: "BW_FMOD_SUPPORT",
        compile_definition: "FMOD_SUPPORT=1",
        config_header: "lib/fmodsound/fmod_config.hpp",
        default_enabled: false,
        sdk_dir: "third_party/fmod",
        sdk_present: false,
        client_gate: "client/CMakeLists.txt:318",
        code_gates: &[
            "client/device_app.cpp",
            "lib/duplo/pymodel.cpp",
            "lib/chunk/chunk_model.cpp",
            "lib/chunk/chunk_terrain.cpp",
        ],
        platform: "windows client (fmodsound links into bwclient)",
        evidence: "CMakeLists.txt:163 default OFF; fmod_config.hpp keeps \
                   ENABLE_JFIQ_HANDLING=1 while disabled and gates \
                   FMOD_SUPPORT_CUES/FMOD_SUPPORT_MEMORYINFO on \
                   FMOD_VERSION > 0x00042000; third_party/fmod absent so the \
                   option cannot build",
        ported_as: "sound.rs (BW-11-05-03)",
    },
    Integration {
        name: "scaleform",
        cmake_option: "BW_SCALEFORM_SUPPORT",
        compile_definition: "SCALEFORM_SUPPORT=1",
        config_header: "lib/scaleform/config.hpp",
        default_enabled: false,
        sdk_dir: "third_party/scaleform",
        sdk_present: false,
        client_gate: "client/CMakeLists.txt:324",
        code_gates: &["lib/scaleform/config.hpp"],
        platform: "windows client (scaleform links into bwclient)",
        evidence: "CMakeLists.txt:164 default OFF; the client target config \
                   re-asserts OFF (BWConfiguration_client.cmake:11); \
                   config.hpp sets SCALEFORM_IME=0 and SF_BUILD_STATICLIB=1, \
                   includes fmodsound/fmod_config.hpp unconditionally and \
                   pulls GFx_Sound_FMOD when enabled; SDK located via \
                   build/cmake/FindScaleformSDK.cmake but absent from the tree",
        ported_as: "",
    },
    Integration {
        name: "speedtree",
        cmake_option: "BW_SPEEDTREE_SUPPORT",
        compile_definition: "SPEEDTREE_SUPPORT=1",
        config_header: "lib/speedtree/speedtree_config_lite.hpp",
        default_enabled: false,
        sdk_dir: "third_party/speedtree",
        sdk_present: false,
        client_gate: "",
        code_gates: &["lib/romp/water_scene_renderer.cpp:570"],
        platform: "client vegetation render",
        evidence: "CMakeLists.txt:165 default OFF; the config header contract \
                   is that a disabled build keeps usable empty implementations \
                   that print warnings to the debug output, never errors, \
                   asserts or exceptions; needs SpeedTreeRT SDK 4.2+ and a \
                   license; SPT_CTREE_VERSION 105; SDK absent from the tree",
        ported_as: "",
    },
    Integration {
        name: "umbra",
        cmake_option: "BW_UMBRA_SUPPORT",
        compile_definition: "UMBRA_ENABLE=1",
        config_header: "lib/chunk/umbra_config.hpp",
        default_enabled: false,
        sdk_dir: "third_party/umbra",
        sdk_present: false,
        client_gate: "",
        code_gates: &[
            "lib/romp/water.cpp:20",
            "lib/duplo/chunk_dynamic_obstacle.cpp:22",
            "lib/duplo/chunk_attachment.cpp:14",
        ],
        platform: "client chunk/visibility render",
        evidence: "CMakeLists.txt:166 default OFF adds UMBRA_ENABLE=1 (the \
                   macro is UMBRA_ENABLE, not a *_SUPPORT name); \
                   umbra_config.hpp says the umbra library to link to is \
                   defined in chunk_umbra.cpp; SDK absent from the tree",
        ported_as: "",
    },
    Integration {
        name: "awesomium",
        cmake_option: "BW_AWESOMIUM_SUPPORT",
        compile_definition: "BW_AWESOMIUM_SUPPORT",
        config_header: "lib/web_render/awesomium_provider.hpp",
        default_enabled: false,
        sdk_dir: "third_party/awesomium",
        sdk_present: false,
        client_gate: "client/CMakeLists.txt:29,348",
        code_gates: &["awesomium_provider.hpp:5 (ENABLE_AWESOMIUM hard-wired 0 on _WIN64)"],
        platform: "windows client, MSVC delay-load",
        evidence: "CMakeLists.txt:167 default OFF; LINK_DIRECTORIES \
                   third_party/awesomium/build/lib behind the option \
                   (client/CMakeLists.txt:29); /DELAYLOAD:awesomium.dll + \
                   DelayImp only when BW_AWESOMIUM_SUPPORT AND MSVC (:348); \
                   SDK absent from the tree",
        ported_as: "web_render.rs (BW-11-05-05)",
    },
    Integration {
        name: "voip",
        cmake_option: "",
        compile_definition: "",
        config_header: "lib/emptyvoip/voip.hpp",
        default_enabled: true,
        sdk_dir: "third_party (voip SDK, unnamed)",
        sdk_present: false,
        client_gate: "client/CMakeLists.txt:9,311",
        code_gates: &["client/bwvoip.cpp"],
        platform: "windows client",
        evidence: "no macro at all: lib/emptyvoip is the always-built stub \
                   (initialise 0, numberChannels 0, getChannel ''); it is in \
                   bwclient's INCLUDE_DIRECTORIES (:9) and link list (:311) \
                   with no option gating it",
        ported_as: "voip.rs (BW-11-05-04)",
    },
    Integration {
        name: "memhook",
        cmake_option: "USE_MEMHOOK",
        compile_definition: "-DUSE_MEMHOOK",
        config_header: "lib/memhook/memhook.hpp",
        default_enabled: false,
        sdk_dir: "lib/memhook (in-tree)",
        sdk_present: true,
        client_gate: "BWConfiguration_client.cmake:9,15",
        code_gates: &["lib/memhook/memhook.cpp"],
        platform: "all client targets",
        evidence: "the one optional integration the client enables: top \
                   default OFF (CMakeLists.txt:162) but the client target \
                   config re-sets USE_MEMHOOK ON (:9) and adds -DUSE_MEMHOOK \
                   (:15); the extras config also turns it ON (:1-2) while the \
                   tools config leaves it OFF (:3)",
        ported_as: "",
    },
    Integration {
        name: "open_automate",
        cmake_option: "",
        compile_definition: "",
        config_header: "",
        default_enabled: true,
        sdk_dir: "third_party/openautomate",
        sdk_present: true,
        client_gate: "client/CMakeLists.txt:298",
        code_gates: &["lib/open_automate/open_automate_wrapper.cpp"],
        platform: "windows client",
        evidence: "unconditional: open_automate sits in bwclient's link list \
                   (client/CMakeLists.txt:298) with no option, and \
                   third_party/openautomate is the only optional SDK shipped \
                   in the tree",
        ported_as: "",
    },
];

/// One audit row: the acceptance's (target, platform, evidence) triple.
#[derive(Debug)]
pub struct AuditRow {
    /// Target name.
    pub target: &'static str,
    /// Platform the target applies to.
    pub platform: &'static str,
    /// Evidence the verdict rests on.
    pub evidence: &'static str,
}

/// The acceptance face: one row per integration target, each with its platform
/// and evidence.
pub fn audit() -> Vec<AuditRow> {
    INTEGRATIONS
        .iter()
        .map(|integration| AuditRow {
            target: integration.name,
            platform: integration.platform,
            evidence: integration.evidence,
        })
        .collect()
}

/// Looks an integration up by name.
pub fn find_integration(name: &str) -> Option<&'static Integration> {
    INTEGRATIONS
        .iter()
        .find(|integration| integration.name == name)
}

/// The CMake options backed by an SDK directory absent from the tree — the
/// options that cannot build.
pub fn sdk_backed_options() -> Vec<&'static str> {
    INTEGRATIONS
        .iter()
        .filter(|integration| !integration.cmake_option.is_empty() && !integration.sdk_present)
        .map(|integration| integration.cmake_option)
        .collect()
}

/// The platform macros the two cstdmf capability switches read
/// (`debug_filter.cpp:31`, `log_msg.cpp:30`).
pub struct PlatformFlags {
    /// `defined( MF_SERVER )` — server builds.
    pub mf_server: bool,
    /// `defined( _WIN32 )` — Windows builds.
    pub win32: bool,
    /// `defined( PLAYSTATION3 )`.
    pub playstation3: bool,
    /// `defined( __APPLE__ )` — Mac builds.
    pub apple: bool,
}

impl PlatformFlags {
    /// A Windows client build: `_WIN32` only.
    pub fn win_client() -> Self {
        PlatformFlags {
            mf_server: false,
            win32: true,
            playstation3: false,
            apple: false,
        }
    }

    /// A Linux server build: `MF_SERVER` only.
    pub fn linux_server() -> Self {
        PlatformFlags {
            mf_server: true,
            win32: false,
            playstation3: false,
            apple: false,
        }
    }

    /// A Mac client build: `__APPLE__` only.
    pub fn mac_client() -> Self {
        PlatformFlags {
            mf_server: false,
            win32: false,
            playstation3: false,
            apple: true,
        }
    }

    /// A PlayStation3 build.
    pub fn playstation3() -> Self {
        PlatformFlags {
            mf_server: false,
            win32: false,
            playstation3: true,
            apple: false,
        }
    }
}

/// `BW_PLATFORM_SUPPORTS_CONSOLE`: `MF_SERVER || PLAYSTATION3 || __APPLE__`
/// (`lib/cstdmf/debug_filter.cpp:31-35`).
pub fn platform_supports_console(platform: &PlatformFlags) -> bool {
    platform.mf_server || platform.playstation3 || platform.apple
}

/// `BW_PLATFORM_SUPPORTS_SYSLOG`: `!defined( _WIN32 ) && !defined( PLAYSTATION3 )`
/// (`lib/cstdmf/log_msg.cpp:30-34`).
pub fn platform_supports_syslog(platform: &PlatformFlags) -> bool {
    !platform.win32 && !platform.playstation3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_rows_all_have_platform_and_evidence() {
        let rows = audit();
        assert_eq!(rows.len(), 8);
        for row in &rows {
            assert!(!row.target.is_empty(), "target missing: {:?}", row);
            assert!(!row.platform.is_empty(), "platform missing: {}", row.target);
            assert!(!row.evidence.is_empty(), "evidence missing: {}", row.target);
        }
    }

    #[test]
    fn five_cmake_options_default_off() {
        for name in [
            "fmod",
            "scaleform",
            "speedtree",
            "umbra",
            "awesomium",
            "memhook",
        ] {
            let integration = find_integration(name).unwrap();
            assert!(
                !integration.default_enabled,
                "{} must default OFF",
                integration.cmake_option
            );
        }
        // voip and open_automate have no option: they are always built.
        assert!(find_integration("voip").unwrap().default_enabled);
        assert!(find_integration("open_automate").unwrap().default_enabled);
    }

    #[test]
    fn only_open_automate_sdk_ships_in_the_tree() {
        for name in [
            "fmod",
            "scaleform",
            "speedtree",
            "umbra",
            "awesomium",
            "voip",
        ] {
            let integration = find_integration(name).unwrap();
            assert!(
                !integration.sdk_present,
                "{} SDK is absent",
                integration.name
            );
        }
        assert!(find_integration("open_automate").unwrap().sdk_present);
        assert!(find_integration("memhook").unwrap().sdk_present);
    }

    #[test]
    fn sdk_backed_options_all_absent() {
        let options = sdk_backed_options();
        assert_eq!(
            options,
            vec![
                "BW_FMOD_SUPPORT",
                "BW_SCALEFORM_SUPPORT",
                "BW_SPEEDTREE_SUPPORT",
                "BW_UMBRA_SUPPORT",
                "BW_AWESOMIUM_SUPPORT",
            ]
        );
    }

    #[test]
    fn umbra_option_names_a_different_macro() {
        let umbra = find_integration("umbra").unwrap();
        assert_eq!(umbra.cmake_option, "BW_UMBRA_SUPPORT");
        assert_eq!(umbra.compile_definition, "UMBRA_ENABLE=1");
    }

    #[test]
    fn fmod_config_stays_active_while_disabled() {
        let fmod = find_integration("fmod").unwrap();
        assert_eq!(fmod.compile_definition, "FMOD_SUPPORT=1");
        // ENABLE_JFIQ_HANDLING=1 and the FMOD_VERSION > 0x00042000 cue gate
        // both live in fmod_config.hpp regardless of the option.
        assert!(fmod.evidence.contains("ENABLE_JFIQ_HANDLING=1"));
        assert!(fmod.evidence.contains("FMOD_VERSION > 0x00042000"));
    }

    #[test]
    fn scaleform_client_config_reasserts_off() {
        let scaleform = find_integration("scaleform").unwrap();
        assert!(
            scaleform
                .evidence
                .contains("BWConfiguration_client.cmake:11")
        );
        assert!(scaleform.evidence.contains("SF_BUILD_STATICLIB=1"));
        assert!(scaleform.evidence.contains("SCALEFORM_IME=0"));
    }

    #[test]
    fn speedtree_disabled_contract_is_warnings_not_errors() {
        let speedtree = find_integration("speedtree").unwrap();
        assert!(speedtree.evidence.contains("print warnings"));
        assert!(speedtree.evidence.contains("SPT_CTREE_VERSION 105"));
        assert_eq!(
            speedtree.code_gates,
            ["lib/romp/water_scene_renderer.cpp:570"]
        );
    }

    #[test]
    fn awesomium_delay_load_is_msvc_only() {
        let awesomium = find_integration("awesomium").unwrap();
        assert_eq!(awesomium.client_gate, "client/CMakeLists.txt:29,348");
        assert!(awesomium.platform.contains("MSVC"));
        assert!(awesomium.evidence.contains("/DELAYLOAD:awesomium.dll"));
    }

    #[test]
    fn voip_is_an_always_built_stub() {
        let voip = find_integration("voip").unwrap();
        assert!(voip.cmake_option.is_empty());
        assert_eq!(voip.client_gate, "client/CMakeLists.txt:9,311");
        assert!(voip.evidence.contains("always-built stub"));
    }

    #[test]
    fn memhook_is_the_client_enabled_exception() {
        let memhook = find_integration("memhook").unwrap();
        assert_eq!(memhook.cmake_option, "USE_MEMHOOK");
        assert_eq!(memhook.client_gate, "BWConfiguration_client.cmake:9,15");
        assert!(memhook.sdk_present);
        assert!(memhook.evidence.contains("client target config re-sets"));
    }

    #[test]
    fn open_automate_links_unconditionally() {
        let open_automate = find_integration("open_automate").unwrap();
        assert!(open_automate.cmake_option.is_empty());
        assert_eq!(open_automate.client_gate, "client/CMakeLists.txt:298");
        assert!(open_automate.sdk_present);
    }

    #[test]
    fn ported_integrations_cross_reference_the_lane() {
        assert_eq!(
            find_integration("fmod").unwrap().ported_as,
            "sound.rs (BW-11-05-03)"
        );
        assert_eq!(
            find_integration("awesomium").unwrap().ported_as,
            "web_render.rs (BW-11-05-05)"
        );
        assert_eq!(
            find_integration("voip").unwrap().ported_as,
            "voip.rs (BW-11-05-04)"
        );
        for name in [
            "scaleform",
            "speedtree",
            "umbra",
            "memhook",
            "open_automate",
        ] {
            assert!(
                find_integration(name).unwrap().ported_as.is_empty(),
                "{} is not a lane delivery",
                name
            );
        }
    }

    #[test]
    fn find_integration_lookup() {
        assert!(find_integration("fmod").is_some());
        assert!(find_integration("nope").is_none());
    }

    #[test]
    fn console_matrix() {
        // debug_filter.cpp:31-35: MF_SERVER || PLAYSTATION3 || __APPLE__.
        assert!(!platform_supports_console(&PlatformFlags::win_client()));
        assert!(platform_supports_console(&PlatformFlags::linux_server()));
        assert!(platform_supports_console(&PlatformFlags::mac_client()));
        assert!(platform_supports_console(&PlatformFlags::playstation3()));
    }

    #[test]
    fn syslog_matrix() {
        // log_msg.cpp:30-34: !defined( _WIN32 ) && !defined( PLAYSTATION3 ).
        assert!(!platform_supports_syslog(&PlatformFlags::win_client()));
        assert!(!platform_supports_syslog(&PlatformFlags::playstation3()));
        assert!(platform_supports_syslog(&PlatformFlags::linux_server()));
        assert!(platform_supports_syslog(&PlatformFlags::mac_client()));
    }

    #[test]
    fn console_default_tracks_platform() {
        // debug_filter.cpp:38-42 seeds s_shouldWriteToConsole_ from the cap.
        let mut platform = PlatformFlags::win_client();
        assert!(!platform_supports_console(&platform));
        platform.apple = true;
        assert!(platform_supports_console(&platform));
    }
}
